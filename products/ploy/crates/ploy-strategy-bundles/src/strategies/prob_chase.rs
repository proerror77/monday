//! ProbChase strategy (S5) — fair-probability deviation entry.
//!
//! Adapted from BTC5m-Dash S5ProbChase for the ploy trading system.
//! Uses a historical fair-probability lookup table indexed by (diff_bucket,
//! remaining_bucket). When the market probability lags behind the fair value
//! by more than a configurable deviation threshold, the strategy enters.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::common::event::EventWindow;
use super::common::fees::crypto_fee_cost;
use super::common::guards::active_order_exists;
use super::common::quote::QuoteState;
use super::common::settlement;
use super::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

// ── Direction enum ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Direction {
    Up,
    Down,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Up => "UP",
            Direction::Down => "DOWN",
        }
    }
}

// ── Config ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FairProbEntry {
    pub diff_pct: f64,
    pub remaining_secs: u64,
    pub fair_prob: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProbChaseConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,

    // Entry: diff crossover threshold (percentage)
    #[serde(default = "default_diff_entry_threshold")]
    pub diff_entry_threshold: f64,
    // Entry: minimum deviation from fair probability to enter
    #[serde(default = "default_deviation_threshold")]
    pub deviation_threshold: f64,

    // Exit: deviation shrinks below this → take profit
    #[serde(default = "default_convergence_threshold")]
    pub convergence_threshold: f64,
    // Exit: deviation grows beyond entry_deviation * this → stop loss
    #[serde(default = "default_divergence_multiplier")]
    pub divergence_multiplier: f64,
    // Exit: max seconds to hold
    #[serde(default = "default_max_hold_secs")]
    pub max_hold_secs: u64,
    // Exit: close if remaining < this
    #[serde(default = "default_hold_to_expiry_secs")]
    pub hold_to_expiry_secs: u64,

    // Timing: entry window
    #[serde(default = "default_min_time")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time")]
    pub max_time_remaining_secs: u64,

    // Position sizing
    #[serde(default = "default_stake_usd")]
    pub stake_usd: Decimal,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,

    // Fair probability table (optional override)
    #[serde(default)]
    pub fair_prob_table: Option<Vec<FairProbEntry>>,
}

// ── Serde defaults ──────────────────────────────────────

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into()]
}
fn default_diff_entry_threshold() -> f64 {
    0.00035
}
fn default_deviation_threshold() -> f64 {
    0.10
}
fn default_convergence_threshold() -> f64 {
    0.03
}
fn default_divergence_multiplier() -> f64 {
    1.5
}
fn default_max_hold_secs() -> u64 {
    30
}
fn default_hold_to_expiry_secs() -> u64 {
    8
}
fn default_min_time() -> u64 {
    30
}
fn default_max_time() -> u64 {
    240
}
fn default_stake_usd() -> Decimal {
    Decimal::new(10, 0)
}
fn default_cooldown_secs() -> u64 {
    5
}
fn default_max_positions() -> usize {
    1000
}
fn default_max_daily_trades() -> u32 {
    1000
}

impl Default for ProbChaseConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            diff_entry_threshold: default_diff_entry_threshold(),
            deviation_threshold: default_deviation_threshold(),
            convergence_threshold: default_convergence_threshold(),
            divergence_multiplier: default_divergence_multiplier(),
            max_hold_secs: default_max_hold_secs(),
            hold_to_expiry_secs: default_hold_to_expiry_secs(),
            min_time_remaining_secs: default_min_time(),
            max_time_remaining_secs: default_max_time(),
            stake_usd: default_stake_usd(),
            cooldown_secs: default_cooldown_secs(),
            max_positions: default_max_positions(),
            max_daily_trades: default_max_daily_trades(),
            allowed_window_secs: Vec::new(),
            fair_prob_table: None,
        }
    }
}

impl From<DirectionalConfig> for ProbChaseConfig {
    fn from(config: DirectionalConfig) -> Self {
        Self {
            symbols: config.symbols,
            min_time_remaining_secs: config.min_time_remaining_secs,
            max_time_remaining_secs: config.max_time_remaining_secs,
            stake_usd: config.stake_usd,
            cooldown_secs: config.cooldown_secs,
            max_positions: config.max_positions as usize,
            max_daily_trades: config.max_daily_trades,
            allowed_window_secs: config.allowed_window_secs,
            ..Self::default()
        }
    }
}

// ── Default fair probability table ──────────────────────
//
// From 300k historical ticks. Symmetric: for negative diff use 1.0 - fair_prob.
// Rows: diff_pct buckets, Columns: remaining_secs buckets.
//
//                30s    60s    90s    120s   180s   240s
// diff 0.035%:  0.62   0.60   0.58   0.57   0.56   0.55
// diff 0.050%:  0.68   0.65   0.63   0.61   0.59   0.58
// diff 0.071%:  0.75   0.72   0.69   0.67   0.64   0.62
// diff 0.100%:  0.82   0.78   0.75   0.72   0.69   0.67
// diff 0.140%:  0.88   0.85   0.82   0.79   0.75   0.72
// diff 0.200%:  0.93   0.90   0.88   0.85   0.82   0.79

const DEFAULT_DIFF_BUCKETS: [f64; 6] = [0.00035, 0.0005, 0.00071, 0.001, 0.0014, 0.002];
const DEFAULT_REM_BUCKETS: [u64; 6] = [30, 60, 90, 120, 180, 240];

#[rustfmt::skip]
const DEFAULT_FAIR_PROBS: [[f64; 6]; 6] = [
    // 30s    60s    90s    120s   180s   240s
    [0.62,  0.60,  0.58,  0.57,  0.56,  0.55], // diff 0.035%
    [0.68,  0.65,  0.63,  0.61,  0.59,  0.58], // diff 0.050%
    [0.75,  0.72,  0.69,  0.67,  0.64,  0.62], // diff 0.071%
    [0.82,  0.78,  0.75,  0.72,  0.69,  0.67], // diff 0.100%
    [0.88,  0.85,  0.82,  0.79,  0.75,  0.72], // diff 0.140%
    [0.93,  0.90,  0.88,  0.85,  0.82,  0.79], // diff 0.200%
];

fn build_default_table() -> Vec<(f64, u64, f64)> {
    let mut table = Vec::with_capacity(36);
    for (di, &diff) in DEFAULT_DIFF_BUCKETS.iter().enumerate() {
        for (ri, &rem) in DEFAULT_REM_BUCKETS.iter().enumerate() {
            table.push((diff, rem, DEFAULT_FAIR_PROBS[di][ri]));
        }
    }
    table
}

fn build_table_from_entries(entries: &[FairProbEntry]) -> Vec<(f64, u64, f64)> {
    entries
        .iter()
        .map(|e| (e.diff_pct, e.remaining_secs, e.fair_prob))
        .collect()
}

// ── Fair probability lookup ─────────────────────────────

/// Bilinear interpolation lookup for fair probability.
///
/// `abs_diff_pct` is the absolute value of diff (always positive).
/// `remaining_secs` is seconds until event expiry.
/// Returns the fair probability for the UP direction; caller uses
/// `1.0 - fair_prob` for DOWN direction.
fn lookup_fair_prob(table: &[(f64, u64, f64)], abs_diff_pct: f64, remaining_secs: u64) -> f64 {
    // Collect unique sorted buckets
    let mut diff_buckets: Vec<f64> = table.iter().map(|t| t.0).collect();
    diff_buckets.sort_by(|a, b| a.partial_cmp(b).unwrap());
    diff_buckets.dedup();

    let mut rem_buckets: Vec<u64> = table.iter().map(|t| t.1).collect();
    rem_buckets.sort();
    rem_buckets.dedup();

    if diff_buckets.is_empty() || rem_buckets.is_empty() {
        return 0.5;
    }

    // Clamp to table bounds
    let d = abs_diff_pct.clamp(diff_buckets[0], *diff_buckets.last().unwrap());
    let r =
        (remaining_secs as f64).clamp(rem_buckets[0] as f64, *rem_buckets.last().unwrap() as f64);

    // Find bracketing indices for diff
    let (di_lo, di_hi) = find_bracket_f64(&diff_buckets, d);
    // Find bracketing indices for remaining
    let (ri_lo, ri_hi) = find_bracket_u64(&rem_buckets, remaining_secs);

    // Build a quick lookup closure
    let prob_at = |di: usize, ri: usize| -> f64 {
        let diff_val = diff_buckets[di];
        let rem_val = rem_buckets[ri];
        table
            .iter()
            .find(|t| (t.0 - diff_val).abs() < 1e-10 && t.1 == rem_val)
            .map(|t| t.2)
            .unwrap_or(0.5)
    };

    // Bilinear interpolation
    let d_lo = diff_buckets[di_lo];
    let d_hi = diff_buckets[di_hi];
    let r_lo = rem_buckets[ri_lo] as f64;
    let r_hi = rem_buckets[ri_hi] as f64;

    let t_d = if (d_hi - d_lo).abs() < 1e-15 {
        0.0
    } else {
        (d - d_lo) / (d_hi - d_lo)
    };
    let t_r = if (r_hi - r_lo).abs() < 1e-15 {
        0.0
    } else {
        (r - r_lo) / (r_hi - r_lo)
    };

    let p00 = prob_at(di_lo, ri_lo);
    let p10 = prob_at(di_hi, ri_lo);
    let p01 = prob_at(di_lo, ri_hi);
    let p11 = prob_at(di_hi, ri_hi);

    let p0 = p00 + t_d * (p10 - p00);
    let p1 = p01 + t_d * (p11 - p01);
    p0 + t_r * (p1 - p0)
}

fn find_bracket_f64(sorted: &[f64], val: f64) -> (usize, usize) {
    if sorted.len() <= 1 {
        return (0, 0);
    }
    for i in 0..sorted.len() - 1 {
        if val <= sorted[i + 1] {
            return (i, i + 1);
        }
    }
    let last = sorted.len() - 1;
    (last, last)
}

fn find_bracket_u64(sorted: &[u64], val: u64) -> (usize, usize) {
    if sorted.len() <= 1 {
        return (0, 0);
    }
    for i in 0..sorted.len() - 1 {
        if val <= sorted[i + 1] {
            return (i, i + 1);
        }
    }
    let last = sorted.len() - 1;
    (last, last)
}

// ── Internal state structs ──────────────────────────────

#[derive(Clone, Copy)]
struct SpotState {
    price: Decimal,
}

#[derive(Clone)]
struct HoldingState {
    token_id: Arc<str>,
    direction: Direction,
    entry_time: DateTime<Utc>,
    entry_deviation: f64,
}

// ── Strategy struct ─────────────────────────────────────

pub struct ProbChaseStrategy {
    config: ProbChaseConfig,
    fair_table: Vec<(f64, u64, f64)>,
    spot: HashMap<Arc<str>, SpotState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    prev_diff: HashMap<Arc<str>, f64>,
    holdings: HashMap<Arc<str>, HoldingState>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    retired_events: HashSet<Arc<str>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

// ── Strategy impl ───────────────────────────────────────

impl ProbChaseStrategy {
    pub fn new(config: ProbChaseConfig) -> Self {
        let fair_table = match &config.fair_prob_table {
            Some(entries) => build_table_from_entries(entries),
            None => build_default_table(),
        };
        Self {
            config,
            fair_table,
            spot: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            prev_diff: HashMap::new(),
            holdings: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            last_entry: HashMap::new(),
            retired_events: HashSet::new(),
            daily_trade_count: 0,
            daily_reset_date: None,
        }
    }

    fn reset_daily_counter(&mut self, now: DateTime<Utc>) {
        let today = now.date_naive();
        if self.daily_reset_date != Some(today) {
            self.daily_trade_count = 0;
            self.daily_reset_date = Some(today);
        }
    }

    fn window_allowed(&self, window_secs: u64) -> bool {
        self.config.allowed_window_secs.is_empty()
            || self.config.allowed_window_secs.contains(&window_secs)
    }

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    /// Compute diff_pct = (spot - price_to_beat) / price_to_beat.
    fn diff_pct(&self, event: &EventWindow) -> Option<f64> {
        let ptb = event.price_to_beat?.to_f64()?;
        if ptb <= 0.0 {
            return None;
        }
        let spot = self.spot.get(&event.symbol)?.price.to_f64()?;
        Some((spot - ptb) / ptb)
    }

    /// Current probability (ask price) for a token.
    fn current_prob(&self, token_id: &Arc<str>) -> Option<f64> {
        self.quotes.get(token_id)?.ask?.to_f64()
    }

    fn resolve_up_won(&self, event: &EventWindow, settlement: Option<bool>) -> Option<bool> {
        settlement::resolve_up_won(
            settlement,
            self.spot.get(&event.symbol).map(|state| state.price),
            event.price_to_beat,
        )
    }

    /// Determine direction for a token by checking event up/down tokens.
    fn determine_direction(&self, token_id: &Arc<str>) -> Direction {
        for events in self.events.values() {
            for event in events {
                if *token_id == event.up_token {
                    return Direction::Up;
                }
                if *token_id == event.down_token {
                    return Direction::Down;
                }
            }
        }
        Direction::Up
    }

    /// Evaluate entry for a single event based on fair-probability deviation.
    fn evaluate_entry(
        &self,
        event: &EventWindow,
        diff: f64,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        let remaining = (event.end_time - now).num_seconds();
        if remaining < self.config.min_time_remaining_secs as i64
            || remaining > self.config.max_time_remaining_secs as i64
        {
            return None;
        }

        // Crossover detection
        let prev = *self.prev_diff.get(&event.event_id)?;
        let threshold = self.config.diff_entry_threshold;

        let (direction, token_id) = if prev <= threshold && diff > threshold {
            (Direction::Up, &event.up_token)
        } else if prev >= -threshold && diff < -threshold {
            (Direction::Down, &event.down_token)
        } else {
            return None;
        };

        // Look up fair probability for this (diff, remaining) point
        let abs_diff = diff.abs();
        let fair_prob = lookup_fair_prob(&self.fair_table, abs_diff, remaining as u64);

        // For DOWN direction, fair prob for the down token = 1.0 - fair_prob
        let fair_prob_for_token = match direction {
            Direction::Up => fair_prob,
            Direction::Down => 1.0 - fair_prob,
        };

        // Get actual market probability
        let actual_prob = self.current_prob(token_id)?;

        // Deviation = fair_prob - actual_prob (positive means market is slow)
        let deviation = fair_prob_for_token - actual_prob;
        if deviation < self.config.deviation_threshold {
            debug!(
                event_id = event.event_id.as_ref(),
                direction = direction.label(),
                fair_prob = fair_prob_for_token,
                actual_prob,
                deviation,
                threshold = self.config.deviation_threshold,
                "entry rejected: insufficient deviation from fair value"
            );
            return None;
        }

        // Cooldown check
        if let Some(last) = self.last_entry.get(&event.symbol) {
            let elapsed = (now - *last).num_seconds();
            if elapsed < self.config.cooldown_secs as i64 {
                return None;
            }
        }

        // No duplicate position or pending order
        if positions.net_qty(token_id) > Decimal::ZERO || active_order_exists(token_id, orders) {
            return None;
        }

        let entry_price = Decimal::try_from(actual_prob).ok()?;
        let quantity = self.entry_quantity(entry_price);
        if quantity <= Decimal::ZERO {
            return None;
        }

        let edge = deviation - crypto_fee_cost(actual_prob);
        let direction_str = direction.label().to_lowercase();
        let intent_id = format!(
            "prob_chase_{}_{}_{}",
            event.event_id,
            direction_str,
            now.timestamp_millis()
        );

        let intent = TradingIntent {
            intent_id: intent_id.clone(),
            deployment_id: String::new(),
            market_id: event.event_id.to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity,
            limit_price: Some(entry_price),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };

        let signal = SignalRecord {
            strategy: self.name().to_string(),
            event_id: Some(event.event_id.to_string()),
            token_id: Some(token_id.to_string()),
            intent_id: Some(intent_id),
            symbol: event.symbol.to_string(),
            direction: direction.label().to_string(),
            p_hat: fair_prob_for_token,
            edge,
            entry_price,
            decision: "enter".to_string(),
            ts: now,
        };

        info!(
            event_id = event.event_id.as_ref(),
            direction = direction.label(),
            diff,
            fair_prob = fair_prob_for_token,
            actual_prob,
            deviation,
            remaining,
            "prob_chase entry signal"
        );

        Some(StrategyDecision::Enter {
            intent,
            signal: Some(signal),
        })
    }

    /// Evaluate exit conditions for all holdings on a given symbol.
    fn exit_decisions(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        for event in self.events.get(symbol).into_iter().flatten() {
            if self.retired_events.contains(&event.event_id) {
                continue;
            }

            let remaining = (event.end_time - now).num_seconds();
            let diff = self.diff_pct(event);

            for (token_id, dir) in [
                (&event.up_token, Direction::Up),
                (&event.down_token, Direction::Down),
            ] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }

                let holding = match self.holdings.get(token_id) {
                    Some(h) => h,
                    None => continue,
                };

                // Stop-loss: diff reverses (trend gone)
                if let Some(d) = diff {
                    let directional_diff = match dir {
                        Direction::Up => d,
                        Direction::Down => -d,
                    };
                    if directional_diff <= 0.0 {
                        debug!(
                            token_id = token_id.as_ref(),
                            diff = directional_diff,
                            "exit: diff reversed"
                        );
                        decisions.push(self.build_exit(event, token_id, qty, "diff_reversed", now));
                        continue;
                    }
                }

                // Compute current deviation from fair value
                if let Some(d) = diff {
                    let abs_diff = d.abs();
                    let rem_u64 = remaining.max(0) as u64;
                    let fair_prob = lookup_fair_prob(&self.fair_table, abs_diff, rem_u64);
                    let fair_for_token = match dir {
                        Direction::Up => fair_prob,
                        Direction::Down => 1.0 - fair_prob,
                    };
                    let actual_prob = self.current_prob(token_id).unwrap_or(0.0);
                    let current_deviation = fair_for_token - actual_prob;

                    // Take-profit: deviation converged
                    if current_deviation < self.config.convergence_threshold {
                        debug!(
                            token_id = token_id.as_ref(),
                            current_deviation, "exit: convergence take-profit"
                        );
                        decisions.push(self.build_exit(
                            event,
                            token_id,
                            qty,
                            "convergence_tp",
                            now,
                        ));
                        continue;
                    }

                    // Stop-loss: deviation widened beyond 1.5x entry
                    let max_deviation = holding.entry_deviation * self.config.divergence_multiplier;
                    if current_deviation > max_deviation {
                        debug!(
                            token_id = token_id.as_ref(),
                            current_deviation,
                            entry_deviation = holding.entry_deviation,
                            max_deviation,
                            "exit: divergence stop-loss"
                        );
                        decisions.push(self.build_exit(event, token_id, qty, "divergence_sl", now));
                        continue;
                    }
                }
            }
        }

        decisions
    }

    fn build_exit(
        &self,
        event: &EventWindow,
        token_id: &Arc<str>,
        qty: Decimal,
        reason: &str,
        now: DateTime<Utc>,
    ) -> StrategyDecision {
        let bid = self.quotes.get(token_id).and_then(|q| q.bid);
        StrategyDecision::Exit(TradingIntent {
            intent_id: format!(
                "prob_chase_{}_{}_{}",
                reason,
                token_id,
                now.timestamp_millis()
            ),
            deployment_id: String::new(),
            market_id: event.event_id.to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Sell,
            quantity: qty,
            limit_price: bid,
            purpose: IntentPurpose::Exit,
            created_at: now,
        })
    }

    fn build_settlement_exits(
        &self,
        event: &EventWindow,
        up_won: bool,
        created_at: DateTime<Utc>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        if positions.net_qty(&event.up_token) > Decimal::ZERO {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("prob_chase_settle_{}_up", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.up_token.to_string(),
                side: TradeSide::Sell,
                quantity: positions.net_qty(&event.up_token),
                limit_price: Some(if up_won {
                    Decimal::new(1, 0)
                } else {
                    Decimal::ZERO
                }),
                purpose: IntentPurpose::Exit,
                created_at,
            }));
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("prob_chase_settle_{}_down", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.down_token.to_string(),
                side: TradeSide::Sell,
                quantity: positions.net_qty(&event.down_token),
                limit_price: Some(if up_won {
                    Decimal::ZERO
                } else {
                    Decimal::new(1, 0)
                }),
                purpose: IntentPurpose::Exit,
                created_at,
            }));
        }

        exits
    }

    fn handle_spot(
        &mut self,
        symbol: &str,
        price: Decimal,
        ts: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        if !self.config.symbols.iter().any(|s| s.as_str() == symbol) {
            return Vec::new();
        }

        self.reset_daily_counter(ts);
        self.spot.insert(Arc::from(symbol), SpotState { price });

        // Backfill price_to_beat for events that don't have one yet
        if let Some(events) = self.events.get_mut(symbol) {
            for event in events.iter_mut() {
                if event.price_to_beat.is_none() {
                    event.price_to_beat = Some(price);
                }
            }
        }

        let events_snapshot: Vec<EventWindow> =
            self.events.get(symbol).cloned().unwrap_or_default();

        // Check exits first
        let exits = self.exit_decisions(symbol, ts, positions);
        if !exits.is_empty() {
            for event in &events_snapshot {
                if let Some(diff) = self.diff_pct(event) {
                    self.prev_diff.insert(event.event_id.clone(), diff);
                }
            }
            return exits;
        }

        // Check entries
        if self.daily_trade_count >= self.config.max_daily_trades
            || positions.positions().count() >= self.config.max_positions
        {
            for event in &events_snapshot {
                if let Some(diff) = self.diff_pct(event) {
                    self.prev_diff.insert(event.event_id.clone(), diff);
                }
            }
            return Vec::new();
        }

        let mut result = Vec::new();
        for event in &events_snapshot {
            if self.retired_events.contains(&event.event_id) {
                continue;
            }
            if let Some(diff) = self.diff_pct(event) {
                if self.prev_diff.contains_key(&event.event_id) {
                    if let Some(decision) = self.evaluate_entry(event, diff, ts, positions, orders)
                    {
                        result.push(decision);
                        self.prev_diff.insert(event.event_id.clone(), diff);
                        for e2 in &events_snapshot {
                            if e2.event_id != event.event_id {
                                if let Some(d2) = self.diff_pct(e2) {
                                    self.prev_diff.insert(e2.event_id.clone(), d2);
                                }
                            }
                        }
                        return result;
                    }
                }
                self.prev_diff.insert(event.event_id.clone(), diff);
            }
        }

        result
    }
}

// ── StrategyLogic impl ──────────────────────────────────

impl StrategyLogic for ProbChaseStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                self.handle_spot(symbol, *price, *ts, positions, orders)
            }
            MarketUpdate::Quote {
                token_id,
                bid,
                ask,
                ts,
                ..
            } => {
                self.quotes.insert(
                    token_id.clone(),
                    QuoteState {
                        bid: *bid,
                        ask: *ask,
                        ts: *ts,
                    },
                );

                // Check exits on quote update
                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                self.exit_decisions(&symbol, *ts, positions)
            }
            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs,
                price_to_beat,
                ..
            } => {
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                    || !self.window_allowed(*window_secs)
                {
                    return Vec::new();
                }

                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());
                self.token_event.insert(up_token.clone(), event_id.clone());
                self.token_event
                    .insert(down_token.clone(), event_id.clone());

                self.events
                    .entry(symbol.clone())
                    .or_default()
                    .push(EventWindow {
                        event_id: event_id.clone(),
                        symbol: symbol.clone(),
                        up_token: up_token.clone(),
                        down_token: down_token.clone(),
                        end_time: *end_time,
                        window_secs: *window_secs,
                        price_to_beat: *price_to_beat,
                    });

                // Seed prev_diff so crossover detection works on next tick
                if let (Some(ptb), Some(spot)) = (
                    price_to_beat.and_then(|p| p.to_f64()),
                    self.spot.get(symbol).and_then(|s| s.price.to_f64()),
                ) {
                    if ptb > 0.0 {
                        self.prev_diff.insert(event_id.clone(), (spot - ptb) / ptb);
                    }
                }

                Vec::new()
            }
            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won,
            } => {
                let mut decisions = Vec::new();
                let mut resolved = Vec::new();

                for events in self.events.values() {
                    for event in events {
                        if event.event_id != *event_id {
                            continue;
                        }
                        if let Some(up_won) = self.resolve_up_won(event, *resolved_up_won) {
                            decisions.extend(
                                self.build_settlement_exits(event, up_won, *end_time, positions),
                            );
                            resolved.push(event.event_id.clone());
                        }
                    }
                }

                if !resolved.is_empty() {
                    for events in self.events.values_mut() {
                        events.retain(|e| !resolved.contains(&e.event_id));
                    }
                    for eid in &resolved {
                        self.prev_diff.remove(eid);
                        self.retired_events.insert(eid.clone());
                    }
                }

                decisions
            }
            _ => Vec::new(),
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        match fill.side {
            TradeSide::Buy => {
                if let Some(symbol) = self.token_symbol.get(fill.token_id.as_str()).cloned() {
                    self.last_entry.insert(symbol, fill.timestamp);
                }
                self.daily_trade_count += 1;

                let token_arc: Arc<str> = Arc::from(fill.token_id.as_str());
                let direction = self.determine_direction(&token_arc);

                // Compute entry deviation for exit tracking
                let entry_deviation = self
                    .token_event
                    .get(&token_arc)
                    .and_then(|eid| {
                        // Find the event to compute fair prob at entry
                        let event = self
                            .events
                            .values()
                            .flatten()
                            .find(|e| e.event_id == *eid)?;
                        let diff = self.diff_pct(event)?;
                        let abs_diff = diff.abs();
                        let remaining =
                            (event.end_time - fill.timestamp).num_seconds().max(0) as u64;
                        let fair_prob = lookup_fair_prob(&self.fair_table, abs_diff, remaining);
                        let fair_for_token = match direction {
                            Direction::Up => fair_prob,
                            Direction::Down => 1.0 - fair_prob,
                        };
                        let actual_prob = self.current_prob(&token_arc)?;
                        Some(fair_for_token - actual_prob)
                    })
                    .unwrap_or(self.config.deviation_threshold);

                self.holdings
                    .entry(token_arc)
                    .and_modify(|h| {
                        h.entry_time = fill.timestamp;
                        h.entry_deviation = entry_deviation;
                    })
                    .or_insert(HoldingState {
                        token_id: Arc::from(fill.token_id.as_str()),
                        direction,
                        entry_time: fill.timestamp,
                        entry_deviation,
                    });
            }
            TradeSide::Sell => {
                let token_arc: Arc<str> = Arc::from(fill.token_id.as_str());
                self.holdings.remove(&token_arc);
                if let Some(event_id) = self.token_event.get(&token_arc).cloned() {
                    self.retired_events.insert(event_id);
                }
            }
        }
    }

    fn name(&self) -> &str {
        "prob_chase"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    fn base_config() -> ProbChaseConfig {
        ProbChaseConfig {
            symbols: vec!["BTCUSDT".into()],
            ..ProbChaseConfig::default()
        }
    }

    #[test]
    fn strategy_name() {
        let s = ProbChaseStrategy::new(base_config());
        assert_eq!(s.name(), "prob_chase");
    }

    #[test]
    fn default_table_has_36_entries() {
        let table = build_default_table();
        assert_eq!(table.len(), 36);
    }

    #[test]
    fn lookup_interpolates_within_bounds() {
        let table = build_default_table();
        // Exact match: diff=0.001, rem=120 → 0.72
        let p = lookup_fair_prob(&table, 0.001, 120);
        assert!((p - 0.72).abs() < 0.001, "expected ~0.72, got {p}");

        // Interpolated: diff=0.00075 (between 0.00071 and 0.001 buckets), rem=90
        let p2 = lookup_fair_prob(&table, 0.00075, 90);
        assert!(
            p2 > 0.69 && p2 < 0.75,
            "expected between 0.69 and 0.75, got {p2}"
        );
    }

    #[test]
    fn entry_on_deviation() {
        let config = ProbChaseConfig {
            diff_entry_threshold: 0.00035,
            deviation_threshold: 0.05, // lower threshold for test
            min_time_remaining_secs: 30,
            max_time_remaining_secs: 240,
            cooldown_secs: 0,
            ..base_config()
        };
        let mut strategy = ProbChaseStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // Discover event
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + Duration::seconds(120),
                window_secs: 300,
                price_to_beat: Some(dec!(70000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // First spot: below threshold (seeds prev_diff)
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(70010), // diff ≈ 0.000143 < 0.00035
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        // Quote for up token — deliberately low so deviation is large
        // Fair prob at diff~0.001, rem~120 is ~0.72, so ask=0.50 gives deviation=0.22
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.48)),
                ask: Some(dec!(0.50)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now - Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        // Second spot: crosses above threshold with large diff
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(70070), // diff = 70/70000 = 0.001 > 0.00035
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected prob_chase entry, got {decisions:?}"
        );
    }

    #[test]
    fn no_entry_when_prob_already_fair() {
        let config = ProbChaseConfig {
            diff_entry_threshold: 0.00035,
            deviation_threshold: 0.10,
            ..base_config()
        };
        let mut strategy = ProbChaseStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt2".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up2".into(),
                down_token: "dn2".into(),
                end_time: now + Duration::seconds(120),
                window_secs: 300,
                price_to_beat: Some(dec!(70000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(70010),
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        // Quote with ask already at fair value — no deviation
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up2".into(),
                bid: Some(dec!(0.70)),
                ask: Some(dec!(0.72)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now - Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(70070),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            !decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected no entry when prob is already fair, got {decisions:?}"
        );
    }
}
