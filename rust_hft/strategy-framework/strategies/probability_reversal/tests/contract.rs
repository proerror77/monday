use hft_core::{
    AssetClass, LocalReceiveTimestamp, MarketDataTimestamps, OrderId, Price, ProductType, Quantity,
    Side, Symbol, VenueId,
};
use hft_research_manifest::prediction_probability::{
    BinaryEpisodeV1, ProbabilityReversalSpecV1, PROBABILITY_REVERSAL_SCHEMA,
};
use ports::{
    AccountView, BookLevel, ExecutionEvent, MarketEvent, MarketSnapshot, ProviderBookIdentity,
    Strategy,
};
use rust_decimal::Decimal;
use strategy_probability_reversal::{ProbabilityReversalStrategy, ProbabilityStrategyConfig};

fn config() -> ProbabilityStrategyConfig {
    ProbabilityStrategyConfig {
        name: "probability".into(),
        spec: ProbabilityReversalSpecV1 {
            schema: PROBABILITY_REVERSAL_SCHEMA.into(),
            episodes: vec![BinaryEpisodeV1 {
                episode_id: "episode-1".into(),
                condition_id: "condition-1".into(),
                underlying: "BTCUSDT".into(),
                venue: "POLYMARKET".into(),
                up_token: "123".into(),
                down_token: "456".into(),
                start_us: 1_000_000,
                end_us: 301_000_000,
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
        },
        max_order_notional: Decimal::from(10),
        max_order_quantity: Decimal::from(100),
    }
}
fn quote(token: &str, ask: i64, time: u64, sequence: u64) -> MarketEvent {
    MarketEvent::Snapshot(MarketSnapshot {
        symbol: Symbol::new(token),
        timestamp: time,
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
        timestamps: MarketDataTimestamps::local_only(LocalReceiveTimestamp::new(time)),
        provider_identity: Some(ProviderBookIdentity {
            market: "condition-1".into(),
            book_hash: None,
        }),
    })
}

#[test]
fn original_up_and_down_thresholds_produce_typed_share_orders() {
    let account = AccountView::default();
    let mut strategy = ProbabilityReversalStrategy::new(config()).unwrap();
    assert!(strategy
        .on_market_event(&quote("123", 25, 297_000_000, 1), &account)
        .is_empty());
    let intents = strategy.on_market_event(&quote("123", 65, 298_000_000, 2), &account);
    assert_eq!(intents.len(), 1);
    let intent = &intents[0];
    assert_eq!(intent.symbol, Symbol::new("123"));
    assert_eq!(intent.side, Side::Buy);
    assert_eq!(intent.asset_class, AssetClass::PredictionMarket);
    assert_eq!(intent.product_type, ProductType::PredictionMarket);
    assert_eq!(intent.target_venue, Some(VenueId::POLYMARKET));
    assert!(intent.quantity.0 * intent.price.unwrap().0 <= Decimal::from(10));
    assert_eq!(strategy.intent_semantic_deadline(intent), Some(301_000_000));
    // Unknown submission retains pending state instead of duplicating an order.
    strategy.on_market_event(&quote("123", 25, 298_100_000, 3), &account);
    assert!(strategy
        .on_market_event(&quote("123", 65, 298_200_000, 4), &account)
        .is_empty());
    let mut strategy = ProbabilityReversalStrategy::new(config()).unwrap();
    strategy.on_market_event(&quote("123", 75, 297_000_000, 1), &account);
    strategy.on_market_event(&quote("456", 20, 298_900_000, 1), &account);
    let intents = strategy.on_market_event(&quote("123", 35, 299_000_000, 2), &account);
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].symbol, Symbol::new("456"));
    assert_eq!(intents[0].price, Some(Price(Decimal::new(20, 2))));
}

#[test]
fn wrong_episode_clock_invalid_price_and_stale_down_cannot_enter() {
    let account = AccountView::default();
    let mut strategy = ProbabilityReversalStrategy::new(config()).unwrap();
    strategy.on_market_event(&quote("123", 75, 297_000_000, 1), &account);
    strategy.on_market_event(&quote("456", 20, 297_100_000, 1), &account);
    assert!(strategy
        .on_market_event(&quote("123", 35, 299_000_000, 2), &account)
        .is_empty());
    for (ask, time, sequence) in [
        (0, 299_100_000, 3),
        (100, 299_200_000, 4),
        (65, 301_000_000, 5),
        (65, 296_000_000, 1),
    ] {
        assert!(strategy
            .on_market_event(&quote("123", ask, time, sequence), &account)
            .is_empty());
    }
    let mut strategy = ProbabilityReversalStrategy::new(config()).unwrap();
    let mut wrong = quote("123", 25, 297_000_000, 1);
    if let MarketEvent::Snapshot(snapshot) = &mut wrong {
        snapshot.provider_identity.as_mut().unwrap().market = "foreign".into();
    }
    strategy.on_market_event(&wrong, &account);
    assert!(strategy
        .on_market_event(&quote("123", 65, 298_000_000, 2), &account)
        .is_empty());
    assert!(strategy
        .on_market_event(&quote("999", 65, 298_100_000, 1), &account)
        .is_empty());
    let mut strategy = ProbabilityReversalStrategy::new(config()).unwrap();
    let mut empty = quote("123", 25, 297_000_000, 1);
    if let MarketEvent::Snapshot(snapshot) = &mut empty {
        snapshot.asks[0].quantity = Quantity::zero();
    }
    assert!(strategy.on_market_event(&empty, &account).is_empty());
    assert!(strategy
        .on_market_event(&quote("123", 65, 298_000_000, 2), &account)
        .is_empty());
}

#[test]
fn config_rejects_cross_episode_tokens_and_nonfinite_thresholds() {
    let mut bad = config();
    let mut episode = bad.spec.episodes[0].clone();
    episode.episode_id = "episode-2".into();
    episode.condition_id = "condition-2".into();
    bad.spec.episodes.push(episode);
    assert!(ProbabilityReversalStrategy::new(bad).is_err());
    let mut bad = config();
    bad.spec.curr_prob_high = f64::NAN;
    assert!(ProbabilityReversalStrategy::new(bad).is_err());
    let mut bad = config();
    bad.spec.episodes[0].up_token = "00123".into();
    assert!(ProbabilityReversalStrategy::new(bad).is_err());
}

#[test]
fn authoritative_partial_position_remains_until_full_close_and_exit_is_bounded() {
    let mut config = config();
    config.max_order_quantity = Decimal::from(3);
    let mut strategy = ProbabilityReversalStrategy::new(config).unwrap();
    let symbol = Symbol::new("123");
    let mut account = AccountView::default();
    account.positions.insert(
        symbol.clone(),
        ports::Position {
            symbol: symbol.clone(),
            quantity: Quantity(Decimal::from(10)),
            avg_price: Price(Decimal::new(65, 2)),
            unrealized_pnl: Decimal::ZERO,
            realized_pnl: Decimal::ZERO,
        },
    );
    let exits = strategy.on_market_event(&quote("123", 90, 297_000_000, 1), &account);
    assert_eq!(exits[0].side, Side::Sell);
    assert_eq!(exits[0].quantity, Quantity(Decimal::from(3)));
    let order_id = OrderId("exit-1".into());
    strategy.on_execution_event(
        &ExecutionEvent::OrderNew {
            order_id: order_id.clone(),
            client_order_id: None,
            account_id: None,
            symbol: symbol.clone(),
            side: Side::Sell,
            quantity: exits[0].quantity,
            requested_price: exits[0].price,
            arrival_price: None,
            timestamp: 297_000_000,
            venue: Some(VenueId::POLYMARKET),
            strategy_id: "probability".into(),
        },
        &account,
    );
    account.positions.get_mut(&symbol).unwrap().quantity = Quantity(Decimal::from(7));
    strategy.on_execution_event(
        &ExecutionEvent::Fill {
            order_id: order_id.clone(),
            price: Price(Decimal::new(88, 2)),
            quantity: Quantity(Decimal::from(3)),
            timestamp: 297_100_000,
            fill_id: "partial-1".into(),
        },
        &account,
    );
    strategy.on_execution_event(
        &ExecutionEvent::OrderCompleted {
            order_id,
            final_price: Price(Decimal::new(88, 2)),
            total_filled: Quantity(Decimal::from(3)),
            timestamp: 297_100_000,
        },
        &account,
    );
    let remaining = strategy.on_market_event(&quote("123", 90, 297_200_000, 2), &account);
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].quantity, Quantity(Decimal::from(3)));
    assert!(strategy
        .on_market_event(&quote("123", 90, 301_000_000, 3), &account)
        .is_empty());
}
