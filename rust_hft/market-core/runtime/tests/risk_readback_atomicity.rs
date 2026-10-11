//! Actual risk/OMS/Portfolio owners with synthetic provider readback.
//! Tokio Mutex FIFO and the worker reply define the race; no sleep is needed.
use engine::execution_worker::{ClientReconcileSnapshot, ControlCommand, WorkerReconcileSnapshot};
use engine::{
    create_execution_queues, Engine, EngineConfig, ExecutionControlHandle, ExecutionQueueConfig,
};
use hft_core::{
    now_micros, AccountId, OrderId, OrderType, Price, Quantity, Side, Symbol, TimeInForce, VenueId,
};
use ports::{ExecutionEvent, OrderIntent, OrderIntentEnvelope, OrderIntentLifecycle, OrderStatus};
use runtime::{RiskConfig, RiskManagerFactory};
use rust_decimal::Decimal;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, task::Poll};
use tokio::sync::{mpsc, Mutex};

async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    std::future::poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await
}

fn candidate(symbol: &str) -> OrderIntentEnvelope {
    OrderIntentEnvelope::new(
        OrderIntent::crypto_spot(
            Symbol::new(symbol),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::from(60))),
            TimeInForce::GTC,
            "readback-atomicity".into(),
            Some(VenueId::BINANCE),
        ),
        OrderIntentLifecycle::new(now_micros(), u64::MAX),
    )
    .with_account_id(AccountId("account".into()))
}

#[tokio::test]
async fn cancellation_after_order_comparison_cannot_use_the_older_readback_to_release_risk() {
    let mut engine = Engine::new(EngineConfig {
        intent_max_latency_us: 60_000_000,
        ..Default::default()
    });
    engine.register_risk_manager_boxed(RiskManagerFactory::create_strategy_aware_risk_manager(
        &RiskConfig {
            risk_type: "Default".into(),
            global_position_limit: Decimal::from(100),
            global_notional_limit: Decimal::from(100),
            max_orders_per_second: 100,
            staleness_threshold_us: 60_000_000,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            ..Default::default()
        },
    ));
    engine.set_order_manager(Box::new(oms_core::OmsCore::new()));
    engine.set_portfolio_manager(Box::new(portfolio_core::Portfolio::with_cash_balance(
        Decimal::from(1_000),
    )));
    engine.set_execution_price_protection(HashMap::from([(
        VenueId::BINANCE,
        ports::ExecutionPriceProtection::VenueQuote,
    )]));
    let (queues, mut worker) = create_execution_queues(ExecutionQueueConfig {
        intent_queue_capacity: 8,
        ..Default::default()
    });
    engine.set_execution_queues(queues);

    engine
        .submit_order_intent_envelope(candidate("BTCUSDT"), now_micros(), None)
        .unwrap();
    let mut pending = worker.receive_envelopes();
    assert_eq!(pending.len(), 1);
    let accepted = pending.pop().unwrap();
    let order_id = OrderId(accepted.client_order_id.clone());
    worker
        .send_event(ExecutionEvent::OrderNew {
            order_id: order_id.clone(),
            client_order_id: Some(accepted.client_order_id),
            account_id: accepted.account_id,
            symbol: accepted.intent.symbol.clone(),
            side: accepted.intent.side,
            quantity: accepted.intent.quantity,
            requested_price: accepted.intent.price,
            arrival_price: accepted.intent.price,
            timestamp: now_micros(),
            venue: accepted.intent.target_venue,
            strategy_id: accepted.intent.strategy_id,
        })
        .unwrap();
    engine.tick().unwrap();

    // This independent snapshot explicitly shows the order OPEN before cancellation.
    let snapshot = WorkerReconcileSnapshot {
        clients: vec![ClientReconcileSnapshot {
            client_index: 0,
            venue: Some(VenueId::BINANCE),
            account_id: Some(AccountId("account".into())),
            open_orders: Ok(vec![ports::OpenOrder {
                order_id: order_id.clone(),
                client_order_id: Some(order_id.0.clone()),
                symbol: accepted.intent.symbol,
                side: Side::Buy,
                order_type: OrderType::Limit,
                original_quantity: Quantity(Decimal::ONE),
                remaining_quantity: Quantity(Decimal::ONE),
                filled_quantity: Quantity::zero(),
                price: Some(Price(Decimal::from(60))),
                status: OrderStatus::New,
                created_at: 1,
                updated_at: now_micros(),
            }]),
            balances: Some(Ok(vec![ports::AccountBalance {
                asset: "USD".into(),
                available: Decimal::from(1_000),
                frozen: Decimal::ZERO,
                total: Decimal::from(1_000),
                usd_value: Some(Decimal::from(1_000)),
            }])),
            positions: Some(Ok(Vec::new())),
            recent_fills: Some(Ok(Vec::new())),
            asset_inventory_capability: Some(
                ports::AssetInventoryCapability::PositionSnapshotRequired,
            ),
            ..Default::default()
        }],
    };
    let engine = Arc::new(Mutex::new(engine));
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let control = ExecutionControlHandle::new(engine.clone(), Some(control_tx), true);
    let mut reconcile = Box::pin(control.reconcile(true));
    assert!(poll_once(reconcile.as_mut()).await.is_pending());
    let ControlCommand::Reconcile { reply, .. } = control_rx.try_recv().unwrap() else {
        panic!("expected the independent worker snapshot request");
    };

    // Queue reconciliation first and cancellation second while the Engine is locked.
    // The old three-lock implementation yields to cancellation after comparing orders.
    let gate = engine.lock().await;
    reply.send(snapshot).unwrap();
    assert!(poll_once(reconcile.as_mut()).await.is_pending());
    worker
        .send_event(ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: now_micros(),
        })
        .unwrap();
    let cancel_engine = engine.clone();
    let mut apply_cancellation = Box::pin(async move {
        cancel_engine.lock().await.tick().unwrap();
    });
    assert!(poll_once(apply_cancellation.as_mut()).await.is_pending());
    drop(gate);

    // With one guard, reconciliation completes before cancellation can mutate OMS.
    // With separate guards, it suspends and later sees an unproven cancellation.
    let first_readback_poll = poll_once(reconcile.as_mut()).await;
    apply_cancellation.await;
    let report = match first_readback_poll {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => reconcile.await.unwrap(),
    };
    assert!(report.complete && report.healthy);

    let mut engine = engine.lock().await;
    assert_eq!(
        engine.export_oms_state()[&order_id].status,
        OrderStatus::Canceled
    );
    assert!(engine.get_account_view().positions.is_empty());
    assert!(
        engine
            .submit_order_intent_envelope(candidate("ETHUSDT"), now_micros(), None)
            .is_err(),
        "the older OPEN-order snapshot released the cancellation's pending risk: 60 + 60 > 100"
    );
}
