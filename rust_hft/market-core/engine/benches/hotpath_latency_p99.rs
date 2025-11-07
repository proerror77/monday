//! Micro-benchmark for hot path latency (ingestion -> aggregation -> strategy -> risk -> enqueue)
//! This is a synthetic benchmark to validate local processing P99 within budget.
//!
//! Updated for Phase 2: includes sharding-aware testing and multi-venue scenarios
//!
//! Run with:
//!   - cargo test -p hft-engine --bench hotpath_latency_p99 -- --nocapture
//!   - or cargo bench -p hft-engine --bench hotpath_latency_p99

#![allow(unused)]

use engine::dataflow::IngestionConfig;
use engine::{Engine, EngineConfig};
use hft_core::{BaseSymbol, Price, Quantity, Symbol, VenueId};
use hft_core::{OrderType, Side, TimeInForce};
use ports::{
    AccountView, BookLevel, ExecutionEvent, MarketEvent, MarketSnapshot, OrderIntent, Strategy,
};
use std::collections::HashMap;
// no external logging needed for this bench

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
            _ => Vec::new(),
        }
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        "latency_bench"
    }
}

#[test]
fn bench_hotpath_latency() {
    println!("开始热路径延迟基准测试");

    // Build engine with small queue and adaptive backpressure
    let mut engine = Engine::new(EngineConfig {
        ingestion: IngestionConfig::high_performance(),
        max_events_per_cycle: 1024,
        aggregation_symbols: vec![],
        ..Default::default()
    });

    // Register strategy
    engine.register_strategy(LatencyBenchmarkStrategy::new());

    // Create local ingester pair
    let ingester = engine.create_event_ingester_pair();

    // Prepare N synthetic snapshots across venues
    let n = 50_000u32;
    {
        let mut guard = ingester.lock().unwrap();
        for i in 0..n {
            let venue = if i % 2 == 0 {
                VenueId::BINANCE
            } else {
                VenueId::BITGET
            };
            let sym = if venue == VenueId::BINANCE {
                Symbol::new("BINANCE:BTCUSDT")
            } else {
                Symbol::new("BITGET:BTCUSDT")
            };
            let snap = MarketSnapshot {
                symbol: sym,
                timestamp: hft_core::now_micros(),
                bids: vec![BookLevel::new_unchecked(50000.0, 1.0)],
                asks: vec![BookLevel::new_unchecked(50001.0, 1.0)],
                sequence: i as u64,
                source_venue: Some(venue),
            };
            guard.ingest(MarketEvent::Snapshot(snap)).unwrap();
        }
    }

    // Drain engine until queues empty
    let start = std::time::Instant::now();
    let mut idle_spins = 0u64;
    while idle_spins < 2000 {
        let tick = engine.tick();
        if let Ok(res) = &tick {
            if res.events_total == 0
                && res.execution_events_processed == 0
                && res.orders_generated == 0
            {
                idle_spins += 1;
            } else {
                idle_spins = 0;
            }
        }
    }
    let elapsed = start.elapsed();

    // Fetch latency stats and print to stdout for manual inspection
    let stats = engine.get_latency_stats();
    if let Some(e2e) = stats.get(&hft_core::LatencyStage::EndToEnd) {
        println!(
            "Hotpath EndToEnd: count={} p50={}us p95={}us p99={}us",
            e2e.count, e2e.p50_micros, e2e.p95_micros, e2e.p99_micros
        );
    }
    if let Some(strategy) = stats.get(&hft_core::LatencyStage::Strategy) {
        println!(
            "Strategy: count={} p50={}us p95={}us p99={}us",
            strategy.count, strategy.p50_micros, strategy.p95_micros, strategy.p99_micros
        );
    }
    println!("Elapsed: {:.3}s", elapsed.as_secs_f64());
}
