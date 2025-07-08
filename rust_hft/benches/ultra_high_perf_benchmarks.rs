/*!
 * Ultra High-Performance OrderBook Benchmarks
 * 
 * 比較原始版本和優化版本的性能差異
 */

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rust_hft::integrations::high_perf_orderbook_manager::{HighPerfOrderBookManager, HighPerfOrderBook, HighPerfLevel};
use rust_decimal_macros::dec;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};
use crossbeam_channel::unbounded;
use std::time::{Duration, Instant};

// 模擬消息生成器
fn generate_test_messages(count: usize) -> Vec<String> {
    let mut messages = Vec::with_capacity(count);
    
    for i in 0..count {
        let price = 50000.0 + (i as f64 % 100.0);
        let message = format!(
            r#"{{"arg":{{"channel":"books5","instId":"BTCUSDT"}},"data":[{{"asks":[["{}","1.0"]],"bids":[["{}","1.0"]],"ts":"{}","seqId":"{}"}}]}}"#,
            price + 1.0,
            price,
            1699000000000 + i,
            i
        );
        messages.push(message);
    }
    
    messages
}

// 生成大量的 L2 測試數據
fn generate_l2_test_data(levels: usize) -> (Vec<HighPerfLevel>, Vec<HighPerfLevel>) {
    let mut bids = Vec::with_capacity(levels);
    let mut asks = Vec::with_capacity(levels);
    
    for i in 0..levels {
        bids.push(HighPerfLevel {
            price: dec!(50000) - rust_decimal::Decimal::from(i),
            amount: dec!(1.0) + rust_decimal::Decimal::from(i) / rust_decimal::Decimal::from(10),
            order_id: None,
            timestamp: i as u64,
        });
        
        asks.push(HighPerfLevel {
            price: dec!(50000) + rust_decimal::Decimal::from(i + 1),
            amount: dec!(1.0) + rust_decimal::Decimal::from(i) / rust_decimal::Decimal::from(10),
            order_id: None,
            timestamp: i as u64,
        });
    }
    
    (bids, asks)
}

// Benchmark: 原始 HighPerfOrderBookManager 性能
fn bench_original_manager(c: &mut Criterion) {
    let mut group = c.benchmark_group("original_manager");
    
    for batch_size in [100, 500, 1000, 2000].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &batch_size| {
                let manager = Arc::new(RwLock::new(HighPerfOrderBookManager::new(batch_size, 100)));
                let messages = generate_test_messages(batch_size);
                
                b.iter(|| {
                    // 模擬批量處理
                    let start = Instant::now();
                    let mut processed = 0;
                    
                    for message in &messages {
                        // 簡化的處理邏輯
                        if let Ok(_mgr) = manager.try_read() {
                            processed += 1;
                            black_box(message);
                        }
                    }
                    
                    let elapsed = start.elapsed();
                    black_box((processed, elapsed));
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: OrderBook L2 更新性能
fn bench_orderbook_l2_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_l2_updates");
    
    for levels in [10, 50, 100, 200].iter() {
        group.throughput(Throughput::Elements(*levels as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(levels),
            levels,
            |b, &levels| {
                let mut orderbook = HighPerfOrderBook::new("PERFTEST".to_string());
                let (bids, asks) = generate_l2_test_data(levels);
                
                b.iter(|| {
                    orderbook.update_l2_fast(black_box(bids.clone()), black_box(asks.clone()));
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: VWAP 計算性能
fn bench_vwap_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("vwap_calculation");
    
    for depths in [5, 10, 20, 50].iter() {
        group.throughput(Throughput::Elements(*depths as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(depths),
            depths,
            |b, &depths| {
                let mut orderbook = HighPerfOrderBook::new("VWAPTEST".to_string());
                let (bids, asks) = generate_l2_test_data(depths);
                orderbook.update_l2_fast(bids, asks);
                
                b.iter(|| {
                    let vwap = orderbook.calculate_vwap_simd(black_box(depths));
                    black_box(vwap);
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: 無鎖隊列性能
fn bench_lockfree_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfree_queue");
    
    for msg_count in [1000, 5000, 10000, 20000].iter() {
        group.throughput(Throughput::Elements(*msg_count as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(msg_count),
            msg_count,
            |b, &msg_count| {
                let (sender, receiver) = unbounded::<String>();
                let messages = generate_test_messages(msg_count);
                
                b.iter(|| {
                    // 發送階段
                    let send_start = Instant::now();
                    for message in &messages {
                        sender.send(message.clone()).unwrap();
                    }
                    let send_time = send_start.elapsed();
                    
                    // 接收階段
                    let recv_start = Instant::now();
                    let mut received = 0;
                    while let Ok(_msg) = receiver.try_recv() {
                        received += 1;
                        if received >= msg_count {
                            break;
                        }
                    }
                    let recv_time = recv_start.elapsed();
                    
                    black_box((send_time, recv_time, received));
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: 原子操作性能
fn bench_atomic_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("atomic_operations");
    
    for op_count in [1000, 10000, 100000].iter() {
        group.throughput(Throughput::Elements(*op_count as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(op_count),
            op_count,
            |b, &op_count| {
                let counter = AtomicU64::new(0);
                
                b.iter(|| {
                    for _i in 0..op_count {
                        counter.fetch_add(1, Ordering::Relaxed);
                    }
                    let final_value = counter.load(Ordering::Relaxed);
                    black_box(final_value);
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: 內存分配性能
fn bench_memory_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_allocation");
    
    for vec_size in [100, 1000, 5000, 10000].iter() {
        group.throughput(Throughput::Elements(*vec_size as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(vec_size),
            vec_size,
            |b, &vec_size| {
                b.iter(|| {
                    // 測試 Vec 預分配 vs 動態增長
                    let mut vec_prealloc = Vec::with_capacity(vec_size);
                    let mut vec_dynamic = Vec::new();
                    
                    for i in 0..vec_size {
                        vec_prealloc.push(i);
                        vec_dynamic.push(i);
                    }
                    
                    black_box((vec_prealloc, vec_dynamic));
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: JSON 解析性能
fn bench_json_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_parsing");
    
    for msg_count in [100, 500, 1000, 2000].iter() {
        group.throughput(Throughput::Elements(*msg_count as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(msg_count),
            msg_count,
            |b, &msg_count| {
                let messages = generate_test_messages(msg_count);
                
                b.iter(|| {
                    let mut parsed_count = 0;
                    
                    for message in &messages {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(message) {
                            parsed_count += 1;
                            black_box(value);
                        }
                    }
                    
                    black_box(parsed_count);
                });
            },
        );
    }
    
    group.finish();
}

// Benchmark: 延遲測量
fn bench_latency_measurement(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_measurement");
    group.measurement_time(Duration::from_secs(10));
    
    group.bench_function("orderbook_update_latency", |b| {
        let mut orderbook = HighPerfOrderBook::new("LATENCYTEST".to_string());
        let (bids, asks) = generate_l2_test_data(20);
        
        b.iter_custom(|iters| {
            let mut total_duration = Duration::new(0, 0);
            
            for _i in 0..iters {
                let start = Instant::now();
                orderbook.update_l2_fast(black_box(bids.clone()), black_box(asks.clone()));
                total_duration += start.elapsed();
            }
            
            total_duration
        });
    });
    
    group.bench_function("vwap_calculation_latency", |b| {
        let mut orderbook = HighPerfOrderBook::new("VWAPLATENCY".to_string());
        let (bids, asks) = generate_l2_test_data(20);
        orderbook.update_l2_fast(bids, asks);
        
        b.iter_custom(|iters| {
            let mut total_duration = Duration::new(0, 0);
            
            for _i in 0..iters {
                let start = Instant::now();
                let vwap = orderbook.calculate_vwap_simd(black_box(5));
                total_duration += start.elapsed();
                black_box(vwap);
            }
            
            total_duration
        });
    });
    
    group.finish();
}

// 壓力測試：模擬高負載場景
fn bench_stress_test(c: &mut Criterion) {
    let mut group = c.benchmark_group("stress_test");
    group.sample_size(10); // 減少樣本數量因為這是壓力測試
    group.measurement_time(Duration::from_secs(15));
    
    group.bench_function("high_volume_simulation", |b| {
        b.iter(|| {
            let manager = Arc::new(RwLock::new(HighPerfOrderBookManager::new(2000, 100)));
            let messages = generate_test_messages(10000);
            
            let start = Instant::now();
            let mut processed = 0;
            let mut batches = 0;
            
            // 模擬高頻更新
            for chunk in messages.chunks(50) {
                if let Ok(_mgr) = manager.try_read() {
                    for message in chunk {
                        processed += 1;
                        black_box(message);
                    }
                    batches += 1;
                }
            }
            
            let elapsed = start.elapsed();
            let throughput = processed as f64 / elapsed.as_secs_f64();
            
            black_box((processed, batches, throughput));
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    bench_original_manager,
    bench_orderbook_l2_updates,
    bench_vwap_calculation,
    bench_lockfree_queue,
    bench_atomic_operations,
    bench_memory_allocation,
    bench_json_parsing,
    bench_latency_measurement,
    bench_stress_test
);

criterion_main!(benches);