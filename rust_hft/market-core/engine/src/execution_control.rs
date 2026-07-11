use crate::execution_worker::{
    CancelDispatchReport, CancelScope, CancelTarget, ControlCommand, WorkerReconcileSnapshot,
};
use crate::Engine;
use hft_core::{HftError, HftResult, OrderId, Symbol, VenueId};
use ports::{OrderReconciliationReport, OrderRecord, OrderStatus};
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Debug, Clone)]
pub struct RuntimeReconciliationReport {
    pub worker_snapshot: WorkerReconcileSnapshot,
    pub order_report: OrderReconciliationReport,
    pub balance_report: Option<BalanceReconciliationReport>,
    pub complete: bool,
    pub healthy: bool,
}

#[derive(Debug, Clone)]
pub struct BalanceReconciliationReport {
    pub local_equity_usd: Decimal,
    pub exchange_equity_usd: Option<Decimal>,
    pub difference_usd: Option<Decimal>,
    pub tolerance_usd: Decimal,
    pub missing_valuations: Vec<String>,
    pub client_errors: Vec<String>,
    pub complete: bool,
    pub healthy: bool,
}

/// Shared control plane for Sentinel, IPC, and gRPC.
///
/// The engine lock is held only while changing mode or taking an OMS snapshot.
/// All exchange I/O remains exclusively owned by the execution worker.
#[derive(Clone)]
pub struct ExecutionControlHandle {
    engine: Arc<Mutex<Engine>>,
    worker_tx: Option<mpsc::UnboundedSender<ControlCommand>>,
    execution_enabled: bool,
    response_timeout: Duration,
    balance_tolerance_usd: Decimal,
}

impl ExecutionControlHandle {
    pub fn new(
        engine: Arc<Mutex<Engine>>,
        worker_tx: Option<mpsc::UnboundedSender<ControlCommand>>,
        execution_enabled: bool,
    ) -> Self {
        Self {
            engine,
            worker_tx,
            execution_enabled,
            response_timeout: Duration::from_secs(5),
            balance_tolerance_usd: Decimal::ONE,
        }
    }

    pub fn with_balance_tolerance_usd(mut self, tolerance_usd: Decimal) -> Self {
        self.balance_tolerance_usd = if tolerance_usd < Decimal::ZERO {
            Decimal::ZERO
        } else {
            tolerance_usd
        };
        self
    }

    pub fn engine(&self) -> Arc<Mutex<Engine>> {
        self.engine.clone()
    }

    pub async fn pause_trading(&self) -> HftResult<()> {
        self.engine.lock().await.pause_trading();
        if self.execution_enabled {
            self.set_intake(false).await?;
        }
        Ok(())
    }

    pub async fn resume_trading(&self) -> HftResult<()> {
        if self.engine.lock().await.trading_mode() == crate::TradingMode::Emergency {
            return Err(HftError::Risk(
                "Emergency mode is sticky; restart is required".to_string(),
            ));
        }
        if self.execution_enabled {
            self.set_intake(true).await?;
        }
        let mut engine = self.engine.lock().await;
        if engine.trading_mode() == crate::TradingMode::Emergency {
            drop(engine);
            if self.execution_enabled {
                self.set_intake(false).await?;
            }
            return Err(HftError::Risk(
                "Emergency mode is sticky; restart is required".to_string(),
            ));
        }
        engine.resume_trading();
        Ok(())
    }

    pub async fn emergency_stop(&self, cancel_orders: bool) -> HftResult<CancelDispatchReport> {
        let targets = {
            let mut engine = self.engine.lock().await;
            if cancel_orders {
                engine.emergency_exit();
                collect_open_targets(&engine, None, None, None)
            } else {
                engine.pause_trading();
                Vec::new()
            }
        };

        if !cancel_orders {
            return Ok(CancelDispatchReport::default());
        }

        self.dispatch_cancellations(targets, true, CancelScope::All)
            .await
    }

    pub async fn cancel_all_orders(&self) -> HftResult<CancelDispatchReport> {
        let targets = {
            let engine = self.engine.lock().await;
            collect_open_targets(&engine, None, None, None)
        };
        self.dispatch_cancellations(targets, false, CancelScope::All)
            .await
    }

    pub async fn cancel_orders_filtered(
        &self,
        symbol: Option<Symbol>,
        venue: Option<VenueId>,
    ) -> HftResult<CancelDispatchReport> {
        let targets = {
            let engine = self.engine.lock().await;
            if venue.is_some()
                && engine.export_oms_state().values().any(|record| {
                    is_open(record.status)
                        && symbol.as_ref().is_none_or(|value| &record.symbol == value)
                        && record.venue.is_none()
                })
            {
                return Err(HftError::Execution(
                    "cannot safely apply venue filter to OMS orders without venue metadata"
                        .to_string(),
                ));
            }
            collect_open_targets(&engine, symbol.as_ref(), venue, None)
        };
        self.dispatch_cancellations(targets, false, CancelScope::Filter { symbol, venue })
            .await
    }

    pub async fn cancel_orders_for_strategy(
        &self,
        strategy_id: &str,
    ) -> HftResult<CancelDispatchReport> {
        let targets = {
            let engine = self.engine.lock().await;
            collect_open_targets(&engine, None, None, Some(strategy_id))
        };
        self.dispatch_cancellations(
            targets,
            false,
            CancelScope::Strategy(strategy_id.to_string()),
        )
        .await
    }

    pub async fn cancel_order(
        &self,
        order_id: OrderId,
        symbol: Symbol,
    ) -> HftResult<CancelDispatchReport> {
        let target = {
            let engine = self.engine.lock().await;
            let state = engine.export_oms_state();
            state.get(&order_id).map_or(
                CancelTarget {
                    order_id: order_id.clone(),
                    symbol,
                    venue: None,
                    account_id: engine.get_account_for_order(&order_id),
                },
                |record| cancel_target(&engine, record),
            )
        };
        self.dispatch_cancellations(vec![target], false, CancelScope::Explicit)
            .await
    }

    pub async fn reconcile(
        &self,
        include_balances: bool,
    ) -> HftResult<RuntimeReconciliationReport> {
        let worker_tx = self.worker_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        worker_tx
            .send(ControlCommand::Reconcile {
                include_balances,
                reply: reply_tx,
            })
            .map_err(|_| HftError::Execution("execution worker control channel closed".into()))?;
        let worker_snapshot = self.await_reply(reply_rx, "reconciliation").await?;
        let exchange_orders = worker_snapshot
            .clients
            .iter()
            .filter_map(|client| client.open_orders.as_ref().ok())
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let order_report = {
            let engine = self.engine.lock().await;
            engine.reconcile_open_orders(&exchange_orders)
        };
        let balance_report = if include_balances {
            let local_equity = self.engine.lock().await.get_account_view().equity();
            Some(reconcile_balances(
                &worker_snapshot,
                local_equity,
                self.balance_tolerance_usd,
            ))
        } else {
            None
        };
        let complete = worker_snapshot.is_complete()
            && balance_report
                .as_ref()
                .is_none_or(|balances| balances.complete);
        let healthy = complete
            && !order_report.has_discrepancies()
            && balance_report
                .as_ref()
                .is_none_or(|balances| balances.healthy);

        Ok(RuntimeReconciliationReport {
            worker_snapshot,
            order_report,
            balance_report,
            complete,
            healthy,
        })
    }

    async fn dispatch_cancellations(
        &self,
        targets: Vec<CancelTarget>,
        emergency: bool,
        scope: CancelScope,
    ) -> HftResult<CancelDispatchReport> {
        if targets.is_empty() && !self.execution_enabled {
            return Ok(CancelDispatchReport::default());
        }

        let worker_tx = self.worker_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        let command = if emergency {
            ControlCommand::EnterEmergency {
                targets,
                reply: reply_tx,
            }
        } else {
            ControlCommand::CancelOrders {
                targets,
                scope,
                reply: reply_tx,
            }
        };
        worker_tx
            .send(command)
            .map_err(|_| HftError::Execution("execution worker control channel closed".into()))?;
        self.await_reply(reply_rx, "cancellation").await
    }

    async fn set_intake(&self, enabled: bool) -> HftResult<()> {
        let worker_tx = self.worker_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        worker_tx
            .send(ControlCommand::SetIntake {
                enabled,
                reply: reply_tx,
            })
            .map_err(|_| HftError::Execution("execution worker control channel closed".into()))?;
        self.await_reply(reply_rx, "execution intake")
            .await?
            .map_err(HftError::Execution)
    }

    fn worker_sender(&self) -> HftResult<mpsc::UnboundedSender<ControlCommand>> {
        self.worker_tx.clone().ok_or_else(|| {
            HftError::Execution("execution worker control is unavailable".to_string())
        })
    }

    async fn await_reply<T>(
        &self,
        reply_rx: oneshot::Receiver<T>,
        operation: &str,
    ) -> HftResult<T> {
        tokio::time::timeout(self.response_timeout, reply_rx)
            .await
            .map_err(|_| HftError::Timeout(format!("{operation} control reply timed out")))?
            .map_err(|_| HftError::Execution(format!("{operation} control reply dropped")))
    }
}

fn reconcile_balances(
    snapshot: &WorkerReconcileSnapshot,
    local_equity_usd: Decimal,
    tolerance_usd: Decimal,
) -> BalanceReconciliationReport {
    let mut exchange_equity_usd = Decimal::ZERO;
    let mut missing_valuations = Vec::new();
    let mut client_errors = Vec::new();

    if snapshot.clients.is_empty() {
        client_errors.push("no execution clients returned balance snapshots".to_string());
    }

    for client in &snapshot.clients {
        match &client.balances {
            None => client_errors.push(format!(
                "client={} did not return a balance snapshot",
                client.client_index
            )),
            Some(Err(error)) => client_errors.push(format!(
                "client={} balance snapshot failed: {}",
                client.client_index, error
            )),
            Some(Ok(balances)) => {
                for balance in balances {
                    if let Some(value) = balance.usd_value {
                        exchange_equity_usd += value;
                    } else if balance.total != Decimal::ZERO
                        || balance.available != Decimal::ZERO
                        || balance.frozen != Decimal::ZERO
                    {
                        missing_valuations.push(format!(
                            "client={} asset={}",
                            client.client_index, balance.asset
                        ));
                    }
                }
            }
        }
    }

    let complete = client_errors.is_empty() && missing_valuations.is_empty();
    let difference_usd = complete.then(|| decimal_abs(local_equity_usd - exchange_equity_usd));
    let healthy = difference_usd.is_some_and(|difference| difference <= tolerance_usd);

    BalanceReconciliationReport {
        local_equity_usd,
        exchange_equity_usd: complete.then_some(exchange_equity_usd),
        difference_usd,
        tolerance_usd,
        missing_valuations,
        client_errors,
        complete,
        healthy,
    }
}

fn decimal_abs(value: Decimal) -> Decimal {
    if value < Decimal::ZERO {
        -value
    } else {
        value
    }
}

fn collect_open_targets(
    engine: &Engine,
    symbol: Option<&Symbol>,
    venue: Option<VenueId>,
    strategy_id: Option<&str>,
) -> Vec<CancelTarget> {
    engine
        .export_oms_state()
        .values()
        .filter(|record| is_open(record.status))
        .filter(|record| symbol.is_none_or(|value| &record.symbol == value))
        .filter(|record| venue.is_none_or(|value| record.venue == Some(value)))
        .filter(|record| {
            strategy_id.is_none_or(|value| record.strategy_id.as_deref() == Some(value))
        })
        .map(|record| cancel_target(engine, record))
        .collect()
}

fn cancel_target(engine: &Engine, record: &OrderRecord) -> CancelTarget {
    CancelTarget {
        order_id: record.order_id.clone(),
        symbol: record.symbol.clone(),
        venue: record.venue,
        account_id: engine.get_account_for_order(&record.order_id),
    }
}

fn is_open(status: OrderStatus) -> bool {
    matches!(
        status,
        OrderStatus::New
            | OrderStatus::Acknowledged
            | OrderStatus::Accepted
            | OrderStatus::PartiallyFilled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EngineConfig, TradingMode};
    use hft_core::{Price, Quantity, Side};
    use ports::{AccountBalance, ExecutionEvent, OrderManager, OrderUpdate, RegisterOrderParams};
    use rust_decimal::Decimal;
    use std::collections::HashMap;

    struct ReconcileOrderManager {
        order: OrderRecord,
    }

    impl OrderManager for ReconcileOrderManager {
        fn register_order(&mut self, _params: RegisterOrderParams) {}

        fn on_execution_event(&mut self, _event: &ExecutionEvent) -> Option<OrderUpdate> {
            None
        }

        fn export_state(&self) -> HashMap<OrderId, OrderRecord> {
            HashMap::from([(self.order.order_id.clone(), self.order.clone())])
        }

        fn import_state(&mut self, _state: HashMap<OrderId, OrderRecord>) {}

        fn open_order_pairs_by_strategy(&self, _strategy_id: &str) -> Vec<(OrderId, Symbol)> {
            Vec::new()
        }

        fn reconcile_with_exchange(
            &self,
            exchange_orders: &[ports::OpenOrder],
        ) -> OrderReconciliationReport {
            if exchange_orders.is_empty() {
                OrderReconciliationReport {
                    local_only: vec![ports::LocalOnlyOrder {
                        order_id: self.order.order_id.clone(),
                        symbol: self.order.symbol.clone(),
                        status: self.order.status,
                    }],
                    ..Default::default()
                }
            } else {
                OrderReconciliationReport::default()
            }
        }
    }

    #[tokio::test]
    async fn emergency_releases_engine_lock_before_waiting_for_worker() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineConfig::default())));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine.clone(), Some(worker_tx), true);
        let worker_engine = engine.clone();
        let worker = tokio::spawn(async move {
            let command = worker_rx.recv().await.expect("control command");
            let engine = worker_engine.lock().await;
            assert_eq!(engine.trading_mode(), TradingMode::Emergency);
            drop(engine);
            match command {
                ControlCommand::EnterEmergency { targets, reply } => {
                    assert!(targets.is_empty());
                    reply
                        .send(CancelDispatchReport::default())
                        .expect("send report");
                }
                _ => panic!("unexpected control command"),
            }
        });

        let report = control.emergency_stop(true).await.expect("emergency stop");
        assert!(report.is_complete());
        worker.await.expect("worker task");
    }

    #[tokio::test]
    async fn reconciliation_is_unhealthy_when_oms_has_a_local_only_order() {
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_order_manager(Box::new(ReconcileOrderManager {
            order: OrderRecord {
                order_id: OrderId("local-only".to_string()),
                client_order_id: None,
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                qty: Quantity::from_f64(1.0).unwrap(),
                cum_qty: Quantity::zero(),
                avg_price: Some(Price::from_f64(100.0).unwrap()),
                status: OrderStatus::Acknowledged,
                venue: Some(VenueId::MOCK),
                strategy_id: Some("test".to_string()),
            },
        }));
        let engine = Arc::new(Mutex::new(engine));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::Reconcile { reply, .. } => reply
                    .send(WorkerReconcileSnapshot {
                        clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                            client_index: 0,
                            venue: Some(VenueId::MOCK),
                            account_id: None,
                            open_orders: Ok(Vec::new()),
                            balances: None,
                        }],
                    })
                    .expect("send reconciliation"),
                _ => panic!("unexpected control command"),
            }
        });

        let report = control.reconcile(false).await.expect("reconcile");
        assert!(report.complete);
        assert!(!report.healthy);
        assert_eq!(report.order_report.local_only.len(), 1);
        worker.await.expect("worker task");
    }

    fn engine_with_equity(equity: Decimal) -> Arc<Mutex<Engine>> {
        let engine = Engine::new(EngineConfig::default());
        let account = ports::AccountView {
            cash_balance: equity,
            ..Default::default()
        };
        engine.account_snapshots.store(Arc::new(account));
        Arc::new(Mutex::new(engine))
    }

    fn balance(asset: &str, total: Decimal, usd_value: Option<Decimal>) -> AccountBalance {
        AccountBalance {
            asset: asset.to_string(),
            available: total,
            frozen: Decimal::ZERO,
            total,
            usd_value,
        }
    }

    async fn reconcile_with_balances(
        engine: Arc<Mutex<Engine>>,
        balances: Result<Vec<AccountBalance>, HftError>,
    ) -> RuntimeReconciliationReport {
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true)
            .with_balance_tolerance_usd(Decimal::ONE);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::Reconcile {
                    include_balances,
                    reply,
                } => {
                    assert!(include_balances);
                    reply
                        .send(WorkerReconcileSnapshot {
                            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                                client_index: 0,
                                venue: Some(VenueId::MOCK),
                                account_id: None,
                                open_orders: Ok(Vec::new()),
                                balances: Some(balances),
                            }],
                        })
                        .expect("send reconciliation");
                }
                _ => panic!("unexpected control command"),
            }
        });
        let report = control.reconcile(true).await.expect("reconcile");
        worker.await.expect("worker task");
        report
    }

    #[tokio::test]
    async fn balance_reconciliation_accepts_authoritative_equity_within_tolerance() {
        let report = reconcile_with_balances(
            engine_with_equity(Decimal::from(100)),
            Ok(vec![balance(
                "USDT",
                Decimal::from(100),
                Some(Decimal::from(100)),
            )]),
        )
        .await;

        assert!(report.complete);
        assert!(report.healthy);
        let balances = report.balance_report.expect("balance report");
        assert!(balances.complete);
        assert!(balances.healthy);
        assert_eq!(balances.difference_usd, Some(Decimal::ZERO));
    }

    #[tokio::test]
    async fn balance_reconciliation_rejects_equity_mismatch() {
        let report = reconcile_with_balances(
            engine_with_equity(Decimal::from(100)),
            Ok(vec![balance(
                "USDT",
                Decimal::from(80),
                Some(Decimal::from(80)),
            )]),
        )
        .await;

        assert!(report.complete);
        assert!(!report.healthy);
        assert_eq!(
            report
                .balance_report
                .expect("balance report")
                .difference_usd,
            Some(Decimal::from(20))
        );
    }

    #[tokio::test]
    async fn balance_reconciliation_is_incomplete_without_nonzero_asset_valuation() {
        let report = reconcile_with_balances(
            engine_with_equity(Decimal::from(100)),
            Ok(vec![balance("BTC", Decimal::ONE, None)]),
        )
        .await;

        assert!(!report.complete);
        assert!(!report.healthy);
        let balances = report.balance_report.expect("balance report");
        assert!(!balances.complete);
        assert_eq!(balances.missing_valuations, vec!["client=0 asset=BTC"]);
    }

    #[tokio::test]
    async fn emergency_resume_does_not_reenable_worker_intake() {
        let mut engine = Engine::new(EngineConfig::default());
        engine.emergency_exit();
        let engine = Arc::new(Mutex::new(engine));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);

        let error = control
            .resume_trading()
            .await
            .expect_err("sticky emergency");
        assert!(error.to_string().contains("sticky"));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), worker_rx.recv())
                .await
                .is_err(),
            "resume must not send an intake-enable command while emergency-latched"
        );
    }
}
