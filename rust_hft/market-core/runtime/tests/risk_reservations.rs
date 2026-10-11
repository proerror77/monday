//! Real Engine, risk factory, OMS and Portfolio; only external I/O is synthetic.
use engine::execution_worker::{ClientReconcileSnapshot, ControlCommand, WorkerReconcileSnapshot};
use engine::{
    create_execution_queues, Engine, EngineConfig, ExecutionControlHandle, ExecutionQueueConfig,
    WorkerQueues,
};
use hft_core::{
    now_micros, AccountId, HftResult, OrderId, OrderType, Price, Quantity, Side, Symbol,
    TimeInForce, VenueId,
};
use ports::{ExecutionEvent, OrderIntent, OrderIntentEnvelope, OrderIntentLifecycle, OrderStatus};
use runtime::{RiskConfig as SystemRiskConfig, RiskManagerFactory};
use rust_decimal::Decimal;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{mpsc, Mutex};

struct Harness {
    engine: Arc<Mutex<Engine>>,
    worker: WorkerQueues,
    fills: Vec<ports::AccountFill>,
}

struct BarOrders;
impl ports::Strategy for BarOrders {
    fn name(&self) -> &str {
        "bar-risk"
    }
    fn on_market_event(
        &mut self,
        event: &ports::MarketEvent,
        _: &ports::AccountView,
    ) -> Vec<OrderIntent> {
        let ports::MarketEvent::Bar(bar) = event else {
            return Vec::new();
        };
        vec![OrderIntent::crypto_spot(
            bar.symbol.clone(),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::from(60))),
            TimeInForce::GTC,
            self.name().into(),
            Some(VenueId::BINANCE),
        )]
    }
    fn on_execution_event(
        &mut self,
        _: &ExecutionEvent,
        _: &ports::AccountView,
    ) -> Vec<OrderIntent> {
        Vec::new()
    }
}

#[tokio::test]
async fn strategy_market_events_cannot_reuse_the_previous_ticks_unfilled_budget() {
    let mut h = Harness::new(8);
    let (mut ingress, consumer) =
        engine::dataflow::EventIngester::new(engine::dataflow::IngestionConfig {
            stale_threshold_us: 60_000_000,
            ..Default::default()
        });
    {
        let mut engine = h.engine.lock().await;
        engine.register_event_consumer(consumer);
        engine.register_strategy(BarOrders);
        engine.set_strategy_venue_mapping(HashMap::from([("bar-risk".into(), VenueId::BINANCE)]));
        engine.set_strategy_account_mapping(HashMap::from([(
            "bar-risk".into(),
            AccountId("account".into()),
        )]));
    }
    for symbol in ["BTCUSDT", "ETHUSDT"] {
        let now = now_micros();
        ingress
            .ingest(ports::MarketEvent::Bar(ports::AggregatedBar {
                symbol: Symbol::new(symbol),
                interval_ms: 60_000,
                open_time: now,
                close_time: now,
                open: Price(Decimal::from(60)),
                high: Price(Decimal::from(60)),
                low: Price(Decimal::from(60)),
                close: Price(Decimal::from(60)),
                volume: Quantity(Decimal::ONE),
                trade_count: 1,
                source_venue: Some(VenueId::BINANCE),
                timestamps: Default::default(),
            }))
            .unwrap();
        h.engine.lock().await.tick().unwrap();
    }
    let queued = h.worker.receive_envelopes();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].intent.symbol, Symbol::new("BTCUSDT"));
}

#[tokio::test]
async fn ambiguous_submission_without_a_terminal_event_keeps_its_reservation() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    h.bind_next().await; // The worker also publishes this provisional identity on an unknown response.
    h.event(ExecutionEvent::ConnectionStatus {
        connected: false,
        timestamp: now_micros(),
    })
    .await;
    assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err());
    assert!(h.worker.receive_envelopes().is_empty());
}

impl Harness {
    fn new(capacity: usize) -> Self {
        Self::new_with_portfolio(
            capacity,
            portfolio_core::Portfolio::with_cash_balance(Decimal::from(1_000)),
        )
    }

    fn new_with_portfolio(capacity: usize, portfolio: portfolio_core::Portfolio) -> Self {
        let config = SystemRiskConfig {
            risk_type: "Default".into(),
            global_position_limit: Decimal::from(100),
            global_notional_limit: Decimal::from(100),
            max_orders_per_second: 100,
            staleness_threshold_us: 60_000_000,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            ..Default::default()
        };
        let mut engine = Engine::new(EngineConfig {
            intent_max_latency_us: 60_000_000,
            ..Default::default()
        });
        engine.register_risk_manager_boxed(RiskManagerFactory::create_strategy_aware_risk_manager(
            &config,
        ));
        engine.set_order_manager(Box::new(oms_core::OmsCore::new()));
        engine.set_portfolio_manager(Box::new(portfolio));
        // No network is involved; the test's limit orders do not require a market-data feed.
        engine.set_execution_price_protection(HashMap::from([(
            VenueId::BINANCE,
            ports::ExecutionPriceProtection::VenueQuote,
        )]));
        let (queues, worker) = create_execution_queues(ExecutionQueueConfig {
            intent_queue_capacity: capacity,
            ..Default::default()
        });
        engine.set_execution_queues(queues);
        Self {
            engine: Arc::new(Mutex::new(engine)),
            worker,
            fills: Vec::new(),
        }
    }

    async fn submit(&mut self, symbol: &str, side: Side, price: i64) -> HftResult<OrderId> {
        self.submit_for_strategy(symbol, side, price, "reservation-test")
            .await
    }

    async fn submit_for_strategy(
        &mut self,
        symbol: &str,
        side: Side,
        price: i64,
        strategy: &str,
    ) -> HftResult<OrderId> {
        let now = now_micros();
        let envelope = OrderIntentEnvelope::new(
            OrderIntent::crypto_spot(
                Symbol::new(symbol),
                side,
                Quantity(Decimal::ONE),
                OrderType::Limit,
                Some(Price(Decimal::from(price))),
                TimeInForce::GTC,
                strategy.into(),
                Some(VenueId::BINANCE),
            ),
            OrderIntentLifecycle::new(now, u64::MAX),
        )
        .with_account_id(AccountId("account".into()));
        let id = OrderId(envelope.client_order_id.clone());
        self.engine
            .lock()
            .await
            .submit_order_intent_envelope(envelope, now, None)?;
        Ok(id)
    }

    async fn bind_next(&mut self) -> OrderId {
        self.bind_next_as(None).await
    }

    async fn bind_next_as(&mut self, venue_id: Option<&str>) -> OrderId {
        let mut pending = self.worker.receive_envelopes();
        assert_eq!(pending.len(), 1);
        let envelope = pending.pop().unwrap();
        let id = OrderId(venue_id.map_or_else(|| envelope.client_order_id.clone(), str::to_owned));
        self.event(ExecutionEvent::OrderNew {
            order_id: id.clone(),
            client_order_id: Some(envelope.client_order_id),
            account_id: envelope.account_id,
            symbol: envelope.intent.symbol,
            side: envelope.intent.side,
            quantity: envelope.intent.quantity,
            requested_price: envelope.intent.price,
            arrival_price: envelope.intent.price,
            timestamp: now_micros(),
            venue: envelope.intent.target_venue,
            strategy_id: envelope.intent.strategy_id,
        })
        .await;
        id
    }

    async fn event(&mut self, event: ExecutionEvent) {
        if let ExecutionEvent::Fill {
            order_id,
            price,
            quantity,
            timestamp,
            fill_id,
        } = &event
        {
            if !self
                .fills
                .iter()
                .any(|fill| fill.order_id == *order_id && fill.fill_id == *fill_id)
            {
                let engine = self.engine.lock().await;
                let orders = engine.export_oms_state();
                let order = &orders[order_id];
                self.fills.push(ports::AccountFill {
                    fill_id: fill_id.clone(),
                    order_id: order_id.clone(),
                    symbol: order.symbol.clone(),
                    side: order.side,
                    price: *price,
                    quantity: *quantity,
                    fee: None,
                    timestamp: *timestamp,
                });
            }
        }
        self.worker.send_event(event).unwrap();
        self.engine.lock().await.tick().unwrap();
    }

    async fn reconcile(&self, include_fills: bool) -> engine::RuntimeReconciliationReport {
        self.reconcile_snapshot(self.snapshot(include_fills).await)
            .await
    }

    async fn snapshot(&self, include_fills: bool) -> WorkerReconcileSnapshot {
        let engine = self.engine.lock().await;
        let account = engine.get_account_view();
        let orders = engine.export_oms_state();
        let open_orders = orders
            .values()
            .filter(|order| {
                matches!(
                    order.status,
                    OrderStatus::New
                        | OrderStatus::Acknowledged
                        | OrderStatus::PartiallyFilled
                        | OrderStatus::Unknown
                )
            })
            .map(|order| ports::OpenOrder {
                order_id: order.order_id.clone(),
                client_order_id: order.client_order_id.clone(),
                symbol: order.symbol.clone(),
                side: order.side,
                order_type: OrderType::Limit,
                original_quantity: order.qty,
                remaining_quantity: Quantity(order.qty.0 - order.cum_qty.0),
                filled_quantity: order.cum_qty,
                price: order.limit_price,
                status: order.status,
                created_at: 1,
                updated_at: now_micros(),
            })
            .collect();
        let equity = account.equity();
        let snapshot = WorkerReconcileSnapshot {
            clients: vec![ClientReconcileSnapshot {
                client_index: 0,
                venue: Some(VenueId::BINANCE),
                account_id: Some(AccountId("account".into())),
                open_orders: Ok(open_orders),
                balances: Some(Ok(vec![ports::AccountBalance {
                    asset: "USD".into(),
                    available: equity,
                    frozen: Decimal::ZERO,
                    total: equity,
                    usd_value: Some(equity),
                }])),
                positions: Some(Ok(account.positions.values().cloned().collect())),
                recent_fills: include_fills.then(|| Ok(self.fills.clone())),
                asset_inventory_capability: Some(
                    ports::AssetInventoryCapability::PositionSnapshotRequired,
                ),
                ..Default::default()
            }],
        };
        snapshot
    }

    async fn reconcile_snapshot(
        &self,
        snapshot: WorkerReconcileSnapshot,
    ) -> engine::RuntimeReconciliationReport {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(self.engine.clone(), Some(tx), true);
        let responder = tokio::spawn(async move {
            match rx.recv().await.unwrap() {
                ControlCommand::Reconcile { reply, .. } => reply.send(snapshot).unwrap(),
                _ => panic!("unexpected control command"),
            }
        });
        let report = control.reconcile(true).await.unwrap();
        responder.await.unwrap();
        report
    }
}

#[tokio::test]
async fn engine_keeps_queued_exposure_across_ticks_and_releases_definite_rejection() {
    let mut h = Harness::new(8);
    let first = h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    h.engine.lock().await.tick().unwrap();
    assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err());
    assert_eq!(h.worker.receive_envelopes().len(), 1);
    h.worker
        .reject_unsent_intent(&first.0, "definitely not submitted")
        .await;
    h.engine.lock().await.tick().unwrap();
    h.submit("SOLUSDT", Side::Buy, 60).await.unwrap();
    assert!(h
        .engine
        .lock()
        .await
        .export_oms_checkpoint()
        .reconciliation_exceptions
        .is_empty());
}

#[tokio::test]
async fn partial_and_duplicate_fills_convert_reservation_into_position_once() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let id = h.bind_next().await;
    h.event(ExecutionEvent::OrderAck {
        order_id: id.clone(),
        timestamp: now_micros(),
    })
    .await;
    h.event(ExecutionEvent::OrderCompleted {
        order_id: id.clone(),
        final_price: Price(Decimal::from(60)),
        total_filled: Quantity(Decimal::ONE),
        timestamp: now_micros(),
    })
    .await;
    assert!(
        h.submit("ETHUSDT", Side::Buy, 45).await.is_err(),
        "a completion notification cannot replace canonical fill accounting"
    );
    let fill = ExecutionEvent::Fill {
        order_id: id.clone(),
        price: Price(Decimal::from(60)),
        quantity: Quantity(Decimal::new(5, 1)),
        timestamp: now_micros(),
        fill_id: "partial".into(),
    };
    h.event(fill.clone()).await;
    h.event(fill).await;
    h.event(ExecutionEvent::OrderAck {
        order_id: id.clone(),
        timestamp: now_micros(),
    })
    .await;
    assert_eq!(
        h.engine.lock().await.export_oms_state()[&id].status,
        OrderStatus::PartiallyFilled
    );
    assert_eq!(
        h.engine.lock().await.get_account_view().positions[&Symbol::new("BTCUSDT")]
            .quantity
            .0,
        Decimal::new(5, 1)
    );
    assert!(h.submit("ETHUSDT", Side::Buy, 45).await.is_err());
    h.submit("SOLUSDT", Side::Buy, 40).await.unwrap();
}

#[tokio::test]
async fn cancel_keeps_unknown_final_quantity_until_complete_fill_readback() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let id = h.bind_next().await;
    h.event(ExecutionEvent::OrderCanceled {
        order_id: id.clone(),
        timestamp: now_micros(),
    })
    .await;
    assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err());
    h.event(ExecutionEvent::OrderReject {
        order_id: id.clone(),
        reason: "late duplicate terminal report".into(),
        timestamp: now_micros(),
    })
    .await;
    assert!(h.submit("BNBUSDT", Side::Buy, 60).await.is_err());
    h.event(ExecutionEvent::Fill {
        order_id: id,
        price: Price(Decimal::from(60)),
        quantity: Quantity(Decimal::new(5, 1)),
        timestamp: now_micros(),
        fill_id: "late-partial".into(),
    })
    .await;
    assert!(h.submit("SOLUSDT", Side::Buy, 45).await.is_err());
    assert!(!h.reconcile(false).await.healthy);
    assert!(h.submit("XRPUSDT", Side::Buy, 45).await.is_err());
    assert!(h.reconcile(true).await.healthy);
    h.submit("ADAUSDT", Side::Buy, 60).await.unwrap();
}

#[tokio::test]
async fn simulated_final_cancel_releases_only_its_unfilled_remainder() {
    let mut h = Harness::new(8);
    h.engine
        .lock()
        .await
        .set_final_cancellation_venues(std::collections::HashSet::from([VenueId::BINANCE]));
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let id = h.bind_next().await;
    h.event(ExecutionEvent::Fill {
        order_id: id.clone(),
        price: Price(Decimal::from(60)),
        quantity: Quantity(Decimal::new(5, 1)),
        timestamp: now_micros(),
        fill_id: "final-partial".into(),
    })
    .await;
    h.event(ExecutionEvent::OrderCanceled {
        order_id: id,
        timestamp: now_micros(),
    })
    .await;
    h.submit("ETHUSDT", Side::Buy, 60).await.unwrap();
    assert!(h.submit("SOLUSDT", Side::Buy, 20).await.is_err());
}

#[tokio::test]
async fn queue_failure_does_not_leak_reservations() {
    let mut h = Harness::new(1);
    let first = h.submit("BTCUSDT", Side::Buy, 30).await.unwrap();
    assert!(h.submit("ETHUSDT", Side::Buy, 30).await.is_err());
    assert_eq!(h.worker.receive_envelopes().len(), 1);
    h.worker.reject_unsent_intent(&first.0, "not sent").await;
    h.engine.lock().await.tick().unwrap();
    h.submit("SOLUSDT", Side::Buy, 90).await.unwrap();
}

#[tokio::test]
async fn restored_queue_and_missing_valuation_cannot_resume_on_empty_snapshot() {
    let mut source = Harness::new(8);
    source.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let queued = source.engine.lock().await.export_risk_reservations();
    let recovered = Harness::new(8);
    recovered
        .engine
        .lock()
        .await
        .import_risk_reservations(queued)
        .unwrap();
    recovered.engine.lock().await.resume_trading();
    assert_eq!(
        recovered.engine.lock().await.trading_mode(),
        engine::TradingMode::Paused
    );
    assert!(!recovered.reconcile(true).await.healthy);

    source.bind_next().await;
    let engine = source.engine.lock().await;
    let oms = engine.export_oms_checkpoint();
    let metadata = engine.export_risk_reservations();
    drop(engine);
    let recovered = Harness::new(8);
    recovered
        .engine
        .lock()
        .await
        .import_oms_checkpoint(oms)
        .unwrap();
    assert!(!recovered.reconcile(true).await.healthy);
    recovered
        .engine
        .lock()
        .await
        .import_risk_reservations(metadata)
        .unwrap();
    assert!(recovered.reconcile(true).await.healthy);
    recovered.engine.lock().await.resume_trading();
    assert_eq!(
        recovered.engine.lock().await.trading_mode(),
        engine::TradingMode::Normal
    );
}

#[tokio::test]
async fn pending_opposite_orders_cannot_net_away_reserved_risk() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    h.bind_next().await;
    // Pass the underlying same-symbol cooldown: the exposure gate must reject this order.
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    assert!(h.submit("BTCUSDT", Side::Sell, 60).await.is_err());
    assert!(h.worker.receive_envelopes().is_empty());
    h.submit("ETHUSDT", Side::Buy, 40).await.unwrap();
}

#[tokio::test]
async fn lower_new_order_price_cannot_discount_an_older_reservation() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 90).await.unwrap();
    h.bind_next().await;
    tokio::time::sleep(std::time::Duration::from_millis(110)).await;
    assert!(h.submit("BTCUSDT", Side::Buy, 20).await.is_err());
    assert!(h.worker.receive_envelopes().is_empty());
    h.submit("ETHUSDT", Side::Buy, 10).await.unwrap();
}

#[tokio::test]
async fn risk_manager_without_pending_contract_cannot_admit_orders() {
    let mut h = Harness::new(8);
    let config = SystemRiskConfig {
        risk_type: "Default".into(),
        global_position_limit: Decimal::from(100),
        global_notional_limit: Decimal::from(100),
        max_orders_per_second: 100,
        staleness_threshold_us: 60_000_000,
        max_drawdown_pct: 5.0,
        max_daily_loss: Decimal::from(10_000),
        ..Default::default()
    };
    h.engine
        .lock()
        .await
        .register_risk_manager_boxed(RiskManagerFactory::create_risk_manager(&config));
    // Even the first order needs the explicit gross-exposure contract.
    assert!(h.submit("BTCUSDT", Side::Buy, 30).await.is_err());
    assert!(h.worker.receive_envelopes().is_empty());
}

impl Harness {
    async fn tokenized(capacity: usize) -> Self {
        let config = SystemRiskConfig {
            risk_type: "Default".into(),
            global_position_limit: Decimal::from(100),
            global_notional_limit: Decimal::from(10_000),
            max_orders_per_second: 100,
            staleness_threshold_us: 60_000_000,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            tokenized_securities: runtime::TokenizedSecuritiesRiskConfig {
                allow_trading: true,
                evidence_max_age_us: 60_000_000,
                max_notional_per_symbol: Decimal::from(100),
                max_asset_class_notional: Decimal::from(100),
                min_top_depth_usd: Decimal::from(10_000),
                max_spread_bps: Decimal::from(10),
                freeze_on_corporate_action: true,
                restricted_jurisdictions: vec![],
            },
            ..Default::default()
        };
        let mut account = ports::AccountView {
            account_id: Some(AccountId("account".into())),
            cash_balance: Decimal::from(1_000),
            ..Default::default()
        };
        for symbol in ["TSLAUSDT", "AAPLUSDT"] {
            let symbol = Symbol::new(symbol);
            let venue = VenueId::BINANCE_TOKENIZED_SECURITIES;
            account.tokenized_securities_attestations.insert(
                hft_core::InstrumentKey::tokenized_security_spot(symbol.clone(), venue),
                ports::TokenizedSecuritiesRuntimeAttestation {
                    account_id: AccountId("account".into()),
                    venue,
                    product_type: hft_core::ProductType::TokenizedSecuritySpot,
                    symbol,
                    source_id: "licensed-reference-feed".into(),
                    observed_at: now_micros(),
                    jurisdiction: Some("AE".into()),
                    account_eligible: true,
                    account_capability: hft_core::AccountCapability {
                        can_trade_tokenized_securities: true,
                        jurisdiction: Some("AE".into()),
                        kyc_level: Some("verified".into()),
                        ..Default::default()
                    },
                    account_symbol_notional: Decimal::ZERO,
                    account_asset_class_notional: Decimal::ZERO,
                    corporate_action_active: false,
                    top_depth_usd: Decimal::from(20_000),
                    spread_bps: Decimal::from(5),
                },
            );
        }
        let mut portfolio = portfolio_core::Portfolio::new();
        portfolio.publish_account_readback(account).unwrap();
        let harness = Self::new_with_portfolio(capacity, portfolio);
        let mut engine = harness.engine.lock().await;
        engine.register_risk_manager_boxed(RiskManagerFactory::create_strategy_aware_risk_manager(
            &config,
        ));
        engine.add_venue_spec(
            VenueId::BINANCE_TOKENIZED_SECURITIES,
            ports::VenueSpec::binance_spot(),
        );
        engine.set_execution_price_protection(HashMap::from([
            (
                VenueId::BINANCE,
                ports::ExecutionPriceProtection::VenueQuote,
            ),
            (
                VenueId::BINANCE_TOKENIZED_SECURITIES,
                ports::ExecutionPriceProtection::VenueQuote,
            ),
        ]));
        drop(engine);
        harness
    }

    async fn submit_tokenized(&mut self, symbol: &str) -> HftResult<()> {
        let now = now_micros();
        let intent = OrderIntent::crypto_spot(
            Symbol::new(symbol),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::from(60))),
            TimeInForce::GTC,
            "reservation-test".into(),
            Some(VenueId::BINANCE_TOKENIZED_SECURITIES),
        )
        .tokenized_security_spot(hft_core::ComplianceContext::default());
        self.engine.lock().await.submit_order_intent_envelope(
            OrderIntentEnvelope::new(intent, OrderIntentLifecycle::new(now, u64::MAX))
                .with_account_id(AccountId("account".into())),
            now,
            None,
        )
    }
}

#[tokio::test]
async fn tokenized_special_limit_accumulates_pending_exposure_across_ticks() {
    let mut h = Harness::tokenized(8).await;
    h.submit_tokenized("TSLAUSDT").await.unwrap();
    h.bind_next().await;
    assert!(h.submit_tokenized("AAPLUSDT").await.is_err());
    assert!(h.worker.receive_envelopes().is_empty());
}

#[tokio::test]
async fn crypto_pending_exposure_does_not_consume_the_tokenized_special_limit() {
    let mut h = Harness::tokenized(8).await;
    h.submit("BTCUSDT", Side::Buy, 500).await.unwrap();
    h.bind_next().await;
    h.submit_tokenized("TSLAUSDT").await.unwrap();
    assert_eq!(h.worker.receive_envelopes().len(), 1);
}

#[tokio::test]
async fn restoring_empty_owners_without_risk_checkpoint_cannot_prove_no_lost_queue() {
    let h = Harness::new(8);
    let oms = h.engine.lock().await.export_oms_checkpoint();
    h.engine.lock().await.import_oms_checkpoint(oms).unwrap();
    assert!(!h.reconcile(true).await.healthy);
    h.engine.lock().await.resume_trading();
    assert_eq!(
        h.engine.lock().await.trading_mode(),
        engine::TradingMode::Paused
    );
    h.engine
        .lock()
        .await
        .import_risk_reservations(engine::RiskReservationCheckpoint::default())
        .unwrap();
    assert!(h.reconcile(true).await.healthy);
}

#[tokio::test]
async fn mismatched_terminal_oms_cannot_discard_admitted_risk_on_restore() {
    let mut source = Harness::new(8);
    source.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let id = source.bind_next().await;
    let engine = source.engine.lock().await;
    let mut orders = engine.export_oms_state();
    let reservations = engine.export_risk_reservations();
    drop(engine);
    let order = orders.get_mut(&id).unwrap();
    order.status = OrderStatus::Rejected;
    order.symbol = Symbol::new("ETHUSDT");
    let recovered = Harness::new(8);
    let mut engine = recovered.engine.lock().await;
    engine.import_oms_state(orders);
    engine.import_risk_reservations(reservations).unwrap();
    drop(engine);
    assert!(!recovered.reconcile(true).await.healthy);
    recovered.engine.lock().await.resume_trading();
    assert_eq!(
        recovered.engine.lock().await.trading_mode(),
        engine::TradingMode::Paused
    );
}

impl Harness {
    async fn enable_portfolio_risk(&mut self) {
        let config = SystemRiskConfig {
            risk_type: "Default".into(),
            global_position_limit: Decimal::from(100),
            global_notional_limit: Decimal::from(1_000),
            max_orders_per_second: 100,
            staleness_threshold_us: 60_000_000,
            max_daily_loss: Decimal::from(10_000),
            max_drawdown_pct: 5.0,
            ..Default::default()
        };
        let portfolios = ["alpha-a", "alpha-b"]
            .into_iter()
            .map(|strategy| runtime::PortfolioSpec {
                name: strategy.into(),
                strategies: vec![strategy.into()],
                max_notional: Some(Decimal::from(100)),
                ..Default::default()
            })
            .collect();
        let strategies: Vec<_> = ["alpha-a", "alpha-b", "unassigned"]
            .into_iter()
            .map(|name| runtime::StrategyConfig {
                name: name.into(),
                strategy_type: runtime::StrategyType::Imbalance,
                symbols: vec![],
                params: runtime::StrategyParams::Imbalance {
                    obi_threshold: 0.5,
                    lot: Decimal::ONE,
                    top_levels: 1,
                },
                risk_limits: Default::default(),
            })
            .collect();
        self.engine
            .lock()
            .await
            .register_risk_manager_boxed(Box::new(runtime::PortfolioBudgetRiskManager::new(
                RiskManagerFactory::create_strategy_aware_risk_manager(&config),
                portfolios,
                &strategies,
            )));
    }
}

#[tokio::test]
async fn pending_orders_reserve_only_their_declared_portfolio() {
    let mut h = Harness::new(8);
    h.enable_portfolio_risk().await;
    h.submit_for_strategy("BTCUSDT", Side::Buy, 60, "alpha-a")
        .await
        .unwrap();
    h.bind_next().await;
    h.submit_for_strategy("ETHUSDT", Side::Buy, 60, "alpha-b")
        .await
        .unwrap();
    h.bind_next().await;
    assert!(h
        .submit_for_strategy("SOLUSDT", Side::Buy, 60, "alpha-a")
        .await
        .is_err());
    assert!(h.worker.receive_envelopes().is_empty());
}

#[tokio::test]
async fn declared_unassigned_strategy_does_not_consume_a_portfolio_budget() {
    let mut h = Harness::new(8);
    h.enable_portfolio_risk().await;
    h.submit_for_strategy("BTCUSDT", Side::Buy, 60, "unassigned")
        .await
        .unwrap();
    h.bind_next().await;
    h.submit_for_strategy("ETHUSDT", Side::Buy, 60, "alpha-a")
        .await
        .unwrap();
    assert_eq!(h.worker.receive_envelopes().len(), 1);
}

#[tokio::test]
async fn unknown_strategy_cannot_be_assumed_outside_a_portfolio_budget() {
    let mut h = Harness::new(8);
    h.enable_portfolio_risk().await;
    assert!(h
        .submit_for_strategy("XRPUSDT", Side::Buy, 60, "")
        .await
        .is_err());
    h.submit_for_strategy("BTCUSDT", Side::Buy, 60, "unknown")
        .await
        .unwrap();
    h.bind_next().await;
    assert!(h
        .submit_for_strategy("ETHUSDT", Side::Buy, 60, "alpha-a")
        .await
        .is_err());
    assert!(h.worker.receive_envelopes().is_empty());
}

#[tokio::test]
async fn foreign_order_without_client_identity_blocks_new_admission() {
    let mut h = Harness::new(8);
    let foreign = OrderId("foreign-without-client-id".into());
    h.event(ExecutionEvent::OrderNew {
        order_id: foreign.clone(),
        client_order_id: None,
        account_id: Some(AccountId("account".into())),
        symbol: Symbol::new("BTCUSDT"),
        side: Side::Buy,
        quantity: Quantity(Decimal::ONE),
        requested_price: Some(Price(Decimal::from(60))),
        arrival_price: None,
        timestamp: now_micros(),
        venue: Some(VenueId::BINANCE),
        strategy_id: "foreign".into(),
    })
    .await;
    assert_eq!(
        h.engine.lock().await.export_oms_state()[&foreign].status,
        OrderStatus::New
    );
    assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err(),
        "the foreign open order lacks an admission binding and must not be treated as zero exposure");
    assert!(h.worker.receive_envelopes().is_empty());
}

#[tokio::test]
async fn cancelled_order_still_open_at_provider_keeps_its_reservation() {
    for changed_venue_id in [false, true] {
        let mut h = Harness::new(8);
        h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
        let order = h.bind_next().await;
        let mut stale = h.snapshot(true).await;
        if changed_venue_id {
            stale.clients[0].open_orders.as_mut().unwrap()[0].order_id =
                OrderId("venue-renamed-order".into());
        }
        h.event(ExecutionEvent::OrderCanceled {
            order_id: order,
            timestamp: now_micros(),
        })
        .await;
        assert!(
            !h.reconcile_snapshot(stale).await.healthy,
            "a provider snapshot still showing the cancelled order open cannot release its risk"
        );
        assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err());
        assert!(h.reconcile(true).await.healthy);
        h.submit("SOLUSDT", Side::Buy, 60).await.unwrap();
    }
}

#[tokio::test]
async fn each_owner_restore_requires_a_new_risk_checkpoint() {
    for owner in ["oms-state", "oms-checkpoint", "portfolio"] {
        let h = Harness::new(8);
        h.engine
            .lock()
            .await
            .import_risk_reservations(engine::RiskReservationCheckpoint::default())
            .unwrap();
        assert!(h.reconcile(true).await.healthy);
        let mut engine = h.engine.lock().await;
        engine.resume_trading();
        assert_eq!(engine.trading_mode(), engine::TradingMode::Normal);
        let oms_state = engine.export_oms_state();
        let oms_checkpoint = engine.export_oms_checkpoint();
        let portfolio = engine.export_portfolio_state();
        match owner {
            "oms-state" => engine.import_oms_state(oms_state),
            "oms-checkpoint" => engine.import_oms_checkpoint(oms_checkpoint).unwrap(),
            "portfolio" => engine.import_portfolio_state(portfolio).unwrap(),
            _ => unreachable!(),
        }
        drop(engine);
        assert!(
            !h.reconcile(true).await.healthy,
            "{owner} reused a risk checkpoint from an earlier restore"
        );
        h.engine
            .lock()
            .await
            .import_risk_reservations(engine::RiskReservationCheckpoint::default())
            .unwrap();
        assert!(h.reconcile(true).await.healthy);
    }
}

#[tokio::test]
async fn unapproved_order_modification_latches_before_another_admission() {
    for (quantity, price) in [
        (Some(Quantity(Decimal::from(2))), None),
        (None, Some(Price(Decimal::from(80)))),
    ] {
        let mut h = Harness::new(8);
        h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
        let order_id = h.bind_next().await;
        h.event(ExecutionEvent::OrderModified {
            order_id: order_id.clone(),
            new_quantity: quantity,
            new_price: price,
            timestamp: now_micros(),
        })
        .await;
        let mut engine = h.engine.lock().await;
        assert_eq!(
            engine.trading_mode(),
            engine::TradingMode::Paused,
            "a venue-confirmed risk increase must latch without waiting for another intent"
        );
        let actual = &engine.export_oms_state()[&order_id];
        assert_eq!(actual.qty, quantity.unwrap_or(Quantity(Decimal::ONE)));
        assert_eq!(actual.limit_price, price.or(Some(Price(Decimal::from(60)))));
        engine.resume_trading();
        assert_eq!(engine.trading_mode(), engine::TradingMode::Paused);
        let oms = engine.export_oms_checkpoint();
        assert!(!oms.reconciliation_exceptions.is_empty());
        let portfolio = engine.export_portfolio_state();
        let risk = engine.export_risk_reservations();
        drop(engine);
        let recovered = Harness::new(8);
        let mut engine = recovered.engine.lock().await;
        engine.import_oms_checkpoint(oms).unwrap();
        engine.import_portfolio_state(portfolio).unwrap();
        engine.import_risk_reservations(risk).unwrap();
        drop(engine);
        assert!(
            !recovered.reconcile(true).await.healthy,
            "a fresh restore cannot turn the unapproved venue amendment into a grant"
        );
    }
}

#[tokio::test]
async fn replayed_venue_rejection_cannot_delete_another_queued_client_identity() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let old = h.bind_next_as(Some("collision")).await;
    let rejection = ExecutionEvent::OrderReject {
        order_id: old,
        reason: "rejected by venue".into(),
        timestamp: now_micros(),
    };
    h.event(rejection.clone()).await;
    assert!(h.reconcile(true).await.healthy);
    let now = now_micros();
    let mut queued = OrderIntentEnvelope::new(
        OrderIntent::crypto_spot(
            Symbol::new("ETHUSDT"),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::from(60))),
            TimeInForce::GTC,
            "reservation-test".into(),
            Some(VenueId::BINANCE),
        ),
        OrderIntentLifecycle::new(now, u64::MAX),
    )
    .with_account_id(AccountId("account".into()));
    queued.client_order_id = "collision".into();
    h.engine
        .lock()
        .await
        .submit_order_intent_envelope(queued, now, None)
        .unwrap();
    h.event(rejection).await;
    assert!(
        h.submit("SOLUSDT", Side::Buy, 60).await.is_err(),
        "venue order IDs and queued client IDs are different namespaces"
    );
    assert_eq!(h.worker.receive_envelopes().len(), 1);
}

#[tokio::test]
async fn halt_still_accounts_for_a_previously_admitted_inflight_order() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 30).await.unwrap();
    let first = h.bind_next().await;
    h.submit("ETHUSDT", Side::Buy, 30).await.unwrap();
    let mut pending = h.worker.receive_envelopes();
    let inflight = pending.pop().unwrap();
    let inflight_id = OrderId("inflight-at-halt".into());
    h.event(ExecutionEvent::OrderModified {
        order_id: first,
        new_quantity: Some(Quantity(Decimal::from(2))),
        new_price: None,
        timestamp: now_micros(),
    })
    .await;
    h.event(ExecutionEvent::OrderNew {
        order_id: inflight_id.clone(),
        client_order_id: Some(inflight.client_order_id),
        account_id: inflight.account_id,
        symbol: inflight.intent.symbol,
        side: inflight.intent.side,
        quantity: inflight.intent.quantity,
        requested_price: inflight.intent.price,
        arrival_price: None,
        timestamp: now_micros(),
        venue: inflight.intent.target_venue,
        strategy_id: inflight.intent.strategy_id,
    })
    .await;
    h.event(ExecutionEvent::Fill {
        order_id: inflight_id.clone(),
        price: Price(Decimal::from(30)),
        quantity: Quantity(Decimal::new(5, 1)),
        timestamp: now_micros(),
        fill_id: "inflight-fill".into(),
    })
    .await;
    let engine = h.engine.lock().await;
    assert_eq!(engine.trading_mode(), engine::TradingMode::Paused);
    assert!(engine.reconciliation_halted());
    assert_eq!(
        engine.export_oms_state()[&inflight_id].cum_qty,
        Quantity(Decimal::new(5, 1))
    );
    assert_eq!(
        engine.get_account_view().positions[&Symbol::new("ETHUSDT")].quantity,
        Quantity(Decimal::new(5, 1))
    );
    assert!(
        serde_json::to_value(engine.export_risk_reservations()).unwrap()["orders"]
            .get(&inflight_id.0)
            .is_some()
    );
}

#[tokio::test]
async fn risk_reducing_modification_preserves_remaining_budget_without_halting() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Sell, 60).await.unwrap();
    let order = h.bind_next().await;
    h.event(ExecutionEvent::OrderModified {
        order_id: order,
        new_quantity: Some(Quantity(Decimal::new(5, 1))),
        new_price: Some(Price(Decimal::from(120))),
        timestamp: now_micros(),
    })
    .await;
    assert_eq!(
        h.engine.lock().await.trading_mode(),
        engine::TradingMode::Normal
    );
    h.submit("ETHUSDT", Side::Buy, 40).await.unwrap();
}

#[tokio::test]
async fn ambiguous_unsent_rejection_cannot_release_an_existing_venue_order() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let existing = h.bind_next_as(Some("collision")).await;
    let now = now_micros();
    let mut queued = OrderIntentEnvelope::new(
        OrderIntent::crypto_spot(
            Symbol::new("ETHUSDT"),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::from(30))),
            TimeInForce::GTC,
            "reservation-test".into(),
            Some(VenueId::BINANCE),
        ),
        OrderIntentLifecycle::new(now, u64::MAX),
    )
    .with_account_id(AccountId("account".into()));
    queued.client_order_id = existing.0.clone();
    h.engine
        .lock()
        .await
        .submit_order_intent_envelope(queued, now, None)
        .unwrap();
    let before = h.engine.lock().await.export_risk_reservations();
    h.event(ExecutionEvent::OrderReject {
        order_id: existing.clone(),
        reason: "execution intake disabled by control".into(),
        timestamp: now_micros(),
    })
    .await;
    assert_eq!(
        h.engine.lock().await.export_oms_state()[&existing].status,
        OrderStatus::New,
        "an ambiguous unsent rejection must not mutate the live venue order"
    );
    assert_eq!(h.engine.lock().await.export_risk_reservations(), before);
    assert_eq!(
        h.engine.lock().await.trading_mode(),
        engine::TradingMode::Paused
    );
    assert!(h.submit("SOLUSDT", Side::Buy, 60).await.is_err());
}

#[tokio::test]
async fn later_amendment_cannot_discount_an_earlier_higher_risk_price() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Sell, 60).await.unwrap();
    let order = h.bind_next().await;
    h.event(ExecutionEvent::OrderModified {
        order_id: order.clone(),
        new_quantity: Some(Quantity(Decimal::new(5, 1))),
        new_price: Some(Price(Decimal::from(120))),
        timestamp: now_micros(),
    })
    .await;
    h.event(ExecutionEvent::OrderModified {
        order_id: order.clone(),
        new_quantity: None,
        new_price: Some(Price(Decimal::from(60))),
        timestamp: now_micros(),
    })
    .await;
    assert!(
        h.submit("ETHUSDT", Side::Buy, 60).await.is_err(),
        "a delayed fill at the prior higher price still consumes the reservation"
    );
    h.event(ExecutionEvent::Fill {
        order_id: order,
        price: Price(Decimal::from(120)),
        quantity: Quantity(Decimal::new(4, 1)),
        timestamp: now_micros(),
        fill_id: "prior-amend-fill".into(),
    })
    .await;
    h.submit("SOLUSDT", Side::Buy, 40).await.unwrap();
}

#[tokio::test]
async fn replacing_portfolio_owner_cannot_erase_existing_exposure() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let order = h.bind_next().await;
    h.event(ExecutionEvent::Fill {
        order_id: order,
        price: Price(Decimal::from(60)),
        quantity: Quantity(Decimal::ONE),
        timestamp: now_micros(),
        fill_id: "existing-exposure".into(),
    })
    .await;
    let authoritative = h.snapshot(true).await;
    h.engine.lock().await.set_portfolio_manager(Box::new(
        portfolio_core::Portfolio::with_cash_balance(Decimal::from(1_000)),
    ));
    assert!(
        h.submit("ETHUSDT", Side::Buy, 60).await.is_err(),
        "replacement owner requires recovery before a new approval"
    );
    assert_eq!(
        h.engine.lock().await.trading_mode(),
        engine::TradingMode::Paused
    );
    assert!(!h.reconcile_snapshot(authoritative).await.healthy);
}

#[tokio::test]
async fn restored_risk_price_history_must_match_the_exact_oms_revision() {
    let mut source = Harness::new(8);
    source.submit("BTCUSDT", Side::Sell, 60).await.unwrap();
    let order = source.bind_next().await;
    let old = source.engine.lock().await.export_risk_reservations();
    source
        .event(ExecutionEvent::OrderModified {
            order_id: order.clone(),
            new_quantity: Some(Quantity(Decimal::new(5, 1))),
            new_price: Some(Price(Decimal::from(120))),
            timestamp: now_micros(),
        })
        .await;
    source
        .event(ExecutionEvent::OrderModified {
            order_id: order,
            new_quantity: None,
            new_price: Some(Price(Decimal::from(60))),
            timestamp: now_micros(),
        })
        .await;
    let engine = source.engine.lock().await;
    let oms = engine.export_oms_checkpoint();
    let portfolio = engine.export_portfolio_state();
    let current = engine.export_risk_reservations();
    drop(engine);
    for (risk, should_reconcile) in [(old, false), (current, true)] {
        let mut recovered = Harness::new(8);
        let mut engine = recovered.engine.lock().await;
        engine.import_oms_checkpoint(oms.clone()).unwrap();
        engine.import_portfolio_state(portfolio.clone()).unwrap();
        engine.import_risk_reservations(risk).unwrap();
        drop(engine);
        assert_eq!(
            recovered.reconcile(true).await.healthy,
            should_reconcile,
            "old risk metadata cannot erase price history from a newer OMS revision"
        );
        if should_reconcile {
            recovered.engine.lock().await.resume_trading();
            assert!(recovered.submit("ETHUSDT", Side::Buy, 60).await.is_err());
        } else {
            let order_id = oms.orders.keys().next().unwrap().clone();
            recovered
                .event(ExecutionEvent::OrderModified {
                    order_id,
                    new_quantity: None,
                    new_price: Some(Price(Decimal::from(60))),
                    timestamp: now_micros(),
                })
                .await;
            assert!(
                !recovered.reconcile(true).await.healthy,
                "a later modification cannot fill a gap in restored price history"
            );
            assert!(!recovered
                .engine
                .lock()
                .await
                .export_oms_checkpoint()
                .reconciliation_exceptions
                .is_empty());
        }
    }
}

#[tokio::test]
async fn saturated_oms_revision_cannot_be_treated_as_a_verified_amendment() {
    let mut source = Harness::new(8);
    source.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let order_id = source.bind_next().await;
    let engine = source.engine.lock().await;
    let original_oms = engine.export_oms_checkpoint();
    let portfolio = engine.export_portfolio_state();
    let original_risk = serde_json::to_value(engine.export_risk_reservations()).unwrap();
    drop(engine);
    for revision in [u32::MAX - 1, u32::MAX] {
        let mut oms = original_oms.clone();
        oms.orders.get_mut(&order_id).unwrap().revision = revision;
        let mut risk = original_risk.clone();
        risk["orders"][&order_id.0]["observed_revision"] = serde_json::json!(revision);
        let risk = serde_json::from_value(risk).unwrap();
        let mut recovered = Harness::new(8);
        let mut engine = recovered.engine.lock().await;
        engine.import_oms_checkpoint(oms).unwrap();
        engine.import_portfolio_state(portfolio.clone()).unwrap();
        engine.import_risk_reservations(risk).unwrap();
        drop(engine);
        assert_eq!(
            recovered.reconcile(true).await.healthy,
            revision < u32::MAX,
            "a saturated counter cannot prove matching amendment history on cold restore"
        );
        if revision < u32::MAX {
            recovered.engine.lock().await.resume_trading();
            recovered
                .event(ExecutionEvent::OrderModified {
                    order_id: order_id.clone(),
                    new_quantity: None,
                    new_price: Some(Price(Decimal::from(60))),
                    timestamp: now_micros(),
                })
                .await;
        }
        assert_eq!(
            recovered.engine.lock().await.trading_mode(),
            engine::TradingMode::Paused
        );
        assert!(recovered.submit("ETHUSDT", Side::Buy, 40).await.is_err());
    }
}

#[tokio::test]
async fn external_order_without_account_identity_requires_reconciliation() {
    let mut h = Harness::new(8);
    let order_id = OrderId("missing-account".into());
    h.event(ExecutionEvent::OrderNew {
        order_id: order_id.clone(),
        client_order_id: None,
        account_id: None,
        symbol: Symbol::new("BTCUSDT"),
        side: Side::Buy,
        quantity: Quantity(Decimal::ONE),
        requested_price: Some(Price(Decimal::from(60))),
        arrival_price: None,
        timestamp: now_micros(),
        venue: Some(VenueId::BINANCE),
        strategy_id: "external".into(),
    })
    .await;
    let engine = h.engine.lock().await;
    assert_eq!(engine.trading_mode(), engine::TradingMode::Paused);
    assert!(engine.reconciliation_halted());
    assert_eq!(
        engine.export_oms_checkpoint().reconciliation_exceptions[0].order_id,
        Some(order_id)
    );
    drop(engine);
    assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err());
}

#[tokio::test]
async fn unknown_order_lifecycle_evidence_always_latches_intake() {
    let order_id = OrderId("unknown-venue-order".into());
    for event in [
        ExecutionEvent::OrderAck {
            order_id: order_id.clone(),
            timestamp: now_micros(),
        },
        ExecutionEvent::OrderCanceled {
            order_id: order_id.clone(),
            timestamp: now_micros(),
        },
        ExecutionEvent::OrderCompleted {
            order_id,
            final_price: Price(Decimal::from(60)),
            total_filled: Quantity(Decimal::ONE),
            timestamp: now_micros(),
        },
    ] {
        let mut h = Harness::new(8);
        h.event(event).await;
        let engine = h.engine.lock().await;
        assert!(!engine
            .export_oms_checkpoint()
            .reconciliation_exceptions
            .is_empty());
        assert_eq!(engine.trading_mode(), engine::TradingMode::Paused);
        assert!(engine.reconciliation_halted());
        drop(engine);
        assert!(h.submit("ETHUSDT", Side::Buy, 60).await.is_err());
    }
}

#[tokio::test]
async fn failed_execution_report_keeps_later_fills_and_withholds_stream_ack() {
    for marker_first in [false, true] {
        let mut h = Harness::new(8);
        h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
        let order_id = h.bind_next().await;
        let marker = ExecutionEvent::ExecutionStreamSynchronized {
            stream_id: 31,
            connected: true,
            timestamp: now_micros(),
        };
        if marker_first {
            h.worker.send_event(marker.clone()).unwrap();
        }
        for event in [
            ExecutionEvent::Fill {
                order_id: OrderId("unknown-prior-order".into()),
                price: Price(Decimal::from(60)),
                quantity: Quantity(Decimal::ONE),
                timestamp: now_micros(),
                fill_id: "unknown-fill".into(),
            },
            ExecutionEvent::Fill {
                order_id: order_id.clone(),
                price: Price(Decimal::from(60)),
                quantity: Quantity(Decimal::new(5, 1)),
                timestamp: now_micros(),
                fill_id: "valid-later-fill".into(),
            },
        ] {
            h.worker.send_event(event).unwrap();
        }
        if !marker_first {
            h.worker.send_event(marker).unwrap();
        }
        let mut engine = h.engine.lock().await;
        assert!(engine.tick().is_err());
        assert_eq!(
            engine.export_oms_state()[&order_id].cum_qty,
            Quantity(Decimal::new(5, 1)),
            "a prior report error must not discard a later authoritative fill"
        );
        assert_eq!(
            engine.get_account_view().positions[&Symbol::new("BTCUSDT")].quantity,
            Quantity(Decimal::new(5, 1))
        );
        assert!(engine.reconciliation_halted());
        assert_eq!(engine.trading_mode(), engine::TradingMode::Paused);
        assert_eq!(
            h.worker.try_receive_applied_execution_stream(),
            None,
            "a failing batch cannot acknowledge a marker even when it appeared first"
        );
        engine.tick().unwrap();
        assert_eq!(
            engine.export_oms_state()[&order_id].cum_qty,
            Quantity(Decimal::new(5, 1))
        );
        h.worker
            .send_event(ExecutionEvent::ExecutionStreamSynchronized {
                stream_id: 32,
                connected: true,
                timestamp: now_micros(),
            })
            .unwrap();
        engine.tick().unwrap();
        assert_eq!(
            h.worker.try_receive_applied_execution_stream(),
            None,
            "a later marker cannot bypass the unresolved report failure"
        );
        assert_eq!(
            engine
                .export_oms_checkpoint()
                .reconciliation_exceptions
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn submitted_rejection_retains_exposure_until_terminal_readback() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let order_id = h.bind_next().await;
    let open_snapshot = h.snapshot(true).await;
    h.event(ExecutionEvent::OrderReject {
        order_id: order_id.clone(),
        reason: "venue reported rejected".into(),
        timestamp: now_micros(),
    })
    .await;
    assert!(
        h.submit("ETHUSDT", Side::Buy, 60).await.is_err(),
        "submitted rejection is not proof that no delayed fill exists"
    );
    assert!(
        !h.reconcile_snapshot(open_snapshot).await.healthy,
        "a rejected order still open remotely cannot release its reservation"
    );
    h.event(ExecutionEvent::Fill {
        order_id,
        price: Price(Decimal::from(60)),
        quantity: Quantity(Decimal::new(5, 1)),
        timestamp: now_micros(),
        fill_id: "delayed-rejected-fill".into(),
    })
    .await;
    assert!(h.submit("SOLUSDT", Side::Buy, 45).await.is_err());
    assert!(!h.reconcile(false).await.healthy);
    assert!(h.reconcile(true).await.healthy);
    h.submit("ADAUSDT", Side::Buy, 60).await.unwrap();
}

#[tokio::test]
async fn typed_unsent_rejection_releases_only_the_queued_identity() {
    let mut h = Harness::new(8);
    h.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
    let existing = h.bind_next_as(Some("collision")).await;
    let now = now_micros();
    let mut queued = OrderIntentEnvelope::new(
        OrderIntent::crypto_spot(
            Symbol::new("ETHUSDT"),
            Side::Buy,
            Quantity(Decimal::ONE),
            OrderType::Limit,
            Some(Price(Decimal::from(30))),
            TimeInForce::GTC,
            "reservation-test".into(),
            Some(VenueId::BINANCE),
        ),
        OrderIntentLifecycle::new(now, u64::MAX),
    )
    .with_account_id(AccountId("account".into()));
    queued.client_order_id = existing.0.clone();
    h.engine
        .lock()
        .await
        .submit_order_intent_envelope(queued, now, None)
        .unwrap();
    let unsent = h.worker.receive_envelopes().pop().unwrap();
    h.worker
        .reject_unsent_intent(
            &unsent.client_order_id,
            "worker rejected before adapter call",
        )
        .await;
    h.engine.lock().await.tick().unwrap();
    assert_eq!(
        h.engine.lock().await.export_oms_state()[&existing].status,
        OrderStatus::New
    );
    assert!(!h.engine.lock().await.reconciliation_halted());
    assert!(h.submit("SOLUSDT", Side::Buy, 60).await.is_err());
    h.submit("BNBUSDT", Side::Buy, 40).await.unwrap();
}

async fn assert_stale_terminal_checkpoint_requires_absent_readback(empty_checkpoint: bool) {
    for rejected in [false, true] {
        for partial in [false, true] {
            for changed_venue_id in [false, true] {
                let mut source = Harness::new(8);
                let empty = source.engine.lock().await.export_risk_reservations();
                source.submit("BTCUSDT", Side::Buy, 60).await.unwrap();
                let order_id = source.bind_next().await;
                if partial {
                    source
                        .event(ExecutionEvent::Fill {
                            order_id: order_id.clone(),
                            price: Price(Decimal::from(60)),
                            quantity: Quantity(Decimal::new(2, 1)),
                            timestamp: now_micros(),
                            fill_id: "before-terminal".into(),
                        })
                        .await;
                }
                let before_terminal = source.engine.lock().await.export_risk_reservations();
                let mut still_open = source.snapshot(true).await;
                if changed_venue_id {
                    still_open.clients[0].open_orders.as_mut().unwrap()[0].order_id =
                        OrderId("changed-venue-identity".into());
                }
                source
                    .event(if rejected {
                        ExecutionEvent::OrderReject {
                            order_id: order_id.clone(),
                            reason: "venue reject".into(),
                            timestamp: now_micros(),
                        }
                    } else {
                        ExecutionEvent::OrderCanceled {
                            order_id: order_id.clone(),
                            timestamp: now_micros(),
                        }
                    })
                    .await;
                let absent = source.snapshot(true).await;
                let owner = source.engine.lock().await;
                let oms = owner.export_oms_checkpoint();
                let portfolio = owner.export_portfolio_state();
                drop(owner);
                let mut recovered = Harness::new(8);
                let mut engine = recovered.engine.lock().await;
                engine.import_oms_checkpoint(oms).unwrap();
                engine.import_portfolio_state(portfolio).unwrap();
                engine
                    .import_risk_reservations(if empty_checkpoint {
                        empty
                    } else {
                        before_terminal
                    })
                    .unwrap();
                drop(engine);
                assert!(!recovered.reconcile_snapshot(still_open).await.healthy,
                    "terminal OMS with stale risk checkpoint cannot accept provider-open order: empty={empty_checkpoint}, rejected={rejected}, partial={partial}, alias={changed_venue_id}");
                recovered.engine.lock().await.resume_trading();
                assert_eq!(
                    recovered.engine.lock().await.trading_mode(),
                    engine::TradingMode::Paused
                );
                assert!(recovered.submit("ETHUSDT", Side::Buy, 60).await.is_err());
                assert!(recovered.reconcile_snapshot(absent).await.healthy,
                    "complete order absence, holdings and fill evidence can settle terminal exposure");
                recovered.engine.lock().await.resume_trading();
                recovered.submit("ETHUSDT", Side::Buy, 60).await.unwrap();
            }
        }
    }
}

#[tokio::test]
async fn terminal_oms_cannot_use_a_pre_terminal_risk_checkpoint_while_provider_is_open() {
    assert_stale_terminal_checkpoint_requires_absent_readback(false).await;
}

#[tokio::test]
async fn terminal_oms_cannot_use_an_earlier_empty_risk_checkpoint_while_provider_is_open() {
    assert_stale_terminal_checkpoint_requires_absent_readback(true).await;
}

#[tokio::test]
async fn reduced_quantity_cannot_offset_an_adverse_approved_execution_price() {
    for (side, new_price) in [(Side::Buy, 120), (Side::Sell, 30)] {
        let mut h = Harness::new(8);
        h.submit("BTCUSDT", side, 60).await.unwrap();
        let order_id = h.bind_next().await;
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(h.engine.clone(), Some(worker_tx), true);
        let error = control
            .replace_order(
                order_id.clone(),
                Symbol::new("BTCUSDT"),
                Some(Quantity(Decimal::new(5, 1))),
                Some(Price(Decimal::from(new_price))),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("approved execution price bound"));
        assert!(
            worker_rx.try_recv().is_err(),
            "unapproved replacement must not reach the adapter"
        );
        h.event(ExecutionEvent::OrderModified {
            order_id,
            new_quantity: Some(Quantity(Decimal::new(5, 1))),
            new_price: Some(Price(Decimal::from(new_price))),
            timestamp: now_micros(),
        })
        .await;
        assert_eq!(
            h.engine.lock().await.trading_mode(),
            engine::TradingMode::Paused,
            "an adverse execution price violates approval even with lower total notional"
        );
        assert!(h.submit("ETHUSDT", Side::Buy, 40).await.is_err());
    }
}
