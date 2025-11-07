//! 簡化的架構效果驗證
//!
//! 專注測試 ArcSwap 和 simd-json 核心性能

use arc_swap::ArcSwap;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

// 簡化的測試結構
#[derive(Clone)]
struct SimpleInstance {
    name: String,
    enabled: bool,
}

struct SimpleSnapshot {
    symbols: ArcSwap<HashMap<String, Arc<Vec<SimpleInstance>>>>,
}

impl SimpleSnapshot {
    fn new(map: HashMap<String, Arc<Vec<SimpleInstance>>>) -> Self {
        Self {
            symbols: ArcSwap::new(Arc::new(map)),
        }
    }

    fn load(&self) -> Arc<HashMap<String, Arc<Vec<SimpleInstance>>>> {
        self.symbols.load_full()
    }
}

struct SimpleManager {
    exchanges: ArcSwap<HashMap<String, Arc<SimpleSnapshot>>>,
    rr_counters: Arc<DashMap<(String, String), Arc<AtomicU64>>>,
}

impl SimpleManager {
    fn new() -> Self {
        Self {
            exchanges: ArcSwap::new(Arc::new(HashMap::new())),
            rr_counters: Arc::new(DashMap::new()),
        }
    }

    fn publish_exchanges(&self, map: HashMap<String, Arc<SimpleSnapshot>>) {
        self.exchanges.store(Arc::new(map));
    }

    fn select_instance(&self, exchange: &str, symbol: &str) -> Option<SimpleInstance> {
        let exchanges = self.exchanges.load_full();
        let snap = exchanges.get(exchange)?;
        let sym_map = snap.load();
        let list = sym_map.get(symbol)?;
        if list.is_empty() {
            return None;
        }

        let enabled: Vec<_> = list.iter().filter(|i| i.enabled).cloned().collect();
        if enabled.is_empty() {
            return None;
        }

        let key = (exchange.to_string(), symbol.to_string());
        let arc = self
            .rr_counters
            .entry(key)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        let n = arc.fetch_add(1, Ordering::Relaxed);
        let idx = (n as usize) % enabled.len();
        Some(enabled[idx].clone())
    }
}

/// 測試 ArcSwap 兩層快照性能
fn benchmark_arcswap_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("ArcSwap性能測試");

    let manager = create_test_manager();

    // 測試無鎖實例選擇
    group.bench_function("實例選擇", |b| {
        b.iter(|| {
            for exchange in ["bitget", "binance", "bybit"] {
                for symbol in ["BTCUSDT", "ETHUSDT", "ADAUSDT"] {
                    let instance = manager.select_instance(exchange, symbol);
                    black_box(instance);
                }
            }
        })
    });

    // 測試快照發布性能
    group.bench_function("快照發布", |b| {
        b.iter(|| {
            let new_snapshot = create_test_snapshots();
            manager.publish_exchanges(new_snapshot);
            black_box(());
        })
    });

    group.finish();
}

/// 測試 JSON 解析性能對比
fn benchmark_json_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("JSON解析對比");

    let json_data = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT","channel":"books15"},"data":[{"asks":[["67189.5","0.25",1],["67190.0","0.15",2]],"bids":[["67188.1","0.12",1],["67187.5","0.25",3]],"ts":"1721810005123"}]}"#;

    // serde_json 解析
    group.bench_function("serde_json", |b| {
        b.iter(|| {
            let result: Result<serde_json::Value, _> = serde_json::from_str(json_data);
            black_box(result);
        })
    });

    // simd-json 解析
    group.bench_function("simd_json", |b| {
        b.iter(|| {
            let mut data = json_data.to_string();
            let result = simd_json::from_str::<serde_json::Value>(&mut data);
            black_box(result);
        })
    });

    group.finish();
}

/// 測試併發性能
fn benchmark_concurrent_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("併發性能測試");

    let manager = Arc::new(create_test_manager());

    group.bench_function("併發實例選擇", |b| {
        b.to_async(tokio::runtime::Runtime::new().unwrap())
            .iter(|| async {
                let tasks: Vec<_> = (0..4)
                    .map(|_| {
                        let manager = manager.clone();
                        tokio::spawn(async move {
                            for _ in 0..100 {
                                for exchange in ["bitget", "binance", "bybit"] {
                                    for symbol in ["BTCUSDT", "ETHUSDT"] {
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

    group.finish();
}

/// 測試端到端延遲
fn benchmark_end_to_end_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("端到端延遲");

    let manager = create_test_manager();
    let json_data = r#"{"action":"snapshot","arg":{"instId":"BTCUSDT"},"data":[{"asks":[["67189.5","0.25",1]],"bids":[["67188.1","0.12",1]],"ts":"1721810005123"}]}"#;

    group.bench_function("完整處理流程", |b| {
        b.iter(|| {
            let start = Instant::now();

            // 1. JSON 解析
            let mut data = json_data.to_string();
            let _parsed = simd_json::from_str::<serde_json::Value>(&mut data);

            // 2. 實例選擇
            let _instance = manager.select_instance("bitget", "BTCUSDT");

            let duration = start.elapsed();
            black_box(duration);
        })
    });

    group.finish();
}

fn create_test_manager() -> SimpleManager {
    let manager = SimpleManager::new();
    let snapshots = create_test_snapshots();
    manager.publish_exchanges(snapshots);
    manager
}

fn create_test_snapshots() -> HashMap<String, Arc<SimpleSnapshot>> {
    let mut snapshots = HashMap::new();

    for exchange in ["bitget", "binance", "bybit"] {
        let mut symbol_map = HashMap::new();

        for symbol in ["BTCUSDT", "ETHUSDT", "ADAUSDT"] {
            let instances = vec![
                SimpleInstance {
                    name: format!("{}-account-1", exchange),
                    enabled: true,
                },
                SimpleInstance {
                    name: format!("{}-account-2", exchange),
                    enabled: true,
                },
            ];
            symbol_map.insert(symbol.to_string(), Arc::new(instances));
        }

        let snapshot = SimpleSnapshot::new(symbol_map);
        snapshots.insert(exchange.to_string(), Arc::new(snapshot));
    }

    snapshots
}

criterion_group!(
    benches,
    benchmark_arcswap_performance,
    benchmark_json_parsing,
    benchmark_concurrent_performance,
    benchmark_end_to_end_latency
);

criterion_main!(benches);
// Archived: consolidated under primary benches (see benches/README.md)
