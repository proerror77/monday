/*!
 * 并发连接测试演示
 * 
 * 展示如何使用并发连接测试器评估 UnifiedBitgetConnector 
 * 在多连接场景下的性能和稳定性
 */

use rust_hft::integrations::UnifiedBitgetChannel;
use anyhow::Result;
use tracing::info;

// 引入测试模块（在实际项目中这些会在 tests/ 目录下）
mod concurrent_connection_test {
    include!("../tests/concurrent_connection_test.rs");
}

use concurrent_connection_test::{ConcurrentConnectionTester, ConcurrentTestConfig};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    info!("🚀 UnifiedBitgetConnector 并发连接测试演示");
    
    // 1. 轻量级快速测试
    info!("📋 执行轻量级并发测试...");
    run_light_test().await?;
    
    // 2. 中等规模测试
    info!("📋 执行中等规模并发测试...");
    run_medium_test().await?;
    
    // 3. 自定义配置测试
    info!("📋 执行自定义配置测试...");
    run_custom_test().await?;
    
    info!("🎉 所有并发测试演示完成！");
    Ok(())
}

/// 轻量级测试 - 适合快速验证
async fn run_light_test() -> Result<()> {
    let config = ConcurrentTestConfig {
        concurrent_connections: 3,
        test_duration_secs: 15,
        symbols: vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "SOLUSDT".to_string(),
        ],
        channels: vec![UnifiedBitgetChannel::OrderBook5],
        connection_timeout_secs: 10,
        min_message_rate: 0.5,
    };
    
    let tester = ConcurrentConnectionTester::new(config.clone());
    let result = tester.run_test().await?;
    
    info!("轻量级测试结果:");
    if result.is_successful(config.min_message_rate) {
        info!("✅ 轻量级测试通过");
    } else {
        info!("⚠️  轻量级测试部分指标未达标");
    }
    
    Ok(())
}

/// 中等规模测试 - 更接近实际使用场景
async fn run_medium_test() -> Result<()> {
    let config = ConcurrentTestConfig {
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
    };
    
    let tester = ConcurrentConnectionTester::new(config.clone());
    let result = tester.run_test().await?;
    
    info!("中等规模测试结果:");
    analyze_detailed_results(&result);
    
    Ok(())
}

/// 自定义配置测试 - 展示高级配置选项
async fn run_custom_test() -> Result<()> {
    let config = ConcurrentTestConfig {
        concurrent_connections: 8,
        test_duration_secs: 20,
        symbols: vec![
            "BTCUSDT".to_string(),
            "ETHUSDT".to_string(),
            "BNBUSDT".to_string(),
            "XRPUSDT".to_string(),
            "ADAUSDT".to_string(),
            "SOLUSDT".to_string(),
            "DOGEUSDT".to_string(),
            "MATICUSDT".to_string(),
        ],
        channels: vec![
            UnifiedBitgetChannel::OrderBook5,
            UnifiedBitgetChannel::Trades,
            UnifiedBitgetChannel::Ticker,
        ],
        connection_timeout_secs: 20,
        min_message_rate: 1.5,
    };
    
    info!("自定义测试配置:");
    info!("  - 并发连接数: {}", config.concurrent_connections);
    info!("  - 测试时长: {}秒", config.test_duration_secs);
    info!("  - 交易对数量: {}", config.symbols.len());
    info!("  - 订阅通道数: {}", config.channels.len());
    
    let tester = ConcurrentConnectionTester::new(config.clone());
    let result = tester.run_test().await?;
    
    info!("自定义测试结果:");
    analyze_detailed_results(&result);
    
    // 生成性能报告
    generate_performance_report(&result, &config);
    
    Ok(())
}

/// 详细分析测试结果
fn analyze_detailed_results(result: &concurrent_connection_test::ConcurrentTestResult) {
    info!("📊 详细结果分析:");
    
    // 连接成功率分析
    let success_rate = (result.successful_connections as f64 / result.total_connections as f64) * 100.0;
    info!("  连接成功率: {:.1}%", success_rate);
    
    if success_rate >= 90.0 {
        info!("  ✅ 连接稳定性优秀");
    } else if success_rate >= 80.0 {
        info!("  ⚠️  连接稳定性良好");
    } else {
        info!("  ❌ 连接稳定性需要改进");
    }
    
    // 消息吞吐量分析
    info!("  平均消息率: {:.2} msg/s", result.average_message_rate);
    if result.average_message_rate >= 5.0 {
        info!("  ✅ 消息吞吐量优秀");
    } else if result.average_message_rate >= 2.0 {
        info!("  ⚠️  消息吞吐量良好");
    } else {
        info!("  ❌ 消息吞吐量需要优化");
    }
    
    // 错误率分析
    if result.total_messages > 0 {
        let error_rate = (result.total_errors as f64 / result.total_messages as f64) * 100.0;
        info!("  错误率: {:.2}%", error_rate);
        
        if error_rate <= 1.0 {
            info!("  ✅ 错误率极低");
        } else if error_rate <= 5.0 {
            info!("  ⚠️  错误率可接受");
        } else {
            info!("  ❌ 错误率偏高");
        }
    }
    
    // 内存使用分析
    info!("  内存使用: {:.1} MB", result.memory_usage_mb);
    if result.memory_usage_mb <= 100.0 {
        info!("  ✅ 内存使用高效");
    } else if result.memory_usage_mb <= 300.0 {
        info!("  ⚠️  内存使用合理");
    } else {
        info!("  ❌ 内存使用较高");
    }
}

/// 生成性能报告
fn generate_performance_report(
    result: &concurrent_connection_test::ConcurrentTestResult,
    config: &ConcurrentTestConfig
) {
    info!("📋 === 性能报告 ===");
    
    // 计算关键性能指标
    let messages_per_connection = result.total_messages as f64 / result.successful_connections as f64;
    let test_efficiency = (result.successful_connections as f64 / result.total_connections as f64) * 
                         (result.average_message_rate / config.min_message_rate);
    
    info!("关键性能指标:");
    info!("  每连接平均消息数: {:.1}", messages_per_connection);
    info!("  测试效率评分: {:.2}", test_efficiency);
    info!("  总体测试时长: {:?}", result.test_duration);
    
    // 性能等级评定
    let performance_grade = if test_efficiency >= 2.0 && result.average_message_rate >= 5.0 {
        "A (优秀)"
    } else if test_efficiency >= 1.5 && result.average_message_rate >= 3.0 {
        "B (良好)"
    } else if test_efficiency >= 1.0 && result.average_message_rate >= 1.0 {
        "C (及格)"
    } else {
        "D (需改进)"
    };
    
    info!("性能等级: {}", performance_grade);
    
    // 建议
    info!("优化建议:");
    if result.failed_connections > 0 {
        info!("  - 增加连接重试机制");
        info!("  - 优化连接超时设置");
    }
    
    if result.average_message_rate < config.min_message_rate * 2.0 {
        info!("  - 检查网络延迟");
        info!("  - 优化消息处理性能");
    }
    
    if result.memory_usage_mb > 200.0 {
        info!("  - 考虑内存池优化");
        info!("  - 减少消息缓存");
    }
    
    info!("==================");
}