//! Sweep strategy (S3) — tail-window high-conviction directional trades.
//!
//! The simplest strategy in the bundle: trades only in the final 60 seconds
//! of an event window when the spot-vs-price_to_beat diff is very large.
//! No crossover requirement, no cooldown locks — just a threshold check.
//!
//! Implements [`StrategyLogic`] so it plugs into [`StrategyRuntime`]
//! for backtest, dry-run, and live modes identically.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::common::event::EventWindow;
use super::common::fees::crypto_fee_cost;
use super::common::guards::active_order_exists;
use super::common::quote::QuoteState;
use super::common::settlement;
use super::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

// ── Direction ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

// ── Configuration ───────────────────────────────────────

/// Sweep strategy configuration, loadable from TOML.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SweepConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,
    /// Minimum |diff_pct| to trigger entry (default ≈ $50 on BTC).
    #[serde(default = "default_diff_sweep_threshold")]
    pub diff_sweep_threshold: f64,
    /// Maximum probability (ask price) to enter — still room to profit.
    #[serde(default = "default_prob_cap")]
    pub prob_cap: f64,
    /// Floor stop: exit when |diff_pct| drops below this.
    #[serde(default = "default_diff_floor_stop")]
    pub diff_floor_stop: f64,
    // Segmented take-profit thresholds
    /// Exit at this prob when remaining >= 40s.
    #[serde(default = "default_tp_40s")]
    pub tp_prob_40s: f64,
    /// Exit at this prob when remaining >= 20s.
    #[serde(default = "default_tp_20s")]
    pub tp_prob_20s: f64,
    /// Exit at this prob when remaining >= 10s (1.0 = never exit early).
    #[serde(default = "default_tp_10s")]
    pub tp_prob_10s: f64,
    /// Hold to expiry when remaining < this many seconds.
    #[serde(default = "default_hold_to_expiry_secs")]
    pub hold_to_expiry_secs: u64,
    // Timing
    /// Minimum seconds remaining to enter (default 0 — trade right up to expiry).
    #[serde(default = "default_min_time_remaining")]
    pub min_time_remaining_secs: u64,
    /// Maximum seconds remaining to enter (default 60 — tail window only).
    #[serde(default = "default_max_time_remaining")]
    pub max_time_remaining_secs: u64,
    // Sizing
    #[serde(default = "default_stake_usd")]
    pub stake_usd: Decimal,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
    /// Only trade markets whose total window duration (seconds) is in this list.
    /// Empty = no filter (trade all windows).
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,
}

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()]
}
fn default_diff_sweep_threshold() -> f64 {
    0.00071
}
fn default_prob_cap() -> f64 {
    0.95
}
fn default_diff_floor_stop() -> f64 {
    0.00007
}
fn default_tp_40s() -> f64 {
    0.98
}
fn default_tp_20s() -> f64 {
    0.99
}
fn default_tp_10s() -> f64 {
    1.00
}
fn default_hold_to_expiry_secs() -> u64 {
    10
}
fn default_min_time_remaining() -> u64 {
    0
}
fn default_max_time_remaining() -> u64 {
    60
}
fn default_stake_usd() -> Decimal {
    dec!(25)
}
fn default_max_positions() -> usize {
    1000
}
fn default_max_daily_trades() -> u32 {
    1000
}

impl Default for SweepConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            diff_sweep_threshold: default_diff_sweep_threshold(),
            prob_cap: default_prob_cap(),
            diff_floor_stop: default_diff_floor_stop(),
            tp_prob_40s: default_tp_40s(),
            tp_prob_20s: default_tp_20s(),
            tp_prob_10s: default_tp_10s(),
            hold_to_expiry_secs: default_hold_to_expiry_secs(),
            min_time_remaining_secs: default_min_time_remaining(),
            max_time_remaining_secs: default_max_time_remaining(),
            stake_usd: default_stake_usd(),
            max_positions: default_max_positions(),
            max_daily_trades: default_max_daily_trades(),
            allowed_window_secs: vec![],
        }
    }
}

impl From<DirectionalConfig> for SweepConfig {
    fn from(config: DirectionalConfig) -> Self {
        Self {
            symbols: config.symbols,
            min_time_remaining_secs: config.min_time_remaining_secs,
            max_time_remaining_secs: config.max_time_remaining_secs,
            stake_usd: config.stake_usd,
            max_positions: config.max_positions as usize,
            max_daily_trades: config.max_daily_trades,
            allowed_window_secs: config.allowed_window_secs,
            ..Self::default()
        }
    }
}

// ── Internal State ──────────────────────────────────────

/// Cached CEX spot price.
struct SpotState {
    price: Decimal,
}

/// Tracked holding for exit management.
struct HoldingState {
    token_id: Arc<str>,
    direction: Direction,
    entry_time: DateTime<Utc>,
}

// ── Strategy Implementation ─────────────────────────────

pub struct SweepStrategy {
    config: SweepConfig,
    spot: HashMap<Arc<str>, SpotState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    holdings: HashMap<Arc<str>, HoldingState>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    retired_events: HashSet<Arc<str>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

impl SweepStrategy {
    pub fn new(config: SweepConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            holdings: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
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

    fn event_has_open_position(&self, event: &EventWindow, positions: &PositionLedger) -> bool {
        positions.net_qty(&event.up_token) > Decimal::ZERO
            || positions.net_qty(&event.down_token) > Decimal::ZERO
    }

    fn event_has_active_order(&self, event: &EventWindow, orders: &OrderLedger) -> bool {
        active_order_exists(&event.up_token, orders)
            || active_order_exists(&event.down_token, orders)
    }

    /// Find candidate events in the sweep tail window.
    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        let min_time = self.config.min_time_remaining_secs as i64;
        let max_time = self.config.max_time_remaining_secs as i64;
        self.events
            .get(symbol)
            .map(|events| {
                let mut candidates: Vec<EventWindow> = events
                    .iter()
                    .filter(|e| {
                        let rem = (e.end_time - now).num_seconds();
                        rem >= min_time && rem <= max_time
                    })
                    .cloned()
                    .collect();
                candidates.sort_by_key(|e| e.end_time);
                candidates
            })
            .unwrap_or_default()
    }

    /// Compute diff_pct = (spot - price_to_beat) / price_to_beat.
    fn diff_pct(&self, symbol: &str, price_to_beat: Decimal) -> Option<f64> {
        let spot = self.spot.get(symbol)?.price;
        let ptb = price_to_beat.to_f64()?;
        if ptb <= 0.0 {
            return None;
        }
        Some((spot.to_f64()? - ptb) / ptb)
    }

    /// Evaluate sweep entry for a single event.
    fn evaluate_entry(
        &self,
        symbol: &str,
        event: &EventWindow,
        _now: DateTime<Utc>,
    ) -> Option<(Direction, Decimal, f64, f64)> {
        let price_to_beat = event.price_to_beat?;
        let diff = self.diff_pct(symbol, price_to_beat)?;
        let abs_diff = diff.abs();

        // Gate 1: diff must exceed sweep threshold
        if abs_diff <= self.config.diff_sweep_threshold {
            debug!(
                symbol,
                event_id = %event.event_id,
                abs_diff = format!("{:.6}", abs_diff),
                threshold = format!("{:.6}", self.config.diff_sweep_threshold),
                "Sweep: diff below threshold"
            );
            return None;
        }

        // Gate 2: direction from diff sign
        let direction = if diff > 0.0 {
            Direction::Up
        } else {
            Direction::Down
        };

        // Gate 3: get entry price (ask of the target token)
        let token_id = match direction {
            Direction::Up => &event.up_token,
            Direction::Down => &event.down_token,
        };
        let quote = self.quotes.get(token_id)?;
        let entry_price = quote.ask?;
        let entry_f = entry_price.to_f64()?;

        // Gate 4: probability cap — still room to profit
        if entry_f >= self.config.prob_cap {
            debug!(
                symbol,
                event_id = %event.event_id,
                entry_price = entry_f,
                cap = self.config.prob_cap,
                "Sweep: probability too high, no room to profit"
            );
            return None;
        }

        // Compute edge after fees
        let fee = crypto_fee_cost(entry_f);
        // For sweep, effective_p is approximated as 1.0 (high conviction)
        // minus a small discount based on how far above threshold we are.
        // Simplified: edge = (1.0 - entry_f) - fee (max payout minus cost).
        let edge = (1.0 - entry_f) - fee;

        info!(
            symbol,
            event_id = %event.event_id,
            ?direction,
            entry_price = %entry_price,
            diff_pct = format!("{:.6}", diff),
            edge = format!("{:.3}", edge),
            "Sweep: entry signal"
        );

        Some((direction, entry_price, abs_diff, edge))
    }

    /// Try sweep entry for a given symbol.
    fn try_entry(
        &self,
        symbol: &str,
        positions: &PositionLedger,
        orders: &OrderLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        let open_count = positions.positions().count();
        if open_count >= self.config.max_positions {
            return vec![];
        }

        let candidates = self.candidate_events(symbol, now);
        for event in candidates {
            if self.event_has_open_position(&event, positions) {
                continue;
            }
            if self.event_has_active_order(&event, orders) {
                continue;
            }

            if let Some((direction, entry_price, _abs_diff, edge)) =
                self.evaluate_entry(symbol, &event, now)
            {
                let direction_str = match direction {
                    Direction::Up => "UP",
                    Direction::Down => "DN",
                };
                let token_id = match direction {
                    Direction::Up => &event.up_token,
                    Direction::Down => &event.down_token,
                };
                let intent = TradingIntent {
                    intent_id: format!("sweep_{}_{}", event.event_id, direction_str),
                    deployment_id: String::new(),
                    market_id: event.event_id.to_string(),
                    token_id: token_id.to_string(),
                    side: TradeSide::Buy,
                    quantity: (self.config.stake_usd / entry_price).round_dp(6),
                    limit_price: Some(entry_price),
                    purpose: IntentPurpose::Entry,
                    created_at: now,
                };
                let signal = SignalRecord {
                    strategy: self.name().to_string(),
                    event_id: Some(event.event_id.to_string()),
                    token_id: Some(token_id.to_string()),
                    intent_id: Some(intent.intent_id.clone()),
                    symbol: event.symbol.to_string(),
                    direction: direction_str.into(),
                    p_hat: entry_price.to_f64().unwrap_or(0.0),
                    edge,
                    entry_price,
                    decision: "enter".into(),
                    ts: now,
                };
                return vec![StrategyDecision::Enter {
                    intent,
                    signal: Some(signal),
                }];
            }
        }
        vec![]
    }

    /// Check exit conditions for all holdings.
    fn check_exits(
        &self,
        symbol: &str,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        let Some(events) = self.events.get(symbol) else {
            return exits;
        };

        for event in events {
            let remaining = (event.end_time - now).num_seconds().max(0) as u64;

            // Check UP token position
            let up_qty = positions.net_qty(&event.up_token);
            if up_qty > Decimal::ZERO {
                if let Some(exit) =
                    self.check_token_exit(event, &event.up_token, up_qty, remaining, symbol, now)
                {
                    exits.push(exit);
                }
            }

            // Check DOWN token position
            let dn_qty = positions.net_qty(&event.down_token);
            if dn_qty > Decimal::ZERO {
                if let Some(exit) =
                    self.check_token_exit(event, &event.down_token, dn_qty, remaining, symbol, now)
                {
                    exits.push(exit);
                }
            }
        }

        exits
    }

    /// Check exit for a single token position.
    fn check_token_exit(
        &self,
        event: &EventWindow,
        token_id: &Arc<str>,
        qty: Decimal,
        remaining: u64,
        symbol: &str,
        now: DateTime<Utc>,
    ) -> Option<StrategyDecision> {
        // Hold to expiry in the final seconds
        if remaining < self.config.hold_to_expiry_secs {
            return None;
        }

        // Floor stop: diff collapsed
        if let Some(price_to_beat) = event.price_to_beat {
            if let Some(diff) = self.diff_pct(symbol, price_to_beat) {
                if diff.abs() <= self.config.diff_floor_stop {
                    info!(
                        symbol,
                        event_id = %event.event_id,
                        token_id = %token_id,
                        diff_pct = format!("{:.6}", diff),
                        "Sweep: floor stop triggered"
                    );
                    let bid = self.quotes.get(token_id).and_then(|q| q.bid);
                    return Some(StrategyDecision::Exit(TradingIntent {
                        intent_id: format!("sweep_exit_{}", event.event_id),
                        deployment_id: String::new(),
                        market_id: event.event_id.to_string(),
                        token_id: token_id.to_string(),
                        side: TradeSide::Sell,
                        quantity: qty,
                        limit_price: bid,
                        purpose: IntentPurpose::Exit,
                        created_at: now,
                    }));
                }
            }
        }

        // Segmented take-profit based on time remaining
        let tp_threshold = if remaining >= 40 {
            self.config.tp_prob_40s
        } else if remaining >= 20 {
            self.config.tp_prob_20s
        } else if remaining >= self.config.hold_to_expiry_secs {
            self.config.tp_prob_10s
        } else {
            return None; // hold to expiry (already handled above, but defensive)
        };

        // Check if current bid exceeds take-profit threshold
        if let Some(bid) = self.quotes.get(token_id).and_then(|q| q.bid) {
            let bid_f = bid.to_f64().unwrap_or(0.0);
            if bid_f >= tp_threshold {
                info!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %token_id,
                    bid = bid_f,
                    tp_threshold,
                    remaining,
                    "Sweep: take-profit triggered"
                );
                return Some(StrategyDecision::Exit(TradingIntent {
                    intent_id: format!("sweep_tp_{}", event.event_id),
                    deployment_id: String::new(),
                    market_id: event.event_id.to_string(),
                    token_id: token_id.to_string(),
                    side: TradeSide::Sell,
                    quantity: qty,
                    limit_price: Some(bid),
                    purpose: IntentPurpose::Exit,
                    created_at: now,
                }));
            }
        }

        None
    }

    fn resolve_expired_event_outcome(
        &self,
        event: &EventWindow,
        settlement: Option<bool>,
    ) -> Option<bool> {
        settlement::resolve_up_won(
            settlement,
            self.spot.get(&event.symbol).map(|state| state.price),
            event.price_to_beat,
        )
    }

    fn build_settlement_exits(
        &self,
        event: &EventWindow,
        end_time: DateTime<Utc>,
        up_won: bool,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        if positions.net_qty(&event.up_token) > Decimal::ZERO {
            let settle_price = if up_won { dec!(1.00) } else { dec!(0.00) };
            let qty = positions.net_qty(&event.up_token);
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("sweep_settle_{}", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.up_token.to_string(),
                side: TradeSide::Sell,
                quantity: qty,
                limit_price: Some(settle_price),
                purpose: IntentPurpose::Exit,
                created_at: end_time,
            }));
            info!(
                event_id = %event.event_id,
                token = %event.up_token,
                outcome = if up_won { "WIN" } else { "LOSE" },
                "Sweep settlement: UP token"
            );
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            let settle_price = if up_won { dec!(0.00) } else { dec!(1.00) };
            let qty = positions.net_qty(&event.down_token);
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("sweep_settle_{}", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.down_token.to_string(),
                side: TradeSide::Sell,
                quantity: qty,
                limit_price: Some(settle_price),
                purpose: IntentPurpose::Exit,
                created_at: end_time,
            }));
            info!(
                event_id = %event.event_id,
                token = %event.down_token,
                outcome = if up_won { "LOSE" } else { "WIN" },
                "Sweep settlement: DOWN token"
            );
        }

        exits
    }

    fn settle_expired_events_for_symbol(
        &mut self,
        symbol: &str,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        let expired: Vec<EventWindow> = self
            .events
            .get(symbol)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.end_time <= now)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut exits = Vec::new();
        let mut resolved_ids = HashSet::new();

        for event in expired {
            if !self.event_has_open_position(&event, positions) {
                resolved_ids.insert(event.event_id.clone());
                continue;
            }

            let Some(up_won) = self.resolve_expired_event_outcome(&event, None) else {
                warn!(
                    event_id = %event.event_id,
                    symbol = %event.symbol,
                    "Sweep settlement pending: outcome unavailable"
                );
                continue;
            };

            exits.extend(self.build_settlement_exits(&event, event.end_time, up_won, positions));
            resolved_ids.insert(event.event_id.clone());
        }

        if !resolved_ids.is_empty() {
            if let Some(events) = self.events.get_mut(symbol) {
                events.retain(|e| !resolved_ids.contains(&e.event_id));
            }
            for id in &resolved_ids {
                self.retired_events.insert(id.clone());
            }
        }

        exits
    }
}

impl StrategyLogic for SweepStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
                    return vec![];
                }

                self.spot
                    .insert(symbol.clone(), SpotState { price: *price });

                // Backfill price_to_beat for events that arrived before first spot
                if let Some(events) = self.events.get_mut(symbol) {
                    for event in events.iter_mut() {
                        if event.price_to_beat.is_none() {
                            event.price_to_beat = Some(*price);
                        }
                    }
                }

                // Settle expired events first
                let exits = self.settle_expired_events_for_symbol(symbol, positions, *ts);
                if !exits.is_empty() {
                    return exits;
                }

                // Check exits for existing holdings
                let exit_decisions = self.check_exits(symbol, positions, *ts);
                if !exit_decisions.is_empty() {
                    return exit_decisions;
                }

                // Try entry
                self.reset_daily_counter(*ts);
                if self.daily_trade_count >= self.config.max_daily_trades {
                    return vec![];
                }
                self.try_entry(symbol, positions, orders, *ts)
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

                // A fresh quote may unlock an exit (take-profit on bid change).
                if let Some(symbol) = self.token_symbol.get(token_id).cloned() {
                    if self
                        .config
                        .symbols
                        .iter()
                        .any(|s| s.as_str() == symbol.as_ref())
                    {
                        let exit_decisions = self.check_exits(&symbol, positions, *ts);
                        if !exit_decisions.is_empty() {
                            return exit_decisions;
                        }
                        // Also try entry on quote update
                        self.reset_daily_counter(*ts);
                        if self.daily_trade_count < self.config.max_daily_trades {
                            return self.try_entry(&symbol, positions, orders, *ts);
                        }
                    }
                }
                vec![]
            }

            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs,
                price_to_beat,
                resolved_up_won: _,
            } => {
                if !self.window_allowed(*window_secs) {
                    return vec![];
                }
                if self.retired_events.contains(event_id) {
                    return vec![];
                }

                let events = self.events.entry(symbol.clone()).or_default();
                // Dedup
                if events.iter().any(|e| e.event_id == *event_id) {
                    return vec![];
                }

                // Use price_to_beat if available, otherwise fallback to current spot
                let ptb = price_to_beat.or_else(|| self.spot.get(symbol).map(|s| s.price));

                // Track token → symbol mapping
                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());
                self.token_event.insert(up_token.clone(), event_id.clone());
                self.token_event
                    .insert(down_token.clone(), event_id.clone());

                events.push(EventWindow {
                    event_id: event_id.clone(),
                    symbol: symbol.clone(),
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    window_secs: *window_secs,
                    price_to_beat: ptb,
                });

                vec![]
            }

            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won: settlement,
            } => {
                let mut exits = Vec::new();
                let mut remove_event = true;
                let mut matching: Vec<EventWindow> = Vec::new();
                for events in self.events.values() {
                    for ev in events {
                        if ev.event_id == *event_id {
                            matching.push(ev.clone());
                        }
                    }
                }

                for event in matching {
                    if !self.event_has_open_position(&event, positions) {
                        continue;
                    }
                    let Some(up_won) = self.resolve_expired_event_outcome(&event, *settlement)
                    else {
                        warn!(
                            event_id = %event_id,
                            "Sweep settlement pending: outcome unavailable"
                        );
                        remove_event = false;
                        continue;
                    };
                    exits.extend(self.build_settlement_exits(&event, *end_time, up_won, positions));
                }

                if remove_event {
                    for events in self.events.values_mut() {
                        events.retain(|e| e.event_id != *event_id);
                    }
                    self.retired_events.insert(event_id.clone());
                }
                exits
            }

            _ => vec![],
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if let Some(symbol) = self.token_symbol.get(fill.token_id.as_str()) {
            let _symbol = symbol.clone();
        }

        match fill.side {
            TradeSide::Buy => {
                self.daily_trade_count += 1;
                // Track holding for exit management
                let direction = if let Some(event_id) = self.token_event.get(fill.token_id.as_str())
                {
                    // Determine direction from which token was bought
                    let is_up = self.events.values().flatten().any(|e| {
                        e.event_id == *event_id && e.up_token.as_ref() == fill.token_id.as_str()
                    });
                    if is_up {
                        Direction::Up
                    } else {
                        Direction::Down
                    }
                } else {
                    Direction::Up // fallback
                };
                self.holdings.insert(
                    Arc::from(fill.token_id.clone()),
                    HoldingState {
                        token_id: Arc::from(fill.token_id.clone()),
                        direction,
                        entry_time: fill.timestamp,
                    },
                );
            }
            TradeSide::Sell => {
                self.holdings.remove(fill.token_id.as_str());
            }
        }
    }

    fn name(&self) -> &str {
        "sweep"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ploy_trading::{OrderLedger, PositionLedger};

    fn default_test_config() -> SweepConfig {
        SweepConfig {
            symbols: vec!["BTCUSDT".into()],
            allowed_window_secs: vec![300],
            ..SweepConfig::default()
        }
    }

    #[test]
    fn no_entry_when_diff_below_threshold() {
        let config = default_test_config();
        let mut strat = SweepStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // Register event ending in 30s (within sweep window)
        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(30),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Set spot barely above price_to_beat (diff < threshold)
        strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100010), // 0.01% diff, well below 0.071%
                ts: now,
            },
            &positions,
            &orders,
        );

        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.50)),
                ask: Some(dec!(0.51)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100020),
                ts: now + chrono::Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "diff below threshold should not enter"
        );
    }

    #[test]
    fn entry_when_diff_exceeds_threshold() {
        let config = default_test_config();
        let mut strat = SweepStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(30),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.59)),
                ask: Some(dec!(0.60)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "dn1".into(),
                bid: Some(dec!(0.39)),
                ask: Some(dec!(0.40)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        // BTC up 0.1% = diff 0.001 > threshold 0.00071
        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100100),
                ts: now + chrono::Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "Expected 1 entry signal");
        match &decisions[0] {
            StrategyDecision::Enter { intent, signal } => {
                assert_eq!(intent.token_id, "up1");
                assert_eq!(intent.side, TradeSide::Buy);
                let signal = signal.as_ref().unwrap();
                assert_eq!(signal.direction, "UP");
                assert_eq!(signal.decision, "enter");
            }
            other => panic!("Expected Enter, got {:?}", other),
        }
    }

    #[test]
    fn no_entry_outside_time_window() {
        let config = default_test_config();
        let mut strat = SweepStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // Event ending in 120s — outside the 0-60s sweep window
        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(120),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.59)),
                ask: Some(dec!(0.60)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100200), // big diff
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(decisions.is_empty(), "outside time window should not enter");
    }

    #[test]
    fn no_entry_when_prob_too_high() {
        let config = default_test_config();
        let mut strat = SweepStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(30),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Ask at 0.96 > prob_cap 0.95
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.95)),
                ask: Some(dec!(0.96)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100200),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(decisions.is_empty(), "prob above cap should not enter");
    }
}
