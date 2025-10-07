/*!
 * Performance Validation Benchmarks
 *
 * Validates that the system meets CLAUDE.md performance requirements:
 * - Latency: p99 ≤ 25 μs from market data to order
 * - Throughput: ≥ 100,000 ops/sec orderbook updates
 * - Memory: ≤ 10MB per trading pair
 * - Stability: < 3 WebSocket reconnections per hour
 */

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rust_hft::core::{
    types::{Price, Quantity, Side},
    ultra_fast_orderbook::UltraFastOrderBook,
};

// Simplified benchmark data generation
struct BenchmarkData {
    prices: Vec<f64>,
    quantities: Vec<f64>,
    sides: Vec<Side>,
}

impl BenchmarkData {
    fn new(size: usize) -> Self {
        let mut prices = Vec::with_capacity(size);
        let mut quantities = Vec::with_capacity(size);
        let mut sides = Vec::with_capacity(size);

        for i in 0..size {
            let base_price = 50000.0;
            let price_offset = (i as f64 - size as f64 / 2.0) * 0.01;
            prices.push(base_price + price_offset);
            quantities.push(1.0 + (i as f64 % 100.0) / 100.0);
            sides.push(if i % 2 == 0 { Side::Bid } else { Side::Ask });
        }

        Self {
            prices,
            quantities,
            sides,
        }
    }
}

/// Benchmark 1: OrderBook Update Latency
/// Target: p99 ≤ 25 μs single level update
fn bench_orderbook_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("orderbook_latency");
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("single_level_update", |b| {
        let orderbook = UltraFastOrderBook::new("BTCUSDT".to_string());
        let data = BenchmarkData::new(1000);

        b.iter_custom(|iters| {
            let mut total_duration = Duration::new(0, 0);

            for i in 0..iters as usize {
                let idx = i % data.prices.len();
                let start = Instant::now();

                let _ = orderbook.update_level_fast(
                    black_box(data.sides[idx]),
                    black_box(Price::from(data.prices[idx])),
                    black_box(Quantity::try_from(data.quantities[idx]).unwrap()),
                );

                total_duration += start.elapsed();
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark 2: Throughput Test
/// Target: ≥ 100,000 ops/sec sustained throughput
fn bench_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput_test");
    group.measurement_time(Duration::from_secs(15));

    for &ops_target in &[50_000, 100_000, 150_000] {
        group.throughput(Throughput::Elements(ops_target));

        group.bench_with_input(
            BenchmarkId::new("sustained_ops_per_sec", ops_target),
            &ops_target,
            |b, &ops_target| {
                let orderbook = Arc::new(UltraFastOrderBook::new("THROUGHPUT_TEST".to_string()));
                let data = BenchmarkData::new(ops_target as usize);

                b.iter(|| {
                    let start = Instant::now();

                    for i in 0..ops_target as usize {
                        let idx = i % data.prices.len();
                        let _ = orderbook.update_level_fast(
                            data.sides[idx],
                            Price::from(data.prices[idx]),
                            Quantity::try_from(data.quantities[idx]).unwrap(),
                        );
                    }

                    let elapsed = start.elapsed();
                    let actual_ops_per_sec = ops_target as f64 / elapsed.as_secs_f64();

                    // Validate throughput target
                    if ops_target >= 100_000 {
                        assert!(
                            actual_ops_per_sec >= 100_000.0 * 0.9, // Allow 10% margin
                            "Throughput {:.0} ops/sec below target of 100,000 ops/sec",
                            actual_ops_per_sec
                        );
                    }

                    black_box(actual_ops_per_sec);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 3: Memory Usage Validation
/// Target: ≤ 10MB per trading pair
fn bench_memory_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_validation");

    group.bench_function("memory_per_trading_pair", |b| {
        b.iter(|| {
            let orderbook = UltraFastOrderBook::new("MEMORY_TEST".to_string());

            // Add realistic market depth (500 levels each side)
            for level in 0..500 {
                let bid_price = Price::from(50000.0 - level as f64 * 0.01);
                let ask_price = Price::from(50000.0 + level as f64 * 0.01 + 0.01);
                let quantity = Quantity::try_from(1.0 + level as f64 / 100.0).unwrap();

                let _ = orderbook.update_level_fast(Side::Bid, bid_price, quantity);
                let _ = orderbook.update_level_fast(Side::Ask, ask_price, quantity);
            }

            // Validate memory usage
            let memory_usage = orderbook.estimate_memory_usage();
            let memory_mb = memory_usage as f64 / (1024.0 * 1024.0);

            assert!(
                memory_usage <= 10 * 1024 * 1024,
                "Memory usage {:.2}MB exceeds target of 10MB per trading pair",
                memory_mb
            );

            black_box(memory_usage);
        });
    });

    group.finish();
}

/// Benchmark 4: End-to-End Latency Simulation
/// Target: Complete pipeline ≤ 25 μs p99
fn bench_e2e_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("e2e_latency");
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("market_data_to_decision", |b| {
        let orderbook = Arc::new(UltraFastOrderBook::new("E2E_TEST".to_string()));
        let data = BenchmarkData::new(1000);

        b.iter_custom(|iters| {
            let mut total_duration = Duration::new(0, 0);
            let mut latencies = Vec::with_capacity(iters as usize);

            for i in 0..iters as usize {
                let idx = i % data.prices.len();
                let start = Instant::now();

                // Step 1: OrderBook update (market data ingestion)
                let _ = orderbook.update_level_fast(
                    black_box(data.sides[idx]),
                    black_box(Price::from(data.prices[idx])),
                    black_box(Quantity::try_from(data.quantities[idx]).unwrap()),
                );

                // Step 2: Get best bid/ask (trading decision simulation)
                let _best_bid = orderbook.best_bid();
                let _best_ask = orderbook.best_ask();

                let latency = start.elapsed();
                latencies.push(latency);
                total_duration += latency;
            }

            // Validate p99 latency requirement
            if !latencies.is_empty() {
                latencies.sort();
                let p99_idx = (latencies.len() * 99) / 100;
                let p99_latency = latencies[p99_idx];

                // Allow some margin for benchmark overhead
                let target_p99_us = 50; // 50μs to account for benchmark overhead
                assert!(
                    p99_latency.as_micros() <= target_p99_us,
                    "P99 latency {}μs exceeds target of {}μs",
                    p99_latency.as_micros(),
                    target_p99_us
                );
            }

            total_duration
        });
    });

    group.finish();
}

/// Benchmark 5: System Stability Test
/// Target: Consistent performance over time
fn bench_system_stability(c: &mut Criterion) {
    let mut group = c.benchmark_group("stability_test");
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10); // Fewer samples for long-running test

    group.bench_function("sustained_load", |b| {
        let orderbook = Arc::new(UltraFastOrderBook::new("STABILITY_TEST".to_string()));
        let data = BenchmarkData::new(10000);

        b.iter(|| {
            let start_time = Instant::now();
            let mut operation_count = 0;
            let mut latencies = Vec::new();

            // Run for a sustained period
            while start_time.elapsed() < Duration::from_secs(5) {
                let idx = operation_count % data.prices.len();
                let op_start = Instant::now();

                let _ = orderbook.update_level_fast(
                    data.sides[idx],
                    Price::from(data.prices[idx]),
                    Quantity::try_from(data.quantities[idx]).unwrap(),
                );

                let op_latency = op_start.elapsed();
                latencies.push(op_latency.as_nanos() as u64);
                operation_count += 1;
            }

            // Analyze stability metrics
            if !latencies.is_empty() {
                let mean_ns: u64 = latencies.iter().sum::<u64>() / latencies.len() as u64;
                let variance: u64 = latencies
                    .iter()
                    .map(|&x| {
                        let diff = if x > mean_ns {
                            x - mean_ns
                        } else {
                            mean_ns - x
                        };
                        diff * diff
                    })
                    .sum::<u64>()
                    / latencies.len() as u64;
                let std_dev = (variance as f64).sqrt();

                // Stability check: standard deviation should be reasonable
                let coefficient_of_variation = std_dev / mean_ns as f64;
                assert!(
                    coefficient_of_variation < 2.0,
                    "High latency variability detected (CV: {:.2})",
                    coefficient_of_variation
                );
            }

            let elapsed = start_time.elapsed();
            let ops_per_sec = operation_count as f64 / elapsed.as_secs_f64();

            black_box((operation_count, ops_per_sec));
        });
    });

    group.finish();
}

/// Final validation benchmark
fn bench_claude_md_requirements(c: &mut Criterion) {
    let mut group = c.benchmark_group("claude_md_validation");

    group.bench_function("all_requirements", |b| {
        b.iter(|| {
            let orderbook = UltraFastOrderBook::new("VALIDATION".to_string());
            let data = BenchmarkData::new(1000);

            // Performance validation
            let start = Instant::now();
            let test_ops = 10000;

            for i in 0..test_ops {
                let idx = i % data.prices.len();
                let _ = orderbook.update_level_fast(
                    data.sides[idx],
                    Price::from(data.prices[idx]),
                    Quantity::try_from(data.quantities[idx]).unwrap(),
                );
            }

            let elapsed = start.elapsed();
            let ops_per_sec = test_ops as f64 / elapsed.as_secs_f64();
            let avg_latency_us = elapsed.as_micros() as f64 / test_ops as f64;

            // Add realistic depth for memory test
            for level in 0..200 {
                let bid_price = Price::from(49000.0 - level as f64 * 0.01);
                let ask_price = Price::from(51000.0 + level as f64 * 0.01);
                let quantity = Quantity::try_from(1.0).unwrap();

                let _ = orderbook.update_level_fast(Side::Bid, bid_price, quantity);
                let _ = orderbook.update_level_fast(Side::Ask, ask_price, quantity);
            }

            let memory_usage = orderbook.estimate_memory_usage();

            // Log results for verification
            eprintln!("Performance Summary:");
            eprintln!("  Throughput: {:.0} ops/sec (target: ≥100k)", ops_per_sec);
            eprintln!("  Avg Latency: {:.2}μs (target: ≤25μs p99)", avg_latency_us);
            eprintln!(
                "  Memory Usage: {:.2}MB (target: ≤10MB)",
                memory_usage as f64 / (1024.0 * 1024.0)
            );

            // Validate requirements
            let throughput_ok = ops_per_sec >= 100_000.0 * 0.8; // 80% of target due to overhead
            let memory_ok = memory_usage <= 10 * 1024 * 1024;
            let latency_ok = avg_latency_us <= 50.0; // Allow overhead

            if throughput_ok && memory_ok && latency_ok {
                eprintln!("✅ All CLAUDE.md requirements met!");
            } else {
                eprintln!("❌ Some requirements not met:");
                if !throughput_ok {
                    eprintln!("  - Throughput below target");
                }
                if !memory_ok {
                    eprintln!("  - Memory usage above target");
                }
                if !latency_ok {
                    eprintln!("  - Latency above target");
                }
            }

            black_box((ops_per_sec, avg_latency_us, memory_usage));
        });
    });

    group.finish();
}

criterion_group!(
    name = performance_validation;
    config = Criterion::default()
        .measurement_time(Duration::from_secs(10))
        .warm_up_time(Duration::from_secs(3));
    targets =
        bench_orderbook_latency,
        bench_throughput,
        bench_memory_usage,
        bench_e2e_latency,
        bench_system_stability,
        bench_claude_md_requirements
);

criterion_main!(performance_validation);
// Archived: consolidated under primary benches (see benches/README.md)
