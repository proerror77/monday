//! DashMap vs RwLock<HashMap> 性能對比基準測試

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

/// 測試數據大小
const NUM_ITEMS: usize = 10000;
const NUM_THREADS: usize = 8;
const NUM_OPERATIONS: usize = 1000;

/// 生成測試數據
fn generate_test_data() -> Vec<(String, i32)> {
    (0..NUM_ITEMS)
        .map(|i| (format!("key_{}", i), i as i32))
        .collect()
}

/// RwLock<HashMap> 併發讀寫基準測試
fn rwlock_hashmap_benchmark(c: &mut Criterion) {
    let data = generate_test_data();
    let map = Arc::new(RwLock::new(HashMap::new()));

    // 預先填充數據
    {
        let mut write_guard = map.write().unwrap();
        for (key, value) in &data {
            write_guard.insert(key.clone(), *value);
        }
    }

    c.bench_function("rwlock_hashmap_concurrent_read", |b| {
        b.iter(|| {
            let map_clone = map.clone();
            let handles: Vec<_> = (0..NUM_THREADS)
                .map(|thread_id| {
                    let map = map_clone.clone();
                    thread::spawn(move || {
                        for i in 0..NUM_OPERATIONS {
                            let key = format!("key_{}", (thread_id * NUM_OPERATIONS + i) % NUM_ITEMS);
                            let _value = map.read().unwrap().get(&key).copied();
                            black_box(_value);
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    c.bench_function("rwlock_hashmap_mixed_read_write", |b| {
        b.iter(|| {
            let map_clone = map.clone();
            let handles: Vec<_> = (0..NUM_THREADS)
                .map(|thread_id| {
                    let map = map_clone.clone();
                    thread::spawn(move || {
                        for i in 0..NUM_OPERATIONS {
                            let key = format!("key_{}", (thread_id * NUM_OPERATIONS + i) % NUM_ITEMS);

                            if i % 10 == 0 {
                                // 10% 寫操作
                                let mut write_guard = map.write().unwrap();
                                write_guard.insert(key, thread_id as i32 * 1000 + i as i32);
                            } else {
                                // 90% 讀操作
                                let _value = map.read().unwrap().get(&key).copied();
                                black_box(_value);
                            }
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });
}

/// DashMap 併發讀寫基準測試
fn dashmap_benchmark(c: &mut Criterion) {
    let data = generate_test_data();
    let map = Arc::new(DashMap::new());

    // 預先填充數據
    for (key, value) in &data {
        map.insert(key.clone(), *value);
    }

    c.bench_function("dashmap_concurrent_read", |b| {
        b.iter(|| {
            let map_clone = map.clone();
            let handles: Vec<_> = (0..NUM_THREADS)
                .map(|thread_id| {
                    let map = map_clone.clone();
                    thread::spawn(move || {
                        for i in 0..NUM_OPERATIONS {
                            let key = format!("key_{}", (thread_id * NUM_OPERATIONS + i) % NUM_ITEMS);
                            let _value = map.get(&key).map(|v| *v);
                            black_box(_value);
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });

    c.bench_function("dashmap_mixed_read_write", |b| {
        b.iter(|| {
            let map_clone = map.clone();
            let handles: Vec<_> = (0..NUM_THREADS)
                .map(|thread_id| {
                    let map = map_clone.clone();
                    thread::spawn(move || {
                        for i in 0..NUM_OPERATIONS {
                            let key = format!("key_{}", (thread_id * NUM_OPERATIONS + i) % NUM_ITEMS);

                            if i % 10 == 0 {
                                // 10% 寫操作
                                map.insert(key, thread_id as i32 * 1000 + i as i32);
                            } else {
                                // 90% 讀操作
                                let _value = map.get(&key).map(|v| *v);
                                black_box(_value);
                            }
                        }
                    })
                })
                .collect();

            for handle in handles {
                handle.join().unwrap();
            }
        });
    });
}

/// 單線程性能測試（用於對比）
fn single_thread_benchmark(c: &mut Criterion) {
    let data = generate_test_data();

    // HashMap 單線程
    let mut hashmap = HashMap::new();
    for (key, value) in &data {
        hashmap.insert(key.clone(), *value);
    }

    c.bench_function("hashmap_single_thread", |b| {
        b.iter(|| {
            for i in 0..NUM_OPERATIONS {
                let key = format!("key_{}", i % NUM_ITEMS);
                if i % 10 == 0 {
                    hashmap.insert(key, i as i32);
                } else {
                    let _value = hashmap.get(&key).copied();
                    black_box(_value);
                }
            }
        });
    });

    // DashMap 單線程
    let dashmap = DashMap::new();
    for (key, value) in &data {
        dashmap.insert(key.clone(), *value);
    }

    c.bench_function("dashmap_single_thread", |b| {
        b.iter(|| {
            for i in 0..NUM_OPERATIONS {
                let key = format!("key_{}", i % NUM_ITEMS);
                if i % 10 == 0 {
                    dashmap.insert(key, i as i32);
                } else {
                    let _value = dashmap.get(&key).map(|v| *v);
                    black_box(_value);
                }
            }
        });
    });
}

criterion_group!(benches, rwlock_hashmap_benchmark, dashmap_benchmark, single_thread_benchmark);
criterion_main!(benches);
// Archived: consolidated under primary benches (see benches/README.md)
