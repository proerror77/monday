use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufReader, Cursor};
use std::mem;

use anyhow::{Context, Result};
use itertools::Itertools;
use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::{
    BacktestConfig, BacktestInputEvidence, ExecutionConfig, RiskConfig, StrategyConfig,
};
use crate::event::{EventEnvelope, EventPayload, EventStream, Level, TradeSide};

const MICROS_IN_SECOND: f64 = 1_000_000.0;
const BPS: f64 = 10_000.0;

pub const TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION: &str =
    "hft-backtest-target-position-replay-v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionDecision {
    pub timestamp_us: i64,
    pub target_position: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPositionReplayConfig {
    pub max_depth_levels: usize,
    pub max_decision_delay_us: u64,
    pub position_notional_usd: f64,
    pub fee_bps: f64,
    pub rebate_bps: f64,
    pub funding_bps: f64,
    pub latency_bps: f64,
    pub additional_slippage_bps: f64,
    pub cross_spread: bool,
    pub capacity_depth_levels: usize,
    pub trade_tape_declared: bool,
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
    pub mean_net_return: f64,
    pub cumulative_net_return: f64,
    pub max_drawdown: f64,
    pub net_sharpe: f64,
    pub max_abs_position: f64,
    pub max_same_side_depth_fraction: Option<f64>,
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

pub(crate) struct TargetPositionReplay<'a> {
    decisions: &'a [TargetPositionDecision],
    config: &'a TargetPositionReplayConfig,
    book: OrderBook,
    seeded: bool,
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
    position: f64,
    marked_mid: Option<f64>,
    total_turnover: f64,
    max_same_side_depth_fraction: Option<f64>,
    returns: Vec<f64>,
}

impl<'a> TargetPositionReplay<'a> {
    pub(crate) fn new(
        decisions: &'a [TargetPositionDecision],
        config: &'a TargetPositionReplayConfig,
    ) -> Result<Self> {
        validate_target_replay_inputs(decisions, config)?;
        Ok(Self {
            decisions,
            config,
            book: OrderBook::new(config.max_depth_levels),
            seeded: false,
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
            position: 0.0,
            marked_mid: None,
            total_turnover: 0.0,
            max_same_side_depth_fraction: config.capacity_depth_levels.gt(&0).then_some(0.0),
            returns: Vec::with_capacity(decisions.len()),
        })
    }

    pub(crate) fn observe(&mut self, event: &EventEnvelope) -> Result<()> {
        self.event_count = self
            .event_count
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("target-position replay event count overflow"))?;
        self.first_event_time_us.get_or_insert(event.ts);
        self.last_event_time_us = Some(event.ts);
        match &event.payload {
            EventPayload::Snapshot { bids, asks } => {
                self.book.apply_snapshot(event.ts, bids, asks);
                self.seeded = true;
                self.snapshot_events += 1;
            }
            EventPayload::L2Update { bids, asks } => {
                if !self.seeded {
                    anyhow::bail!("target-position replay received an L2 update before a snapshot");
                }
                self.book.apply_delta(event.ts, bids, asks);
                self.l2_update_events += 1;
            }
            EventPayload::Trade { .. } => {
                if !self.config.trade_tape_declared {
                    anyhow::bail!("target-position replay tape contains undeclared trade events");
                }
                self.trade_events += 1;
            }
        }
        if self.seeded {
            let (bid_levels, ask_levels) = self.book.depth_level_counts();
            self.min_bid_depth_levels = self.min_bid_depth_levels.min(bid_levels);
            self.max_bid_depth_levels = self.max_bid_depth_levels.max(bid_levels);
            self.min_ask_depth_levels = self.min_ask_depth_levels.min(ask_levels);
            self.max_ask_depth_levels = self.max_ask_depth_levels.max(ask_levels);
            if let Some(mid) = self.book.mid_price() {
                while self.decision_index < self.decisions.len()
                    && self.decisions[self.decision_index].timestamp_us <= event.ts
                {
                    let decision = &self.decisions[self.decision_index];
                    let delay = u64::try_from(event.ts - decision.timestamp_us).map_err(|_| {
                        anyhow::anyhow!("target-position replay decision clock reversed")
                    })?;
                    if delay > self.config.max_decision_delay_us {
                        anyhow::bail!("target-position replay decision exceeded its maximum delay");
                    }
                    self.max_decision_delay_us = self.max_decision_delay_us.max(delay);
                    let change = decision.target_position - self.position;
                    let turnover = change.abs();
                    if turnover > f64::EPSILON {
                        self.position_changes += 1;
                        if let Some(max_fraction) = &mut self.max_same_side_depth_fraction {
                            let depth_notional = self
                                .book
                                .same_side_depth(change, self.config.capacity_depth_levels)
                                .ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "target-position replay has no same-side depth for a position change"
                                    )
                                })?
                                * mid;
                            if depth_notional <= 0.0 {
                                anyhow::bail!(
                                    "target-position replay has non-positive same-side depth"
                                );
                            }
                            *max_fraction = (*max_fraction)
                                .max(self.config.position_notional_usd * turnover / depth_notional);
                        }
                    }
                    self.total_turnover += turnover;
                    let gross_return = self
                        .marked_mid
                        .map(|previous_mid| self.position * (mid / previous_mid - 1.0))
                        .unwrap_or(0.0);
                    let spread_cost_bps = if self.config.cross_spread {
                        self.book.spread_bps().ok_or_else(|| {
                            anyhow::anyhow!("target-position replay has no spread")
                        })? / 2.0
                    } else {
                        0.0
                    };
                    let transaction_cost = turnover
                        * (self.config.fee_bps - self.config.rebate_bps
                            + self.config.latency_bps
                            + self.config.additional_slippage_bps
                            + spread_cost_bps)
                        / BPS;
                    let funding_cost = self.position.abs() * self.config.funding_bps / BPS;
                    self.returns
                        .push(gross_return - transaction_cost - funding_cost);
                    self.marked_mid = Some(mid);
                    self.position = decision.target_position;
                    self.decision_index += 1;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<TargetPositionReplayMetrics> {
        if self.decision_index != self.decisions.len() {
            anyhow::bail!("target-position replay tape ended before all decisions");
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
        Ok(TargetPositionReplayMetrics {
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
            mean_net_return,
            cumulative_net_return,
            max_drawdown,
            net_sharpe,
            max_abs_position: self
                .decisions
                .iter()
                .map(|decision| decision.target_position.abs())
                .fold(0.0, f64::max),
            max_same_side_depth_fraction: self.max_same_side_depth_fraction,
        })
    }
}

fn validate_target_replay_inputs(
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
) -> Result<()> {
    let costs = [
        config.position_notional_usd,
        config.fee_bps,
        config.rebate_bps,
        config.funding_bps,
        config.latency_bps,
        config.additional_slippage_bps,
    ];
    let capacity_disabled =
        config.position_notional_usd == 0.0 && config.capacity_depth_levels == 0;
    let capacity_enabled = config.position_notional_usd > 0.0 && config.capacity_depth_levels > 0;
    if decisions.is_empty()
        || decisions
            .windows(2)
            .any(|pair| pair[0].timestamp_us >= pair[1].timestamp_us)
        || decisions.iter().any(|decision| {
            !decision.target_position.is_finite() || decision.target_position.abs() > 1.0
        })
        || config.max_depth_levels == 0
        || config.max_decision_delay_us == 0
        || costs.iter().any(|value| !value.is_finite() || *value < 0.0)
        || !(capacity_disabled || capacity_enabled)
        || config.capacity_depth_levels > config.max_depth_levels
    {
        anyhow::bail!("target-position replay inputs are invalid");
    }
    Ok(())
}

pub struct BacktestEngine {
    cfg: BacktestConfig,
    order_book: OrderBook,
    liquidity: LiquidityMap,
    flow: FlowTracker,
    execution: ExecutionManager,
    stats: BacktestStats,
    last_ts: Option<i64>,
}

impl BacktestEngine {
    pub fn new(cfg: BacktestConfig) -> Self {
        let max_levels = cfg.data.max_depth_levels;
        let tick_size = cfg.data.tick_size.max(1e-6);
        let strategy = cfg.strategy.clone();
        let execution_cfg = cfg.execution.clone();
        let risk_cfg = cfg.risk.clone();
        Self {
            cfg,
            order_book: OrderBook::new(max_levels),
            liquidity: LiquidityMap::new(strategy, tick_size, max_levels),
            flow: FlowTracker::new(),
            execution: ExecutionManager::new(execution_cfg, risk_cfg, tick_size),
            stats: BacktestStats::default(),
            last_ts: None,
        }
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
            if let Some((fill_qty, fill_price)) = self.execution.executable_exit(&self.order_book) {
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
                self.liquidity.update(
                    event.ts,
                    &self.order_book.snapshot(self.cfg.data.max_depth_levels),
                );
                self.flow.update_ofi(ts_sec, ofi);
                self.evaluate_signals(event.ts)?;
            }
            EventPayload::L2Update { bids, asks } => {
                let ofi = self.order_book.apply_delta(event.ts, bids, asks);
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
                if let Some((qty, entry_price)) = self.order_book.executable_entry(
                    PositionSide::Short,
                    requested_qty,
                    self.cfg.execution.max_fill_ratio,
                    self.cfg.execution.max_slippage_ticks,
                    self.cfg.data.tick_size,
                ) {
                    if self.execution.can_enter(qty) {
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
                if let Some((qty, entry_price)) = self.order_book.executable_entry(
                    PositionSide::Long,
                    requested_qty,
                    self.cfg.execution.max_fill_ratio,
                    self.cfg.execution.max_slippage_ticks,
                    self.cfg.data.tick_size,
                ) {
                    if self.execution.can_enter(qty) {
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
            &self.order_book,
            &mut self.stats,
        );

        Ok(())
    }

    fn finish(&mut self) -> BacktestResult {
        BacktestResult {
            trades: mem::take(&mut self.execution.trades),
            summary: self
                .stats
                .clone_into_summary(self.execution.position.qty.abs()),
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

    fn spread_bps(&self) -> Option<f64> {
        let (bid, _) = self.best_bid()?;
        let (ask, _) = self.best_ask()?;
        let mid = self.mid_price()?;
        (mid > 0.0).then_some((ask - bid) / mid * BPS)
    }

    fn depth_level_counts(&self) -> (usize, usize) {
        (self.bids.len(), self.asks.len())
    }

    fn same_side_depth(&self, position_change: f64, levels: usize) -> Option<f64> {
        if position_change > 0.0 {
            Some(self.asks.values().take(levels).copied().sum())
        } else if position_change < 0.0 {
            Some(self.bids.values().rev().take(levels).copied().sum())
        } else {
            None
        }
    }

    fn executable_exit(
        &self,
        position_side: PositionSide,
        requested_qty: f64,
        max_fill_ratio: f64,
        max_slippage_ticks: f64,
        tick_size: f64,
    ) -> Option<(f64, f64)> {
        let participation = max_fill_ratio.clamp(0.0, 1.0);
        if requested_qty <= 0.0
            || participation <= 0.0
            || !max_slippage_ticks.is_finite()
            || max_slippage_ticks < 0.0
            || !tick_size.is_finite()
            || tick_size <= 0.0
        {
            return None;
        }
        let (levels, worst_price): (Box<dyn Iterator<Item = (f64, f64)> + '_>, f64) =
            match position_side {
                PositionSide::Long => {
                    let best = self.best_bid()?.0;
                    (
                        Box::new(
                            self.bids
                                .iter()
                                .rev()
                                .map(|(price, qty)| (price.into_inner(), *qty)),
                        ),
                        best - max_slippage_ticks * tick_size,
                    )
                }
                PositionSide::Short => {
                    let best = self.best_ask()?.0;
                    (
                        Box::new(
                            self.asks
                                .iter()
                                .map(|(price, qty)| (price.into_inner(), *qty)),
                        ),
                        best + max_slippage_ticks * tick_size,
                    )
                }
            };
        let mut remaining = requested_qty;
        let mut filled = 0.0;
        let mut notional = 0.0;
        for (price, displayed_qty) in levels {
            let outside_slippage = match position_side {
                PositionSide::Long => price < worst_price - 1e-12,
                PositionSide::Short => price > worst_price + 1e-12,
            };
            if outside_slippage {
                break;
            }
            let level_fill = remaining.min(displayed_qty.max(0.0) * participation);
            if level_fill <= 0.0 {
                continue;
            }
            filled += level_fill;
            notional += level_fill * price;
            remaining -= level_fill;
            if remaining <= 1e-12 {
                break;
            }
        }
        (filled > 0.0).then_some((filled, notional / filled))
    }

    fn executable_entry(
        &self,
        position_side: PositionSide,
        requested_qty: f64,
        max_fill_ratio: f64,
        max_slippage_ticks: f64,
        tick_size: f64,
    ) -> Option<(f64, f64)> {
        let participation = max_fill_ratio.clamp(0.0, 1.0);
        if requested_qty <= 0.0
            || participation <= 0.0
            || max_slippage_ticks < 0.0
            || tick_size <= 0.0
        {
            return None;
        }
        let (levels, worst_price): (Box<dyn Iterator<Item = (f64, f64)> + '_>, f64) =
            match position_side {
                PositionSide::Long => {
                    let best = self.best_ask()?.0;
                    (
                        Box::new(
                            self.asks
                                .iter()
                                .map(|(price, qty)| (price.into_inner(), *qty)),
                        ),
                        best + max_slippage_ticks * tick_size,
                    )
                }
                PositionSide::Short => {
                    let best = self.best_bid()?.0;
                    (
                        Box::new(
                            self.bids
                                .iter()
                                .rev()
                                .map(|(price, qty)| (price.into_inner(), *qty)),
                        ),
                        best - max_slippage_ticks * tick_size,
                    )
                }
            };
        let mut remaining = requested_qty;
        let mut filled = 0.0;
        let mut notional = 0.0;
        for (price, displayed_qty) in levels {
            let outside_slippage = match position_side {
                PositionSide::Long => price > worst_price + 1e-12,
                PositionSide::Short => price < worst_price - 1e-12,
            };
            if outside_slippage {
                break;
            }
            let level_fill = remaining.min(displayed_qty.max(0.0) * participation);
            if level_fill <= 0.0 {
                continue;
            }
            filled += level_fill;
            notional += level_fill * price;
            remaining -= level_fill;
            if remaining <= 1e-12 {
                break;
            }
        }
        (filled > 0.0).then_some((filled, notional / filled))
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
    tick_size: f64,
    position: PositionState,
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
    fn new(cfg: ExecutionConfig, risk: RiskConfig, tick_size: f64) -> Self {
        Self {
            cfg,
            risk,
            tick_size,
            position: PositionState::default(),
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

    fn can_enter(&self, qty: f64) -> bool {
        if self.disabled {
            return false;
        }
        let projected = self.position.qty.abs() + qty.abs();
        projected <= self.risk.inventory_limit + 1e-9
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

    fn executable_exit(&self, order_book: &OrderBook) -> Option<(f64, f64)> {
        order_book.executable_exit(
            self.position.side?,
            self.position.qty,
            self.cfg.max_fill_ratio,
            self.risk.slippage_limit_ticks,
            self.tick_size,
        )
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
        if qty <= 0.0 {
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
        if self.position.side == Some(PositionSide::Long) {
            let total_qty = self.position.qty + qty;
            let new_entry =
                (self.position.entry_price * self.position.qty + price * qty) / total_qty;
            self.position.qty = total_qty;
            self.position.entry_price = new_entry;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
        } else {
            self.position.side = Some(PositionSide::Long);
            self.position.qty = qty;
            self.position.entry_price = price;
            self.position.entry_ts = ts;
            self.position.reference_level = reference_level;
            self.position.reference_depth = reference_depth;
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
        order_book: &OrderBook,
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
            if let Some((fill_qty, fill_price)) = self.executable_exit(order_book) {
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
        let gross_pnl = match side {
            PositionSide::Short => (entry_price - price) * qty,
            PositionSide::Long => (price - entry_price) * qty,
        };
        let fees =
            (entry_price.abs() + price.abs()) * qty.abs() * self.cfg.fee_bps.max(0.0) / 10_000.0;
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

    fn clone_into_summary(&self, open_position_qty: f64) -> SummaryMetrics {
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
        }
    }
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
        ];
        let config = TargetPositionReplayConfig {
            max_depth_levels: 1,
            max_decision_delay_us: 1_000_000,
            position_notional_usd: 0.0,
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            additional_slippage_bps: 0.25,
            cross_spread: false,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let first = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();
        let second = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.snapshot_events, 1);
        assert_eq!(first.l2_update_events, 2);
        assert_eq!(first.trade_events, 0);
        assert_eq!(first.decision_count, 2);
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
    fn target_position_replay_uses_relative_drawdown_without_a_final_interval() {
        let tape = concat!(
            "{\"timestamp\":1000000,\"sequence\":1,\"event\":\"snapshot\",\"bids\":[[99,10]],\"asks\":[[101,10]]}\n",
            "{\"timestamp\":2000000,\"sequence\":2,\"event\":\"l2_update\",\"bids\":[[99,0],[109,10]],\"asks\":[[101,0],[111,10]]}\n",
            "{\"timestamp\":3000000,\"sequence\":3,\"event\":\"l2_update\",\"bids\":[[109,0],[99,10]],\"asks\":[[111,0],[101,10]]}\n",
        );
        let decisions =
            [1_000_000, 2_000_000, 3_000_000].map(|timestamp_us| TargetPositionDecision {
                timestamp_us,
                target_position: 1.0,
            });
        let config = TargetPositionReplayConfig {
            max_depth_levels: 1,
            max_decision_delay_us: 1,
            position_notional_usd: 0.0,
            fee_bps: 0.0,
            rebate_bps: 0.0,
            funding_bps: 10.0,
            latency_bps: 0.0,
            additional_slippage_bps: 0.0,
            cross_spread: false,
            capacity_depth_levels: 0,
            trade_tape_declared: false,
        };

        let metrics = replay_target_positions(tape.as_bytes(), &decisions, &config).unwrap();
        let expected_gain = 0.1 - 0.001;
        let expected_loss = 100.0 / 110.0 - 1.0 - 0.001;
        assert!((metrics.cumulative_net_return - (expected_gain + expected_loss)).abs() < 1e-12);
        assert!((metrics.max_drawdown - (-expected_loss / (1.0 + expected_gain))).abs() < 1e-12);
    }

    #[test]
    fn backtest_replays_in_memory_l2_events() {
        let mut engine = BacktestEngine::new(test_config());
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
        let mut engine = BacktestEngine::new(config);
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
    fn session_end_exit_respects_risk_slippage_ceiling() {
        let mut config = test_config();
        config.data.tick_size = 0.1;
        config.execution.max_fill_ratio = 1.0;
        config.risk.slippage_limit_ticks = 1.0;
        let mut engine = BacktestEngine::new(config);
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
    fn exit_respects_displayed_depth_and_leaves_residual_position() {
        let mut execution = ExecutionManager::new(
            ExecutionConfig {
                max_fill_ratio: 0.5,
                ..ExecutionConfig::default()
            },
            RiskConfig::default(),
            0.01,
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
}
