use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::common::event::EventWindow;
use super::common::fees::crypto_fee_cost;
use super::common::guards::active_order_exists;
use super::common::quote::QuoteState;
use super::common::settlement;
use super::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

const DRIFT_WINDOW_SECS: i64 = 35;
const MIN_DRIFT_POINTS: usize = 4;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReversalConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,
    #[serde(default = "default_max_distance_pct")]
    pub max_distance_pct: f64,
    #[serde(default = "default_max_drift_flip_age_secs")]
    pub max_drift_flip_age_secs: u64,
    #[serde(default = "default_min_post_flip_drift")]
    pub min_post_flip_drift: f64,
    #[serde(default = "default_lob_depth_pct")]
    pub lob_depth_pct: f64,
    #[serde(default = "default_min_lob_depth_ratio")]
    pub min_lob_depth_ratio: f64,
    #[serde(default = "default_max_ask_for_reversal")]
    pub max_ask_for_reversal: f64,
    #[serde(default = "default_max_pm_lag_secs")]
    pub max_pm_lag_secs: u64,
    #[serde(default = "default_min_time_remaining_secs")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time_remaining_secs")]
    pub max_time_remaining_secs: u64,
    #[serde(default = "default_min_edge")]
    pub min_edge: f64,
    #[serde(default = "default_take_profit_ask")]
    pub take_profit_ask: f64,
    #[serde(default = "default_stop_distance_pct")]
    pub stop_distance_pct: f64,
    #[serde(default = "default_cooldown_secs")]
    pub cooldown_secs: u64,
    #[serde(default = "default_stake_usd")]
    pub stake_usd: Decimal,
    #[serde(default = "default_allowed_window_secs")]
    pub allowed_window_secs: Vec<u64>,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
}

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into(), "DOGEUSDT".into()]
}

fn default_max_distance_pct() -> f64 {
    0.015
}

fn default_max_drift_flip_age_secs() -> u64 {
    20
}

fn default_min_post_flip_drift() -> f64 {
    0.0001
}

fn default_lob_depth_pct() -> f64 {
    0.001
}

fn default_min_lob_depth_ratio() -> f64 {
    1.3
}

fn default_max_ask_for_reversal() -> f64 {
    0.25
}

fn default_max_pm_lag_secs() -> u64 {
    30
}

fn default_min_time_remaining_secs() -> u64 {
    60
}

fn default_max_time_remaining_secs() -> u64 {
    240
}

fn default_min_edge() -> f64 {
    0.05
}

fn default_take_profit_ask() -> f64 {
    0.65
}

fn default_stop_distance_pct() -> f64 {
    0.025
}

fn default_cooldown_secs() -> u64 {
    90
}

fn default_stake_usd() -> Decimal {
    Decimal::new(10, 0)
}

fn default_allowed_window_secs() -> Vec<u64> {
    vec![300]
}

fn default_max_positions() -> usize {
    1000
}

fn default_max_daily_trades() -> u32 {
    1000
}

impl Default for ReversalConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            max_distance_pct: default_max_distance_pct(),
            max_drift_flip_age_secs: default_max_drift_flip_age_secs(),
            min_post_flip_drift: default_min_post_flip_drift(),
            lob_depth_pct: default_lob_depth_pct(),
            min_lob_depth_ratio: default_min_lob_depth_ratio(),
            max_ask_for_reversal: default_max_ask_for_reversal(),
            max_pm_lag_secs: default_max_pm_lag_secs(),
            min_time_remaining_secs: default_min_time_remaining_secs(),
            max_time_remaining_secs: default_max_time_remaining_secs(),
            min_edge: default_min_edge(),
            take_profit_ask: default_take_profit_ask(),
            stop_distance_pct: default_stop_distance_pct(),
            cooldown_secs: default_cooldown_secs(),
            stake_usd: default_stake_usd(),
            allowed_window_secs: default_allowed_window_secs(),
            max_positions: default_max_positions(),
            max_daily_trades: default_max_daily_trades(),
        }
    }
}

impl From<DirectionalConfig> for ReversalConfig {
    fn from(config: DirectionalConfig) -> Self {
        Self {
            symbols: config.symbols,
            max_distance_pct: config.reversal_max_distance_pct,
            max_drift_flip_age_secs: config.reversal_max_drift_flip_age_secs,
            min_post_flip_drift: config.reversal_min_post_flip_drift,
            lob_depth_pct: config.reversal_lob_depth_pct,
            min_lob_depth_ratio: config.reversal_min_lob_depth_ratio,
            max_ask_for_reversal: config.reversal_max_ask_for_reversal,
            max_pm_lag_secs: config.reversal_max_pm_lag_secs,
            min_time_remaining_secs: config.min_time_remaining_secs,
            max_time_remaining_secs: config.max_time_remaining_secs,
            min_edge: config.min_edge,
            take_profit_ask: config.reversal_take_profit_ask,
            stop_distance_pct: config.reversal_stop_distance_pct,
            cooldown_secs: config.cooldown_secs,
            stake_usd: config.stake_usd,
            allowed_window_secs: config.allowed_window_secs,
            max_positions: config.max_positions,
            max_daily_trades: config.max_daily_trades,
        }
    }
}

#[derive(Clone, Copy)]
struct SpotState {
    price: Decimal,
}

#[derive(Clone, Copy, Default)]
struct LobDepthState {
    obi: f64,
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
    ts: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Default)]
struct DriftState {
    current_direction: f64,
    current_drift: f64,
    flip_ts: Option<DateTime<Utc>>,
    post_flip_drift: f64,
}

#[derive(Clone, Copy)]
struct EntryState {
    entry_price: Decimal,
    entry_time: DateTime<Utc>,
    remaining_qty: Decimal,
}

pub struct ReversalStrategy {
    config: ReversalConfig,
    spot: HashMap<Arc<str>, SpotState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    lob: HashMap<Arc<str>, LobDepthState>,
    price_history: HashMap<Arc<str>, VecDeque<(DateTime<Utc>, f64)>>,
    drift: HashMap<Arc<str>, DriftState>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    entry_state: HashMap<Arc<str>, EntryState>,
    retired_events: HashSet<Arc<str>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

impl ReversalStrategy {
    pub fn new(config: ReversalConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            events: HashMap::new(),
            quotes: HashMap::new(),
            lob: HashMap::new(),
            price_history: HashMap::new(),
            drift: HashMap::new(),
            last_entry: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            entry_state: HashMap::new(),
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

    fn update_drift(&mut self, symbol: &str, price: Decimal, ts: DateTime<Utc>) {
        let Some(price_f) = price.to_f64() else {
            return;
        };
        if price_f <= 0.0 {
            return;
        }

        let history = self.price_history.entry(Arc::from(symbol)).or_default();
        history.push_back((ts, price_f.ln()));
        while history.len() > 1 {
            let oldest = history.front().expect("history not empty").0;
            if (ts - oldest).num_seconds() > DRIFT_WINDOW_SECS {
                history.pop_front();
            } else {
                break;
            }
        }

        let state = self.drift.entry(Arc::from(symbol)).or_default();
        if history.len() < MIN_DRIFT_POINTS {
            return;
        }

        let mid = history.len() / 2;
        let contiguous = history.make_contiguous();
        let older = &contiguous[..mid];
        let newer = &contiguous[mid..];
        let older_drift = segment_drift(older);
        let current_drift = segment_drift(newer);
        let older_dir = direction_sign(older_drift);
        let current_dir = direction_sign(current_drift);

        if current_dir != 0.0 {
            if older_dir != 0.0 && older_dir != current_dir {
                state.flip_ts = Some(ts);
            }
            state.current_direction = current_dir;
            state.current_drift = current_drift;
            state.post_flip_drift = current_drift.abs();
        }
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
                intent_id: format!("reversal_settle_{}_up", event.event_id),
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
                intent_id: format!("reversal_settle_{}_down", event.event_id),
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

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    fn effective_probability(
        &self,
        distance_pct: f64,
        drift: DriftState,
        ratio: f64,
        flip_age_secs: f64,
    ) -> f64 {
        let distance_score =
            1.0 - (distance_pct.abs() / self.config.max_distance_pct.max(1e-9)).clamp(0.0, 1.0);
        let flip_score = 1.0
            - (flip_age_secs / self.config.max_drift_flip_age_secs.max(1) as f64).clamp(0.0, 1.0);
        let drift_score = ((drift.post_flip_drift / self.config.min_post_flip_drift.max(1e-9))
            .clamp(1.0, 2.0)
            - 1.0)
            .clamp(0.0, 1.0);
        let ratio_score = ((ratio / self.config.min_lob_depth_ratio.max(1e-9)).clamp(1.0, 2.0)
            - 1.0)
            .clamp(0.0, 1.0);

        (0.50
            + (0.10 * distance_score)
            + (0.08 * flip_score)
            + (0.10 * drift_score)
            + (0.07 * ratio_score))
            .clamp(0.51, 0.90)
    }

    fn build_signal(
        &self,
        event: &EventWindow,
        token_id: &str,
        direction: &str,
        p_hat: f64,
        edge: f64,
        entry_price: Decimal,
        now: DateTime<Utc>,
    ) -> SignalRecord {
        SignalRecord {
            strategy: self.name().to_string(),
            event_id: Some(event.event_id.to_string()),
            token_id: Some(token_id.to_string()),
            intent_id: Some(format!(
                "reversal_signal_{}_{}",
                token_id,
                now.timestamp_millis()
            )),
            symbol: event.symbol.to_string(),
            direction: direction.to_string(),
            p_hat,
            edge,
            entry_price,
            decision: "enter".to_string(),
            ts: now,
        }
    }

    fn evaluate_entry(
        &self,
        event: &EventWindow,
        symbol: &str,
        spot: f64,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        let price_to_beat = event.price_to_beat?.to_f64()?;
        if price_to_beat <= 0.0 || spot <= 0.0 {
            return None;
        }

        let distance_pct = (spot - price_to_beat) / price_to_beat;
        if distance_pct.abs() > self.config.max_distance_pct {
            return None;
        }

        let drift = *self.drift.get(symbol)?;
        let flip_ts = drift.flip_ts?;
        let flip_age_secs = (now - flip_ts).num_milliseconds() as f64 / 1000.0;
        if flip_age_secs < 0.0
            || flip_age_secs > self.config.max_drift_flip_age_secs as f64
            || drift.post_flip_drift < self.config.min_post_flip_drift
        {
            return None;
        }

        let betting_up = drift.current_direction > 0.0;
        let (token_id, direction) = if betting_up {
            (&event.up_token, "UP")
        } else {
            (&event.down_token, "DOWN")
        };

        if positions.net_qty(token_id) > Decimal::ZERO || active_order_exists(token_id, orders) {
            return None;
        }

        let lob = *self.lob.get(symbol)?;
        let lob_age_secs = lob
            .ts
            .map(|ts| (now - ts).num_seconds())
            .unwrap_or(i64::MAX);
        if lob_age_secs > self.config.max_pm_lag_secs as i64 {
            return None;
        }
        let ratio = if betting_up {
            lob.bid_depth_near / lob.ask_depth_near.max(0.001)
        } else {
            lob.ask_depth_near / lob.bid_depth_near.max(0.001)
        };
        if ratio < self.config.min_lob_depth_ratio {
            return None;
        }

        let quote = *self.quotes.get(token_id)?;
        let ask = quote.ask?.to_f64()?;
        if ask >= self.config.max_ask_for_reversal {
            return None;
        }
        if (now - quote.ts).num_seconds() > self.config.max_pm_lag_secs as i64 {
            return None;
        }

        let time_remaining = (event.end_time - now).num_seconds();
        if time_remaining < self.config.min_time_remaining_secs as i64
            || time_remaining > self.config.max_time_remaining_secs as i64
        {
            return None;
        }

        if let Some(last_entry) = self.last_entry.get(symbol) {
            if (now - *last_entry).num_seconds() < self.config.cooldown_secs as i64 {
                return None;
            }
        }

        let effective_p = self.effective_probability(distance_pct, drift, ratio, flip_age_secs);
        let edge = effective_p - ask - crypto_fee_cost(ask);
        if edge < self.config.min_edge {
            return None;
        }

        let entry_price = Decimal::try_from(ask).ok()?;
        let quantity = self.entry_quantity(entry_price);
        if quantity <= Decimal::ZERO {
            return None;
        }

        let intent_id = format!(
            "pm5d_reversal_{}_{}_{}",
            symbol.to_lowercase(),
            direction.to_lowercase(),
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
        let mut signal = self.build_signal(
            event,
            token_id,
            direction,
            effective_p,
            edge,
            entry_price,
            now,
        );
        signal.intent_id = Some(intent_id);

        Some(StrategyDecision::Enter {
            intent,
            signal: Some(signal),
        })
    }

    fn exit_decisions_for_symbol(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        spot: Option<f64>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        for event in self.events.get(symbol).into_iter().flatten() {
            if self.retired_events.contains(&event.event_id) {
                continue;
            }

            for (token_id, is_up) in [(&event.up_token, true), (&event.down_token, false)] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }

                let Some(exit_bid) = self.quotes.get(token_id).and_then(|q| q.bid) else {
                    continue;
                };

                if let Some(quote) = self.quotes.get(token_id) {
                    if let Some(ask) = quote.ask.and_then(|value| value.to_f64()) {
                        if ask >= self.config.take_profit_ask {
                            decisions.push(StrategyDecision::Exit(TradingIntent {
                                intent_id: format!(
                                    "reversal_take_profit_{}_{}",
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

                if let (Some(price_to_beat), Some(spot_price)) =
                    (event.price_to_beat.and_then(|value| value.to_f64()), spot)
                {
                    let dist = (spot_price - price_to_beat) / price_to_beat;
                    let wrong_direction = if is_up {
                        dist < -self.config.stop_distance_pct
                    } else {
                        dist > self.config.stop_distance_pct
                    };
                    if wrong_direction {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!(
                                "reversal_stop_loss_{}_{}",
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

    fn handle_spot(
        &mut self,
        symbol: &str,
        price: Decimal,
        ts: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        if !self
            .config
            .symbols
            .iter()
            .any(|configured| configured.as_str() == symbol)
        {
            return Vec::new();
        }

        self.reset_daily_counter(ts);
        self.update_drift(symbol, price, ts);
        self.spot.insert(Arc::from(symbol), SpotState { price });

        if let Some(events) = self.events.get_mut(symbol) {
            for event in events {
                if event.price_to_beat.is_none() {
                    event.price_to_beat = Some(price);
                }
            }
        }

        let spot = match price.to_f64() {
            Some(value) => value,
            None => return Vec::new(),
        };

        let exits = self.exit_decisions_for_symbol(symbol, ts, Some(spot), positions);
        if !exits.is_empty() {
            return exits;
        }

        if self.daily_trade_count >= self.config.max_daily_trades
            || positions.positions().count() >= self.config.max_positions
        {
            return Vec::new();
        }

        for event in self.candidate_events(symbol, ts) {
            if let Some(decision) = self.evaluate_entry(&event, symbol, spot, ts, positions, orders)
            {
                return vec![decision];
            }
        }

        Vec::new()
    }
}

impl StrategyLogic for ReversalStrategy {
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
                let spot = self
                    .spot
                    .get(&symbol)
                    .and_then(|state| state.price.to_f64());
                self.exit_decisions_for_symbol(&symbol, *ts, spot, positions)
            }
            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts,
            } => {
                let state = self.lob.entry(symbol.clone()).or_default();
                state.obi = *obi;
                state.spread_bps = *spread_bps;
                state.ts = Some(*ts);
                Vec::new()
            }
            MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ts,
            } => {
                self.lob.insert(
                    symbol.clone(),
                    LobDepthState {
                        obi: *obi,
                        spread_bps: *spread_bps,
                        bid_depth_near: *bid_depth_near,
                        ask_depth_near: *ask_depth_near,
                        ts: Some(*ts),
                    },
                );
                Vec::new()
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
                    })
                    .or_insert(EntryState {
                        entry_price: fill.price,
                        entry_time: fill.timestamp,
                        remaining_qty: fill.quantity,
                    });
            }
            TradeSide::Sell => {
                if let Some(entry) = self.entry_state.get_mut(fill.token_id.as_str()) {
                    entry.remaining_qty = (entry.remaining_qty - fill.quantity).max(Decimal::ZERO);
                    if entry.remaining_qty <= Decimal::ZERO {
                        self.entry_state.remove(fill.token_id.as_str());
                        if let Some(event_id) =
                            self.token_event.get(fill.token_id.as_str()).cloned()
                        {
                            self.retired_events.insert(event_id);
                        }
                    }
                }
            }
        }
    }

    fn name(&self) -> &str {
        "pm_5m_reversal"
    }
}

fn segment_drift(points: &[(DateTime<Utc>, f64)]) -> f64 {
    if points.len() < 2 {
        return 0.0;
    }
    let (start_ts, start_log_price) = points.first().expect("points not empty");
    let (end_ts, end_log_price) = points.last().expect("points not empty");
    let dt = (*end_ts - *start_ts).num_milliseconds() as f64 / 1000.0;
    if dt <= 0.0 {
        return 0.0;
    }
    (end_log_price - start_log_price) / dt
}

fn direction_sign(value: f64) -> f64 {
    if value > 1e-7 {
        1.0
    } else if value < -1e-7 {
        -1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use ploy_trading::{OrderLedger, PositionLedger};
    use rust_decimal_macros::dec;

    #[test]
    fn reversal_strategy_has_repo_fit_name() {
        let config = ReversalConfig::default();
        let strategy = ReversalStrategy::new(config);
        assert_eq!(strategy.name(), "pm_5m_reversal");
    }

    #[test]
    fn reversal_signal_triggers_entry_after_recent_drift_flip() {
        let config = ReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            max_distance_pct: 0.02,
            max_drift_flip_age_secs: 30,
            min_post_flip_drift: 0.00001,
            min_lob_depth_ratio: 1.5,
            max_ask_for_reversal: 0.40,
            max_pm_lag_secs: 30,
            min_time_remaining_secs: 30,
            max_time_remaining_secs: 240,
            min_edge: 0.01,
            cooldown_secs: 0,
            ..ReversalConfig::default()
        };
        let mut strategy = ReversalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        for (offset, price) in [(35, dec!(100.0)), (28, dec!(99.8)), (21, dec!(99.6))] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price,
                    ts: now - Duration::seconds(offset),
                },
                &positions,
                &orders,
            );
        }
        for (offset, price) in [(12, dec!(99.75)), (6, dec!(99.88))] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price,
                    ts: now - Duration::seconds(offset),
                },
                &positions,
                &orders,
            );
        }

        strategy.on_update(
            &MarketUpdate::L2Depth {
                symbol: "BTCUSDT".into(),
                obi: 0.2,
                spread_bps: 8,
                bid_depth_near: 12.0,
                ask_depth_near: 6.0,
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.21)),
                ask: Some(dec!(0.24)),
                ts: now - Duration::seconds(1),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(99.92),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|decision| matches!(decision, StrategyDecision::Enter { .. })),
            "expected reversal entry, got {decisions:?}"
        );
    }

    #[test]
    fn opposing_lob_depth_blocks_reversal_entry() {
        let config = ReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            max_distance_pct: 0.02,
            max_drift_flip_age_secs: 30,
            min_post_flip_drift: 0.00001,
            min_lob_depth_ratio: 1.5,
            max_ask_for_reversal: 0.40,
            max_pm_lag_secs: 30,
            min_time_remaining_secs: 30,
            max_time_remaining_secs: 240,
            min_edge: 0.01,
            cooldown_secs: 0,
            ..ReversalConfig::default()
        };
        let mut strategy = ReversalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt2".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up2".into(),
                down_token: "dn2".into(),
                end_time: now + Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        for (offset, price) in [(35, dec!(100.0)), (28, dec!(99.8)), (21, dec!(99.6))] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price,
                    ts: now - Duration::seconds(offset),
                },
                &positions,
                &orders,
            );
        }
        for (offset, price) in [(12, dec!(99.75)), (6, dec!(99.88))] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price,
                    ts: now - Duration::seconds(offset),
                },
                &positions,
                &orders,
            );
        }

        strategy.on_update(
            &MarketUpdate::L2Depth {
                symbol: "BTCUSDT".into(),
                obi: -0.2,
                spread_bps: 8,
                bid_depth_near: 4.0,
                ask_depth_near: 10.0,
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up2".into(),
                bid: Some(dec!(0.21)),
                ask: Some(dec!(0.24)),
                ts: now - Duration::seconds(1),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(99.92),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            !decisions
                .iter()
                .any(|decision| matches!(decision, StrategyDecision::Enter { .. })),
            "expected Gate 3 rejection, got {decisions:?}"
        );
    }

    #[test]
    fn quote_take_profit_emits_exit_for_open_reversal_position() {
        let config = ReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            take_profit_ask: 0.60,
            ..ReversalConfig::default()
        };
        let mut strategy = ReversalStrategy::new(config);
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt3".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up3".into(),
                down_token: "dn3".into(),
                end_time: now + Duration::seconds(180),
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        let buy_fill = FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up3".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.24),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        positions.apply_fill(&buy_fill);
        strategy.on_fill(&buy_fill);

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up3".into(),
                bid: Some(dec!(0.61)),
                ask: Some(dec!(0.63)),
                ts: now + Duration::seconds(15),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "expected a take-profit exit");
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "up3");
                assert_eq!(intent.quantity, dec!(5));
                assert_eq!(intent.limit_price, Some(dec!(0.61)));
            }
            other => panic!("expected exit, got {other:?}"),
        }
    }
}
