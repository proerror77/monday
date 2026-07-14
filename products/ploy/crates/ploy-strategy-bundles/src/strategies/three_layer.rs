//! Three-layer strategy config and regime classification.

use chrono::{DateTime, Utc};
use ploy_trading::{
    FillRecord, IntentPurpose, OrderLedger, OrderState, PositionLedger, TradeSide, TradingIntent,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::sync::Arc;
use tracing::{debug, info, warn};

use super::common::event::EventWindow;
use super::common::fees::crypto_fee_cost;
use super::common::quote::QuoteState;
use super::event_ml_model::{
    self, is_event_ml_runtime_score, EventMlModelContract, EventMlModelError,
};
use super::three_layer_model::{
    self, AutoSettlementFactorInputs, BookConfirmationInputs, EntryScoreInputs,
    ThreeLayerModelConfig,
};
use super::three_layer_profile::ThreeLayerProfile;
use crate::strategies::directional::DirectionalConfig;
use crate::traits::{MarketUpdate, SignalRecord, StrategyDecision, StrategyLogic};
use ploy_market_contracts::BookLevel;
use ploy_operator_contracts::Regime;

#[derive(Debug, Clone, Copy)]
enum SettlementAutofactorEdgeMetric {
    TopQuote,
    Executable,
    RawFormula,
}

/// Configuration for the three-layer directional strategy.
#[derive(Debug, Clone)]
pub struct ThreeLayerConfig {
    pub symbols: Vec<String>,
    pub profile: ThreeLayerProfile,
    pub min_direction_prob: f64,
    pub min_distance_over_sigma: f64,
    pub min_confirmation_score: f64,
    pub require_confirmation: bool,
    pub min_drift_confirmation: f64,
    pub min_edge: f64,
    pub min_reward_risk: f64,
    pub alpha_contrarian: bool,
    pub cex_contrarian: bool,
    pub probability_shrink: f64,
    pub probability_haircut: f64,
    pub take_profit_ask: f64,
    pub stop_distance_pct: f64,
    pub max_pm_lag_secs: u64,
    pub min_time_remaining_secs: u64,
    pub max_time_remaining_secs: u64,
    pub cooldown_secs: u64,
    pub stake_usd: Decimal,
    pub max_positions: usize,
    pub max_daily_trades: u32,
    pub allowed_window_secs: Vec<u64>,
    pub min_entry_price: f64,
    pub max_entry_price: f64,
    pub min_entry_score: f64,
    pub autofactor_runtime_score: Option<String>,
    pub event_ml_model: Option<EventMlModelContract>,
    pub visible_depth_haircut: Decimal,
    pub max_sweep_levels: usize,
    pub max_sweep_price_delta: Decimal,
}

impl From<DirectionalConfig> for ThreeLayerConfig {
    fn from(c: DirectionalConfig) -> Self {
        Self {
            symbols: c.symbols,
            profile: c.three_layer_strategy_profile,
            min_direction_prob: c.three_layer_min_direction_prob,
            min_distance_over_sigma: c.three_layer_min_distance_over_sigma,
            min_confirmation_score: c.three_layer_min_confirmation_score,
            require_confirmation: c.three_layer_require_confirmation,
            min_drift_confirmation: c.three_layer_min_drift_confirmation,
            min_edge: c.three_layer_min_edge,
            min_reward_risk: c.three_layer_min_reward_risk,
            alpha_contrarian: c.three_layer_alpha_contrarian,
            cex_contrarian: c.three_layer_cex_contrarian,
            probability_shrink: c.three_layer_probability_shrink,
            probability_haircut: c.three_layer_probability_haircut,
            take_profit_ask: c.three_layer_take_profit_ask,
            stop_distance_pct: c.three_layer_stop_distance_pct,
            max_pm_lag_secs: c.three_layer_max_pm_lag_secs,
            min_time_remaining_secs: c.min_time_remaining_secs,
            max_time_remaining_secs: c.max_time_remaining_secs,
            cooldown_secs: c.cooldown_secs,
            stake_usd: c.stake_usd,
            max_positions: c.max_positions,
            max_daily_trades: c.max_daily_trades,
            allowed_window_secs: c.allowed_window_secs,
            min_entry_price: c.min_entry_price,
            max_entry_price: c.max_entry_price,
            min_entry_score: c.three_layer_min_entry_score,
            autofactor_runtime_score: c.three_layer_autofactor_runtime_score,
            event_ml_model: None,
            visible_depth_haircut: Decimal::ONE,
            max_sweep_levels: 0,
            max_sweep_price_delta: Decimal::ZERO,
        }
    }
}

impl ThreeLayerConfig {
    pub fn from_directional_runtime(c: DirectionalConfig) -> Result<Self, String> {
        let mut config = Self::from(c.clone());
        let runtime_score = config.autofactor_runtime_score.as_deref().unwrap_or("");
        if !is_event_ml_runtime_score(runtime_score) {
            return Ok(config);
        }

        let path = c.three_layer_event_ml_model_path.as_ref().ok_or_else(|| {
            "three_layer_event_ml_model_path is required when runtime score starts with event_ml_model:"
                .to_string()
        })?;
        let raw = fs::read_to_string(path)
            .map_err(|err| format!("read Event ML model artifact {}: {err}", path.display()))?;
        let model = event_ml_model::parse_event_ml_baseline_model(&raw)
            .map_err(|err| format!("parse Event ML model artifact {}: {err}", path.display()))?;
        model
            .validate()
            .map_err(|err| format!("invalid Event ML model artifact {}: {err}", path.display()))?;
        validate_event_ml_runtime_schema(&model)
            .map_err(|err| format!("unsupported Event ML runtime feature schema: {err}"))?;
        config.event_ml_model = Some(model);
        Ok(config)
    }

    fn uses_event_ml_model(&self) -> bool {
        self.autofactor_runtime_score
            .as_deref()
            .map(is_event_ml_runtime_score)
            .unwrap_or(false)
    }

    fn model_config(&self) -> ThreeLayerModelConfig {
        ThreeLayerModelConfig {
            profile: self.profile,
            min_direction_prob: self.min_direction_prob,
            min_distance_over_sigma: self.min_distance_over_sigma,
            min_confirmation_score: self.min_confirmation_score,
            min_drift_confirmation: self.min_drift_confirmation,
            min_edge: self.min_edge,
            min_reward_risk: self.min_reward_risk,
            alpha_contrarian: self.alpha_contrarian,
            cex_contrarian: self.cex_contrarian,
            probability_shrink: self.probability_shrink,
            probability_haircut: self.probability_haircut,
            min_entry_price: self.min_entry_price,
            max_entry_price: self.max_entry_price,
            min_entry_score: self.min_entry_score,
        }
    }
}

const VOL_WINDOW_SECS: f64 = 120.0;
const MIN_VOL_POINTS: usize = 5;
const ACCOUNT_BALANCE_REJECT_PAUSE_SECS: i64 = 15;
const HARD_TOKEN_REJECT_PAUSE_SECS: i64 = 35;
const NO_LIQUIDITY_REJECT_PAUSE_SECS: i64 = 15;

struct DriftTracker {
    history: VecDeque<(DateTime<Utc>, f64)>,
}

impl DriftTracker {
    fn new() -> Self {
        Self {
            history: VecDeque::new(),
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, price: f64) {
        if price <= 0.0 {
            return;
        }
        self.history.push_back((ts, price.ln()));
        while self.history.len() > 1 {
            let oldest = self.history.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > VOL_WINDOW_SECS {
                self.history.pop_front();
            } else {
                break;
            }
        }
    }

    fn drift_30s(&self) -> f64 {
        self.drift_since_secs(30)
    }

    fn drift_since_secs(&self, secs: i64) -> f64 {
        if self.history.len() < 2 {
            return 0.0;
        }
        let now = self.history.back().unwrap();
        let cutoff = now.0 - chrono::Duration::seconds(secs);
        let anchor = self
            .history
            .iter()
            .find(|(ts, _)| *ts >= cutoff)
            .unwrap_or(self.history.front().unwrap());
        now.1 - anchor.1
    }

    fn drift_speed_since_secs(&self, secs: i64) -> f64 {
        if self.history.len() < 2 {
            return 0.0;
        }
        let now = self.history.back().unwrap();
        let cutoff = now.0 - chrono::Duration::seconds(secs);
        let anchor = self
            .history
            .iter()
            .find(|(ts, _)| *ts >= cutoff)
            .unwrap_or(self.history.front().unwrap());
        let dt = (now.0 - anchor.0).num_milliseconds() as f64 / 1000.0;
        if dt <= 0.0 {
            0.0
        } else {
            (now.1 - anchor.1) / dt
        }
    }

    fn sigma_horizon(&mut self, horizon_secs: f64) -> f64 {
        if self.history.len() < MIN_VOL_POINTS {
            return 0.0;
        }
        let contiguous = self.history.make_contiguous();
        let returns: Vec<f64> = contiguous.windows(2).map(|w| w[1].1 - w[0].1).collect();
        if returns.is_empty() {
            return 0.0;
        }
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        let avg_dt = {
            let total_secs = (self.history.back().unwrap().0 - self.history.front().unwrap().0)
                .num_milliseconds() as f64
                / 1000.0;
            total_secs / returns.len() as f64
        };
        if avg_dt <= 0.0 {
            return 0.0;
        }
        let var_per_sec = var / avg_dt;
        (var_per_sec * horizon_secs).sqrt()
    }
}

#[derive(Clone, Copy, Default)]
struct DirectionDriftState {
    prev_drift_30s: f64,
    flip_ts: Option<DateTime<Utc>>,
    post_flip_drift: f64,
}

impl DirectionDriftState {
    fn update(&mut self, drift_30s: f64, ts: DateTime<Utc>) {
        let old_sign = signum(self.prev_drift_30s);
        let new_sign = signum(drift_30s);
        if old_sign != 0.0 && new_sign != 0.0 && old_sign != new_sign {
            self.flip_ts = Some(ts);
        }
        self.prev_drift_30s = drift_30s;
        self.post_flip_drift = drift_30s.abs();
    }

    fn flip_age_secs(&self, now: DateTime<Utc>) -> f64 {
        self.flip_ts
            .map(|ts| (now - ts).num_milliseconds() as f64 / 1000.0)
            .unwrap_or(f64::NAN)
    }
}

struct MpriceDriftAccumulator {
    entries: VecDeque<(DateTime<Utc>, f64)>,
    window_secs: f64,
}

impl MpriceDriftAccumulator {
    fn new(window_secs: f64) -> Self {
        Self {
            entries: VecDeque::new(),
            window_secs,
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, microprice_offset_bps: f64) {
        self.entries.push_back((ts, microprice_offset_bps));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_milliseconds() as f64 / 1000.0 > self.window_secs {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn cum_drift(&self) -> f64 {
        self.entries.iter().map(|e| e.1).sum()
    }
}

#[derive(Clone, Default)]
struct QuoteDepth {
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
    bid_levels: Vec<BookLevel>,
    ask_levels: Vec<BookLevel>,
}

struct QuoteAskHistory {
    entries: VecDeque<(DateTime<Utc>, f64)>,
}

impl QuoteAskHistory {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    fn push(&mut self, ts: DateTime<Utc>, ask: f64) {
        if !ask.is_finite() {
            return;
        }
        self.entries.push_back((ts, ask));
        while self.entries.len() > 1 {
            let oldest = self.entries.front().unwrap().0;
            if (ts - oldest).num_seconds() > 35 {
                self.entries.pop_front();
            } else {
                break;
            }
        }
    }

    fn change_since(&self, ts: DateTime<Utc>, now_ask: f64, secs: i64) -> Option<f64> {
        let cutoff = ts - chrono::Duration::seconds(secs);
        self.entries
            .iter()
            .rev()
            .find(|(entry_ts, _)| *entry_ts <= cutoff)
            .map(|(_, ask)| now_ask - *ask)
    }
}

#[derive(Clone, Copy, Default)]
struct LobState {
    obi: f64,
    obi_prev: f64,
    spread_bps: u32,
    bid_depth_near: f64,
    ask_depth_near: f64,
    signed_trade_imbalance: f64,
    last_aggtrade_ts: Option<DateTime<Utc>>,
    ts: Option<DateTime<Utc>>,
}

impl LobState {
    fn depth_imbalance(&self) -> f64 {
        let total = self.bid_depth_near + self.ask_depth_near;
        if total <= 0.0 {
            return 0.0;
        }
        (self.bid_depth_near - self.ask_depth_near) / total
    }

    fn obi_delta(&self) -> f64 {
        self.obi - self.obi_prev
    }

    fn apply_l2(
        &mut self,
        obi_raw: f64,
        spread_bps: u32,
        bid_near: f64,
        ask_near: f64,
        ts: DateTime<Utc>,
    ) {
        self.obi_prev = self.obi;
        // EWMA smoothing with 5-second half-life.
        // At 10 Hz LOB, raw OBI fluctuates wildly per 100ms tick.
        // Smoothing captures the 5-second trend, filtering noise while
        // preserving real microstructure signals. Causal (no look-ahead).
        if let Some(prev_ts) = self.ts {
            let dt = (ts - prev_ts).num_milliseconds() as f64 / 1000.0;
            if dt > 0.0 && dt < 60.0 {
                let alpha = 1.0 - (-dt / 5.0_f64).exp();
                self.obi = self.obi * (1.0 - alpha) + obi_raw * alpha;
            } else {
                self.obi = obi_raw;
            }
        } else {
            self.obi = obi_raw;
        }
        self.spread_bps = spread_bps;
        self.bid_depth_near = bid_near;
        self.ask_depth_near = ask_near;
        self.ts = Some(ts);
    }

    fn apply_aggtrade(&mut self, quantity: f64, is_buyer_maker: bool, ts: DateTime<Utc>) {
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
        let signed_qty = if is_buyer_maker { -quantity } else { quantity };
        self.signed_trade_imbalance = self.signed_trade_imbalance * decay + signed_qty;
        self.last_aggtrade_ts = Some(ts);
    }
}

// ── Gate Functions ──────────────────────────────────────────────────

#[cfg(test)]
fn norm_cdf(x: f64) -> f64 {
    three_layer_model::norm_cdf(x)
}

#[cfg(test)]
fn calibrate_direction_probability(
    direction_probability: f64,
    probability_shrink: f64,
    probability_haircut: f64,
) -> f64 {
    three_layer_model::calibrate_direction_probability(
        direction_probability,
        probability_shrink,
        probability_haircut,
    )
}

fn profile_confirmation_score(
    direction_sign: f64,
    lob: &LobState,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> f64 {
    three_layer_model::profile_confirmation_score(
        BookConfirmationInputs {
            direction_sign,
            obi: lob.obi,
            obi_delta: lob.obi_delta(),
            depth_imbalance: lob.depth_imbalance(),
            cum_mprice_drift_5m,
            drift_30s,
            signed_trade_imbalance: lob.signed_trade_imbalance,
            regime,
        },
        &config.model_config(),
    )
}

fn evaluate_entry_score(config: &ThreeLayerConfig, inputs: EntryScoreInputs) -> f64 {
    three_layer_model::evaluate_entry_score(&config.model_config(), inputs)
}

fn confirmation_gate_passes(value: f64, threshold: f64, contrarian: bool) -> bool {
    three_layer_model::confirmation_gate_passes(value, threshold, contrarian)
}

fn evaluate_direction_score(
    distance_over_sigma: f64,
    _sigma_horizon: f64,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64, f64)> {
    let score = three_layer_model::evaluate_direction_score(
        distance_over_sigma,
        cum_mprice_drift_5m,
        drift_30s,
        regime,
        &config.model_config(),
    )?;
    Some((
        score.direction_sign,
        score.effective_probability,
        score.direction_score,
    ))
}

#[cfg(test)]
fn evaluate_confirmation_bonus(
    direction_sign: f64,
    lob: &LobState,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerConfig,
) -> f64 {
    let mut model_config = config.model_config();
    model_config.profile = ThreeLayerProfile::Mixed;
    three_layer_model::profile_confirmation_score(
        BookConfirmationInputs {
            direction_sign,
            obi: lob.obi,
            obi_delta: lob.obi_delta(),
            depth_imbalance: lob.depth_imbalance(),
            cum_mprice_drift_5m,
            drift_30s,
            signed_trade_imbalance: lob.signed_trade_imbalance,
            regime,
        },
        &model_config,
    )
}

#[cfg(test)]
fn expected_value_per_share(direction_probability: f64, entry_price: f64) -> f64 {
    three_layer_model::expected_value_per_share(direction_probability, entry_price)
}

#[cfg(test)]
fn expected_value_per_staked_dollar(direction_probability: f64, entry_price: f64) -> f64 {
    three_layer_model::expected_value_per_staked_dollar(direction_probability, entry_price)
}

fn evaluate_edge_score(
    direction_probability: f64,
    ask: f64,
    _regime: Regime,
    config: &ThreeLayerConfig,
) -> Option<(f64, f64, f64, f64)> {
    let score =
        three_layer_model::evaluate_edge_score(direction_probability, ask, &config.model_config())?;
    Some((
        score.entry_price,
        score.expected_value_per_share,
        score.reward_risk,
        score.edge_score,
    ))
}

fn spread_adjusted_external_move_score(side_external_move_30s: f64, side_spread: f64) -> f64 {
    three_layer_model::spread_adjusted_external_move_score(side_external_move_30s, side_spread)
}

fn autofactor_formula_entry_score(
    runtime_score: &str,
    inputs: AutoSettlementFactorInputs,
    min_edge: f64,
) -> Option<(f64, f64)> {
    let raw = three_layer_model::auto_settlement_formula_score(runtime_score, inputs)?;
    let formula_name = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score);
    let base_formula_name = normalized_autofactor_formula_name(formula_name);
    let (threshold, scale) = if base_formula_name
        .starts_with("amplitude_weighted_momentum_30s_sigma")
        || base_formula_name.starts_with("poly_lag_pressure")
        || base_formula_name.starts_with("spread_adjusted_external_move")
    {
        (0.0, 0.02)
    } else {
        (min_edge.max(0.0), 0.08)
    };
    let normalized =
        three_layer_model::threshold_score(raw, threshold, scale, false).clamp(-0.50, 1.0);
    Some((raw, normalized))
}

fn is_settlement_autofactor_runtime_score(runtime_score: &str) -> bool {
    let name = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score);
    let name = normalized_autofactor_formula_name(name);
    name.starts_with("auto_settlement_full_depth_settlement_edge")
        || name.starts_with("auto_settlement_conservative_settlement_edge")
        || name.starts_with("auto_settlement_model_full_depth_settlement_edge")
        || name.starts_with("auto_settlement_model_conservative_settlement_edge")
        || is_predictive_settlement_autofactor_name(name)
}

fn is_model_settlement_autofactor_runtime_score(runtime_score: &str) -> bool {
    let name = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score);
    let name = normalized_autofactor_formula_name(name);
    name.starts_with("auto_settlement_model_full_depth_settlement_edge")
        || name.starts_with("auto_settlement_model_conservative_settlement_edge")
}

fn is_predictive_settlement_autofactor_runtime_score(runtime_score: &str) -> bool {
    let name = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score);
    is_predictive_settlement_autofactor_name(normalized_autofactor_formula_name(name))
}

fn is_predictive_settlement_autofactor_name(name: &str) -> bool {
    name.starts_with("amplitude_weighted_momentum_30s_sigma")
        || name.starts_with("poly_lag_pressure")
        || (name.starts_with("spread_adjusted_external_move")
            && name != "spread_adjusted_external_move")
}

fn settlement_autofactor_entry_score_passes(
    runtime_score: &str,
    raw_score: Option<f64>,
    total_score: f64,
    config: &ThreeLayerConfig,
) -> bool {
    if is_predictive_settlement_autofactor_runtime_score(runtime_score) {
        total_score >= config.min_entry_score
    } else {
        raw_score
            .map(|score| score >= config.min_edge.max(0.0))
            .unwrap_or(total_score >= config.min_entry_score)
    }
}

fn normalized_autofactor_formula_name(mut name: &str) -> &str {
    loop {
        if let Some(stripped) = name.strip_prefix("mut2_") {
            name = stripped;
        } else if let Some(stripped) = name.strip_prefix("mut_") {
            name = stripped;
        } else if let Some(stripped) = name.strip_prefix("mcts_") {
            name = stripped;
        } else if let Some(stripped) = name.strip_prefix("llm_") {
            name = stripped;
        } else {
            return name;
        }
    }
}

fn signum(value: f64) -> f64 {
    if value > 1e-12 {
        1.0
    } else if value < -1e-12 {
        -1.0
    } else {
        0.0
    }
}

fn validate_event_ml_runtime_schema(model: &EventMlModelContract) -> Result<(), EventMlModelError> {
    model.validate()?;
    for feature in &model.feature_schema {
        if !is_supported_event_ml_runtime_feature(feature) {
            return Err(EventMlModelError::MissingFeature(feature.clone()));
        }
    }
    Ok(())
}

fn is_supported_event_ml_runtime_feature(feature: &str) -> bool {
    matches!(
        feature,
        "signed_distance_to_beat"
            | "abs_distance_to_beat"
            | "drift_10s"
            | "drift_30s"
            | "flip_age_secs"
            | "post_flip_drift"
            | "sigma_horizon"
            | "fair_prob_up"
            | "fair_prob_up_clean"
            | "prob_disagreement"
            | "implied_sigma_horizon"
            | "vol_gap"
            | "distance_over_sigma"
            | "model_prob_up"
            | "model_edge_up"
            | "reward_risk_up"
            | "reward_risk_down"
            | "obi"
            | "spread_bps"
            | "bid_depth_near"
            | "ask_depth_near"
            | "depth_ratio"
            | "depth_imbalance"
            | "pm_up_ask"
            | "pm_down_ask"
            | "pm_lag_secs"
            | "cum_mprice_drift_5m"
            | "cum_trade_imbalance_5m"
    )
}

fn quote_mid(bid: f64, ask: f64) -> f64 {
    if bid.is_finite() && ask.is_finite() && bid > 0.0 && ask > 0.0 && bid <= ask {
        0.5 * (bid + ask)
    } else if ask.is_finite() && ask > 0.0 {
        ask
    } else if bid.is_finite() && bid > 0.0 {
        bid
    } else {
        f64::NAN
    }
}

fn fair_market_prob_up(up_bid: f64, up_ask: f64, down_bid: f64, down_ask: f64) -> f64 {
    let up_mid = quote_mid(up_bid, up_ask);
    let down_mid = quote_mid(down_bid, down_ask);
    if !up_mid.is_finite() || !down_mid.is_finite() || up_mid <= 0.0 || down_mid <= 0.0 {
        return f64::NAN;
    }
    let total = up_mid + down_mid;
    if total <= 0.0 {
        return f64::NAN;
    }
    (up_mid / total).clamp(1e-4, 1.0 - 1e-4)
}

fn clean_market_prob_up(
    up_bid: f64,
    up_ask: f64,
    down_bid: f64,
    down_ask: f64,
    up_break_even_prob: f64,
    down_break_even_prob: f64,
) -> f64 {
    if !up_break_even_prob.is_finite() || !down_break_even_prob.is_finite() {
        return f64::NAN;
    }
    let down_implied_up = 1.0 - down_break_even_prob;
    let ask_clean = 0.5 * (up_break_even_prob + down_implied_up);
    let mid_fair = fair_market_prob_up(up_bid, up_ask, down_bid, down_ask);
    if mid_fair.is_finite() {
        (0.5 * ask_clean + 0.5 * mid_fair).clamp(1e-4, 1.0 - 1e-4)
    } else {
        ask_clean.clamp(1e-4, 1.0 - 1e-4)
    }
}

fn implied_prob_disagreement(up_break_even_prob: f64, down_break_even_prob: f64) -> f64 {
    if !up_break_even_prob.is_finite() || !down_break_even_prob.is_finite() {
        return f64::NAN;
    }
    up_break_even_prob - (1.0 - down_break_even_prob)
}

fn inv_normal_cdf(p: f64) -> f64 {
    if !p.is_finite() {
        return f64::NAN;
    }
    let p = p.clamp(1e-12, 1.0 - 1e-12);
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const P_LOW: f64 = 0.02425;
    const P_HIGH: f64 = 1.0 - P_LOW;
    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        return (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    if p > P_HIGH {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        return -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    let q = p - 0.5;
    let r = q * q;
    (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
        / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
}

fn implied_sigma_horizon(price_to_beat: f64, spot: f64, fair_prob_up: f64) -> f64 {
    if !price_to_beat.is_finite()
        || !spot.is_finite()
        || price_to_beat <= 0.0
        || spot <= 0.0
        || !fair_prob_up.is_finite()
    {
        return f64::NAN;
    }
    let log_ratio = (spot / price_to_beat).ln().abs();
    if log_ratio <= 1e-12 {
        return 0.0;
    }
    let z = inv_normal_cdf(fair_prob_up);
    if !z.is_finite() || z.abs() <= 1e-9 {
        return f64::NAN;
    }
    log_ratio / z.abs()
}

// ── ThreeLayerStrategy ─────────────────────────────────────────────

pub struct ThreeLayerStrategy {
    config: ThreeLayerConfig,
    drift: HashMap<Arc<str>, DriftTracker>,
    drift_state: HashMap<Arc<str>, DirectionDriftState>,
    lob: HashMap<Arc<str>, LobState>,
    mprice_acc: HashMap<Arc<str>, MpriceDriftAccumulator>,
    spot: HashMap<Arc<str>, Decimal>,
    quotes: HashMap<Arc<str>, QuoteState>,
    quote_depth: HashMap<Arc<str>, QuoteDepth>,
    quote_ask_history: HashMap<Arc<str>, QuoteAskHistory>,
    token_symbol: HashMap<Arc<str>, Arc<str>>,
    token_event: HashMap<Arc<str>, Arc<str>>,
    events: HashMap<Arc<str>, Vec<EventWindow>>,
    last_entry: HashMap<Arc<str>, DateTime<Utc>>,
    token_reject_until: HashMap<Arc<str>, DateTime<Utc>>,
    balance_exhausted_until: Option<DateTime<Utc>>,
    daily_trade_count: u32,
    daily_trade_date: Option<chrono::NaiveDate>,
    feed_time: Option<DateTime<Utc>>,
    diagnostics: HashMap<&'static str, u64>,
}

impl ThreeLayerStrategy {
    pub fn new(config: ThreeLayerConfig) -> Self {
        Self {
            config,
            drift: HashMap::new(),
            drift_state: HashMap::new(),
            lob: HashMap::new(),
            mprice_acc: HashMap::new(),
            spot: HashMap::new(),
            quotes: HashMap::new(),
            quote_depth: HashMap::new(),
            quote_ask_history: HashMap::new(),
            token_symbol: HashMap::new(),
            token_event: HashMap::new(),
            events: HashMap::new(),
            last_entry: HashMap::new(),
            token_reject_until: HashMap::new(),
            balance_exhausted_until: None,
            daily_trade_count: 0,
            daily_trade_date: None,
            feed_time: None,
            diagnostics: HashMap::new(),
        }
    }

    fn bump(&mut self, key: &'static str) {
        self.bump_by(key, 1);
    }

    fn bump_by(&mut self, key: &'static str, value: u64) {
        *self.diagnostics.entry(key).or_insert(0) += value;
    }

    fn maybe_entry_for_symbol(
        &mut self,
        symbol: &Arc<str>,
        ts: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        if self
            .last_entry
            .get(symbol)
            .is_some_and(|last| (ts - *last).num_seconds() < self.config.cooldown_secs as i64)
        {
            return None;
        }
        self.try_entry(symbol, ts, positions, orders)
    }

    fn bump_settlement_autofactor_edge_bucket(
        &mut self,
        metric: SettlementAutofactorEdgeMetric,
        value: f64,
    ) {
        const TOP_QUOTE_EDGE_BUCKETS: [&str; 7] = [
            "settlement_autofactor_top_quote_edge_lt_neg5pct",
            "settlement_autofactor_top_quote_edge_neg5_to_neg2pct",
            "settlement_autofactor_top_quote_edge_neg2_to_0pct",
            "settlement_autofactor_top_quote_edge_0_to_1pct",
            "settlement_autofactor_top_quote_edge_1_to_2pct",
            "settlement_autofactor_top_quote_edge_2_to_5pct",
            "settlement_autofactor_top_quote_edge_ge_5pct",
        ];
        const EXECUTABLE_EDGE_BUCKETS: [&str; 7] = [
            "settlement_autofactor_executable_edge_lt_neg5pct",
            "settlement_autofactor_executable_edge_neg5_to_neg2pct",
            "settlement_autofactor_executable_edge_neg2_to_0pct",
            "settlement_autofactor_executable_edge_0_to_1pct",
            "settlement_autofactor_executable_edge_1_to_2pct",
            "settlement_autofactor_executable_edge_2_to_5pct",
            "settlement_autofactor_executable_edge_ge_5pct",
        ];
        const RAW_FORMULA_BUCKETS: [&str; 7] = [
            "settlement_autofactor_raw_score_lt_neg5pct",
            "settlement_autofactor_raw_score_neg5_to_neg2pct",
            "settlement_autofactor_raw_score_neg2_to_0pct",
            "settlement_autofactor_raw_score_0_to_1pct",
            "settlement_autofactor_raw_score_1_to_2pct",
            "settlement_autofactor_raw_score_2_to_5pct",
            "settlement_autofactor_raw_score_ge_5pct",
        ];

        if !value.is_finite() {
            self.bump(match metric {
                SettlementAutofactorEdgeMetric::TopQuote => {
                    "settlement_autofactor_top_quote_edge_nonfinite"
                }
                SettlementAutofactorEdgeMetric::Executable => {
                    "settlement_autofactor_executable_edge_nonfinite"
                }
                SettlementAutofactorEdgeMetric::RawFormula => {
                    "settlement_autofactor_raw_score_nonfinite"
                }
            });
            return;
        }

        let bucket = if value < -0.05 {
            0
        } else if value < -0.02 {
            1
        } else if value < 0.0 {
            2
        } else if value < 0.01 {
            3
        } else if value < 0.02 {
            4
        } else if value < 0.05 {
            5
        } else {
            6
        };

        self.bump(match metric {
            SettlementAutofactorEdgeMetric::TopQuote => TOP_QUOTE_EDGE_BUCKETS[bucket],
            SettlementAutofactorEdgeMetric::Executable => EXECUTABLE_EDGE_BUCKETS[bucket],
            SettlementAutofactorEdgeMetric::RawFormula => RAW_FORMULA_BUCKETS[bucket],
        });
    }

    fn bump_predictive_autofactor_counterfactual_scores(&mut self, raw: f64, score: f64) {
        if !score.is_finite() {
            self.bump("settlement_autofactor_predictive_score_nonfinite");
            return;
        }
        let reverse_score =
            three_layer_model::threshold_score(-raw, 0.0, 0.02, false).clamp(-0.50, 1.0);
        for (threshold, direct_key, reverse_key) in [
            (
                0.05,
                "settlement_autofactor_predictive_score_ge_005",
                "settlement_autofactor_predictive_reverse_score_ge_005",
            ),
            (
                0.10,
                "settlement_autofactor_predictive_score_ge_010",
                "settlement_autofactor_predictive_reverse_score_ge_010",
            ),
            (
                0.15,
                "settlement_autofactor_predictive_score_ge_015",
                "settlement_autofactor_predictive_reverse_score_ge_015",
            ),
            (
                0.25,
                "settlement_autofactor_predictive_score_ge_025",
                "settlement_autofactor_predictive_reverse_score_ge_025",
            ),
        ] {
            if score >= threshold {
                self.bump(direct_key);
            }
            if reverse_score >= threshold {
                self.bump(reverse_key);
            }
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.feed_time.unwrap_or_else(Utc::now)
    }

    fn reset_daily_counter(&mut self, ts: DateTime<Utc>) {
        let today = ts.date_naive();
        if self.daily_trade_date != Some(today) {
            self.daily_trade_count = 0;
            self.daily_trade_date = Some(today);
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
                    .filter(|e| {
                        self.window_allowed(e.window_secs) && e.end_time > now && {
                            let remaining = (e.end_time - now).num_seconds();
                            remaining >= self.config.min_time_remaining_secs as i64
                                && remaining <= self.config.max_time_remaining_secs as i64
                        }
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn entry_quantity(&self, entry_price: Decimal) -> Decimal {
        if entry_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (self.config.stake_usd / entry_price).round_dp(6)
    }

    fn sweep_limit_price(
        base_price: Decimal,
        side: TradeSide,
        max_price_delta: Decimal,
    ) -> Decimal {
        let delta = max_price_delta.max(Decimal::ZERO);
        match side {
            TradeSide::Buy => base_price + delta,
            TradeSide::Sell => (base_price - delta).max(Decimal::ZERO),
        }
    }

    fn sweep_fill_price(
        levels: &[BookLevel],
        quantity: Decimal,
        fallback_price: Decimal,
        limit_price: Decimal,
        side: TradeSide,
        visible_depth_haircut: Decimal,
        max_sweep_levels: usize,
    ) -> Option<Decimal> {
        if levels.is_empty() {
            return Some(fallback_price);
        }

        let level_limit = if max_sweep_levels == 0 {
            usize::MAX
        } else {
            max_sweep_levels
        };
        let mut remaining = quantity;
        let mut notional = Decimal::ZERO;
        let haircut = visible_depth_haircut.max(Decimal::ZERO);
        for level in levels.iter().take(level_limit) {
            match side {
                TradeSide::Buy if level.price > limit_price => break,
                TradeSide::Sell if level.price < limit_price => break,
                _ => {}
            }
            let usable_size = (level.size * haircut).max(Decimal::ZERO);
            if usable_size <= Decimal::ZERO {
                continue;
            }
            let take = usable_size.min(remaining);
            notional += take * level.price;
            remaining -= take;
            if remaining <= Decimal::ZERO {
                return Some((notional / quantity).round_dp(6));
            }
        }
        None
    }

    fn reject_cooldown_secs(&self) -> i64 {
        self.config.cooldown_secs.max(1) as i64
    }

    fn balance_pause_active(&self, now: DateTime<Utc>) -> bool {
        self.balance_exhausted_until
            .is_some_and(|until| now < until)
    }

    fn token_reject_active(&self, token_id: &str, now: DateTime<Utc>) -> bool {
        self.token_reject_until
            .get(token_id)
            .is_some_and(|until| now < *until)
    }

    fn set_token_reject_until(&mut self, token_id: &str, until: DateTime<Utc>) {
        self.token_reject_until
            .entry(Arc::<str>::from(token_id))
            .and_modify(|existing| {
                if until > *existing {
                    *existing = until;
                }
            })
            .or_insert(until);
    }

    fn set_balance_exhausted_until(&mut self, until: DateTime<Utc>) {
        match self.balance_exhausted_until {
            Some(existing) if existing >= until => {}
            _ => self.balance_exhausted_until = Some(until),
        }
    }

    fn record_quote_ask(&mut self, token_id: Arc<str>, ask: Option<Decimal>, ts: DateTime<Utc>) {
        let Some(ask) = ask.and_then(|price| price.to_f64()) else {
            return;
        };
        self.quote_ask_history
            .entry(token_id)
            .or_insert_with(QuoteAskHistory::new)
            .push(ts, ask);
    }

    fn pm_momentum_score(&self, token_id: &str, ask: f64, now: DateTime<Utc>) -> f64 {
        let Some(history) = self.quote_ask_history.get(token_id) else {
            return 0.0;
        };
        let pm_momentum = [
            history.change_since(now, ask, 10),
            history.change_since(now, ask, 30),
        ]
        .into_iter()
        .flatten()
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);

        if pm_momentum.is_finite() {
            (pm_momentum / 0.08).clamp(-0.50, 1.0)
        } else {
            0.0
        }
    }

    fn active_order_exists(orders: &OrderLedger, token_id: &str) -> bool {
        orders.orders().any(|o| {
            o.token_id == token_id
                && matches!(
                    o.state,
                    OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
                )
        })
    }

    fn event_position_exists(
        event: &EventWindow,
        positions: &PositionLedger,
        selected_token_id: &str,
    ) -> bool {
        [event.up_token.as_ref(), event.down_token.as_ref()]
            .into_iter()
            .any(|token_id| {
                token_id != selected_token_id && positions.net_qty(token_id) > Decimal::ZERO
            })
    }

    fn event_active_order_exists(
        event: &EventWindow,
        orders: &OrderLedger,
        selected_token_id: &str,
    ) -> bool {
        [event.up_token.as_ref(), event.down_token.as_ref()]
            .into_iter()
            .any(|token_id| {
                token_id != selected_token_id && Self::active_order_exists(orders, token_id)
            })
    }

    fn event_ml_quote(&self, token_id: &str, now: DateTime<Utc>) -> Option<(f64, f64, f64)> {
        let quote = self.quotes.get(token_id)?;
        let bid = quote
            .bid
            .and_then(|price| price.to_f64())
            .unwrap_or(f64::NAN);
        let ask = quote.ask.and_then(|price| price.to_f64())?;
        let lag_secs = (now - quote.ts).num_seconds() as f64;
        if ask.is_finite()
            && ask > 0.0
            && ask < 1.0
            && lag_secs.is_finite()
            && lag_secs >= 0.0
            && lag_secs <= self.config.max_pm_lag_secs as f64
        {
            Some((bid, ask, lag_secs))
        } else {
            None
        }
    }

    fn runtime_side_fair_probability(
        &self,
        event: &EventWindow,
        betting_up: bool,
        now: DateTime<Utc>,
    ) -> Option<f64> {
        let up_quote = self.event_ml_quote(&event.up_token, now)?;
        let down_quote = self.event_ml_quote(&event.down_token, now)?;
        let up_break_even_prob = (up_quote.1 + crypto_fee_cost(up_quote.1)).clamp(1e-4, 1.0 - 1e-4);
        let down_break_even_prob =
            (down_quote.1 + crypto_fee_cost(down_quote.1)).clamp(1e-4, 1.0 - 1e-4);
        let fair_up = clean_market_prob_up(
            up_quote.0,
            up_quote.1,
            down_quote.0,
            down_quote.1,
            up_break_even_prob,
            down_break_even_prob,
        );
        if !fair_up.is_finite() {
            return None;
        }
        Some(if betting_up { fair_up } else { 1.0 - fair_up })
    }

    fn runtime_side_model_probability(
        &self,
        spot_price: f64,
        price_to_beat: f64,
        sigma_horizon: f64,
        betting_up: bool,
    ) -> Option<f64> {
        if spot_price <= 0.0
            || price_to_beat <= 0.0
            || !sigma_horizon.is_finite()
            || sigma_horizon <= 0.0
        {
            return None;
        }
        let probability_up =
            three_layer_model::norm_cdf((spot_price / price_to_beat).ln() / sigma_horizon);
        if !probability_up.is_finite() {
            return None;
        }
        Some(if betting_up {
            probability_up
        } else {
            1.0 - probability_up
        })
    }

    fn settlement_autofactor_best_direction(
        &self,
        event: &EventWindow,
        now: DateTime<Utc>,
        spot_price: f64,
        price_to_beat: f64,
        sigma_horizon: f64,
        use_model_probability: bool,
    ) -> Option<(f64, f64, f64)> {
        [true, false]
            .into_iter()
            .filter_map(|betting_up| {
                let token_id = if betting_up {
                    event.up_token.as_ref()
                } else {
                    event.down_token.as_ref()
                };
                let quote = self.quotes.get(token_id)?;
                let ask = quote.ask.and_then(|price| price.to_f64())?;
                if ask < self.config.min_entry_price || ask > self.config.max_entry_price {
                    return None;
                }
                let side_probability = if use_model_probability {
                    self.runtime_side_model_probability(
                        spot_price,
                        price_to_beat,
                        sigma_horizon,
                        betting_up,
                    )?
                } else {
                    self.runtime_side_fair_probability(event, betting_up, now)?
                };
                let edge = three_layer_model::expected_value_per_share(side_probability, ask);
                if !edge.is_finite() {
                    return None;
                }
                let direction_sign = if betting_up { 1.0 } else { -1.0 };
                let direction_score =
                    three_layer_model::threshold_score(side_probability, 0.50, 0.25, false)
                        .clamp(-0.50, 1.0);
                Some((edge, direction_sign, side_probability, direction_score))
            })
            .max_by(|left, right| {
                left.0
                    .partial_cmp(&right.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(_, direction_sign, side_probability, direction_score)| {
                (direction_sign, side_probability, direction_score)
            })
    }

    #[allow(clippy::too_many_arguments)]
    fn event_ml_feature_values(
        &self,
        symbol: &str,
        now: DateTime<Utc>,
        spot_price: f64,
        price_to_beat: f64,
        time_remaining: i64,
        up_quote: (f64, f64, f64),
        down_quote: (f64, f64, f64),
        sigma_horizon: f64,
        distance_over_sigma: f64,
    ) -> BTreeMap<String, f64> {
        let (up_bid, up_ask, up_lag) = up_quote;
        let (down_bid, down_ask, down_lag) = down_quote;
        let signed_distance = (spot_price - price_to_beat) / price_to_beat;
        let up_break_even_prob = (up_ask + crypto_fee_cost(up_ask)).clamp(1e-4, 1.0 - 1e-4);
        let down_break_even_prob = (down_ask + crypto_fee_cost(down_ask)).clamp(1e-4, 1.0 - 1e-4);
        let fair_prob_up = fair_market_prob_up(up_bid, up_ask, down_bid, down_ask);
        let fair_prob_up_clean = clean_market_prob_up(
            up_bid,
            up_ask,
            down_bid,
            down_ask,
            up_break_even_prob,
            down_break_even_prob,
        );
        let implied_sigma = implied_sigma_horizon(price_to_beat, spot_price, fair_prob_up_clean);
        let model_prob_up = three_layer_model::norm_cdf(
            (spot_price / price_to_beat).ln() / sigma_horizon.max(1e-12),
        );
        let lob = self.lob.get(symbol).copied().unwrap_or_default();
        let depth_ratio = if lob.ask_depth_near > 0.0 {
            lob.bid_depth_near / lob.ask_depth_near
        } else {
            f64::NAN
        };
        let drift = self.drift.get(symbol);
        let drift_state = self.drift_state.get(symbol).copied().unwrap_or_default();
        let mut values = BTreeMap::new();
        values.insert("signed_distance_to_beat".to_string(), signed_distance);
        values.insert("abs_distance_to_beat".to_string(), signed_distance.abs());
        values.insert(
            "drift_10s".to_string(),
            drift
                .map(|tracker| tracker.drift_speed_since_secs(10))
                .unwrap_or(0.0),
        );
        values.insert(
            "drift_30s".to_string(),
            drift
                .map(|tracker| tracker.drift_speed_since_secs(30))
                .unwrap_or(0.0),
        );
        values.insert("flip_age_secs".to_string(), drift_state.flip_age_secs(now));
        values.insert("post_flip_drift".to_string(), drift_state.post_flip_drift);
        values.insert("sigma_horizon".to_string(), sigma_horizon);
        values.insert("fair_prob_up".to_string(), fair_prob_up);
        values.insert("fair_prob_up_clean".to_string(), fair_prob_up_clean);
        values.insert(
            "prob_disagreement".to_string(),
            implied_prob_disagreement(up_break_even_prob, down_break_even_prob),
        );
        values.insert("implied_sigma_horizon".to_string(), implied_sigma);
        values.insert("vol_gap".to_string(), implied_sigma - sigma_horizon);
        values.insert("distance_over_sigma".to_string(), distance_over_sigma);
        values.insert("model_prob_up".to_string(), model_prob_up);
        values.insert(
            "model_edge_up".to_string(),
            model_prob_up - up_ask - crypto_fee_cost(up_ask),
        );
        values.insert(
            "reward_risk_up".to_string(),
            three_layer_model::reward_risk_ratio(up_ask),
        );
        values.insert(
            "reward_risk_down".to_string(),
            three_layer_model::reward_risk_ratio(down_ask),
        );
        values.insert("obi".to_string(), lob.obi);
        values.insert("spread_bps".to_string(), lob.spread_bps as f64);
        values.insert("bid_depth_near".to_string(), lob.bid_depth_near);
        values.insert("ask_depth_near".to_string(), lob.ask_depth_near);
        values.insert("depth_ratio".to_string(), depth_ratio);
        values.insert("depth_imbalance".to_string(), lob.depth_imbalance());
        values.insert("pm_up_ask".to_string(), up_ask);
        values.insert("pm_down_ask".to_string(), down_ask);
        values.insert("pm_lag_secs".to_string(), up_lag.min(down_lag));
        values.insert(
            "cum_mprice_drift_5m".to_string(),
            self.mprice_acc
                .get(symbol)
                .map(MpriceDriftAccumulator::cum_drift)
                .unwrap_or(0.0),
        );
        values.insert(
            "cum_trade_imbalance_5m".to_string(),
            lob.signed_trade_imbalance,
        );
        values.insert("time_remaining_secs".to_string(), time_remaining as f64);
        values
    }

    #[allow(clippy::too_many_arguments)]
    fn try_event_ml_entry_for_event(
        &mut self,
        symbol: &str,
        now: DateTime<Utc>,
        event: &EventWindow,
        spot_price: f64,
        price_to_beat: f64,
        time_remaining: i64,
        sigma_horizon: f64,
        distance_over_sigma: f64,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        let Some(model) = self.config.event_ml_model.as_ref() else {
            self.bump("skip_event_ml_model_unavailable");
            return None;
        };
        let Some(up_quote) = self.event_ml_quote(&event.up_token, now) else {
            self.bump("skip_event_ml_up_quote_unavailable");
            return None;
        };
        let Some(down_quote) = self.event_ml_quote(&event.down_token, now) else {
            self.bump("skip_event_ml_down_quote_unavailable");
            return None;
        };
        let values = self.event_ml_feature_values(
            symbol,
            now,
            spot_price,
            price_to_beat,
            time_remaining,
            up_quote,
            down_quote,
            sigma_horizon,
            distance_over_sigma,
        );
        let score = match model.score_map(&values) {
            Ok(score) => score,
            Err(err) => {
                self.bump("skip_event_ml_score_unavailable");
                debug!(strategy = "three_layer", symbol, error = %err, "Event ML score unavailable");
                return None;
            }
        };
        let q_up = score.probability;
        let up_edge = q_up - up_quote.1 - crypto_fee_cost(up_quote.1);
        let q_down = 1.0 - q_up;
        let down_edge = q_down - down_quote.1 - crypto_fee_cost(down_quote.1);
        let (token_id, direction, side_probability, entry_price_f, edge) = if up_edge >= down_edge {
            (&event.up_token, "UP", q_up, up_quote.1, up_edge)
        } else {
            (&event.down_token, "DOWN", q_down, down_quote.1, down_edge)
        };
        if edge < self.config.min_edge {
            self.bump("skip_event_ml_edge");
            return None;
        }
        let total_score =
            three_layer_model::threshold_score(edge, self.config.min_edge.max(0.0), 0.08, false)
                .clamp(-0.50, 1.0);
        if total_score < self.config.min_entry_score {
            self.bump("skip_entry_score");
            return None;
        }
        if positions.net_qty(token_id) > Decimal::ZERO {
            self.bump("skip_existing_position");
            return None;
        }
        if Self::active_order_exists(orders, token_id) {
            self.bump("skip_active_order");
            return None;
        }
        if Self::event_position_exists(event, positions, token_id) {
            self.bump("skip_existing_event_position");
            return None;
        }
        if Self::event_active_order_exists(event, orders, token_id) {
            self.bump("skip_active_event_order");
            return None;
        }
        if self.token_reject_active(token_id, now) {
            self.bump("skip_token_reject_cooldown");
            return None;
        }

        let Some(entry_price) = Decimal::try_from(entry_price_f).ok() else {
            self.bump("skip_bad_entry_price");
            return None;
        };
        let entry_limit_price = Self::sweep_limit_price(
            entry_price,
            TradeSide::Buy,
            self.config.max_sweep_price_delta,
        );
        let quantity = self.entry_quantity(entry_limit_price);
        if quantity <= Decimal::ZERO {
            self.bump("skip_zero_quantity");
            return None;
        }
        let Some(depth) = self.quote_depth.get(token_id) else {
            self.bump("skip_no_ask_size");
            return None;
        };
        let Some(ask_size) = depth.ask_size else {
            self.bump("skip_no_ask_size");
            return None;
        };
        if depth.ask_levels.is_empty() && ask_size < quantity {
            self.bump("skip_insufficient_ask_size");
            return None;
        }
        let Some(executable_entry_price) = Self::sweep_fill_price(
            &depth.ask_levels,
            quantity,
            entry_price,
            entry_limit_price,
            TradeSide::Buy,
            self.config.visible_depth_haircut,
            self.config.max_sweep_levels,
        ) else {
            self.bump("skip_insufficient_ask_depth");
            return None;
        };
        let executable_entry_price_f = executable_entry_price.to_f64().unwrap_or(entry_price_f);
        let executable_edge =
            side_probability - executable_entry_price_f - crypto_fee_cost(executable_entry_price_f);
        if executable_edge < self.config.min_edge {
            self.bump("skip_event_ml_edge");
            return None;
        }

        let intent_id = format!(
            "tl_event_ml_{}_{}_{}_{}",
            symbol.to_lowercase(),
            direction.to_lowercase(),
            event.event_id,
            now.timestamp_millis()
        );
        let intent = TradingIntent {
            intent_id: intent_id.clone(),
            deployment_id: String::new(),
            market_id: event.event_id.to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity,
            limit_price: Some(entry_limit_price),
            purpose: IntentPurpose::Entry,
            created_at: now,
        };
        let signal = SignalRecord {
            strategy: self.name().to_string(),
            event_id: Some(event.event_id.to_string()),
            token_id: Some(token_id.to_string()),
            intent_id: Some(intent_id),
            symbol: symbol.to_string(),
            direction: direction.to_string(),
            p_hat: side_probability,
            edge: executable_edge,
            entry_price: executable_entry_price,
            decision: "enter".to_string(),
            ts: now,
        };

        info!(
            strategy = "three_layer",
            symbol = %symbol,
            direction,
            event_ml_runtime_score = self.config.autofactor_runtime_score.as_deref().unwrap_or(""),
            event_ml_probability_up = q_up,
            edge = executable_edge,
            total_score,
            entry_price = %executable_entry_price,
            limit_price = %entry_limit_price,
            "Event ML entry signal"
        );
        self.bump("entry_signals");
        Some(StrategyDecision::Enter {
            intent,
            signal: Some(signal),
        })
    }

    fn try_entry(
        &mut self,
        symbol: &str,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Option<StrategyDecision> {
        self.bump("entry_evaluations");
        if self.balance_pause_active(now) {
            self.bump("skip_balance_pause");
            debug!(symbol, "Balance exhausted pause active, skipping entry");
            return None;
        }
        if self.config.max_daily_trades > 0
            && self.daily_trade_count >= self.config.max_daily_trades
        {
            self.bump("skip_max_daily_trades");
            return None;
        }
        if positions.positions().count() >= self.config.max_positions {
            self.bump("skip_max_positions");
            return None;
        }

        let Some(spot_price) = self.spot.get(symbol).and_then(|price| price.to_f64()) else {
            self.bump("skip_no_spot");
            return None;
        };
        if spot_price <= 0.0 {
            self.bump("skip_bad_spot");
            return None;
        }

        // Cooldown check
        if let Some(last) = self.last_entry.get(symbol) {
            if (now - *last).num_seconds() < self.config.cooldown_secs as i64 {
                self.bump("skip_symbol_cooldown");
                return None;
            }
        }

        let candidates = self.candidate_events(symbol, now);
        if candidates.is_empty() {
            self.bump("skip_no_candidate_events");
        } else {
            self.bump_by("candidate_events", candidates.len() as u64);
        }
        for event in &candidates {
            let time_remaining = (event.end_time - now).num_seconds();
            let Some(price_to_beat) = event.price_to_beat.and_then(|price| price.to_f64()) else {
                self.bump("skip_no_price_to_beat");
                continue;
            };
            if price_to_beat <= 0.0 {
                self.bump("skip_bad_price_to_beat");
                continue;
            }

            let regime = Regime::from_secs(time_remaining);

            let Some(drift_tracker) = self.drift.get_mut(symbol) else {
                self.bump("skip_no_drift_tracker");
                return None;
            };
            let sigma_h = drift_tracker.sigma_horizon(time_remaining as f64);
            let drift_30s = drift_tracker.drift_30s();

            let distance_over_sigma = if sigma_h > 0.0 {
                (spot_price - price_to_beat) / (sigma_h * price_to_beat)
            } else {
                0.0
            };

            let cum_mprice_drift_5m = self
                .mprice_acc
                .get(symbol)
                .map(|acc| acc.cum_drift())
                .unwrap_or(0.0);

            if self.config.uses_event_ml_model() {
                if let Some(decision) = self.try_event_ml_entry_for_event(
                    symbol,
                    now,
                    event,
                    spot_price,
                    price_to_beat,
                    time_remaining,
                    sigma_h,
                    distance_over_sigma,
                    positions,
                    orders,
                ) {
                    return Some(decision);
                }
                continue;
            }

            let runtime_score = self.config.autofactor_runtime_score.clone();
            let uses_settlement_autofactor = runtime_score
                .as_deref()
                .map(is_settlement_autofactor_runtime_score)
                .unwrap_or(false);
            let uses_model_settlement_autofactor = runtime_score
                .as_deref()
                .map(is_model_settlement_autofactor_runtime_score)
                .unwrap_or(false);
            let uses_predictive_settlement_autofactor = runtime_score
                .as_deref()
                .map(is_predictive_settlement_autofactor_runtime_score)
                .unwrap_or(false);

            // Layer 1: Direction score. Settlement AutoFactor profiles select
            // the executable UP/DOWN side by settlement edge, not by the
            // legacy CEX-only direction gate.
            let Some((direction_sign, effective_p, direction_score)) =
                (if uses_settlement_autofactor {
                    self.settlement_autofactor_best_direction(
                        event,
                        now,
                        spot_price,
                        price_to_beat,
                        sigma_h,
                        uses_model_settlement_autofactor,
                    )
                } else {
                    evaluate_direction_score(
                        distance_over_sigma,
                        sigma_h,
                        cum_mprice_drift_5m,
                        drift_30s,
                        regime,
                        &self.config,
                    )
                })
            else {
                if uses_settlement_autofactor {
                    self.bump("skip_settlement_side_score");
                } else {
                    self.bump("skip_direction_score");
                }
                continue;
            };

            let betting_up = direction_sign > 0.0;
            let (token_id, direction) = if betting_up {
                (&event.up_token, "UP")
            } else {
                (&event.down_token, "DOWN")
            };
            if uses_settlement_autofactor {
                self.bump("settlement_autofactor_side_selected");
            }

            // Skip if already positioned or have active order on this token
            if positions.net_qty(token_id) > Decimal::ZERO {
                self.bump("skip_existing_position");
                continue;
            }
            if Self::active_order_exists(orders, token_id) {
                self.bump("skip_active_order");
                continue;
            }
            if Self::event_position_exists(event, positions, token_id) {
                self.bump("skip_existing_event_position");
                continue;
            }
            if Self::event_active_order_exists(event, orders, token_id) {
                self.bump("skip_active_event_order");
                continue;
            }
            if self.token_reject_active(token_id, now) {
                self.bump("skip_token_reject_cooldown");
                continue;
            }

            // Check quote freshness
            let Some(quote) = self.quotes.get(token_id) else {
                self.bump("skip_no_pm_quote");
                continue;
            };
            let Some(ask) = quote.ask.and_then(|price| price.to_f64()) else {
                self.bump("skip_no_pm_ask");
                continue;
            };
            let bid = quote.bid.and_then(|price| price.to_f64());
            let quote_age = (now - quote.ts).num_seconds();
            if quote_age > self.config.max_pm_lag_secs as i64 {
                self.bump("skip_stale_pm_quote");
                continue;
            }

            // Layer 2: profile-specific confirmation component
            let lob = self.lob.get(symbol).copied().unwrap_or_default();
            let confirmation_score = profile_confirmation_score(
                direction_sign,
                &lob,
                cum_mprice_drift_5m,
                drift_30s,
                regime,
                &self.config,
            );
            if self.config.require_confirmation
                && !confirmation_gate_passes(
                    confirmation_score,
                    self.config.min_confirmation_score,
                    self.config.cex_contrarian,
                )
            {
                self.bump("skip_confirmation_gate");
                continue;
            }

            let side_spread = bid
                .filter(|bid| bid.is_finite() && ask.is_finite() && ask > 0.0)
                .map(|bid| ((ask - bid).max(0.0)) / ask)
                .unwrap_or(f64::NAN);
            let repricing_score =
                spread_adjusted_external_move_score(drift_30s * direction_sign, side_spread);
            if self.config.profile == ThreeLayerProfile::RepricingMomentum {
                if !repricing_score.is_finite() {
                    self.bump("skip_repricing_score_unavailable");
                    continue;
                }
                if repricing_score < self.config.min_confirmation_score {
                    self.bump("skip_repricing_score");
                    continue;
                }
            }

            let settlement_formula_side_probability = if uses_settlement_autofactor {
                let side_probability = if uses_model_settlement_autofactor {
                    self.runtime_side_model_probability(
                        spot_price,
                        price_to_beat,
                        sigma_h,
                        betting_up,
                    )
                } else {
                    self.runtime_side_fair_probability(event, betting_up, now)
                };
                let Some(side_probability) = side_probability else {
                    self.bump("skip_autofactor_score_unavailable");
                    continue;
                };
                Some(side_probability)
            } else {
                None
            };
            let entry_probability = settlement_formula_side_probability.unwrap_or(effective_p);

            // Layer 3: Edge score
            let (entry_price_f, edge, rr, edge_score) =
                if settlement_formula_side_probability.is_some() {
                    let top_quote_edge = entry_probability - ask - crypto_fee_cost(ask);
                    let top_quote_edge_score = three_layer_model::threshold_score(
                        top_quote_edge,
                        self.config.min_edge.max(0.0),
                        0.08,
                        false,
                    )
                    .clamp(-0.50, 1.0);
                    (ask, top_quote_edge, 0.0, top_quote_edge_score)
                } else {
                    let Some(edge_score) =
                        evaluate_edge_score(entry_probability, ask, regime, &self.config)
                    else {
                        self.bump("skip_edge_score");
                        continue;
                    };
                    edge_score
                };
            if settlement_formula_side_probability.is_none()
                && self.config.profile.uses_snapshot_scoring()
                && rr < self.config.min_reward_risk
            {
                self.bump("skip_reward_risk");
                continue;
            };

            let executable_formula_entry_price_f = if settlement_formula_side_probability.is_some()
            {
                self.bump("settlement_autofactor_depth_check_attempts");
                let Some(entry_price) = Decimal::try_from(entry_price_f).ok() else {
                    self.bump("skip_bad_entry_price");
                    continue;
                };
                let entry_limit_price = Self::sweep_limit_price(
                    entry_price,
                    TradeSide::Buy,
                    self.config.max_sweep_price_delta,
                );
                let quantity = self.entry_quantity(entry_limit_price);
                if quantity <= Decimal::ZERO {
                    self.bump("skip_zero_quantity");
                    continue;
                }
                let Some(depth) = self.quote_depth.get(token_id) else {
                    self.bump("settlement_autofactor_depth_missing");
                    self.bump("skip_no_ask_size");
                    continue;
                };
                let Some(ask_size) = depth.ask_size else {
                    self.bump("settlement_autofactor_depth_missing");
                    self.bump("skip_no_ask_size");
                    continue;
                };
                if depth.ask_levels.is_empty() && ask_size < quantity {
                    self.bump("settlement_autofactor_top_size_insufficient");
                    self.bump("skip_insufficient_ask_size");
                    continue;
                }
                let Some(executable_entry_price) = Self::sweep_fill_price(
                    &depth.ask_levels,
                    quantity,
                    entry_price,
                    entry_limit_price,
                    TradeSide::Buy,
                    self.config.visible_depth_haircut,
                    self.config.max_sweep_levels,
                ) else {
                    self.bump("settlement_autofactor_depth_unfillable");
                    self.bump("skip_insufficient_ask_depth");
                    continue;
                };
                self.bump("settlement_autofactor_depth_fillable");
                executable_entry_price.to_f64().unwrap_or(entry_price_f)
            } else {
                entry_price_f
            };
            let formula_settlement_edge = settlement_formula_side_probability
                .map(|side_probability| {
                    side_probability
                        - executable_formula_entry_price_f
                        - crypto_fee_cost(executable_formula_entry_price_f)
                })
                .unwrap_or(edge);

            let entry_capacity_ratio = self
                .quote_depth
                .get(token_id)
                .and_then(|depth| depth.ask_size)
                .and_then(|ask_size| {
                    let ask_size_f = ask_size.to_f64()?;
                    let stake_usd = self.config.stake_usd.to_f64()?;
                    if executable_formula_entry_price_f > 0.0 && stake_usd.is_finite() {
                        let entry_shares = stake_usd / executable_formula_entry_price_f;
                        (entry_shares > 0.0).then_some(ask_size_f / entry_shares)
                    } else {
                        None
                    }
                })
                .unwrap_or(f64::NAN);
            let pm_momentum_score = self.pm_momentum_score(token_id, ask, now);
            let (autofactor_raw_score, total_score) =
                if let Some(runtime_score) = runtime_score.as_deref() {
                    let inputs = AutoSettlementFactorInputs {
                        settlement_edge: formula_settlement_edge,
                        entry_price: executable_formula_entry_price_f,
                        distance_over_sigma,
                        direction_sign,
                        drift_30s,
                        sigma_horizon: sigma_h,
                        entry_capacity_ratio,
                        side_spread,
                        external_pressure: confirmation_score,
                        pm_lag_secs: quote_age.max(0) as f64,
                        iv_change_1m: 0.0,
                    };
                    let Some((raw, normalized)) =
                        autofactor_formula_entry_score(runtime_score, inputs, self.config.min_edge)
                    else {
                        self.bump("skip_autofactor_score_unavailable");
                        continue;
                    };
                    if uses_settlement_autofactor {
                        self.bump("settlement_autofactor_formula_evaluations");
                        self.bump_settlement_autofactor_edge_bucket(
                            SettlementAutofactorEdgeMetric::TopQuote,
                            edge,
                        );
                        self.bump_settlement_autofactor_edge_bucket(
                            SettlementAutofactorEdgeMetric::Executable,
                            formula_settlement_edge,
                        );
                        self.bump_settlement_autofactor_edge_bucket(
                            SettlementAutofactorEdgeMetric::RawFormula,
                            raw,
                        );
                        if is_predictive_settlement_autofactor_runtime_score(runtime_score) {
                            self.bump_predictive_autofactor_counterfactual_scores(raw, normalized);
                        }
                        if raw >= self.config.min_edge.max(0.0) {
                            self.bump("settlement_autofactor_raw_score_pass_min_edge");
                        } else {
                            self.bump("settlement_autofactor_raw_score_fail_min_edge");
                        }
                        if formula_settlement_edge >= self.config.min_edge {
                            self.bump("settlement_autofactor_executable_edge_pass_min_edge");
                        } else {
                            self.bump("settlement_autofactor_executable_edge_fail_min_edge");
                        }
                    }
                    (Some(raw), normalized)
                } else {
                    (
                        None,
                        evaluate_entry_score(
                            &self.config,
                            EntryScoreInputs {
                                direction_score,
                                distance_over_sigma,
                                direction_sign,
                                edge,
                                edge_score,
                                confirmation: confirmation_score,
                                repricing_score,
                                drift_30s,
                                pm_momentum_score,
                                liquidity_score: 1.0,
                            },
                        ),
                    )
                };

            let entry_score_passes = if let (true, Some(runtime_score)) =
                (uses_settlement_autofactor, runtime_score.as_deref())
            {
                settlement_autofactor_entry_score_passes(
                    runtime_score,
                    autofactor_raw_score,
                    total_score,
                    &self.config,
                )
            } else {
                total_score >= self.config.min_entry_score
            };
            if !entry_score_passes {
                self.bump("skip_entry_score");
                info!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    total_score = format!("{:.3}", total_score),
                    direction_score = format!("{:.3}", direction_score),
                    edge_score = format!("{:.3}", edge_score),
                    confirmation_score = format!("{:.3}", confirmation_score),
                    repricing_score = format!("{:.3}", repricing_score),
                    pm_momentum_score = format!("{:.3}", pm_momentum_score),
                    autofactor_runtime_score = self.config.autofactor_runtime_score.as_deref().unwrap_or(""),
                    autofactor_raw_score = autofactor_raw_score.map(|score| format!("{score:.4}")).unwrap_or_default(),
                    min_entry_score = self.config.min_entry_score,
                    profile = %self.config.profile,
                    regime = regime.as_str(),
                    "score below threshold"
                );
                continue;
            }

            let Some(entry_price) = Decimal::try_from(entry_price_f).ok() else {
                self.bump("skip_bad_entry_price");
                continue;
            };
            let entry_limit_price = Self::sweep_limit_price(
                entry_price,
                TradeSide::Buy,
                self.config.max_sweep_price_delta,
            );
            let quantity = self.entry_quantity(entry_limit_price);
            if quantity <= Decimal::ZERO {
                self.bump("skip_zero_quantity");
                continue;
            }
            let Some(depth) = self.quote_depth.get(token_id) else {
                self.bump("skip_no_ask_size");
                debug!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    token_id = %token_id,
                    quantity = %quantity,
                    "PM quote has no ask size; skipping non-executable entry"
                );
                continue;
            };
            let Some(ask_size) = depth.ask_size else {
                self.bump("skip_no_ask_size");
                debug!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    token_id = %token_id,
                    quantity = %quantity,
                    "PM quote has no ask size; skipping non-executable entry"
                );
                continue;
            };
            if depth.ask_levels.is_empty() && ask_size < quantity {
                self.bump("skip_insufficient_ask_size");
                debug!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    token_id = %token_id,
                    quantity = %quantity,
                    ask_size = %ask_size,
                    "PM ask size cannot fill fixed stake; skipping entry"
                );
                continue;
            }
            let Some(executable_entry_price) = Self::sweep_fill_price(
                &depth.ask_levels,
                quantity,
                entry_price,
                entry_limit_price,
                TradeSide::Buy,
                self.config.visible_depth_haircut,
                self.config.max_sweep_levels,
            ) else {
                self.bump("skip_insufficient_ask_depth");
                debug!(
                    strategy = "three_layer",
                    symbol = %symbol,
                    direction,
                    token_id = %token_id,
                    quantity = %quantity,
                    entry_price = %entry_price,
                    limit_price = %entry_limit_price,
                    "PM ask depth cannot fill fixed stake; skipping entry"
                );
                continue;
            };
            let executable_entry_price_f = executable_entry_price.to_f64().unwrap_or(entry_price_f);
            let settlement_executable_edge = entry_probability
                - executable_entry_price_f
                - crypto_fee_cost(executable_entry_price_f);
            let executable_edge = if uses_predictive_settlement_autofactor {
                autofactor_raw_score.unwrap_or(settlement_executable_edge)
            } else {
                settlement_executable_edge
            };
            if !executable_edge.is_finite() {
                self.bump("skip_edge_score");
                continue;
            }
            if !uses_predictive_settlement_autofactor && executable_edge < self.config.min_edge {
                self.bump("skip_edge_score");
                continue;
            }
            let signal_probability = if uses_predictive_settlement_autofactor {
                (executable_entry_price_f
                    + crypto_fee_cost(executable_entry_price_f)
                    + executable_edge)
                    .clamp(0.01, 0.99)
            } else {
                entry_probability
            };

            let intent_id = format!(
                "tl_{}_{}_{}_{}",
                symbol.to_lowercase(),
                direction.to_lowercase(),
                event.event_id,
                now.timestamp_millis()
            );

            let intent = TradingIntent {
                intent_id: intent_id.clone(),
                deployment_id: String::new(),
                market_id: event.event_id.to_string(),
                token_id: token_id.to_string(),
                side: TradeSide::Buy,
                quantity,
                limit_price: Some(entry_limit_price),
                purpose: IntentPurpose::Entry,
                created_at: now,
            };

            let signal = SignalRecord {
                strategy: self.name().to_string(),
                event_id: Some(event.event_id.to_string()),
                token_id: Some(token_id.to_string()),
                intent_id: Some(intent_id),
                symbol: symbol.to_string(),
                direction: direction.to_string(),
                p_hat: signal_probability,
                edge: executable_edge,
                entry_price: executable_entry_price,
                decision: "enter".to_string(),
                ts: now,
            };

            info!(
                strategy = "three_layer",
                symbol = %symbol,
                direction,
                total_score = format!("{:.3}", total_score),
                direction_score = format!("{:.3}", direction_score),
                edge_score = format!("{:.3}", edge_score),
                confirmation_score = format!("{:.3}", confirmation_score),
                repricing_score = format!("{:.3}", repricing_score),
                pm_momentum_score = format!("{:.3}", pm_momentum_score),
                autofactor_runtime_score = self.config.autofactor_runtime_score.as_deref().unwrap_or(""),
                autofactor_raw_score = autofactor_raw_score.map(|score| format!("{score:.4}")).unwrap_or_default(),
                p_hat = signal_probability,
                edge = executable_edge,
                entry_price = %executable_entry_price,
                limit_price = %entry_limit_price,
                profile = %self.config.profile,
                regime = regime.as_str(),
                "entry signal"
            );

            self.bump("entry_signals");
            return Some(StrategyDecision::Enter {
                intent,
                signal: Some(signal),
            });
        }
        None
    }

    fn exit_decisions_for_symbol(
        &mut self,
        symbol: &str,
        now: DateTime<Utc>,
        spot: Option<f64>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        let mut decisions = Vec::new();

        let events = self.events.get(symbol).cloned().unwrap_or_default();
        for event in events {
            for (token_id, is_up) in [(&event.up_token, true), (&event.down_token, false)] {
                let qty = positions.net_qty(token_id);
                if qty <= Decimal::ZERO {
                    continue;
                }
                if Self::active_order_exists(orders, token_id) {
                    continue;
                }
                if self.token_reject_active(token_id, now) {
                    continue;
                }
                let Some(exit_bid) = self.quotes.get(token_id).and_then(|q| q.bid) else {
                    continue;
                };
                let Some(depth) = self.quote_depth.get(token_id) else {
                    debug!(
                        strategy = "three_layer",
                        token_id = %token_id,
                        quantity = %qty,
                        "PM quote has no bid size; skipping non-executable exit"
                    );
                    continue;
                };
                let Some(bid_size) = depth.bid_size else {
                    debug!(
                        strategy = "three_layer",
                        token_id = %token_id,
                        quantity = %qty,
                        "PM quote has no bid size; skipping non-executable exit"
                    );
                    continue;
                };
                if depth.bid_levels.is_empty() && bid_size < qty {
                    self.bump("skip_insufficient_bid_size");
                    debug!(
                        strategy = "three_layer",
                        token_id = %token_id,
                        quantity = %qty,
                        bid_size = %bid_size,
                        "PM bid size cannot fill current position; skipping exit"
                    );
                    continue;
                }
                let exit_limit_price = Self::sweep_limit_price(
                    exit_bid,
                    TradeSide::Sell,
                    self.config.max_sweep_price_delta,
                );
                let Some(executable_exit_price) = Self::sweep_fill_price(
                    &depth.bid_levels,
                    qty,
                    exit_bid,
                    exit_limit_price,
                    TradeSide::Sell,
                    self.config.visible_depth_haircut,
                    self.config.max_sweep_levels,
                ) else {
                    self.bump("skip_insufficient_bid_depth");
                    debug!(
                        strategy = "three_layer",
                        token_id = %token_id,
                        quantity = %qty,
                        exit_bid = %exit_bid,
                        limit_price = %exit_limit_price,
                        "PM bid depth cannot fill current position; skipping exit"
                    );
                    continue;
                };

                // Take profit must be executable: SELL can only hit the bid.
                // The config field keeps its historical name for TOML compatibility.
                if let Some(bid) = executable_exit_price.to_f64() {
                    if bid >= self.config.take_profit_ask {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!("tl_tp_{}_{}", token_id, now.timestamp_millis()),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_limit_price),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                        continue;
                    }
                }

                // Stop loss
                if let (Some(price_to_beat), Some(spot_price)) =
                    (event.price_to_beat.and_then(|v| v.to_f64()), spot)
                {
                    let dist = (spot_price - price_to_beat) / price_to_beat;
                    let wrong_direction = if is_up {
                        dist < -self.config.stop_distance_pct
                    } else {
                        dist > self.config.stop_distance_pct
                    };
                    if wrong_direction {
                        decisions.push(StrategyDecision::Exit(TradingIntent {
                            intent_id: format!("tl_sl_{}_{}", token_id, now.timestamp_millis()),
                            deployment_id: String::new(),
                            market_id: event.event_id.to_string(),
                            token_id: token_id.to_string(),
                            side: TradeSide::Sell,
                            quantity: qty,
                            limit_price: Some(exit_limit_price),
                            purpose: IntentPurpose::Exit,
                            created_at: now,
                        }));
                    }
                }
            }
        }

        decisions
    }

    fn build_settlement_exits(
        &self,
        event: &EventWindow,
        up_won: bool,
        now: DateTime<Utc>,
        positions: &PositionLedger,
        orders: &OrderLedger,
    ) -> Vec<StrategyDecision> {
        let mut exits = Vec::new();

        if positions.net_qty(&event.up_token) > Decimal::ZERO {
            if Self::active_order_exists(orders, &event.up_token) {
                debug!(
                    token_id = %event.up_token,
                    "Active order exists, skipping settlement exit"
                );
            } else if self.token_reject_active(&event.up_token, now) {
                debug!(
                    token_id = %event.up_token,
                    "Token reject cooldown active, skipping settlement exit"
                );
            } else {
                exits.push(StrategyDecision::Exit(TradingIntent {
                    intent_id: format!("tl_settle_{}_up", event.event_id),
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
                    created_at: now,
                }));
            }
        }

        if positions.net_qty(&event.down_token) > Decimal::ZERO {
            if Self::active_order_exists(orders, &event.down_token) {
                debug!(
                    token_id = %event.down_token,
                    "Active order exists, skipping settlement exit"
                );
            } else if self.token_reject_active(&event.down_token, now) {
                debug!(
                    token_id = %event.down_token,
                    "Token reject cooldown active, skipping settlement exit"
                );
            } else {
                exits.push(StrategyDecision::Exit(TradingIntent {
                    intent_id: format!("tl_settle_{}_down", event.event_id),
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
                    created_at: now,
                }));
            }
        }

        exits
    }

    fn resolve_up_won(&self, event: &EventWindow, resolved: Option<bool>) -> Option<bool> {
        if resolved.is_none() {
            debug!(
                strategy = "three_layer",
                event_id = %event.event_id,
                "Skipping settlement exit without official resolution"
            );
        }
        resolved
    }
}

// ── StrategyLogic impl ────────────────────────────────────────────

impl StrategyLogic for ThreeLayerStrategy {
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
                self.feed_time = Some(*ts);
                self.reset_daily_counter(*ts);

                let price_f64 = match price.to_f64() {
                    Some(p) if p > 0.0 => p,
                    _ => {
                        self.spot.remove(symbol);
                        self.drift.remove(symbol);
                        self.drift_state.remove(symbol);
                        self.lob.remove(symbol);
                        self.mprice_acc.remove(symbol);
                        return Vec::new();
                    }
                };

                self.drift
                    .entry(symbol.clone())
                    .or_insert_with(DriftTracker::new)
                    .push(*ts, price_f64);
                let drift_30s_speed = self
                    .drift
                    .get(symbol)
                    .map(|tracker| tracker.drift_speed_since_secs(30))
                    .unwrap_or(0.0);
                self.drift_state
                    .entry(symbol.clone())
                    .or_default()
                    .update(drift_30s_speed, *ts);
                self.spot.insert(symbol.clone(), *price);

                // Set price_to_beat for events that don't have one yet
                if let Some(events) = self.events.get_mut(symbol) {
                    for event in events.iter_mut() {
                        if event.price_to_beat.is_none() {
                            event.price_to_beat = Some(*price);
                        }
                    }
                }

                // Check exits first
                let spot_opt = Some(price_f64);
                let mut decisions =
                    self.exit_decisions_for_symbol(symbol, *ts, spot_opt, positions, orders);

                if let Some(entry) = self.maybe_entry_for_symbol(symbol, *ts, positions, orders) {
                    decisions.push(entry);
                }
                decisions
            }

            MarketUpdate::Quote {
                token_id,
                bid,
                ask,
                bid_size,
                ask_size,
                bid_levels,
                ask_levels,
                ts,
            } => {
                if self.quotes.get(token_id.as_ref()).is_some_and(|current| {
                    current.ts > *ts
                        && (current.bid.is_some() || current.ask.is_some())
                        && (bid.is_some() || ask.is_some())
                }) {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.quotes.insert(
                    token_id.clone(),
                    QuoteState {
                        bid: *bid,
                        ask: *ask,
                        ts: *ts,
                    },
                );
                self.quote_depth.insert(
                    token_id.clone(),
                    QuoteDepth {
                        bid_size: *bid_size,
                        ask_size: *ask_size,
                        bid_levels: bid_levels.clone(),
                        ask_levels: ask_levels.clone(),
                    },
                );
                self.record_quote_ask(token_id.clone(), *ask, *ts);

                let Some(symbol) = self.token_symbol.get(token_id).cloned() else {
                    return Vec::new();
                };
                let spot = self.spot.get(&symbol).and_then(|p| p.to_f64());
                let mut decisions =
                    self.exit_decisions_for_symbol(&symbol, *ts, spot, positions, orders);
                if let Some(entry) = self.maybe_entry_for_symbol(&symbol, *ts, positions, orders) {
                    decisions.push(entry);
                }
                decisions
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
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                let qty_f64 = quantity.to_f64().unwrap_or(0.0);
                self.lob.entry(symbol.clone()).or_default().apply_aggtrade(
                    qty_f64,
                    *is_buyer_maker,
                    *ts,
                );
                self.maybe_entry_for_symbol(symbol, *ts, positions, orders)
                    .into_iter()
                    .collect()
            }

            MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ts,
            } => {
                if !self
                    .config
                    .symbols
                    .iter()
                    .any(|s| s.as_str() == symbol.as_ref())
                {
                    return Vec::new();
                }
                self.feed_time = Some(*ts);
                self.lob.entry(symbol.clone()).or_default().apply_l2(
                    *obi,
                    *spread_bps,
                    *bid_depth_near,
                    *ask_depth_near,
                    *ts,
                );

                let mid = (bid_depth_near + ask_depth_near) / 2.0;
                if mid > 0.0 {
                    let microprice_offset = (bid_depth_near - ask_depth_near) / mid;
                    self.mprice_acc
                        .entry(symbol.clone())
                        .or_insert_with(|| MpriceDriftAccumulator::new(300.0))
                        .push(*ts, microprice_offset);
                }
                self.maybe_entry_for_symbol(symbol, *ts, positions, orders)
                    .into_iter()
                    .collect()
            }

            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts,
            } => {
                if let Some(lob) = self.lob.get_mut(symbol) {
                    lob.obi_prev = lob.obi;
                    // Same EWMA smoothing as L2Depth path
                    if let Some(prev_ts) = lob.ts {
                        let dt = (*ts - prev_ts).num_milliseconds() as f64 / 1000.0;
                        if dt > 0.0 && dt < 60.0 {
                            let alpha = 1.0 - (-dt / 5.0_f64).exp();
                            lob.obi = lob.obi * (1.0 - alpha) + obi * alpha;
                        } else {
                            lob.obi = *obi;
                        }
                    } else {
                        lob.obi = *obi;
                    }
                    lob.spread_bps = *spread_bps;
                    lob.ts = Some(*ts);
                }
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
                            decisions.extend(self.build_settlement_exits(
                                event, up_won, *end_time, positions, orders,
                            ));
                            resolved_events.push(event.event_id.clone());
                        }
                    }
                }

                if !resolved_events.is_empty() {
                    for events in self.events.values_mut() {
                        events.retain(|e| !resolved_events.contains(&e.event_id));
                    }
                }

                decisions
            }

            _ => Vec::new(),
        }
    }

    fn on_fill(&mut self, fill: &FillRecord) {
        if fill.side == TradeSide::Buy {
            if let Some(symbol) = self.token_symbol.get(fill.token_id.as_str()).cloned() {
                self.last_entry.insert(symbol, fill.timestamp);
            }
            self.daily_trade_count += 1;
        }
    }

    fn on_reject(&mut self, intent: &TradingIntent, reason: &str) {
        let now = self.now();
        let reason_lc = reason.to_ascii_lowercase();
        // Reject-string provenance:
        // - Polymarket CLOB/live executor: "not enough balance", "not enough allowance",
        //   "no orders found to match with FAK order", "insufficient liquidity", "no match".
        // - Venue precision/min-size rejects observed in live records: "invalid amount(s)".
        // - Local engine dust guard: "below retry threshold".
        let balance_or_allowance = reason_lc.contains("not enough balance")
            || reason_lc.contains("not enough allowance")
            || (reason_lc.contains("not enough") && reason_lc.contains("allowance"))
            || (reason_lc.contains("insufficient") && reason_lc.contains("allowance"));
        let invalid_amount =
            reason_lc.contains("invalid amount") || reason_lc.contains("below retry threshold");
        let no_liquidity = reason_lc.contains("no orders found")
            || reason_lc.contains("insufficient liquidity")
            || reason_lc.contains("no full-depth liquidity")
            || reason_lc.contains("insufficient full-depth liquidity")
            || reason_lc.contains("no match");
        let token_cooldown_secs = if balance_or_allowance || invalid_amount {
            HARD_TOKEN_REJECT_PAUSE_SECS
        } else if no_liquidity {
            NO_LIQUIDITY_REJECT_PAUSE_SECS
        } else {
            self.reject_cooldown_secs()
        };
        let token_until = now + chrono::Duration::seconds(token_cooldown_secs);

        if balance_or_allowance && intent.side == TradeSide::Buy {
            self.set_balance_exhausted_until(
                now + chrono::Duration::seconds(ACCOUNT_BALANCE_REJECT_PAUSE_SECS),
            );
        }

        self.set_token_reject_until(&intent.token_id, token_until);

        if intent.side == TradeSide::Buy {
            if let Some(symbol) = self.token_symbol.get(intent.token_id.as_str()).cloned() {
                self.last_entry.insert(symbol, now);
            }
        }

        warn!(
            strategy = self.name(),
            intent_id = %intent.intent_id,
            token_id = %intent.token_id,
            reason = %reason,
            cooldown_secs = token_cooldown_secs,
            "Order rejected; suppressing duplicate live intents during cooldown"
        );
    }

    fn name(&self) -> &str {
        "three_layer"
    }

    fn diagnostics(&self) -> Vec<(String, u64)> {
        let mut diagnostics = self
            .diagnostics
            .iter()
            .map(|(key, value)| ((*key).to_string(), *value))
            .collect::<Vec<_>>();
        diagnostics.sort_by(|a, b| a.0.cmp(&b.0));
        diagnostics
    }
}

#[cfg(test)]
mod tests {
    use super::super::event_ml_model::{
        EventMlFeatureStandardizer, EventMlFeatureWeight, EventMlStandardizer,
    };
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn regime_from_secs_boundaries() {
        assert_eq!(Regime::from_secs(300), Regime::Early);
        assert_eq!(Regime::from_secs(181), Regime::Early);
        assert_eq!(Regime::from_secs(180), Regime::Middle);
        assert_eq!(Regime::from_secs(61), Regime::Middle);
        assert_eq!(Regime::from_secs(60), Regime::Late);
        assert_eq!(Regime::from_secs(6), Regime::Late);
        assert_eq!(Regime::from_secs(5), Regime::Expiry);
        assert_eq!(Regime::from_secs(0), Regime::Expiry);
    }

    #[test]
    fn config_from_directional_preserves_fields() {
        let dc: DirectionalConfig = serde_json::from_str("{}").unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::Mixed);
        assert_eq!(tlc.min_edge, 0.03);
        assert_eq!(tlc.min_reward_risk, 1.2);
        assert_eq!(tlc.take_profit_ask, 0.70);
        assert!((tlc.min_entry_score - 0.30).abs() < f64::EPSILON);
        assert!(!tlc.symbols.is_empty());
    }

    #[test]
    fn config_from_directional_accepts_profile_aliases() {
        let dc: DirectionalConfig = serde_json::from_str(r#"{"strategy_profile":"obi"}"#).unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::ObiSoft);

        let dc: DirectionalConfig =
            serde_json::from_str(r#"{"strategy_profile":"obi_hard"}"#).unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::ObiHard);

        let dc: DirectionalConfig =
            serde_json::from_str(r#"{"strategy_profile":"repricing_momentum"}"#).unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::RepricingMomentum);

        let dc: DirectionalConfig =
            serde_json::from_str(r#"{"strategy_profile":"settlement_probability"}"#).unwrap();
        let tlc: ThreeLayerConfig = dc.into();
        assert_eq!(tlc.profile, ThreeLayerProfile::SettlementProbability);
    }

    #[test]
    fn event_ml_directional_runtime_requires_model_path() {
        let dc: DirectionalConfig = serde_json::from_str(
            r#"{"three_layer_autofactor_runtime_score":"event_ml_model:baseline_v1"}"#,
        )
        .unwrap();

        let err = ThreeLayerConfig::from_directional_runtime(dc).expect_err("missing model path");

        assert!(err.contains("three_layer_event_ml_model_path"));
    }

    #[test]
    fn event_ml_directional_runtime_loads_model_artifact() {
        let path = std::env::temp_dir().join(format!(
            "ploy-event-ml-baseline-{}-{}.json",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let artifact = serde_json::json!({
            "model": {
                "kind": "event_ml_logistic_baseline_model",
                "version": 1,
                "family": "logistic_regression",
                "target_label": "settlement_up",
                "feature_schema": ["pm_up_ask"],
                "intercept": 0.25,
                "weights": [{"feature": "pm_up_ask", "weight": -1.0}],
                "standardizer": {
                    "method": "zscore",
                    "fit_split": "train",
                    "features": [{"feature": "pm_up_ask", "mean": 0.5, "std": 0.2}]
                }
            }
        });
        std::fs::write(&path, artifact.to_string()).unwrap();
        let config_json = serde_json::json!({
            "three_layer_autofactor_runtime_score": "event_ml_model:baseline_v1",
            "three_layer_event_ml_model_path": path,
        })
        .to_string();
        let dc: DirectionalConfig = serde_json::from_str(&config_json).unwrap();

        let tlc = ThreeLayerConfig::from_directional_runtime(dc).expect("load Event ML model");

        assert!(tlc.event_ml_model.is_some());
        assert!(tlc.uses_event_ml_model());
        let _ = std::fs::remove_file(
            serde_json::from_str::<serde_json::Value>(&config_json)
                .unwrap()
                .get("three_layer_event_ml_model_path")
                .and_then(serde_json::Value::as_str)
                .unwrap(),
        );
    }

    #[test]
    fn mprice_drift_accumulator_evicts_old_entries() {
        use chrono::TimeZone;
        let mut acc = MpriceDriftAccumulator::new(300.0);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        acc.push(t0, 1.5);
        acc.push(t0 + chrono::Duration::seconds(100), 2.0);
        acc.push(t0 + chrono::Duration::seconds(301), 3.0);
        assert!((acc.cum_drift() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn drift_tracker_detects_direction() {
        use chrono::TimeZone;
        let mut tracker = DriftTracker::new();
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        for i in 0..30 {
            let t = t0 + chrono::Duration::seconds(i);
            let price = 50000.0 + i as f64 * 10.0;
            tracker.push(t, price);
        }
        assert!(tracker.drift_30s() > 0.0);
    }

    #[test]
    fn diagnostics_count_entry_gate_reasons() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());

        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &PositionLedger::default(),
            &OrderLedger::default(),
        );

        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("entry_evaluations"), Some(&1));
        assert_eq!(diagnostics.get("skip_no_candidate_events"), Some(&1));
    }

    #[test]
    fn max_daily_trades_zero_disables_daily_cap() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut config = test_config();
        config.max_daily_trades = 0;
        let mut strategy = ThreeLayerStrategy::new(config);
        strategy.daily_trade_count = 10_000;
        strategy.spot.insert(Arc::from("BTCUSDT"), dec!(100000));

        let decision = strategy.try_entry(
            "BTCUSDT",
            now,
            &PositionLedger::default(),
            &OrderLedger::default(),
        );

        assert!(decision.is_none());
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("entry_evaluations"), Some(&1));
        assert_eq!(diagnostics.get("skip_no_candidate_events"), Some(&1));
        assert_eq!(diagnostics.get("skip_max_daily_trades"), None);
    }

    #[test]
    fn positive_max_daily_trades_still_blocks_entries() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut config = test_config();
        config.max_daily_trades = 1;
        let mut strategy = ThreeLayerStrategy::new(config);
        strategy.daily_trade_count = 1;

        let decision = strategy.try_entry(
            "BTCUSDT",
            now,
            &PositionLedger::default(),
            &OrderLedger::default(),
        );

        assert!(decision.is_none());
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("entry_evaluations"), Some(&1));
        assert_eq!(diagnostics.get("skip_max_daily_trades"), Some(&1));
    }

    fn test_config() -> ThreeLayerConfig {
        ThreeLayerConfig {
            symbols: vec!["BTCUSDT".into()],
            profile: ThreeLayerProfile::Mixed,
            min_direction_prob: 0.56,
            min_distance_over_sigma: 0.3,
            min_confirmation_score: 0.10,
            require_confirmation: false,
            min_drift_confirmation: 0.0002,
            min_edge: 0.03,
            min_reward_risk: 1.2,
            alpha_contrarian: false,
            cex_contrarian: false,
            probability_shrink: 1.0,
            probability_haircut: 0.0,
            take_profit_ask: 0.70,
            stop_distance_pct: 0.020,
            max_pm_lag_secs: 15,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            cooldown_secs: 0,
            stake_usd: Decimal::new(25, 0),
            max_positions: 10,
            max_daily_trades: 100,
            allowed_window_secs: vec![300],
            min_entry_price: 0.15,
            max_entry_price: 0.85,
            min_entry_score: 0.30,
            autofactor_runtime_score: None,
            event_ml_model: None,
            visible_depth_haircut: Decimal::ONE,
            max_sweep_levels: 0,
            max_sweep_price_delta: Decimal::ZERO,
        }
    }

    fn event_ml_model_with_intercept(intercept: f64) -> EventMlModelContract {
        EventMlModelContract {
            kind: "event_ml_logistic_baseline_model".to_string(),
            version: 1,
            family: "logistic_regression".to_string(),
            target_label: "settlement_up".to_string(),
            feature_schema: Vec::new(),
            intercept,
            weights: Vec::new(),
            standardizer: EventMlStandardizer {
                method: "zscore".to_string(),
                fit_split: "train".to_string(),
                features: Vec::new(),
            },
        }
    }

    fn event_ml_model_with_feature(feature: &str) -> EventMlModelContract {
        EventMlModelContract {
            kind: "event_ml_logistic_baseline_model".to_string(),
            version: 1,
            family: "logistic_regression".to_string(),
            target_label: "settlement_up".to_string(),
            feature_schema: vec![feature.to_string()],
            intercept: 0.0,
            weights: vec![EventMlFeatureWeight {
                feature: feature.to_string(),
                weight: 1.0,
            }],
            standardizer: EventMlStandardizer {
                method: "zscore".to_string(),
                fit_split: "train".to_string(),
                features: vec![EventMlFeatureStandardizer {
                    feature: feature.to_string(),
                    mean: 0.0,
                    std: 1.0,
                }],
            },
        }
    }

    fn discover_test_event(strategy: &mut ThreeLayerStrategy, now: DateTime<Utc>) {
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        strategy.on_update(
            &MarketUpdate::EventDiscovered {
                event_id: Arc::from("evt1"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("token-up"),
                down_token: Arc::from("token-down"),
                end_time: now + chrono::Duration::seconds(300),
                window_secs: 300,
                price_to_beat: Some(dec!(100000)),
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );
    }

    fn position_with_token(token_id: &str, now: DateTime<Utc>) -> PositionLedger {
        let mut positions = PositionLedger::default();
        positions.apply_fill(&FillRecord {
            fill_id: format!("fill-{token_id}"),
            order_id: format!("order-{token_id}"),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            price: dec!(0.45),
            fee: Decimal::ZERO,
            timestamp: now,
        });
        positions
    }

    fn active_order_for_token(token_id: &str, now: DateTime<Utc>) -> OrderLedger {
        let mut orders = OrderLedger::default();
        let intent = TradingIntent {
            intent_id: format!("intent-{token_id}"),
            deployment_id: "three-layer-live".into(),
            market_id: "evt1".into(),
            token_id: token_id.to_string(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            limit_price: Some(dec!(0.72)),
            purpose: IntentPurpose::Exit,
            created_at: now,
        };
        orders.insert_from_intent(format!("order-{token_id}"), &intent);
        orders.acknowledge(&format!("order-{token_id}"), format!("venue-{token_id}"));
        orders
    }

    fn take_profit_quote(token_id: &str, ts: DateTime<Utc>) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: Some(dec!(0.72)),
            ask: Some(dec!(0.75)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts,
        }
    }

    fn level(price: Decimal, size: Decimal) -> BookLevel {
        BookLevel { price, size }
    }

    fn take_profit_quote_with_bid_levels(
        token_id: &str,
        ts: DateTime<Utc>,
        bid_levels: Vec<BookLevel>,
    ) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: Some(dec!(0.72)),
            ask: Some(dec!(0.75)),
            bid_size: Some(dec!(100)),
            ask_size: Some(dec!(100)),
            bid_levels,
            ask_levels: Vec::new(),
            ts,
        }
    }

    fn entry_quote(token_id: &str, ts: DateTime<Utc>, ask_size: Option<Decimal>) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: Some(dec!(0.19)),
            ask: Some(dec!(0.20)),
            bid_size: Some(dec!(100)),
            ask_size,
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts,
        }
    }

    fn entry_quote_with_ask_levels(
        token_id: &str,
        ts: DateTime<Utc>,
        ask_size: Option<Decimal>,
        ask_levels: Vec<BookLevel>,
    ) -> MarketUpdate {
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: Some(dec!(0.19)),
            ask: Some(dec!(0.20)),
            bid_size: Some(dec!(100)),
            ask_size,
            bid_levels: Vec::new(),
            ask_levels,
            ts,
        }
    }

    fn rejected_exit_intent(token_id: &str, now: DateTime<Utc>) -> TradingIntent {
        TradingIntent {
            intent_id: format!("reject-{token_id}"),
            deployment_id: "three-layer-live".into(),
            market_id: "evt1".into(),
            token_id: token_id.to_string(),
            side: TradeSide::Sell,
            quantity: dec!(10),
            limit_price: Some(dec!(0.72)),
            purpose: IntentPurpose::Exit,
            created_at: now,
        }
    }

    fn rejected_buy_intent(token_id: &str, now: DateTime<Utc>) -> TradingIntent {
        TradingIntent {
            intent_id: format!("buy-reject-{token_id}"),
            deployment_id: "three-layer-live".into(),
            market_id: "evt1".into(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: dec!(10),
            limit_price: Some(dec!(0.72)),
            purpose: IntentPurpose::Entry,
            created_at: now,
        }
    }

    #[test]
    fn reject_cooldown_suppresses_duplicate_live_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let first_decisions =
            strategy.on_update(&take_profit_quote("token-down", now), &positions, &orders);
        let exit_intent = match first_decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => intent.clone(),
            other => panic!("expected one take-profit exit, got {other:?}"),
        };

        strategy.on_reject(
            &exit_intent,
            "not enough balance / allowance: available balance is lower than order size",
        );
        let next_decisions = strategy.on_update(
            &take_profit_quote("token-down", now + chrono::Duration::seconds(1)),
            &positions,
            &orders,
        );

        assert!(
            next_decisions.is_empty(),
            "rejected token should not emit duplicate live exit"
        );
    }

    #[test]
    fn reject_cooldown_preserves_longer_existing_pause() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        strategy.feed_time = Some(now);
        let intent = rejected_exit_intent("token-down", now);

        strategy.on_reject(&intent, "invalid amounts");
        let first_token_until = strategy
            .token_reject_until
            .get("token-down")
            .copied()
            .expect("token cooldown");

        strategy.feed_time = Some(now + chrono::Duration::seconds(10));
        strategy.on_reject(&intent, "no orders found to match with FAK order");

        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            first_token_until
        );
        assert!(strategy.balance_exhausted_until.is_none());
    }

    #[test]
    fn reject_cooldown_classifies_observed_live_reasons() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let intent = rejected_exit_intent("token-down", now);

        strategy.feed_time = Some(now);
        strategy.on_reject(&intent, "invalid amounts");
        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(HARD_TOKEN_REJECT_PAUSE_SECS)
        );
        assert!(strategy.balance_exhausted_until.is_none());

        strategy.token_reject_until.clear();
        strategy.feed_time = Some(now);
        strategy.on_reject(&intent, "no orders found to match with FAK order");
        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(NO_LIQUIDITY_REJECT_PAUSE_SECS)
        );

        strategy.token_reject_until.clear();
        strategy.feed_time = Some(now);
        strategy.on_reject(
            &intent,
            "not enough allowance: allowance is lower than order size",
        );
        assert_eq!(
            strategy
                .token_reject_until
                .get("token-down")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(HARD_TOKEN_REJECT_PAUSE_SECS)
        );
        assert!(strategy.balance_exhausted_until.is_none());
    }

    #[test]
    fn buy_balance_reject_uses_short_account_pause() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let intent = rejected_buy_intent("token-up", now);

        strategy.feed_time = Some(now);
        strategy.on_reject(
            &intent,
            "not enough balance: available balance is lower than order size",
        );

        assert_eq!(
            strategy
                .token_reject_until
                .get("token-up")
                .copied()
                .expect("token cooldown"),
            now + chrono::Duration::seconds(HARD_TOKEN_REJECT_PAUSE_SECS)
        );
        assert_eq!(
            strategy.balance_exhausted_until.expect("balance pause"),
            now + chrono::Duration::seconds(ACCOUNT_BALANCE_REJECT_PAUSE_SECS)
        );
    }

    #[test]
    fn active_order_suppresses_duplicate_live_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = active_order_for_token("token-down", now);

        let decisions =
            strategy.on_update(&take_profit_quote("token-down", now), &positions, &orders);

        assert!(
            decisions.is_empty(),
            "active order should gate duplicate live exit"
        );
    }

    #[test]
    fn take_profit_requires_executable_bid_not_ask_only() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.62)),
                ask: Some(dec!(0.75)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "a high ask alone is not an executable take-profit sell"
        );
    }

    #[test]
    fn older_quote_cannot_overwrite_newer_ws_tick() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 7, 13, 9, 0, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let quote = |ask, ts| MarketUpdate::Quote {
            token_id: Arc::from("token-up"),
            bid: Some(ask - dec!(0.01)),
            ask: Some(ask),
            bid_size: Some(dec!(10)),
            ask_size: Some(dec!(10)),
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts,
        };

        strategy.on_update(&quote(dec!(0.60), now), &positions, &orders);
        strategy.on_update(
            &quote(dec!(0.40), now - chrono::Duration::seconds(1)),
            &positions,
            &orders,
        );

        assert_eq!(strategy.quotes["token-up"].ask, Some(dec!(0.60)));
        assert_eq!(strategy.quotes["token-up"].ts, now);
    }

    #[test]
    fn wire_quote_recovers_after_local_disconnect_marker() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 7, 13, 9, 0, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        let quote = |bid, ask, ts| MarketUpdate::Quote {
            token_id: Arc::from("token-up"),
            bid,
            ask,
            bid_size: bid.map(|_| dec!(10)),
            ask_size: ask.map(|_| dec!(10)),
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts,
        };

        strategy.on_update(
            &quote(Some(dec!(0.59)), Some(dec!(0.60)), now),
            &positions,
            &orders,
        );
        strategy.on_update(
            &quote(None, None, now + chrono::Duration::seconds(1)),
            &positions,
            &orders,
        );
        strategy.on_update(
            &quote(
                Some(dec!(0.54)),
                Some(dec!(0.55)),
                now + chrono::Duration::milliseconds(500),
            ),
            &positions,
            &orders,
        );

        assert_eq!(strategy.quotes["token-up"].ask, Some(dec!(0.55)));
    }

    #[test]
    fn empty_quote_tick_clears_cached_execution_depth() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 7, 13, 9, 0, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.49)),
                ask: Some(dec!(0.50)),
                bid_size: Some(dec!(10)),
                ask_size: Some(dec!(20)),
                bid_levels: vec![level(dec!(0.49), dec!(10))],
                ask_levels: vec![level(dec!(0.50), dec!(20))],
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: None,
                ask: None,
                bid_size: None,
                ask_size: None,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now + chrono::Duration::milliseconds(1),
            },
            &positions,
            &orders,
        );

        let depth = &strategy.quote_depth["token-up"];
        assert_eq!(depth.bid_size, None);
        assert_eq!(depth.ask_size, None);
        assert!(depth.bid_levels.is_empty());
        assert!(depth.ask_levels.is_empty());
    }

    #[test]
    fn take_profit_requires_bid_size_for_position_quantity() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.72)),
                ask: Some(dec!(0.75)),
                bid_size: Some(dec!(5)),
                ask_size: Some(dec!(100)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "sell exits must not be emitted when top bid cannot fill the position"
        );
    }

    #[test]
    fn take_profit_requires_full_bid_depth_for_position_quantity() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &take_profit_quote_with_bid_levels("token-down", now, vec![level(dec!(0.72), dec!(5))]),
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "sell exits must not be emitted when full-depth bid cannot fill the position"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_bid_depth"), Some(&1));
    }

    #[test]
    fn take_profit_emits_when_full_bid_depth_covers_position_quantity() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &take_profit_quote_with_bid_levels(
                "token-down",
                now,
                vec![level(dec!(0.72), dec!(6)), level(dec!(0.72), dec!(5))],
            ),
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.side, TradeSide::Sell);
                assert_eq!(intent.limit_price, Some(dec!(0.72)));
                assert_eq!(intent.quantity, dec!(10));
            }
            other => panic!("expected one executable exit, got {other:?}"),
        }
    }

    #[test]
    fn take_profit_allows_small_multi_level_sweep_delta() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.max_sweep_levels = 3;
        config.max_sweep_price_delta = dec!(0.003);
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &take_profit_quote_with_bid_levels(
                "token-down",
                now,
                vec![level(dec!(0.72), dec!(5)), level(dec!(0.718), dec!(5))],
            ),
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Exit(intent)] => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.side, TradeSide::Sell);
                assert_eq!(intent.limit_price, Some(dec!(0.717)));
                assert_eq!(intent.quantity, dec!(10));
            }
            other => panic!("expected one executable swept exit, got {other:?}"),
        }
    }

    #[test]
    fn take_profit_rejects_large_sweep_price_jump() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.max_sweep_levels = 3;
        config.max_sweep_price_delta = dec!(0.003);
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &take_profit_quote_with_bid_levels(
                "token-down",
                now,
                vec![level(dec!(0.72), dec!(5)), level(dec!(0.716), dec!(5))],
            ),
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "exit must reject deeper bid levels outside the configured sweep delta"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_bid_depth"), Some(&1));
    }

    #[test]
    fn take_profit_depth_gate_applies_haircut_and_sweep_level_limit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.visible_depth_haircut = dec!(0.5);
        config.max_sweep_levels = 1;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &take_profit_quote_with_bid_levels(
                "token-down",
                now,
                vec![level(dec!(0.72), dec!(10)), level(dec!(0.72), dec!(20))],
            ),
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "exit depth gate must match executor haircut and max sweep levels"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_bid_depth"), Some(&1));
    }

    #[test]
    fn entry_requires_ask_size_for_fixed_stake() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(25))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry should not fire when ask size cannot fill the fixed stake"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_ask_size"), Some(&1));
    }

    #[test]
    fn entry_requires_full_ask_depth_for_fixed_stake() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote_with_ask_levels(
                "token-up",
                now,
                Some(dec!(200)),
                vec![level(dec!(0.20), dec!(25))],
            ),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry should not fire when full-depth ask cannot fill the fixed stake"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_ask_depth"), Some(&1));
    }

    #[test]
    fn entry_depth_gate_applies_haircut_and_sweep_level_limit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        config.visible_depth_haircut = dec!(0.5);
        config.max_sweep_levels = 1;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote_with_ask_levels(
                "token-up",
                now,
                Some(dec!(200)),
                vec![level(dec!(0.20), dec!(125)), level(dec!(0.20), dec!(200))],
            ),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry depth gate must match executor haircut and max sweep levels"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_ask_depth"), Some(&1));
    }

    #[test]
    fn entry_requires_known_ask_size() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(&entry_quote("token-up", now, None), &positions, &orders);
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry should not fire when quote size is unavailable"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_no_ask_size"), Some(&1));
    }

    #[test]
    fn entry_emits_when_ask_size_covers_fixed_stake() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, .. }] => {
                assert_eq!(intent.token_id, "token-up");
                assert_eq!(intent.side, TradeSide::Buy);
                assert_eq!(intent.limit_price, Some(dec!(0.20)));
                assert_eq!(intent.quantity, dec!(125));
            }
            other => panic!("expected one executable entry, got {other:?}"),
        }
    }

    #[test]
    fn fresh_quote_tick_triggers_entry_without_waiting_for_next_spot_tick() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        let initial = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        assert!(initial.is_empty());

        let decisions = strategy.on_update(
            &entry_quote(
                "token-up",
                now + chrono::Duration::milliseconds(1),
                Some(dec!(200)),
            ),
            &positions,
            &orders,
        );

        assert!(matches!(
            decisions.as_slice(),
            [StrategyDecision::Enter { intent, .. }] if intent.token_id == "token-up"
        ));
    }

    #[test]
    fn unavailable_spot_tick_blocks_quote_triggered_entry_until_reconnect() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::L2Depth {
                symbol: Arc::from("BTCUSDT"),
                obi: 0.8,
                spread_bps: 1,
                bid_depth_near: 20.0,
                ask_depth_near: 5.0,
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: Decimal::ZERO,
                ts: now + chrono::Duration::milliseconds(1),
            },
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &entry_quote(
                "token-up",
                now + chrono::Duration::milliseconds(2),
                Some(dec!(200)),
            ),
            &positions,
            &orders,
        );

        assert!(decisions.is_empty());
        assert!(!strategy.spot.contains_key("BTCUSDT"));
        assert!(!strategy.lob.contains_key("BTCUSDT"));
    }

    #[test]
    fn aggtrade_tick_re_evaluates_entry_without_waiting_for_spot() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &MarketUpdate::AggTrade {
                symbol: Arc::from("BTCUSDT"),
                agg_trade_id: 1,
                price: dec!(100000),
                quantity: dec!(1),
                is_buyer_maker: false,
                ts: now + chrono::Duration::milliseconds(1),
            },
            &positions,
            &orders,
        );

        assert!(matches!(
            decisions.as_slice(),
            [StrategyDecision::Enter { intent, .. }] if intent.token_id == "token-up"
        ));
    }

    #[test]
    fn l2_depth_tick_re_evaluates_entry_without_waiting_for_spot() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &MarketUpdate::L2Depth {
                symbol: Arc::from("BTCUSDT"),
                obi: 0.25,
                spread_bps: 1,
                bid_depth_near: 20.0,
                ask_depth_near: 10.0,
                ts: now + chrono::Duration::milliseconds(1),
            },
            &positions,
            &orders,
        );

        assert!(matches!(
            decisions.as_slice(),
            [StrategyDecision::Enter { intent, .. }] if intent.token_id == "token-up"
        ));
    }

    #[test]
    fn entry_allows_small_multi_level_sweep_delta() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        config.max_sweep_levels = 3;
        config.max_sweep_price_delta = dec!(0.003);
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote_with_ask_levels(
                "token-up",
                now,
                Some(dec!(50)),
                vec![level(dec!(0.20), dec!(50)), level(dec!(0.203), dec!(80))],
            ),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, signal }] => {
                assert_eq!(intent.token_id, "token-up");
                assert_eq!(intent.side, TradeSide::Buy);
                assert_eq!(intent.limit_price, Some(dec!(0.203)));
                assert_eq!(intent.quantity, dec!(123.152709));
                assert!(signal.as_ref().expect("signal").entry_price > dec!(0.20));
            }
            other => panic!("expected one executable swept entry, got {other:?}"),
        }
    }

    #[test]
    fn entry_rejects_large_sweep_price_jump() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        config.max_sweep_levels = 3;
        config.max_sweep_price_delta = dec!(0.003);
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote_with_ask_levels(
                "token-up",
                now,
                Some(dec!(50)),
                vec![level(dec!(0.20), dec!(50)), level(dec!(0.204), dec!(80))],
            ),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "entry must reject deeper ask levels outside the configured sweep delta"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_insufficient_ask_depth"), Some(&1));
    }

    #[test]
    fn opposite_side_position_blocks_same_event_entry() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "an existing DOWN position must block a new UP entry in the same event"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_existing_event_position"), Some(&1));
    }

    #[test]
    fn opposite_side_active_order_blocks_same_event_entry() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = active_order_for_token("token-down", now);

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "an active DOWN order must block a new UP entry in the same event"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_active_event_order"), Some(&1));
    }

    #[test]
    fn event_ml_runtime_model_selects_best_settlement_edge_side() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_edge = 0.01;
        config.min_entry_score = -0.50;
        config.autofactor_runtime_score = Some("event_ml_model:baseline_v1".to_string());
        config.event_ml_model = Some(event_ml_model_with_intercept(-2.0));
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        strategy.on_update(
            &entry_quote("token-down", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, signal }] => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.side, TradeSide::Buy);
                assert_eq!(intent.limit_price, Some(dec!(0.20)));
                let signal = signal.as_ref().expect("signal");
                assert_eq!(signal.direction, "DOWN");
                assert!(
                    signal.p_hat > 0.80,
                    "expected DOWN probability, got {}",
                    signal.p_hat
                );
            }
            other => panic!("expected one Event ML executable DOWN entry, got {other:?}"),
        }
    }

    #[test]
    fn event_ml_opposite_side_position_blocks_same_event_entry() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.min_edge = 0.01;
        config.min_entry_score = -0.50;
        config.autofactor_runtime_score = Some("event_ml_model:baseline_v1".to_string());
        config.event_ml_model = Some(event_ml_model_with_intercept(-2.0));
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-up", now);
        let orders = OrderLedger::default();

        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        strategy.on_update(
            &entry_quote("token-down", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "Event ML must not enter DOWN when the same event already has an UP position"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_existing_event_position"), Some(&1));
    }

    #[test]
    fn event_ml_runtime_schema_rejects_unsupported_features() {
        let model = event_ml_model_with_feature("microprice_offset_bps");

        let err = validate_event_ml_runtime_schema(&model).expect_err("unsupported feature");

        assert!(
            matches!(err, EventMlModelError::MissingFeature(feature) if feature == "microprice_offset_bps")
        );
    }

    #[test]
    fn repricing_momentum_profile_gates_on_spread_adjusted_external_move() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::RepricingMomentum;
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        config.min_confirmation_score = 0.05;
        config.min_entry_score = 0.0;
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        let mut strategy = ThreeLayerStrategy::new(config.clone());
        discover_test_event(&mut strategy, now);
        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );
        assert!(decisions.is_empty());
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(diagnostics.get("skip_repricing_score"), Some(&1));

        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        for i in 0..30 {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: Arc::from("BTCUSDT"),
                    price: Decimal::from(99_400 + i * 20),
                    ts: now - chrono::Duration::seconds(30 - i as i64),
                },
                &positions,
                &orders,
            );
        }
        strategy.on_update(
            &entry_quote("token-up", now, Some(dec!(200))),
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            matches!(decisions.as_slice(), [StrategyDecision::Enter { .. }]),
            "positive side external move should pass repricing gate, got {decisions:?}"
        );
    }

    #[test]
    fn balance_pause_blocks_entries_but_not_take_profit_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        strategy.balance_exhausted_until = Some(now + chrono::Duration::seconds(15));
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions =
            strategy.on_update(&take_profit_quote("token-down", now), &positions, &orders);

        assert_eq!(
            decisions.len(),
            1,
            "risk exits must not wait for buy balance cooldown"
        );
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.limit_price, Some(dec!(0.72)));
            }
            other => panic!("expected take-profit exit, got {other:?}"),
        }
    }

    #[test]
    fn balance_pause_does_not_block_official_settlement_exit() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        strategy.balance_exhausted_until = Some(now + chrono::Duration::seconds(15));
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        let decisions = strategy.on_update(
            &MarketUpdate::EventExpired {
                event_id: Arc::from("evt1"),
                end_time: now,
                resolved_up_won: Some(false),
            },
            &positions,
            &orders,
        );

        assert_eq!(
            decisions.len(),
            1,
            "settlement exits must ignore entry cooldowns"
        );
        match &decisions[0] {
            StrategyDecision::Exit(intent) => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.limit_price, Some(Decimal::new(1, 0)));
            }
            other => panic!("expected settlement exit, got {other:?}"),
        }
    }

    #[test]
    fn settlement_exit_requires_official_resolution_even_when_spot_implies_winner() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 5, 12, 6, 9, 0).unwrap();
        let mut strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut strategy, now);
        let positions = position_with_token("token-down", now);
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(90000),
                ts: now,
            },
            &PositionLedger::default(),
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::EventExpired {
                event_id: Arc::from("evt1"),
                end_time: now,
                resolved_up_won: None,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "settlement exits must not infer official result from spot/price_to_beat"
        );
    }

    #[test]
    fn official_settlement_resolution_prices_winner_and_loser_tokens() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 5, 12, 6, 9, 0).unwrap();
        let orders = OrderLedger::default();

        let mut up_strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut up_strategy, now);
        let up_decisions = up_strategy.on_update(
            &MarketUpdate::EventExpired {
                event_id: Arc::from("evt1"),
                end_time: now,
                resolved_up_won: Some(true),
            },
            &position_with_token("token-up", now),
            &orders,
        );
        match &up_decisions[..] {
            [StrategyDecision::Exit(intent)] => {
                assert_eq!(intent.token_id, "token-up");
                assert_eq!(intent.limit_price, Some(Decimal::new(1, 0)));
            }
            other => panic!("expected one UP settlement exit, got {other:?}"),
        }

        let mut down_strategy = ThreeLayerStrategy::new(test_config());
        discover_test_event(&mut down_strategy, now);
        let down_decisions = down_strategy.on_update(
            &MarketUpdate::EventExpired {
                event_id: Arc::from("evt1"),
                end_time: now,
                resolved_up_won: Some(true),
            },
            &position_with_token("token-down", now),
            &orders,
        );
        match &down_decisions[..] {
            [StrategyDecision::Exit(intent)] => {
                assert_eq!(intent.token_id, "token-down");
                assert_eq!(intent.limit_price, Some(Decimal::ZERO));
            }
            other => panic!("expected one DOWN settlement exit, got {other:?}"),
        }
    }

    #[test]
    fn direction_rejects_weak_early_signal() {
        let config = test_config();
        let result = evaluate_direction_score(0.1, 0.02, 0.0, 0.0, Regime::Early, &config);
        assert!(result.is_none(), "should reject weak direction signal");
    }

    #[test]
    fn direction_passes_strong_early_signal() {
        let config = test_config();
        let result = evaluate_direction_score(1.5, 0.02, 0.0, 0.0, Regime::Early, &config);
        assert!(result.is_some(), "should pass strong early signal");
        let (dir, prob, score) = result.unwrap();
        assert!(dir > 0.0);
        assert!(prob > 0.56);
        assert!(
            score > 0.0,
            "direction score should be positive for strong signal"
        );
    }

    #[test]
    fn direction_rejects_when_raw_alpha_probability_is_neutral() {
        let config = test_config();

        let result = evaluate_direction_score(0.05, 0.02, 0.0, 0.0, Regime::Middle, &config);

        assert!(
            result.is_none(),
            "cheap executable odds must not override neutral directional alpha"
        );
    }

    #[test]
    fn direction_gate_allows_strong_alpha_to_reach_calibrated_ev() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        config.probability_shrink = 0.0;
        config.probability_haircut = 0.0;

        let result = evaluate_direction_score(2.0, 0.02, 0.0, 0.0, Regime::Early, &config);

        let (_dir, prob, score) =
            result.expect("strong raw directional alpha should pass the direction gate");
        assert!((prob - 0.50).abs() < 1e-9);
        assert!(
            score > 0.0,
            "direction score should remain based on alpha strength before EV calibration"
        );
    }

    #[test]
    fn alpha_contrarian_direction_fades_model_favored_side() {
        let mut config = test_config();
        config.alpha_contrarian = true;
        config.min_direction_prob = 0.56;

        let result = evaluate_direction_score(1.5, 0.02, 0.0, 0.0, Regime::Early, &config)
            .expect("contrarian mode should score instead of early-vetoing");
        let (dir, prob, score) = result;
        assert!(dir < 0.0, "positive distance should fade into DOWN");
        assert!(
            prob > 0.50,
            "contrarian effective probability should be transformed alpha confidence"
        );
        assert!(score > 0.0, "strong inverse alpha should be rewarded");
    }

    #[test]
    fn confirmation_returns_negative_for_opposing_lob() {
        let config = test_config();
        let lob = LobState {
            obi: -0.5,
            obi_prev: -0.3,
            spread_bps: 10,
            bid_depth_near: 50.0,
            ask_depth_near: 100.0,
            signed_trade_imbalance: -20.0,
            last_aggtrade_ts: None,
            ts: None,
        };
        let bonus = evaluate_confirmation_bonus(1.0, &lob, -5.0, -0.001, Regime::Late, &config);
        assert!(
            bonus < 0.0,
            "opposing LOB should produce negative bonus, got {}",
            bonus
        );
    }

    #[test]
    fn cex_contrarian_inverts_confirmation_bonus() {
        let mut config = test_config();
        config.cex_contrarian = true;
        let lob = LobState {
            obi: -0.5,
            obi_prev: -0.3,
            spread_bps: 10,
            bid_depth_near: 50.0,
            ask_depth_near: 100.0,
            signed_trade_imbalance: -20.0,
            last_aggtrade_ts: None,
            ts: None,
        };
        let bonus = evaluate_confirmation_bonus(1.0, &lob, -5.0, -0.001, Regime::Late, &config);
        assert!(
            bonus > 0.0,
            "opposing LOB should be rewarded in contrarian confirmation mode, got {}",
            bonus
        );
    }

    #[test]
    fn champion_profile_ignores_confirmation_component() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        let total = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.50,
                distance_over_sigma: -0.20,
                direction_sign: -1.0,
                edge: 0.01,
                edge_score: 0.20,
                confirmation: -1.0,
                repricing_score: 0.0,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );
        let with_positive_confirmation = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.50,
                distance_over_sigma: -0.20,
                direction_sign: -1.0,
                edge: 0.01,
                edge_score: 0.20,
                confirmation: 1.0,
                repricing_score: 0.0,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );
        assert!((total - with_positive_confirmation).abs() < f64::EPSILON);
    }

    #[test]
    fn obi_soft_profile_scores_side_aware_book_imbalance() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::ObiSoft;
        let lob = LobState {
            obi: -0.5,
            obi_prev: -0.2,
            spread_bps: 10,
            bid_depth_near: 50.0,
            ask_depth_near: 100.0,
            signed_trade_imbalance: -20.0,
            last_aggtrade_ts: None,
            ts: None,
        };
        let score = profile_confirmation_score(-1.0, &lob, -0.5, 0.0, Regime::Middle, &config);
        assert!(
            score > 0.0,
            "DOWN-side OBI profile should reward opposing raw UP pressure, got {score}"
        );
    }

    #[test]
    fn hard_confirmation_gate_rejects_weak_obi_when_required() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::ObiHard;
        config.require_confirmation = true;
        config.min_confirmation_score = 0.10;

        assert!(!confirmation_gate_passes(
            0.0,
            config.min_confirmation_score,
            config.cex_contrarian
        ));

        let mut soft_config = config.clone();
        soft_config.profile = ThreeLayerProfile::ObiSoft;
        soft_config.require_confirmation = false;
        let soft_score = evaluate_entry_score(
            &soft_config,
            EntryScoreInputs {
                direction_score: 0.80,
                distance_over_sigma: 0.40,
                direction_sign: 1.0,
                edge: 0.06,
                edge_score: 0.70,
                confirmation: 0.0,
                repricing_score: 0.0,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );

        assert!(
            soft_score > soft_config.min_entry_score,
            "OBI-soft should still be able to score weak confirmation instead of hard rejecting"
        );
    }

    #[test]
    fn spread_adjusted_external_move_matches_autofactor_formula() {
        let side_external_move_30s = 0.004;
        let side_spread = 0.03;
        let score = spread_adjusted_external_move_score(side_external_move_30s, side_spread);

        assert!((score - 0.10).abs() < 1e-9);
        assert!(spread_adjusted_external_move_score(0.004, f64::NAN).is_nan());
        assert!(spread_adjusted_external_move_score(0.004, -0.01).is_nan());
    }

    #[test]
    fn repricing_momentum_profile_rewards_spread_adjusted_external_move() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::RepricingMomentum;
        config.min_confirmation_score = 0.05;
        config.min_entry_score = 0.25;

        let weak = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.20,
                distance_over_sigma: 0.20,
                direction_sign: 1.0,
                edge: 0.04,
                edge_score: 0.20,
                confirmation: 0.0,
                repricing_score: 0.01,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );
        let strong = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.20,
                distance_over_sigma: 0.20,
                direction_sign: 1.0,
                edge: 0.04,
                edge_score: 0.20,
                confirmation: 0.0,
                repricing_score: 0.20,
                drift_30s: 0.0,
                pm_momentum_score: 0.0,
                liquidity_score: 1.0,
            },
        );

        assert!(strong > weak);
        assert!(strong > config.min_entry_score);
    }

    #[test]
    fn settlement_probability_profile_rewards_probability_edge_not_repricing() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_entry_score = 0.25;

        let weak_edge = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.45,
                distance_over_sigma: 0.35,
                direction_sign: 1.0,
                edge: 0.01,
                edge_score: 0.05,
                confirmation: 1.0,
                repricing_score: 1.0,
                drift_30s: 0.0,
                pm_momentum_score: 1.0,
                liquidity_score: 1.0,
            },
        );
        let strong_edge = evaluate_entry_score(
            &config,
            EntryScoreInputs {
                direction_score: 0.45,
                distance_over_sigma: 0.35,
                direction_sign: 1.0,
                edge: 0.08,
                edge_score: 0.90,
                confirmation: -1.0,
                repricing_score: -1.0,
                drift_30s: 0.0,
                pm_momentum_score: -1.0,
                liquidity_score: 1.0,
            },
        );

        assert!(strong_edge > weak_edge);
        assert!(strong_edge > config.min_entry_score);
    }

    #[test]
    fn autofactor_settlement_formula_can_override_entry_score() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;

        let inputs = AutoSettlementFactorInputs {
            settlement_edge: 0.06,
            entry_price: 0.30,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.0,
            sigma_horizon: 0.0,
            entry_capacity_ratio: 3.0,
            side_spread: 0.03,
            external_pressure: 0.0,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };
        let (raw, score) = autofactor_formula_entry_score(
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            inputs,
            config.min_edge,
        )
        .expect("settlement formula score");

        assert!((raw - 0.06).abs() < 1e-9);
        assert!(score > config.min_entry_score);
    }

    #[test]
    fn runtime_settlement_autofactor_uses_pm_side_fair_probability() {
        let config = test_config();
        let now = Utc::now();
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.39)),
                ask: Some(dec!(0.40)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.58)),
                ask: Some(dec!(0.60)),
                bid_size: Some(dec!(100)),
                ask_size: Some(dec!(100)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );
        let event = strategy.events.get("BTCUSDT").unwrap()[0].clone();

        let up_prob = strategy
            .runtime_side_fair_probability(&event, true, now)
            .expect("up side fair probability");
        let down_prob = strategy
            .runtime_side_fair_probability(&event, false, now)
            .expect("down side fair probability");

        assert!(up_prob > 0.40);
        assert!(up_prob < 0.50);
        assert!(((up_prob + down_prob) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn runtime_settlement_autofactor_entry_records_pm_probability_edge() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_distance_over_sigma = 0.0;
        config.min_direction_prob = 0.5;
        config.autofactor_runtime_score =
            Some("autofactor_formula:auto_settlement_full_depth_settlement_edge".to_string());
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.39)),
                ask: Some(dec!(0.40)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.30)),
                ask: Some(dec!(0.32)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, signal }] => {
                assert_eq!(intent.token_id, "token-up");
                let signal = signal.as_ref().expect("signal");
                assert!(
                    signal.p_hat > 0.53,
                    "settlement AutoFactor must record PM side fair probability, got {}",
                    signal.p_hat
                );
                assert!(
                    signal.edge > 0.12,
                    "settlement AutoFactor must record executable PM settlement edge, got {}",
                    signal.edge
                );
            }
            other => panic!("expected one settlement AutoFactor entry, got {other:?}"),
        }
    }

    #[test]
    fn settlement_autofactor_bypasses_legacy_direction_gate_and_selects_edge_side() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_direction_prob = 0.99;
        config.min_distance_over_sigma = 10.0;
        config.min_reward_risk = 0.0;
        config.autofactor_runtime_score =
            Some("autofactor_formula:auto_settlement_full_depth_settlement_edge".to_string());
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.49)),
                ask: Some(dec!(0.50)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.29)),
                ask: Some(dec!(0.30)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts: now,
            },
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert!(
            matches!(decisions.as_slice(), [StrategyDecision::Enter { .. }]),
            "settlement AutoFactor should evaluate executable edge even when legacy direction gate would reject; decisions={decisions:?} diagnostics={diagnostics:?}"
        );
        assert_eq!(diagnostics.get("skip_direction_score"), None);
    }

    #[test]
    fn settlement_autofactor_uses_full_depth_before_edge_gate() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.01;
        config.min_entry_score = 0.25;
        config.max_sweep_levels = 3;
        config.max_sweep_price_delta = dec!(0.10);
        config.autofactor_runtime_score =
            Some("autofactor_formula:auto_settlement_full_depth_settlement_edge".to_string());
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.53)),
                ask: Some(dec!(0.55)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.515), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.43)),
                ask: Some(dec!(0.47)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.47), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        match decisions.as_slice() {
            [StrategyDecision::Enter { signal, .. }] => {
                let signal = signal.as_ref().expect("signal");
                assert!(
                    signal.edge > 0.01,
                    "expected full-depth executable edge above min_edge, got {}",
                    signal.edge
                );
            }
            other => panic!(
                "settlement AutoFactor should not reject on stale top-ask edge before full-depth executable edge; decisions={other:?} diagnostics={diagnostics:?}"
            ),
        }
        assert_eq!(diagnostics.get("skip_edge_score"), None);
        assert_eq!(
            diagnostics.get("settlement_autofactor_depth_fillable"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_formula_evaluations"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_raw_score_pass_min_edge"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_executable_edge_pass_min_edge"),
            Some(&1)
        );
    }

    #[test]
    fn settlement_autofactor_diagnostics_bucket_failed_raw_score() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;
        config.min_entry_score = 0.25;
        config.autofactor_runtime_score =
            Some("autofactor_formula:auto_settlement_full_depth_settlement_edge".to_string());
        let mut strategy = ThreeLayerStrategy::new(config);
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.49)),
                ask: Some(dec!(0.51)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.51), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.49)),
                ask: Some(dec!(0.51)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.51), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100000),
                ts: now,
            },
            &positions,
            &orders,
        );

        assert!(
            decisions.is_empty(),
            "negative executable settlement edge should not enter"
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            diagnostics.get("settlement_autofactor_depth_fillable"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_formula_evaluations"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_raw_score_fail_min_edge"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_executable_edge_fail_min_edge"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_raw_score_neg5_to_neg2pct"),
            Some(&1)
        );
        assert_eq!(diagnostics.get("skip_entry_score"), Some(&1));
    }

    #[test]
    fn model_settlement_autofactor_uses_external_model_probability() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;
        config.min_entry_score = 0.25;
        config.autofactor_runtime_score =
            Some("autofactor_formula:auto_settlement_model_full_depth_settlement_edge".to_string());
        let mut strategy = ThreeLayerStrategy::new(config);
        for (offset, price) in [
            (-5, dec!(100000)),
            (-4, dec!(100020)),
            (-3, dec!(99990)),
            (-2, dec!(100040)),
            (-1, dec!(100000)),
        ] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: Arc::from("BTCUSDT"),
                    price,
                    ts: now + chrono::Duration::seconds(offset),
                },
                &PositionLedger::default(),
                &OrderLedger::default(),
            );
        }
        discover_test_event(&mut strategy, now);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.19)),
                ask: Some(dec!(0.20)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.20), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.79)),
                ask: Some(dec!(0.80)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.80), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100500),
                ts: now,
            },
            &positions,
            &orders,
        );

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, signal }] => {
                assert_eq!(intent.token_id, "token-up");
                let signal = signal.as_ref().expect("signal");
                assert!(
                    signal.p_hat > 0.50,
                    "model settlement AutoFactor must use model probability, got {}",
                    signal.p_hat
                );
                assert!(
                    signal.edge > 0.02,
                    "expected executable model settlement edge above min_edge, got {}",
                    signal.edge
                );
            }
            other => panic!("expected model settlement AutoFactor entry, got {other:?}"),
        }
    }

    #[test]
    fn identifies_settlement_autofactor_runtime_scores() {
        assert!(is_settlement_autofactor_runtime_score(
            "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike"
        ));
        assert!(is_settlement_autofactor_runtime_score(
            "autofactor_formula:auto_settlement_model_full_depth_settlement_edge_x_near_strike"
        ));
        assert!(is_model_settlement_autofactor_runtime_score(
            "autofactor_formula:auto_settlement_model_conservative_settlement_edge_x_capacity"
        ));
        assert!(is_settlement_autofactor_runtime_score(
            "autofactor_formula:mut_auto_settlement_conservative_settlement_edge_x_capacity"
        ));
        assert!(is_settlement_autofactor_runtime_score(
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
        ));
        assert!(is_settlement_autofactor_runtime_score(
            "autofactor_formula:mut_poly_lag_pressure_spread_adjusted"
        ));
        assert!(is_settlement_autofactor_runtime_score(
            "autofactor_formula:mut_spread_adjusted_external_move_full_depth_entry_gate"
        ));
        assert!(!is_settlement_autofactor_runtime_score(
            "autofactor_formula:spread_adjusted_external_move"
        ));
    }

    #[test]
    fn predictive_settlement_autofactor_uses_normalized_entry_gate() {
        let mut config = test_config();
        config.min_edge = 0.02;
        config.min_entry_score = 0.25;

        assert!(settlement_autofactor_entry_score_passes(
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
            Some(0.006),
            0.30,
            &config,
        ));
        assert!(!settlement_autofactor_entry_score_passes(
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            Some(0.006),
            0.30,
            &config,
        ));
        assert!(settlement_autofactor_entry_score_passes(
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            Some(0.021),
            0.30,
            &config,
        ));
    }

    #[test]
    fn predictive_autofactor_records_direct_and_reverse_threshold_counts() {
        let mut strategy = ThreeLayerStrategy::new(test_config());

        strategy.bump_predictive_autofactor_counterfactual_scores(0.006, 0.30);
        strategy.bump_predictive_autofactor_counterfactual_scores(-0.004, -0.20);

        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            diagnostics.get("settlement_autofactor_predictive_score_ge_005"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_predictive_score_ge_025"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_predictive_reverse_score_ge_005"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_predictive_reverse_score_ge_015"),
            Some(&1)
        );
        assert_eq!(
            diagnostics.get("settlement_autofactor_predictive_reverse_score_ge_025"),
            None
        );
    }

    #[test]
    fn autofactor_near_strike_formula_adjusts_settlement_edge() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;

        let near_inputs = AutoSettlementFactorInputs {
            settlement_edge: 0.06,
            entry_price: 0.30,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.0,
            sigma_horizon: 0.0,
            entry_capacity_ratio: 3.0,
            side_spread: 0.03,
            external_pressure: 0.0,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };
        let far_inputs = AutoSettlementFactorInputs {
            distance_over_sigma: 0.90,
            ..near_inputs
        };

        let (near_raw, near_score) = autofactor_formula_entry_score(
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike",
            near_inputs,
            config.min_edge,
        )
        .expect("near-strike settlement formula score");
        let (far_raw, far_score) = autofactor_formula_entry_score(
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike",
            far_inputs,
            config.min_edge,
        )
        .expect("far settlement formula score");

        assert!((near_raw - 0.048).abs() < 1e-9);
        assert!((far_raw - 0.006).abs() < 1e-9);
        assert!(near_score > far_score);
    }

    #[test]
    fn autofactor_entry_price_quality_formula_penalizes_brittle_ticket_prices() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;

        let good_inputs = AutoSettlementFactorInputs {
            settlement_edge: 0.06,
            entry_price: 0.30,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.0,
            sigma_horizon: 0.0,
            entry_capacity_ratio: 3.0,
            side_spread: 0.03,
            external_pressure: 0.0,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };
        let low_ticket_inputs = AutoSettlementFactorInputs {
            entry_price: 0.10,
            ..good_inputs
        };

        let (good_raw, good_score) = autofactor_formula_entry_score(
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_entry_price_quality",
            good_inputs,
            config.min_edge,
        )
        .expect("entry-price-quality settlement formula score");
        let (low_raw, low_score) = autofactor_formula_entry_score(
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_entry_price_quality",
            low_ticket_inputs,
            config.min_edge,
        )
        .expect("low-ticket settlement formula score");

        assert!((good_raw - 0.06).abs() < 1e-9);
        assert!((low_raw - 0.01).abs() < 1e-9);
        assert!(good_score > low_score);
    }

    #[test]
    fn autofactor_composed_model_formula_supports_external_pressure_and_spread() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;

        let inputs = AutoSettlementFactorInputs {
            settlement_edge: 0.06,
            entry_price: 0.30,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.0,
            sigma_horizon: 0.0,
            entry_capacity_ratio: 3.0,
            side_spread: 0.03,
            external_pressure: 0.50,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };

        let (raw, score) = autofactor_formula_entry_score(
            "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
            inputs,
            config.min_edge,
        )
        .expect("composed settlement model formula should score");

        assert!((raw - 0.75).abs() < 1e-9);
        assert!(score > 0.0);
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_near_strike_near_strike",
            inputs,
            config.min_edge,
        )
        .is_none());
    }

    #[test]
    fn predictive_autofactor_formula_uses_external_drift_before_pm_edge() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_entry_score = 0.25;

        let aligned = AutoSettlementFactorInputs {
            settlement_edge: -0.01,
            entry_price: 0.48,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.006,
            sigma_horizon: 4.0,
            entry_capacity_ratio: 3.0,
            side_spread: 0.03,
            external_pressure: 0.0,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };
        let opposed = AutoSettlementFactorInputs {
            drift_30s: -0.006,
            ..aligned
        };

        let (aligned_raw, aligned_score) = autofactor_formula_entry_score(
            "autofactor_formula:amplitude_weighted_momentum_30s_sigma",
            aligned,
            config.min_edge,
        )
        .expect("predictive external formula score");
        let (opposed_raw, opposed_score) = autofactor_formula_entry_score(
            "autofactor_formula:amplitude_weighted_momentum_30s_sigma",
            opposed,
            config.min_edge,
        )
        .expect("opposed predictive external formula score");

        assert!(aligned_raw > 0.0);
        assert!(opposed_raw < 0.0);
        assert!(aligned_score > config.min_entry_score);
        assert!(opposed_score < aligned_score);
    }

    #[test]
    fn predictive_autofactor_runtime_entry_uses_formula_edge_not_pm_fair_edge() {
        use chrono::TimeZone;

        let now = Utc.with_ymd_and_hms(2026, 4, 25, 6, 9, 0).unwrap();
        let mut config = test_config();
        config.profile = ThreeLayerProfile::SettlementProbability;
        config.min_edge = 0.02;
        config.min_entry_score = 0.25;
        config.min_distance_over_sigma = 0.0;
        config.autofactor_runtime_score = Some(
            "autofactor_formula:mut_spread_adjusted_external_move_spread_adjusted".to_string(),
        );
        let mut strategy = ThreeLayerStrategy::new(config);
        let positions = PositionLedger::default();
        let orders = OrderLedger::default();

        for (offset, price) in [
            (-30, dec!(100000)),
            (-20, dec!(100100)),
            (-10, dec!(100300)),
            (-1, dec!(100600)),
        ] {
            strategy.on_update(
                &MarketUpdate::SpotPrice {
                    symbol: Arc::from("BTCUSDT"),
                    price,
                    ts: now + chrono::Duration::seconds(offset),
                },
                &positions,
                &orders,
            );
        }
        discover_test_event(&mut strategy, now);
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-up"),
                bid: Some(dec!(0.49)),
                ask: Some(dec!(0.50)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.50), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );
        strategy.on_update(
            &MarketUpdate::Quote {
                token_id: Arc::from("token-down"),
                bid: Some(dec!(0.48)),
                ask: Some(dec!(0.52)),
                bid_size: Some(dec!(200)),
                ask_size: Some(dec!(200)),
                bid_levels: Vec::new(),
                ask_levels: vec![level(dec!(0.52), dec!(200))],
                ts: now,
            },
            &positions,
            &orders,
        );

        let decisions = strategy.on_update(
            &MarketUpdate::SpotPrice {
                symbol: Arc::from("BTCUSDT"),
                price: dec!(100800),
                ts: now,
            },
            &positions,
            &orders,
        );
        let diagnostics = strategy
            .diagnostics()
            .into_iter()
            .collect::<HashMap<_, _>>();

        match decisions.as_slice() {
            [StrategyDecision::Enter { intent, signal }] => {
                assert_eq!(intent.token_id, "token-up");
                let signal = signal.as_ref().expect("signal");
                assert!(
                    signal.edge > 0.02,
                    "predictive formula edge should drive entry, got {}",
                    signal.edge
                );
            }
            other => panic!(
                "strong predictive AutoFactor should not be killed by PM fair-edge gate; decisions={other:?} diagnostics={diagnostics:?}"
            ),
        }
        assert_eq!(diagnostics.get("skip_edge_score"), None);
        assert_eq!(
            diagnostics.get("settlement_autofactor_formula_evaluations"),
            Some(&2),
            "the second evaluation is the immediate quote-tick entry path"
        );
    }

    #[test]
    fn predictive_poly_lag_pressure_formula_uses_quote_age_pressure_and_drift() {
        let config = test_config();
        let inputs = AutoSettlementFactorInputs {
            settlement_edge: -0.01,
            entry_price: 0.48,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.006,
            sigma_horizon: 4.0,
            entry_capacity_ratio: 3.0,
            side_spread: 0.03,
            external_pressure: 0.80,
            pm_lag_secs: 6.0,
            iv_change_1m: 0.0,
        };

        let (raw, score) = autofactor_formula_entry_score(
            "autofactor_formula:mut_poly_lag_pressure_spread_adjusted",
            inputs,
            config.min_edge,
        )
        .expect("poly lag pressure predictive formula should score finite inputs");

        assert!(raw > 0.0);
        assert!(score > 0.0);
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mut_poly_lag_pressure_spread_adjusted",
            AutoSettlementFactorInputs {
                pm_lag_secs: f64::NAN,
                ..inputs
            },
            config.min_edge,
        )
        .is_none());
    }

    #[test]
    fn predictive_autofactor_formula_supports_full_depth_entry_gate() {
        let config = test_config();
        let fillable = AutoSettlementFactorInputs {
            settlement_edge: -0.01,
            entry_price: 0.48,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.006,
            sigma_horizon: 4.0,
            entry_capacity_ratio: 1.20,
            side_spread: 0.03,
            external_pressure: 0.0,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };
        let unfillable = AutoSettlementFactorInputs {
            entry_capacity_ratio: 0.80,
            ..fillable
        };

        let (raw, score) = autofactor_formula_entry_score(
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate",
            fillable,
            config.min_edge,
        )
        .expect("hard-gated predictive formula should score fillable rows");
        assert!(raw > 0.0);
        assert!(score > 0.0);
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate",
            unfillable,
            config.min_edge,
        )
        .is_none());

        let (spread_raw, spread_score) = autofactor_formula_entry_score(
            "autofactor_formula:mut_spread_adjusted_external_move_full_depth_entry_gate",
            fillable,
            config.min_edge,
        )
        .expect("hard-gated spread-adjusted predictive formula should score fillable rows");
        assert!(spread_raw > raw);
        assert!(spread_score > score);

        let (mcts_spread_raw, mcts_spread_score) = autofactor_formula_entry_score(
            "autofactor_formula:mcts_mcts_spread_adjusted_external_move_full_depth_entry_gate_spread_adjusted",
            fillable,
            config.min_edge,
        )
        .expect("MCTS-guided spread-adjusted predictive formula should score fillable rows");
        assert!(mcts_spread_raw > spread_raw);
        assert!(mcts_spread_score >= spread_score);

        let (mcts_momentum_raw, _) = autofactor_formula_entry_score(
            "autofactor_formula:mcts_mcts_amplitude_weighted_momentum_30s_sigma_spread_adjusted_full_depth_entry_gate",
            fillable,
            config.min_edge,
        )
        .expect("MCTS-guided momentum predictive formula should score fillable rows");
        assert!(mcts_momentum_raw > raw);
    }

    #[test]
    fn predictive_autofactor_formula_supports_selector_threshold_gate() {
        let config = test_config();
        let near = AutoSettlementFactorInputs {
            settlement_edge: -0.01,
            entry_price: 0.48,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.006,
            sigma_horizon: 4.0,
            entry_capacity_ratio: 1.20,
            side_spread: 0.03,
            external_pressure: 0.0,
            pm_lag_secs: 0.0,
            iv_change_1m: 0.0,
        };
        let far = AutoSettlementFactorInputs {
            distance_over_sigma: 4.0,
            ..near
        };

        let (raw, score) = autofactor_formula_entry_score(
            "autofactor_formula:mut_spread_adjusted_external_move_select_near_strike_ge_075",
            near,
            config.min_edge,
        )
        .expect("selector-gated predictive formula should score rows passing the selector");
        assert!(raw > 0.0);
        assert!(score > 0.0);
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mut_spread_adjusted_external_move_select_near_strike_ge_075",
            far,
            config.min_edge,
        )
        .is_none());
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mut_spread_adjusted_external_move_select_entry_price_quality_ge_075",
            near,
            config.min_edge,
        )
        .is_some());
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mut_spread_adjusted_external_move_select_entry_capacity_ge_075",
            AutoSettlementFactorInputs {
                entry_capacity_ratio: 0.10,
                ..near
            },
            config.min_edge,
        )
        .is_none());

        assert!(autofactor_formula_entry_score(
            "autofactor_formula:llm_mut_spread_adjusted_external_move_select_near_strike_ge_075_runtime_pass_through_add_capacity_gate",
            near,
            config.min_edge,
        )
        .is_some());
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:llm_mut_spread_adjusted_external_move_select_near_strike_ge_075_runtime_pass_through_add_capacity_gate",
            AutoSettlementFactorInputs {
                entry_capacity_ratio: 0.10,
                ..near
            },
            config.min_edge,
        )
        .is_none());
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:llm_mut_spread_adjusted_external_move_select_near_strike_ge_075_runtime_pass_through_add_spread_penalty",
            near,
            config.min_edge,
        )
        .is_some());
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mcts_mcts_spread_adjusted_external_move_select_entry_price_quality_ge_025_select_entry_capacity_ge_025",
            near,
            config.min_edge,
        )
        .is_some());
        assert!(autofactor_formula_entry_score(
            "autofactor_formula:mcts_mcts_spread_adjusted_external_move_select_entry_price_quality_ge_025_select_entry_capacity_ge_025",
            AutoSettlementFactorInputs {
                entry_capacity_ratio: 0.01,
                ..near
            },
            config.min_edge,
        )
        .is_none());
    }

    #[test]
    fn edge_score_returns_continuous_value() {
        let config = test_config();
        // ask=0.35 → fee≈0.00455, edge=0.55-0.35-0.00455≈0.195 → edge_score≈1.95 clamped to 1.0
        let result = evaluate_edge_score(0.55, 0.35, Regime::Early, &config);
        assert!(result.is_some());
        let (_ask, edge, _rr, edge_score) = result.unwrap();
        assert!(edge > 0.0);
        assert!(edge_score > 0.0 && edge_score <= 1.0);
        // ask=0.50 → fee=0.005, edge=0.52-0.50-0.005=0.015 → edge_score=0.15
        let result = evaluate_edge_score(0.52, 0.50, Regime::Early, &config);
        assert!(
            result.is_some(),
            "edge_score model should not reject low edge, just score it low"
        );
        let (_ask, _edge, _rr, edge_score) = result.unwrap();
        assert!(
            edge_score < 0.3,
            "low edge should produce low score, got {}",
            edge_score
        );
    }

    #[test]
    fn expectancy_rejects_high_probability_when_entry_is_too_rich() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        config.min_edge = 0.01;
        config.max_entry_price = 0.95;

        assert!(
            evaluate_edge_score(0.70, 0.75, Regime::Early, &config).is_none(),
            "direction probability alone is insufficient when executable entry price makes EV negative"
        );
        assert!(
            evaluate_edge_score(0.58, 0.35, Regime::Early, &config).is_some(),
            "lower direction probability can pass when executable price/payoff create positive EV"
        );
    }

    #[test]
    fn expectancy_prefers_better_price_even_with_lower_probability() {
        let high_probability_rich_entry = expected_value_per_staked_dollar(0.72, 0.70);
        let lower_probability_cheap_entry = expected_value_per_staked_dollar(0.57, 0.28);

        assert!(
            lower_probability_cheap_entry > high_probability_rich_entry,
            "EV should compare probability together with executable price, not probability alone"
        );
    }

    #[test]
    fn probability_calibration_shrinks_and_haircuts_overconfident_alpha() {
        let calibrated = calibrate_direction_probability(0.70, 0.50, 0.03);
        assert!((calibrated - 0.57).abs() < 1e-9);
        assert!(
            expected_value_per_share(calibrated, 0.60) < 0.0,
            "calibrated probability should prevent rich-entry EV overstatement"
        );
    }

    #[test]
    fn alpha_contrarian_edge_score_does_not_reward_negative_edge() {
        let mut config = test_config();
        config.profile = ThreeLayerProfile::Champion;
        config.alpha_contrarian = true;
        config.min_edge = -0.005;
        let result = evaluate_edge_score(0.35, 0.50, Regime::Early, &config);
        assert!(
            result.is_none(),
            "negative edge should be rejected for snapshot profiles even in contrarian mode"
        );
    }

    #[test]
    fn norm_cdf_basic_values() {
        assert!((norm_cdf(0.0) - 0.5).abs() < 0.001);
        assert!(norm_cdf(2.0) > 0.97);
        assert!(norm_cdf(-2.0) < 0.03);
    }
}
