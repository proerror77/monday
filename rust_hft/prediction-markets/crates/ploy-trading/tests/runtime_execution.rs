use chrono::Utc;
use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent, TradingRuntime};
use rust_decimal_macros::dec;
use std::collections::BTreeMap;

fn sample_intent() -> TradingIntent {
    TradingIntent {
        intent_id: "intent-1".to_string(),
        deployment_id: "example.paper".to_string(),
        market_id: "market-1".to_string(),
        token_id: "yes-token".to_string(),
        side: TradeSide::Buy,
        quantity: dec!(5),
        limit_price: Some(dec!(0.40)),
        purpose: IntentPurpose::Entry,
        created_at: Utc::now(),
    }
}

#[test]
fn submitting_and_filling_an_intent_updates_runtime_snapshot() {
    let mut runtime = TradingRuntime::default();
    let intent = sample_intent();
    runtime
        .submit_intent(intent.clone(), "order-1", None)
        .expect("valid intent");
    runtime.acknowledge_order("order-1", "venue-1");
    runtime.record_fill(FillRecord {
        fill_id: "fill-1".to_string(),
        order_id: "order-1".to_string(),
        token_id: "yes-token".to_string(),
        side: TradeSide::Buy,
        quantity: dec!(5),
        price: dec!(0.40),
        fee: dec!(0.05),
        timestamp: Utc::now(),
    });

    let mut marks = BTreeMap::new();
    marks.insert("yes-token".to_string(), dec!(0.55));
    let snapshot = runtime.snapshot(&marks);

    assert_eq!(snapshot.intents.len(), 1);
    assert_eq!(snapshot.orders.len(), 1);
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.positions.len(), 1);
    assert_eq!(snapshot.orders[0].intent_id, intent.intent_id);
    assert_eq!(
        snapshot.orders[0].venue_order_id.as_deref(),
        Some("venue-1")
    );
    assert_eq!(snapshot.positions[0].net_qty, dec!(5));
    assert_eq!(snapshot.pnl.unrealized_pnl.round_dp(2), dec!(0.75));
    assert_eq!(snapshot.risk.active_orders, 0);
    assert_eq!(snapshot.risk.open_positions, 1);
}

#[test]
fn risk_snapshot_only_counts_active_intents() {
    let mut runtime = TradingRuntime::default();
    let intent = sample_intent();
    runtime
        .submit_intent(intent, "order-1", None)
        .expect("valid intent");

    let active = runtime.snapshot(&BTreeMap::new());
    assert_eq!(active.risk.pending_intents, 1);
    assert_eq!(active.risk.active_orders, 1);

    runtime.reject_order("order-1", "risk_reject");
    let rejected = runtime.snapshot(&BTreeMap::new());
    assert_eq!(rejected.risk.pending_intents, 0);
    assert_eq!(rejected.risk.active_orders, 0);
}

#[test]
fn recording_the_same_fill_twice_is_idempotent() {
    let mut runtime = TradingRuntime::default();
    let intent = sample_intent();
    runtime
        .submit_intent(intent, "order-1", None)
        .expect("valid intent");
    runtime.acknowledge_order("order-1", "venue-1");

    let fill = FillRecord {
        fill_id: "fill-1".to_string(),
        order_id: "order-1".to_string(),
        token_id: "yes-token".to_string(),
        side: TradeSide::Buy,
        quantity: dec!(5),
        price: dec!(0.40),
        fee: dec!(0.05),
        timestamp: Utc::now(),
    };

    runtime.record_fill(fill.clone());
    runtime.record_fill(fill);

    let snapshot = runtime.snapshot(&BTreeMap::new());
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.orders[0].filled_qty, dec!(5));
    assert_eq!(snapshot.positions[0].net_qty, dec!(5));
}

#[test]
fn canceling_an_active_order_removes_it_from_risk() {
    let mut runtime = TradingRuntime::default();
    let intent = sample_intent();
    runtime
        .submit_intent(intent, "order-1", None)
        .expect("valid intent");
    runtime.acknowledge_order("order-1", "venue-1");

    let before_cancel = runtime.snapshot(&BTreeMap::new());
    assert_eq!(before_cancel.risk.pending_intents, 1);
    assert_eq!(before_cancel.risk.active_orders, 1);
    assert_eq!(
        runtime
            .order("order-1")
            .expect("order")
            .venue_order_id
            .as_deref(),
        Some("venue-1")
    );

    runtime.cancel_order("order-1");

    let after_cancel = runtime.snapshot(&BTreeMap::new());
    assert_eq!(
        after_cancel.orders[0].state,
        ploy_trading::OrderState::Canceled
    );
    assert_eq!(after_cancel.risk.pending_intents, 0);
    assert_eq!(after_cancel.risk.active_orders, 0);
}
