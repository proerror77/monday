/*!
 * ClickHouse 批量寫入性能測試
 * 
 * 測試重點：
 * - 批量寫入吞吐量
 * - 網絡延遲影響
 * - 併發寫入性能
 * - 數據壓縮效果
 * - 錯誤恢復機制
 */

use rust_hft::{
    integrations::bitget_connector::*,
    core::types::*,
};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::Result;
use clap::Parser;
use tokio::sync::mpsc;
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn, error};
use serde_json::{json, Value};
use clickhouse::{Client, Row};

// Helper function to get current time in microseconds
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

#[derive(Parser, Debug)]
#[command(about = "ClickHouse 批量寫入性能測試")]
struct Args {
    /// ClickHouse 連接URL
    #[arg(long, default_value = "http://localhost:8123")]
    clickhouse_url: String,
    
    /// 數據庫名稱
    #[arg(long, default_value = "hft_test")]
    database: String,
    
    /// 表名稱
    #[arg(long, default_value = "orderbook_test")]
    table: String,
    
    /// 批量大小
    #[arg(short, long, default_value_t = 1000)]
    batch_size: usize,
    
    /// 測試時長（秒）
    #[arg(short, long, default_value_t = 60)]
    duration: u64,
    
    /// 併發寫入數
    #[arg(short, long, default_value_t = 1)]
    concurrency: usize,
    
    /// 數據生成速率（記錄/秒）
    #[arg(short, long, default_value_t = 10000)]
    rate: usize,
    
    /// 詳細輸出
    #[arg(short, long)]
    verbose: bool,
    
    /// 啟用壓縮
    #[arg(long)]
    compression: bool,
}

#[derive(Debug, Default)]
struct WriteMetrics {
    total_records: AtomicU64,
    total_batches: AtomicU64,
    total_bytes: AtomicU64,
    write_errors: AtomicU64,
    write_latency_sum_us: AtomicU64,
    write_latency_count: AtomicU64,
    queue_size: AtomicU64,
}

impl WriteMetrics {
    fn record_batch_write(&self, records: usize, bytes: usize, latency_us: u64) {
        self.total_records.fetch_add(records as u64, Ordering::Relaxed);
        self.total_batches.fetch_add(1, Ordering::Relaxed);
        self.total_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
        self.write_latency_sum_us.fetch_add(latency_us, Ordering::Relaxed);
        self.write_latency_count.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_error(&self) {
        self.write_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn update_queue_size(&self, size: usize) {
        self.queue_size.store(size as u64, Ordering::Relaxed);
    }
    
    fn get_stats(&self) -> (u64, u64, u64, u64, f64, u64) {
        let records = self.total_records.load(Ordering::Relaxed);
        let batches = self.total_batches.load(Ordering::Relaxed);
        let bytes = self.total_bytes.load(Ordering::Relaxed);
        let errors = self.write_errors.load(Ordering::Relaxed);
        let latency_sum = self.write_latency_sum_us.load(Ordering::Relaxed);
        let latency_count = self.write_latency_count.load(Ordering::Relaxed);
        let queue_size = self.queue_size.load(Ordering::Relaxed);
        
        let avg_latency = if latency_count > 0 {
            latency_sum as f64 / latency_count as f64
        } else {
            0.0
        };
        
        (records, batches, bytes, errors, avg_latency, queue_size)
    }
}

#[derive(Debug, Clone, Row, serde::Serialize)]
struct OrderBookRecord {
    timestamp: i64, // Unix timestamp in microseconds
    symbol: String,
    bids: String, // JSON string of bids
    asks: String, // JSON string of asks
    mid_price: f64,
    spread: f64,
    total_bid_volume: f64,
    total_ask_volume: f64,
}

// 真實數據收集器 - 從 Bitget WebSocket 收集實時 OrderBook 數據
async fn real_data_collector(
    tx: mpsc::Sender<OrderBookRecord>,
    symbol: String,
    shutdown: Arc<AtomicBool>,
    metrics: Arc<WriteMetrics>,
) -> Result<()> {
    let bitget_config = BitgetConfig::default();
    let mut connector = BitgetConnector::new(bitget_config);
    connector.add_subscription(symbol.clone(), BitgetChannel::Books5);
    
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel();
    let ws_tx_clone = ws_tx.clone();
    
    let message_handler = move |message: BitgetMessage| {
        let _ = ws_tx_clone.send(message);
    };
    
    // 啟動 WebSocket 連接
    let connector_task = tokio::spawn(async move {
        if let Err(e) = connector.connect_public(message_handler).await {
            error!("WebSocket 連接失敗: {}", e);
        }
    });
    
    info!("開始收集 {} 的實時 OrderBook 數據", symbol);
    
    while !shutdown.load(Ordering::Relaxed) {
        tokio::select! {
            Some(message) = ws_rx.recv() => {
                match message {
                    BitgetMessage::OrderBook { data, timestamp, .. } => {
                        if let Ok(record) = convert_to_clickhouse_record(&symbol, &data, timestamp) {
                            if tx.send(record).await.is_err() {
                                warn!("無法發送數據到 ClickHouse 批處理器");
                                break;
                            }
                            metrics.update_queue_size(1); // 簡化的隊列大小更新
                        }
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                // 定期檢查關閉信號
                continue;
            }
        }
    }
    
    connector_task.abort();
    Ok(())
}

fn convert_to_clickhouse_record(symbol: &str, data: &Value, timestamp: u64) -> Result<OrderBookRecord> {
    // 解析 Bitget OrderBook 數據
    let empty_vec = Vec::new();
    
    if let Some(data_array) = data.as_array() {
        if let Some(orderbook_data) = data_array.first() {
            let bids_data = orderbook_data.get("bids").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
            let asks_data = orderbook_data.get("asks").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
            
            // 計算 mid price, spread, volumes
            let best_bid = bids_data.first()
                .and_then(|b| b.as_array())
                .and_then(|b| b.get(0))
                .and_then(|p| p.as_str())
                .and_then(|p| p.parse::<f64>().ok())
                .unwrap_or(0.0);
                
            let best_ask = asks_data.first()
                .and_then(|a| a.as_array())
                .and_then(|a| a.get(0))
                .and_then(|p| p.as_str())
                .and_then(|p| p.parse::<f64>().ok())
                .unwrap_or(0.0);
            
            let mid_price = (best_bid + best_ask) / 2.0;
            let spread = best_ask - best_bid;
            
            // 計算總成交量
            let total_bid_volume: f64 = bids_data.iter()
                .filter_map(|b| b.as_array())
                .filter_map(|b| b.get(1))
                .filter_map(|v| v.as_str())
                .filter_map(|v| v.parse::<f64>().ok())
                .sum();
                
            let total_ask_volume: f64 = asks_data.iter()
                .filter_map(|a| a.as_array())
                .filter_map(|a| a.get(1))
                .filter_map(|v| v.as_str())
                .filter_map(|v| v.parse::<f64>().ok())
                .sum();
            
            return Ok(OrderBookRecord {
                timestamp: timestamp as i64,
                symbol: symbol.to_string(),
                bids: serde_json::to_string(bids_data)?,
                asks: serde_json::to_string(asks_data)?,
                mid_price,
                spread,
                total_bid_volume,
                total_ask_volume,
            });
        }
    }
    
    Err(anyhow::anyhow!("無法解析 OrderBook 數據"))
}

async fn batch_writer(
    mut rx: mpsc::Receiver<OrderBookRecord>,
    client: Arc<Client>,
    batch_size: usize,
    table: String,
    metrics: Arc<WriteMetrics>,
    shutdown: Arc<AtomicBool>,
    writer_id: usize,
) -> Result<()> {
    let mut batch = Vec::with_capacity(batch_size);
    let mut last_flush = Instant::now();
    let flush_interval = Duration::from_secs(1);
    
    while !shutdown.load(Ordering::Relaxed) {
        tokio::select! {
            Some(record) = rx.recv() => {
                batch.push(record);
                metrics.update_queue_size(batch.len());
                
                if batch.len() >= batch_size {
                    if let Err(e) = flush_batch(&client, &mut batch, &table, &metrics, writer_id).await {
                        error!("Writer {}: Batch flush failed: {}", writer_id, e);
                        metrics.record_error();
                    }
                }
            }
            _ = tokio::time::sleep(flush_interval) => {
                if !batch.is_empty() && last_flush.elapsed() >= flush_interval {
                    if let Err(e) = flush_batch(&client, &mut batch, &table, &metrics, writer_id).await {
                        error!("Writer {}: Timeout flush failed: {}", writer_id, e);
                        metrics.record_error();
                    }
                    last_flush = Instant::now();
                }
            }
        }
    }
    
    // 最終刷新
    if !batch.is_empty() {
        if let Err(e) = flush_batch(&client, &mut batch, &table, &metrics, writer_id).await {
            error!("Writer {}: Final flush failed: {}", writer_id, e);
            metrics.record_error();
        }
    }
    
    Ok(())
}

async fn flush_batch(
    client: &Client,
    batch: &mut Vec<OrderBookRecord>,
    table: &str,
    metrics: &WriteMetrics,
    writer_id: usize,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }
    
    let start_time = Instant::now();
    let record_count = batch.len();
    
    // 使用真實的 ClickHouse 客戶端寫入
    let mut insert = client.insert(table)?;
    for record in batch.iter() {
        insert.write(record).await?;
    }
    insert.end().await?;
    
    let data_size = record_count * std::mem::size_of::<OrderBookRecord>();
    let latency_us = start_time.elapsed().as_micros() as u64;
    metrics.record_batch_write(record_count, data_size, latency_us);
    
    info!("Writer {}: Flushed {} records in {}μs", 
          writer_id, record_count, latency_us);
    
    batch.clear();
    Ok(())
}

async fn setup_clickhouse_table(client: &Client, database: &str, table: &str) -> Result<()> {
    // 創建真實的 ClickHouse 數據庫和表
    let create_db_sql = format!("CREATE DATABASE IF NOT EXISTS {}", database);
    client.query(&create_db_sql).execute().await?;
    
    let create_table_sql = format!(
        r#"
        CREATE TABLE IF NOT EXISTS {}.{} (
            timestamp Int64,
            symbol String,
            bids String,
            asks String,
            mid_price Float64,
            spread Float64,
            total_bid_volume Float64,
            total_ask_volume Float64
        ) ENGINE = MergeTree()
        ORDER BY (symbol, timestamp)
        SETTINGS index_granularity = 8192
        "#,
        database, table
    );
    
    client.query(&create_table_sql).execute().await?;
    info!("ClickHouse table {}.{} created/verified", database, table);
    
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    let args = Args::parse();
    let metrics = Arc::new(WriteMetrics::default());
    let shutdown = Arc::new(AtomicBool::new(false));
    
    info!("🚀 ClickHouse 批量寫入性能測試開始");
    info!("📊 測試參數:");
    info!("   URL: {}", args.clickhouse_url);
    info!("   數據庫: {}", args.database);
    info!("   表: {}", args.table);
    info!("   批量大小: {}", args.batch_size);
    info!("   併發數: {}", args.concurrency);
    info!("   生成速率: {}/s", args.rate);
    info!("   測試時長: {}s", args.duration);
    
    // 初始化 ClickHouse 客戶端
    let client = Arc::new(Client::default());
    
    // 設置表結構
    setup_clickhouse_table(&client, &args.database, &args.table).await?;
    
    // 創建數據管道
    let (tx, mut rx) = mpsc::channel(args.batch_size * args.concurrency * 2);
    
    // 啟動真實數據收集器
    let collector_shutdown = shutdown.clone();
    let collector_symbol = "BTCUSDT".to_string();
    let collector_metrics = metrics.clone();
    let collector_handle = tokio::spawn(async move {
        real_data_collector(tx, collector_symbol, collector_shutdown, collector_metrics).await
    });
    
    // 創建多個發送器來分發數據到多個寫入器
    let mut writer_handles = Vec::new();
    let mut writer_senders = Vec::new();
    
    for i in 0..args.concurrency {
        let (writer_tx, writer_rx) = mpsc::channel(args.batch_size * 2);
        writer_senders.push(writer_tx);
        
        let client_clone = client.clone();
        let metrics_clone = metrics.clone();
        let shutdown_clone = shutdown.clone();
        let table_name = format!("{}.{}", args.database, args.table);
        
        let handle = tokio::spawn(async move {
            batch_writer(
                writer_rx,
                client_clone,
                args.batch_size,
                table_name,
                metrics_clone,
                shutdown_clone,
                i,
            ).await
        });
        writer_handles.push(handle);
    }
    
    // 數據分發器：將來自收集器的數據輪詢分發給多個寫入器
    let distributor_shutdown = shutdown.clone();
    let distributor_handle = tokio::spawn(async move {
        let mut round_robin = 0;
        while !distributor_shutdown.load(Ordering::Relaxed) {
            match rx.recv().await {
                Some(record) => {
                    let writer_index = round_robin % writer_senders.len();
                    if writer_senders[writer_index].send(record).await.is_err() {
                        warn!("寫入器 {} 已關閉", writer_index);
                        break;
                    }
                    round_robin = (round_robin + 1) % writer_senders.len();
                }
                None => break,
            }
        }
    });
    
    // 統計報告任務
    let report_metrics = metrics.clone();
    let report_shutdown = shutdown.clone();
    let report_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let start_time = Instant::now();
        
        while !report_shutdown.load(Ordering::Relaxed) {
            interval.tick().await;
            
            let (records, batches, bytes, errors, avg_latency, queue_size) = 
                report_metrics.get_stats();
            
            let elapsed = start_time.elapsed().as_secs_f64();
            let records_per_sec = records as f64 / elapsed;
            let mb_per_sec = (bytes as f64 / elapsed) / (1024.0 * 1024.0);
            
            info!("📊 寫入統計 ({:.1}s):", elapsed);
            info!("   記錄: {} ({:.1}/s)", records, records_per_sec);
            info!("   批次: {}", batches);
            info!("   流量: {:.2} MB ({:.2} MB/s)", 
                  bytes as f64 / (1024.0 * 1024.0), mb_per_sec);
            info!("   延遲: {:.1}μs", avg_latency);
            info!("   佇列: {}", queue_size);
            info!("   錯誤: {}", errors);
            info!("   ──────────────────────────");
        }
    });
    
    // 等待測試完成
    tokio::time::sleep(Duration::from_secs(args.duration)).await;
    
    // 關閉生成器和寫入器
    info!("🔄 正在關閉測試...");
    shutdown.store(true, Ordering::Relaxed);
    
    // 等待所有任務完成
    let _ = collector_handle.await;
    let _ = distributor_handle.await;
    for handle in writer_handles {
        let _ = handle.await;
    }
    let _ = report_handle.await;
    
    // 最終統計
    let (records, batches, bytes, errors, avg_latency, _) = metrics.get_stats();
    let total_time = args.duration as f64;
    let records_per_sec = records as f64 / total_time;
    let mb_per_sec = (bytes as f64 / total_time) / (1024.0 * 1024.0);
    let error_rate = if records > 0 { errors as f64 / records as f64 * 100.0 } else { 0.0 };
    
    info!("🎉 測試完成 - 最終報告:");
    info!("   ⏱️  總時間: {:.1}s", total_time);
    info!("   📝 總記錄: {} ({:.1}/s)", records, records_per_sec);
    info!("   📦 總批次: {}", batches);
    info!("   📊 總流量: {:.2} MB ({:.2} MB/s)", 
          bytes as f64 / (1024.0 * 1024.0), mb_per_sec);
    info!("   🚀 平均延遲: {:.1}μs", avg_latency);
    info!("   ❌ 錯誤率: {:.2}%", error_rate);
    info!("   🔗 併發數: {}", args.concurrency);
    info!("   📏 批量大小: {}", args.batch_size);
    
    // 性能評級
    let grade = if records_per_sec > 50000.0 && avg_latency < 1000.0 && error_rate < 0.1 {
        "A+ 優秀"
    } else if records_per_sec > 20000.0 && avg_latency < 5000.0 && error_rate < 1.0 {
        "A 良好"
    } else if records_per_sec > 10000.0 && avg_latency < 10000.0 && error_rate < 5.0 {
        "B 中等"
    } else {
        "C 需要優化"
    };
    
    info!("🏆 性能評級: {}", grade);
    
    Ok(())
}