//! Directional binary-option strategy (pm_5m_directional).
//!
//! Estimates P(S_T >= S_0) via log-normal model on CEX spot prices,
//! then buys UP or DOWN tokens on Polymarket when edge exceeds threshold.
//!
//! Implements [`StrategyLogic`] so it plugs into [`StrategyRuntime`]
//! for backtest, dry-run, and live modes identically.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
use super::three_layer_profile::ThreeLayerProfile;
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
pub struct DirectionalConfig {
    // Symbols
    #[serde(default = "default_symbols")]
    pub symbols: Vec<String>,
    #[serde(default)]
    pub symbol_profiles: HashMap<String, DirectionalSymbolProfile>,

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

    // Mean-reversion / V4 prototype parameters
    #[serde(default = "default_min_deviation_pct")]
    pub min_deviation_pct: f64,
    #[serde(default = "default_min_reversal_consistency")]
    pub min_reversal_consistency: f64,
    #[serde(default = "default_min_trend_consistency")]
    pub min_trend_consistency: f64,
    #[serde(default = "default_min_trend_persistence_secs")]
    pub min_trend_persistence_secs: u64,
    #[serde(default = "default_take_profit_price_delta")]
    pub take_profit_price_delta: f64,
    #[serde(default = "default_stop_loss_price_delta")]
    pub stop_loss_price_delta: f64,
    #[serde(default = "default_max_hold_secs")]
    pub max_hold_secs: u64,
    #[serde(default = "default_reversal_bonus_cap")]
    pub reversal_bonus_cap: f64,
    #[serde(default = "default_use_multiscale_volatility")]
    pub use_multiscale_volatility: bool,
    #[serde(default = "default_use_price_structure_adjustment")]
    pub use_price_structure_adjustment: bool,

    // Reversal / V5 prototype parameters
    #[serde(default = "default_reversal_max_distance_pct")]
    pub reversal_max_distance_pct: f64,
    #[serde(default = "default_reversal_max_drift_flip_age_secs")]
    pub reversal_max_drift_flip_age_secs: u64,
    #[serde(default = "default_reversal_min_post_flip_drift")]
    pub reversal_min_post_flip_drift: f64,
    #[serde(default = "default_reversal_lob_depth_pct")]
    pub reversal_lob_depth_pct: f64,
    #[serde(default = "default_reversal_min_lob_depth_ratio")]
    pub reversal_min_lob_depth_ratio: f64,
    #[serde(default = "default_reversal_max_ask_for_reversal")]
    pub reversal_max_ask_for_reversal: f64,
    #[serde(default = "default_reversal_max_pm_lag_secs")]
    pub reversal_max_pm_lag_secs: u64,
    #[serde(default = "default_reversal_take_profit_ask")]
    pub reversal_take_profit_ask: f64,
    #[serde(default = "default_reversal_stop_distance_pct")]
    pub reversal_stop_distance_pct: f64,

    // ── Three-Layer strategy parameters ──────────────────────────────
    /// Runtime profile for profile-specific snapshot optimizer parity.
    #[serde(default, alias = "strategy_profile")]
    pub three_layer_strategy_profile: ThreeLayerProfile,

    /// Direction gate: minimum effective probability to consider a trade.
    #[serde(default = "default_tl_min_direction_prob")]
    pub three_layer_min_direction_prob: f64,

    /// Direction gate: minimum |distance_over_sigma| to consider a trade.
    #[serde(default = "default_tl_min_distance_over_sigma")]
    pub three_layer_min_distance_over_sigma: f64,

    /// Confirmation gate: minimum absolute confirmation score to pass.
    #[serde(default = "default_tl_min_confirmation_score")]
    pub three_layer_min_confirmation_score: f64,

    /// Confirmation gate: require the profile-specific CEX/PM confirmation score to pass.
    #[serde(default = "default_tl_require_confirmation")]
    pub three_layer_require_confirmation: bool,

    /// Confirmation gate: in late/expiry regimes, require drift_30s to agree with direction.
    #[serde(default = "default_tl_min_drift_confirmation")]
    pub three_layer_min_drift_confirmation: f64,

    /// Worth-it gate: minimum edge after fees.
    #[serde(default = "default_tl_min_edge")]
    pub three_layer_min_edge: f64,

    /// Worth-it gate: minimum reward/risk ratio.
    #[serde(default = "default_tl_min_reward_risk")]
    pub three_layer_min_reward_risk: f64,

    /// Research mode: fade the model-favored side instead of following it.
    #[serde(default = "default_tl_alpha_contrarian")]
    pub three_layer_alpha_contrarian: bool,

    /// Research mode: reward CEX/LOB opposition instead of confirmation.
    #[serde(default = "default_tl_cex_contrarian")]
    pub three_layer_cex_contrarian: bool,

    /// Probability calibration: shrink effective direction probability toward 50/50 before EV.
    #[serde(default = "default_tl_probability_shrink")]
    pub three_layer_probability_shrink: f64,

    /// Probability calibration: subtract a conservative haircut after shrink before EV.
    #[serde(default = "default_tl_probability_haircut")]
    pub three_layer_probability_haircut: f64,

    /// Take-profit: exit when token ask reaches this level.
    #[serde(default = "default_tl_take_profit_ask")]
    pub three_layer_take_profit_ask: f64,

    /// Stop-loss: exit when spot moves against direction by this pct.
    #[serde(default = "default_tl_stop_distance_pct")]
    pub three_layer_stop_distance_pct: f64,

    /// Maximum PM quote staleness (seconds) before rejecting entry.
    #[serde(default = "default_tl_max_pm_lag_secs")]
    pub three_layer_max_pm_lag_secs: u64,

    /// Scoring model: minimum total score to enter (0.0-1.0).
    #[serde(default = "default_tl_min_entry_score")]
    pub three_layer_min_entry_score: f64,

    /// Optional AutoFactor formula score promoted from the research handoff.
    #[serde(default)]
    pub three_layer_autofactor_runtime_score: Option<String>,

    /// Event ML baseline artifact used when `three_layer_autofactor_runtime_score`
    /// starts with `event_ml_model:`.
    #[serde(default)]
    pub three_layer_event_ml_model_path: Option<PathBuf>,

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

    // Time-frame filter
    /// Only trade markets whose total window duration (seconds) is in this list.
    /// e.g. [300, 900] for 5-minute and 15-minute markets only.
    /// Empty or absent = no filter (trade all allowed windows).
    #[serde(default)]
    pub allowed_window_secs: Vec<u64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DirectionalSymbolProfile {
    #[serde(default)]
    pub min_probability: Option<f64>,
    #[serde(default)]
    pub max_entry_price: Option<f64>,
    #[serde(default)]
    pub min_edge: Option<f64>,
    #[serde(default)]
    pub min_time_remaining_secs: Option<u64>,
    #[serde(default)]
    pub max_time_remaining_secs: Option<u64>,
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
fn default_min_deviation_pct() -> f64 {
    0.005
}
fn default_min_reversal_consistency() -> f64 {
    0.55
}
fn default_min_trend_consistency() -> f64 {
    0.50
}
fn default_min_trend_persistence_secs() -> u64 {
    0
}
fn default_take_profit_price_delta() -> f64 {
    0.10
}
fn default_stop_loss_price_delta() -> f64 {
    0.05
}
fn default_max_hold_secs() -> u64 {
    120
}
fn default_reversal_bonus_cap() -> f64 {
    0.20
}
fn default_use_multiscale_volatility() -> bool {
    true
}
fn default_use_price_structure_adjustment() -> bool {
    true
}
fn default_reversal_max_distance_pct() -> f64 {
    0.015
}
fn default_reversal_max_drift_flip_age_secs() -> u64 {
    20
}
fn default_reversal_min_post_flip_drift() -> f64 {
    0.0001
}
fn default_reversal_lob_depth_pct() -> f64 {
    0.001
}
fn default_reversal_min_lob_depth_ratio() -> f64 {
    1.3
}
fn default_reversal_max_ask_for_reversal() -> f64 {
    0.25
}
fn default_reversal_max_pm_lag_secs() -> u64 {
    30
}
fn default_reversal_take_profit_ask() -> f64 {
    0.65
}
fn default_reversal_stop_distance_pct() -> f64 {
    0.025
}
fn default_tl_min_direction_prob() -> f64 {
    0.56
}
fn default_tl_min_distance_over_sigma() -> f64 {
    0.3
}
fn default_tl_min_confirmation_score() -> f64 {
    0.10
}
fn default_tl_require_confirmation() -> bool {
    false
}
fn default_tl_min_drift_confirmation() -> f64 {
    0.0002
}
fn default_tl_min_edge() -> f64 {
    0.03
}
fn default_tl_min_reward_risk() -> f64 {
    1.2
}
fn default_tl_alpha_contrarian() -> bool {
    false
}
fn default_tl_cex_contrarian() -> bool {
    false
}
fn default_tl_probability_shrink() -> f64 {
    1.0
}
fn default_tl_probability_haircut() -> f64 {
    0.0
}
fn default_tl_take_profit_ask() -> f64 {
    0.70
}
fn default_tl_stop_distance_pct() -> f64 {
    0.020
}
fn default_tl_max_pm_lag_secs() -> u64 {
    15
}
fn default_tl_min_entry_score() -> f64 {
    0.30
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

/// Ring buffer of recent tick returns for realized volatility calculation.
struct ReturnBuffer {
    /// (log_return, dt_secs) pairs, newest at the back.
    entries: Vec<(f64, f64)>,
    /// Cumulative age in seconds from the newest entry.
    total_secs: f64,
    /// Rolling high/low within the buffer window.
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

    /// Push a new tick and evict entries older than `window_secs`.
    fn push(&mut self, log_return: f64, dt_secs: f64, price: f64, window_secs: f64) {
        self.entries.push((log_return, dt_secs));
        self.total_secs += dt_secs;

        // Update high/low
        if price > self.high {
            self.high = price;
        }
        if price < self.low {
            self.low = price;
        }

        // Evict old entries
        while self.total_secs > window_secs && self.entries.len() > 2 {
            let (_, old_dt) = self.entries.remove(0);
            self.total_secs -= old_dt;
        }
    }

    /// Realized variance per second: sum(r²) / total_time.
    fn realized_var_per_sec(&self) -> f64 {
        if self.total_secs <= 0.0 || self.entries.is_empty() {
            return 0.0;
        }
        let sum_r2: f64 = self.entries.iter().map(|(r, _)| r * r).sum();
        sum_r2 / self.total_secs
    }

    /// Parkinson range-based variance per second: ln(H/L)² / (4·ln2·T).
    fn parkinson_var_per_sec(&self) -> f64 {
        if self.high <= 0.0 || self.low <= 0.0 || self.high <= self.low || self.total_secs <= 0.0 {
            return 0.0;
        }
        let log_hl = (self.high / self.low).ln();
        log_hl * log_hl / (4.0 * std::f64::consts::LN_2 * self.total_secs)
    }

    /// Directional consistency: fraction of ticks moving in the dominant direction.
    /// Returns (consistency 0.0-1.0, dominant_direction +1/-1).
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

    /// Price drift speed: cumulative log return / elapsed time.
    fn drift_speed(&self) -> f64 {
        if self.total_secs <= 0.0 {
            return 0.0;
        }
        let cum_return: f64 = self.entries.iter().map(|(r, _)| r).sum();
        cum_return / self.total_secs
    }

    /// Price acceleration: compare drift speed of recent half vs older half.
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
        let old_speed = old_sum / old_dt;
        let new_speed = new_sum / new_dt;
        new_speed - old_speed
    }

    fn aligned_consistency(&self, signal_dir: f64) -> f64 {
        let (consistency, dominant_dir) = self.directional_consistency();
        if dominant_dir * signal_dir > 0.0 {
            consistency
        } else {
            1.0 - consistency
        }
    }

    fn trailing_persistence_secs(&self, signal_dir: f64) -> f64 {
        let mut secs = 0.0;
        for (ret, dt_secs) in self.entries.iter().rev() {
            if ret * signal_dir > 0.0 {
                secs += *dt_secs;
            } else {
                break;
            }
        }
        secs
    }

    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[derive(Default)]
struct MicrostructureState {
    signed_trade_imbalance: f64,
    last_aggtrade_ts: Option<DateTime<Utc>>,
    last_obi: Option<f64>,
    obi_delta: f64,
    last_l2_ts: Option<DateTime<Utc>>,
    spread_bps: Option<u32>,
}

impl MicrostructureState {
    fn apply_aggtrade(&mut self, quantity: Decimal, is_buyer_maker: bool, ts: DateTime<Utc>) {
        let qty = quantity.to_f64().unwrap_or(0.0);
        if qty <= 0.0 {
            return;
        }

        let seconds = self
            .last_aggtrade_ts
            .map(|last| (ts - last).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(0.0)
            .max(0.0);
        let decay = if seconds > 0.0 {
            (-seconds / 30.0).exp()
        } else {
            1.0
        };

        // Binance aggTrade semantics: buyer_maker=true means seller aggression.
        let signed_qty = if is_buyer_maker { -qty } else { qty };
        self.signed_trade_imbalance = self.signed_trade_imbalance * decay + signed_qty;
        self.last_aggtrade_ts = Some(ts);
    }

    fn apply_l2(&mut self, obi: f64, spread_bps: u32, ts: DateTime<Utc>) {
        self.obi_delta = self.last_obi.map(|last| obi - last).unwrap_or(0.0);
        self.last_obi = Some(obi);
        self.spread_bps = Some(spread_bps);
        self.last_l2_ts = Some(ts);
    }

    fn directional_score(&self, signal_dir: f64) -> f64 {
        let trade_score = (self.signed_trade_imbalance / 50.0).clamp(-1.0, 1.0) * 0.35;
        let obi_score = self.last_obi.unwrap_or(0.0).clamp(-1.0, 1.0) * 0.20;
        let obi_delta_score = self.obi_delta.clamp(-1.0, 1.0) * 0.35;
        let spread_score = match self.spread_bps.unwrap_or(0) {
            0..=6 => 0.10,
            7..=15 => 0.0,
            _ => -0.10,
        };

        signal_dir * (trade_score + obi_score + obi_delta_score) + spread_score
    }
}

/// Rolling window for return buffer (seconds).
const RETURN_BUFFER_WINDOW_SECS: f64 = 300.0;

#[derive(Default)]
struct VolatilityState {
    ewma_var_per_sec: f64,
}

// ── Strategy Implementation ──────────────────────────────

pub struct DirectionalStrategy {
    config: DirectionalConfig,
    // Market state
    spot: HashMap<Arc<str>, SpotState>,
    volatility: HashMap<Arc<str>, VolatilityState>,
    return_buffers: HashMap<Arc<str>, ReturnBuffer>,
    microstructure: HashMap<Arc<str>, MicrostructureState>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    quotes: HashMap<Arc<str>, QuoteState>,
    // Gating state
    cooldowns: HashMap<Arc<str>, DateTime<Utc>>,
    daily_trades: u32,
    last_trade_date: Option<chrono::NaiveDate>,
    /// Realized PnL for the current trading day (circuit breaker).
    daily_realized_pnl: Decimal,
    // Token → symbol mapping
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    /// Entry price cache: token_id → entry price (for PnL tracking on settlement).
    entry_prices: HashMap<Arc<str>, Decimal>,
    /// Most recent feed timestamp seen across all updates.
    /// Used instead of Utc::now() so replay runs are deterministic.
    feed_time: Option<DateTime<Utc>>,
    /// When set, all new entries are blocked until this time (balance exhausted pause).
    balance_exhausted_until: Option<DateTime<Utc>>,
}

impl DirectionalStrategy {
    pub fn new(config: DirectionalConfig) -> Self {
        Self {
            config,
            spot: HashMap::new(),
            volatility: HashMap::new(),
            return_buffers: HashMap::new(),
            microstructure: HashMap::new(),
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

    fn symbol_profile(&self, symbol: &str) -> Option<&DirectionalSymbolProfile> {
        self.config.symbol_profiles.get(symbol)
    }

    fn effective_min_probability(&self, symbol: &str) -> f64 {
        self.symbol_profile(symbol)
            .and_then(|profile| profile.min_probability)
            .unwrap_or(self.config.min_probability)
    }

    fn effective_max_entry_price(&self, symbol: &str) -> f64 {
        self.symbol_profile(symbol)
            .and_then(|profile| profile.max_entry_price)
            .unwrap_or(self.config.max_entry_price)
    }

    fn effective_min_edge(&self, symbol: &str) -> f64 {
        self.symbol_profile(symbol)
            .and_then(|profile| profile.min_edge)
            .unwrap_or(self.config.min_edge)
    }

    fn effective_min_time_remaining_secs(&self, symbol: &str) -> u64 {
        self.symbol_profile(symbol)
            .and_then(|profile| profile.min_time_remaining_secs)
            .unwrap_or(self.config.min_time_remaining_secs)
    }

    fn effective_max_time_remaining_secs(&self, symbol: &str) -> u64 {
        self.symbol_profile(symbol)
            .and_then(|profile| profile.max_time_remaining_secs)
            .unwrap_or(self.config.max_time_remaining_secs)
    }

    /// Pick the nearest event within the valid time window.
    fn pick_event(&self, symbol: &str, now: DateTime<Utc>) -> Option<EventWindow> {
        self.events
            .get(symbol)?
            .iter()
            .filter(|e| {
                let rem = (e.end_time - now).num_seconds();
                rem >= self.effective_min_time_remaining_secs(symbol) as i64
                    && rem <= self.effective_max_time_remaining_secs(symbol) as i64
            })
            .min_by_key(|e| e.end_time)
            .cloned()
    }

    fn candidate_events(&self, symbol: &str, now: DateTime<Utc>) -> Vec<EventWindow> {
        let min_time = self.effective_min_time_remaining_secs(symbol) as i64;
        let max_time = self.effective_max_time_remaining_secs(symbol) as i64;
        self.events
            .get(symbol)
            .map(|events| {
                let mut candidates: Vec<EventWindow> = events
                    .iter()
                    .filter(|event| {
                        let rem = (event.end_time - now).num_seconds();
                        rem >= min_time && rem <= max_time
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

    fn daily_trade_cap_reached(&self) -> bool {
        self.config.max_daily_trades > 0 && self.daily_trades >= self.config.max_daily_trades
    }

    fn daily_trade_cap_allows_entry(&self) -> bool {
        !self.daily_trade_cap_reached()
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

        // 1. EWMA (existing)
        let floor = self.floor_var_per_sec();
        let state = self.volatility.entry(Arc::from(symbol)).or_default();
        state.ewma_var_per_sec = if state.ewma_var_per_sec <= 0.0 {
            inst_var_per_sec.max(floor)
        } else {
            (EWMA_LAMBDA * state.ewma_var_per_sec) + ((1.0 - EWMA_LAMBDA) * inst_var_per_sec)
        };

        // 2. Return buffer for RV, Parkinson, and price structure features
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

        let best_var = if self.config.use_multiscale_volatility {
            // Use the maximum of three estimators for a conservative sigma.
            // - EWMA: smooth, adapts slowly, good for regime detection
            // - Realized Var: direct measurement of recent tick-level volatility
            // - Parkinson: range-based, 5x more efficient, captures intra-window extremes
            let (rv, parkinson) = self
                .return_buffers
                .get(symbol)
                .filter(|buf| buf.len() >= 5)
                .map(|buf| (buf.realized_var_per_sec(), buf.parkinson_var_per_sec()))
                .unwrap_or((0.0, 0.0));
            ewma.max(rv).max(parkinson).max(floor)
        } else {
            ewma.max(floor)
        };
        (best_var * secs).sqrt()
    }

    fn shares_for_entry_price(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        // Keep a stable venue-facing share quantity while preserving the
        // fixed-dollar stake semantics configured at strategy level.
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

            // Track P&L for daily loss limit: payout - cost (cost = qty * entry_price ≈ qty * ask)
            // Approximation: we don't have the exact entry price here, so use settle_price * qty
            // as the realized value and subtract stake_usd as the cost basis.
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

    /// Core signal evaluation — the 7-gate pipeline.
    ///
    /// Gate 0: Price validity
    /// Gate 1: Quote availability + direction
    /// Gate 2: Price filter (bounds + no-trade zone)
    /// Gate 3: Probability threshold (log-normal base)
    /// Gate 4: Price structure adjustment (drift, consistency, acceleration)
    /// Gate 5: Adjusted probability threshold
    /// Gate 6: Edge after fees
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
        let min_probability = self.effective_min_probability(symbol);
        let max_entry_price = self.effective_max_entry_price(symbol);
        let min_edge = self.effective_min_edge(symbol);

        // Probability estimation (log-normal model)
        let p_hat = estimate_probability(s0, st, sigma_horizon);

        // Gate 1: Direction + quote availability
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

        // Gate 2: Price filter (bounds + no-trade zone)
        if entry_f < self.config.min_entry_price
            || entry_f > max_entry_price
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

        // Gate 3: Base probability threshold
        let base_p = if direction == Direction::Up {
            p_hat
        } else {
            1.0 - p_hat
        };
        if base_p < min_probability {
            debug!(
                symbol,
                event_id = %event.event_id,
                effective_p = base_p,
                threshold = min_probability,
                "Gate 3: Probability too low"
            );
            return None;
        }

        // Gate 4: Price structure adjustment (Bayesian likelihood update)
        // Use drift speed, acceleration, and directional consistency from the
        // return buffer to adjust the base probability up or down.
        let effective_p = if self.config.use_price_structure_adjustment {
            if let Some(buf) = self.return_buffers.get(symbol) {
                let requires_structure_history = self.config.min_trend_consistency > 0.5
                    || self.config.min_trend_persistence_secs > 0;
                let signal_dir = if direction == Direction::Up {
                    1.0
                } else {
                    -1.0
                };

                if requires_structure_history {
                    if buf.len() < 3 {
                        debug!(
                            symbol,
                            event_id = %event.event_id,
                            history_len = buf.len(),
                            "Gate 4: insufficient structure history"
                        );
                        return None;
                    }

                    let aligned_consistency = buf.aligned_consistency(signal_dir);
                    let persistence_secs = buf.trailing_persistence_secs(signal_dir);

                    if aligned_consistency < self.config.min_trend_consistency {
                        debug!(
                            symbol,
                            event_id = %event.event_id,
                            aligned_consistency = format!("{:.2}", aligned_consistency),
                            threshold = format!("{:.2}", self.config.min_trend_consistency),
                            "Gate 4: aligned consistency too weak"
                        );
                        return None;
                    }
                    if persistence_secs + f64::EPSILON
                        < self.config.min_trend_persistence_secs as f64
                    {
                        debug!(
                            symbol,
                            event_id = %event.event_id,
                            persistence_secs = format!("{:.2}", persistence_secs),
                            threshold = self.config.min_trend_persistence_secs,
                            "Gate 4: trend persistence too short"
                        );
                        return None;
                    }
                }

                if buf.len() >= 10 {
                    let drift = buf.drift_speed();
                    let accel = buf.drift_acceleration();
                    let (consistency, dom_dir) = buf.directional_consistency();
                    let aligned_consistency = buf.aligned_consistency(signal_dir);
                    let persistence_secs = buf.trailing_persistence_secs(signal_dir);

                    // Drift alignment: is the price moving in our signal direction?
                    // Positive = drift confirms signal, negative = drift opposes signal.
                    let drift_alignment = drift * signal_dir;

                    // Acceleration alignment: is the drift accelerating in our direction?
                    let accel_alignment = accel * signal_dir;

                    // Consistency bonus: if >70% of ticks move in our direction, boost confidence.
                    // If dominant direction opposes our signal, penalize.
                    let consistency_factor = if dom_dir * signal_dir > 0.0 {
                        // Ticks confirm our direction
                        1.0 + (consistency - 0.5).max(0.0) * 0.3 // up to +15% boost
                    } else {
                        // Ticks oppose our direction
                        1.0 - (consistency - 0.5).max(0.0) * 0.3 // up to -15% penalty
                    };

                    // Drift factor: strong drift in our direction boosts, against penalizes.
                    // Normalize by sigma to make it scale-invariant.
                    let drift_factor = if sigma_horizon > 0.0 {
                        let normalized_drift =
                            drift_alignment * secs_remaining.sqrt() / sigma_horizon;
                        1.0 + normalized_drift.clamp(-0.3, 0.3) * 0.2
                    } else {
                        1.0
                    };

                    // Acceleration factor: accelerating in our direction is bullish.
                    let accel_factor = if sigma_horizon > 0.0 {
                        let normalized_accel = accel_alignment * secs_remaining / sigma_horizon;
                        1.0 + normalized_accel.clamp(-0.5, 0.5) * 0.1
                    } else {
                        1.0
                    };

                    // Apply all factors to base probability (multiplicative on odds ratio)
                    let microstructure_factor = self
                        .microstructure
                        .get(symbol)
                        .map(|state| 1.0 + state.directional_score(signal_dir).clamp(-0.4, 0.4))
                        .unwrap_or(1.0);
                    let combined_factor =
                        consistency_factor * drift_factor * accel_factor * microstructure_factor;
                    let odds = base_p / (1.0 - base_p).max(1e-9);
                    let adjusted_odds = odds * combined_factor;
                    let adjusted_p = adjusted_odds / (1.0 + adjusted_odds);

                    debug!(
                        symbol,
                        event_id = %event.event_id,
                        base_p = format!("{:.3}", base_p),
                        adjusted_p = format!("{:.3}", adjusted_p),
                        aligned_consistency = format!("{:.2}", aligned_consistency),
                        persistence_secs = format!("{:.2}", persistence_secs),
                        microstructure_factor = format!("{:.3}", microstructure_factor),
                        drift = format!("{:.6}", drift),
                        accel = format!("{:.6}", accel),
                        consistency = format!("{:.2}", consistency),
                        "Gate 4: Price structure adjustment"
                    );

                    adjusted_p
                } else {
                    base_p
                }
            } else {
                if self.config.min_trend_consistency > 0.5
                    || self.config.min_trend_persistence_secs > 0
                {
                    debug!(
                        symbol,
                        event_id = %event.event_id,
                        "Gate 4: missing structure history"
                    );
                    return None;
                }
                base_p
            }
        } else {
            base_p
        };

        // Gate 5: Adjusted probability threshold
        if effective_p < min_probability {
            debug!(
                symbol,
                event_id = %event.event_id,
                effective_p,
                base_p,
                "Gate 5: Adjusted probability below threshold"
            );
            return None;
        }

        // Gate 6: Edge after fees
        let cost = crypto_fee_cost(entry_f);
        let edge = effective_p - entry_f - cost;
        if edge < min_edge {
            debug!(
                symbol,
                event_id = %event.event_id,
                edge,
                threshold = min_edge,
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
                "pm5d_{}_{}_{}",
                event.symbol,
                match direction {
                    Direction::Up => "UP",
                    Direction::Down => "DN",
                },
                now.timestamp_millis(),
            ),
            deployment_id: String::new(), // filled by runtime
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

        // Already holding this symbol?
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
                    min_time = self.effective_min_time_remaining_secs(symbol),
                    max_time = self.effective_max_time_remaining_secs(symbol),
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
                    "✓ Entry signal PASSED all gates",
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

impl StrategyLogic for DirectionalStrategy {
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

                // Advance feed clock so EventDiscovered pruning is deterministic in replay.
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
                if self.daily_trade_cap_reached() {
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

                // Also try entry: a fresh quote may unlock a signal that was
                // previously blocked by missing ask price (Gate 1).
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
                        if self.daily_trade_cap_allows_entry()
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

            MarketUpdate::AggTrade {
                symbol,
                quantity,
                is_buyer_maker,
                ts,
                ..
            } => {
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
                    return vec![];
                }
                self.microstructure
                    .entry(symbol.clone())
                    .or_default()
                    .apply_aggtrade(*quantity, *is_buyer_maker, *ts);
                if self.feed_time.map_or(true, |ft| *ts > ft) {
                    self.feed_time = Some(*ts);
                }
                vec![]
            }

            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts,
            } => {
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
                    return vec![];
                }
                self.microstructure
                    .entry(symbol.clone())
                    .or_default()
                    .apply_l2(*obi, *spread_bps, *ts);
                if self.feed_time.map_or(true, |ft| *ts > ft) {
                    self.feed_time = Some(*ts);
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
                    debug!(symbol = %symbol, event_id = %event_id, window_secs = *window_secs, "Ignoring disallowed event window");
                    return vec![];
                }
                // Use feed_time (last seen spot/quote timestamp) as "now" so that
                // replay runs are deterministic and don't drop historical windows
                // that arrived before the first spot tick.
                // Fall back to the event's own end_time only when no feed time is
                // known yet — this keeps the window alive until spot data arrives.
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
                // Dedup
                if events.iter().any(|e| e.event_id == *event_id) {
                    return vec![];
                }
                // Use price_to_beat if available, otherwise fallback to current spot
                let price_to_beat =
                    price_to_beat.or_else(|| self.spot.get(symbol).map(|s| s.price));

                // Track token → symbol mapping
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

                // Quotes for this event may have arrived before EventDiscovered
                // (feed ordering: quotes sorted by received_at, events by start_time).
                // If we already have a cached quote for either token, try entry now.
                let has_cached_quote =
                    self.quotes.contains_key(up_token) || self.quotes.contains_key(down_token);
                if has_cached_quote
                    && self
                        .config
                        .symbols
                        .iter()
                        .any(|s| s.as_str() == symbol.as_ref())
                    && self.daily_trade_cap_allows_entry()
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

        // Track entry prices and realized PnL for circuit breaker.
        match fill.side {
            ploy_trading::TradeSide::Buy => {
                self.entry_prices
                    .insert(Arc::from(fill.token_id.clone()), fill.price);
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
            // Balance exhausted: pause all entries for 5 minutes to avoid hammering
            // the venue with guaranteed-to-fail orders while waiting for funds to settle.
            let pause_until = now + chrono::Duration::minutes(5);
            warn!(
                until = %pause_until,
                "Balance exhausted — pausing all entries for 5 minutes"
            );
            self.balance_exhausted_until = Some(pause_until);
            return;
        }

        // For all other rejections (FAK no match, precision errors, no market):
        // arm the per-symbol cooldown so the same event isn't retried on every tick.
        if let Some(symbol) = self.token_symbol.get(intent.token_id.as_str()).cloned() {
            self.cooldowns.insert(symbol.clone(), now);
            debug!(symbol = &*symbol, reason, "Rejection cooldown armed");
        }
    }

    fn name(&self) -> &str {
        "pm_5m_directional"
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
            min_probability: 0.55,
            min_z_score: 0.35,
            min_entry_price: 0.15,
            max_entry_price: 0.85,
            no_trade_zone_min: 0.45,
            no_trade_zone_max: 0.55,
            min_edge: 0.02,
            min_deviation_pct: 0.005,
            min_reversal_consistency: 0.55,
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
            three_layer_strategy_profile: ThreeLayerProfile::Mixed,
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
    fn symbol_profile_override_replaces_global_thresholds() {
        let mut config = default_config();
        config.symbols = vec!["ETHUSDT".into()];
        config.min_probability = 0.52;
        config.max_entry_price = 0.85;
        config.min_edge = 0.02;
        config.min_time_remaining_secs = 60;
        config.max_time_remaining_secs = 300;
        config.symbol_profiles.insert(
            "ETHUSDT".into(),
            DirectionalSymbolProfile {
                min_probability: Some(0.70),
                max_entry_price: Some(0.55),
                min_edge: Some(0.05),
                min_time_remaining_secs: Some(90),
                max_time_remaining_secs: Some(180),
                ..Default::default()
            },
        );
        let strat = DirectionalStrategy::new(config);

        assert!((strat.effective_min_probability("ETHUSDT") - 0.70).abs() < f64::EPSILON);
        assert!((strat.effective_max_entry_price("ETHUSDT") - 0.55).abs() < f64::EPSILON);
        assert!((strat.effective_min_edge("ETHUSDT") - 0.05).abs() < f64::EPSILON);
        assert_eq!(strat.effective_min_time_remaining_secs("ETHUSDT"), 90);
        assert_eq!(strat.effective_max_time_remaining_secs("ETHUSDT"), 180);
    }

    #[test]
    fn normal_cdf_at_zero_is_half() {
        let p = normal_cdf(0.0);
        assert!((p - 0.5).abs() < 1e-6, "got {}", p);
    }

    #[test]
    fn probability_above_s0_is_high() {
        // BTC up 1%, 0.5% horizon sigma → high probability of staying above
        let p = estimate_probability(100_000.0, 101_000.0, 0.005);
        assert!(p > 0.7, "p={}", p);
    }

    #[test]
    fn probability_below_s0_is_low() {
        let p = estimate_probability(100_000.0, 99_000.0, 0.005);
        assert!(p < 0.3, "p={}", p);
    }

    #[test]
    fn event_windowing_picks_nearest() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        // Register two events: one ending in 120s, one in 240s
        let e1_end = now + chrono::Duration::seconds(120);
        let e2_end = now + chrono::Duration::seconds(240);

        strat.events.insert(
            "BTCUSDT".into(),
            vec![
                EventWindow {
                    event_id: "e1".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up1".into(),
                    down_token: "dn1".into(),
                    end_time: e1_end,
                    window_secs: 300,
                    price_to_beat: Some(dec!(100000)),
                },
                EventWindow {
                    event_id: "e2".into(),
                    symbol: "BTCUSDT".into(),
                    up_token: "up2".into(),
                    down_token: "dn2".into(),
                    end_time: e2_end,
                    window_secs: 300,
                    price_to_beat: Some(dec!(100000)),
                },
            ],
        );

        let picked = strat.pick_event("BTCUSDT", now).unwrap();
        assert_eq!(picked.event_id.as_ref(), "e1"); // nearer one
    }

    #[test]
    fn cooldown_blocks_entry() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        strat.cooldowns.insert("BTCUSDT".into(), now);
        // cooldown_secs = 0 means no cooldown — always false
        assert!(!strat.in_cooldown("BTCUSDT", now + chrono::Duration::seconds(1)));
        assert!(!strat.in_cooldown("BTCUSDT", now));
    }

    #[test]
    fn zero_max_daily_trades_disables_cap() {
        let mut config = default_config();
        config.max_daily_trades = 0;
        let mut strat = DirectionalStrategy::new(config);

        strat.daily_trades = 10_000;

        assert!(!strat.daily_trade_cap_reached());
        assert!(strat.daily_trade_cap_allows_entry());
    }

    #[test]
    fn positive_max_daily_trades_caps_at_limit() {
        let mut config = default_config();
        config.max_daily_trades = 2;
        let mut strat = DirectionalStrategy::new(config);

        strat.daily_trades = 1;
        assert!(!strat.daily_trade_cap_reached());
        assert!(strat.daily_trade_cap_allows_entry());

        strat.daily_trades = 2;
        assert!(strat.daily_trade_cap_reached());
        assert!(!strat.daily_trade_cap_allows_entry());
    }

    #[test]
    fn on_update_processes_spot_price() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        let update = MarketUpdate::SpotPrice {
            symbol: "BTCUSDT".into(),
            price: dec!(100000),
            ts: Utc::now(),
        };

        // No event registered → no decisions, but price is stored
        let decisions = strat.on_update(&update, &positions, &orders);
        assert!(decisions.is_empty());
        assert!(strat.spot.contains_key("BTCUSDT"));
    }

    #[test]
    fn full_signal_generates_entry() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
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

        // 2. Set initial spot (becomes price_to_beat via event)
        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(100000),
                ts: now,
            },
        );
        // Manually set price_to_beat since event was registered before spot
        strat.events.get_mut("BTCUSDT").unwrap()[0].price_to_beat = Some(dec!(100000));

        // 3. Provide quotes — UP ask cheap (0.30) meaning market underprices UP
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

        // 4. BTC moved up 1.5% → should trigger UP entry
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
                assert_eq!(intent.quantity, dec!(83.333333));
                let signal = signal.as_ref().expect("entry signal should be recorded");
                assert_eq!(signal.event_id.as_deref(), Some("evt1"));
                assert_eq!(signal.token_id.as_deref(), Some("up1"));
                assert_eq!(signal.intent_id.as_deref(), Some(intent.intent_id.as_str()));
                assert_eq!(signal.direction, "UP");
                assert_eq!(signal.decision, "enter");
            }
            other => panic!("Expected Enter, got {:?}", other),
        }
    }

    fn strengthened_v3_config() -> DirectionalConfig {
        let mut config = default_config();
        config.min_probability = 0.52;
        config.min_trend_consistency = 0.62;
        config.min_trend_persistence_secs = 20;
        config
    }

    fn seed_event_for_structure_tests(
        strat: &mut DirectionalStrategy,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) {
        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt-structure".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-structure".into(),
                down_token: "dn-structure".into(),
                end_time: now + chrono::Duration::seconds(120),
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            positions,
            &OrderLedger::default(),
        );

        let bootstrap = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            positions,
            &OrderLedger::default(),
        );
        assert!(bootstrap.is_empty(), "bootstrap spot should not trade");
    }

    fn seed_quotes_for_structure_tests(
        strat: &mut DirectionalStrategy,
        positions: &PositionLedger,
        now: DateTime<Utc>,
    ) {
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "up-structure".into(),
                bid: Some(dec!(0.29)),
                ask: Some(dec!(0.30)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            positions,
            &OrderLedger::default(),
        );
        strat.on_update(
            &MarketUpdate::Quote {
                token_id: "dn-structure".into(),
                bid: Some(dec!(0.69)),
                ask: Some(dec!(0.70)),
                ts: now,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
            },
            positions,
            &OrderLedger::default(),
        );
    }

    fn replay_spot_path(
        strat: &mut DirectionalStrategy,
        positions: &PositionLedger,
        now: DateTime<Utc>,
        prices: &[i64],
    ) -> Vec<StrategyDecision> {
        let mut last = Vec::new();
        for (idx, price) in prices.iter().enumerate() {
            if idx == prices.len() - 1 {
                seed_quotes_for_structure_tests(
                    strat,
                    positions,
                    now + chrono::Duration::seconds(((idx + 1) * 5) as i64 - 1),
                );
            }
            last = strat.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: "BTCUSDT".into(),
                    price: Decimal::from(*price),
                    ts: now + chrono::Duration::seconds(((idx + 1) * 5) as i64),
                },
                positions,
                &OrderLedger::default(),
            );
        }
        last
    }

    #[test]
    fn weak_trend_consistency_blocks_v3_entry() {
        let config = strengthened_v3_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let now = Utc::now();

        seed_event_for_structure_tests(&mut strat, &positions, now);

        let decisions = replay_spot_path(
            &mut strat,
            &positions,
            now,
            &[
                99950, 99900, 99850, 99800, 100150, 100500, 100900, 101300, 101700, 102000,
            ],
        );

        assert!(
            decisions.is_empty(),
            "weak aligned consistency should block the V3 entry"
        );
    }

    #[test]
    fn short_trend_persistence_blocks_v3_entry() {
        let config = strengthened_v3_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let now = Utc::now();

        seed_event_for_structure_tests(&mut strat, &positions, now);

        let decisions = replay_spot_path(
            &mut strat,
            &positions,
            now,
            &[
                100200, 100420, 100650, 100900, 101120, 101350, 101280, 101520, 101760, 102000,
            ],
        );

        assert!(
            decisions.is_empty(),
            "short trailing persistence should block the V3 entry"
        );
    }

    #[test]
    fn strong_persistent_trend_still_enters_with_v3_strengthening() {
        let config = strengthened_v3_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let now = Utc::now();

        seed_event_for_structure_tests(&mut strat, &positions, now);

        let decisions = replay_spot_path(
            &mut strat,
            &positions,
            now,
            &[
                99980, 100180, 100380, 100320, 100620, 100920, 101220, 101520, 101760, 102000,
            ],
        );

        assert_eq!(
            decisions.len(),
            1,
            "strong persistent structure should still enter"
        );
        match &decisions[0] {
            StrategyDecision::Enter { intent, .. } => assert_eq!(intent.token_id, "up-structure"),
            other => panic!("Expected Enter, got {:?}", other),
        }
    }

    #[test]
    fn event_before_first_spot_backfills_price_to_beat_and_allows_entry() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let now = Utc::now();

        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(120),
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        assert_eq!(
            strat.events["BTCUSDT"][0].price_to_beat, None,
            "precondition: event arrived before first spot"
        );

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

        let first_spot_decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            first_spot_decisions.is_empty(),
            "first spot should initialize the event, not trade immediately"
        );
        assert_eq!(strat.events["BTCUSDT"][0].price_to_beat, Some(dec!(100000)));

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(101500),
                ts: now + chrono::Duration::seconds(60),
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "Expected 1 entry signal");
    }

    #[test]
    fn active_order_blocks_duplicate_entry_for_same_event() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let now = Utc::now();

        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now + chrono::Duration::seconds(120),
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &OrderLedger::default(),
        );

        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(100000),
                ts: now,
            },
        );
        strat.events.get_mut("BTCUSDT").unwrap()[0].price_to_beat = Some(dec!(100000));

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
            &OrderLedger::default(),
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
            &OrderLedger::default(),
        );

        let mut orders = OrderLedger::default();
        orders.insert_from_intent(
            "order-1",
            &TradingIntent {
                intent_id: "intent-1".into(),
                deployment_id: "live".into(),
                market_id: "evt1".into(),
                token_id: "up1".into(),
                side: TradeSide::Buy,
                quantity: dec!(10),
                limit_price: Some(dec!(0.30)),
                purpose: IntentPurpose::Entry,
                created_at: now,
            },
        );
        orders.acknowledge("order-1", "venue-1");

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(101500),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "active order should block duplicate entry for the same event"
        );
    }

    #[test]
    fn active_order_on_nearer_event_does_not_block_later_window_for_same_symbol() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let positions = PositionLedger::default();
        let now = Utc::now();
        strat.feed_time = Some(now);

        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt-near".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-near".into(),
                down_token: "dn-near".into(),
                end_time: now + chrono::Duration::seconds(120),
                window_secs: 300,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &OrderLedger::default(),
        );
        strat.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: "evt-far".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up-far".into(),
                down_token: "dn-far".into(),
                end_time: now + chrono::Duration::seconds(220),
                window_secs: 900,
                price_to_beat: None,
                resolved_up_won: None,
            },
            &positions,
            &OrderLedger::default(),
        );

        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(100000),
                ts: now,
            },
        );
        strat.events.get_mut("BTCUSDT").unwrap()[0].price_to_beat = Some(dec!(100000));
        strat.events.get_mut("BTCUSDT").unwrap()[1].price_to_beat = Some(dec!(100000));

        for token_id in ["up-near", "dn-near", "up-far", "dn-far"] {
            let ask = if token_id.starts_with("up") {
                dec!(0.30)
            } else {
                dec!(0.70)
            };
            let bid = ask - dec!(0.01);
            strat.on_update(
                &MarketUpdate::Quote {
                    token_id: token_id.into(),
                    bid: Some(bid),
                    ask: Some(ask),
                    ts: now,
                    bid_size: None,
                    ask_size: None,
                    bid_levels: Vec::new(),
                    ask_levels: Vec::new(),
                },
                &positions,
                &OrderLedger::default(),
            );
        }

        let mut orders = OrderLedger::default();
        orders.insert_from_intent(
            "order-1",
            &TradingIntent {
                intent_id: "intent-1".into(),
                deployment_id: "live".into(),
                market_id: "evt-near".into(),
                token_id: "up-near".into(),
                side: TradeSide::Buy,
                quantity: dec!(10),
                limit_price: Some(dec!(0.30)),
                purpose: IntentPurpose::Entry,
                created_at: now,
            },
        );
        orders.acknowledge("order-1", "venue-1");

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(101500),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert_eq!(decisions.len(), 1, "later event should still be tradable");
        match &decisions[0] {
            StrategyDecision::Enter { intent, .. } => assert_eq!(intent.token_id, "up-far"),
            other => panic!("Expected Enter, got {:?}", other),
        }
    }

    #[test]
    fn official_settlement_overrides_last_spot_direction() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        strat.events.insert(
            "BTCUSDT".into(),
            vec![EventWindow {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now,
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
            }],
        );
        strat.token_symbol.insert("up1".into(), "BTCUSDT".into());
        strat.token_symbol.insert("dn1".into(), "BTCUSDT".into());
        strat.spot.insert(
            "BTCUSDT".into(),
            SpotState {
                price: dec!(101000),
                ts: now,
            },
        );

        let mut positions = PositionLedger::default();
        positions.apply_fill(&FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: now,
        });

        let decisions = strat.on_update(
            &MarketUpdate::EventExpired {
                event_id: "evt1".into(),
                end_time: now,
                // resolved_up_won: Some(false) means UP lost → DOWN token wins
                resolved_up_won: Some(false),
            },
            &positions,
            &OrderLedger::default(),
        );

        assert_eq!(decisions.len(), 1, "Expected settlement exit");
        match &decisions[0] {
            StrategyDecision::Exit(intent) => assert_eq!(intent.limit_price, Some(dec!(0.00))),
            other => panic!("Expected Exit, got {:?}", other),
        }
    }

    #[test]
    fn unresolved_expiry_waits_for_later_spot_instead_of_defaulting_up() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        strat.events.insert(
            "BTCUSDT".into(),
            vec![EventWindow {
                event_id: "evt1".into(),
                symbol: "BTCUSDT".into(),
                up_token: "up1".into(),
                down_token: "dn1".into(),
                end_time: now,
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
            }],
        );
        strat.token_symbol.insert("up1".into(), "BTCUSDT".into());
        strat.token_symbol.insert("dn1".into(), "BTCUSDT".into());

        let mut positions = PositionLedger::default();
        positions.apply_fill(&FillRecord {
            fill_id: "fill-1".into(),
            order_id: "order-1".into(),
            token_id: "up1".into(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: now,
        });

        let pending = strat.on_update(
            &MarketUpdate::EventExpired {
                event_id: "evt1".into(),
                end_time: now,
                resolved_up_won: None,
            },
            &positions,
            &OrderLedger::default(),
        );

        assert!(
            pending.is_empty(),
            "unresolved expiry should stay pending until settlement can be determined"
        );
        assert_eq!(
            strat.events["BTCUSDT"].len(),
            1,
            "event should remain tracked"
        );

        let decisions = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(99000),
                ts: now + chrono::Duration::seconds(5),
            },
            &positions,
            &OrderLedger::default(),
        );

        assert_eq!(decisions.len(), 1, "later spot should resolve the expiry");
        match &decisions[0] {
            StrategyDecision::Exit(intent) => assert_eq!(intent.limit_price, Some(dec!(0.00))),
            other => panic!("Expected Exit, got {:?}", other),
        }
        assert!(
            strat
                .events
                .get("BTCUSDT")
                .map(|events| events.is_empty())
                .unwrap_or(true),
            "resolved expiry should be pruned after settlement"
        );
    }

    #[test]
    fn spot_updates_accumulate_realized_volatility() {
        let config = default_config();
        let mut strat = DirectionalStrategy::new(config);
        let now = Utc::now();

        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        let _ = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        let _ = strat.on_update(
            &MarketUpdate::SpotPrice {
                symbol: "BTCUSDT".into(),
                price: dec!(100100),
                ts: now + chrono::Duration::seconds(5),
            },
            &positions,
            &orders,
        );

        let vol_state = strat.volatility.get("BTCUSDT").expect("vol state");
        assert!(vol_state.ewma_var_per_sec > 0.0);
    }
}
