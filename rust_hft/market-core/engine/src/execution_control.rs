use crate::execution_worker::{
    CancelDispatchReport, CancelScope, CancelTarget, ControlCommand, WorkerReconcileSnapshot,
};
use crate::Engine;
use hft_core::{AccountId, HftError, HftResult, OrderId, Quantity, Symbol, VenueId};
use ports::{OrderReconciliationReport, OrderRecord, OrderStatus};
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Debug, Clone)]
pub struct RuntimeReconciliationReport {
    pub worker_snapshot: WorkerReconcileSnapshot,
    pub order_report: OrderReconciliationReport,
    pub balance_report: Option<BalanceReconciliationReport>,
    pub position_report: Option<PositionReconciliationReport>,
    pub fill_report: Option<FillReconciliationReport>,
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

#[derive(Debug, Clone)]
pub struct PositionQuantityMismatch {
    pub symbol: Symbol,
    pub local_quantity: Quantity,
    pub exchange_quantity: Quantity,
}

#[derive(Debug, Clone)]
pub struct PositionReconciliationReport {
    pub local_only: Vec<Symbol>,
    pub exchange_only: Vec<Symbol>,
    pub quantity_mismatch: Vec<PositionQuantityMismatch>,
    pub client_errors: Vec<String>,
    pub complete: bool,
    pub healthy: bool,
}

#[derive(Debug, Clone)]
pub struct FillReconciliationReport {
    /// Authoritative venue fills that are absent from the local accounting ledger.
    pub exchange_only_fill_ids: Vec<String>,
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
    // ponytail: one process-wide mode-change gate; split per account only if measured control-plane contention requires it.
    operation_gate: Arc<Mutex<()>>,
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
            operation_gate: Arc::new(Mutex::new(())),
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

    pub fn with_operation_gate(mut self, operation_gate: Arc<Mutex<()>>) -> Self {
        self.operation_gate = operation_gate;
        self
    }

    pub fn engine(&self) -> Arc<Mutex<Engine>> {
        self.engine.clone()
    }

    pub async fn pause_trading(&self) -> HftResult<()> {
        let _operation = self.operation_gate.lock().await;
        self.pause_trading_unlocked().await
    }

    async fn pause_trading_unlocked(&self) -> HftResult<()> {
        self.engine.lock().await.pause_trading();
        if self.execution_enabled {
            self.set_intake(false).await?;
        }
        Ok(())
    }

    pub async fn resume_trading(&self) -> HftResult<()> {
        let _operation = self.operation_gate.lock().await;
        self.resume_trading_unlocked(crate::TradingMode::Normal)
            .await
    }

    async fn resume_trading_unlocked(&self, target_mode: crate::TradingMode) -> HftResult<()> {
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
        match target_mode {
            crate::TradingMode::Normal => engine.resume_trading(),
            crate::TradingMode::Degraded => engine.enter_degrade_mode(),
            crate::TradingMode::Paused | crate::TradingMode::Emergency => {
                drop(engine);
                if self.execution_enabled {
                    self.set_intake(false).await?;
                }
                return Err(HftError::Risk(
                    "cannot resume execution into a non-trading mode".to_string(),
                ));
            }
        }
        Ok(())
    }

    pub async fn emergency_stop(&self, cancel_orders: bool) -> HftResult<CancelDispatchReport> {
        let _operation = self.operation_gate.lock().await;
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

    /// Cancel an exchange-open order discovered from an authoritative client snapshot even when
    /// Monday has no restored OMS record for it. Venue/account identity is mandatory so the
    /// cancellation cannot be routed by a possibly incomplete symbol catalog after restart.
    pub async fn cancel_authoritative_order(
        &self,
        order_id: OrderId,
        symbol: Symbol,
        venue: Option<VenueId>,
        account_id: Option<AccountId>,
    ) -> HftResult<CancelDispatchReport> {
        if venue.is_none() && account_id.is_none() {
            return Err(HftError::Execution(
                "authoritative cancellation requires venue or account routing identity".to_string(),
            ));
        }
        self.dispatch_cancellations(
            vec![CancelTarget {
                order_id,
                symbol,
                venue,
                account_id,
            }],
            false,
            CancelScope::Explicit,
        )
        .await
    }

    pub async fn replace_order(
        &self,
        order_id: OrderId,
        symbol: Symbol,
        new_quantity: Option<hft_core::Quantity>,
        new_price: Option<hft_core::Price>,
    ) -> HftResult<()> {
        if new_quantity.is_none() && new_price.is_none() {
            return Err(HftError::InvalidOrder(
                "replacement requires a new quantity or price".to_string(),
            ));
        }
        {
            let engine = self.engine.lock().await;
            if matches!(
                engine.trading_mode(),
                crate::TradingMode::Paused | crate::TradingMode::Emergency
            ) {
                return Err(HftError::Risk(
                    "order replacement is disabled while trading is paused or emergency-latched"
                        .to_string(),
                ));
            }
            let state = engine.export_oms_state();
            let order = state.get(&order_id).ok_or_else(|| {
                HftError::OrderNotFound(format!(
                    "replacement requires an OMS-tracked order: {}",
                    order_id.0
                ))
            })?;
            if order.symbol != symbol {
                return Err(HftError::InvalidOrder(format!(
                    "replacement symbol mismatch for {}: expected {}, got {}",
                    order_id.0,
                    order.symbol.as_str(),
                    symbol.as_str()
                )));
            }
            if !is_open(order.status) {
                return Err(HftError::InvalidOrder(format!(
                    "order {} is not open and cannot be replaced",
                    order_id.0
                )));
            }
            if order.cum_qty.0 > Decimal::ZERO {
                return Err(HftError::InvalidOrder(format!(
                    "order {} is partially filled; cancel and submit a new reviewed intent",
                    order_id.0
                )));
            }
            if let Some(quantity) = new_quantity {
                if quantity.0 <= Decimal::ZERO {
                    return Err(HftError::InvalidOrder(
                        "replacement quantity must be positive".to_string(),
                    ));
                }
                if quantity.0 > order.qty.0 {
                    return Err(HftError::Risk(format!(
                        "replacement quantity cannot increase from {} to {} without a new risk-reviewed intent",
                        order.qty.0, quantity.0
                    )));
                }
            }
            if new_price.is_some_and(|price| price.0 <= Decimal::ZERO) {
                return Err(HftError::InvalidOrder(
                    "replacement price must be positive".to_string(),
                ));
            }
        }
        let worker_tx = self.worker_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        worker_tx
            .send(ControlCommand::ReplaceOrder {
                order_id,
                symbol,
                new_quantity,
                new_price,
                reply: reply_tx,
            })
            .map_err(|_| HftError::Execution("execution worker control channel closed".into()))?;
        self.await_reply(reply_rx, "order replacement")
            .await?
            .map_err(HftError::Execution)
    }

    pub async fn reconcile(
        &self,
        include_balances: bool,
    ) -> HftResult<RuntimeReconciliationReport> {
        if include_balances {
            self.engine
                .lock()
                .await
                .publish_runtime_truth_status(crate::RuntimeTruthStatus {
                    reconciliation_complete: false,
                    reconciliation_healthy: false,
                    observed_at_us: hft_core::now_micros(),
                    account_id: None,
                });
        }
        let worker_tx = self.worker_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        worker_tx
            .send(ControlCommand::Reconcile {
                include_balances,
                include_positions: include_balances,
                include_recent_fills: include_balances,
                reply: reply_tx,
            })
            .map_err(|_| HftError::Execution("execution worker control channel closed".into()))?;
        let worker_snapshot = self.await_reply(reply_rx, "reconciliation").await?;
        let order_report = {
            let engine = self.engine.lock().await;
            engine.reconcile_open_orders(&worker_snapshot)
        };
        let (balance_report, position_report, fill_report) = if include_balances {
            let engine = self.engine.lock().await;
            let account_view = engine.get_account_view();
            let portfolio_state = engine.export_portfolio_state();
            (
                Some(reconcile_balances(
                    &worker_snapshot,
                    account_view.equity(),
                    self.balance_tolerance_usd,
                )),
                reconcile_positions(&worker_snapshot, &account_view.positions),
                Some(reconcile_recent_fills(
                    &worker_snapshot,
                    &portfolio_state.processed_fill_ids,
                )),
            )
        } else {
            (None, None, None)
        };
        let complete = worker_snapshot.is_complete()
            && (!include_balances
                || worker_snapshot.clients.iter().all(|client| {
                    client.positions.as_ref().is_some_and(Result::is_ok)
                        && client.recent_fills.as_ref().is_some_and(Result::is_ok)
                }))
            && balance_report
                .as_ref()
                .is_none_or(|balances| balances.complete)
            && position_report
                .as_ref()
                .is_none_or(|positions| positions.complete)
            && fill_report.as_ref().is_none_or(|fills| fills.complete);
        let healthy = complete
            && !order_report.has_discrepancies()
            && balance_report
                .as_ref()
                .is_none_or(|balances| balances.healthy)
            && position_report
                .as_ref()
                .is_none_or(|positions| positions.healthy)
            && fill_report.as_ref().is_none_or(|fills| fills.healthy);

        let report = RuntimeReconciliationReport {
            worker_snapshot,
            order_report,
            balance_report,
            position_report,
            fill_report,
            complete,
            healthy,
        };
        if include_balances {
            self.engine
                .lock()
                .await
                .publish_runtime_truth_status(crate::RuntimeTruthStatus {
                    reconciliation_complete: report.complete,
                    reconciliation_healthy: report.healthy,
                    observed_at_us: hft_core::now_micros(),
                    account_id: report.worker_snapshot.account_id(),
                });
        }
        Ok(report)
    }

    /// Quiesce both strategy production and worker intake before taking an authoritative
    /// snapshot. Queued intents are rejected by the worker before the pause is acknowledged, and
    /// the previous active mode is restored only after a healthy report. An unhealthy or failed
    /// reconciliation therefore cannot race one more order onto the venue.
    pub async fn reconcile_guarded(
        &self,
        include_balances: bool,
    ) -> HftResult<RuntimeReconciliationReport> {
        let _operation = self.operation_gate.lock().await;
        let previous_mode = self.engine.lock().await.trading_mode();
        let was_active = matches!(
            previous_mode,
            crate::TradingMode::Normal | crate::TradingMode::Degraded
        );
        if was_active {
            self.pause_trading_unlocked().await?;
        }

        let report = match self.reconcile(include_balances).await {
            Ok(report) => report,
            Err(error) => {
                if let Err(pause_error) = self.pause_trading_unlocked().await {
                    return Err(HftError::Execution(format!(
                        "authoritative reconciliation failed ({error}); execution pause also failed ({pause_error})"
                    )));
                }
                return Err(error);
            }
        };

        if report.healthy && was_active {
            self.resume_trading_unlocked(previous_mode).await?;
        } else if !report.healthy {
            self.pause_trading_unlocked().await?;
        }
        Ok(report)
    }

    /// Returns a venue-authoritative account snapshot without mutating local portfolio state.
    /// This is intentionally separate from the periodic hot-path reconciliation because fetching
    /// paginated fill history can be expensive.
    pub async fn inspect_account(&self) -> HftResult<WorkerReconcileSnapshot> {
        let worker_tx = self.worker_sender()?;
        let (reply_tx, reply_rx) = oneshot::channel();
        worker_tx
            .send(ControlCommand::Reconcile {
                include_balances: true,
                include_positions: true,
                include_recent_fills: true,
                reply: reply_tx,
            })
            .map_err(|_| HftError::Execution("execution worker control channel closed".into()))?;
        self.await_reply(reply_rx, "account inspection").await
    }

    /// Initialize a pristine in-memory portfolio from a complete venue snapshot.
    ///
    /// This is only valid before live intake starts. Existing local state or exchange-open orders
    /// are never overwritten, so recovery still requires an explicit persisted OMS/portfolio state.
    pub async fn bootstrap_pristine_account(
        &self,
        snapshot: &WorkerReconcileSnapshot,
    ) -> HftResult<bool> {
        // This bootstrap exists for the Polymarket migration only. Applying an exchange
        // snapshot as a generic source of truth can hide a missing persisted Monday ledger on
        // venues whose balances/positions have different accounting semantics.
        let [client] = snapshot.clients.as_slice() else {
            return Ok(false);
        };
        if client.venue != Some(VenueId::POLYMARKET) {
            return Ok(false);
        }
        if !snapshot.is_complete() {
            return Err(HftError::Execution(
                "authoritative account bootstrap requires a complete venue snapshot".to_string(),
            ));
        }
        if snapshot.clients.iter().any(|client| {
            client
                .open_orders
                .as_ref()
                .is_ok_and(|orders| !orders.is_empty())
        }) {
            return Err(HftError::Execution(
                "authoritative account bootstrap refuses exchange-open orders; restore Monday OMS state or cancel them explicitly"
                    .to_string(),
            ));
        }

        let mut cash_balance = Decimal::ZERO;
        let mut positions = std::collections::HashMap::new();
        let mut baseline_processed_fill_ids =
            std::collections::HashMap::<OrderId, std::collections::HashSet<String>>::new();
        let mut baseline_recent_accounting_event_ids = Vec::new();
        let mut saw_balances = false;
        for client in &snapshot.clients {
            let balances = client.balances.as_ref().ok_or_else(|| {
                HftError::Execution(format!(
                    "execution client {} did not provide balances for account bootstrap",
                    client.client_index
                ))
            })?;
            let balances = balances.as_ref().map_err(|error| {
                HftError::Execution(format!(
                    "execution client {} balance bootstrap failed: {error}",
                    client.client_index
                ))
            })?;
            saw_balances = true;
            cash_balance += balances
                .iter()
                .filter(|balance| {
                    matches!(
                        balance.asset.to_ascii_uppercase().as_str(),
                        "USD" | "USDC" | "USDC.E" | "USDT"
                    )
                })
                .map(|balance| balance.total)
                .sum::<Decimal>();

            let client_positions = client.positions.as_ref().ok_or_else(|| {
                HftError::Execution(format!(
                    "execution client {} did not provide positions for Polymarket account bootstrap",
                    client.client_index
                ))
            })?;
            for position in client_positions.as_ref().map_err(|error| {
                HftError::Execution(format!(
                    "execution client {} position bootstrap failed: {error}",
                    client.client_index
                ))
            })? {
                if positions
                    .insert(position.symbol.clone(), position.clone())
                    .is_some()
                {
                    return Err(HftError::Execution(format!(
                        "account bootstrap found duplicate position identity {} across execution clients",
                        position.symbol.as_str()
                    )));
                }
            }

            let recent_fills = client.recent_fills.as_ref().ok_or_else(|| {
                HftError::Execution(format!(
                    "execution client {} did not provide recent fills for account bootstrap",
                    client.client_index
                ))
            })?;
            for fill in recent_fills.as_ref().map_err(|error| {
                HftError::Execution(format!(
                    "execution client {} recent-fill bootstrap failed: {error}",
                    client.client_index
                ))
            })? {
                if fill.fill_id.is_empty() {
                    return Err(HftError::Execution(format!(
                        "execution client {} returned a recent fill without fill_id",
                        client.client_index
                    )));
                }
                let inserted = baseline_processed_fill_ids
                    .entry(fill.order_id.clone())
                    .or_default()
                    .insert(fill.fill_id.clone());
                if !inserted {
                    return Err(HftError::Execution(format!(
                        "account bootstrap found duplicate recent fill {}:{}",
                        fill.order_id.0, fill.fill_id
                    )));
                }
                baseline_recent_accounting_event_ids
                    .push((fill.order_id.clone(), format!("fill:{}", fill.fill_id)));
            }
        }
        if !saw_balances {
            return Err(HftError::Execution(
                "account bootstrap received no balance snapshots".to_string(),
            ));
        }

        let mut engine = self.engine.lock().await;
        let current = engine.export_portfolio_state();
        let pristine = current.account_view.cash_balance == Decimal::ZERO
            && current.account_view.positions.is_empty()
            && current.account_view.realized_pnl == Decimal::ZERO
            && current.account_view.unrealized_pnl == Decimal::ZERO
            && engine.export_oms_state().is_empty();
        if !pristine {
            return Ok(false);
        }

        let unrealized_pnl = positions
            .values()
            .map(|position| position.unrealized_pnl)
            .sum::<Decimal>();
        let mut account_view = ports::AccountView {
            cash_balance,
            positions,
            unrealized_pnl,
            realized_pnl: Decimal::ZERO,
            high_water_mark: Decimal::ZERO,
            drawdown_pct: 0.0,
            max_drawdown_pct: 0.0,
            session_start_us: hft_core::now_micros(),
        };
        account_view.high_water_mark = account_view.equity();
        engine.import_portfolio_state(ports::PortfolioState {
            account_view,
            order_meta: current.order_meta,
            market_prices: current.market_prices,
            processed_fill_ids: baseline_processed_fill_ids,
            recent_accounting_event_ids: baseline_recent_accounting_event_ids,
        })?;
        Ok(true)
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

fn reconcile_recent_fills(
    snapshot: &WorkerReconcileSnapshot,
    processed_fill_ids: &std::collections::HashMap<OrderId, std::collections::HashSet<String>>,
) -> FillReconciliationReport {
    let mut exchange_only_fill_ids = Vec::new();
    let mut client_errors = Vec::new();
    let mut observed = std::collections::HashSet::new();

    if snapshot.clients.is_empty() {
        client_errors.push("no execution clients returned recent-fill snapshots".to_string());
    }
    for client in &snapshot.clients {
        match &client.recent_fills {
            None => client_errors.push(format!(
                "client={} does not support authoritative recent fills",
                client.client_index
            )),
            Some(Err(error)) => client_errors.push(format!(
                "client={} recent-fill snapshot failed: {}",
                client.client_index, error
            )),
            Some(Ok(fills)) => {
                for fill in fills {
                    let identity = (fill.order_id.clone(), fill.fill_id.clone());
                    if fill.fill_id.is_empty() {
                        client_errors.push(format!(
                            "client={} returned a recent fill without fill_id",
                            client.client_index
                        ));
                        continue;
                    }
                    if !observed.insert(identity.clone()) {
                        client_errors.push(format!(
                            "duplicate authoritative fill identity order={} fill={}",
                            fill.order_id.0, fill.fill_id
                        ));
                        continue;
                    }
                    let locally_processed = processed_fill_ids
                        .get(&fill.order_id)
                        .is_some_and(|ids| ids.contains(&fill.fill_id));
                    if !locally_processed {
                        exchange_only_fill_ids
                            .push(format!("{}:{}", fill.order_id.0, fill.fill_id));
                    }
                }
            }
        }
    }
    exchange_only_fill_ids.sort();
    let complete = client_errors.is_empty();
    let healthy = complete && exchange_only_fill_ids.is_empty();
    FillReconciliationReport {
        exchange_only_fill_ids,
        client_errors,
        complete,
        healthy,
    }
}

fn reconcile_positions(
    snapshot: &WorkerReconcileSnapshot,
    local_positions: &std::collections::HashMap<Symbol, ports::Position>,
) -> Option<PositionReconciliationReport> {
    #[derive(Default)]
    struct VenuePositions {
        quantities: BTreeMap<String, Decimal>,
        blockers: Vec<String>,
    }

    let mut by_venue = BTreeMap::<Option<VenueId>, VenuePositions>::new();
    let mut client_errors = std::collections::BTreeSet::new();
    for client in &snapshot.clients {
        let venue = client
            .venue
            .map_or_else(|| "UNKNOWN".to_string(), |venue| venue.to_string());
        let account = client
            .account_id
            .as_ref()
            .map_or("default", |account| account.0.as_str());
        let scope = format!(
            "client={} venue={} account={}",
            client.client_index, venue, account
        );
        let venue_positions = by_venue.entry(client.venue).or_default();
        match &client.positions {
            None => {
                let error = format!("{scope} does not support authoritative position snapshots");
                venue_positions.blockers.push(error.clone());
                client_errors.insert(error);
            }
            Some(Err(error)) => {
                let error = format!("{scope} position snapshot failed: {error}");
                venue_positions.blockers.push(error.clone());
                client_errors.insert(error);
            }
            Some(Ok(positions)) => {
                for position in positions {
                    *venue_positions
                        .quantities
                        .entry(position.symbol.as_str().to_string())
                        .or_default() += position.quantity.0;
                }
            }
        }
    }

    let local = local_positions
        .iter()
        .map(|(symbol, position)| (symbol.as_str().to_string(), position.quantity.0))
        .collect::<BTreeMap<_, _>>();
    let mut symbols = local
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for venue in by_venue.values() {
        symbols.extend(venue.quantities.keys().cloned());
    }

    let tolerance = Decimal::new(1, 6);
    let mut local_only = Vec::new();
    let mut exchange_only = Vec::new();
    let mut quantity_mismatch = Vec::new();

    for symbol in symbols {
        let local_quantity = local.get(&symbol).copied().unwrap_or(Decimal::ZERO);
        let polymarket_scoped = is_polymarket_token_symbol(&symbol)
            && by_venue.contains_key(&Some(VenueId::POLYMARKET));
        let mut exchange_quantity = Decimal::ZERO;
        let mut scoped = false;
        let mut blocked = false;
        for (venue, positions) in &by_venue {
            // Outcome token IDs are globally stable Monday symbols, so unrelated venues cannot
            // hide a Polymarket discrepancy. Unknown venues remain in scope because they cannot
            // be safely attributed.
            if polymarket_scoped
                && !matches!(venue, Some(value) if *value == VenueId::POLYMARKET)
                && venue.is_some()
            {
                continue;
            }
            scoped = true;
            if positions.blockers.is_empty() {
                exchange_quantity += positions
                    .quantities
                    .get(&symbol)
                    .copied()
                    .unwrap_or(Decimal::ZERO);
            } else {
                blocked = true;
                client_errors.extend(positions.blockers.iter().cloned());
            }
        }
        if !scoped || blocked {
            client_errors.insert(format!(
                "cannot safely attribute position symbol={symbol} to a complete venue/account snapshot"
            ));
            continue;
        }

        let local_nonzero = local_quantity != Decimal::ZERO;
        let exchange_nonzero = exchange_quantity != Decimal::ZERO;
        match (local_nonzero, exchange_nonzero) {
            (true, false) => local_only.push(Symbol::new(&symbol)),
            (false, true) => exchange_only.push(Symbol::new(&symbol)),
            (true, true) if decimal_abs(local_quantity - exchange_quantity) > tolerance => {
                quantity_mismatch.push(PositionQuantityMismatch {
                    symbol: Symbol::new(&symbol),
                    local_quantity: Quantity(local_quantity),
                    exchange_quantity: Quantity(exchange_quantity),
                });
            }
            _ => {}
        }
    }

    let client_errors = client_errors.into_iter().collect::<Vec<_>>();
    let complete = client_errors.is_empty();
    let healthy = complete
        && local_only.is_empty()
        && exchange_only.is_empty()
        && quantity_mismatch.is_empty();
    Some(PositionReconciliationReport {
        local_only,
        exchange_only,
        quantity_mismatch,
        client_errors,
        complete,
        healthy,
    })
}

fn is_polymarket_token_symbol(symbol: &str) -> bool {
    !symbol.is_empty()
        && symbol.as_bytes().iter().all(u8::is_ascii_digit)
        && symbol != "0"
        && !symbol.starts_with('0')
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
    use oms_core::OmsCore;
    use ports::{
        AccountBalance, ExecutionEvent, OrderManager, OrderUpdate, PortfolioManager,
        RegisterOrderParams,
    };
    use rust_decimal::Decimal;
    use std::collections::HashMap;

    struct ReconcileOrderManager {
        order: OrderRecord,
    }

    struct TestPortfolio {
        state: ports::PortfolioState,
        snapshot: snapshot::SnapshotContainer<ports::AccountView>,
    }

    impl Default for TestPortfolio {
        fn default() -> Self {
            let account_view = ports::AccountView::default();
            Self {
                state: ports::PortfolioState {
                    account_view: account_view.clone(),
                    order_meta: HashMap::new(),
                    market_prices: HashMap::new(),
                    processed_fill_ids: HashMap::new(),
                    recent_accounting_event_ids: Vec::new(),
                },
                snapshot: snapshot::SnapshotContainer::new(account_view),
            }
        }
    }

    impl PortfolioManager for TestPortfolio {
        fn register_order(&mut self, order_id: OrderId, symbol: Symbol, side: Side) {
            self.state.order_meta.insert(order_id, (symbol, side));
        }

        fn on_execution_event(&mut self, _event: &ExecutionEvent) {}

        fn reader(&self) -> Arc<dyn snapshot::SnapshotReader<ports::AccountView>> {
            self.snapshot.reader()
        }

        fn update_market_prices(&mut self, prices: &HashMap<Symbol, Price>) {
            self.state.market_prices.extend(prices.clone());
        }

        fn export_state(&self) -> ports::PortfolioState {
            self.state.clone()
        }

        fn import_state(&mut self, state: ports::PortfolioState) {
            self.snapshot.store(Arc::new(state.account_view.clone()));
            self.state = state;
        }
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
    async fn authoritative_exchange_only_cancel_preserves_venue_and_account_routing() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineConfig::default())));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let account_id = AccountId("poly-main".to_string());
        let worker = tokio::spawn({
            let account_id = account_id.clone();
            async move {
                match worker_rx.recv().await.expect("control command") {
                    ControlCommand::CancelOrders {
                        targets,
                        scope: CancelScope::Explicit,
                        reply,
                    } => {
                        assert_eq!(targets.len(), 1);
                        assert_eq!(targets[0].venue, Some(VenueId::POLYMARKET));
                        assert_eq!(targets[0].account_id.as_ref(), Some(&account_id));
                        reply
                            .send(CancelDispatchReport {
                                requested: 1,
                                submitted: vec![targets[0].order_id.clone()],
                                failures: Vec::new(),
                            })
                            .expect("send cancellation report");
                    }
                    _ => panic!("unexpected control command"),
                }
            }
        });

        let report = control
            .cancel_authoritative_order(
                OrderId("venue-order-1".to_string()),
                Symbol::new("123"),
                Some(VenueId::POLYMARKET),
                Some(account_id),
            )
            .await
            .expect("authoritative cancellation");
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
                account_id: None,
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
                            positions: None,
                            recent_fills: None,
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

    #[tokio::test]
    async fn reconciliation_does_not_match_same_order_id_across_client_identity() {
        let order_id = OrderId("42".to_string());
        let binance_account = AccountId("binance-main".to_string());
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_order_manager(Box::new(ReconcileOrderManager {
            order: OrderRecord {
                order_id: order_id.clone(),
                client_order_id: Some("client-42".to_string()),
                account_id: Some(binance_account.clone()),
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                qty: Quantity(Decimal::ONE),
                cum_qty: Quantity::zero(),
                avg_price: Some(Price(Decimal::from(100))),
                status: OrderStatus::Acknowledged,
                venue: Some(VenueId::BINANCE),
                strategy_id: Some("test".to_string()),
            },
        }));
        engine
            .order_account_map
            .insert(order_id.clone(), binance_account.clone());
        let engine = Arc::new(Mutex::new(engine));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let worker = tokio::spawn({
            async move {
                let open_order = |exchange_order_id: &str, symbol: &str| ports::OpenOrder {
                    order_id: OrderId(exchange_order_id.to_string()),
                    client_order_id: Some("client-42".to_string()),
                    symbol: Symbol::new(symbol),
                    side: Side::Buy,
                    order_type: hft_core::OrderType::Limit,
                    original_quantity: Quantity(Decimal::ONE),
                    remaining_quantity: Quantity(Decimal::ONE),
                    filled_quantity: Quantity::zero(),
                    price: Some(Price(Decimal::from(100))),
                    status: OrderStatus::Accepted,
                    created_at: 1,
                    updated_at: 1,
                };
                match worker_rx.recv().await.expect("reconcile command") {
                    ControlCommand::Reconcile { reply, .. } => reply
                        .send(WorkerReconcileSnapshot {
                            clients: vec![
                                crate::execution_worker::ClientReconcileSnapshot {
                                    client_index: 0,
                                    venue: Some(VenueId::BINANCE),
                                    account_id: Some(binance_account),
                                    open_orders: Ok(vec![open_order("42", "BTCUSDT")]),
                                    balances: None,
                                    positions: None,
                                    recent_fills: None,
                                },
                                crate::execution_worker::ClientReconcileSnapshot {
                                    client_index: 1,
                                    venue: Some(VenueId::BITGET),
                                    account_id: Some(AccountId("bitget-main".to_string())),
                                    open_orders: Ok(vec![
                                        open_order("42", "ETHUSDT"),
                                        open_order("bitget-42", "ETHUSDT"),
                                    ]),
                                    balances: None,
                                    positions: None,
                                    recent_fills: None,
                                },
                            ],
                        })
                        .expect("send reconciliation"),
                    _ => panic!("unexpected control command"),
                }
            }
        });

        let report = control.reconcile(false).await.expect("reconcile");

        assert!(report.complete);
        assert!(!report.healthy);
        assert_eq!(
            report.order_report.exchange_only,
            vec![order_id, OrderId("bitget-42".to_string())]
        );
        worker.await.expect("worker task");
    }

    #[tokio::test]
    async fn imported_oms_state_restores_account_identity_for_reconciliation() {
        let order_id = OrderId("restored-42".to_string());
        let account_id = AccountId("binance-main".to_string());
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_order_manager(Box::new(OmsCore::new()));
        engine.set_strategy_account_mapping(HashMap::from([(
            "alpha".to_string(),
            AccountId("wrong-strategy-account".to_string()),
        )]));
        engine.import_oms_state(HashMap::from([(
            order_id.clone(),
            OrderRecord {
                order_id: order_id.clone(),
                client_order_id: Some("client-restored-42".to_string()),
                account_id: Some(account_id.clone()),
                symbol: Symbol::new("BTCUSDT"),
                side: Side::Buy,
                qty: Quantity(Decimal::ONE),
                cum_qty: Quantity::zero(),
                avg_price: Some(Price(Decimal::from(100))),
                status: OrderStatus::Acknowledged,
                venue: Some(VenueId::BINANCE),
                strategy_id: Some("alpha".to_string()),
            },
        )]));
        assert_eq!(
            engine.get_account_for_order(&order_id),
            Some(account_id.clone())
        );
        let engine = Arc::new(Mutex::new(engine));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let worker = tokio::spawn({
            let order_id = order_id.clone();
            async move {
                match worker_rx.recv().await.expect("reconcile command") {
                    ControlCommand::Reconcile { reply, .. } => reply
                        .send(WorkerReconcileSnapshot {
                            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                                client_index: 0,
                                venue: Some(VenueId::BINANCE),
                                account_id: Some(account_id),
                                open_orders: Ok(vec![ports::OpenOrder {
                                    order_id,
                                    client_order_id: Some("client-restored-42".to_string()),
                                    symbol: Symbol::new("BTCUSDT"),
                                    side: Side::Buy,
                                    order_type: hft_core::OrderType::Limit,
                                    original_quantity: Quantity(Decimal::ONE),
                                    remaining_quantity: Quantity(Decimal::ONE),
                                    filled_quantity: Quantity::zero(),
                                    price: Some(Price(Decimal::from(100))),
                                    status: OrderStatus::Accepted,
                                    created_at: 1,
                                    updated_at: 1,
                                }]),
                                balances: None,
                                positions: None,
                                recent_fills: None,
                            }],
                        })
                        .expect("send reconciliation"),
                    _ => panic!("unexpected control command"),
                }
            }
        });

        let report = control.reconcile(false).await.expect("reconcile");

        assert!(report.complete);
        assert!(report.healthy);
        assert!(!report.order_report.has_discrepancies());
        worker.await.expect("worker task");
    }

    #[tokio::test]
    async fn order_only_reconciliation_does_not_publish_authoritative_account_truth() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineConfig::default())));
        let runtime_truth = engine.lock().await.runtime_truth_reader();
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::Reconcile {
                    include_balances,
                    include_positions,
                    include_recent_fills,
                    reply,
                } => {
                    assert!(!include_balances);
                    assert!(!include_positions);
                    assert!(!include_recent_fills);
                    reply
                        .send(WorkerReconcileSnapshot {
                            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                                client_index: 0,
                                venue: Some(VenueId::POLYMARKET),
                                account_id: None,
                                open_orders: Ok(Vec::new()),
                                balances: None,
                                positions: None,
                                recent_fills: None,
                            }],
                        })
                        .expect("send reconciliation");
                }
                _ => panic!("unexpected control command"),
            }
        });

        let report = control.reconcile(false).await.expect("reconcile");
        worker.await.expect("worker task");

        assert!(report.complete);
        assert!(report.healthy);
        let truth = runtime_truth.load();
        assert!(!truth.reconciliation_complete);
        assert!(!truth.reconciliation_healthy);
        assert_eq!(truth.observed_at_us, 0);
    }

    #[tokio::test]
    async fn guarded_reconciliation_quiesces_before_snapshot_and_resumes_only_when_healthy() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineConfig::default())));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine.clone(), Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("pause command") {
                ControlCommand::SetIntake {
                    enabled: false,
                    reply,
                } => reply.send(Ok(())).expect("ack pause barrier"),
                _ => panic!("snapshot ran before intake pause"),
            }
            match worker_rx.recv().await.expect("reconcile command") {
                ControlCommand::Reconcile { reply, .. } => reply
                    .send(WorkerReconcileSnapshot {
                        clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                            client_index: 0,
                            venue: Some(VenueId::POLYMARKET),
                            account_id: None,
                            open_orders: Ok(Vec::new()),
                            balances: None,
                            positions: None,
                            recent_fills: None,
                        }],
                    })
                    .expect("send healthy snapshot"),
                _ => panic!("unexpected command before reconciliation"),
            }
            match worker_rx.recv().await.expect("healthy resume command") {
                ControlCommand::SetIntake {
                    enabled: true,
                    reply,
                } => reply.send(Ok(())).expect("ack healthy resume"),
                _ => panic!("healthy reconciliation did not resume intake"),
            }
        });

        let report = control
            .reconcile_guarded(false)
            .await
            .expect("healthy guarded reconciliation");

        assert!(report.healthy);
        assert_eq!(engine.lock().await.trading_mode(), TradingMode::Normal);
        worker.await.expect("worker task");
    }

    #[tokio::test]
    async fn guarded_reconciliation_keeps_intake_paused_after_exchange_discrepancy() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineConfig::default())));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine.clone(), Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("pause command") {
                ControlCommand::SetIntake {
                    enabled: false,
                    reply,
                } => reply.send(Ok(())).expect("ack pause barrier"),
                _ => panic!("snapshot ran before intake pause"),
            }
            let exchange_order = ports::OpenOrder {
                order_id: OrderId("exchange-only".to_string()),
                client_order_id: None,
                symbol: Symbol::new("123"),
                side: Side::Buy,
                order_type: hft_core::OrderType::Limit,
                original_quantity: Quantity(Decimal::ONE),
                remaining_quantity: Quantity(Decimal::ONE),
                filled_quantity: Quantity::zero(),
                price: Some(Price(Decimal::new(5, 1))),
                status: OrderStatus::Accepted,
                created_at: 1,
                updated_at: 1,
            };
            match worker_rx.recv().await.expect("reconcile command") {
                ControlCommand::Reconcile { reply, .. } => reply
                    .send(WorkerReconcileSnapshot {
                        clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                            client_index: 0,
                            venue: Some(VenueId::POLYMARKET),
                            account_id: None,
                            open_orders: Ok(vec![exchange_order]),
                            balances: None,
                            positions: None,
                            recent_fills: None,
                        }],
                    })
                    .expect("send unhealthy snapshot"),
                _ => panic!("unexpected command before reconciliation"),
            }
            match worker_rx.recv().await.expect("unhealthy pause command") {
                ControlCommand::SetIntake {
                    enabled: false,
                    reply,
                } => reply.send(Ok(())).expect("ack sticky pause"),
                _ => panic!("unhealthy reconciliation attempted to resume intake"),
            }
            assert!(worker_rx.try_recv().is_err());
        });

        let report = control
            .reconcile_guarded(false)
            .await
            .expect("unhealthy report remains inspectable");

        assert!(!report.healthy);
        assert_eq!(engine.lock().await.trading_mode(), TradingMode::Paused);
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
                    include_positions,
                    include_recent_fills,
                    reply,
                } => {
                    assert!(include_balances);
                    assert!(include_positions);
                    assert!(include_recent_fills);
                    reply
                        .send(WorkerReconcileSnapshot {
                            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                                client_index: 0,
                                venue: Some(VenueId::MOCK),
                                account_id: None,
                                open_orders: Ok(Vec::new()),
                                balances: Some(balances),
                                positions: Some(Ok(Vec::new())),
                                recent_fills: Some(Ok(Vec::new())),
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

    #[test]
    fn recent_fill_reconciliation_detects_unaccounted_exchange_fill() {
        let order_id = OrderId("order-1".to_string());
        let snapshot = WorkerReconcileSnapshot {
            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                client_index: 0,
                venue: Some(VenueId::POLYMARKET),
                account_id: None,
                open_orders: Ok(Vec::new()),
                balances: None,
                positions: None,
                recent_fills: Some(Ok(vec![ports::AccountFill {
                    fill_id: "fill-1".to_string(),
                    order_id: order_id.clone(),
                    symbol: Symbol::new("123"),
                    side: Side::Buy,
                    price: Price(Decimal::new(5, 1)),
                    quantity: Quantity(Decimal::ONE),
                    fee: None,
                    timestamp: 1,
                }])),
            }],
        };

        let missing = reconcile_recent_fills(&snapshot, &HashMap::new());
        assert!(missing.complete);
        assert!(!missing.healthy);
        assert_eq!(missing.exchange_only_fill_ids, vec!["order-1:fill-1"]);

        let processed = HashMap::from([(
            order_id,
            std::collections::HashSet::from(["fill-1".to_string()]),
        )]);
        let matched = reconcile_recent_fills(&snapshot, &processed);
        assert!(matched.healthy);
    }

    #[tokio::test]
    async fn live_reconciliation_is_incomplete_without_positions_and_recent_fills() {
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control =
            ExecutionControlHandle::new(engine_with_equity(Decimal::ZERO), Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::Reconcile { reply, .. } => reply
                    .send(WorkerReconcileSnapshot {
                        clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                            client_index: 0,
                            venue: Some(VenueId::BINANCE),
                            account_id: None,
                            open_orders: Ok(Vec::new()),
                            balances: Some(Ok(Vec::new())),
                            positions: None,
                            recent_fills: None,
                        }],
                    })
                    .expect("send reconciliation"),
                _ => panic!("unexpected control command"),
            }
        });

        let report = control.reconcile(true).await.expect("reconcile");
        worker.await.expect("worker task");

        assert!(!report.complete);
        assert!(!report.healthy);
        assert!(report
            .position_report
            .expect("position report")
            .client_errors
            .iter()
            .any(|error| error.contains("does not support")));
    }

    #[test]
    fn position_reconciliation_detects_exchange_only_and_quantity_mismatch() {
        let token_a = Symbol::new("111");
        let token_b = Symbol::new("222");
        let local = HashMap::from([(
            token_a.clone(),
            ports::Position {
                symbol: token_a.clone(),
                quantity: Quantity(Decimal::from(2)),
                avg_price: Price(Decimal::new(45, 2)),
                unrealized_pnl: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
            },
        )]);
        let snapshot = WorkerReconcileSnapshot {
            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                client_index: 0,
                venue: Some(VenueId::POLYMARKET),
                account_id: None,
                open_orders: Ok(Vec::new()),
                balances: None,
                positions: Some(Ok(vec![
                    ports::Position {
                        symbol: token_a.clone(),
                        quantity: Quantity(Decimal::from(3)),
                        avg_price: Price(Decimal::new(45, 2)),
                        unrealized_pnl: Decimal::ZERO,
                        realized_pnl: Decimal::ZERO,
                    },
                    ports::Position {
                        symbol: token_b.clone(),
                        quantity: Quantity(Decimal::ONE),
                        avg_price: Price(Decimal::new(55, 2)),
                        unrealized_pnl: Decimal::ZERO,
                        realized_pnl: Decimal::ZERO,
                    },
                ])),
                recent_fills: None,
            }],
        };

        let report = reconcile_positions(&snapshot, &local).expect("position report");

        assert!(report.complete);
        assert!(!report.healthy);
        assert_eq!(report.exchange_only, vec![token_b]);
        assert_eq!(report.quantity_mismatch.len(), 1);
        assert_eq!(report.quantity_mismatch[0].symbol, token_a);
    }

    #[test]
    fn position_reconciliation_keeps_polymarket_local_only_with_unsupported_other_venue() {
        let token = Symbol::new("123456789");
        let local = HashMap::from([(
            token.clone(),
            ports::Position {
                symbol: token.clone(),
                quantity: Quantity(Decimal::ONE),
                avg_price: Price(Decimal::new(50, 2)),
                unrealized_pnl: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
            },
        )]);
        let snapshot = WorkerReconcileSnapshot {
            clients: vec![
                crate::execution_worker::ClientReconcileSnapshot {
                    client_index: 0,
                    venue: Some(VenueId::POLYMARKET),
                    account_id: None,
                    open_orders: Ok(Vec::new()),
                    balances: None,
                    positions: Some(Ok(Vec::new())),
                    recent_fills: None,
                },
                crate::execution_worker::ClientReconcileSnapshot {
                    client_index: 1,
                    venue: Some(VenueId::BINANCE),
                    account_id: None,
                    open_orders: Ok(Vec::new()),
                    balances: None,
                    positions: None,
                    recent_fills: None,
                },
            ],
        };

        let report = reconcile_positions(&snapshot, &local).expect("position report");

        assert!(!report.complete);
        assert!(!report.healthy);
        assert_eq!(report.local_only, vec![token]);
        assert!(report.client_errors.iter().any(|error| {
            error.contains("client=1")
                && error.contains("venue=BINANCE")
                && error.contains("does not support")
        }));
    }

    #[test]
    fn position_reconciliation_fails_incomplete_when_local_venue_is_ambiguous() {
        let symbol = Symbol::new("BTCUSDT");
        let local = HashMap::from([(
            symbol.clone(),
            ports::Position {
                symbol,
                quantity: Quantity(Decimal::ONE),
                avg_price: Price(Decimal::from(100)),
                unrealized_pnl: Decimal::ZERO,
                realized_pnl: Decimal::ZERO,
            },
        )]);
        let snapshot = WorkerReconcileSnapshot {
            clients: vec![
                crate::execution_worker::ClientReconcileSnapshot {
                    client_index: 0,
                    venue: Some(VenueId::POLYMARKET),
                    account_id: None,
                    open_orders: Ok(Vec::new()),
                    balances: None,
                    positions: Some(Ok(Vec::new())),
                    recent_fills: None,
                },
                crate::execution_worker::ClientReconcileSnapshot {
                    client_index: 1,
                    venue: Some(VenueId::BINANCE),
                    account_id: Some(hft_core::AccountId("binance-main".to_string())),
                    open_orders: Ok(Vec::new()),
                    balances: None,
                    positions: None,
                    recent_fills: None,
                },
            ],
        };

        let report = reconcile_positions(&snapshot, &local).expect("position report");

        assert!(!report.complete);
        assert!(!report.healthy);
        assert!(report.local_only.is_empty());
        assert!(report
            .client_errors
            .iter()
            .any(|error| error.contains("account=binance-main")));
        assert!(report
            .client_errors
            .iter()
            .any(|error| error.contains("cannot safely attribute")));
    }

    #[tokio::test]
    async fn account_inspection_requests_balances_positions_and_recent_fills() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineConfig::default())));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::Reconcile {
                    include_balances,
                    include_positions,
                    include_recent_fills,
                    reply,
                } => {
                    assert!(include_balances);
                    assert!(include_positions);
                    assert!(include_recent_fills);
                    reply
                        .send(WorkerReconcileSnapshot::default())
                        .expect("send inspection snapshot");
                }
                _ => panic!("unexpected control command"),
            }
        });

        control.inspect_account().await.expect("inspect account");
        worker.await.expect("worker task");
    }

    fn polymarket_account_snapshot(open_orders: Vec<ports::OpenOrder>) -> WorkerReconcileSnapshot {
        let token = Symbol::new("112548421964662546558474258688565408276");
        WorkerReconcileSnapshot {
            clients: vec![crate::execution_worker::ClientReconcileSnapshot {
                client_index: 0,
                venue: Some(VenueId::POLYMARKET),
                account_id: Some(hft_core::AccountId("polymarket-main".to_string())),
                open_orders: Ok(open_orders),
                balances: Some(Ok(vec![balance(
                    "USDC",
                    Decimal::from(80),
                    Some(Decimal::from(80)),
                )])),
                positions: Some(Ok(vec![ports::Position {
                    symbol: token.clone(),
                    quantity: Quantity(Decimal::from(2)),
                    avg_price: Price(Decimal::new(45, 2)),
                    unrealized_pnl: Decimal::new(10, 2),
                    realized_pnl: Decimal::ZERO,
                }])),
                recent_fills: Some(Ok(vec![ports::AccountFill {
                    fill_id: "historical-fill-1".to_string(),
                    order_id: OrderId("historical-order-1".to_string()),
                    symbol: token,
                    side: Side::Buy,
                    price: Price(Decimal::new(45, 2)),
                    quantity: Quantity(Decimal::from(2)),
                    fee: None,
                    timestamp: 1,
                }])),
            }],
        }
    }

    #[tokio::test]
    async fn pristine_polymarket_account_bootstrap_imports_authoritative_cash_and_positions() {
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_portfolio_manager(Box::new(TestPortfolio::default()));
        let engine = Arc::new(Mutex::new(engine));
        let control = ExecutionControlHandle::new(engine.clone(), None, false);

        assert!(control
            .bootstrap_pristine_account(&polymarket_account_snapshot(Vec::new()))
            .await
            .expect("bootstrap Polymarket account"));

        let account = engine.lock().await.get_account_view();
        assert_eq!(account.cash_balance, Decimal::from(80));
        assert_eq!(account.positions.len(), 1);
        assert_eq!(account.unrealized_pnl, Decimal::new(10, 2));
        assert_eq!(account.high_water_mark, account.equity());
        assert!(account.session_start_us > 0);
        drop(account);
        let state = engine.lock().await.export_portfolio_state();
        assert!(state
            .processed_fill_ids
            .get(&OrderId("historical-order-1".to_string()))
            .is_some_and(|ids| ids.contains("historical-fill-1")));

        assert!(!control
            .bootstrap_pristine_account(&polymarket_account_snapshot(Vec::new()))
            .await
            .expect("non-pristine portfolio must not be overwritten"));
    }

    #[tokio::test]
    async fn account_bootstrap_is_polymarket_only_and_refuses_exchange_open_orders() {
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_portfolio_manager(Box::new(TestPortfolio::default()));
        let control = ExecutionControlHandle::new(Arc::new(Mutex::new(engine)), None, false);

        let mut other_venue = polymarket_account_snapshot(Vec::new());
        other_venue.clients[0].venue = Some(VenueId::MOCK);
        assert!(!control
            .bootstrap_pristine_account(&other_venue)
            .await
            .expect("other venues are not eligible for bootstrap"));

        let token = Symbol::new("112548421964662546558474258688565408276");
        let open_order = ports::OpenOrder {
            order_id: OrderId("venue-order-1".to_string()),
            client_order_id: Some("monday-order-1".to_string()),
            symbol: token,
            side: Side::Buy,
            order_type: hft_core::OrderType::Limit,
            original_quantity: Quantity(Decimal::ONE),
            remaining_quantity: Quantity(Decimal::ONE),
            filled_quantity: Quantity::zero(),
            price: Some(Price(Decimal::new(50, 2))),
            status: OrderStatus::Accepted,
            created_at: 1,
            updated_at: 1,
        };
        let error = control
            .bootstrap_pristine_account(&polymarket_account_snapshot(vec![open_order]))
            .await
            .expect_err("exchange-open orders must block bootstrap");
        assert!(error.to_string().contains("exchange-open orders"));
    }

    #[tokio::test]
    async fn replacement_round_trips_through_execution_worker_control() {
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_order_manager(Box::new(ReconcileOrderManager {
            order: OrderRecord {
                order_id: OrderId("venue-order-1".to_string()),
                client_order_id: None,
                account_id: None,
                symbol: Symbol::new("123"),
                side: Side::Buy,
                qty: Quantity(Decimal::from(3)),
                cum_qty: Quantity::zero(),
                avg_price: None,
                status: OrderStatus::Acknowledged,
                venue: Some(VenueId::POLYMARKET),
                strategy_id: Some("test".to_string()),
            },
        }));
        let engine = Arc::new(Mutex::new(engine));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control = ExecutionControlHandle::new(engine, Some(worker_tx), true);
        let worker = tokio::spawn(async move {
            match worker_rx.recv().await.expect("control command") {
                ControlCommand::ReplaceOrder {
                    order_id,
                    symbol,
                    new_quantity,
                    new_price,
                    reply,
                } => {
                    assert_eq!(order_id.0, "venue-order-1");
                    assert_eq!(symbol.as_str(), "123");
                    assert_eq!(new_quantity, Some(Quantity(Decimal::from(2))));
                    assert_eq!(new_price, Some(Price(Decimal::new(51, 2))));
                    reply.send(Ok(())).expect("send replacement result");
                }
                _ => panic!("unexpected control command"),
            }
        });

        control
            .replace_order(
                OrderId("venue-order-1".to_string()),
                Symbol::new("123"),
                Some(Quantity(Decimal::from(2))),
                Some(Price(Decimal::new(51, 2))),
            )
            .await
            .expect("replace order");
        worker.await.expect("worker task");
    }

    #[tokio::test]
    async fn replacement_rejects_unknown_partial_and_quantity_increasing_orders() {
        let order_id = OrderId("venue-order-1".to_string());
        let symbol = Symbol::new("123");
        let mut engine = Engine::new(EngineConfig::default());
        engine.set_order_manager(Box::new(ReconcileOrderManager {
            order: OrderRecord {
                order_id: order_id.clone(),
                client_order_id: None,
                account_id: None,
                symbol: symbol.clone(),
                side: Side::Buy,
                qty: Quantity(Decimal::from(3)),
                cum_qty: Quantity(Decimal::ONE),
                avg_price: Some(Price(Decimal::new(50, 2))),
                status: OrderStatus::PartiallyFilled,
                venue: Some(VenueId::POLYMARKET),
                strategy_id: Some("test".to_string()),
            },
        }));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control =
            ExecutionControlHandle::new(Arc::new(Mutex::new(engine)), Some(worker_tx), true);

        let unknown = control
            .replace_order(
                OrderId("unknown".to_string()),
                symbol.clone(),
                Some(Quantity(Decimal::ONE)),
                None,
            )
            .await
            .expect_err("unknown replacement must fail");
        assert!(unknown.to_string().contains("OMS-tracked"));

        let partial = control
            .replace_order(order_id, symbol, Some(Quantity(Decimal::from(4))), None)
            .await
            .expect_err("partial replacement must fail before quantity evaluation");
        assert!(partial.to_string().contains("partially filled"));
        assert!(
            worker_rx.try_recv().is_err(),
            "rejected replacements must not reach the worker"
        );

        let mut engine = Engine::new(EngineConfig::default());
        engine.set_order_manager(Box::new(ReconcileOrderManager {
            order: OrderRecord {
                order_id: OrderId("unfilled-order".to_string()),
                client_order_id: None,
                account_id: None,
                symbol: Symbol::new("123"),
                side: Side::Buy,
                qty: Quantity(Decimal::from(3)),
                cum_qty: Quantity::zero(),
                avg_price: None,
                status: OrderStatus::Acknowledged,
                venue: Some(VenueId::POLYMARKET),
                strategy_id: Some("test".to_string()),
            },
        }));
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let control =
            ExecutionControlHandle::new(Arc::new(Mutex::new(engine)), Some(worker_tx), true);
        let increase = control
            .replace_order(
                OrderId("unfilled-order".to_string()),
                Symbol::new("123"),
                Some(Quantity(Decimal::from(4))),
                None,
            )
            .await
            .expect_err("quantity-increasing replacement must fail");
        assert!(increase.to_string().contains("cannot increase"));
        assert!(
            worker_rx.try_recv().is_err(),
            "risk-increasing replacement must not reach the worker"
        );
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
