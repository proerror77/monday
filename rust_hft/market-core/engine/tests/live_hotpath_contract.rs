use engine::{
    create_execution_queues, dataflow::EventIngester, Engine, EngineConfig, ExecutionQueueConfig,
};
use hft_core::{
    monotonic_micros, now_micros, AssetClass, ComplianceContext, HftError, LatencyCaptureBoundary,
    LatencyStage, LatencyTracker, OrderType, Price, ProductType, Quantity, Side, Symbol,
    TimeInForce, VenueId, VenueSymbol,
};
use ports::{
    AccountView, ArbitrageOpportunity, BookLevel, BookUpdate, ExecutionEvent, MarketEvent,
    MarketSnapshot, OrderIntent, RiskManager, RiskMetrics, Strategy, StrategyContext, TopOfBook,
    TrackedMarketEvent, Trade, VenueSpec,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use strategy_imbalance::{ImbalanceParams, ImbalanceStrategy};

struct CountingStrategy {
    calls: Arc<AtomicUsize>,
}

struct OneShotOrderStrategy {
    emitted: bool,
}

struct MarketOrderProbeStrategy {
    emitted: bool,
}

impl Strategy for MarketOrderProbeStrategy {
    fn on_market_event(
        &mut self,
        _event: &MarketEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        if self.emitted {
            return Vec::new();
        }
        self.emitted = true;
        vec![OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: AssetClass::Crypto,
            product_type: ProductType::Spot,
            compliance_context: ComplianceContext::default(),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.01).expect("valid quantity"),
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: "market-order-probe".to_string(),
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
        "market-order-probe"
    }
}

struct PriceCaptureRisk {
    captured: Arc<Mutex<Vec<Option<Price>>>>,
}

impl RiskManager for PriceCaptureRisk {
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        _account: &AccountView,
        _venue_specs: &HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent> {
        intents
    }

    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        _account: &AccountView,
        _venue: &VenueSpec,
    ) -> Vec<OrderIntent> {
        intents
    }

    fn review_with_venue_specs(
        &mut self,
        intents: Vec<OrderIntent>,
        _account: &AccountView,
        _venue_specs: &HashMap<VenueId, VenueSpec>,
    ) -> Vec<OrderIntent> {
        self.captured
            .lock()
            .expect("capture lock")
            .extend(intents.iter().map(|intent| intent.price));
        intents
    }

    fn on_execution_event(&mut self, _event: &ExecutionEvent) {}

    fn emergency_stop(&mut self) -> Result<(), HftError> {
        Ok(())
    }

    fn get_risk_metrics(&self) -> HashMap<String, Decimal> {
        HashMap::new()
    }

    fn should_halt_trading(&self, _account: &AccountView) -> bool {
        false
    }

    fn risk_metrics(&self) -> RiskMetrics {
        RiskMetrics {
            max_drawdown: Decimal::ZERO,
            current_drawdown: Decimal::ZERO,
            var_1d: Decimal::ZERO,
            leverage: Decimal::ZERO,
            concentration_risk: Decimal::ZERO,
            order_rate: Decimal::ZERO,
            last_update: 0,
        }
    }
}

struct ArbitrageCaptureStrategy {
    captured: Arc<Mutex<Option<ArbitrageOpportunity>>>,
}

struct ContextSequenceCaptureStrategy {
    captured: Arc<Mutex<Vec<(u64, f64)>>>,
}

impl Strategy for ContextSequenceCaptureStrategy {
    fn on_market_event(
        &mut self,
        _event: &MarketEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn on_market_event_with_context(
        &mut self,
        event: &MarketEvent,
        context: &StrategyContext<'_>,
    ) -> Vec<OrderIntent> {
        if matches!(event, MarketEvent::Update(_)) {
            let book = context.book.expect("update must have synchronized book");
            self.captured
                .lock()
                .expect("capture lock")
                .push((book.sequence, book.bid_prices[0].to_f64()));
        }
        Vec::new()
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        "context-sequence-capture"
    }
}

impl Strategy for ArbitrageCaptureStrategy {
    fn on_market_event(&mut self, event: &MarketEvent, _account: &AccountView) -> Vec<OrderIntent> {
        if let MarketEvent::Arbitrage(opportunity) = event {
            *self.captured.lock().expect("capture lock") = Some(opportunity.clone());
        }
        Vec::new()
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        "arbitrage-capture"
    }
}

impl Strategy for OneShotOrderStrategy {
    fn on_market_event(
        &mut self,
        _event: &MarketEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        if self.emitted {
            return Vec::new();
        }
        self.emitted = true;
        vec![OrderIntent {
            symbol: Symbol::new("BTCUSDT"),
            asset_class: AssetClass::Crypto,
            product_type: ProductType::Spot,
            compliance_context: ComplianceContext::default(),
            side: Side::Buy,
            quantity: Quantity::from_f64(0.01).expect("valid quantity"),
            order_type: OrderType::Limit,
            price: Some(Price::from_f64(100.0).expect("valid price")),
            time_in_force: TimeInForce::IOC,
            strategy_id: String::new(),
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
        "one-shot"
    }
}

impl Strategy for CountingStrategy {
    fn on_market_event(
        &mut self,
        _event: &MarketEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Vec::new()
    }

    fn on_execution_event(
        &mut self,
        _event: &ExecutionEvent,
        _account: &AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }

    fn name(&self) -> &str {
        "counting"
    }
}

fn level(price: f64, quantity: f64) -> BookLevel {
    BookLevel {
        price: Price::from_f64(price).expect("valid price"),
        quantity: Quantity::from_f64(quantity).expect("valid quantity"),
    }
}

#[tokio::test]
async fn receive_latency_cohort_excludes_non_receive_boundaries() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config.clone());
    let (mut ingester, consumer) = EventIngester::new(config.ingestion);
    engine.register_event_consumer(consumer);
    let symbol = Symbol::new("BTCUSDT");
    let adapter_publish = TrackedMarketEvent::new(MarketEvent::Snapshot(MarketSnapshot {
        symbol: symbol.clone(),
        timestamp: now_micros(),
        bids: vec![level(100.0, 1.0)],
        asks: vec![level(101.0, 1.0)],
        sequence: 1,
        source_venue: Some(VenueId::BINANCE),
        timestamps: Default::default(),
    }));
    assert_eq!(
        adapter_publish.tracker.capture_boundary,
        LatencyCaptureBoundary::AdapterPublish
    );
    ingester
        .ingest_tracked_lossless(adapter_publish)
        .await
        .expect("adapter publish accepted");
    engine.tick().expect("adapter publish tick");
    assert!(engine
        .get_latency_stats()
        .get(&LatencyStage::Ingestion)
        .is_none());
    assert!(engine
        .get_latency_stats()
        .get(&LatencyStage::EndToEnd)
        .is_none());

    ingester
        .ingest_tracked_lossless(TrackedMarketEvent::from_snapshot_completion(
            MarketEvent::Snapshot(MarketSnapshot {
                symbol: symbol.clone(),
                timestamp: now_micros(),
                bids: vec![level(100.0, 1.0)],
                asks: vec![level(101.0, 1.0)],
                sequence: 2,
                source_venue: Some(VenueId::BINANCE),
                timestamps: Default::default(),
            }),
        ))
        .await
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");
    assert!(engine
        .get_latency_stats()
        .get(&LatencyStage::Ingestion)
        .is_none());
    assert!(engine
        .get_latency_stats()
        .get(&LatencyStage::EndToEnd)
        .is_none());

    let mut tracker = LatencyTracker::from_userspace_websocket_message_delivery(monotonic_micros());
    tracker.record_stage_with_offset(LatencyStage::WsReceive, 0);
    tracker.record_stage_with_offset(LatencyStage::Parsing, 1);
    ingester
        .ingest_tracked_lossless(TrackedMarketEvent {
            event: MarketEvent::Update(BookUpdate {
                symbol,
                timestamp: now_micros(),
                bids: vec![level(100.0, 2.0)],
                asks: vec![],
                first_sequence: Some(3),
                sequence: 3,
                is_snapshot: false,
                source_venue: Some(VenueId::BINANCE),
                timestamps: Default::default(),
            }),
            tracker,
        })
        .await
        .expect("userspace WebSocket message accepted");
    engine.tick().expect("userspace WebSocket message tick");
    assert_eq!(
        engine
            .get_latency_stats()
            .get(&LatencyStage::Ingestion)
            .expect("userspace ingestion cohort")
            .count,
        1
    );
    assert_eq!(
        engine
            .get_latency_stats()
            .get(&LatencyStage::EndToEnd)
            .expect("userspace end-to-end cohort")
            .count,
        1
    );

    ingester
        .ingest_tracked_lossless(TrackedMarketEvent::new(MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 3.0)],
            asks: vec![],
            first_sequence: Some(4),
            sequence: 4,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        })))
        .await
        .expect("later adapter publish accepted");
    engine.tick().expect("later adapter publish tick");
    assert_eq!(
        engine
            .get_latency_stats()
            .get(&LatencyStage::Ingestion)
            .expect("receive ingestion cohort remains isolated")
            .count,
        1
    );
    assert_eq!(
        engine
            .get_latency_stats()
            .get(&LatencyStage::EndToEnd)
            .expect("receive end-to-end cohort remains isolated")
            .count,
        1
    );
}

#[test]
fn live_delta_updates_market_view_and_runs_strategy() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    config.top_n = 2;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let calls = Arc::new(AtomicUsize::new(0));
    engine.register_strategy(CountingStrategy {
        calls: Arc::clone(&calls),
    });

    let symbol = Symbol::new("BTCUSDT");
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 2.0), level(99.0, 1.0), level(98.0, 1.0)],
            asks: vec![level(101.0, 2.0), level(102.0, 1.0), level(103.0, 1.0)],
            sequence: 100,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    let first = engine.tick().expect("snapshot tick");
    assert!(first.snapshot_published);
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 0.0), level(100.5, 3.0)],
            asks: vec![level(101.0, 0.0), level(100.75, 4.0)],
            first_sequence: Some(101),
            sequence: 101,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("delta accepted");
    let second = engine.tick().expect("delta tick");

    assert!(
        second.snapshot_published,
        "book delta must publish a new view"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "strategy must observe the delta event in the same tick"
    );

    let key = VenueSymbol::new(VenueId::BINANCE, symbol);
    let view = engine.get_market_view();
    let book = view.get_orderbook(&key).expect("Binance book exists");
    assert_eq!(book.sequence, 101);
    assert_eq!(book.bid_prices.len(), 2);
    assert_eq!(book.ask_prices.len(), 2);
    assert_eq!(
        view.get_best_bid_for_venue(&key).expect("best bid").0,
        Price::from_f64(100.5).expect("valid price")
    );
    assert_eq!(
        view.get_best_ask_for_venue(&key).expect("best ask").0,
        Price::from_f64(100.75).expect("valid price")
    );

    drop(view);
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: key.symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.6, 1.0)],
            asks: vec![],
            first_sequence: Some(103),
            sequence: 103,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("gap event accepted");
    let gap = engine.tick().expect("gap tick");
    assert!(gap.snapshot_published);
    assert!(
        engine.get_market_view().get_orderbook(&key).is_none(),
        "a sequence gap must invalidate the venue book"
    );
}

#[test]
fn realtime_quote_overlays_bbo_without_destroying_deeper_l2() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    config.top_n = 3;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let calls = Arc::new(AtomicUsize::new(0));
    engine.register_strategy(CountingStrategy {
        calls: Arc::clone(&calls),
    });

    let symbol = Symbol::new("BTCUSDT");
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(101.0, 1.0), level(100.0, 2.0), level(99.0, 3.0)],
            asks: vec![level(102.0, 1.0), level(103.0, 2.0), level(104.0, 3.0)],
            sequence: 100,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Quote(TopOfBook {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            sequence: 105,
            bid: level(100.0, 5.0),
            ask: level(103.0, 6.0),
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("quote accepted");
    let tick = engine.tick().expect("quote tick");

    assert!(tick.snapshot_published);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    let key = VenueSymbol::new(VenueId::BINANCE, symbol);
    let view = engine.get_market_view();
    let book = view.get_orderbook(&key).expect("Binance book exists");
    assert_eq!(book.sequence, 105);
    assert_eq!(
        book.bid_prices.as_slice(),
        &[
            hft_core::FixedPrice::from_f64(100.0),
            hft_core::FixedPrice::from_f64(99.0),
        ]
    );
    assert_eq!(
        book.ask_prices.as_slice(),
        &[
            hft_core::FixedPrice::from_f64(103.0),
            hft_core::FixedPrice::from_f64(104.0),
        ]
    );
    assert_eq!(
        book.bid_quantities[0],
        hft_core::FixedQuantity::from_f64(5.0)
    );
    assert_eq!(
        book.ask_quantities[0],
        hft_core::FixedQuantity::from_f64(6.0)
    );
}

#[test]
fn l2_strategy_context_excludes_a_newer_quote_overlay() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let captured = Arc::new(Mutex::new(Vec::new()));
    engine.register_strategy(ContextSequenceCaptureStrategy {
        captured: Arc::clone(&captured),
    });
    let symbol = Symbol::new("BTCUSDT");

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0)],
            sequence: 20,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Quote(TopOfBook {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            sequence: 25,
            bid: level(99.0, 10.0),
            ask: level(102.0, 10.0),
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("quote accepted");
    engine.tick().expect("quote tick");
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Update(BookUpdate {
            symbol,
            timestamp: now_micros(),
            bids: vec![level(100.0, 2.0)],
            asks: Vec::new(),
            first_sequence: Some(21),
            sequence: 21,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("delta accepted");
    engine.tick().expect("delta tick");

    assert_eq!(*captured.lock().expect("capture lock"), vec![(21, 100.0)]);
}

#[test]
fn realtime_quote_drives_production_imbalance_strategy_with_quote_sequence() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    config.top_n = 2;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let (engine_queues, mut worker_queues) =
        create_execution_queues(ExecutionQueueConfig::default());
    engine.set_execution_queues(engine_queues);
    engine.register_strategy(ImbalanceStrategy::with_name(
        Symbol::new("BTCUSDT"),
        Some(ImbalanceParams {
            obi_threshold: 0.5,
            lot: 0.01,
            top_levels: 1,
        }),
        "quote-obi".to_string(),
    ));

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0), level(99.0, 1.0)],
            asks: vec![level(101.0, 1.0), level(102.0, 1.0)],
            sequence: 20,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");
    assert!(worker_queues.receive_envelopes().is_empty());

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Quote(TopOfBook {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            sequence: 25,
            bid: level(100.0, 10.0),
            ask: level(101.0, 1.0),
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("quote accepted");
    engine.tick().expect("quote tick");

    let envelopes = worker_queues.receive_envelopes();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].intent.side, Side::Buy);
    assert_eq!(envelopes[0].intent.price, Some(level(101.0, 1.0).price));
    assert_eq!(envelopes[0].lifecycle.source_book_seq, Some(25));
}

#[test]
fn crossed_realtime_quote_invalidates_the_venue_book() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let symbol = Symbol::new("BTCUSDT");
    let key = VenueSymbol::new(VenueId::BINANCE, symbol.clone());

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");
    assert!(engine.get_market_view().get_orderbook(&key).is_some());

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Quote(TopOfBook {
            symbol,
            timestamp: now_micros(),
            sequence: 2,
            bid: level(102.0, 1.0),
            ask: level(101.0, 1.0),
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("quote accepted");
    engine.tick().expect("quote tick");

    assert!(engine.get_market_view().get_orderbook(&key).is_none());
}

#[test]
fn rebuilt_lob_delta_drives_production_imbalance_strategy() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    config.top_n = 2;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let (engine_queues, mut worker_queues) =
        create_execution_queues(ExecutionQueueConfig::default());
    engine.set_execution_queues(engine_queues);
    engine.register_strategy(ImbalanceStrategy::with_name(
        Symbol::new("BTCUSDT"),
        Some(ImbalanceParams {
            obi_threshold: 0.5,
            lot: 0.01,
            top_levels: 1,
        }),
        "production-obi".to_string(),
    ));

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 2.0), level(99.0, 1.0)],
            asks: vec![level(101.0, 2.0), level(102.0, 1.0)],
            sequence: 10,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");
    assert!(
        worker_queues.receive_envelopes().is_empty(),
        "balanced snapshot must not trigger the OBI strategy"
    );

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 10.0)],
            asks: Vec::new(),
            first_sequence: Some(11),
            sequence: 11,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("delta accepted");
    engine.tick().expect("delta tick");

    let envelopes = worker_queues.receive_envelopes();
    assert_eq!(
        envelopes.len(),
        1,
        "a bid-heavy rebuilt LOB must trigger the production OBI strategy"
    );
    assert_eq!(envelopes[0].intent.side, Side::Buy);
    assert_eq!(envelopes[0].intent.target_venue, Some(VenueId::BINANCE));
    assert_eq!(envelopes[0].lifecycle.source_book_seq, Some(11));
}

#[test]
fn cross_venue_delta_recomputes_and_publishes_arbitrage_opportunity() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let captured = Arc::new(Mutex::new(None));
    engine.register_strategy(ArbitrageCaptureStrategy {
        captured: Arc::clone(&captured),
    });
    let symbol = Symbol::new("BTCUSDT");

    for (venue, sequence) in [(VenueId::BINANCE, 1), (VenueId::BYBIT, 10)] {
        let bid = if venue == VenueId::BYBIT {
            100.5
        } else {
            100.0
        };
        ingester
            .lock()
            .expect("ingester lock")
            .ingest(MarketEvent::Snapshot(MarketSnapshot {
                symbol: symbol.clone(),
                timestamp: now_micros(),
                bids: vec![level(bid, 2.0)],
                asks: vec![level(101.0, 2.0)],
                sequence,
                source_venue: Some(venue),
                timestamps: Default::default(),
            }))
            .expect("snapshot accepted");
        engine.tick().expect("snapshot tick");
    }
    assert!(captured.lock().expect("capture lock").is_none());

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: Vec::new(),
            asks: vec![level(101.0, 0.0), level(99.0, 1.0)],
            first_sequence: Some(2),
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("delta accepted");
    engine.tick().expect("delta tick");

    let opportunity = captured
        .lock()
        .expect("capture lock")
        .clone()
        .expect("delta must publish an arbitrage opportunity");
    assert_eq!(opportunity.symbol, symbol);
    assert_eq!(opportunity.ask_venue, VenueId::BINANCE);
    assert_eq!(opportunity.bid_venue, VenueId::BYBIT);
}

#[test]
fn deleting_top_level_refills_published_top_n_from_canonical_depth() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    config.top_n = 2;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let symbol = Symbol::new("BTCUSDT");
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0), level(99.0, 2.0), level(98.0, 3.0)],
            asks: vec![level(101.0, 1.0), level(102.0, 2.0), level(103.0, 3.0)],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 0.0)],
            asks: Vec::new(),
            first_sequence: Some(2),
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("delta accepted");
    engine.tick().expect("delta tick");

    let key = VenueSymbol::new(VenueId::BINANCE, symbol);
    let view = engine.get_market_view();
    let book = view.get_orderbook(&key).expect("book remains synchronized");
    assert_eq!(book.bid_prices.len(), 2);
    assert_eq!(book.bid_prices[0].to_f64(), 99.0);
    assert_eq!(book.bid_prices[1].to_f64(), 98.0);
}

#[test]
fn batched_deltas_keep_the_lob_state_from_their_own_sequence() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let captured = Arc::new(Mutex::new(Vec::new()));
    engine.register_strategy(ContextSequenceCaptureStrategy {
        captured: Arc::clone(&captured),
    });
    let symbol = Symbol::new("BTCUSDT");
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: symbol.clone(),
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");

    let mut guard = ingester.lock().expect("ingester lock");
    for (sequence, price) in [(2, 100.5), (3, 100.75)] {
        guard
            .ingest(MarketEvent::Update(BookUpdate {
                symbol: symbol.clone(),
                timestamp: now_micros(),
                bids: vec![level(price, 1.0)],
                asks: Vec::new(),
                first_sequence: Some(sequence),
                sequence,
                is_snapshot: false,
                source_venue: Some(VenueId::BINANCE),
                timestamps: Default::default(),
            }))
            .expect("delta accepted");
    }
    drop(guard);
    engine.tick().expect("batched delta tick");

    assert_eq!(
        *captured.lock().expect("capture lock"),
        vec![(2, 100.5), (3, 100.75)]
    );
}

#[test]
fn market_order_is_priced_from_event_lob_before_risk_review() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let (engine_queues, mut worker_queues) =
        create_execution_queues(ExecutionQueueConfig::default());
    engine.set_execution_queues(engine_queues);
    engine.register_strategy(MarketOrderProbeStrategy { emitted: false });
    let captured = Arc::new(Mutex::new(Vec::new()));
    engine.register_risk_manager(PriceCaptureRisk {
        captured: Arc::clone(&captured),
    });

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");

    assert_eq!(
        *captured.lock().expect("capture lock"),
        vec![Some(Price::from_f64(101.0).unwrap())]
    );
    let envelopes = worker_queues.receive_envelopes();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(
        envelopes[0].intent.price,
        Some(Price::from_f64(101.0).unwrap())
    );
}

#[test]
fn venue_disconnect_invalidates_stale_books_until_a_fresh_snapshot() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let symbol = Symbol::new("BTCUSDT");
    let key = VenueSymbol::new(VenueId::BYBIT, symbol.clone());
    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol,
            timestamp: now_micros(),
            bids: vec![level(100.0, 1.0)],
            asks: vec![level(101.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BYBIT),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("snapshot tick");
    assert!(engine.get_market_view().get_orderbook(&key).is_some());

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Disconnect {
            reason: "test disconnect".to_string(),
            source_venue: Some(VenueId::BYBIT),
            symbol: None,
        })
        .expect("disconnect accepted");
    engine.tick().expect("disconnect tick");

    assert!(engine.get_market_view().get_orderbook(&key).is_none());
}

#[test]
fn trade_event_runs_strategy_without_waiting_for_a_book_snapshot() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let calls = Arc::new(AtomicUsize::new(0));
    engine.register_strategy(CountingStrategy {
        calls: Arc::clone(&calls),
    });

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Trade(Trade {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(0.1).unwrap(),
            side: Side::Buy,
            trade_id: "trade-1".to_string(),
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("trade accepted");
    let tick = engine.tick().expect("trade tick");
    assert!(!tick.snapshot_published);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn strategy_order_reaches_worker_with_lifecycle_and_idempotency_key() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let (engine_queues, mut worker_queues) =
        create_execution_queues(ExecutionQueueConfig::default());
    engine.set_execution_queues(engine_queues);
    engine.register_strategy(OneShotOrderStrategy { emitted: false });

    ingester
        .lock()
        .expect("ingester lock")
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 2.0)],
            asks: vec![level(101.0, 2.0)],
            sequence: 700,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    engine.tick().expect("strategy tick");

    let envelopes = worker_queues.receive_envelopes();
    assert_eq!(envelopes.len(), 1);
    let envelope = &envelopes[0];
    assert!(!envelope.client_order_id.is_empty());
    assert_eq!(envelope.lifecycle.source_book_seq, Some(700));
    assert_eq!(envelope.lifecycle.max_latency_us, Some(3_000));
    assert!(envelope.lifecycle.valid_until > envelope.lifecycle.created_ts);
    assert_eq!(envelope.intent.strategy_id, "one-shot");
}

#[test]
fn intent_from_an_older_event_in_the_same_batch_is_rejected() {
    let mut config = EngineConfig::default();
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let ingester = engine.create_event_ingester_pair();
    let (engine_queues, mut worker_queues) =
        create_execution_queues(ExecutionQueueConfig::default());
    engine.set_execution_queues(engine_queues);
    engine.register_strategy(OneShotOrderStrategy { emitted: false });

    let mut ingester = ingester.lock().expect("ingester lock");
    ingester
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 2.0)],
            asks: vec![level(101.0, 2.0)],
            sequence: 700,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("snapshot accepted");
    ingester
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 3.0)],
            asks: vec![],
            first_sequence: Some(701),
            sequence: 701,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .expect("delta accepted");
    drop(ingester);

    engine.tick().expect("strategy tick");

    assert!(worker_queues.receive_envelopes().is_empty());
}

#[test]
fn market_consumers_rotate_without_exceeding_the_global_tick_budget() {
    let mut config = EngineConfig::default();
    config.max_events_per_cycle = 1;
    config.ingestion.stale_threshold_us = 1_000_000;
    let mut engine = Engine::new(config);
    let first = engine.create_event_ingester_pair();
    let second = engine.create_event_ingester_pair();

    let snapshot = |symbol: &str, sequence| {
        MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new(symbol),
            timestamp: now_micros(),
            bids: vec![level(100.0, 2.0)],
            asks: vec![level(101.0, 2.0)],
            sequence,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        })
    };
    first
        .lock()
        .unwrap()
        .ingest(snapshot("BTCUSDT", 1))
        .unwrap();
    first
        .lock()
        .unwrap()
        .ingest(MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: now_micros(),
            bids: vec![level(100.0, 3.0)],
            asks: vec![],
            first_sequence: Some(2),
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
        }))
        .unwrap();
    second
        .lock()
        .unwrap()
        .ingest(snapshot("ETHUSDT", 1))
        .unwrap();

    assert_eq!(engine.tick().unwrap().events_processed, 1);
    assert_eq!(engine.tick().unwrap().events_processed, 1);

    let view = engine.get_market_view();
    assert!(view
        .get_orderbook(&hft_core::VenueSymbol::new(
            VenueId::BINANCE,
            Symbol::new("ETHUSDT"),
        ))
        .is_some());
}
