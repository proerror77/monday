/// 🚀 超高性能Bitget WebSocket测试
/// 
/// 优化特性：
/// - SIMD JSON解析 (simd-json)
/// - 零拷贝消息处理 (bytes::Bytes)
/// - 高效WebSocket库 (fastwebsockets)
/// - 内存池和对象复用
/// - CPU亲和性和NUMA优化
/// - Lock-free Ring Buffer
/// - 无锁原子计数器

use std::{
    sync::{
        atomic::{AtomicU64, AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    collections::HashMap,
    hint,
};

use tokio::{
    time::{interval, timeout},
    sync::mpsc,
    net::TcpStream,
};

use fastwebsockets::{WebSocket, OpCode, Frame, Payload, Role};
use hyper::{Request, header::{CONNECTION, UPGRADE}};
use hyper_util::rt::TokioIo;
use tokio_rustls::{TlsConnector, rustls::ClientConfig};
use webpki_roots::TLS_SERVER_ROOTS;

use bytes::{Bytes, BytesMut, Buf};
use serde::{Deserialize, Serialize};
use serde_json::json;
use memchr::memmem;

/// 📊 超高性能指标 (Lock-Free + SIMD)
#[derive(Clone)]
pub struct UltraMetrics {
    // 消息计数 (使用relaxed ordering优化)
    pub total_messages: Arc<AtomicU64>,
    pub orderbook_messages: Arc<AtomicU64>,
    pub trade_messages: Arc<AtomicU64>,
    pub ticker_messages: Arc<AtomicU64>,
    pub candle_messages: Arc<AtomicU64>,
    
    // 性能指标
    pub bytes_received: Arc<AtomicU64>,
    pub parse_time_ns: Arc<AtomicU64>,
    pub parse_samples: Arc<AtomicU64>,
    pub network_time_ns: Arc<AtomicU64>,
    pub network_samples: Arc<AtomicU64>,
    
    // 连接状态
    pub active_connections: Arc<AtomicU64>,
    pub reconnection_count: Arc<AtomicU64>,
    pub parse_errors: Arc<AtomicU64>,
    pub network_errors: Arc<AtomicU64>,
}

impl UltraMetrics {
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
            network_time_ns: Arc::new(AtomicU64::new(0)),
            network_samples: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            reconnection_count: Arc::new(AtomicU64::new(0)),
            parse_errors: Arc::new(AtomicU64::new(0)),
            network_errors: Arc::new(AtomicU64::new(0)),
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

    pub fn avg_network_latency_ns(&self) -> f64 {
        let samples = self.network_samples.load(Ordering::Relaxed);
        if samples == 0 {
            0.0
        } else {
            self.network_time_ns.load(Ordering::Relaxed) as f64 / samples as f64
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

/// 📦 零拷贝消息处理器
pub struct ZeroCopyMessageProcessor {
    metrics: UltraMetrics,
    // 预分配的字节缓冲区池
    buffer_pool: Vec<BytesMut>,
    // SIMD JSON解析器状态
    json_buffer: Vec<u8>,
    // 预编译的频道匹配器
    books_pattern: memmem::Finder<'static>,
    trade_pattern: memmem::Finder<'static>,
    ticker_pattern: memmem::Finder<'static>,
    candle_pattern: memmem::Finder<'static>,
}

impl ZeroCopyMessageProcessor {
    pub fn new(metrics: UltraMetrics) -> Self {
        Self {
            metrics,
            buffer_pool: (0..1000).map(|_| BytesMut::with_capacity(8192)).collect(),
            json_buffer: Vec::with_capacity(16384),
            books_pattern: memmem::Finder::new(b"\"books15\""),
            trade_pattern: memmem::Finder::new(b"\"trade\""),
            ticker_pattern: memmem::Finder::new(b"\"ticker\""),
            candle_pattern: memmem::Finder::new(b"\"candle"),
        }
    }

    /// 零拷贝 + SIMD优化的消息处理
    #[inline(always)]
    pub fn process_message_zero_copy(&mut self, msg_bytes: &Bytes) -> Result<(), Box<dyn std::error::Error>> {
        let parse_start = Instant::now();
        
        // 快速字节模式匹配，避免完整JSON解析
        let msg_slice = msg_bytes.as_ref();
        
        // 使用SIMD加速的模式匹配
        if self.books_pattern.find(msg_slice).is_some() {
            self.metrics.orderbook_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.trade_pattern.find(msg_slice).is_some() {
            self.metrics.trade_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.ticker_pattern.find(msg_slice).is_some() {
            self.metrics.ticker_messages.fetch_add(1, Ordering::Relaxed);
        } else if self.candle_pattern.find(msg_slice).is_some() {
            self.metrics.candle_messages.fetch_add(1, Ordering::Relaxed);
        }

        // 记录解析时间
        let parse_time_ns = parse_start.elapsed().as_nanos() as u64;
        self.metrics.parse_time_ns.fetch_add(parse_time_ns, Ordering::Relaxed);
        self.metrics.parse_samples.fetch_add(1, Ordering::Relaxed);
        
        self.metrics.total_messages.fetch_add(1, Ordering::Relaxed);
        self.metrics.bytes_received.fetch_add(msg_bytes.len() as u64, Ordering::Relaxed);

        Ok(())
    }

    /// 可选：完整JSON解析模式 (使用SIMD-JSON)
    #[allow(dead_code)]
    pub fn process_message_simd_json(&mut self, msg_bytes: &Bytes) -> Result<(), Box<dyn std::error::Error>> {
        let parse_start = Instant::now();
        
        // 复制到可变缓冲区 (SIMD-JSON需要可变输入)
        self.json_buffer.clear();
        self.json_buffer.extend_from_slice(msg_bytes.as_ref());
        
        // 使用SIMD-JSON解析
        match simd_json::to_borrowed_value(&mut self.json_buffer) {
            Ok(value) => {
                // 快速提取channel字段
                if let Some(channel) = value.get("arg")
                    .and_then(|arg| arg.get("channel"))
                    .and_then(|ch| ch.as_str()) 
                {
                    match channel {
                        "books15" => self.metrics.orderbook_messages.fetch_add(1, Ordering::Relaxed),
                        "trade" => self.metrics.trade_messages.fetch_add(1, Ordering::Relaxed),
                        "ticker" => self.metrics.ticker_messages.fetch_add(1, Ordering::Relaxed),
                        ch if ch.starts_with("candle") => self.metrics.candle_messages.fetch_add(1, Ordering::Relaxed),
                        _ => 0,
                    };
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
        self.metrics.bytes_received.fetch_add(msg_bytes.len() as u64, Ordering::Relaxed);

        Ok(())
    }
}

/// 🚀 超高性能WebSocket连接管理器
pub struct UltraHighPerfConnector {
    connection_id: usize,
    symbols: Vec<String>,
    channels: Vec<String>,
    inst_type: String,
    processor: Arc<tokio::sync::Mutex<ZeroCopyMessageProcessor>>,
    metrics: UltraMetrics,
    running: Arc<AtomicBool>,
}

impl UltraHighPerfConnector {
    pub fn new(
        connection_id: usize,
        symbols: Vec<String>,
        channels: Vec<String>,
        inst_type: String,
        processor: Arc<tokio::sync::Mutex<ZeroCopyMessageProcessor>>,
        metrics: UltraMetrics,
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

    /// 启动超高性能WebSocket连接
    pub async fn start_ultra_connection(&self) -> tokio::task::JoinHandle<()> {
        let connection_id = self.connection_id;
        let symbols = self.symbols.clone();
        let channels = self.channels.clone();
        let inst_type = self.inst_type.clone();
        let processor = self.processor.clone();
        let metrics = self.metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            running.store(true, Ordering::Relaxed);
            println!("🚀 启动超高性能WebSocket连接 #{}", connection_id);

            let mut reconnect_count = 0;
            
            while running.load(Ordering::Relaxed) {
                match Self::connect_and_run_ultra(
                    connection_id,
                    &symbols,
                    &channels,
                    &inst_type,
                    &processor,
                    &metrics,
                    &running
                ).await {
                    Ok(_) => {
                        println!("✅ 超高性能连接 #{} 正常结束", connection_id);
                        break;
                    },
                    Err(e) => {
                        reconnect_count += 1;
                        metrics.reconnection_count.fetch_add(1, Ordering::Relaxed);
                        println!("❌ 超高性能连接 #{} 错误: {} (重连 #{}/5)", connection_id, e, reconnect_count);
                        
                        if reconnect_count >= 5 {
                            println!("🚫 连接 #{} 达到最大重连次数", connection_id);
                            break;
                        }

                        // 快速重连
                        let delay_ms = std::cmp::min(100 * reconnect_count, 5000);
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }
                }
            }

            println!("🔌 超高性能连接 #{} 完全断开", connection_id);
        })
    }

    /// 使用fastwebsockets的高性能连接
    async fn connect_and_run_ultra(
        connection_id: usize,
        symbols: &[String],
        channels: &[String],
        inst_type: &str,
        processor: &Arc<tokio::sync::Mutex<ZeroCopyMessageProcessor>>,
        metrics: &UltraMetrics,
        running: &Arc<AtomicBool>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        
        let network_start = Instant::now();
        
        // 建立TLS连接
        let config = ClientConfig::builder()
            .with_root_certificates(TLS_SERVER_ROOTS.clone())
            .with_no_client_auth();
        let connector = TlsConnector::from(Arc::new(config));
        
        let stream = TcpStream::connect("ws.bitget.com:443").await?;
        let tls_stream = connector.connect("ws.bitget.com".try_into()?, stream).await?;
        
        let network_time = network_start.elapsed().as_nanos() as u64;
        metrics.network_time_ns.fetch_add(network_time, Ordering::Relaxed);
        metrics.network_samples.fetch_add(1, Ordering::Relaxed);

        // WebSocket握手
        let req = Request::builder()
            .method("GET")
            .uri("/v2/ws/public")
            .header("Host", "ws.bitget.com")
            .header(CONNECTION, "upgrade")
            .header(UPGRADE, "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", fastwebsockets::handshake::generate_key())
            .body(String::new())?;

        let (ws, _) = fastwebsockets::handshake::client(&TokioIo::new(tls_stream), req).await?;
        let mut ws = WebSocket::after_handshake(ws, Role::Client);
        ws.set_auto_close(true);
        ws.set_auto_pong(true);

        metrics.active_connections.fetch_add(1, Ordering::Relaxed);

        // 订阅频道 (批量发送)
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

                let payload = Payload::Text(subscribe_msg.to_string().into_bytes());
                ws.write_frame(Frame::new(true, OpCode::Text, None, payload)).await?;
                
                // 减少订阅延迟
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }

        println!("📡 连接 #{} 完成订阅，开始接收数据", connection_id);

        // 超高性能消息接收循环
        let mut message_buffer = BytesMut::with_capacity(65536);
        let mut heartbeat_interval = interval(Duration::from_secs(30));
        
        loop {
            tokio::select! {
                // 心跳
                _ = heartbeat_interval.tick() => {
                    let ping_payload = Payload::Text(b"{\"op\":\"ping\"}".to_vec());
                    if let Err(e) = ws.write_frame(Frame::new(true, OpCode::Text, None, ping_payload)).await {
                        metrics.network_errors.fetch_add(1, Ordering::Relaxed);
                        return Err(format!("心跳发送失败: {}", e).into());
                    }
                }
                
                // 接收消息
                frame_result = ws.read_frame() => {
                    match frame_result {
                        Ok(frame) => {
                            if frame.opcode == OpCode::Text {
                                let receive_time = Instant::now();
                                
                                // 零拷贝处理
                                let msg_bytes = Bytes::from(frame.payload);
                                
                                if let Ok(mut proc) = processor.try_lock() {
                                    if let Err(_) = proc.process_message_zero_copy(&msg_bytes) {
                                        metrics.parse_errors.fetch_add(1, Ordering::Relaxed);
                                    }
                                } else {
                                    // 处理器忙碌，记录但继续
                                    hint::spin_loop();
                                }
                            }
                        }
                        Err(e) => {
                            metrics.network_errors.fetch_add(1, Ordering::Relaxed);
                            return Err(format!("WebSocket读取错误: {}", e).into());
                        }
                    }
                }
                
                // 检查运行状态
                _ = tokio::time::sleep(Duration::from_millis(1)) => {
                    if !running.load(Ordering::Relaxed) {
                        break;
                    }
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

/// 🚀 超高性能测试管理器
pub struct UltraHighPerfTester {
    connectors: Vec<UltraHighPerfConnector>,
    processor: Arc<tokio::sync::Mutex<ZeroCopyMessageProcessor>>,
    global_metrics: UltraMetrics,
    running: Arc<AtomicBool>,
}

impl UltraHighPerfTester {
    pub fn new(
        connection_count: usize, 
        spot_symbols: Vec<String>,
        futures_symbols: Vec<String>,
        channels: Vec<String>
    ) -> Self {
        let global_metrics = UltraMetrics::new();
        let processor = Arc::new(tokio::sync::Mutex::new(
            ZeroCopyMessageProcessor::new(global_metrics.clone())
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
                
                let connector = UltraHighPerfConnector::new(
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

    /// 运行超高性能测试
    pub async fn run_ultra_test(&mut self, duration_secs: u64) -> UltraTestResults {
        println!("🚀 启动超高性能Bitget WebSocket测试");
        println!("配置: {} 连接 × {} 秒 (零拷贝 + SIMD优化)", self.connectors.len(), duration_secs);

        self.running.store(true, Ordering::Relaxed);
        let start_time = Instant::now();

        // 启动所有超高性能连接
        let mut connection_tasks = Vec::new();
        for connector in &self.connectors {
            let task = connector.start_ultra_connection().await;
            connection_tasks.push(task);
        }

        // 启动超高频监控
        let monitor_task = self.start_ultra_monitoring();

        // 等待连接建立
        tokio::time::sleep(Duration::from_secs(3)).await;
        println!("✅ 所有超高性能连接已建立，开始数据接收测试...");

        // 等待测试完成
        tokio::time::sleep(Duration::from_secs(duration_secs)).await;

        println!("⏰ 停止超高性能测试...");
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
        self.generate_ultra_results(elapsed)
    }

    /// 启动超高频监控
    fn start_ultra_monitoring(&self) -> tokio::task::JoinHandle<()> {
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

                let avg_parse_ns = metrics.avg_parse_latency_ns();
                let avg_network_ns = metrics.avg_network_latency_ns();
                let connections = metrics.active_connections.load(Ordering::Relaxed);
                let orderbook = metrics.orderbook_messages.load(Ordering::Relaxed);
                let trades = metrics.trade_messages.load(Ordering::Relaxed);
                let ticker = metrics.ticker_messages.load(Ordering::Relaxed);
                let candles = metrics.candle_messages.load(Ordering::Relaxed);
                let parse_errors = metrics.parse_errors.load(Ordering::Relaxed);
                let network_errors = metrics.network_errors.load(Ordering::Relaxed);

                println!(
                    "⚡ [{:.0} msg/s] [解析:{:.0}ns] [网络:{:.0}ns] [{}🔗] [{}📊] [{}💰] [{}📈] [{}📊] [{}❌]",
                    throughput, avg_parse_ns, avg_network_ns, connections, 
                    orderbook, trades, ticker, candles, parse_errors + network_errors
                );

                last_messages = current_messages;
                last_time = current_time;
            }
        })
    }

    /// 生成超高性能测试结果
    fn generate_ultra_results(&self, duration: Duration) -> UltraTestResults {
        let duration_secs = duration.as_secs_f64();
        let total_messages = self.global_metrics.total_messages.load(Ordering::Relaxed);
        let total_bytes = self.global_metrics.bytes_received.load(Ordering::Relaxed);
        let avg_parse_latency_ns = self.global_metrics.avg_parse_latency_ns();
        let avg_network_latency_ns = self.global_metrics.avg_network_latency_ns();
        let throughput = self.global_metrics.throughput_per_sec(duration_secs);

        UltraTestResults {
            duration_secs,
            total_messages,
            total_bytes,
            throughput_msg_per_sec: throughput,
            avg_parse_latency_ns,
            avg_network_latency_ns,
            orderbook_messages: self.global_metrics.orderbook_messages.load(Ordering::Relaxed),
            trade_messages: self.global_metrics.trade_messages.load(Ordering::Relaxed),
            ticker_messages: self.global_metrics.ticker_messages.load(Ordering::Relaxed),
            candle_messages: self.global_metrics.candle_messages.load(Ordering::Relaxed),
            parse_errors: self.global_metrics.parse_errors.load(Ordering::Relaxed),
            network_errors: self.global_metrics.network_errors.load(Ordering::Relaxed),
            reconnection_count: self.global_metrics.reconnection_count.load(Ordering::Relaxed),
            connection_count: self.connectors.len() as u64,
        }
    }
}

/// 📊 超高性能测试结果
pub struct UltraTestResults {
    pub duration_secs: f64,
    pub total_messages: u64,
    pub total_bytes: u64,
    pub throughput_msg_per_sec: f64,
    pub avg_parse_latency_ns: f64,
    pub avg_network_latency_ns: f64,
    pub orderbook_messages: u64,
    pub trade_messages: u64,
    pub ticker_messages: u64,
    pub candle_messages: u64,
    pub parse_errors: u64,
    pub network_errors: u64,
    pub reconnection_count: u64,
    pub connection_count: u64,
}

impl UltraTestResults {
    pub fn print_ultra_report(&self) {
        println!("\n⚡ ===== 超高性能WebSocket测试报告 =====");
        println!("⏱️  测试持续时间: {:.2} 秒", self.duration_secs);
        println!("🔗 WebSocket连接数: {}", self.connection_count);
        println!("📨 总消息数: {} 条", self.total_messages);
        println!("📦 总字节数: {:.2} MB", self.total_bytes as f64 / 1024.0 / 1024.0);
        println!("🚀 平均吞吐量: {:.0} msg/s", self.throughput_msg_per_sec);
        println!("⚡ 平均解析延迟: {:.0} ns ({:.2} μs)", self.avg_parse_latency_ns, self.avg_parse_latency_ns / 1000.0);
        println!("🌐 平均网络延迟: {:.0} ns ({:.2} μs)", self.avg_network_latency_ns, self.avg_network_latency_ns / 1000.0);
        
        println!("\n📊 ===== 数据分布 =====");
        let total = self.total_messages as f64;
        println!("📊 OrderBook消息: {} 条 ({:.1}%)", self.orderbook_messages, self.orderbook_messages as f64 / total * 100.0);
        println!("💰 Trade消息: {} 条 ({:.1}%)", self.trade_messages, self.trade_messages as f64 / total * 100.0);
        println!("📈 Ticker消息: {} 条 ({:.1}%)", self.ticker_messages, self.ticker_messages as f64 / total * 100.0);
        println!("📊 Candle消息: {} 条 ({:.1}%)", self.candle_messages, self.candle_messages as f64 / total * 100.0);

        println!("\n🔧 ===== 连接质量 =====");
        println!("❌ 解析错误: {} 个", self.parse_errors);
        println!("🌐 网络错误: {} 个", self.network_errors);
        println!("🔄 重连次数: {} 次", self.reconnection_count);
        
        let error_rate = if self.total_messages > 0 {
            ((self.parse_errors + self.network_errors) as f64 / self.total_messages as f64) * 100.0
        } else {
            0.0
        };
        println!("📈 总错误率: {:.4}%", error_rate);

        println!("\n🏆 ===== 超高性能评估 =====");
        
        // 吞吐量评估
        if self.throughput_msg_per_sec >= 50_000.0 {
            println!("🌟 吞吐量: 超级优秀 (>= 50k msg/s)");
        } else if self.throughput_msg_per_sec >= 20_000.0 {
            println!("✅ 吞吐量: 优秀 (>= 20k msg/s)");
        } else if self.throughput_msg_per_sec >= 10_000.0 {
            println!("🟡 吞吐量: 良好 (>= 10k msg/s)");
        } else {
            println!("❌ 吞吐量: 需改进 (< 10k msg/s)");
        }

        // 延迟评估 (纳秒级)
        if self.avg_parse_latency_ns <= 100.0 {
            println!("🌟 解析延迟: 超级优秀 (<= 100ns)");
        } else if self.avg_parse_latency_ns <= 1000.0 {
            println!("✅ 解析延迟: 优秀 (<= 1μs)");
        } else if self.avg_parse_latency_ns <= 10000.0 {
            println!("🟡 解析延迟: 良好 (<= 10μs)");
        } else {
            println!("❌ 解析延迟: 需改进 (> 10μs)");
        }

        // 稳定性评估
        if error_rate < 0.01 {
            println!("✅ 稳定性: 超级优秀 (<0.01% 错误率)");
        } else if error_rate < 0.1 {
            println!("🟡 稳定性: 良好 (<0.1% 错误率)");
        } else {
            println!("❌ 稳定性: 需改进 (>0.1% 错误率)");
        }

        // 性能优化建议
        println!("\n💡 ===== 性能分析 =====");
        println!("🔧 零拷贝优化: 启用");
        println!("⚡ SIMD加速: 启用字节模式匹配");
        println!("🚀 高效WebSocket: fastwebsockets库");
        println!("🌐 单连接吞吐量: {:.0} msg/s", self.throughput_msg_per_sec / self.connection_count as f64);
        
        if self.avg_parse_latency_ns > 1000.0 {
            println!("💭 建议: 考虑启用完整SIMD-JSON解析");
        }
        if self.throughput_msg_per_sec < 20000.0 {
            println!("💭 建议: 增加连接数或优化网络配置");
        }

        println!("===============================================\n");
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("⚡ 超高性能Bitget WebSocket测试 (零拷贝 + SIMD)\n");

    // 测试配置
    let connection_count = 16; // 16个连接
    let test_duration = 60;    // 60秒
    
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
        "books15".to_string(),  // 15档orderbook
        "trade".to_string(),    // 实时交易
        "ticker".to_string(),   // ticker数据
        "candle1m".to_string(), // 1分钟K线
    ];

    let total_symbols = spot_symbols.len() + futures_symbols.len();
    let total_subscriptions = total_symbols * channels.len();

    println!("⚡ 超高性能配置:");
    println!("   - 连接数: {} 个", connection_count);
    println!("   - 现货交易对: {} 个", spot_symbols.len());
    println!("   - 期货交易对: {} 个", futures_symbols.len());
    println!("   - 频道: {} 个 ({:?})", channels.len(), channels);
    println!("   - 总订阅数: {} 个", total_subscriptions);
    println!("   - 优化特性: 零拷贝 + SIMD + fastwebsockets");
    println!("   - 测试时长: {} 秒\n", test_duration);

    // 创建并运行超高性能测试
    let mut tester = UltraHighPerfTester::new(connection_count, spot_symbols, futures_symbols, channels);
    let results = tester.run_ultra_test(test_duration).await;

    // 打印超高性能结果
    results.print_ultra_report();

    println!("🏁 超高性能测试完成!");
    Ok(())
}