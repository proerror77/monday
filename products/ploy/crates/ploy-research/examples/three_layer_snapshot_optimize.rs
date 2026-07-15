//! three_layer_snapshot_optimize - PM5D three-layer optimizer over an immutable research snapshot.
//!
//! This runner is the canonical optimizer path for multi-day research. It loads a
//! compiled `ResearchSnapshot`, expands side-aware `FactorObservationV2` rows,
//! and optimizes gates against official-settlement executable PM CLOB labels
//! without replaying raw tick Parquet for every trial.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use optimizer::prelude::*;
use ploy_research::{
    build_data_health_report, build_factor_observations_v2_with_deribit_and_pm_books,
    load_research_snapshot, FactorObservationV2, FactorReviewOptions, ResearchSnapshotManifest,
};
use ploy_strategy_bundles::strategies::three_layer_model as tl_model;
use ploy_strategy_bundles::ThreeLayerProfile;
use serde::Serialize;

const OPTIMIZER_MIN_DIRECTION_PROB: f64 = 0.515;
const OPTIMIZER_MAX_DIRECTION_PROB: f64 = 0.68;
const CEX_DIRECTION_FIRST_MIN_DIRECTION_PROB: f64 = 0.55;
const STABLE_DIRECTION_MIN_DIRECTION_PROB: f64 = 0.55;
const STABLE_REVERSAL_SOFT_MIN_DIRECTION_PROB: f64 = 0.54;
const STABLE_REVERSAL_FILLABLE_MIN_DIRECTION_PROB: f64 = 0.535;
const LOG_GROWTH_RISK_BUDGET_STAKES: f64 = 40.0;
const SNAPSHOT_MIN_ENTRY_PRICE: f64 = 0.10;
const SNAPSHOT_MAX_ENTRY_PRICE: f64 = 0.85;

#[derive(Debug, Clone, Copy, Serialize)]
struct SnapshotThreeLayerParams {
    min_direction_prob: f64,
    min_distance_over_sigma: f64,
    min_confirmation_score: f64,
    require_confirmation: bool,
    min_drift_confirmation: f64,
    min_edge: f64,
    min_reward_risk: f64,
    min_entry_score: f64,
    alpha_contrarian: bool,
    cex_contrarian: bool,
    probability_shrink: f64,
    probability_haircut: f64,
    cooldown_secs: i64,
    min_time_remaining_secs: i64,
    max_time_remaining_secs: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StrategyProfile {
    /// Historical behavior: blend continuation and CEX/PM book confirmation.
    Mixed,
    /// Strategy A: contrarian alpha inside executable liquidity/risk gates.
    Champion,
    /// Strategy B: Strategy A plus CEX/PM order-book imbalance soft score.
    ObiSoft,
    /// Strategy B-hard: Strategy A plus a hard CEX/PM order-book confirmation gate.
    ObiHard,
    /// Strategy C: Strategy A plus CEX continuation soft score.
    ContinuationSoft,
    /// Strategy D: spread-adjusted external repricing momentum.
    RepricingMomentum,
    /// CEX direction first: Binance-side momentum is the directional selector;
    /// Polymarket ask/liquidity only gate executable EV.
    CexDirectionFirst,
    /// Stable direction: low-degree research profile using improving PM exit bid
    /// plus positive CEX-continuation edge, with conservative probability EV.
    StableDirection,
    /// Stable reversal: low-degree research profile for the observed inverted
    /// alpha side, requiring PM exit-bid improvement before executable EV.
    StableReversal,
    /// Stable reversal soft: same inverted alpha side, but PM dynamics are a
    /// score instead of a single-factor veto to test sample-power recovery.
    StableReversalSoft,
    /// Stable reversal fillable: same soft reversal hypothesis, but uses actual
    /// executable round-trip fillability instead of full-depth-only fillability.
    StableReversalFillable,
}

impl StrategyProfile {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "mixed" | "legacy" => Ok(Self::Mixed),
            "champion" | "a" | "alpha" | "alpha_only" | "contrarian_alpha" => Ok(Self::Champion),
            "obi" | "obi_soft" | "b" | "book_imbalance" | "orderbook" => Ok(Self::ObiSoft),
            "obi_hard" | "obi_confirmed" | "book_imbalance_hard" | "orderbook_hard" => {
                Ok(Self::ObiHard)
            }
            "continuation" | "continuation_soft" | "c" | "cex_continuation" => {
                Ok(Self::ContinuationSoft)
            }
            "repricing"
            | "repricing_momentum"
            | "reprice_momentum"
            | "spread_adjusted_external_move" => Ok(Self::RepricingMomentum),
            "cex_direction_first" | "cex_direction" | "direction_first" | "binance_direction" => {
                Ok(Self::CexDirectionFirst)
            }
            "stable_direction" | "stable_direction_soft" | "stable" | "stable_pm_cex" => {
                Ok(Self::StableDirection)
            }
            "stable_reversal" | "stable_contrarian" | "stable_alpha_reversal" => {
                Ok(Self::StableReversal)
            }
            "stable_reversal_soft" | "reversal_pm_soft" | "stable_contrarian_soft" => {
                Ok(Self::StableReversalSoft)
            }
            "stable_reversal_fillable" | "reversal_fillable" | "stable_contrarian_fillable" => {
                Ok(Self::StableReversalFillable)
            }
            other => anyhow::bail!(
                "unknown --strategy-profile {other:?}; expected mixed, champion, obi_soft, obi_hard, continuation_soft, repricing_momentum, cex_direction_first, stable_direction, stable_reversal, stable_reversal_soft, or stable_reversal_fillable"
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Champion => "champion",
            Self::ObiSoft => "obi_soft",
            Self::ObiHard => "obi_hard",
            Self::ContinuationSoft => "continuation_soft",
            Self::RepricingMomentum => "repricing_momentum",
            Self::CexDirectionFirst => "cex_direction_first",
            Self::StableDirection => "stable_direction",
            Self::StableReversal => "stable_reversal",
            Self::StableReversalSoft => "stable_reversal_soft",
            Self::StableReversalFillable => "stable_reversal_fillable",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Mixed => "legacy mixed continuation + order-book confirmation",
            Self::Champion => "contrarian alpha + executable liquidity/risk gates",
            Self::ObiSoft => "champion + CEX/PM order-book imbalance soft score",
            Self::ObiHard => "champion + hard CEX/PM order-book confirmation gate",
            Self::ContinuationSoft => "champion + CEX continuation soft score",
            Self::RepricingMomentum => "spread-adjusted external repricing momentum",
            Self::CexDirectionFirst => {
                "CEX direction first + Polymarket executable EV/liquidity gates"
            }
            Self::StableDirection => {
                "stable PM exit-bid + CEX-continuation edge gates with conservative EV"
            }
            Self::StableReversal => {
                "stable PM exit-bid gate + contrarian alpha probability with conservative EV"
            }
            Self::StableReversalSoft => {
                "soft PM-dynamics score + contrarian alpha probability with conservative EV"
            }
            Self::StableReversalFillable => {
                "soft reversal + executable round-trip fillability with conservative EV"
            }
        }
    }

    fn fixes_alpha_contrarian(self) -> Option<bool> {
        match self {
            Self::Mixed => None,
            Self::Champion
            | Self::ObiSoft
            | Self::ObiHard
            | Self::ContinuationSoft
            | Self::RepricingMomentum => Some(true),
            Self::CexDirectionFirst => Some(true),
            Self::StableDirection => Some(false),
            Self::StableReversal | Self::StableReversalSoft | Self::StableReversalFillable => {
                Some(true)
            }
        }
    }

    fn fixes_cex_contrarian(self) -> Option<bool> {
        match self {
            Self::Champion | Self::ObiHard | Self::CexDirectionFirst => Some(false),
            Self::StableDirection
            | Self::StableReversal
            | Self::StableReversalSoft
            | Self::StableReversalFillable => Some(false),
            Self::Mixed | Self::ObiSoft | Self::ContinuationSoft | Self::RepricingMomentum => None,
        }
    }

    fn fixes_confirmation_threshold(self) -> Option<f64> {
        match self {
            Self::Champion | Self::CexDirectionFirst => Some(0.0),
            Self::StableDirection | Self::StableReversal => Some(0.10),
            Self::StableReversalSoft | Self::StableReversalFillable => None,
            Self::Mixed
            | Self::ObiSoft
            | Self::ObiHard
            | Self::ContinuationSoft
            | Self::RepricingMomentum => None,
        }
    }

    fn fixes_require_confirmation(self) -> Option<bool> {
        match self {
            Self::ObiHard => Some(true),
            Self::CexDirectionFirst => Some(false),
            Self::StableDirection | Self::StableReversal => Some(true),
            Self::StableReversalSoft | Self::StableReversalFillable => Some(false),
            Self::Mixed
            | Self::Champion
            | Self::ObiSoft
            | Self::ContinuationSoft
            | Self::RepricingMomentum => None,
        }
    }

    fn fixes_probability_shrink(self) -> Option<f64> {
        match self {
            Self::StableDirection
            | Self::StableReversal
            | Self::StableReversalSoft
            | Self::StableReversalFillable => Some(0.38),
            Self::Mixed
            | Self::Champion
            | Self::ObiSoft
            | Self::ObiHard
            | Self::ContinuationSoft
            | Self::RepricingMomentum
            | Self::CexDirectionFirst => None,
        }
    }

    fn fixes_probability_haircut(self) -> Option<f64> {
        match self {
            Self::StableDirection
            | Self::StableReversal
            | Self::StableReversalSoft
            | Self::StableReversalFillable => Some(0.04),
            Self::Mixed
            | Self::Champion
            | Self::ObiSoft
            | Self::ObiHard
            | Self::ContinuationSoft
            | Self::RepricingMomentum
            | Self::CexDirectionFirst => None,
        }
    }

    fn runtime_profile(self) -> Option<ThreeLayerProfile> {
        match self {
            Self::Mixed => Some(ThreeLayerProfile::Mixed),
            Self::Champion => Some(ThreeLayerProfile::Champion),
            Self::ObiSoft => Some(ThreeLayerProfile::ObiSoft),
            Self::ObiHard => Some(ThreeLayerProfile::ObiHard),
            Self::ContinuationSoft => Some(ThreeLayerProfile::ContinuationSoft),
            Self::RepricingMomentum => Some(ThreeLayerProfile::RepricingMomentum),
            Self::CexDirectionFirst => None,
            Self::StableDirection
            | Self::StableReversal
            | Self::StableReversalSoft
            | Self::StableReversalFillable => None,
        }
    }
}

fn model_config(
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> Option<tl_model::ThreeLayerModelConfig> {
    Some(tl_model::ThreeLayerModelConfig {
        profile: profile.runtime_profile()?,
        min_direction_prob: params.min_direction_prob,
        min_distance_over_sigma: params.min_distance_over_sigma,
        min_confirmation_score: params.min_confirmation_score,
        min_drift_confirmation: params.min_drift_confirmation,
        min_edge: params.min_edge,
        min_reward_risk: params.min_reward_risk,
        alpha_contrarian: params.alpha_contrarian,
        cex_contrarian: params.cex_contrarian,
        probability_shrink: params.probability_shrink,
        probability_haircut: params.probability_haircut,
        min_entry_price: SNAPSHOT_MIN_ENTRY_PRICE,
        max_entry_price: SNAPSHOT_MAX_ENTRY_PRICE,
        min_entry_score: params.min_entry_score,
    })
}

#[derive(Debug, Clone, Serialize)]
struct SnapshotObjectiveMetrics {
    candidates: usize,
    selected: usize,
    trades: usize,
    rejected_duplicate: usize,
    rejected_cooldown: usize,
    rejected_non_executable: usize,
    net_pnl: f64,
    avg_pnl: f64,
    avg_entry_price: f64,
    avg_reward_risk: f64,
    avg_expected_value_per_share: f64,
    avg_expected_value_per_stake: f64,
    avg_realized_return_per_stake: f64,
    expectancy_calibration_gap: f64,
    sharpe: f64,
    max_drawdown: f64,
    log_growth: f64,
    avg_log_return: f64,
    positive_day_rate: f64,
    positive_symbol_rate: f64,
    concentration: f64,
    reject_rate: f64,
    objective: f64,
    fill_rate: f64,
    win_rate: f64,
}

#[derive(Debug, Serialize)]
struct OptimizeSummary<'a> {
    snapshot_schema: &'a str,
    snapshot_hash: &'a str,
    snapshot_generated_at: DateTime<Utc>,
    snapshot_data_requirements: &'a [String],
    snapshot_data_audit_status: Option<&'a str>,
    snapshot_data_audit_report: Option<&'a str>,
    snapshot_include_deribit: bool,
    symbols: &'a [String],
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    val_start: DateTime<Utc>,
    val_end: DateTime<Utc>,
    strategy_profile: StrategyProfile,
    trials: usize,
    stake_usd: f64,
    selection_objective: f64,
    min_trades: usize,
    min_trades_source: &'a str,
    train_opportunities: usize,
    val_opportunities: usize,
    train_rows: usize,
    val_rows: usize,
    train_underpowered: bool,
    validation_underpowered: bool,
    best_params: SnapshotThreeLayerParams,
    train_metrics: SnapshotObjectiveMetrics,
    val_metrics: SnapshotObjectiveMetrics,
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

fn parse_window(args: &[String], date_flag: &str, ts_flag: &str, is_end: bool) -> DateTime<Utc> {
    if let Some(raw) = flag_value(args, ts_flag) {
        return parse_timestamp(&raw);
    }
    let raw = flag_value(args, date_flag).unwrap_or_else(|| panic!("{date_flag} required"));
    if is_end {
        parse_date_end(&raw)
    } else {
        parse_date_start(&raw)
    }
}

fn slice_by_time(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> &[FactorObservationV2] {
    let lo = rows.partition_point(|row| row.tick_ts < start);
    let hi = rows.partition_point(|row| row.tick_ts < end);
    &rows[lo..hi]
}

fn validate_snapshot_scope(
    manifest: &ResearchSnapshotManifest,
    symbols: &[String],
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    val_start: DateTime<Utc>,
    val_end: DateTime<Utc>,
    stake_usd: f64,
) -> Result<()> {
    if !manifest.immutable_input {
        anyhow::bail!("snapshot manifest is not immutable_input=true");
    }
    if manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("snapshot manifest is missing snapshot_hash");
    }
    if !manifest.require_official_settlement {
        anyhow::bail!("snapshot was not compiled with require_official_settlement=true");
    }
    if (manifest.stake_usd - stake_usd).abs() > 1e-9 {
        anyhow::bail!(
            "snapshot stake_usd {} does not match optimizer stake_usd {}",
            manifest.stake_usd,
            stake_usd
        );
    }

    let snapshot_symbols: HashSet<&str> = manifest.symbols.iter().map(String::as_str).collect();
    let missing: Vec<&str> = symbols
        .iter()
        .map(String::as_str)
        .filter(|symbol| !snapshot_symbols.contains(symbol))
        .collect();
    if !missing.is_empty() {
        anyhow::bail!(
            "snapshot symbols {:?} do not cover requested symbols {:?}; missing {:?}",
            manifest.symbols,
            symbols,
            missing
        );
    }

    let requested_start = train_start.min(val_start);
    let requested_end = train_end.max(val_end);
    if manifest.start > requested_start || manifest.end < requested_end {
        anyhow::bail!(
            "snapshot window {} -> {} does not cover optimizer window {} -> {}",
            manifest.start,
            manifest.end,
            requested_start,
            requested_end
        );
    }

    Ok(())
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn reward_risk_ratio(entry_price: f64) -> f64 {
    tl_model::reward_risk_ratio(entry_price)
}

fn expected_value_per_share(direction_probability: f64, entry_price: f64) -> f64 {
    tl_model::expected_value_per_share(direction_probability, entry_price)
}

fn expected_value_per_staked_dollar(direction_probability: f64, entry_price: f64) -> f64 {
    tl_model::expected_value_per_staked_dollar(direction_probability, entry_price)
}

fn calibrate_direction_probability(
    direction_probability: f64,
    probability_shrink: f64,
    probability_haircut: f64,
) -> f64 {
    tl_model::calibrate_direction_probability(
        direction_probability,
        probability_shrink,
        probability_haircut,
    )
}

fn direction_probability_search_floor(profile: StrategyProfile) -> f64 {
    match profile {
        StrategyProfile::CexDirectionFirst => CEX_DIRECTION_FIRST_MIN_DIRECTION_PROB,
        StrategyProfile::StableDirection | StrategyProfile::StableReversal => {
            STABLE_DIRECTION_MIN_DIRECTION_PROB
        }
        StrategyProfile::StableReversalSoft => STABLE_REVERSAL_SOFT_MIN_DIRECTION_PROB,
        StrategyProfile::StableReversalFillable => STABLE_REVERSAL_FILLABLE_MIN_DIRECTION_PROB,
        StrategyProfile::Mixed
        | StrategyProfile::Champion
        | StrategyProfile::ObiSoft
        | StrategyProfile::ObiHard
        | StrategyProfile::ContinuationSoft
        | StrategyProfile::RepricingMomentum => OPTIMIZER_MIN_DIRECTION_PROB,
    }
}

fn min_edge_search_bounds(profile: StrategyProfile) -> (f64, f64) {
    match profile {
        // For stable_direction, three_layer_min_edge is interpreted as minimum
        // expected value per staked dollar, not per-share edge. This keeps the
        // search focused on executable expectancy instead of cheap low-probability
        // asks that only look good in per-share terms.
        StrategyProfile::StableDirection | StrategyProfile::StableReversal => (0.15, 0.45),
        StrategyProfile::StableReversalSoft => (0.08, 0.35),
        StrategyProfile::StableReversalFillable => (0.05, 0.30),
        StrategyProfile::Mixed
        | StrategyProfile::Champion
        | StrategyProfile::ObiSoft
        | StrategyProfile::ObiHard
        | StrategyProfile::ContinuationSoft
        | StrategyProfile::RepricingMomentum
        | StrategyProfile::CexDirectionFirst => (0.0, 0.08),
    }
}

fn cex_direction_signal(row: &FactorObservationV2) -> f64 {
    let side = row.side.multiplier();
    let return_60s = finite_or_zero(row.cex_bar_return_60s * side / 0.006).clamp(-1.0, 1.0);
    let return_30s = finite_or_zero(row.cex_bar_return_30s * side / 0.003).clamp(-1.0, 1.0);
    let continuation = finite_or_zero(row.cex_continuation_score_side).clamp(-1.0, 1.0);
    let consecutive = finite_or_zero(row.cex_consecutive_bar_side / 3.0).clamp(-1.0, 1.0);

    0.35 * return_60s + 0.25 * return_30s + 0.25 * continuation + 0.15 * consecutive
}

fn cex_direction_probability(row: &FactorObservationV2) -> f64 {
    let signal = cex_direction_signal(row);
    if !signal.is_finite() {
        return f64::NAN;
    }
    (0.5 + 0.20 * signal).clamp(0.01, 0.99)
}

fn confirmation_score(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> f64 {
    let side = row.side.multiplier();
    match profile {
        StrategyProfile::CexDirectionFirst => return cex_direction_signal(row),
        StrategyProfile::StableDirection => return stable_direction_confirmation_score(row),
        StrategyProfile::StableReversal => return stable_reversal_confirmation_score(row),
        StrategyProfile::StableReversalSoft | StrategyProfile::StableReversalFillable => {
            return stable_reversal_soft_confirmation_score(row);
        }
        StrategyProfile::Mixed
        | StrategyProfile::Champion
        | StrategyProfile::ObiSoft
        | StrategyProfile::ObiHard
        | StrategyProfile::ContinuationSoft
        | StrategyProfile::RepricingMomentum => {}
    }
    let Some(config) = model_config(params, profile) else {
        return f64::NAN;
    };
    tl_model::profile_confirmation_score(
        tl_model::BookConfirmationInputs {
            direction_sign: side,
            obi: finite_or_zero(row.obi_10),
            obi_delta: finite_or_zero(row.obi_delta_10s_side * side),
            depth_imbalance: finite_or_zero(row.depth_imbalance),
            cum_mprice_drift_5m: finite_or_zero(row.microprice_offset_bps / 10.0),
            drift_30s: finite_or_zero(row.drift_30s),
            signed_trade_imbalance: finite_or_zero(
                row.trade_imbalance_delta_10s_side * side * 50.0,
            ),
            regime: row.regime,
        },
        &config,
    )
}

fn is_stable_research_profile(profile: StrategyProfile) -> bool {
    matches!(
        profile,
        StrategyProfile::StableDirection
            | StrategyProfile::StableReversal
            | StrategyProfile::StableReversalSoft
            | StrategyProfile::StableReversalFillable
    )
}

fn stable_direction_confirmation_score(row: &FactorObservationV2) -> f64 {
    let cex_edge = finite_or_zero(row.cex_continuation_edge_gate / 0.08).clamp(-1.0, 1.0);
    let exit_bid = finite_or_zero(row.exit_bid_change_30s / 0.08).clamp(-1.0, 1.0);
    let pm_reprice = finite_or_zero(row.pm_reprice_speed_30s * 30.0 / 0.08).clamp(-1.0, 1.0);
    0.45 * cex_edge + 0.40 * exit_bid + 0.15 * pm_reprice
}

fn stable_direction_hard_gates_pass(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
) -> bool {
    if !row.label_full_depth_entry_fillable || !row.label_full_depth_exit_fillable {
        return false;
    }
    if !row.cex_continuation_edge_gate.is_finite()
        || !row.exit_bid_change_30s.is_finite()
        || row.cex_continuation_edge_gate <= 0.0
        || row.exit_bid_change_30s <= 0.0
    {
        return false;
    }
    stable_direction_confirmation_score(row) >= params.min_confirmation_score
}

fn stable_reversal_confirmation_score(row: &FactorObservationV2) -> f64 {
    let exit_bid = finite_or_zero(row.exit_bid_change_30s / 0.08).clamp(-1.0, 1.0);
    let entry_ask = finite_or_zero(row.entry_ask_change_30s / 0.08).clamp(-1.0, 1.0);
    let pm_reprice = finite_or_zero(row.pm_reprice_speed_30s * 30.0 / 0.08).clamp(-1.0, 1.0);
    let cex_edge_support = finite_or_zero(row.cex_continuation_edge_gate / 0.08)
        .clamp(-1.0, 1.0)
        .max(0.0);
    0.45 * exit_bid + 0.25 * entry_ask + 0.20 * pm_reprice + 0.10 * cex_edge_support
}

fn stable_reversal_hard_gates_pass(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
) -> bool {
    if !row.label_full_depth_entry_fillable || !row.label_full_depth_exit_fillable {
        return false;
    }
    if !row.exit_bid_change_30s.is_finite()
        || !row.entry_ask_change_30s.is_finite()
        || row.exit_bid_change_30s <= 0.0
    {
        return false;
    }
    stable_reversal_confirmation_score(row) >= params.min_confirmation_score
}

fn stable_reversal_soft_confirmation_score(row: &FactorObservationV2) -> f64 {
    let exit_bid = finite_or_zero(row.exit_bid_change_30s / 0.08).clamp(-1.0, 1.0);
    let entry_ask = finite_or_zero(row.entry_ask_change_30s / 0.08).clamp(-1.0, 1.0);
    let pm_reprice = finite_or_zero(row.pm_reprice_speed_30s * 30.0 / 0.08).clamp(-1.0, 1.0);
    let cex_edge = finite_or_zero(row.cex_continuation_edge_gate / 0.08).clamp(-1.0, 1.0);
    let best_pm = exit_bid.max(entry_ask).max(pm_reprice);
    0.42 * best_pm + 0.23 * exit_bid + 0.20 * pm_reprice + 0.15 * cex_edge
}

fn stable_reversal_soft_hard_gates_pass(row: &FactorObservationV2) -> bool {
    if !row.label_full_depth_entry_fillable || !row.label_full_depth_exit_fillable {
        return false;
    }
    let has_pm_dynamic = row.exit_bid_change_30s.is_finite()
        || row.entry_ask_change_30s.is_finite()
        || row.pm_reprice_speed_30s.is_finite();
    has_pm_dynamic && stable_reversal_soft_confirmation_score(row) > -0.25
}

fn stable_reversal_fillable_hard_gates_pass(row: &FactorObservationV2) -> bool {
    if !roundtrip_fillable(row) {
        return false;
    }
    let has_pm_dynamic = row.exit_bid_change_30s.is_finite()
        || row.entry_ask_change_30s.is_finite()
        || row.pm_reprice_speed_30s.is_finite();
    has_pm_dynamic && stable_reversal_soft_confirmation_score(row) > -0.30
}

fn three_layer_entry_score(
    row: &FactorObservationV2,
    edge: f64,
    confirmation: f64,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> f64 {
    if profile == StrategyProfile::CexDirectionFirst {
        let alpha_prob = profile_direction_probability(row, params, profile);
        let direction_score = directional_score(alpha_prob, params.min_direction_prob, 0.25, false);
        let distance_score = directional_score(
            row.side_distance_over_sigma,
            params.min_distance_over_sigma,
            0.60,
            params.alpha_contrarian,
        );
        let drift_side = row.drift_30s * row.side.multiplier();
        let drift_score = ((drift_side - params.min_drift_confirmation) * 800.0).clamp(-0.50, 1.0);
        return 0.55 * direction_score + 0.28 * distance_score + 0.17 * drift_score;
    }

    let alpha_prob = profile_direction_probability(row, params, profile);
    let calibrated_prob = calibrated_profile_probability(row, params, profile);
    let direction_score = directional_score(alpha_prob, params.min_direction_prob, 0.25, false);
    let pm_momentum = row
        .entry_ask_change_10s
        .max(row.entry_ask_change_30s)
        .max(row.pm_reprice_speed_30s * 30.0);
    let pm_momentum_score = if pm_momentum.is_finite() {
        (pm_momentum / 0.08).clamp(-0.50, 1.0)
    } else {
        0.0
    };

    if is_stable_research_profile(profile) {
        let distance_score = directional_score(
            row.side_distance_over_sigma,
            params.min_distance_over_sigma,
            0.60,
            params.alpha_contrarian,
        );
        let ev_per_stake = expected_value_per_staked_dollar(calibrated_prob, row.entry_ask);
        let expectancy_score = directional_score(ev_per_stake, params.min_edge, 0.25, false);
        let confirmation_score = directional_score(
            confirmation,
            params.min_confirmation_score,
            0.50,
            params.cex_contrarian,
        );
        let liquidity_score = if row.label_full_depth_entry_fillable {
            1.0
        } else if row.label_executable_fillable {
            0.5
        } else {
            -0.5
        };
        return 0.24 * direction_score
            + 0.12 * distance_score
            + 0.28 * expectancy_score
            + 0.26 * confirmation_score
            + 0.10 * liquidity_score;
    }

    let Some(config) = model_config(params, profile) else {
        return -0.50;
    };
    let Some(edge_score) = tl_model::evaluate_edge_score(calibrated_prob, row.entry_ask, &config)
        .map(|s| s.edge_score)
    else {
        return -0.50;
    };

    tl_model::evaluate_entry_score(
        &config,
        tl_model::EntryScoreInputs {
            direction_score,
            distance_over_sigma: row.side_distance_over_sigma * row.side.multiplier(),
            direction_sign: row.side.multiplier(),
            edge,
            edge_score,
            confirmation,
            repricing_score: tl_model::spread_adjusted_external_move_score(
                row.cex_bar_return_30s * row.side.multiplier(),
                row.pm_spread_bps / 10_000.0,
            ),
            drift_30s: row.drift_30s,
            pm_momentum_score,
            liquidity_score: 1.0,
        },
    )
}

fn confirmation_gate_passes(value: f64, threshold: f64, contrarian: bool) -> bool {
    if !value.is_finite() || !threshold.is_finite() {
        return false;
    }
    if contrarian {
        value <= threshold
    } else {
        value >= threshold
    }
}

fn transformed_model_probability(side_model_prob: f64, alpha_contrarian: bool) -> f64 {
    if !side_model_prob.is_finite() {
        return f64::NAN;
    }
    if alpha_contrarian {
        1.0 - side_model_prob
    } else {
        side_model_prob
    }
}

fn direction_alpha_probability(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
) -> f64 {
    transformed_model_probability(row.side_model_prob, params.alpha_contrarian)
}

fn calibrated_model_probability(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
) -> f64 {
    calibrate_direction_probability(
        direction_alpha_probability(row, params),
        params.probability_shrink,
        params.probability_haircut,
    )
}

fn profile_direction_probability(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> f64 {
    match profile {
        StrategyProfile::CexDirectionFirst => cex_direction_probability(row),
        StrategyProfile::Mixed
        | StrategyProfile::Champion
        | StrategyProfile::ObiSoft
        | StrategyProfile::ObiHard
        | StrategyProfile::ContinuationSoft
        | StrategyProfile::RepricingMomentum
        | StrategyProfile::StableDirection
        | StrategyProfile::StableReversal
        | StrategyProfile::StableReversalSoft
        | StrategyProfile::StableReversalFillable => direction_alpha_probability(row, params),
    }
}

fn calibrated_profile_probability(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> f64 {
    match profile {
        StrategyProfile::CexDirectionFirst => calibrate_direction_probability(
            cex_direction_probability(row),
            params.probability_shrink,
            params.probability_haircut,
        ),
        StrategyProfile::Mixed
        | StrategyProfile::Champion
        | StrategyProfile::ObiSoft
        | StrategyProfile::ObiHard
        | StrategyProfile::ContinuationSoft
        | StrategyProfile::RepricingMomentum
        | StrategyProfile::StableDirection
        | StrategyProfile::StableReversal
        | StrategyProfile::StableReversalSoft
        | StrategyProfile::StableReversalFillable => calibrated_model_probability(row, params),
    }
}

fn executable_edge_threshold(params: &SnapshotThreeLayerParams) -> f64 {
    params.min_edge.max(0.0)
}

#[cfg(test)]
fn executable_edge_score(edge: f64, params: &SnapshotThreeLayerParams) -> f64 {
    tl_model::threshold_score(edge, executable_edge_threshold(params), 0.08, false)
}

fn directional_score(value: f64, threshold: f64, scale: f64, contrarian: bool) -> f64 {
    tl_model::threshold_score(value, threshold, scale, contrarian)
}

fn executable_pnl(row: &FactorObservationV2) -> Option<f64> {
    row.label_full_depth_executable_pnl_15u
        .or(row.label_executable_pnl_15u)
        .filter(|pnl| pnl.is_finite())
}

fn entry_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_entry_fillable || row.label_executable_fillable
}

fn roundtrip_fillable(row: &FactorObservationV2) -> bool {
    (row.label_full_depth_entry_fillable && row.label_full_depth_exit_fillable)
        || (row.label_executable_fillable && row.label_exit_fillable)
}

fn row_passes_gates(
    row: &FactorObservationV2,
    params: &SnapshotThreeLayerParams,
    profile: StrategyProfile,
) -> bool {
    if row.time_remaining_secs < params.min_time_remaining_secs
        || row.time_remaining_secs > params.max_time_remaining_secs
    {
        return false;
    }
    if !row.entry_ask.is_finite()
        || row.entry_ask < SNAPSHOT_MIN_ENTRY_PRICE
        || row.entry_ask > SNAPSHOT_MAX_ENTRY_PRICE
    {
        return false;
    }
    if !row.pm_lag_secs.is_finite() || row.pm_lag_secs < 0.0 || row.pm_lag_secs > 15.0 {
        return false;
    }
    if profile != StrategyProfile::CexDirectionFirst && !row.side_model_prob.is_finite() {
        return false;
    }
    if !row.side_distance_over_sigma.is_finite() {
        return false;
    }
    if profile == StrategyProfile::StableDirection && !stable_direction_hard_gates_pass(row, params)
    {
        return false;
    }
    if profile == StrategyProfile::StableReversal && !stable_reversal_hard_gates_pass(row, params) {
        return false;
    }
    if profile == StrategyProfile::StableReversalSoft && !stable_reversal_soft_hard_gates_pass(row)
    {
        return false;
    }
    if profile == StrategyProfile::StableReversalFillable
        && !stable_reversal_fillable_hard_gates_pass(row)
    {
        return false;
    }

    let drift_side = row.drift_30s * row.side.multiplier();
    if !drift_side.is_finite() {
        return false;
    }

    let direction_alpha = profile_direction_probability(row, params, profile);
    if !direction_alpha.is_finite() || direction_alpha < params.min_direction_prob {
        return false;
    }

    let direction_probability = calibrated_profile_probability(row, params, profile);
    let edge = expected_value_per_share(direction_probability, row.entry_ask);
    if !edge.is_finite() {
        return false;
    }
    if is_stable_research_profile(profile) {
        let ev_per_stake = expected_value_per_staked_dollar(direction_probability, row.entry_ask);
        if !ev_per_stake.is_finite() || ev_per_stake < params.min_edge || edge <= 0.0 {
            return false;
        }
    } else if edge < executable_edge_threshold(params) {
        return false;
    }
    let reward_risk = reward_risk_ratio(row.entry_ask);
    if !reward_risk.is_finite() || reward_risk < params.min_reward_risk {
        return false;
    }
    if !entry_fillable(row) {
        return false;
    }
    let confirmation = confirmation_score(row, params, profile);
    if !confirmation.is_finite() {
        return false;
    }
    if params.require_confirmation
        && !confirmation_gate_passes(
            confirmation,
            params.min_confirmation_score,
            params.cex_contrarian,
        )
    {
        return false;
    }
    three_layer_entry_score(row, edge, confirmation, params, profile) >= params.min_entry_score
}

fn evaluate_snapshot_objective(
    rows: &[FactorObservationV2],
    params: &SnapshotThreeLayerParams,
    min_trades: usize,
    stake_usd: f64,
    profile: StrategyProfile,
) -> SnapshotObjectiveMetrics {
    let mut last_trade_by_symbol: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut traded_event_sides: HashSet<String> = HashSet::new();
    let mut pnls = Vec::new();
    let mut pnl_by_day: HashMap<String, f64> = HashMap::new();
    let mut pnl_by_symbol: HashMap<String, f64> = HashMap::new();
    let mut candidates = 0usize;
    let mut selected = 0usize;
    let mut rejected_duplicate = 0usize;
    let mut rejected_cooldown = 0usize;
    let mut rejected_non_executable = 0usize;
    let mut entry_price_sum = 0.0;
    let mut reward_risk_sum = 0.0;
    let mut expected_value_per_share_sum = 0.0;
    let mut expected_value_per_stake_sum = 0.0;
    let mut realized_return_per_stake_sum = 0.0;
    let stake = stake_usd.max(1.0);

    for row in rows {
        if !row_passes_gates(row, params, profile) {
            continue;
        }
        candidates += 1;

        let event_side_key = format!("{}:{}", row.event_id, row.side.as_str());
        if traded_event_sides.contains(&event_side_key) {
            rejected_duplicate += 1;
            continue;
        }
        if let Some(last_ts) = last_trade_by_symbol.get(&row.symbol) {
            if (row.tick_ts - *last_ts).num_seconds() < params.cooldown_secs {
                rejected_cooldown += 1;
                continue;
            }
        }

        selected += 1;
        if !entry_fillable(row) {
            rejected_non_executable += 1;
            continue;
        }
        let Some(pnl) = executable_pnl(row) else {
            rejected_non_executable += 1;
            continue;
        };
        let reward_risk = reward_risk_ratio(row.entry_ask);
        if !reward_risk.is_finite() {
            rejected_non_executable += 1;
            continue;
        }
        let direction_probability = calibrated_profile_probability(row, params, profile);
        let ev_per_share = expected_value_per_share(direction_probability, row.entry_ask);
        let ev_per_stake = expected_value_per_staked_dollar(direction_probability, row.entry_ask);
        if !ev_per_share.is_finite() || !ev_per_stake.is_finite() {
            rejected_non_executable += 1;
            continue;
        }

        pnls.push(pnl);
        entry_price_sum += row.entry_ask;
        reward_risk_sum += reward_risk;
        expected_value_per_share_sum += ev_per_share;
        expected_value_per_stake_sum += ev_per_stake;
        realized_return_per_stake_sum += pnl / stake;
        *pnl_by_day
            .entry(row.tick_ts.date_naive().to_string())
            .or_default() += pnl;
        *pnl_by_symbol.entry(row.symbol.clone()).or_default() += pnl;
        traded_event_sides.insert(event_side_key);
        last_trade_by_symbol.insert(row.symbol.clone(), row.tick_ts);
    }

    let trades = pnls.len();
    let net_pnl = pnls.iter().sum::<f64>();
    let avg_pnl = if trades == 0 {
        f64::NAN
    } else {
        net_pnl / trades as f64
    };
    let avg_entry_price = if trades == 0 {
        f64::NAN
    } else {
        entry_price_sum / trades as f64
    };
    let avg_reward_risk = if trades == 0 {
        f64::NAN
    } else {
        reward_risk_sum / trades as f64
    };
    let avg_expected_value_per_share = if trades == 0 {
        f64::NAN
    } else {
        expected_value_per_share_sum / trades as f64
    };
    let avg_expected_value_per_stake = if trades == 0 {
        f64::NAN
    } else {
        expected_value_per_stake_sum / trades as f64
    };
    let avg_realized_return_per_stake = if trades == 0 {
        f64::NAN
    } else {
        realized_return_per_stake_sum / trades as f64
    };
    let expectancy_calibration_gap =
        if avg_expected_value_per_stake.is_finite() && avg_realized_return_per_stake.is_finite() {
            (avg_expected_value_per_stake - avg_realized_return_per_stake).max(0.0)
        } else {
            f64::NAN
        };
    let sharpe = trade_sharpe(&pnls);
    let fill_rate = ratio(trades, selected);
    let reject_rate = ratio(rejected_non_executable, selected);
    let max_drawdown = max_drawdown(&pnls);
    let log_growth = compounded_log_growth(&pnls, stake_usd);
    let avg_log_return = if trades == 0 {
        f64::NAN
    } else {
        log_growth / trades as f64
    };
    let positive_day_rate = positive_bucket_rate(&pnl_by_day);
    let positive_symbol_rate = positive_bucket_rate(&pnl_by_symbol);
    let concentration = concentration_ratio(&pnl_by_day).max(concentration_ratio(&pnl_by_symbol));
    let win_rate = if trades == 0 {
        f64::NAN
    } else {
        ratio(pnls.iter().filter(|pnl| **pnl > 0.0).count(), trades)
    };
    let objective = if trades < min_trades {
        -1_000_000.0 + trades as f64
    } else {
        stable_compounding_objective(StableObjectiveInputs {
            trades,
            min_trades,
            stake_usd,
            net_pnl,
            sharpe,
            max_drawdown,
            log_growth,
            avg_entry_price,
            avg_reward_risk,
            avg_expected_value_per_stake,
            avg_realized_return_per_stake,
            expectancy_calibration_gap,
            fill_rate,
            reject_rate,
            positive_day_rate,
            positive_symbol_rate,
            concentration,
        })
    };

    SnapshotObjectiveMetrics {
        candidates,
        selected,
        trades,
        rejected_duplicate,
        rejected_cooldown,
        rejected_non_executable,
        net_pnl,
        avg_pnl,
        avg_entry_price,
        avg_reward_risk,
        avg_expected_value_per_share,
        avg_expected_value_per_stake,
        avg_realized_return_per_stake,
        expectancy_calibration_gap,
        sharpe,
        max_drawdown,
        log_growth,
        avg_log_return,
        positive_day_rate,
        positive_symbol_rate,
        concentration,
        reject_rate,
        objective,
        fill_rate,
        win_rate,
    }
}

#[derive(Debug, Clone, Copy)]
struct StableObjectiveInputs {
    trades: usize,
    min_trades: usize,
    stake_usd: f64,
    net_pnl: f64,
    sharpe: f64,
    max_drawdown: f64,
    log_growth: f64,
    avg_entry_price: f64,
    avg_reward_risk: f64,
    avg_expected_value_per_stake: f64,
    avg_realized_return_per_stake: f64,
    expectancy_calibration_gap: f64,
    fill_rate: f64,
    reject_rate: f64,
    positive_day_rate: f64,
    positive_symbol_rate: f64,
    concentration: f64,
}

fn stable_compounding_objective(inputs: StableObjectiveInputs) -> f64 {
    if inputs.trades < inputs.min_trades {
        return -1_000_000.0 + inputs.trades as f64;
    }
    let stake = inputs.stake_usd.max(1.0);
    let sample_power = sample_power_multiplier(inputs.trades, inputs.min_trades);
    let pnl_stakes = inputs.net_pnl / stake;
    let pnl_term = pnl_stakes.signum() * pnl_stakes.abs().ln_1p();
    let drawdown_stakes = inputs.max_drawdown / stake;
    let reward_risk_quality = if inputs.avg_reward_risk.is_finite() {
        (inputs.avg_reward_risk - 1.0).clamp(-1.0, 2.0)
    } else {
        -1.0
    };
    let expectancy_quality = if inputs.avg_expected_value_per_stake.is_finite() {
        inputs.avg_expected_value_per_stake.clamp(-0.5, 1.5)
    } else {
        -0.5
    };
    let realized_expectancy_quality = if inputs.avg_realized_return_per_stake.is_finite() {
        inputs.avg_realized_return_per_stake.clamp(-0.5, 1.5)
    } else {
        -0.5
    };
    let calibration_penalty = if inputs.expectancy_calibration_gap.is_finite() {
        2.5 * inputs.expectancy_calibration_gap.clamp(0.0, 2.0)
    } else {
        2.0
    };
    let rich_entry_penalty = if inputs.avg_entry_price.is_finite() {
        ((inputs.avg_entry_price - 0.55).max(0.0) * 4.0).clamp(0.0, 2.0)
    } else {
        2.0
    };
    let stability_bonus = 2.0 * inputs.positive_day_rate
        + 1.5 * inputs.positive_symbol_rate
        + inputs.fill_rate.clamp(0.0, 1.0)
        + 0.5 * reward_risk_quality
        + 0.50 * expectancy_quality
        + 1.25 * realized_expectancy_quality;
    let risk_penalty = 0.45 * drawdown_stakes
        + 2.0 * inputs.reject_rate.clamp(0.0, 1.0)
        + 2.0 * inputs.concentration.clamp(0.0, 1.0)
        + rich_entry_penalty
        + calibration_penalty;
    let sharpe_bonus = 0.25 * inputs.sharpe.clamp(-5.0, 5.0);

    sample_power * (inputs.log_growth + pnl_term + stability_bonus + sharpe_bonus - risk_penalty)
}

fn holistic_selection_objective(
    train: &SnapshotObjectiveMetrics,
    validation: &SnapshotObjectiveMetrics,
    min_trades: usize,
) -> f64 {
    if train.trades < min_trades || validation.trades < min_trades {
        return -1_000_000.0 + train.trades.min(validation.trades) as f64;
    }
    if train.net_pnl <= 0.0 || validation.net_pnl <= 0.0 {
        return train.objective.min(validation.objective) - 10_000.0;
    }

    let stability_gap = (train.positive_day_rate - validation.positive_day_rate).abs()
        + (train.positive_symbol_rate - validation.positive_symbol_rate).abs()
        + (train.fill_rate - validation.fill_rate).abs()
        + (train.concentration - validation.concentration).abs();
    let reward_risk_gap =
        finite_gap(train.avg_reward_risk, validation.avg_reward_risk).clamp(0.0, 3.0);
    let entry_gap = finite_gap(train.avg_entry_price, validation.avg_entry_price).clamp(0.0, 1.0);
    let expectancy_gap = finite_gap(
        train.avg_expected_value_per_stake,
        validation.avg_expected_value_per_stake,
    )
    .clamp(0.0, 2.0);
    let realized_expectancy_gap = finite_gap(
        train.avg_realized_return_per_stake,
        validation.avg_realized_return_per_stake,
    )
    .clamp(0.0, 2.0);
    let calibration_gap = finite_gap(
        train.expectancy_calibration_gap,
        validation.expectancy_calibration_gap,
    )
    .clamp(0.0, 2.0);
    let overstatement_penalty = train.expectancy_calibration_gap.clamp(0.0, 2.0)
        + 1.5 * validation.expectancy_calibration_gap.clamp(0.0, 2.0);
    let generalization_penalty = 3.0 * stability_gap
        + 0.75 * reward_risk_gap
        + 2.0 * entry_gap
        + expectancy_gap
        + realized_expectancy_gap
        + calibration_gap
        + overstatement_penalty;

    0.40 * train.objective + 0.60 * validation.objective - generalization_penalty
}

fn profile_selection_objective(
    train: &SnapshotObjectiveMetrics,
    validation: &SnapshotObjectiveMetrics,
    min_trades: usize,
    profile: StrategyProfile,
) -> f64 {
    let objective = holistic_selection_objective(train, validation, min_trades);
    if !is_stable_research_profile(profile) || objective < -900_000.0 {
        return objective;
    }

    if train.fill_rate < 0.98 || validation.fill_rate < 0.98 {
        return objective - 20_000.0;
    }
    if validation.avg_realized_return_per_stake <= 0.0 {
        return objective - 20_000.0;
    }
    if train.expectancy_calibration_gap > 0.45 || validation.expectancy_calibration_gap > 0.30 {
        return objective
            - 10_000.0
            - 5_000.0
                * (validation.expectancy_calibration_gap - 0.30)
                    .max(0.0)
                    .clamp(0.0, 2.0);
    }
    objective
}

fn finite_gap(left: f64, right: f64) -> f64 {
    if left.is_finite() && right.is_finite() {
        (left - right).abs()
    } else {
        1.0
    }
}

fn max_drawdown(pnls: &[f64]) -> f64 {
    let mut equity = 0.0;
    let mut peak = 0.0;
    let mut max_drawdown = 0.0;
    for pnl in pnls {
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let drawdown = peak - equity;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
    }
    max_drawdown
}

fn compounded_log_growth(pnls: &[f64], stake_usd: f64) -> f64 {
    let risk_budget = (stake_usd * LOG_GROWTH_RISK_BUDGET_STAKES).max(1.0);
    pnls.iter()
        .map(|pnl| (1.0 + (pnl / risk_budget).clamp(-0.99, 10.0)).ln())
        .sum()
}

fn positive_bucket_rate(buckets: &HashMap<String, f64>) -> f64 {
    if buckets.is_empty() {
        return 0.0;
    }
    ratio(
        buckets.values().filter(|pnl| **pnl > 0.0).count(),
        buckets.len(),
    )
}

fn concentration_ratio(buckets: &HashMap<String, f64>) -> f64 {
    let total_abs = buckets.values().map(|pnl| pnl.abs()).sum::<f64>();
    if total_abs <= 1e-9 {
        return 0.0;
    }
    buckets
        .values()
        .map(|pnl| pnl.abs() / total_abs)
        .fold(0.0, f64::max)
}

fn trade_sharpe(pnls: &[f64]) -> f64 {
    if pnls.is_empty() {
        return -100.0;
    }
    let mean = pnls.iter().sum::<f64>() / pnls.len() as f64;
    let variance = pnls.iter().map(|pnl| (pnl - mean).powi(2)).sum::<f64>() / pnls.len() as f64;
    let std = variance.sqrt();
    if std <= 1e-9 {
        return mean.signum() * (pnls.len() as f64).sqrt();
    }
    mean / std * (pnls.len() as f64).sqrt()
}

fn ratio(num: usize, den: usize) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn event_side_opportunities(rows: &[FactorObservationV2]) -> usize {
    rows.iter()
        .map(|row| format!("{}:{}", row.event_id, row.side.as_str()))
        .collect::<HashSet<_>>()
        .len()
}

fn slice_coverage(rows: &[FactorObservationV2]) -> (usize, usize, usize) {
    let event_sides = event_side_opportunities(rows);
    let days = rows
        .iter()
        .map(|row| row.tick_ts.date_naive())
        .collect::<HashSet<_>>()
        .len();
    let symbols = rows
        .iter()
        .map(|row| row.symbol.as_str())
        .collect::<HashSet<_>>()
        .len();
    (event_sides, days, symbols)
}

fn default_min_trades(
    train_rows: &[FactorObservationV2],
    val_rows: &[FactorObservationV2],
) -> usize {
    let (train_events, train_days, train_symbols) = slice_coverage(train_rows);
    let (val_events, val_days, val_symbols) = slice_coverage(val_rows);
    default_min_trades_from_coverage(
        train_events.min(val_events),
        train_days.min(val_days),
        train_symbols.min(val_symbols),
    )
}

fn default_min_trades_from_coverage(
    event_side_opportunities: usize,
    days: usize,
    symbols: usize,
) -> usize {
    let opportunity_floor = ((event_side_opportunities as f64) * 0.015).ceil() as usize;
    let bucket_floor = days.saturating_mul(symbols).saturating_mul(8);
    opportunity_floor.max(bucket_floor).clamp(40, 1_500)
}

fn sample_power_multiplier(trades: usize, min_trades: usize) -> f64 {
    if trades < min_trades || min_trades == 0 {
        return 0.0;
    }
    let full_power_trades = min_trades.saturating_mul(4).max(min_trades + 1);
    (trades as f64 / full_power_trades as f64).min(1.0).sqrt()
}

fn write_outputs(
    output_dir: Option<PathBuf>,
    summary: &OptimizeSummary<'_>,
    trials: &[optimizer::sampler::CompletedTrial<f64>],
) -> Result<()> {
    let Some(output_dir) = output_dir else {
        return Ok(());
    };
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("create output dir {}", output_dir.display()))?;

    let summary_json =
        serde_json::to_string_pretty(summary).context("serialize snapshot optimizer summary")?;
    fs::write(
        output_dir.join("three-layer-snapshot-optimize-summary.json"),
        summary_json,
    )
    .context("write snapshot optimizer summary")?;

    let params = summary.best_params;
    let config = format!(
        "# PM5D three-layer snapshot optimizer output\n\
         # snapshot_hash = {}\n\
         # strategy_profile = {}\n\
         # selection_objective = {:.6}\n\
         # train_objective = {:.6}\n\
         # validation_objective = {:.6}\n\
         # validation_sharpe = {:.6}\n\
         # validation_pnl = {:.6}\n\
         # validation_trades = {}\n\
         # validation_avg_entry = {:.6}\n\
         # validation_avg_reward_risk = {:.6}\n\
         # validation_avg_expected_value_per_share = {:.6}\n\
         # validation_avg_expected_value_per_stake = {:.6}\n\
         # validation_avg_realized_return_per_stake = {:.6}\n\
         # validation_expectancy_calibration_gap = {:.6}\n\
         # validation_max_drawdown = {:.6}\n\
         # validation_log_growth = {:.6}\n\
         # validation_positive_day_rate = {:.6}\n\
         # validation_positive_symbol_rate = {:.6}\n\
         # min_trades = {}\n\
         # min_trades_source = {}\n\
         # data_requirements = {}\n\
         # data_audit_status = {}\n\
         # data_audit_report = {}\n\
         # include_deribit = {}\n\
         # train_underpowered = {}\n\
         # validation_underpowered = {}\n\
         three_layer_min_direction_prob = {:.6}\n\
         three_layer_min_distance_over_sigma = {:.6}\n\
         three_layer_min_confirmation_score = {:.6}\n\
         three_layer_require_confirmation = {}\n\
         three_layer_min_drift_confirmation = {:.8}\n\
         three_layer_min_edge = {:.6}\n\
         three_layer_min_reward_risk = {:.6}\n\
         three_layer_min_entry_score = {:.6}\n\
         three_layer_probability_shrink = {:.6}\n\
         three_layer_probability_haircut = {:.6}\n\
         three_layer_alpha_contrarian = {}\n\
         three_layer_cex_contrarian = {}\n\
         cooldown_secs = {}\n\
         min_time_remaining_secs = {}\n\
         max_time_remaining_secs = {}\n\
         # three_layer_take_profit_ask and three_layer_stop_distance_pct are not optimized here.\n",
        summary.snapshot_hash,
        summary.strategy_profile.as_str(),
        summary.selection_objective,
        summary.train_metrics.objective,
        summary.val_metrics.objective,
        summary.val_metrics.sharpe,
        summary.val_metrics.net_pnl,
        summary.val_metrics.trades,
        summary.val_metrics.avg_entry_price,
        summary.val_metrics.avg_reward_risk,
        summary.val_metrics.avg_expected_value_per_share,
        summary.val_metrics.avg_expected_value_per_stake,
        summary.val_metrics.avg_realized_return_per_stake,
        summary.val_metrics.expectancy_calibration_gap,
        summary.val_metrics.max_drawdown,
        summary.val_metrics.log_growth,
        summary.val_metrics.positive_day_rate,
        summary.val_metrics.positive_symbol_rate,
        summary.min_trades,
        summary.min_trades_source,
        if summary.snapshot_data_requirements.is_empty() {
            "<unspecified>".to_string()
        } else {
            summary.snapshot_data_requirements.join(",")
        },
        summary
            .snapshot_data_audit_status
            .unwrap_or("<not-recorded>"),
        summary
            .snapshot_data_audit_report
            .unwrap_or("<not-recorded>"),
        summary.snapshot_include_deribit,
        summary.train_underpowered,
        summary.validation_underpowered,
        params.min_direction_prob,
        params.min_distance_over_sigma,
        params.min_confirmation_score,
        params.require_confirmation,
        params.min_drift_confirmation,
        params.min_edge,
        params.min_reward_risk,
        params.min_entry_score,
        params.probability_shrink,
        params.probability_haircut,
        params.alpha_contrarian,
        params.cex_contrarian,
        params.cooldown_secs,
        params.min_time_remaining_secs,
        params.max_time_remaining_secs,
    );
    fs::write(
        output_dir.join("three-layer-snapshot-best-params.toml"),
        config,
    )
    .context("write snapshot optimizer best params")?;

    let mut top_trials = trials.to_vec();
    top_trials.sort_by(|lhs, rhs| {
        rhs.value
            .partial_cmp(&lhs.value)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top_text = top_trials
        .iter()
        .take(20)
        .map(|trial| format!("trial={} objective={:.6}", trial.id, trial.value))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(
        output_dir.join("three-layer-snapshot-top-trials.txt"),
        top_text,
    )
    .context("write snapshot optimizer top trials")?;
    Ok(())
}

fn print_metrics(label: &str, metrics: &SnapshotObjectiveMetrics) {
    eprintln!(
        "{label}: objective={:.3} sharpe={:.3} pnl=${:.2} avg_entry={:.3} avg_rr={:.3} avg_ev_share={:.4} avg_ev_stake={:.3} avg_real_stake={:.3} ev_gap={:.3} max_dd=${:.2} log_growth={:.3} pos_day={:.2}% pos_symbol={:.2}% concentration={:.2}% trades={} candidates={} selected={} fill_rate={:.2}% win_rate={:.2}% reject={:.2}% non_exec={} dup={} cooldown={}",
        metrics.objective,
        metrics.sharpe,
        metrics.net_pnl,
        metrics.avg_entry_price,
        metrics.avg_reward_risk,
        metrics.avg_expected_value_per_share,
        metrics.avg_expected_value_per_stake,
        metrics.avg_realized_return_per_stake,
        metrics.expectancy_calibration_gap,
        metrics.max_drawdown,
        metrics.log_growth,
        metrics.positive_day_rate * 100.0,
        metrics.positive_symbol_rate * 100.0,
        metrics.concentration * 100.0,
        metrics.trades,
        metrics.candidates,
        metrics.selected,
        metrics.fill_rate * 100.0,
        metrics.win_rate * 100.0,
        metrics.reject_rate * 100.0,
        metrics.rejected_non_executable,
        metrics.rejected_duplicate,
        metrics.rejected_cooldown,
    );
}

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    let snapshot_dir = PathBuf::from(flag_value(&args, "--snapshot-dir").unwrap_or_else(|| {
        eprintln!("ERROR: --snapshot-dir is required for three_layer_snapshot_optimize");
        std::process::exit(2);
    }));
    let train_start = parse_window(&args, "--train-start", "--train-start-ts", false);
    let train_end = parse_window(&args, "--train-end", "--train-end-ts", true);
    let val_start = parse_window(&args, "--val-start", "--val-start-ts", false);
    let val_end = parse_window(&args, "--val-end", "--val-end-ts", true);
    let symbols = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT,ETHUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let n_trials = flag_value(&args, "--trials")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(50usize);
    let strategy_profile = StrategyProfile::parse(
        &flag_value(&args, "--strategy-profile").unwrap_or_else(|| "mixed".to_string()),
    )?;
    let stake_usd = flag_value(&args, "--stake-usd")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(15.0);
    let min_trades_override =
        flag_value(&args, "--min-trades").and_then(|raw| raw.parse::<usize>().ok());
    let output_dir = flag_value(&args, "--output-dir").map(PathBuf::from);

    let started = Instant::now();
    let snapshot = load_research_snapshot(&snapshot_dir)
        .with_context(|| format!("load research snapshot {}", snapshot_dir.display()))?;
    validate_snapshot_scope(
        &snapshot.manifest,
        &symbols,
        train_start,
        train_end,
        val_start,
        val_end,
        stake_usd,
    )?;
    let snapshot_hash = snapshot
        .manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or("<missing>");

    eprintln!("=== PM5D Three-Layer Snapshot Optimization ===");
    eprintln!(
        "Research snapshot: schema={} hash={} generated_at={}",
        snapshot.manifest.schema_version, snapshot_hash, snapshot.manifest.generated_at
    );
    eprintln!(
        "Snapshot rows: observations={} deribit={} pm_books={} load_ms={}",
        snapshot.observations.len(),
        snapshot.deribit_snapshots.len(),
        snapshot.pm_book_snapshots.len(),
        started.elapsed().as_millis()
    );
    eprintln!(
        "Train: {} -> {}  Val: {} -> {}  Symbols: {:?}",
        train_start, train_end, val_start, val_end, symbols
    );
    eprintln!(
        "Strategy profile: {} ({})",
        strategy_profile.as_str(),
        strategy_profile.description()
    );
    eprintln!(
        "Trials: {n_trials}  Algorithm: TPE  Stake: ${stake_usd:.2}  Min trades: {}",
        min_trades_override
            .map(|value| value.to_string())
            .unwrap_or_else(|| "dynamic".to_string())
    );
    eprintln!(
        "Objective labels: official settlement + executable PM CLOB full-depth/top-book fillability"
    );
    eprintln!(
        "Note: stop-loss/take-profit path exits are not optimized in this observation-level runner."
    );

    let review_options = FactorReviewOptions {
        stake_usd,
        min_observations: min_trades_override.unwrap_or(20),
        top_quantile: 0.2,
        max_quote_age_secs: snapshot.manifest.max_quote_age_secs,
    };
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        &snapshot.observations,
        &snapshot.deribit_snapshots,
        &snapshot.pm_book_snapshots,
        &review_options,
    );
    let symbol_set = symbols.iter().map(String::as_str).collect::<HashSet<_>>();
    v2_rows.retain(|row| symbol_set.contains(row.symbol.as_str()));
    v2_rows.sort_by_key(|row| row.tick_ts);

    let health = build_data_health_report(&snapshot.observations, &v2_rows);
    eprintln!(
        "Data health: source_obs={} v2_rows={} executable_pnl_rows={} full_depth_pnl_rows={} entry_fill_rate={:.2}% full_depth_entry_fill_rate={:.2}%",
        health.source_observations,
        health.v2_rows,
        health.executable_pnl_rows,
        health.full_depth_executable_pnl_rows,
        health.entry_fill_rate() * 100.0,
        health.full_depth_entry_fill_rate() * 100.0,
    );

    let train_rows = slice_by_time(&v2_rows, train_start, train_end).to_vec();
    let val_rows = slice_by_time(&v2_rows, val_start, val_end).to_vec();
    let train_opportunities = event_side_opportunities(&train_rows);
    let val_opportunities = event_side_opportunities(&val_rows);
    let min_trades =
        min_trades_override.unwrap_or_else(|| default_min_trades(&train_rows, &val_rows));
    let min_trades_source = if min_trades_override.is_some() {
        "cli"
    } else {
        "dynamic_default"
    };
    if train_rows.len() < min_trades {
        anyhow::bail!(
            "training slice has only {} rows; need at least {}",
            train_rows.len(),
            min_trades
        );
    }
    if val_rows.len() < min_trades {
        anyhow::bail!(
            "validation slice has only {} rows; need at least {}",
            val_rows.len(),
            min_trades
        );
    }
    eprintln!(
        "Slice rows: train={} validation={}",
        train_rows.len(),
        val_rows.len()
    );
    eprintln!(
        "Event-side opportunities: train={} validation={}",
        train_opportunities, val_opportunities
    );
    eprintln!("Trade floor: min_trades={min_trades} source={min_trades_source}");

    let train_rows = Arc::new(train_rows);
    let val_rows = Arc::new(val_rows);
    let study: Study<f64> = Study::maximize(TpeSampler::new());
    let p_min_direction_prob = FloatParam::new(
        direction_probability_search_floor(strategy_profile),
        OPTIMIZER_MAX_DIRECTION_PROB,
    )
    .name("three_layer_min_direction_prob");
    let p_min_distance_over_sigma =
        FloatParam::new(-0.20, 0.60).name("three_layer_min_distance_over_sigma");
    let p_min_confirmation_score =
        FloatParam::new(-0.15, 0.25).name("three_layer_min_confirmation_score");
    let p_min_drift_confirmation =
        FloatParam::new(-0.0005, 0.0008).name("three_layer_min_drift_confirmation");
    let (min_edge_low, min_edge_high) = min_edge_search_bounds(strategy_profile);
    let p_min_edge = FloatParam::new(min_edge_low, min_edge_high).name("three_layer_min_edge");
    let p_min_reward_risk = FloatParam::new(0.20, 2.0).name("three_layer_min_reward_risk");
    let p_min_entry_score = FloatParam::new(0.05, 0.55).name("three_layer_min_entry_score");
    let p_require_confirmation = BoolParam::new().name("three_layer_require_confirmation");
    let p_probability_shrink = FloatParam::new(0.35, 1.0).name("three_layer_probability_shrink");
    let p_probability_haircut = FloatParam::new(0.0, 0.08).name("three_layer_probability_haircut");
    let p_alpha_contrarian = BoolParam::new().name("alpha_contrarian");
    let p_cex_contrarian = BoolParam::new().name("cex_contrarian");
    let p_cooldown_secs = IntParam::new(15, 60).name("cooldown_secs");
    let p_min_time_remaining_secs = IntParam::new(30, 120).name("min_time_remaining_secs");
    let p_max_time_span_secs = IntParam::new(30, 120).name("three_layer_time_span_secs");

    {
        let train_rows = Arc::clone(&train_rows);
        let p_min_direction_prob_c = p_min_direction_prob.clone();
        let p_min_distance_over_sigma_c = p_min_distance_over_sigma.clone();
        let p_min_confirmation_score_c = p_min_confirmation_score.clone();
        let p_min_drift_confirmation_c = p_min_drift_confirmation.clone();
        let p_min_edge_c = p_min_edge.clone();
        let p_min_reward_risk_c = p_min_reward_risk.clone();
        let p_min_entry_score_c = p_min_entry_score.clone();
        let p_require_confirmation_c = p_require_confirmation.clone();
        let p_probability_shrink_c = p_probability_shrink.clone();
        let p_probability_haircut_c = p_probability_haircut.clone();
        let p_alpha_contrarian_c = p_alpha_contrarian.clone();
        let p_cex_contrarian_c = p_cex_contrarian.clone();
        let p_cooldown_secs_c = p_cooldown_secs.clone();
        let p_min_time_remaining_secs_c = p_min_time_remaining_secs.clone();
        let p_max_time_span_secs_c = p_max_time_span_secs.clone();
        let val_rows = Arc::clone(&val_rows);

        study
            .optimize(n_trials, move |trial: &mut Trial| {
                let min_time_remaining_secs = p_min_time_remaining_secs_c.suggest(trial)?;
                let alpha_contrarian = match strategy_profile.fixes_alpha_contrarian() {
                    Some(value) => value,
                    None => p_alpha_contrarian_c.suggest(trial)?,
                };
                let cex_contrarian = match strategy_profile.fixes_cex_contrarian() {
                    Some(value) => value,
                    None => p_cex_contrarian_c.suggest(trial)?,
                };
                let min_confirmation_score =
                    match strategy_profile.fixes_confirmation_threshold() {
                        Some(value) => value,
                        None => p_min_confirmation_score_c.suggest(trial)?,
                    };
                let require_confirmation = match strategy_profile.fixes_require_confirmation() {
                    Some(value) => value,
                    None => p_require_confirmation_c.suggest(trial)?,
                };
                let probability_shrink = match strategy_profile.fixes_probability_shrink() {
                    Some(value) => value,
                    None => p_probability_shrink_c.suggest(trial)?,
                };
                let probability_haircut = match strategy_profile.fixes_probability_haircut() {
                    Some(value) => value,
                    None => p_probability_haircut_c.suggest(trial)?,
                };
                let params = SnapshotThreeLayerParams {
                    min_direction_prob: p_min_direction_prob_c.suggest(trial)?,
                    min_distance_over_sigma: p_min_distance_over_sigma_c.suggest(trial)?,
                    min_confirmation_score,
                    require_confirmation,
                    min_drift_confirmation: p_min_drift_confirmation_c.suggest(trial)?,
                    min_edge: p_min_edge_c.suggest(trial)?,
                    min_reward_risk: p_min_reward_risk_c.suggest(trial)?,
                    min_entry_score: p_min_entry_score_c.suggest(trial)?,
                    alpha_contrarian,
                    cex_contrarian,
                    probability_shrink,
                    probability_haircut,
                    cooldown_secs: p_cooldown_secs_c.suggest(trial)?,
                    min_time_remaining_secs,
                    max_time_remaining_secs: min_time_remaining_secs
                        + p_max_time_span_secs_c.suggest(trial)?,
                };

                let train_metrics = evaluate_snapshot_objective(
                    &train_rows,
                    &params,
                    min_trades,
                    stake_usd,
                    strategy_profile,
                );
                let val_metrics = evaluate_snapshot_objective(
                    &val_rows,
                    &params,
                    min_trades,
                    stake_usd,
                    strategy_profile,
                );
                let objective = profile_selection_objective(
                    &train_metrics,
                    &val_metrics,
                    min_trades,
                    strategy_profile,
                );
                eprintln!(
                    "  Trial {:>3}: profile={} source=snapshot-observation objective={:>9.3} train_obj={:>8.3} val_obj={:>8.3} train_pnl=${:>8.2}/{} val_pnl=${:>8.2}/{} val_dd=${:>7.2} val_entry={:.3} val_rr={:.2} val_ev_stake={:.3} val_real_stake={:.3} val_ev_gap={:.3} val_pos_sym={:>5.1}% | dir_prob={:.3} shrink={:.3} haircut={:.3} dist_sigma={:.3} conf={:.3} require_conf={} drift={:.5} min_ev={:.3} rr={:.2} score={:.3} alpha_contra={} cex_contra={} cd={}s time={}..{}",
                    trial.id(),
                    strategy_profile.as_str(),
                    objective,
                    train_metrics.objective,
                    val_metrics.objective,
                    train_metrics.net_pnl,
                    train_metrics.trades,
                    val_metrics.net_pnl,
                    val_metrics.trades,
                    val_metrics.max_drawdown,
                    val_metrics.avg_entry_price,
                    val_metrics.avg_reward_risk,
                    val_metrics.avg_expected_value_per_stake,
                    val_metrics.avg_realized_return_per_stake,
                    val_metrics.expectancy_calibration_gap,
                    val_metrics.positive_symbol_rate * 100.0,
                    params.min_direction_prob,
                    params.probability_shrink,
                    params.probability_haircut,
                    params.min_distance_over_sigma,
                    params.min_confirmation_score,
                    params.require_confirmation,
                    params.min_drift_confirmation,
                    params.min_edge,
                    params.min_reward_risk,
                    params.min_entry_score,
                    params.alpha_contrarian,
                    params.cex_contrarian,
                    params.cooldown_secs,
                    params.min_time_remaining_secs,
                    params.max_time_remaining_secs,
                );
                Ok::<f64, Error>(objective)
            })
            .context("snapshot TPE optimization failed")?;
    }

    let best = study
        .best_trial()
        .context("no completed optimizer trials")?;
    let best_min_time_remaining_secs = best.get(&p_min_time_remaining_secs).unwrap_or(30);
    let best_params = SnapshotThreeLayerParams {
        min_direction_prob: best
            .get(&p_min_direction_prob)
            .unwrap_or_else(|| direction_probability_search_floor(strategy_profile)),
        min_distance_over_sigma: best.get(&p_min_distance_over_sigma).unwrap_or(0.10),
        min_confirmation_score: strategy_profile
            .fixes_confirmation_threshold()
            .unwrap_or_else(|| best.get(&p_min_confirmation_score).unwrap_or(0.05)),
        require_confirmation: strategy_profile
            .fixes_require_confirmation()
            .unwrap_or_else(|| best.get(&p_require_confirmation).unwrap_or(false)),
        min_drift_confirmation: best.get(&p_min_drift_confirmation).unwrap_or(0.0001),
        min_edge: best.get(&p_min_edge).unwrap_or(0.02),
        min_reward_risk: best.get(&p_min_reward_risk).unwrap_or(0.8),
        min_entry_score: best.get(&p_min_entry_score).unwrap_or(0.10),
        alpha_contrarian: strategy_profile
            .fixes_alpha_contrarian()
            .unwrap_or_else(|| best.get(&p_alpha_contrarian).unwrap_or(false)),
        cex_contrarian: strategy_profile
            .fixes_cex_contrarian()
            .unwrap_or_else(|| best.get(&p_cex_contrarian).unwrap_or(false)),
        probability_shrink: strategy_profile
            .fixes_probability_shrink()
            .unwrap_or_else(|| best.get(&p_probability_shrink).unwrap_or(1.0)),
        probability_haircut: strategy_profile
            .fixes_probability_haircut()
            .unwrap_or_else(|| best.get(&p_probability_haircut).unwrap_or(0.0)),
        cooldown_secs: best.get(&p_cooldown_secs).unwrap_or(15),
        min_time_remaining_secs: best_min_time_remaining_secs,
        max_time_remaining_secs: best_min_time_remaining_secs
            + best.get(&p_max_time_span_secs).unwrap_or(60),
    };

    let train_metrics = evaluate_snapshot_objective(
        &train_rows,
        &best_params,
        min_trades,
        stake_usd,
        strategy_profile,
    );
    let val_metrics = evaluate_snapshot_objective(
        &val_rows,
        &best_params,
        min_trades,
        stake_usd,
        strategy_profile,
    );
    let selection_objective =
        profile_selection_objective(&train_metrics, &val_metrics, min_trades, strategy_profile);
    let train_underpowered = train_metrics.trades < min_trades;
    let validation_underpowered = val_metrics.trades < min_trades;

    eprintln!("\n=== Best Parameters (Training) ===");
    eprintln!(
        "Strategy profile:                 {}",
        strategy_profile.as_str()
    );
    eprintln!("Objective:                       {:.3}", best.value);
    eprintln!(
        "three_layer_min_direction_prob = {:.6}",
        best_params.min_direction_prob
    );
    eprintln!(
        "three_layer_min_distance_over_sigma = {:.6}",
        best_params.min_distance_over_sigma
    );
    eprintln!(
        "three_layer_min_confirmation_score = {:.6}",
        best_params.min_confirmation_score
    );
    eprintln!(
        "three_layer_require_confirmation = {}",
        best_params.require_confirmation
    );
    eprintln!(
        "three_layer_min_drift_confirmation = {:.8}",
        best_params.min_drift_confirmation
    );
    eprintln!("three_layer_min_edge = {:.6}", best_params.min_edge);
    eprintln!(
        "three_layer_min_reward_risk = {:.6}",
        best_params.min_reward_risk
    );
    eprintln!(
        "three_layer_min_entry_score = {:.6}",
        best_params.min_entry_score
    );
    eprintln!(
        "three_layer_probability_shrink = {:.6}",
        best_params.probability_shrink
    );
    eprintln!(
        "three_layer_probability_haircut = {:.6}",
        best_params.probability_haircut
    );
    eprintln!("alpha_contrarian = {}", best_params.alpha_contrarian);
    eprintln!("cex_contrarian = {}", best_params.cex_contrarian);
    eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
    eprintln!(
        "min_time_remaining_secs = {}",
        best_params.min_time_remaining_secs
    );
    eprintln!(
        "max_time_remaining_secs = {}",
        best_params.max_time_remaining_secs
    );
    eprintln!("\n=== Snapshot Objective Metrics ===");
    eprintln!("Selection objective: {:.3}", selection_objective);
    print_metrics("Train", &train_metrics);
    print_metrics("Validation", &val_metrics);
    if train_underpowered {
        eprintln!(
            "WARNING: best training result has {} trades below min_trades={}; do not use this parameter set.",
            train_metrics.trades, min_trades
        );
    }
    if validation_underpowered {
        eprintln!(
            "WARNING: validation result has {} trades below min_trades={}; do not use this parameter set.",
            val_metrics.trades, min_trades
        );
    }

    eprintln!("\n=== Config Snippet ===");
    eprintln!("# Paste into [strategy] only after walk-forward and dry-run/live parity review.");
    eprintln!("# strategy_profile = {}", strategy_profile.as_str());
    eprintln!(
        "three_layer_min_direction_prob = {:.6}",
        best_params.min_direction_prob
    );
    eprintln!(
        "three_layer_min_distance_over_sigma = {:.6}",
        best_params.min_distance_over_sigma
    );
    eprintln!(
        "three_layer_min_confirmation_score = {:.6}",
        best_params.min_confirmation_score
    );
    eprintln!(
        "three_layer_require_confirmation = {}",
        best_params.require_confirmation
    );
    eprintln!(
        "three_layer_min_drift_confirmation = {:.8}",
        best_params.min_drift_confirmation
    );
    eprintln!("three_layer_min_edge = {:.6}", best_params.min_edge);
    eprintln!(
        "three_layer_min_reward_risk = {:.6}",
        best_params.min_reward_risk
    );
    eprintln!(
        "three_layer_min_entry_score = {:.6}",
        best_params.min_entry_score
    );
    eprintln!(
        "three_layer_probability_shrink = {:.6}",
        best_params.probability_shrink
    );
    eprintln!(
        "three_layer_probability_haircut = {:.6}",
        best_params.probability_haircut
    );
    eprintln!(
        "three_layer_alpha_contrarian = {}",
        best_params.alpha_contrarian
    );
    eprintln!(
        "three_layer_cex_contrarian = {}",
        best_params.cex_contrarian
    );
    eprintln!("cooldown_secs = {}", best_params.cooldown_secs);
    eprintln!(
        "min_time_remaining_secs = {}",
        best_params.min_time_remaining_secs
    );
    eprintln!(
        "max_time_remaining_secs = {}",
        best_params.max_time_remaining_secs
    );
    eprintln!("# stop-loss/take-profit path exits require a future path replay label.");

    let trials = study.trials();
    let summary = OptimizeSummary {
        snapshot_schema: &snapshot.manifest.schema_version,
        snapshot_hash,
        snapshot_generated_at: snapshot.manifest.generated_at,
        snapshot_data_requirements: &snapshot.manifest.data_requirements,
        snapshot_data_audit_status: snapshot.manifest.data_audit_status.as_deref(),
        snapshot_data_audit_report: snapshot.manifest.data_audit_report.as_deref(),
        snapshot_include_deribit: snapshot.manifest.include_deribit,
        symbols: &symbols,
        train_start,
        train_end,
        val_start,
        val_end,
        strategy_profile,
        trials: n_trials,
        stake_usd,
        selection_objective,
        min_trades,
        min_trades_source,
        train_opportunities,
        val_opportunities,
        train_rows: train_rows.len(),
        val_rows: val_rows.len(),
        train_underpowered,
        validation_underpowered,
        best_params,
        train_metrics,
        val_metrics,
    };
    write_outputs(output_dir, &summary, &trials)?;
    if train_underpowered || validation_underpowered {
        anyhow::bail!(
            "snapshot optimizer result is underpowered: train_trades={} validation_trades={} min_trades={}",
            summary.train_metrics.trades,
            summary.val_metrics.trades,
            summary.min_trades
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        calibrate_direction_probability, calibrated_model_probability,
        calibrated_profile_probability, cex_direction_probability, compounded_log_growth,
        default_min_trades_from_coverage, direction_alpha_probability,
        direction_probability_search_floor, directional_score, evaluate_snapshot_objective,
        executable_edge_score, expected_value_per_share, expected_value_per_staked_dollar,
        holistic_selection_objective, max_drawdown, min_edge_search_bounds, parse_date_end,
        profile_direction_probability, profile_selection_objective, reward_risk_ratio,
        row_passes_gates, sample_power_multiplier, stable_compounding_objective,
        stable_direction_confirmation_score, stable_reversal_confirmation_score,
        stable_reversal_soft_confirmation_score, trade_sharpe, transformed_model_probability,
        SnapshotObjectiveMetrics, SnapshotThreeLayerParams, StableObjectiveInputs, StrategyProfile,
        OPTIMIZER_MAX_DIRECTION_PROB, OPTIMIZER_MIN_DIRECTION_PROB,
        STABLE_DIRECTION_MIN_DIRECTION_PROB, STABLE_REVERSAL_FILLABLE_MIN_DIRECTION_PROB,
        STABLE_REVERSAL_SOFT_MIN_DIRECTION_PROB,
    };
    use chrono::{TimeZone, Utc};
    use ploy_research::{FactorObservationV2, Regime, ReviewSide};

    fn test_params() -> SnapshotThreeLayerParams {
        SnapshotThreeLayerParams {
            min_direction_prob: 0.55,
            min_distance_over_sigma: 0.0,
            min_confirmation_score: 0.0,
            require_confirmation: false,
            min_drift_confirmation: 0.0,
            min_edge: 0.0,
            min_reward_risk: 0.5,
            min_entry_score: 0.1,
            alpha_contrarian: false,
            cex_contrarian: false,
            probability_shrink: 1.0,
            probability_haircut: 0.0,
            cooldown_secs: 30,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 180,
        }
    }

    fn metrics_for_selection(
        trades: usize,
        net_pnl: f64,
        objective: f64,
    ) -> SnapshotObjectiveMetrics {
        let avg_realized_return_per_stake = if trades == 0 {
            f64::NAN
        } else {
            net_pnl / 15.0 / trades as f64
        };
        let expectancy_calibration_gap = if avg_realized_return_per_stake.is_finite() {
            (0.19 - avg_realized_return_per_stake).max(0.0)
        } else {
            f64::NAN
        };
        SnapshotObjectiveMetrics {
            candidates: trades,
            selected: trades,
            trades,
            rejected_duplicate: 0,
            rejected_cooldown: 0,
            rejected_non_executable: 0,
            net_pnl,
            avg_pnl: if trades == 0 {
                f64::NAN
            } else {
                net_pnl / trades as f64
            },
            avg_entry_price: 0.42,
            avg_reward_risk: 1.35,
            avg_expected_value_per_share: 0.08,
            avg_expected_value_per_stake: 0.19,
            avg_realized_return_per_stake,
            expectancy_calibration_gap,
            sharpe: 2.0,
            max_drawdown: 60.0,
            log_growth: 8.0,
            avg_log_return: if trades == 0 {
                f64::NAN
            } else {
                8.0 / trades as f64
            },
            positive_day_rate: 1.0,
            positive_symbol_rate: 0.8,
            concentration: 0.35,
            reject_rate: 0.0,
            objective,
            fill_rate: 1.0,
            win_rate: 0.45,
        }
    }

    fn test_row(
        event_id: &str,
        side_model_prob: f64,
        entry_ask: f64,
        executable_pnl: Option<f64>,
        fillable: bool,
        ts_offset_secs: i64,
    ) -> FactorObservationV2 {
        FactorObservationV2 {
            event_id: event_id.to_string(),
            symbol: "BTCUSDT".to_string(),
            tick_ts: Utc.with_ymd_and_hms(2026, 4, 25, 0, 0, 0).unwrap()
                + chrono::Duration::seconds(ts_offset_secs),
            time_remaining_secs: 120,
            official_resolution_observed_at: None,
            regime: Regime::Middle,
            side: ReviewSide::Up,
            pm_token_id: "up-token".to_string(),
            side_model_prob,
            side_chainlink_prob: f64::NAN,
            side_fair_prob: side_model_prob,
            side_model_edge: side_model_prob - entry_ask,
            side_distance_over_sigma: 0.25,
            abs_distance_to_beat: 10.0,
            drift_10s: 0.0,
            drift_30s: 0.0,
            post_flip_drift: 0.0,
            sigma_horizon: 1.0,
            vol_gap: 0.0,
            obi_10: 0.0,
            depth_imbalance: 0.0,
            depth_acceleration: 0.0,
            microprice_offset_bps: 0.0,
            cex_spread_bps: 1.0,
            cum_mprice_drift_5m: 0.0,
            cum_trade_imbalance_5m: 0.0,
            obi_delta_10s_side: 0.0,
            obi_delta_30s_side: 0.0,
            obi_persistence_30s_side: 0.0,
            obi_flip_count_60s: 0.0,
            depth_imbalance_delta_30s_side: 0.0,
            microprice_momentum_30s_side: 0.0,
            trade_imbalance_delta_10s_side: 0.0,
            trade_imbalance_delta_30s_side: 0.0,
            cex_bar_return_30s: 0.0,
            cex_bar_return_60s: 0.0,
            cex_bar_volume_ratio_30s: 1.0,
            cex_bar_volume_trend_3: 0.0,
            cex_signed_volume_ratio_30s: 0.0,
            cex_consecutive_bar_side: 0.0,
            cex_breakout_volume_side: 0.0,
            cex_continuation_score_side: 0.0,
            cex_continuation_edge_gate: 0.0,
            cex_continuation_liquidity_gate: 0.0,
            entry_ask,
            exit_bid: 0.70,
            entry_ask_size: if fillable { 100.0 } else { 0.0 },
            exit_bid_size: if fillable { 100.0 } else { 0.0 },
            opposite_ask: 1.0 - entry_ask,
            opposite_bid: 1.0 - entry_ask - 0.02,
            up_down_ask_sum: 1.0,
            pm_spread_bps: 50.0,
            pm_lag_secs: 1.0,
            entry_ask_change_10s: 0.0,
            entry_ask_change_30s: 0.0,
            exit_bid_change_30s: 0.0,
            pm_spread_change_30s: 0.0,
            entry_size_change_30s: 0.0,
            up_down_ask_sum_change_30s: 0.0,
            pm_reprice_speed_30s: 0.0,
            pm_quote_stability_30s: 1.0,
            deribit_mark_iv: 0.0,
            deribit_bid_iv: 0.0,
            deribit_ask_iv: 0.0,
            deribit_iv_spread: 0.0,
            deribit_iv_lag_secs: 0.0,
            deribit_iv_horizon: 0.0,
            deribit_iv_gap_horizon: 0.0,
            deribit_iv_change_30s: 0.0,
            deribit_iv_change_60s: 0.0,
            deribit_underlying_basis_bps: 0.0,
            deribit_delta: 0.0,
            deribit_gamma: 0.0,
            deribit_vega: 0.0,
            deribit_theta: 0.0,
            stake_usd: 15.0,
            entry_shares: if entry_ask > 0.0 {
                15.0 / entry_ask
            } else {
                0.0
            },
            entry_fee_usd: 0.0,
            entry_capacity_ratio: if fillable { 10.0 } else { 0.0 },
            exit_capacity_ratio: if fillable { 10.0 } else { 0.0 },
            entry_liquidity_usd: if fillable { 1500.0 } else { 0.0 },
            exit_liquidity_usd: if fillable { 1500.0 } else { 0.0 },
            liquidity_shortfall_usd: if fillable { 0.0 } else { 15.0 },
            slippage_to_fill_15u_bps: 0.0,
            entry_sweep_avg_price_15u: entry_ask,
            exit_sweep_avg_price_15u: 0.70,
            entry_sweep_shares_15u: if entry_ask > 0.0 {
                15.0 / entry_ask
            } else {
                0.0
            },
            entry_sweep_fee_usd_15u: 0.0,
            exit_sweep_shares_15u: 15.0 / 0.70,
            entry_sweep_levels_15u: if fillable { 1.0 } else { 0.0 },
            exit_sweep_levels_15u: if fillable { 1.0 } else { 0.0 },
            entry_sweep_slippage_bps: 0.0,
            exit_sweep_slippage_bps: 0.0,
            conservative_entry_sweep_avg_price_15u: entry_ask,
            conservative_entry_sweep_shares_15u: if entry_ask > 0.0 {
                15.0 / entry_ask
            } else {
                0.0
            },
            conservative_entry_sweep_levels_15u: if fillable { 1.0 } else { 0.0 },
            conservative_entry_sweep_slippage_bps: 0.0,
            roundtrip_cost_usd: 0.0,
            roundtrip_pnl_now_15u: executable_pnl,
            roundtrip_pnl_now_full_depth_15u: executable_pnl,
            portfolio_stake_usd: 15.0,
            portfolio_event_exposure_usd: 15.0,
            same_event_observation_count: 1.0,
            same_event_side_observation_count: 1.0,
            side_is_up: 1.0,
            label_settlement_win: Some(if executable_pnl.unwrap_or(0.0) > 0.0 {
                1.0
            } else {
                0.0
            }),
            label_executable_pnl_15u: executable_pnl,
            label_full_depth_executable_pnl_15u: executable_pnl,
            label_conservative_executable_pnl_15u: executable_pnl,
            label_executable_fillable: fillable,
            label_exit_fillable: fillable,
            label_full_depth_entry_fillable: fillable,
            label_full_depth_exit_fillable: fillable,
            label_conservative_entry_fillable: fillable,
            label_future_exit_bid_change_5s: None,
            label_future_exit_bid_change_10s: None,
            label_future_exit_bid_change_30s: None,
            label_future_exit_bid_change_60s: None,
            label_future_exit_pnl_5s: None,
            label_future_exit_pnl_10s: None,
            label_future_exit_pnl_30s: None,
            label_future_exit_pnl_60s: None,
            label_future_exit_full_depth_pnl_5s: None,
            label_future_exit_full_depth_pnl_10s: None,
            label_future_exit_full_depth_pnl_30s: None,
            label_future_exit_full_depth_pnl_60s: None,
            label_future_exit_full_depth_value_5s: None,
            label_future_exit_full_depth_value_10s: None,
            label_future_exit_full_depth_value_30s: None,
            label_future_exit_full_depth_value_60s: None,
            label_future_exit_fillable_5s: None,
            label_future_exit_fillable_10s: None,
            label_future_exit_fillable_30s: None,
            label_future_exit_fillable_60s: None,
            label_future_exit_full_depth_fillable_5s: None,
            label_future_exit_full_depth_fillable_10s: None,
            label_future_exit_full_depth_fillable_30s: None,
            label_future_exit_full_depth_fillable_60s: None,
        }
    }

    fn with_cex_direction(mut row: FactorObservationV2, direction: f64) -> FactorObservationV2 {
        row.cex_bar_return_30s = 0.003 * direction;
        row.cex_bar_return_60s = 0.006 * direction;
        row.cex_consecutive_bar_side = 3.0 * direction;
        row.cex_continuation_score_side = 0.80 * direction;
        row
    }

    #[test]
    fn date_end_is_exclusive_next_day() {
        assert_eq!(
            parse_date_end("2026-04-27").to_rfc3339(),
            "2026-04-28T00:00:00+00:00"
        );
    }

    #[test]
    fn reward_risk_rejects_invalid_prices() {
        assert!(reward_risk_ratio(0.0).is_nan());
        assert!(reward_risk_ratio(1.0).is_nan());
        assert!(reward_risk_ratio(0.25).is_finite());
    }

    #[test]
    fn trade_sharpe_penalizes_empty_trials() {
        assert_eq!(trade_sharpe(&[]), -100.0);
        assert!(trade_sharpe(&[1.0, 2.0, 3.0]).is_finite());
    }

    #[test]
    fn directional_score_supports_normal_and_contrarian_search() {
        assert!(directional_score(0.62, 0.55, 0.25, false) > 0.0);
        assert!(directional_score(0.48, 0.55, 0.25, true) > 0.0);
        assert!(directional_score(0.48, 0.55, 0.25, false) < 0.0);
        assert_eq!(directional_score(f64::NAN, 0.55, 0.25, false), -0.50);
    }

    #[test]
    fn contrarian_probability_transforms_before_edge_scoring() {
        assert!((transformed_model_probability(0.22, true) - 0.78).abs() < 1e-9);
        assert!((transformed_model_probability(0.78, false) - 0.78).abs() < 1e-9);
    }

    #[test]
    fn probability_calibration_lowers_rich_entry_ev() {
        let mut params = test_params();
        params.probability_shrink = 0.50;
        params.probability_haircut = 0.03;
        let row = test_row("event-calibrated", 0.70, 0.60, Some(-1.0), true, 0);

        let raw_probability =
            transformed_model_probability(row.side_model_prob, params.alpha_contrarian);
        let calibrated_probability = calibrated_model_probability(&row, &params);

        assert!((calibrated_probability - 0.57).abs() < 1e-9);
        assert!(calibrated_probability < raw_probability);
        assert!(
            expected_value_per_share(calibrated_probability, row.entry_ask)
                < expected_value_per_share(raw_probability, row.entry_ask),
            "calibration should lower overconfident EV on rich executable entries"
        );
        assert!(
            expected_value_per_share(calibrated_probability, row.entry_ask) < 0.0,
            "rich entry should fail EV after shrink and haircut"
        );
    }

    #[test]
    fn calibrate_direction_probability_handles_invalid_inputs() {
        assert!(calibrate_direction_probability(f64::NAN, 0.5, 0.0).is_nan());
        assert_eq!(calibrate_direction_probability(0.70, 2.0, -1.0), 0.70);
    }

    #[test]
    fn executable_edge_score_never_rewards_negative_edge() {
        let mut params = test_params();
        params.alpha_contrarian = true;
        params.min_edge = -0.02;

        assert!(
            executable_edge_score(-0.001, &params) < 0.0,
            "negative edge should remain a penalty even in contrarian mode"
        );
        assert!(executable_edge_score(0.04, &params) > 0.0);
    }

    #[test]
    fn expected_value_uses_direction_probability_and_executable_price() {
        let high_probability_rich_entry = expected_value_per_share(0.70, 0.75);
        let lower_probability_cheap_entry = expected_value_per_share(0.58, 0.35);

        assert!(
            high_probability_rich_entry < 0.0,
            "high direction probability is not enough when the executable ask is too rich"
        );
        assert!(
            lower_probability_cheap_entry > high_probability_rich_entry,
            "expected value must compare probability and executable price together"
        );
        assert!(
            expected_value_per_staked_dollar(0.57, 0.28)
                > expected_value_per_staked_dollar(0.72, 0.70),
            "lower probability can still be better when payoff per staked dollar is higher"
        );
    }

    #[test]
    fn optimizer_search_keeps_direction_probability_meaningful() {
        assert!(
            OPTIMIZER_MIN_DIRECTION_PROB > 0.50,
            "optimizer must not search neutral direction-probability gates"
        );
        assert!(OPTIMIZER_MIN_DIRECTION_PROB < OPTIMIZER_MAX_DIRECTION_PROB);
        assert!(
            direction_probability_search_floor(StrategyProfile::CexDirectionFirst)
                > OPTIMIZER_MIN_DIRECTION_PROB,
            "CEX-direction-first must not solve by hugging the legacy weak direction floor"
        );
    }

    #[test]
    fn snapshot_gate_rejects_cheap_neutral_direction_probability() {
        let mut params = test_params();
        params.min_direction_prob = 0.56;
        params.min_entry_score = 0.0;
        let row = test_row("event-cheap-neutral", 0.51, 0.12, Some(12.0), true, 0);

        assert!(
            !row_passes_gates(&row, &params, StrategyProfile::Champion),
            "cheap executable odds must not pass when directional alpha is below the configured gate"
        );
    }

    #[test]
    fn snapshot_gate_uses_alpha_probability_before_calibrated_ev() {
        let mut params = test_params();
        params.min_direction_prob = 0.56;
        params.probability_shrink = 0.0;
        params.probability_haircut = 0.0;
        params.min_entry_score = 0.0;
        let row = test_row("event-strong-alpha-cheap", 0.70, 0.12, Some(12.0), true, 0);

        assert!(direction_alpha_probability(&row, &params) >= params.min_direction_prob);
        assert!((calibrated_model_probability(&row, &params) - 0.50).abs() < 1e-9);
        assert!(
            row_passes_gates(&row, &params, StrategyProfile::Champion),
            "strong directional alpha should reach calibrated EV scoring instead of being double-gated by probability shrink"
        );
    }

    #[test]
    fn snapshot_hard_confirmation_gate_rejects_weak_obi() {
        let mut params = test_params();
        params.min_direction_prob = 0.56;
        params.min_confirmation_score = 0.05;
        params.min_entry_score = 0.0;
        let row = test_row("event-weak-obi", 0.62, 0.30, Some(2.0), true, 0);

        assert!(
            row_passes_gates(&row, &params, StrategyProfile::ObiSoft),
            "OBI-soft should treat weak confirmation as a score component, not a hard veto"
        );

        params.require_confirmation = true;
        assert!(
            !row_passes_gates(&row, &params, StrategyProfile::ObiHard),
            "OBI-hard must keep direction/EV gates but add a fillable confirmation veto"
        );
    }

    #[test]
    fn cex_direction_first_rejects_cheap_pm_entry_without_cex_direction() {
        let mut params = test_params();
        params.min_direction_prob = 0.56;
        params.min_entry_score = 0.0;
        params.min_reward_risk = 0.20;
        let row = test_row(
            "event-cheap-but-neutral-cex",
            0.90,
            0.12,
            Some(12.0),
            true,
            0,
        );

        assert_eq!(
            cex_direction_probability(&row),
            0.50,
            "neutral CEX state should not inherit PM model probability"
        );
        assert!(
            !row_passes_gates(&row, &params, StrategyProfile::CexDirectionFirst),
            "PM ask/edge alone must not pass the CEX-direction-first selector"
        );
    }

    #[test]
    fn cex_direction_first_uses_cex_probability_before_pm_ev_gate() {
        let mut params = test_params();
        params.min_direction_prob = 0.56;
        params.min_entry_score = 0.0;
        params.min_reward_risk = 0.20;
        let row = with_cex_direction(
            test_row("event-cex-supported", 0.10, 0.30, Some(5.0), true, 0),
            1.0,
        );

        assert!(direction_alpha_probability(&row, &params) < params.min_direction_prob);
        assert!(
            profile_direction_probability(&row, &params, StrategyProfile::CexDirectionFirst)
                >= params.min_direction_prob
        );
        assert!(
            calibrated_profile_probability(&row, &params, StrategyProfile::CexDirectionFirst)
                > 0.65
        );
        assert!(
            row_passes_gates(&row, &params, StrategyProfile::CexDirectionFirst),
            "supported Binance/CEX direction should reach the PM executable EV gate even when the old PM-side model disagrees"
        );
    }

    #[test]
    fn stable_direction_requires_pm_exit_and_cex_edge_confirmation() {
        let mut params = test_params();
        params.min_direction_prob = STABLE_DIRECTION_MIN_DIRECTION_PROB;
        params.min_confirmation_score = 0.10;
        params.require_confirmation = true;
        params.min_edge = 0.15;
        params.min_entry_score = 0.0;
        params.probability_shrink = StrategyProfile::StableDirection
            .fixes_probability_shrink()
            .unwrap();
        params.probability_haircut = StrategyProfile::StableDirection
            .fixes_probability_haircut()
            .unwrap();

        let weak_row = test_row("event-stable-weak", 0.85, 0.25, Some(8.0), true, 0);
        assert!(
            !row_passes_gates(&weak_row, &params, StrategyProfile::StableDirection),
            "stable_direction should not pass on probability and price without stable PM/CEX confirmation"
        );

        let mut confirmed_row = test_row("event-stable-confirmed", 0.85, 0.25, Some(8.0), true, 0);
        confirmed_row.cex_continuation_edge_gate = 0.08;
        confirmed_row.exit_bid_change_30s = 0.08;
        confirmed_row.pm_reprice_speed_30s = 0.08 / 30.0;

        assert!(stable_direction_confirmation_score(&confirmed_row) >= 0.90);
        assert!(
            row_passes_gates(&confirmed_row, &params, StrategyProfile::StableDirection),
            "stable_direction should pass only when direction probability, executable EV, fillability, and stable confirmation agree"
        );
    }

    #[test]
    fn stable_reversal_uses_inverted_alpha_with_pm_exit_confirmation() {
        let mut params = test_params();
        params.alpha_contrarian = StrategyProfile::StableReversal
            .fixes_alpha_contrarian()
            .unwrap();
        params.min_direction_prob = STABLE_DIRECTION_MIN_DIRECTION_PROB;
        params.min_confirmation_score = 0.10;
        params.require_confirmation = true;
        params.min_edge = 0.15;
        params.min_entry_score = 0.0;
        params.probability_shrink = StrategyProfile::StableReversal
            .fixes_probability_shrink()
            .unwrap();
        params.probability_haircut = StrategyProfile::StableReversal
            .fixes_probability_haircut()
            .unwrap();

        let mut row = test_row("event-stable-reversal", 0.22, 0.25, Some(8.0), true, 0);
        row.exit_bid_change_30s = 0.08;
        row.entry_ask_change_30s = 0.04;
        row.pm_reprice_speed_30s = 0.04 / 30.0;

        assert!(direction_alpha_probability(&row, &params) >= params.min_direction_prob);
        assert!(stable_reversal_confirmation_score(&row) >= params.min_confirmation_score);
        assert!(
            row_passes_gates(&row, &params, StrategyProfile::StableReversal),
            "stable_reversal should test the observed inverted alpha side only after PM exit-bid confirmation"
        );
        assert!(
            !row_passes_gates(&row, &params, StrategyProfile::StableDirection),
            "stable_direction should still reject the same low model-probability row"
        );
    }

    #[test]
    fn stable_reversal_soft_keeps_inverted_alpha_but_softens_pm_veto() {
        let mut params = test_params();
        params.alpha_contrarian = StrategyProfile::StableReversalSoft
            .fixes_alpha_contrarian()
            .unwrap();
        params.min_direction_prob = STABLE_REVERSAL_SOFT_MIN_DIRECTION_PROB;
        params.min_confirmation_score = 0.0;
        params.require_confirmation = false;
        params.min_edge = 0.08;
        params.min_reward_risk = 0.20;
        params.min_entry_score = 0.0;
        params.probability_shrink = StrategyProfile::StableReversalSoft
            .fixes_probability_shrink()
            .unwrap();
        params.probability_haircut = StrategyProfile::StableReversalSoft
            .fixes_probability_haircut()
            .unwrap();

        let mut row = test_row("event-stable-reversal-soft", 0.22, 0.25, Some(8.0), true, 0);
        row.exit_bid_change_30s = -0.01;
        row.entry_ask_change_30s = 0.06;
        row.pm_reprice_speed_30s = 0.06 / 30.0;
        row.cex_continuation_edge_gate = 0.01;

        assert!(direction_alpha_probability(&row, &params) >= params.min_direction_prob);
        assert!(stable_reversal_soft_confirmation_score(&row).is_finite());
        assert!(
            row_passes_gates(&row, &params, StrategyProfile::StableReversalSoft),
            "soft reversal should let PM dynamics contribute as score instead of requiring positive exit-bid improvement"
        );
        assert!(
            !row_passes_gates(&row, &params, StrategyProfile::StableReversal),
            "hard reversal should still reject the same row because exit_bid_change_30s is not positive"
        );
    }

    #[test]
    fn stable_reversal_fillable_uses_executable_roundtrip_not_full_depth_only() {
        let mut params = test_params();
        params.alpha_contrarian = StrategyProfile::StableReversalFillable
            .fixes_alpha_contrarian()
            .unwrap();
        params.min_direction_prob = STABLE_REVERSAL_FILLABLE_MIN_DIRECTION_PROB;
        params.min_confirmation_score = 0.0;
        params.require_confirmation = false;
        params.min_edge = 0.05;
        params.min_reward_risk = 0.20;
        params.min_entry_score = 0.0;
        params.probability_shrink = StrategyProfile::StableReversalFillable
            .fixes_probability_shrink()
            .unwrap();
        params.probability_haircut = StrategyProfile::StableReversalFillable
            .fixes_probability_haircut()
            .unwrap();

        let mut row = test_row(
            "event-stable-reversal-fillable",
            0.22,
            0.25,
            Some(8.0),
            true,
            0,
        );
        row.label_full_depth_entry_fillable = false;
        row.label_full_depth_exit_fillable = false;
        row.label_full_depth_executable_pnl_15u = None;
        row.exit_bid_change_30s = -0.01;
        row.entry_ask_change_30s = 0.05;
        row.pm_reprice_speed_30s = 0.05 / 30.0;

        assert!(
            row_passes_gates(&row, &params, StrategyProfile::StableReversalFillable),
            "fillable reversal should test actual executable round-trip labels without requiring full-depth labels"
        );
        assert!(
            !row_passes_gates(&row, &params, StrategyProfile::StableReversalSoft),
            "soft reversal keeps the stricter full-depth entry+exit gate"
        );
    }

    #[test]
    fn snapshot_gate_rejects_non_fillable_entries_before_selection() {
        let mut params = test_params();
        params.cooldown_secs = 0;
        params.min_entry_score = 0.0;

        let rows = vec![
            test_row("event-fillable", 0.62, 0.30, Some(2.0), true, 0),
            test_row("event-not-fillable", 0.62, 0.30, Some(999.0), false, 1),
        ];
        let metrics =
            evaluate_snapshot_objective(&rows, &params, 1, 15.0, StrategyProfile::Champion);

        assert_eq!(metrics.candidates, 1);
        assert_eq!(metrics.selected, 1);
        assert_eq!(metrics.trades, 1);
        assert_eq!(metrics.rejected_non_executable, 0);
        assert!((metrics.net_pnl - 2.0).abs() < 1e-9);
        assert!(metrics.avg_expected_value_per_stake.is_finite());
    }

    #[test]
    fn stable_objective_prefers_smooth_compounding_over_choppy_sharpe() {
        let smooth = stable_compounding_objective(StableObjectiveInputs {
            trades: 800,
            min_trades: 400,
            stake_usd: 15.0,
            net_pnl: 900.0,
            sharpe: 2.0,
            max_drawdown: 60.0,
            log_growth: 12.0,
            avg_entry_price: 0.42,
            avg_reward_risk: 1.35,
            avg_expected_value_per_stake: 0.19,
            avg_realized_return_per_stake: 0.15,
            expectancy_calibration_gap: 0.04,
            fill_rate: 0.95,
            reject_rate: 0.02,
            positive_day_rate: 1.0,
            positive_symbol_rate: 1.0,
            concentration: 0.25,
        });
        let choppy = stable_compounding_objective(StableObjectiveInputs {
            trades: 800,
            min_trades: 400,
            stake_usd: 15.0,
            net_pnl: 900.0,
            sharpe: 4.5,
            max_drawdown: 450.0,
            log_growth: 2.0,
            avg_entry_price: 0.72,
            avg_reward_risk: 0.35,
            avg_expected_value_per_stake: -0.05,
            avg_realized_return_per_stake: -0.02,
            expectancy_calibration_gap: 0.03,
            fill_rate: 0.70,
            reject_rate: 0.25,
            positive_day_rate: 0.50,
            positive_symbol_rate: 0.50,
            concentration: 0.80,
        });

        assert!(smooth > choppy);
    }

    #[test]
    fn stable_objective_penalizes_predicted_ev_overstatement() {
        let base = StableObjectiveInputs {
            trades: 800,
            min_trades: 400,
            stake_usd: 15.0,
            net_pnl: 900.0,
            sharpe: 2.0,
            max_drawdown: 80.0,
            log_growth: 10.0,
            avg_entry_price: 0.42,
            avg_reward_risk: 1.35,
            avg_expected_value_per_stake: 0.12,
            avg_realized_return_per_stake: 0.10,
            expectancy_calibration_gap: 0.02,
            fill_rate: 0.95,
            reject_rate: 0.02,
            positive_day_rate: 1.0,
            positive_symbol_rate: 1.0,
            concentration: 0.25,
        };
        let honest = stable_compounding_objective(base);
        let overstated = stable_compounding_objective(StableObjectiveInputs {
            avg_expected_value_per_stake: 1.30,
            avg_realized_return_per_stake: 0.02,
            expectancy_calibration_gap: 1.28,
            ..base
        });

        assert!(
            honest > overstated,
            "same realized PnL should score worse when predicted EV greatly exceeds realized return"
        );
    }

    #[test]
    fn drawdown_and_log_growth_capture_compounding_risk() {
        assert_eq!(max_drawdown(&[10.0, -5.0, -20.0, 15.0]), 25.0);
        assert!(compounded_log_growth(&[1.0, 1.0, 1.0], 15.0) > 0.0);
        assert!(
            compounded_log_growth(&[-15.0], 15.0) > -0.1,
            "a fixed-size binary loss should be measured against the research risk budget, not treated as total ruin"
        );
        assert!(
            compounded_log_growth(&[-15.0, 5.0, 5.0, 5.0], 15.0).abs() < 0.01,
            "fixed-dollar sizing should make offsetting PnL roughly neutral in log-growth terms"
        );
    }

    #[test]
    fn default_min_trades_scales_with_event_coverage_not_rows() {
        assert_eq!(default_min_trades_from_coverage(10_368, 3, 6), 156);
        assert_eq!(default_min_trades_from_coverage(1_000, 1, 2), 40);
        assert_eq!(default_min_trades_from_coverage(200_000, 7, 6), 1_500);
    }

    #[test]
    fn holistic_selection_requires_powered_train_and_validation_windows() {
        let train = metrics_for_selection(300, 900.0, 10.0);
        let sparse_validation = metrics_for_selection(39, 300.0, 8.0);
        let powered_validation = metrics_for_selection(160, 300.0, 8.0);

        assert!(
            holistic_selection_objective(&train, &sparse_validation, 40) < -999_000.0,
            "sparse validation should remain rejected even when profitable"
        );
        assert!(holistic_selection_objective(&train, &powered_validation, 40) > 0.0);
    }

    #[test]
    fn holistic_selection_rejects_non_profitable_validation() {
        let train = metrics_for_selection(300, 900.0, 10.0);
        let validation_loss = metrics_for_selection(160, -50.0, 8.0);

        assert!(holistic_selection_objective(&train, &validation_loss, 40) < -9_000.0);
    }

    #[test]
    fn holistic_selection_penalizes_validation_calibration_gap() {
        let train = metrics_for_selection(300, 900.0, 10.0);
        let mut calibrated_validation = metrics_for_selection(160, 300.0, 8.0);
        calibrated_validation.avg_expected_value_per_stake = 0.12;
        calibrated_validation.avg_realized_return_per_stake = 0.11;
        calibrated_validation.expectancy_calibration_gap = 0.01;

        let mut overstated_validation = calibrated_validation.clone();
        overstated_validation.avg_expected_value_per_stake = 1.20;
        overstated_validation.avg_realized_return_per_stake = 0.05;
        overstated_validation.expectancy_calibration_gap = 1.15;

        assert!(
            holistic_selection_objective(&train, &calibrated_validation, 40)
                > holistic_selection_objective(&train, &overstated_validation, 40)
        );
    }

    #[test]
    fn stable_direction_profile_hardens_ev_gap_and_min_edge_search() {
        assert_eq!(
            min_edge_search_bounds(StrategyProfile::StableDirection),
            (0.15, 0.45)
        );
        let train = metrics_for_selection(300, 900.0, 10.0);
        let mut calibrated_validation = metrics_for_selection(160, 300.0, 8.0);
        calibrated_validation.fill_rate = 1.0;
        calibrated_validation.avg_realized_return_per_stake = 0.15;
        calibrated_validation.expectancy_calibration_gap = 0.02;

        let mut overstated_validation = calibrated_validation.clone();
        overstated_validation.expectancy_calibration_gap = 0.60;

        assert!(
            profile_selection_objective(
                &train,
                &calibrated_validation,
                40,
                StrategyProfile::StableDirection
            ) > profile_selection_objective(
                &train,
                &overstated_validation,
                40,
                StrategyProfile::StableDirection
            )
        );
    }

    #[test]
    fn sample_power_multiplier_penalizes_threshold_hugging() {
        assert_eq!(sample_power_multiplier(499, 500), 0.0);
        assert!(sample_power_multiplier(500, 500) < 0.6);
        assert_eq!(sample_power_multiplier(2_000, 500), 1.0);
        assert_eq!(sample_power_multiplier(3_000, 500), 1.0);
    }

    #[test]
    fn strategy_profile_parses_operator_aliases() {
        assert_eq!(
            StrategyProfile::parse("mixed").unwrap(),
            StrategyProfile::Mixed
        );
        assert_eq!(
            StrategyProfile::parse("A").unwrap(),
            StrategyProfile::Champion
        );
        assert_eq!(
            StrategyProfile::parse("obi").unwrap(),
            StrategyProfile::ObiSoft
        );
        assert_eq!(
            StrategyProfile::parse("obi_hard").unwrap(),
            StrategyProfile::ObiHard
        );
        assert_eq!(
            StrategyProfile::parse("cex_continuation").unwrap(),
            StrategyProfile::ContinuationSoft
        );
        assert_eq!(
            StrategyProfile::parse("spread_adjusted_external_move").unwrap(),
            StrategyProfile::RepricingMomentum
        );
        assert_eq!(
            StrategyProfile::parse("cex_direction_first").unwrap(),
            StrategyProfile::CexDirectionFirst
        );
        assert_eq!(
            StrategyProfile::parse("stable_direction").unwrap(),
            StrategyProfile::StableDirection
        );
        assert_eq!(
            StrategyProfile::parse("stable_reversal").unwrap(),
            StrategyProfile::StableReversal
        );
        assert_eq!(
            StrategyProfile::parse("reversal_pm_soft").unwrap(),
            StrategyProfile::StableReversalSoft
        );
        assert_eq!(
            StrategyProfile::parse("reversal_fillable").unwrap(),
            StrategyProfile::StableReversalFillable
        );
        assert!(StrategyProfile::parse("unknown").is_err());
    }

    #[test]
    fn three_arm_profiles_share_contrarian_alpha_base() {
        assert_eq!(
            StrategyProfile::Champion.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::ObiSoft.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::ObiHard.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::ContinuationSoft.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::RepricingMomentum.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::CexDirectionFirst.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::StableDirection.fixes_alpha_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableReversal.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::StableReversalSoft.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::StableReversalFillable.fixes_alpha_contrarian(),
            Some(true)
        );
        assert_eq!(StrategyProfile::Mixed.fixes_alpha_contrarian(), None);
        assert_eq!(
            StrategyProfile::Champion.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::CexDirectionFirst.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableDirection.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableReversal.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableReversalSoft.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableReversalFillable.fixes_cex_contrarian(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::Champion.fixes_confirmation_threshold(),
            Some(0.0)
        );
        assert_eq!(
            StrategyProfile::CexDirectionFirst.fixes_confirmation_threshold(),
            Some(0.0)
        );
        assert_eq!(
            StrategyProfile::StableDirection.fixes_confirmation_threshold(),
            Some(0.10)
        );
        assert_eq!(
            StrategyProfile::StableReversal.fixes_confirmation_threshold(),
            Some(0.10)
        );
        assert_eq!(
            StrategyProfile::StableReversalSoft.fixes_confirmation_threshold(),
            None
        );
        assert_eq!(
            StrategyProfile::StableReversalFillable.fixes_confirmation_threshold(),
            None
        );
        assert_eq!(StrategyProfile::ObiHard.fixes_cex_contrarian(), Some(false));
        assert_eq!(
            StrategyProfile::ObiHard.fixes_require_confirmation(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::CexDirectionFirst.fixes_require_confirmation(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableDirection.fixes_require_confirmation(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::StableReversal.fixes_require_confirmation(),
            Some(true)
        );
        assert_eq!(
            StrategyProfile::StableReversalSoft.fixes_require_confirmation(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableReversalFillable.fixes_require_confirmation(),
            Some(false)
        );
        assert_eq!(
            StrategyProfile::StableDirection.fixes_probability_shrink(),
            Some(0.38)
        );
        assert_eq!(
            StrategyProfile::StableDirection.fixes_probability_haircut(),
            Some(0.04)
        );
        assert_eq!(
            StrategyProfile::StableReversal.fixes_probability_shrink(),
            Some(0.38)
        );
        assert_eq!(
            StrategyProfile::StableReversal.fixes_probability_haircut(),
            Some(0.04)
        );
        assert_eq!(
            StrategyProfile::StableReversalSoft.fixes_probability_shrink(),
            Some(0.38)
        );
        assert_eq!(
            StrategyProfile::StableReversalSoft.fixes_probability_haircut(),
            Some(0.04)
        );
        assert_eq!(
            StrategyProfile::StableReversalFillable.fixes_probability_shrink(),
            Some(0.38)
        );
        assert_eq!(
            StrategyProfile::StableReversalFillable.fixes_probability_haircut(),
            Some(0.04)
        );
        assert_eq!(
            direction_probability_search_floor(StrategyProfile::StableReversalSoft),
            STABLE_REVERSAL_SOFT_MIN_DIRECTION_PROB
        );
        assert_eq!(
            direction_probability_search_floor(StrategyProfile::StableReversalFillable),
            STABLE_REVERSAL_FILLABLE_MIN_DIRECTION_PROB
        );
        assert_eq!(
            min_edge_search_bounds(StrategyProfile::StableReversalSoft),
            (0.08, 0.35)
        );
        assert_eq!(
            min_edge_search_bounds(StrategyProfile::StableReversalFillable),
            (0.05, 0.30)
        );
    }
}
