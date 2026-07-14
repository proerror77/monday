//! Pure three-layer scoring model shared by runtime and research tools.

use ploy_operator_contracts::Regime;

use super::common::fees::crypto_fee_cost;
use super::three_layer_profile::ThreeLayerProfile;

#[derive(Debug, Clone, Copy)]
pub struct ThreeLayerModelConfig {
    pub profile: ThreeLayerProfile,
    pub min_direction_prob: f64,
    pub min_distance_over_sigma: f64,
    pub min_confirmation_score: f64,
    pub min_drift_confirmation: f64,
    pub min_edge: f64,
    pub min_reward_risk: f64,
    pub alpha_contrarian: bool,
    pub cex_contrarian: bool,
    pub probability_shrink: f64,
    pub probability_haircut: f64,
    pub min_entry_price: f64,
    pub max_entry_price: f64,
    pub min_entry_score: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct BookConfirmationInputs {
    pub direction_sign: f64,
    pub obi: f64,
    pub obi_delta: f64,
    pub depth_imbalance: f64,
    pub cum_mprice_drift_5m: f64,
    pub drift_30s: f64,
    pub signed_trade_imbalance: f64,
    pub regime: Regime,
}

#[derive(Debug, Clone, Copy)]
pub struct EntryScoreInputs {
    pub direction_score: f64,
    pub distance_over_sigma: f64,
    pub direction_sign: f64,
    pub edge: f64,
    pub edge_score: f64,
    pub confirmation: f64,
    pub repricing_score: f64,
    pub drift_30s: f64,
    pub pm_momentum_score: f64,
    pub liquidity_score: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct AutoSettlementFactorInputs {
    pub settlement_edge: f64,
    pub entry_price: f64,
    pub distance_over_sigma: f64,
    pub direction_sign: f64,
    pub drift_30s: f64,
    pub sigma_horizon: f64,
    pub entry_capacity_ratio: f64,
    pub side_spread: f64,
    pub external_pressure: f64,
    pub pm_lag_secs: f64,
    pub iv_change_1m: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DirectionScore {
    pub direction_sign: f64,
    pub effective_probability: f64,
    pub direction_score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeScore {
    pub entry_price: f64,
    pub expected_value_per_share: f64,
    pub reward_risk: f64,
    pub edge_score: f64,
}

pub fn threshold_score(value: f64, threshold: f64, scale: f64, contrarian: bool) -> f64 {
    if !value.is_finite() || !threshold.is_finite() || !scale.is_finite() || scale <= 0.0 {
        return -0.50;
    }
    let signed = if contrarian {
        threshold - value
    } else {
        value - threshold
    };
    (signed / scale).clamp(-0.50, 1.0)
}

pub fn spread_adjusted_external_move_score(side_external_move_30s: f64, side_spread: f64) -> f64 {
    if !side_external_move_30s.is_finite() || !side_spread.is_finite() || side_spread < 0.0 {
        return f64::NAN;
    }
    side_external_move_30s / (side_spread + 0.01)
}

pub fn auto_settlement_near_strike_score(distance_over_sigma: f64, direction_sign: f64) -> f64 {
    if !distance_over_sigma.is_finite() || !direction_sign.is_finite() {
        return f64::NAN;
    }
    let side_distance_over_sigma = distance_over_sigma * direction_sign;
    (1.0 - side_distance_over_sigma.abs()).clamp(0.0, 1.0)
}

pub fn auto_settlement_entry_capacity_score(entry_capacity_ratio: f64) -> f64 {
    if entry_capacity_ratio.is_finite() {
        (entry_capacity_ratio / 3.0).clamp(0.0, 1.0)
    } else {
        f64::NAN
    }
}

pub fn auto_settlement_entry_price_quality_score(entry_price: f64) -> f64 {
    if !entry_price.is_finite() || entry_price <= 0.0 || entry_price >= 1.0 {
        return f64::NAN;
    }
    let low_ticket_gate = ((entry_price - 0.08) / 0.12).clamp(0.0, 1.0);
    let expensive_ticket_gate = ((0.85 - entry_price) / 0.20).clamp(0.0, 1.0);
    low_ticket_gate.min(expensive_ticket_gate)
}

pub fn auto_settlement_formula_score(
    runtime_score: &str,
    inputs: AutoSettlementFactorInputs,
) -> Option<f64> {
    let name = runtime_score
        .strip_prefix("autofactor_formula:")
        .unwrap_or(runtime_score);
    let normalized_name = normalize_autofactor_formula_name(name);
    if let Some(score) = predictive_formula_score(normalized_name, inputs) {
        return score.is_finite().then_some(score);
    }
    if normalized_name == "amplitude_weighted_momentum_30s_sigma" {
        if !inputs.drift_30s.is_finite()
            || !inputs.direction_sign.is_finite()
            || !inputs.sigma_horizon.is_finite()
        {
            return None;
        }
        let side_drift = inputs.drift_30s * inputs.direction_sign;
        let score = side_drift * inputs.sigma_horizon.abs().ln_1p();
        return score.is_finite().then_some(score);
    }

    let settlement_prefix = [
        "auto_settlement_full_depth_settlement_edge",
        "auto_settlement_conservative_settlement_edge",
        "auto_settlement_model_full_depth_settlement_edge",
        "auto_settlement_model_conservative_settlement_edge",
    ]
    .into_iter()
    .find(|prefix| normalized_name.starts_with(prefix))?;
    if !inputs.settlement_edge.is_finite() {
        return None;
    }
    let mut score = inputs.settlement_edge;
    let suffix = normalize_runtime_formula_suffix(normalized_name.strip_prefix(settlement_prefix)?);
    let suffix = apply_selector_gate(&suffix, inputs)?;

    match suffix.as_str() {
        "" => {}
        "_x_near_strike" => {
            score *=
                auto_settlement_near_strike_score(inputs.distance_over_sigma, inputs.direction_sign)
        }
        "_x_capacity" => score *= auto_settlement_entry_capacity_score(inputs.entry_capacity_ratio),
        "_x_entry_price_quality" => {
            score *= auto_settlement_entry_price_quality_score(inputs.entry_price)
        }
        "_x_near_strike_x_capacity" => {
            score *= auto_settlement_near_strike_score(
                inputs.distance_over_sigma,
                inputs.direction_sign,
            );
            score *= auto_settlement_entry_capacity_score(inputs.entry_capacity_ratio);
        }
        "_x_near_strike_x_capacity_x_entry_price_quality" => {
            score *= auto_settlement_near_strike_score(
                inputs.distance_over_sigma,
                inputs.direction_sign,
            );
            score *= auto_settlement_entry_capacity_score(inputs.entry_capacity_ratio);
            score *= auto_settlement_entry_price_quality_score(inputs.entry_price);
        }
        "_spread_adjusted" => {
            if !inputs.side_spread.is_finite() || inputs.side_spread < 0.0 {
                return None;
            }
            score /= inputs.side_spread + 0.01;
        }
        "_x_external_pressure" => {
            if !inputs.external_pressure.is_finite() {
                return None;
            }
            score *= inputs.external_pressure;
        }
        "_x_iv_change" => {
            if !inputs.iv_change_1m.is_finite() {
                return None;
            }
            score *= inputs.iv_change_1m;
        }
        _ => score = composed_settlement_formula_suffix_score(score, &suffix, inputs)?,
    }
    score.is_finite().then_some(score)
}

fn composed_settlement_formula_suffix_score(
    mut score: f64,
    suffix: &str,
    inputs: AutoSettlementFactorInputs,
) -> Option<f64> {
    let mut applied_effects: Vec<&'static str> = Vec::new();
    for mutation in suffix.strip_prefix('_')?.split('_') {
        let effect = match mutation {
            "squashed" => Some("squashed"),
            "strike" => Some("near_strike"),
            "capacity" => Some("capacity"),
            "gate" => Some("full_depth_entry_gate"),
            "quality" => Some("entry_price_quality"),
            "adjusted" => Some("spread_adjusted"),
            "pressure" => Some("external_pressure"),
            "change" => Some("iv_change"),
            _ => None,
        };
        if let Some(effect) = effect {
            if applied_effects.contains(&effect) {
                return None;
            }
            applied_effects.push(effect);
        }
        match mutation {
            "x" => continue,
            "squashed" => score = score.tanh(),
            "near" => continue,
            "strike" => {
                score *= auto_settlement_near_strike_score(
                    inputs.distance_over_sigma,
                    inputs.direction_sign,
                );
            }
            "capacity" => {
                score *= auto_settlement_entry_capacity_score(inputs.entry_capacity_ratio)
            }
            "full" | "depth" | "entry" => continue,
            "gate" => {
                if !inputs.entry_capacity_ratio.is_finite() || inputs.entry_capacity_ratio < 1.0 {
                    return None;
                }
            }
            "price" => continue,
            "quality" => score *= auto_settlement_entry_price_quality_score(inputs.entry_price),
            "spread" => continue,
            "adjusted" => {
                if !inputs.side_spread.is_finite() || inputs.side_spread < 0.0 {
                    return None;
                }
                score /= inputs.side_spread + 0.01;
            }
            "external" => continue,
            "pressure" => {
                if !inputs.external_pressure.is_finite() {
                    return None;
                }
                score *= inputs.external_pressure;
            }
            "iv" => continue,
            "change" => {
                if !inputs.iv_change_1m.is_finite() {
                    return None;
                }
                score *= inputs.iv_change_1m;
            }
            _ => return None,
        }
        if !score.is_finite() {
            return None;
        }
    }
    Some(score)
}

fn normalize_autofactor_formula_name(mut name: &str) -> &str {
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

fn predictive_formula_score(name: &str, inputs: AutoSettlementFactorInputs) -> Option<f64> {
    let base = if name.starts_with("amplitude_weighted_momentum_30s_sigma") {
        "amplitude_weighted_momentum_30s_sigma"
    } else if name.starts_with("poly_lag_pressure") {
        "poly_lag_pressure"
    } else if name.starts_with("spread_adjusted_external_move") {
        "spread_adjusted_external_move"
    } else {
        return None;
    };
    let suffix = normalize_runtime_formula_suffix(name.strip_prefix(base)?);
    let mut score = match base {
        "amplitude_weighted_momentum_30s_sigma" => {
            if !inputs.drift_30s.is_finite()
                || !inputs.direction_sign.is_finite()
                || !inputs.sigma_horizon.is_finite()
            {
                return None;
            }
            inputs.drift_30s * inputs.direction_sign * inputs.sigma_horizon.abs().ln_1p()
        }
        "poly_lag_pressure" => {
            if !inputs.external_pressure.is_finite()
                || !inputs.drift_30s.is_finite()
                || !inputs.direction_sign.is_finite()
                || !inputs.pm_lag_secs.is_finite()
            {
                return None;
            }
            inputs.external_pressure
                * (inputs.drift_30s * inputs.direction_sign).abs().ln_1p()
                * (inputs.pm_lag_secs.max(0.0) / 3.0).tanh()
        }
        "spread_adjusted_external_move" => spread_adjusted_external_move_score(
            inputs.drift_30s * inputs.direction_sign,
            inputs.side_spread,
        ),
        _ => return None,
    };
    if !score.is_finite() {
        return None;
    }
    if suffix.is_empty() {
        return Some(score);
    }
    let suffix = apply_selector_gate(&suffix, inputs)?;
    if suffix.is_empty() {
        return Some(score);
    }
    for mutation in suffix.strip_prefix('_')?.split('_') {
        match mutation {
            "squashed" => score = score.tanh(),
            "near" => continue,
            "strike" => {
                score *= auto_settlement_near_strike_score(
                    inputs.distance_over_sigma,
                    inputs.direction_sign,
                );
            }
            "capacity" => {
                score *= auto_settlement_entry_capacity_score(inputs.entry_capacity_ratio)
            }
            "full" | "depth" | "entry" => continue,
            "gate" => {
                if !inputs.entry_capacity_ratio.is_finite() || inputs.entry_capacity_ratio < 1.0 {
                    return None;
                }
            }
            "price" => continue,
            "quality" => score *= auto_settlement_entry_price_quality_score(inputs.entry_price),
            "spread" => continue,
            "adjusted" => {
                if !inputs.side_spread.is_finite() || inputs.side_spread < 0.0 {
                    return None;
                }
                score /= inputs.side_spread + 0.01;
            }
            _ => return None,
        }
        if !score.is_finite() {
            return None;
        }
    }
    Some(score)
}

fn normalize_runtime_formula_suffix(suffix: &str) -> String {
    suffix
        .replace(
            "_runtime_pass_through_add_spread_penalty",
            "_spread_adjusted",
        )
        .replace(
            "_runtime_pass_through_add_capacity_gate",
            "_full_depth_entry_gate",
        )
        .replace("_add_capacity_gate", "_full_depth_entry_gate")
}

fn apply_selector_gate(suffix: &str, inputs: AutoSettlementFactorInputs) -> Option<String> {
    let mut remaining_suffix = suffix.to_string();
    while let Some((remaining, selector)) = remaining_suffix.split_once("_select_") {
        let remaining = remaining.to_string();
        let selector = selector.to_string();
        let (feature, raw_threshold, trailing_suffix) = parse_selector_gate(&selector)?;
        let threshold = parse_selector_threshold(raw_threshold)?;
        let gate_score = match feature {
            "near_strike" => {
                auto_settlement_near_strike_score(inputs.distance_over_sigma, inputs.direction_sign)
            }
            "entry_price_quality" => auto_settlement_entry_price_quality_score(inputs.entry_price),
            "entry_capacity" => auto_settlement_entry_capacity_score(inputs.entry_capacity_ratio),
            "full_depth_entry" => {
                if !inputs.entry_capacity_ratio.is_finite() {
                    return None;
                }
                if inputs.entry_capacity_ratio >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            _ => return None,
        };
        if !gate_score.is_finite() || gate_score < threshold {
            return None;
        }
        remaining_suffix = format!("{remaining}{trailing_suffix}");
    }
    Some(remaining_suffix)
}

fn parse_selector_gate(selector: &str) -> Option<(&'static str, &str, String)> {
    for feature in [
        "entry_price_quality",
        "full_depth_entry",
        "entry_capacity",
        "near_strike",
    ] {
        let prefix = format!("{feature}_ge_");
        let Some(raw) = selector.strip_prefix(&prefix) else {
            continue;
        };
        let (threshold, trailing_suffix) = match raw.split_once('_') {
            Some((threshold, trailing)) => (threshold, format!("_{trailing}")),
            None => (raw, String::new()),
        };
        return Some((feature, threshold, trailing_suffix));
    }
    None
}

fn parse_selector_threshold(raw: &str) -> Option<f64> {
    let threshold = if raw.contains('.') {
        raw.parse().ok()?
    } else {
        raw.parse::<f64>().ok()? / 100.0
    };
    (threshold.is_finite() && (0.0..=1.0).contains(&threshold)).then_some(threshold)
}

/// Normal CDF approximation (Abramowitz & Stegun).
pub fn norm_cdf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.2316419 * x.abs());
    let d = 0.3989422804014327 * (-x * x / 2.0).exp();
    let p =
        d * t * (0.3193815 + t * (-0.3565638 + t * (1.781478 + t * (-1.821256 + t * 1.330274))));
    if x >= 0.0 {
        1.0 - p
    } else {
        p
    }
}

pub fn calibrate_direction_probability(
    direction_probability: f64,
    probability_shrink: f64,
    probability_haircut: f64,
) -> f64 {
    if !direction_probability.is_finite()
        || !probability_shrink.is_finite()
        || !probability_haircut.is_finite()
    {
        return f64::NAN;
    }
    let shrink = probability_shrink.clamp(0.0, 1.0);
    let haircut = probability_haircut.clamp(0.0, 0.49);
    (0.5 + (direction_probability - 0.5) * shrink - haircut).clamp(0.01, 0.99)
}

pub fn transformed_side_probability(side_model_prob: f64, alpha_contrarian: bool) -> f64 {
    if !side_model_prob.is_finite() {
        return f64::NAN;
    }
    if alpha_contrarian {
        1.0 - side_model_prob
    } else {
        side_model_prob
    }
}

pub fn executable_edge_threshold(config: &ThreeLayerModelConfig) -> f64 {
    if config.profile.uses_snapshot_scoring() {
        config.min_edge.max(0.0)
    } else {
        config.min_edge
    }
}

pub fn expected_value_per_share(direction_probability: f64, entry_price: f64) -> f64 {
    if !direction_probability.is_finite()
        || !entry_price.is_finite()
        || !(0.0..=1.0).contains(&direction_probability)
        || !(0.0..1.0).contains(&entry_price)
    {
        return f64::NAN;
    }
    let fee = crypto_fee_cost(entry_price);
    let win_payoff = 1.0 - entry_price - fee;
    let loss_cost = entry_price + fee;
    direction_probability * win_payoff - (1.0 - direction_probability) * loss_cost
}

pub fn expected_value_per_staked_dollar(direction_probability: f64, entry_price: f64) -> f64 {
    let expected_value = expected_value_per_share(direction_probability, entry_price);
    if !expected_value.is_finite() || !entry_price.is_finite() || entry_price <= 0.0 {
        return f64::NAN;
    }
    expected_value / entry_price
}

pub fn reward_risk_ratio(entry_price: f64) -> f64 {
    if !entry_price.is_finite() || entry_price <= 0.0 || entry_price >= 1.0 {
        return f64::NAN;
    }
    let fee = crypto_fee_cost(entry_price);
    let reward = 1.0 - entry_price - fee;
    let risk = entry_price + fee;
    if risk <= 0.0 {
        f64::NAN
    } else {
        reward / risk
    }
}

pub fn evaluate_direction_score(
    distance_over_sigma: f64,
    cum_mprice_drift_5m: f64,
    drift_30s: f64,
    regime: Regime,
    config: &ThreeLayerModelConfig,
) -> Option<DirectionScore> {
    if !config.alpha_contrarian
        && distance_over_sigma.abs() < config.min_distance_over_sigma
        && regime == Regime::Early
    {
        return None;
    }

    let model_prob_up = norm_cdf(distance_over_sigma);
    let direction_prob = match regime {
        Regime::Early => model_prob_up,
        Regime::Middle => {
            let lob_nudge = (cum_mprice_drift_5m / 100.0).clamp(-0.08, 0.08);
            (model_prob_up + lob_nudge).clamp(0.01, 0.99)
        }
        Regime::Late => {
            let drift_nudge = (drift_30s * 500.0).clamp(-0.12, 0.12);
            let lob_nudge = (cum_mprice_drift_5m / 80.0).clamp(-0.06, 0.06);
            (model_prob_up + drift_nudge + lob_nudge).clamp(0.01, 0.99)
        }
        Regime::Expiry => {
            let drift_nudge = (drift_30s * 800.0).clamp(-0.15, 0.15);
            (model_prob_up + drift_nudge).clamp(0.01, 0.99)
        }
    };

    let (direction_sign, raw_effective_p) = if config.alpha_contrarian {
        let inverse_alpha_p = direction_prob.max(1.0 - direction_prob);
        if direction_prob >= 0.5 {
            (-1.0_f64, inverse_alpha_p)
        } else {
            (1.0_f64, inverse_alpha_p)
        }
    } else if direction_prob >= 0.5 {
        (1.0_f64, direction_prob)
    } else {
        (-1.0_f64, 1.0 - direction_prob)
    };
    if !raw_effective_p.is_finite() || raw_effective_p < config.min_direction_prob {
        return None;
    }

    let effective_probability = calibrate_direction_probability(
        raw_effective_p,
        config.probability_shrink,
        config.probability_haircut,
    );
    if !effective_probability.is_finite() {
        return None;
    }

    let direction_score = if config.profile.uses_snapshot_scoring() {
        threshold_score(raw_effective_p, config.min_direction_prob, 0.25, false)
    } else {
        ((effective_probability - 0.50) / 0.50).clamp(0.0, 1.0)
    };

    Some(DirectionScore {
        direction_sign,
        effective_probability,
        direction_score,
    })
}

pub fn profile_confirmation_score(
    inputs: BookConfirmationInputs,
    config: &ThreeLayerModelConfig,
) -> f64 {
    match config.profile {
        ThreeLayerProfile::Mixed => {
            let raw_score = (inputs.signed_trade_imbalance / 50.0).clamp(-1.0, 1.0) * 0.30
                + inputs.obi.clamp(-1.0, 1.0) * 0.25
                + inputs.obi_delta.clamp(-1.0, 1.0) * 0.25
                + inputs.depth_imbalance.clamp(-1.0, 1.0) * 0.20
                + (inputs.cum_mprice_drift_5m / 200.0).clamp(-0.15, 0.15);
            let aligned_score = inputs.direction_sign * raw_score;
            let drift_factor = match inputs.regime {
                Regime::Late | Regime::Expiry => {
                    let drift_aligned = inputs.drift_30s * inputs.direction_sign;
                    (drift_aligned * 500.0).clamp(-0.10, 0.10)
                }
                _ => 0.0,
            };
            let score = aligned_score + drift_factor;
            if config.cex_contrarian {
                (-score).clamp(-0.20, 0.20)
            } else {
                score.clamp(-0.20, 0.20)
            }
        }
        ThreeLayerProfile::Champion => 0.0,
        ThreeLayerProfile::ObiSoft | ThreeLayerProfile::ObiHard => {
            let obi = (inputs.obi * inputs.direction_sign).clamp(-1.0, 1.0);
            let obi_delta = (inputs.obi_delta * inputs.direction_sign).clamp(-1.0, 1.0);
            let depth = (inputs.depth_imbalance * inputs.direction_sign).clamp(-1.0, 1.0);
            let microprice = (inputs.cum_mprice_drift_5m * inputs.direction_sign).clamp(-1.0, 1.0);
            let trade_imbalance =
                ((inputs.signed_trade_imbalance / 50.0) * inputs.direction_sign).clamp(-1.0, 1.0);

            0.50 * obi
                + 0.20 * obi_delta
                + 0.15 * depth
                + 0.10 * microprice
                + 0.05 * trade_imbalance
        }
        ThreeLayerProfile::ContinuationSoft => {
            let drift_continuation =
                (inputs.drift_30s * inputs.direction_sign * 800.0).clamp(-1.0, 1.0);
            let microprice = (inputs.cum_mprice_drift_5m * inputs.direction_sign).clamp(-1.0, 1.0);
            let trade_imbalance =
                ((inputs.signed_trade_imbalance / 50.0) * inputs.direction_sign).clamp(-1.0, 1.0);
            0.50 * drift_continuation + 0.30 * microprice + 0.20 * trade_imbalance
        }
        ThreeLayerProfile::RepricingMomentum | ThreeLayerProfile::SettlementProbability => 0.0,
    }
}

pub fn confirmation_gate_passes(value: f64, threshold: f64, contrarian: bool) -> bool {
    if !value.is_finite() || !threshold.is_finite() {
        return false;
    }
    if contrarian {
        value <= threshold
    } else {
        value >= threshold
    }
}

pub fn evaluate_edge_score(
    direction_probability: f64,
    ask: f64,
    config: &ThreeLayerModelConfig,
) -> Option<EdgeScore> {
    if ask < config.min_entry_price || ask > config.max_entry_price {
        return None;
    }

    let edge = expected_value_per_share(direction_probability, ask);
    if !edge.is_finite() {
        return None;
    }

    let reward_risk = reward_risk_ratio(ask);
    if !reward_risk.is_finite() {
        return None;
    }

    let required_edge = executable_edge_threshold(config);
    if config.profile.uses_snapshot_scoring() && edge < required_edge {
        return None;
    }

    let stake_expectancy = expected_value_per_staked_dollar(direction_probability, ask);
    let edge_score = if config.profile.uses_snapshot_scoring() {
        let per_share_score = threshold_score(edge, required_edge, 0.08, false);
        let per_stake_score = threshold_score(stake_expectancy, 0.0, 0.25, false);
        (0.70 * per_share_score + 0.30 * per_stake_score).clamp(-0.50, 1.0)
    } else {
        (stake_expectancy / 0.40).clamp(0.0, 1.0)
    };

    Some(EdgeScore {
        entry_price: ask,
        expected_value_per_share: edge,
        reward_risk,
        edge_score,
    })
}

pub fn evaluate_entry_score(config: &ThreeLayerModelConfig, inputs: EntryScoreInputs) -> f64 {
    if !config.profile.uses_snapshot_scoring() {
        return inputs.direction_score * 0.50
            + inputs.edge_score * 0.35
            + inputs.confirmation * 0.15;
    }

    let side_distance = inputs.distance_over_sigma * inputs.direction_sign;
    let distance_score = threshold_score(
        side_distance,
        config.min_distance_over_sigma,
        0.60,
        config.alpha_contrarian,
    );
    let edge_score = threshold_score(inputs.edge, executable_edge_threshold(config), 0.08, false);
    let drift_side = inputs.drift_30s * inputs.direction_sign;
    let drift_score = ((drift_side - config.min_drift_confirmation) * 800.0).clamp(-0.50, 1.0);
    let confirmation_score = threshold_score(
        inputs.confirmation,
        config.min_confirmation_score,
        0.50,
        config.cex_contrarian,
    );
    let repricing_score = threshold_score(
        inputs.repricing_score,
        config.min_confirmation_score,
        0.10,
        false,
    );

    match config.profile {
        ThreeLayerProfile::Champion => {
            0.33 * inputs.direction_score
                + 0.17 * distance_score
                + 0.25 * edge_score
                + 0.10 * drift_score
                + 0.10 * inputs.pm_momentum_score
                + 0.05 * inputs.liquidity_score
        }
        ThreeLayerProfile::ObiSoft
        | ThreeLayerProfile::ObiHard
        | ThreeLayerProfile::ContinuationSoft => {
            0.25 * inputs.direction_score
                + 0.12 * distance_score
                + 0.18 * edge_score
                + 0.15 * confirmation_score
                + 0.10 * drift_score
                + 0.12 * inputs.pm_momentum_score
                + 0.08 * inputs.liquidity_score
        }
        ThreeLayerProfile::RepricingMomentum => {
            0.20 * inputs.direction_score
                + 0.12 * distance_score
                + 0.20 * edge_score
                + 0.30 * repricing_score
                + 0.10 * inputs.pm_momentum_score
                + 0.08 * inputs.liquidity_score
        }
        ThreeLayerProfile::SettlementProbability => {
            0.35 * inputs.direction_score
                + 0.15 * distance_score
                + 0.40 * edge_score
                + 0.10 * inputs.liquidity_score
        }
        ThreeLayerProfile::Mixed => unreachable!("mixed profile returned above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_auto_settlement_inputs() -> AutoSettlementFactorInputs {
        AutoSettlementFactorInputs {
            settlement_edge: 0.12,
            entry_price: 0.40,
            distance_over_sigma: 0.20,
            direction_sign: 1.0,
            drift_30s: 0.004,
            sigma_horizon: 2.0,
            entry_capacity_ratio: 1.20,
            side_spread: 0.02,
            external_pressure: 0.0,
            pm_lag_secs: 2.0,
            iv_change_1m: 0.0,
        }
    }

    #[test]
    fn settlement_formula_supports_selector_threshold_gate() {
        let score = auto_settlement_formula_score(
            "autofactor_formula:auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_075",
            sample_auto_settlement_inputs(),
        )
        .expect("selector should pass for high-quality entry price");

        assert!((score - 0.048).abs() < 1e-12);

        assert!(auto_settlement_formula_score(
            "autofactor_formula:auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_075",
            AutoSettlementFactorInputs {
                entry_price: 0.10,
                ..sample_auto_settlement_inputs()
            },
        )
        .is_none());
    }

    #[test]
    fn settlement_formula_rejects_malformed_selector_threshold_gate() {
        assert!(auto_settlement_formula_score(
            "autofactor_formula:auto_settlement_model_conservative_settlement_edge_x_capacity_select_entry_price_quality_ge_bad",
            sample_auto_settlement_inputs(),
        )
        .is_none());
    }
}
