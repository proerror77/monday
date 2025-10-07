//! Micro-benchmark for hot path latency (ingestion -> aggregation -> strategy -> risk -> enqueue)
//! This is a synthetic benchmark to validate local processing P99 within budget.
//!
//! Updated for Phase 2: includes sharding-aware testing and multi-venue scenarios
//!
//! Run with: cargo test --bench hotpath_latency_p99 --release

#![allow(unused)]

use hft_core::{Price, Quantity, Symbol, VenueId, BaseSymbol};
use engine::{Engine, EngineConfig};
use engine::dataflow::IngestionConfig;
use ports::{MarketEvent, MarketSnapshot, BookLevel, Strategy, AccountView, OrderIntent, OrderType, TimeInForce, Side, ExecutionEvent};
use std::collections::HashMap;
use tracing::{info, debug};

/// Minimal strategy for latency benchmarking
struct LatencyBenchmarkStrategy {
    processed_events: u64,
    venue_counts: HashMap<VenueId, u64>,
}

impl LatencyBenchmarkStrategy {
    fn new() -> Self {
        Self {
            processed_events: 0,
            venue_counts: HashMap::new(),
        }
    }

    fn get_venue_counts(&self) -> &HashMap<VenueId, u64> {
        &self.venue_counts
    }
}

impl Strategy for LatencyBenchmarkStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        self.processed_events += 1;

        match event {
            MarketEvent::Snapshot(snapshot) => {
                // Count events per venue for sharding validation
                if let Some(venue) = &snapshot.source_venue {
                    *self.venue_counts.entry(*venue).or_insert(0) += 1;
                }

                // Generate minimal order intent to exercise full pipeline
                vec![OrderIntent {
                    symbol: snapshot.symbol.clone(),
                    side: Side::Buy,
                    quantity: Quantity::from_f64(0.001).unwrap(),
                    order_type: OrderType::Market,
                    price: None,
                    time_in_force: TimeInForce::IOC,
                    strategy_id: "latency_bench".to_string(),
                    target_venue: snapshot.source_venue, // Route back to source venue
                }]
            }
            _ => Vec::new()
        }
    }

    fn on_execution_event(&mut self, _event: &ExecutionEvent, _account: &AccountView) -> Vec<OrderIntent> {
        Vec::new()
    }
}

#[test]
fn bench_hotpath_latency() {
    tracing_subscriber::fmt::init();

    info!("开始热路径延迟基准测试");

    // Build engine with small queue and adaptive backpressure
    let mut engine = Engine::new(EngineConfig {
        ingestion: IngestionConfig::high_performance(),
        max_events_per_cycle: 1024,
        aggregation_symbols: vec![],
        ..Default::default()
    });

    // Register enhanced strategy
    let strategy = LatencyBenchmarkStrategy::new();
    engine.register_strategy(strategy);

    // Create local ingester pair
    let ingester = engine.create_event_ingester_pair();

    // Prepare N synthetic snapshots with multi-venue data
    let n = 50_000u32;
    let venues = vec![VenueId::BINANCE, VenueId::BITGET];
    let symbols = vec!["BTCUSDT", "ETHUSDT"];

    info!("生成 {} 个测试快照", n);
    {
        let mut guard = ingester.lock().unwrap();
        for i in 0..n {
            let venue_idx = i as usize % venues.len();
            let symbol_idx = i as usize % symbols.len();
            let venue = venues[venue_idx];
            let symbol_name = symbols[symbol_idx];

            let snap = MarketSnapshot {
                symbol: Symbol(format!("{}:{}", venue.as_str(), symbol_name)),
                timestamp: hft_core::now_micros(),
                bids: vec![BookLevel {
                    price: Price::from_f64(50000.0 + i as f64 * 0.01).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap()
                }],
                asks: vec![BookLevel {
                    price: Price::from_f64(50001.0 + i as f64 * 0.01).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap()
                }],
                sequence: i as u64,
                source_venue: Some(venue),
            };
            guard.ingest(MarketEvent::Snapshot(snap)).unwrap();
        }
    }

    // Drain engine until queues empty
    info!("开始处理快照");
    let start = std::time::Instant::now();
    let mut spins = 0u64;
    let mut processed_ticks = 0u64;

    while spins < 10_000 {
        match engine.tick() {
            Ok(result) => {
                if result.events_processed > 0 {
                    processed_ticks += 1;
                    spins = 0; // Reset spin count when processing events
                } else {
                    spins += 1;
                }

                // Print progress every 1000 ticks
                if processed_ticks > 0 && processed_ticks % 1000 == 0 {
                    debug!("已处理 {} 个tick", processed_ticks);
                }
            }
            Err(e) => {
                println!("引擎错误: {}", e);
                break;
            }
        }
    }

    let elapsed = start.elapsed();
    info!("处理完成，耗时: {:?}, 处理了 {} 个tick", elapsed, processed_ticks);

    // Fetch latency stats and print comprehensive results
    let stats = engine.get_latency_stats();

    println!("\n=== 热路径延迟基准测试结果 ===");
    println!("总处理时间: {:?}", elapsed);
    println!("处理的tick数: {}", processed_ticks);

    if let Some(e2e) = stats.get(&hft_core::LatencyStage::EndToEnd) {
        println!("端到端延迟: count={} p50={}μs p95={}μs p99={}μs max={}μs",
                e2e.count, e2e.p50_micros, e2e.p95_micros, e2e.p99_micros, e2e.max_micros);
    }

    if let Some(ingestion) = stats.get(&hft_core::LatencyStage::Ingestion) {
        println!("数据接入延迟: count={} p50={}μs p95={}μs p99={}μs",
                ingestion.count, ingestion.p50_micros, ingestion.p95_micros, ingestion.p99_micros);
    }

    if let Some(aggregation) = stats.get(&hft_core::LatencyStage::Aggregation) {
        println!("聚合阶段延迟: count={} p50={}μs p95={}μs p99={}μs",
                aggregation.count, aggregation.p50_micros, aggregation.p95_micros, aggregation.p99_micros);
    }

    if let Some(strategy) = stats.get(&hft_core::LatencyStage::Strategy) {
        println!("策略阶段延迟: count={} p50={}μs p95={}μs p99={}μs",
                strategy.count, strategy.p50_micros, strategy.p95_micros, strategy.p99_micros);
    }

    if let Some(execution) = stats.get(&hft_core::LatencyStage::Execution) {
        println!("执行阶段延迟: count={} p50={}μs p95={}μs p99={}μs",
                execution.count, execution.p50_micros, execution.p95_micros, execution.p99_micros);
    }

    // Validate P99 meets target (adjust threshold as needed)
    if let Some(e2e) = stats.get(&hft_core::LatencyStage::EndToEnd) {
        const TARGET_P99_MICROS: u64 = 100; // Target: P99 < 100μs
        assert!(e2e.p99_micros < TARGET_P99_MICROS,
               "P99延迟超过目标: {}μs > {}μs", e2e.p99_micros, TARGET_P99_MICROS);
        println!("✓ P99延迟达标: {}μs < {}μs", e2e.p99_micros, TARGET_P99_MICROS);
    }

    println!("=== 基准测试完成 ===\n");
}

/// Test multi-venue latency with venue-specific routing
#[test]
fn bench_multi_venue_latency() {
    tracing_subscriber::fmt::init();

    info!("开始多交易所延迟基准测试");

    let mut engine = Engine::new(EngineConfig {
        ingestion: IngestionConfig::high_performance(),
        max_events_per_cycle: 2048, // Higher capacity for multi-venue
        aggregation_symbols: vec![],
        ..Default::default()
    });

    let strategy = LatencyBenchmarkStrategy::new();
    engine.register_strategy(strategy);
    let ingester = engine.create_event_ingester_pair();

    // Generate interleaved venue snapshots
    let n_per_venue = 10_000u32;
    let venues = vec![VenueId::BINANCE, VenueId::BITGET];

    {
        let mut guard = ingester.lock().unwrap();
        for i in 0..(n_per_venue * venues.len() as u32) {
            let venue_idx = i as usize % venues.len();
            let venue = venues[venue_idx];

            let snap = MarketSnapshot {
                symbol: Symbol(format!("{}:BTCUSDT", venue.as_str())),
                timestamp: hft_core::now_micros(),
                bids: vec![BookLevel {
                    price: Price::from_f64(50000.0 + i as f64 * 0.01).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap()
                }],
                asks: vec![BookLevel {
                    price: Price::from_f64(50001.0 + i as f64 * 0.01).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap()
                }],
                sequence: i as u64,
                source_venue: Some(venue),
            };
            guard.ingest(MarketEvent::Snapshot(snap)).unwrap();
        }
    }

    // Process all events
    let start = std::time::Instant::now();
    let mut spins = 0u64;
    let mut total_events = 0u32;

    while spins < 5_000 {
        match engine.tick() {
            Ok(result) => {
                total_events += result.events_processed;
                if result.events_processed > 0 {
                    spins = 0;
                } else {
                    spins += 1;
                }
            }
            Err(e) => {
                println!("引擎错误: {}", e);
                break;
            }
        }
    }

    let elapsed = start.elapsed();
    let throughput = total_events as f64 / elapsed.as_secs_f64();

    println!("\n=== 多交易所延迟基准测试结果 ===");
    println!("总处理时间: {:?}", elapsed);
    println!("处理的事件数: {}", total_events);
    println!("吞吐量: {:.0} events/sec", throughput);

    // Print venue-specific latency if available
    let stats = engine.get_latency_stats();
    if let Some(e2e) = stats.get(&hft_core::LatencyStage::EndToEnd) {
        println!("多交易所端到端延迟: p50={}μs p95={}μs p99={}μs",
                e2e.p50_micros, e2e.p95_micros, e2e.p99_micros);

        // Verify throughput meets minimum target
        const MIN_THROUGHPUT: f64 = 10_000.0; // 10k events/sec minimum
        assert!(throughput > MIN_THROUGHPUT,
               "吞吐量不足: {:.0} < {:.0} events/sec", throughput, MIN_THROUGHPUT);
        println!("✓ 吞吐量达标: {:.0} > {:.0} events/sec", throughput, MIN_THROUGHPUT);
    }

    println!("=== 多交易所基准测试完成 ===\n");
}

