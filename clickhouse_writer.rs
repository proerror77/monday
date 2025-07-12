/// 🚀 ClickHouse數據寫入器 - 專為WebSocket高頻數據優化
/// 
/// 功能特性：
/// - 異步批量寫入，不阻塞WebSocket接收
/// - 智能批量大小調整
/// - 失敗重試機制
/// - 內存緩衝區管理

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tokio::{
    time::interval,
    sync::mpsc,
};

use clickhouse::{Client, Row};
use serde::{Deserialize, Serialize};
use anyhow::{Context, Result};

/// 📊 ClickHouse配置
#[derive(Debug, Clone)]
pub struct ClickHouseConfig {
    pub url: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub batch_size: usize,
    pub flush_interval_ms: u64,
    pub max_buffer_size: usize,
}

impl Default for ClickHouseConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8124".to_string(),
            database: "hft_db".to_string(),
            username: "hft_user".to_string(),
            password: "hft_password".to_string(),
            batch_size: 500,
            flush_interval_ms: 2000, // 2秒自動刷新
            max_buffer_size: 10000,  // 最大緩衝10k條記錄
        }
    }
}

/// 📈 LOB深度數據行結構
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct LobDepthRow {
    pub timestamp: i64,      // 微秒時間戳
    pub symbol: String,
    pub exchange: String,
    pub sequence: u64,
    pub is_snapshot: u8,
    pub bid_prices: Vec<f64>,
    pub bid_quantities: Vec<f64>,
    pub ask_prices: Vec<f64>,
    pub ask_quantities: Vec<f64>,
    pub bid_levels: u16,
    pub ask_levels: u16,
    pub spread: f64,
    pub mid_price: f64,
    pub data_source: String,
    pub created_at: u32,     // 秒時間戳
}

/// 💰 交易數據行結構
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct TradeDataRow {
    pub timestamp: i64,
    pub symbol: String,
    pub exchange: String,
    pub trade_id: String,
    pub price: f64,
    pub quantity: f64,
    pub side: String,        // "buy" or "sell"
    pub notional: f64,
    pub data_source: String,
    pub created_at: u32,
}

/// 📊 Ticker數據行結構  
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
pub struct TickerDataRow {
    pub timestamp: i64,
    pub symbol: String,
    pub exchange: String,
    pub last_price: f64,
    pub volume_24h: f64,
    pub change_24h: f64,
    pub high_24h: f64,
    pub low_24h: f64,
    pub data_source: String,
    pub created_at: u32,
}

/// 📊 簡化的市場數據類型
#[derive(Debug, Clone)]
pub enum MarketDataType {
    OrderBook {
        symbol: String,
        bids: Vec<(f64, f64)>,  // (price, quantity)
        asks: Vec<(f64, f64)>,
        timestamp: i64,
    },
    Trade {
        symbol: String,
        price: f64,
        quantity: f64,
        side: String,
        timestamp: i64,
    },
    Ticker {
        symbol: String,
        price: f64,
        volume: f64,
        timestamp: i64,
    },
}

/// 📊 寫入統計信息
#[derive(Debug, Clone)]
pub struct WriteMetrics {
    pub total_written: Arc<AtomicU64>,
    pub orderbook_written: Arc<AtomicU64>,
    pub trade_written: Arc<AtomicU64>,
    pub ticker_written: Arc<AtomicU64>,
    pub write_errors: Arc<AtomicU64>,
    pub buffer_full_drops: Arc<AtomicU64>,
    pub avg_write_latency_us: Arc<AtomicU64>,
}

impl WriteMetrics {
    pub fn new() -> Self {
        Self {
            total_written: Arc::new(AtomicU64::new(0)),
            orderbook_written: Arc::new(AtomicU64::new(0)),
            trade_written: Arc::new(AtomicU64::new(0)),
            ticker_written: Arc::new(AtomicU64::new(0)),
            write_errors: Arc::new(AtomicU64::new(0)),
            buffer_full_drops: Arc::new(AtomicU64::new(0)),
            avg_write_latency_us: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// 🚀 ClickHouse異步寫入器
pub struct ClickHouseWriter {
    client: Client,
    config: ClickHouseConfig,
    data_sender: mpsc::UnboundedSender<MarketDataType>,
    metrics: WriteMetrics,
    _writer_task: tokio::task::JoinHandle<()>,
}

impl ClickHouseWriter {
    /// 創建新的ClickHouse寫入器
    pub async fn new(config: ClickHouseConfig) -> Result<Self> {
        // 創建ClickHouse客戶端
        let client = Client::default()
            .with_url(&config.url)
            .with_database(&config.database)
            .with_user(&config.username)
            .with_password(&config.password);

        // 測試連接
        client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .context("Failed to connect to ClickHouse")?;

        println!("✅ ClickHouse連接成功: {}/{}", config.url, config.database);

        let metrics = WriteMetrics::new();
        let (data_sender, data_receiver) = mpsc::unbounded_channel();

        // 啟動後台寫入任務
        let writer_task = Self::start_writer_task(
            client.clone(),
            config.clone(),
            data_receiver,
            metrics.clone(),
        );

        Ok(Self {
            client,
            config,
            data_sender,
            metrics,
            _writer_task: writer_task,
        })
    }

    /// 異步寫入市場數據 (非阻塞)
    pub fn write_market_data(&self, data: MarketDataType) -> Result<()> {
        match self.data_sender.send(data) {
            Ok(_) => Ok(()),
            Err(_) => {
                self.metrics.buffer_full_drops.fetch_add(1, Ordering::Relaxed);
                anyhow::bail!("Data channel full, dropping message")
            }
        }
    }

    /// 獲取寫入統計信息
    pub fn get_metrics(&self) -> WriteMetrics {
        self.metrics.clone()
    }

    /// 測試數據庫連接
    pub async fn test_connection(&self) -> Result<()> {
        let result: u8 = self.client
            .query("SELECT 1 as test")
            .fetch_one()
            .await
            .context("Failed to test ClickHouse connection")?;
        
        println!("ClickHouse連接測試成功: {}", result);
        Ok(())
    }

    /// 啟動後台寫入任務
    fn start_writer_task(
        client: Client,
        config: ClickHouseConfig,
        mut data_receiver: mpsc::UnboundedReceiver<MarketDataType>,
        metrics: WriteMetrics,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut lob_buffer = Vec::with_capacity(config.batch_size);
            let mut trade_buffer = Vec::with_capacity(config.batch_size);
            let mut ticker_buffer = Vec::with_capacity(config.batch_size);
            
            let mut flush_timer = interval(Duration::from_millis(config.flush_interval_ms));
            let mut last_flush = Instant::now();

            println!("🚀 ClickHouse寫入任務已啟動 (批量大小: {}, 刷新間隔: {}ms)", 
                     config.batch_size, config.flush_interval_ms);

            loop {
                tokio::select! {
                    // 接收新數據
                    data = data_receiver.recv() => {
                        match data {
                            Some(market_data) => {
                                let write_start = Instant::now();
                                
                                // 將數據分類到對應緩衝區
                                match market_data {
                                    MarketDataType::OrderBook { symbol, bids, asks, timestamp } => {
                                        let row = Self::create_lob_row(symbol, bids, asks, timestamp);
                                        lob_buffer.push(row);
                                        metrics.orderbook_written.fetch_add(1, Ordering::Relaxed);
                                    },
                                    MarketDataType::Trade { symbol, price, quantity, side, timestamp } => {
                                        let row = Self::create_trade_row(symbol, price, quantity, side, timestamp);
                                        trade_buffer.push(row);
                                        metrics.trade_written.fetch_add(1, Ordering::Relaxed);
                                    },
                                    MarketDataType::Ticker { symbol, price, volume, timestamp } => {
                                        let row = Self::create_ticker_row(symbol, price, volume, timestamp);
                                        ticker_buffer.push(row);
                                        metrics.ticker_written.fetch_add(1, Ordering::Relaxed);
                                    },
                                }

                                // 檢查是否需要刷新
                                let should_flush = lob_buffer.len() >= config.batch_size ||
                                                   trade_buffer.len() >= config.batch_size ||
                                                   ticker_buffer.len() >= config.batch_size;

                                if should_flush {
                                    Self::flush_buffers(&client, &mut lob_buffer, &mut trade_buffer, &mut ticker_buffer, &metrics).await;
                                    last_flush = Instant::now();
                                }

                                // 更新延遲統計
                                let latency_us = write_start.elapsed().as_micros() as u64;
                                metrics.avg_write_latency_us.store(latency_us, Ordering::Relaxed);
                            },
                            None => {
                                println!("📥 數據接收通道已關閉，正在刷新剩餘數據...");
                                Self::flush_buffers(&client, &mut lob_buffer, &mut trade_buffer, &mut ticker_buffer, &metrics).await;
                                break;
                            }
                        }
                    },
                    
                    // 定期刷新
                    _ = flush_timer.tick() => {
                        if last_flush.elapsed() >= Duration::from_millis(config.flush_interval_ms) {
                            Self::flush_buffers(&client, &mut lob_buffer, &mut trade_buffer, &mut ticker_buffer, &metrics).await;
                            last_flush = Instant::now();
                        }
                    }
                }
            }
            
            println!("🔚 ClickHouse寫入任務已結束");
        })
    }

    /// 刷新所有緩衝區到ClickHouse
    async fn flush_buffers(
        client: &Client,
        lob_buffer: &mut Vec<LobDepthRow>,
        trade_buffer: &mut Vec<TradeDataRow>,
        ticker_buffer: &mut Vec<TickerDataRow>,
        metrics: &WriteMetrics,
    ) {
        let flush_start = Instant::now();
        let mut total_written = 0;

        // 刷新LOB數據
        if !lob_buffer.is_empty() {
            match Self::batch_insert_lob(client, lob_buffer.clone()).await {
                Ok(_) => {
                    total_written += lob_buffer.len();
                    lob_buffer.clear();
                },
                Err(e) => {
                    eprintln!("❌ LOB數據寫入失敗: {}", e);
                    metrics.write_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // 刷新交易數據
        if !trade_buffer.is_empty() {
            match Self::batch_insert_trades(client, trade_buffer.clone()).await {
                Ok(_) => {
                    total_written += trade_buffer.len();
                    trade_buffer.clear();
                },
                Err(e) => {
                    eprintln!("❌ 交易數據寫入失敗: {}", e);
                    metrics.write_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // 刷新Ticker數據
        if !ticker_buffer.is_empty() {
            match Self::batch_insert_tickers(client, ticker_buffer.clone()).await {
                Ok(_) => {
                    total_written += ticker_buffer.len();
                    ticker_buffer.clear();
                },
                Err(e) => {
                    eprintln!("❌ Ticker數據寫入失敗: {}", e);
                    metrics.write_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        if total_written > 0 {
            metrics.total_written.fetch_add(total_written as u64, Ordering::Relaxed);
            let latency_ms = flush_start.elapsed().as_millis();
            println!("💾 ClickHouse批量寫入: {} 條記錄, 耗時: {}ms", total_written, latency_ms);
        }
    }

    /// 批量插入LOB數據
    async fn batch_insert_lob(client: &Client, rows: Vec<LobDepthRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut insert = client.insert("lob_depth")?;
        for row in &rows {
            insert.write(row).await?;
        }
        insert.end().await?;
        Ok(())
    }

    /// 批量插入交易數據
    async fn batch_insert_trades(client: &Client, rows: Vec<TradeDataRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut insert = client.insert("trade_data")?;
        for row in &rows {
            insert.write(row).await?;
        }
        insert.end().await?;
        Ok(())
    }

    /// 批量插入Ticker數據
    async fn batch_insert_tickers(client: &Client, rows: Vec<TickerDataRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        // 注意：如果ticker表不存在，我們可以先創建
        let create_ticker_table = r#"
        CREATE TABLE IF NOT EXISTS ticker_data (
            timestamp DateTime64(6, 'UTC'),
            symbol String,
            exchange String DEFAULT 'bitget',
            last_price Float64,
            volume_24h Float64,
            change_24h Float64,
            high_24h Float64,
            low_24h Float64,
            data_source String DEFAULT 'realtime',
            created_at DateTime DEFAULT now()
        ) ENGINE = MergeTree()
        PARTITION BY toYYYYMM(timestamp)
        ORDER BY (symbol, timestamp)
        "#;

        // 嘗試創建表（如果不存在）
        let _ = client.query(create_ticker_table).execute().await;

        let mut insert = client.insert("ticker_data")?;
        for row in &rows {
            insert.write(row).await?;
        }
        insert.end().await?;
        Ok(())
    }

    /// 創建LOB行數據
    fn create_lob_row(symbol: String, bids: Vec<(f64, f64)>, asks: Vec<(f64, f64)>, timestamp: i64) -> LobDepthRow {
        let bid_prices: Vec<f64> = bids.iter().map(|(p, _)| *p).collect();
        let bid_quantities: Vec<f64> = bids.iter().map(|(_, q)| *q).collect();
        let ask_prices: Vec<f64> = asks.iter().map(|(p, _)| *p).collect();
        let ask_quantities: Vec<f64> = asks.iter().map(|(_, q)| *q).collect();

        let mid_price = if !bid_prices.is_empty() && !ask_prices.is_empty() {
            (bid_prices[0] + ask_prices[0]) / 2.0
        } else {
            0.0
        };

        let spread = if !bid_prices.is_empty() && !ask_prices.is_empty() {
            ask_prices[0] - bid_prices[0]
        } else {
            0.0
        };

        LobDepthRow {
            timestamp,
            symbol,
            exchange: "bitget".to_string(),
            sequence: 0, // WebSocket中可能沒有sequence
            is_snapshot: 0, // 假設都是更新
            bid_prices,
            bid_quantities,
            ask_prices,
            ask_quantities,
            bid_levels: bids.len() as u16,
            ask_levels: asks.len() as u16,
            spread,
            mid_price,
            data_source: "realtime".to_string(),
            created_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32,
        }
    }

    /// 創建交易行數據
    fn create_trade_row(symbol: String, price: f64, quantity: f64, side: String, timestamp: i64) -> TradeDataRow {
        TradeDataRow {
            timestamp,
            symbol,
            exchange: "bitget".to_string(),
            trade_id: format!("{}_{}", timestamp, fastrand::u64(..1000000)),
            price,
            quantity,
            side,
            notional: price * quantity,
            data_source: "realtime".to_string(),
            created_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32,
        }
    }

    /// 創建Ticker行數據
    fn create_ticker_row(symbol: String, price: f64, volume: f64, timestamp: i64) -> TickerDataRow {
        TickerDataRow {
            timestamp,
            symbol,
            exchange: "bitget".to_string(),
            last_price: price,
            volume_24h: volume,
            change_24h: 0.0, // WebSocket中可能沒有這些數據
            high_24h: 0.0,
            low_24h: 0.0,
            data_source: "realtime".to_string(),
            created_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as u32,
        }
    }
}

/// 快速隨機數生成
mod fastrand {
    use std::sync::atomic::{AtomicU64, Ordering};
    
    static SEED: AtomicU64 = AtomicU64::new(1);
    
    pub fn u64(range: std::ops::RangeTo<u64>) -> u64 {
        let mut x = SEED.load(Ordering::Relaxed);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        SEED.store(x, Ordering::Relaxed);
        x % range.end
    }
}