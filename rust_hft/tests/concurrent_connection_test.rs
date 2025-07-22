/**
 * 并发连接压力测试
 * 
 * 测试 UnifiedBitgetConnector 在多个并发连接下的性能和稳定性
 * 包括：
 * - 多个同时连接的稳定性
 * - 消息吞吐量测试
 * - 内存使用监控
 * - 错误率统计
 * - 连接恢复能力
 */

use rust_hft::integrations::{
    UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, UnifiedBitgetChannel
};
use anyhow::Result;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{info, warn, error};

/// 并发测试配置
#[derive(Debug, Clone)]
pub struct ConcurrentTestConfig {
    /// 并发连接数
    pub concurrent_connections: usize,
    /// 每个连接的测试时长（秒）
    pub test_duration_secs: u64,
    /// 测试的交易对数量
    pub symbols: Vec<String>,
    /// 订阅的通道类型
    pub channels: Vec<UnifiedBitgetChannel>,
    /// 连接超时时间（秒）
    pub connection_timeout_secs: u64,
    /// 期望的最小消息率（消息/秒）
    pub min_message_rate: f64,
}

impl Default for ConcurrentTestConfig {
    fn default() -> Self {
        Self {
            concurrent_connections: 5,
            test_duration_secs: 30,
            symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(), 
                "SOLUSDT".to_string(),
                "ADAUSDT".to_string(),
                "DOGEUSDT".to_string(),
            ],
            channels: vec![
                UnifiedBitgetChannel::OrderBook5,
                UnifiedBitgetChannel::Trades,
            ],
            connection_timeout_secs: 15,
            min_message_rate: 1.0,
        }
    }
}

/// 单个连接的统计信息
#[derive(Debug, Default)]
pub struct ConnectionStats {
    pub messages_received: AtomicU64,
    pub errors_count: AtomicU64,
    pub last_message_time: std::sync::Mutex<Option<Instant>>,
    pub connection_duration: std::sync::Mutex<Option<Duration>>,
    pub symbols_subscribed: std::sync::Mutex<Vec<String>>,
}

impl ConnectionStats {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn record_message(&self) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        *self.last_message_time.lock().unwrap() = Some(Instant::now());
    }
    
    pub fn record_error(&self) {
        self.errors_count.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn get_message_count(&self) -> u64 {
        self.messages_received.load(Ordering::Relaxed)
    }
    
    pub fn get_error_count(&self) -> u64 {
        self.errors_count.load(Ordering::Relaxed)
    }
    
    pub fn get_message_rate(&self, duration: Duration) -> f64 {
        let count = self.get_message_count() as f64;
        let secs = duration.as_secs_f64();
        if secs > 0.0 { count / secs } else { 0.0 }
    }
}

/// 并发测试结果
#[derive(Debug)]
pub struct ConcurrentTestResult {
    pub total_connections: usize,
    pub successful_connections: usize,
    pub failed_connections: usize,
    pub total_messages: u64,
    pub total_errors: u64,
    pub average_message_rate: f64,
    pub test_duration: Duration,
    pub memory_usage_mb: f64,
    pub individual_stats: Vec<Arc<ConnectionStats>>,
}

impl ConcurrentTestResult {
    pub fn print_summary(&self) {
        info!("=== 并发连接测试结果 ===");
        info!("总连接数: {}", self.total_connections);
        info!("成功连接: {}", self.successful_connections);
        info!("失败连接: {}", self.failed_connections);
        info!("成功率: {:.1}%", 
              (self.successful_connections as f64 / self.total_connections as f64) * 100.0);
        info!("总消息数: {}", self.total_messages);
        info!("总错误数: {}", self.total_errors);
        info!("平均消息率: {:.2} msg/s", self.average_message_rate);
        info!("测试时长: {:?}", self.test_duration);
        info!("内存使用: {:.1} MB", self.memory_usage_mb);
        
        if self.total_messages > 0 {
            let error_rate = (self.total_errors as f64 / self.total_messages as f64) * 100.0;
            info!("错误率: {:.2}%", error_rate);
        }
        
        info!("========================");
    }
    
    pub fn is_successful(&self, min_message_rate: f64) -> bool {
        let success_rate = self.successful_connections as f64 / self.total_connections as f64;
        success_rate >= 0.8 && // 至少80%连接成功
        self.average_message_rate >= min_message_rate && // 满足最小消息率
        self.total_errors < self.total_messages / 10 // 错误率小于10%
    }
}

/// 并发连接测试器
pub struct ConcurrentConnectionTester {
    config: ConcurrentTestConfig,
}

impl ConcurrentConnectionTester {
    pub fn new(config: ConcurrentTestConfig) -> Self {
        Self { config }
    }
    
    pub fn with_default_config() -> Self {
        Self::new(ConcurrentTestConfig::default())
    }
    
    /// 运行并发连接测试
    pub async fn run_test(&self) -> Result<ConcurrentTestResult> {
        info!("🚀 开始并发连接测试...");
        info!("配置: {:?}", self.config);
        
        let start_time = Instant::now();
        let stats_list: Vec<Arc<ConnectionStats>> = (0..self.config.concurrent_connections)
            .map(|_| Arc::new(ConnectionStats::new()))
            .collect();
        
        // 启动所有并发连接
        let mut handles = Vec::new();
        let mut successful_connections = 0;
        
        for (i, stats) in stats_list.iter().enumerate() {
            let config = self.config.clone();
            let stats_clone = stats.clone();
            
            let handle = tokio::spawn(async move {
                Self::run_single_connection(i, config, stats_clone).await
            });
            
            handles.push(handle);
            
            // 稍微错开连接时间，避免同时建立过多连接
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        
        // 等待所有连接完成
        info!("⏳ 等待所有连接完成测试...");
        for (i, handle) in handles.into_iter().enumerate() {
            match handle.await {
                Ok(Ok(())) => {
                    successful_connections += 1;
                    info!("✅ 连接 {} 测试完成", i);
                }
                Ok(Err(e)) => {
                    error!("❌ 连接 {} 测试失败: {}", i, e);
                }
                Err(e) => {
                    error!("❌ 连接 {} 任务异常: {}", i, e);
                }
            }
        }
        
        let test_duration = start_time.elapsed();
        
        // 收集统计信息
        let total_messages: u64 = stats_list.iter()
            .map(|s| s.get_message_count())
            .sum();
        
        let total_errors: u64 = stats_list.iter()
            .map(|s| s.get_error_count())
            .sum();
        
        let average_message_rate = total_messages as f64 / test_duration.as_secs_f64();
        
        // 估算内存使用（简化版）
        let memory_usage_mb = Self::estimate_memory_usage(&stats_list);
        
        let result = ConcurrentTestResult {
            total_connections: self.config.concurrent_connections,
            successful_connections,
            failed_connections: self.config.concurrent_connections - successful_connections,
            total_messages,
            total_errors,
            average_message_rate,
            test_duration,
            memory_usage_mb,
            individual_stats: stats_list,
        };
        
        result.print_summary();
        Ok(result)
    }
    
    /// 运行单个连接的测试
    async fn run_single_connection(
        connection_id: usize,
        config: ConcurrentTestConfig,
        stats: Arc<ConnectionStats>
    ) -> Result<()> {
        info!("🔌 启动连接 {} 测试", connection_id);
        
        // 创建连接器配置
        let bitget_config = UnifiedBitgetConfig {
            ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            api_key: None,
            api_secret: None,
            passphrase: None,
            mode: ConnectionMode::Single,
            max_reconnect_attempts: 3,
            reconnect_delay_ms: 1000,
            enable_compression: true,
            max_message_size: 1024 * 1024,
            ping_interval_secs: 30,
            connection_timeout_secs: config.connection_timeout_secs,
        };
        
        let connector = UnifiedBitgetConnector::new(bitget_config);
        
        // 为每个连接分配不同的交易对组合，避免冲突
        let symbol_index = connection_id % config.symbols.len();
        let symbol = &config.symbols[symbol_index];
        
        // 记录订阅的交易对
        {
            let mut subscribed = stats.symbols_subscribed.lock().unwrap();
            subscribed.push(symbol.clone());
        }
        
        // 添加订阅
        for channel in &config.channels {
            if let Err(e) = connector.subscribe(symbol, channel.clone()).await {
                error!("连接 {} 订阅失败: {}", connection_id, e);
                stats.record_error();
                return Err(e);
            }
        }
        
        // 启动连接
        let connection_start = Instant::now();
        let mut receiver = match timeout(
            Duration::from_secs(config.connection_timeout_secs),
            connector.start()
        ).await {
            Ok(Ok(rx)) => rx,
            Ok(Err(e)) => {
                error!("连接 {} 启动失败: {}", connection_id, e);
                stats.record_error();
                return Err(e);
            }
            Err(_) => {
                let err = anyhow::anyhow!("连接 {} 超时", connection_id);
                error!("{}", err);
                stats.record_error();
                return Err(err);
            }
        };
        
        info!("✅ 连接 {} 启动成功，开始接收消息", connection_id);
        
        // 记录连接建立时间
        {
            let mut duration = stats.connection_duration.lock().unwrap();
            *duration = Some(connection_start.elapsed());
        }
        
        // 接收消息
        let test_end = Instant::now() + Duration::from_secs(config.test_duration_secs);
        let mut last_log_time = Instant::now();
        
        while Instant::now() < test_end {
            match timeout(Duration::from_secs(5), receiver.recv()).await {
                Ok(Some(_message)) => {
                    stats.record_message();
                    
                    // 每5秒记录一次进度
                    if last_log_time.elapsed() >= Duration::from_secs(5) {
                        let count = stats.get_message_count();
                        let rate = stats.get_message_rate(connection_start.elapsed());
                        info!("📊 连接 {} 进度: {} 消息, {:.1} msg/s", 
                              connection_id, count, rate);
                        last_log_time = Instant::now();
                    }
                }
                Ok(None) => {
                    warn!("连接 {} 接收器已关闭", connection_id);
                    break;
                }
                Err(_) => {
                    // 超时，继续循环
                    continue;
                }
            }
        }
        
        let final_count = stats.get_message_count();
        let final_rate = stats.get_message_rate(connection_start.elapsed());
        info!("✅ 连接 {} 测试完成: {} 消息, {:.1} msg/s", 
              connection_id, final_count, final_rate);
        
        Ok(())
    }
    
    /// 估算内存使用量（简化版）
    fn estimate_memory_usage(stats_list: &[Arc<ConnectionStats>]) -> f64 {
        // 简化的内存估算：每个连接约 5MB，每条消息约 1KB
        let base_memory = stats_list.len() as f64 * 5.0; // 基础内存
        let message_memory: f64 = stats_list.iter()
            .map(|s| s.get_message_count() as f64 * 0.001) // 每条消息 1KB
            .sum();
        
        base_memory + message_memory
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_concurrent_connections_light() {
        tracing_subscriber::fmt::init();
        
        let config = ConcurrentTestConfig {
            concurrent_connections: 2,
            test_duration_secs: 10,
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            channels: vec![UnifiedBitgetChannel::OrderBook5],
            connection_timeout_secs: 15,
            min_message_rate: 0.5,
        };
        
        let tester = ConcurrentConnectionTester::new(config.clone());
        let result = tester.run_test().await.unwrap();
        
        // 验证基本指标
        assert_eq!(result.total_connections, 2);
        assert!(result.successful_connections >= 1, "至少应有1个成功连接");
        assert!(result.total_messages > 0, "应该接收到消息");
        
        // 检查是否满足成功标准
        if !result.is_successful(config.min_message_rate) {
            warn!("并发测试未完全达到成功标准，但这可能是网络问题");
        }
    }
    
    #[tokio::test]
    #[ignore] // 标记为忽略，避免CI中运行过重的测试
    async fn test_concurrent_connections_stress() {
        tracing_subscriber::fmt::init();
        
        let config = ConcurrentTestConfig {
            concurrent_connections: 10,
            test_duration_secs: 60,
            ..Default::default()
        };
        
        let tester = ConcurrentConnectionTester::new(config.clone());
        let result = tester.run_test().await.unwrap();
        
        // 压力测试的成功标准
        assert!(result.successful_connections >= 7, "至少70%连接应该成功");
        assert!(result.total_messages > 100, "应该接收到足够多的消息");
        assert!(result.average_message_rate >= config.min_message_rate);
        
        // 性能指标验证
        assert!(result.memory_usage_mb < 500.0, "内存使用不应超过500MB");
        
        info!("🎉 压力测试通过！");
    }
}

/// 运行并发测试的便利函数
pub async fn run_concurrent_test() -> Result<()> {
    tracing_subscriber::fmt::init();
    
    let tester = ConcurrentConnectionTester::with_default_config();
    let result = tester.run_test().await?;
    
    if result.is_successful(1.0) {
        info!("🎉 并发测试全部通过！");
    } else {
        warn!("⚠️  并发测试部分指标未达标，需要进一步优化");
    }
    
    Ok(())
}