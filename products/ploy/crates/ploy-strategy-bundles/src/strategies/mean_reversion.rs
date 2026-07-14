//! Mean-reversion PM5D strategy prototype (V4).
//!
//! This strategy is intentionally scoped to currently-available live data:
//! Binance spot prices, PM quotes, event metadata, and the existing
//! `ReturnBuffer`-style drift/acceleration view. It does not depend on live L2.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use super::common::event::EventWindow;
use super::common::fees::crypto_fee_cost;
use super::common::guards::active_order_exists;
use super::common::quote::QuoteState;
use super::common::settlement;
use super::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

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

const EWMA_LAMBDA: f64 = 0.94;
const RETURN_BUFFER_WINDOW_SECS: f64 = 300.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
}

struct SpotState {
    price: Decimal,
    ts: DateTime<Utc>,
}

struct ReturnBuffer {
    entries: Vec<(f64, f64)>,
    total_secs: f64,
    high: f64,
    low: f64,
}

impl ReturnBuffer {
    fn new() -> Self {
        Self {
            entries: Vec::with_capacity(256),
            total_secs: 0.0,
            high: f64::NEG_INFINITY,
            low: f64::INFINITY,
        }
    }

    fn push(&mut self, log_return: f64, dt_secs: f64, price: f64, window_secs: f64) {
        self.entries.push((log_return, dt_secs));
        self.total_secs += dt_secs;

        if price > self.high {
            self.high = price;
        }
        if price < self.low {
            self.low = price;
        }

        while self.total_secs > window_secs && self.entries.len() > 2 {
            let (_, old_dt) = self.entries.remove(0);
            self.total_secs -= old_dt;
        }
    }

    fn realized_var_per_sec(&self) -> f64 {
        if self.total_secs <= 0.0 || self.entries.is_empty() {
            return 0.0;
        }
        let sum_r2: f64 = self.entries.iter().map(|(r, _)| r * r).sum();
        sum_r2 / self.total_secs
    }

    fn parkinson_var_per_sec(&self) -> f64 {
        if self.high <= 0.0 || self.low <= 0.0 || self.high <= self.low || self.total_secs <= 0.0 {
            return 0.0;
        }
        let log_hl = (self.high / self.low).ln();
        log_hl * log_hl / (4.0 * std::f64::consts::LN_2 * self.total_secs)
    }

    fn directional_consistency(&self) -> (f64, f64) {
        if self.entries.is_empty() {
            return (0.5, 0.0);
        }
        let up_count = self.entries.iter().filter(|(r, _)| *r > 0.0).count();
        let total = self.entries.len();
        let up_frac = up_count as f64 / total as f64;
        if up_frac >= 0.5 {
            (up_frac, 1.0)
        } else {
            (1.0 - up_frac, -1.0)
        }
    }

    fn drift_speed(&self) -> f64 {
        if self.total_secs <= 0.0 {
            return 0.0;
        }
        let cum_return: f64 = self.entries.iter().map(|(r, _)| r).sum();
        cum_return / self.total_secs
    }

    fn drift_acceleration(&self) -> f64 {
        let n = self.entries.len();
        if n < 4 {
            return 0.0;
        }
        let mid = n / 2;
        let (old_sum, old_dt): (f64, f64) = self.entries[..mid]
            .iter()
            .fold((0.0, 0.0), |(sr, sd), (r, d)| (sr + r, sd + d));
        let (new_sum, new_dt): (f64, f64) = self.entries[mid..]
            .iter()
            .fold((0.0, 0.0), |(sr, sd), (r, d)| (sr + r, sd + d));
        if old_dt <= 0.0 || new_dt <= 0.0 {
            return 0.0;
        }
        (new_sum / new_dt) - (old_sum / old_dt)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Default)]
struct VolatilityState {
    ewma_var_per_sec: f64,
}

struct EntryState {
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    event_id: Arc<str>,
    remaining_qty: Decimal,
}

pub struct MeanReversionStrategy {
    config: DirectionalConfig,
    spot: HashMap<Arc<str>, SpotState>,
    volatility: HashMap<Arc<str>, VolatilityState>,
    return_buffers: HashMap<Arc<str>, ReturnBuffer>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    cooldowns: HashMap<Arc<str>, DateTime<Utc>>,
    daily_trades: u32,
    last_trade_date: Option<chrono::NaiveDate>,
    daily_realized_pnl: Decimal,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    entry_state: HashMap<Arc<str>, EntryState>,
    retired_events: HashSet<Arc<str>>,
    feed_time: Option<DateTime<Utc>>,
    balance_exhausted_until: Option<DateTime<Utc>>,
}

impl MeanReversionStrategy {
    pub fn new(config: DirectionalConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            volatility: HashMap::new(),
            return_buffers: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            cooldowns: HashMap::new(),
            daily_trades: 0,
            last_trade_date: None,
            daily_realized_pnl: Decimal::ZERO,
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            entry_state: HashMap::new(),
            retired_events: HashSet::new(),
            feed_time: None,
            balance_exhausted_until: None,
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

        let log_return = (curr_f / prev_f).ln();
        let inst_var_per_sec = log_return * log_return / dt_secs.max(1e-6);
        let floor = self.floor_var_per_sec();
        let state = self.volatility.entry(Arc::from(symbol)).or_default();
        state.ewma_var_per_sec = if state.ewma_var_per_sec <= 0.0 {
            inst_var_per_sec.max(floor)
        } else {
            (EWMA_LAMBDA * state.ewma_var_per_sec) + ((1.0 - EWMA_LAMBDA) * inst_var_per_sec)
        };

        let buf = self
            .return_buffers
            .entry(Arc::from(symbol))
            .or_insert_with(ReturnBuffer::new);
        buf.push(log_return, dt_secs, curr_f, RETURN_BUFFER_WINDOW_SECS);
    }

    fn sigma_horizon(&self, symbol: &str, time_remaining_secs: f64) -> f64 {
        let secs = time_remaining_secs.max(1.0);
        let floor = self.floor_var_per_sec();
        let ewma = self
            .volatility
            .get(symbol)
            .map(|state| state.ewma_var_per_sec)
            .unwrap_or(floor);
        let (rv, parkinson) = self
            .return_buffers
            .get(symbol)
            .filter(|buf| buf.len() >= 5)
            .map(|buf| (buf.realized_var_per_sec(), buf.parkinson_var_per_sec()))
            .unwrap_or((0.0, 0.0));
        let best_var = ewma.max(rv).max(parkinson).max(floor);
        (best_var * secs).sqrt()
    }

    fn reset_daily_counter(&mut self, now: DateTime<Utc>) {
        let today = now.date_naive();
        if self.last_trade_date != Some(today) {
            self.daily_trades = 0;
            self.daily_realized_pnl = Decimal::ZERO;
            self.last_trade_date = Some(today);
        }
    }

    fn in_cooldown(&self, symbol: &str, now: DateTime<Utc>) -> bool {
        self.cooldowns
            .get(symbol)
            .is_some_and(|last| (now - *last).num_seconds() < self.config.cooldown_secs as i64)
    }

    fn shares_for_entry_price(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        self.events
            .get(symbol)
            .map(|events| {
                let mut candidates: Vec<_> = events
                    .iter()
                    .filter(|event| {
                        if self.retired_events.contains(&event.event_id) {
                            return false;
                        }
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

    fn event_has_open_position(&self, event: &EventWindow, positions: &PositionLedger) -> bool {
        positions.net_qty(&event.up_token) > Decimal::ZERO
            || positions.net_qty(&event.down_token) > Decimal::ZERO
    }

    fn event_has_active_order(&self, event: &EventWindow, orders: &OrderLedger) -> bool {
        active_order_exists(&event.up_token, orders)
            || active_order_exists(&event.down_token, orders)
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
        settlement::resolve_up_won(
            settlement,
            self.spot.get(&event.symbol).map(|spot| spot.price),
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
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("mr_settle_{}", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.up_token.to_string(),
                side: TradeSide::Sell,
                quantity: positions.net_qty(&event.up_token),
                limit_price: Some(if up_won { dec!(1.00) } else { dec!(0.00) }),
                purpose: IntentPurpose::Exit,
                created_at: end_time,
            }));
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            exits.push(StrategyDecision::Exit(TradingIntent {
                intent_id: format!("mr_settle_{}", event.event_id),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: event.down_token.to_string(),
                side: TradeSide::Sell,
                quantity: positions.net_qty(&event.down_token),
                limit_price: Some(if up_won { dec!(0.00) } else { dec!(1.00) }),
                purpose: IntentPurpose::Exit,
                created_at: end_time,
            }));
        }

        exits
    }

    fn settle_expired_events_for_symbol(
        &mut self,
        symbol: &str,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        let expired_events: Vec<_> = self
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
        let mut resolved = HashSet::new();

        for event in expired_events {
            if !self.event_has_open_position(&event, positions) {
                resolved.insert(event.event_id.clone());
                continue;
            }

            let Some(up_won) = self.resolve_expired_event_outcome(&event, None) else {
                continue;
            };

            exits.extend(self.build_settlement_exits(&event, event.end_time, up_won, positions));
            resolved.insert(event.event_id.clone());
            self.retired_events.remove(&event.event_id);
        }

        if let Some(events) = self.events.get_mut(symbol) {
            events.retain(|event| !resolved.contains(&event.event_id));
        }

        exits
    }

    fn try_exit(
        &self,
        symbol: &str,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        for event in self.events.get(symbol).into_iter().flatten() {
            for token_id in [&event.up_token, &event.down_token] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }

                let Some(entry) = self.entry_state.get(token_id) else {
                    continue;
                };
                let Some(quote) = self.quotes.get(token_id) else {
                    continue;
                };
                let Some(bid) = quote.bid else {
                    continue;
                };

                let bid_f = match bid.to_f64() {
                    Some(value) => value,
                    None => continue,
                };
                let entry_f = match entry.entry_price.to_f64() {
                    Some(value) => value,
                    None => continue,
                };

                let timed_out =
                    (now - entry.entry_time).num_seconds() >= self.config.max_hold_secs as i64;
                let take_profit_hit = bid_f >= entry_f + self.config.take_profit_price_delta;
                let stop_loss_hit = bid_f <= entry_f - self.config.stop_loss_price_delta;

                if !(take_profit_hit || stop_loss_hit || timed_out) {
                    continue;
                }

                exits.push(StrategyDecision::Exit(TradingIntent {
                    intent_id: format!("mr_exit_{}_{}", event.event_id, now.timestamp_millis()),
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

        exits
    }

    fn evaluate_entry(
        &self,
        symbol: &str,
        spot_price: Decimal,
        event: &EventWindow,
        now: DateTime<Utc>,
    ) -> Option<(Direction, Decimal, f64, f64)> {
        let open_price = event.price_to_beat?;
        if open_price <= Decimal::ZERO || spot_price <= Decimal::ZERO {
            return None;
        }

        let s0 = open_price.to_f64()?;
        let st = spot_price.to_f64()?;
        let deviation_pct = (st / s0) - 1.0;
        if deviation_pct.abs() < self.config.min_deviation_pct {
            return None;
        }

        let secs_remaining = (event.end_time - now).num_seconds().max(0) as f64;
        let sigma_horizon = self.sigma_horizon(symbol, secs_remaining);
        let p_up = estimate_probability(s0, st, sigma_horizon);

        let direction = if deviation_pct < 0.0 {
            Direction::Up
        } else {
            Direction::Down
        };

        let (token_id, entry_price, directional_p) = match direction {
            Direction::Up => (
                &event.up_token,
                self.quotes.get(&event.up_token)?.ask?,
                p_up,
            ),
            Direction::Down => (
                &event.down_token,
                self.quotes.get(&event.down_token)?.ask?,
                1.0 - p_up,
            ),
        };
        let entry_f = entry_price.to_f64()?;

        // Mean reversion should not start from the pure directional probability alone:
        // once the market is stretched away from S0, the prototype assumes a neutralizing
        // pull back toward 0.5 before reversal evidence lifts confidence further.
        let countertrend_floor = (0.5 - (deviation_pct.abs() / 2.0)).clamp(0.35, 0.50);
        let base_p = directional_p.max(countertrend_floor);

        if entry_f < self.config.min_entry_price
            || entry_f > self.config.max_entry_price
            || (entry_f >= self.config.no_trade_zone_min
                && entry_f <= self.config.no_trade_zone_max)
        {
            debug!(
                symbol = %symbol,
                token_id = %token_id,
                entry_price = entry_f,
                "mean-reversion price filter"
            );
            return None;
        }

        let buf = self.return_buffers.get(symbol)?;
        if buf.len() < 6 {
            return None;
        }

        let target_dir = if direction == Direction::Up {
            1.0
        } else {
            -1.0
        };
        let drift_alignment = buf.drift_speed() * target_dir;
        let accel_alignment = buf.drift_acceleration() * target_dir;
        let (consistency, dominant_dir) = buf.directional_consistency();

        if accel_alignment <= 0.0 {
            return None;
        }

        let mut reversal_bonus = 0.0;
        if drift_alignment > 0.0 {
            reversal_bonus += 0.05;
        } else {
            reversal_bonus -= 0.05;
        }
        reversal_bonus += accel_alignment.clamp(0.0, 0.20).min(0.08);

        if dominant_dir * target_dir > 0.0 {
            reversal_bonus +=
                ((consistency - 0.5).max(0.0) * 0.30).min(self.config.reversal_bonus_cap / 2.0);
        } else if consistency >= self.config.min_reversal_consistency {
            reversal_bonus -=
                ((consistency - 0.5) * 0.30).min(self.config.reversal_bonus_cap / 2.0);
        }

        let mispricing_bonus = (base_p - entry_f)
            .max(0.0)
            .min(self.config.reversal_bonus_cap / 2.0);
        let effective_p = (base_p
            + reversal_bonus.clamp(
                -self.config.reversal_bonus_cap,
                self.config.reversal_bonus_cap,
            )
            + mispricing_bonus)
            .clamp(0.01, 0.99);

        if consistency < self.config.min_reversal_consistency && dominant_dir * target_dir <= 0.0 {
            return None;
        }
        if effective_p < self.config.min_probability {
            return None;
        }

        let edge = effective_p - entry_f - crypto_fee_cost(entry_f);
        if edge < self.config.min_edge {
            return None;
        }

        Some((direction, entry_price, effective_p, edge))
    }

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
                "pm5d_mr_{}_{}_{}",
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

    fn try_entry(
        &self,
        symbol: &str,
        positions: &PositionLedger,
        orders: &OrderLedger,
        now: DateTime<Utc>,
    ) -> Vec<StrategyDecision> {
        if let Some(until) = self.balance_exhausted_until {
            if now < until {
                return Vec::new();
            }
        }
        if let Some(max_loss) = self.config.max_daily_loss_usd {
            if self.daily_realized_pnl <= -max_loss {
                return Vec::new();
            }
        }
        if positions.positions().count() >= self.config.max_positions {
            return Vec::new();
        }

        let spot_price = match self.spot.get(symbol) {
            Some(state) => state.price,
            None => return Vec::new(),
        };

        for event in self.candidate_events(symbol, now) {
            if self.event_has_open_position(&event, positions)
                || self.event_has_active_order(&event, orders)
            {
                continue;
            }

            if let Some((direction, entry_price, effective_p, edge)) =
                self.evaluate_entry(symbol, spot_price, &event, now)
            {
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
                info!(
                    symbol,
                    event_id = %event.event_id,
                    ?direction,
                    entry_price = %entry_price,
                    effective_p,
                    edge,
                    "mean-reversion entry passed"
                );
                return vec![StrategyDecision::Enter {
                    intent,
                    signal: Some(signal),
                }];
            }
        }

        Vec::new()
    }
}

impl StrategyLogic for MeanReversionStrategy {
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
                    return Vec::new();
                }

                if self.feed_time.is_none_or(|ft| *ts > ft) {
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

                let settlement_exits =
                    self.settle_expired_events_for_symbol(symbol, positions, *ts);
                if !settlement_exits.is_empty() {
                    return settlement_exits;
                }

                let active_exits = self.try_exit(symbol, positions, *ts);
                if !active_exits.is_empty() {
                    return active_exits;
                }

                self.reset_daily_counter(*ts);
                if self.daily_trades >= self.config.max_daily_trades
                    || self.in_cooldown(symbol, *ts)
                {
                    return Vec::new();
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

                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                if self.feed_time.is_none_or(|ft| *ts > ft) {
                    self.feed_time = Some(*ts);
                }

                let active_exits = self.try_exit(&symbol, positions, *ts);
                if !active_exits.is_empty() {
                    return active_exits;
                }

                if self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                    && self.daily_trades < self.config.max_daily_trades
                    && !self.in_cooldown(&symbol, *ts)
                {
                    return self.try_entry(&symbol, positions, orders, *ts);
                }

                Vec::new()
            }
            MarketUpdate::AggTrade { .. } => Vec::new(),
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
                    return Vec::new();
                }
                let now = self
                    .feed_time
                    .unwrap_or_else(|| *end_time - chrono::Duration::seconds(1));

                let events = self.events.entry(symbol.clone()).or_default();
                if events.iter().any(|event| event.event_id == *event_id) {
                    return Vec::new();
                }

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
                    price_to_beat: price_to_beat
                        .or_else(|| self.spot.get(symbol).map(|state| state.price)),
                });

                if self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                    && self.daily_trades < self.config.max_daily_trades
                    && !self.in_cooldown(symbol, now)
                {
                    return self.try_entry(symbol, positions, orders, now);
                }

                Vec::new()
            }
            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won: settlement,
            } => {
                let mut exits = Vec::new();
                let mut remove = true;

                let mut matching = Vec::new();
                for events in self.events.values() {
                    for event in events {
                        if event.event_id == *event_id {
                            matching.push(event.clone());
                        }
                    }
                }

                for event in matching {
                    if !self.event_has_open_position(&event, positions) {
                        continue;
                    }

                    let Some(up_won) = self.resolve_expired_event_outcome(&event, *settlement)
                    else {
                        warn!(event_id = %event_id, "mean-reversion settlement pending");
                        remove = false;
                        continue;
                    };
                    exits.extend(self.build_settlement_exits(&event, *end_time, up_won, positions));
                    self.retired_events.remove(&event.event_id);
                }

                if remove {
                    for events in self.events.values_mut() {
                        events.retain(|event| event.event_id != *event_id);
                    }
                }

                exits
            }
            _ => Vec::new(),
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if let Some(symbol) = self.token_symbol.get(fill.token_id.as_str()) {
            self.cooldowns.insert(symbol.clone(), fill.timestamp);
            self.daily_trades += 1;
        }

        match fill.side {
            TradeSide::Buy => {
                let event_id = self
                    .token_event
                    .get(fill.token_id.as_str())
                    .cloned()
                    .unwrap_or_default();
                self.entry_state
                    .entry(Arc::from(fill.token_id.as_str()))
                    .and_modify(|entry| {
                        let total_qty = entry.remaining_qty + fill.quantity;
                        if total_qty > Decimal::ZERO {
                            let total_cost = (entry.entry_price * entry.remaining_qty)
                                + (fill.price * fill.quantity);
                            entry.entry_price = total_cost / total_qty;
                        }
                        entry.remaining_qty = total_qty;
                        entry.entry_time = fill.timestamp;
                        if entry.event_id.is_empty() {
                            entry.event_id = event_id.clone();
                        }
                    })
                    .or_insert(EntryState {
                        entry_price: fill.price,
                        entry_time: fill.timestamp,
                        event_id,
                        remaining_qty: fill.quantity,
                    });
            }
            TradeSide::Sell => {
                if let Some(entry) = self.entry_state.get_mut(fill.token_id.as_str()) {
                    let matched_qty = fill.quantity.min(entry.remaining_qty);
                    let pnl = (fill.price - entry.entry_price) * matched_qty - fill.fee;
                    self.daily_realized_pnl += pnl;
                    entry.remaining_qty -= matched_qty;
                    let retire_event = entry.remaining_qty <= Decimal::ZERO;
                    let event_id = entry.event_id.clone();
                    if retire_event {
                        self.entry_state.remove(fill.token_id.as_str());
                        if !event_id.is_empty() {
                            self.retired_events.insert(event_id);
                        }
                    }
                }
            }
        }
    }

    fn on_reject(&mut self, intent: &TradingIntent, reason: &str) {
        let now = self.feed_time.unwrap_or_else(Utc::now);

        if reason.contains("not enough balance") {
            self.balance_exhausted_until = Some(now + chrono::Duration::minutes(5));
            return;
        }

        if let Some(symbol) = self.token_symbol.get(intent.token_id.as_str()).cloned() {
            self.cooldowns.insert(symbol, now);
        }
    }

    fn name(&self) -> &str {
        "pm_5m_mean_reversion"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::MarketUpdate;
    use ploy_trading::{OrderLedger, PositionLedger};

    fn default_config() -> DirectionalConfig {
        DirectionalConfig {
            symbols: vec!["BTCUSDT".into()],
            symbol_profiles: HashMap::new(),
            vol_floor: 0.001,
            min_probability: 0.45,
            min_z_score: 0.35,
            min_entry_price: 0.15,
            max_entry_price: 0.45,
            no_trade_zone_min: 0.46,
            no_trade_zone_max: 0.54,
            min_edge: 0.03,
            min_deviation_pct: 0.005,
            min_reversal_consistency: 0.50,
            min_trend_consistency: 0.50,
            min_trend_persistence_secs: 0,
            take_profit_price_delta: 0.10,
            stop_loss_price_delta: 0.05,
            max_hold_secs: 120,
            reversal_bonus_cap: 0.20,
            use_multiscale_volatility: true,
            use_price_structure_adjustment: true,
            reversal_max_distance_pct: 0.015,
            reversal_max_drift_flip_age_secs: 20,
            reversal_min_post_flip_drift: 0.0001,
            reversal_lob_depth_pct: 0.001,
            reversal_min_lob_depth_ratio: 1.3,
            reversal_max_ask_for_reversal: 0.25,
            reversal_max_pm_lag_secs: 30,
            reversal_take_profit_ask: 0.65,
            reversal_stop_distance_pct: 0.025,
            three_layer_strategy_profile: crate::strategies::ThreeLayerProfile::Mixed,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: dec!(25),
            max_positions: 1000,
            max_daily_trades: 1000,
            max_daily_loss_usd: None,
            allowed_window_secs: vec![300, 900],
            three_layer_min_direction_prob: 0.56,
            three_layer_min_distance_over_sigma: 0.3,
            three_layer_min_confirmation_score: 0.10,
            three_layer_require_confirmation: false,
            three_layer_min_drift_confirmation: 0.0002,
            three_layer_min_edge: 0.03,
            three_layer_min_reward_risk: 1.2,
            three_layer_alpha_contrarian: false,
            three_layer_cex_contrarian: false,
            three_layer_probability_shrink: 1.0,
            three_layer_probability_haircut: 0.0,
            three_layer_take_profit_ask: 0.70,
            three_layer_stop_distance_pct: 0.020,
            three_layer_max_pm_lag_secs: 15,
            three_layer_min_entry_score: 0.30,
            three_layer_autofactor_runtime_score: None,
            three_layer_event_ml_model_path: None,
        }
    }

    #[test]
    fn reversal_signal_buys_countertrend_token() {
        let mut strategy = MeanReversionStrategy::new(default_config());
        let now = Utc::now();
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        strategy.on_update(
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
        strategy.on_update(
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

        let mut saw_entry = false;

        for (idx, price) in [100000, 99500, 99000, 98500, 98000, 98250, 98600, 98950]
            .into_iter()
            .enumerate()
        {
            let decisions = strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price: Decimal::from(price),
                    ts: now + chrono::Duration::seconds((idx as i64) * 5),
                },
                &positions,
                &orders,
            );

            if idx < 5 {
                assert!(decisions.is_empty());
            } else if !decisions.is_empty() {
                match &decisions[0] {
                    StrategyDecision::Enter { intent, .. } => assert_eq!(intent.token_id, "up1"),
                    other => panic!("expected enter, got {other:?}"),
                }
                saw_entry = true;
                break;
            }
        }

        assert!(saw_entry, "expected an entry once reversal began");
    }

    #[test]
    fn quote_take_profit_emits_exit_before_settlement() {
        let mut strategy = MeanReversionStrategy::new(default_config());
        let now = Utc::now();
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        let buy_fill = FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.30),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        positions.apply_fill(&buy_fill);
        strategy.on_fill(&buy_fill);

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.42)),
                ask: Some(dec!(0.43)),
                ts: now + chrono::Duration::seconds(30),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "expected take-profit exit");
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "up1");
                assert_eq!(intent.limit_price, Some(dec!(0.42)));
            }
            other => panic!("expected exit, got {other:?}"),
        }
    }

    #[test]
    fn early_exit_retires_event_from_reentry() {
        let mut strategy = MeanReversionStrategy::new(default_config());
        let now = Utc::now();
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        let buy_fill = FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.30),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        positions.apply_fill(&buy_fill);
        strategy.on_fill(&buy_fill);

        let sell_fill = FillRecord {
            fill_id: "fill-2".into(),
            order_id: "order-2".into(),
            token_id: "up1".into(),
            side: TradeSide::Sell,
            quantity: dec!(5),
            price: dec!(0.42),
            fee: Decimal::ZERO,
            timestamp: now + chrono::Duration::seconds(30),
        };
        strategy.on_fill(&sell_fill);

        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(98950),
                ts: now + chrono::Duration::seconds(40),
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "event should be retired after early exit"
        );
    }

    #[test]
    fn partial_exit_keeps_remaining_position_exitable() {
        let mut strategy = MeanReversionStrategy::new(default_config());
        let now = Utc::now();
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        let buy_fill = FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.30),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        positions.apply_fill(&buy_fill);
        strategy.on_fill(&buy_fill);

        let partial_exit_fill = FillRecord {
            fill_id: "fill-2".into(),
            order_id: "order-2".into(),
            token_id: "up1".into(),
            side: TradeSide::Sell,
            quantity: dec!(2),
            price: dec!(0.42),
            fee: Decimal::ZERO,
            timestamp: now + chrono::Duration::seconds(30),
        };
        positions.apply_fill(&partial_exit_fill);
        strategy.on_fill(&partial_exit_fill);

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.45)),
                ask: Some(dec!(0.46)),
                ts: now + chrono::Duration::seconds(35),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        assert_eq!(positions.net_qty("up1"), dec!(3));
        assert_eq!(decisions.len(), 1, "remaining qty should still be exitable");
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "up1");
                assert_eq!(intent.quantity, dec!(3));
            }
            other => panic!("expected exit, got {other:?}"),
        }
    }
}
