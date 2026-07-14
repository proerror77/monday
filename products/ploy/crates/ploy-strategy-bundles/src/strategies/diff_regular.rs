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
use super::common::guards::active_order_exists;
use super::common::holding::BasicHoldingState;
use super::common::quote::QuoteState;
use super::common::settlement;
use super::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

// ── Config ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiffRegularConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,
    // Entry thresholds (percentage-based)
    #[serde(default = "default_diff_entry_threshold")]
    pub diff_entry_threshold: f64,
    #[serde(default = "default_prob_cap")]
    pub prob_cap: f64,
    // Cooldown
    #[serde(default = "default_diff_overheat_threshold")]
    pub diff_overheat_threshold: f64,
    #[serde(default = "default_prob_overheat")]
    pub prob_overheat: f64,
    #[serde(default = "default_diff_neutral_threshold")]
    pub diff_neutral_threshold: f64,
    #[serde(default = "default_neutral_hold_secs")]
    pub neutral_hold_secs: u64,
    // Exit
    #[serde(default = "default_diff_floor_stop")]
    pub diff_floor_stop: f64,
    #[serde(default = "default_hold_to_expiry_secs")]
    pub hold_to_expiry_secs: u64,
    // Timing
    #[serde(default = "default_min_time_remaining")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time_remaining")]
    pub max_time_remaining_secs: u64,
    // Sizing
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
}
fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into()]
}
fn default_diff_entry_threshold() -> f64 {
    0.00057
}
fn default_prob_cap() -> f64 {
    0.75
}
fn default_diff_overheat_threshold() -> f64 {
    0.0015
}
fn default_prob_overheat() -> f64 {
    0.85
}
fn default_diff_neutral_threshold() -> f64 {
    0.0002
}
fn default_neutral_hold_secs() -> u64 {
    3
}
fn default_diff_floor_stop() -> f64 {
    0.00007
}
fn default_hold_to_expiry_secs() -> u64 {
    8
}
fn default_min_time_remaining() -> u64 {
    48
}
fn default_max_time_remaining() -> u64 {
    168
}
fn default_stake_usd() -> Decimal {
    Decimal::new(10, 0)
}
fn default_cooldown_secs() -> u64 {
    90
}
fn default_max_positions() -> usize {
    1000
}
fn default_max_daily_trades() -> u32 {
    1000
}

impl Default for DiffRegularConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            diff_entry_threshold: default_diff_entry_threshold(),
            prob_cap: default_prob_cap(),
            diff_overheat_threshold: default_diff_overheat_threshold(),
            prob_overheat: default_prob_overheat(),
            diff_neutral_threshold: default_diff_neutral_threshold(),
            neutral_hold_secs: default_neutral_hold_secs(),
            diff_floor_stop: default_diff_floor_stop(),
            hold_to_expiry_secs: default_hold_to_expiry_secs(),
            min_time_remaining_secs: default_min_time_remaining(),
            max_time_remaining_secs: default_max_time_remaining(),
            stake_usd: default_stake_usd(),
            cooldown_secs: default_cooldown_secs(),
            max_positions: default_max_positions(),
            max_daily_trades: default_max_daily_trades(),
            allowed_window_secs: Vec::new(),
        }
    }
}

impl From<DirectionalConfig> for DiffRegularConfig {
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

// ── Internal state ──────────────────────────────────────

#[derive(Clone, Copy)]
struct SpotState {
    price: Decimal,
}

/// Dual-direction cooldown lock: when triggered, blocks BOTH UP and DOWN
/// entries for the symbol until the diff returns to neutral for
/// `neutral_hold_secs` consecutive seconds.
#[derive(Clone, Copy)]
struct DualCooldownLock {
    locked_at: DateTime<Utc>,
    neutral_since: Option<DateTime<Utc>>,
}

// ── Strategy ────────────────────────────────────────────

pub struct DiffRegularStrategy {
    config: DiffRegularConfig,
    spot: HashMap<Arc<str>, SpotState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    prev_diff: HashMap<Arc<str>, f64>,
    /// Per-symbol dual cooldown: locks BOTH directions when triggered.
    dual_cooldown: HashMap<Arc<str>, DualCooldownLock>,
    holdings: HashMap<Arc<str>, BasicHoldingState>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    retired_events: HashSet<Arc<str>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

impl DiffRegularStrategy {
    pub fn new(config: DiffRegularConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            prev_diff: HashMap::new(),
            dual_cooldown: HashMap::new(),
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

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        self.events
            .get(symbol)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| {
                        !self.retired_events.contains(&event.event_id)
                            && self.window_allowed(event.window_secs)
                            && event.end_time > now
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

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    /// Check and update the dual-direction cooldown lock for a symbol.
    /// Returns `true` if the symbol is currently locked (entry blocked).
    fn is_dual_locked(&mut self, symbol: &Arc<str>, diff: f64, now: DateTime<Utc>) -> bool {
        if let Some(lock) = self.dual_cooldown.get_mut(symbol) {
            if diff.abs() < self.config.diff_neutral_threshold {
                // Diff returned to neutral zone
                if lock.neutral_since.is_none() {
                    lock.neutral_since = Some(now);
                }
                if let Some(neutral_since) = lock.neutral_since {
                    if (now - neutral_since).num_seconds() >= self.config.neutral_hold_secs as i64 {
                        // Held neutral long enough — unlock
                        self.dual_cooldown.remove(symbol);
                        debug!(symbol = %symbol, "dual cooldown cleared after neutral hold");
                        return false;
                    }
                }
            } else {
                // Diff moved away from neutral — reset the neutral timer
                if let Some(lock) = self.dual_cooldown.get_mut(symbol) {
                    lock.neutral_since = None;
                }
            }
            return true;
        }
        false
    }

    /// Arm the dual-direction cooldown lock for a symbol.
    fn arm_dual_lock(&mut self, symbol: &Arc<str>, now: DateTime<Utc>) {
        info!(symbol = %symbol, "arming dual-direction cooldown lock");
        self.dual_cooldown.insert(
            symbol.clone(),
            DualCooldownLock {
                locked_at: now,
                neutral_since: None,
            },
        );
    }

    /// Stepped take-profit threshold: linearly interpolates from 90% at
    /// `hold_to_expiry_secs` to 100% at 0s remaining.
    fn take_profit_threshold(&self, remaining_secs: i64) -> f64 {
        let hold = self.config.hold_to_expiry_secs as f64;
        if remaining_secs as f64 <= hold {
            // Inside hold-to-expiry zone — don't exit, let it settle
            return 2.0; // unreachable threshold
        }
        // Linear from 0.90 (at hold_to_expiry boundary) to 1.00 (at 0s)
        let t = (remaining_secs as f64 - hold).max(0.0);
        let max_range = (self.config.max_time_remaining_secs as f64 - hold).max(1.0);
        let ratio = (t / max_range).clamp(0.0, 1.0);
        // At t=0 (near expiry): 0.90, at t=max_range: 1.00
        0.90 + 0.10 * (1.0 - ratio)
    }

    fn build_signal(
        &self,
        event: &EventWindow,
        token_id: &str,
        direction: &str,
        diff: f64,
        entry_price: Decimal,
        now: DateTime<Utc>,
    ) -> SignalRecord {
        SignalRecord {
            strategy: self.name().to_string(),
            event_id: Some(event.event_id.to_string()),
            token_id: Some(token_id.to_string()),
            intent_id: None,
            symbol: event.symbol.to_string(),
            direction: direction.to_string(),
            p_hat: entry_price.to_f64().unwrap_or(0.0),
            edge: diff.abs(),
            entry_price,
            decision: "enter".to_string(),
            ts: now,
        }
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
                intent_id: format!("diff_regular_settle_{}_up", event.event_id),
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
                intent_id: format!("diff_regular_settle_{}_down", event.event_id),
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

    /// Core entry evaluation: diff crossover detection with dual-direction cooldown.
    fn evaluate_entry(
        &mut self,
        event: &EventWindow,
        symbol: &Arc<str>,
        diff: f64,
        prev_diff: f64,
        ts: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        let remaining = (event.end_time - ts).num_seconds();
        if remaining < self.config.min_time_remaining_secs as i64
            || remaining > self.config.max_time_remaining_secs as i64
        {
            return None;
        }

        // Detect crossover direction
        let threshold = self.config.diff_entry_threshold;
        let (direction_str, token_id) = if prev_diff <= threshold && diff > threshold {
            ("UP", &event.up_token)
        } else if prev_diff >= -threshold && diff < -threshold {
            ("DOWN", &event.down_token)
        } else {
            return None;
        };

        // Probability gate: ask price = implied probability, must be < prob_cap
        // But first check overheat — high prob or extreme diff arms dual lock
        // even if we'd also reject on prob_cap.
        let quote = self.quotes.get(token_id)?;
        let ask = quote.ask?.to_f64()?;

        // Overheat check: high probability OR extreme diff → arm dual lock
        if ask >= self.config.prob_overheat || diff.abs() >= self.config.diff_overheat_threshold {
            self.arm_dual_lock(symbol, ts);
            return None;
        }

        if ask >= self.config.prob_cap {
            debug!(
                event_id = %event.event_id,
                direction = direction_str,
                ask,
                "entry rejected: probability exceeds cap"
            );
            return None;
        }

        // Dual-direction cooldown check
        if self.dual_cooldown.contains_key(symbol) {
            debug!(
                event_id = %event.event_id,
                direction = direction_str,
                "entry rejected: dual cooldown active"
            );
            return None;
        }

        // Existing position / active order check
        if positions.net_qty(token_id) > Decimal::ZERO || active_order_exists(token_id, orders) {
            return None;
        }

        // Per-symbol cooldown
        if let Some(last_entry) = self.last_entry.get(symbol) {
            if (ts - *last_entry).num_seconds() < self.config.cooldown_secs as i64 {
                return None;
            }
        }

        let entry_price = Decimal::try_from(ask).ok()?;
        let quantity = self.entry_quantity(entry_price);
        if quantity <= Decimal::ZERO {
            return None;
        }

        let intent_id = format!(
            "diff_regular_{}_{}_{}",
            event.event_id,
            direction_str.to_lowercase(),
            ts.timestamp_millis()
        );
        let mut signal = self.build_signal(event, token_id, direction_str, diff, entry_price, ts);
        signal.intent_id = Some(intent_id.clone());

        let intent = TradingIntent {
            intent_id,
            deployment_id: String::new(),
            market_id: event.event_id.to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity,
            limit_price: Some(entry_price),
            purpose: IntentPurpose::Entry,
            created_at: ts,
        };

        info!(
            event_id = %event.event_id,
            direction = direction_str,
            diff,
            ask,
            remaining,
            "diff_regular entry signal"
        );

        Some(StrategyDecision::Enter {
            intent,
            signal: Some(signal),
        })
    }

    /// Exit logic: stepped take-profit + floor stop.
    fn exit_decisions_for_symbol(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();
        let spot_f = self.spot.get(symbol).and_then(|s| s.price.to_f64());

        for event in self.events.get(symbol).into_iter().flatten() {
            if self.retired_events.contains(&event.event_id) {
                continue;
            }

            let remaining = (event.end_time - now).num_seconds();
            let price_to_beat = event.price_to_beat.and_then(|p| p.to_f64());

            for (token_id, is_up) in [(&event.up_token, true), (&event.down_token, false)] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }

                let Some(exit_bid) = self.quotes.get(token_id).and_then(|q| q.bid) else {
                    continue;
                };

                // Floor stop: abs(diff) <= floor_threshold → exit immediately
                if let (Some(ptb), Some(spot)) = (price_to_beat, spot_f) {
                    if ptb > 0.0 {
                        let diff = (spot - ptb) / ptb;
                        let favorable = if is_up { diff > 0.0 } else { diff < 0.0 };
                        if diff.abs() <= self.config.diff_floor_stop && !favorable {
                            debug!(
                                event_id = %event.event_id,
                                token_id = %token_id,
                                diff,
                                "floor stop triggered"
                            );
                            decisions.push(StrategyDecision::Exit(TradingIntent {
                                intent_id: format!(
                                    "diff_regular_floor_{}_{}",
                                    token_id,
                                    now.timestamp_millis()
                                ),
                                deployment_id: String::new(),
                                market_id: event.event_id.to_string(),
                                token_id: token_id.to_string(),
                                side: TradeSide::Sell,
                                quantity: qty,
                                limit_price: Some(exit_bid),
                                purpose: IntentPurpose::Exit,
                                created_at: now,
                            }));
                            continue;
                        }
                    }
                }

                // Hold to expiry: if remaining < hold_to_expiry_secs, don't exit
                if remaining > 0 && remaining < self.config.hold_to_expiry_secs as i64 {
                    continue;
                }

                // Stepped take-profit via quote bid
                if let Some(bid) = exit_bid.to_f64() {
                    let tp_threshold = self.take_profit_threshold(remaining);
                    if bid >= tp_threshold {
                        info!(
                            event_id = %event.event_id,
                            token_id = %token_id,
                            bid,
                            tp_threshold,
                            remaining,
                            "stepped take-profit triggered"
                        );
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!(
                                "diff_regular_tp_{}_{}",
                                token_id,
                                now.timestamp_millis()
                            ),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_bid),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                    }
                }
            }
        }

        decisions
    }

    /// Main spot-price handler: compute diff, update cooldown, check entry/exit.
    fn handle_spot(
        &mut self,
        symbol: &Arc<str>,
        price: Decimal,
        ts: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        if !self
            .config
            .symbols
            .iter()
            .any(|configured| configured.as_str() == symbol.as_ref())
        {
            return Vec::new();
        }

        self.reset_daily_counter(ts);
        self.spot.insert(symbol.clone(), SpotState { price });

        // Backfill price_to_beat for events that don't have one yet
        if let Some(events) = self.events.get_mut(symbol.as_ref()) {
            for event in events {
                if event.price_to_beat.is_none() {
                    event.price_to_beat = Some(price);
                }
            }
        }

        let spot_f = match price.to_f64() {
            Some(v) => v,
            None => return Vec::new(),
        };

        // Check exits first
        let exits = self.exit_decisions_for_symbol(symbol, ts, positions);
        if !exits.is_empty() {
            return exits;
        }

        if self.daily_trade_count >= self.config.max_daily_trades
            || positions.positions().count() >= self.config.max_positions
        {
            return Vec::new();
        }

        // Compute diff for each candidate event and check crossover
        let candidates = self.candidate_events(symbol, ts);
        for event in &candidates {
            let ptb = match event.price_to_beat.and_then(|p| p.to_f64()) {
                Some(v) if v > 0.0 => v,
                _ => continue,
            };

            let diff = (spot_f - ptb) / ptb;
            let prev = *self.prev_diff.get(&event.event_id).unwrap_or(&0.0);

            // Update dual cooldown state
            self.is_dual_locked(symbol, diff, ts);

            // Store current diff as prev for next tick
            self.prev_diff.insert(event.event_id.clone(), diff);

            if let Some(decision) =
                self.evaluate_entry(&event.clone(), symbol, diff, prev, ts, positions, orders)
            {
                return vec![decision];
            }
        }

        Vec::new()
    }
}

impl StrategyLogic for DiffRegularStrategy {
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

                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                self.exit_decisions_for_symbol(&symbol, *ts, positions)
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
                    .any(|configured| configured.as_str() == symbol.as_ref())
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
                Vec::new()
            }
            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won,
            } => {
                let mut decisions = Vec::new();
                let mut resolved_events = Vec::new();

                for events in self.events.values() {
                    for event in events {
                        if event.event_id != *event_id {
                            continue;
                        }
                        if let Some(up_won) = self.resolve_up_won(event, *resolved_up_won) {
                            decisions.extend(
                                self.build_settlement_exits(event, up_won, *end_time, positions),
                            );
                            resolved_events.push(event.event_id.clone());
                        }
                    }
                }

                if !resolved_events.is_empty() {
                    for events in self.events.values_mut() {
                        events.retain(|event| !resolved_events.contains(&event.event_id));
                    }
                    for eid in &resolved_events {
                        self.prev_diff.remove(eid);
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
                let token: Arc<str> = Arc::from(fill.token_id.as_str());
                if let Some(symbol) = self.token_symbol.get(&token).cloned() {
                    self.last_entry.insert(symbol, fill.timestamp);
                }
                self.daily_trade_count += 1;

                let direction = if self
                    .events
                    .values()
                    .flatten()
                    .any(|e| e.up_token.as_ref() == fill.token_id.as_str())
                {
                    "UP"
                } else {
                    "DOWN"
                };

                self.holdings.insert(
                    token.clone(),
                    BasicHoldingState {
                        token_id: token,
                        direction: direction.to_string(),
                        entry_time: fill.timestamp,
                    },
                );
            }
            TradeSide::Sell => {
                let token: Arc<str> = Arc::from(fill.token_id.as_str());
                self.holdings.remove(&token);
                if let Some(event_id) = self.token_event.get(&token).cloned() {
                    self.retired_events.insert(event_id);
                }
            }
        }
    }

    fn name(&self) -> &str {
        "diff_regular"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    #[test]
    fn strategy_name() {
        let s = DiffRegularStrategy::new(DiffRegularConfig::default());
        assert_eq!(s.name(), "diff_regular");
    }

    #[test]
    fn diff_crossover_triggers_up_entry() {
        let config = DiffRegularConfig {
            symbols: vec!["BTCUSDT".into()],
            diff_entry_threshold: 0.00057,
            prob_cap: 0.75,
            cooldown_secs: 0,
            min_time_remaining_secs: 48,
            max_time_remaining_secs: 168,
            ..DiffRegularConfig::default()
        };
        let mut strategy = DiffRegularStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // Register event
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + Duration::seconds(100),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Quote for UP token
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.48)),
                ask: Some(dec!(0.50)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        // First spot: diff = 0 (at price_to_beat)
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now + Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        // Second spot: diff = 60/100000 = 0.0006 > 0.00057 threshold
        // prev_diff was 0, now crosses above threshold → UP crossover
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100060),
                ts: now + Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected UP entry on diff crossover, got {decisions:?}"
        );
    }

    #[test]
    fn high_probability_arms_dual_lock_blocks_both_directions() {
        let config = DiffRegularConfig {
            symbols: vec!["BTCUSDT".into()],
            diff_entry_threshold: 0.00057,
            prob_cap: 0.75,
            prob_overheat: 0.85,
            cooldown_secs: 0,
            min_time_remaining_secs: 48,
            max_time_remaining_secs: 168,
            ..DiffRegularConfig::default()
        };
        let mut strategy = DiffRegularStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt2".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up2".into(),
                down_token: "dn2".into(),
                end_time: now + Duration::seconds(100),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Quote with ask >= prob_overheat (0.85) → should arm dual lock
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up2".into(),
                bid: Some(dec!(0.84)),
                ask: Some(dec!(0.86)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        // First spot: diff = 0
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now + Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        // Crossover spot: diff > threshold, but ask >= prob_overheat → dual lock
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100060),
                ts: now + Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        assert!(
            !decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected no entry due to overheat dual lock"
        );

        // Verify the lock is armed
        assert!(
            strategy.dual_cooldown.contains_key("BTCUSDT"),
            "dual cooldown should be armed"
        );
    }

    #[test]
    fn floor_stop_exits_position() {
        let config = DiffRegularConfig {
            symbols: vec!["BTCUSDT".into()],
            diff_floor_stop: 0.00007,
            ..DiffRegularConfig::default()
        };
        let mut strategy = DiffRegularStrategy::new(config);
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt3".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up3".into(),
                down_token: "dn3".into(),
                end_time: now + Duration::seconds(100),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Simulate a buy fill to create a position
        let buy_fill = FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up3".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.50),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        positions.apply_fill(&buy_fill);
        strategy.on_fill(&buy_fill);

        let no_quote_decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(99999),
                ts: now + Duration::seconds(5),
            },
            &positions,
            &orders,
        );
        assert!(
            no_quote_decisions.is_empty(),
            "floor stop must not emit an exit without an executable bid"
        );

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up3".into(),
                bid: Some(dec!(0.44)),
                ask: Some(dec!(0.45)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now + Duration::seconds(6),
            },
            &positions,
            &orders,
        );

        // Spot near price_to_beat but slightly negative diff (unfavorable for UP)
        // diff = (99999 - 100000) / 100000 = -0.00001, abs = 0.00001 < 0.00007
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(99999),
                ts: now + Duration::seconds(10),
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Exit(..))),
            "expected floor stop exit, got {decisions:?}"
        );
        match &decisions[0] {
            StrategyDecision::Exit(intent) => assert_eq!(intent.limit_price, Some(dec!(0.44))),
            other => panic!("expected exit, got {other:?}"),
        }
    }
}
