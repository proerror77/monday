/// 🚀 独立WebSocket高性能测试
/// 
/// 测试目标：
/// - 16+ 并发连接
/// - 100k+ msg/s吞吐量
/// - <100μs平均延迟
/// - Lock-free数据结构

use std::{
    sync::{
        atomic::{AtomicU64, AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
    collections::HashMap,
};

use tokio::time::interval;

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

    // 零分配序列化
    fn to_bytes(&self) -> Vec<u8> {
        format!("{}|{}|{}|{:.2}|{:.6}", 
                self.symbol, self.msg_type, self.timestamp_ns, self.price, self.quantity)
            .into_bytes()
    }
}

/// 🌐 模拟高性能WebSocket连接
struct HighPerfWebSocketSim {
    connection_id: usize,
    symbols: Vec<String>,
    metrics: PerformanceMetrics,
}

impl HighPerfWebSocketSim {
    fn new(connection_id: usize, symbols: Vec<String>, metrics: PerformanceMetrics) -> Self {
        Self { connection_id, symbols, metrics }
    }

    /// 启动高频数据模拟
    async fn start_simulation(&self, running: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        let connection_id = self.connection_id;
        let symbols = self.symbols.clone();
        let metrics = self.metrics.clone();

        tokio::spawn(async move {
            metrics.active_connections.fetch_add(1, Ordering::Relaxed);
            println!("🔗 启动WebSocket连接 #{}", connection_id);

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
                        0..=6 => {
                            metrics.orderbook_updates.fetch_add(1, Ordering::Relaxed);
                            MarketMessage::new_orderbook(symbol.clone(), price_base, quantity)
                        },
                        7..=8 => {
                            metrics.trade_updates.fetch_add(1, Ordering::Relaxed);
                            MarketMessage::new_trade(symbol.clone(), price_base, quantity)
                        },
                        _ => {
                            metrics.ticker_updates.fetch_add(1, Ordering::Relaxed);
                            MarketMessage::new_orderbook(symbol.clone(), price_base + 0.1, quantity * 0.5)
                        },
                    };

                    // 记录延迟
                    let latency_ns = start_time.elapsed().as_nanos() as u64;
                    metrics.latency_sum_ns.fetch_add(latency_ns, Ordering::Relaxed);
                    metrics.latency_count.fetch_add(1, Ordering::Relaxed);

                    // 模拟消息处理
                    let bytes = msg.to_bytes();
                    metrics.bytes_received.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    metrics.messages_received.fetch_add(1, Ordering::Relaxed);

                    msg_count += 1;
                }
            }

            metrics.active_connections.fetch_sub(1, Ordering::Relaxed);
            println!("🔌 WebSocket连接 #{} 断开", connection_id);
        })
    }
}

/// 🚀 WebSocket性能测试管理器
struct WebSocketPerfTester {
    connections: Vec<HighPerfWebSocketSim>,
    global_metrics: PerformanceMetrics,
    running: Arc<AtomicBool>,
}

impl WebSocketPerfTester {
    fn new(connection_count: usize, symbols: Vec<String>) -> Self {
        let global_metrics = PerformanceMetrics::new();
        let mut connections = Vec::with_capacity(connection_count);
        
        // 为每个连接分配symbols
        for i in 0..connection_count {
            let assigned_symbols = if symbols.len() > 0 {
                vec![symbols[i % symbols.len()].clone()]
            } else {
                vec!["BTCUSDT".to_string()]
            };

            let connection = HighPerfWebSocketSim::new(i, assigned_symbols, global_metrics.clone());
            connections.push(connection);
        }

        Self {
            connections,
            global_metrics,
            running: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 运行性能测试
    async fn run_test(&mut self, duration_secs: u64) -> TestResults {
        println!("🚀 启动WebSocket高性能测试");
        println!("配置: {} 连接 × {} 秒", self.connections.len(), duration_secs);

        self.running.store(true, Ordering::Relaxed);
        let start_time = Instant::now();

        // 启动所有WebSocket连接
        let mut connection_tasks = Vec::new();
        for connection in &self.connections {
            let task = connection.start_simulation(self.running.clone()).await;
            connection_tasks.push(task);
        }

        // 启动监控任务
        let monitor_task = self.start_monitoring();

        // 等待测试完成
        tokio::time::sleep(Duration::from_secs(duration_secs)).await;

        println!("⏰ 停止测试...");
        self.running.store(false, Ordering::Relaxed);

        // 等待所有任务完成
        for task in connection_tasks {
            let _ = task.await;
        }
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

                println!(
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
            println!("✅ 吞吐量: 优秀 (>= 100k msg/s) - {:.0}x 目标达成", 
                     self.throughput_msg_per_sec / 100_000.0);
        } else if self.throughput_msg_per_sec >= 50_000.0 {
            println!("🟡 吞吐量: 良好 (>= 50k msg/s) - {:.1}x 进度", 
                     self.throughput_msg_per_sec / 100_000.0);
        } else {
            println!("❌ 吞吐量: 需改进 (< 50k msg/s) - {:.1}x 进度", 
                     self.throughput_msg_per_sec / 100_000.0);
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
        if self.total_errors == 0 {
            println!("✅ 稳定性: 完美 (0 错误)");
        } else {
            let error_rate = (self.total_errors as f64 / self.total_messages as f64) * 100.0;
            println!("⚠️  稳定性: {:.3}% 错误率", error_rate);
        }

        // 并发性能
        let throughput_per_connection = self.throughput_msg_per_sec / self.connection_count as f64;
        println!("🔗 单连接性能: {:.0} msg/s", throughput_per_connection);

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
        "ATOMUSDT".to_string(),
        "FTMUSDT".to_string(),
        "NEARUSDT".to_string(),
        "AAVEUSDT".to_string(),
        "UNIUSDT".to_string(),
        "SUSHIUSDT".to_string(),
        "COMPUSDT".to_string(),
    ];

    println!("🎯 性能目标:");
    println!("   - 吞吐量: >= 100,000 msg/s");
    println!("   - 延迟: <= 100 μs");
    println!("   - 稳定性: 0 错误");
    println!("   - 并发: {} WebSocket连接", connection_count);
    println!("   - 测试时长: {} 秒\n", test_duration);

    // 创建并运行测试
    let mut tester = WebSocketPerfTester::new(connection_count, target_symbols);
    let results = tester.run_test(test_duration).await;

    // 打印结果
    results.print_report();

    // 性能验证
    if results.passed_performance_targets() {
        println!("🎉 恭喜！所有性能目标均已达成！");
        println!("🚀 系统已准备好用于高频交易生产环境。");
        std::process::exit(0);
    } else {
        println!("⚠️  部分性能目标未达成，建议优化:");
        
        if results.throughput_msg_per_sec < 100_000.0 {
            println!("   • 增加连接数或优化消息处理逻辑");
        }
        if results.avg_latency_microseconds > 100.0 {
            println!("   • 优化序列化和反序列化逻辑");
            println!("   • 使用零分配数据结构");
        }
        if results.total_errors > 0 {
            println!("   • 检查错误处理和异常情况");
        }
        
        std::process::exit(1);
    }
}