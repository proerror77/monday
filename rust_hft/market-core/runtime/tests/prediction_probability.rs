#![cfg(feature = "strategy-probability-reversal")]
use hft_core::{
    now_micros, AssetClass, LocalReceiveTimestamp, MarketDataTimestamps, Price, Quantity, Symbol,
    VenueId,
};
use hft_research_manifest::prediction_probability::{
    BinaryEpisodeV1, ProbabilityReversalSpecV1, PROBABILITY_REVERSAL_SCHEMA,
};
use ports::{BookLevel, MarketEvent, MarketSnapshot, ProviderBookIdentity};
use runtime::{
    StrategyConfig, StrategyParams, StrategyRiskLimits, StrategyType, SystemBuilder, SystemConfig,
};
use rust_decimal::Decimal;

fn spec(now: u64) -> ProbabilityReversalSpecV1 {
    ProbabilityReversalSpecV1 {
        schema: PROBABILITY_REVERSAL_SCHEMA.into(),
        episodes: vec![BinaryEpisodeV1 {
            episode_id: "paper-episode".into(),
            condition_id: "paper-condition".into(),
            underlying: "BTCUSDT".into(),
            venue: "POLYMARKET".into(),
            up_token: "123".into(),
            down_token: "456".into(),
            start_us: now - 1_000_000,
            end_us: now + 3_000_000,
        }],
        prev_prob_low: 0.3,
        curr_prob_high: 0.6,
        prev_prob_high: 0.7,
        curr_prob_low: 0.4,
        take_profit_prob: 0.85,
        stop_loss_prob: 0.5,
        min_time_remaining_secs: 1,
        max_time_remaining_secs: 5,
        stake_usd: Decimal::from(10),
        max_positions: 1000,
        max_daily_trades: 1000,
        quote_max_age_us: 500_000,
    }
}
fn snapshot(ask: i64, now: u64, sequence: u64) -> MarketEvent {
    outcome_snapshot("123", ask, now, sequence)
}
fn outcome_snapshot(token: &str, ask: i64, now: u64, sequence: u64) -> MarketEvent {
    MarketEvent::Snapshot(MarketSnapshot {
        symbol: Symbol::new(token),
        timestamp: now,
        bids: vec![BookLevel {
            price: Price(Decimal::new(ask - 2, 2)),
            quantity: Quantity(Decimal::from(100)),
        }],
        asks: vec![BookLevel {
            price: Price(Decimal::new(ask, 2)),
            quantity: Quantity(Decimal::from(100)),
        }],
        sequence,
        source_venue: Some(VenueId::POLYMARKET),
        timestamps: MarketDataTimestamps::local_only(LocalReceiveTimestamp::new(now)),
        provider_identity: Some(ProviderBookIdentity {
            market: "paper-condition".into(),
            book_hash: None,
        }),
    })
}
#[tokio::test]
async fn configured_probability_runs_through_shared_engine_risk_oms_and_queue() {
    let now = now_micros();
    let fixed = spec(now);
    let end = fixed.episodes[0].end_us;
    let mut config = SystemConfig::default();
    config.engine.intent_max_latency_us = 10_000_000;
    config.engine.intent_max_order_notional = Some(Decimal::from(5));
    config.engine.intent_max_order_quantity = Some(Decimal::from(8));
    config.risk.global_position_limit = Decimal::from(100);
    config.risk.global_notional_limit = Decimal::from(1000);
    config.risk.max_daily_trades = 100;
    config.risk.max_orders_per_second = 100;
    config.risk.staleness_threshold_us = 10_000_000;
    config.strategies = vec![StrategyConfig {
        name: "probability".into(),
        strategy_type: StrategyType::ProbabilityReversal,
        symbols: vec![Symbol::new("123"), Symbol::new("456")],
        params: StrategyParams::ProbabilityReversal {
            spec: Box::new(fixed),
            max_order_notional: Decimal::from(10),
            max_order_quantity: Decimal::from(100),
        },
        risk_limits: StrategyRiskLimits {
            max_notional: Decimal::from(100),
            max_position: Decimal::from(100),
            daily_loss_limit: Decimal::from(100),
            cooldown_ms: 0,
        },
    }];
    let runtime = SystemBuilder::new(config)
        .register_strategies_from_config_strict()
        .unwrap()
        .register_simulated_execution_client(VenueId::POLYMARKET)
        .build();
    let (queues, mut reader) =
        engine::create_execution_queues(engine::ExecutionQueueConfig::default());
    let mut engine = runtime.engine.lock().await;
    engine.set_execution_queues(queues);
    engine.update_cash_balance(Decimal::from(1000)).unwrap();
    let ingester = engine.create_event_ingester_pair();
    // No execution worker is started. This proves the shared queue boundary.
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 2))
        .unwrap();
    engine.tick().unwrap();
    let intents = reader.receive_envelopes();
    // The per-order runtime ceiling rejects this $10 proposal rather than
    // allowing a strategy configuration to enlarge the verified $5 ceiling.
    assert!(intents.is_empty());
    assert!(engine.export_oms_state().is_empty());
    engine
        .set_intent_execution_limits(None, Some(Decimal::from(10)), Some(Decimal::from(100)))
        .unwrap();
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 3))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 4))
        .unwrap();
    engine.tick().unwrap();
    let recovered = reader.receive_envelopes();
    assert_eq!(
        recovered.len(),
        1,
        "known pre-risk rejection must release the unsubmitted proposal"
    );
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 5))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 6))
        .unwrap();
    engine.tick().unwrap();
    assert!(
        reader.receive_envelopes().is_empty(),
        "actual queue acceptance remains Unknown without a correlated report"
    );
    drop(engine);
    let mut config = runtime.config.clone();
    config.engine.intent_max_order_notional = Some(Decimal::from(10));
    config.engine.intent_max_order_quantity = Some(Decimal::from(100));
    if let StrategyParams::ProbabilityReversal { spec, .. } = &mut config.strategies[0].params {
        spec.max_positions = 1;
    }
    let runtime = SystemBuilder::new(config)
        .register_strategies_from_config_strict()
        .unwrap()
        .register_simulated_execution_client(VenueId::POLYMARKET)
        .build();
    let (queues, mut reader) =
        engine::create_execution_queues(engine::ExecutionQueueConfig::default());
    let mut engine = runtime.engine.lock().await;
    engine.set_execution_queues(queues);
    engine.update_cash_balance(Decimal::from(1000)).unwrap();
    let ingester = engine.create_event_ingester_pair();
    let received = now_micros();
    let cex = hft_core::OrderId("unrelated-cex".into());
    // Build the unrelated position through the same approval and queue boundary.
    // An unbound OrderNew is unknown submission evidence and must halt admission.
    engine.set_execution_price_protection(std::collections::HashMap::from([(
        VenueId::BYBIT,
        ports::ExecutionPriceProtection::VenueQuote,
    )]));
    let cex_envelope = ports::OrderIntentEnvelope::new(
        ports::OrderIntent::crypto_spot(
            Symbol::new("BTCUSDT"),
            hft_core::Side::Buy,
            Quantity(Decimal::ONE),
            hft_core::OrderType::Limit,
            Some(Price(Decimal::from(10))),
            hft_core::TimeInForce::GTC,
            "unrelated-cex".into(),
            Some(VenueId::BYBIT),
        ),
        ports::OrderIntentLifecycle::new(received, u64::MAX),
    )
    .with_account_id(hft_core::AccountId("paper-account".into()));
    engine
        .submit_order_intent_envelope(cex_envelope, received, None)
        .unwrap();
    let cex_admitted = reader.receive_envelopes();
    assert_eq!(cex_admitted.len(), 1);
    reader
        .send_event(ports::ExecutionEvent::OrderNew {
            order_id: cex.clone(),
            client_order_id: Some(cex_admitted[0].client_order_id.clone()),
            account_id: Some(hft_core::AccountId("paper-account".into())),
            symbol: Symbol::new("BTCUSDT"),
            side: hft_core::Side::Buy,
            quantity: Quantity(Decimal::ONE),
            requested_price: cex_admitted[0].intent.price,
            arrival_price: None,
            timestamp: received,
            venue: Some(VenueId::BYBIT),
            strategy_id: "unrelated-cex".into(),
        })
        .unwrap();
    reader
        .send_event(ports::ExecutionEvent::Fill {
            order_id: cex.clone(),
            price: cex_admitted[0].intent.price.unwrap(),
            quantity: Quantity(Decimal::ONE),
            timestamp: received,
            fill_id: "existing-cex-fill".into(),
        })
        .unwrap();
    engine.tick().unwrap();
    assert_eq!(
        engine.account_reader().load().positions[&Symbol::new("BTCUSDT")].quantity,
        Quantity(Decimal::ONE)
    );
    assert_eq!(
        engine.export_oms_state()[&cex].cum_qty,
        Quantity(Decimal::ONE)
    );
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, now_micros(), 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 2))
        .unwrap();
    engine.tick().unwrap();
    let intents = reader.receive_envelopes();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].intent.asset_class, AssetClass::PredictionMarket);
    assert_eq!(intents[0].lifecycle.valid_until, end);
    assert_eq!(
        intents[0].lifecycle.max_order_notional,
        Some(Decimal::from(10))
    );
    assert_eq!(
        intents[0].lifecycle.max_order_quantity,
        Some(Decimal::from(100))
    );
    assert_eq!(engine.export_oms_state().len(), 1);
    assert!(engine.export_oms_state().contains_key(&cex));
    // Reports cross the real FIFO queue, then canonical OMS and Portfolio.
    let buy = hft_core::OrderId("paper-buy".into());
    reader
        .send_event(ports::ExecutionEvent::OrderNew {
            order_id: buy.clone(),
            client_order_id: Some(intents[0].client_order_id.clone()),
            account_id: Some(hft_core::AccountId("paper-account".into())),
            symbol: Symbol::new("123"),
            side: hft_core::Side::Buy,
            quantity: intents[0].intent.quantity,
            requested_price: intents[0].intent.price,
            arrival_price: None,
            timestamp: received + 2,
            venue: Some(VenueId::POLYMARKET),
            strategy_id: "probability".into(),
        })
        .unwrap();
    reader
        .send_event(ports::ExecutionEvent::Fill {
            order_id: buy.clone(),
            price: Price(Decimal::new(65, 2)),
            quantity: Quantity(Decimal::from(3)),
            timestamp: received + 3,
            fill_id: "buy-partial".into(),
        })
        .unwrap();
    engine.tick().unwrap();
    assert_eq!(
        engine.export_oms_state()[&buy].cum_qty,
        Quantity(Decimal::from(3))
    );
    assert_eq!(
        engine.account_reader().load().positions[&Symbol::new("123")].quantity,
        Quantity(Decimal::from(3))
    );
    let cash = engine.account_reader().load().cash_balance;
    reader
        .send_event(ports::ExecutionEvent::Fill {
            order_id: buy.clone(),
            price: Price(Decimal::new(65, 2)),
            quantity: Quantity(Decimal::from(3)),
            timestamp: received + 3,
            fill_id: "buy-partial".into(),
        })
        .unwrap();
    engine.tick().unwrap();
    assert_eq!(engine.account_reader().load().cash_balance, cash);
    assert_eq!(
        engine.export_oms_state()[&buy].cum_qty,
        Quantity(Decimal::from(3))
    );
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(90, now_micros(), 3))
        .unwrap();
    engine.tick().unwrap();
    assert!(
        reader.receive_envelopes().is_empty(),
        "unknown/partial buy keeps the existing order pending"
    );
    reader
        .send_event(ports::ExecutionEvent::OrderCanceled {
            order_id: buy,
            timestamp: received + 5,
        })
        .unwrap();
    engine.tick().unwrap();
    // Preserve the real shared-risk cooldown and refresh the source quote.
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(90, now_micros(), 4))
        .unwrap();
    engine.tick().unwrap();
    let exits = reader.receive_envelopes();
    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].intent.side, hft_core::Side::Sell);
    assert_eq!(exits[0].intent.quantity, Quantity(Decimal::from(3)));
    assert!(intents[0].validate_pre_execution(end, None).is_err());
    drop(engine);
    let mut shorter = runtime.config.clone();
    shorter.engine.intent_max_latency_us = 100_000;
    let runtime = SystemBuilder::new(shorter)
        .register_strategies_from_config_strict()
        .unwrap()
        .register_simulated_execution_client(VenueId::POLYMARKET)
        .build();
    let (queues, mut reader) =
        engine::create_execution_queues(engine::ExecutionQueueConfig::default());
    let mut engine = runtime.engine.lock().await;
    engine.set_execution_queues(queues);
    engine.update_cash_balance(Decimal::from(1000)).unwrap();
    let ingester = engine.create_event_ingester_pair();
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 2))
        .unwrap();
    engine.tick().unwrap();
    let capped = reader.receive_envelopes();
    assert_eq!(capped.len(), 1);
    assert_eq!(
        capped[0].lifecycle.valid_until,
        capped[0].lifecycle.created_ts + 100_000
    );
    assert!(
        capped[0].lifecycle.valid_until < end,
        "a strategy deadline cannot enlarge the engine TTL"
    );
    drop(engine);
    let mut down_config = runtime.config.clone();
    down_config.engine.intent_max_latency_us = 10_000_000;
    let runtime = SystemBuilder::new(down_config)
        .register_strategies_from_config_strict()
        .unwrap()
        .register_simulated_execution_client(VenueId::POLYMARKET)
        .build();
    let (queues, mut reader) =
        engine::create_execution_queues(engine::ExecutionQueueConfig::default());
    let mut engine = runtime.engine.lock().await;
    engine.set_execution_queues(queues);
    engine.update_cash_balance(Decimal::from(1000)).unwrap();
    let ingester = engine.create_event_ingester_pair();
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(75, received, 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(outcome_snapshot("456", 20, now_micros(), 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(35, now_micros(), 2))
        .unwrap();
    engine.tick().unwrap();
    let down = reader.receive_envelopes();
    assert_eq!(down.len(), 1);
    assert_eq!(down[0].intent.symbol, Symbol::new("456"));
    assert_eq!(down[0].intent.price, Some(Price(Decimal::new(20, 2))));
    assert_eq!(down[0].intent.quantity, Quantity(Decimal::from(50)));
    assert_eq!(down[0].intent.asset_class, AssetClass::PredictionMarket);
    assert_eq!(down[0].lifecycle.arrival_price, down[0].intent.price);
    assert_eq!(
        down[0].source_book_identity.as_ref().unwrap().symbol,
        Symbol::new("123")
    );
    let order = hft_core::OrderId("down-buy".into());
    reader
        .send_event(ports::ExecutionEvent::OrderNew {
            order_id: order.clone(),
            client_order_id: Some(down[0].client_order_id.clone()),
            account_id: Some(hft_core::AccountId("paper-account".into())),
            symbol: Symbol::new("456"),
            side: hft_core::Side::Buy,
            quantity: down[0].intent.quantity,
            requested_price: down[0].intent.price,
            arrival_price: None,
            timestamp: received + 3,
            venue: Some(VenueId::POLYMARKET),
            strategy_id: "probability".into(),
        })
        .unwrap();
    reader
        .send_event(ports::ExecutionEvent::Fill {
            order_id: order.clone(),
            price: Price(Decimal::new(20, 2)),
            quantity: Quantity(Decimal::ONE),
            timestamp: received + 4,
            fill_id: "down-partial".into(),
        })
        .unwrap();
    engine.tick().unwrap();
    assert_eq!(
        engine.export_oms_state()[&order].cum_qty,
        Quantity(Decimal::ONE)
    );
    assert_eq!(
        engine.account_reader().load().positions[&Symbol::new("456")].quantity,
        Quantity(Decimal::ONE)
    );
    reader
        .send_event(ports::ExecutionEvent::OrderCanceled {
            order_id: order,
            timestamp: received + 5,
        })
        .unwrap();
    engine.tick().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    ingester
        .lock()
        .unwrap()
        .ingest(outcome_snapshot("456", 90, now_micros(), 2))
        .unwrap();
    engine.tick().unwrap();
    let exit = reader.receive_envelopes();
    assert_eq!(exit.len(), 1);
    assert_eq!(exit[0].intent.side, hft_core::Side::Sell);
    assert_eq!(exit[0].intent.quantity, Quantity(Decimal::ONE));
    drop(engine);
    let runtime = SystemBuilder::new(runtime.config.clone())
        .register_strategies_from_config_strict()
        .unwrap()
        .register_simulated_execution_client(VenueId::POLYMARKET)
        .build();
    let (mut queues, mut reader) = engine::create_execution_queues(engine::ExecutionQueueConfig {
        intent_queue_capacity: 1,
        ..Default::default()
    });
    // Occupy the local queue; no worker or venue is started.
    queues
        .send_envelope(ports::OrderIntentEnvelope::new(
            down[0].intent.clone(),
            ports::OrderIntentLifecycle::default(),
        ))
        .unwrap();
    let mut engine = runtime.engine.lock().await;
    engine.set_execution_queues(queues);
    engine.update_cash_balance(Decimal::from(1000)).unwrap();
    let ingester = engine.create_event_ingester_pair();
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 2))
        .unwrap();
    engine.tick().unwrap();
    let occupied = reader.receive_envelopes();
    assert_eq!(occupied.len(), 1);
    assert_eq!(occupied[0].intent.symbol, Symbol::new("456"));
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    let received = now_micros();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 3))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, now_micros(), 4))
        .unwrap();
    engine.tick().unwrap();
    assert_eq!(
        reader.receive_envelopes().len(),
        1,
        "actual QueueFull must release only the unsubmitted pending proposal"
    );
}

struct SharedProbability(
    std::sync::Arc<std::sync::Mutex<strategy_probability_reversal::ProbabilityReversalStrategy>>,
);

impl ports::Strategy for SharedProbability {
    fn name(&self) -> &str {
        "probability"
    }
    fn supported_asset_classes(&self) -> &'static [AssetClass] {
        &[AssetClass::PredictionMarket]
    }
    fn on_market_event(
        &mut self,
        event: &MarketEvent,
        account: &ports::AccountView,
    ) -> Vec<ports::OrderIntent> {
        self.0.lock().unwrap().on_market_event(event, account)
    }
    fn on_execution_event(
        &mut self,
        event: &ports::ExecutionEvent,
        account: &ports::AccountView,
    ) -> Vec<ports::OrderIntent> {
        self.0.lock().unwrap().on_execution_event(event, account)
    }
    fn observe_execution_state(
        &mut self,
        event: &ports::ExecutionEvent,
        account: &ports::AccountView,
    ) {
        self.0
            .lock()
            .unwrap()
            .observe_execution_state(event, account);
    }
    fn observe_unsent_intent_rejection(&mut self, client_order_id: &str) {
        self.0
            .lock()
            .unwrap()
            .observe_unsent_intent_rejection(client_order_id);
    }
    fn observe_intent_submission(
        &mut self,
        intent: &ports::OrderIntent,
        result: ports::IntentSubmissionResult<'_>,
    ) {
        self.0
            .lock()
            .unwrap()
            .observe_intent_submission(intent, result);
    }
}

#[tokio::test]
async fn rejection_provenance_preserves_real_strategy_pending_and_late_fill_count() {
    use ports::Strategy;
    use std::sync::{Arc, Mutex};
    for colliding_venue_id in [true, false] {
        let now = now_micros();
        let mut fixed = spec(now);
        fixed.max_daily_trades = 1;
        let mut next = fixed.episodes[0].clone();
        next.episode_id = "next-episode".into();
        next.condition_id = "next-condition".into();
        next.up_token = "789".into();
        next.down_token = "987".into();
        fixed.episodes.push(next);
        let strategy = Arc::new(Mutex::new(
            strategy_probability_reversal::ProbabilityReversalStrategy::new(
                strategy_probability_reversal::ProbabilityStrategyConfig {
                    name: "probability".into(),
                    spec: fixed,
                    max_order_notional: Decimal::from(10),
                    max_order_quantity: Decimal::from(100),
                },
            )
            .unwrap(),
        ));
        let mut engine = engine::Engine::new(engine::EngineConfig {
            intent_max_latency_us: 60_000_000,
            ..Default::default()
        });
        engine.set_order_manager(Box::new(oms_core::OmsCore::new()));
        engine.set_portfolio_manager(Box::new(portfolio_core::Portfolio::with_cash_balance(
            Decimal::from(1000),
        )));
        engine.set_execution_price_protection(std::collections::HashMap::from([(
            VenueId::POLYMARKET,
            ports::ExecutionPriceProtection::VenueQuote,
        )]));
        engine.register_strategy(SharedProbability(Arc::clone(&strategy)));
        let (queues, mut worker) =
            engine::create_execution_queues(engine::ExecutionQueueConfig::default());
        engine.set_execution_queues(queues);
        let mut venue_reports = engine.subscribe_execution_events();
        let feed = |token: &str, ask: i64, sequence: u64, account: &ports::AccountView| {
            let mut event = outcome_snapshot(token, ask, now_micros(), sequence);
            if token == "789" {
                if let MarketEvent::Snapshot(snapshot) = &mut event {
                    snapshot.provider_identity.as_mut().unwrap().market = "next-condition".into();
                }
            }
            strategy.lock().unwrap().on_market_event(&event, account)
        };
        let account = engine.get_account_view();
        assert!(feed("123", 25, 1, &account).is_empty());
        let first = feed("123", 65, 2, &account).remove(0);
        let mut first_envelope = ports::OrderIntentEnvelope::new(
            first.clone(),
            ports::OrderIntentLifecycle::new(now_micros(), u64::MAX),
        )
        .with_account_id(hft_core::AccountId("strategy-account".into()));
        first_envelope.client_order_id = "client-a".into();
        engine
            .submit_order_intent_envelope(first_envelope, now_micros(), None)
            .unwrap();
        strategy.lock().unwrap().observe_intent_submission(
            &first,
            ports::IntentSubmissionResult::Enqueued {
                client_order_id: "client-a",
            },
        );
        let accepted = worker.receive_envelopes().pop().unwrap();
        let venue_id = hft_core::OrderId(
            if colliding_venue_id {
                "collision"
            } else {
                "venue-a"
            }
            .into(),
        );
        worker
            .send_event(ports::ExecutionEvent::OrderNew {
                order_id: venue_id.clone(),
                client_order_id: Some(accepted.client_order_id),
                account_id: accepted.account_id,
                symbol: first.symbol.clone(),
                side: first.side,
                quantity: first.quantity,
                requested_price: first.price,
                arrival_price: first.price,
                timestamp: now_micros(),
                venue: Some(VenueId::POLYMARKET),
                strategy_id: "probability".into(),
            })
            .unwrap();
        engine.tick().unwrap();
        assert!(matches!(
            venue_reports.try_recv(),
            Ok(ports::ExecutionEvent::OrderNew { .. })
        ));
        feed("123", 75, 3, &account);
        feed("456", 20, 1, &account);
        let second = feed("123", 35, 4, &account).remove(0);
        let mut second_envelope = ports::OrderIntentEnvelope::new(
            second.clone(),
            ports::OrderIntentLifecycle::new(now_micros(), u64::MAX),
        )
        .with_account_id(hft_core::AccountId("strategy-account".into()));
        second_envelope.client_order_id = "collision".into();
        engine
            .submit_order_intent_envelope(second_envelope, now_micros(), None)
            .unwrap();
        strategy.lock().unwrap().observe_intent_submission(
            &second,
            ports::IntentSubmissionResult::Enqueued {
                client_order_id: "collision",
            },
        );
        let unsent = worker.receive_envelopes().pop().unwrap();
        worker
            .send_event(ports::ExecutionEvent::OrderReject {
                order_id: hft_core::OrderId("collision".into()),
                reason: "unproven private rejection".into(),
                timestamp: now_micros(),
            })
            .unwrap();
        engine.tick().unwrap();
        let private_was_published = venue_reports.try_recv().is_ok();
        feed("123", 75, 5, &account);
        feed("456", 20, 2, &account);
        let private_kept_pending = feed("123", 35, 6, &account).is_empty();
        worker
            .reject_unsent_intent(&unsent.client_order_id, "definitely not sent")
            .await;
        engine.tick().unwrap();
        let local_was_published = venue_reports.try_recv().is_ok();
        feed("123", 75, 7, &account);
        feed("456", 20, 3, &account);
        let retried = feed("123", 35, 8, &account);
        let local_released_pending = retried.len() == 1;
        if let Some(intent) = retried.first() {
            strategy
                .lock()
                .unwrap()
                .observe_intent_submission(intent, ports::IntentSubmissionResult::NotSubmitted);
        }
        worker
            .send_event(ports::ExecutionEvent::Fill {
                order_id: venue_id.clone(),
                price: first.price.unwrap(),
                quantity: first.quantity,
                timestamp: now_micros(),
                fill_id: "late-a-fill".into(),
            })
            .unwrap();
        engine.tick().unwrap();
        assert_eq!(engine.export_oms_state()[&venue_id].cum_qty, first.quantity);
        let account = engine.get_account_view();
        feed("789", 25, 1, &account);
        assert!(
            feed("789", 65, 2, &account).is_empty(),
            "A's late fill must consume the one-entry daily budget despite B's colliding client ID"
        );
        assert!(
            private_kept_pending,
            "private rejection must not clear B's queued state"
        );
        assert!(
            local_released_pending,
            "typed unsent rejection must free only B's queued state"
        );
        assert!(
            !private_was_published && !local_was_published,
            "quarantined and unsent reports must not masquerade as venue order lifecycle events"
        );
        assert!(std::iter::from_fn(|| venue_reports.try_recv().ok()).any(|event|
            matches!(event, ports::ExecutionEvent::Fill { order_id, .. } if order_id == venue_id)));
    }
}
