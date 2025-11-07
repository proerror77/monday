mod exchanges;
mod multi_collector;

use anyhow::Result;
use clap::{Parser, Subcommand};
use multi_collector::{run_multi_collector, MultiCollectorArgs};

#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "USDT-FUTURES 專業數據收集器：四流同收，專業 schema", long_about = None)]
struct Args {
    /// ClickHouse URL（HTTP/HTTPS）
    #[arg(
        long,
        default_value = "https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"
    )]
    ch_url: String,
    /// ClickHouse 資料庫
    #[arg(long, default_value = "hft_db")]
    database: String,
    /// 每批次寫入最大筆數
    #[arg(long, default_value_t = 5000)]
    batch_size: usize,
    /// Flush 週期（毫秒）
    #[arg(long, default_value_t = 1000)]
    flush_ms: u64,
    /// 獲取 TOP-N 合約（按 24h 成交量）
    #[arg(long, default_value_t = 10)]
    top_limit: usize,
}

// USDT-FUTURES 專業數據結構，對應新 schema

#[derive(Debug, Serialize, Clone, Row)]
struct FuturesTickerRow {
    exchange_ts: u64, // timestamp in milliseconds
    local_ts: u64,    // timestamp in milliseconds
    symbol: String,
    last_price: f64,
    best_bid: f64,
    best_ask: f64,
    base_vol: f64,
    quote_vol: f64,
    open_24h: f64,
    high_24h: f64,
    low_24h: f64,
    change_24h: f64,
    change_pct_24h: f64,
}

#[derive(Debug, Serialize, Clone, Row)]
struct FuturesBboRow {
    exchange_ts: u64, // timestamp in milliseconds
    local_ts: u64,    // timestamp in milliseconds
    symbol: String,
    best_bid: f64,
    best_bid_size: f64,
    best_ask: f64,
    best_ask_size: f64,
    checksum: u32,
}

#[derive(Debug, Serialize, Clone, Row)]
struct FuturesDepthRow {
    exchange_ts: u64, // timestamp in milliseconds
    local_ts: u64,    // timestamp in milliseconds
    symbol: String,
    bids: Vec<(f64, f64)>, // Array(Tuple(Float64, Float64))
    asks: Vec<(f64, f64)>, // Array(Tuple(Float64, Float64))
    checksum: u32,
}

#[derive(Debug, Serialize, Clone, Row)]
struct FuturesTradesRow {
    exchange_ts: u64, // timestamp in milliseconds
    local_ts: u64,    // timestamp in milliseconds
    symbol: String,
    trade_id: String,
    price: f64,
    size: f64, // 修復: qty -> size 匹配表結構
    side: u8,  // 修復: 改為 u8 匹配 Enum8('buy' = 1, 'sell' = 2)
}

#[derive(Debug, Clone)]
struct BatchCollector {
    ticker: Vec<FuturesTickerRow>,
    bbo: Vec<FuturesBboRow>,
    depth: Vec<FuturesDepthRow>,
    trades: Vec<FuturesTradesRow>,
    batch_size: usize,
}

impl BatchCollector {
    fn new(batch_size: usize) -> Self {
        Self {
            ticker: Vec::new(),
            bbo: Vec::new(),
            depth: Vec::new(),
            trades: Vec::new(),
            batch_size,
        }
    }

    async fn flush_if_needed(&mut self, client: &ChClient) -> Result<()> {
        if self.ticker.len() >= self.batch_size {
            self.flush_ticker(client).await?;
        }
        if self.bbo.len() >= self.batch_size {
            self.flush_bbo(client).await?;
        }
        if self.depth.len() >= self.batch_size {
            self.flush_depth(client).await?;
        }
        if self.trades.len() >= self.batch_size {
            self.flush_trades(client).await?;
        }
        Ok(())
    }

    async fn flush_ticker(&mut self, client: &ChClient) -> Result<()> {
        if !self.ticker.is_empty() {
            let mut ins = client.insert::<FuturesTickerRow>("bitget_orderbook")?;
            for row in &self.ticker {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 ticker 數據", self.ticker.len());
            self.ticker.clear();
        }
        Ok(())
    }

    async fn flush_bbo(&mut self, client: &ChClient) -> Result<()> {
        if !self.bbo.is_empty() {
            let mut ins = client.insert::<FuturesBboRow>("bitget_orderbook")?;
            for row in &self.bbo {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 bbo 數據", self.bbo.len());
            self.bbo.clear();
        }
        Ok(())
    }

    async fn flush_depth(&mut self, client: &ChClient) -> Result<()> {
        if !self.depth.is_empty() {
            let mut ins = client.insert::<FuturesDepthRow>("bitget_orderbook")?;
            for row in &self.depth {
                ins.write(row).await?;
            }
            ins.end().await?;
            info!("已寫入 {} 條 depth 數據", self.depth.len());
            self.depth.clear();
        }
        Ok(())
    }

    async fn flush_trades(&mut self, client: &ChClient) -> Result<()> {
        if !self.trades.is_empty() {
            let mut ins = client.insert::<FuturesTradesRow>("bitget_trades")?;
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
        self.flush_bbo(client).await?;
        self.flush_depth(client).await?;
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
    let top_symbols: Vec<String> = contracts
        .into_iter()
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
        parts.push(bids[i].0.clone()); // bid price
        parts.push(bids[i].1.clone()); // bid qty
        parts.push(asks[i].0.clone()); // ask price
        parts.push(asks[i].1.clone()); // ask qty
    }

    let data_str = parts.join(":");
    let mut hasher = Crc32::new();
    hasher.update(data_str.as_bytes());
    hasher.finalize()
}

// 快速獲取當前時間戳（毫秒）- HFT 優化版本
#[inline]
fn fast_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// 解析時間戳為 u64 毫秒
fn parse_timestamp(ts_str: &str) -> u64 {
    ts_str
        .parse::<u64>()
        .unwrap_or_else(|_| fast_timestamp_millis())
}

// 安全解析 f64
fn parse_f64(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
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

        // 3. books (深度) - v2 API uses "books" channel
        subscribe_msgs.push(serde_json::json!({
            "op": "subscribe",
            "args": [{"instType": "USDT-FUTURES", "channel": "books", "instId": symbol}]
        }));

        // 4. trade - 正確的頻道名稱
        subscribe_msgs.push(serde_json::json!({
            "op": "subscribe",
            "args": [{"instType": "USDT-FUTURES", "channel": "trade", "instId": symbol}]
        }));
    }

    // 發送所有訂閱 - 增加延遲避免rate limit
    for (i, msg) in subscribe_msgs.iter().enumerate() {
        info!(
            "發送訂閱 {}/{}: {}",
            i + 1,
            subscribe_msgs.len(),
            msg.to_string()
        );
        write.send(Message::Text(msg.to_string())).await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await; // 增加到200ms間隔
    }

    // 定時 flush 任務
    let flush_interval = tokio::time::Duration::from_millis(1000); // 1秒
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

async fn process_message(
    text: &str,
    collector: &mut BatchCollector,
    client: &ChClient,
) -> Result<()> {
    let json: JsonValue = serde_json::from_str(text)?;
    let now = fast_timestamp_millis();

    // 解析 channel 和 symbol
    if let Some(arg) = json.get("arg") {
        let channel = arg.get("channel").and_then(|c| c.as_str()).unwrap_or("");
        let symbol = arg
            .get("instId")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        // Debug log for unknown channels
        if !["ticker", "books1", "books", "trade"].contains(&channel) && !channel.is_empty() {
            info!(
                "未知頻道: {} for symbol: {}, 完整消息: {}",
                channel, symbol, text
            );
        }

        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            for item in data {
                match channel {
                    "ticker" => {
                        process_ticker(item, &symbol, now, collector).await?;
                    }
                    "books1" => {
                        process_bbo(item, &symbol, now, collector).await?;
                    }
                    "books" => {
                        process_depth(item, &symbol, now, collector).await?;
                    }
                    "trade" => {
                        process_trade(item, &symbol, now, collector).await?;
                    }
                    _ => {
                        if !channel.is_empty() {
                            info!("未處理的頻道 '{}', symbol: {}", channel, symbol);
                        }
                    }
                }
            }
        }
    } else {
        // Handle messages without 'arg' field - subscription responses and errors
        if text.contains("\"event\":\"subscribe\"") {
            info!("訂閱確認: {}", text);
        } else if text.contains("\"event\":\"error\"") {
            warn!("訂閱錯誤: {}", text);
        } else if !text.contains("pong") {
            info!(
                "無 arg 字段的消息: {}",
                text.chars().take(200).collect::<String>()
            );
        }
    }

    collector.flush_if_needed(client).await?;
    Ok(())
}

async fn process_ticker(
    data: &JsonValue,
    symbol: &str,
    local_ts: u64,
    collector: &mut BatchCollector,
) -> Result<()> {
    let exchange_ts = data
        .get("ts")
        .and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let row = FuturesTickerRow {
        exchange_ts,
        local_ts,
        symbol: symbol.to_string(),
        last_price: data
            .get("lastPr")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        best_bid: data
            .get("bidPr")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        best_ask: data
            .get("askPr")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        base_vol: data
            .get("baseVolume")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        quote_vol: data
            .get("quoteVolume")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        open_24h: data
            .get("open24h")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        high_24h: data
            .get("high24h")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        low_24h: data
            .get("low24h")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        change_24h: data
            .get("change24h")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
        change_pct_24h: data
            .get("change24h")
            .and_then(|v| v.as_str())
            .map(|s| parse_f64(s))
            .unwrap_or(0.0),
    };

    collector.ticker.push(row);
    Ok(())
}

async fn process_bbo(
    data: &JsonValue,
    symbol: &str,
    local_ts: u64,
    collector: &mut BatchCollector,
) -> Result<()> {
    let exchange_ts = data
        .get("ts")
        .and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let checksum = data.get("checksum").and_then(|c| c.as_u64()).unwrap_or(0) as u32;

    if let (Some(bids), Some(asks)) = (
        data.get("bids").and_then(|b| b.as_array()),
        data.get("asks").and_then(|a| a.as_array()),
    ) {
        if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
            let best_bid_price = best_bid
                .get(0)
                .and_then(|p| p.as_str())
                .map(|s| parse_f64(s))
                .unwrap_or(0.0);
            let best_bid_size = best_bid
                .get(1)
                .and_then(|q| q.as_str())
                .map(|s| parse_f64(s))
                .unwrap_or(0.0);
            let best_ask_price = best_ask
                .get(0)
                .and_then(|p| p.as_str())
                .map(|s| parse_f64(s))
                .unwrap_or(0.0);
            let best_ask_size = best_ask
                .get(1)
                .and_then(|q| q.as_str())
                .map(|s| parse_f64(s))
                .unwrap_or(0.0);

            let row = FuturesBboRow {
                exchange_ts,
                local_ts,
                symbol: symbol.to_string(),
                best_bid: best_bid_price,
                best_bid_size,
                best_ask: best_ask_price,
                best_ask_size,
                checksum,
            };

            collector.bbo.push(row);
        }
    }

    Ok(())
}

async fn process_depth(
    data: &JsonValue,
    symbol: &str,
    local_ts: u64,
    collector: &mut BatchCollector,
) -> Result<()> {
    let exchange_ts = data
        .get("ts")
        .and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let checksum = data.get("checksum").and_then(|c| c.as_u64()).unwrap_or(0) as u32;

    let mut bids = Vec::new();
    let mut asks = Vec::new();

    // 處理 bids - 存儲為 Vec<(f64, f64)>
    if let Some(bids_array) = data.get("bids").and_then(|b| b.as_array()) {
        for bid in bids_array {
            if let Some(bid_array) = bid.as_array() {
                if bid_array.len() >= 2 {
                    let price = bid_array[0].as_str().map(|s| parse_f64(s)).unwrap_or(0.0);
                    let qty = bid_array[1].as_str().map(|s| parse_f64(s)).unwrap_or(0.0);
                    bids.push((price, qty));
                }
            }
        }
    }

    // 處理 asks - 存儲為 Vec<(f64, f64)>
    if let Some(asks_array) = data.get("asks").and_then(|a| a.as_array()) {
        for ask in asks_array {
            if let Some(ask_array) = ask.as_array() {
                if ask_array.len() >= 2 {
                    let price = ask_array[0].as_str().map(|s| parse_f64(s)).unwrap_or(0.0);
                    let qty = ask_array[1].as_str().map(|s| parse_f64(s)).unwrap_or(0.0);
                    asks.push((price, qty));
                }
            }
        }
    }

    // 只有在有數據時才創建行
    if !bids.is_empty() || !asks.is_empty() {
        let row = FuturesDepthRow {
            exchange_ts,
            local_ts,
            symbol: symbol.to_string(),
            bids,
            asks,
            checksum,
        };

        collector.depth.push(row);
    }

    Ok(())
}

async fn process_trade(
    data: &JsonValue,
    symbol: &str,
    local_ts: u64,
    collector: &mut BatchCollector,
) -> Result<()> {
    let exchange_ts = data
        .get("ts")
        .and_then(|t| t.as_str())
        .map(|t| parse_timestamp(t))
        .unwrap_or(local_ts);

    let trade_id = data
        .get("tradeId")
        .and_then(|id| id.as_str())
        .unwrap_or("")
        .to_string();
    let price = data
        .get("price")
        .and_then(|p| p.as_str())
        .map(|s| parse_f64(s))
        .unwrap_or(0.0);
    let qty = data
        .get("size")
        .and_then(|q| q.as_str())
        .map(|s| parse_f64(s))
        .unwrap_or(0.0);
    let side_str = data.get("side").and_then(|s| s.as_str()).unwrap_or("");

    // 將字符串轉換為枚舉數值: "buy" = 1, "sell" = 2
    let side = match side_str {
        "buy" => 1u8,
        "sell" => 2u8,
        _ => 0u8, // 默認值，表示未知
    };

    let row = FuturesTradesRow {
        exchange_ts,
        local_ts,
        symbol: symbol.to_string(),
        trade_id,
        price,
        size: qty, // 將 qty 賦值給 size 字段
        side,
    };

    collector.trades.push(row);
    Ok(())
}

async fn verify_tables(client: &ChClient) -> Result<()> {
    // 验证表是否存在，无需创建，因为已经在 ClickHouse Cloud 中创建
    let tables = vec!["bitget_orderbook", "bitget_trades"];

    for table_name in tables {
        info!("验证表是否存在: {}", table_name);
        let result = client
            .query(&format!("SHOW TABLES FROM hft_db LIKE '{}'", table_name))
            .fetch_optional::<String>()
            .await;

        match result {
            Ok(Some(_)) => info!("✅ 表 {} 存在", table_name),
            Ok(None) => warn!("⚠️  表 {} 不存在，数据收集可能失败", table_name),
            Err(e) => warn!("检查表 {} 时出错: {}", table_name, e),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize rustls crypto provider
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("Failed to install rustls crypto provider"))?;

    tracing_subscriber::fmt::init();
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

    // 验证必要的資料表
    info!("正在验证 ClickHouse 資料表...");
    verify_tables(&client).await?;

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
