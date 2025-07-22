/*!
 * 快速WebSocket性能验证
 * 
 * 快速验证当前UnifiedBitgetConnector的性能表现
 * 用于验证多商品并发连接的稳定性和吞吐量
 */

use rust_hft::integrations::{
    UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, UnifiedBitgetChannel
};
use anyhow::Result;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{info, warn};

/// 快速性能检查结果
#[derive(Debug)]
struct PerformanceCheckResult {
    connections_attempted: usize,
    connections_successful: usize,
    total_messages: u64,
    test_duration: Duration,
    message_rate: f64,
    connection_success_rate: f64,
}

impl PerformanceCheckResult {
    fn print_summary(&self) {
        info!("🎯 === 快速性能检查结果 ===");
        info!("连接测试: {}/{} 成功 ({:.1}%)", 
              self.connections_successful, self.connections_attempted, self.connection_success_rate);
        info!("消息吞吐: {} 消息, {:.1} msg/s", 
              self.total_messages, self.message_rate);
        info!("测试时长: {:?}", self.test_duration);
        
        // 性能等级评定
        let grade = if self.message_rate >= 80.0 && self.connection_success_rate >= 95.0 {
            "🏆 优秀"
        } else if self.message_rate >= 50.0 && self.connection_success_rate >= 90.0 {
            "⭐ 良好"
        } else if self.message_rate >= 30.0 && self.connection_success_rate >= 80.0 {
            "✅ 及格"
        } else {
            "⚠️  需优化"
        };
        
        info!("性能等级: {}", grade);
        info!("============================");
    }
}

/// 运行单个连接的快速性能检查
async fn run_single_connection_check(
    connection_id: usize,
    symbols: Vec<String>,
    channels: Vec<UnifiedBitgetChannel>,
    test_duration_secs: u64,
    message_counter: Arc<AtomicU64>
) -> Result<bool> {
    let config = UnifiedBitgetConfig {
        ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        api_key: None,
        api_secret: None,
        passphrase: None,
        mode: ConnectionMode::Single,
        max_reconnect_attempts: 2,
        reconnect_delay_ms: 500,
        enable_compression: true,
        max_message_size: 1024 * 1024,
        ping_interval_secs: 30,
        connection_timeout_secs: 10,
    };
    
    let connector = UnifiedBitgetConnector::new(config);
    
    // 添加订阅
    for symbol in &symbols {
        for channel in &channels {
            connector.subscribe(symbol, channel.clone()).await?;
        }
    }
    
    info!("连接 {} 添加了 {} 个订阅", connection_id, symbols.len() * channels.len());
    
    // 启动连接
    let mut receiver = match timeout(Duration::from_secs(10), connector.start()).await {
        Ok(Ok(rx)) => {
            info!("✅ 连接 {} 启动成功", connection_id);
            rx
        }
        Ok(Err(e)) => {
            warn!("❌ 连接 {} 启动失败: {}", connection_id, e);
            return Ok(false);
        }
        Err(_) => {
            warn!("⏰ 连接 {} 启动超时", connection_id);
            return Ok(false);
        }
    };
    
    // 接收消息
    let start_time = Instant::now();
    let mut local_count = 0;
    
    while start_time.elapsed().as_secs() < test_duration_secs {
        match timeout(Duration::from_millis(500), receiver.recv()).await {
            Ok(Some(_message)) => {
                local_count += 1;
                message_counter.fetch_add(1, Ordering::Relaxed);
                
                // 每500条消息报告一次
                if local_count % 500 == 0 {
                    let rate = local_count as f64 / start_time.elapsed().as_secs_f64();
                    info!("🚀 连接 {} 接收: {} 消息 ({:.1} msg/s)", connection_id, local_count, rate);
                }
            }
            Ok(None) => {
                warn!("连接 {} 接收器关闭", connection_id);
                break;
            }
            Err(_) => {
                // 超时，继续循环
                continue;
            }
        }
    }
    
    let final_rate = local_count as f64 / start_time.elapsed().as_secs_f64();
    info!("🏁 连接 {} 完成: {} 消息, {:.1} msg/s", connection_id, local_count, final_rate);
    
    Ok(true)
}

/// 快速性能检查主函数
pub async fn quick_performance_check() -> Result<PerformanceCheckResult> {
    info!("🚀 开始快速WebSocket性能检查");
    
    // 测试配置
    let test_connections = 3;
    let test_duration_secs = 20;
    let symbols_per_connection = 4;
    let channels = vec![UnifiedBitgetChannel::OrderBook5, UnifiedBitgetChannel::Trades];
    
    // 热门交易对
    let all_symbols = vec![
        "BTCUSDT", "ETHUSDT", "BNBUSDT", "XRPUSDT", 
        "ADAUSDT", "SOLUSDT", "DOGEUSDT", "MATICUSDT",
        "LINKUSDT", "UNIUSDT", "LTCUSDT", "BCHUSDT"
    ];
    
    let message_counter = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::new();
    let mut successful_connections = 0;
    
    let overall_start = Instant::now();
    
    // 启动并发连接
    for i in 0..test_connections {
        let start_idx = i * symbols_per_connection;
        let symbols: Vec<String> = (0..symbols_per_connection)
            .map(|j| all_symbols[(start_idx + j) % all_symbols.len()].to_string())
            .collect();
        
        let channels_clone = channels.clone();
        let counter_clone = message_counter.clone();
        
        let handle = tokio::spawn(async move {
            run_single_connection_check(i, symbols, channels_clone, test_duration_secs, counter_clone).await
        });
        
        handles.push(handle);
        
        // 错开启动时间
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    
    info!("⏳ 等待 {} 秒性能测试完成...", test_duration_secs);
    
    // 等待所有连接完成
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok(true)) => {
                successful_connections += 1;
                info!("✅ 连接 {} 测试成功", i);
            }
            Ok(Ok(false)) => {
                warn!("⚠️  连接 {} 测试失败", i);
            }
            Ok(Err(e)) => {
                warn!("❌ 连接 {} 发生错误: {}", i, e);
            }
            Err(e) => {
                warn!("💥 连接 {} 任务异常: {}", i, e);
            }
        }
    }
    
    let test_duration = overall_start.elapsed();
    let total_messages = message_counter.load(Ordering::Relaxed);
    let message_rate = total_messages as f64 / test_duration.as_secs_f64();
    let connection_success_rate = (successful_connections as f64 / test_connections as f64) * 100.0;
    
    Ok(PerformanceCheckResult {
        connections_attempted: test_connections,
        connections_successful: successful_connections,
        total_messages,
        test_duration,
        message_rate,
        connection_success_rate,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🎯 UnifiedBitgetConnector 快速性能验证");
    
    match quick_performance_check().await {
        Ok(result) => {
            result.print_summary();
            
            // 基于结果给出建议
            if result.connection_success_rate >= 95.0 && result.message_rate >= 60.0 {
                info!("🎉 性能验证通过！连接器工作优秀");
            } else if result.connection_success_rate >= 80.0 && result.message_rate >= 30.0 {
                info!("✅ 性能验证基本通过，可进行更大规模测试");
            } else {
                warn!("⚠️  性能验证未达预期，建议检查网络连接或优化配置");
            }
        }
        Err(e) => {
            warn!("❌ 性能检查失败: {}", e);
        }
    }
    
    Ok(())
}