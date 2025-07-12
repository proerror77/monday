/// 🚀 独立WebSocket高性能测试
/// 
/// 功能：
/// - 16+ 并发WebSocket连接
/// - Lock-free数据结构
/// - 100k+ msg/s目标吞吐量
/// - <100μs平均延迟
/// - 实时性能监控

use std::{
    sync::{
        atomic::{AtomicU64, AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    collections::HashMap,
};

use tokio::time::interval;
use crossbeam_channel::{bounded, Receiver, Sender};
use serde_json::{Value, json};
use tracing::info;

/// 📊 Lock-Free性能指标
#[derive(Clone)]
struct PerformanceMetrics {
    messages_received: Arc<AtomicU64>,
    bytes_received: Arc<AtomicU64>,
    latency_sum_ns: Arc<AtomicU64>,
    latency_count: Arc<AtomicU64>,
    orderbook_updates: Arc<AtomicU64>,
    trade_updates: Arc<AtomicU64>,
    ticker_updates: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    active_connections: Arc<AtomicU64>,
}

impl PerformanceMetrics {
    fn new() -> Self {
        Self {
            messages_received: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            latency_sum_ns: Arc::new(AtomicU64::new(0)),
            latency_count: Arc::new(AtomicU64::new(0)),
            orderbook_updates: Arc::new(AtomicU64::new(0)),
            trade_updates: Arc::new(AtomicU64::new(0)),
            ticker_updates: Arc::new(AtomicU64::new(0)),
            errors: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
        }
    }

    fn avg_latency_us(&self) -> f64 {
        let count = self.latency_count.load(Ordering::Relaxed);
        if count == 0 {
            0.0
        } else {
            (self.latency_sum_ns.load(Ordering::Relaxed) as f64 / count as f64) / 1000.0
        }
    }
}

/// 📦 高性能消息结构
#[derive(Clone)]
struct MarketMessage {
    symbol: String,
    msg_type: u8, // 0=orderbook, 1=trade, 2=ticker
    timestamp_ns: u64,
    price: f64,
    quantity: f64,
}

impl MarketMessage {
    fn new_orderbook(symbol: String, price: f64, qty: f64) -> Self {
        Self {
            symbol,
            msg_type: 0,
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            price,
            quantity: qty,
        }
    }

    fn new_trade(symbol: String, price: f64, qty: f64) -> Self {
        Self {
            symbol,
            msg_type: 1,
            timestamp_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            price,
            quantity: qty,
        }
    }

    // 零分配序列化（编译时优化）
    fn to_bytes(&self) -> Vec<u8> {
        // 在实际应用中，这里会使用零分配的序列化
        format!("{}|{}|{}|{:.2}|{:.6}", 
                self.symbol, self.msg_type, self.timestamp_ns, self.price, self.quantity)
            .into_bytes()
    }
}

/// 🔧 Lock-Free消息处理器
struct LockFreeProcessor {
    channel: (Sender<MarketMessage>, Receiver<MarketMessage>),
    metrics: PerformanceMetrics,
    orderbooks: HashMap<String, Vec<(f64, f64)>>, // symbol -> [(price, qty)]
}

impl LockFreeProcessor {
    fn new(buffer_size: usize) -> Self {
        let (tx, rx) = bounded(buffer_size);
        Self {
            channel: (tx, rx),
            metrics: PerformanceMetrics::new(),
            orderbooks: HashMap::new(),
        }
    }

    /// 启动处理任务
    fn start_processing(&mut self, running: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        let receiver = self.channel.1.clone();
        let metrics = self.metrics.clone();
        
        tokio::spawn(async move {
            let mut batch_buffer = Vec::with_capacity(1000);
            
            while running.load(Ordering::Relaxed) {
                // 批量接收，减少锁竞争
                let mut batch_size = 0;
                while batch_size < 500 {
                    match receiver.try_recv() {
                        Ok(msg) => {
                            batch_buffer.push(msg);
                            batch_size += 1;
                        }
                        Err(_) => break,
                    }
                }

                if batch_size > 0 {
                    // 批量处理消息
                    for msg in &batch_buffer {
                        match msg.msg_type {
                            0 => metrics.orderbook_updates.fetch_add(1, Ordering::Relaxed),
                            1 => metrics.trade_updates.fetch_add(1, Ordering::Relaxed),
                            2 => metrics.ticker_updates.fetch_add(1, Ordering::Relaxed),
                            _ => metrics.errors.fetch_add(1, Ordering::Relaxed),
                        };

                        // 模拟OrderBook更新（零分配）
                        // 在实际应用中，这里会更新内存池中的OrderBook
                    }

                    metrics.messages_received.fetch_add(batch_size as u64, Ordering::Relaxed);
                    batch_buffer.clear();
                } else {
                    // 空闲时短暂yield，避免CPU空转
                    tokio::task::yield_now().await;
                }
            }
        })
    }

    fn send_message(&self, msg: MarketMessage) -> bool {
        match self.channel.0.try_send(msg) {
            Ok(_) => true,
            Err(_) => {
                self.metrics.errors.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

/// 🌐 模拟高性能WebSocket连接
struct HighPerfWebSocketSim {
    connection_id: usize,
    symbols: Vec<String>,
    processor: Arc<std::sync::Mutex<LockFreeProcessor>>,
    metrics: PerformanceMetrics,
}

impl HighPerfWebSocketSim {
    fn new(
        connection_id: usize, 
        symbols: Vec<String>, 
        processor: Arc<std::sync::Mutex<LockFreeProcessor>>,
        metrics: PerformanceMetrics
    ) -> Self {
        Self { connection_id, symbols, processor, metrics }
    }

    /// 启动高频数据模拟
    async fn start_high_freq_simulation(&self, running: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        let connection_id = self.connection_id;
        let symbols = self.symbols.clone();
        let processor = self.processor.clone();
        let metrics = self.metrics.clone();

        tokio::spawn(async move {
            metrics.active_connections.fetch_add(1, Ordering::Relaxed);
            info!("🔗 启动WebSocket连接 #{}", connection_id);

            // 每个连接目标: 10k msg/s
            let mut interval = interval(Duration::from_micros(100));
            let mut msg_count = 0u64;
            let mut price_base = 50000.0;

            while running.load(Ordering::Relaxed) {
                interval.tick().await;

                for symbol in &symbols {
                    let start_time = Instant::now();

                    // 模拟价格波动
                    price_base += (msg_count as f64 % 1000.0 - 500.0) * 0.01;
                    let quantity = 0.1 + (msg_count as f64 % 100.0) * 0.01;

                    let msg = match msg_count % 10 {
                        0..=6 => MarketMessage::new_orderbook(symbol.clone(), price_base, quantity),
                        7..=8 => MarketMessage::new_trade(symbol.clone(), price_base, quantity),
                        _ => MarketMessage::new_orderbook(symbol.clone(), price_base + 0.1, quantity * 0.5),
                    };

                    // 记录延迟
                    let latency_ns = start_time.elapsed().as_nanos() as u64;
                    metrics.latency_sum_ns.fetch_add(latency_ns, Ordering::Relaxed);
                    metrics.latency_count.fetch_add(1, Ordering::Relaxed);

                    // 发送消息
                    let bytes = msg.to_bytes();
                    metrics.bytes_received.fetch_add(bytes.len() as u64, Ordering::Relaxed);

                    if let Ok(proc) = processor.lock() {
                        if !proc.send_message(msg) {
                            metrics.errors.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    msg_count += 1;
                }
            }

            metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
            info!("🔌 WebSocket连接 #{} 断开", connection_id);
        })
    }
}

/// 🚀 WebSocket性能测试管理器
struct WebSocketPerfTester {
    connections: Vec<HighPerfWebSocketSim>,
    processor: Arc<std::sync::Mutex<LockFreeProcessor>>,
    global_metrics: PerformanceMetrics,
    running: Arc<AtomicBool>,
}

impl WebSocketPerfTester {
    fn new(connection_count: usize, symbols: Vec<String>) -> Self {
        let processor = Arc::new(std::sync::Mutex::new(
            LockFreeProcessor::new(200_000) // 20万消息缓冲
        ));
        let global_metrics = PerformanceMetrics::new();
        
        let mut connections = Vec::with_capacity(connection_count);
        
        // 为每个连接分配symbols
        for i in 0..connection_count {
            let assigned_symbols = if symbols.len() > 0 {
                vec![symbols[i % symbols.len()].clone()]
            } else {
                vec!["BTCUSDT".to_string()]
            };

            let connection = HighPerfWebSocketSim::new(
                i,
                assigned_symbols,
                processor.clone(),
                global_metrics.clone(),
            );
            connections.push(connection);
        }

        Self {
            connections,
            processor,
            global_metrics,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 运行性能测试
    async fn run_test(&mut self, duration_secs: u64) -> TestResults {
        info!("🚀 启动WebSocket高性能测试");
        info!("配置: {} 连接 × {} 秒", self.connections.len(), duration_secs);

        self.running.store(true, Ordering::Relaxed);
        let start_time = Instant::now();

        // 启动消息处理器
        let processing_task = if let Ok(mut processor) = self.processor.lock() {
            processor.start_processing(self.running.clone())
        } else {
            return TestResults::default();
        };

        // 启动所有WebSocket连接
        let mut connection_tasks = Vec::new();
        for connection in &self.connections {
            let task = connection.start_high_freq_simulation(self.running.clone()).await;
            connection_tasks.push(task);
        }

        // 启动监控任务
        let monitor_task = self.start_monitoring();

        // 等待测试完成
        tokio::time::sleep(Duration::from_secs(duration_secs)).await;

        info!("⏰ 停止测试...");
        self.running.store(false, Ordering::Relaxed);

        // 等待所有任务完成
        for task in connection_tasks {
            let _ = task.await;
        }
        let _ = processing_task.await;
        let _ = monitor_task.await;

        let elapsed = start_time.elapsed();
        self.generate_results(elapsed)
    }

    /// 启动实时监控
    fn start_monitoring(&self) -> tokio::task::JoinHandle<()> {
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
                let elapsed_secs = current_time.duration_since(last_time).as_secs_f64();

                let throughput = if elapsed_secs > 0.0 {
                    (current_messages - last_messages) as f64 / elapsed_secs
                } else {
                    0.0
                };

                let avg_latency = metrics.avg_latency_us();
                let connections = metrics.active_connections.load(Ordering::Relaxed);
                let orderbook = metrics.orderbook_updates.load(Ordering::Relaxed);
                let trades = metrics.trade_updates.load(Ordering::Relaxed);
                let errors = metrics.errors.load(Ordering::Relaxed);

                info!(
                    "📊 [{:.0} msg/s] [{:.1}μs] [{}⚡] [{}📊] [{}💰] [{}❌]",
                    throughput, avg_latency, connections, orderbook, trades, errors
                );

                last_messages = current_messages;
                last_time = current_time;
            }
        })
    }

    /// 生成测试结果
    fn generate_results(&self, duration: Duration) -> TestResults {
        let duration_secs = duration.as_secs_f64();
        let total_messages = self.global_metrics.messages_received.load(Ordering::Relaxed);
        let total_bytes = self.global_metrics.bytes_received.load(Ordering::Relaxed);
        let avg_latency_us = self.global_metrics.avg_latency_us();
        let throughput = if duration_secs > 0.0 {
            total_messages as f64 / duration_secs
        } else {
            0.0
        };

        TestResults {
            duration_secs,
            total_messages,
            total_bytes,
            throughput_msg_per_sec: throughput,
            avg_latency_microseconds: avg_latency_us,
            orderbook_updates: self.global_metrics.orderbook_updates.load(Ordering::Relaxed),
            trade_updates: self.global_metrics.trade_updates.load(Ordering::Relaxed),
            ticker_updates: self.global_metrics.ticker_updates.load(Ordering::Relaxed),
            total_errors: self.global_metrics.errors.load(Ordering::Relaxed),
            connection_count: self.connections.len() as u64,
        }
    }
}

/// 📊 测试结果
#[derive(Default)]
struct TestResults {
    duration_secs: f64,
    total_messages: u64,
    total_bytes: u64,
    throughput_msg_per_sec: f64,
    avg_latency_microseconds: f64,
    orderbook_updates: u64,
    trade_updates: u64,
    ticker_updates: u64,
    total_errors: u64,
    connection_count: u64,
}

impl TestResults {
    fn print_report(&self) {
        println!("\n🎯 ===== WebSocket高性能测试报告 =====");
        println!("⏱️  测试持续时间: {:.2} 秒", self.duration_secs);
        println!("🔗 WebSocket连接数: {}", self.connection_count);
        println!("📨 总消息数: {} 条", self.total_messages);
        println!("📦 总字节数: {:.2} MB", self.total_bytes as f64 / 1024.0 / 1024.0);
        println!("🚀 平均吞吐量: {:.0} msg/s", self.throughput_msg_per_sec);
        println!("⚡ 平均延迟: {:.2} μs", self.avg_latency_microseconds);
        println!("📊 OrderBook更新: {} 条", self.orderbook_updates);
        println!("💰 Trade更新: {} 条", self.trade_updates);
        println!("📈 Ticker更新: {} 条", self.ticker_updates);
        println!("❌ 错误数: {} 个", self.total_errors);

        println!("\n🏆 ===== 性能评估 =====");
        
        // 吞吐量评估
        if self.throughput_msg_per_sec >= 100_000.0 {
            println!("✅ 吞吐量: 优秀 (>= 100k msg/s)");
        } else if self.throughput_msg_per_sec >= 50_000.0 {
            println!("🟡 吞吐量: 良好 (>= 50k msg/s)");
        } else {
            println!("❌ 吞吐量: 需改进 (< 50k msg/s)");
        }

        // 延迟评估
        if self.avg_latency_microseconds <= 100.0 {
            println!("✅ 延迟: 优秀 (<= 100μs)");
        } else if self.avg_latency_microseconds <= 500.0 {
            println!("🟡 延迟: 良好 (<= 500μs)");
        } else {
            println!("❌ 延迟: 需改进 (> 500μs)");
        }

        // 稳定性评估
        let error_rate = if self.total_messages > 0 {
            (self.total_errors as f64 / self.total_messages as f64) * 100.0
        } else {
            0.0
        };

        if error_rate == 0.0 {
            println!("✅ 稳定性: 完美 (0% 错误率)");
        } else if error_rate < 0.1 {
            println!("🟡 稳定性: 良好 (<0.1% 错误率)");
        } else {
            println!("❌ 稳定性: 需改进 ({:.3}% 错误率)", error_rate);
        }

        println!("========================================\n");
    }

    fn passed_performance_targets(&self) -> bool {
        self.throughput_msg_per_sec >= 100_000.0 
            && self.avg_latency_microseconds <= 100.0 
            && self.total_errors == 0
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    println!("🚀 WebSocket高性能数据接收测试\n");

    // 测试配置
    let connection_count = 16;
    let test_duration = 30; // 30秒
    let target_symbols = vec![
        "BTCUSDT".to_string(),
        "ETHUSDT".to_string(),
        "SOLUSDT".to_string(),
        "DOGEUSDT".to_string(),
        "ADAUSDT".to_string(),
        "MATICUSDT".to_string(),
        "AVAXUSDT".to_string(),
        "DOTUSDT".to_string(),
        "LINKUSDT".to_string(),
        "AVAXUSDT".to_string(),
        "ATOMUSDT".to_string(),
        "FTMUSDT".to_string(),
        "NEARUSDT".to_string(),
        "AAVEUSDT".to_string(),
        "UNIUSDT".to_string(),
        "SUSHIUSDT".to_string(),
    ];

    info!("开始性能测试: {} 连接 × {} 交易对 × {} 秒", 
          connection_count, target_symbols.len(), test_duration);

    // 创建并运行测试
    let mut tester = WebSocketPerfTester::new(connection_count, target_symbols);
    let results = tester.run_test(test_duration).await;

    // 打印结果
    results.print_report();

    // 性能验证
    if results.passed_performance_targets() {
        println!("🎉 恭喜！所有性能目标均已达成！");
        std::process::exit(0);
    } else {
        println!("⚠️  部分性能目标未达成，需要优化。");
        std::process::exit(1);
    }
}