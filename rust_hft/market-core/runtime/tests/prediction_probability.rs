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
        .ingest(snapshot(65, received + 1, 2))
        .unwrap();
    engine.tick().unwrap();
    let intents = reader.receive_envelopes();
    // The per-order runtime ceiling rejects this $10 proposal rather than
    // allowing a strategy configuration to enlarge the verified $5 ceiling.
    assert!(intents.is_empty());
    assert!(engine.export_oms_state().is_empty());
    drop(engine);
    let mut config = runtime.config.clone();
    config.engine.intent_max_order_notional = Some(Decimal::from(10));
    config.engine.intent_max_order_quantity = Some(Decimal::from(100));
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
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(25, received, 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(65, received + 1, 2))
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
    assert!(engine.export_oms_state().is_empty());
    // Reports cross the real FIFO queue, then canonical OMS and Portfolio.
    let buy = hft_core::OrderId("paper-buy".into());
    reader
        .send_event(ports::ExecutionEvent::OrderNew {
            order_id: buy.clone(),
            client_order_id: Some("paper-buy".into()),
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
        .ingest(snapshot(90, received + 4, 3))
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
        .ingest(snapshot(65, received + 1, 2))
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
        .ingest(outcome_snapshot("456", 20, received + 1, 1))
        .unwrap();
    engine.tick().unwrap();
    ingester
        .lock()
        .unwrap()
        .ingest(snapshot(35, received + 2, 2))
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
            client_order_id: Some("down-buy".into()),
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
}
