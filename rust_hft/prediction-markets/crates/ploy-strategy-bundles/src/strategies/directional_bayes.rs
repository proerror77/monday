//! Bayesian directional binary-option strategy (pm_5m_directional_bayes).
//!
//! Fork of [`directional`] with a single change: the raw log-normal `p_hat`
//! is fused with the Polymarket ask price (market consensus) via Bayesian
//! posterior odds before the probability and edge gates fire.
//!
//! **Prior** = Polymarket ask price (`entry_f`), representing market consensus.
//! **Likelihood** = log-normal model estimate (`p_hat`).
//! **Posterior** = `prior_odds * likelihood_ratio / (1 + prior_odds * likelihood_ratio)`.
//!
//! This shrinks extreme model estimates toward the market price, reducing
//! false signals when the model disagrees strongly with the order book.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
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
use super::common::quote::QuoteState;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

// ── Probability Model ────────────────────────────────────

/// Standard normal CDF (Abramowitz-Stegun approximation).
fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592_f64;
    let a2 = -0.284496736_f64;
    let a3 = 1.421413741_f64;
    let a4 = -1.453152027_f64;
    let a5 = 1.061405429_f64;
    let p = 0.3275911_f64;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * z);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp();

    0.5 * (1.0 + sign * y)
}

/// Estimate P(S_T >= S_0) using log-normal model with horizon sigma.
fn estimate_probability(s0: f64, st: f64, sigma_horizon: f64) -> f64 {
    if sigma_horizon <= 0.0 {
        return if st >= s0 { 1.0 } else { 0.0 };
    }
    if s0 <= 0.0 || st <= 0.0 {
        return 0.5;
    }
    let z = (st / s0).ln() / sigma_horizon;
    normal_cdf(z)
}

// ── Bayesian Fusion ─────────────────────────────────────

/// Fuse a log-normal model probability (likelihood) with the Polymarket
/// ask price (prior / market consensus) via Bayes' rule in odds space.
///
/// Returns the posterior probability.
fn bayesian_posterior(prior: f64, likelihood: f64) -> f64 {
    let prior_odds = prior / (1.0 - prior).max(1e-9);
    let likelihood_ratio = likelihood / (1.0 - likelihood).max(1e-9);
    let posterior_odds = prior_odds * likelihood_ratio;
    posterior_odds / (1.0 + posterior_odds)
}

const EWMA_LAMBDA: f64 = 0.94;

// ── Direction ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

// ── Configuration ────────────────────────────────────────

/// Strategy configuration, loadable from TOML.
///
/// Same struct drives backtest, dry-run, and live — no more divergence.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BayesianDirectionalConfig {
    // Symbols
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,

    // Probability gates
    #[serde(default = "default_vol_floor")]
    pub vol_floor: f64,
    #[serde(default = "default_min_probability")]
    pub min_probability: f64,
    /// Deprecated: z-score gate removed (redundant with probability gate).
    /// Kept for config backward compatibility.
    #[serde(default = "default_min_z_score")]
    pub min_z_score: f64,

    // Price gates
    #[serde(default = "default_min_entry_price")]
    pub min_entry_price: f64,
    #[serde(default = "default_max_entry_price")]
    pub max_entry_price: f64,
    #[serde(default = "default_no_trade_min")]
    pub no_trade_zone_min: f64,
    #[serde(default = "default_no_trade_max")]
    pub no_trade_zone_max: f64,

    // Edge
    #[serde(default = "default_min_edge")]
    pub min_edge: f64,

    // Timing
    #[serde(default = "default_min_time")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time")]
    pub max_time_remaining_secs: u64,
    #[serde(default = "default_cooldown")]
    pub cooldown_secs: u64,

    // Sizing
    #[serde(default = "default_stake_usd", alias = "quantity")]
    pub stake_usd: Decimal,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,

    // Risk
    /// Daily loss circuit breaker (USD). `None` = no limit.
    #[serde(default)]
    pub max_daily_loss_usd: Option<Decimal>,

    /// Only trade markets whose total window duration (seconds) is in this list.
    /// e.g. [300, 900] for 5-minute and 15-minute markets only.
    /// Empty or absent = no filter.
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,
}

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()]
}
fn default_vol_floor() -> f64 {
    0.001
}
fn default_min_probability() -> f64 {
    0.55
}
fn default_min_z_score() -> f64 {
    0.35
}
fn default_min_entry_price() -> f64 {
    0.15
}
fn default_max_entry_price() -> f64 {
    0.85
}
fn default_no_trade_min() -> f64 {
    0.45
}
fn default_no_trade_max() -> f64 {
    0.55
}
fn default_min_edge() -> f64 {
    0.02
}
fn default_min_time() -> u64 {
    60
}
fn default_max_time() -> u64 {
    300
}
fn default_cooldown() -> u64 {
    0
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

// ── Internal State ───────────────────────────────────────

/// Tracked CEX spot price for a symbol.
struct SpotState {
    price: Decimal,
    ts: DateTime<Utc>,
}

#[derive(Default)]
struct VolatilityState {
    ewma_var_per_sec: f64,
}

// ── Strategy Implementation ──────────────────────────────

pub struct BayesianDirectionalStrategy {
    config: BayesianDirectionalConfig,
    // Market state
    spot: HashMap<Arc<str>, SpotState>,
    volatility: HashMap<Arc<str>, VolatilityState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    // Gating state
    cooldowns: HashMap<Arc<str>, DateTime<Utc>>,
    daily_trades: u32,
    last_trade_date: Option<chrono::NaiveDate>,
    /// Realized PnL for the current trading day (circuit breaker).
    daily_realized_pnl: Decimal,
    // Token -> symbol mapping
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    /// Entry price cache: token_id -> entry price (for PnL tracking on settlement).
    entry_prices: HashMap<Arc<str>, Decimal>,
    /// Most recent feed timestamp seen across all updates.
    /// Used instead of Utc::now() so replay runs are deterministic.
    feed_time: Option<DateTime<Utc>>,
    /// When set, all new entries are blocked until this time (balance exhausted pause).
    balance_exhausted_until: Option<DateTime<Utc>>,
}

impl BayesianDirectionalStrategy {
    pub fn new(config: BayesianDirectionalConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            volatility: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            cooldowns: HashMap::new(),
            daily_trades: 0,
            last_trade_date: None,
            daily_realized_pnl: Decimal::ZERO,
            token_symbol: HashMap::new(),
            entry_prices: HashMap::new(),
            feed_time: None,
            balance_exhausted_until: None,
        }
    }

    /// Pick the nearest event within the valid time window.
    fn pick_event(&self, symbol: &str, now: DateTime<Utc>) -> Option<EventWindow> {
        self.events
            .get(symbol)?
            .iter()
            .filter(|e| {
                let rem = (e.end_time - now).num_seconds();
                rem >= self.config.min_time_remaining_secs as i64
                    && rem <= self.config.max_time_remaining_secs as i64
            })
            .min_by_key(|e| e.end_time)
            .cloned()
    }

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        self.events
            .get(symbol)
            .map(|events| {
                let mut candidates: Vec<EventWindow> = events
                    .iter()
                    .filter(|event| {
                        let rem = (event.end_time - now).num_seconds();
                        rem >= self.config.min_time_remaining_secs as i64
                            && rem <= self.config.max_time_remaining_secs as i64
                    })
                    .cloned()
                    .collect();
                candidates.sort_by_key(|event| event.end_time);
                candidates
            })
            .unwrap_or_default()
    }

    fn in_cooldown(&self, symbol: &str, now: DateTime<Utc>) -> bool {
        self.cooldowns.get(symbol).map_or(false, |last| {
            (now - *last).num_seconds() < self.config.cooldown_secs as i64
        })
    }

    fn reset_daily_counter(&mut self, now: DateTime<Utc>) {
        let today = now.date_naive();
        if self.last_trade_date != Some(today) {
            self.daily_trades = 0;
            self.daily_realized_pnl = Decimal::ZERO;
            self.last_trade_date = Some(today);
        }
    }

    fn floor_var_per_sec(&self) -> f64 {
        let sigma_floor = self.config.vol_floor.max(1e-9);
        sigma_floor * sigma_floor / 900.0
    }

    fn update_volatility(&mut self, symbol: &str, price: Decimal, ts: DateTime<Utc>) {
        let Some(previous) = self.spot.get(symbol) else {
            return;
        };
        if previous.price <= Decimal::ZERO || price <= Decimal::ZERO {
            return;
        }

        let dt_secs = (ts - previous.ts).num_milliseconds() as f64 / 1000.0;
        if dt_secs <= 0.0 {
            return;
        }

        let Some(prev_f) = previous.price.to_f64() else {
            return;
        };
        let Some(curr_f) = price.to_f64() else {
            return;
        };
        if prev_f <= 0.0 || curr_f <= 0.0 {
            return;
        }

        let inst_var_per_sec = (curr_f / prev_f).ln().powi(2) / dt_secs.max(1e-6);
        let floor = self.floor_var_per_sec();
        let state = self.volatility.entry(Arc::from(symbol)).or_default();
        state.ewma_var_per_sec = if state.ewma_var_per_sec <= 0.0 {
            inst_var_per_sec.max(floor)
        } else {
            (EWMA_LAMBDA * state.ewma_var_per_sec) + ((1.0 - EWMA_LAMBDA) * inst_var_per_sec)
        };
    }

    fn sigma_horizon(&self, symbol: &str, time_remaining_secs: f64) -> f64 {
        let secs = time_remaining_secs.max(1.0);
        let floor = self.floor_var_per_sec();
        let realized = self
            .volatility
            .get(symbol)
            .map(|state| state.ewma_var_per_sec)
            .unwrap_or(floor);
        (realized.max(floor) * secs).sqrt()
    }

    fn shares_for_entry_price(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    fn event_has_open_position(&self, event: &EventWindow, positions: &PositionLedger) -> bool {
        positions.net_qty(&event.up_token) > Decimal::ZERO
            || positions.net_qty(&event.down_token) > Decimal::ZERO
    }

    fn event_has_active_order(&self, event: &EventWindow, orders: &OrderLedger) -> bool {
        orders.orders().any(|order| {
            matches!(
                order.state,
                ploy_trading::OrderState::Pending
                    | ploy_trading::OrderState::Acknowledged
                    | ploy_trading::OrderState::PartiallyFilled
            ) && (order.token_id.as_str() == &*event.up_token
                || order.token_id.as_str() == &*event.down_token)
        })
    }

    fn window_allowed(&self, window_secs: u64) -> bool {
        self.config.allowed_window_secs.is_empty()
            || self.config.allowed_window_secs.contains(&window_secs)
    }

    fn resolve_expired_event_outcome(
        &self,
        event: &EventWindow,
        settlement: Option<bool>,
    ) -> Option<bool> {
        if let Some(resolved) = settlement {
            return Some(resolved);
        }

        match (
            self.spot.get(&event.symbol).map(|spot| spot.price),
            event.price_to_beat,
        ) {
            (Some(current), Some(open)) => Some(current >= open),
            _ => None,
        }
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
                intent_id: format!("settle_{}", event.event_id),
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
                settle_price = %settle_price,
                "Settlement: UP token"
            );
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            let settle_price = if up_won { dec!(0.00) } else { dec!(1.00) };
            let qty = positions.net_qty(&event.down_token);
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("settle_{}", event.event_id),
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
                settle_price = %settle_price,
                "Settlement: DOWN token"
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
        let expired_events: Vec<EventWindow> = self
            .events
            .get(symbol)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event.end_time <= now)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let mut exits = Vec::new();
        let mut resolved_event_ids = HashSet::new();

        for event in expired_events {
            if !self.event_has_open_position(&event, positions) {
                resolved_event_ids.insert(event.event_id.clone());
                continue;
            }

            let Some(up_won) = self.resolve_expired_event_outcome(&event, None) else {
                warn!(
                    event_id = %event.event_id,
                    symbol = %event.symbol,
                    "Settlement pending: official outcome unavailable and fallback spot/open price missing"
                );
                continue;
            };

            let up_qty = positions.net_qty(&event.up_token);
            let down_qty = positions.net_qty(&event.down_token);
            if up_qty > Decimal::ZERO {
                let payout = if up_won { up_qty } else { Decimal::ZERO };
                self.daily_realized_pnl += payout - self.config.stake_usd;
            }
            if down_qty > Decimal::ZERO {
                let payout = if up_won { Decimal::ZERO } else { down_qty };
                self.daily_realized_pnl += payout - self.config.stake_usd;
            }

            exits.extend(self.build_settlement_exits(&event, event.end_time, up_won, positions));
            resolved_event_ids.insert(event.event_id.clone());
        }

        if !resolved_event_ids.is_empty() {
            if let Some(events) = self.events.get_mut(symbol) {
                events.retain(|event| !resolved_event_ids.contains(&event.event_id));
            }
        }

        exits
    }

    /// Core signal evaluation — the 5-gate pipeline with Bayesian fusion.
    ///
    /// Gate 0: Price validity
    /// Gate 1: Quote availability + direction (uses raw p_hat)
    /// Gate B: Bayesian posterior fusion (prior=ask, likelihood=model)
    /// Gate 2: Price filter (bounds + no-trade zone)
    /// Gate 3: Probability threshold (uses posterior)
    /// Gate 4: Edge after fees (uses posterior)
    fn evaluate_entry(
        &self,
        symbol: &str,
        spot_price: Decimal,
        event: &EventWindow,
        now: DateTime<Utc>,
    ) -> Option<(Direction, Decimal, f64, f64)> {
        // Gate 0: Price validity
        let price_to_beat = event.price_to_beat?;
        if price_to_beat <= Decimal::ZERO || spot_price <= Decimal::ZERO {
            debug!(symbol = %symbol, event_id = %event.event_id, "Gate 0: Invalid prices");
            return None;
        }

        let s0 = price_to_beat.to_f64()?;
        let st = spot_price.to_f64()?;
        let secs_remaining = (event.end_time - now).num_seconds().max(0) as f64;
        let sigma_horizon = self.sigma_horizon(symbol, secs_remaining);

        // Probability estimation (log-normal model)
        let p_hat = estimate_probability(s0, st, sigma_horizon);

        // Gate 1: Direction + quote availability (uses raw p_hat for direction)
        let (direction, entry_price) = if p_hat >= 0.5 {
            let quote = self.quotes.get(&event.up_token);
            if quote.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.up_token,
                    "Gate 1: UP token quote missing"
                );
                return None;
            }
            let ask = quote.unwrap().ask;
            if ask.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.up_token,
                    "Gate 1: UP token ask price missing"
                );
                return None;
            }
            (Direction::Up, ask.unwrap())
        } else {
            let quote = self.quotes.get(&event.down_token);
            if quote.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.down_token,
                    "Gate 1: DOWN token quote missing"
                );
                return None;
            }
            let ask = quote.unwrap().ask;
            if ask.is_none() {
                debug!(
                    symbol,
                    event_id = %event.event_id,
                    token_id = %event.down_token,
                    "Gate 1: DOWN token ask price missing"
                );
                return None;
            }
            (Direction::Down, ask.unwrap())
        };

        let entry_f = entry_price.to_f64()?;

        // ── Gate B: Bayesian posterior fusion ──
        // Prior = Polymarket ask (market consensus for the chosen direction).
        // Likelihood = log-normal model estimate.
        // For UP direction: prior = entry_f (ask for UP token),
        //                   likelihood = p_hat (model's P(up)).
        // For DOWN direction: prior = entry_f (ask for DOWN token),
        //                     likelihood = 1 - p_hat (model's P(down)).
        let (prior, likelihood) = match direction {
            Direction::Up => (entry_f, p_hat),
            Direction::Down => (entry_f, 1.0 - p_hat),
        };
        let posterior = bayesian_posterior(prior, likelihood);

        debug!(
            symbol,
            event_id = %event.event_id,
            ?direction,
            p_hat,
            prior,
            likelihood,
            posterior,
            "Gate B: Bayesian fusion"
        );

        // Gate 2: Price filter (bounds + no-trade zone)
        if entry_f < self.config.min_entry_price
            || entry_f > self.config.max_entry_price
            || (entry_f >= self.config.no_trade_zone_min
                && entry_f <= self.config.no_trade_zone_max)
        {
            debug!(
                symbol,
                event_id = %event.event_id,
                entry_price = entry_f,
                "Gate 2: Price filter (bounds or no-trade zone)"
            );
            return None;
        }

        // Gate 3: Probability threshold (uses Bayesian posterior)
        let effective_p = posterior;
        if effective_p < self.config.min_probability {
            debug!(
                symbol,
                event_id = %event.event_id,
                effective_p,
                threshold = self.config.min_probability,
                "Gate 3: Posterior probability too low"
            );
            return None;
        }

        // Gate 4: Edge after fees (uses Bayesian posterior)
        let cost = crypto_fee_cost(entry_f);
        let edge = effective_p - entry_f - cost;
        if edge < self.config.min_edge {
            debug!(
                symbol,
                event_id = %event.event_id,
                edge,
                threshold = self.config.min_edge,
                effective_p,
                entry_price = entry_f,
                cost,
                "Gate 4: Edge too low"
            );
            return None;
        }

        Some((direction, entry_price, effective_p, edge))
    }

    /// Build a TradingIntent from a signal.
    fn build_intent(
        &self,
        event: &EventWindow,
        direction: Direction,
        entry_price: Decimal,
        now: DateTime<Utc>,
    ) -> TradingIntent {
        let token_id = match direction {
            Direction::Up => event.up_token.clone(),
            Direction::Down => event.down_token.clone(),
        };

        TradingIntent {
            intent_id: format!(
                "pm5db_{}_{}_{}",
                event.symbol,
                match direction {
                    Direction::Up => "UP",
                    Direction::Down => "DN",
                },
                now.timestamp_millis(),
            ),
            deployment_id: String::new(),
            market_id: event.event_id.to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: self.shares_for_entry_price(entry_price),
            limit_price: Some(entry_price),
            purpose: IntentPurpose::Entry,
            created_at: now,
        }
    }

    fn build_signal_record(
        &self,
        event: &EventWindow,
        intent: &TradingIntent,
        direction: Direction,
        effective_p: f64,
        edge: f64,
        entry_price: Decimal,
        now: DateTime<Utc>,
    ) -> SignalRecord {
        SignalRecord {
            strategy: self.name().to_string(),
            event_id: Some(event.event_id.to_string()),
            token_id: Some(intent.token_id.clone()),
            intent_id: Some(intent.intent_id.clone()),
            symbol: event.symbol.to_string(),
            direction: match direction {
                Direction::Up => "UP".into(),
                Direction::Down => "DOWN".into(),
            },
            p_hat: effective_p,
            edge,
            entry_price,
            decision: "enter".into(),
            ts: now,
        }
    }

    /// Try directional entry for a given symbol after a spot price update.
    fn try_entry(
        &self,
        symbol: &str,
        positions: &PositionLedger,
        orders: &OrderLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        // Balance exhausted pause (set by on_reject when venue returns insufficient balance)
        if let Some(until) = self.balance_exhausted_until {
            if now < until {
                debug!(
                    symbol,
                    until = %until,
                    "Balance exhausted pause active, skipping entry"
                );
                return vec![];
            }
        }

        // Daily loss circuit breaker
        if let Some(max_loss) = self.config.max_daily_loss_usd {
            if self.daily_realized_pnl <= -max_loss {
                debug!(
                    symbol,
                    daily_pnl = %self.daily_realized_pnl,
                    max_loss = %max_loss,
                    "Daily loss circuit breaker triggered"
                );
                return vec![];
            }
        }

        // Position count check
        let open_count = positions.positions().count();
        if open_count >= self.config.max_positions {
            debug!(
                symbol,
                open_count,
                max = self.config.max_positions,
                "Max positions reached"
            );
            return vec![];
        }

        let spot_price = match self.spot.get(symbol) {
            Some(s) => s.price,
            None => {
                debug!(symbol = %symbol, "No spot price available");
                return vec![];
            }
        };

        let candidates = self.candidate_events(symbol, now);
        if candidates.is_empty() {
            if let Some(events) = self.events.get(symbol) {
                let total = events.len();
                let in_window = events
                    .iter()
                    .filter(|e| {
                        let rem = (e.end_time - now).num_seconds();
                        rem >= self.config.min_time_remaining_secs as i64
                            && rem <= self.config.max_time_remaining_secs as i64
                    })
                    .count();
                debug!(
                    symbol,
                    total_events = total,
                    in_window,
                    min_time = self.config.min_time_remaining_secs,
                    max_time = self.config.max_time_remaining_secs,
                    "No event in valid time window"
                );
            }
            return vec![];
        }

        for event in candidates {
            if self.event_has_open_position(&event, positions) {
                debug!(symbol = %symbol, event_id = %event.event_id, "Already holding position for this event");
                continue;
            }
            if self.event_has_active_order(&event, orders) {
                debug!(symbol = %symbol, event_id = %event.event_id, "Active order already exists for this event");
                continue;
            }

            if let Some((direction, entry_price, effective_p, edge)) =
                self.evaluate_entry(symbol, spot_price, &event, now)
            {
                info!(
                    symbol,
                    event_id = %event.event_id,
                    ?direction,
                    entry_price = %entry_price,
                    p = format!("{:.1}%", effective_p * 100.0),
                    edge = format!("{:.1}%", edge * 100.0),
                    "Bayes entry signal PASSED all gates",
                );
                let intent = self.build_intent(&event, direction, entry_price, now);
                let signal = self.build_signal_record(
                    &event,
                    &intent,
                    direction,
                    effective_p,
                    edge,
                    entry_price,
                    now,
                );
                return vec![StrategyDecision::Enter {
                    intent,
                    signal: Some(signal),
                }];
            }
        }

        vec![]
    }
}

impl StrategyLogic for BayesianDirectionalStrategy {
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

                if self.feed_time.map_or(true, |ft| *ts > ft) {
                    self.feed_time = Some(*ts);
                }

                self.update_volatility(symbol, *price, *ts);
                self.spot.insert(
                    symbol.clone(),
                    SpotState {
                        price: *price,
                        ts: *ts,
                    },
                );
                if let Some(events) = self.events.get_mut(symbol) {
                    for event in events.iter_mut() {
                        if event.price_to_beat.is_none() {
                            event.price_to_beat = Some(*price);
                        }
                    }
                }

                let exits = self.settle_expired_events_for_symbol(symbol, positions, *ts);
                if !exits.is_empty() {
                    return exits;
                }

                self.reset_daily_counter(*ts);
                if self.daily_trades >= self.config.max_daily_trades {
                    return vec![];
                }
                if let Some(loss_limit) = self.config.max_daily_loss_usd {
                    if self.daily_realized_pnl <= -loss_limit {
                        return vec![];
                    }
                }
                if self.in_cooldown(symbol, *ts) {
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

                if let Some(symbol) = self.token_symbol.get(token_id).cloned() {
                    if self
                        .config
                        .symbols
                        .iter()
                        .any(|s| s.as_str() == symbol.as_ref())
                    {
                        if self.feed_time.map_or(true, |ft| *ts > ft) {
                            self.feed_time = Some(*ts);
                        }
                        self.reset_daily_counter(*ts);
                        if self.daily_trades < self.config.max_daily_trades
                            && !self.in_cooldown(&symbol, *ts)
                            && self
                                .config
                                .max_daily_loss_usd
                                .map_or(true, |limit| self.daily_realized_pnl > -limit)
                        {
                            return self.try_entry(&symbol, positions, orders, *ts);
                        }
                    }
                }
                vec![]
            }

            MarketUpdate::AggTrade { .. } => vec![],

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
                    debug!(symbol = %symbol, event_id = %event_id, window_secs = *window_secs, "Ignoring disallowed event window");
                    return vec![];
                }
                let now = self
                    .feed_time
                    .unwrap_or_else(|| *end_time - chrono::Duration::seconds(1));
                let mut stale_expired = Vec::new();
                if let Some(existing) = self.events.get(symbol) {
                    stale_expired = existing
                        .iter()
                        .filter(|event| {
                            event.end_time <= now && !self.event_has_open_position(event, positions)
                        })
                        .map(|event| event.event_id.clone())
                        .collect();
                }
                let events = self.events.entry(symbol.clone()).or_default();
                if !stale_expired.is_empty() {
                    let stale_expired: HashSet<Arc<str>> = stale_expired.into_iter().collect();
                    events.retain(|event| !stale_expired.contains(&event.event_id));
                }
                if events.iter().any(|e| e.event_id == *event_id) {
                    return vec![];
                }
                let price_to_beat =
                    price_to_beat.or_else(|| self.spot.get(symbol).map(|s| s.price));

                self.token_symbol.insert(up_token.clone(), symbol.clone());
                self.token_symbol.insert(down_token.clone(), symbol.clone());

                events.push(EventWindow {
                    event_id: event_id.clone(),
                    symbol: symbol.clone(),
                    up_token: up_token.clone(),
                    down_token: down_token.clone(),
                    end_time: *end_time,
                    window_secs: *window_secs,
                    price_to_beat,
                });

                let has_cached_quote =
                    self.quotes.contains_key(up_token) || self.quotes.contains_key(down_token);
                if has_cached_quote
                    && self
                        .config
                        .symbols
                        .iter()
                        .any(|s| s.as_str() == symbol.as_ref())
                    && self.daily_trades < self.config.max_daily_trades
                    && !self.in_cooldown(symbol, now)
                {
                    return self.try_entry(symbol, positions, orders, now);
                }
                vec![]
            }

            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won: settlement,
            } => {
                let mut exits = Vec::new();
                let mut remove_event = true;
                let mut matching_events = Vec::new();
                for events in self.events.values() {
                    for ev in events {
                        if ev.event_id == *event_id {
                            matching_events.push(ev.clone());
                        }
                    }
                }

                for event in matching_events {
                    if !self.event_has_open_position(&event, positions) {
                        continue;
                    }

                    let Some(up_won) = self.resolve_expired_event_outcome(&event, *settlement)
                    else {
                        warn!(
                            event_id = %event_id,
                            symbol = %event.symbol,
                            "Settlement pending: official outcome unavailable and fallback spot/open price missing"
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
                }
                exits
            }

            _ => vec![],
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if let Some(symbol) = self.token_symbol.get(fill.token_id.as_str()) {
            self.cooldowns.insert(symbol.clone(), fill.timestamp);
            self.daily_trades += 1;
        }

        match fill.side {
            ploy_trading::TradeSide::Buy => {
                self.entry_prices
                    .insert(Arc::from(fill.token_id.as_str()), fill.price);
            }
            ploy_trading::TradeSide::Sell => {
                if let Some(entry_price) = self.entry_prices.remove(fill.token_id.as_str()) {
                    let pnl = (fill.price - entry_price) * fill.quantity - fill.fee;
                    self.daily_realized_pnl += pnl;
                }
            }
        }
    }

    fn on_reject(&mut self, intent: &ploy_trading::TradingIntent, reason: &str) {
        let now = self.feed_time.unwrap_or_else(Utc::now);

        if reason.contains("not enough balance") {
            let pause_until = now + chrono::Duration::minutes(5);
            warn!(
                until = %pause_until,
                "Balance exhausted — pausing all entries for 5 minutes"
            );
            self.balance_exhausted_until = Some(pause_until);
            return;
        }

        if let Some(symbol) = self.token_symbol.get(intent.token_id.as_str()).cloned() {
            self.cooldowns.insert(symbol.clone(), now);
            debug!(symbol = %symbol, reason, "Rejection cooldown armed");
        }
    }

    fn name(&self) -> &str {
        "pm_5m_directional_bayes"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MarketUpdate;
    use ploy_trading::{OrderLedger, PositionLedger};

    fn default_config() -> BayesianDirectionalConfig {
        BayesianDirectionalConfig {
            symbols: vec!["BTCUSDT".into()],
            vol_floor: 0.001,
            min_probability: 0.55,
            min_z_score: 0.35,
            min_entry_price: 0.15,
            max_entry_price: 0.85,
            no_trade_zone_min: 0.45,
            no_trade_zone_max: 0.55,
            min_edge: 0.02,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: dec!(25),
            max_positions: 1000,
            max_daily_trades: 1000,
            max_daily_loss_usd: None,
            allowed_window_secs: vec![300, 900],
        }
    }

    #[test]
    fn bayesian_posterior_equal_prior_and_likelihood() {
        // When prior == likelihood == 0.5, posterior should be 0.5
        let p = bayesian_posterior(0.5, 0.5);
        assert!((p - 0.5).abs() < 1e-6, "got {}", p);
        // When prior == likelihood != 0.5, posterior is amplified (odds multiply)
        let p2 = bayesian_posterior(0.6, 0.6);
        assert!(p2 > 0.6, "expected amplification, got {}", p2);
    }

    #[test]
    fn bayesian_posterior_shrinks_toward_prior() {
        // Model says 0.8, market says 0.3 → posterior should be between 0.3 and 0.8
        let p = bayesian_posterior(0.3, 0.8);
        assert!(p > 0.3 && p < 0.8, "got {}", p);
    }

    #[test]
    fn bayesian_posterior_at_half() {
        // Uninformative prior (0.5) should just return the likelihood
        let p = bayesian_posterior(0.5, 0.7);
        assert!((p - 0.7).abs() < 1e-6, "got {}", p);
    }

    #[test]
    fn name_returns_bayes_variant() {
        let strat = BayesianDirectionalStrategy::new(default_config());
        assert_eq!(strat.name(), "pm_5m_directional_bayes");
    }

    #[test]
    fn full_signal_generates_entry_with_bayesian_fusion() {
        let config = default_config();
        let mut strat = BayesianDirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // 1. Register event ending in 120s
        let end_time = now + chrono::Duration::seconds(120);
        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time,
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // 2. Set initial spot
        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(100000),
                ts: now,
            },
        );
        strat.events.get_mut("BTCUSDT").unwrap()[0].price_to_beat = Some(dec!(100000));

        // 3. Provide quotes — UP ask cheap (0.30)
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.29)),
                ask: Some(dec!(0.30)),
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
                bid: Some(dec!(0.69)),
                ask: Some(dec!(0.70)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        // 4. BTC moved up 1.5% -> should trigger UP entry
        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(101500),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "Expected 1 entry signal");
        match &decisions[0] {
            StrategyDecision::Enter { intent, signal } => {
                assert_eq!(intent.token_id, "up1");
                assert_eq!(intent.side, TradeSide::Buy);
                let signal = signal.as_ref().expect("entry signal should be recorded");
                assert_eq!(signal.strategy, "pm_5m_directional_bayes");
                assert_eq!(signal.direction, "UP");
            }
            other => panic!("Expected Enter, got {:?}", other),
        }
    }
}
