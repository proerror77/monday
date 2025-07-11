/*!
 * Ultra-Performance Benchmark for <1μs Decision Latency
 * 
 * 測試零分配決策引擎的極致性能
 */

use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId};
use rust_hft::core::ultra_performance::{
    ZeroAllocDecisionEngine, UltraFastOrderBookUpdater, HighPerfSPSCQueue, LatencyMeasurer
};
use std::hint::black_box;

/// 基準測試：零分配決策引擎
fn bench_zero_alloc_decision_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("zero_alloc_decision_engine");
    
    // 創建並初始化引擎
    let mut engine = ZeroAllocDecisionEngine::new();
    let weights: Vec<f32> = (0..64).map(|i| 0.1 + i as f32 * 0.001).collect();
    engine.load_weights(&weights).unwrap();
    
    // 測試不同的批次大小
    for batch_size in [1, 10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::new("batch_decisions", batch_size),
            batch_size,
            |b, &batch_size| {
                b.iter(|| {
                    for _ in 0..batch_size {
                        let features = [0.5f32; 64];
                        let decision = black_box(engine.make_decision(&features));
                        black_box(decision);
                    }
                })
            },
        );
    }
    
    group.finish();
}

/// 基準測試：單次決策延遲
fn bench_single_decision_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_decision_latency");
    group.significance_level(0.1).sample_size(10000);
    
    let mut engine = ZeroAllocDecisionEngine::new();
    let weights: Vec<f32> = (0..64).map(|i| 0.1 + i as f32 * 0.001).collect();
    engine.load_weights(&weights).unwrap();
    
    // 不同的特徵模式
    let test_patterns = [
        ("strong_buy", [0.8f32; 64]),
        ("strong_sell", [-0.8f32; 64]),
        ("neutral", [0.0f32; 64]),
        ("mixed", {
            let mut pattern = [0.0f32; 64];
            for (i, val) in pattern.iter_mut().enumerate() {
                *val = if i % 2 == 0 { 0.5 } else { -0.5 };
            }
            pattern
        }),
    ];
    
    for (pattern_name, features) in test_patterns.iter() {
        group.bench_function(
            BenchmarkId::new("pattern", pattern_name),
            |b| {
                b.iter(|| {
                    let decision = black_box(engine.make_decision(black_box(features)));
                    black_box(decision);
                })
            },
        );
    }
    
    group.finish();
}

/// 基準測試：OrderBook更新性能
fn bench_ultra_fast_orderbook(c: &mut Criterion) {
    let mut group = c.benchmark_group("ultra_fast_orderbook");
    
    let mut updater = UltraFastOrderBookUpdater::new();
    
    // 測試數據
    let bids = vec![
        (50000.0, 1.5), (49999.0, 2.0), (49998.0, 1.8), (49997.0, 2.2),
        (49996.0, 1.9), (49995.0, 2.1), (49994.0, 1.7), (49993.0, 2.3),
    ];
    let asks = vec![
        (50001.0, 1.2), (50002.0, 2.5), (50003.0, 1.9), (50004.0, 2.1),
        (50005.0, 1.8), (50006.0, 2.0), (50007.0, 1.6), (50008.0, 2.4),
    ];
    
    group.bench_function("update_and_access", |b| {
        b.iter(|| {
            updater.update_book(black_box(&bids), black_box(&asks));
            let prices = black_box(updater.get_best_prices());
            let spread = black_box(updater.get_spread());
            black_box((prices, spread));
        })
    });
    
    group.finish();
}

/// 基準測試：高性能SPSC隊列
fn bench_high_perf_spsc_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_perf_spsc_queue");
    
    for capacity in [64, 256, 1024, 4096].iter() {
        group.bench_with_input(
            BenchmarkId::new("push_pop_cycle", capacity),
            capacity,
            |b, &capacity| {
                let queue = HighPerfSPSCQueue::new(capacity);
                
                b.iter(|| {
                    // 推送階段
                    for i in 0..capacity / 2 {
                        let _ = black_box(queue.try_push(black_box(i)));
                    }
                    
                    // 彈出階段
                    for _ in 0..capacity / 2 {
                        let _ = black_box(queue.try_pop());
                    }
                })
            },
        );
    }
    
    group.finish();
}

/// 基準測試：延遲測量開銷
fn bench_latency_measurement_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_measurement");
    
    let measurer = LatencyMeasurer::new();
    
    group.bench_function("start_end_cycle", |b| {
        b.iter(|| {
            let start = black_box(measurer.start());
            // 模擬微小的工作負載
            std::hint::black_box(42 + 24);
            let latency = black_box(measurer.end_and_record(start));
            black_box(latency);
        })
    });
    
    group.bench_function("statistics_calculation", |b| {
        // 先填充一些數據
        for _ in 0..1000 {
            let start = measurer.start();
            std::thread::sleep(std::time::Duration::from_nanos(100));
            measurer.end_and_record(start);
        }
        
        b.iter(|| {
            let stats = black_box(measurer.calculate_stats());
            black_box(stats);
        })
    });
    
    group.finish();
}

/// 綜合性能基準測試
fn bench_integrated_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("integrated_performance");
    
    // 設置完整的交易管道
    let mut decision_engine = ZeroAllocDecisionEngine::new();
    let weights: Vec<f32> = (0..64).map(|i| 0.1 + i as f32 * 0.001).collect();
    decision_engine.load_weights(&weights).unwrap();
    
    let mut orderbook_updater = UltraFastOrderBookUpdater::new();
    let latency_measurer = LatencyMeasurer::new();
    
    // 測試數據
    let features = [0.3f32; 64];
    let bids = vec![(50000.0, 1.5), (49999.0, 2.0), (49998.0, 1.8)];
    let asks = vec![(50001.0, 1.2), (50002.0, 2.5), (50003.0, 1.9)];
    
    group.bench_function("full_trading_pipeline", |b| {
        b.iter(|| {
            let start = latency_measurer.start();
            
            // 1. 更新OrderBook
            orderbook_updater.update_book(black_box(&bids), black_box(&asks));
            
            // 2. 獲取價格信息
            let (best_bid, best_ask) = black_box(orderbook_updater.get_best_prices());
            
            // 3. 做出交易決策
            let decision = black_box(decision_engine.make_decision(black_box(&features)));
            
            // 4. 記錄延遲
            let latency = latency_measurer.end_and_record(start);
            
            black_box((best_bid, best_ask, decision, latency));
        })
    });
    
    group.finish();
}

/// 內存分配基準測試
fn bench_memory_allocation_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_allocation_impact");
    
    // 零分配版本
    group.bench_function("zero_allocation", |b| {
        let mut engine = ZeroAllocDecisionEngine::new();
        let weights: Vec<f32> = (0..64).map(|i| 0.1 + i as f32 * 0.001).collect();
        engine.load_weights(&weights).unwrap();
        
        b.iter(|| {
            let features = [0.5f32; 64];
            let decision = black_box(engine.make_decision(black_box(&features)));
            black_box(decision);
        })
    });
    
    // 傳統分配版本（對比）
    group.bench_function("with_allocation", |b| {
        b.iter(|| {
            // 模擬傳統方法：每次都分配向量
            let features: Vec<f32> = vec![0.5f32; 64];
            let weights: Vec<f32> = vec![0.1f32; 64];
            
            let result: f32 = features.iter()
                .zip(weights.iter())
                .map(|(f, w)| f * w)
                .sum();
            
            let decision = if result > 0.5 { 1i8 } else if result < -0.5 { -1i8 } else { 0i8 };
            black_box(decision);
        })
    });
    
    group.finish();
}

criterion_group!(
    ultra_performance_benches,
    bench_zero_alloc_decision_engine,
    bench_single_decision_latency,
    bench_ultra_fast_orderbook,
    bench_high_perf_spsc_queue,
    bench_latency_measurement_overhead,
    bench_integrated_performance,
    bench_memory_allocation_impact
);

criterion_main!(ultra_performance_benches);