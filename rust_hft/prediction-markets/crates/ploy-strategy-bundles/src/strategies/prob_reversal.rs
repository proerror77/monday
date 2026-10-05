//! Research-only adapter for the canonical probability-reversal implementation.
use super::{
    common::{fees::crypto_fee_cost, guards::active_order_exists},
    directional::DirectionalConfig,
};
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};
use chrono::{DateTime, Utc};
use hft_core::{
    LocalReceiveTimestamp, MarketDataTimestamps, OrderId, Price, Quantity, Side, Symbol, VenueId,
};
use hft_research_manifest::prediction_probability::{
    BinaryEpisodeV1, ProbabilityReversalSpecV1, PROBABILITY_REVERSAL_SCHEMA,
};
use portfolio_core::prediction::{
    FillRecord, IntentPurpose, OrderLedger, PositionLedger, TradeSide, TradingIntent,
};
use ports::{
    AccountView, BookLevel, ExecutionEvent, MarketEvent, MarketSnapshot, ProviderBookIdentity,
    Strategy,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use strategy_probability_reversal::{ProbabilityReversalStrategy, ProbabilityStrategyConfig};
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
            max_positions: config.max_positions,
            max_daily_trades: config.max_daily_trades,
            allowed_window_secs: config.allowed_window_secs,
            ..Self::default()
        }
    }
}

struct EpisodeAdapter {
    binding: BinaryEpisodeV1,
    strategy: ProbabilityReversalStrategy,
    sequence: HashMap<String, u64>,
}
pub struct ProbReversalStrategy {
    config: ProbReversalConfig,
    episodes: HashMap<String, EpisodeAdapter>,
    token_episode: HashMap<String, String>,
    last_account: AccountView,
    filled_entries: HashSet<String>,
    day: Option<i64>,
    daily_entries: u32,
}
impl ProbReversalStrategy {
    pub fn new(config: ProbReversalConfig) -> Self {
        Self {
            config,
            episodes: HashMap::new(),
            token_episode: HashMap::new(),
            last_account: AccountView::default(),
            filled_entries: HashSet::new(),
            day: None,
            daily_entries: 0,
        }
    }
    fn account(positions: &PositionLedger) -> AccountView {
        let mut account = AccountView::default();
        for p in positions.positions() {
            let symbol = Symbol::new(&p.token_id);
            account.positions.insert(
                symbol.clone(),
                ports::Position {
                    symbol,
                    quantity: Quantity(p.net_qty),
                    avg_price: Price(p.avg_entry_price),
                    unrealized_pnl: Decimal::ZERO,
                    realized_pnl: p.realized_pnl,
                },
            );
        }
        account
    }
    fn discover(
        &mut self,
        event_id: &str,
        symbol: &str,
        up: &str,
        down: &str,
        end: DateTime<Utc>,
        window_secs: u64,
    ) {
        if self.episodes.len() >= 1024
            || self.episodes.contains_key(event_id)
            || self.token_episode.contains_key(up)
            || self.token_episode.contains_key(down)
            || !self.config.symbols.iter().any(|s| s == symbol)
            || (!self.config.allowed_window_secs.is_empty()
                && !self.config.allowed_window_secs.contains(&window_secs))
        {
            return;
        }
        let Ok(end_us) = u64::try_from(end.timestamp_micros()) else {
            return;
        };
        let Some(start_us) = window_secs
            .checked_mul(1_000_000)
            .and_then(|window| end_us.checked_sub(window))
        else {
            return;
        };
        // Historical discovery has no provider condition receipt. This label is
        // local replay identity; it cannot produce a runtime admission artifact.
        let binding = BinaryEpisodeV1 {
            episode_id: event_id.into(),
            condition_id: event_id.into(),
            underlying: symbol.into(),
            venue: "POLYMARKET".into(),
            up_token: up.into(),
            down_token: down.into(),
            start_us,
            end_us,
        };
        let spec = ProbabilityReversalSpecV1 {
            schema: PROBABILITY_REVERSAL_SCHEMA.into(),
            episodes: vec![binding.clone()],
            prev_prob_low: self.config.prev_prob_low,
            curr_prob_high: self.config.curr_prob_high,
            prev_prob_high: self.config.prev_prob_high,
            curr_prob_low: self.config.curr_prob_low,
            take_profit_prob: self.config.take_profit_prob,
            stop_loss_prob: self.config.stop_loss_prob,
            min_time_remaining_secs: self.config.min_time_remaining_secs,
            max_time_remaining_secs: self.config.max_time_remaining_secs,
            stake_usd: self.config.stake_usd,
            max_positions: self.config.max_positions,
            max_daily_trades: self.config.max_daily_trades,
            quote_max_age_us: 5_000_000,
        };
        let Ok(strategy) = ProbabilityReversalStrategy::new(ProbabilityStrategyConfig {
            name: "prob_reversal".into(),
            spec,
            max_order_notional: Decimal::MAX,
            max_order_quantity: Decimal::MAX,
        }) else {
            return;
        };
        self.token_episode.insert(up.into(), event_id.into());
        self.token_episode.insert(down.into(), event_id.into());
        self.episodes.insert(
            event_id.into(),
            EpisodeAdapter {
                binding,
                strategy,
                sequence: HashMap::new(),
            },
        );
    }
}
impl StrategyLogic for ProbReversalStrategy {
    fn on_update(
        &mut self,
        update: &MarketUpdate,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        self.last_account = Self::account(positions);
        for order in orders.orders() {
            let Some(episode_id) = self.token_episode.get(&order.token_id) else {
                continue;
            };
            let Some(adapter) = self.episodes.get_mut(episode_id) else {
                continue;
            };
            let id = OrderId(order.order_id.clone());
            let event = match order.state {
                portfolio_core::prediction::OrderState::Filled => {
                    Some(ExecutionEvent::OrderCompleted {
                        order_id: id,
                        final_price: Price(order.limit_price.unwrap_or(Decimal::ZERO)),
                        total_filled: Quantity(order.filled_qty),
                        timestamp: 0,
                    })
                }
                portfolio_core::prediction::OrderState::Canceled => {
                    Some(ExecutionEvent::OrderCanceled {
                        order_id: id,
                        timestamp: 0,
                    })
                }
                portfolio_core::prediction::OrderState::Rejected => {
                    Some(ExecutionEvent::OrderReject {
                        order_id: id,
                        reason: order.rejection_reason.clone().unwrap_or_default(),
                        timestamp: 0,
                    })
                }
                _ => None,
            };
            if let Some(event) = event {
                adapter
                    .strategy
                    .on_execution_event(&event, &self.last_account);
            }
        }
        match update {
            MarketUpdate::EventDiscovered {
                event_id,
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs,
                ..
            } => {
                self.discover(
                    event_id,
                    symbol,
                    up_token,
                    down_token,
                    *end_time,
                    *window_secs,
                );
                Vec::new()
            }
            MarketUpdate::EventExpired { event_id, .. } => {
                if let Some(e) = self.episodes.remove(event_id.as_ref()) {
                    self.token_episode.remove(&e.binding.up_token);
                    self.token_episode.remove(&e.binding.down_token);
                }
                Vec::new()
            }
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ts,
                ..
            } => {
                let Some(event_id) = self.token_episode.get(token_id.as_ref()).cloned() else {
                    return Vec::new();
                };
                let Some(adapter) = self.episodes.get_mut(&event_id) else {
                    return Vec::new();
                };
                let Ok(observed) = u64::try_from(ts.timestamp_micros()) else {
                    return Vec::new();
                };
                let day = ts.timestamp() / 86_400;
                if self.day != Some(day) {
                    self.day = Some(day);
                    self.daily_entries = 0;
                    self.filled_entries.clear();
                }
                let sequence = adapter.sequence.entry(token_id.to_string()).or_default();
                *sequence = sequence.saturating_add(1);
                let event = MarketEvent::Snapshot(MarketSnapshot {
                    symbol: Symbol::new(token_id.as_ref()),
                    timestamp: observed,
                    bids: vec![BookLevel {
                        price: Price(*bid),
                        quantity: Quantity(Decimal::ONE),
                    }],
                    asks: vec![BookLevel {
                        price: Price(*ask),
                        quantity: Quantity(Decimal::ONE),
                    }],
                    sequence: *sequence,
                    source_venue: Some(VenueId::POLYMARKET),
                    timestamps: MarketDataTimestamps::local_only(LocalReceiveTimestamp::new(
                        observed,
                    )),
                    provider_identity: Some(ProviderBookIdentity {
                        market: event_id.clone(),
                        book_hash: None,
                    }),
                });
                adapter
                    .strategy
                    .on_market_event(&event, &self.last_account)
                    .into_iter()
                    .filter(|i| {
                        !active_order_exists(&Arc::from(i.symbol.as_str()), orders)
                            && (i.side == Side::Sell
                                || self.daily_entries < self.config.max_daily_trades)
                    })
                    .map(|intent| {
                        let token = intent.symbol.as_str().to_owned();
                        let buying = intent.side == Side::Buy;
                        let trading = TradingIntent {
                            intent_id: format!("prob_reversal_{}_{}_{}", event_id, token, observed),
                            deployment_id: String::new(),
                            market_id: event_id.clone(),
                            token_id: token.clone(),
                            side: if buying {
                                TradeSide::Buy
                            } else {
                                TradeSide::Sell
                            },
                            quantity: intent.quantity.0,
                            limit_price: intent.price.map(|p| p.0),
                            purpose: if buying {
                                IntentPurpose::Entry
                            } else {
                                IntentPurpose::Exit
                            },
                            created_at: *ts,
                        };
                        if !buying {
                            return StrategyDecision::Exit(trading);
                        }
                        let price = intent.price.unwrap().0;
                        let up = token == adapter.binding.up_token;
                        let probability = if up {
                            ask.to_f64().unwrap_or(0.0)
                        } else {
                            1.0 - ask.to_f64().unwrap_or(0.0)
                        };
                        StrategyDecision::Enter {
                            intent: trading,
                            signal: Some(SignalRecord {
                                strategy: "prob_reversal".into(),
                                event_id: Some(event_id.clone()),
                                token_id: Some(token),
                                intent_id: None,
                                symbol: adapter.binding.underlying.clone(),
                                direction: if up { "UP".into() } else { "DOWN".into() },
                                p_hat: probability,
                                edge: probability
                                    - price.to_f64().unwrap_or(0.0)
                                    - crypto_fee_cost(price.to_f64().unwrap_or(0.0)),
                                entry_price: price,
                                decision: "enter".into(),
                                ts: *ts,
                            }),
                        }
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }
    fn on_fill(&mut self, fill: &FillRecord) {
        if fill.side == TradeSide::Buy && self.filled_entries.insert(fill.order_id.clone()) {
            self.daily_entries = self.daily_entries.saturating_add(1);
        }
        let Some(event_id) = self.token_episode.get(&fill.token_id).cloned() else {
            return;
        };
        let Some(adapter) = self.episodes.get_mut(&event_id) else {
            return;
        };
        let symbol = Symbol::new(&fill.token_id);
        let side = if fill.side == TradeSide::Buy {
            Side::Buy
        } else {
            Side::Sell
        };
        let timestamp = u64::try_from(fill.timestamp.timestamp_micros()).unwrap_or(0);
        let order_id = OrderId(fill.order_id.clone());
        adapter.strategy.on_execution_event(
            &ExecutionEvent::OrderNew {
                order_id: order_id.clone(),
                client_order_id: None,
                account_id: None,
                symbol,
                side,
                quantity: Quantity(fill.quantity),
                requested_price: Some(Price(fill.price)),
                arrival_price: None,
                timestamp,
                venue: Some(VenueId::POLYMARKET),
                strategy_id: "prob_reversal".into(),
            },
            &self.last_account,
        );
        adapter.strategy.on_execution_event(
            &ExecutionEvent::Fill {
                order_id,
                price: Price(fill.price),
                quantity: Quantity(fill.quantity),
                timestamp,
                fill_id: fill.fill_id.clone(),
            },
            &self.last_account,
        );
    }
    fn name(&self) -> &str {
        "prob_reversal"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use portfolio_core::prediction::{OrderLedger, PositionLedger};
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
                up_token: "101".into(),
                down_token: "201".into(),
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
                token_id: "101".into(),
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
                token_id: "101".into(),
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
            assert_eq!(intent.token_id, "101");
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
                up_token: "102".into(),
                down_token: "202".into(),
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
                token_id: "202".into(),
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
                token_id: "102".into(),
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
                token_id: "102".into(),
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
            assert_eq!(intent.token_id, "202");
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
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt3".into(),
                symbol: "BTCUSDT".into(),
                up_token: "103".into(),
                down_token: "203".into(),
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
            token_id: "103".into(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            price: dec!(0.65),
            fee: Decimal::ZERO,
            timestamp: now,
        };
        let positions = crate::canonical_test_support::position_projection(
            crate::canonical_test_support::entry_intent("103", dec!(10)),
            "o1",
            "venue-o1",
            [fill.clone()],
        );
        assert_eq!(positions.net_qty("103"), dec!(10));
        strategy.on_fill(&fill);

        // Quote at 0.90 — above take_profit_prob.
        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: "103".into(),
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
                assert_eq!(intent.token_id, "103");
                assert_eq!(intent.quantity, dec!(10));
            }
            other => panic!("expected exit, got {other:?}"),
        }
        assert!(
            strategy
                .on_update(
                    &MarketUpdate::EventExpired {
                        event_id: "evt3".into(),
                        end_time: now + Duration::seconds(3),
                        resolved_up_won: Some(true),
                    },
                    &positions,
                    &orders
                )
                .is_empty(),
            "settlement is evidence, never a 0/1 sell order"
        );
        assert_eq!(positions.net_qty("103"), dec!(10));
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
                up_token: "104".into(),
                down_token: "204".into(),
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
                token_id: "104".into(),
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
                token_id: "104".into(),
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
