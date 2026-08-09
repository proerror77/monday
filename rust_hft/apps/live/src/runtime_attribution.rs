use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::deployment_envelope::{
    ActivationArtifact, ActivationMode, ActivationRequest, RuntimeFeedbackLog,
};
use alpha_domain::{AttributionKind, AttributionMode, AttributionOutcome, RuntimeAttributionEvent};
use chrono::{DateTime, Utc};
use engine::{aggregation::MarketView, RuntimeTruthStatus};
use hft_core::{Side, Symbol, VenueId, VenueSymbol};
use ports::{AccountView, ExecutionEvent};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use snapshot::SnapshotReader;
use tokio::sync::{broadcast, oneshot};

const PORTFOLIO_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(1);
const COVERAGE_COMPLETE: f64 = 1.0;
const COVERAGE_MISSING: f64 = 0.0;

pub struct RuntimeAttributionObserver {
    receiver: broadcast::Receiver<ExecutionEvent>,
    market_reader: Arc<dyn SnapshotReader<MarketView>>,
    account_reader: Arc<dyn SnapshotReader<AccountView>>,
    runtime_truth_reader: Arc<dyn SnapshotReader<RuntimeTruthStatus>>,
    activation: ActivationRequest,
    feedback_log: RuntimeFeedbackLog,
    stale_us: u64,
}

#[derive(Debug, Clone)]
struct OrderMetadata {
    strategy_id: String,
    venue: String,
    symbol: String,
    side: Side,
    requested_price: Option<Decimal>,
    arrival_price: Option<Decimal>,
    timing: OrderTiming,
    seen_fill_ids: HashSet<String>,
}

#[derive(Debug, Clone, Default)]
struct OrderTiming {
    write_to_private_ack_us: Option<u64>,
    write_to_private_report_us: Option<u64>,
    intent_to_private_report_us: Option<u64>,
}

#[derive(Debug, Clone)]
struct StrategyTarget {
    strategy_id: String,
    symbol: Option<String>,
    risk_capital: Decimal,
}

#[derive(Debug)]
struct AttributionState {
    orders: HashMap<String, OrderMetadata>,
    targets: BTreeMap<String, StrategyTarget>,
    ledgers: BTreeMap<String, StrategyLedger>,
    valuation_started: bool,
    permanently_invalid: Option<String>,
}

#[derive(Debug)]
struct StrategyLedger {
    realized_pnl: Decimal,
    positions: BTreeMap<String, PositionLedger>,
    high_water_mark: Decimal,
    max_drawdown_pct: f64,
}

#[derive(Debug, Default)]
struct PositionLedger {
    net_quantity: Decimal,
    avg_entry_price: Decimal,
}

#[derive(Debug)]
struct StrategySnapshotMetrics {
    gross_realized_pnl: Decimal,
    gross_unrealized_pnl: Decimal,
    gross_total_pnl: Decimal,
    session_equity: Decimal,
    session_high_water_mark: Decimal,
    session_drawdown_pct: f64,
    session_max_drawdown_pct: f64,
}

impl RuntimeAttributionObserver {
    pub fn new(
        receiver: broadcast::Receiver<ExecutionEvent>,
        market_reader: Arc<dyn SnapshotReader<MarketView>>,
        account_reader: Arc<dyn SnapshotReader<AccountView>>,
        runtime_truth_reader: Arc<dyn SnapshotReader<RuntimeTruthStatus>>,
        activation: ActivationRequest,
        feedback_log: RuntimeFeedbackLog,
        stale_us: u64,
    ) -> Self {
        Self {
            receiver,
            market_reader,
            account_reader,
            runtime_truth_reader,
            activation,
            feedback_log,
            stale_us,
        }
    }

    pub async fn run(mut self, mut shutdown: oneshot::Receiver<()>) -> anyhow::Result<()> {
        let mut state = AttributionState::new(&self.activation)?;
        let mut snapshots = tokio::time::interval(PORTFOLIO_SNAPSHOT_INTERVAL);
        snapshots.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                received = self.receiver.recv() => match received {
                    Ok(event) => {
                        if let Some(event) = execution_attribution(&self.activation, &mut state, &event)? {
                            self.feedback_log.append(&event)?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let event = stream_gap_attribution(
                            &self.activation,
                            &mut state,
                            skipped,
                            Utc::now(),
                        );
                        self.feedback_log.append(&event)?;
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                },
                _ = snapshots.tick() => {
                    let market = self.market_reader.load();
                    let account = self.account_reader.load();
                    let runtime_truth = self.runtime_truth_reader.load();
                    for event in portfolio_attribution(
                        &self.activation,
                        &mut state,
                        market.as_ref(),
                        account.as_ref(),
                        runtime_truth.as_ref(),
                        Utc::now(),
                        self.stale_us,
                    )? {
                        self.feedback_log.append(&event)?;
                    }
                }
                _ = &mut shutdown => {
                    self.drain_pending(&mut state)?;
                    let market = self.market_reader.load();
                    let account = self.account_reader.load();
                    let runtime_truth = self.runtime_truth_reader.load();
                    for event in portfolio_attribution(
                        &self.activation,
                        &mut state,
                        market.as_ref(),
                        account.as_ref(),
                        runtime_truth.as_ref(),
                        Utc::now(),
                        self.stale_us,
                    )? {
                        self.feedback_log.append(&event)?;
                    }
                    return Ok(());
                }
            }
        }
    }

    fn drain_pending(&mut self, state: &mut AttributionState) -> anyhow::Result<()> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    if let Some(event) = execution_attribution(&self.activation, state, &event)? {
                        self.feedback_log.append(&event)?;
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    let event =
                        stream_gap_attribution(&self.activation, state, skipped, Utc::now());
                    self.feedback_log.append(&event)?;
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => return Ok(()),
            }
        }
    }
}

impl AttributionState {
    fn new(activation: &ActivationRequest) -> anyhow::Result<Self> {
        let targets = strategy_targets(activation)?;
        let ledgers = targets
            .iter()
            .map(|(strategy_id, target)| {
                (
                    strategy_id.clone(),
                    StrategyLedger {
                        realized_pnl: Decimal::ZERO,
                        positions: BTreeMap::new(),
                        high_water_mark: target.risk_capital,
                        max_drawdown_pct: 0.0,
                    },
                )
            })
            .collect();
        Ok(Self {
            orders: HashMap::new(),
            targets,
            ledgers,
            valuation_started: false,
            permanently_invalid: None,
        })
    }

    fn mark_invalid(&mut self, reason: String) {
        self.orders.clear();
        self.permanently_invalid = Some(reason);
    }

    fn single_target(&self) -> Option<&StrategyTarget> {
        let mut targets = self.targets.values();
        let target = targets.next()?;
        if targets.next().is_none() {
            Some(target)
        } else {
            None
        }
    }
}

impl StrategyLedger {
    fn apply_fill(&mut self, symbol: &str, side: Side, price: Decimal, quantity: Decimal) {
        let signed_quantity = signed_quantity(side, quantity);
        let position = self.positions.entry(symbol.to_string()).or_default();
        position.apply_fill(signed_quantity, price, &mut self.realized_pnl);
        if position.net_quantity.is_zero() {
            position.avg_entry_price = Decimal::ZERO;
        }
    }

    fn snapshot(
        &mut self,
        target: &StrategyTarget,
        market: &MarketView,
        venue: VenueId,
    ) -> anyhow::Result<StrategySnapshotMetrics> {
        let mut gross_unrealized_pnl = Decimal::ZERO;
        for (symbol, position) in &self.positions {
            if position.net_quantity.is_zero() {
                continue;
            }
            let key = VenueSymbol::new(venue, Symbol::new(symbol.as_str()));
            let Some(mid_price) = market.get_mid_price_for_venue(&key) else {
                anyhow::bail!("missing mid price for {symbol}");
            };
            gross_unrealized_pnl +=
                (mid_price.0 - position.avg_entry_price) * position.net_quantity;
        }

        let gross_realized_pnl = self.realized_pnl;
        let gross_total_pnl = gross_realized_pnl + gross_unrealized_pnl;
        let session_equity = target.risk_capital + gross_total_pnl;
        if session_equity > self.high_water_mark {
            self.high_water_mark = session_equity;
        }
        let drawdown = if session_equity < self.high_water_mark {
            self.high_water_mark - session_equity
        } else {
            Decimal::ZERO
        };
        let session_drawdown_pct = drawdown_pct(self.high_water_mark, drawdown)?;
        self.max_drawdown_pct = self.max_drawdown_pct.max(session_drawdown_pct);

        Ok(StrategySnapshotMetrics {
            gross_realized_pnl,
            gross_unrealized_pnl,
            gross_total_pnl,
            session_equity,
            session_high_water_mark: self.high_water_mark,
            session_drawdown_pct,
            session_max_drawdown_pct: self.max_drawdown_pct,
        })
    }
}

impl PositionLedger {
    fn apply_fill(
        &mut self,
        signed_fill_quantity: Decimal,
        fill_price: Decimal,
        realized_pnl: &mut Decimal,
    ) {
        if self.net_quantity.is_zero() || same_direction(self.net_quantity, signed_fill_quantity) {
            let existing_abs = self.net_quantity.abs();
            let incoming_abs = signed_fill_quantity.abs();
            let new_quantity = self.net_quantity + signed_fill_quantity;
            let total_abs = new_quantity.abs();
            self.net_quantity = new_quantity;
            self.avg_entry_price = if total_abs.is_zero() {
                Decimal::ZERO
            } else {
                ((self.avg_entry_price * existing_abs) + (fill_price * incoming_abs)) / total_abs
            };
            return;
        }

        let close_quantity = decimal_min(self.net_quantity.abs(), signed_fill_quantity.abs());
        *realized_pnl +=
            (fill_price - self.avg_entry_price) * close_quantity * quantity_sign(self.net_quantity);

        let new_quantity = self.net_quantity + signed_fill_quantity;
        if new_quantity.is_zero() {
            self.net_quantity = Decimal::ZERO;
            self.avg_entry_price = Decimal::ZERO;
            return;
        }

        if same_direction(self.net_quantity, new_quantity) {
            self.net_quantity = new_quantity;
            return;
        }

        self.net_quantity = new_quantity;
        self.avg_entry_price = fill_price;
    }
}

fn execution_attribution(
    activation: &ActivationRequest,
    state: &mut AttributionState,
    event: &ExecutionEvent,
) -> anyhow::Result<Option<RuntimeAttributionEvent>> {
    match event {
        ExecutionEvent::OrderNew {
            order_id,
            symbol,
            side,
            venue,
            strategy_id,
            requested_price,
            arrival_price,
            ..
        } => {
            state.orders.remove(&order_id.0);
            let Some(venue) = venue.as_ref().map(ToString::to_string) else {
                return Ok(None);
            };
            let Some(expected_strategy_id) = expected_strategy_id(activation, symbol.as_str())
            else {
                return Ok(None);
            };
            if strategy_id != &expected_strategy_id
                || !venue.eq_ignore_ascii_case(&activation.venue)
            {
                return Ok(None);
            }
            state.orders.insert(
                order_id.0.clone(),
                OrderMetadata {
                    strategy_id: strategy_id.clone(),
                    venue,
                    symbol: symbol.to_string(),
                    side: *side,
                    requested_price: requested_price.map(|price| price.0),
                    arrival_price: arrival_price.map(|price| price.0),
                    timing: OrderTiming::default(),
                    seen_fill_ids: HashSet::new(),
                },
            );
            Ok(None)
        }
        ExecutionEvent::OrderLifecycleTiming {
            order_id,
            write_to_private_ack_us,
            write_to_private_report_us,
            intent_to_private_report_us,
            ..
        } => {
            let Some(metadata) = state.orders.get_mut(&order_id.0) else {
                return Ok(None);
            };
            metadata.timing = OrderTiming {
                write_to_private_ack_us: *write_to_private_ack_us,
                write_to_private_report_us: *write_to_private_report_us,
                intent_to_private_report_us: *intent_to_private_report_us,
            };
            Ok(None)
        }
        ExecutionEvent::Fill {
            order_id,
            price,
            quantity,
            timestamp,
            fill_id,
        } => {
            let Some(metadata) = state.orders.get_mut(&order_id.0) else {
                return Ok(None);
            };
            if price.0 <= Decimal::ZERO || quantity.0 <= Decimal::ZERO {
                anyhow::bail!("runtime attribution fill price and quantity must be positive");
            }
            let source_id = if fill_id.trim().is_empty() {
                format!("{}:{timestamp}", order_id.0)
            } else {
                fill_id.clone()
            };
            if !metadata.seen_fill_ids.insert(source_id.clone()) {
                return Ok(None);
            }
            let metadata = metadata.clone();
            if state.permanently_invalid.is_none() {
                if let Some(ledger) = state.ledgers.get_mut(&metadata.strategy_id) {
                    ledger.apply_fill(&metadata.symbol, metadata.side, price.0, quantity.0);
                }
            }
            let mut metrics = BTreeMap::new();
            metrics.insert(
                "fill_price".to_string(),
                finite_metric("fill_price", price.to_f64())?,
            );
            metrics.insert(
                "fill_quantity".to_string(),
                finite_metric("fill_quantity", quantity.to_f64())?,
            );
            if let Some(value) = metadata.timing.write_to_private_ack_us {
                metrics.insert("write_to_private_ack_us".to_string(), value as f64);
            }
            if let Some(value) = metadata.timing.write_to_private_report_us {
                metrics.insert("write_to_private_report_us".to_string(), value as f64);
            }
            if let Some(value) = metadata.timing.intent_to_private_report_us {
                metrics.insert("intent_to_private_report_us".to_string(), value as f64);
            }
            if let Some(requested_price) = metadata.requested_price {
                let requested = finite_metric("requested_price", requested_price.to_f64())?;
                let fill = finite_metric("fill_price", price.to_f64())?;
                if requested > 0.0 && fill > 0.0 {
                    let signed = match metadata.side {
                        Side::Buy => fill / requested - 1.0,
                        Side::Sell => 1.0 - fill / requested,
                    };
                    metrics.insert(
                        "realized_slippage_bps".to_string(),
                        (signed * 10_000.0).max(0.0),
                    );
                }
            }
            if let Some(arrival_price) = metadata.arrival_price {
                let arrival = finite_metric("arrival_price", arrival_price.to_f64())?;
                let fill = finite_metric("fill_price", price.to_f64())?;
                if arrival > 0.0 && fill > 0.0 {
                    let signed = match metadata.side {
                        Side::Buy => fill / arrival - 1.0,
                        Side::Sell => 1.0 - fill / arrival,
                    };
                    metrics.insert(
                        "arrival_slippage_bps".to_string(),
                        (signed * 10_000.0).max(0.0),
                    );
                }
            }
            metrics.insert(
                "evidence_available_at_us".to_string(),
                Utc::now().timestamp_micros().max(0) as f64,
            );
            let mut event = order_attribution(
                activation,
                &metadata,
                order_id.0.clone(),
                format!(
                    "fill:{}:{}:{source_id}",
                    activation.deployment_id, order_id.0
                ),
                AttributionKind::Fill,
                AttributionOutcome::Healthy,
                execution_time(*timestamp),
            );
            event.metrics = metrics;
            Ok(Some(event))
        }
        ExecutionEvent::OrderReject {
            order_id,
            reason,
            timestamp,
        } => {
            let Some(metadata) = state
                .orders
                .remove(&order_id.0)
                .or_else(|| inferred_pre_submission_reject(activation, state, order_id.0.as_str()))
            else {
                return Ok(None);
            };
            let mut event = order_attribution(
                activation,
                &metadata,
                order_id.0.clone(),
                format!(
                    "reject:{}:{}:{timestamp}",
                    activation.deployment_id, order_id.0
                ),
                AttributionKind::Reject,
                AttributionOutcome::Failed,
                execution_time(*timestamp),
            );
            event.reason = Some(reason.clone());
            Ok(Some(event))
        }
        ExecutionEvent::OrderCanceled {
            order_id,
            timestamp,
        } => {
            let Some(metadata) = state.orders.remove(&order_id.0) else {
                return Ok(None);
            };
            Ok(Some(order_attribution(
                activation,
                &metadata,
                order_id.0.clone(),
                format!(
                    "cancel:{}:{}:{timestamp}",
                    activation.deployment_id, order_id.0
                ),
                AttributionKind::Cancel,
                AttributionOutcome::Healthy,
                execution_time(*timestamp),
            )))
        }
        ExecutionEvent::OrderCompleted { order_id, .. } => {
            state.orders.remove(&order_id.0);
            Ok(None)
        }
        _ => Ok(None),
    }
}

fn portfolio_attribution(
    activation: &ActivationRequest,
    state: &mut AttributionState,
    market: &MarketView,
    account: &AccountView,
    runtime_truth: &RuntimeTruthStatus,
    observed_at: DateTime<Utc>,
    stale_us: u64,
) -> anyhow::Result<Vec<RuntimeAttributionEvent>> {
    let venue = VenueId::from_str(&activation.venue)
        .ok_or_else(|| anyhow::anyhow!("unsupported activation venue {}", activation.venue))?;
    let invalid_reason = state.permanently_invalid.clone();
    let observed_at_us = u64::try_from(observed_at.timestamp_micros()).unwrap_or_default();
    let mut missing_marks = Vec::new();
    let mut stale_marks = Vec::new();
    for symbol in &activation.instruments {
        let key = VenueSymbol::new(venue, Symbol::new(symbol.as_str()));
        match market.get_orderbook(&key) {
            Some(orderbook) if orderbook.get_mid_price().is_none() => {
                missing_marks.push(symbol.clone());
            }
            Some(orderbook) if observed_at_us.saturating_sub(orderbook.timestamp) > stale_us => {
                stale_marks.push(symbol.clone());
            }
            Some(_) => {}
            None => missing_marks.push(symbol.clone()),
        }
    }
    if invalid_reason.is_none() && !state.valuation_started {
        if !stale_marks.is_empty() || !missing_marks.is_empty() {
            return Ok(Vec::new());
        }
        state.valuation_started = true;
    }
    let strategy_ids = state.targets.keys().cloned().collect::<Vec<_>>();
    let mut events = Vec::with_capacity(strategy_ids.len());

    for strategy_id in strategy_ids {
        let target =
            state.targets.get(&strategy_id).cloned().ok_or_else(|| {
                anyhow::anyhow!("missing runtime attribution target {strategy_id}")
            })?;
        let event = if let Some(reason) = invalid_reason.as_ref() {
            invalid_portfolio_attribution(
                activation,
                &target,
                observed_at,
                AttributionOutcome::Failed,
                reason.clone(),
                false,
            )?
        } else if !stale_marks.is_empty() {
            invalid_portfolio_attribution(
                activation,
                &target,
                observed_at,
                AttributionOutcome::Decayed,
                format!(
                    "stale mid price beyond {}us for activation instruments {}; gross/session metrics withheld",
                    stale_us,
                    stale_marks.join(",")
                ),
                false,
            )?
        } else if !missing_marks.is_empty() {
            invalid_portfolio_attribution(
                activation,
                &target,
                observed_at,
                AttributionOutcome::Decayed,
                format!(
                    "missing mid price for activation instruments {}; gross/session metrics withheld",
                    missing_marks.join(",")
                ),
                false,
            )?
        } else {
            let ledger = state.ledgers.get_mut(&strategy_id).ok_or_else(|| {
                anyhow::anyhow!("missing runtime attribution ledger {strategy_id}")
            })?;
            match ledger.snapshot(&target, market, venue) {
                Ok(snapshot) => {
                    valid_portfolio_attribution(activation, &target, snapshot, observed_at)?
                }
                Err(error) => invalid_portfolio_attribution(
                    activation,
                    &target,
                    observed_at,
                    AttributionOutcome::Decayed,
                    format!("{error}; gross/session metrics withheld"),
                    false,
                )?,
            }
        };
        let mut event = event;
        attach_authoritative_account_metrics(
            &mut event,
            &activation.account_id,
            account,
            runtime_truth,
            observed_at,
        )?;
        events.push(event);
    }

    Ok(events)
}

fn attach_authoritative_account_metrics(
    event: &mut RuntimeAttributionEvent,
    expected_account_id: &str,
    account: &AccountView,
    runtime_truth: &RuntimeTruthStatus,
    observed_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    let account_matches = runtime_truth
        .account_id
        .as_ref()
        .is_some_and(|account_id| account_id.0 == expected_account_id);
    if account_matches {
        for (name, value) in [
            ("authoritative_account_cash_balance", account.cash_balance),
            ("authoritative_account_realized_pnl", account.realized_pnl),
            (
                "authoritative_account_unrealized_pnl",
                account.unrealized_pnl,
            ),
            ("authoritative_account_total_pnl", account.total_pnl()),
            ("authoritative_account_equity", account.equity()),
        ] {
            event
                .metrics
                .insert(name.to_string(), decimal_metric(name, value)?);
        }
        event.metrics.insert(
            "authoritative_account_open_positions".to_string(),
            account.positions.len() as f64,
        );
    }
    event.metrics.insert(
        "authoritative_account_snapshot_coverage".to_string(),
        if account_matches
            && runtime_truth.reconciliation_complete
            && runtime_truth.reconciliation_healthy
        {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    event.metrics.insert(
        "venue_reconciliation_complete".to_string(),
        if runtime_truth.reconciliation_complete {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    event.metrics.insert(
        "venue_reconciliation_healthy".to_string(),
        if runtime_truth.reconciliation_healthy {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    let observed_at_us = u64::try_from(observed_at.timestamp_micros()).unwrap_or_default();
    let reconciliation_age_us = observed_at_us.saturating_sub(runtime_truth.observed_at_us);
    event.metrics.insert(
        "venue_reconciliation_age_us".to_string(),
        reconciliation_age_us as f64,
    );
    const MAX_RECONCILIATION_AGE_US: u64 = 30_000_000;
    if event.mode == AttributionMode::LiveSmall
        && event.kind == AttributionKind::PortfolioSnapshot
        && event.outcome == AttributionOutcome::Healthy
        && (!runtime_truth.reconciliation_complete
            || !runtime_truth.reconciliation_healthy
            || runtime_truth.observed_at_us == 0
            || !account_matches
            || reconciliation_age_us > MAX_RECONCILIATION_AGE_US)
    {
        event.outcome = AttributionOutcome::Decayed;
        event.reason = Some(
            "authoritative venue reconciliation is missing, unhealthy, stale, or not scoped to the activation account; portfolio evidence withheld from promotion"
                .to_string(),
        );
    }
    Ok(())
}

fn stream_gap_attribution(
    activation: &ActivationRequest,
    state: &mut AttributionState,
    skipped: u64,
    observed_at: DateTime<Utc>,
) -> RuntimeAttributionEvent {
    state.mark_invalid(format!(
        "execution event receiver lagged by {skipped} events; strategy ledger invalidated"
    ));
    let mut event = base_attribution(
        activation,
        format!(
            "stream-gap:{}:{}:{skipped}",
            activation.deployment_id,
            observed_at.timestamp_micros()
        ),
        AttributionKind::StreamGap,
        AttributionOutcome::Failed,
        observed_at,
    );
    event
        .metrics
        .insert("skipped_events".to_string(), skipped as f64);
    event.reason = state.permanently_invalid.clone();
    event
}

fn order_attribution(
    activation: &ActivationRequest,
    metadata: &OrderMetadata,
    order_id: String,
    event_id: String,
    kind: AttributionKind,
    outcome: AttributionOutcome,
    observed_at: DateTime<Utc>,
) -> RuntimeAttributionEvent {
    let mut event = base_attribution(activation, event_id, kind, outcome, observed_at);
    event.strategy_id = Some(metadata.strategy_id.clone());
    event.order_id = Some(order_id);
    event.account_id = Some(activation.account_id.clone());
    event.venue = Some(metadata.venue.clone());
    event.symbol = Some(metadata.symbol.clone());
    event
}

fn valid_portfolio_attribution(
    activation: &ActivationRequest,
    target: &StrategyTarget,
    snapshot: StrategySnapshotMetrics,
    observed_at: DateTime<Utc>,
) -> anyhow::Result<RuntimeAttributionEvent> {
    let mut event = portfolio_event(activation, target, observed_at, AttributionOutcome::Healthy);
    event.metrics = snapshot_metrics(target, snapshot)?;
    Ok(event)
}

fn invalid_portfolio_attribution(
    activation: &ActivationRequest,
    target: &StrategyTarget,
    observed_at: DateTime<Utc>,
    outcome: AttributionOutcome,
    reason: String,
    mark_coverage_complete: bool,
) -> anyhow::Result<RuntimeAttributionEvent> {
    let mut event = portfolio_event(activation, target, observed_at, outcome);
    event.metrics = coverage_metrics(
        target.risk_capital,
        false,
        false,
        false,
        mark_coverage_complete,
    )?;
    event.reason = Some(reason);
    Ok(event)
}

fn portfolio_event(
    activation: &ActivationRequest,
    target: &StrategyTarget,
    observed_at: DateTime<Utc>,
    outcome: AttributionOutcome,
) -> RuntimeAttributionEvent {
    let mut event = base_attribution(
        activation,
        format!(
            "portfolio:{}:{}:{}",
            activation.deployment_id,
            target.strategy_id,
            observed_at.timestamp_micros()
        ),
        AttributionKind::PortfolioSnapshot,
        outcome,
        observed_at,
    );
    event.strategy_id = Some(target.strategy_id.clone());
    event.account_id = Some(activation.account_id.clone());
    event.venue = Some(activation.venue.clone());
    event.symbol = target.symbol.clone();
    event
}

fn snapshot_metrics(
    target: &StrategyTarget,
    snapshot: StrategySnapshotMetrics,
) -> anyhow::Result<BTreeMap<String, f64>> {
    let mut metrics = coverage_metrics(target.risk_capital, true, true, false, true)?;
    metrics.insert(
        "gross_realized_pnl".to_string(),
        decimal_metric("gross_realized_pnl", snapshot.gross_realized_pnl)?,
    );
    metrics.insert(
        "gross_unrealized_pnl".to_string(),
        decimal_metric("gross_unrealized_pnl", snapshot.gross_unrealized_pnl)?,
    );
    metrics.insert(
        "gross_total_pnl".to_string(),
        decimal_metric("gross_total_pnl", snapshot.gross_total_pnl)?,
    );
    metrics.insert(
        "session_equity".to_string(),
        decimal_metric("session_equity", snapshot.session_equity)?,
    );
    metrics.insert(
        "session_high_water_mark".to_string(),
        decimal_metric("session_high_water_mark", snapshot.session_high_water_mark)?,
    );
    metrics.insert(
        "session_drawdown_pct".to_string(),
        finite_metric("session_drawdown_pct", Some(snapshot.session_drawdown_pct))?,
    );
    metrics.insert(
        "session_max_drawdown_pct".to_string(),
        finite_metric(
            "session_max_drawdown_pct",
            Some(snapshot.session_max_drawdown_pct),
        )?,
    );
    Ok(metrics)
}

fn coverage_metrics(
    risk_capital: Decimal,
    gross_complete: bool,
    session_complete: bool,
    fee_complete: bool,
    mark_complete: bool,
) -> anyhow::Result<BTreeMap<String, f64>> {
    let mut metrics = BTreeMap::new();
    metrics.insert(
        "activation_risk_capital".to_string(),
        decimal_metric("activation_risk_capital", risk_capital)?,
    );
    metrics.insert(
        "gross_pnl_coverage_complete".to_string(),
        if gross_complete {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    metrics.insert(
        "session_risk_coverage_complete".to_string(),
        if session_complete {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    metrics.insert(
        "fee_coverage_complete".to_string(),
        if fee_complete {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    metrics.insert(
        "mark_coverage_complete".to_string(),
        if mark_complete {
            COVERAGE_COMPLETE
        } else {
            COVERAGE_MISSING
        },
    );
    Ok(metrics)
}

fn inferred_pre_submission_reject(
    activation: &ActivationRequest,
    state: &AttributionState,
    order_id: &str,
) -> Option<OrderMetadata> {
    if ![
        "no_client_",
        "route_failed_",
        "failed_",
        "emergency_blocked_",
    ]
    .iter()
    .any(|prefix| order_id.starts_with(prefix))
    {
        return None;
    }
    let target = state.single_target()?;
    let symbol = target.symbol.clone()?;
    Some(OrderMetadata {
        strategy_id: target.strategy_id.clone(),
        venue: activation.venue.clone(),
        symbol,
        side: Side::Buy,
        requested_price: None,
        arrival_price: None,
        timing: OrderTiming::default(),
        seen_fill_ids: HashSet::new(),
    })
}

fn expected_strategy_id(activation: &ActivationRequest, symbol: &str) -> Option<String> {
    if !activation
        .instruments
        .iter()
        .any(|instrument| instrument == symbol)
    {
        return None;
    }
    Some(match activation.artifact {
        ActivationArtifact::Formula => format!("{}:{symbol}", activation.bundle_id),
        ActivationArtifact::Onnx => activation.bundle_id.clone(),
    })
}

fn strategy_targets(
    activation: &ActivationRequest,
) -> anyhow::Result<BTreeMap<String, StrategyTarget>> {
    let max_notional = positive_decimal("max_notional", activation.max_notional)?;
    let max_symbol_exposure =
        positive_decimal("max_symbol_exposure", activation.max_symbol_exposure)?;
    let mut targets = BTreeMap::new();

    match activation.artifact {
        ActivationArtifact::Formula => {
            let risk_capital = decimal_min(max_notional, max_symbol_exposure);
            for symbol in &activation.instruments {
                let strategy_id = format!("{}:{symbol}", activation.bundle_id);
                targets.insert(
                    strategy_id.clone(),
                    StrategyTarget {
                        strategy_id,
                        symbol: Some(symbol.clone()),
                        risk_capital,
                    },
                );
            }
        }
        ActivationArtifact::Onnx => {
            targets.insert(
                activation.bundle_id.clone(),
                StrategyTarget {
                    strategy_id: activation.bundle_id.clone(),
                    symbol: if activation.instruments.len() == 1 {
                        activation.instruments.first().cloned()
                    } else {
                        None
                    },
                    risk_capital: max_notional,
                },
            );
        }
    }

    Ok(targets)
}

fn base_attribution(
    activation: &ActivationRequest,
    event_id: String,
    kind: AttributionKind,
    outcome: AttributionOutcome,
    observed_at: DateTime<Utc>,
) -> RuntimeAttributionEvent {
    RuntimeAttributionEvent {
        event_id,
        deployment_id: activation.deployment_id.clone(),
        asset_revision_id: activation.asset_revision_id.clone(),
        mission_id: None,
        mode: match activation.mode {
            ActivationMode::Paper => AttributionMode::Paper,
            ActivationMode::Shadow => AttributionMode::Shadow,
            ActivationMode::LiveSmall => AttributionMode::LiveSmall,
        },
        outcome,
        kind,
        strategy_id: None,
        order_id: None,
        account_id: None,
        venue: None,
        symbol: None,
        metrics: BTreeMap::new(),
        reason: None,
        observed_at,
    }
}

fn positive_decimal(name: &str, value: f64) -> anyhow::Result<Decimal> {
    let value = Decimal::from_f64_retain(value)
        .ok_or_else(|| anyhow::anyhow!("runtime attribution {name} is not finite"))?;
    if value <= Decimal::ZERO {
        anyhow::bail!("runtime attribution {name} must be positive");
    }
    Ok(value)
}

fn drawdown_pct(high_water_mark: Decimal, drawdown: Decimal) -> anyhow::Result<f64> {
    if high_water_mark <= Decimal::ZERO {
        return Ok(0.0);
    }
    finite_metric(
        "session_drawdown_pct",
        ((drawdown / high_water_mark) * Decimal::from(100)).to_f64(),
    )
}

fn signed_quantity(side: Side, quantity: Decimal) -> Decimal {
    match side {
        Side::Buy => quantity,
        Side::Sell => -quantity,
    }
}

fn quantity_sign(quantity: Decimal) -> Decimal {
    if quantity.is_sign_negative() {
        Decimal::NEGATIVE_ONE
    } else {
        Decimal::ONE
    }
}

fn same_direction(left: Decimal, right: Decimal) -> bool {
    (left.is_sign_positive() && right.is_sign_positive())
        || (left.is_sign_negative() && right.is_sign_negative())
}

fn decimal_min(left: Decimal, right: Decimal) -> Decimal {
    if left <= right {
        left
    } else {
        right
    }
}

fn decimal_metric(name: &str, value: Decimal) -> anyhow::Result<f64> {
    finite_metric(name, value.to_f64())
}

fn finite_metric(name: &str, value: Option<f64>) -> anyhow::Result<f64> {
    value
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("runtime attribution metric {name} is not finite"))
}

fn execution_time(timestamp: u64) -> DateTime<Utc> {
    i64::try_from(timestamp)
        .ok()
        .and_then(DateTime::<Utc>::from_timestamp_micros)
        .unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deployment_envelope::ActivationArtifact;
    use engine::aggregation::TopNSnapshot;
    use hft_core::{OrderId, Price, Quantity, Side, Symbol, VenueId};
    use ports::{BookLevel, MarketSnapshot};
    use snapshot::SnapshotContainer;

    const NOW_US: u64 = 1_700_000_000_000_000;

    fn activation() -> ActivationRequest {
        formula_activation(&["BTCUSDT"])
    }

    fn formula_activation(symbols: &[&str]) -> ActivationRequest {
        ActivationRequest {
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            promotion_id: "promotion-1".to_string(),
            bundle_id: "bundle-1".to_string(),
            bundle_hash: "a".repeat(64),
            account_id: "account-1".to_string(),
            venue: "bitget".to_string(),
            instruments: symbols.iter().map(|symbol| symbol.to_string()).collect(),
            artifact: ActivationArtifact::Formula,
            mode: ActivationMode::Paper,
            max_notional: 1_000.0,
            max_symbol_exposure: 500.0,
            max_order_size: 100.0,
            max_slippage_bps: 10.0,
        }
    }

    fn onnx_activation(symbols: &[&str]) -> ActivationRequest {
        let mut activation = formula_activation(symbols);
        activation.artifact = ActivationArtifact::Onnx;
        activation
    }

    fn order_new(order_id: &str) -> ExecutionEvent {
        order_new_for(
            order_id,
            "BTCUSDT",
            Side::Buy,
            "bundle-1:BTCUSDT",
            Some(VenueId::BITGET),
        )
    }

    fn order_new_for(
        order_id: &str,
        symbol: &str,
        side: Side,
        strategy_id: &str,
        venue: Option<VenueId>,
    ) -> ExecutionEvent {
        ExecutionEvent::OrderNew {
            order_id: OrderId(order_id.to_string()),
            client_order_id: None,
            account_id: None,
            symbol: Symbol::new(symbol),
            side,
            quantity: Quantity::from_f64(1.0).unwrap(),
            requested_price: Some(Price::from_f64(100.0).unwrap()),
            arrival_price: Some(Price::from_f64(100.0).unwrap()),
            timestamp: NOW_US,
            venue,
            strategy_id: strategy_id.to_string(),
        }
    }

    fn market_with_mid(quotes: &[(&str, f64)]) -> MarketView {
        let timestamp = Utc::now().timestamp_micros().max(0) as u64;
        let timed_quotes = quotes
            .iter()
            .map(|(symbol, mid)| (*symbol, *mid, timestamp))
            .collect::<Vec<_>>();
        market_with_timed_mids(&timed_quotes)
    }

    fn market_with_timed_mids(quotes: &[(&str, f64, u64)]) -> MarketView {
        let mut market = MarketView {
            orderbooks: Default::default(),
            arbitrage_opportunities: Vec::new(),
            timestamp: quotes
                .iter()
                .map(|(_, _, timestamp)| *timestamp)
                .max()
                .unwrap_or_default(),
            version: 1,
        };
        for (symbol, mid, timestamp) in quotes {
            let snapshot = MarketSnapshot {
                symbol: Symbol::new(*symbol),
                timestamp: *timestamp,
                bids: vec![BookLevel::new(mid - 1.0, 1.0).unwrap()],
                asks: vec![BookLevel::new(mid + 1.0, 1.0).unwrap()],
                sequence: 1,
                source_venue: Some(VenueId::BITGET),
                timestamps: Default::default(),
            };
            let mut topn = TopNSnapshot::new(Symbol::new(*symbol), 1);
            topn.update_from_snapshot(&snapshot);
            market.orderbooks.insert(
                VenueSymbol::new(VenueId::BITGET, Symbol::new(*symbol)),
                Arc::new(topn),
            );
        }
        market
    }

    fn snapshot_events(
        activation: &ActivationRequest,
        state: &mut AttributionState,
        market: &MarketView,
    ) -> Vec<RuntimeAttributionEvent> {
        portfolio_attribution(
            activation,
            state,
            market,
            &AccountView::default(),
            &RuntimeTruthStatus {
                reconciliation_complete: true,
                reconciliation_healthy: true,
                observed_at_us: NOW_US,
                account_id: Some(hft_core::AccountId(activation.account_id.clone())),
            },
            execution_time(NOW_US),
            u64::MAX,
        )
        .unwrap()
    }

    fn event_for<'a>(
        events: &'a [RuntimeAttributionEvent],
        strategy_id: &str,
    ) -> &'a RuntimeAttributionEvent {
        events
            .iter()
            .find(|event| event.strategy_id.as_deref() == Some(strategy_id))
            .unwrap()
    }

    #[test]
    fn order_new_metadata_attributes_fill_reject_and_cancel() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();

        execution_attribution(&activation, &mut state, &order_new("fill-order")).unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::OrderLifecycleTiming {
                order_id: OrderId("fill-order".to_string()),
                observed_at: NOW_US,
                write_to_private_ack_us: Some(20),
                write_to_private_report_us: Some(40),
                intent_to_private_report_us: Some(75),
            },
        )
        .unwrap();
        let fill = execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("fill-order".to_string()),
                price: Price::from_f64(101.0).unwrap(),
                quantity: Quantity::from_f64(0.5).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "fill-1".to_string(),
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(fill.kind, AttributionKind::Fill);
        assert_eq!(fill.event_id, "fill:deployment-1:fill-order:fill-1");
        assert_eq!(fill.strategy_id.as_deref(), Some("bundle-1:BTCUSDT"));
        assert_eq!(fill.order_id.as_deref(), Some("fill-order"));
        assert_eq!(fill.account_id.as_deref(), Some("account-1"));
        assert_eq!(fill.symbol.as_deref(), Some("BTCUSDT"));
        assert_eq!(fill.metrics["fill_price"], 101.0);
        assert_eq!(fill.metrics["write_to_private_ack_us"], 20.0);
        assert_eq!(fill.metrics["write_to_private_report_us"], 40.0);
        assert_eq!(fill.metrics["intent_to_private_report_us"], 75.0);
        assert!((fill.metrics["realized_slippage_bps"] - 100.0).abs() < 1e-9);
        assert!((fill.metrics["arrival_slippage_bps"] - 100.0).abs() < 1e-9);
        assert!(fill.metrics["evidence_available_at_us"] > NOW_US as f64);

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "sell-order",
                "BTCUSDT",
                Side::Sell,
                "bundle-1:BTCUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        let sell_fill = execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("sell-order".to_string()),
                price: Price::from_f64(50.0).unwrap(),
                quantity: Quantity::from_f64(0.5).unwrap(),
                timestamp: NOW_US + 2,
                fill_id: "sell-fill-1".to_string(),
            },
        )
        .unwrap()
        .unwrap();
        assert!((sell_fill.metrics["realized_slippage_bps"] - 5_000.0).abs() < 1e-9);

        execution_attribution(&activation, &mut state, &order_new("reject-order")).unwrap();
        let reject = execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::OrderReject {
                order_id: OrderId("reject-order".to_string()),
                reason: "venue rejected".to_string(),
                timestamp: NOW_US + 2,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(reject.kind, AttributionKind::Reject);
        assert_eq!(
            reject.event_id,
            format!("reject:deployment-1:reject-order:{}", NOW_US + 2)
        );
        assert_eq!(reject.outcome, AttributionOutcome::Failed);
        assert_eq!(reject.reason.as_deref(), Some("venue rejected"));

        execution_attribution(&activation, &mut state, &order_new("cancel-order")).unwrap();
        let cancel = execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::OrderCanceled {
                order_id: OrderId("cancel-order".to_string()),
                timestamp: NOW_US + 3,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(cancel.kind, AttributionKind::Cancel);
        assert_eq!(
            cancel.event_id,
            format!("cancel:deployment-1:cancel-order:{}", NOW_US + 3)
        );
        assert_eq!(cancel.outcome, AttributionOutcome::Healthy);
        assert!(!state.orders.contains_key("reject-order"));
        assert!(!state.orders.contains_key("cancel-order"));
    }

    #[test]
    fn unknown_orders_are_not_attributed() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();
        let events = [
            ExecutionEvent::Fill {
                order_id: OrderId("unknown".to_string()),
                price: Price::from_f64(101.0).unwrap(),
                quantity: Quantity::from_f64(0.5).unwrap(),
                timestamp: NOW_US,
                fill_id: "unknown-fill".to_string(),
            },
            ExecutionEvent::OrderReject {
                order_id: OrderId("unknown".to_string()),
                reason: "unknown".to_string(),
                timestamp: NOW_US,
            },
            ExecutionEvent::OrderCanceled {
                order_id: OrderId("unknown".to_string()),
                timestamp: NOW_US,
            },
        ];

        for event in events {
            assert!(execution_attribution(&activation, &mut state, &event)
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn duplicate_fill_id_is_not_counted_or_emitted_twice() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();
        execution_attribution(&activation, &mut state, &order_new("fill-order")).unwrap();
        let fill = ExecutionEvent::Fill {
            order_id: OrderId("fill-order".to_string()),
            price: Price::from_f64(100.0).unwrap(),
            quantity: Quantity::from_f64(1.0).unwrap(),
            timestamp: NOW_US + 1,
            fill_id: "fill-duplicate".to_string(),
        };

        assert!(execution_attribution(&activation, &mut state, &fill)
            .unwrap()
            .is_some());
        assert!(execution_attribution(&activation, &mut state, &fill)
            .unwrap()
            .is_none());
        execution_attribution(&activation, &mut state, &order_new("second-order")).unwrap();
        assert!(execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("second-order".to_string()),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 2,
                fill_id: "fill-duplicate".to_string(),
            },
        )
        .unwrap()
        .is_some());
        assert_eq!(
            state.ledgers["bundle-1:BTCUSDT"].positions["BTCUSDT"].net_quantity,
            Decimal::from(2)
        );
    }

    #[test]
    fn single_instrument_pre_submission_reject_is_scoped_to_signed_bundle() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();
        let reject = execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::OrderReject {
                order_id: OrderId("route_failed_1".to_string()),
                reason: "no route".to_string(),
                timestamp: NOW_US,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(reject.kind, AttributionKind::Reject);
        assert_eq!(reject.strategy_id.as_deref(), Some("bundle-1:BTCUSDT"));
        assert_eq!(reject.symbol.as_deref(), Some("BTCUSDT"));
    }

    #[test]
    fn order_new_requires_the_exact_expected_strategy_id() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "wrong-strategy",
                "BTCUSDT",
                Side::Buy,
                "other-bundle:BTCUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "missing-venue",
                "BTCUSDT",
                Side::Buy,
                "bundle-1:BTCUSDT",
                None,
            ),
        )
        .unwrap();

        let fill = execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("wrong-strategy".to_string()),
                price: Price::from_f64(101.0).unwrap(),
                quantity: Quantity::from_f64(0.5).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "fill-ignored".to_string(),
            },
        )
        .unwrap();

        assert!(fill.is_none());
        assert!(state.orders.is_empty());
    }

    #[test]
    fn formula_snapshots_split_strategies_and_ignore_other_shared_account_orders() {
        let activation = formula_activation(&["BTCUSDT", "ETHUSDT"]);
        let mut state = AttributionState::new(&activation).unwrap();

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "btc-good",
                "BTCUSDT",
                Side::Buy,
                "bundle-1:BTCUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("btc-good".to_string()),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "btc-fill".to_string(),
            },
        )
        .unwrap();

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "btc-other",
                "BTCUSDT",
                Side::Buy,
                "other-bundle:BTCUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        assert!(execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("btc-other".to_string()),
                price: Price::from_f64(130.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 2,
                fill_id: "btc-other-fill".to_string(),
            },
        )
        .unwrap()
        .is_none());

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "eth-good",
                "ETHUSDT",
                Side::Buy,
                "bundle-1:ETHUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("eth-good".to_string()),
                price: Price::from_f64(50.0).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: NOW_US + 3,
                fill_id: "eth-fill".to_string(),
            },
        )
        .unwrap();

        let events = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 105.0), ("ETHUSDT", 55.0)]),
        );
        assert_eq!(events.len(), 2);

        let btc = event_for(&events, "bundle-1:BTCUSDT");
        assert_eq!(btc.symbol.as_deref(), Some("BTCUSDT"));
        assert_eq!(btc.metrics["gross_total_pnl"], 5.0);
        assert_eq!(btc.metrics["session_equity"], 505.0);
        assert_eq!(btc.metrics["fee_coverage_complete"], 0.0);

        let eth = event_for(&events, "bundle-1:ETHUSDT");
        assert_eq!(eth.symbol.as_deref(), Some("ETHUSDT"));
        assert_eq!(eth.metrics["gross_total_pnl"], 10.0);
        assert_eq!(eth.metrics["session_equity"], 510.0);
    }

    #[test]
    fn onnx_strategy_aggregates_symbols_under_bundle_id() {
        let activation = onnx_activation(&["BTCUSDT", "ETHUSDT"]);
        let mut state = AttributionState::new(&activation).unwrap();

        for (order_id, symbol, price, quantity) in [
            ("btc-order", "BTCUSDT", 100.0, 1.0),
            ("eth-order", "ETHUSDT", 50.0, 2.0),
        ] {
            execution_attribution(
                &activation,
                &mut state,
                &order_new_for(
                    order_id,
                    symbol,
                    Side::Buy,
                    "bundle-1",
                    Some(VenueId::BITGET),
                ),
            )
            .unwrap();
            execution_attribution(
                &activation,
                &mut state,
                &ExecutionEvent::Fill {
                    order_id: OrderId(order_id.to_string()),
                    price: Price::from_f64(price).unwrap(),
                    quantity: Quantity::from_f64(quantity).unwrap(),
                    timestamp: NOW_US,
                    fill_id: format!("{order_id}-fill"),
                },
            )
            .unwrap();
        }

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "wrong",
                "BTCUSDT",
                Side::Buy,
                "other-bundle",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        assert!(execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("wrong".to_string()),
                price: Price::from_f64(120.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "wrong-fill".to_string(),
            },
        )
        .unwrap()
        .is_none());

        let events = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 105.0), ("ETHUSDT", 55.0)]),
        );
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.strategy_id.as_deref(), Some("bundle-1"));
        assert_eq!(event.symbol, None);
        assert_eq!(event.metrics["gross_total_pnl"], 15.0);
        assert_eq!(event.metrics["session_equity"], 1015.0);
        assert_eq!(
            event.metrics["authoritative_account_snapshot_coverage"],
            1.0
        );
        assert_eq!(event.metrics["authoritative_account_equity"], 0.0);
    }

    #[test]
    fn session_ledger_handles_long_partial_close_and_crossing() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();

        execution_attribution(&activation, &mut state, &order_new("buy-1")).unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("buy-1".to_string()),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: NOW_US,
                fill_id: "buy-fill".to_string(),
            },
        )
        .unwrap();

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "sell-1",
                "BTCUSDT",
                Side::Sell,
                "bundle-1:BTCUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("sell-1".to_string()),
                price: Price::from_f64(110.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "sell-fill-1".to_string(),
            },
        )
        .unwrap();

        execution_attribution(
            &activation,
            &mut state,
            &order_new_for(
                "sell-2",
                "BTCUSDT",
                Side::Sell,
                "bundle-1:BTCUSDT",
                Some(VenueId::BITGET),
            ),
        )
        .unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("sell-2".to_string()),
                price: Price::from_f64(90.0).unwrap(),
                quantity: Quantity::from_f64(2.0).unwrap(),
                timestamp: NOW_US + 2,
                fill_id: "sell-fill-2".to_string(),
            },
        )
        .unwrap();

        let rising = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 80.0)]),
        );
        let event = &rising[0];
        assert_eq!(event.metrics["gross_realized_pnl"], 0.0);
        assert_eq!(event.metrics["gross_unrealized_pnl"], 10.0);
        assert_eq!(event.metrics["gross_total_pnl"], 10.0);
        assert_eq!(event.metrics["session_high_water_mark"], 510.0);
        assert_eq!(event.metrics["session_drawdown_pct"], 0.0);

        let falling = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 95.0)]),
        );
        let event = &falling[0];
        assert_eq!(event.metrics["gross_total_pnl"], -5.0);
        assert_eq!(event.metrics["session_high_water_mark"], 510.0);
        assert!(event.metrics["session_drawdown_pct"] > 2.9);
        assert_eq!(
            event.metrics["session_drawdown_pct"],
            event.metrics["session_max_drawdown_pct"]
        );
    }

    #[test]
    fn portfolio_snapshot_waits_for_initial_mark_coverage() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();

        let events = snapshot_events(&activation, &mut state, &market_with_mid(&[]));
        assert!(events.is_empty());

        let events = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 100.0)]),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, AttributionOutcome::Healthy);
    }

    #[test]
    fn paper_and_shadow_snapshots_do_not_require_live_reconciliation() {
        for mode in [ActivationMode::Paper, ActivationMode::Shadow] {
            let mut activation = activation();
            activation.mode = mode;
            let mut state = AttributionState::new(&activation).unwrap();
            let events = portfolio_attribution(
                &activation,
                &mut state,
                &market_with_mid(&[("BTCUSDT", 100.0)]),
                &AccountView::default(),
                &RuntimeTruthStatus::default(),
                execution_time(NOW_US),
                u64::MAX,
            )
            .unwrap();

            assert_eq!(events[0].outcome, AttributionOutcome::Healthy);
        }
    }

    #[test]
    fn live_small_snapshot_decays_without_authoritative_reconciliation() {
        let mut activation = activation();
        activation.mode = ActivationMode::LiveSmall;
        let mut state = AttributionState::new(&activation).unwrap();
        let events = portfolio_attribution(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 100.0)]),
            &AccountView::default(),
            &RuntimeTruthStatus::default(),
            execution_time(NOW_US),
            u64::MAX,
        )
        .unwrap();

        assert_eq!(events[0].outcome, AttributionOutcome::Decayed);
    }

    #[test]
    fn global_account_view_is_not_attributed_to_activation_account() {
        let mut activation = activation();
        activation.mode = ActivationMode::LiveSmall;
        let mut state = AttributionState::new(&activation).unwrap();
        let account = AccountView {
            cash_balance: Decimal::from(42_000),
            ..AccountView::default()
        };
        let events = portfolio_attribution(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 100.0)]),
            &account,
            &RuntimeTruthStatus {
                reconciliation_complete: true,
                reconciliation_healthy: true,
                observed_at_us: NOW_US,
                account_id: None,
            },
            execution_time(NOW_US),
            u64::MAX,
        )
        .unwrap();

        assert_eq!(events[0].outcome, AttributionOutcome::Decayed);
        assert_eq!(events[0].account_id.as_deref(), Some("account-1"));
        assert_eq!(
            events[0].metrics["authoritative_account_snapshot_coverage"],
            0.0
        );
        assert!(!events[0]
            .metrics
            .contains_key("authoritative_account_equity"));
    }

    #[test]
    fn multi_instrument_portfolio_requires_each_mark_to_be_fresh() {
        let activation = formula_activation(&["BTCUSDT", "ETHUSDT"]);
        let mut state = AttributionState::new(&activation).unwrap();
        let observed_at = Utc::now();
        let now_us = observed_at.timestamp_micros().max(0) as u64;
        let stale_us = 1_000;
        let runtime_truth = RuntimeTruthStatus {
            reconciliation_complete: true,
            reconciliation_healthy: true,
            observed_at_us: now_us,
            account_id: Some(hft_core::AccountId(activation.account_id.clone())),
        };

        let one_stale = market_with_timed_mids(&[
            ("BTCUSDT", 100.0, now_us),
            ("ETHUSDT", 50.0, now_us - stale_us - 1),
        ]);
        let events = portfolio_attribution(
            &activation,
            &mut state,
            &one_stale,
            &AccountView::default(),
            &runtime_truth,
            observed_at,
            stale_us,
        )
        .unwrap();
        assert!(events.is_empty());

        let complete =
            market_with_timed_mids(&[("BTCUSDT", 100.0, now_us), ("ETHUSDT", 50.0, now_us)]);
        let events = portfolio_attribution(
            &activation,
            &mut state,
            &complete,
            &AccountView::default(),
            &runtime_truth,
            observed_at,
            stale_us,
        )
        .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.outcome == AttributionOutcome::Healthy));

        let events = portfolio_attribution(
            &activation,
            &mut state,
            &one_stale,
            &AccountView::default(),
            &runtime_truth,
            observed_at,
            stale_us,
        )
        .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| event.outcome == AttributionOutcome::Decayed));
        assert!(events.iter().all(|event| event
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("ETHUSDT"))));
    }

    #[test]
    fn portfolio_snapshot_decays_when_mark_is_missing() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();

        let initial = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 100.0)]),
        );
        assert_eq!(initial[0].outcome, AttributionOutcome::Healthy);

        execution_attribution(&activation, &mut state, &order_new("buy-1")).unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("buy-1".to_string()),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US,
                fill_id: "buy-fill".to_string(),
            },
        )
        .unwrap();

        let events = snapshot_events(&activation, &mut state, &market_with_mid(&[]));
        let event = &events[0];
        assert_eq!(event.outcome, AttributionOutcome::Decayed);
        assert_eq!(event.strategy_id.as_deref(), Some("bundle-1:BTCUSDT"));
        assert_eq!(event.metrics["gross_pnl_coverage_complete"], 0.0);
        assert_eq!(event.metrics["session_risk_coverage_complete"], 0.0);
        assert!(!event.metrics.contains_key("gross_total_pnl"));
        assert!(event
            .reason
            .as_deref()
            .unwrap()
            .contains("missing mid price"));
    }

    #[test]
    fn stream_gap_invalidates_ledger_and_future_snapshots_stay_failed() {
        let activation = activation();
        let mut state = AttributionState::new(&activation).unwrap();

        execution_attribution(&activation, &mut state, &order_new("buy-1")).unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("buy-1".to_string()),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US,
                fill_id: "buy-fill".to_string(),
            },
        )
        .unwrap();

        let gap = stream_gap_attribution(&activation, &mut state, 7, execution_time(NOW_US));
        assert_eq!(gap.kind, AttributionKind::StreamGap);
        assert_eq!(gap.outcome, AttributionOutcome::Failed);
        assert_eq!(gap.metrics["skipped_events"], 7.0);

        execution_attribution(&activation, &mut state, &order_new("buy-2")).unwrap();
        execution_attribution(
            &activation,
            &mut state,
            &ExecutionEvent::Fill {
                order_id: OrderId("buy-2".to_string()),
                price: Price::from_f64(101.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "buy-fill-2".to_string(),
            },
        )
        .unwrap();

        let events = snapshot_events(
            &activation,
            &mut state,
            &market_with_mid(&[("BTCUSDT", 110.0)]),
        );
        let event = &events[0];
        assert_eq!(event.outcome, AttributionOutcome::Failed);
        assert_eq!(event.metrics["gross_pnl_coverage_complete"], 0.0);
        assert!(!event.metrics.contains_key("gross_total_pnl"));
        assert!(event
            .reason
            .as_deref()
            .unwrap()
            .contains("ledger invalidated"));
    }

    #[tokio::test]
    async fn shutdown_drains_execution_events_and_writes_final_portfolio() {
        let activation = activation();
        let (execution_tx, _) = broadcast::channel(8);
        let market = SnapshotContainer::new(market_with_mid(&[("BTCUSDT", 105.0)]));
        let path = std::env::temp_dir().join(format!(
            "hft-runtime-attribution-{}-{}.jsonl",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let feedback_key = ed25519_dalek::SigningKey::from_bytes(&[13_u8; 32]);
        let feedback_log =
            RuntimeFeedbackLog::open(&path, "runtime-feedback-1", feedback_key.clone()).unwrap();
        let observer = RuntimeAttributionObserver::new(
            execution_tx.subscribe(),
            market.reader(),
            snapshot::SnapshotContainer::new(AccountView::default()).reader(),
            snapshot::SnapshotContainer::new(RuntimeTruthStatus {
                reconciliation_complete: true,
                reconciliation_healthy: true,
                observed_at_us: hft_core::now_micros(),
                account_id: Some(hft_core::AccountId(activation.account_id.clone())),
            })
            .reader(),
            activation,
            feedback_log,
            u64::MAX,
        );
        execution_tx.send(order_new("buy-1")).unwrap();
        execution_tx
            .send(ExecutionEvent::Fill {
                order_id: OrderId("buy-1".to_string()),
                price: Price::from_f64(100.0).unwrap(),
                quantity: Quantity::from_f64(1.0).unwrap(),
                timestamp: NOW_US + 1,
                fill_id: "fill-1".to_string(),
            })
            .unwrap();

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handle = tokio::spawn(observer.run(shutdown_rx));
        shutdown_tx.send(()).unwrap();
        handle.await.unwrap().unwrap();

        let trusted = BTreeMap::from([(
            "runtime-feedback-1".to_string(),
            feedback_key.verifying_key(),
        )]);
        let events: Vec<RuntimeAttributionEvent> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let signed: alpha_domain::SignedRuntimeAttributionEvent =
                    serde_json::from_str(line).unwrap();
                alpha_domain::verify_runtime_attribution_event(&signed, &trusted).unwrap()
            })
            .collect();
        assert!(events
            .iter()
            .any(|event| event.kind == AttributionKind::Fill));
        let snapshot = events
            .iter()
            .rev()
            .find(|event| event.kind == AttributionKind::PortfolioSnapshot)
            .unwrap();
        assert_eq!(snapshot.outcome, AttributionOutcome::Healthy);
        assert_eq!(snapshot.strategy_id.as_deref(), Some("bundle-1:BTCUSDT"));
        assert_eq!(snapshot.metrics["gross_total_pnl"], 5.0);
        std::fs::remove_file(path).unwrap();
    }
}
