//! Core traits for the unified strategy runtime.
//!
//! These traits define the pluggable components of [`StrategyRuntime`]:
//! - [`Feed`] — market data source (historical replay or live WebSocket)
//! - [`Executor`] — order execution (simulated or real exchange)
//! - [`StrategyLogic`] — signal evaluation and decision making
//! - [`Recorder`] — signal and trade persistence

use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub use ploy_market_contracts::{Feed, MarketUpdate};
use ploy_trading::{FillRecord, OrderLedger, PositionLedger, TradingIntent};
use rust_decimal::Decimal;

// ── Execution ────────────────────────────────────────────

/// Result of submitting a trading intent to a venue or simulator.
#[derive(Debug, Clone)]
pub struct ExecutionReport {
    /// Assigned order identifier.
    pub order_id: String,
    /// Fill record if the order was (partially) filled.
    pub fill: Option<FillRecord>,
    /// Whether the order was rejected by the venue.
    pub rejected: bool,
    /// Reason for rejection, if any.
    pub rejection_reason: Option<String>,
    /// Simulated slippage (simulator only).
    pub slippage: Option<Decimal>,
    /// Simulated market impact (simulator only).
    pub market_impact: Option<Decimal>,
    /// Execution price source used by the simulator or venue adapter.
    pub price_basis: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitOutcome {
    Acknowledged,
    Rejected,
    Unknown,
}

impl ExecutionReport {
    pub fn submit_outcome(&self) -> SubmitOutcome {
        if self.rejected {
            SubmitOutcome::Rejected
        } else if self.order_id.is_empty() && self.rejection_reason.is_some() {
            SubmitOutcome::Unknown
        } else {
            SubmitOutcome::Acknowledged
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExecutionPolicy {
    pub max_slippage_bps: Decimal,
    pub max_attempts: u8,
    pub reconcile_cycles_before_retry: u8,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            max_slippage_bps: Decimal::ZERO,
            max_attempts: 1,
            reconcile_cycles_before_retry: 1,
        }
    }
}

/// Order executor — real exchange or execution simulator.
#[async_trait]
pub trait Executor: Send {
    /// Observe a market update before decisions are submitted.
    ///
    /// Simulated executors can use this to keep quote/liquidity state current.
    fn observe_market_update(&mut self, _update: &MarketUpdate) {}

    fn execution_policy(&self) -> ExecutionPolicy {
        ExecutionPolicy::default()
    }

    fn last_reconcile_attempted(&self) -> bool {
        true
    }

    fn owns_live_retries(&self) -> bool {
        true
    }

    /// Normalize an intent immediately before submission.
    ///
    /// Live executors use this to align the runtime's requested quantity with
    /// venue-specific signing constraints. Simulated executors should keep the
    /// strategy intent unchanged.
    fn prepare_intent(&self, intent: &TradingIntent) -> TradingIntent {
        intent.clone()
    }

    /// Submit a trading intent and return the execution report.
    /// `order_id` is the caller-assigned ID that must be used in the returned fill.
    async fn submit(&mut self, intent: &TradingIntent, order_id: &str) -> ExecutionReport;

    /// Cancel an active order. Returns true if cancellation succeeded.
    async fn cancel(&mut self, order_id: &str) -> bool;

    /// Reconcile fills for active venue orders. Default executors can ignore this.
    async fn reconcile_fills(&mut self, _orders: &OrderLedger) -> Result<Vec<FillRecord>, String> {
        Ok(Vec::new())
    }
}

// ── Strategy Logic ───────────────────────────────────────

/// Decision produced by strategy evaluation.
#[derive(Debug, Clone)]
pub enum StrategyDecision {
    /// Open a new position.
    Enter {
        intent: TradingIntent,
        signal: Option<SignalRecord>,
    },
    /// Close or reduce an existing position.
    Exit(TradingIntent),
    /// No action.
    Hold,
}

/// Strategy signal evaluator — the pluggable "brain".
///
/// Implementors process market updates and produce trading decisions.
/// The runtime provides read access to positions and orders so the
/// strategy can check exposure, cooldowns, and duplicate prevention.
pub trait StrategyLogic: Send {
    /// Process a market update. May produce zero or more decisions.
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision>;

    /// Called after a fill is recorded so the strategy can update
    /// internal state (cooldowns, daily counters, etc.).
    fn on_fill(&mut self, fill: &FillRecord);

    /// Called when an order is rejected by the venue so the strategy can
    /// arm cooldowns and avoid hammering the same signal repeatedly.
    ///
    /// `reason` is the raw rejection string from the venue.
    /// Default: no-op (strategies that don't need rejection handling can ignore it).
    fn on_reject(&mut self, _intent: &ploy_trading::TradingIntent, _reason: &str) {}

    /// Strategy name for logging and metrics.
    fn name(&self) -> &str;

    /// Optional strategy-specific counters for backtest/optimizer diagnostics.
    fn diagnostics(&self) -> Vec<(String, u64)> {
        Vec::new()
    }
}

/// Blanket impl so `Box<dyn StrategyLogic>` can be used as a generic `S: StrategyLogic`.
impl StrategyLogic for Box<dyn StrategyLogic> {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        (**self).on_update(update, positions, orders)
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        (**self).on_fill(fill);
    }

    fn on_reject(&mut self, intent: &ploy_trading::TradingIntent, reason: &str) {
        (**self).on_reject(intent, reason);
    }

    fn name(&self) -> &str {
        (**self).name()
    }

    fn diagnostics(&self) -> Vec<(String, u64)> {
        (**self).diagnostics()
    }
}

// ── Recording ────────────────────────────────────────────

/// A signal evaluation record for persistence and analysis.
#[derive(Debug, Clone)]
pub struct SignalRecord {
    pub strategy: String,
    pub event_id: Option<String>,
    pub token_id: Option<String>,
    pub intent_id: Option<String>,
    pub symbol: String,
    pub direction: String,
    pub p_hat: f64,
    pub edge: f64,
    pub entry_price: Decimal,
    /// "enter", "reject:insufficient_edge", "reject:cooldown", etc.
    pub decision: String,
    pub ts: DateTime<Utc>,
}

/// Signal and trade recorder for observability.
#[async_trait]
pub trait Recorder: Send {
    /// Record a signal evaluation (both entries and rejections).
    async fn record_signal(&mut self, signal: &SignalRecord) -> Result<(), String>;

    /// Record order submission or rejection for execution audit.
    async fn record_order(
        &mut self,
        _strategy: &str,
        _intent: &TradingIntent,
        _signal: Option<&SignalRecord>,
        _report: &ExecutionReport,
        _order_id: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Record a fill for execution audit.
    async fn record_fill(
        &mut self,
        _strategy: &str,
        _intent: &TradingIntent,
        _signal: Option<&SignalRecord>,
        _fill: &FillRecord,
        _report: &ExecutionReport,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Flush buffered records to storage.
    async fn flush(&mut self) -> Result<(), String>;
}

/// No-op recorder for tests.
pub struct NullRecorder;

#[async_trait]
impl Recorder for NullRecorder {
    async fn record_signal(&mut self, _signal: &SignalRecord) -> Result<(), String> {
        Ok(())
    }
    async fn flush(&mut self) -> Result<(), String> {
        Ok(())
    }
}
