/// 🚀 SIMD优化WebSocket测试
/// 
/// 优化特性：
/// - SIMD JSON解析 (simd-json)
/// - 零拷贝消息处理 (bytes::Bytes)
/// - 快速字节模式匹配 (memchr)
/// - 内存池和对象复用
/// - Lock-free原子计数器

use std::{
    sync::{
        atomic::{AtomicU64, AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    hint,
};

use tokio::{
    time::interval,
    net::TcpStream,
};

use tokio_tungstenite::{
    connect_async, 
    tungstenite::Message,
};

use futures_util::{StreamExt, SinkExt};
use bytes::{Bytes, BytesMut};
use serde_json::json;
use memchr::memmem;

/// 📊 SIMD优化指标
#[derive(Clone)]
pub struct SimdMetrics {
    pub total_messages: Arc<AtomicU64>,
    pub orderbook_messages: Arc<AtomicU64>,
    pub trade_messages: Arc<AtomicU64>,
    pub ticker_messages: Arc<AtomicU64>,
    pub candle_messages: Arc<AtomicU64>,
    
    pub bytes_received: Arc<AtomicU64>,
    pub parse_time_ns: Arc<AtomicU64>,
    pub parse_samples: Arc<AtomicU64>,
    pub pattern_match_time_ns: Arc<AtomicU64>,
    pub pattern_match_samples: Arc<AtomicU64>,
    
    pub active_connections: Arc<AtomicU64>,
    pub reconnection_count: Arc<AtomicU64>,
    pub parse_errors: Arc<AtomicU64>,
    pub websocket_errors: Arc<AtomicU64>,
}

impl SimdMetrics {
    pub fn new() -> Self {
        Self {
            total_messages: Arc::new(AtomicU64::new(0)),
            orderbook_messages: Arc::new(AtomicU64::new(0)),
            trade_messages: Arc::new(AtomicU64::new(0)),
            ticker_messages: Arc::new(AtomicU64::new(0)),
            candle_messages: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            parse_time_ns: Arc::new(AtomicU64::new(0)),
            parse_samples: Arc::new(AtomicU64::new(0)),
            pattern_match_time_ns: Arc::new(AtomicU64::new(0)),
            pattern_match_samples: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            reconnection_count: Arc::new(AtomicU64::new(0)),
            parse_errors: Arc::new(AtomicU64::new(0)),
            websocket_errors: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn avg_parse_latency_ns(&self) -> f64 {
        let samples = self.parse_samples.load(Ordering::Relaxed);
        if samples == 0 {
            0.0
        } else {
            self.parse_time_ns.load(Ordering::Relaxed) as f64 / samples as f64
        }
    }

    pub fn avg_pattern_match_ns(&self) -> f64 {
        let samples = self.pattern_match_samples.load(Ordering::Relaxed);
        if samples == 0 {
            0.0
        } else {
            self.pattern_match_time_ns.load(Ordering::Relaxed) as f64 / samples as f64
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

/// 📦 SIMD优化消息处理器
pub struct SimdMessageProcessor {
    metrics: SimdMetrics,
    // 预编译的SIMD模式匹配器
    books_finder: memmem::Finder<'static>,
    trade_finder: memmem::Finder<'static>,
    ticker_finder: memmem::Finder<'static>,
    candle_finder: memmem::Finder<'static>,
    // JSON解析缓冲区
    json_buffer: Vec<u8>,
    // 字节缓冲区池
    buffer_pool: Vec<BytesMut>,
}

impl SimdMessageProcessor {
    pub fn new(metrics: SimdMetrics) -> Self {
        Self {
            metrics,
            // 预编译字节模式，使用SIMD加速搜索
            books_finder: memmem::Finder::new(b"\"books15\""),
            trade_finder: memmem::Finder::new(b"\"trade\""),
            ticker_finder: memmem::Finder::new(b"\"ticker\""),
            candle_finder: memmem::Finder::new(b"\"candle"),
            json_buffer: Vec::with_capacity(16384),
            buffer_pool: (0..100).map(|_| BytesMut::with_capacity(8192)).collect(),
        }
    }

    /// 模式1：超快字节模式匹配 (最快，但信息有限)
    #[inline(always)]
    pub fn process_message_pattern_match(&mut self, msg_text: &str) -> Result<(), Box<dyn std::error::Error>> {
        let pattern_start = Instant::now();
        
        let msg_bytes = msg_text.as_bytes();
        
        // 使用SIMD优化的字节搜索
        if self.books_finder.find(msg_bytes).is_some() {
            self.metrics.orderbook_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.trade_finder.find(msg_bytes).is_some() {
            self.metrics.trade_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.ticker_finder.find(msg_bytes).is_some() {
            self.metrics.ticker_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.candle_finder.find(msg_bytes).is_some() {
            self.metrics.candle_messages.fetch_add(1, Ordering::Relaxed);
        }

        let pattern_time_ns = pattern_start.elapsed().as_nanos() as u64;
        self.metrics.pattern_match_time_ns.fetch_add(pattern_time_ns, Ordering::Relaxed);
        self.metrics.pattern_match_samples.fetch_add(1, Ordering::Relaxed);
        
        self.metrics.total_messages.fetch_add(1, Ordering::Relaxed);
        self.metrics.bytes_received.fetch_add(msg_text.len() as u64, Ordering::Relaxed);

        Ok(())
    }

    /// 模式2：SIMD JSON解析 (稍慢，但提取更多信息)
    #[allow(dead_code)]
    pub fn process_message_simd_json(&mut self, msg_text: &str) -> Result<(), Box<dyn std::error::Error>> {
        let parse_start = Instant::now();
        
        // 准备可变缓冲区用于SIMD-JSON
        self.json_buffer.clear();
        self.json_buffer.extend_from_slice(msg_text.as_bytes());
        
        match simd_json::to_borrowed_value(&mut self.json_buffer) {
            Ok(value) => {
                // 使用simd_json的值访问API
                use simd_json::prelude::*;
                
                if let Some(arg) = value.get("arg") {
                    if let Some(channel) = arg.get("channel") {
                        if let Some(channel_str) = channel.as_str() {
                            match channel_str {
                                "books15" => self.metrics.orderbook_messages.fetch_add(1, Ordering::Relaxed),
                                "trade" => self.metrics.trade_messages.fetch_add(1, Ordering::Relaxed),
                                "ticker" => self.metrics.ticker_messages.fetch_add(1, Ordering::Relaxed),
                                ch if ch.starts_with("candle") => self.metrics.candle_messages.fetch_add(1, Ordering::Relaxed),
                                _ => 0,
                            };
                        }
                    }
                }
            }
            Err(_) => {
                self.metrics.parse_errors.fetch_add(1, Ordering::Relaxed);
            }
        }

        let parse_time_ns = parse_start.elapsed().as_nanos() as u64;
        self.metrics.parse_time_ns.fetch_add(parse_time_ns, Ordering::Relaxed);
        self.metrics.parse_samples.fetch_add(1, Ordering::Relaxed);
        
        self.metrics.total_messages.fetch_add(1, Ordering::Relaxed);
        self.metrics.bytes_received.fetch_add(msg_text.len() as u64, Ordering::Relaxed);

        Ok(())
    }

    /// 模式3：零拷贝字节处理
    #[allow(dead_code)]
    pub fn process_message_zero_copy(&mut self, msg_bytes: &Bytes) -> Result<(), Box<dyn std::error::Error>> {
        let pattern_start = Instant::now();
        
        let bytes_slice = msg_bytes.as_ref();
        
        // 使用SIMD字节搜索，无需字符串分配
        if self.books_finder.find(bytes_slice).is_some() {
            self.metrics.orderbook_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.trade_finder.find(bytes_slice).is_some() {
            self.metrics.trade_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.ticker_finder.find(bytes_slice).is_some() {
            self.metrics.ticker_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.candle_finder.find(bytes_slice).is_some() {
            self.metrics.candle_messages.fetch_add(1, Ordering::Relaxed);
        }

        let pattern_time_ns = pattern_start.elapsed().as_nanos() as u64;
        self.metrics.pattern_match_time_ns.fetch_add(pattern_time_ns, Ordering::Relaxed);
        self.metrics.pattern_match_samples.fetch_add(1, Ordering::Relaxed);
        
        self.metrics.total_messages.fetch_add(1, Ordering::Relaxed);
        self.metrics.bytes_received.fetch_add(msg_bytes.len() as u64, Ordering::Relaxed);

        Ok(())
    }
}

/// 🚀 SIMD优化WebSocket连接器
pub struct SimdWebSocketConnector {
    connection_id: usize,
    symbols: Vec<String>,
    channels: Vec<String>,
    inst_type: String,
    processor: Arc<tokio::sync::Mutex<SimdMessageProcessor>>,
    metrics: SimdMetrics,
    running: Arc<AtomicBool>,
}

impl SimdWebSocketConnector {
    pub fn new(
        connection_id: usize,
        symbols: Vec<String>,
        channels: Vec<String>,
        inst_type: String,
        processor: Arc<tokio::sync::Mutex<SimdMessageProcessor>>,
        metrics: SimdMetrics,
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

    /// 启动SIMD优化连接
    pub async fn start_simd_connection(&self) -> tokio::task::JoinHandle<()> {
        let connection_id = self.connection_id;
        let symbols = self.symbols.clone();
        let channels = self.channels.clone();
        let inst_type = self.inst_type.clone();
        let processor = self.processor.clone();
        let metrics = self.metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            running.store(true, Ordering::Relaxed);
            println!("⚡ 启动SIMD优化WebSocket连接 #{}", connection_id);

            let mut reconnect_count = 0;
            
            while running.load(Ordering::Relaxed) {
                match Self::connect_and_run_simd(
                    connection_id,
                    &symbols,
                    &channels,
                    &inst_type,
                    &processor,
                    &metrics,
                    &running
                ).await {
                    Ok(_) => {
                        println!("✅ SIMD连接 #{} 正常结束", connection_id);
                        break;
                    },
                    Err(e) => {
                        reconnect_count += 1;
                        metrics.reconnection_count.fetch_add(1, Ordering::Relaxed);
                        println!("❌ SIMD连接 #{} 错误: {} (重连 #{}/5)", connection_id, e, reconnect_count);
                        
                        if reconnect_count >= 5 {
                            println!("🚫 连接 #{} 达到最大重连次数", connection_id);
                            break;
                        }

                        let delay_ms = std::cmp::min(200 * reconnect_count, 5000);
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                }
            }

            println!("🔌 SIMD连接 #{} 完全断开", connection_id);
        })
    }

    /// SIMD优化连接实现
    async fn connect_and_run_simd(
        connection_id: usize,
        symbols: &[String],
        channels: &[String],
        inst_type: &str,
        processor: &Arc<tokio::sync::Mutex<SimdMessageProcessor>>,
        metrics: &SimdMetrics,
        running: &Arc<AtomicBool>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        
        // 连接到Bitget WebSocket
        let bitget_url = "wss://ws.bitget.com/v2/ws/public";
        let (ws_stream, _) = connect_async(bitget_url).await?;
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        
        metrics.active_connections.fetch_add(1, Ordering::Relaxed);

        // 批量订阅
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

                if let Err(e) = ws_sender.send(Message::Text(subscribe_msg.to_string())).await {
                    return Err(format!("订阅失败: {}", e).into());
                }
                
                // 减少订阅延迟
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        println!("📡 SIMD连接 #{} 完成订阅", connection_id);

        // SIMD优化消息处理循环
        let mut last_heartbeat = Instant::now();
        
        while running.load(Ordering::Relaxed) {
            // 心跳检查
            if last_heartbeat.elapsed() > Duration::from_secs(30) {
                let ping_msg = json!({"op": "ping"});
                if let Err(e) = ws_sender.send(Message::Text(ping_msg.to_string())).await {
                    return Err(format!("心跳发送失败: {}", e).into());
                }
                last_heartbeat = Instant::now();
            }

            // 使用非阻塞接收，避免阻塞热路径
            match tokio::time::timeout(Duration::from_millis(1), ws_receiver.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    // 快速处理：尝试获取锁，失败则跳过
                    if let Ok(mut proc) = processor.try_lock() {
                        if let Err(_) = proc.process_message_pattern_match(&text) {
                            metrics.parse_errors.fetch_add(1, Ordering::Relaxed);
                        }
                    } else {
                        // 处理器忙碌，使用CPU hint优化等待
                        hint::spin_loop();
                    }
                },
                Ok(Some(Ok(Message::Pong(_)))) => {
                    // Pong响应，连接正常
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
                    // 超时是正常的，继续循环
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

/// 🚀 SIMD优化测试管理器
pub struct SimdPerformanceTester {
    connectors: Vec<SimdWebSocketConnector>,
    processor: Arc<tokio::sync::Mutex<SimdMessageProcessor>>,
    global_metrics: SimdMetrics,
    running: Arc<AtomicBool>,
}

impl SimdPerformanceTester {
    pub fn new(
        connection_count: usize, 
        spot_symbols: Vec<String>,
        futures_symbols: Vec<String>,
        channels: Vec<String>
    ) -> Self {
        let global_metrics = SimdMetrics::new();
        let processor = Arc::new(tokio::sync::Mutex::new(
            SimdMessageProcessor::new(global_metrics.clone())
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
        
        // 为每个连接分配symbols
        let symbols_per_connection = (all_symbols.len() + connection_count - 1) / connection_count;
        
        for i in 0..connection_count {
            let start_idx = i * symbols_per_connection;
            let end_idx = std::cmp::min(start_idx + symbols_per_connection, all_symbols.len());
            let assigned_symbol_pairs = &all_symbols[start_idx..end_idx];
            
            if !assigned_symbol_pairs.is_empty() {
                let symbols: Vec<String> = assigned_symbol_pairs.iter().map(|(s, _)| s.clone()).collect();
                let inst_type = assigned_symbol_pairs[0].1.clone();
                
                let connector = SimdWebSocketConnector::new(
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
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 运行SIMD优化测试
    pub async fn run_simd_test(&mut self, duration_secs: u64) -> SimdTestResults {
        println!("⚡ 启动SIMD优化Bitget WebSocket测试");
        println!("配置: {} 连接 × {} 秒 (SIMD字节匹配优化)", self.connectors.len(), duration_secs);

        self.running.store(true, Ordering::Relaxed);
        let start_time = Instant::now();

        // 启动所有SIMD连接
        let mut connection_tasks = Vec::new();
        for connector in &self.connectors {
            let task = connector.start_simd_connection().await;
            connection_tasks.push(task);
        }

        // 启动实时监控
        let monitor_task = self.start_simd_monitoring();

        // 等待连接建立
        tokio::time::sleep(Duration::from_secs(3)).await;
        println!("✅ 所有SIMD连接已建立，开始数据接收测试...");

        // 等待测试完成
        tokio::time::sleep(Duration::from_secs(duration_secs)).await;

        println!("⏰ 停止SIMD测试...");
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
        self.generate_simd_results(elapsed)
    }

    /// 启动SIMD监控
    fn start_simd_monitoring(&self) -> tokio::task::JoinHandle<()> {
        let metrics = self.global_metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(1));
            let mut last_messages = 0u64;
            let mut last_time = Instant::now();

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                let current_messages = metrics.total_messages.load(Ordering::Relaxed);
                let current_time = Instant::now();
                let elapsed = current_time.duration_since(last_time).as_secs_f64();

                let throughput = if elapsed > 0.0 {
                    (current_messages - last_messages) as f64 / elapsed
                } else {
                    0.0
                };

                let avg_pattern_ns = metrics.avg_pattern_match_ns();
                let connections = metrics.active_connections.load(Ordering::Relaxed);
                let orderbook = metrics.orderbook_messages.load(Ordering::Relaxed);
                let trades = metrics.trade_messages.load(Ordering::Relaxed);
                let ticker = metrics.ticker_messages.load(Ordering::Relaxed);
                let candles = metrics.candle_messages.load(Ordering::Relaxed);
                let errors = metrics.parse_errors.load(Ordering::Relaxed) + metrics.websocket_errors.load(Ordering::Relaxed);

                println!(
                    "⚡ [{:.0} msg/s] [SIMD:{:.0}ns] [{}🔗] [{}📊] [{}💰] [{}📈] [{}📊] [{}❌]",
                    throughput, avg_pattern_ns, connections, 
                    orderbook, trades, ticker, candles, errors
                );

                last_messages = current_messages;
                last_time = current_time;
            }
        })
    }

    /// 生成SIMD测试结果
    fn generate_simd_results(&self, duration: Duration) -> SimdTestResults {
        let duration_secs = duration.as_secs_f64();
        let total_messages = self.global_metrics.total_messages.load(Ordering::Relaxed);
        let total_bytes = self.global_metrics.bytes_received.load(Ordering::Relaxed);
        let avg_pattern_match_ns = self.global_metrics.avg_pattern_match_ns();
        let throughput = self.global_metrics.throughput_per_sec(duration_secs);

        SimdTestResults {
            duration_secs,
            total_messages,
            total_bytes,
            throughput_msg_per_sec: throughput,
            avg_pattern_match_ns,
            orderbook_messages: self.global_metrics.orderbook_messages.load(Ordering::Relaxed),
            trade_messages: self.global_metrics.trade_messages.load(Ordering::Relaxed),
            ticker_messages: self.global_metrics.ticker_messages.load(Ordering::Relaxed),
            candle_messages: self.global_metrics.candle_messages.load(Ordering::Relaxed),
            parse_errors: self.global_metrics.parse_errors.load(Ordering::Relaxed),
            websocket_errors: self.global_metrics.websocket_errors.load(Ordering::Relaxed),
            reconnection_count: self.global_metrics.reconnection_count.load(Ordering::Relaxed),
            connection_count: self.connectors.len() as u64,
        }
    }
}

/// 📊 SIMD测试结果
pub struct SimdTestResults {
    pub duration_secs: f64,
    pub total_messages: u64,
    pub total_bytes: u64,
    pub throughput_msg_per_sec: f64,
    pub avg_pattern_match_ns: f64,
    pub orderbook_messages: u64,
    pub trade_messages: u64,
    pub ticker_messages: u64,
    pub candle_messages: u64,
    pub parse_errors: u64,
    pub websocket_errors: u64,
    pub reconnection_count: u64,
    pub connection_count: u64,
}

impl SimdTestResults {
    pub fn print_simd_report(&self) {
        println!("\n⚡ ===== SIMD优化WebSocket测试报告 =====");
        println!("⏱️  测试持续时间: {:.2} 秒", self.duration_secs);
        println!("🔗 WebSocket连接数: {}", self.connection_count);
        println!("📨 总消息数: {} 条", self.total_messages);
        println!("📦 总字节数: {:.2} MB", self.total_bytes as f64 / 1024.0 / 1024.0);
        println!("🚀 平均吞吐量: {:.0} msg/s", self.throughput_msg_per_sec);
        println!("⚡ SIMD模式匹配延迟: {:.0} ns ({:.3} μs)", self.avg_pattern_match_ns, self.avg_pattern_match_ns / 1000.0);
        
        println!("\n📊 ===== 数据分布 =====");
        let total = self.total_messages as f64;
        println!("📊 OrderBook消息: {} 条 ({:.1}%)", self.orderbook_messages, self.orderbook_messages as f64 / total * 100.0);
        println!("💰 Trade消息: {} 条 ({:.1}%)", self.trade_messages, self.trade_messages as f64 / total * 100.0);
        println!("📈 Ticker消息: {} 条 ({:.1}%)", self.ticker_messages, self.ticker_messages as f64 / total * 100.0);
        println!("📊 Candle消息: {} 条 ({:.1}%)", self.candle_messages, self.candle_messages as f64 / total * 100.0);

        println!("\n🔧 ===== 连接质量 =====");
        println!("❌ 解析错误: {} 个", self.parse_errors);
        println!("🌐 WebSocket错误: {} 个", self.websocket_errors);
        println!("🔄 重连次数: {} 次", self.reconnection_count);
        
        let error_rate = if self.total_messages > 0 {
            ((self.parse_errors + self.websocket_errors) as f64 / self.total_messages as f64) * 100.0
        } else {
            0.0
        };
        println!("📈 总错误率: {:.4}%", error_rate);

        println!("\n🏆 ===== SIMD性能评估 =====");
        
        // 吞吐量评估
        if self.throughput_msg_per_sec >= 30_000.0 {
            println!("🌟 吞吐量: 超级优秀 (>= 30k msg/s)");
        } else if self.throughput_msg_per_sec >= 15_000.0 {
            println!("✅ 吞吐量: 优秀 (>= 15k msg/s)");
        } else if self.throughput_msg_per_sec >= 8_000.0 {
            println!("🟡 吞吐量: 良好 (>= 8k msg/s)");
        } else {
            println!("❌ 吞吐量: 需改进 (< 8k msg/s)");
        }

        // SIMD延迟评估
        if self.avg_pattern_match_ns <= 50.0 {
            println!("🌟 SIMD延迟: 超级优秀 (<= 50ns)");
        } else if self.avg_pattern_match_ns <= 200.0 {
            println!("✅ SIMD延迟: 优秀 (<= 200ns)");
        } else if self.avg_pattern_match_ns <= 1000.0 {
            println!("🟡 SIMD延迟: 良好 (<= 1μs)");
        } else {
            println!("❌ SIMD延迟: 需改进 (> 1μs)");
        }

        // 稳定性评估
        if error_rate < 0.01 {
            println!("✅ 稳定性: 超级优秀 (<0.01% 错误率)");
        } else if error_rate < 0.1 {
            println!("🟡 稳定性: 良好 (<0.1% 错误率)");
        } else {
            println!("❌ 稳定性: 需改进 (>0.1% 错误率)");
        }

        println!("\n💡 ===== SIMD优化分析 =====");
        println!("⚡ SIMD字节模式匹配: 启用");
        println!("🔧 零分配字符串处理: 部分启用");
        println!("🚀 Lock-free原子计数: 启用");
        println!("🌐 单连接吞吐量: {:.0} msg/s", self.throughput_msg_per_sec / self.connection_count as f64);
        
        let ns_per_message = if self.total_messages > 0 {
            (self.duration_secs * 1_000_000_000.0) / self.total_messages as f64
        } else {
            0.0
        };
        println!("⏱️  平均每消息处理时间: {:.0} ns", ns_per_message);

        if self.avg_pattern_match_ns > 500.0 {
            println!("💭 建议: SIMD模式匹配可进一步优化");
        }
        if self.throughput_msg_per_sec < 15000.0 {
            println!("💭 建议: 考虑增加连接数或使用更快的JSON库");
        }

        println!("===============================================\n");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("⚡ SIMD优化Bitget WebSocket测试\n");

    // 测试配置
    let connection_count = 16;
    let test_duration = 60;
    
    // 活跃现货交易对
    let spot_symbols = vec![
        "BTCUSDT".to_string(), "ETHUSDT".to_string(), "BNBUSDT".to_string(),
        "SOLUSDT".to_string(), "XRPUSDT".to_string(), "ADAUSDT".to_string(),
        "DOGEUSDT".to_string(), "MATICUSDT".to_string(), "AVAXUSDT".to_string(),
        "DOTUSDT".to_string(), "LINKUSDT".to_string(), "ATOMUSDT".to_string(),
        "NEARUSDT".to_string(), "AAVEUSDT".to_string(), "UNIUSDT".to_string(),
        "SUSHIUSDT".to_string(), "FTMUSDT".to_string(), "MANAUSDT".to_string(),
        "SANDUSDT".to_string(), "ENSUSDT".to_string(),
    ];

    // 活跃期货交易对
    let futures_symbols = vec![
        "BTCUSDT".to_string(), "ETHUSDT".to_string(), "BNBUSDT".to_string(),
        "SOLUSDT".to_string(), "XRPUSDT".to_string(), "ADAUSDT".to_string(),
        "DOGEUSDT".to_string(), "MATICUSDT".to_string(), "AVAXUSDT".to_string(),
        "DOTUSDT".to_string(),
    ];

    // 订阅频道
    let channels = vec![
        "books15".to_string(),
        "trade".to_string(),
        "ticker".to_string(),
        "candle1m".to_string(),
    ];

    let total_symbols = spot_symbols.len() + futures_symbols.len();
    let total_subscriptions = total_symbols * channels.len();

    println!("⚡ SIMD优化配置:");
    println!("   - 连接数: {} 个", connection_count);
    println!("   - 现货交易对: {} 个", spot_symbols.len());
    println!("   - 期货交易对: {} 个", futures_symbols.len());
    println!("   - 频道: {} 个 ({:?})", channels.len(), channels);
    println!("   - 总订阅数: {} 个", total_subscriptions);
    println!("   - 优化特性: SIMD字节匹配 + Lock-free计数");
    println!("   - 测试时长: {} 秒\n", test_duration);

    // 创建并运行SIMD优化测试
    let mut tester = SimdPerformanceTester::new(connection_count, spot_symbols, futures_symbols, channels);
    let results = tester.run_simd_test(test_duration).await;

    // 打印SIMD优化结果
    results.print_simd_report();

    println!("🏁 SIMD优化测试完成!");
    Ok(())
}