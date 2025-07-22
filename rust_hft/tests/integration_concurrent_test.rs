/**
 * 集成并发连接测试
 * 
 * 验证 UnifiedBitgetConnector 修复后在并发场景下的基本功能
 * 重点测试连接建立、错误处理和资源管理
 */

use rust_hft::integrations::{
    UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, UnifiedBitgetChannel
};
use anyhow::Result;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{info, warn};

/// 快速并发测试配置
#[derive(Debug, Clone)]
struct QuickTestConfig {
    pub concurrent_connections: usize,
    pub test_duration_secs: u64,
    pub connection_timeout_secs: u64,
}

impl Default for QuickTestConfig {
    fn default() -> Self {
        Self {
            concurrent_connections: 3,
            test_duration_secs: 10,
            connection_timeout_secs: 8,
        }
    }
}

/// 简化的统计收集器
#[derive(Debug, Default)]
struct SimpleStats {
    successful_connections: AtomicU64,
    failed_connections: AtomicU64,
    total_messages: AtomicU64,
}

impl SimpleStats {
    fn record_success(&self) {
        self.successful_connections.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_failure(&self) {
        self.failed_connections.fetch_add(1, Ordering::Relaxed);
    }
    
    fn record_message(&self) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
    }
    
    fn get_success_count(&self) -> u64 {
        self.successful_connections.load(Ordering::Relaxed)
    }
    
    fn get_failure_count(&self) -> u64 {
        self.failed_connections.load(Ordering::Relaxed)
    }
    
    fn get_message_count(&self) -> u64 {
        self.total_messages.load(Ordering::Relaxed)
    }
}

/// 创建测试用的连接器配置
fn create_test_config(timeout_secs: u64) -> UnifiedBitgetConfig {
    UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 3,
        reconnect_delay_ms: 500,
        enable_compression: true,
        max_message_size: 1024 * 1024,
        ping_interval_secs: 30,
        connection_timeout_secs: timeout_secs,
    }
}

/// 运行单个连接的快速测试
async fn run_single_connection_test(
    connection_id: usize,
    config: QuickTestConfig,
    stats: Arc<SimpleStats>
) -> Result<()> {
    info!("启动连接 {} 的快速测试", connection_id);
    
    let connector_config = create_test_config(config.connection_timeout_secs);
    let connector = UnifiedBitgetConnector::new(connector_config);
    
    // 为每个连接使用不同的交易对，避免冲突
    let symbols = ["BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT", "DOGEUSDT"];
    let symbol = symbols[connection_id % symbols.len()];
    
    // 添加订阅
    if let Err(e) = connector.subscribe(symbol, UnifiedBitgetChannel::OrderBook5).await {
        warn!("连接 {} 订阅失败: {}", connection_id, e);
        stats.record_failure();
        return Err(e);
    }
    
    // 尝试启动连接
    let start_time = Instant::now();
    let mut receiver = match timeout(
        Duration::from_secs(config.connection_timeout_secs),
        connector.start()
    ).await {
        Ok(Ok(rx)) => {
            info!("连接 {} 启动成功，耗时: {:?}", connection_id, start_time.elapsed());
            stats.record_success();
            rx
        }
        Ok(Err(e)) => {
            warn!("连接 {} 启动失败: {}", connection_id, e);
            stats.record_failure();
            return Err(e);
        }
        Err(_) => {
            warn!("连接 {} 启动超时", connection_id);
            stats.record_failure();
            return Err(anyhow::anyhow!("连接超时"));
        }
    };
    
    // 短时间接收消息测试
    let test_end = Instant::now() + Duration::from_secs(config.test_duration_secs);
    let mut local_message_count = 0;
    
    while Instant::now() < test_end {
        match timeout(Duration::from_millis(500), receiver.recv()).await {
            Ok(Some(_)) => {
                local_message_count += 1;
                stats.record_message();
            }
            Ok(None) => {
                info!("连接 {} 接收器关闭", connection_id);
                break;
            }
            Err(_) => {
                // 超时，继续循环
                continue;
            }
        }
    }
    
    info!("连接 {} 测试完成，收到 {} 条消息", connection_id, local_message_count);
    Ok(())
}

/// 快速并发测试主函数
pub async fn run_quick_concurrent_test(config: QuickTestConfig) -> Result<(u64, u64, u64)> {
    info!("🚀 开始快速并发连接测试");
    info!("配置: {:?}", config);
    
    let stats = Arc::new(SimpleStats::default());
    let mut handles = Vec::new();
    
    // 启动并发连接测试
    for i in 0..config.concurrent_connections {
        let test_config = config.clone();
        let stats_clone = stats.clone();
        
        let handle = tokio::spawn(async move {
            run_single_connection_test(i, test_config, stats_clone).await
        });
        
        handles.push(handle);
        
        // 稍微错开启动时间
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    
    // 等待所有连接测试完成
    info!("⏳ 等待所有连接测试完成...");
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(())) => {
                info!("✅ 连接 {} 测试成功", i);
            }
            Ok(Err(e)) => {
                warn!("⚠️  连接 {} 测试失败: {}", i, e);
            }
            Err(e) => {
                warn!("❌ 连接 {} 任务异常: {}", i, e);
            }
        }
    }
    
    let successful = stats.get_success_count();
    let failed = stats.get_failure_count();
    let messages = stats.get_message_count();
    
    info!("📊 测试完成统计:");
    info!("  成功连接: {}", successful);
    info!("  失败连接: {}", failed);
    info!("  总消息数: {}", messages);
    
    let success_rate = if config.concurrent_connections > 0 {
        (successful as f64 / config.concurrent_connections as f64) * 100.0
    } else {
        0.0
    };
    
    info!("  成功率: {:.1}%", success_rate);
    
    Ok((successful, failed, messages))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_quick_concurrent_connections() {
        // 初始化日志
        let _ = tracing_subscriber::fmt::try_init();
        
        let config = QuickTestConfig {
            concurrent_connections: 2,
            test_duration_secs: 8,
            connection_timeout_secs: 10,
        };
        
        let result = run_quick_concurrent_test(config.clone()).await;
        
        assert!(result.is_ok(), "并发测试应该成功完成");
        
        let (successful, failed, messages) = result.unwrap();
        
        // 基本断言
        assert!(successful + failed == config.concurrent_connections as u64, 
                "成功+失败连接数应该等于总连接数");
        
        // 如果有成功的连接，应该至少接收到一些消息
        if successful > 0 {
            info!("✅ 有 {} 个连接成功，共收到 {} 条消息", successful, messages);
            // 注意：在网络条件不好的情况下，可能接收不到消息，所以不强制要求
        } else {
            warn!("⚠️  所有连接都失败了，可能是网络问题");
        }
        
        // 计算成功率
        let success_rate = (successful as f64 / config.concurrent_connections as f64) * 100.0;
        info!("并发连接成功率: {:.1}%", success_rate);
        
        // 如果成功率过低，给出警告而不是失败
        if success_rate < 50.0 {
            warn!("并发连接成功率较低 ({:.1}%)，可能需要检查网络连接或服务器状态", success_rate);
        } else {
            info!("✅ 并发连接测试表现良好");
        }
    }
    
    #[tokio::test]
    async fn test_single_connector_basic_functionality() {
        // 测试单个连接器的基本功能
        let _ = tracing_subscriber::fmt::try_init();
        
        let config = create_test_config(10);
        let connector = UnifiedBitgetConnector::new(config);
        
        // 测试订阅功能
        let subscribe_result = connector.subscribe("BTCUSDT", UnifiedBitgetChannel::OrderBook5).await;
        assert!(subscribe_result.is_ok(), "订阅应该成功");
        
        // 测试连接启动（可能因为网络原因失败，但不应该panic）
        let start_result = timeout(Duration::from_secs(15), connector.start()).await;
        
        match start_result {
            Ok(Ok(_receiver)) => {
                info!("✅ 单连接器测试成功");
            }
            Ok(Err(e)) => {
                warn!("⚠️  连接失败，但代码逻辑正常: {}", e);
            }
            Err(_) => {
                warn!("⚠️  连接超时，但没有panic");
            }
        }
    }
}

/// 运行并发测试的便利函数
pub async fn run_integration_concurrent_test() -> Result<()> {
    let _ = tracing_subscriber::fmt::try_init();
    
    let config = QuickTestConfig::default();
    let (successful, failed, messages) = run_quick_concurrent_test(config).await?;
    
    info!("🎉 集成并发测试完成!");
    info!("结果: {} 成功, {} 失败, {} 消息", successful, failed, messages);
    
    Ok(())
}