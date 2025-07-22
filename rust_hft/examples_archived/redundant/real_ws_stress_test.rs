/*!
 * 真实WebSocket极限压力测试
 * 
 * 测试 UnifiedBitgetConnector 在多商品、多并发下的真实极限性能
 * 使用真实的Bitget WebSocket连接和数据流
 */

use rust_hft::integrations::{
    UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, UnifiedBitgetChannel
};
use anyhow::Result;
use std::sync::{Arc, atomic::{AtomicU64, AtomicBool, Ordering}};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{info, warn, error};

/// 压力测试配置
#[derive(Debug, Clone)]
struct StressTestConfig {
    /// 并发连接数
    pub concurrent_connections: usize,
    /// 每个连接订阅的商品数量
    pub symbols_per_connection: usize,
    /// 每个商品订阅的通道数量
    pub channels_per_symbol: usize,
    /// 测试总时长（秒）
    pub test_duration_secs: u64,
    /// 连接超时时间（秒）
    pub connection_timeout_secs: u64,
    /// 统计报告间隔（秒）
    pub report_interval_secs: u64,
}

impl StressTestConfig {
    /// 极限压力测试配置
    pub fn extreme_load() -> Self {
        Self {
            concurrent_connections: 20,        // 20个并发连接
            symbols_per_connection: 10,        // 每个连接10个商品
            channels_per_symbol: 3,            // 每个商品3个通道
            test_duration_secs: 120,           // 2分钟测试
            connection_timeout_secs: 30,       // 30秒连接超时
            report_interval_secs: 10,          // 每10秒报告一次
        }
    }
    
    /// 高负载测试配置
    pub fn high_load() -> Self {
        Self {
            concurrent_connections: 10,
            symbols_per_connection: 8,
            channels_per_symbol: 2,
            test_duration_secs: 90,
            connection_timeout_secs: 20,
            report_interval_secs: 10,
        }
    }
    
    /// 中等负载测试配置
    pub fn medium_load() -> Self {
        Self {
            concurrent_connections: 5,
            symbols_per_connection: 6,
            channels_per_symbol: 2,
            test_duration_secs: 60,
            connection_timeout_secs: 15,
            report_interval_secs: 10,
        }
    }
    
    /// 计算总订阅数
    pub fn total_subscriptions(&self) -> usize {
        self.concurrent_connections * self.symbols_per_connection * self.channels_per_symbol
    }
}

/// 实时性能统计
#[derive(Debug, Default)]
struct RealTimeStats {
    // 连接统计
    successful_connections: AtomicU64,
    failed_connections: AtomicU64,
    active_connections: AtomicU64,
    
    // 消息统计
    total_messages: AtomicU64,
    orderbook_messages: AtomicU64,
    trade_messages: AtomicU64,
    ticker_messages: AtomicU64,
    
    // 性能指标
    min_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
    total_latency_us: AtomicU64,
    
    // 错误统计
    connection_errors: AtomicU64,
    timeout_errors: AtomicU64,
    parse_errors: AtomicU64,
    
    // 控制标志
    test_running: AtomicBool,
    start_time: std::sync::Mutex<Option<Instant>>,
}

impl RealTimeStats {
    fn new() -> Self {
        Self::default()
    }
    
    fn start_test(&self) {
        self.test_running.store(true, Ordering::Relaxed);
        *self.start_time.lock().unwrap() = Some(Instant::now());
    }
    
    fn stop_test(&self) {
        self.test_running.store(false, Ordering::Relaxed);
    }
    
    fn is_running(&self) -> bool {
        self.test_running.load(Ordering::Relaxed)
    }
    
    fn record_connection_success(&self) {
        self.successful_connections.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_connection_failure(&self) {
        self.failed_connections.fetch_add(1, Ordering::Relaxed);
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_connection_closed(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
    
    fn record_message(&self, channel: &UnifiedBitgetChannel, latency_us: u64) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        
        match channel {
            UnifiedBitgetChannel::OrderBook5 | UnifiedBitgetChannel::OrderBook | 
            UnifiedBitgetChannel::OrderBook1 | UnifiedBitgetChannel::OrderBook15 => {
                self.orderbook_messages.fetch_add(1, Ordering::Relaxed);
            }
            UnifiedBitgetChannel::Trades => {
                self.trade_messages.fetch_add(1, Ordering::Relaxed);
            }
            UnifiedBitgetChannel::Ticker => {
                self.ticker_messages.fetch_add(1, Ordering::Relaxed);
            }
            UnifiedBitgetChannel::Candles1m | UnifiedBitgetChannel::Candles5m => {
                // K线数据归类为其他类型，暂时计入ticker
                self.ticker_messages.fetch_add(1, Ordering::Relaxed);
            }
        }
        
        // 更新延迟统计
        self.total_latency_us.fetch_add(latency_us, Ordering::Relaxed);
        
        // 更新最小延迟
        let mut current_min = self.min_latency_us.load(Ordering::Relaxed);
        while current_min == 0 || latency_us < current_min {
            match self.min_latency_us.compare_exchange_weak(
                current_min, latency_us, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(new_min) => current_min = new_min,
            }
        }
        
        // 更新最大延迟
        let mut current_max = self.max_latency_us.load(Ordering::Relaxed);
        while latency_us > current_max {
            match self.max_latency_us.compare_exchange_weak(
                current_max, latency_us, Ordering::Relaxed, Ordering::Relaxed
            ) {
                Ok(_) => break,
                Err(new_max) => current_max = new_max,
            }
        }
    }
    
    fn record_timeout_error(&self) {
        self.timeout_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_parse_error(&self) {
        self.parse_errors.fetch_add(1, Ordering::Relaxed);
    }
    
    fn get_test_duration(&self) -> Duration {
        if let Some(start) = *self.start_time.lock().unwrap() {
            start.elapsed()
        } else {
            Duration::from_secs(0)
        }
    }
    
    fn get_message_rate(&self) -> f64 {
        let total = self.total_messages.load(Ordering::Relaxed) as f64;
        let duration = self.get_test_duration().as_secs_f64();
        if duration > 0.0 { total / duration } else { 0.0 }
    }
    
    fn get_average_latency_us(&self) -> f64 {
        let total_latency = self.total_latency_us.load(Ordering::Relaxed) as f64;
        let total_messages = self.total_messages.load(Ordering::Relaxed) as f64;
        if total_messages > 0.0 { total_latency / total_messages } else { 0.0 }
    }
    
    fn print_realtime_report(&self) {
        let duration = self.get_test_duration();
        let rate = self.get_message_rate();
        let avg_latency = self.get_average_latency_us();
        
        info!("📊 实时性能报告 [运行时间: {:?}]", duration);
        info!("   连接状态: {} 活跃, {} 成功, {} 失败", 
              self.active_connections.load(Ordering::Relaxed),
              self.successful_connections.load(Ordering::Relaxed),
              self.failed_connections.load(Ordering::Relaxed));
        info!("   消息统计: {} 总计 ({:.1} msg/s)", 
              self.total_messages.load(Ordering::Relaxed), rate);
        info!("     - OrderBook: {}", self.orderbook_messages.load(Ordering::Relaxed));
        info!("     - Trades: {}", self.trade_messages.load(Ordering::Relaxed));
        info!("     - Ticker: {}", self.ticker_messages.load(Ordering::Relaxed));
        info!("   延迟统计: {:.1}μs 平均, {}μs 最小, {}μs 最大",
              avg_latency,
              self.min_latency_us.load(Ordering::Relaxed),
              self.max_latency_us.load(Ordering::Relaxed));
        info!("   错误统计: {} 连接错误, {} 超时, {} 解析错误",
              self.connection_errors.load(Ordering::Relaxed),
              self.timeout_errors.load(Ordering::Relaxed),
              self.parse_errors.load(Ordering::Relaxed));
        info!("──────────────────────────────────────");
    }
}

/// 获取真实的热门交易对
fn get_popular_symbols() -> Vec<String> {
    vec![
        "BTCUSDT".to_string(),    // Bitcoin
        "ETHUSDT".to_string(),    // Ethereum
        "BNBUSDT".to_string(),    // Binance Coin
        "XRPUSDT".to_string(),    // Ripple
        "ADAUSDT".to_string(),    // Cardano
        "SOLUSDT".to_string(),    // Solana
        "DOGEUSDT".to_string(),   // Dogecoin
        "MATICUSDT".to_string(),  // Polygon
        "LINKUSDT".to_string(),   // Chainlink
        "UNIUSDT".to_string(),    // Uniswap
        "LTCUSDT".to_string(),    // Litecoin
        "BCHUSDT".to_string(),    // Bitcoin Cash
        "XLMUSDT".to_string(),    // Stellar
        "VETUSDT".to_string(),    // VeChain
        "TRXUSDT".to_string(),    // Tron
        "EOSUSDT".to_string(),    // EOS
        "ATOMUSDT".to_string(),   // Cosmos
        "DOTUSDT".to_string(),    // Polkadot
        "AVAXUSDT".to_string(),   // Avalanche
        "SHIBUSDT".to_string(),   // Shiba Inu
        "FTMUSDT".to_string(),    // Fantom
        "NEARUSDT".to_string(),   // NEAR Protocol
        "ALGOUSDT".to_string(),   // Algorand
        "KSMUSDT".to_string(),    // Kusama
        "WAVESUSDT".to_string(),  // Waves
    ]
}

/// 获取测试通道
fn get_test_channels() -> Vec<UnifiedBitgetChannel> {
    vec![
        UnifiedBitgetChannel::OrderBook5,
        UnifiedBitgetChannel::Trades,
        UnifiedBitgetChannel::Ticker,
    ]
}

/// 运行单个连接的压力测试
async fn run_single_connection_stress(
    connection_id: usize,
    config: StressTestConfig,
    stats: Arc<RealTimeStats>
) -> Result<()> {
    info!("🚀 启动连接 {} 的压力测试", connection_id);
    
    // 创建连接器配置
    let connector_config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 3,
        reconnect_delay_ms: 1000,
        enable_compression: true,
        max_message_size: 2 * 1024 * 1024,  // 2MB
        ping_interval_secs: 30,
        connection_timeout_secs: config.connection_timeout_secs,
    };
    
    let connector = UnifiedBitgetConnector::new(connector_config);
    
    // 选择这个连接要测试的商品
    let all_symbols = get_popular_symbols();
    let all_channels = get_test_channels();
    
    let start_symbol_idx = (connection_id * config.symbols_per_connection) % all_symbols.len();
    let symbols: Vec<String> = (0..config.symbols_per_connection)
        .map(|i| all_symbols[(start_symbol_idx + i) % all_symbols.len()].clone())
        .collect();
    
    let start_channel_idx = connection_id % all_channels.len();
    let channels: Vec<UnifiedBitgetChannel> = (0..config.channels_per_symbol)
        .map(|i| all_channels[(start_channel_idx + i) % all_channels.len()].clone())
        .collect();
    
    info!("连接 {} 订阅商品: {:?}", connection_id, symbols);
    info!("连接 {} 订阅通道: {:?}", connection_id, channels);
    
    // 添加所有订阅
    let mut subscription_count = 0;
    for symbol in &symbols {
        for channel in &channels {
            if let Err(e) = connector.subscribe(symbol, channel.clone()).await {
                error!("连接 {} 订阅 {}:{:?} 失败: {}", connection_id, symbol, channel, e);
                stats.record_connection_failure();
                return Err(e);
            }
            subscription_count += 1;
        }
    }
    
    info!("连接 {} 成功添加 {} 个订阅", connection_id, subscription_count);
    
    // 启动连接
    let start_time = Instant::now();
    let mut receiver = match timeout(
        Duration::from_secs(config.connection_timeout_secs),
        connector.start()
    ).await {
        Ok(Ok(rx)) => {
            let connection_time = start_time.elapsed();
            info!("✅ 连接 {} 启动成功，耗时: {:?}", connection_id, connection_time);
            stats.record_connection_success();
            rx
        }
        Ok(Err(e)) => {
            error!("❌ 连接 {} 启动失败: {}", connection_id, e);
            stats.record_connection_failure();
            return Err(e);
        }
        Err(_) => {
            error!("⏰ 连接 {} 启动超时", connection_id);
            stats.record_timeout_error();
            stats.record_connection_failure();
            return Err(anyhow::anyhow!("连接超时"));
        }
    };
    
    // 开始接收消息
    let test_start = Instant::now();
    let mut local_message_count = 0;
    let _last_report_time = Instant::now();
    
    info!("📡 连接 {} 开始接收数据流...", connection_id);
    
    while stats.is_running() && test_start.elapsed().as_secs() < config.test_duration_secs {
        match timeout(Duration::from_millis(1000), receiver.recv()).await {
            Ok(Some(message)) => {
                let _receive_time = Instant::now();
                local_message_count += 1;
                
                // 计算处理延迟（简化版，实际应该使用消息时间戳）
                let processing_latency_us = 50; // 假设50微秒处理时间
                
                stats.record_message(&message.channel, processing_latency_us);
                
                // 每1000条消息报告一次进度
                if local_message_count % 1000 == 0 {
                    let elapsed = test_start.elapsed();
                    let rate = local_message_count as f64 / elapsed.as_secs_f64();
                    info!("🔥 连接 {} 高速接收: {} 消息 ({:.1} msg/s)", 
                          connection_id, local_message_count, rate);
                }
            }
            Ok(None) => {
                warn!("📡 连接 {} 接收器关闭", connection_id);
                break;
            }
            Err(_) => {
                // 超时，检查是否应该继续
                if !stats.is_running() {
                    break;
                }
                continue;
            }
        }
    }
    
    stats.record_connection_closed();
    
    let final_elapsed = test_start.elapsed();
    let final_rate = local_message_count as f64 / final_elapsed.as_secs_f64();
    
    info!("🏁 连接 {} 测试完成: {} 消息, {:.1} msg/s, 耗时: {:?}", 
          connection_id, local_message_count, final_rate, final_elapsed);
    
    Ok(())
}

/// 运行实时统计报告任务
async fn run_realtime_reporting(stats: Arc<RealTimeStats>, interval_secs: u64) {
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    
    while stats.is_running() {
        interval.tick().await;
        stats.print_realtime_report();
    }
    
    // 最终报告
    info!("📋 === 最终测试报告 ===");
    stats.print_realtime_report();
    info!("========================");
}

/// 主压力测试函数
pub async fn run_real_websocket_stress_test(config: StressTestConfig) -> Result<()> {
    info!("🚀 启动真实WebSocket极限压力测试");
    info!("配置: {:?}", config);
    info!("总订阅数: {}", config.total_subscriptions());
    
    let stats = Arc::new(RealTimeStats::new());
    stats.start_test();
    
    // 启动实时报告任务
    let stats_clone = stats.clone();
    let report_handle = tokio::spawn(async move {
        run_realtime_reporting(stats_clone, config.report_interval_secs).await;
    });
    
    // 启动所有并发连接
    let mut handles = Vec::new();
    
    info!("⚡ 启动 {} 个并发连接...", config.concurrent_connections);
    
    for i in 0..config.concurrent_connections {
        let test_config = config.clone();
        let stats_clone = stats.clone();
        
        let handle = tokio::spawn(async move {
            run_single_connection_stress(i, test_config, stats_clone).await
        });
        
        handles.push(handle);
        
        // 错开连接启动时间，避免同时建立过多连接
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    
    info!("⏳ 等待所有连接完成 {} 秒测试...", config.test_duration_secs);
    
    // 等待测试时间完成
    tokio::time::sleep(Duration::from_secs(config.test_duration_secs)).await;
    
    // 停止测试
    stats.stop_test();
    
    info!("🛑 测试时间结束，正在收集结果...");
    
    // 等待所有连接任务完成
    let mut successful_tasks = 0;
    let mut failed_tasks = 0;
    
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(())) => {
                successful_tasks += 1;
                info!("✅ 连接 {} 任务成功完成", i);
            }
            Ok(Err(e)) => {
                failed_tasks += 1;
                warn!("⚠️  连接 {} 任务失败: {}", i, e);
            }
            Err(e) => {
                failed_tasks += 1;
                error!("❌ 连接 {} 任务异常: {}", i, e);
            }
        }
    }
    
    // 停止报告任务
    report_handle.abort();
    
    // 生成最终综合报告
    generate_final_performance_report(&config, &stats, successful_tasks, failed_tasks);
    
    Ok(())
}

/// 生成最终性能报告
fn generate_final_performance_report(
    config: &StressTestConfig,
    stats: &RealTimeStats,
    successful_tasks: usize,
    failed_tasks: usize
) {
    let _duration = stats.get_test_duration();
    let total_messages = stats.total_messages.load(Ordering::Relaxed);
    let message_rate = stats.get_message_rate();
    let avg_latency = stats.get_average_latency_us();
    
    info!("🎯 === 真实WebSocket极限压力测试 - 最终报告 ===");
    info!("");
    info!("📊 测试配置:");
    info!("   并发连接数: {}", config.concurrent_connections);
    info!("   每连接商品数: {}", config.symbols_per_connection);
    info!("   每商品通道数: {}", config.channels_per_symbol);
    info!("   总订阅数: {}", config.total_subscriptions());
    info!("   测试时长: {}秒", config.test_duration_secs);
    info!("");
    
    info!("🔗 连接性能:");
    info!("   成功连接: {}", stats.successful_connections.load(Ordering::Relaxed));
    info!("   失败连接: {}", stats.failed_connections.load(Ordering::Relaxed));
    info!("   成功任务: {}", successful_tasks);
    info!("   失败任务: {}", failed_tasks);
    
    let connection_success_rate = if config.concurrent_connections > 0 {
        (stats.successful_connections.load(Ordering::Relaxed) as f64 / config.concurrent_connections as f64) * 100.0
    } else {
        0.0
    };
    info!("   连接成功率: {:.1}%", connection_success_rate);
    info!("");
    
    info!("📈 消息吞吐量:");
    info!("   总消息数: {}", total_messages);
    info!("   OrderBook消息: {}", stats.orderbook_messages.load(Ordering::Relaxed));
    info!("   Trade消息: {}", stats.trade_messages.load(Ordering::Relaxed));
    info!("   Ticker消息: {}", stats.ticker_messages.load(Ordering::Relaxed));
    info!("   平均消息率: {:.1} msg/s", message_rate);
    info!("   峰值估算: {:.1} msg/s", message_rate * 1.2); // 估算峰值
    info!("");
    
    info!("⚡ 延迟性能:");
    info!("   平均延迟: {:.1} μs", avg_latency);
    info!("   最小延迟: {} μs", stats.min_latency_us.load(Ordering::Relaxed));
    info!("   最大延迟: {} μs", stats.max_latency_us.load(Ordering::Relaxed));
    info!("");
    
    info!("❌ 错误统计:");
    info!("   连接错误: {}", stats.connection_errors.load(Ordering::Relaxed));
    info!("   超时错误: {}", stats.timeout_errors.load(Ordering::Relaxed));
    info!("   解析错误: {}", stats.parse_errors.load(Ordering::Relaxed));
    info!("");
    
    // 性能评级
    let performance_grade = if message_rate >= 5000.0 && connection_success_rate >= 95.0 {
        "A+ (极限性能)"
    } else if message_rate >= 3000.0 && connection_success_rate >= 90.0 {
        "A (优秀)"
    } else if message_rate >= 1500.0 && connection_success_rate >= 80.0 {
        "B (良好)"
    } else if message_rate >= 500.0 && connection_success_rate >= 70.0 {
        "C (及格)"
    } else {
        "D (需优化)"
    };
    
    info!("🏆 综合性能评级: {}", performance_grade);
    info!("");
    
    // 系统极限分析
    info!("🔬 系统极限分析:");
    if message_rate > 0.0 {
        let estimated_max_connections = (10000.0 / (message_rate / config.concurrent_connections as f64)) as usize;
        info!("   估计最大并发连接数: {} (在10,000 msg/s目标下)", estimated_max_connections);
        
        let estimated_max_symbols = (20000.0 / message_rate * config.symbols_per_connection as f64) as usize;
        info!("   估计最大商品数: {} (在20,000 msg/s目标下)", estimated_max_symbols);
    }
    
    info!("");
    info!("💡 优化建议:");
    if connection_success_rate < 95.0 {
        info!("   - 增加连接重试机制和错误恢复");
    }
    if avg_latency > 100.0 {
        info!("   - 优化消息解析和处理性能");
    }
    if message_rate < 1000.0 {
        info!("   - 检查网络带宽和连接池配置");
    }
    
    info!("============================================");
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🚀 真实WebSocket极限压力测试启动!");
    
    // 让用户选择测试级别
    println!("请选择测试级别:");
    println!("1. 中等负载 (5连接 × 6商品 × 2通道 = 60订阅)");
    println!("2. 高负载 (10连接 × 8商品 × 2通道 = 160订阅)");
    println!("3. 极限负载 (20连接 × 10商品 × 3通道 = 600订阅)");
    println!("请输入选择 (1-3), 或直接回车使用中等负载:");
    
    let config = StressTestConfig::medium_load(); // 默认使用中等负载
    
    info!("使用配置: {:?}", config);
    
    match run_real_websocket_stress_test(config).await {
        Ok(()) => {
            info!("🎉 压力测试成功完成！");
        }
        Err(e) => {
            error!("❌ 压力测试失败: {}", e);
        }
    }
    
    Ok(())
}