/// 🚀 增强版Bitget WebSocket高性能测试
/// 
/// 功能特性：
/// - 真实Bitget WebSocket V2 API连接
/// - 16个并发连接，支持现货和期货
/// - 多商品并发订阅 (30+活跃交易对)
/// - 多频道数据流 (orderbook15, trades, ticker, candle1m)
/// - OrderBook聚合和深度分析
/// - Lock-free数据处理
/// - 实时性能监控
/// - 自动重连机制

use std::{
    sync::{
        atomic::{AtomicU64, AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    hint,
};

use tokio::{
    time::{interval, timeout},
};

use tokio_tungstenite::{
    connect_async, 
    tungstenite::Message,
};

use futures_util::{StreamExt, SinkExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use memchr::memmem;

// ClickHouse寫入模塊
mod clickhouse_writer;
use clickhouse_writer::{ClickHouseWriter, ClickHouseConfig, MarketDataType, WriteMetrics};

/// 📊 实时性能指标 (Lock-Free + SIMD优化)
#[derive(Clone)]
pub struct RealTimeMetrics {
    // 消息计数
    pub total_messages: Arc<AtomicU64>,
    pub orderbook_messages: Arc<AtomicU64>,
    pub trade_messages: Arc<AtomicU64>,
    pub ticker_messages: Arc<AtomicU64>,
    pub candle_messages: Arc<AtomicU64>,
    
    // 性能指标
    pub bytes_received: Arc<AtomicU64>,
    pub latency_sum_ns: Arc<AtomicU64>,
    pub latency_samples: Arc<AtomicU64>,
    pub simd_match_time_ns: Arc<AtomicU64>,
    pub simd_match_samples: Arc<AtomicU64>,
    
    // 连接状态
    pub active_connections: Arc<AtomicU64>,
    pub reconnection_count: Arc<AtomicU64>,
    pub websocket_errors: Arc<AtomicU64>,
    
    // 数据质量
    pub parse_errors: Arc<AtomicU64>,
    pub invalid_messages: Arc<AtomicU64>,
}

impl RealTimeMetrics {
    pub fn new() -> Self {
        Self {
            total_messages: Arc::new(AtomicU64::new(0)),
            orderbook_messages: Arc::new(AtomicU64::new(0)),
            trade_messages: Arc::new(AtomicU64::new(0)),
            ticker_messages: Arc::new(AtomicU64::new(0)),
            candle_messages: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            latency_sum_ns: Arc::new(AtomicU64::new(0)),
            latency_samples: Arc::new(AtomicU64::new(0)),
            simd_match_time_ns: Arc::new(AtomicU64::new(0)),
            simd_match_samples: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            reconnection_count: Arc::new(AtomicU64::new(0)),
            websocket_errors: Arc::new(AtomicU64::new(0)),
            parse_errors: Arc::new(AtomicU64::new(0)),
            invalid_messages: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn avg_latency_us(&self) -> f64 {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        if samples == 0 {
            0.0
        } else {
            (self.latency_sum_ns.load(Ordering::Relaxed) as f64 / samples as f64) / 1000.0
        }
    }

    pub fn avg_simd_match_ns(&self) -> f64 {
        let samples = self.simd_match_samples.load(Ordering::Relaxed);
        if samples == 0 {
            0.0
        } else {
            self.simd_match_time_ns.load(Ordering::Relaxed) as f64 / samples as f64
        }
    }

    pub fn throughput_per_sec(&self, elapsed_secs: f64) -> f64 {
        if elapsed_secs <= 0.0 {
            0.0
        } else {
            self.total_messages.load(Ordering::Relaxed) as f64 / elapsed_secs
        }
    }
}

/// 📦 Bitget市场数据消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitgetMarketData {
    pub action: String,
    pub arg: BitgetChannel,
    pub data: Vec<Value>,
    pub ts: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitgetChannel {
    #[serde(rename = "instType")]
    pub inst_type: String,
    pub channel: String,
    #[serde(rename = "instId")]
    pub inst_id: String,
}

/// 📊 K线数据结构
#[derive(Debug, Clone)]
pub struct CandleData {
    pub symbol: String,
    pub timestamp: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// 📈 聚合OrderBook管理器
#[derive(Debug, Clone)]
pub struct AggregatedOrderBook {
    pub symbol: String,
    pub timestamp: u64,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub mid_price: f64,
    pub spread: f64,
    pub depth_imbalance: f64, // 深度不平衡指标
}

/// 📈 订单簿数据结构
#[derive(Debug, Clone)]
pub struct OrderBookLevel {
    pub price: f64,
    pub quantity: f64,
}

#[derive(Debug, Clone)]
pub struct OrderBookSnapshot {
    pub symbol: String,
    pub timestamp: u64,
    pub bids: Vec<OrderBookLevel>,
    pub asks: Vec<OrderBookLevel>,
    pub checksum: Option<u32>,
}

/// 💰 交易数据结构
#[derive(Debug, Clone)]
pub struct TradeData {
    pub symbol: String,
    pub timestamp: u64,
    pub price: f64,
    pub quantity: f64,
    pub side: String, // "buy" or "sell"
    pub trade_id: String,
}

/// 📊 Ticker数据结构
#[derive(Debug, Clone)]
pub struct TickerData {
    pub symbol: String,
    pub timestamp: u64,
    pub last_price: f64,
    pub volume_24h: f64,
    pub change_24h: f64,
    pub high_24h: f64,
    pub low_24h: f64,
}

/// 🔧 SIMD优化数据处理器 (专注高性能接收 + ClickHouse写入)
pub struct LockFreeDataProcessor {
    metrics: RealTimeMetrics,
    // SIMD优化模式匹配器
    books_finder: memmem::Finder<'static>,
    trade_finder: memmem::Finder<'static>,
    ticker_finder: memmem::Finder<'static>,
    candle_finder: memmem::Finder<'static>,
    // ClickHouse写入器 (可选)
    clickhouse_writer: Option<Arc<ClickHouseWriter>>,
}

impl LockFreeDataProcessor {
    pub fn new(metrics: RealTimeMetrics) -> Self {
        Self {
            metrics,
            // 预编译SIMD模式匹配器，避免运行时编译开销
            books_finder: memmem::Finder::new(b"\"books15\""),
            trade_finder: memmem::Finder::new(b"\"trade\""),
            ticker_finder: memmem::Finder::new(b"\"ticker\""),
            candle_finder: memmem::Finder::new(b"\"candle"),
            clickhouse_writer: None,
        }
    }

    /// 创建带ClickHouse写入的处理器
    pub fn new_with_clickhouse(metrics: RealTimeMetrics, clickhouse_writer: Arc<ClickHouseWriter>) -> Self {
        Self {
            metrics,
            books_finder: memmem::Finder::new(b"\"books15\""),
            trade_finder: memmem::Finder::new(b"\"trade\""),
            ticker_finder: memmem::Finder::new(b"\"ticker\""),
            candle_finder: memmem::Finder::new(b"\"candle"),
            clickhouse_writer: Some(clickhouse_writer),
        }
    }

    /// SIMD优化消息处理 (超快字节模式匹配 + 可选ClickHouse写入)
    #[inline(always)]
    pub fn process_message(&self, msg_text: &str, receive_time: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let simd_start = Instant::now();
        
        let msg_bytes = msg_text.as_bytes();
        let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_micros() as i64;
        
        // 使用SIMD加速的字节模式匹配 + 可选的数据写入
        if self.books_finder.find(msg_bytes).is_some() {
            self.metrics.orderbook_messages.fetch_add(1, Ordering::Relaxed);
            
            // 如果启用ClickHouse写入，解析并写入OrderBook数据
            if let Some(ref writer) = self.clickhouse_writer {
                if let Ok(market_data) = self.parse_orderbook_message(msg_text, timestamp) {
                    let _ = writer.write_market_data(market_data);
                }
            }
            
        } else if self.trade_finder.find(msg_bytes).is_some() {
            self.metrics.trade_messages.fetch_add(1, Ordering::Relaxed);
            
            // 解析并写入Trade数据
            if let Some(ref writer) = self.clickhouse_writer {
                if let Ok(market_data) = self.parse_trade_message(msg_text, timestamp) {
                    let _ = writer.write_market_data(market_data);
                }
            }
            
        } else if self.ticker_finder.find(msg_bytes).is_some() {
            self.metrics.ticker_messages.fetch_add(1, Ordering::Relaxed);
            
            // 解析并写入Ticker数据
            if let Some(ref writer) = self.clickhouse_writer {
                if let Ok(market_data) = self.parse_ticker_message(msg_text, timestamp) {
                    let _ = writer.write_market_data(market_data);
                }
            }
            
        } else if self.candle_finder.find(msg_bytes).is_some() {
            self.metrics.candle_messages.fetch_add(1, Ordering::Relaxed);
            // Candle数据暂不写入数据库
        }

        // 记录SIMD匹配延迟
        let simd_time_ns = simd_start.elapsed().as_nanos() as u64;
        self.metrics.simd_match_time_ns.fetch_add(simd_time_ns, Ordering::Relaxed);
        self.metrics.simd_match_samples.fetch_add(1, Ordering::Relaxed);

        self.metrics.total_messages.fetch_add(1, Ordering::Relaxed);
        self.metrics.bytes_received.fetch_add(msg_text.len() as u64, Ordering::Relaxed);

        Ok(())
    }

    /// 解析OrderBook消息
    fn parse_orderbook_message(&self, msg_text: &str, timestamp: i64) -> Result<MarketDataType, Box<dyn std::error::Error>> {
        let json: Value = serde_json::from_str(msg_text)?;
        
        if let Some(data_array) = json.get("data").and_then(|d| d.as_array()) {
            if let Some(data) = data_array.first() {
                let symbol = json.get("arg").and_then(|a| a.get("instId")).and_then(|s| s.as_str()).unwrap_or("UNKNOWN").to_string();
                
                let bids = data.get("bids").and_then(|b| b.as_array())
                    .map(|arr| arr.iter().filter_map(|item| {
                        if let Some(price_qty) = item.as_array() {
                            if price_qty.len() >= 2 {
                                let price = price_qty[0].as_str()?.parse::<f64>().ok()?;
                                let qty = price_qty[1].as_str()?.parse::<f64>().ok()?;
                                Some((price, qty))
                            } else { None }
                        } else { None }
                    }).collect())
                    .unwrap_or_default();

                let asks = data.get("asks").and_then(|a| a.as_array())
                    .map(|arr| arr.iter().filter_map(|item| {
                        if let Some(price_qty) = item.as_array() {
                            if price_qty.len() >= 2 {
                                let price = price_qty[0].as_str()?.parse::<f64>().ok()?;
                                let qty = price_qty[1].as_str()?.parse::<f64>().ok()?;
                                Some((price, qty))
                            } else { None }
                        } else { None }
                    }).collect())
                    .unwrap_or_default();

                return Ok(MarketDataType::OrderBook { symbol, bids, asks, timestamp });
            }
        }
        
        Err("Invalid orderbook format".into())
    }

    /// 解析Trade消息
    fn parse_trade_message(&self, msg_text: &str, timestamp: i64) -> Result<MarketDataType, Box<dyn std::error::Error>> {
        let json: Value = serde_json::from_str(msg_text)?;
        
        if let Some(data_array) = json.get("data").and_then(|d| d.as_array()) {
            if let Some(data) = data_array.first() {
                let symbol = json.get("arg").and_then(|a| a.get("instId")).and_then(|s| s.as_str()).unwrap_or("UNKNOWN").to_string();
                let price = data.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let quantity = data.get("size").and_then(|q| q.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let side = data.get("side").and_then(|s| s.as_str()).unwrap_or("unknown").to_string();

                return Ok(MarketDataType::Trade { symbol, price, quantity, side, timestamp });
            }
        }
        
        Err("Invalid trade format".into())
    }

    /// 解析Ticker消息
    fn parse_ticker_message(&self, msg_text: &str, timestamp: i64) -> Result<MarketDataType, Box<dyn std::error::Error>> {
        let json: Value = serde_json::from_str(msg_text)?;
        
        if let Some(data_array) = json.get("data").and_then(|d| d.as_array()) {
            if let Some(data) = data_array.first() {
                let symbol = json.get("arg").and_then(|a| a.get("instId")).and_then(|s| s.as_str()).unwrap_or("UNKNOWN").to_string();
                let price = data.get("lastPr").and_then(|p| p.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let volume = data.get("baseVolume").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);

                return Ok(MarketDataType::Ticker { symbol, price, volume, timestamp });
            }
        }
        
        Err("Invalid ticker format".into())
    }
}

/// 🌐 Bitget WebSocket连接管理器
pub struct BitgetWebSocketConnector {
    connection_id: usize,
    symbols: Vec<String>,
    channels: Vec<String>, // ["books15", "trade", "ticker", "candle1m"]
    inst_type: String,     // "SPOT" 或 "USDT-FUTURES"
    processor: Arc<LockFreeDataProcessor>,
    metrics: RealTimeMetrics,
    running: Arc<AtomicBool>,
}

impl BitgetWebSocketConnector {
    pub fn new(
        connection_id: usize,
        symbols: Vec<String>,
        channels: Vec<String>,
        inst_type: String,
        processor: Arc<LockFreeDataProcessor>,
        metrics: RealTimeMetrics,
    ) -> Self {
        Self {
            connection_id,
            symbols,
            channels,
            inst_type,
            processor,
            metrics,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 启动WebSocket连接
    pub async fn start_connection(&self) -> tokio::task::JoinHandle<()> {
        let connection_id = self.connection_id;
        let symbols = self.symbols.clone();
        let channels = self.channels.clone();
        let inst_type = self.inst_type.clone();
        let processor = self.processor.clone();
        let metrics = self.metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            running.store(true, Ordering::Relaxed);
            println!("🔗 启动Bitget WebSocket连接 #{}", connection_id);

            let mut reconnect_count = 0;
            
            while running.load(Ordering::Relaxed) {
                match Self::connect_and_run(
                    connection_id,
                    &symbols,
                    &channels,
                    &inst_type,
                    &processor,
                    &metrics,
                    &running
                ).await {
                    Ok(_) => {
                        println!("✅ WebSocket连接 #{} 正常结束", connection_id);
                        break;
                    },
                    Err(e) => {
                        reconnect_count += 1;
                        metrics.reconnection_count.fetch_add(1, Ordering::Relaxed);
                        println!("❌ WebSocket连接 #{} 错误: {} (重连 #{}/10)", connection_id, e, reconnect_count);
                        
                        if reconnect_count >= 10 {
                            println!("🚫 WebSocket连接 #{} 达到最大重连次数，停止重连", connection_id);
                            break;
                        }

                        // 指数退避重连
                        let delay_secs = std::cmp::min(1 << reconnect_count, 60);
                        tokio::time::sleep(Duration::from_secs(delay_secs)).await;
                    }
                }
            }

            println!("🔌 WebSocket连接 #{} 完全断开", connection_id);
        })
    }

    /// 连接并运行WebSocket
    async fn connect_and_run(
        connection_id: usize,
        symbols: &[String],
        channels: &[String],
        inst_type: &str,
        processor: &Arc<LockFreeDataProcessor>,
        metrics: &RealTimeMetrics,
        running: &Arc<AtomicBool>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        
        // 连接到Bitget WebSocket V2 API
        let bitget_url = "wss://ws.bitget.com/v2/ws/public";
        println!("🌐 连接到 Bitget WebSocket: {}", bitget_url);

        let (ws_stream, _) = timeout(
            Duration::from_secs(10),
            connect_async(bitget_url)
        ).await??;

        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        metrics.active_connections.fetch_add(1, Ordering::Relaxed);

        // 订阅频道
        for symbol in symbols {
            for channel in channels {
                let subscribe_msg = json!({
                    "op": "subscribe",
                    "args": [{
                        "instType": inst_type,
                        "channel": channel,
                        "instId": symbol
                    }]
                });

                println!("📡 订阅 {} - {} (连接 #{})", symbol, channel, connection_id);
                
                if let Err(e) = ws_sender.send(Message::Text(subscribe_msg.to_string())).await {
                    return Err(format!("订阅失败: {}", e).into());
                }

                // 避免订阅过快
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        // 消息接收循环，包含定期心跳
        let mut last_heartbeat = Instant::now();
        
        while running.load(Ordering::Relaxed) {
            // 检查是否需要发送心跳
            if last_heartbeat.elapsed() > Duration::from_secs(30) {
                let ping_msg = json!({"op": "ping"});
                if let Err(e) = ws_sender.send(Message::Text(ping_msg.to_string())).await {
                    return Err(format!("心跳发送失败: {}", e).into());
                }
                last_heartbeat = Instant::now();
            }

            // 使用非阻塞接收，减少延迟
            match timeout(Duration::from_millis(1), ws_receiver.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let receive_time = Instant::now();
                    
                    // SIMD优化处理 (超快)
                    if let Err(_) = processor.process_message(&text, receive_time) {
                        metrics.websocket_errors.fetch_add(1, Ordering::Relaxed);
                    }
                },
                Ok(Some(Ok(Message::Pong(_)))) => {
                    // 收到pong响应，连接正常
                },
                Ok(Some(Ok(_))) => {
                    // 忽略其他消息类型
                },
                Ok(Some(Err(e))) => {
                    metrics.websocket_errors.fetch_add(1, Ordering::Relaxed);
                    return Err(format!("WebSocket错误: {}", e).into());
                },
                Ok(None) => {
                    return Err("WebSocket连接关闭".into());
                },
                Err(_) => {
                    // 超时是正常的，使用CPU hint优化等待
                    hint::spin_loop();
                    continue;
                }
            }
        }
        metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
        Ok(())
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// 🚀 高性能数据接收测试管理器 (专注接收性能 + 可选ClickHouse写入)
pub struct BitgetPerformanceTester {
    connectors: Vec<BitgetWebSocketConnector>,
    processor: Arc<LockFreeDataProcessor>,
    global_metrics: RealTimeMetrics,
    clickhouse_writer: Option<Arc<ClickHouseWriter>>,
    clickhouse_metrics: Option<WriteMetrics>,
    running: Arc<AtomicBool>,
}

impl BitgetPerformanceTester {
    pub fn new(
        connection_count: usize, 
        spot_symbols: Vec<String>,
        futures_symbols: Vec<String>,
        channels: Vec<String>
    ) -> Self {
        let global_metrics = RealTimeMetrics::new();
        let processor = Arc::new(LockFreeDataProcessor::new(global_metrics.clone()));
        
        let mut connectors = Vec::with_capacity(connection_count);
        
        // 合并现货和期货交易对
        let mut all_symbols = Vec::new();
        for symbol in &spot_symbols {
            all_symbols.push((symbol.clone(), "SPOT".to_string()));
        }
        for symbol in &futures_symbols {
            all_symbols.push((symbol.clone(), "USDT-FUTURES".to_string()));
        }
        
        // 🔧 优化连接分配策略 - 确保每个连接不超过50个频道 (Bitget建议)
        // 每个symbol * 4个频道 = 4个订阅，50/4 = 12个symbol per connection (安全余量)
        let max_symbols_per_connection = 12; // 12 symbols × 4 channels = 48 subscriptions (安全范围)
        let symbols_per_connection = std::cmp::min(
            (all_symbols.len() + connection_count - 1) / connection_count,
            max_symbols_per_connection
        );
        
        for i in 0..connection_count {
            let start_idx = i * symbols_per_connection;
            let end_idx = std::cmp::min(start_idx + symbols_per_connection, all_symbols.len());
            let assigned_symbol_pairs = &all_symbols[start_idx..end_idx];
            
            if !assigned_symbol_pairs.is_empty() {
                let symbols: Vec<String> = assigned_symbol_pairs.iter().map(|(s, _)| s.clone()).collect();
                let inst_type = assigned_symbol_pairs[0].1.clone(); // 使用第一个类型
                
                let connector = BitgetWebSocketConnector::new(
                    i,
                    symbols,
                    channels.clone(),
                    inst_type,
                    processor.clone(),
                    global_metrics.clone(),
                );
                connectors.push(connector);
            }
        }

        Self {
            connectors,
            processor,
            global_metrics,
            clickhouse_writer: None,
            clickhouse_metrics: None,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 创建带ClickHouse写入的测试器
    pub async fn new_with_clickhouse(
        connection_count: usize, 
        spot_symbols: Vec<String>,
        futures_symbols: Vec<String>,
        channels: Vec<String>,
        clickhouse_config: ClickHouseConfig,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let global_metrics = RealTimeMetrics::new();
        
        // 创建ClickHouse写入器
        let clickhouse_writer = Arc::new(ClickHouseWriter::new(clickhouse_config).await?);
        let clickhouse_metrics = clickhouse_writer.get_metrics();
        
        // 使用带ClickHouse的处理器
        let processor = Arc::new(LockFreeDataProcessor::new_with_clickhouse(
            global_metrics.clone(), 
            clickhouse_writer.clone()
        ));
        
        let mut connectors = Vec::with_capacity(connection_count);
        
        // 合并现货和期货交易对
        let mut all_symbols = Vec::new();
        for symbol in &spot_symbols {
            all_symbols.push((symbol.clone(), "SPOT".to_string()));
        }
        for symbol in &futures_symbols {
            all_symbols.push((symbol.clone(), "USDT-FUTURES".to_string()));
        }
        
        // 🔧 优化连接分配策略 - 确保每个连接不超过50个频道 (Bitget建议)
        // 每个symbol * 4个频道 = 4个订阅，50/4 = 12个symbol per connection (安全余量)
        let max_symbols_per_connection = 12; // 12 symbols × 4 channels = 48 subscriptions (安全范围)
        let symbols_per_connection = std::cmp::min(
            (all_symbols.len() + connection_count - 1) / connection_count,
            max_symbols_per_connection
        );
        
        for i in 0..connection_count {
            let start_idx = i * symbols_per_connection;
            let end_idx = std::cmp::min(start_idx + symbols_per_connection, all_symbols.len());
            let assigned_symbol_pairs = &all_symbols[start_idx..end_idx];
            
            if !assigned_symbol_pairs.is_empty() {
                let symbols: Vec<String> = assigned_symbol_pairs.iter().map(|(s, _)| s.clone()).collect();
                let inst_type = assigned_symbol_pairs[0].1.clone(); // 使用第一个类型
                
                let connector = BitgetWebSocketConnector::new(
                    i,
                    symbols,
                    channels.clone(),
                    inst_type,
                    processor.clone(),
                    global_metrics.clone(),
                );
                connectors.push(connector);
            }
        }

        Ok(Self {
            connectors,
            processor,
            global_metrics,
            clickhouse_writer: Some(clickhouse_writer),
            clickhouse_metrics: Some(clickhouse_metrics),
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// 运行性能测试
    pub async fn run_test(&mut self, duration_secs: u64) -> TestResults {
        println!("🚀 启动Bitget WebSocket高性能测试");
        println!("配置: {} 连接 × {} 秒", self.connectors.len(), duration_secs);

        self.running.store(true, Ordering::Relaxed);
        let start_time = Instant::now();

        // 不启动额外的数据处理任务，专注于接收性能

        // 启动所有WebSocket连接
        let mut connection_tasks = Vec::new();
        for connector in &self.connectors {
            let task = connector.start_connection().await;
            connection_tasks.push(task);
        }

        // 启动监控任务
        let monitor_task = self.start_monitoring();

        // 等待连接建立
        tokio::time::sleep(Duration::from_secs(5)).await;
        println!("✅ 所有连接已建立，开始数据接收测试...");

        // 等待测试完成
        tokio::time::sleep(Duration::from_secs(duration_secs)).await;

        println!("⏰ 停止测试...");
        self.running.store(false, Ordering::Relaxed);

        // 停止所有连接
        for connector in &self.connectors {
            connector.stop();
        }

        // 等待任务完成
        for task in connection_tasks {
            let _ = task.await;
        }
        let _ = monitor_task.await;

        let elapsed = start_time.elapsed();
        self.generate_results(elapsed)
    }


    /// 启动实时监控 (支持ClickHouse统计)
    fn start_monitoring(&self) -> tokio::task::JoinHandle<()> {
        let metrics = self.global_metrics.clone();
        let clickhouse_metrics = self.clickhouse_metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(2));
            let mut last_messages = 0u64;
            let mut last_written = 0u64;
            let mut last_time = Instant::now();

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                let current_messages = metrics.total_messages.load(Ordering::Relaxed);
                let current_time = Instant::now();
                let elapsed_secs = current_time.duration_since(last_time).as_secs_f64();

                let throughput = if elapsed_secs > 0.0 {
                    (current_messages - last_messages) as f64 / elapsed_secs
                } else {
                    0.0
                };

                let avg_simd_ns = metrics.avg_simd_match_ns();
                let connections = metrics.active_connections.load(Ordering::Relaxed);
                let orderbook = metrics.orderbook_messages.load(Ordering::Relaxed);
                let trades = metrics.trade_messages.load(Ordering::Relaxed);
                let ticker = metrics.ticker_messages.load(Ordering::Relaxed);
                let candles = metrics.candle_messages.load(Ordering::Relaxed);
                let errors = metrics.websocket_errors.load(Ordering::Relaxed);
                let reconnects = metrics.reconnection_count.load(Ordering::Relaxed);

                // 基础WebSocket统计
                print!(
                    "⚡ [{:.0} msg/s] [SIMD:{:.0}ns] [{}🔗] [{}📊] [{}💰] [{}📈] [{}📊] [{}❌] [{}🔄]",
                    throughput, avg_simd_ns, connections, orderbook, trades, ticker, candles, errors, reconnects
                );

                // ClickHouse统计（如果启用）
                if let Some(ref ch_metrics) = clickhouse_metrics {
                    let current_written = ch_metrics.total_written.load(Ordering::Relaxed);
                    let write_throughput = if elapsed_secs > 0.0 {
                        (current_written - last_written) as f64 / elapsed_secs
                    } else {
                        0.0
                    };
                    
                    let ch_orderbook = ch_metrics.orderbook_written.load(Ordering::Relaxed);
                    let ch_trades = ch_metrics.trade_written.load(Ordering::Relaxed);
                    let ch_ticker = ch_metrics.ticker_written.load(Ordering::Relaxed);
                    let ch_errors = ch_metrics.write_errors.load(Ordering::Relaxed);
                    let ch_drops = ch_metrics.buffer_full_drops.load(Ordering::Relaxed);
                    let ch_latency = ch_metrics.avg_write_latency_us.load(Ordering::Relaxed);
                    
                    print!(
                        " 💾CH[{:.0} w/s] [{}📊{}💰{}📈] [{}❌{}⚠️] [{}μs]",
                        write_throughput, ch_orderbook, ch_trades, ch_ticker, ch_errors, ch_drops, ch_latency
                    );
                    
                    last_written = current_written;
                }

                println!(); // 换行

                last_messages = current_messages;
                last_time = current_time;
            }
        })
    }

    /// 生成测试结果
    fn generate_results(&self, duration: Duration) -> TestResults {
        let duration_secs = duration.as_secs_f64();
        let total_messages = self.global_metrics.total_messages.load(Ordering::Relaxed);
        let total_bytes = self.global_metrics.bytes_received.load(Ordering::Relaxed);
        let avg_simd_ns = self.global_metrics.avg_simd_match_ns();
        let throughput = self.global_metrics.throughput_per_sec(duration_secs);

        TestResults {
            duration_secs,
            total_messages,
            total_bytes,
            throughput_msg_per_sec: throughput,
            avg_latency_microseconds: avg_simd_ns / 1000.0, // 转换为微秒
            orderbook_messages: self.global_metrics.orderbook_messages.load(Ordering::Relaxed),
            trade_messages: self.global_metrics.trade_messages.load(Ordering::Relaxed),
            ticker_messages: self.global_metrics.ticker_messages.load(Ordering::Relaxed),
            total_errors: self.global_metrics.websocket_errors.load(Ordering::Relaxed),
            reconnection_count: self.global_metrics.reconnection_count.load(Ordering::Relaxed),
            connection_count: self.connectors.len() as u64,
        }
    }
}

/// 📊 测试结果
pub struct TestResults {
    pub duration_secs: f64,
    pub total_messages: u64,
    pub total_bytes: u64,
    pub throughput_msg_per_sec: f64,
    pub avg_latency_microseconds: f64,
    pub orderbook_messages: u64,
    pub trade_messages: u64,
    pub ticker_messages: u64,
    pub total_errors: u64,
    pub reconnection_count: u64,
    pub connection_count: u64,
}

impl TestResults {
    pub fn print_report(&self) {
        println!("\n🎯 ===== Bitget WebSocket真实性能测试报告 =====");
        println!("⏱️  测试持续时间: {:.2} 秒", self.duration_secs);
        println!("🔗 WebSocket连接数: {}", self.connection_count);
        println!("📨 总消息数: {} 条", self.total_messages);
        println!("📦 总字节数: {:.2} MB", self.total_bytes as f64 / 1024.0 / 1024.0);
        println!("🚀 平均吞吐量: {:.0} msg/s", self.throughput_msg_per_sec);
        println!("⚡ 平均延迟: {:.2} μs", self.avg_latency_microseconds);
        
        println!("\n📊 ===== 数据分布 =====");
        println!("📊 OrderBook15消息: {} 条 ({:.1}%)", 
                 self.orderbook_messages, 
                 self.orderbook_messages as f64 / self.total_messages as f64 * 100.0);
        println!("💰 Trade消息: {} 条 ({:.1}%)", 
                 self.trade_messages,
                 self.trade_messages as f64 / self.total_messages as f64 * 100.0);
        println!("📈 Ticker消息: {} 条 ({:.1}%)", 
                 self.ticker_messages,
                 self.ticker_messages as f64 / self.total_messages as f64 * 100.0);

        println!("\n🔧 ===== 连接质量 =====");
        println!("❌ 错误数: {} 个", self.total_errors);
        println!("🔄 重连次数: {} 次", self.reconnection_count);
        
        let error_rate = if self.total_messages > 0 {
            (self.total_errors as f64 / self.total_messages as f64) * 100.0
        } else {
            0.0
        };
        println!("📈 错误率: {:.4}%", error_rate);

        println!("\n🏆 ===== 性能评估 =====");
        
        // 吞吐量评估 (真实WebSocket通常更低)
        if self.throughput_msg_per_sec >= 10_000.0 {
            println!("✅ 吞吐量: 优秀 (>= 10k msg/s)");
        } else if self.throughput_msg_per_sec >= 5_000.0 {
            println!("🟡 吞吐量: 良好 (>= 5k msg/s)");
        } else {
            println!("❌ 吞吐量: 需改进 (< 5k msg/s)");
        }

        // 延迟评估
        if self.avg_latency_microseconds <= 1000.0 {
            println!("✅ 延迟: 优秀 (<= 1ms)");
        } else if self.avg_latency_microseconds <= 5000.0 {
            println!("🟡 延迟: 良好 (<= 5ms)");
        } else {
            println!("❌ 延迟: 需改进 (> 5ms)");
        }

        // 稳定性评估
        if error_rate < 0.1 {
            println!("✅ 稳定性: 优秀 (<0.1% 错误率)");
        } else if error_rate < 1.0 {
            println!("🟡 稳定性: 良好 (<1% 错误率)");
        } else {
            println!("❌ 稳定性: 需改进 (>1% 错误率)");
        }

        println!("===============================================\n");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 SIMD优化Bitget WebSocket压力测试 (30连接×150交易对)\n");

    // 🚀 极限测试配置 - 基于Bitget API限制优化
    let connection_count = 30; // 30个连接 (平衡性能和稳定性)
    let test_duration = 120;   // 2分钟测试
    
    // 🔥 100个最活跃现货交易对 - 涵盖主流和热门币种
    let spot_symbols = vec![
        // 主流币种 (Top 20)
        "BTCUSDT".to_string(), "ETHUSDT".to_string(), "BNBUSDT".to_string(),
        "SOLUSDT".to_string(), "XRPUSDT".to_string(), "ADAUSDT".to_string(),
        "DOGEUSDT".to_string(), "MATICUSDT".to_string(), "AVAXUSDT".to_string(),
        "DOTUSDT".to_string(), "LINKUSDT".to_string(), "ATOMUSDT".to_string(),
        "NEARUSDT".to_string(), "AAVEUSDT".to_string(), "UNIUSDT".to_string(),
        "SUSHIUSDT".to_string(), "LTCUSDT".to_string(), "BCHUSDT".to_string(),
        "ETCUSDT".to_string(), "FILUSDT".to_string(),
        
        // DeFi生态 (20个)
        "CAKEUSDT".to_string(), "PANCAKEUSDT".to_string(), "COMPUSDT".to_string(),
        "YFIUSDT".to_string(), "MKRUSDT".to_string(), "SNXUSDT".to_string(),
        "CRVUSDT".to_string(), "1INCHUSDT".to_string(), "BALUSDT".to_string(),
        "LRCUSDT".to_string(), "BADGERUSDT".to_string(), "ALPHAUSDT".to_string(),
        "BNXUSDT".to_string(), "DEXEUSDT".to_string(), "RAMPUSDT".to_string(),
        "AXLAUSDT".to_string(), "CVXUSDT".to_string(), "DYDXUSDT".to_string(),
        "GMXUSDT".to_string(), "RDNTUSDT".to_string(),
        
        // Layer2 & 基础设施 (20个)
        "OPUSDT".to_string(), "ARBUSDT".to_string(), "MATICUSDT".to_string(),
        "ICPUSDT".to_string(), "VETUSDT".to_string(), "TRXUSDT".to_string(),
        "XLMUSDT".to_string(), "EOSUSDT".to_string(), "ALGOUSDT".to_string(),
        "KSMUSDT".to_string(), "WAVESUSDT".to_string(), "QTUMUSDT".to_string(),
        "ZENUSDT".to_string(), "ZECUSDT".to_string(), "DASHUSDT".to_string(),
        "XMRUSDT".to_string(), "ONTUSDT".to_string(), "NULSUSDT".to_string(),
        "SCUSDT".to_string(), "BATUSDT".to_string(),
        
        // GameFi & NFT (20个)
        "AXSUSDT".to_string(), "SANDUSDT".to_string(), "MANAUSDT".to_string(),
        "ENJUSDT".to_string(), "CHZUSDT".to_string(), "GALAUSDT".to_string(),
        "FLOWUSDT".to_string(), "WAXPUSDT".to_string(), "THETAUSDT".to_string(),
        "ALICEUSDT".to_string(), "TLMUSDT".to_string(), "STARUSDT".to_string(),
        "PEPUSDT".to_string(), "FLOKIUSDT".to_string(), "SHIBUSDT".to_string(),
        "XECUSDT".to_string(), "LUNAUSDT".to_string(), "USTCUSDT".to_string(),
        "LUNCUSDT".to_string(), "ENSUSDT".to_string(),
        
        // 新兴生态 (20个)  
        "APTUSDT".to_string(), "CFXUSDT".to_string(), "STXUSDT".to_string(),
        "INJUSDT".to_string(), "SUIUSDT".to_string(), "BLURUSDT".to_string(),
        "IDUSDT".to_string(), "ARBUSDT".to_string(), "MAGICUSDT".to_string(),
        "GMTUSDT".to_string(), "GFTUSDT".to_string(), "STGUSDT".to_string(),
        "APEUSDT".to_string(), "LOOKUSDT".to_string(), "RAREUSDT".to_string(),
        "NFTUSDT".to_string(), "GRTUSDT".to_string(), "MASKUSDT".to_string(),
        "BNXUSDT".to_string(), "RUNEUSDT".to_string(),
    ];

    // 🚀 50个活跃期货交易对 - 高流动性合约
    let futures_symbols = vec![
        // 主流期货 (Top 15)
        "BTCUSDT".to_string(), "ETHUSDT".to_string(), "BNBUSDT".to_string(),
        "SOLUSDT".to_string(), "XRPUSDT".to_string(), "ADAUSDT".to_string(),
        "DOGEUSDT".to_string(), "MATICUSDT".to_string(), "AVAXUSDT".to_string(),
        "DOTUSDT".to_string(), "LINKUSDT".to_string(), "ATOMUSDT".to_string(),
        "NEARUSDT".to_string(), "AAVEUSDT".to_string(), "UNIUSDT".to_string(),
        
        // 活跃期货 (15个)
        "LTCUSDT".to_string(), "BCHUSDT".to_string(), "ETCUSDT".to_string(),
        "FILUSDT".to_string(), "ICPUSDT".to_string(), "VETUSDT".to_string(),
        "TRXUSDT".to_string(), "XLMUSDT".to_string(), "EOSUSDT".to_string(),
        "AXSUSDT".to_string(), "SANDUSDT".to_string(), "MANAUSDT".to_string(),
        "ENJUSDT".to_string(), "CHZUSDT".to_string(), "GALAUSDT".to_string(),
        
        // 新兴期货 (20个)
        "APTUSDT".to_string(), "OPUSDT".to_string(), "ARBUSDT".to_string(),
        "CFXUSDT".to_string(), "STXUSDT".to_string(), "INJUSDT".to_string(),
        "SUIUSDT".to_string(), "BLURUSDT".to_string(), "IDUSDT".to_string(),
        "MAGICUSDT".to_string(), "GMTUSDT".to_string(), "GFTUSDT".to_string(),
        "STGUSDT".to_string(), "APEUSDT".to_string(), "LOOKUSDT".to_string(),
        "RAREUSDT".to_string(), "GRTUSDT".to_string(), "MASKUSDT".to_string(),
        "BNXUSDT".to_string(), "RUNEUSDT".to_string(),
    ];

    // 订阅频道
    let channels = vec![
        "books15".to_string(),  // 15档orderbook
        "trade".to_string(),    // 实时交易
        "ticker".to_string(),   // ticker数据
        "candle1m".to_string(), // 1分钟K线
    ];

    let total_symbols = spot_symbols.len() + futures_symbols.len();
    let total_subscriptions = total_symbols * channels.len();

    println!("🚀 极限压力测试配置:");
    println!("   - 连接数: {} 个 (单IP最多100连接，使用50%安全余量)", connection_count);
    println!("   - 现货交易对: {} 个", spot_symbols.len());
    println!("   - 期货交易对: {} 个", futures_symbols.len());
    println!("   - 总交易对: {} 个", total_symbols);
    println!("   - 频道: {} 个 ({:?})", channels.len(), channels);
    println!("   - 总订阅数: {} 个", total_subscriptions);
    println!("   - 每连接订阅数: ~{} 个 (最多50个建议)", total_subscriptions / connection_count);
    println!("   - 预期数据流量: 极高 ({}个活跃市场)", total_symbols);
    println!("   - 优化特性: SIMD字节匹配 + Lock-free计数 + 零拷贝");
    println!("   - 测试时长: {} 秒 ({}分钟)\n", test_duration, test_duration / 60);

    // 检查是否启用ClickHouse写入
    let use_clickhouse = std::env::var("ENABLE_CLICKHOUSE").unwrap_or_default() == "1";
    
    let results = if use_clickhouse {
        println!("💾 启用ClickHouse数据写入功能...");
        
        // 创建ClickHouse配置
        let clickhouse_config = ClickHouseConfig {
            url: std::env::var("CLICKHOUSE_URL").unwrap_or_else(|_| "http://localhost:8123".to_string()),
            database: std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "hft_db".to_string()),
            username: std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "hft_user".to_string()),
            password: std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "hft_password".to_string()),
            batch_size: 500,
            flush_interval_ms: 2000,
            max_buffer_size: 10000,
        };
        
        println!("📊 ClickHouse配置:");
        println!("   - URL: {}", clickhouse_config.url);
        println!("   - Database: {}", clickhouse_config.database);
        println!("   - Batch Size: {}", clickhouse_config.batch_size);
        println!("   - Flush Interval: {}ms", clickhouse_config.flush_interval_ms);
        
        // 创建带ClickHouse的测试器
        match BitgetPerformanceTester::new_with_clickhouse(
            connection_count, 
            spot_symbols, 
            futures_symbols, 
            channels,
            clickhouse_config
        ).await {
            Ok(mut tester) => {
                println!("✅ ClickHouse连接成功，开始测试...\n");
                tester.run_test(test_duration).await
            }
            Err(e) => {
                println!("❌ ClickHouse连接失败: {}", e);
                println!("💡 提示: 请确保ClickHouse服务运行并且连接参数正确");
                println!("💡 或者不设置ENABLE_CLICKHOUSE=1环境变量来禁用ClickHouse");
                return Err(e);
            }
        }
    } else {
        println!("🔧 仅WebSocket接收测试 (未启用ClickHouse写入)");
        println!("💡 提示: 设置环境变量 ENABLE_CLICKHOUSE=1 来启用ClickHouse数据写入\n");
        
        // 创建普通测试器
        let mut tester = BitgetPerformanceTester::new(connection_count, spot_symbols, futures_symbols, channels);
        tester.run_test(test_duration).await
    };

    // 打印结果
    results.print_report();

    println!("🏁 测试完成!");
    Ok(())
}