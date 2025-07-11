/*!
 * 测试快速重连策略
 * 
 * 验证针对ticker数据优化的重连机制：
 * - Fast模式：100ms -> 500ms -> 1s -> 2s
 * - Standard模式：1s -> 3s -> 5s  
 * - Custom模式：自定义参数
 */

use rust_hft::integrations::bitget_connector::*;
use anyhow::Result;
use tracing::{info, warn, error};
use tokio::time::{Duration, Instant};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    info!("🚀 测试快速重连策略");
    
    // 测试Fast策略 (适用于ticker数据)
    test_reconnect_strategy("Fast Strategy (Ticker优化)", ReconnectStrategy::Fast).await?;
    
    // 测试Standard策略
    test_reconnect_strategy("Standard Strategy", ReconnectStrategy::Standard).await?;
    
    // 测试Custom策略
    test_reconnect_strategy(
        "Custom Strategy (超快速)", 
        ReconnectStrategy::Custom {
            initial_delay_ms: 50,
            max_delay_ms: 1000,
            multiplier: 1.5,
        }
    ).await?;
    
    info!("✅ 所有重连策略测试完成");
    Ok(())
}

async fn test_reconnect_strategy(name: &str, strategy: ReconnectStrategy) -> Result<()> {
    info!("\n📊 测试策略: {}", name);
    info!("策略配置: {:?}", strategy);
    
    // 创建配置
    let config = BitgetConfig {
        public_ws_url: "wss://invalid-url-for-testing.com".to_string(), // 故意使用无效URL
        private_ws_url: "wss://invalid-url-for-testing.com".to_string(),
        timeout_seconds: 3, // 短超时时间让错误发生更快
        auto_reconnect: true,
        max_reconnect_attempts: 4, // 测试4次重连
        reconnect_strategy: strategy,
    };
    
    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Ticker);
    
    // 记录重连时间
    let reconnect_count = Arc::new(AtomicUsize::new(0));
    let test_start = Instant::now();
    
    let count_clone = reconnect_count.clone();
    let message_handler = move |_msg: BitgetMessage| {
        // 不会被调用，因为连接会失败
    };
    
    // 测试连接（预期会失败并重连）
    match connector.connect_public(message_handler).await {
        Ok(_) => warn!("连接意外成功"),
        Err(e) => {
            let total_time = test_start.elapsed();
            info!("⏱️  总重连时间: {:?}", total_time);
            info!("❌ 预期连接失败: {}", e);
        }
    }
    
    // 等待一下，让日志输出完整
    tokio::time::sleep(Duration::from_millis(500)).await;
    
    Ok(())
}

/// 测试不同配置的构造函数
#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_constructors() {
        // 测试ticker数据配置
        let ticker_config = BitgetConfig::for_ticker_data();
        assert_eq!(ticker_config.max_reconnect_attempts, 5);
        match ticker_config.reconnect_strategy {
            ReconnectStrategy::Fast => (),
            _ => panic!("Expected Fast strategy for ticker data"),
        }
        
        // 测试orderbook数据配置
        let orderbook_config = BitgetConfig::for_orderbook_data();
        assert_eq!(orderbook_config.max_reconnect_attempts, 3);
        match orderbook_config.reconnect_strategy {
            ReconnectStrategy::Standard => (),
            _ => panic!("Expected Standard strategy for orderbook data"),
        }
        
        // 测试自定义配置
        let custom_config = BitgetConfig::with_custom_reconnect(100, 2000, 2.0);
        match custom_config.reconnect_strategy {
            ReconnectStrategy::Custom { initial_delay_ms, max_delay_ms, multiplier } => {
                assert_eq!(initial_delay_ms, 100);
                assert_eq!(max_delay_ms, 2000);
                assert_eq!(multiplier, 2.0);
            },
            _ => panic!("Expected Custom strategy"),
        }
    }
    
    #[test]
    fn test_delay_calculation() {
        let config = BitgetConfig::for_ticker_data();
        let connector = BitgetConnector::new(config);
        
        // 访问内部方法测试延迟计算（这需要将方法改为public用于测试）
        // 在实际实现中可能需要添加测试方法
    }
}