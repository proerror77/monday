//! DiffEnhanced strategy — crossover-based entry on diff (spot vs price_to_beat)
//! with trailing stop, floor stop, probability pullback, and stepped take-profit exits.
//!
//! Adapted from BTC5m-Dash S1Enhanced for the ploy trading system.
//! Diff is expressed as a percentage: `(spot - price_to_beat) / price_to_beat`.

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
pub struct DiffEnhancedConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,

    // Entry: time window (seconds remaining)
    #[serde(default = "default_entry_min_remaining")]
    pub entry_min_remaining_secs: u64,
    #[serde(default = "default_entry_max_remaining")]
    pub entry_max_remaining_secs: u64,

    // Entry: diff crossover threshold (percentage)
    #[serde(default = "default_diff_entry_threshold")]
    pub diff_entry_threshold: f64,

    // Entry: probability chase cap
    #[serde(default = "default_prob_chase_cap")]
    pub prob_chase_cap: f64,

    // Cooldown: overheat thresholds
    #[serde(default = "default_diff_overheat_threshold")]
    pub diff_overheat_threshold: f64,
    #[serde(default = "default_prob_overheat")]
    pub prob_overheat: f64,
    #[serde(default = "default_diff_neutral_threshold")]
    pub diff_neutral_threshold: f64,
    #[serde(default = "default_neutral_unlock_secs")]
    pub neutral_unlock_secs: u64,

    // Exit: trailing stop drawdown (percentage)
    #[serde(default = "default_diff_trailing_drawdown")]
    pub diff_trailing_drawdown: f64,

    // Exit: floor stop (percentage)
    #[serde(default = "default_diff_floor_stop")]
    pub diff_floor_stop: f64,

    // Exit: probability pullback take-profit
    #[serde(default = "default_prob_pullback_peak")]
    pub prob_pullback_peak: f64,
    #[serde(default = "default_prob_pullback_drop")]
    pub prob_pullback_drop: f64,

    // Exit: stepped take-profit range
    #[serde(default = "default_stepped_tp_low")]
    pub stepped_tp_low: f64,
    #[serde(default = "default_stepped_tp_high")]
    pub stepped_tp_high: f64,

    // Exit: force close and min hold
    #[serde(default = "default_force_close_secs")]
    pub force_close_secs: u64,
    #[serde(default = "default_min_hold_secs")]
    pub min_hold_secs: u64,

    // Position sizing
    #[serde(default = "default_stake_usd")]
    pub stake_usd: Decimal,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,
}

// ── Serde defaults ──────────────────────────────────────

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into()]
}
fn default_entry_min_remaining() -> u64 {
    50
}
fn default_entry_max_remaining() -> u64 {
    210
}
fn default_diff_entry_threshold() -> f64 {
    0.0005
}
fn default_prob_chase_cap() -> f64 {
    0.80
}
fn default_diff_overheat_threshold() -> f64 {
    0.0008
}
fn default_prob_overheat() -> f64 {
    0.85
}
fn default_diff_neutral_threshold() -> f64 {
    0.00035
}
fn default_neutral_unlock_secs() -> u64 {
    3
}
fn default_diff_trailing_drawdown() -> f64 {
    0.00028
}
fn default_diff_floor_stop() -> f64 {
    0.00007
}
fn default_prob_pullback_peak() -> f64 {
    0.85
}
fn default_prob_pullback_drop() -> f64 {
    0.08
}
fn default_stepped_tp_low() -> f64 {
    0.90
}
fn default_stepped_tp_high() -> f64 {
    1.00
}
fn default_force_close_secs() -> u64 {
    10
}
fn default_min_hold_secs() -> u64 {
    3
}
fn default_stake_usd() -> Decimal {
    Decimal::new(10, 0)
}
fn default_max_positions() -> usize {
    1000
}
fn default_max_daily_trades() -> u32 {
    1000
}

impl Default for DiffEnhancedConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            entry_min_remaining_secs: default_entry_min_remaining(),
            entry_max_remaining_secs: default_entry_max_remaining(),
            diff_entry_threshold: default_diff_entry_threshold(),
            prob_chase_cap: default_prob_chase_cap(),
            diff_overheat_threshold: default_diff_overheat_threshold(),
            prob_overheat: default_prob_overheat(),
            diff_neutral_threshold: default_diff_neutral_threshold(),
            neutral_unlock_secs: default_neutral_unlock_secs(),
            diff_trailing_drawdown: default_diff_trailing_drawdown(),
            diff_floor_stop: default_diff_floor_stop(),
            prob_pullback_peak: default_prob_pullback_peak(),
            prob_pullback_drop: default_prob_pullback_drop(),
            stepped_tp_low: default_stepped_tp_low(),
            stepped_tp_high: default_stepped_tp_high(),
            force_close_secs: default_force_close_secs(),
            min_hold_secs: default_min_hold_secs(),
            stake_usd: default_stake_usd(),
            max_positions: default_max_positions(),
            max_daily_trades: default_max_daily_trades(),
            allowed_window_secs: Vec::new(),
        }
    }
}

impl From<DirectionalConfig> for DiffEnhancedConfig {
    fn from(config: DirectionalConfig) -> Self {
        Self {
            symbols: config.symbols,
            entry_min_remaining_secs: config.min_time_remaining_secs,
            entry_max_remaining_secs: config.max_time_remaining_secs,
            stake_usd: config.stake_usd,
            max_positions: config.max_positions as usize,
            max_daily_trades: config.max_daily_trades,
            allowed_window_secs: config.allowed_window_secs,
            ..Self::default()
        }
    }
}

// ── Internal state structs ──────────────────────────────

#[derive(Clone, Copy)]
struct SpotState {
    price: Decimal,
}

#[derive(Clone)]
struct CooldownLock {
    #[allow(dead_code)]
    direction: Direction,
    #[allow(dead_code)]
    locked_at: DateTime<Utc>,
    neutral_since: Option<DateTime<Utc>>,
}

#[derive(Clone)]
struct HoldingState {
    token_id: Arc<str>,
    direction: Direction,
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    peak_diff: f64,
    peak_prob: f64,
}

// ── Strategy struct ─────────────────────────────────────

pub struct DiffEnhancedStrategy {
    config: DiffEnhancedConfig,
    spot: HashMap<Arc<str>, SpotState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    prev_diff: HashMap<Arc<str>, f64>,
    cooldown_locks: HashMap<(Arc<str>, Direction), CooldownLock>,
    holdings: HashMap<Arc<str>, HoldingState>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    retired_events: HashSet<Arc<str>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

impl DiffEnhancedStrategy {
    pub fn new(config: DiffEnhancedConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            prev_diff: HashMap::new(),
            cooldown_locks: HashMap::new(),
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

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        self.events
            .get(symbol)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| {
                        !self.retired_events.contains(&e.event_id)
                            && self.window_allowed(e.window_secs)
                            && e.end_time > now
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn resolve_up_won(&self, event: &EventWindow, settlement: Option<bool>) -> Option<bool> {
        settlement::resolve_up_won(
            settlement,
            self.spot.get(&event.symbol).map(|state| state.price),
            event.price_to_beat,
        )
    }

    /// Compute diff_pct = (spot - price_to_beat) / price_to_beat for an event.
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

    /// Check if a direction is cooldown-locked for a symbol.
    fn is_locked(&self, symbol: &Arc<str>, dir: Direction, now: DateTime<Utc>) -> bool {
        let key = (symbol.clone(), dir);
        let Some(lock) = self.cooldown_locks.get(&key) else {
            return false;
        };
        // Unlock when diff has been neutral for neutral_unlock_secs
        if let Some(neutral_since) = lock.neutral_since {
            let neutral_duration = (now - neutral_since).num_seconds();
            if neutral_duration >= self.config.neutral_unlock_secs as i64 {
                return false;
            }
        }
        true
    }

    /// Update cooldown lock state based on current diff and probability.
    fn update_cooldown(
        &mut self,
        symbol: &Arc<str>,
        diff: f64,
        now: DateTime<Utc>,
        up_prob: Option<f64>,
        down_prob: Option<f64>,
    ) {
        let abs_diff = diff.abs();

        // Check overheat: diff >= overheat AND prob >= prob_overheat
        // Or prob >= chase_cap (lock that direction)
        for (dir, prob) in [(Direction::Up, up_prob), (Direction::Down, down_prob)] {
            let key = (symbol.clone(), dir);
            let relevant_diff = match dir {
                Direction::Up => diff,
                Direction::Down => -diff,
            };

            if let Some(p) = prob {
                let overheated = relevant_diff >= self.config.diff_overheat_threshold
                    && p >= self.config.prob_overheat;
                let chasing = p >= self.config.prob_chase_cap;

                if overheated || chasing {
                    if !self.cooldown_locks.contains_key(&key) {
                        debug!(
                            symbol = symbol.as_ref(),
                            direction = dir.label(),
                            prob = p,
                            diff = relevant_diff,
                            "cooldown lock engaged"
                        );
                        self.cooldown_locks.insert(
                            key.clone(),
                            CooldownLock {
                                direction: dir,
                                locked_at: now,
                                neutral_since: None,
                            },
                        );
                    }
                }
            }

            // Track neutral return for unlock
            if let Some(lock) = self.cooldown_locks.get_mut(&key) {
                if abs_diff <= self.config.diff_neutral_threshold {
                    if lock.neutral_since.is_none() {
                        lock.neutral_since = Some(now);
                    }
                } else {
                    lock.neutral_since = None;
                }

                // Actually remove if unlocked
                if let Some(neutral_since) = lock.neutral_since {
                    if (now - neutral_since).num_seconds() >= self.config.neutral_unlock_secs as i64
                    {
                        debug!(
                            symbol = symbol.as_ref(),
                            direction = dir.label(),
                            "cooldown lock released"
                        );
                        self.cooldown_locks.remove(&key);
                    }
                }
            }
        }
    }

    /// Evaluate crossover entry for a single event.
    fn evaluate_entry(
        &self,
        event: &EventWindow,
        diff: f64,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        let remaining = (event.end_time - now).num_seconds();
        if remaining < self.config.entry_min_remaining_secs as i64
            || remaining > self.config.entry_max_remaining_secs as i64
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

        // Probability chase cap
        let prob = self.current_prob(token_id)?;
        if prob >= self.config.prob_chase_cap {
            debug!(
                event_id = event.event_id.as_ref(),
                direction = direction.label(),
                prob,
                "entry rejected: probability chase cap"
            );
            return None;
        }

        // Cooldown lock
        if self.is_locked(&event.symbol, direction, now) {
            debug!(
                event_id = event.event_id.as_ref(),
                direction = direction.label(),
                "entry rejected: cooldown locked"
            );
            return None;
        }

        // No duplicate position or pending order
        if positions.net_qty(token_id) > Decimal::ZERO || active_order_exists(token_id, orders) {
            return None;
        }

        let entry_price = Decimal::try_from(prob).ok()?;
        let quantity = self.entry_quantity(entry_price);
        if quantity <= Decimal::ZERO {
            return None;
        }

        let edge = diff.abs() - prob - crypto_fee_cost(prob);
        let intent_id = format!(
            "diff_enhanced_{}_{}_{}",
            event.event_id,
            direction.label().to_lowercase(),
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
            p_hat: prob,
            edge,
            entry_price,
            decision: "enter".to_string(),
            ts: now,
        };

        info!(
            event_id = event.event_id.as_ref(),
            direction = direction.label(),
            diff,
            prob,
            remaining,
            "diff_enhanced entry signal"
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

                let hold_secs = (now - holding.entry_time).num_seconds();
                let prob = self.current_prob(token_id).unwrap_or(0.0);

                // Force close: remaining <= force_close_secs
                if remaining <= self.config.force_close_secs as i64 {
                    debug!(token_id = token_id.as_ref(), "exit: force close");
                    decisions.push(self.build_exit(event, token_id, qty, "force_close", now));
                    continue;
                }

                // Floor stop: abs(diff) <= floor_stop → exit immediately (ignores min hold)
                if let Some(d) = diff {
                    let directional_diff = match dir {
                        Direction::Up => d,
                        Direction::Down => -d,
                    };
                    if directional_diff.abs() <= self.config.diff_floor_stop {
                        debug!(
                            token_id = token_id.as_ref(),
                            diff = directional_diff,
                            "exit: floor stop"
                        );
                        decisions.push(self.build_exit(event, token_id, qty, "floor_stop", now));
                        continue;
                    }
                }

                // Minimum hold time — only floor stop applies before this
                if hold_secs < self.config.min_hold_secs as i64 {
                    continue;
                }

                // Trailing stop: peak_diff - current_diff > trailing_drawdown
                if let Some(d) = diff {
                    let directional_diff = match dir {
                        Direction::Up => d,
                        Direction::Down => -d,
                    };
                    let drawdown = holding.peak_diff - directional_diff;
                    if drawdown > self.config.diff_trailing_drawdown {
                        debug!(
                            token_id = token_id.as_ref(),
                            peak_diff = holding.peak_diff,
                            current_diff = directional_diff,
                            drawdown,
                            "exit: trailing stop"
                        );
                        decisions.push(self.build_exit(event, token_id, qty, "trailing_stop", now));
                        continue;
                    }
                }

                // Probability pullback take-profit
                if holding.peak_prob >= self.config.prob_pullback_peak
                    && prob <= holding.peak_prob - self.config.prob_pullback_drop
                {
                    debug!(
                        token_id = token_id.as_ref(),
                        peak_prob = holding.peak_prob,
                        current_prob = prob,
                        "exit: probability pullback"
                    );
                    decisions.push(self.build_exit(event, token_id, qty, "prob_pullback", now));
                    continue;
                }

                // Stepped take-profit: linearly from stepped_tp_low to stepped_tp_high
                // as time progresses from entry to end_time
                let total_window = (event.end_time - holding.entry_time).num_seconds() as f64;
                if total_window > 0.0 {
                    let elapsed_frac = (hold_secs as f64 / total_window).clamp(0.0, 1.0);
                    let tp_threshold = self.config.stepped_tp_low
                        + (self.config.stepped_tp_high - self.config.stepped_tp_low) * elapsed_frac;
                    if prob >= tp_threshold {
                        debug!(
                            token_id = token_id.as_ref(),
                            prob, tp_threshold, "exit: stepped take-profit"
                        );
                        decisions.push(self.build_exit(event, token_id, qty, "stepped_tp", now));
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
                "diff_enhanced_{}_{}_{}",
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
                intent_id: format!("diff_enhanced_settle_{}_up", event.event_id),
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
                intent_id: format!("diff_enhanced_settle_{}_down", event.event_id),
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

    /// Update peak_diff and peak_prob for active holdings.
    fn update_holding_peaks(&mut self, symbol: &str) {
        let events = match self.events.get(symbol) {
            Some(e) => e.clone(),
            None => return,
        };

        for event in &events {
            let diff = self.diff_pct(event);

            for (token_id, dir) in [
                (&event.up_token, Direction::Up),
                (&event.down_token, Direction::Down),
            ] {
                if let Some(holding) = self.holdings.get_mut(token_id) {
                    if let Some(d) = diff {
                        let directional_diff = match dir {
                            Direction::Up => d,
                            Direction::Down => -d,
                        };
                        if directional_diff > holding.peak_diff {
                            holding.peak_diff = directional_diff;
                        }
                    }
                    // Inline current_prob to avoid borrow conflict
                    let prob = self
                        .quotes
                        .get(token_id)
                        .and_then(|q| q.ask)
                        .and_then(|a| a.to_f64());
                    if let Some(p) = prob {
                        if p > holding.peak_prob {
                            holding.peak_prob = p;
                        }
                    }
                }
            }
        }
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

        // Compute diff for each active event, update prev_diff, cooldowns, peaks
        let events_snapshot: Vec<EventWindow> =
            self.events.get(symbol).cloned().unwrap_or_default();

        for event in &events_snapshot {
            if self.retired_events.contains(&event.event_id) {
                continue;
            }
            if let Some(diff) = self.diff_pct(event) {
                let up_prob = self.current_prob(&event.up_token);
                let down_prob = self.current_prob(&event.down_token);
                self.update_cooldown(&event.symbol, diff, ts, up_prob, down_prob);
            }
        }

        self.update_holding_peaks(symbol);

        // Check exits first
        let exits = self.exit_decisions(symbol, ts, positions);
        if !exits.is_empty() {
            // Still update prev_diff before returning
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
                        // Only one entry per update
                        self.prev_diff.insert(event.event_id.clone(), diff);
                        // Update remaining prev_diffs
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

impl StrategyLogic for DiffEnhancedStrategy {
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

                // Update peak_prob for active holding
                if let Some(holding) = self.holdings.get_mut(token_id) {
                    if let Some(a) = ask.and_then(|v| v.to_f64()) {
                        if a > holding.peak_prob {
                            holding.peak_prob = a;
                        }
                    }
                }

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

                // Compute initial diff for peak tracking
                let initial_diff = self
                    .token_event
                    .get(&token_arc)
                    .and_then(|eid| self.prev_diff.get(eid).copied())
                    .unwrap_or(0.0)
                    .abs();

                let initial_prob = self
                    .current_prob(&token_arc)
                    .unwrap_or(fill.price.to_f64().unwrap_or(0.0));

                self.holdings
                    .entry(token_arc)
                    .and_modify(|h| {
                        // Average in
                        h.entry_price = fill.price;
                        h.entry_time = fill.timestamp;
                    })
                    .or_insert(HoldingState {
                        token_id: Arc::from(fill.token_id.as_str()),
                        direction,
                        entry_price: fill.price,
                        entry_time: fill.timestamp,
                        peak_diff: initial_diff,
                        peak_prob: initial_prob,
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
        "diff_enhanced"
    }
}

impl DiffEnhancedStrategy {
    /// Determine direction for a token by checking if it's an up or down token.
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
        Direction::Up // fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    fn base_config() -> DiffEnhancedConfig {
        DiffEnhancedConfig {
            symbols: vec!["BTCUSDT".into()],
            ..DiffEnhancedConfig::default()
        }
    }

    #[test]
    fn strategy_name() {
        let s = DiffEnhancedStrategy::new(base_config());
        assert_eq!(s.name(), "diff_enhanced");
    }

    #[test]
    fn crossover_entry_up() {
        let config = DiffEnhancedConfig {
            diff_entry_threshold: 0.0005,
            prob_chase_cap: 0.80,
            entry_min_remaining_secs: 50,
            entry_max_remaining_secs: 210,
            ..base_config()
        };
        let mut strategy = DiffEnhancedStrategy::new(config);
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
                price: dec!(70020), // diff = 20/70000 ≈ 0.000286 < 0.0005
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        // Quote for up token
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

        // Second spot: crosses above threshold
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(70040), // diff = 40/70000 ≈ 0.000571 > 0.0005
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected crossover entry UP, got {decisions:?}"
        );
    }

    #[test]
    fn chase_cap_blocks_entry() {
        let config = DiffEnhancedConfig {
            prob_chase_cap: 0.80,
            ..base_config()
        };
        let mut strategy = DiffEnhancedStrategy::new(config);
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
                price: dec!(70020),
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        // Quote with ask >= chase cap
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up2".into(),
                bid: Some(dec!(0.82)),
                ask: Some(dec!(0.85)),
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
                price: dec!(70040),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            !decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected chase cap rejection, got {decisions:?}"
        );
    }

    #[test]
    fn force_close_exit() {
        let config = DiffEnhancedConfig {
            force_close_secs: 10,
            ..base_config()
        };
        let mut strategy = DiffEnhancedStrategy::new(config);
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt3".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up3".into(),
                down_token: "dn3".into(),
                end_time: now + Duration::seconds(8), // 8s remaining < 10s
                window_secs: 300,
                price_to_beat: Some(dec!(70000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Simulate a fill to create holding
        let fill = FillRecord {
            fill_id: "f1".into(),
            order_id: "o1".into(),
            token_id: "up3".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.50),
            fee: Decimal::ZERO,
            timestamp: now - Duration::seconds(30),
        };
        positions.apply_fill(&fill);
        strategy.on_fill(&fill);

        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(70035),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Exit(..))),
            "expected force close exit, got {decisions:?}"
        );
    }
}
