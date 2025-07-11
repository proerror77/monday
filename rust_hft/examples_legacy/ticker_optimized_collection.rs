/*!
 * Ticker数据优化收集系统
 * 
 * 使用Fast重连策略专门优化ticker数据收集：
 * - 100ms -> 500ms -> 1s -> 2s 重连延迟
 * - 5次重连尝试提高稳定性
 * - 专为DL/RL数据准备设计
 */

use rust_hft::integrations::bitget_connector::*;
use anyhow::Result;
use tracing::{info, warn, error};
use tokio::time::Duration;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    info!("🎯 启动Ticker数据优化收集系统");
    info!("📊 专为DL/RL数据准备设计，使用快速重连策略");
    
    // 创建专门为ticker数据优化的配置
    let config = BitgetConfig::for_ticker_data();
    info!("⚙️  配置详情:");
    info!("  - 重连策略: {:?}", config.reconnect_strategy);
    info!("  - 最大重连次数: {}", config.max_reconnect_attempts);
    info!("  - 自动重连: {}", config.auto_reconnect);
    
    // 创建连接器并订阅ticker数据
    let mut connector = BitgetConnector::new(config);
    
    // 订阅多个主流币种的ticker数据
    let symbols = vec!["BTCUSDT", "ETHUSDT", "SOLUSDT", "ADAUSDT", "BNBUSDT"];
    for symbol in &symbols {
        connector.add_subscription(symbol.to_string(), BitgetChannel::Ticker);
        info!("📈 订阅 {} ticker数据", symbol);
    }
    
    // 数据计数器
    let message_count = Arc::new(AtomicU64::new(0));
    let start_time = std::time::Instant::now();
    
    let count_clone = message_count.clone();
    let message_handler = move |msg: BitgetMessage| {
        let count = count_clone.fetch_add(1, Ordering::Relaxed) + 1;
        
        match msg {
            BitgetMessage::Ticker { symbol, data, timestamp } => {
                if count % 100 == 0 { // 每100条消息输出一次统计
                    let elapsed = start_time.elapsed();
                    let rate = count as f64 / elapsed.as_secs_f64();
                    info!("📊 已收集 {} 条ticker消息, 速率: {:.1} msg/s, 最新: {}", 
                          count, rate, symbol);
                }
                
                // 在这里可以添加数据处理逻辑
                // 例如：保存到数据库、实时特征提取等
                if let Some(data_array) = data.as_array() {
                    if let Some(ticker_data) = data_array.first() {
                        if let (Some(last_price), Some(volume)) = (
                            ticker_data.get("lastPr").and_then(|v| v.as_str()),
                            ticker_data.get("volume24h").and_then(|v| v.as_str())
                        ) {
                            if count <= 5 { // 输出前几条数据示例
                                info!("💰 {} - 价格: {}, 24h成交量: {}", symbol, last_price, volume);
                            }
                        }
                    }
                }
            }
            _ => {
                warn!("收到非ticker数据: {:?}", msg);
            }
        }
    };
    
    info!("🚀 开始连接Bitget WebSocket (使用快速重连策略)...");
    
    // 启动数据收集
    // 注意：这里会使用快速重连策略，如果连接失败会快速重试
    match connector.connect_public(message_handler).await {
        Ok(_) => {
            info!("✅ WebSocket连接正常结束");
        }
        Err(e) => {
            error!("❌ WebSocket连接失败: {}", e);
            error!("💡 建议检查网络连接或更换代理");
        }
    }
    
    let final_count = message_count.load(Ordering::Relaxed);
    let total_time = start_time.elapsed();
    let avg_rate = final_count as f64 / total_time.as_secs_f64();
    
    info!("📋 数据收集统计:");
    info!("  - 总消息数: {}", final_count);
    info!("  - 运行时间: {:.2}s", total_time.as_secs_f64());
    info!("  - 平均速率: {:.1} msg/s", avg_rate);
    
    Ok(())
}

/// 演示如何使用不同重连策略的配置
#[allow(dead_code)]
fn demo_different_strategies() {
    // Ticker数据优化配置（快速重连）
    let ticker_config = BitgetConfig::for_ticker_data();
    println!("Ticker配置 - 重连策略: {:?}", ticker_config.reconnect_strategy);
    
    // Orderbook数据标准配置
    let orderbook_config = BitgetConfig::for_orderbook_data();
    println!("Orderbook配置 - 重连策略: {:?}", orderbook_config.reconnect_strategy);
    
    // 自定义超快速配置（适用于极重要的数据流）
    let ultra_fast_config = BitgetConfig::with_custom_reconnect(50, 800, 1.8);
    println!("超快速配置 - 重连策略: {:?}", ultra_fast_config.reconnect_strategy);
    
    // 保守配置（适用于网络不稳定环境）
    let conservative_config = BitgetConfig::with_custom_reconnect(2000, 10000, 1.2);
    println!("保守配置 - 重连策略: {:?}", conservative_config.reconnect_strategy);
}