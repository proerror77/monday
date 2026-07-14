//! Unified strategy runtime engine.
//!
//! [`StrategyRuntime`] is the core loop that connects a [`Feed`], a
//! [`StrategyLogic`], and an [`Executor`] through the canonical
//! [`ploy_trading::TradingRuntime`] lifecycle.
//!
//! The same `StrategyRuntime` drives backtest, dry-run, and live trading —
//! only the trait implementations differ.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ploy_trading::{OrderState, PnlSnapshot, RiskSnapshot, TradingIntent, TradingRuntime};
use rust_decimal::Decimal;
use tracing::{debug, info, warn};
use uuid::Uuid;

use ploy_trading::IntentPurpose;
use rust_decimal_macros::dec;

use crate::traits::{
    Executor, Feed, MarketUpdate, Recorder, StrategyDecision, StrategyLogic, SubmitOutcome,
};

const MIN_LIVE_RETRY_SHARES: Decimal = dec!(1.00);
const MIN_LIVE_RETRY_NOTIONAL: Decimal = dec!(1.00);
const DIAGNOSTIC_LOG_INTERVAL_UPDATES: u64 = 50_000;

/// Operating mode for the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Replay historical data with simulated execution.
    Backtest,
    /// Replay a previously recorded canonical market-update log.
    Replay,
    /// Live market data with simulated execution.
    DryRun,
    /// Live market data with real exchange execution.
    Live,
}

/// Configuration for the runtime loop.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub mode: RuntimeMode,
    /// Maximum signal evaluations per second. `None` = unlimited.
    pub throttle_hz: Option<u32>,
    /// Stop after N updates (backtest bound). `None` = run forever.
    pub max_updates: Option<u64>,
    /// Skip settlement exit orders (live mode: Polymarket auto-settles on-chain).
    pub skip_settlement_exits: bool,
}

/// Summary produced at the end of a runtime session.
#[derive(Debug, Clone)]
pub struct RuntimeResult {
    pub mode: RuntimeMode,
    pub updates_processed: u64,
    pub quote_updates_observed: u64,
    pub depth_quote_updates_observed: u64,
    pub intents_submitted: u64,
    pub fills_recorded: u64,
    pub non_settlement_fills_observed: u64,
    pub full_depth_fills_observed: u64,
    pub pnl: PnlSnapshot,
    pub risk: RiskSnapshot,
    pub elapsed_secs: f64,
    pub strategy_diagnostics: Vec<(String, u64)>,
}

#[derive(Debug, Clone)]
struct PendingLiveOrder {
    intent: TradingIntent,
    attempts: u8,
    reconciles_without_fill: u8,
}

fn retry_attempt_from_intent_id(intent_id: &str) -> u8 {
    retry_suffix(intent_id).map_or(1, |(_, attempt)| attempt.max(1))
}

fn retry_root_intent_id(intent_id: &str) -> &str {
    retry_suffix(intent_id).map_or(intent_id, |(root, _)| root)
}

fn retry_suffix(intent_id: &str) -> Option<(&str, u8)> {
    let (root, suffix) = intent_id.rsplit_once("_retry")?;
    if root.is_empty() || suffix.is_empty() || !suffix.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    Some((root, suffix.parse().ok()?))
}

fn live_retry_remainder_is_dust(remaining_qty: Decimal, limit_price: Option<Decimal>) -> bool {
    if remaining_qty < MIN_LIVE_RETRY_SHARES {
        return true;
    }

    limit_price
        .map(|price| remaining_qty * price < MIN_LIVE_RETRY_NOTIONAL)
        .unwrap_or(false)
}

fn observe_fill_evidence(
    intent: &TradingIntent,
    price_basis: Option<&str>,
    non_settlement_fills_observed: &mut u64,
    full_depth_fills_observed: &mut u64,
) {
    let is_settlement = intent.purpose == IntentPurpose::Exit
        && intent
            .limit_price
            .is_some_and(|price| price == dec!(0) || price == dec!(1));
    if is_settlement || price_basis == Some("settlement") {
        return;
    }
    *non_settlement_fills_observed += 1;
    if price_basis == Some("full_depth_sweep") {
        *full_depth_fills_observed += 1;
    }
}

/// Unified strategy runtime.
///
/// Generic over:
/// - `S`: strategy logic (signal evaluation)
/// - `F`: data feed (historical or live)
/// - `E`: order executor (simulated or real)
pub struct StrategyRuntime<S, F, E>
where
    S: StrategyLogic,
    F: Feed,
    E: Executor,
{
    strategy: S,
    feed: F,
    executor: E,
    recorder: Box<dyn Recorder>,
    trading: TradingRuntime,
    config: RuntimeConfig,
    deployment_id: Option<String>,
}

impl<S, F, E> StrategyRuntime<S, F, E>
where
    S: StrategyLogic,
    F: Feed,
    E: Executor,
{
    /// Create a new runtime with the given components.
    pub fn new(
        strategy: S,
        feed: F,
        executor: E,
        recorder: Box<dyn Recorder>,
        config: RuntimeConfig,
    ) -> Self {
        Self::new_with_trading(
            strategy,
            feed,
            executor,
            recorder,
            config,
            TradingRuntime::default(),
        )
    }

    pub fn new_with_trading(
        strategy: S,
        feed: F,
        executor: E,
        recorder: Box<dyn Recorder>,
        config: RuntimeConfig,
        trading: TradingRuntime,
    ) -> Self {
        Self {
            strategy,
            feed,
            executor,
            recorder,
            trading,
            config,
            deployment_id: None,
        }
    }

    /// Attach the platform deployment identity used to attribute strategy orders.
    pub fn with_deployment_id(mut self, deployment_id: impl Into<String>) -> Self {
        let deployment_id = deployment_id.into();
        let deployment_id = deployment_id.trim();
        if !deployment_id.is_empty() {
            self.deployment_id = Some(deployment_id.to_string());
        }
        self
    }

    /// Run the strategy loop until the feed is exhausted or max_updates reached.
    pub async fn run(&mut self) -> RuntimeResult {
        let start = std::time::Instant::now();
        let mut updates_processed: u64 = 0;
        let mut quote_updates_observed: u64 = 0;
        let mut depth_quote_updates_observed: u64 = 0;
        let mut intents_submitted: u64 = 0;
        let mut fills_recorded: u64 = 0;
        let mut non_settlement_fills_observed: u64 = 0;
        let mut full_depth_fills_observed: u64 = 0;
        let mut last_eval_ts: Option<DateTime<Utc>> = None;
        let mut pending_live_orders = self.initial_pending_live_orders();

        info!(
            mode = ?self.config.mode,
            strategy = self.strategy.name(),
            "StrategyRuntime started",
        );

        'runtime: while let Some(update) = self.feed.next().await {
            updates_processed += 1;
            if let MarketUpdate::Quote {
                bid_levels,
                ask_levels,
                ..
            } = &update
            {
                quote_updates_observed += 1;
                if !bid_levels.is_empty() || !ask_levels.is_empty() {
                    depth_quote_updates_observed += 1;
                }
            }
            self.executor.observe_market_update(&update);

            if let MarketUpdate::EventExpired { event_id, .. } = &update {
                let canceled = self
                    .trading
                    .cancel_active_entry_orders_for_market(event_id.as_ref());
                if canceled > 0 {
                    debug!(
                        event_id = %event_id,
                        canceled,
                        "Canceled active entry orders for expired event",
                    );
                }
            }

            // Throttle: skip high-frequency evaluation updates if within the same time slot.
            // Lifecycle and quote updates always pass through because they mutate strategy state
            // that must stay aligned with the executor's order-book view.
            if let Some(hz) = self.config.throttle_hz {
                if !Self::bypasses_throttle(&update) {
                    if let Some(ts) = Self::update_ts(&update) {
                        if let Some(last) = last_eval_ts {
                            let min_gap_ms = 1000 / hz as i64;
                            if (ts - last).num_milliseconds() < min_gap_ms {
                                continue;
                            }
                        }
                        last_eval_ts = Some(ts);
                    }
                }
            }

            // 1. Strategy evaluates the update.
            let decisions =
                self.strategy
                    .on_update(&update, self.trading.positions(), self.trading.orders());

            // 1b. In live mode, filter out settlement exits (Polymarket auto-settles on-chain).
            let decisions: Vec<StrategyDecision> = if self.config.skip_settlement_exits {
                decisions
                    .into_iter()
                    .filter(|d| {
                        !matches!(d, StrategyDecision::Exit(intent)
                            if intent.purpose == IntentPurpose::Exit
                            && intent.limit_price.map_or(false, |p| p == dec!(0) || p == dec!(1)))
                    })
                    .collect()
            } else {
                decisions
            };

            // 2. Execute each decision.
            for decision in decisions {
                let (mut intent, signal) = match decision {
                    StrategyDecision::Enter { intent, signal } => (intent, signal),
                    StrategyDecision::Exit(intent) => (intent, None),
                    StrategyDecision::Hold => continue,
                };
                self.ensure_deployment_attribution(&mut intent);
                let strategy_name = self.strategy.name().to_string();
                let signal_ref = signal.as_ref();

                let mut intent = self.executor.prepare_intent(&intent);
                self.ensure_deployment_attribution(&mut intent);
                let order_id = Uuid::new_v4().to_string();

                let report = self.executor.submit(&intent, &order_id).await;
                let mut halt_after_submission = false;
                if let Some(signal) = signal_ref {
                    if let Err(error) = self.recorder.record_signal(signal).await {
                        warn!(%error, "Failed to persist signal record");
                        if self.config.mode == RuntimeMode::Live {
                            halt_after_submission = true;
                        }
                    }
                }
                if let Err(error) = self
                    .recorder
                    .record_order(&strategy_name, &intent, signal_ref, &report, &order_id)
                    .await
                {
                    warn!(%error, "Failed to persist order record");
                    if self.config.mode == RuntimeMode::Live {
                        panic!("live recorder failed: {error}");
                    }
                }

                if report.submit_outcome() == SubmitOutcome::Unknown {
                    warn!(
                        order_id = %order_id,
                        error = report.rejection_reason.as_deref().unwrap_or("transport ambiguity"),
                        "Live submission outcome is unknown; stopping without retry"
                    );
                    break 'runtime;
                }

                if report.submit_outcome() == SubmitOutcome::Rejected && report.fill.is_none() {
                    // Pure rejection — keep the signal audit trail, but don't record an intent.
                    // Notify the strategy so it can arm cooldowns and avoid hammering the same
                    // signal on every tick (e.g. balance exhausted, FAK no match).
                    let reason = report.rejection_reason.as_deref().unwrap_or("unknown");
                    warn!(order_id = %order_id, reason = %reason, "Order rejected");
                    self.strategy.on_reject(&intent, reason);
                    if halt_after_submission {
                        break 'runtime;
                    }
                    continue;
                }

                // Intent accepted (possibly with fill)
                if let Err(err) = self
                    .trading
                    .submit_intent(intent.clone(), order_id.clone(), None)
                {
                    let reason = err.to_string();
                    warn!(order_id = %order_id, reason = %reason, "Ignoring invalid intent");
                    self.strategy.on_reject(&intent, &reason);
                    continue;
                }
                if !report.order_id.is_empty() && report.order_id != order_id {
                    self.trading
                        .acknowledge_order(&order_id, report.order_id.clone());
                }
                intents_submitted += 1;

                if let Some(fill) = report.fill.as_ref() {
                    if !self.trading.record_fill(fill.clone()) {
                        warn!(
                            order_id = %order_id,
                            fill_id = %fill.fill_id,
                            qty = %fill.quantity,
                            "Ignoring immediate duplicate, orphan, or overfilled fill",
                        );
                        continue;
                    }
                    if let Err(error) = self
                        .recorder
                        .record_fill(&strategy_name, &intent, signal_ref, fill, &report)
                        .await
                    {
                        warn!(%error, "Failed to persist fill record");
                        if self.config.mode == RuntimeMode::Live {
                            panic!("live recorder failed: {error}");
                        }
                    }
                    self.strategy.on_fill(&fill);
                    fills_recorded += 1;
                    observe_fill_evidence(
                        &intent,
                        report.price_basis,
                        &mut non_settlement_fills_observed,
                        &mut full_depth_fills_observed,
                    );
                    debug!(
                        order_id = %order_id,
                        token = %fill.token_id,
                        qty = %fill.quantity,
                        price = %fill.price,
                        "Fill recorded",
                    );
                } else if self.config.mode == RuntimeMode::Live && !report.rejected {
                    pending_live_orders.insert(
                        order_id.clone(),
                        PendingLiveOrder {
                            intent: intent.clone(),
                            attempts: 1,
                            reconciles_without_fill: 0,
                        },
                    );
                    debug!(
                        order_id = %order_id,
                        token = %intent.token_id,
                        "Live order acknowledged without fill; waiting for reconciliation",
                    );
                }
                if halt_after_submission {
                    warn!(
                        order_id = %order_id,
                        "Stopping live runtime after post-submit signal audit failure"
                    );
                    break 'runtime;
                }
            }

            let reconcile_completed = match self.reconcile_active_fills().await {
                Ok(fills) => {
                    let strategy_name = self.strategy.name().to_string();
                    for fill in fills {
                        let Some(order_before) = self.trading.order(&fill.order_id).cloned() else {
                            warn!(
                                order_id = %fill.order_id,
                                fill_id = %fill.fill_id,
                                "Ignoring reconciled fill for unknown order",
                            );
                            continue;
                        };
                        let Some(intent) = self.trading.intent(&order_before.intent_id).cloned()
                        else {
                            warn!(
                                order_id = %fill.order_id,
                                fill_id = %fill.fill_id,
                                intent_id = %order_before.intent_id,
                                "Ignoring reconciled fill for unknown intent",
                            );
                            continue;
                        };
                        if !self.trading.record_fill(fill.clone()) {
                            warn!(
                                order_id = %fill.order_id,
                                fill_id = %fill.fill_id,
                                qty = %fill.quantity,
                                "Ignoring duplicate, orphan, or overfilled reconciled fill",
                            );
                            continue;
                        }

                        let report = crate::traits::ExecutionReport {
                            order_id: order_before
                                .venue_order_id
                                .clone()
                                .unwrap_or_else(|| fill.order_id.clone()),
                            fill: Some(fill.clone()),
                            rejected: false,
                            rejection_reason: None,
                            slippage: None,
                            market_impact: None,
                            price_basis: None,
                        };

                        if let Err(error) = self
                            .recorder
                            .record_fill(&strategy_name, &intent, None, &fill, &report)
                            .await
                        {
                            if self.config.mode == RuntimeMode::Live {
                                panic!("live recorder failed: {error}");
                            }
                            warn!(%error, "Failed to persist reconciled fill");
                        }
                        if let Err(error) = self
                            .recorder
                            .record_order(&strategy_name, &intent, None, &report, &fill.order_id)
                            .await
                        {
                            if self.config.mode == RuntimeMode::Live {
                                panic!("live recorder failed: {error}");
                            }
                            warn!(%error, "Failed to persist reconciled order");
                        }
                        self.strategy.on_fill(&fill);
                        fills_recorded += 1;
                        observe_fill_evidence(
                            &intent,
                            report.price_basis,
                            &mut non_settlement_fills_observed,
                            &mut full_depth_fills_observed,
                        );
                        if self
                            .trading
                            .order(&fill.order_id)
                            .is_some_and(|order| order.state == OrderState::Filled)
                        {
                            pending_live_orders.remove(&fill.order_id);
                        } else if let Some(pending) = pending_live_orders.get_mut(&fill.order_id) {
                            pending.reconciles_without_fill = 0;
                        }
                        debug!(
                            order_id = %fill.order_id,
                            token = %fill.token_id,
                            qty = %fill.quantity,
                            price = %fill.price,
                            "Reconciled fill recorded",
                        );
                    }
                    self.executor.last_reconcile_attempted()
                }
                Err(error) => {
                    warn!(error = %error, "Fill reconciliation failed");
                    if self.config.mode == RuntimeMode::Live {
                        panic!("live fill reconciliation failed: {error}");
                    }
                    false
                }
            };

            if reconcile_completed && self.executor.owns_live_retries() {
                if let Err(error) = self
                    .process_live_unfilled_orders(
                        &mut pending_live_orders,
                        &mut intents_submitted,
                        &mut fills_recorded,
                        &mut non_settlement_fills_observed,
                        &mut full_depth_fills_observed,
                    )
                    .await
                {
                    panic!("live recorder failed: {error}");
                }
            }

            if updates_processed % DIAGNOSTIC_LOG_INTERVAL_UPDATES == 0 {
                info!(
                    mode = ?self.config.mode,
                    strategy = self.strategy.name(),
                    updates = updates_processed,
                    intents = intents_submitted,
                    fills = fills_recorded,
                    diagnostics = ?self.strategy.diagnostics(),
                    "StrategyRuntime diagnostic checkpoint",
                );
            }

            // 3. Check update limit (backtest bound).
            if let Some(max) = self.config.max_updates {
                if updates_processed >= max {
                    info!(updates = updates_processed, "Max updates reached, stopping");
                    break;
                }
            }
        }

        if let Err(error) = self.recorder.flush().await {
            warn!(%error, "Failed to flush recorder");
            if self.config.mode == RuntimeMode::Live {
                panic!("live recorder flush failed: {error}");
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let mark_prices: BTreeMap<String, Decimal> = BTreeMap::new();
        let snapshot = self.trading.snapshot(&mark_prices);

        let result = RuntimeResult {
            mode: self.config.mode,
            updates_processed,
            quote_updates_observed,
            depth_quote_updates_observed,
            intents_submitted,
            fills_recorded,
            non_settlement_fills_observed,
            full_depth_fills_observed,
            pnl: snapshot.pnl,
            risk: snapshot.risk,
            elapsed_secs: elapsed,
            strategy_diagnostics: self.strategy.diagnostics(),
        };

        info!(
            mode = ?result.mode,
            updates = result.updates_processed,
            intents = result.intents_submitted,
            fills = result.fills_recorded,
            elapsed = format!("{:.1}s", result.elapsed_secs),
            net_pnl = %result.pnl.net_pnl(),
            diagnostics = ?result.strategy_diagnostics,
            "StrategyRuntime finished",
        );

        result
    }

    /// Read-only access to the trading runtime state.
    pub fn trading(&self) -> &TradingRuntime {
        &self.trading
    }

    async fn reconcile_active_fills(&mut self) -> Result<Vec<ploy_trading::FillRecord>, String> {
        if self.trading.orders().active_orders() == 0 {
            return Ok(Vec::new());
        }
        self.executor.reconcile_fills(self.trading.orders()).await
    }

    fn ensure_deployment_attribution(&self, intent: &mut TradingIntent) {
        if intent.deployment_id.is_empty() {
            if let Some(deployment_id) = self.deployment_id.as_deref() {
                intent.deployment_id = deployment_id.to_string();
            }
        }

        if intent.deployment_id.is_empty()
            && matches!(self.config.mode, RuntimeMode::Live | RuntimeMode::DryRun)
        {
            panic!(
                "strategy runtime in {:?} emitted intent {} without deployment_id and runtime deployment_id is not configured",
                self.config.mode, intent.intent_id
            );
        }
    }

    fn initial_pending_live_orders(&self) -> BTreeMap<String, PendingLiveOrder> {
        if self.config.mode != RuntimeMode::Live {
            return BTreeMap::new();
        }

        self.trading
            .orders()
            .orders()
            .filter(|order| {
                order.venue_order_id.is_some()
                    && matches!(
                        order.state,
                        OrderState::Acknowledged | OrderState::PartiallyFilled
                    )
            })
            .filter_map(|order| {
                let intent = self.trading.intent(&order.intent_id)?.clone();
                Some((
                    order.order_id.clone(),
                    PendingLiveOrder {
                        intent,
                        attempts: retry_attempt_from_intent_id(&order.intent_id),
                        reconciles_without_fill: 0,
                    },
                ))
            })
            .collect()
    }

    async fn process_live_unfilled_orders(
        &mut self,
        pending_live_orders: &mut BTreeMap<String, PendingLiveOrder>,
        intents_submitted: &mut u64,
        fills_recorded: &mut u64,
        non_settlement_fills_observed: &mut u64,
        full_depth_fills_observed: &mut u64,
    ) -> Result<(), String> {
        if self.config.mode != RuntimeMode::Live || pending_live_orders.is_empty() {
            return Ok(());
        }

        let policy = self.executor.execution_policy();
        let strategy_name = self.strategy.name().to_string();
        let order_ids = pending_live_orders.keys().cloned().collect::<Vec<_>>();

        for order_id in order_ids {
            let Some(mut pending) = pending_live_orders.remove(&order_id) else {
                continue;
            };
            let Some(order) = self.trading.order(&order_id).cloned() else {
                continue;
            };

            if !matches!(
                order.state,
                OrderState::Acknowledged | OrderState::PartiallyFilled
            ) {
                continue;
            }

            let remaining_qty = (order.requested_qty - order.filled_qty).max(Decimal::ZERO);
            if remaining_qty <= Decimal::ZERO {
                continue;
            }

            pending.reconciles_without_fill += 1;
            if pending.reconciles_without_fill < policy.reconcile_cycles_before_retry {
                pending_live_orders.insert(order_id, pending);
                continue;
            }

            if live_retry_remainder_is_dust(remaining_qty, order.limit_price) {
                let terminal_reason = format!(
                    "live order remainder below retry threshold; remaining_qty={remaining_qty}"
                );
                self.trading.cancel_order(&order_id);
                let terminal_report = crate::traits::ExecutionReport {
                    order_id: order
                        .venue_order_id
                        .clone()
                        .unwrap_or_else(|| order_id.clone()),
                    fill: None,
                    rejected: true,
                    rejection_reason: Some(terminal_reason.clone()),
                    slippage: None,
                    market_impact: None,
                    price_basis: None,
                };
                self.recorder
                    .record_order(
                        &strategy_name,
                        &pending.intent,
                        None,
                        &terminal_report,
                        &order_id,
                    )
                    .await?;
                self.strategy.on_reject(&pending.intent, &terminal_reason);
                warn!(order_id = %order_id, reason = %terminal_reason, "Live order terminal dust");
                continue;
            }

            let terminal_reason = if pending.attempts >= policy.max_attempts {
                format!(
                    "live order unfilled after {} attempt(s); remaining_qty={remaining_qty}",
                    pending.attempts
                )
            } else {
                format!(
                    "live FAK attempt unfilled; retrying remaining_qty={remaining_qty} after {} attempt(s)",
                    pending.attempts
                )
            };
            self.trading.cancel_order(&order_id);
            let terminal_report = crate::traits::ExecutionReport {
                order_id: order
                    .venue_order_id
                    .clone()
                    .unwrap_or_else(|| order_id.clone()),
                fill: None,
                rejected: true,
                rejection_reason: Some(terminal_reason.clone()),
                slippage: None,
                market_impact: None,
                price_basis: None,
            };
            self.recorder
                .record_order(
                    &strategy_name,
                    &pending.intent,
                    None,
                    &terminal_report,
                    &order_id,
                )
                .await?;

            if pending.attempts >= policy.max_attempts {
                self.strategy.on_reject(&pending.intent, &terminal_reason);
                warn!(order_id = %order_id, reason = %terminal_reason, "Live order terminal unfilled");
                continue;
            }

            let retry_attempt = pending.attempts + 1;
            let mut retry_intent = pending.intent.clone();
            retry_intent.intent_id = format!(
                "{}_retry{retry_attempt}",
                retry_root_intent_id(&pending.intent.intent_id)
            );
            retry_intent.quantity = remaining_qty;
            retry_intent.created_at = Utc::now();
            self.ensure_deployment_attribution(&mut retry_intent);
            let mut retry_intent = self.executor.prepare_intent(&retry_intent);
            self.ensure_deployment_attribution(&mut retry_intent);
            let retry_order_id = Uuid::new_v4().to_string();
            let report = self.executor.submit(&retry_intent, &retry_order_id).await;
            self.recorder
                .record_order(
                    &strategy_name,
                    &retry_intent,
                    None,
                    &report,
                    &retry_order_id,
                )
                .await?;

            if report.rejected && report.fill.is_none() {
                let reason = report.rejection_reason.as_deref().unwrap_or("unknown");
                warn!(
                    order_id = %retry_order_id,
                    attempt = retry_attempt,
                    reason = %reason,
                    "Live retry rejected",
                );
                self.strategy.on_reject(&retry_intent, reason);
                continue;
            }

            if let Err(err) =
                self.trading
                    .submit_intent(retry_intent.clone(), retry_order_id.clone(), None)
            {
                let reason = err.to_string();
                warn!(order_id = %retry_order_id, reason = %reason, "Ignoring invalid retry intent");
                self.strategy.on_reject(&retry_intent, &reason);
                continue;
            }
            if !report.order_id.is_empty() && report.order_id != retry_order_id {
                self.trading
                    .acknowledge_order(&retry_order_id, report.order_id.clone());
            }
            *intents_submitted += 1;

            if let Some(fill) = report.fill.as_ref() {
                if self.trading.record_fill(fill.clone()) {
                    self.recorder
                        .record_fill(&strategy_name, &retry_intent, None, fill, &report)
                        .await?;
                    self.strategy.on_fill(fill);
                    *fills_recorded += 1;
                    observe_fill_evidence(
                        &retry_intent,
                        report.price_basis,
                        non_settlement_fills_observed,
                        full_depth_fills_observed,
                    );
                }
            } else {
                pending_live_orders.insert(
                    retry_order_id,
                    PendingLiveOrder {
                        intent: retry_intent,
                        attempts: retry_attempt,
                        reconciles_without_fill: 0,
                    },
                );
            }
        }
        Ok(())
    }

    fn bypasses_throttle(update: &MarketUpdate) -> bool {
        matches!(
            update,
            MarketUpdate::EventDiscovered { .. }
                | MarketUpdate::EventExpired { .. }
                | MarketUpdate::Quote { .. }
        )
    }

    /// Extract timestamp from a market update (for throttling).
    fn update_ts(update: &MarketUpdate) -> Option<DateTime<Utc>> {
        match update {
            MarketUpdate::SpotPrice { ts, .. }
            | MarketUpdate::AggTrade { ts, .. }
            | MarketUpdate::Quote { ts, .. }
            | MarketUpdate::L2 { ts, .. }
            | MarketUpdate::L2Depth { ts, .. }
            | MarketUpdate::SportsState { ts, .. }
            | MarketUpdate::SportsPregame { ts, .. }
            | MarketUpdate::SportsLive { ts, .. }
            | MarketUpdate::ReferencePrice { ts, .. }
            | MarketUpdate::Kline { ts, .. } => Some(*ts),
            MarketUpdate::EventDiscovered { end_time, .. } => Some(*end_time),
            MarketUpdate::EventExpired { end_time, .. } => Some(*end_time),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use ploy_trading::{
        FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    use super::{
        observe_fill_evidence, retry_attempt_from_intent_id, retry_root_intent_id, RuntimeConfig,
        RuntimeMode, StrategyRuntime,
    };
    use crate::traits::{
        ExecutionPolicy, ExecutionReport, Executor, Feed, MarketUpdate, NullRecorder, Recorder,
        SignalRecord, StrategyDecision, StrategyLogic,
    };

    struct SingleUpdateFeed {
        next: Option<MarketUpdate>,
    }

    struct MultiUpdateFeed {
        updates: VecDeque<MarketUpdate>,
    }

    #[test]
    fn retry_suffix_parsing_only_treats_numeric_suffix_as_attempt() {
        assert_eq!(retry_attempt_from_intent_id("pm5d_BTCUSDT_UP"), 1);
        assert_eq!(
            retry_root_intent_id("pm5d_BTCUSDT_UP_retry"),
            "pm5d_BTCUSDT_UP_retry"
        );
        assert_eq!(retry_attempt_from_intent_id("pm5d_BTCUSDT_UP_retry"), 1);
        assert_eq!(
            retry_root_intent_id("pm5d_BTCUSDT_UP_retry2"),
            "pm5d_BTCUSDT_UP"
        );
        assert_eq!(retry_attempt_from_intent_id("pm5d_BTCUSDT_UP_retry2"), 2);
    }

    #[test]
    fn settlement_intent_cannot_count_as_full_depth_fillability() {
        let mut non_settlement = 0;
        let mut full_depth = 0;
        let settlement = TradingIntent {
            intent_id: "settlement".into(),
            deployment_id: "test".into(),
            market_id: "event".into(),
            token_id: "token".into(),
            side: TradeSide::Sell,
            quantity: dec!(1),
            limit_price: Some(dec!(1)),
            purpose: IntentPurpose::Exit,
            created_at: Utc::now(),
        };

        observe_fill_evidence(
            &settlement,
            Some("full_depth_sweep"),
            &mut non_settlement,
            &mut full_depth,
        );

        assert_eq!(non_settlement, 0);
        assert_eq!(full_depth, 0);
    }

    #[async_trait]
    impl Feed for SingleUpdateFeed {
        async fn next(&mut self) -> Option<MarketUpdate> {
            self.next.take()
        }
    }

    #[async_trait]
    impl Feed for MultiUpdateFeed {
        async fn next(&mut self) -> Option<MarketUpdate> {
            self.updates.pop_front()
        }
    }

    struct RejectingExecutor;

    #[async_trait]
    impl Executor for RejectingExecutor {
        async fn submit(&mut self, _intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: true,
                rejection_reason: Some("simulated rejection".into()),
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    struct RecordingStrategy {
        emitted: bool,
        signal: SignalRecord,
        intent: TradingIntent,
    }

    struct NoopStrategy;

    impl StrategyLogic for RecordingStrategy {
        fn on_update(
            &mut self,
            _update: &MarketUpdate,
            _positions: &PositionLedger,
            _orders: &OrderLedger,
        ) -> Vec<StrategyDecision> {
            if self.emitted {
                return vec![];
            }
            self.emitted = true;
            vec![StrategyDecision::Enter {
                intent: self.intent.clone(),
                signal: Some(self.signal.clone()),
            }]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {}

        fn name(&self) -> &str {
            "recording_strategy"
        }
    }

    impl StrategyLogic for NoopStrategy {
        fn on_update(
            &mut self,
            _update: &MarketUpdate,
            _positions: &PositionLedger,
            _orders: &OrderLedger,
        ) -> Vec<StrategyDecision> {
            vec![]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {}

        fn name(&self) -> &str {
            "noop_strategy"
        }
    }

    struct CountingStrategy {
        updates: Arc<Mutex<Vec<&'static str>>>,
    }

    impl StrategyLogic for CountingStrategy {
        fn on_update(
            &mut self,
            update: &MarketUpdate,
            _positions: &PositionLedger,
            _orders: &OrderLedger,
        ) -> Vec<StrategyDecision> {
            let kind = match update {
                MarketUpdate::SpotPrice { .. } => "spot",
                MarketUpdate::Quote { .. } => "quote",
                MarketUpdate::EventDiscovered { .. } => "discovered",
                MarketUpdate::EventExpired { .. } => "expired",
                _ => "other",
            };
            self.updates.lock().unwrap().push(kind);
            vec![]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {}

        fn name(&self) -> &str {
            "counting_strategy"
        }
    }

    #[tokio::test]
    async fn quote_updates_bypass_runtime_throttle_to_keep_strategy_book_fresh() {
        let now = Utc::now();
        let seen_updates = Arc::new(Mutex::new(Vec::new()));
        let feed = MultiUpdateFeed {
            updates: VecDeque::from(vec![
                MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price: dec!(100000),
                    ts: now,
                },
                MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price: dec!(100001),
                    ts: now + Duration::milliseconds(100),
                },
                MarketUpdate::Quote {
                    token_id: "token-up".into(),
                    bid: Some(dec!(0.56)),
                    ask: Some(dec!(0.57)),
                    bid_size: Some(dec!(100)),
                    ask_size: Some(dec!(100)),
                    bid_levels: vec![],
                    ask_levels: vec![],
                    ts: now + Duration::milliseconds(200),
                },
                MarketUpdate::Quote {
                    token_id: "token-up".into(),
                    bid: Some(dec!(0.58)),
                    ask: Some(dec!(0.59)),
                    bid_size: Some(dec!(100)),
                    ask_size: Some(dec!(100)),
                    bid_levels: vec![ploy_market_contracts::BookLevel {
                        price: dec!(0.58),
                        size: dec!(100),
                    }],
                    ask_levels: vec![ploy_market_contracts::BookLevel {
                        price: dec!(0.59),
                        size: dec!(100),
                    }],
                    ts: now + Duration::milliseconds(300),
                },
            ]),
        };
        let strategy = CountingStrategy {
            updates: seen_updates.clone(),
        };
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: Some(1),
            max_updates: None,
            skip_settlement_exits: false,
        };

        let mut runtime = StrategyRuntime::new(
            strategy,
            feed,
            RejectingExecutor,
            Box::new(CollectingRecorder {
                signals: Arc::new(Mutex::new(Vec::new())),
                orders: Arc::new(Mutex::new(Vec::new())),
                fills: Arc::new(Mutex::new(Vec::new())),
            }),
            config,
        )
        .with_deployment_id("test.dryrun");
        let result = runtime.run().await;

        assert_eq!(result.updates_processed, 4);
        assert_eq!(result.quote_updates_observed, 2);
        assert_eq!(result.depth_quote_updates_observed, 1);
        assert_eq!(
            seen_updates.lock().unwrap().as_slice(),
            ["spot", "quote", "quote"]
        );
    }

    struct FillCountingStrategy {
        emitted: bool,
        intent: TradingIntent,
        fill_count: Arc<Mutex<u32>>,
    }

    impl StrategyLogic for FillCountingStrategy {
        fn on_update(
            &mut self,
            _update: &MarketUpdate,
            _positions: &PositionLedger,
            _orders: &OrderLedger,
        ) -> Vec<StrategyDecision> {
            if self.emitted {
                return vec![];
            }
            self.emitted = true;
            vec![StrategyDecision::Exit(self.intent.clone())]
        }

        fn on_fill(&mut self, _fill: &FillRecord) {
            *self.fill_count.lock().unwrap() += 1;
        }

        fn name(&self) -> &str {
            "fill_counting_strategy"
        }
    }

    struct CollectingRecorder {
        signals: Arc<Mutex<Vec<SignalRecord>>>,
        orders: Arc<Mutex<Vec<(String, String, String)>>>,
        fills: Arc<Mutex<Vec<String>>>,
    }

    struct FailingRecorder {
        submissions: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Recorder for FailingRecorder {
        async fn record_signal(&mut self, _signal: &SignalRecord) -> Result<(), String> {
            assert_eq!(self.submissions.lock().unwrap().len(), 1);
            Err("recorder unavailable".to_string())
        }

        async fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn live_signal_recorder_failure_stops_after_order_state_is_recorded() {
        let now = Utc::now();
        let submissions = Arc::new(Mutex::new(Vec::new()));
        let strategy = RecordingStrategy {
            emitted: false,
            signal: SignalRecord {
                strategy: "test".into(),
                event_id: Some("event-1".into()),
                token_id: Some("token-1".into()),
                intent_id: Some("intent-1".into()),
                symbol: "BTCUSDT".into(),
                direction: "UP".into(),
                p_hat: 0.7,
                edge: 0.1,
                entry_price: dec!(0.40),
                decision: "enter".into(),
                ts: now,
            },
            intent: TradingIntent {
                intent_id: "intent-1".into(),
                deployment_id: "example.live".into(),
                market_id: "event-1".into(),
                token_id: "token-1".into(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.40)),
                purpose: IntentPurpose::Entry,
                created_at: now,
            },
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let executor = AcknowledgingExecutor {
            submissions: submissions.clone(),
            ..AcknowledgingExecutor::default()
        };
        let mut runtime = StrategyRuntime::new(
            strategy,
            feed,
            executor,
            Box::new(FailingRecorder {
                submissions: submissions.clone(),
            }),
            RuntimeConfig {
                mode: RuntimeMode::Live,
                throttle_hz: None,
                max_updates: None,
                skip_settlement_exits: false,
            },
        );

        let result = runtime.run().await;

        assert_eq!(submissions.lock().unwrap().len(), 1);
        assert_eq!(result.intents_submitted, 1);
        let snapshot = runtime.trading.snapshot(&Default::default());
        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(
            snapshot.orders[0].venue_order_id.as_deref(),
            Some("venue-order-1")
        );
    }

    #[async_trait]
    impl Recorder for CollectingRecorder {
        async fn record_signal(&mut self, signal: &SignalRecord) -> Result<(), String> {
            self.signals.lock().unwrap().push(signal.clone());
            Ok(())
        }

        async fn record_order(
            &mut self,
            strategy: &str,
            intent: &TradingIntent,
            _signal: Option<&SignalRecord>,
            _report: &ExecutionReport,
            _order_id: &str,
        ) -> Result<(), String> {
            self.orders.lock().unwrap().push((
                strategy.to_string(),
                intent.intent_id.clone(),
                intent.deployment_id.clone(),
            ));
            Ok(())
        }

        async fn record_fill(
            &mut self,
            _strategy: &str,
            _intent: &TradingIntent,
            _signal: Option<&SignalRecord>,
            fill: &FillRecord,
            _report: &ExecutionReport,
        ) -> Result<(), String> {
            self.fills.lock().unwrap().push(fill.fill_id.clone());
            Ok(())
        }

        async fn flush(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn records_entry_signal_even_when_execution_is_rejected() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_test".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_test".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder_store = Arc::new(Mutex::new(Vec::new()));
        let order_store = Arc::new(Mutex::new(Vec::new()));
        let fill_store = Arc::new(Mutex::new(Vec::new()));
        let recorder = Box::new(CollectingRecorder {
            signals: recorder_store.clone(),
            orders: order_store.clone(),
            fills: fill_store.clone(),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let executor = RejectingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: false,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.dryrun");
        let result = runtime.run().await;

        assert_eq!(result.intents_submitted, 0);
        let signals = recorder_store.lock().unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(
            signals[0].intent_id.as_deref(),
            Some("pm5d_BTCUSDT_UP_test")
        );
        assert_eq!(signals[0].decision, "enter");
        assert_eq!(signals[0].entry_price, Decimal::new(30, 2));
        let orders = order_store.lock().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, "recording_strategy");
        assert_eq!(orders[0].1, "pm5d_BTCUSDT_UP_test");
        assert!(fill_store.lock().unwrap().is_empty());
    }

    struct FillingExecutor;

    #[async_trait]
    impl Executor for FillingExecutor {
        async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            ExecutionReport {
                order_id: order_id.to_string(),
                fill: Some(FillRecord {
                    fill_id: "fill-1".into(),
                    order_id: order_id.to_string(),
                    token_id: intent.token_id.clone(),
                    side: intent.side,
                    quantity: intent.quantity,
                    price: intent.limit_price.expect("limit price"),
                    fee: dec!(0.01),
                    timestamp: intent.created_at,
                }),
                rejected: false,
                rejection_reason: None,
                slippage: Some(Decimal::ZERO),
                market_impact: Some(Decimal::ZERO),
                price_basis: Some("full_depth_sweep"),
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    struct CapturingExecutor {
        deployment_ids: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Executor for CapturingExecutor {
        async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            self.deployment_ids
                .lock()
                .unwrap()
                .push(intent.deployment_id.clone());
            ExecutionReport {
                order_id: order_id.to_string(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct AcknowledgingExecutor {
        policy: ExecutionPolicy,
        submissions: Arc<Mutex<Vec<String>>>,
        fill_on_retry: bool,
    }

    impl Default for AcknowledgingExecutor {
        fn default() -> Self {
            Self {
                policy: ExecutionPolicy::default(),
                submissions: Arc::new(Mutex::new(Vec::new())),
                fill_on_retry: false,
            }
        }
    }

    #[async_trait]
    impl Executor for AcknowledgingExecutor {
        fn execution_policy(&self) -> ExecutionPolicy {
            self.policy
        }

        async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            self.submissions
                .lock()
                .unwrap()
                .push(intent.intent_id.clone());
            if self.fill_on_retry && intent.intent_id.ends_with("_retry2") {
                return ExecutionReport {
                    order_id: order_id.to_string(),
                    fill: Some(FillRecord {
                        fill_id: format!("fill-{order_id}"),
                        order_id: order_id.to_string(),
                        token_id: intent.token_id.clone(),
                        side: intent.side,
                        quantity: intent.quantity,
                        price: intent.limit_price.expect("retry limit price"),
                        fee: Decimal::ZERO,
                        timestamp: intent.created_at,
                    }),
                    rejected: false,
                    rejection_reason: None,
                    slippage: Some(Decimal::ZERO),
                    market_impact: Some(Decimal::ZERO),
                    price_basis: Some("full_depth_sweep"),
                };
            }
            ExecutionReport {
                order_id: "venue-order-1".into(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct PreparingExecutor {
        prepared_quantity: Decimal,
        submissions: Arc<Mutex<Vec<Decimal>>>,
    }

    #[async_trait]
    impl Executor for PreparingExecutor {
        fn prepare_intent(&self, intent: &TradingIntent) -> TradingIntent {
            let mut prepared = intent.clone();
            prepared.quantity = self.prepared_quantity;
            prepared
        }

        async fn submit(&mut self, intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
            self.submissions.lock().unwrap().push(intent.quantity);
            ExecutionReport {
                order_id: "venue-order-prepared".into(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }
    }

    struct SkippedReconcileExecutor;
    struct IdleReconcileCountingExecutor {
        reconciles: Arc<Mutex<usize>>,
    }
    struct FailingReconcileExecutor {
        submissions: usize,
    }

    #[async_trait]
    impl Executor for IdleReconcileCountingExecutor {
        async fn submit(&mut self, _intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
            unreachable!("noop strategy must not submit")
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            false
        }

        async fn reconcile_fills(
            &mut self,
            _orders: &OrderLedger,
        ) -> Result<Vec<FillRecord>, String> {
            *self.reconciles.lock().unwrap() += 1;
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn idle_market_updates_do_not_reconcile_without_active_orders() {
        let reconciles = Arc::new(Mutex::new(0));
        let now = Utc::now();
        let mut runtime = StrategyRuntime::new(
            NoopStrategy,
            SingleUpdateFeed {
                next: Some(MarketUpdate::Quote {
                    token_id: "token-up".into(),
                    bid: Some(dec!(0.40)),
                    ask: Some(dec!(0.41)),
                    bid_size: Some(dec!(10)),
                    ask_size: Some(dec!(10)),
                    bid_levels: Vec::new(),
                    ask_levels: Vec::new(),
                    ts: now,
                }),
            },
            IdleReconcileCountingExecutor {
                reconciles: reconciles.clone(),
            },
            Box::new(NullRecorder),
            RuntimeConfig {
                mode: RuntimeMode::Live,
                throttle_hz: None,
                max_updates: None,
                skip_settlement_exits: false,
            },
        );

        runtime.run().await;

        assert_eq!(*reconciles.lock().unwrap(), 0);
    }

    #[async_trait]
    impl Executor for FailingReconcileExecutor {
        async fn submit(&mut self, _intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
            self.submissions += 1;
            assert_eq!(self.submissions, 1, "submitted after reconcile failure");
            ExecutionReport {
                order_id: "venue-1".to_string(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            false
        }

        async fn reconcile_fills(
            &mut self,
            _orders: &OrderLedger,
        ) -> Result<Vec<FillRecord>, String> {
            Err("control plane unavailable".to_string())
        }
    }

    #[tokio::test]
    #[should_panic(expected = "live fill reconciliation failed")]
    async fn live_reconcile_error_stops_runtime_immediately() {
        let now = Utc::now();
        let strategy = RecordingStrategy {
            emitted: false,
            signal: SignalRecord {
                strategy: "test".into(),
                event_id: Some("event-1".into()),
                token_id: Some("token-1".into()),
                intent_id: Some("intent-1".into()),
                symbol: "BTCUSDT".into(),
                direction: "UP".into(),
                p_hat: 0.7,
                edge: 0.1,
                entry_price: dec!(0.4),
                decision: "enter".into(),
                ts: now,
            },
            intent: TradingIntent {
                intent_id: "intent-1".into(),
                deployment_id: "example.live".into(),
                market_id: "event-1".into(),
                token_id: "token-1".into(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                limit_price: Some(dec!(0.4)),
                purpose: IntentPurpose::Entry,
                created_at: now,
            },
        };
        let mut runtime = StrategyRuntime::new(
            strategy,
            MultiUpdateFeed {
                updates: VecDeque::from(vec![
                    MarketUpdate::SpotPrice {
                        symbol: "BTCUSDT".into(),
                        price: dec!(1),
                        ts: now,
                    },
                    MarketUpdate::SpotPrice {
                        symbol: "BTCUSDT".into(),
                        price: dec!(2),
                        ts: now,
                    },
                ]),
            },
            FailingReconcileExecutor { submissions: 0 },
            Box::new(NullRecorder),
            RuntimeConfig {
                mode: RuntimeMode::Live,
                throttle_hz: None,
                max_updates: None,
                skip_settlement_exits: false,
            },
        );
        runtime.run().await;
    }

    struct CanonicalFillExecutor {
        order_id: Option<String>,
        emitted: bool,
    }

    #[async_trait]
    impl Executor for CanonicalFillExecutor {
        fn owns_live_retries(&self) -> bool {
            false
        }

        async fn submit(&mut self, _intent: &TradingIntent, order_id: &str) -> ExecutionReport {
            self.order_id = Some(order_id.to_string());
            ExecutionReport {
                order_id: "venue-1".to_string(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            false
        }

        async fn reconcile_fills(
            &mut self,
            _orders: &OrderLedger,
        ) -> Result<Vec<FillRecord>, String> {
            if self.emitted {
                return Ok(Vec::new());
            }
            self.emitted = true;
            Ok(vec![FillRecord {
                fill_id: "canonical-fill-1".to_string(),
                order_id: self.order_id.clone().expect("submitted order"),
                token_id: "token-up".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                price: dec!(0.40),
                fee: Decimal::ZERO,
                timestamp: Utc::now(),
            }])
        }
    }

    #[tokio::test]
    async fn canonical_reconciled_fill_updates_strategy_position() {
        let now = Utc::now();
        let intent = TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "example.live".to_string(),
            market_id: "event-1".to_string(),
            token_id: "token-up".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let strategy = RecordingStrategy {
            emitted: false,
            signal: SignalRecord {
                strategy: "test".to_string(),
                event_id: Some("event-1".to_string()),
                token_id: Some("token-up".to_string()),
                intent_id: Some("intent-1".to_string()),
                symbol: "BTCUSDT".to_string(),
                direction: "UP".to_string(),
                p_hat: 0.7,
                edge: 0.1,
                entry_price: dec!(0.40),
                decision: "enter".to_string(),
                ts: now,
            },
            intent,
        };
        let mut runtime = StrategyRuntime::new(
            strategy,
            SingleUpdateFeed {
                next: Some(MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price: dec!(100000),
                    ts: now,
                }),
            },
            CanonicalFillExecutor {
                order_id: None,
                emitted: false,
            },
            Box::new(NullRecorder),
            RuntimeConfig {
                mode: RuntimeMode::Live,
                throttle_hz: None,
                max_updates: Some(1),
                skip_settlement_exits: false,
            },
        );

        let result = runtime.run().await;
        assert_eq!(result.fills_recorded, 1);
        assert_eq!(result.non_settlement_fills_observed, 1);
        assert_eq!(result.full_depth_fills_observed, 0);
        assert_eq!(runtime.trading().positions().net_qty("token-up"), dec!(2));
    }

    #[async_trait]
    impl Executor for SkippedReconcileExecutor {
        fn execution_policy(&self) -> ExecutionPolicy {
            ExecutionPolicy {
                max_slippage_bps: Decimal::ZERO,
                max_attempts: 1,
                reconcile_cycles_before_retry: 1,
            }
        }

        fn last_reconcile_attempted(&self) -> bool {
            false
        }

        async fn submit(&mut self, _intent: &TradingIntent, _order_id: &str) -> ExecutionReport {
            ExecutionReport {
                order_id: "venue-order-1".into(),
                fill: None,
                rejected: false,
                rejection_reason: None,
                slippage: None,
                market_impact: None,
                price_basis: None,
            }
        }

        async fn cancel(&mut self, _order_id: &str) -> bool {
            true
        }

        async fn reconcile_fills(
            &mut self,
            _orders: &OrderLedger,
        ) -> Result<Vec<FillRecord>, String> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn records_order_and_fill_audit_for_successful_execution() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_fill".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_fill".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder_store = Arc::new(Mutex::new(Vec::new()));
        let order_store = Arc::new(Mutex::new(Vec::new()));
        let fill_store = Arc::new(Mutex::new(Vec::new()));
        let recorder = Box::new(CollectingRecorder {
            signals: recorder_store.clone(),
            orders: order_store.clone(),
            fills: fill_store.clone(),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let executor = FillingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: false,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.dryrun");
        let result = runtime.run().await;

        assert_eq!(result.intents_submitted, 1);
        assert_eq!(result.fills_recorded, 1);
        assert_eq!(result.non_settlement_fills_observed, 1);
        assert_eq!(result.full_depth_fills_observed, 1);
        assert_eq!(order_store.lock().unwrap().len(), 1);
        assert_eq!(fill_store.lock().unwrap().as_slice(), ["fill-1"]);
    }

    #[tokio::test]
    async fn fills_empty_intent_deployment_id_before_submit_and_record() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm5d.threelayer".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("intent-attribution".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "intent-attribution".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let order_store = Arc::new(Mutex::new(Vec::new()));
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: order_store.clone(),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let submitted_deployments = Arc::new(Mutex::new(Vec::new()));
        let executor = CapturingExecutor {
            deployment_ids: submitted_deployments.clone(),
        };
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: false,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("pm5d.threelayer.obi-soft.dryrun");
        let result = runtime.run().await;

        assert_eq!(result.intents_submitted, 1);
        assert_eq!(
            submitted_deployments.lock().unwrap().as_slice(),
            ["pm5d.threelayer.obi-soft.dryrun"]
        );
        assert_eq!(
            order_store.lock().unwrap()[0].2,
            "pm5d.threelayer.obi-soft.dryrun"
        );
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());
        assert_eq!(
            snapshot.intents[0].deployment_id,
            "pm5d.threelayer.obi-soft.dryrun"
        );
    }

    #[tokio::test]
    async fn runtime_records_prepared_intent_quantity() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_prepared".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_prepared".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let submissions = Arc::new(Mutex::new(Vec::new()));
        let executor = PreparingExecutor {
            prepared_quantity: dec!(7.50),
            submissions: submissions.clone(),
        };
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: Some(1),
            skip_settlement_exits: false,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.live");
        let result = runtime.run().await;

        assert_eq!(result.intents_submitted, 1);
        assert_eq!(submissions.lock().unwrap().as_slice(), [dec!(7.50)]);
        let orders = runtime.trading().orders().orders().collect::<Vec<_>>();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].requested_qty, dec!(7.50));
    }

    #[tokio::test]
    async fn live_ack_without_fill_is_not_treated_as_filled() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_ack".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_ack".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let executor = AcknowledgingExecutor::default();
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.live");
        let _ = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(snapshot.orders[0].state, ploy_trading::OrderState::Canceled);
        assert_eq!(
            snapshot.orders[0].venue_order_id.as_deref(),
            Some("venue-order-1")
        );
        assert_eq!(snapshot.fills.len(), 0);
        assert_eq!(snapshot.risk.active_orders, 0);
    }

    #[tokio::test]
    async fn live_ack_without_completed_reconcile_stays_pending() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_wait".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_wait".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime =
            StrategyRuntime::new(strategy, feed, SkippedReconcileExecutor, recorder, config)
                .with_deployment_id("test.live");
        let _ = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(
            snapshot.orders[0].state,
            ploy_trading::OrderState::Acknowledged
        );
        assert_eq!(snapshot.risk.active_orders, 1);
    }

    #[tokio::test]
    async fn live_unfilled_ack_retries_remaining_quantity_within_attempt_limit() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_retry".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_retry".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let submissions = Arc::new(Mutex::new(Vec::new()));
        let executor = AcknowledgingExecutor {
            policy: ExecutionPolicy {
                max_slippage_bps: Decimal::ZERO,
                max_attempts: 2,
                reconcile_cycles_before_retry: 1,
            },
            submissions: submissions.clone(),
            fill_on_retry: true,
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.live");
        let result = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(
            submissions.lock().unwrap().as_slice(),
            ["pm5d_BTCUSDT_UP_retry", "pm5d_BTCUSDT_UP_retry_retry2"]
        );
        assert_eq!(snapshot.orders.len(), 2);
        assert_eq!(
            snapshot
                .orders
                .iter()
                .filter(|order| order.state == ploy_trading::OrderState::Canceled)
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .orders
                .iter()
                .filter(|order| order.state == ploy_trading::OrderState::Filled)
                .count(),
            1
        );
        assert_eq!(snapshot.fills.len(), 1);
        assert_eq!(result.non_settlement_fills_observed, 1);
        assert_eq!(result.full_depth_fills_observed, 1);
    }

    #[tokio::test]
    async fn live_unfilled_ack_does_not_retry_dust_remainder() {
        let now = Utc::now();
        let signal = SignalRecord {
            strategy: "pm_5m_directional".into(),
            event_id: Some("evt1".into()),
            token_id: Some("token-up".into()),
            intent_id: Some("pm5d_BTCUSDT_UP_dust".into()),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.71,
            edge: 0.08,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: now,
        };
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_dust".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Buy,
            quantity: dec!(0.02),
            limit_price: Some(dec!(0.30)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let submissions = Arc::new(Mutex::new(Vec::new()));
        let executor = AcknowledgingExecutor {
            policy: ExecutionPolicy {
                max_slippage_bps: Decimal::ZERO,
                max_attempts: 2,
                reconcile_cycles_before_retry: 1,
            },
            submissions: submissions.clone(),
            fill_on_retry: false,
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let strategy = RecordingStrategy {
            emitted: false,
            signal,
            intent,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.live");
        let _ = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(
            submissions.lock().unwrap().as_slice(),
            ["pm5d_BTCUSDT_UP_dust"]
        );
        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(snapshot.orders[0].state, ploy_trading::OrderState::Canceled);
        assert_eq!(snapshot.fills.len(), 0);
        assert_eq!(snapshot.risk.active_orders, 0);
    }

    #[tokio::test]
    async fn restored_live_ack_order_is_managed_as_pending() {
        let now = Utc::now();
        let mut trading = ploy_trading::TradingRuntime::default();
        trading
            .submit_intent(
                TradingIntent {
                    intent_id: "restored-intent".into(),
                    deployment_id: "example.live".into(),
                    market_id: "evt1".into(),
                    token_id: "token-up".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(10),
                    limit_price: Some(dec!(0.30)),
                    purpose: IntentPurpose::Entry,
                    created_at: now,
                },
                "restored-order",
                None,
            )
            .expect("valid restored intent");
        trading.acknowledge_order("restored-order", "venue-restored");

        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new_with_trading(
            NoopStrategy,
            feed,
            AcknowledgingExecutor::default(),
            recorder,
            config,
            trading,
        );
        let _ = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(snapshot.orders.len(), 1);
        assert_eq!(snapshot.orders[0].state, ploy_trading::OrderState::Canceled);
        assert_eq!(snapshot.fills.len(), 0);
    }

    #[tokio::test]
    async fn restored_live_retry_order_continues_from_existing_attempt_suffix() {
        let now = Utc::now();
        let mut trading = ploy_trading::TradingRuntime::default();
        trading
            .submit_intent(
                TradingIntent {
                    intent_id: "pm5d_BTCUSDT_UP_retry2".into(),
                    deployment_id: "example.live".into(),
                    market_id: "evt1".into(),
                    token_id: "token-up".into(),
                    side: TradeSide::Buy,
                    quantity: dec!(10),
                    limit_price: Some(dec!(0.30)),
                    purpose: IntentPurpose::Entry,
                    created_at: now,
                },
                "restored-order",
                None,
            )
            .expect("valid restored intent");
        trading.acknowledge_order("restored-order", "venue-restored");

        let submissions = Arc::new(Mutex::new(Vec::new()));
        let executor = AcknowledgingExecutor {
            policy: ExecutionPolicy {
                max_slippage_bps: Decimal::ZERO,
                max_attempts: 3,
                reconcile_cycles_before_retry: 1,
            },
            submissions: submissions.clone(),
            fill_on_retry: false,
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new_with_trading(
            NoopStrategy,
            feed,
            executor,
            recorder,
            config,
            trading,
        );
        let _ = runtime.run().await;
        let snapshot = runtime.trading().snapshot(&BTreeMap::new());

        assert_eq!(
            submissions.lock().unwrap().as_slice(),
            ["pm5d_BTCUSDT_UP_retry3"]
        );
        assert_eq!(snapshot.orders.len(), 2);
        assert!(snapshot
            .orders
            .iter()
            .any(|order| order.intent_id == "pm5d_BTCUSDT_UP_retry3"
                && order.state == ploy_trading::OrderState::Acknowledged));
    }

    #[tokio::test]
    async fn live_exit_ack_without_fill_does_not_trigger_synthetic_fill() {
        let now = Utc::now();
        let fill_count = Arc::new(Mutex::new(0));
        let intent = TradingIntent {
            intent_id: "pm5d_BTCUSDT_UP_exit_ack".into(),
            deployment_id: String::new(),
            market_id: "evt1".into(),
            token_id: "token-up".into(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            limit_price: Some(dec!(0.42)),
            purpose: IntentPurpose::Exit,
            created_at: now,
        };
        let strategy = FillCountingStrategy {
            emitted: false,
            intent,
            fill_count: fill_count.clone(),
        };
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            }),
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let executor = AcknowledgingExecutor::default();
        let config = RuntimeConfig {
            mode: RuntimeMode::Live,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(strategy, feed, executor, recorder, config)
            .with_deployment_id("test.live");
        let _ = runtime.run().await;

        assert_eq!(*fill_count.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn agg_trade_updates_are_accepted_by_runtime() {
        let now = Utc::now();
        let feed = SingleUpdateFeed {
            next: Some(MarketUpdate::AggTrade {
                symbol: "BTCUSDT".into(),
                agg_trade_id: 42,
                price: dec!(100000),
                quantity: dec!(0.25),
                is_buyer_maker: false,
                ts: now,
            }),
        };
        let recorder = Box::new(CollectingRecorder {
            signals: Arc::new(Mutex::new(Vec::new())),
            orders: Arc::new(Mutex::new(Vec::new())),
            fills: Arc::new(Mutex::new(Vec::new())),
        });
        let executor = RejectingExecutor;
        let config = RuntimeConfig {
            mode: RuntimeMode::DryRun,
            throttle_hz: None,
            max_updates: None,
            skip_settlement_exits: true,
        };

        let mut runtime = StrategyRuntime::new(NoopStrategy, feed, executor, recorder, config);
        let result = runtime.run().await;

        assert_eq!(result.updates_processed, 1);
        assert_eq!(result.intents_submitted, 0);
        assert_eq!(result.fills_recorded, 0);
    }
}
