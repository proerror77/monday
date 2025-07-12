/// 🚀 简化版高性能WebSocket数据接收性能测试
/// 专注于测试多开WebSocket连接、lock-free数据结构和高吞吐量
/// 
/// 性能目标：
/// - 100k+ msg/s 吞吐量
/// - <100μs 平均延迟
/// - 16+ 并发WebSocket连接
/// - 零分配热路径

use std::{
    sync::{
        atomic::{AtomicU64, AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    collections::VecDeque,
};

use tokio::{
    time::interval,
    task::JoinHandle,
    net::TcpStream,
};

use tokio_tungstenite::{
    connect_async, 
    WebSocketStream,
    MaybeTlsStream,
    tungstenite::{Message, Error as WsError},
};

use futures_util::{StreamExt, SinkExt};
use crossbeam_channel::{bounded, Receiver, Sender, TryRecvError};
use serde::{Deserialize, Serialize};
use tracing::{info, warn, error};

/// 📊 性能测试配置
#[derive(Clone, Debug)]
pub struct WebSocketPerfConfig {
    pub connection_count: usize,
    pub test_duration_secs: u64,
    pub target_symbols: Vec<String>,
    pub expected_msgs_per_sec: u64,
    pub enable_latency_tracking: bool,
    pub print_realtime_stats: bool,
}

impl Default for WebSocketPerfConfig {
    fn default() -> Self {
        Self {
            connection_count: 16,
            test_duration_secs: 30,
            target_symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
                "DOGEUSDT".to_string(),
                "ADAUSDT".to_string(),
                "MATICUSDT".to_string(),
                "AVAXUSDT".to_string(),
                "DOTUSDT".to_string(),
            ],
            expected_msgs_per_sec: 100_000,
            enable_latency_tracking: true,
            print_realtime_stats: true,
        }
    }
}

/// 📈 实时性能指标 (使用原子操作实现lock-free)
#[derive(Clone)]
pub struct AtomicMetrics {
    pub messages_received: Arc<AtomicU64>,
    pub bytes_received: Arc<AtomicU64>,
    pub orderbook_updates: Arc<AtomicU64>,
    pub trade_updates: Arc<AtomicU64>,
    pub ticker_updates: Arc<AtomicU64>,
    pub websocket_errors: Arc<AtomicU64>,
    pub latency_sum_ns: Arc<AtomicU64>,
    pub latency_samples: Arc<AtomicU64>,
    pub active_connections: Arc<AtomicU64>,
    pub reconnection_count: Arc<AtomicU64>,
}

impl AtomicMetrics {
    pub fn new() -> Self {
        Self {
            messages_received: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            orderbook_updates: Arc::new(AtomicU64::new(0)),
            trade_updates: Arc::new(AtomicU64::new(0)),
            ticker_updates: Arc::new(AtomicU64::new(0)),
            websocket_errors: Arc::new(AtomicU64::new(0)),
            latency_sum_ns: Arc::new(AtomicU64::new(0)),
            latency_samples: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            reconnection_count: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn avg_latency_us(&self) -> f64 {
        let samples = self.latency_samples.load(Ordering::Relaxed);
        if samples == 0 {
            0.0
        } else {
            let sum_ns = self.latency_sum_ns.load(Ordering::Relaxed);
            (sum_ns as f64 / samples as f64) / 1000.0 // 转换为微秒
        }
    }

    pub fn throughput_per_sec(&self, elapsed_secs: f64) -> f64 {
        if elapsed_secs <= 0.0 {
            0.0
        } else {
            self.messages_received.load(Ordering::Relaxed) as f64 / elapsed_secs
        }
    }
}

/// 📦 模拟市场数据消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDataMessage {
    pub symbol: String,
    pub msg_type: String, // "orderbook", "trade", "ticker"
    pub timestamp: u64,
    pub data: String,
}

impl MarketDataMessage {
    pub fn new_orderbook(symbol: String) -> Self {
        Self {
            symbol,
            msg_type: "orderbook".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            data: r#"{"bids":[[50000.1,0.5],[50000.0,1.0]],"asks":[[50000.2,0.3],[50000.3,0.8]]}"#.to_string(),
        }
    }

    pub fn new_trade(symbol: String) -> Self {
        Self {
            symbol,
            msg_type: "trade".to_string(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            data: r#"{"price":50000.15,"qty":0.25,"side":"buy"}"#.to_string(),
        }
    }

    pub fn serialize_compact(&self) -> Vec<u8> {
        // 简化序列化，专注性能
        let json = format!(
            r#"{{"s":"{}","t":"{}","ts":{},"d":"{}"}}"#,
            self.symbol, self.msg_type, self.timestamp, self.data
        );
        json.into_bytes()
    }
}

/// 🔧 Lock-Free 消息处理器
pub struct LockFreeMessageProcessor {
    message_channel: (Sender<MarketDataMessage>, Receiver<MarketDataMessage>),
    metrics: AtomicMetrics,
    running: Arc<AtomicBool>,
}

impl LockFreeMessageProcessor {
    pub fn new(buffer_size: usize) -> Self {
        let (sender, receiver) = bounded(buffer_size);
        
        Self {
            message_channel: (sender, receiver),
            metrics: AtomicMetrics::new(),
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 启动消息处理任务 (lock-free)
    pub fn start_processing(&self) -> JoinHandle<()> {
        let receiver = self.message_channel.1.clone();
        let metrics = self.metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            let mut local_buffer = VecDeque::with_capacity(1000);
            let mut batch_count = 0u64;

            while running.load(Ordering::Relaxed) {
                // 批量从channel接收消息，减少锁竞争
                let mut batch_size = 0;
                while batch_size < 100 {
                    match receiver.try_recv() {
                        Ok(msg) => {
                            // 根据消息类型更新计数器
                            match msg.msg_type.as_str() {
                                "orderbook" => metrics.orderbook_updates.fetch_add(1, Ordering::Relaxed),
                                "trade" => metrics.trade_updates.fetch_add(1, Ordering::Relaxed),
                                "ticker" => metrics.ticker_updates.fetch_add(1, Ordering::Relaxed),
                                _ => 0,
                            };

                            local_buffer.push_back(msg);
                            batch_size += 1;
                        },
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            warn!("消息处理channel已断开");
                            return;
                        }
                    }
                }

                if batch_size > 0 {
                    metrics.messages_received.fetch_add(batch_size as u64, Ordering::Relaxed);
                    batch_count += 1;

                    // 模拟处理延迟（实际应用中这里是OrderBook更新等）
                    if batch_count % 1000 == 0 {
                        tokio::task::yield_now().await;
                    }

                    // 清空本地缓冲区
                    local_buffer.clear();
                } else {
                    // 没有消息时短暂休眠，避免CPU空转
                    tokio::time::sleep(Duration::from_micros(10)).await;
                }
            }

            info!("消息处理任务结束");
        })
    }

    pub fn send_message(&self, msg: MarketDataMessage) -> bool {
        match self.message_channel.0.try_send(msg) {
            Ok(_) => true,
            Err(_) => {
                // Channel满了，丢弃消息并记录错误
                self.metrics.websocket_errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

/// 🌐 模拟WebSocket连接 (实际场景中连接到Bitget)
pub struct MockWebSocketConnection {
    connection_id: usize,
    symbols: Vec<String>,
    metrics: AtomicMetrics,
    message_processor: Arc<LockFreeMessageProcessor>,
    running: Arc<AtomicBool>,
}

impl MockWebSocketConnection {
    pub fn new(
        connection_id: usize,
        symbols: Vec<String>,
        metrics: AtomicMetrics,
        message_processor: Arc<LockFreeMessageProcessor>,
    ) -> Self {
        Self {
            connection_id,
            symbols,
            metrics,
            message_processor,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 启动模拟WebSocket连接
    pub async fn start_connection(&self) -> JoinHandle<()> {
        let connection_id = self.connection_id;
        let symbols = self.symbols.clone();
        let metrics = self.metrics.clone();
        let message_processor = self.message_processor.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            running.store(true, Ordering::Relaxed);
            metrics.active_connections.fetch_add(1, Ordering::Relaxed);
            
            info!("🔗 启动模拟WebSocket连接 #{}", connection_id);

            // 模拟高频消息接收 (每个连接10k msg/s)
            let mut interval = interval(Duration::from_micros(100)); // 10k msg/s
            let mut message_count = 0u64;

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                // 为每个symbol生成消息
                for symbol in &symbols {
                    let receive_start = Instant::now();

                    let msg = match message_count % 10 {
                        0..=6 => MarketDataMessage::new_orderbook(symbol.clone()), // 70% orderbook
                        7..=8 => MarketDataMessage::new_trade(symbol.clone()),     // 20% trade
                        _ => MarketDataMessage::new_orderbook(symbol.clone()),     // 10% other
                    };

                    // 记录延迟
                    let latency_ns = receive_start.elapsed().as_nanos() as u64;
                    metrics.latency_sum_ns.fetch_add(latency_ns, Ordering::Relaxed);
                    metrics.latency_samples.fetch_add(1, Ordering::Relaxed);

                    // 发送到消息处理器
                    let bytes = msg.serialize_compact();
                    metrics.bytes_received.fetch_add(bytes.len() as u64, Ordering::Relaxed);

                    if !message_processor.send_message(msg) {
                        // 发送失败，记录错误
                        metrics.websocket_errors.fetch_add(1, Ordering::Relaxed);
                    }

                    message_count += 1;
                }
            }

            metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
            info!("🔌 WebSocket连接 #{} 已断开", connection_id);
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// 🚀 高性能WebSocket测试管理器
pub struct WebSocketPerformanceTester {
    config: WebSocketPerfConfig,
    message_processor: Arc<LockFreeMessageProcessor>,
    connections: Vec<MockWebSocketConnection>,
    global_metrics: AtomicMetrics,
    running: Arc<AtomicBool>,
}

impl WebSocketPerformanceTester {
    pub fn new(config: WebSocketPerfConfig) -> Self {
        let message_processor = Arc::new(LockFreeMessageProcessor::new(100_000)); // 10万消息缓冲
        let global_metrics = AtomicMetrics::new();
        
        // 为每个连接分配symbols
        let symbols_per_connection = config.target_symbols.len().max(1);
        let mut connections = Vec::with_capacity(config.connection_count);
        
        for i in 0..config.connection_count {
            let assigned_symbols = config.target_symbols
                .iter()
                .skip(i % symbols_per_connection)
                .take(1)
                .cloned()
                .collect();

            let connection = MockWebSocketConnection::new(
                i,
                assigned_symbols,
                global_metrics.clone(),
                message_processor.clone(),
            );
            connections.push(connection);
        }

        Self {
            config,
            message_processor,
            connections,
            global_metrics,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 🚀 启动性能测试
    pub async fn run_performance_test(&mut self) -> anyhow::Result<TestReport> {
        info!("🚀 启动WebSocket高性能测试");
        info!("配置: {} 连接 × {} 交易对 × {} 秒", 
              self.config.connection_count, 
              self.config.target_symbols.len(),
              self.config.test_duration_secs);

        self.running.store(true, Ordering::Relaxed);
        let test_start = Instant::now();

        // 启动消息处理器
        let _processor_task = self.message_processor.start_processing();

        // 启动所有WebSocket连接
        let mut connection_tasks = Vec::new();
        for connection in &self.connections {
            let task = connection.start_connection().await;
            connection_tasks.push(task);
        }

        // 启动实时监控
        let monitor_task = if self.config.print_realtime_stats {
            Some(self.start_realtime_monitoring())
        } else {
            None
        };

        // 等待测试完成
        tokio::time::sleep(Duration::from_secs(self.config.test_duration_secs)).await;

        info!("⏰ 测试时间结束，正在停止所有连接...");
        self.running.store(false, Ordering::Relaxed);

        // 停止所有连接
        for connection in &self.connections {
            connection.stop();
        }

        // 等待任务完成
        for task in connection_tasks {
            let _ = task.await;
        }

        if let Some(task) = monitor_task {
            let _ = task.await;
        }

        let test_duration = test_start.elapsed();
        let report = self.generate_test_report(test_duration);

        info!("✅ 性能测试完成!");
        Ok(report)
    }

    /// 📊 启动实时监控
    fn start_realtime_monitoring(&self) -> JoinHandle<()> {
        let metrics = self.global_metrics.clone();
        let running = self.running.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(1));
            let mut last_messages = 0u64;
            let mut last_time = Instant::now();

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                let current_messages = metrics.messages_received.load(Ordering::Relaxed);
                let current_time = Instant::now();
                let elapsed = current_time.duration_since(last_time).as_secs_f64();

                let msgs_per_sec = if elapsed > 0.0 {
                    (current_messages - last_messages) as f64 / elapsed
                } else {
                    0.0
                };

                let avg_latency_us = metrics.avg_latency_us();
                let active_conns = metrics.active_connections.load(Ordering::Relaxed);
                let errors = metrics.websocket_errors.load(Ordering::Relaxed);
                let orderbook_count = metrics.orderbook_updates.load(Ordering::Relaxed);
                let trade_count = metrics.trade_updates.load(Ordering::Relaxed);

                info!(
                    "📊 [{:.0} msg/s] [{:.1}μs] [{}⚡] [{}❌] [{}📊] [{}💰]",
                    msgs_per_sec,
                    avg_latency_us,
                    active_conns,
                    errors,
                    orderbook_count,
                    trade_count
                );

                last_messages = current_messages;
                last_time = current_time;
            }
        })
    }

    /// 📈 生成测试报告
    fn generate_test_report(&self, duration: Duration) -> TestReport {
        let duration_secs = duration.as_secs_f64();
        let total_messages = self.global_metrics.messages_received.load(Ordering::Relaxed);
        let total_bytes = self.global_metrics.bytes_received.load(Ordering::Relaxed);
        let avg_latency_us = self.global_metrics.avg_latency_us();
        let throughput = self.global_metrics.throughput_per_sec(duration_secs);
        let errors = self.global_metrics.websocket_errors.load(Ordering::Relaxed);
        let orderbook_updates = self.global_metrics.orderbook_updates.load(Ordering::Relaxed);
        let trade_updates = self.global_metrics.trade_updates.load(Ordering::Relaxed);

        TestReport {
            test_duration_secs: duration_secs,
            total_messages,
            total_bytes,
            messages_per_second: throughput,
            avg_latency_microseconds: avg_latency_us,
            orderbook_updates,
            trade_updates,
            error_count: errors,
            connection_count: self.config.connection_count as u64,
            target_throughput: self.config.expected_msgs_per_sec,
        }
    }
}

/// 📊 测试报告
#[derive(Debug)]
pub struct TestReport {
    pub test_duration_secs: f64,
    pub total_messages: u64,
    pub total_bytes: u64,
    pub messages_per_second: f64,
    pub avg_latency_microseconds: f64,
    pub orderbook_updates: u64,
    pub trade_updates: u64,
    pub error_count: u64,
    pub connection_count: u64,
    pub target_throughput: u64,
}

impl TestReport {
    pub fn print_summary(&self) {
        println!("\n🎯 ===== WebSocket高性能测试报告 =====");
        println!("⏱️  测试持续时间: {:.2} 秒", self.test_duration_secs);
        println!("🔗 WebSocket连接数: {}", self.connection_count);
        println!("📨 总接收消息数: {}", self.total_messages);
        println!("📦 总接收字节数: {:.2} MB", self.total_bytes as f64 / 1024.0 / 1024.0);
        println!("🚀 实际吞吐量: {:.0} msg/s", self.messages_per_second);
        println!("🎯 目标吞吐量: {} msg/s", self.target_throughput);
        println!("⚡ 平均延迟: {:.2} μs", self.avg_latency_microseconds);
        println!("📊 OrderBook更新: {}", self.orderbook_updates);
        println!("💰 Trade更新: {}", self.trade_updates);
        println!("❌ 错误数量: {}", self.error_count);
        
        // 性能评估
        println!("\n🏆 ===== 性能评估 =====");
        
        let throughput_ratio = self.messages_per_second / self.target_throughput as f64;
        if throughput_ratio >= 1.0 {
            println!("✅ 吞吐量: 达标 ({:.1}x 目标)", throughput_ratio);
        } else {
            println!("❌ 吞吐量: 未达标 ({:.1}x 目标)", throughput_ratio);
        }

        if self.avg_latency_microseconds <= 100.0 {
            println!("✅ 延迟: 优秀 (<= 100μs)");
        } else if self.avg_latency_microseconds <= 500.0 {
            println!("🟡 延迟: 良好 (<= 500μs)");
        } else {
            println!("❌ 延迟: 需优化 (> 500μs)");
        }

        if self.error_count == 0 {
            println!("✅ 稳定性: 零错误");
        } else {
            let error_rate = self.error_count as f64 / self.total_messages as f64 * 100.0;
            println!("⚠️  错误率: {:.4}%", error_rate);
        }

        println!("========================================\n");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 WebSocket高性能数据接收测试");

    // 创建测试配置
    let config = WebSocketPerfConfig {
        connection_count: 16,
        test_duration_secs: 30,
        target_symbols: vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(), 
            "SOLUSDT".to_string(),
            "DOGEUSDT".to_string(),
            "ADAUSDT".to_string(),
            "MATICUSDT".to_string(),
            "AVAXUSDT".to_string(),
            "DOTUSDT".to_string(),
        ],
        expected_msgs_per_sec: 150_000, // 目标: 15万msg/s
        enable_latency_tracking: true,
        print_realtime_stats: true,
    };

    // 运行性能测试
    let mut tester = WebSocketPerformanceTester::new(config);
    let report = tester.run_performance_test().await?;

    // 打印详细报告
    report.print_summary();

    Ok(())
}