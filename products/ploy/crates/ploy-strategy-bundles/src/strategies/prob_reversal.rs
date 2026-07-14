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
use super::common::holding::BasicHoldingState;
use super::common::quote::QuoteState;
use super::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};

// ── Config ──────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProbReversalConfig {
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,
    // Entry: probability reversal thresholds
    #[serde(default = "default_prev_low")]
    pub prev_prob_low: f64,
    #[serde(default = "default_curr_high")]
    pub curr_prob_high: f64,
    #[serde(default = "default_prev_high")]
    pub prev_prob_high: f64,
    #[serde(default = "default_curr_low")]
    pub curr_prob_low: f64,
    // Exit
    #[serde(default = "default_tp_prob")]
    pub take_profit_prob: f64,
    #[serde(default = "default_sl_prob")]
    pub stop_loss_prob: f64,
    // Timing
    #[serde(default = "default_min_time")]
    pub min_time_remaining_secs: u64,
    #[serde(default = "default_max_time")]
    pub max_time_remaining_secs: u64,
    // Sizing
    #[serde(default = "default_stake_usd")]
    pub stake_usd: Decimal,
    #[serde(default = "default_max_positions")]
    pub max_positions: usize,
    #[serde(default = "default_max_daily_trades")]
    pub max_daily_trades: u32,
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,
}

fn default_symbols() -> Vec<String> {
    vec!["BTCUSDT".into(), "DOGEUSDT".into()]
}
fn default_prev_low() -> f64 {
    0.30
}
fn default_curr_high() -> f64 {
    0.60
}
fn default_prev_high() -> f64 {
    0.70
}
fn default_curr_low() -> f64 {
    0.40
}
fn default_tp_prob() -> f64 {
    0.85
}
fn default_sl_prob() -> f64 {
    0.50
}
fn default_min_time() -> u64 {
    1
}
fn default_max_time() -> u64 {
    5
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

impl Default for ProbReversalConfig {
    fn default() -> Self {
        Self {
            symbols: default_symbols(),
            prev_prob_low: default_prev_low(),
            curr_prob_high: default_curr_high(),
            prev_prob_high: default_prev_high(),
            curr_prob_low: default_curr_low(),
            take_profit_prob: default_tp_prob(),
            stop_loss_prob: default_sl_prob(),
            min_time_remaining_secs: default_min_time(),
            max_time_remaining_secs: default_max_time(),
            stake_usd: default_stake_usd(),
            max_positions: default_max_positions(),
            max_daily_trades: default_max_daily_trades(),
            allowed_window_secs: Vec::new(),
        }
    }
}

impl From<DirectionalConfig> for ProbReversalConfig {
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

// ── State ───────────────────────────────────────────────

// ── Strategy ────────────────────────────────────────────

pub struct ProbReversalStrategy {
    config: ProbReversalConfig,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    prev_up_prob: HashMap<Arc<str>, f64>,
    holdings: HashMap<Arc<str>, BasicHoldingState>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    retired_events: HashSet<Arc<str>>,
    daily_trade_count: u32,
    daily_reset_date: Option<NaiveDate>,
}

impl ProbReversalStrategy {
    pub fn new(config: ProbReversalConfig) -> Self {
        Self {
            config,
            events: HashMap::new(),
            quotes: HashMap::new(),
            prev_up_prob: HashMap::new(),
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
    fn find_event_for_token(&self, token_id: &Arc<str>) -> Option<&EventWindow> {
        let event_id = self.token_event.get(token_id)?;
        self.events
            .values()
            .flatten()
            .find(|event| event.event_id == *event_id && event.contains_token(token_id))
    }

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    fn resolve_up_won(&self, _event: &EventWindow, settlement: Option<bool>) -> Option<bool> {
        if settlement.is_some() {
            return settlement;
        }
        // Without spot data we cannot infer settlement for this strategy.
        None
    }

    fn build_settlement_exits(
        &self,
        event: &EventWindow,
        up_won: bool,
        created_at: DateTime<Utc>,
        positions: &PositionLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();
        for token_id in [&event.up_token, &event.down_token] {
            let wins = event
                .token_wins(token_id, up_won)
                .expect("event token list is built from event sides");
            let qty = positions.net_qty(token_id);
            if qty > Decimal::ZERO {
                exits.push(StrategyDecision::Exit(TradingIntent {
                    intent_id: format!("prob_reversal_settle_{}_{}", event.event_id, token_id),
                    deployment_id: String::new(),
                    market_id: event.event_id.to_string(),
                    token_id: token_id.to_string(),
                    side: TradeSide::Sell,
                    quantity: qty,
                    limit_price: Some(if wins {
                        Decimal::new(1, 0)
                    } else {
                        Decimal::ZERO
                    }),
                    purpose: IntentPurpose::Exit,
                    created_at,
                }));
            }
        }
        exits
    }
    fn handle_quote(
        &mut self,
        token_id: &Arc<str>,
        bid: Option<Decimal>,
        ask: Option<Decimal>,
        ts: &DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        // 1. Update quote state.
        self.quotes
            .insert(token_id.clone(), QuoteState { bid, ask, ts: *ts });
        self.reset_daily_counter(*ts);

        // 2. Resolve event for this token.
        let event = match self.find_event_for_token(token_id) {
            Some(e) => e.clone(),
            None => return Vec::new(),
        };
        if self.retired_events.contains(&event.event_id) {
            return Vec::new();
        }

        let remaining = (event.end_time - *ts).num_seconds();
        let is_up_token = *token_id == event.up_token;

        // Current probability: use ask price as implied probability.
        let current_prob = match ask.and_then(|a| a.to_f64()) {
            Some(p) if p > 0.0 && p < 1.0 => p,
            _ => {
                // Still store prev for next tick.
                if is_up_token {
                    if let Some(a) = ask.and_then(|a| a.to_f64()) {
                        self.prev_up_prob.insert(token_id.clone(), a);
                    }
                }
                return Vec::new();
            }
        };

        // For UP token: up_prob = current_prob.
        // For DOWN token: up_prob = 1 - current_prob.
        let up_prob = if is_up_token {
            current_prob
        } else {
            1.0 - current_prob
        };

        // 3. Check exit conditions for any holding on this token.
        let mut decisions = Vec::new();
        if self.holdings.contains_key(token_id) {
            let Some(exit_bid) = bid else {
                return decisions;
            };
            let held_prob = current_prob; // probability of the token we hold
            if held_prob >= self.config.take_profit_prob {
                let qty = positions.net_qty(token_id);
                if qty > Decimal::ZERO {
                    info!(
                        token_id = %token_id,
                        prob = held_prob,
                        "prob_reversal take-profit"
                    );
                    decisions.push(StrategyDecision::Exit(TradingIntent {
                        intent_id: format!(
                            "prob_reversal_tp_{}_{}",
                            token_id,
                            ts.timestamp_millis()
                        ),
                        deployment_id: String::new(),
                        market_id: event.event_id.to_string(),
                        token_id: token_id.to_string(),
                        side: TradeSide::Sell,
                        quantity: qty,
                        limit_price: Some(exit_bid),
                        purpose: IntentPurpose::Exit,
                        created_at: *ts,
                    }));
                }
            } else if held_prob <= self.config.stop_loss_prob {
                let qty = positions.net_qty(token_id);
                if qty > Decimal::ZERO {
                    info!(
                        token_id = %token_id,
                        prob = held_prob,
                        "prob_reversal stop-loss"
                    );
                    decisions.push(StrategyDecision::Exit(TradingIntent {
                        intent_id: format!(
                            "prob_reversal_sl_{}_{}",
                            token_id,
                            ts.timestamp_millis()
                        ),
                        deployment_id: String::new(),
                        market_id: event.event_id.to_string(),
                        token_id: token_id.to_string(),
                        side: TradeSide::Sell,
                        quantity: qty,
                        limit_price: Some(exit_bid),
                        purpose: IntentPurpose::Exit,
                        created_at: *ts,
                    }));
                }
            }
        }

        if !decisions.is_empty() {
            // Update prev before returning.
            if is_up_token {
                self.prev_up_prob.insert(token_id.clone(), up_prob);
            }
            return decisions;
        }
        // 4. Entry logic — only on UP token quotes (we track up_prob).
        if is_up_token {
            let prev = self.prev_up_prob.get(token_id).copied();
            // Update prev for next tick.
            self.prev_up_prob.insert(token_id.clone(), up_prob);

            if let Some(prev_up) = prev {
                // Time window check: 1s to 5s remaining.
                if remaining >= self.config.min_time_remaining_secs as i64
                    && remaining <= self.config.max_time_remaining_secs as i64
                    && self.daily_trade_count < self.config.max_daily_trades
                    && positions.positions().count() < self.config.max_positions
                {
                    // Buy UP: prev < 30% AND current > 60%
                    if prev_up < self.config.prev_prob_low
                        && up_prob > self.config.curr_prob_high
                        && positions.net_qty(&event.up_token) <= Decimal::ZERO
                        && !active_order_exists(&event.up_token, orders)
                    {
                        let entry_price = ask.unwrap(); // safe: we checked above
                        let quantity = self.entry_quantity(entry_price);
                        if quantity > Decimal::ZERO {
                            let direction_str = "up";
                            let fee = crypto_fee_cost(current_prob);
                            let edge = up_prob - current_prob - fee;
                            info!(
                                event_id = %event.event_id,
                                prev_up = prev_up,
                                curr_up = up_prob,
                                remaining,
                                edge,
                                "prob_reversal BUY UP"
                            );
                            return vec![StrategyDecision::Enter {
                                intent: TradingIntent {
                                    intent_id: format!(
                                        "prob_reversal_{}_{}_{}",
                                        event.event_id,
                                        direction_str,
                                        ts.timestamp_millis()
                                    ),
                                    deployment_id: String::new(),
                                    market_id: event.event_id.to_string(),
                                    token_id: event.up_token.to_string(),
                                    side: TradeSide::Buy,
                                    quantity,
                                    limit_price: Some(entry_price),
                                    purpose: IntentPurpose::Entry,
                                    created_at: *ts,
                                },
                                signal: Some(SignalRecord {
                                    strategy: self.name().to_string(),
                                    event_id: Some(event.event_id.to_string()),
                                    token_id: Some(event.up_token.to_string()),
                                    intent_id: None,
                                    symbol: event.symbol.to_string(),
                                    direction: "UP".to_string(),
                                    p_hat: up_prob,
                                    edge,
                                    entry_price,
                                    decision: "enter".to_string(),
                                    ts: *ts,
                                }),
                            }];
                        }
                    }

                    // Buy DOWN: prev > 70% AND current < 40%
                    if prev_up > self.config.prev_prob_high
                        && up_prob < self.config.curr_prob_low
                        && positions.net_qty(&event.down_token) <= Decimal::ZERO
                        && !active_order_exists(&event.down_token, orders)
                    {
                        let down_ask = self.quotes.get(&event.down_token).and_then(|q| q.ask);
                        if let Some(entry_price) = down_ask {
                            let quantity = self.entry_quantity(entry_price);
                            if quantity > Decimal::ZERO {
                                let direction_str = "down";
                                let down_prob = 1.0 - up_prob;
                                let ep_f = entry_price.to_f64().unwrap_or(0.0);
                                let fee = crypto_fee_cost(ep_f);
                                let edge = down_prob - ep_f - fee;
                                info!(
                                    event_id = %event.event_id,
                                    prev_up = prev_up,
                                    curr_up = up_prob,
                                    remaining,
                                    edge,
                                    "prob_reversal BUY DOWN"
                                );
                                return vec![StrategyDecision::Enter {
                                    intent: TradingIntent {
                                        intent_id: format!(
                                            "prob_reversal_{}_{}_{}",
                                            event.event_id,
                                            direction_str,
                                            ts.timestamp_millis()
                                        ),
                                        deployment_id: String::new(),
                                        market_id: event.event_id.to_string(),
                                        token_id: event.down_token.to_string(),
                                        side: TradeSide::Buy,
                                        quantity,
                                        limit_price: Some(entry_price),
                                        purpose: IntentPurpose::Entry,
                                        created_at: *ts,
                                    },
                                    signal: Some(SignalRecord {
                                        strategy: self.name().to_string(),
                                        event_id: Some(event.event_id.to_string()),
                                        token_id: Some(event.down_token.to_string()),
                                        intent_id: None,
                                        symbol: event.symbol.to_string(),
                                        direction: "DOWN".to_string(),
                                        p_hat: down_prob,
                                        edge,
                                        entry_price,
                                        decision: "enter".to_string(),
                                        ts: *ts,
                                    }),
                                }];
                            }
                        }
                    }
                }
            }
        } else {
            // DOWN token quote — just update prev_up_prob indirectly.
            // We derive up_prob = 1 - down_ask for tracking.
            self.prev_up_prob
                .entry(event.up_token.clone())
                .or_insert(up_prob);
        }

        Vec::new()
    }
}
impl StrategyLogic for ProbReversalStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        match update {
            MarketUpdate::Quote {
                token_id,
                bid,
                ask,
                ts,
                ..
            } => self.handle_quote(token_id, *bid, *ask, ts, positions, orders),

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
                debug!(event_id = %event_id, "prob_reversal registered event");
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

                for eid in &resolved {
                    self.retired_events.insert(eid.clone());
                }
                if !resolved.is_empty() {
                    for events in self.events.values_mut() {
                        events.retain(|e| !resolved.contains(&e.event_id));
                    }
                }
                decisions
            }

            _ => Vec::new(),
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        let token_id: Arc<str> = Arc::from(fill.token_id.as_str());
        match fill.side {
            TradeSide::Buy => {
                self.daily_trade_count += 1;
                let direction = if self
                    .find_event_for_token(&token_id)
                    .map(|e| e.up_token == token_id)
                    .unwrap_or(false)
                {
                    "UP"
                } else {
                    "DOWN"
                };
                self.holdings.insert(
                    token_id,
                    BasicHoldingState {
                        token_id: Arc::from(fill.token_id.as_str()),
                        direction: direction.to_string(),
                        entry_time: fill.timestamp,
                    },
                );
            }
            TradeSide::Sell => {
                self.holdings.remove(&token_id);
                if let Some(event_id) = self.token_event.get(&token_id).cloned() {
                    self.retired_events.insert(event_id);
                }
            }
        }
    }

    fn name(&self) -> &str {
        "prob_reversal"
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
        let s = ProbReversalStrategy::new(ProbReversalConfig::default());
        assert_eq!(s.name(), "prob_reversal");
    }

    #[test]
    fn up_reversal_triggers_entry() {
        let config = ProbReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            ..ProbReversalConfig::default()
        };
        let mut strategy = ProbReversalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        // Register event ending in 3 seconds.
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + Duration::seconds(3),
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // First tick: UP ask = 0.25 (prev_up = 0.25, below 0.30).
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.23)),
                ask: Some(dec!(0.25)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now - Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        // Second tick: UP ask = 0.65 (dramatic reversal, above 0.60).
        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up1".into(),
                bid: Some(dec!(0.63)),
                ask: Some(dec!(0.65)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected UP reversal entry, got {decisions:?}"
        );
        if let StrategyDecision::Enter { intent, signal } = &decisions[0] {
            assert_eq!(intent.token_id, "up1");
            assert_eq!(intent.side, TradeSide::Buy);
            assert_eq!(signal.as_ref().unwrap().direction, "UP");
        }
    }

    #[test]
    fn down_reversal_triggers_entry() {
        let config = ProbReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            ..ProbReversalConfig::default()
        };
        let mut strategy = ProbReversalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt2".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up2".into(),
                down_token: "dn2".into(),
                end_time: now + Duration::seconds(3),
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Seed DOWN token quote so entry_price is available.
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "dn2".into(),
                bid: Some(dec!(0.18)),
                ask: Some(dec!(0.20)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now - Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        // First UP tick: ask = 0.75 (prev_up = 0.75, above 0.70).
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up2".into(),
                bid: Some(dec!(0.73)),
                ask: Some(dec!(0.75)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now - Duration::seconds(1),
            },
            &positions,
            &orders,
        );

        // Second UP tick: ask = 0.35 (dramatic drop, below 0.40).
        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up2".into(),
                bid: Some(dec!(0.33)),
                ask: Some(dec!(0.35)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "expected DOWN reversal entry, got {decisions:?}"
        );
        if let StrategyDecision::Enter { intent, signal } = &decisions[0] {
            assert_eq!(intent.token_id, "dn2");
            assert_eq!(signal.as_ref().unwrap().direction, "DOWN");
        }
    }

    #[test]
    fn take_profit_exit() {
        let config = ProbReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            take_profit_prob: 0.85,
            ..ProbReversalConfig::default()
        };
        let mut strategy = ProbReversalStrategy::new(config);
        let mut positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt3".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up3".into(),
                down_token: "dn3".into(),
                end_time: now + Duration::seconds(60),
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        // Simulate a fill.
        let fill = FillRecord {
            fill_id: "f1".into(),
            order_id: "o1".into(),
            token_id: "up3".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            price: dec!(0.65),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        positions.apply_fill(&fill);
        strategy.on_fill(&fill);

        // Quote at 0.90 — above take_profit_prob.
        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up3".into(),
                bid: Some(dec!(0.88)),
                ask: Some(dec!(0.90)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now + Duration::seconds(2),
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1);
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "up3");
                assert_eq!(intent.quantity, dec!(10));
            }
            other => panic!("expected exit, got {other:?}"),
        }
    }

    #[test]
    fn no_entry_outside_time_window() {
        let config = ProbReversalConfig {
            symbols: vec!["BTCUSDT".into()],
            min_time_remaining_secs: 1,
            max_time_remaining_secs: 5,
            ..ProbReversalConfig::default()
        };
        let mut strategy = ProbReversalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt4".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up4".into(),
                down_token: "dn4".into(),
                end_time: now + Duration::seconds(30), // 30s remaining — outside 1-5s window
                window_secs: 300,
                price_to_beat: Some(dec!(100.0)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "up4".into(),
                bid: Some(dec!(0.23)),
                ask: Some(dec!(0.25)),
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
            &MarketUpdate::Quote {
                token_id: "up4".into(),
                bid: Some(dec!(0.63)),
                ask: Some(dec!(0.65)),
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            !decisions
                .iter()
                .any(|d| matches!(d, StrategyDecision::Enter { .. })),
            "should not enter outside time window"
        );
    }
}
