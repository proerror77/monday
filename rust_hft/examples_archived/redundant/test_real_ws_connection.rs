/*!
 * 真實WebSocket連接測試腳本
 * 
 * 此腳本專注於：
 * - 真實連接到Bitget WebSocket
 * - 高性能數據收集
 * - 實時數據入庫到ClickHouse
 * - 性能監控和統計
 */

use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use clickhouse::{Client, Row};
use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "真實WebSocket連接和數據入庫測試")]
struct Args {
    /// 交易對
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,
    
    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 30)]
    duration: u64,
    
    /// 是否啟用ClickHouse寫入
    #[arg(long)]
    enable_clickhouse: bool,
    
    /// ClickHouse URL
    #[arg(long, default_value = "http://localhost:8123")]
    clickhouse_url: String,
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, Row, serde::Serialize)]
struct MarketDataRecord {
    timestamp: i64,
    symbol: String,
    message_type: String,
    best_bid: f64,
    best_ask: f64,
    mid_price: f64,
    spread: f64,
    bid_volume: f64,
    ask_volume: f64,
}

struct PerformanceMetrics {
    total_messages: AtomicU64,
    orderbook_messages: AtomicU64,
    trade_messages: AtomicU64,
    errors: AtomicU64,
    total_latency_us: AtomicU64,
    db_writes: AtomicU64,
    db_errors: AtomicU64,
}

impl PerformanceMetrics {
    fn new() -> Self {
        Self {
            total_messages: AtomicU64::new(0),
            orderbook_messages: AtomicU64::new(0),
            trade_messages: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            db_writes: AtomicU64::new(0),
            db_errors: AtomicU64::new(0),
        }
    }
    
    fn record_message(&self, msg_type: &str, latency_us: u64) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);
        
        match msg_type {
            "orderbook" => self.orderbook_messages.fetch_add(1, Ordering::Relaxed),
            "trade" => self.trade_messages.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };
    }
    
    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_db_write(&self, success: bool) {
        if success {
            self.db_writes.fetch_add(1, Ordering::Relaxed);
        } else {
            self.db_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
    
    fn get_stats(&self) -> (u64, u64, u64, u64, f64, u64, u64) {
        let total = self.total_messages.load(Ordering::Relaxed);
        let orderbook = self.orderbook_messages.load(Ordering::Relaxed);
        let trades = self.trade_messages.load(Ordering::Relaxed);
        let errors = self.errors.load(Ordering::Relaxed);
        let latency_sum = self.total_latency_us.load(Ordering::Relaxed);
        let db_writes = self.db_writes.load(Ordering::Relaxed);
        let db_errors = self.db_errors.load(Ordering::Relaxed);
        
        let avg_latency = if total > 0 { latency_sum as f64 / total as f64 } else { 0.0 };
        
        (total, orderbook, trades, errors, avg_latency, db_writes, db_errors)
    }
}

async fn setup_clickhouse_table(client: &Client) -> Result<()> {
    let create_table_sql = r#"
        CREATE TABLE IF NOT EXISTS hft_test.market_data (
            timestamp Int64,
            symbol String,
            message_type String,
            best_bid Float64,
            best_ask Float64,
            mid_price Float64,
            spread Float64,
            bid_volume Float64,
            ask_volume Float64
        ) ENGINE = MergeTree()
        ORDER BY (symbol, timestamp)
        SETTINGS index_granularity = 8192
    "#;
    
    client.query("CREATE DATABASE IF NOT EXISTS hft_test").execute().await?;
    client.query(create_table_sql).execute().await?;
    info!("ClickHouse表已創建/驗證");
    
    Ok(())
}

async fn clickhouse_writer(
    mut rx: mpsc::Receiver<MarketDataRecord>,
    client: Arc<Client>,
    metrics: Arc<PerformanceMetrics>,
) -> Result<()> {
    let mut batch = Vec::with_capacity(100);
    let mut last_flush = Instant::now();
    let flush_interval = Duration::from_secs(1);
    
    while let Some(record) = rx.recv().await {
        batch.push(record);
        
        if batch.len() >= 100 || last_flush.elapsed() >= flush_interval {
            let start_time = Instant::now();
            
            match flush_batch(&client, &mut batch).await {
                Ok(_) => {
                    metrics.record_db_write(true);
                    let latency = start_time.elapsed();
                    info!("批量寫入 {} 條記錄，耗時 {:?}", batch.len(), latency);
                }
                Err(e) => {
                    error!("ClickHouse寫入失敗: {}", e);
                    metrics.record_db_write(false);
                }
            }
            
            batch.clear();
            last_flush = Instant::now();
        }
    }
    
    if !batch.is_empty() {
        let _ = flush_batch(&client, &mut batch).await;
        info!("最終寫入 {} 條記錄", batch.len());
    }
    
    Ok(())
}

async fn flush_batch(client: &Client, batch: &mut Vec<MarketDataRecord>) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    
    let mut insert = client.insert("hft_test.market_data")?;
    for record in batch.iter() {
        insert.write(record).await?;
    }
    insert.end().await?;
    
    Ok(())
}

fn parse_orderbook_data(symbol: &str, data: &Value) -> Option<MarketDataRecord> {
    let timestamp = chrono::Utc::now().timestamp_micros();
    
    if let Some(data_array) = data.as_array() {
        if let Some(orderbook_data) = data_array.first() {
            let bids = orderbook_data.get("bids").and_then(|v| v.as_array())?;
            let asks = orderbook_data.get("asks").and_then(|v| v.as_array())?;
            
            if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
                let bid_price = best_bid.as_array()?.get(0)?.as_str()?.parse::<f64>().ok()?;
                let ask_price = best_ask.as_array()?.get(0)?.as_str()?.parse::<f64>().ok()?;
                let bid_volume = best_bid.as_array()?.get(1)?.as_str()?.parse::<f64>().ok()?;
                let ask_volume = best_ask.as_array()?.get(1)?.as_str()?.parse::<f64>().ok()?;
                
                let mid_price = (bid_price + ask_price) / 2.0;
                let spread = ask_price - bid_price;
                
                return Some(MarketDataRecord {
                    timestamp,
                    symbol: symbol.to_string(),
                    message_type: "orderbook".to_string(),
                    best_bid: bid_price,
                    best_ask: ask_price,
                    mid_price,
                    spread,
                    bid_volume,
                    ask_volume,
                });
            }
        }
    }
    
    None
}

async fn websocket_client(
    symbol: String,
    metrics: Arc<PerformanceMetrics>,
    db_tx: Option<mpsc::Sender<MarketDataRecord>>,
    verbose: bool,
) -> Result<()> {
    let url = "wss://ws.bitget.com/v2/ws/public";
    info!("連接到 Bitget WebSocket: {}", url);
    
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();
    
    // 訂閱OrderBook數據
    let subscribe_msg = json!({
        "op": "subscribe",
        "args": [{
            "instType": "SPOT",
            "channel": "books5",
            "instId": symbol
        }]
    });
    
    write.send(Message::Text(subscribe_msg.to_string())).await?;
    info!("已訂閱 {} 的OrderBook數據", symbol);
    
    let mut message_count = 0u64;
    
    while let Some(msg) = read.next().await {
        let start_time = Instant::now();
        
        match msg? {
            Message::Text(text) => {
                message_count += 1;
                
                if let Ok(data) = serde_json::from_str::<Value>(&text) {
                    if let Some(arg) = data.get("arg") {
                        if let Some(channel) = arg.get("channel") {
                            if channel == "books5" {
                                let latency_us = start_time.elapsed().as_micros() as u64;
                                metrics.record_message("orderbook", latency_us);
                                
                                if let Some(record) = parse_orderbook_data(&symbol, data.get("data").unwrap_or(&Value::Null)) {
                                    if let Some(ref tx) = db_tx {
                                        if let Err(_) = tx.send(record).await {
                                            warn!("數據庫寫入通道已關閉");
                                            break;
                                        }
                                    }
                                    
                                    if verbose && message_count % 100 == 0 {
                                        info!("已處理 {} 條消息", message_count);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    metrics.record_error();
                }
            }
            Message::Pong(_) => {
                if verbose {
                    info!("收到 Pong");
                }
            }
            _ => {}
        }
    }
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    
    info!("🚀 開始真實WebSocket連接測試");
    info!("交易對: {}", args.symbol);
    info!("測試時長: {}秒", args.duration);
    info!("ClickHouse寫入: {}", if args.enable_clickhouse { "啟用" } else { "禁用" });
    
    let metrics = Arc::new(PerformanceMetrics::new());
    
    // 設置ClickHouse客戶端和寫入器
    let (db_tx, clickhouse_handle) = if args.enable_clickhouse {
        let client = Arc::new(Client::default());
        setup_clickhouse_table(&client).await?;
        
        let (tx, rx) = mpsc::channel(1000);
        let metrics_clone = metrics.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = clickhouse_writer(rx, client, metrics_clone).await {
                error!("ClickHouse寫入器錯誤: {}", e);
            }
        });
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };
    
    // 啟動WebSocket客戶端
    let ws_metrics = metrics.clone();
    let ws_symbol = args.symbol.clone();
    let ws_handle = tokio::spawn(async move {
        if let Err(e) = websocket_client(ws_symbol, ws_metrics, db_tx, args.verbose).await {
            error!("WebSocket客戶端錯誤: {}", e);
        }
    });
    
    // 統計報告任務
    let report_metrics = metrics.clone();
    let report_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        
        loop {
            interval.tick().await;
            let (total, orderbook, trades, errors, avg_latency, db_writes, db_errors) = 
                report_metrics.get_stats();
            
            info!("📊 實時統計:");
            info!("  總消息: {}", total);
            info!("  OrderBook: {}", orderbook);
            info!("  Trade: {}", trades);
            info!("  錯誤: {}", errors);
            info!("  平均延遲: {:.2}μs", avg_latency);
            info!("  數據庫寫入: {}", db_writes);
            info!("  數據庫錯誤: {}", db_errors);
        }
    });
    
    // 運行測試
    tokio::time::sleep(Duration::from_secs(args.duration)).await;
    
    info!("🔄 測試完成，正在關閉...");
    
    // 停止任務
    ws_handle.abort();
    report_handle.abort();
    if let Some(handle) = clickhouse_handle {
        handle.abort();
    }
    
    // 最終統計
    let (total, orderbook, trades, errors, avg_latency, db_writes, db_errors) = 
        metrics.get_stats();
    
    info!("🏁 最終統計報告:");
    info!("  總消息數: {}", total);
    info!("  OrderBook消息: {}", orderbook);
    info!("  Trade消息: {}", trades);
    info!("  錯誤數: {}", errors);
    info!("  平均延遲: {:.2}μs", avg_latency);
    info!("  數據庫寫入: {}", db_writes);
    info!("  數據庫錯誤: {}", db_errors);
    
    if total > 0 {
        let throughput = total as f64 / args.duration as f64;
        info!("  消息吞吐量: {:.2} 消息/秒", throughput);
    }
    
    if args.enable_clickhouse && db_writes > 0 {
        info!("  數據庫寫入成功率: {:.2}%", (db_writes as f64 / (db_writes + db_errors) as f64) * 100.0);
    }
    
    Ok(())
}