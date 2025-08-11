/*!
 * Simplified Performance Benchmarks
 * 
 * Core performance tests focusing on CLAUDE.md requirements:
 * - p99 ≤ 25 μs latency for critical path
 * - ≥ 100,000 ops/sec orderbook updates
 * - Memory efficiency testing
 */

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rust_hft::core::{LockFreeOrderBook, UltraFastOrderBook};
use rust_hft::core::types::{Price, Quantity, Side, OrderBookUpdate, PriceLevel};
use std::sync::Arc;
use std::time::{Duration, Instant};
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;

/// Generate test orderbook updates
fn generate_test_updates(count: usize) -> Vec<OrderBookUpdate> {
    let mut updates = Vec::with_capacity(count);
    
    for i in 0..count {
        let base_price = 50000.0 + (i as f64 % 1000.0) / 10.0;
        
        let mut bids = Vec::new();
        let mut asks = Vec::new();
        
        // Generate 10 levels of depth
        for level in 0..10 {
            let bid_price = base_price - (level as f64 * 0.1);
            let ask_price = base_price + (level as f64 * 0.1) + 0.1;
            let quantity = 1.0 + (level as f64 * 0.5);
            
            bids.push(PriceLevel {
                price: Price::from(bid_price),
                quantity: Quantity::try_from(quantity).unwrap(),
                order_count: Some(level as u32 + 1),
            });
            
            asks.push(PriceLevel {
                price: Price::from(ask_price),
                quantity: Quantity::try_from(quantity).unwrap(),
                order_count: Some(level as u32 + 1),
            });
        }
        
        updates.push(OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            bids,
            asks,
            timestamp: 1699000000000 + i as u64,
            sequence_start: i as u64,
            sequence_end: i as u64,
            is_snapshot: i == 0,
        });
    }
    
    updates
}

/// Benchmark: Lock-Free OrderBook Critical Path
fn bench_lockfree_critical_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfree_critical_path");
    
    for batch_size in [10, 50, 100].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &batch_size| {
                let orderbook = Arc::new(LockFreeOrderBook::new("BTCUSDT".to_string()));
                let updates = generate_test_updates(batch_size);
                
                b.iter_custom(|iters| {
                    let mut total_duration = Duration::new(0, 0);
                    
                    for _i in 0..iters {
                        for update in &updates {
                            let start = Instant::now();
                            
                            // Critical path: Update orderbook
                            for bid in &update.bids {
                                let _ = orderbook.update_level(Side::Bid, bid.price, bid.quantity);
                            }
                            for ask in &update.asks {
                                let _ = orderbook.update_level(Side::Ask, ask.price, ask.quantity);
                            }
                            
                            // Critical path: Get best prices
                            black_box(orderbook.best_bid());
                            black_box(orderbook.best_ask());
                            black_box(orderbook.mid_price());
                            
                            total_duration += start.elapsed();
                        }
                    }
                    
                    total_duration
                });
            },
        );
    }
    
    group.finish();
}

/// Benchmark: High-Throughput Updates
fn bench_high_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_throughput");
    
    for ops_target in [10_000, 50_000, 100_000].iter() {
        group.throughput(Throughput::Elements(*ops_target as u64));
        
        group.bench_with_input(
            BenchmarkId::from_parameter(ops_target),
            ops_target,
            |b, &ops_target| {
                let orderbook = Arc::new(LockFreeOrderBook::new("THROUGHPUT_TEST".to_string()));
                
                b.iter(|| {
                    let start = Instant::now();
                    let mut operations = 0;
                    
                    while operations < ops_target && start.elapsed() < Duration::from_secs(10) {
                        let price_variation = (operations % 1000) as f64 * 0.01;
                        let base_price = 50000.0;
                        
                        let bid_price = Price::from(base_price - 1.0 - price_variation);
                        let ask_price = Price::from(base_price + 1.0 + price_variation);
                        let quantity = Quantity::try_from(1.0 + price_variation).unwrap();
                        
                        let _ = orderbook.update_level(Side::Bid, bid_price, quantity);
                        let _ = orderbook.update_level(Side::Ask, ask_price, quantity);
                        
                        operations += 2;
                    }
                    
                    let elapsed = start.elapsed();
                    let actual_throughput = operations as f64 / elapsed.as_secs_f64();
                    
                    black_box((operations, actual_throughput));
                });
            },
        );
    }
    
    group.finish();
}

/// Benchmark: Ultra-Fast OrderBook vs Lock-Free
fn bench_orderbook_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_comparison");
    
    let updates = generate_test_updates(100);
    
    // Lock-Free OrderBook
    group.bench_function("lockfree_orderbook", |b| {
        let orderbook = Arc::new(LockFreeOrderBook::new("COMPARE_LF".to_string()));
        
        b.iter(|| {
            for update in &updates {
                for bid in &update.bids {
                    let _ = orderbook.update_level(Side::Bid, bid.price, bid.quantity);
                }
                for ask in &update.asks {
                    let _ = orderbook.update_level(Side::Ask, ask.price, ask.quantity);
                }
                black_box(orderbook.best_bid());
                black_box(orderbook.best_ask());
            }
        });
    });
    
    // Ultra-Fast OrderBook (if available and compiled)
    #[cfg(feature = "ultra_fast")]
    group.bench_function("ultra_fast_orderbook", |b| {
        let orderbook = Arc::new(UltraFastOrderBook::new("COMPARE_UF".to_string()));
        
        b.iter(|| {
            for update in &updates {
                let bid_levels: Vec<_> = update.bids.iter()
                    .map(|l| (l.price, l.quantity))
                    .collect();
                let ask_levels: Vec<_> = update.asks.iter()
                    .map(|l| (l.price, l.quantity))
                    .collect();
                
                let _ = orderbook.update_bulk_simd(Side::Bid, &bid_levels);
                let _ = orderbook.update_bulk_simd(Side::Ask, &ask_levels);
                
                black_box(orderbook.best_bid());
                black_box(orderbook.best_ask());
            }
        });
    });
    
    group.finish();
}

/// Benchmark: Memory Usage
fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_usage");
    
    for depth_levels in [50, 100, 200].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(depth_levels),
            depth_levels,
            |b, &depth_levels| {
                b.iter(|| {
                    let orderbook = LockFreeOrderBook::new("MEMORY_TEST".to_string());
                    
                    // Fill orderbook with specified depth
                    for level in 0..depth_levels {
                        let bid_price = Price::from(50000.0 - level as f64 * 0.1);
                        let ask_price = Price::from(50000.0 + level as f64 * 0.1 + 0.1);
                        let quantity = Quantity::try_from(10.0 + level as f64).unwrap();
                        
                        let _ = orderbook.update_level(Side::Bid, bid_price, quantity);
                        let _ = orderbook.update_level(Side::Ask, ask_price, quantity);
                    }
                    
                    let stats = orderbook.stats();
                    let estimated_memory_kb = (stats.bid_levels + stats.ask_levels) * 256 / 1024;
                    
                    black_box((stats.bid_levels, stats.ask_levels, estimated_memory_kb));
                });
            },
        );
    }
    
    group.finish();
}

/// Benchmark: Latency Distribution
fn bench_latency_distribution(c: &mut Criterion) {
    let mut group = c.benchmark_group("latency_distribution");
    group.measurement_time(Duration::from_secs(10));
    
    group.bench_function("orderbook_update_latency", |b| {
        let orderbook = Arc::new(LockFreeOrderBook::new("LATENCY_TEST".to_string()));
        
        b.iter_custom(|iters| {
            let mut latencies = Vec::with_capacity(iters as usize);
            
            for i in 0..iters {
                let price = Price::from(50000.0 + (i as f64 % 100.0));
                let quantity = Quantity::try_from(1.0).unwrap();
                
                let start = Instant::now();
                let _ = orderbook.update_level(Side::Bid, price, quantity);
                let _ = orderbook.best_bid();
                let elapsed = start.elapsed();
                
                latencies.push(elapsed);
            }
            
            // Calculate statistics
            latencies.sort();
            let len = latencies.len();
            let p50 = latencies[len / 2];
            let p95 = latencies[(len * 95) / 100];
            let p99 = latencies[(len * 99) / 100];
            
            // Log statistics for analysis
            if len > 1000 {
                eprintln!("Latency stats: P50={:?}, P95={:?}, P99={:?}", p50, p95, p99);
                
                // Check if we meet CLAUDE.md requirements
                if p99.as_micros() > 25 {
                    eprintln!("⚠️  P99 latency {}μs exceeds target of 25μs", p99.as_micros());
                } else {
                    eprintln!("✅ P99 latency {}μs meets target of <25μs", p99.as_micros());
                }
            }
            
            latencies.iter().sum()
        });
    });
    
    group.finish();
}

criterion_group!(
    performance_benchmarks,
    bench_lockfree_critical_path,
    bench_high_throughput,
    bench_orderbook_comparison,
    bench_memory_usage,
    bench_latency_distribution
);

criterion_main!(performance_benchmarks);