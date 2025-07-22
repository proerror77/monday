/*!
 * 延遲基準測試
 * 
 * 提取自各個獨立測試項目的性能測試代碼
 */

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};
use std::time::Instant;
use dashmap::DashMap;
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;

// 核心組件導入
use rust_hft::core::orderbook::OrderBook;
use std::collections::BTreeMap;

type Price = OrderedFloat<f64>;
type Quantity = Decimal;

/// 生成測試數據
fn generate_orderbook_test_data(count: usize) -> Vec<(Side, Price, Quantity)> {
    let mut data = Vec::with_capacity(count);
    
    for i in 0..count {
        let side = if i % 2 == 0 { Side::Bid } else { Side::Ask };
        let base_price = if side == Side::Bid { 100.0 } else { 100.01 };
        let price = Price::from(base_price + (i as f64 * 0.0001));
        let quantity = Quantity::try_from(10.0 + (i as f64 % 50.0)).unwrap();
        data.push((side, price, quantity));
    }
    
    data
}

/// OrderBook 性能基準測試
fn bench_orderbook_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook");
    
    for size in [1000, 5000, 10000].iter() {
        let test_data = generate_orderbook_test_data(*size);
        
        group.throughput(Throughput::Elements(*size as u64));
        
        // 傳統 OrderBook 基準測試
        group.bench_with_input(
            BenchmarkId::new("traditional", size),
            &test_data,
            |b, data| {
                b.iter(|| {
                    let mut ob = TraditionalOrderBook::new();
                    for (side, price, quantity) in data {
                        ob.update(*side, *price, *quantity);
                    }
                    black_box(ob.get_best_prices());
                })
            },
        );
        
        // Lock-Free OrderBook 基準測試
        group.bench_with_input(
            BenchmarkId::new("lockfree", size),
            &test_data,
            |b, data| {
                b.iter(|| {
                    let ob = LockFreeOrderBook::new();
                    for (side, price, quantity) in data {
                        ob.update(*side, *price, *quantity);
                    }
                    black_box(ob.get_best_prices());
                })
            },
        );
    }
    
    group.finish();
}

/// 零拷貝處理性能基準測試
fn bench_zero_copy_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("zero_copy");
    
    for size in [512, 1024, 2048].iter() {
        let message = TestMessage::new(*size);
        
        group.throughput(Throughput::Bytes(*size as u64));
        
        // 傳統處理基準測試
        group.bench_with_input(
            BenchmarkId::new("traditional", size),
            &message,
            |b, msg| {
                let processor = TraditionalProcessor::new();
                b.iter(|| {
                    black_box(processor.process(msg));
                })
            },
        );
        
        // 零拷貝處理基準測試
        group.bench_with_input(
            BenchmarkId::new("zero_copy", size),
            &message,
            |b, msg| {
                let processor = ZeroCopyProcessor::new();
                b.iter(|| {
                    black_box(processor.process_zero_copy(&msg.data, msg.timestamp));
                })
            },
        );
    }
    
    group.finish();
}

/// 批量處理性能基準測試
fn bench_batch_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_processing");
    
    for batch_size in [8, 16, 32, 64].iter() {
        let messages: Vec<_> = (0..*batch_size).map(|_| TestMessage::new(512)).collect();
        let message_refs: Vec<_> = messages.iter().collect();
        
        group.throughput(Throughput::Elements(*batch_size as u64));
        
        group.bench_with_input(
            BenchmarkId::new("batch_zero_copy", batch_size),
            &message_refs,
            |b, msgs| {
                use rust_hft::tests::unit::zero_copy_test::BatchZeroCopyProcessor;
                let processor = BatchZeroCopyProcessor::new();
                b.iter(|| {
                    black_box(processor.process_batch(msgs));
                })
            },
        );
    }
    
    group.finish();
}

/// 統一HFT系統性能基準測試
fn bench_unified_hft_system(c: &mut Criterion) {
    let mut group = c.benchmark_group("unified_hft");
    
    for message_count in [100, 500, 1000].iter() {
        let engine = UnifiedHftEngine::new("BTCUSDT".to_string(), 1);
        let messages: Vec<_> = (0..*message_count)
            .map(|i| ZeroCopyMarketMessage::new("BTCUSDT".to_string(), i as u64))
            .collect();
        
        group.throughput(Throughput::Elements(*message_count as u64));
        
        group.bench_with_input(
            BenchmarkId::new("unified_processing", message_count),
            &messages,
            |b, msgs| {
                b.iter(|| {
                    engine.start();
                    for msg in msgs {
                        black_box(engine.process_market_message(msg).unwrap());
                    }
                    engine.stop();
                })
            },
        );
    }
    
    group.finish();
}

/// 並發性能基準測試
fn bench_concurrent_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("concurrent");
    
    for thread_count in [1, 2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::new("lockfree_concurrent", thread_count),
            thread_count,
            |b, &threads| {
                b.iter(|| {
                    let orderbook = Arc::new(LockFreeOrderBook::new());
                    let handles: Vec<_> = (0..threads)
                        .map(|thread_id| {
                            let ob = orderbook.clone();
                            std::thread::spawn(move || {
                                for i in 0..1000 {
                                    let side = if (i + thread_id) % 2 == 0 { Side::Bid } else { Side::Ask };
                                    let price = Price::from(100.0 + (i as f64 * 0.001));
                                    let quantity = Quantity::try_from(1.0).unwrap();
                                    ob.update(side, price, quantity);
                                }
                            })
                        })
                        .collect();
                    
                    for handle in handles {
                        handle.join().unwrap();
                    }
                    
                    black_box(orderbook.get_best_prices());
                })
            },
        );
    }
    
    group.finish();
}

/// 延遲分佈測試
fn bench_latency_distribution(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_distribution");
    group.sample_size(10000); // 增加樣本數量以獲得更好的分佈數據
    
    // 測試單次OrderBook更新的延遲分佈
    group.bench_function("single_orderbook_update", |b| {
        let orderbook = LockFreeOrderBook::new();
        let mut counter = 0u64;
        
        b.iter(|| {
            let side = if counter % 2 == 0 { Side::Bid } else { Side::Ask };
            let price = Price::from(100.0 + (counter as f64 * 0.001));
            let quantity = Decimal::try_from(1.0).unwrap();
            
            let start = Instant::now();
            orderbook.update(side, price, quantity);
            let latency = start.elapsed();
            
            counter += 1;
            black_box(latency);
        })
    });
    
    // 測試單次零拷貝處理的延遲分佈
    group.bench_function("single_zero_copy_process", |b| {
        let processor = ZeroCopyProcessor::new();
        let message = TestMessage::new(512);
        
        b.iter(|| {
            let start = Instant::now();
            processor.process_zero_copy(&message.data, message.timestamp);
            let latency = start.elapsed();
            black_box(latency);
        })
    });
    
    group.finish();
}

/// 內存分配基準測試
fn bench_memory_allocation(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_allocation");
    
    for size in [256, 512, 1024, 2048].iter() {
        // 測試零拷貝 vs 傳統分配
        group.bench_with_input(
            BenchmarkId::new("zero_allocation", size),
            size,
            |b, &msg_size| {
                let data = vec![0u8; msg_size];
                b.iter(|| {
                    // 零拷貝：直接使用引用
                    let slice = &data[..];
                    let _sum: u32 = slice.iter().map(|&b| b as u32).sum();
                    black_box(slice);
                })
            },
        );
        
        group.bench_with_input(
            BenchmarkId::new("with_allocation", size),
            size,
            |b, &msg_size| {
                let data = vec![0u8; msg_size];
                b.iter(|| {
                    // 傳統：克隆數據
                    let owned = data.clone();
                    let _sum: u32 = owned.iter().map(|&b| b as u32).sum();
                    black_box(owned);
                })
            },
        );
    }
    
    group.finish();
}

criterion_group!(
    benches,
    bench_orderbook_performance,
    bench_zero_copy_processing,
    bench_batch_processing,
    bench_unified_hft_system,
    bench_concurrent_performance,
    bench_latency_distribution,
    bench_memory_allocation
);

criterion_main!(benches);