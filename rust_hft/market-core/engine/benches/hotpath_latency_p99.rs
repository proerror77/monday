//! Steady-state quote-to-intent latency gate.
//!
//! Measures one local market event through aggregation, strategy, lifecycle validation, and the
//! execution SPSC queue. Network and exchange push cadence are intentionally excluded.

use engine::dataflow::IngestionConfig;
use engine::{create_execution_queues, Engine, EngineConfig, ExecutionQueueConfig};
use hft_core::{OrderType, Price, Quantity, Side, Symbol, TimeInForce, VenueId};
use ports::{
    AccountView, BookLevel, ExecutionEvent, MarketEvent, MarketSnapshot, OrderIntent, Strategy,
};
use std::time::Instant;

struct BenchmarkStrategy;

impl Strategy for BenchmarkStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        let MarketEvent::Snapshot(snapshot) = event else {
            return Vec::new();
        };
        vec![OrderIntent {
            symbol: snapshot.symbol.clone(),
            asset_class: hft_core::AssetClass::Crypto,
            product_type: hft_core::ProductType::Spot,
            compliance_context: hft_core::ComplianceContext::default(),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.001).unwrap(),
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: "latency_bench".to_string(),
            target_venue: Some(VenueId::BINANCE),
        }]
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

fn percentile(sorted: &[u64], percentile: f64) -> u64 {
    let index = ((sorted.len() - 1) as f64 * percentile).ceil() as usize;
    sorted[index]
}

#[test]
fn quote_to_worker_queue_p99_stays_below_budget() {
    const WARMUP: u64 = 1_000;
    const SAMPLES: u64 = 20_000;
    const P99_BUDGET_NS: u64 = 500_000;
    const P999_BUDGET_NS: u64 = 1_000_000;

    let mut engine = Engine::new(EngineConfig {
        ingestion: IngestionConfig::high_performance(),
        intent_max_latency_us: 1_000_000,
        max_events_per_cycle: 1,
        aggregation_symbols: vec![],
        ..Default::default()
    });
    engine.register_strategy(BenchmarkStrategy);
    let ingester = engine.create_event_ingester_pair();
    let (engine_queues, mut worker_queues) =
        create_execution_queues(ExecutionQueueConfig::default());
    engine.set_execution_queues(engine_queues);

    let mut run_once = |sequence, engine: &mut Engine| {
        ingester
            .lock()
            .unwrap()
            .ingest(MarketEvent::Snapshot(MarketSnapshot {
                symbol: Symbol::new("BTCUSDT"),
                timestamp: hft_core::now_micros(),
                bids: vec![BookLevel {
                    price: Price::from_f64(50_000.0).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap(),
                }],
                asks: vec![BookLevel {
                    price: Price::from_f64(50_001.0).unwrap(),
                    quantity: Quantity::from_f64(1.0).unwrap(),
                }],
                sequence,
                source_venue: Some(VenueId::BINANCE),
                timestamps: Default::default(),
            }))
            .unwrap();
        engine.tick().unwrap();
        let envelopes = worker_queues.receive_envelopes();
        assert_eq!(envelopes.len(), 1);
    };

    for sequence in 1..=WARMUP {
        run_once(sequence, &mut engine);
    }

    let mut samples = Vec::with_capacity(SAMPLES as usize);
    for sequence in WARMUP + 1..=WARMUP + SAMPLES {
        let started = Instant::now();
        run_once(sequence, &mut engine);
        samples.push(started.elapsed().as_nanos() as u64);
    }
    samples.sort_unstable();
    let p50 = percentile(&samples, 0.50);
    let p99 = percentile(&samples, 0.99);
    let p999 = percentile(&samples, 0.999);
    println!("quote_to_worker_queue p50={p50}ns p99={p99}ns p999={p999}ns");
    assert!(
        p99 <= P99_BUDGET_NS,
        "quote-to-worker p99 {p99}ns exceeds {P99_BUDGET_NS}ns"
    );
    assert!(
        p999 <= P999_BUDGET_NS,
        "quote-to-worker p999 {p999}ns exceeds {P999_BUDGET_NS}ns"
    );
}
