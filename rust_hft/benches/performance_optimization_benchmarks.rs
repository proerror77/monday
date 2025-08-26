/*!
 * Performance Optimization Benchmarks
 *
 * Comprehensive benchmarking suite targeting CLAUDE.md requirements:
 * - p99 ≤ 25 μs latency for market data processing to order execution
 * - ≥ 100,000 ops/sec orderbook updates
 * - ≤ 10MB memory usage per trading pair
 * - < 3 WebSocket reconnections per hour
 */

use criterion::{
    black_box, criterion_group, criterion_main, measurement::WallTime, BenchmarkId, Criterion,
    Measurement, Throughput,
};
use crossbeam_channel::unbounded;
use ordered_float::OrderedFloat;
use rust_decimal::Decimal;
use rust_hft::core::lockfree_orderbook::{LockFreeOrderBook, LockFreePriceLevel};
use rust_hft::core::types::{OrderBookUpdate, Price, PriceLevel, Quantity, Side};
use rust_hft::integrations::multi_exchange_manager::MultiExchangeManager;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Runtime;

/// 高精度延迟测量器
struct LatencyMeasurement {
    samples: Vec<Duration>,
    start_time: Instant,
}

impl LatencyMeasurement {
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(10000),
            start_time: Instant::now(),
        }
    }

    fn record(&mut self) {
        let elapsed = self.start_time.elapsed();
        self.samples.push(elapsed);
        self.start_time = Instant::now();
    }

    fn p99_latency(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::from_nanos(0);
        }
        let mut sorted = self.samples.clone();
        sorted.sort();
        let index = (sorted.len() as f64 * 0.99) as usize;
        sorted[index.min(sorted.len() - 1)]
    }

    fn average_latency(&self) -> Duration {
        if self.samples.is_empty() {
            return Duration::from_nanos(0);
        }
        let total: Duration = self.samples.iter().sum();
        total / self.samples.len() as u32
    }
}

/// 生成测试数据
fn generate_market_data_batch(count: usize, symbol: &str) -> Vec<OrderBookUpdate> {
    let mut updates = Vec::with_capacity(count);

    for i in 0..count {
        let base_price = 50000.0 + (i as f64 % 1000.0) / 10.0; // Price variation

        let mut bids = Vec::new();
        let mut asks = Vec::new();

        // Generate 20 levels of depth
        for level in 0..20 {
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
            symbol: symbol.to_string(),
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

/// Benchmark 1: Lock-Free OrderBook Critical Path Performance
/// Target: Process orderbook updates in < 25μs p99
fn bench_lockfree_orderbook_critical_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfree_orderbook_critical_path");

    // Test with different update batch sizes
    for batch_size in [1, 10, 50, 100, 500].iter() {
        group.throughput(Throughput::Elements(*batch_size as u64));
        group.measurement_time(Duration::from_secs(30));

        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            batch_size,
            |b, &batch_size| {
                let orderbook = Arc::new(LockFreeOrderBook::new("BTCUSDT".to_string()));
                let updates = generate_market_data_batch(batch_size, "BTCUSDT");

                b.iter_custom(|iters| {
                    let mut total_duration = Duration::new(0, 0);

                    for _i in 0..iters {
                        for update in &updates {
                            let start = Instant::now();

                            // Critical path: Update orderbook
                            for bid in &update.bids {
                                orderbook
                                    .update_level(Side::Bid, bid.price, bid.quantity)
                                    .unwrap();
                            }
                            for ask in &update.asks {
                                orderbook
                                    .update_level(Side::Ask, ask.price, ask.quantity)
                                    .unwrap();
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

/// Benchmark 2: High-Throughput Orderbook Updates
/// Target: ≥ 100,000 ops/sec
fn bench_high_throughput_updates(c: &mut Criterion) {
    let mut group = c.benchmark_group("high_throughput_updates");

    for ops_target in [50_000, 100_000, 150_000, 200_000].iter() {
        group.throughput(Throughput::Elements(*ops_target as u64));
        group.measurement_time(Duration::from_secs(10));

        group.bench_with_input(
            BenchmarkId::from_parameter(ops_target),
            ops_target,
            |b, &ops_target| {
                let orderbook = Arc::new(LockFreeOrderBook::new("THROUGHPUT_TEST".to_string()));

                b.iter(|| {
                    let start = Instant::now();
                    let mut operations = 0;

                    while operations < ops_target {
                        let price_variation = (operations % 1000) as f64 * 0.01;
                        let base_price = 50000.0;

                        // Simulate bid/ask updates
                        let bid_price = Price::from(base_price - 1.0 - price_variation);
                        let ask_price = Price::from(base_price + 1.0 + price_variation);
                        let quantity = Quantity::try_from(1.0 + price_variation).unwrap();

                        orderbook
                            .update_level(Side::Bid, bid_price, quantity)
                            .unwrap();
                        orderbook
                            .update_level(Side::Ask, ask_price, quantity)
                            .unwrap();

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

/// Benchmark 3: Memory Usage per Trading Pair
/// Target: ≤ 10MB per trading pair
fn bench_memory_usage_per_pair(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_usage_per_pair");

    for depth_levels in [20, 50, 100, 200, 500].iter() {
        group.throughput(Throughput::Elements(*depth_levels as u64));

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

                        orderbook
                            .update_level(Side::Bid, bid_price, quantity)
                            .unwrap();
                        orderbook
                            .update_level(Side::Ask, ask_price, quantity)
                            .unwrap();
                    }

                    // Estimate memory usage
                    let bid_levels = orderbook.stats().bid_levels;
                    let ask_levels = orderbook.stats().ask_levels;
                    let estimated_memory_kb = (bid_levels + ask_levels) * 256 / 1024; // Rough estimate

                    black_box((bid_levels, ask_levels, estimated_memory_kb));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 4: SIMD-Optimized Calculations
fn bench_simd_calculations(c: &mut Criterion) {
    let mut group = c.benchmark_group("simd_calculations");

    for data_size in [100, 1000, 5000, 10000].iter() {
        group.throughput(Throughput::Elements(*data_size as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(data_size),
            data_size,
            |b, &data_size| {
                // Generate test data
                let prices: Vec<f64> = (0..data_size)
                    .map(|i| 50000.0 + (i as f64 * 0.01))
                    .collect();
                let quantities: Vec<f64> = (0..data_size).map(|i| 1.0 + (i as f64 * 0.1)).collect();

                b.iter(|| {
                    // VWAP calculation with SIMD optimization potential
                    let mut total_value = 0.0;
                    let mut total_quantity = 0.0;

                    // Manual vectorization hint
                    for i in (0..data_size).step_by(4) {
                        let end = (i + 4).min(data_size);
                        for j in i..end {
                            total_value += prices[j] * quantities[j];
                            total_quantity += quantities[j];
                        }
                    }

                    let vwap = if total_quantity > 0.0 {
                        total_value / total_quantity
                    } else {
                        0.0
                    };

                    black_box(vwap);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 5: Lock-Free Queue Performance
fn bench_lockfree_queue_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("lockfree_queue_performance");

    for msg_count in [1000, 5000, 10000, 50000].iter() {
        group.throughput(Throughput::Elements(*msg_count as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(msg_count),
            msg_count,
            |b, &msg_count| {
                let (sender, receiver) = unbounded::<OrderBookUpdate>();
                let test_data = generate_market_data_batch(*msg_count, "QUEUE_TEST");

                b.iter(|| {
                    let start = Instant::now();

                    // Producer
                    for update in &test_data {
                        sender.send(update.clone()).unwrap();
                    }

                    // Consumer
                    let mut received = 0;
                    while let Ok(_update) = receiver.try_recv() {
                        received += 1;
                        if received >= *msg_count {
                            break;
                        }
                    }

                    let elapsed = start.elapsed();
                    let throughput = received as f64 / elapsed.as_secs_f64();

                    black_box((received, throughput));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 6: Atomic Operations Performance
fn bench_atomic_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("atomic_operations_performance");

    for op_count in [10000, 100000, 1000000].iter() {
        group.throughput(Throughput::Elements(*op_count as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(op_count),
            op_count,
            |b, &op_count| {
                let counter = AtomicU64::new(0);
                let timestamp = AtomicU64::new(0);
                let sequence = AtomicU64::new(0);

                b.iter(|| {
                    for _i in 0..*op_count {
                        // Simulate typical HFT atomic operations
                        counter.fetch_add(1, Ordering::Relaxed);
                        timestamp.store(
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_micros() as u64,
                            Ordering::Release,
                        );
                        sequence.fetch_add(1, Ordering::AcqRel);
                    }

                    let final_counter = counter.load(Ordering::Relaxed);
                    let final_timestamp = timestamp.load(Ordering::Acquire);
                    let final_sequence = sequence.load(Ordering::Acquire);

                    black_box((final_counter, final_timestamp, final_sequence));
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 7: JSON Parsing Performance
fn bench_json_parsing_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("json_parsing_performance");

    let test_json = r#"{"arg":{"channel":"books15","instId":"BTCUSDT"},"data":[{"asks":[["67189.5","0.25",1],["67189.6","0.30",2]],"bids":[["67188.1","0.12",1],["67188.0","0.18",3]],"ts":"1721810005123","seqId":"12345"}]}"#;

    for msg_count in [100, 500, 1000, 5000].iter() {
        group.throughput(Throughput::Elements(*msg_count as u64));

        group.bench_with_input(
            BenchmarkId::from_parameter(msg_count),
            msg_count,
            |b, &msg_count| {
                let messages = vec![test_json; msg_count];

                b.iter(|| {
                    let mut parsed_count = 0;

                    for message in &messages {
                        // Compare serde_json vs simd-json
                        if let Ok(_value) = serde_json::from_str::<serde_json::Value>(message) {
                            parsed_count += 1;
                        }
                    }

                    black_box(parsed_count);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark 8: End-to-End Latency Test
/// Simulates complete pipeline: WebSocket -> Parse -> OrderBook -> Decision
fn bench_end_to_end_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end_latency");
    group.measurement_time(Duration::from_secs(20));

    group.bench_function("complete_pipeline_latency", |b| {
        let orderbook = Arc::new(LockFreeOrderBook::new("E2E_TEST".to_string()));
        let test_json = r#"{"arg":{"channel":"books15","instId":"BTCUSDT"},"data":[{"asks":[["67189.5","0.25",1]],"bids":[["67188.1","0.12",1]],"ts":"1721810005123","seqId":"12345"}]}"#;
        
        b.iter_custom(|iters| {
            let mut latency_tracker = LatencyMeasurement::new();
            let mut total_duration = Duration::new(0, 0);
            
            for _i in 0..iters {
                let pipeline_start = Instant::now();
                
                // Step 1: JSON Parsing
                let parsed = serde_json::from_str::<serde_json::Value>(test_json).unwrap();
                
                // Step 2: Extract market data
                if let Some(data_array) = parsed["data"].as_array() {
                    for data in data_array {
                        // Step 3: Update orderbook
                        if let Some(bids) = data["bids"].as_array() {
                            for bid in bids {
                                if let (Some(price), Some(qty)) = (bid[0].as_str(), bid[1].as_str()) {
                                    let price_val = price.parse::<f64>().unwrap();
                                    let qty_val = qty.parse::<f64>().unwrap();
                                    
                                    orderbook.update_level(
                                        Side::Bid,
                                        Price::from(price_val),
                                        Quantity::try_from(qty_val).unwrap()
                                    ).unwrap();
                                }
                            }
                        }
                        
                        // Step 4: Trading decision (simulate)
                        let _mid_price = orderbook.mid_price();
                        let _spread = orderbook.spread();
                    }
                }
                
                total_duration += pipeline_start.elapsed();
            }
            
            total_duration
        });
    });

    group.finish();
}

/// Benchmark 9: Stress Test - High Load Simulation
fn bench_stress_test_high_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("stress_test_high_load");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(30));

    group.bench_function("multi_symbol_high_frequency", |b| {
        b.iter(|| {
            let symbols = ["BTCUSDT", "ETHUSDT", "ADAUSDT", "DOTUSDT", "SOLUSDT"];
            let mut orderbooks = Vec::new();

            // Initialize orderbooks
            for symbol in &symbols {
                orderbooks.push(Arc::new(LockFreeOrderBook::new(symbol.to_string())));
            }

            let updates_per_symbol = 2000;
            let start = Instant::now();
            let mut total_operations = 0;

            // Simulate high-frequency updates across multiple symbols
            for symbol_idx in 0..symbols.len() {
                let orderbook = &orderbooks[symbol_idx];
                let updates = generate_market_data_batch(updates_per_symbol, symbols[symbol_idx]);

                for update in updates {
                    // Process each update
                    for bid in update.bids {
                        orderbook
                            .update_level(Side::Bid, bid.price, bid.quantity)
                            .unwrap();
                        total_operations += 1;
                    }
                    for ask in update.asks {
                        orderbook
                            .update_level(Side::Ask, ask.price, ask.quantity)
                            .unwrap();
                        total_operations += 1;
                    }

                    // Query operations
                    black_box(orderbook.best_bid());
                    black_box(orderbook.best_ask());
                    total_operations += 2;
                }
            }

            let elapsed = start.elapsed();
            let throughput = total_operations as f64 / elapsed.as_secs_f64();

            black_box((total_operations, throughput));
        });
    });

    group.finish();
}

criterion_group!(
    performance_benchmarks,
    bench_lockfree_orderbook_critical_path,
    bench_high_throughput_updates,
    bench_memory_usage_per_pair,
    bench_simd_calculations,
    bench_lockfree_queue_performance,
    bench_atomic_operations,
    bench_json_parsing_performance,
    bench_end_to_end_latency,
    bench_stress_test_high_load
);

criterion_main!(performance_benchmarks);
