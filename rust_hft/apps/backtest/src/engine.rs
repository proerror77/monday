use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufReader, Cursor};
use std::mem;
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use itertools::Itertools;
use ordered_float::OrderedFloat;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use hft_core::{
    BookBudget, DisplayedBookLevel, DisplayedBookSnapshot, OrderType, Price, Quantity, Side,
};

use crate::config::{
    BacktestConfig, BacktestInputEvidence, ExecutionConfig, RiskConfig, StrategyConfig,
};
use crate::event::{EventEnvelope, EventPayload, EventStream, Level, TradeSide};
use hft_research_manifest::CexSpotInstrumentRulesV1;

const MICROS_IN_SECOND: f64 = 1_000_000.0;
const BPS: f64 = 10_000.0;

pub const TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION: &str =
    "hft-backtest-target-position-replay-v3";
pub const LEGACY_TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION: &str =
    "hft-backtest-target-position-replay-v2";
pub const TARGET_POSITION_REPLAY_TRACE_SCHEMA_VERSION: &str = "hft-target-position-replay-trace-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionDecision {
    pub timestamp_us: i64,
    pub target_position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionReplayConfig {
    /// Market identity is part of replay semantics; Spot cannot borrow base
    /// inventory or use derivatives funding.
    #[serde(default = "default_replay_market")]
    pub market: String,
    pub max_depth_levels: usize,
    pub max_decision_delay_us: u64,
    /// Deterministic decision-to-order arrival latency.  The next observed
    /// book at or after `decision_timestamp_us + order_latency_us` is the
    /// only book eligible for this IOC attempt.
    #[serde(default)]
    pub order_latency_us: u64,
    pub position_notional_usd: f64,
    pub fee_bps: f64,
    pub rebate_bps: f64,
    pub funding_bps: f64,
    pub latency_bps: f64,
    pub additional_slippage_bps: f64,
    /// This replay is IOC taker execution against displayed levels.  A
    /// passive or mid-queue model is not available, so `false` is rejected.
    pub cross_spread: bool,
    pub capacity_depth_levels: usize,
    pub trade_tape_declared: bool,
}

fn default_replay_market() -> String {
    "usdm".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionReplayMetrics {
    pub event_count: usize,
    pub snapshot_events: usize,
    pub l2_update_events: usize,
    pub trade_events: usize,
    pub decision_count: usize,
    pub position_changes: usize,
    pub first_event_time_us: i64,
    pub last_event_time_us: i64,
    pub max_decision_delay_us: u64,
    pub min_bid_depth_levels: usize,
    pub max_bid_depth_levels: usize,
    pub min_ask_depth_levels: usize,
    pub max_ask_depth_levels: usize,
    pub total_turnover: f64,
    /// Requested target turnover retained for policy comparison.
    #[serde(default)]
    pub requested_turnover: f64,
    /// Executed turnover measured from filled notional / configured notional.
    #[serde(default)]
    pub executed_turnover: f64,
    pub mean_net_return: f64,
    pub cumulative_net_return: f64,
    pub max_drawdown: f64,
    pub net_sharpe: f64,
    pub max_abs_position: f64,
    pub max_same_side_depth_fraction: Option<f64>,
    #[serde(default)]
    pub order_count: usize,
    #[serde(default)]
    pub filled_order_count: usize,
    #[serde(default)]
    pub partial_order_count: usize,
    #[serde(default)]
    pub canceled_order_count: usize,
    #[serde(default)]
    pub fill_count: usize,
    #[serde(default)]
    pub max_residual_quantity: f64,
    #[serde(default)]
    pub displayed_depth_unavailable: bool,
    #[serde(default)]
    pub final_cash: f64,
    #[serde(default)]
    pub final_inventory: f64,
    #[serde(default)]
    pub total_fees: f64,
    #[serde(default)]
    pub total_funding_cost: f64,
    #[serde(default)]
    pub total_execution_cost: f64,
    #[serde(default)]
    pub trace_event_count: usize,
    #[serde(default)]
    pub trace_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionReplayFill {
    pub price: f64,
    pub quantity: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionReplayTraceEvent {
    pub schema_version: String,
    pub decision_index: usize,
    pub decision_timestamp_us: i64,
    pub order_timestamp_us: i64,
    pub arrival_timestamp_us: i64,
    pub time_in_force: String,
    pub target_position: f64,
    pub side: Option<Side>,
    pub order_type: Option<OrderType>,
    pub requested_quantity: f64,
    pub filled_quantity: f64,
    pub residual_quantity: f64,
    pub vwap: Option<f64>,
    pub fees: f64,
    pub funding_cost: f64,
    pub execution_cost: f64,
    pub cash_after: f64,
    pub inventory_after: f64,
    pub status: String,
    pub fills: Vec<TargetPositionReplayFill>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TargetPositionReplayOutput {
    pub metrics: TargetPositionReplayMetrics,
    pub trace_bytes: Vec<u8>,
    pub trace_sha256: String,
}

pub fn replay_target_positions(
    event_bytes: &[u8],
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
) -> Result<TargetPositionReplayMetrics> {
    let mut replay = TargetPositionReplay::new(decisions, config)?;
    let stream = EventStream::new(BufReader::new(Cursor::new(event_bytes)), None, None, true);
    for event in stream {
        replay.observe(&event?)?;
    }
    replay.finish()
}

/// Replay a target-position tape with the immutable full Spot exchangeInfo
/// rules carried by the materialization snapshot. USD-M callers should use
/// `replay_target_positions`; Spot callers must provide this binding so the
/// replay cannot silently fall back to generic tick/step/min-notional fields.
pub fn replay_target_positions_with_spot_rules(
    event_bytes: &[u8],
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
    spot_instrument_rules: Option<&CexSpotInstrumentRulesV1>,
) -> Result<TargetPositionReplayMetrics> {
    let mut replay =
        TargetPositionReplay::new_with_spot_rules(decisions, config, spot_instrument_rules)?;
    let stream = EventStream::new(BufReader::new(Cursor::new(event_bytes)), None, None, true);
    for event in stream {
        replay.observe(&event?)?;
    }
    replay.finish()
}

pub fn replay_target_positions_with_trace(
    event_bytes: &[u8],
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
) -> Result<TargetPositionReplayOutput> {
    let mut replay = TargetPositionReplay::new(decisions, config)?;
    let stream = EventStream::new(BufReader::new(Cursor::new(event_bytes)), None, None, true);
    for event in stream {
        replay.observe(&event?)?;
    }
    replay.finish_with_trace()
}

pub fn replay_target_positions_with_trace_and_spot_rules(
    event_bytes: &[u8],
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
    spot_instrument_rules: Option<&CexSpotInstrumentRulesV1>,
) -> Result<TargetPositionReplayOutput> {
    let mut replay =
        TargetPositionReplay::new_with_spot_rules(decisions, config, spot_instrument_rules)?;
    let stream = EventStream::new(BufReader::new(Cursor::new(event_bytes)), None, None, true);
    for event in stream {
        replay.observe(&event?)?;
    }
    replay.finish_with_trace()
}

pub(crate) struct TargetPositionReplay<'a> {
    decisions: &'a [TargetPositionDecision],
    config: &'a TargetPositionReplayConfig,
    spot_instrument_rules: Option<&'a CexSpotInstrumentRulesV1>,
    book: OrderBook,
    displayed_budget: BookBudget,
    seeded: bool,
    book_generation: u64,
    decision_index: usize,
    event_count: usize,
    snapshot_events: usize,
    l2_update_events: usize,
    trade_events: usize,
    position_changes: usize,
    first_event_time_us: Option<i64>,
    last_event_time_us: Option<i64>,
    max_decision_delay_us: u64,
    min_bid_depth_levels: usize,
    max_bid_depth_levels: usize,
    min_ask_depth_levels: usize,
    max_ask_depth_levels: usize,
    inventory: f64,
    cash: f64,
    initial_cash: f64,
    last_trade_price: Option<f64>,
    marked_mid: Option<f64>,
    total_turnover: f64,
    requested_turnover: f64,
    executed_turnover: f64,
    max_same_side_depth_fraction: Option<f64>,
    returns: Vec<f64>,
    order_count: usize,
    filled_order_count: usize,
    partial_order_count: usize,
    canceled_order_count: usize,
    fill_count: usize,
    max_residual_quantity: f64,
    displayed_depth_unavailable: bool,
    max_abs_inventory_ratio: f64,
    total_fees: f64,
    total_funding_cost: f64,
    total_execution_cost: f64,
    trace: Vec<u8>,
}

impl<'a> TargetPositionReplay<'a> {
    pub(crate) fn new(
        decisions: &'a [TargetPositionDecision],
        config: &'a TargetPositionReplayConfig,
    ) -> Result<Self> {
        Self::new_with_spot_rules(decisions, config, None)
    }

    pub(crate) fn new_with_spot_rules(
        decisions: &'a [TargetPositionDecision],
        config: &'a TargetPositionReplayConfig,
        spot_instrument_rules: Option<&'a CexSpotInstrumentRulesV1>,
    ) -> Result<Self> {
        validate_target_replay_inputs(decisions, config, spot_instrument_rules)?;
        Ok(Self {
            decisions,
            config,
            spot_instrument_rules,
            book: OrderBook::new(config.max_depth_levels),
            displayed_budget: BookBudget::default(),
            seeded: false,
            book_generation: 0,
            decision_index: 0,
            event_count: 0,
            snapshot_events: 0,
            l2_update_events: 0,
            trade_events: 0,
            position_changes: 0,
            first_event_time_us: None,
            last_event_time_us: None,
            max_decision_delay_us: 0,
            min_bid_depth_levels: usize::MAX,
            max_bid_depth_levels: 0,
            min_ask_depth_levels: usize::MAX,
            max_ask_depth_levels: 0,
            inventory: 0.0,
            cash: config.position_notional_usd,
            initial_cash: config.position_notional_usd,
            last_trade_price: None,
            marked_mid: None,
            total_turnover: 0.0,
            requested_turnover: 0.0,
            executed_turnover: 0.0,
            max_same_side_depth_fraction: config.capacity_depth_levels.gt(&0).then_some(0.0),
            returns: Vec::with_capacity(decisions.len()),
            order_count: 0,
            filled_order_count: 0,
            partial_order_count: 0,
            canceled_order_count: 0,
            fill_count: 0,
            max_residual_quantity: 0.0,
            displayed_depth_unavailable: false,
            max_abs_inventory_ratio: 0.0,
            total_fees: 0.0,
            total_funding_cost: 0.0,
            total_execution_cost: 0.0,
            trace: Vec::with_capacity(decisions.len() * 256),
        })
    }

    pub(crate) fn observe(&mut self, event: &EventEnvelope) -> Result<()> {
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("target-position replay event count overflow"))?;
        self.first_event_time_us.get_or_insert(event.ts);
        self.last_event_time_us = Some(event.ts);
        let sequence = event
            .sequence
            .context("target-position replay event is missing sequence")?;
        if event.ts <= 0 {
            anyhow::bail!("target-position replay event timestamp must be positive");
        }
        let book_observed = match &event.payload {
            EventPayload::Snapshot { bids, asks } => {
                if self.seeded {
                    self.observe_series_boundary(event.ts)?;
                    self.marked_mid = None;
                }
                self.last_trade_price = None;
                self.book_generation = self.book_generation.checked_add(1).ok_or_else(|| {
                    anyhow::anyhow!("target-position replay book generation overflow")
                })?;
                self.book.apply_snapshot(event.ts, bids, asks);
                let snapshot = self.book.displayed_snapshot(
                    sequence,
                    self.book_generation,
                    u64::try_from(event.ts)?,
                );
                if !self
                    .displayed_budget
                    .observe(&snapshot, u64::try_from(event.ts)?)
                {
                    anyhow::bail!("target-position replay received an invalid or stale snapshot");
                }
                self.seeded = true;
                self.snapshot_events += 1;
                true
            }
            EventPayload::L2Update { bids, asks } => {
                if !self.seeded {
                    anyhow::bail!("target-position replay received an L2 update before a snapshot");
                }
                self.book.apply_delta(event.ts, bids, asks);
                let snapshot = self.book.displayed_snapshot(
                    sequence,
                    self.book_generation,
                    u64::try_from(event.ts)?,
                );
                if !self
                    .displayed_budget
                    .observe(&snapshot, u64::try_from(event.ts)?)
                {
                    anyhow::bail!("target-position replay received an invalid or stale L2 book");
                }
                self.l2_update_events += 1;
                true
            }
            EventPayload::Trade { price, .. } => {
                if !self.config.trade_tape_declared {
                    anyhow::bail!("target-position replay tape contains undeclared trade events");
                }
                if !price.is_finite() || *price <= 0.0 {
                    anyhow::bail!("target-position replay trade price is invalid");
                }
                self.last_trade_price = Some(*price);
                self.trade_events += 1;
                false
            }
        };
        if self.seeded && book_observed {
            let (bid_levels, ask_levels) = self.book.depth_level_counts();
            self.min_bid_depth_levels = self.min_bid_depth_levels.min(bid_levels);
            self.max_bid_depth_levels = self.max_bid_depth_levels.max(bid_levels);
            self.min_ask_depth_levels = self.min_ask_depth_levels.min(ask_levels);
            self.max_ask_depth_levels = self.max_ask_depth_levels.max(ask_levels);
            self.process_due_decisions(event.ts)?;
        }
        Ok(())
    }

    fn process_due_decisions(&mut self, arrival_ts_us: i64) -> Result<()> {
        let mid = self
            .displayed_budget
            .mid_price()
            .and_then(|price| price.to_f64())
            .context("target-position replay has no valid mid price")?;
        if !mid.is_finite() || mid <= 0.0 {
            anyhow::bail!("target-position replay has a non-positive mid price");
        }
        let book_fresh = self
            .displayed_budget
            .is_fresh(u64::try_from(arrival_ts_us)?);
        let order_latency_us = i64::try_from(self.config.order_latency_us)
            .context("target-position replay order latency exceeds i64")?;
        while self.decision_index < self.decisions.len() {
            let decision = self.decisions[self.decision_index].clone();
            let eligible_at = decision
                .timestamp_us
                .checked_add(order_latency_us)
                .context("target-position replay order arrival overflows")?;
            if eligible_at > arrival_ts_us {
                break;
            }
            let delay = u64::try_from(arrival_ts_us - decision.timestamp_us)
                .map_err(|_| anyhow::anyhow!("target-position replay decision clock reversed"))?;
            if delay > self.config.max_decision_delay_us {
                anyhow::bail!("target-position replay decision exceeded its maximum delay");
            }
            self.max_decision_delay_us = self.max_decision_delay_us.max(delay);
            self.apply_decision(&decision, arrival_ts_us, mid, book_fresh)?;
            self.decision_index += 1;
        }
        Ok(())
    }

    fn apply_decision(
        &mut self,
        decision: &TargetPositionDecision,
        arrival_ts_us: i64,
        mid: f64,
        book_fresh: bool,
    ) -> Result<()> {
        let equity_before = self.cash + self.inventory * self.marked_mid.unwrap_or(mid);
        let funding_cost = self.inventory.abs() * mid * self.config.funding_bps / BPS;
        self.cash -= funding_cost;
        self.total_funding_cost += funding_cost;
        let target_inventory = decision.target_position * self.config.position_notional_usd / mid;
        if self.config.market == "spot" && target_inventory < -f64::EPSILON {
            anyhow::bail!("Spot target-position replay cannot create a short inventory");
        }
        let raw_requested_quantity = (target_inventory - self.inventory).abs();
        let requested_quantity = if self.config.market == "spot" {
            let rules = self
                .spot_instrument_rules
                .context("validated Spot instrument rules are unavailable")?;
            spot_submission_quantity(rules, raw_requested_quantity)?
        } else {
            raw_requested_quantity
        };
        let side = (target_inventory - self.inventory > f64::EPSILON)
            .then_some(Side::Buy)
            .or_else(|| (target_inventory - self.inventory < -f64::EPSILON).then_some(Side::Sell));
        let mut fills_for_trace = Vec::new();
        let mut filled_quantity = 0.0;
        let mut fill_notional = 0.0;
        let mut fees = 0.0;
        let mut status = "no_order".to_string();

        if let Some(side) = side {
            self.position_changes += 1;
            self.order_count += 1;
            if let Some(max_fraction) = &mut self.max_same_side_depth_fraction {
                let depth = self
                    .displayed_budget
                    .same_side_depth(side, self.config.capacity_depth_levels)
                    .and_then(|quantity| quantity.to_f64())
                    .filter(|depth| *depth > 0.0);
                if let Some(depth) = depth {
                    let depth_notional = depth * mid;
                    if depth_notional > 0.0 {
                        *max_fraction =
                            (*max_fraction).max(requested_quantity * mid / depth_notional);
                    }
                } else {
                    self.displayed_depth_unavailable = true;
                }
            }
            // Match against a candidate budget first.  A Spot cash rejection
            // must leave the observed displayed liquidity available for the
            // next decision; BookBudget::fills mutates the levels it consumes.
            let mut trial_budget = self.displayed_budget.clone();
            let fills = if book_fresh {
                trial_budget.fills(
                    side,
                    OrderType::Market,
                    None,
                    Quantity::from_f64(requested_quantity).map_err(|_| {
                        anyhow::anyhow!(
                            "target-position replay requested quantity is not representable"
                        )
                    })?,
                )
            } else {
                self.displayed_depth_unavailable = true;
                Vec::new()
            };
            let planned = fills
                .iter()
                .map(|fill| {
                    Ok::<_, anyhow::Error>((
                        fill.price
                            .to_f64()
                            .context("target-position replay fill price is not representable")?,
                        fill.quantity
                            .to_f64()
                            .context("target-position replay fill quantity is not representable")?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let planned_notional = planned
                .iter()
                .map(|(price, quantity)| price * quantity)
                .sum::<f64>();
            let planned_fees =
                planned_notional * (self.config.fee_bps - self.config.rebate_bps) / BPS;
            let planned_execution_cost = planned_notional
                * (self.config.latency_bps + self.config.additional_slippage_bps)
                / BPS;
            let invalid_spot_rules = if self.config.market == "spot" {
                let rules = self
                    .spot_instrument_rules
                    .context("validated Spot instrument rules are unavailable")?;
                !spot_market_order_is_admissible(rules, requested_quantity, self.last_trade_price)?
            } else {
                false
            };
            let insufficient_cash = self.config.market == "spot"
                && side == Side::Buy
                && planned_notional + planned_fees.max(0.0) + planned_execution_cost
                    > self.cash + 1e-9;
            if invalid_spot_rules {
                self.canceled_order_count += 1;
                status = "cancelled_invalid_instrument_rules".to_string();
            } else if insufficient_cash {
                self.canceled_order_count += 1;
                status = "cancelled_insufficient_cash".to_string();
            } else {
                self.displayed_budget = trial_budget;
                self.fill_count += planned.len();
                for ((price, quantity), _fill) in planned.iter().zip(fills) {
                    filled_quantity += *quantity;
                    fill_notional += *price * *quantity;
                    fills_for_trace.push(TargetPositionReplayFill {
                        price: *price,
                        quantity: *quantity,
                    });
                    match side {
                        Side::Buy => {
                            self.cash -= *price * *quantity;
                            self.inventory += *quantity;
                        }
                        Side::Sell => {
                            self.cash += *price * *quantity;
                            self.inventory -= *quantity;
                        }
                    }
                }
                if self.inventory.abs() <= f64::EPSILON {
                    self.inventory = 0.0;
                }
            }
            let residual_quantity = (requested_quantity - filled_quantity).max(0.0);
            let residual_quantity = if residual_quantity <= f64::EPSILON {
                0.0
            } else {
                residual_quantity
            };
            self.max_residual_quantity = self.max_residual_quantity.max(residual_quantity);
            if invalid_spot_rules || insufficient_cash {
                // The order was rejected before any fill; its residual is
                // retained in the trace for the admission decision.
                self.max_residual_quantity = self.max_residual_quantity.max(residual_quantity);
            } else if !book_fresh {
                self.canceled_order_count += 1;
                status = "cancelled_stale_book".to_string();
            } else if filled_quantity <= f64::EPSILON {
                self.canceled_order_count += 1;
                status = "cancelled_no_liquidity".to_string();
            } else if residual_quantity > f64::EPSILON {
                self.partial_order_count += 1;
                self.canceled_order_count += 1;
                status = "partial_fill_cancelled".to_string();
            } else {
                self.filled_order_count += 1;
                status = "filled".to_string();
            }
            if !invalid_spot_rules && !insufficient_cash {
                fees = fill_notional * (self.config.fee_bps - self.config.rebate_bps) / BPS;
                let declared_execution_cost = fill_notional
                    * (self.config.latency_bps + self.config.additional_slippage_bps)
                    / BPS;
                self.cash -= fees + declared_execution_cost;
                self.total_fees += fees;
                self.total_execution_cost += declared_execution_cost;
            }
        }

        let equity_after = self.cash + self.inventory * mid;
        self.max_abs_inventory_ratio = self
            .max_abs_inventory_ratio
            .max(self.inventory.abs() * mid / self.initial_cash);
        self.returns
            .push((equity_after - equity_before) / self.initial_cash);
        self.requested_turnover += requested_quantity * mid / self.config.position_notional_usd;
        self.executed_turnover += fill_notional / self.config.position_notional_usd;
        self.total_turnover = self.executed_turnover;
        self.marked_mid = Some(mid);
        let residual_quantity = (target_inventory - self.inventory).abs();
        let residual_quantity = if residual_quantity <= f64::EPSILON {
            0.0
        } else {
            residual_quantity
        };
        self.max_residual_quantity = self.max_residual_quantity.max(residual_quantity);
        let vwap = (filled_quantity > f64::EPSILON).then_some(fill_notional / filled_quantity);
        let trace_event = TargetPositionReplayTraceEvent {
            schema_version: TARGET_POSITION_REPLAY_TRACE_SCHEMA_VERSION.to_string(),
            decision_index: self.decision_index,
            decision_timestamp_us: decision.timestamp_us,
            order_timestamp_us: decision.timestamp_us,
            arrival_timestamp_us: arrival_ts_us,
            time_in_force: "IOC".to_string(),
            target_position: decision.target_position,
            side,
            order_type: side.map(|_| OrderType::Market),
            requested_quantity,
            filled_quantity,
            residual_quantity,
            vwap,
            fees,
            funding_cost,
            execution_cost: fill_notional
                * (self.config.latency_bps + self.config.additional_slippage_bps)
                / BPS,
            cash_after: self.cash,
            inventory_after: self.inventory,
            status,
            fills: fills_for_trace,
        };
        serde_json::to_writer(&mut self.trace, &trace_event)?;
        self.trace.push(b'\n');
        Ok(())
    }

    fn observe_series_boundary(&self, snapshot_ts: i64) -> Result<()> {
        if self.inventory.abs() > f64::EPSILON {
            anyhow::bail!("target-position replay received a new snapshot before flattening");
        }
        if self
            .decisions
            .get(self.decision_index)
            .is_some_and(|decision| decision.timestamp_us < snapshot_ts)
        {
            anyhow::bail!(
                "target-position replay has leftover pre-snapshot decisions from the prior series"
            );
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<TargetPositionReplayMetrics> {
        Ok(self.finish_with_trace()?.metrics)
    }

    pub(crate) fn finish_with_trace(self) -> Result<TargetPositionReplayOutput> {
        if self.decision_index != self.decisions.len() {
            anyhow::bail!("target-position replay tape ended before all decisions");
        }
        if self.inventory.abs() > f64::EPSILON {
            anyhow::bail!("target-position replay tape ended before flattening actual inventory");
        }
        if self.config.trade_tape_declared && self.trade_events == 0 {
            anyhow::bail!("target-position replay manifest declares trades but none were replayed");
        }
        let first_event_time_us = self
            .first_event_time_us
            .context("target-position replay is empty")?;
        let last_event_time_us = self
            .last_event_time_us
            .context("target-position replay is empty")?;
        let cumulative_net_return = self.returns.iter().sum::<f64>();
        let mean_net_return = cumulative_net_return / self.returns.len() as f64;
        let variance = self
            .returns
            .iter()
            .map(|value| (value - mean_net_return).powi(2))
            .sum::<f64>()
            / self.returns.len() as f64;
        let net_sharpe = if variance > 0.0 {
            mean_net_return / variance.sqrt() * (self.returns.len() as f64).sqrt()
        } else {
            0.0
        };
        let mut equity = 1.0_f64;
        let mut peak = 1.0_f64;
        let mut max_drawdown = 0.0_f64;
        for value in &self.returns {
            equity += value;
            peak = peak.max(equity);
            if peak > f64::EPSILON {
                max_drawdown = max_drawdown.max((peak - equity) / peak);
            }
        }
        let trace_sha256 = hex::encode(Sha256::digest(&self.trace));
        let metrics = TargetPositionReplayMetrics {
            event_count: self.event_count,
            snapshot_events: self.snapshot_events,
            l2_update_events: self.l2_update_events,
            trade_events: self.trade_events,
            decision_count: self.decisions.len(),
            position_changes: self.position_changes,
            first_event_time_us,
            last_event_time_us,
            max_decision_delay_us: self.max_decision_delay_us,
            min_bid_depth_levels: self.min_bid_depth_levels,
            max_bid_depth_levels: self.max_bid_depth_levels,
            min_ask_depth_levels: self.min_ask_depth_levels,
            max_ask_depth_levels: self.max_ask_depth_levels,
            total_turnover: self.total_turnover,
            requested_turnover: self.requested_turnover,
            executed_turnover: self.executed_turnover,
            mean_net_return,
            cumulative_net_return,
            max_drawdown,
            net_sharpe,
            max_abs_position: self.max_abs_inventory_ratio,
            max_same_side_depth_fraction: self.max_same_side_depth_fraction,
            order_count: self.order_count,
            filled_order_count: self.filled_order_count,
            partial_order_count: self.partial_order_count,
            canceled_order_count: self.canceled_order_count,
            fill_count: self.fill_count,
            max_residual_quantity: self.max_residual_quantity,
            displayed_depth_unavailable: self.displayed_depth_unavailable,
            final_cash: self.cash,
            final_inventory: self.inventory,
            total_fees: self.total_fees,
            total_funding_cost: self.total_funding_cost,
            total_execution_cost: self.total_execution_cost,
            trace_event_count: self.decisions.len(),
            trace_sha256: trace_sha256.clone(),
        };
        Ok(TargetPositionReplayOutput {
            metrics,
            trace_bytes: self.trace,
            trace_sha256,
        })
    }
}

fn validate_target_replay_inputs(
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
    spot_instrument_rules: Option<&CexSpotInstrumentRulesV1>,
) -> Result<()> {
    let market = config
        .market
        .parse::<data::binance_lob_replay::Market>()
        .map_err(anyhow::Error::msg)?;
    if config.market != market.as_str() {
        anyhow::bail!("target-position replay market must be canonical lowercase");
    }
    match (market, spot_instrument_rules) {
        (data::binance_lob_replay::Market::Spot, Some(rules)) => {
            rules
                .validate()
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let market_notional_applies = rules.notional_filter.apply_min_to_market
                || rules.notional_filter.apply_max_to_market.unwrap_or(false);
            if market_notional_applies && rules.notional_filter.avg_price_mins != 0 {
                anyhow::bail!(
                    "Spot target-position replay requires avg_price_mins=0; average-price evidence is unsupported"
                );
            }
        }
        (data::binance_lob_replay::Market::Spot, None) => {
            anyhow::bail!("Spot target-position replay requires full instrument rules");
        }
        (data::binance_lob_replay::Market::Usdm, Some(_)) => {
            anyhow::bail!("USD-M target-position replay cannot carry Spot instrument rules");
        }
        (data::binance_lob_replay::Market::Usdm, None) => {}
    }
    let costs = [
        config.position_notional_usd,
        config.fee_bps,
        config.rebate_bps,
        config.funding_bps,
        config.latency_bps,
        config.additional_slippage_bps,
    ];
    if decisions.is_empty()
        || decisions
            .windows(2)
            .any(|pair| pair[0].timestamp_us >= pair[1].timestamp_us)
        || decisions.iter().any(|decision| {
            !decision.target_position.is_finite() || decision.target_position.abs() > 1.0
        })
        || config.max_depth_levels == 0
        || config.max_decision_delay_us == 0
        || config.order_latency_us > config.max_decision_delay_us
        || costs.iter().any(|value| !value.is_finite() || *value < 0.0)
        || !config.position_notional_usd.is_finite()
        || config.position_notional_usd <= 0.0
        || config.capacity_depth_levels > config.max_depth_levels
        || (market == data::binance_lob_replay::Market::Spot
            && (config.position_notional_usd <= 0.0
                || config.funding_bps != 0.0
                || decisions
                    .iter()
                    .any(|decision| decision.target_position < -f64::EPSILON)))
    {
        anyhow::bail!("target-position replay inputs are invalid");
    }
    if !config.cross_spread {
        anyhow::bail!(
            "target-position replay requires cross_spread=true; passive or mid-queue execution is unsupported"
        );
    }
    Ok(())
}

fn spot_market_order_is_admissible(
    rules: &CexSpotInstrumentRulesV1,
    requested_quantity: f64,
    last_trade_price: Option<f64>,
) -> Result<bool> {
    if !requested_quantity.is_finite() || requested_quantity <= 0.0 {
        return Ok(false);
    }
    if !spot_quantity_matches(&rules.lot_size_filter, requested_quantity) {
        return Ok(false);
    }
    if let Some(market_filter) = &rules.market_lot_size_filter {
        if !spot_quantity_matches(market_filter, requested_quantity) {
            return Ok(false);
        }
    }
    let market_notional_applies = rules.notional_filter.apply_min_to_market
        || rules.notional_filter.apply_max_to_market.unwrap_or(false);
    if !market_notional_applies {
        return Ok(true);
    }
    let reference_price = last_trade_price.context(
        "Spot market notional admission requires a last-trade price; replay evidence is missing",
    )?;
    if !reference_price.is_finite() || reference_price <= 0.0 {
        return Ok(false);
    }
    if rules.notional_filter.avg_price_mins != 0 {
        anyhow::bail!(
            "Spot target-position replay requires unsupported average-price evidence for market notional admission"
        );
    }
    let requested_notional = reference_price * requested_quantity;
    if !requested_notional.is_finite() || requested_notional <= 0.0 {
        return Ok(false);
    }
    let minimum_applies = rules.notional_filter.apply_min_to_market;
    if minimum_applies
        && requested_notional + 1e-9
            < rules
                .notional_filter
                .min_notional
                .parse::<f64>()
                .unwrap_or(f64::INFINITY)
    {
        return Ok(false);
    }
    if rules.notional_filter.apply_max_to_market.unwrap_or(false)
        && rules
            .notional_filter
            .max_notional
            .as_deref()
            .and_then(|value| value.parse::<f64>().ok())
            .is_some_and(|maximum| requested_notional > maximum + 1e-9)
    {
        return Ok(false);
    }
    Ok(true)
}

fn spot_quantity_matches(
    filter: &hft_research_manifest::CexSpotQuantityFilterV1,
    quantity: f64,
) -> bool {
    let min = filter.min_quantity.parse::<f64>().ok();
    let max = filter.max_quantity.parse::<f64>().ok();
    let step = filter.step_size.parse::<f64>().ok();
    if min.is_none() || max.is_none() || step.is_none() {
        return false;
    }
    let min = min.unwrap();
    let max = max.unwrap();
    let step = step.unwrap();
    (min <= 0.0 || quantity + 1e-9 >= min)
        && (max <= 0.0 || quantity <= max + 1e-9)
        && (step <= 0.0 || aligned_to_step(quantity, step))
}

fn decimal_gcd(mut left: i128, mut right: i128) -> i128 {
    left = left.abs();
    right = right.abs();
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn common_quantity_step(left: Decimal, right: Decimal) -> Result<Decimal> {
    let scale = left.scale().max(right.scale());
    let left_scale = 10_i128
        .checked_pow(scale - left.scale())
        .context("Spot quantity step scale overflow")?;
    let right_scale = 10_i128
        .checked_pow(scale - right.scale())
        .context("Spot quantity step scale overflow")?;
    let left_units = left
        .mantissa()
        .abs()
        .checked_mul(left_scale)
        .context("Spot quantity step integer conversion overflow")?;
    let right_units = right
        .mantissa()
        .abs()
        .checked_mul(right_scale)
        .context("Spot quantity step integer conversion overflow")?;
    let gcd = decimal_gcd(left_units, right_units);
    if gcd == 0 {
        return Ok(left);
    }
    let lcm = left_units
        .checked_div(gcd)
        .and_then(|value| value.checked_mul(right_units))
        .context("Spot quantity step least common multiple overflow")?;
    Decimal::try_from_i128_with_scale(lcm, scale)
        .context("Spot quantity step Decimal conversion overflow")
}

fn spot_submission_quantity(
    rules: &CexSpotInstrumentRulesV1,
    requested_quantity: f64,
) -> Result<f64> {
    if !requested_quantity.is_finite() || requested_quantity <= 0.0 {
        return Ok(0.0);
    }
    let mut step = Decimal::from_str(&rules.lot_size_filter.step_size)
        .context("Spot LOT_SIZE step is invalid")?;
    if step <= Decimal::ZERO {
        bail!("Spot LOT_SIZE step must be positive");
    }
    if let Some(market_filter) = &rules.market_lot_size_filter {
        let market_step = Decimal::from_str(&market_filter.step_size)
            .context("Spot MARKET_LOT_SIZE step is invalid")?;
        if market_step > Decimal::ZERO {
            step = common_quantity_step(step, market_step)?;
        }
    }
    if rules.base_asset_precision > 28 {
        bail!("Spot base-asset precision exceeds Decimal capacity");
    }
    step = common_quantity_step(step, Decimal::new(1, rules.base_asset_precision as u32))?;
    let requested = Decimal::from_f64_retain(requested_quantity)
        .context("Spot requested quantity is not representable")?;
    let requested = requested.round_dp(rules.base_asset_precision as u32);
    let units = (requested / step).floor();
    (units * step)
        .to_f64()
        .context("Spot submitted quantity is not representable")
}

fn aligned_to_step(value: f64, step: f64) -> bool {
    if !value.is_finite() || !step.is_finite() || step <= 0.0 {
        return false;
    }
    let units = value / step;
    (units - units.round()).abs() <= 1e-9_f64.max(units.abs() * 1e-12)
}

pub struct BacktestEngine {
    cfg: BacktestConfig,
    order_book: OrderBook,
    displayed_budget: BookBudget,
    book_generation: u64,
    next_sequence: u64,
    liquidity: LiquidityMap,
    flow: FlowTracker,
    execution: ExecutionManager,
    stats: BacktestStats,
    last_ts: Option<i64>,
}

impl BacktestEngine {
    pub fn new(cfg: BacktestConfig) -> Result<Self> {
        let max_levels = cfg.data.max_depth_levels;
        let tick_size = cfg.data.tick_size.max(1e-6);
        let strategy = cfg.strategy.clone();
        let execution_cfg = cfg.execution.clone();
        let risk_cfg = cfg.risk.clone();
        let market = cfg.data.market.parse().map_err(anyhow::Error::msg)?;
        Ok(Self {
            cfg,
            order_book: OrderBook::new(max_levels),
            displayed_budget: BookBudget::default(),
            book_generation: 0,
            next_sequence: 0,
            liquidity: LiquidityMap::new(strategy, tick_size, max_levels),
            flow: FlowTracker::new(),
            execution: ExecutionManager::new(execution_cfg, risk_cfg, tick_size, market),
            stats: BacktestStats::default(),
            last_ts: None,
        })
    }

    pub fn run(&mut self) -> Result<BacktestResult> {
        let verified = self.cfg.validate_data_artifact()?;
        let stream = EventStream::new(
            BufReader::new(Cursor::new(verified.bytes)),
            self.cfg.data.start_ts,
            self.cfg.data.end_ts,
            self.cfg.data.require_sequence,
        );
        let mut result = self.run_with_stream(stream)?;
        result.input_evidence = Some(verified.evidence);
        Ok(result)
    }

    pub fn run_with_stream<I>(&mut self, stream: I) -> Result<BacktestResult>
    where
        I: Iterator<Item = anyhow::Result<EventEnvelope>>,
    {
        for evt in stream {
            let event = evt?;
            self.process_event(&event)?;
        }

        // 平倉殘餘持倉
        if self.execution.has_position() {
            if let Some((fill_qty, fill_price)) = self
                .execution
                .executable_exit_from_budget(&mut self.displayed_budget)
            {
                let ts = self
                    .last_ts
                    .map(|t| t as f64 / MICROS_IN_SECOND)
                    .unwrap_or(0.0);
                self.execution.exit_position(
                    ts,
                    fill_price,
                    fill_qty,
                    ExitReason::SessionEnd,
                    &mut self.stats,
                );
            }
        }

        Ok(self.finish())
    }

    fn process_event(&mut self, event: &EventEnvelope) -> Result<()> {
        self.last_ts = Some(event.ts);
        let ts_sec = event.ts as f64 / MICROS_IN_SECOND;
        match &event.payload {
            EventPayload::Snapshot { bids, asks } => {
                let ofi = self.order_book.apply_snapshot(event.ts, bids, asks);
                self.observe_displayed_book(event, true)?;
                self.liquidity.update(
                    event.ts,
                    &self.order_book.snapshot(self.cfg.data.max_depth_levels),
                );
                self.flow.update_ofi(ts_sec, ofi);
                self.evaluate_signals(event.ts)?;
            }
            EventPayload::L2Update { bids, asks } => {
                let ofi = self.order_book.apply_delta(event.ts, bids, asks);
                self.observe_displayed_book(event, false)?;
                self.liquidity.update(
                    event.ts,
                    &self.order_book.snapshot(self.cfg.data.max_depth_levels),
                );
                self.flow.update_ofi(ts_sec, ofi);
                self.evaluate_signals(event.ts)?;
            }
            EventPayload::Trade {
                side,
                price,
                quantity,
            } => {
                self.flow.update_trade(ts_sec, *side, *quantity);
                self.stats.last_trade_price = Some(*price);
            }
        }
        Ok(())
    }

    fn observe_displayed_book(&mut self, event: &EventEnvelope, snapshot: bool) -> Result<()> {
        let sequence = event
            .sequence
            .unwrap_or_else(|| self.next_sequence.saturating_add(1));
        self.next_sequence = self.next_sequence.max(sequence);
        if snapshot {
            self.book_generation = self
                .book_generation
                .checked_add(1)
                .context("backtest book generation overflow")?;
        } else if self.book_generation == 0 {
            self.displayed_budget.clear_levels();
            return Ok(());
        }
        let received_at_us =
            u64::try_from(event.ts).context("backtest event timestamp is invalid")?;
        let displayed =
            self.order_book
                .displayed_snapshot(sequence, self.book_generation, received_at_us);
        if !self.displayed_budget.observe(&displayed, received_at_us) {
            // The feature book remains available for signal diagnostics, but
            // execution loses its budget until a fresh valid observation.
            self.displayed_budget.clear_levels();
        }
        Ok(())
    }

    fn executable_entry_from_budget(
        &self,
        side: PositionSide,
        requested_qty: f64,
    ) -> Option<(f64, f64, BookBudget)> {
        let mut trial_budget = self.displayed_budget.clone();
        let (book_side, worst) = match side {
            PositionSide::Long => {
                let best = trial_budget.best_ask()?.0.to_f64()?;
                let worst = Price::from_f64(
                    best + self.cfg.execution.max_slippage_ticks * self.cfg.data.tick_size,
                )
                .ok()?;
                (Side::Buy, worst)
            }
            PositionSide::Short => {
                let best = trial_budget.best_bid()?.0.to_f64()?;
                let worst = Price::from_f64(
                    best - self.cfg.execution.max_slippage_ticks * self.cfg.data.tick_size,
                )
                .ok()?;
                (Side::Sell, worst)
            }
        };
        let fills = trial_budget.fills_with_price_bound_and_participation(
            book_side,
            OrderType::Market,
            None,
            Quantity::from_f64(requested_qty).ok()?,
            Some(worst),
            Quantity::from_f64(self.cfg.execution.max_fill_ratio.clamp(0.0, 1.0))
                .ok()?
                .0,
        );
        aggregate_displayed_fills(&fills).map(|(qty, price)| (qty, price, trial_budget))
    }

    fn evaluate_signals(&mut self, ts: i64) -> Result<()> {
        let mid = match self.order_book.mid_price() {
            Some(m) => m,
            None => return Ok(()),
        };
        let ts_sec = ts as f64 / MICROS_IN_SECOND;

        // 支撐／壓力
        let supports = self
            .liquidity
            .support_levels(mid, self.cfg.strategy.support_count);
        let resistances = self
            .liquidity
            .resistance_levels(mid, self.cfg.strategy.resistance_count);

        // 計算流動性特徵
        let depth_support = supports.first().cloned();
        let depth_resistance = resistances.first().cloned();
        let tt_vol_down = self
            .flow
            .tt_sell_volume(self.cfg.strategy.breakout_window_secs);
        let tt_vol_up = self
            .flow
            .tt_buy_volume(self.cfg.strategy.breakout_window_secs);
        let cvd_delta = self.flow.cvd_delta(self.cfg.strategy.breakout_window_secs);
        let ofi = self.flow.ofi(self.cfg.strategy.breakout_window_secs);

        // 短向破位
        if let Some(level) = depth_support {
            let price_delta = self.cfg.strategy.price_delta_ticks * self.cfg.data.tick_size;
            let price_condition = mid <= level.price - price_delta;
            let depth_condition =
                level.depth > 0.0 && tt_vol_down >= self.cfg.strategy.volume_factor * level.depth;
            let cvd_condition = self.cfg.strategy.cvd_threshold == 0.0
                || cvd_delta <= -self.cfg.strategy.cvd_threshold.abs();
            let ofi_condition = ofi <= -self.cfg.strategy.ofi_threshold.abs().max(1e-9);

            if price_condition && depth_condition && cvd_condition && ofi_condition {
                let requested_qty = if self.cfg.strategy.volume_factor == 0.0 {
                    self.execution
                        .calc_lob_qty(level.depth, self.cfg.execution.base_qty)
                } else {
                    self.execution.calc_trade_flow_qty(
                        level.depth,
                        tt_vol_down,
                        self.cfg.execution.base_qty,
                    )
                };
                if let Some((qty, entry_price, trial_budget)) =
                    self.executable_entry_from_budget(PositionSide::Short, requested_qty)
                {
                    if self
                        .execution
                        .can_enter(PositionSide::Short, qty, entry_price)
                    {
                        self.displayed_budget = trial_budget;
                        self.execution.enter_short(
                            ts_sec,
                            entry_price,
                            qty,
                            level.price,
                            level.depth,
                            &mut self.stats,
                        );
                    }
                }
            }
        }

        // 多向破位（可選）
        if let Some(level) = depth_resistance {
            let price_delta = self.cfg.strategy.price_delta_ticks * self.cfg.data.tick_size;
            let price_condition = mid >= level.price + price_delta;
            let depth_condition =
                level.depth > 0.0 && tt_vol_up >= self.cfg.strategy.volume_factor * level.depth;
            let cvd_condition = self.cfg.strategy.cvd_threshold == 0.0
                || cvd_delta >= self.cfg.strategy.cvd_threshold.abs();
            let ofi_condition = ofi >= self.cfg.strategy.ofi_threshold.abs().max(1e-9);

            if price_condition && depth_condition && cvd_condition && ofi_condition {
                let requested_qty = if self.cfg.strategy.volume_factor == 0.0 {
                    self.execution
                        .calc_lob_qty(level.depth, self.cfg.execution.base_qty)
                } else {
                    self.execution.calc_trade_flow_qty(
                        level.depth,
                        tt_vol_up,
                        self.cfg.execution.base_qty,
                    )
                };
                if let Some((qty, entry_price, trial_budget)) =
                    self.executable_entry_from_budget(PositionSide::Long, requested_qty)
                {
                    if self
                        .execution
                        .can_enter(PositionSide::Long, qty, entry_price)
                    {
                        self.displayed_budget = trial_budget;
                        self.execution.enter_long(
                            ts_sec,
                            entry_price,
                            qty,
                            level.price,
                            level.depth,
                            &mut self.stats,
                        );
                    }
                }
            }
        }

        self.execution.evaluate_exit(
            ts_sec,
            mid,
            ofi,
            cvd_delta,
            &mut self.displayed_budget,
            &mut self.stats,
        );

        Ok(())
    }

    fn finish(&mut self) -> BacktestResult {
        let trades = mem::take(&mut self.execution.trades);
        BacktestResult {
            summary: self.stats.clone_into_summary(
                self.execution.position.qty.abs(),
                self.execution.cash,
                self.execution.inventory,
                &trades,
            ),
            trades,
            input_evidence: None,
        }
    }
}

pub struct BacktestResult {
    pub trades: Vec<TradeRecord>,
    pub summary: SummaryMetrics,
    pub input_evidence: Option<BacktestInputEvidence>,
}

#[derive(Debug, Clone)]
struct LiquidityLevel {
    price: f64,
    depth: f64,
}

// ----- Order Book -----
#[derive(Default)]
struct OrderBook {
    bids: BTreeMap<OrderedFloat<f64>, f64>,
    asks: BTreeMap<OrderedFloat<f64>, f64>,
    max_levels: usize,
    last_best_bid_qty: f64,
    last_best_ask_qty: f64,
    last_ts: Option<i64>,
}

impl OrderBook {
    fn new(max_levels: usize) -> Self {
        Self {
            max_levels,
            ..Default::default()
        }
    }

    fn apply_snapshot(&mut self, ts: i64, bids: &[Level], asks: &[Level]) -> (f64, f64) {
        self.bids.clear();
        self.asks.clear();
        for level in bids {
            if level.quantity > 0.0 {
                self.bids.insert(OrderedFloat(level.price), level.quantity);
            }
        }
        for level in asks {
            if level.quantity > 0.0 {
                self.asks.insert(OrderedFloat(level.price), level.quantity);
            }
        }
        self.trim_to_max_levels();
        let delta = self.update_best_sizes();
        self.last_ts = Some(ts);
        delta
    }

    fn apply_delta(&mut self, ts: i64, bids: &[Level], asks: &[Level]) -> (f64, f64) {
        for level in bids {
            let key = OrderedFloat(level.price);
            if level.quantity <= 0.0 {
                self.bids.remove(&key);
            } else {
                self.bids.insert(key, level.quantity);
            }
        }
        for level in asks {
            let key = OrderedFloat(level.price);
            if level.quantity <= 0.0 {
                self.asks.remove(&key);
            } else {
                self.asks.insert(key, level.quantity);
            }
        }
        self.trim_to_max_levels();
        let delta = self.update_best_sizes();
        self.last_ts = Some(ts);
        delta
    }

    fn best_bid(&self) -> Option<(f64, f64)> {
        self.bids
            .iter()
            .next_back()
            .map(|(p, q)| (p.into_inner(), *q))
    }

    fn trim_to_max_levels(&mut self) {
        while self.bids.len() > self.max_levels {
            self.bids.pop_first();
        }
        while self.asks.len() > self.max_levels {
            self.asks.pop_last();
        }
    }

    fn best_ask(&self) -> Option<(f64, f64)> {
        self.asks.iter().next().map(|(p, q)| (p.into_inner(), *q))
    }

    fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some((bid, _)), Some((ask, _))) if ask >= bid => Some((bid + ask) / 2.0),
            _ => None,
        }
    }

    fn depth_level_counts(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    fn displayed_snapshot(
        &self,
        sequence: u64,
        generation: u64,
        received_at_us: u64,
    ) -> DisplayedBookSnapshot {
        DisplayedBookSnapshot::new(
            sequence,
            generation,
            received_at_us,
            self.bids
                .iter()
                .rev()
                .take(self.max_levels)
                .map(|(price, quantity)| {
                    DisplayedBookLevel::new(
                        Price::from_f64(price.into_inner()).unwrap_or(Price::zero()),
                        Quantity::from_f64(*quantity).unwrap_or(Quantity::zero()),
                    )
                })
                .collect(),
            self.asks
                .iter()
                .take(self.max_levels)
                .map(|(price, quantity)| {
                    DisplayedBookLevel::new(
                        Price::from_f64(price.into_inner()).unwrap_or(Price::zero()),
                        Quantity::from_f64(*quantity).unwrap_or(Quantity::zero()),
                    )
                })
                .collect(),
        )
    }

    #[cfg(test)]
    fn executable_exit(
        &self,
        position_side: PositionSide,
        requested_qty: f64,
        max_fill_ratio: f64,
        max_slippage_ticks: f64,
        tick_size: f64,
    ) -> Option<(f64, f64)> {
        if requested_qty <= 0.0
            || !max_fill_ratio.is_finite()
            || max_fill_ratio <= 0.0
            || !max_slippage_ticks.is_finite()
            || max_slippage_ticks < 0.0
            || !tick_size.is_finite()
            || tick_size <= 0.0
        {
            return None;
        }
        let order_side = match position_side {
            PositionSide::Long => Side::Sell,
            PositionSide::Short => Side::Buy,
        };
        let best = match order_side {
            Side::Sell => self.best_bid()?.0,
            Side::Buy => self.best_ask()?.0,
        };
        let best_f64 = best;
        let bound = match order_side {
            Side::Sell => Price::from_f64(best_f64 - max_slippage_ticks * tick_size).ok()?,
            Side::Buy => Price::from_f64(best_f64 + max_slippage_ticks * tick_size).ok()?,
        };
        let mut budget = BookBudget::default();
        if !budget.observe(&self.displayed_snapshot(1, 1, 1), 1) {
            return None;
        }
        let participation = Quantity::from_f64(max_fill_ratio.clamp(0.0, 1.0)).ok()?.0;
        let fills = budget.fills_with_price_bound_and_participation(
            order_side,
            OrderType::Market,
            None,
            Quantity::from_f64(requested_qty).ok()?,
            Some(bound),
            participation,
        );
        aggregate_displayed_fills(&fills)
    }

    #[cfg(test)]
    fn executable_entry(
        &self,
        position_side: PositionSide,
        requested_qty: f64,
        max_fill_ratio: f64,
        max_slippage_ticks: f64,
        tick_size: f64,
    ) -> Option<(f64, f64)> {
        if requested_qty <= 0.0
            || !max_fill_ratio.is_finite()
            || max_fill_ratio <= 0.0
            || !max_slippage_ticks.is_finite()
            || max_slippage_ticks < 0.0
            || !tick_size.is_finite()
            || tick_size <= 0.0
        {
            return None;
        }
        let order_side = match position_side {
            PositionSide::Long => Side::Buy,
            PositionSide::Short => Side::Sell,
        };
        let best = match order_side {
            Side::Buy => self.best_ask()?.0,
            Side::Sell => self.best_bid()?.0,
        };
        let bound = match order_side {
            Side::Buy => Price::from_f64(best + max_slippage_ticks * tick_size).ok()?,
            Side::Sell => Price::from_f64(best - max_slippage_ticks * tick_size).ok()?,
        };
        let mut budget = BookBudget::default();
        if !budget.observe(&self.displayed_snapshot(1, 1, 1), 1) {
            return None;
        }
        let participation = Quantity::from_f64(max_fill_ratio.clamp(0.0, 1.0)).ok()?.0;
        let fills = budget.fills_with_price_bound_and_participation(
            order_side,
            OrderType::Market,
            None,
            Quantity::from_f64(requested_qty).ok()?,
            Some(bound),
            participation,
        );
        aggregate_displayed_fills(&fills)
    }

    fn snapshot(&self, max_levels: usize) -> DepthSnapshot {
        let bids = self
            .bids
            .iter()
            .rev()
            .take(max_levels)
            .map(|(p, q)| Level {
                price: p.into_inner(),
                quantity: *q,
            })
            .collect_vec();
        let asks = self
            .asks
            .iter()
            .take(max_levels)
            .map(|(p, q)| Level {
                price: p.into_inner(),
                quantity: *q,
            })
            .collect_vec();
        DepthSnapshot { bids, asks }
    }

    fn update_best_sizes(&mut self) -> (f64, f64) {
        let bid_qty = self.best_bid().map(|(_, q)| q).unwrap_or(0.0);
        let ask_qty = self.best_ask().map(|(_, q)| q).unwrap_or(0.0);
        let delta_bid = bid_qty - self.last_best_bid_qty;
        let delta_ask = ask_qty - self.last_best_ask_qty;
        self.last_best_bid_qty = bid_qty;
        self.last_best_ask_qty = ask_qty;
        (delta_bid, delta_ask)
    }
}

#[derive(Clone)]
struct DepthSnapshot {
    bids: Vec<Level>,
    asks: Vec<Level>,
}

fn aggregate_displayed_fills(fills: &[hft_core::DisplayedFill]) -> Option<(f64, f64)> {
    let mut quantity = 0.0;
    let mut notional = 0.0;
    for fill in fills {
        let price = fill.price.to_f64()?;
        let fill_quantity = fill.quantity.to_f64()?;
        quantity += fill_quantity;
        notional += price * fill_quantity;
    }
    (quantity > 0.0).then_some((quantity, notional / quantity))
}

// ----- Liquidity Map -----
struct LiquidityMap {
    cfg: StrategyConfig,
    tick_size: f64,
    max_levels: usize,
    total_duration: f64,
    contributions: VecDeque<Contribution>,
    bid_stats: HashMap<OrderedFloat<f64>, LevelStat>,
    ask_stats: HashMap<OrderedFloat<f64>, LevelStat>,
    last_snapshot: Option<DepthSnapshot>,
    last_ts: Option<i64>,
}

#[derive(Clone)]
struct Contribution {
    duration: f64,
    bids: Vec<Level>,
    asks: Vec<Level>,
}

#[derive(Default)]
struct LevelStat {
    weighted_qty: f64,
    duration: f64,
}

impl LiquidityMap {
    fn new(cfg: StrategyConfig, tick_size: f64, max_levels: usize) -> Self {
        Self {
            cfg,
            tick_size,
            max_levels,
            total_duration: 0.0,
            contributions: VecDeque::new(),
            bid_stats: HashMap::new(),
            ask_stats: HashMap::new(),
            last_snapshot: None,
            last_ts: None,
        }
    }

    fn update(&mut self, ts: i64, snapshot: &DepthSnapshot) {
        if let (Some(prev), Some(prev_ts)) = (&self.last_snapshot, self.last_ts) {
            let dt = ((ts - prev_ts) as f64 / MICROS_IN_SECOND).max(0.0);
            if dt > 0.0 {
                let contrib = Contribution {
                    duration: dt,
                    bids: prev.bids.clone(),
                    asks: prev.asks.clone(),
                };
                self.add_contribution(&contrib);
                self.contributions.push_back(contrib);
                self.total_duration += dt;
                self.trim();
            }
        }
        self.last_snapshot = Some(snapshot.clone());
        self.last_ts = Some(ts);
    }

    fn add_contribution(&mut self, contrib: &Contribution) {
        for level in contrib.bids.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            let stat = self.bid_stats.entry(key).or_default();
            stat.weighted_qty += level.quantity * contrib.duration;
            stat.duration += contrib.duration;
        }
        for level in contrib.asks.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            let stat = self.ask_stats.entry(key).or_default();
            stat.weighted_qty += level.quantity * contrib.duration;
            stat.duration += contrib.duration;
        }
    }

    fn remove_contribution(&mut self, contrib: &Contribution, portion: f64) {
        if portion <= 0.0 {
            return;
        }
        for level in contrib.bids.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            if let Some(stat) = self.bid_stats.get_mut(&key) {
                stat.weighted_qty -= level.quantity * portion;
                stat.duration -= portion;
                if stat.weighted_qty <= 1e-9 || stat.duration <= 1e-9 {
                    self.bid_stats.remove(&key);
                }
            }
        }
        for level in contrib.asks.iter().take(self.max_levels) {
            if level.quantity <= 0.0 {
                continue;
            }
            let key = OrderedFloat(self.round_price(level.price));
            if let Some(stat) = self.ask_stats.get_mut(&key) {
                stat.weighted_qty -= level.quantity * portion;
                stat.duration -= portion;
                if stat.weighted_qty <= 1e-9 || stat.duration <= 1e-9 {
                    self.ask_stats.remove(&key);
                }
            }
        }
    }

    fn trim(&mut self) {
        let window = self.cfg.liquidity_window_secs.max(1.0);
        while self.total_duration > window && !self.contributions.is_empty() {
            let overflow = self.total_duration - window;
            if let Some(front) = self.contributions.front() {
                if front.duration <= overflow + 1e-9 {
                    let front = self.contributions.pop_front().unwrap();
                    self.remove_contribution(&front, front.duration);
                    self.total_duration -= front.duration;
                } else {
                    let mut partial = front.clone();
                    partial.duration = overflow;
                    self.remove_contribution(&partial, overflow);
                    if let Some(front_mut) = self.contributions.front_mut() {
                        front_mut.duration -= overflow;
                    }
                    self.total_duration -= overflow;
                }
            }
        }
    }

    fn round_price(&self, price: f64) -> f64 {
        let steps = (price / self.tick_size).round();
        steps * self.tick_size
    }

    fn support_levels(&self, mid: f64, count: usize) -> Vec<LiquidityLevel> {
        let window = self
            .cfg
            .liquidity_window_secs
            .max(self.total_duration)
            .max(1.0);
        let alpha = self.cfg.smoothing_alpha.clamp(0.0, 1.0);
        let mut levels = self
            .bid_stats
            .iter()
            .filter_map(|(price, stat)| {
                let p = price.into_inner();
                if p >= mid && stat.duration > 0.0 {
                    let avg = stat.weighted_qty / window;
                    let inst = if stat.duration > 0.0 {
                        stat.weighted_qty / stat.duration.max(1e-9)
                    } else {
                        avg
                    };
                    let depth = alpha * avg + (1.0 - alpha) * inst;
                    if depth > 0.0 {
                        return Some(LiquidityLevel { price: p, depth });
                    }
                }
                None
            })
            .collect_vec();
        levels.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        levels.truncate(count);
        levels
    }

    fn resistance_levels(&self, mid: f64, count: usize) -> Vec<LiquidityLevel> {
        let window = self
            .cfg
            .liquidity_window_secs
            .max(self.total_duration)
            .max(1.0);
        let alpha = self.cfg.smoothing_alpha.clamp(0.0, 1.0);
        let mut levels = self
            .ask_stats
            .iter()
            .filter_map(|(price, stat)| {
                let p = price.into_inner();
                if p <= mid && stat.duration > 0.0 {
                    let avg = stat.weighted_qty / window;
                    let inst = if stat.duration > 0.0 {
                        stat.weighted_qty / stat.duration.max(1e-9)
                    } else {
                        avg
                    };
                    let depth = alpha * avg + (1.0 - alpha) * inst;
                    if depth > 0.0 {
                        return Some(LiquidityLevel { price: p, depth });
                    }
                }
                None
            })
            .collect_vec();
        levels.sort_by(|a, b| {
            b.depth
                .partial_cmp(&a.depth)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        levels.truncate(count);
        levels
    }
}

// ----- Flow Tracker -----
struct FlowTracker {
    trades: VecDeque<TradeSample>,
    ofi: VecDeque<OfiSample>,
    cvd: VecDeque<CvdSample>,
    cvd_value: f64,
    sell_volume: f64,
    buy_volume: f64,
}

#[derive(Clone)]
struct TradeSample {
    ts: f64,
    side: TradeSide,
    quantity: f64,
}

#[derive(Clone)]
struct OfiSample {
    ts: f64,
    value: f64,
}

#[derive(Clone)]
struct CvdSample {
    ts: f64,
    value: f64,
}

impl FlowTracker {
    fn new() -> Self {
        Self {
            trades: VecDeque::new(),
            ofi: VecDeque::new(),
            cvd: VecDeque::new(),
            cvd_value: 0.0,
            sell_volume: 0.0,
            buy_volume: 0.0,
        }
    }

    fn update_trade(&mut self, ts: f64, side: TradeSide, qty: f64) {
        let sample = TradeSample {
            ts,
            side,
            quantity: qty.max(0.0),
        };
        match side {
            TradeSide::Buy => self.buy_volume += sample.quantity,
            TradeSide::Sell => self.sell_volume += sample.quantity,
        }
        self.trades.push_back(sample);

        match side {
            TradeSide::Buy => self.cvd_value += qty,
            TradeSide::Sell => self.cvd_value -= qty,
        }
        self.cvd.push_back(CvdSample {
            ts,
            value: self.cvd_value,
        });
    }

    fn update_ofi(&mut self, ts: f64, delta: (f64, f64)) {
        let value = delta.0 - delta.1;
        if value.abs() < 1e-9 {
            return;
        }
        self.ofi.push_back(OfiSample { ts, value });
    }

    fn tt_sell_volume(&mut self, window: f64) -> f64 {
        self.prune(window);
        self.sell_volume
    }

    fn tt_buy_volume(&mut self, window: f64) -> f64 {
        self.prune(window);
        self.buy_volume
    }

    fn ofi(&mut self, window: f64) -> f64 {
        self.prune(window);
        self.ofi.iter().map(|s| s.value).sum()
    }

    fn cvd_delta(&mut self, window: f64) -> f64 {
        self.prune(window);
        if let Some(first) = self.cvd.front() {
            self.cvd_value - first.value
        } else {
            0.0
        }
    }

    fn prune(&mut self, window: f64) {
        if window <= 0.0 {
            return;
        }
        let cutoff = match self
            .trades
            .back()
            .map(|s| s.ts)
            .or_else(|| self.ofi.back().map(|s| s.ts))
            .or_else(|| self.cvd.back().map(|s| s.ts))
        {
            Some(ts) => ts - window,
            None => return,
        };

        while let Some(sample) = self.trades.front() {
            if sample.ts >= cutoff {
                break;
            }
            let sample = self.trades.pop_front().unwrap();
            match sample.side {
                TradeSide::Buy => self.buy_volume -= sample.quantity,
                TradeSide::Sell => self.sell_volume -= sample.quantity,
            }
        }

        while let Some(sample) = self.ofi.front() {
            if sample.ts >= cutoff {
                break;
            }
            self.ofi.pop_front();
        }

        while let Some(sample) = self.cvd.front() {
            if sample.ts >= cutoff {
                break;
            }
            self.cvd.pop_front();
        }
    }
}

// ----- Execution -----
struct ExecutionManager {
    cfg: ExecutionConfig,
    risk: RiskConfig,
    market: data::binance_lob_replay::Market,
    tick_size: f64,
    position: PositionState,
    cash: f64,
    inventory: f64,
    trades: Vec<TradeRecord>,
    pnl: f64,
    equity_curve: Vec<(f64, f64)>,
    consecutive_losses: usize,
    disabled: bool,
}

#[derive(Default)]
struct PositionState {
    side: Option<PositionSide>,
    qty: f64,
    entry_price: f64,
    entry_ts: f64,
    reference_level: f64,
    reference_depth: f64,
    entry_fee: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    Long,
    Short,
}

#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub entry_ts: f64,
    pub exit_ts: f64,
    pub side: PositionSide,
    pub qty: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub pnl: f64,
    pub gross_pnl: f64,
    pub fees: f64,
    pub reason: ExitReason,
    pub reference_level: f64,
    pub reference_depth: f64,
}

#[derive(Debug, Clone, Copy)]
pub enum ExitReason {
    PriceReversion,
    StopLoss,
    TakeProfit,
    HoldTimeout,
    SessionEnd,
    RiskStop,
}

impl PositionState {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

impl ExecutionManager {
    fn new(
        cfg: ExecutionConfig,
        risk: RiskConfig,
        tick_size: f64,
        market: data::binance_lob_replay::Market,
    ) -> Self {
        let initial_cash = cfg.initial_cash;
        let initial_inventory = cfg.initial_inventory;
        Self {
            cfg,
            risk,
            market,
            tick_size,
            position: PositionState::default(),
            cash: initial_cash,
            inventory: initial_inventory,
            trades: Vec::new(),
            pnl: 0.0,
            equity_curve: Vec::new(),
            consecutive_losses: 0,
            disabled: false,
        }
    }

    fn has_position(&self) -> bool {
        self.position.side.is_some()
    }

    fn can_enter(&self, side: PositionSide, qty: f64, price: f64) -> bool {
        if self.disabled {
            return false;
        }
        if self.market == data::binance_lob_replay::Market::Spot && side == PositionSide::Short {
            return false;
        }
        let projected = self.position.qty.abs() + qty.abs();
        if projected > self.risk.inventory_limit + 1e-9 {
            return false;
        }
        if self.market == data::binance_lob_replay::Market::Spot {
            let fee = price * qty * self.cfg.fee_bps.max(0.0) / BPS;
            self.cash >= price * qty + fee - 1e-9
                && self.inventory + qty <= self.risk.inventory_limit + 1e-9
        } else {
            true
        }
    }

    fn cap_qty(&self, requested: f64, depth: f64) -> f64 {
        if !requested.is_finite()
            || requested <= 0.0
            || !depth.is_finite()
            || depth <= 0.0
            || !self.cfg.max_position.is_finite()
            || self.cfg.max_position <= 0.0
        {
            return 0.0;
        }
        requested
            .min(self.cfg.max_position)
            .min(depth * self.cfg.max_fill_ratio.clamp(0.0, 1.0))
            .max(0.0)
    }

    fn calc_lob_qty(&self, depth: f64, base_qty: f64) -> f64 {
        self.cap_qty(base_qty, depth)
    }

    fn calc_trade_flow_qty(&self, depth: f64, vol: f64, base_qty: f64) -> f64 {
        if depth <= 0.0 || base_qty <= 0.0 {
            return 0.0;
        }
        let ratio = (vol / depth).min(self.cfg.max_position / base_qty);
        self.cap_qty(base_qty * ratio, depth)
    }

    #[cfg(test)]
    fn executable_exit(&self, order_book: &OrderBook) -> Option<(f64, f64)> {
        order_book.executable_exit(
            self.position.side?,
            self.position.qty,
            self.cfg.max_fill_ratio,
            self.risk.slippage_limit_ticks,
            self.tick_size,
        )
    }

    fn executable_exit_from_budget(&self, budget: &mut BookBudget) -> Option<(f64, f64)> {
        let position_side = self.position.side?;
        let order_side = match position_side {
            PositionSide::Long => Side::Sell,
            PositionSide::Short => Side::Buy,
        };
        let best = match order_side {
            Side::Sell => budget.best_bid()?.0.to_f64()?,
            Side::Buy => budget.best_ask()?.0.to_f64()?,
        };
        let slippage = self.risk.slippage_limit_ticks * self.tick_size;
        let bound = match order_side {
            Side::Sell => Price::from_f64(best - slippage).ok()?,
            Side::Buy => Price::from_f64(best + slippage).ok()?,
        };
        let fills = budget.fills_with_price_bound_and_participation(
            order_side,
            OrderType::Market,
            None,
            Quantity::from_f64(self.position.qty).ok()?,
            Some(bound),
            Quantity::from_f64(self.cfg.max_fill_ratio.clamp(0.0, 1.0))
                .ok()?
                .0,
        );
        aggregate_displayed_fills(&fills)
    }

    fn enter_short(
        &mut self,
        ts: f64,
        price: f64,
        qty: f64,
        reference_level: f64,
        reference_depth: f64,
        stats: &mut BacktestStats,
    ) {
        if qty <= 0.0 || self.market == data::binance_lob_replay::Market::Spot {
            return;
        }
        if self.position.side == Some(PositionSide::Short) {
            // 累加倉位
            let total_qty = self.position.qty + qty;
            let new_entry =
                (self.position.entry_price * self.position.qty + price * qty) / total_qty;
            self.position.qty = total_qty;
            self.position.entry_price = new_entry;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        } else {
            self.position.side = Some(PositionSide::Short);
            self.position.qty = qty;
            self.position.entry_price = price;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        }
        stats.max_position = stats.max_position.max(self.position.qty.abs());
        debug!(
            "enter short qty={:.4} price={:.4} ref={:.4}",
            qty, price, reference_level
        );
    }

    fn enter_long(
        &mut self,
        ts: f64,
        price: f64,
        qty: f64,
        reference_level: f64,
        reference_depth: f64,
        stats: &mut BacktestStats,
    ) {
        if qty <= 0.0 {
            return;
        }
        let entry_fee = if self.market == data::binance_lob_replay::Market::Spot {
            price * qty * self.cfg.fee_bps.max(0.0) / BPS
        } else {
            0.0
        };
        if self.market == data::binance_lob_replay::Market::Spot {
            let required_cash = price * qty + entry_fee;
            if self.cash + 1e-9 < required_cash {
                return;
            }
            self.cash -= required_cash;
            self.inventory += qty;
        }
        if self.position.side == Some(PositionSide::Long) {
            let total_qty = self.position.qty + qty;
            let new_entry =
                (self.position.entry_price * self.position.qty + price * qty) / total_qty;
            self.position.qty = total_qty;
            self.position.entry_price = new_entry;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
            self.position.entry_fee += entry_fee;
        } else {
            self.position.side = Some(PositionSide::Long);
            self.position.qty = qty;
            self.position.entry_price = price;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
            self.position.entry_fee = entry_fee;
        }
        stats.max_position = stats.max_position.max(self.position.qty.abs());
        debug!(
            "enter long qty={:.4} price={:.4} ref={:.4}",
            qty, price, reference_level
        );
    }

    fn evaluate_exit(
        &mut self,
        ts: f64,
        mid: f64,
        ofi: f64,
        cvd_delta: f64,
        displayed_budget: &mut BookBudget,
        stats: &mut BacktestStats,
    ) {
        if self.position.side.is_none() {
            return;
        }
        let elapsed = ts - self.position.entry_ts;
        let stop_loss_price = match self.position.side {
            Some(PositionSide::Short) => {
                self.position.entry_price + self.cfg.stop_loss_ticks * self.tick_size
            }
            Some(PositionSide::Long) => {
                self.position.entry_price - self.cfg.stop_loss_ticks * self.tick_size
            }
            None => 0.0,
        };
        let take_profit_price = match self.position.side {
            Some(PositionSide::Short) => {
                self.position.entry_price - self.cfg.take_profit_ticks * self.tick_size
            }
            Some(PositionSide::Long) => {
                self.position.entry_price + self.cfg.take_profit_ticks * self.tick_size
            }
            None => 0.0,
        };

        let (stop_hit, tp_hit, reversion) = match self.position.side {
            Some(PositionSide::Short) => (
                mid >= stop_loss_price,
                mid <= take_profit_price,
                mid >= self.position.reference_level || (cvd_delta >= 0.0 && ofi >= 0.0),
            ),
            Some(PositionSide::Long) => (
                mid <= stop_loss_price,
                mid >= take_profit_price,
                mid <= self.position.reference_level || (cvd_delta <= 0.0 && ofi <= 0.0),
            ),
            None => (false, false, false),
        };

        let hold_timeout = self
            .cfg
            .hold_secs
            .map(|hold| elapsed >= hold)
            .unwrap_or(false);

        let reason = if stop_hit {
            Some(ExitReason::StopLoss)
        } else if tp_hit {
            Some(ExitReason::TakeProfit)
        } else if hold_timeout {
            Some(ExitReason::HoldTimeout)
        } else if reversion {
            Some(ExitReason::PriceReversion)
        } else {
            None
        };

        if let Some(reason) = reason {
            if let Some((fill_qty, fill_price)) = self.executable_exit_from_budget(displayed_budget)
            {
                self.exit_position(ts, fill_price, fill_qty, reason, stats);
            }
        }
    }

    fn exit_position(
        &mut self,
        ts: f64,
        price: f64,
        fill_qty: f64,
        reason: ExitReason,
        stats: &mut BacktestStats,
    ) {
        if self.position.side.is_none() || self.position.qty == 0.0 {
            return;
        }
        let qty = fill_qty.min(self.position.qty).max(0.0);
        if qty <= 0.0 {
            return;
        }
        let entry_price = self.position.entry_price;
        let side = self.position.side.unwrap();
        let entry_fee_for_exit = if self.market == data::binance_lob_replay::Market::Spot {
            self.position.entry_fee * (qty / self.position.qty)
        } else {
            0.0
        };
        let gross_pnl = match side {
            PositionSide::Short => (entry_price - price) * qty,
            PositionSide::Long => (price - entry_price) * qty,
        };
        let exit_fee = price * qty.abs() * self.cfg.fee_bps.max(0.0) / BPS;
        let fees = if self.market == data::binance_lob_replay::Market::Spot {
            if side != PositionSide::Long || self.inventory + 1e-9 < qty {
                return;
            }
            self.inventory -= qty;
            self.cash += price * qty - exit_fee;
            self.position.entry_fee -= entry_fee_for_exit;
            entry_fee_for_exit + exit_fee
        } else {
            (entry_price.abs() + price.abs()) * qty.abs() * self.cfg.fee_bps.max(0.0) / BPS
        };
        let pnl = gross_pnl - fees;
        let turnover = (entry_price.abs() + price.abs()) * qty.abs();
        self.pnl += pnl;
        self.equity_curve.push((ts, self.pnl));
        stats.update(pnl, gross_pnl, fees, turnover, self.pnl);

        if pnl < 0.0 {
            self.consecutive_losses += 1;
            if self.consecutive_losses >= self.risk.max_consecutive_losses {
                self.disabled = true;
                warn!(
                    "停機：連續虧損達 {} 筆，停止進場",
                    self.risk.max_consecutive_losses
                );
            }
        } else {
            self.consecutive_losses = 0;
        }

        let mut exit_reason = reason;
        if let Some(limit) = self.risk.daily_loss_limit {
            if self.pnl <= -limit {
                self.disabled = true;
                exit_reason = ExitReason::RiskStop;
                warn!("停機：日損達 {:.4}，停止進場", limit);
            }
        }

        self.trades.push(TradeRecord {
            entry_ts: self.position.entry_ts,
            exit_ts: ts,
            side,
            qty,
            entry_price,
            exit_price: price,
            pnl,
            gross_pnl,
            fees,
            reason: exit_reason,
            reference_level: self.position.reference_level,
            reference_depth: self.position.reference_depth,
        });

        self.position.qty -= qty;
        if self.position.qty <= 1e-12 {
            self.position.reset();
        }
    }
}

// ----- Backtest Stats -----
#[derive(Default)]
struct BacktestStats {
    pub total_pnl: f64,
    pub gross_pnl: f64,
    pub total_fees: f64,
    pub turnover: f64,
    pub wins: usize,
    pub losses: usize,
    pub max_drawdown: f64,
    pub peak_equity: f64,
    pub max_position: f64,
    pub last_trade_price: Option<f64>,
}

impl BacktestStats {
    fn update(&mut self, pnl: f64, gross_pnl: f64, fees: f64, turnover: f64, equity: f64) {
        self.total_pnl += pnl;
        self.gross_pnl += gross_pnl;
        self.total_fees += fees;
        self.turnover += turnover;
        if pnl >= 0.0 {
            self.wins += 1;
        } else {
            self.losses += 1;
        }
        self.peak_equity = self.peak_equity.max(equity);
        let drawdown = self.peak_equity - equity;
        self.max_drawdown = self.max_drawdown.max(drawdown);
    }

    fn clone_into_summary(
        &self,
        open_position_qty: f64,
        ending_cash: f64,
        ending_inventory: f64,
        trades: &[TradeRecord],
    ) -> SummaryMetrics {
        let total_trades = self.wins + self.losses;
        let win_rate = if total_trades > 0 {
            self.wins as f64 / total_trades as f64
        } else {
            0.0
        };
        SummaryMetrics {
            total_pnl: self.total_pnl,
            gross_pnl: self.gross_pnl,
            total_fees: self.total_fees,
            turnover: self.turnover,
            trades: total_trades,
            win_rate,
            max_drawdown: self.max_drawdown,
            max_position: self.max_position,
            open_position_qty,
            ending_cash,
            ending_inventory,
            net_sharpe: per_trade_net_sharpe(trades),
        }
    }
}

// Per-trade Sharpe over net pnl: mean / population standard deviation with an
// epsilon floor. Unannualized, matching the alpha-harness per-observation
// Sharpe convention (annualize only after the period frequency is explicit).
fn per_trade_net_sharpe(trades: &[TradeRecord]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }
    let average = trades.iter().map(|trade| trade.pnl).sum::<f64>() / trades.len() as f64;
    let deviation = (trades
        .iter()
        .map(|trade| (trade.pnl - average).powi(2))
        .sum::<f64>()
        / trades.len() as f64)
        .sqrt();
    average / deviation.max(f64::EPSILON)
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryMetrics {
    pub total_pnl: f64,
    pub gross_pnl: f64,
    pub total_fees: f64,
    pub turnover: f64,
    pub trades: usize,
    pub win_rate: f64,
    pub max_drawdown: f64,
    pub max_position: f64,
    pub open_position_qty: f64,
    pub ending_cash: f64,
    pub ending_inventory: f64,
    pub net_sharpe: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        BacktestConfig, DataConfig, ExecutionConfig, OutputConfig, RiskConfig, StrategyConfig,
    };

    fn test_config() -> BacktestConfig {
        BacktestConfig {
            data: DataConfig {
                path: "unused.ndjson".to_string(),
                market: "usdm".to_string(),
                format: "ndjson".to_string(),
                tick_size: 0.01,
                lot_size: 0.01,
                max_depth_levels: 5,
                manifest_path: None,
                manifest_sha256: None,
                require_sequence: false,
                start_ts: None,
                end_ts: None,
            },
            strategy: StrategyConfig {
                liquidity_window_secs: 1.0,
                breakout_window_secs: 1.0,
                price_delta_ticks: 1.0,
                volume_factor: 1.0,
                cvd_threshold: 0.0,
                ofi_threshold: 0.0,
                support_count: 1,
                resistance_count: 1,
                smoothing_alpha: 0.2,
            },
            execution: ExecutionConfig::default(),
            risk: RiskConfig::default(),
            output: OutputConfig::default(),
        }
    }

    fn spot_replay_rules() -> CexSpotInstrumentRulesV1 {
        CexSpotInstrumentRulesV1 {
            schema: "binance.spot_reference.v1".to_string(),
            venue: "binance".to_string(),
            market: "spot".to_string(),
            symbol: "BTCUSDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            status: "TRADING".to_string(),
            is_spot_trading_allowed: true,
            base_asset_precision: 8,
            quote_asset_precision: 8,
            price_filter: hft_research_manifest::CexSpotPriceFilterV1 {
                min_price: "0".to_string(),
                max_price: "0".to_string(),
                tick_size: "0.1".to_string(),
            },
            lot_size_filter: hft_research_manifest::CexSpotQuantityFilterV1 {
                min_quantity: "0.001".to_string(),
                max_quantity: "100".to_string(),
                step_size: "0.001".to_string(),
            },
            market_lot_size_filter: Some(hft_research_manifest::CexSpotQuantityFilterV1 {
                min_quantity: "0.001".to_string(),
                max_quantity: "100".to_string(),
                step_size: "0.001".to_string(),
            }),
            notional_filter: hft_research_manifest::CexSpotNotionalFilterV1 {
                filter_type: "MIN_NOTIONAL".to_string(),
                min_notional: "5".to_string(),
                max_notional: None,
                apply_min_to_market: false,
                apply_max_to_market: None,
                avg_price_mins: 5,
            },
            source_time_ms: 1,
            source_clock_received_at_ns: 1_000_000,
            received_at_ns: 1_000_000,
            source_endpoint: "/api/v3/exchangeInfo".to_string(),
            source_clock_endpoint: "/api/v3/time".to_string(),
        }
    }

    #[test]
    fn target_position_replay_is_deterministic_and_snapshot_gated() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[\"100\",\"10\"],[\"99\",\"10\"]],\"asks\":[[\"101\",\"10\"],[\"102\",\"10\"]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[\"100\",\"9\"],[\"98\",\"7\"]],\"asks\":[[\"101\",\"11\"],[\"103\",\"7\"]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[\"100\",\"8\"]],\"asks\":[[\"101\",\"12\"]]}\n",
        );
        let decisions = vec![
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: -1.0,
            },
            TargetPositionDecision {
                timestamp_us: 3_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            additional_slippage_bps: 0.25,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let first = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();
        let second = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.snapshot_events, 1);
        assert_eq!(first.l2_update_events, 2);
        assert_eq!(first.trade_events, 0);
        assert_eq!(first.decision_count, 3);
        assert_eq!(first.max_bid_depth_levels, 1);
        assert_eq!(first.max_ask_depth_levels, 1);
        let unseeded = "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"l2_update\",\"bids\":[[100,1]],\"asks\":[[101,1]]}\n";
        assert!(
            replay_target_positions(unseeded.as_bytes(), &decisions[..1], &config)
                .unwrap_err()
                .to_string()
                .contains("before a snapshot")
        );
    }

    #[test]
    fn backtest_engine_rejects_unknown_market_without_panicking() {
        let mut config = test_config();
        config.data.market = "typo-market".to_string();
        assert!(BacktestEngine::new(config).is_err());
    }

    #[test]
    fn spot_target_position_replay_keeps_cash_nonnegative_across_multiple_fills() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,3],[102,3],[103,3]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,10]],\"asks\":[[101,3],[102,3],[103,3]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,10]],\"asks\":[[101,3],[102,3],[103,3]]}\n",
        );
        let decisions = vec![
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 0.5,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "spot".to_string(),
            max_depth_levels: 3,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 1_000.0,
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            additional_slippage_bps: 0.25,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let rules = spot_replay_rules();
        let metrics = replay_target_positions_with_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap();

        assert!(metrics.final_cash >= -1e-9);
        assert!(metrics.final_inventory.abs() <= 1e-9);
        assert!(metrics.total_fees > 0.0);
        assert!(metrics.total_execution_cost > 0.0);
    }

    #[test]
    fn spot_cash_rejection_does_not_consume_displayed_liquidity() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 0.5,
            },
            TargetPositionDecision {
                timestamp_us: 3_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "spot".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let rules = spot_replay_rules();
        let output = replay_target_positions_with_trace_and_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .expect("Spot replay with a rejected then affordable buy");
        let trace = tape_trace_events(&output.trace_bytes);

        assert_eq!(trace.len(), 3);
        assert_eq!(trace[0].status, "cancelled_insufficient_cash");
        assert!(trace[0].fills.is_empty());
        assert_eq!(trace[1].status, "filled");
        assert_eq!(trace[1].fills.len(), 1);
        assert!((trace[1].fills[0].quantity - 0.5).abs() < 1e-12);
        assert_eq!(trace[2].status, "filled");
        assert_eq!(trace[2].fills.len(), 1);
        assert!((trace[2].fills[0].quantity - 0.5).abs() < 1e-12);
        assert_eq!(output.metrics.canceled_order_count, 1);
        assert_eq!(output.metrics.filled_order_count, 2);
        assert_eq!(output.metrics.final_inventory, 0.0);
    }

    #[test]
    fn spot_replay_applies_market_lot_size_and_market_notional_flags() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":1500000,\"sequence\":2,\"event\":\"trade\",\"side\":\"buy\",\"price\":100,\"quantity\":1}\n",
            "{\"timestamp\":2000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":3000000,\"sequence\":4,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_500_000,
                target_position: 0.5,
            },
            TargetPositionDecision {
                timestamp_us: 2_500_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "spot".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: true,
        };

        let mut rules = spot_replay_rules();
        rules.market_lot_size_filter.as_mut().unwrap().step_size = "1".to_string();
        let output = replay_target_positions_with_trace_and_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap();
        let trace = tape_trace_events(&output.trace_bytes);
        assert_eq!(trace[0].status, "cancelled_invalid_instrument_rules");
        assert!(trace[0].fills.is_empty());

        rules.market_lot_size_filter.as_mut().unwrap().step_size = "0.001".to_string();
        rules.notional_filter = hft_research_manifest::CexSpotNotionalFilterV1 {
            filter_type: "NOTIONAL".to_string(),
            min_notional: "200".to_string(),
            max_notional: Some("1000".to_string()),
            apply_min_to_market: false,
            apply_max_to_market: Some(true),
            avg_price_mins: 0,
        };
        let output = replay_target_positions_with_trace_and_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap();
        let trace = tape_trace_events(&output.trace_bytes);
        assert_eq!(trace[0].status, "filled");
        assert_eq!(trace[1].status, "filled");

        rules.notional_filter = hft_research_manifest::CexSpotNotionalFilterV1 {
            filter_type: "NOTIONAL".to_string(),
            min_notional: "5".to_string(),
            max_notional: Some("10".to_string()),
            apply_min_to_market: true,
            apply_max_to_market: Some(false),
            avg_price_mins: 0,
        };
        let output = replay_target_positions_with_trace_and_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap();
        let trace = tape_trace_events(&output.trace_bytes);
        assert_eq!(trace[0].status, "filled");
        assert_eq!(trace[1].status, "filled");

        rules.notional_filter.avg_price_mins = 5;
        let error = replay_target_positions_with_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("average-price evidence is unsupported"));
    }

    #[test]
    fn spot_submission_quantity_is_floored_to_common_step_and_precision() {
        let mut rules = spot_replay_rules();
        rules.lot_size_filter.step_size = "0.001".to_string();
        rules.market_lot_size_filter.as_mut().unwrap().step_size = "0.002".to_string();

        let quantity = spot_submission_quantity(&rules, 0.0055).unwrap();

        assert!((quantity - 0.004).abs() < 1e-12);
        assert!(spot_quantity_matches(&rules.lot_size_filter, quantity));
        assert!(spot_quantity_matches(
            rules.market_lot_size_filter.as_ref().unwrap(),
            quantity
        ));
    }

    #[test]
    fn spot_submission_quantity_rounds_binary_float_before_flooring() {
        let mut rules = spot_replay_rules();
        rules.lot_size_filter.step_size = "0.1".to_string();
        rules.market_lot_size_filter.as_mut().unwrap().step_size = "0.1".to_string();

        let quantity = spot_submission_quantity(&rules, 0.3).unwrap();

        assert!((quantity - 0.3).abs() < 1e-12);
    }

    #[test]
    fn spot_buy_then_flatten_preserves_a_decimal_step_quantity() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 0.3,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "spot".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };
        let mut rules = spot_replay_rules();
        rules.lot_size_filter.step_size = "0.1".to_string();
        rules.market_lot_size_filter.as_mut().unwrap().step_size = "0.1".to_string();

        let output = replay_target_positions_with_trace_and_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap();
        let trace = tape_trace_events(&output.trace_bytes);

        assert_eq!(trace.len(), 2);
        for event in &trace {
            assert!((event.requested_quantity - 0.3).abs() < 1e-12);
            assert!((event.filled_quantity - 0.3).abs() < 1e-12);
            assert_eq!(event.status, "filled");
        }
        assert!(output.metrics.final_inventory.abs() <= 1e-12);
    }

    #[test]
    fn spot_market_notional_bounds_use_requested_quantity_before_partial_fills() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,0.25]]}\n",
            "{\"timestamp\":1500000,\"sequence\":2,\"event\":\"trade\",\"side\":\"buy\",\"price\":100,\"quantity\":1}\n",
            "{\"timestamp\":2000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,0.25]]}\n",
            "{\"timestamp\":3000000,\"sequence\":4,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,0.25]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_500_000,
                target_position: 0.5,
            },
            TargetPositionDecision {
                timestamp_us: 2_500_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "spot".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: true,
        };
        let rules = spot_replay_rules();
        let rules = hft_research_manifest::CexSpotInstrumentRulesV1 {
            notional_filter: hft_research_manifest::CexSpotNotionalFilterV1 {
                filter_type: "NOTIONAL".to_string(),
                min_notional: "5".to_string(),
                max_notional: Some("40".to_string()),
                apply_min_to_market: true,
                apply_max_to_market: Some(true),
                avg_price_mins: 0,
            },
            ..rules
        };

        let output = replay_target_positions_with_trace_and_spot_rules(
            tape.as_bytes(),
            &decisions,
            &config,
            Some(&rules),
        )
        .unwrap();
        let trace = tape_trace_events(&output.trace_bytes);
        assert_eq!(trace[0].status, "cancelled_invalid_instrument_rules");
        assert!(trace[0].fills.is_empty());
        assert_eq!(trace[1].status, "no_order");
        assert_eq!(output.metrics.fill_count, 0);
        assert_eq!(output.metrics.final_inventory, 0.0);
    }

    #[test]
    fn target_position_replay_models_latency_ioc_partial_fills_and_trace_hash() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,0],[89,1]],\"asks\":[[101,0],[91,1]]}\n",
            "{\"timestamp\":5000000,\"sequence\":4,\"event\":\"l2_update\",\"bids\":[[89,1]],\"asks\":[[91,1]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 3_000_000,
                target_position: 0.0,
            },
        ];
        let mut config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 2_000_000,
            order_latency_us: 2_000_000,
            position_notional_usd: 90.0,
            fee_bps: 10.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };
        let delayed = replay_target_positions_with_trace(tape.as_bytes(), &decisions, &config)
            .expect("delayed IOC replay");
        let delayed_events = tape_trace_events(&delayed.trace_bytes);
        assert_eq!(delayed_events[0].arrival_timestamp_us, 3_000_000);
        assert_eq!(delayed_events[0].vwap, Some(91.0));
        assert_eq!(delayed_events[1].arrival_timestamp_us, 5_000_000);
        assert_eq!(delayed.metrics.final_inventory, 0.0);
        assert_eq!(delayed.metrics.partial_order_count, 0);
        assert_eq!(delayed.metrics.canceled_order_count, 0);
        assert_eq!(delayed.metrics.trace_sha256, delayed.trace_sha256);

        config.order_latency_us = 0;
        let immediate = replay_target_positions_with_trace(tape.as_bytes(), &decisions, &config)
            .expect("immediate IOC replay");
        let immediate_events = tape_trace_events(&immediate.trace_bytes);
        assert_eq!(immediate_events[0].arrival_timestamp_us, 1_000_000);
        assert_eq!(immediate_events[0].vwap, Some(101.0));
        assert_ne!(delayed.trace_sha256, immediate.trace_sha256);
    }

    #[test]
    fn target_position_replay_waits_for_a_book_after_trade_arrival() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":5000000,\"sequence\":2,\"event\":\"trade\",\"side\":\"buy\",\"price\":101,\"quantity\":1}\n",
            "{\"timestamp\":6000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,10]],\"asks\":[[101,0],[111,10]]}\n",
            "{\"timestamp\":7000000,\"sequence\":4,\"event\":\"l2_update\",\"bids\":[[99,10]],\"asks\":[[111,10]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 4_900_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 6_500_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 2_000_000,
            order_latency_us: 0,
            position_notional_usd: 105.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: true,
        };

        let output = replay_target_positions_with_trace(tape.as_bytes(), &decisions, &config)
            .expect("stale trade replay");
        let events = tape_trace_events(&output.trace_bytes);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].arrival_timestamp_us, 6_000_000);
        assert_eq!(events[0].status, "filled");
        assert_eq!(events[0].vwap, Some(111.0));
        assert_eq!(events[1].arrival_timestamp_us, 7_000_000);
        assert_eq!(events[1].status, "filled");
        assert_eq!(output.metrics.fill_count, 2);
        assert_eq!(output.metrics.canceled_order_count, 0);
        assert!(!output.metrics.displayed_depth_unavailable);
        assert_eq!(output.metrics.final_inventory, 0.0);
    }

    #[test]
    fn target_position_replay_missing_book_after_trade_is_fail_closed() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":5000000,\"sequence\":2,\"event\":\"trade\",\"side\":\"buy\",\"price\":101,\"quantity\":1}\n",
        );
        let decisions = [TargetPositionDecision {
            timestamp_us: 4_900_000,
            target_position: 1.0,
        }];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: true,
        };

        let error = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap_err();

        assert!(error
            .to_string()
            .contains("tape ended before all decisions"));
    }

    #[test]
    fn target_position_replay_uses_actual_depth_vwap_and_explicit_residual_cancel() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 150.0,
            fee_bps: 10.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };
        let output = replay_target_positions_with_trace(tape.as_bytes(), &decisions, &config)
            .expect("partial IOC replay");
        let events = tape_trace_events(&output.trace_bytes);
        assert_eq!(events[0].status, "partial_fill_cancelled");
        assert_eq!(events[0].filled_quantity, 1.0);
        assert!((events[0].residual_quantity - (150.0 / 100.0 - 1.0)).abs() < 1e-12);
        assert_eq!(events[0].vwap, Some(101.0));
        assert_eq!(events[1].status, "filled");
        assert_eq!(output.metrics.partial_order_count, 1);
        assert_eq!(output.metrics.canceled_order_count, 1);
        assert_eq!(output.metrics.fill_count, 2);
        assert_eq!(output.metrics.final_inventory, 0.0);
        assert!((output.metrics.final_cash - 147.8).abs() < 1e-12);
        assert!((output.metrics.total_fees - 0.2).abs() < 1e-12);
        assert!((output.metrics.total_turnover - (101.0 + 99.0) / 150.0).abs() < 1e-12);
        assert!((output.metrics.requested_turnover - (1.0 + 2.0 / 3.0)).abs() < 1e-12);
        assert_eq!(
            output.metrics.final_inventory,
            events.last().unwrap().inventory_after
        );
    }

    #[test]
    fn target_position_replay_reversal_and_flatten_use_actual_inventory() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,5]],\"asks\":[[101,5]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,5]],\"asks\":[[101,5]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,5]],\"asks\":[[101,5]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: -1.0,
            },
            TargetPositionDecision {
                timestamp_us: 3_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };
        let output = replay_target_positions_with_trace(tape.as_bytes(), &decisions, &config)
            .expect("reversal replay");
        let events = tape_trace_events(&output.trace_bytes);
        assert_eq!(
            events.iter().map(|event| event.side).collect::<Vec<_>>(),
            vec![Some(Side::Buy), Some(Side::Sell), Some(Side::Buy),]
        );
        assert_eq!(events[0].filled_quantity, 1.0);
        assert_eq!(events[1].filled_quantity, 2.0);
        assert_eq!(events[2].filled_quantity, 1.0);
        assert_eq!(output.metrics.final_inventory, 0.0);
    }

    #[test]
    fn target_position_replay_capacity_exhaustion_is_a_cancelled_ioc() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 0.5,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 3_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 200.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 1,
            trade_tape_declared: false,
        };
        let output = replay_target_positions_with_trace(tape.as_bytes(), &decisions, &config)
            .expect("capacity exhaustion replay");
        let events = tape_trace_events(&output.trace_bytes);
        assert_eq!(events[0].status, "filled");
        assert_eq!(events[1].status, "cancelled_no_liquidity");
        assert_eq!(events[2].status, "filled");
        assert!(output.metrics.displayed_depth_unavailable);
        assert_eq!(output.metrics.canceled_order_count, 1);
        assert_eq!(output.metrics.final_inventory, 0.0);
    }

    #[test]
    fn target_position_replay_requires_positive_notional() {
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1,
            order_latency_us: 0,
            position_notional_usd: 0.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };
        let decisions = [TargetPositionDecision {
            timestamp_us: 1,
            target_position: 0.0,
        }];
        assert!(replay_target_positions(
            b"{\"timestamp\":1,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            &decisions,
            &config,
        )
        .is_err());
    }

    #[test]
    fn target_position_replay_rejects_unsupported_non_crossing_execution() {
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: false,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };
        let decisions = [TargetPositionDecision {
            timestamp_us: 1,
            target_position: 0.0,
        }];

        let error = replay_target_positions(
            b"{\"timestamp\":1,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,1]],\"asks\":[[101,1]]}\n",
            &decisions,
            &config,
        )
        .unwrap_err();

        assert!(error.to_string().contains("requires cross_spread=true"));
    }

    fn tape_trace_events(bytes: &[u8]) -> Vec<TargetPositionReplayTraceEvent> {
        bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).expect("trace event"))
            .collect()
    }

    #[test]
    fn target_position_replay_uses_relative_drawdown_without_a_final_interval() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,0],[109,10]],\"asks\":[[101,0],[111,10]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[109,0],[99,10]],\"asks\":[[111,0],[101,10]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 3_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 10.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let metrics = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();
        assert!(metrics.cumulative_net_return < 0.0);
        assert!(metrics.total_fees.abs() < 1e-12);
        assert_eq!(metrics.final_inventory, 0.0);
        assert_eq!(metrics.order_count, 3);
        assert_eq!(metrics.filled_order_count, 3);
    }

    #[test]
    fn target_position_replay_rejects_a_new_snapshot_with_an_open_position() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"snapshot\",\"bids\":[[89,10]],\"asks\":[[91,10]]}\n",
        );
        let decisions = [TargetPositionDecision {
            timestamp_us: 1_000_000,
            target_position: 1.0,
        }];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let error = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap_err();
        assert!(error.to_string().contains("before flattening"));
    }

    #[test]
    fn target_position_replay_rejects_leftover_prior_series_decisions() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,0],[109,10]],\"asks\":[[101,0],[111,10]]}\n",
            "{\"timestamp\":10000000,\"sequence\":3,\"event\":\"snapshot\",\"bids\":[[89,10]],\"asks\":[[91,10]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 0.0,
            },
            TargetPositionDecision {
                timestamp_us: 5_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 10_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let error = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap_err();
        assert!(error
            .to_string()
            .contains("leftover pre-snapshot decisions"));
    }

    #[test]
    fn target_position_replay_resets_marked_mid_across_snapshot_boundaries() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,0],[109,10]],\"asks\":[[101,0],[111,10]]}\n",
            "{\"timestamp\":10000000,\"sequence\":3,\"event\":\"snapshot\",\"bids\":[[89,10]],\"asks\":[[91,10]]}\n",
            "{\"timestamp\":11000000,\"sequence\":4,\"event\":\"l2_update\",\"bids\":[[89,0],[99,10]],\"asks\":[[91,0],[101,10]]}\n",
        );
        let decisions = [
            TargetPositionDecision {
                timestamp_us: 1_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 2_000_000,
                target_position: 0.0,
            },
            TargetPositionDecision {
                timestamp_us: 10_000_000,
                target_position: 1.0,
            },
            TargetPositionDecision {
                timestamp_us: 11_000_000,
                target_position: 0.0,
            },
        ];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let metrics = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();
        assert!((metrics.cumulative_net_return - 0.16888888888888887).abs() < 1e-12);
        assert_eq!(metrics.final_inventory, 0.0);
    }

    #[test]
    fn target_position_replay_requires_a_flat_finish() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,0],[109,10]],\"asks\":[[101,0],[111,10]]}\n",
        );
        let decisions = [TargetPositionDecision {
            timestamp_us: 1_000_000,
            target_position: 1.0,
        }];
        let config = TargetPositionReplayConfig {
            market: "usdm".to_string(),
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            order_latency_us: 0,
            position_notional_usd: 100.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: true,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let error = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap_err();
        assert!(error
            .to_string()
            .contains("ended before flattening actual inventory"));
    }

    #[test]
    fn backtest_replays_in_memory_l2_events() {
        let mut engine = BacktestEngine::new(test_config()).unwrap();
        let stream = vec![
            Ok(EventEnvelope {
                ts: 1_000_000,
                sequence: None,
                payload: EventPayload::Snapshot {
                    bids: vec![Level {
                        price: 100.0,
                        quantity: 5.0,
                    }],
                    asks: vec![Level {
                        price: 100.2,
                        quantity: 5.0,
                    }],
                },
            }),
            Ok(EventEnvelope {
                ts: 1_100_000,
                sequence: None,
                payload: EventPayload::L2Update {
                    bids: vec![Level {
                        price: 100.1,
                        quantity: 4.0,
                    }],
                    asks: vec![Level {
                        price: 100.2,
                        quantity: 0.0,
                    }],
                },
            }),
            Ok(EventEnvelope {
                ts: 1_200_000,
                sequence: None,
                payload: EventPayload::Trade {
                    side: TradeSide::Buy,
                    price: 100.1,
                    quantity: 1.0,
                },
            }),
        ];

        let result = engine
            .run_with_stream(stream.into_iter())
            .expect("in-memory replay should run");

        assert_eq!(result.summary.trades, result.trades.len());
        assert_eq!(engine.stats.last_trade_price, Some(100.1));
    }

    #[test]
    fn lob_only_breakout_produces_a_depth_bounded_trade() {
        let mut config = test_config();
        config.data.tick_size = 0.1;
        config.strategy.volume_factor = 0.0;
        config.strategy.ofi_threshold = 1.0;
        config.execution.base_qty = 2.0;
        config.execution.max_position = 2.0;
        config.risk.inventory_limit = 2.0;
        let mut engine = BacktestEngine::new(config).unwrap();
        let stream = vec![
            Ok(EventEnvelope {
                ts: 1_000_000,
                sequence: None,
                payload: EventPayload::Snapshot {
                    bids: vec![Level {
                        price: 100.0,
                        quantity: 10.0,
                    }],
                    asks: vec![Level {
                        price: 100.2,
                        quantity: 10.0,
                    }],
                },
            }),
            Ok(EventEnvelope {
                ts: 2_000_000,
                sequence: None,
                payload: EventPayload::Snapshot {
                    bids: vec![Level {
                        price: 99.6,
                        quantity: 1.0,
                    }],
                    asks: vec![Level {
                        price: 99.8,
                        quantity: 10.0,
                    }],
                },
            }),
        ];

        let result = engine.run_with_stream(stream.into_iter()).unwrap();

        assert_eq!(result.trades.len(), 1);
        assert!((result.trades[0].qty - 0.1).abs() < 1e-9);
    }

    #[test]
    fn risk_rejection_does_not_consume_entry_liquidity() {
        let mut config = test_config();
        config.data.tick_size = 0.1;
        config.strategy.volume_factor = 0.0;
        config.strategy.ofi_threshold = 1.0;
        config.execution.base_qty = 2.0;
        config.execution.max_position = 2.0;
        config.risk.inventory_limit = 0.0;
        let mut engine = BacktestEngine::new(config).unwrap();
        let stream = vec![
            Ok(EventEnvelope {
                ts: 1_000_000,
                sequence: None,
                payload: EventPayload::Snapshot {
                    bids: vec![Level {
                        price: 100.0,
                        quantity: 10.0,
                    }],
                    asks: vec![Level {
                        price: 100.2,
                        quantity: 10.0,
                    }],
                },
            }),
            Ok(EventEnvelope {
                ts: 2_000_000,
                sequence: None,
                payload: EventPayload::Snapshot {
                    bids: vec![Level {
                        price: 99.6,
                        quantity: 1.0,
                    }],
                    asks: vec![Level {
                        price: 99.8,
                        quantity: 10.0,
                    }],
                },
            }),
        ];

        let result = engine.run_with_stream(stream.into_iter()).unwrap();

        assert!(result.trades.is_empty());
        assert_eq!(engine.execution.position.side, None);
        assert_eq!(
            engine
                .displayed_budget
                .best_ask()
                .and_then(|(_, quantity)| quantity.to_f64()),
            Some(10.0)
        );
    }

    #[test]
    fn session_end_exit_respects_risk_slippage_ceiling() {
        let mut config = test_config();
        config.data.tick_size = 0.1;
        config.execution.max_fill_ratio = 1.0;
        config.risk.slippage_limit_ticks = 1.0;
        let mut engine = BacktestEngine::new(config).unwrap();
        engine.order_book.apply_snapshot(
            1,
            &[
                Level {
                    price: 100.0,
                    quantity: 1.0,
                },
                Level {
                    price: 99.8,
                    quantity: 10.0,
                },
            ],
            &[Level {
                price: 100.1,
                quantity: 10.0,
            }],
        );
        engine.book_generation = 1;
        engine.next_sequence = 1;
        assert!(engine
            .displayed_budget
            .observe(&engine.order_book.displayed_snapshot(1, 1, 1), 1,));
        engine
            .execution
            .enter_long(1.0, 100.1, 2.0, 100.0, 10.0, &mut engine.stats);

        let result = engine
            .run_with_stream(std::iter::empty())
            .expect("session-end exit should complete");

        assert_eq!(result.trades.len(), 1);
        assert_eq!(result.trades[0].qty, 1.0);
        assert_eq!(result.trades[0].exit_price, 100.0);
        assert_eq!(result.summary.open_position_qty, 1.0);
    }

    #[test]
    fn short_exit_respects_risk_slippage_ceiling() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                max_fill_ratio: 1.0,
                ..ExecutionConfig::default()
            },
            RiskConfig {
                slippage_limit_ticks: 1.0,
                ..RiskConfig::default()
            },
            0.1,
            data::binance_lob_replay::Market::Usdm,
        );
        execution.position.side = Some(PositionSide::Short);
        execution.position.qty = 2.0;
        let mut book = OrderBook::new(5);
        book.apply_snapshot(
            1,
            &[Level {
                price: 100.0,
                quantity: 10.0,
            }],
            &[
                Level {
                    price: 100.1,
                    quantity: 1.0,
                },
                Level {
                    price: 100.3,
                    quantity: 10.0,
                },
            ],
        );

        assert_eq!(execution.executable_exit(&book), Some((1.0, 100.1)));
    }

    #[test]
    fn execution_fees_are_deducted_from_backtest_pnl() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                fee_bps: 10.0,
                ..ExecutionConfig::default()
            },
            RiskConfig::default(),
            0.01,
            data::binance_lob_replay::Market::Usdm,
        );
        let mut stats = BacktestStats::default();
        execution.enter_long(1.0, 100.0, 1.0, 100.0, 10.0, &mut stats);
        execution.exit_position(2.0, 110.0, 1.0, ExitReason::SessionEnd, &mut stats);

        assert_eq!(execution.trades.len(), 1);
        assert_eq!(execution.trades[0].gross_pnl, 10.0);
        assert!((execution.trades[0].fees - 0.21).abs() < 1e-9);
        assert!((execution.trades[0].pnl - 9.79).abs() < 1e-9);
    }

    #[test]
    fn spot_execution_uses_cash_inventory_and_rejects_shorts() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                fee_bps: 10.0,
                initial_cash: 100.0,
                ..ExecutionConfig::default()
            },
            RiskConfig {
                inventory_limit: 2.0,
                ..RiskConfig::default()
            },
            0.01,
            data::binance_lob_replay::Market::Spot,
        );
        let mut stats = BacktestStats::default();

        assert!(!execution.can_enter(PositionSide::Short, 1.0, 100.0));
        assert!(execution.can_enter(PositionSide::Long, 1.0, 10.0));
        execution.enter_long(1.0, 10.0, 1.0, 10.0, 1.0, &mut stats);
        assert!((execution.cash - 89.99).abs() < 1e-9);
        assert_eq!(execution.inventory, 1.0);
        execution.exit_position(2.0, 11.0, 1.0, ExitReason::SessionEnd, &mut stats);
        assert!((execution.cash - 100.979).abs() < 1e-9);
        assert_eq!(execution.inventory, 0.0);
        assert!((execution.trades[0].fees - 0.021).abs() < 1e-9);
    }

    #[test]
    fn spot_execution_rejects_unfunded_entry() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig::default(),
            RiskConfig::default(),
            0.01,
            data::binance_lob_replay::Market::Spot,
        );
        let mut stats = BacktestStats::default();

        assert!(!execution.can_enter(PositionSide::Long, 1.0, 100.0));
        execution.enter_long(1.0, 100.0, 1.0, 100.0, 1.0, &mut stats);
        assert!(!execution.has_position());
        assert_eq!(execution.inventory, 0.0);
    }

    #[test]
    fn spot_partial_exits_charge_entry_fee_once() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                fee_bps: 10.0,
                initial_cash: 1_000.0,
                ..ExecutionConfig::default()
            },
            RiskConfig {
                inventory_limit: 3.0,
                ..RiskConfig::default()
            },
            0.01,
            data::binance_lob_replay::Market::Spot,
        );
        let mut stats = BacktestStats::default();
        execution.enter_long(1.0, 100.0, 2.0, 100.0, 1.0, &mut stats);
        execution.exit_position(2.0, 110.0, 1.0, ExitReason::SessionEnd, &mut stats);
        execution.exit_position(3.0, 110.0, 1.0, ExitReason::SessionEnd, &mut stats);

        assert_eq!(execution.trades.len(), 2);
        assert!((execution.trades[0].fees - 0.21).abs() < 1e-9);
        assert!((execution.trades[1].fees - 0.21).abs() < 1e-9);
        assert!((execution.cash - 1_019.58).abs() < 1e-9);
        assert_eq!(execution.inventory, 0.0);
    }

    #[test]
    fn exit_respects_displayed_depth_and_leaves_residual_position() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                max_fill_ratio: 0.5,
                ..ExecutionConfig::default()
            },
            RiskConfig::default(),
            0.01,
            data::binance_lob_replay::Market::Usdm,
        );
        let mut stats = BacktestStats::default();
        execution.enter_long(1.0, 100.0, 2.0, 100.0, 10.0, &mut stats);
        let mut book = OrderBook::new(5);
        book.apply_snapshot(
            2,
            &[Level {
                price: 101.0,
                quantity: 1.0,
            }],
            &[Level {
                price: 102.0,
                quantity: 1.0,
            }],
        );
        let (fill_qty, fill_price) = execution.executable_exit(&book).unwrap();
        execution.exit_position(
            2.0,
            fill_price,
            fill_qty,
            ExitReason::SessionEnd,
            &mut stats,
        );

        assert_eq!(execution.trades[0].qty, 0.5);
        assert_eq!(execution.position.qty, 1.5);
    }

    #[test]
    fn entry_walks_current_l2_and_respects_slippage_band() {
        let mut book = OrderBook::new(5);
        book.apply_snapshot(
            1,
            &[
                Level {
                    price: 99.9,
                    quantity: 1.0,
                },
                Level {
                    price: 99.8,
                    quantity: 4.0,
                },
            ],
            &[
                Level {
                    price: 100.0,
                    quantity: 1.0,
                },
                Level {
                    price: 100.1,
                    quantity: 4.0,
                },
                Level {
                    price: 100.2,
                    quantity: 10.0,
                },
            ],
        );

        let (fill_qty, fill_price) = book
            .executable_entry(PositionSide::Long, 10.0, 0.5, 1.0, 0.1)
            .unwrap();

        assert_eq!(fill_qty, 2.5);
        assert!((fill_price - 100.08).abs() < 1e-9);
    }

    #[test]
    fn summary_net_sharpe_uses_unannualized_per_trade_population_stddev() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                fee_bps: 10.0,
                ..ExecutionConfig::default()
            },
            RiskConfig::default(),
            0.01,
            data::binance_lob_replay::Market::Usdm,
        );
        let mut stats = BacktestStats::default();
        execution.enter_long(1.0, 100.0, 1.0, 100.0, 10.0, &mut stats);
        execution.exit_position(2.0, 110.0, 1.0, ExitReason::SessionEnd, &mut stats);
        execution.enter_long(3.0, 100.0, 1.0, 100.0, 10.0, &mut stats);
        execution.exit_position(4.0, 90.0, 1.0, ExitReason::SessionEnd, &mut stats);

        let summary =
            stats.clone_into_summary(0.0, execution.cash, execution.inventory, &execution.trades);

        let pnls = execution
            .trades
            .iter()
            .map(|trade| trade.pnl)
            .collect::<Vec<_>>();
        let average = pnls.iter().sum::<f64>() / pnls.len() as f64;
        let deviation = (pnls.iter().map(|pnl| (pnl - average).powi(2)).sum::<f64>()
            / pnls.len() as f64)
            .sqrt();
        assert!(deviation > 0.0);
        assert!((summary.net_sharpe - average / deviation).abs() < 1e-12);
        assert_eq!(summary.trades, 2);

        let empty = BacktestStats::default().clone_into_summary(0.0, 0.0, 0.0, &[]);
        assert_eq!(empty.net_sharpe, 0.0);
    }
}
