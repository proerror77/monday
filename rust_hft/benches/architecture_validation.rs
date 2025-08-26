//! 整體架構效果驗證基準測試
//!
//! 驗證 ArcSwap + simd-json + Redis Streams 的完整性能表現

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rust_hft::exchanges::{
    ExchangeInstance, ExchangeInstanceConfig, ExchangeManager, ExchangeSnapshot,
};
use rust_hft::integrations::redis_streams_controller::{ControlEvent, RedisStreamsController};
use rust_hft::utils::fast_json::{fast_parse_str, BitgetMessageParser};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::runtime::Runtime;

/// 驗證 ArcSwap 兩層快照架構的性能
fn benchmark_arcswap_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("ArcSwap兩層快照");

    // 創建測試數據
    let manager = create_test_exchange_manager();
    let symbols = vec!["BTCUSDT", "ETHUSDT", "ADAUSDT", "BNBUSDT", "SOLUSDT"];
    let exchanges = vec!["bitget", "binance", "bybit"];

    // 測試無鎖實例選擇性能
    group.bench_function("無鎖實例選擇", |b| {
        b.iter(|| {
            for exchange in &exchanges {
                for symbol in &symbols {
                    let instance = manager.select_instance(exchange, symbol);
                    black_box(instance);
                }
            }
        })
    });

    // 測試併發讀取性能
    group.bench_function("併發讀取_4線程", |b| {
        b.to_async(Runtime::new().unwrap()).iter(|| async {
            let tasks: Vec<_> = (0..4)
                .map(|_| {
                    let manager = &manager;
                    tokio::spawn(async move {
                        for _ in 0..1000 {
                            for exchange in &exchanges {
                                for symbol in &symbols {
                                    let instance = manager.select_instance(exchange, symbol);
                                    black_box(instance);
                                }
                            }
                        }
                    })
                })
                .collect();

            for task in tasks {
                task.await.unwrap();
            }
        })
    });

    // 測試快照發布性能
    group.bench_function("快照發布", |b| {
        b.iter(|| {
            let new_snapshot = create_test_snapshots();
            manager.publish_exchanges(new_snapshot);
        })
    });

    group.finish();
}

/// 驗證 simd-json 解析性能對比
fn benchmark_json_parsing_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("JSON解析性能對比");

    // 測試數據
    let orderbook_json = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT","channel":"books15"},"data":[{"asks":[["67189.5","0.25",1],["67190.0","0.15",2],["67191.0","0.30",1]],"bids":[["67188.1","0.12",1],["67187.5","0.25",3],["67186.0","0.18",2]],"ts":"1721810005123"}]}"#;

    let trade_json = r#"{"arg":{"instId":"BTCUSDT","channel":"trade"},"data":[{"px":"67188.4","sz":"0.02","side":"buy","ts":"1721810005345","tradeId":"1234567890"}]}"#;

    // simd-json 解析性能
    group.bench_function("simd-json_訂單簿", |b| {
        let mut parser = BitgetMessageParser::new();
        b.iter(|| {
            let result = parser.parse_orderbook_message(orderbook_json);
            black_box(result);
        })
    });

    group.bench_function("simd-json_交易", |b| {
        let mut parser = BitgetMessageParser::new();
        b.iter(|| {
            let result = parser.parse_trade_message(trade_json);
            black_box(result);
        })
    });

    // 傳統 serde_json 對比
    group.bench_function("serde_json_訂單簿", |b| {
        b.iter(|| {
            let result: Result<serde_json::Value, _> = serde_json::from_str(orderbook_json);
            black_box(result);
        })
    });

    group.bench_function("serde_json_交易", |b| {
        b.iter(|| {
            let result: Result<serde_json::Value, _> = serde_json::from_str(trade_json);
            black_box(result);
        })
    });

    // fast_parse_str 性能
    group.bench_function("fast_parse_str", |b| {
        b.iter(|| {
            let result = fast_parse_str(orderbook_json);
            black_box(result);
        })
    });

    group.finish();
}

/// 驗證 Redis Streams 控制面性能
fn benchmark_redis_streams_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("Redis_Streams控制面");

    let rt = Runtime::new().unwrap();

    // 創建 Redis Streams 控制器
    let controller = rt.block_on(async {
        RedisStreamsController::new(
            "redis://localhost:6379",
            "benchmark-group".to_string(),
            "benchmark-consumer".to_string(),
        )
        .await
        .expect("Failed to create Redis controller")
    });

    // 測試事件發布性能
    group.bench_function("事件發布", |b| {
        b.to_async(&rt).iter(|| async {
            let result = controller
                .publish_ops_alert("latency".to_string(), 35.2, Some(25.0))
                .await;
            black_box(result);
        })
    });

    // 測試批量事件發布
    group.bench_function("批量事件發布_100", |b| {
        b.to_async(&rt).iter(|| async {
            for i in 0..100 {
                let result = controller
                    .publish_ops_alert("latency".to_string(), 25.0 + (i as f64) * 0.1, Some(25.0))
                    .await;
                black_box(result);
            }
        })
    });

    group.finish();
}

/// 驗證端到端延遲性能
fn benchmark_end_to_end_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("端到端延遲");

    let rt = Runtime::new().unwrap();
    let manager = create_test_exchange_manager();

    // 模擬完整的消息處理流程
    group.bench_function("完整消息處理流程", |b| {
        let mut parser = BitgetMessageParser::new();
        let json_message = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT","channel":"books15"},"data":[{"asks":[["67189.5","0.25",1]],"bids":[["67188.1","0.12",1]],"ts":"1721810005123"}]}"#;
        
        b.iter(|| {
            let start = Instant::now();
            
            // 1. JSON 解析 (simd-json)
            let orderbook = parser.parse_orderbook_message(json_message);
            
            // 2. 實例選擇 (ArcSwap)
            let instance = manager.select_instance("bitget", "BTCUSDT");
            
            // 3. 處理結果
            black_box((orderbook, instance));
            
            let duration = start.elapsed();
            black_box(duration);
        })
    });

    // 併發處理性能
    group.bench_function("併發消息處理_8線程", |b| {
        b.to_async(&rt).iter(|| async {
            let tasks: Vec<_> = (0..8).map(|_| {
                let manager = &manager;
                tokio::spawn(async move {
                    let mut parser = BitgetMessageParser::new();
                    let json_message = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT","channel":"books15"},"data":[{"asks":[["67189.5","0.25",1]],"bids":[["67188.1","0.12",1]],"ts":"1721810005123"}]}"#;
                    
                    for _ in 0..100 {
                        let start = Instant::now();
                        
                        // JSON 解析 + 實例選擇
                        let orderbook = parser.parse_orderbook_message(json_message);
                        let instance = manager.select_instance("bitget", "BTCUSDT");
                        
                        black_box((orderbook, instance));
                        
                        let duration = start.elapsed();
                        black_box(duration);
                    }
                })
            }).collect();
            
            for task in tasks {
                task.await.unwrap();
            }
        })
    });

    group.finish();
}

/// 壓力測試：高併發場景下的系統表現
fn benchmark_stress_test(c: &mut Criterion) {
    let mut group = c.benchmark_group("壓力測試");

    let rt = Runtime::new().unwrap();
    let manager = create_test_exchange_manager();

    // 高併發實例選擇
    group.bench_function("高併發實例選擇_16線程", |b| {
        b.to_async(&rt).iter(|| async {
            let tasks: Vec<_> = (0..16)
                .map(|thread_id| {
                    let manager = &manager;
                    tokio::spawn(async move {
                        let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "BNBUSDT"];
                        let exchanges = ["bitget", "binance", "bybit"];

                        for _ in 0..1000 {
                            for exchange in &exchanges {
                                for symbol in &symbols {
                                    let instance = manager.select_instance(exchange, symbol);
                                    black_box(instance);

                                    // 模擬輪詢計數更新
                                    tokio::task::yield_now().await;
                                }
                            }
                        }
                        thread_id
                    })
                })
                .collect();

            let results: Vec<_> = futures::future::join_all(tasks).await;
            black_box(results);
        })
    });

    // 混合負載：讀取 + 更新
    group.bench_function("混合負載_讀寫", |b| {
        b.to_async(&rt).iter(|| async {
            let read_tasks: Vec<_> = (0..12)
                .map(|_| {
                    let manager = &manager;
                    tokio::spawn(async move {
                        for _ in 0..500 {
                            let instance = manager.select_instance("bitget", "BTCUSDT");
                            black_box(instance);
                        }
                    })
                })
                .collect();

            let write_tasks: Vec<_> = (0..4)
                .map(|_| {
                    let manager = &manager;
                    tokio::spawn(async move {
                        for _ in 0..10 {
                            let new_snapshot = create_test_snapshots();
                            manager.publish_exchanges(new_snapshot);
                            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                        }
                    })
                })
                .collect();

            futures::future::join_all(read_tasks).await;
            futures::future::join_all(write_tasks).await;
        })
    });

    group.finish();
}

/// 輔助函數：創建測試用的 ExchangeManager
fn create_test_exchange_manager() -> Arc<ExchangeManager> {
    let manager = Arc::new(ExchangeManager::new());

    // 初始化測試數據
    let snapshots = create_test_snapshots();
    manager.publish_exchanges(snapshots);

    manager
}

/// 輔助函數：創建測試快照
fn create_test_snapshots() -> HashMap<String, Arc<ExchangeSnapshot>> {
    let mut snapshots = HashMap::new();

    let exchanges = ["bitget", "binance", "bybit"];
    let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "BNBUSDT", "SOLUSDT"];

    for exchange in &exchanges {
        let mut symbol_map = HashMap::new();

        for symbol in &symbols {
            let instances = vec![
                ExchangeInstance {
                    config: ExchangeInstanceConfig {
                        name: exchange.to_string(),
                        account_id: format!("{}-account-1", exchange),
                        api_key: "test_api_key".to_string(),
                        secret_key: "test_secret_key".to_string(),
                        passphrase: None,
                        enabled: true,
                        priority: 1,
                        testnet: false,
                    },
                    health: Arc::new(tokio::sync::RwLock::new(
                        rust_hft::exchanges::HealthMetrics::default(),
                    )),
                    exchange: Arc::new(tokio::sync::RwLock::new(Box::new(
                        rust_hft::exchanges::bitget::BitgetExchange::new(),
                    ))),
                    last_selected: None,
                },
                ExchangeInstance {
                    config: ExchangeInstanceConfig {
                        name: exchange.to_string(),
                        account_id: format!("{}-account-2", exchange),
                        api_key: "test_api_key_2".to_string(),
                        secret_key: "test_secret_key_2".to_string(),
                        passphrase: None,
                        enabled: true,
                        priority: 2,
                        testnet: false,
                    },
                    health: Arc::new(tokio::sync::RwLock::new(
                        rust_hft::exchanges::HealthMetrics::default(),
                    )),
                    exchange: Arc::new(tokio::sync::RwLock::new(Box::new(
                        rust_hft::exchanges::bitget::BitgetExchange::new(),
                    ))),
                    last_selected: None,
                },
            ];

            symbol_map.insert(symbol.to_string(), Arc::new(instances));
        }

        let snapshot = ExchangeSnapshot::new(symbol_map);
        snapshots.insert(exchange.to_string(), Arc::new(snapshot));
    }

    snapshots
}

criterion_group!(
    benches,
    benchmark_arcswap_performance,
    benchmark_json_parsing_performance,
    benchmark_redis_streams_performance,
    benchmark_end_to_end_latency,
    benchmark_stress_test
);

criterion_main!(benches);
