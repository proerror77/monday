use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde_json::Value as JsonValue;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn, error};
use clickhouse::Client as ChClient;
use std::collections::HashMap;
use chrono::{DateTime, Utc};
use crc32fast::Hasher as Crc32;
use rust_decimal::Decimal;
use serde::Serialize;
use std::str::FromStr;
use anyhow::Result;

#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "USDT-FUTURES 專業數據收集器：四流同收，專業 schema", long_about = None)]
struct Args {
    /// ClickHouse URL（HTTP/HTTPS）
    #[arg(long, default_value = "https://ivigyu08to.ap-northeast-1.aws.clickhouse.cloud:8443")]
    ch_url: String,
    /// ClickHouse 資料庫
    #[arg(long, default_value = "hft")]
    database: String,
    /// 每批次寫入最大筆數
    #[arg(long, default_value_t = 5000)]
    batch_size: usize,
    /// Flush 週期（毫秒）
    #[arg(long, default_value_t = 1000)]
    flush_ms: u64,
    /// 獲取 TOP-N 合約（按 24h 成交量）
    #[arg(long, default_value_t = 30)]
    top_limit: usize,
}

// USDT-FUTURES 專業數據結構，對應新 schema

#[derive(Debug, Serialize)]
struct FuturesTickerRow {
    exchange_ts: DateTime<Utc>,
    local_ts: DateTime<Utc>,
    symbol: String,
    last_price: Decimal,
    best_bid: Decimal,
    best_ask: Decimal,
    base_vol: Decimal,
    quote_vol: Decimal,
    open_24h: Decimal,
    high_24h: Decimal,
    low_24h: Decimal,
    change_24h: Decimal,
    change_pct_24h: Decimal,
}

#[derive(Debug, Serialize)]
struct FuturesBooks1Row {
    exchange_ts: DateTime<Utc>,
    local_ts: DateTime<Utc>,
    symbol: String,
    bid_price: Decimal,
    bid_qty: Decimal,
    ask_price: Decimal,
    ask_qty: Decimal,
    checksum: u32,
}

#[derive(Debug, Serialize)]
struct FuturesBooksRow {
    exchange_ts: DateTime<Utc>,
    local_ts: DateTime<Utc>,
    symbol: String,
    side: String,  // "bid" or "ask"
    price: Decimal,
    qty: Decimal,
    level: u8,
    checksum: u32,
}

#[derive(Debug, Serialize)]
struct FuturesTradesRow {
    exchange_ts: DateTime<Utc>,
    local_ts: DateTime<Utc>,
    symbol: String,
    trade_id: String,
    price: Decimal,
    qty: Decimal,
    side: String,  // "buy" or "sell"
}

#[derive(Debug, Clone)]
struct BatchCollector {
    ticker: Vec<FuturesTickerRow>,
    books1: Vec<FuturesBooks1Row>,
    books: Vec<FuturesBooksRow>,
    trades: Vec<FuturesTradesRow>,
    batch_size: usize,
}

impl BatchCollector {
    fn new(batch_size: usize) -> Self {
        Self {
            ticker: Vec::new(),
            books1: Vec::new(),
            books: Vec::new(),
            trades: Vec::new(),
            batch_size,
        }
    }

    async fn flush_if_needed(&mut self, client: &ChClient) -> Result<()> {
        if self.ticker.len() >= self.batch_size {
            self.flush_ticker(client).await?;
        }
        if self.books1.len() >= self.batch_size {
            self.flush_books1(client).await?;
        }
        if self.books.len() >= self.batch_size {
            self.flush_books(client).await?;
        }
        if self.trades.len() >= self.batch_size {
            self.flush_trades(client).await?;
        }
        Ok(())
    }

    async fn flush_ticker(&mut self, client: &ChClient) -> Result<()> {
        if !self.ticker.is_empty() {
            let mut ins = client.insert::<FuturesTickerRow>("bitget_usdt_futures_ticker")?;
            for row in &self.ticker {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 ticker 數據", self.ticker.len());
            self.ticker.clear();
        }
        Ok(())
    }

    async fn flush_books1(&mut self, client: &ChClient) -> Result<()> {
        if !self.books1.is_empty() {
            let mut ins = client.insert::<FuturesBooks1Row>("bitget_usdt_futures_books1")?;
            for row in &self.books1 {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 books1 數據", self.books1.len());
            self.books1.clear();
        }
        Ok(())
    }

    async fn flush_books(&mut self, client: &ChClient) -> Result<()> {
        if !self.books.is_empty() {
            let mut ins = client.insert::<FuturesBooksRow>("bitget_usdt_futures_books")?;
            for row in &self.books {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 books 數據", self.books.len());
            self.books.clear();
        }
        Ok(())
    }

    async fn flush_trades(&mut self, client: &ChClient) -> Result<()> {
        if !self.trades.is_empty() {
            let mut ins = client.insert::<FuturesTradesRow>("bitget_usdt_futures_trades")?;
            for row in &self.trades {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 trades 數據", self.trades.len());
            self.trades.clear();
        }
        Ok(())
    }

    async fn flush_all(&mut self, client: &ChClient) -> Result<()> {
        self.flush_ticker(client).await?;
        self.flush_books1(client).await?;
        self.flush_books(client).await?;
        self.flush_trades(client).await?;
        Ok(())
    }
}

// 獲取 TOP-N USDT-FUTURES 合約
async fn fetch_top_usdt_futures(limit: usize) -> Result<Vec<String>> {
    let client = reqwest::Client::new();
    let url = "https://api.bitget.com/api/v2/mix/market/tickers?productType=USDT-FUTURES";

    let response = client.get(url).send().await?;
    let json: JsonValue = response.json().await?;

    let mut contracts = Vec::new();
    if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
        for item in data {
            if let Some(symbol) = item.get("symbol").and_then(|s| s.as_str()) {
                if let Some(quote_vol_str) = item.get("quoteVolume").and_then(|q| q.as_str()) {
                    if let Ok(quote_vol) = quote_vol_str.parse::<f64>() {
                        contracts.push((symbol.to_string(), quote_vol));
                    }
                }
            }
        }
    }

    // 按成交量排序，取 TOP-N
    contracts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_symbols: Vec<String> = contracts.into_iter()
        .take(limit)
        .map(|(symbol, _)| symbol)
        .collect();

    info!("獲取到 TOP-{} USDT-FUTURES 合約: {:?}", limit, top_symbols);
    Ok(top_symbols)
}

// 計算 CRC32 checksum
fn compute_crc32_from_books(bids: &[(String, String)], asks: &[(String, String)]) -> u32 {
    let mut parts = Vec::new();
    let max_levels = std::cmp::min(25, std::cmp::min(bids.len(), asks.len()));

    for i in 0..max_levels {
        parts.push(bids[i].0.clone());  // bid price
        parts.push(bids[i].1.clone());  // bid qty
        parts.push(asks[i].0.clone());  // ask price
        parts.push(asks[i].1.clone());  // ask qty
    }

    let data_str = parts.join(":");
    let mut hasher = Crc32::new();
    hasher.update(data_str.as_bytes());
    hasher.finalize()
}

// 解析時間戳為 DateTime<Utc>
fn parse_timestamp(ts_str: &str) -> DateTime<Utc> {
    if let Ok(ts_ms) = ts_str.parse::<i64>() {
        if let Some(datetime) = DateTime::from_timestamp_millis(ts_ms) {
            return datetime;
        }
    }
    Utc::now()
}

// 安全解析 Decimal
fn parse_decimal(s: &str) -> Decimal {
    Decimal::from_str(s).unwrap_or_else(|_| Decimal::ZERO)
}

async fn bitget_usdt_futures_stream(
    symbols: Vec<String>,
    client: ChClient,
    batch_size: usize,
) -> Result<()> {
    info!("開始 Bitget USDT-FUTURES 數據收集，symbols: {:?}", symbols);

    let mut batch_collector = BatchCollector::new(batch_size);
    let url = "wss://ws.bitget.com/v2/ws/public";

    let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // 構建訂閱消息 - 四個數據流
    let mut subscribe_msgs = Vec::new();
    for symbol in &symbols {
        // 1. ticker
        subscribe_msgs.push(serde_json::json!({
            "op": "subscribe",
            "args": [{"instType": "USDT-FUTURES", "channel": "ticker", "instId": symbol}]
        }));

        // 2. books1 (BBO)
        subscribe_msgs.push(serde_json::json!({
            "op": "subscribe",
            "args": [{"instType": "USDT-FUTURES", "channel": "books1", "instId": symbol}]
        }));

        // 3. books (深度)
        subscribe_msgs.push(serde_json::json!({
            "op": "subscribe",
            "args": [{"instType": "USDT-FUTURES", "channel": "books", "instId": symbol}]
        }));

        // 4. trade
        subscribe_msgs.push(serde_json::json!({
            "op": "subscribe",
            "args": [{"instType": "USDT-FUTURES", "channel": "trade", "instId": symbol}]
        }));
    }

    // 發送所有訂閱
    for msg in subscribe_msgs {
        write.send(Message::Text(msg.to_string())).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // 定時 flush 任務
    let flush_interval = tokio::time::Duration::from_millis(1000);  // 1秒
    let mut flush_timer = tokio::time::interval(flush_interval);

    loop {
        tokio::select! {
            _ = flush_timer.tick() => {
                if let Err(e) = batch_collector.flush_all(&client).await {
                    error!("定時 flush 失敗: {}", e);
                }
            }

            message = read.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(e) = process_message(&text, &mut batch_collector, &client).await {
                            warn!("處理消息失敗: {}", e);
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // 處理 pong 回應
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        error!("WebSocket 錯誤: {}", e);
                        break;
                    }
                    None => {
                        warn!("WebSocket 連接關閉");
                        break;
                    }
                }
            }
        }
    }

    // 最後 flush
    batch_collector.flush_all(&client).await?;
    Ok(())
}

async fn process_message(text: &str, collector: &mut BatchCollector, client: &ChClient) -> Result<()> {
    let json: JsonValue = serde_json::from_str(text)?;
    let now = Utc::now();

    // 解析 channel 和 symbol
    if let Some(arg) = json.get("arg") {
        let channel = arg.get("channel").and_then(|c| c.as_str()).unwrap_or("");
        let symbol = arg.get("instId").and_then(|s| s.as_str()).unwrap_or("").to_string();

        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            for item in data {
                match channel {
                    "ticker" => {
                        process_ticker(item, &symbol, now, collector).await?;
                    }
                    "books1" => {
                        process_books1(item, &symbol, now, collector).await?;
                    }
                    "books" => {
                        process_books(item, &symbol, now, collector).await?;
                    }
                    "trade" => {
                        process_trade(item, &symbol, now, collector).await?;
                    }
                    _ => {}
                }
            }
        }
    }

    collector.flush_if_needed(client).await?;
    Ok(())
}

async fn process_ticker(data: &JsonValue, symbol: &str, local_ts: DateTime<Utc>, collector: &mut BatchCollector) -> Result<()> {
    let exchange_ts = data.get("ts").and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let row = FuturesTickerRow {
        exchange_ts,
        local_ts,
        symbol: symbol.to_string(),
        last_price: data.get("last").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        best_bid: data.get("bestBid").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        best_ask: data.get("bestAsk").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        base_vol: data.get("baseVol").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        quote_vol: data.get("quoteVol").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        open_24h: data.get("open").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        high_24h: data.get("high").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        low_24h: data.get("low").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        change_24h: data.get("change").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
        change_pct_24h: data.get("changePercent").and_then(|v| v.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO),
    };

    collector.ticker.push(row);
    Ok(())
}

async fn process_books1(data: &JsonValue, symbol: &str, local_ts: DateTime<Utc>, collector: &mut BatchCollector) -> Result<()> {
    let exchange_ts = data.get("ts").and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let checksum = data.get("checksum").and_then(|c| c.as_u64()).unwrap_or(0) as u32;

    if let (Some(bids), Some(asks)) = (
        data.get("bids").and_then(|b| b.as_array()),
        data.get("asks").and_then(|a| a.as_array())
    ) {
        if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
            let bid_price = best_bid.get(0).and_then(|p| p.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
            let bid_qty = best_bid.get(1).and_then(|q| q.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
            let ask_price = best_ask.get(0).and_then(|p| p.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
            let ask_qty = best_ask.get(1).and_then(|q| q.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);

            let row = FuturesBooks1Row {
                exchange_ts,
                local_ts,
                symbol: symbol.to_string(),
                bid_price,
                bid_qty,
                ask_price,
                ask_qty,
                checksum,
            };

            collector.books1.push(row);
        }
    }

    Ok(())
}

async fn process_books(data: &JsonValue, symbol: &str, local_ts: DateTime<Utc>, collector: &mut BatchCollector) -> Result<()> {
    let exchange_ts = data.get("ts").and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let checksum = data.get("checksum").and_then(|c| c.as_u64()).unwrap_or(0) as u32;

    // 處理 bids
    if let Some(bids) = data.get("bids").and_then(|b| b.as_array()) {
        for (level, bid) in bids.iter().enumerate() {
            if let Some(bid_array) = bid.as_array() {
                if bid_array.len() >= 2 {
                    let price = bid_array[0].as_str().map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
                    let qty = bid_array[1].as_str().map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);

                    let row = FuturesBooksRow {
                        exchange_ts,
                        local_ts,
                        symbol: symbol.to_string(),
                        side: "bid".to_string(),
                        price,
                        qty,
                        level: level as u8 + 1,
                        checksum,
                    };

                    collector.books.push(row);
                }
            }
        }
    }

    // 處理 asks
    if let Some(asks) = data.get("asks").and_then(|a| a.as_array()) {
        for (level, ask) in asks.iter().enumerate() {
            if let Some(ask_array) = ask.as_array() {
                if ask_array.len() >= 2 {
                    let price = ask_array[0].as_str().map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
                    let qty = ask_array[1].as_str().map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);

                    let row = FuturesBooksRow {
                        exchange_ts,
                        local_ts,
                        symbol: symbol.to_string(),
                        side: "ask".to_string(),
                        price,
                        qty,
                        level: level as u8 + 1,
                        checksum,
                    };

                    collector.books.push(row);
                }
            }
        }
    }

    Ok(())
}

async fn process_trade(data: &JsonValue, symbol: &str, local_ts: DateTime<Utc>, collector: &mut BatchCollector) -> Result<()> {
    let exchange_ts = data.get("ts").and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let trade_id = data.get("tradeId").and_then(|id| id.as_str()).unwrap_or("").to_string();
    let price = data.get("px").and_then(|p| p.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
    let qty = data.get("sz").and_then(|q| q.as_str()).map(|s| parse_decimal(s)).unwrap_or(Decimal::ZERO);
    let side = data.get("side").and_then(|s| s.as_str()).unwrap_or("").to_string();

    let row = FuturesTradesRow {
        exchange_ts,
        local_ts,
        symbol: symbol.to_string(),
        trade_id,
        price,
        qty,
        side,
    };

    collector.trades.push(row);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::init();
    let args = Args::parse();

    // 建立 ClickHouse 客戶端
    let mut client = clickhouse::Client::default()
        .with_url(&args.ch_url)
        .with_database(&args.database);

    // 如果提供了用戶名和密碼，則添加認證
    if let Ok(user) = std::env::var("CLICKHOUSE_USER") {
        client = client.with_user(user);
    }
    if let Ok(password) = std::env::var("CLICKHOUSE_PASSWORD") {
        client = client.with_password(password);
    }

    info!("正在獲取 TOP-{} USDT-FUTURES 合約...", args.top_limit);
    let symbols = fetch_top_usdt_futures(args.top_limit).await?;

    if symbols.is_empty() {
        error!("未獲取到任何合約，退出");
        return Ok(());
    }

    info!("開始收集數據，目標合約: {:?}", symbols);

    // 啟動數據收集
    bitget_usdt_futures_stream(symbols, client, args.batch_size).await?;

    Ok(())
}