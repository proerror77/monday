/*!
 * UnifiedBitgetConnector 并发性能基准测试
 * 
 * 专门测试修复后的 UnifiedBitgetConnector 在并发场景下的性能表现
 * 重点关注连接建立时间、消息吞吐量和资源使用情况
 */

use criterion::{black_box, criterion_group, criterion_main, Criterion, BatchSize, BenchmarkId};
use rust_hft::integrations::{
    UnifiedBitgetConnector, UnifiedBitgetConfig, ConnectionMode, UnifiedBitgetChannel
};
use tokio::runtime::Runtime;
use std::time::Duration;
use tracing::info;

/// 基准测试配置
struct BenchmarkConfig {
    concurrent_count: usize,
    test_duration_ms: u64,
    symbol: String,
    channel: UnifiedBitgetChannel,
}

impl BenchmarkConfig {
    fn new(concurrent_count: usize) -> Self {
        Self {
            concurrent_count,
            test_duration_ms: 5000, // 5秒快速测试
            symbol: "BTCUSDT".to_string(),
            channel: UnifiedBitgetChannel::OrderBook5,
        }
    }
}

/// 创建统一的连接器配置
fn create_connector_config() -> UnifiedBitgetConfig {
    UnifiedBitgetConfig {
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
    }
}

/// 测试单个连接器的连接建立时间
async fn benchmark_connection_establishment() -> Result<Duration, anyhow::Error> {
    let config = create_connector_config();
    let connector = UnifiedBitgetConnector::new(config);
    
    // 添加订阅
    connector.subscribe("BTCUSDT", UnifiedBitgetChannel::OrderBook5).await?;
    
    let start = std::time::Instant::now();
    
    // 启动连接
    let _receiver = connector.start().await?;
    
    let connection_time = start.elapsed();
    Ok(connection_time)
}

/// 测试并发连接建立
async fn benchmark_concurrent_connections(config: BenchmarkConfig) -> Result<Vec<Duration>, anyhow::Error> {
    let mut handles = Vec::new();
    let mut connection_times = Vec::new();
    
    for i in 0..config.concurrent_count {
        let symbol = format!("{}_{}", config.symbol, i % 5); // 分散到不同symbol
        let channel = config.channel.clone();
        
        let handle = tokio::spawn(async move {
            let connector_config = create_connector_config();
            let connector = UnifiedBitgetConnector::new(connector_config);
            
            // 添加订阅
            connector.subscribe(&symbol, channel).await?;
            
            let start = std::time::Instant::now();
            
            // 启动连接
            let _receiver = connector.start().await?;
            
            let connection_time = start.elapsed();
            Ok::<Duration, anyhow::Error>(connection_time)
        });
        
        handles.push(handle);
        
        // 稍微错开启动时间
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    
    // 等待所有连接完成
    for handle in handles {
        match handle.await {
            Ok(Ok(duration)) => connection_times.push(duration),
            Ok(Err(e)) => {
                eprintln!("连接失败: {}", e);
            }
            Err(e) => {
                eprintln!("任务失败: {}", e);
            }
        }
    }
    
    Ok(connection_times)
}

/// 测试消息接收性能
async fn benchmark_message_throughput(concurrent_count: usize) -> Result<(u64, Duration), anyhow::Error> {
    let config = create_connector_config();
    let connector = UnifiedBitgetConnector::new(config);
    
    // 添加订阅
    connector.subscribe("BTCUSDT", UnifiedBitgetChannel::OrderBook5).await?;
    connector.subscribe("BTCUSDT", UnifiedBitgetChannel::Trades).await?;
    
    // 启动连接
    let mut receiver = connector.start().await?;
    
    let start = std::time::Instant::now();
    let test_duration = Duration::from_millis(3000); // 3秒测试
    let mut message_count = 0u64;
    
    // 接收消息直到超时
    while start.elapsed() < test_duration {
        match tokio::time::timeout(Duration::from_millis(100), receiver.recv()).await {
            Ok(Some(_)) => {
                message_count += 1;
            }
            Ok(None) => break,
            Err(_) => continue, // 超时，继续
        }
    }
    
    let actual_duration = start.elapsed();
    Ok((message_count, actual_duration))
}

/// Criterion 基准测试函数
fn bench_connection_establishment(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    c.bench_function("single_connection_establishment", |b| {
        b.to_async(&rt).iter_batched(
            || (), // 没有 setup
            |_| async {
                black_box(benchmark_connection_establishment().await)
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_concurrent_connections(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    let concurrent_counts = vec![2, 3, 5];
    
    for count in concurrent_counts {
        c.bench_with_input(
            BenchmarkId::new("concurrent_connections", count),
            &count,
            |b, &count| {
                b.to_async(&rt).iter_batched(
                    || BenchmarkConfig::new(count),
                    |config| async move {
                        black_box(benchmark_concurrent_connections(config).await)
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
}

fn bench_message_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    
    c.bench_function("message_throughput", |b| {
        b.to_async(&rt).iter_batched(
            || 1,
            |count| async move {
                black_box(benchmark_message_throughput(count).await)
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    benches,
    bench_connection_establishment,
    bench_concurrent_connections,
    bench_message_throughput
);
criterion_main!(benches);