use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use ploy_operator_contracts::Regime;
use serde::{Deserialize, Serialize};

use crate::autofactor::{LlmPriorSpec, LlmProbabilityBlendSpec};
use crate::factors::{
    normalized_underlying_symbol, pearson_ic, spearman_ic, FactorObservation,
    ResearchPmBookSnapshot,
};

const DEFAULT_STAKE_USD: f64 = 15.0;
const DEFAULT_TOP_QUANTILE: f64 = 0.2;
const CONSERVATIVE_VISIBLE_DEPTH_HAIRCUT: f64 = 0.5;
const CONSERVATIVE_MAX_SWEEP_LEVELS: usize = 3;
const PM_BOOK_MAX_AGE_SECS: i64 = 30;
const EPS: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactorFamily {
    Alpha,
    CexLob,
    CexAggTrade,
    PmLiquidity,
    PmDynamics,
    DeribitVol,
    Execution,
    Regime,
    Exit,
    PortfolioRisk,
}

impl FactorFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            FactorFamily::Alpha => "alpha",
            FactorFamily::CexLob => "cex_lob",
            FactorFamily::CexAggTrade => "cex_agg_trade",
            FactorFamily::PmLiquidity => "pm_liquidity",
            FactorFamily::PmDynamics => "pm_dynamics",
            FactorFamily::DeribitVol => "deribit_vol",
            FactorFamily::Execution => "execution",
            FactorFamily::Regime => "regime",
            FactorFamily::Exit => "exit",
            FactorFamily::PortfolioRisk => "portfolio_risk",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreeLayerArchive {
    DirectionProbabilityEdge,
    CexMicrostructureConfirmation,
    PmExecutableLiquidityRiskGate,
}

impl ThreeLayerArchive {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreeLayerArchive::DirectionProbabilityEdge => "layer1_direction_probability_edge",
            ThreeLayerArchive::CexMicrostructureConfirmation => {
                "layer2_cex_microstructure_confirmation"
            }
            ThreeLayerArchive::PmExecutableLiquidityRiskGate => {
                "layer3_pm_executable_liquidity_risk_gate"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewSide {
    Up,
    Down,
}

impl ReviewSide {
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewSide::Up => "up",
            ReviewSide::Down => "down",
        }
    }

    pub fn multiplier(self) -> f64 {
        match self {
            ReviewSide::Up => 1.0,
            ReviewSide::Down => -1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FactorReviewOptions {
    pub stake_usd: f64,
    pub min_observations: usize,
    pub top_quantile: f64,
    pub max_quote_age_secs: i64,
}

impl Default for FactorReviewOptions {
    fn default() -> Self {
        Self {
            stake_usd: DEFAULT_STAKE_USD,
            min_observations: 20,
            top_quantile: DEFAULT_TOP_QUANTILE,
            max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeribitFeatureSnapshot {
    pub symbol: String,
    pub ts: DateTime<Utc>,
    pub mark_iv: f64,
    pub bid_iv: f64,
    pub ask_iv: f64,
    pub underlying_price: f64,
    pub delta: f64,
    pub gamma: f64,
    pub vega: f64,
    pub theta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorObservationV2 {
    pub event_id: String,
    pub symbol: String,
    pub tick_ts: DateTime<Utc>,
    pub time_remaining_secs: i64,
    pub regime: Regime,
    pub side: ReviewSide,
    #[serde(default)]
    pub pm_token_id: String,

    pub side_model_prob: f64,
    #[serde(default = "nan_f64")]
    pub side_chainlink_prob: f64,
    pub side_fair_prob: f64,
    pub side_model_edge: f64,
    pub side_distance_over_sigma: f64,
    pub abs_distance_to_beat: f64,
    pub drift_10s: f64,
    pub drift_30s: f64,
    pub post_flip_drift: f64,
    pub sigma_horizon: f64,
    pub vol_gap: f64,
    pub obi_10: f64,
    pub depth_imbalance: f64,
    pub depth_acceleration: f64,
    pub microprice_offset_bps: f64,
    pub cex_spread_bps: f64,
    pub cum_mprice_drift_5m: f64,
    pub cum_trade_imbalance_5m: f64,
    pub obi_delta_10s_side: f64,
    pub obi_delta_30s_side: f64,
    pub obi_persistence_30s_side: f64,
    pub obi_flip_count_60s: f64,
    pub depth_imbalance_delta_30s_side: f64,
    pub microprice_momentum_30s_side: f64,
    pub trade_imbalance_delta_10s_side: f64,
    pub trade_imbalance_delta_30s_side: f64,
    pub cex_bar_return_30s: f64,
    pub cex_bar_return_60s: f64,
    pub cex_bar_volume_ratio_30s: f64,
    pub cex_bar_volume_trend_3: f64,
    pub cex_signed_volume_ratio_30s: f64,
    pub cex_consecutive_bar_side: f64,
    pub cex_breakout_volume_side: f64,
    pub cex_continuation_score_side: f64,
    pub cex_continuation_edge_gate: f64,
    pub cex_continuation_liquidity_gate: f64,

    pub entry_ask: f64,
    pub exit_bid: f64,
    pub entry_ask_size: f64,
    pub exit_bid_size: f64,
    pub opposite_ask: f64,
    pub opposite_bid: f64,
    pub up_down_ask_sum: f64,
    pub pm_spread_bps: f64,
    pub pm_lag_secs: f64,
    pub entry_ask_change_10s: f64,
    pub entry_ask_change_30s: f64,
    pub exit_bid_change_30s: f64,
    pub pm_spread_change_30s: f64,
    pub entry_size_change_30s: f64,
    pub up_down_ask_sum_change_30s: f64,
    pub pm_reprice_speed_30s: f64,
    pub pm_quote_stability_30s: f64,

    pub deribit_mark_iv: f64,
    pub deribit_bid_iv: f64,
    pub deribit_ask_iv: f64,
    pub deribit_iv_spread: f64,
    pub deribit_iv_lag_secs: f64,
    pub deribit_iv_horizon: f64,
    pub deribit_iv_gap_horizon: f64,
    pub deribit_iv_change_30s: f64,
    pub deribit_iv_change_60s: f64,
    pub deribit_underlying_basis_bps: f64,
    pub deribit_delta: f64,
    pub deribit_gamma: f64,
    pub deribit_vega: f64,
    pub deribit_theta: f64,

    pub stake_usd: f64,
    pub entry_shares: f64,
    pub entry_fee_usd: f64,
    pub entry_capacity_ratio: f64,
    pub exit_capacity_ratio: f64,
    pub entry_liquidity_usd: f64,
    pub exit_liquidity_usd: f64,
    pub liquidity_shortfall_usd: f64,
    pub slippage_to_fill_15u_bps: f64,
    pub entry_sweep_avg_price_15u: f64,
    pub exit_sweep_avg_price_15u: f64,
    pub entry_sweep_shares_15u: f64,
    #[serde(default = "nan_f64")]
    pub entry_sweep_fee_usd_15u: f64,
    pub exit_sweep_shares_15u: f64,
    pub entry_sweep_levels_15u: f64,
    pub exit_sweep_levels_15u: f64,
    pub entry_sweep_slippage_bps: f64,
    pub exit_sweep_slippage_bps: f64,
    pub conservative_entry_sweep_avg_price_15u: f64,
    pub conservative_entry_sweep_shares_15u: f64,
    pub conservative_entry_sweep_levels_15u: f64,
    pub conservative_entry_sweep_slippage_bps: f64,
    pub roundtrip_cost_usd: f64,
    pub roundtrip_pnl_now_15u: Option<f64>,
    pub roundtrip_pnl_now_full_depth_15u: Option<f64>,

    pub portfolio_stake_usd: f64,
    pub portfolio_event_exposure_usd: f64,
    pub same_event_observation_count: f64,
    pub same_event_side_observation_count: f64,
    pub side_is_up: f64,

    pub label_settlement_win: Option<f64>,
    pub label_executable_pnl_15u: Option<f64>,
    pub label_full_depth_executable_pnl_15u: Option<f64>,
    pub label_conservative_executable_pnl_15u: Option<f64>,
    pub label_executable_fillable: bool,
    pub label_exit_fillable: bool,
    pub label_full_depth_entry_fillable: bool,
    pub label_full_depth_exit_fillable: bool,
    pub label_conservative_entry_fillable: bool,
    pub label_future_exit_bid_change_5s: Option<f64>,
    pub label_future_exit_bid_change_10s: Option<f64>,
    pub label_future_exit_bid_change_30s: Option<f64>,
    pub label_future_exit_bid_change_60s: Option<f64>,
    pub label_future_exit_pnl_5s: Option<f64>,
    pub label_future_exit_pnl_10s: Option<f64>,
    pub label_future_exit_pnl_30s: Option<f64>,
    pub label_future_exit_pnl_60s: Option<f64>,
    pub label_future_exit_full_depth_pnl_5s: Option<f64>,
    pub label_future_exit_full_depth_pnl_10s: Option<f64>,
    pub label_future_exit_full_depth_pnl_30s: Option<f64>,
    pub label_future_exit_full_depth_pnl_60s: Option<f64>,
    pub label_future_exit_full_depth_value_5s: Option<f64>,
    pub label_future_exit_full_depth_value_10s: Option<f64>,
    pub label_future_exit_full_depth_value_30s: Option<f64>,
    pub label_future_exit_full_depth_value_60s: Option<f64>,
    pub label_future_exit_fillable_5s: Option<f64>,
    pub label_future_exit_fillable_10s: Option<f64>,
    pub label_future_exit_fillable_30s: Option<f64>,
    pub label_future_exit_fillable_60s: Option<f64>,
    pub label_future_exit_full_depth_fillable_5s: Option<f64>,
    pub label_future_exit_full_depth_fillable_10s: Option<f64>,
    pub label_future_exit_full_depth_fillable_30s: Option<f64>,
    pub label_future_exit_full_depth_fillable_60s: Option<f64>,
}

#[derive(Clone, Copy)]
pub struct FactorV2Descriptor {
    pub name: &'static str,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub accessor: fn(&FactorObservationV2) -> f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DataHealthReport {
    pub source_observations: usize,
    pub v2_rows: usize,
    pub settlement_label_rows: usize,
    pub entry_quote_rows: usize,
    pub exit_quote_rows: usize,
    pub entry_size_rows: usize,
    pub exit_size_rows: usize,
    pub entry_fillable_rows: usize,
    pub exit_fillable_rows: usize,
    pub entry_full_depth_fillable_rows: usize,
    pub exit_full_depth_fillable_rows: usize,
    pub executable_pnl_rows: usize,
    pub full_depth_executable_pnl_rows: usize,
    pub deribit_rows: usize,
    pub avg_pm_lag_secs: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_entry_sweep_slippage_bps: f64,
    pub avg_exit_sweep_slippage_bps: f64,
}

impl DataHealthReport {
    pub fn entry_fill_rate(&self) -> f64 {
        ratio(self.entry_fillable_rows, self.v2_rows)
    }

    pub fn exit_fill_rate(&self) -> f64 {
        ratio(self.exit_fillable_rows, self.v2_rows)
    }

    pub fn rejection_rate(&self) -> f64 {
        1.0 - self.entry_fill_rate()
    }

    pub fn full_depth_entry_fill_rate(&self) -> f64 {
        ratio(self.entry_full_depth_fillable_rows, self.v2_rows)
    }

    pub fn full_depth_exit_fill_rate(&self) -> f64 {
        ratio(self.exit_full_depth_fillable_rows, self.v2_rows)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FullDepthExecutionMatrixOptions {
    pub stakes_usd: Vec<f64>,
    pub visible_depth_haircut: f64,
    pub max_levels: Option<usize>,
    pub min_bucket_observations: usize,
    pub max_quote_age_secs: i64,
}

impl Default for FullDepthExecutionMatrixOptions {
    fn default() -> Self {
        Self {
            stakes_usd: vec![1.0, 3.0, 5.0, 10.0, 15.0],
            visible_depth_haircut: 1.0,
            max_levels: None,
            min_bucket_observations: 20,
            max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FullDepthExecutionMatrixRow {
    pub stake_usd: f64,
    pub symbol: String,
    pub side: ReviewSide,
    pub time_bucket: String,
    pub distance_bucket: String,
    pub entry_price_bucket: String,
    pub spread_bucket: String,
    pub quote_age_bucket: String,
    pub count: usize,
    pub entry_fill_rate: f64,
    pub entry_avg_price_mean: f64,
    pub entry_avg_slippage_bps: f64,
    pub entry_p50_slippage_bps: f64,
    pub entry_p90_slippage_bps: f64,
    pub entry_avg_levels_used: f64,
    pub exit_5s_fill_rate: f64,
    pub exit_10s_fill_rate: f64,
    pub exit_30s_fill_rate: f64,
    pub exit_10s_avg_slippage_bps: f64,
    pub exit_30s_avg_slippage_bps: f64,
    pub roundtrip_fill_rate_5s: f64,
    pub roundtrip_fill_rate_10s: f64,
    pub roundtrip_fill_rate_30s: f64,
    pub avg_settlement_pnl: f64,
    pub avg_reprice_pnl_5s: f64,
    pub avg_reprice_pnl_10s: f64,
    pub avg_reprice_pnl_30s: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FullDepthExecutionMatrixReport {
    pub options: FullDepthExecutionMatrixOptions,
    pub rows: Vec<FullDepthExecutionMatrixRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityReportOptions {
    pub bucket_count: usize,
    pub min_bucket_observations: usize,
    pub top_edge_quantile: f64,
    pub event_surface_min_bucket_observations: usize,
    pub event_surface_shrinkage_observations: usize,
}

impl Default for SettlementProbabilityReportOptions {
    fn default() -> Self {
        Self {
            bucket_count: 10,
            min_bucket_observations: 20,
            top_edge_quantile: 0.2,
            event_surface_min_bucket_observations: 10,
            event_surface_shrinkage_observations: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SettlementProbabilityWalkForwardOptions {
    pub walk_forward: FactorWalkForwardOptions,
    pub probability: SettlementProbabilityReportOptions,
    pub max_test_brier_score: f64,
    pub max_test_log_loss: f64,
    pub max_test_expected_calibration_error: f64,
}

impl Default for SettlementProbabilityWalkForwardOptions {
    fn default() -> Self {
        Self {
            walk_forward: FactorWalkForwardOptions::default(),
            probability: SettlementProbabilityReportOptions::default(),
            max_test_brier_score: 0.25,
            max_test_log_loss: std::f64::consts::LN_2,
            max_test_expected_calibration_error: 0.05,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityBaselineRow {
    pub model: String,
    pub n: usize,
    pub avg_predicted_q: f64,
    pub actual_win_rate: f64,
    pub brier_score: f64,
    pub log_loss: f64,
    pub expected_calibration_error: f64,
    pub avg_edge: f64,
    pub avg_full_depth_settlement_pnl: f64,
    pub avg_conservative_settlement_pnl: f64,
    pub conservative_coverage_rate: f64,
    pub profit_factor: f64,
    pub edge_bucket_monotonic_non_decreasing: bool,
    pub top_edge_count: usize,
    pub top_edge_avg_edge: f64,
    pub top_edge_win_rate: f64,
    pub top_edge_avg_full_depth_settlement_pnl: f64,
    pub top_edge_avg_conservative_settlement_pnl: f64,
    pub top_edge_conservative_coverage_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityCalibrationRow {
    pub model: String,
    pub q_bucket: String,
    pub count: usize,
    pub avg_predicted_q: f64,
    pub actual_win_rate: f64,
    pub calibration_error: f64,
    pub avg_edge: f64,
    pub avg_full_depth_settlement_pnl: f64,
    pub avg_conservative_settlement_pnl: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityEdgeBucketRow {
    pub model: String,
    pub edge_bucket: String,
    pub count: usize,
    pub avg_edge: f64,
    pub avg_predicted_q: f64,
    pub actual_win_rate: f64,
    pub avg_full_depth_entry_price: f64,
    pub avg_full_depth_settlement_pnl: f64,
    pub avg_conservative_settlement_pnl: f64,
    pub profit_factor: f64,
    pub conservative_profit_factor: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityAntiOverfitRow {
    pub model: String,
    pub test: String,
    pub n: usize,
    pub observed_edge_win_rank_ic: f64,
    pub perturbed_edge_win_rank_ic: f64,
    pub observed_top_edge_avg_full_depth_settlement_pnl: f64,
    pub perturbed_top_edge_avg_full_depth_settlement_pnl: f64,
    pub pass: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilitySymbolHoldoutRow {
    pub model: String,
    pub symbol: String,
    pub n: usize,
    pub edge_win_rank_ic: f64,
    pub top_edge_avg_full_depth_settlement_pnl: f64,
    pub pass: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityAblationRow {
    pub model: String,
    pub reference_model: String,
    pub n: usize,
    pub delta_brier_score: f64,
    pub delta_log_loss: f64,
    pub delta_expected_calibration_error: f64,
    pub delta_top_edge_avg_full_depth_settlement_pnl: f64,
    pub improves_error: bool,
    pub improves_top_edge_pnl: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityReport {
    pub options: SettlementProbabilityReportOptions,
    pub baselines: Vec<SettlementProbabilityBaselineRow>,
    pub calibration: Vec<SettlementProbabilityCalibrationRow>,
    pub edge_buckets: Vec<SettlementProbabilityEdgeBucketRow>,
    pub anti_overfit: Vec<SettlementProbabilityAntiOverfitRow>,
    pub symbol_holdouts: Vec<SettlementProbabilitySymbolHoldoutRow>,
    pub ablations: Vec<SettlementProbabilityAblationRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityWalkForwardWindow {
    pub window_index: usize,
    pub model: String,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub train_n: usize,
    pub test_n: usize,
    pub train_brier_score: f64,
    pub test_brier_score: f64,
    pub train_log_loss: f64,
    pub test_log_loss: f64,
    pub train_expected_calibration_error: f64,
    pub test_expected_calibration_error: f64,
    pub train_top_edge_avg_full_depth_settlement_pnl: f64,
    pub test_top_edge_avg_full_depth_settlement_pnl: f64,
    pub test_top_edge_avg_conservative_settlement_pnl: f64,
    pub train_top_edge_conservative_coverage_rate: f64,
    pub test_top_edge_conservative_coverage_rate: f64,
    pub test_edge_bucket_monotonic_non_decreasing: bool,
    pub pass: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityWalkForwardAggregate {
    pub model: String,
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub pass_window_ratio: f64,
    pub avg_test_brier_score: f64,
    pub avg_test_log_loss: f64,
    pub avg_test_expected_calibration_error: f64,
    pub avg_test_top_edge_avg_full_depth_settlement_pnl: f64,
    pub min_test_top_edge_avg_full_depth_settlement_pnl: f64,
    pub avg_test_top_edge_avg_conservative_settlement_pnl: f64,
    pub min_test_top_edge_avg_conservative_settlement_pnl: f64,
    pub min_top_edge_conservative_coverage_rate: f64,
}

#[derive(Debug, Clone)]
pub struct SettlementProbabilityWalkForwardReport {
    pub options: SettlementProbabilityWalkForwardOptions,
    pub windows: Vec<SettlementProbabilityWalkForwardWindow>,
    pub aggregates: Vec<SettlementProbabilityWalkForwardAggregate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictionResearchCandidateFeedback {
    pub model: String,
    pub hypothesis: String,
    pub probability_blend: LlmProbabilityBlendSpec,
    pub verdict: String,
    pub reason_codes: Vec<String>,
    pub metrics: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PredictionResearchFeedback {
    pub schema_version: String,
    pub mission_id: String,
    pub target: String,
    pub symbols: Vec<String>,
    pub horizon: String,
    pub data_snapshot_id: String,
    pub prompt_snapshot_id: String,
    pub search_policy_snapshot_id: String,
    pub candidates: Vec<PredictionResearchCandidateFeedback>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementProbabilityDataQualityMode {
    StrictContinuous,
    EventComplete,
}

impl SettlementProbabilityDataQualityMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::StrictContinuous => "strict_continuous",
            Self::EventComplete => "event_complete",
        }
    }
}

impl Default for SettlementProbabilityDataQualityMode {
    fn default() -> Self {
        Self::StrictContinuous
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityPromotionGateOptions {
    pub stake_usd: f64,
    pub min_entry_fill_rate: f64,
    pub max_expected_calibration_error: f64,
    pub min_positive_window_ratio: f64,
    pub require_deribit: bool,
    pub include_deribit: bool,
    pub data_audit_status: Option<String>,
    pub data_quality_mode: SettlementProbabilityDataQualityMode,
    pub event_complete_events: usize,
    pub event_complete_rows: usize,
    pub min_event_complete_events: usize,
    pub min_event_complete_rows: usize,
    pub global_full_depth_entry_fill_rate: Option<f64>,
    pub replay_parity_ready: bool,
    pub replay_parity_evidence: Option<String>,
}

impl Default for SettlementProbabilityPromotionGateOptions {
    fn default() -> Self {
        Self {
            stake_usd: DEFAULT_STAKE_USD,
            min_entry_fill_rate: 0.30,
            max_expected_calibration_error: 0.05,
            min_positive_window_ratio: 0.60,
            require_deribit: false,
            include_deribit: false,
            data_audit_status: None,
            data_quality_mode: SettlementProbabilityDataQualityMode::StrictContinuous,
            event_complete_events: 0,
            event_complete_rows: 0,
            min_event_complete_events: 20,
            min_event_complete_rows: 40,
            global_full_depth_entry_fill_rate: None,
            replay_parity_ready: false,
            replay_parity_evidence: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityPromotionGateRow {
    pub gate: String,
    pub passed: bool,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettlementProbabilityPromotionGateReport {
    pub options: SettlementProbabilityPromotionGateOptions,
    pub ready_for_dry_run_handoff: bool,
    pub gates: Vec<SettlementProbabilityPromotionGateRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SingleFactorReview {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub n: usize,
    pub coverage: f64,
    pub settlement_pearson_ic: f64,
    pub settlement_rank_ic: f64,
    pub executable_pnl_pearson_ic: f64,
    pub executable_pnl_rank_ic: f64,
    pub selected_n: usize,
    pub selected_rejection_rate: f64,
    pub selected_executable_fill_rate: f64,
    pub selected_avg_slippage_bps: f64,
    pub selected_total_pnl_after_cost: f64,
    pub selected_avg_pnl_after_cost: f64,
    pub selected_sharpe: f64,
    pub selected_max_drawdown: f64,
    pub by_symbol_positive_ratio: f64,
    pub by_time_bucket_positive_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecutableEvBucketSummary {
    pub dimension: String,
    pub bucket: String,
    pub rows: usize,
    pub fillable_rows: usize,
    pub fill_rate: f64,
    pub pnl_rows: usize,
    pub total_pnl_15u: f64,
    pub avg_pnl_15u: f64,
    pub roi_on_stake: f64,
    pub t_stat: f64,
    pub underpowered: bool,
    pub positive_ev: bool,
    pub statistically_supported: bool,
    pub avg_side_model_prob: f64,
    pub avg_side_model_edge: f64,
    pub avg_entry_ask: f64,
    pub avg_exit_bid: f64,
    pub avg_pm_lag_secs: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_entry_liquidity_usd: f64,
    pub avg_exit_liquidity_usd: f64,
    pub avg_liquidity_shortfall_usd: f64,
    pub avg_slippage_to_fill_bps: f64,
    pub avg_entry_sweep_slippage_bps: f64,
    pub avg_exit_sweep_slippage_bps: f64,
    pub avg_roundtrip_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectionSideAuditLegSummary {
    pub rows: usize,
    pub fillable_rows: usize,
    pub fill_rate: f64,
    pub settlement_win_rows: usize,
    pub settlement_win_rate: f64,
    pub pnl_rows: usize,
    pub total_pnl_15u: f64,
    pub avg_pnl_15u: f64,
    pub roi_on_stake: f64,
    pub t_stat: f64,
    pub underpowered: bool,
    pub positive_ev: bool,
    pub statistically_supported: bool,
    pub avg_side_model_prob: f64,
    pub avg_side_model_edge: f64,
    pub avg_entry_ask: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_entry_liquidity_usd: f64,
    pub avg_entry_sweep_slippage_bps: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DirectionSideAuditSummary {
    pub selector: String,
    pub bucket: String,
    pub pairs: usize,
    pub pnl_pair_rows: usize,
    pub total_pnl_delta_15u: f64,
    pub avg_pnl_delta_15u: f64,
    pub avg_selector_margin: f64,
    pub favored: DirectionSideAuditLegSummary,
    pub opposite: DirectionSideAuditLegSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct BinanceDirectionBucketSummary {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub bucket: String,
    pub rows: usize,
    pub fillable_rows: usize,
    pub fill_rate: f64,
    pub settlement_rows: usize,
    pub settlement_win_rows: usize,
    pub settlement_win_rate: f64,
    pub lift_vs_coinflip: f64,
    pub t_stat_vs_coinflip: f64,
    pub pnl_rows: usize,
    pub total_pnl_15u: f64,
    pub avg_pnl_15u: f64,
    pub roi_on_stake: f64,
    pub pnl_t_stat: f64,
    pub positive_ev: bool,
    pub executable_ev_supported: bool,
    pub avg_factor_value: f64,
    pub min_factor_value: f64,
    pub max_factor_value: f64,
    pub avg_entry_ask: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_entry_liquidity_usd: f64,
    pub avg_entry_sweep_slippage_bps: f64,
    pub by_symbol_positive_ratio: f64,
    pub by_time_bucket_positive_ratio: f64,
    pub statistically_supported: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactorReviewV2Report {
    pub options: FactorReviewOptions,
    pub health: DataHealthReport,
    pub reviews: Vec<SingleFactorReview>,
    pub executable_ev_buckets: Vec<ExecutableEvBucketSummary>,
    pub direction_side_audit: Vec<DirectionSideAuditSummary>,
    pub binance_direction_audit: Vec<BinanceDirectionBucketSummary>,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardOptions {
    pub review: FactorReviewOptions,
    pub train_window_days: i64,
    pub test_window_days: i64,
    pub step_days: i64,
    pub train_window_hours: Option<i64>,
    pub test_window_hours: Option<i64>,
    pub step_hours: Option<i64>,
    pub top_n: usize,
    pub factor_name_filter: Option<String>,
}

impl Default for FactorWalkForwardOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            train_window_days: 2,
            test_window_days: 1,
            step_days: 1,
            train_window_hours: None,
            test_window_hours: None,
            step_hours: None,
            top_n: 20,
            factor_name_filter: None,
        }
    }
}

impl FactorWalkForwardOptions {
    pub fn train_duration(&self) -> Duration {
        walk_forward_duration(self.train_window_days, self.train_window_hours)
    }

    pub fn test_duration(&self) -> Duration {
        walk_forward_duration(self.test_window_days, self.test_window_hours)
    }

    pub fn step_duration(&self) -> Duration {
        walk_forward_duration(self.step_days, self.step_hours)
    }

    pub fn train_window_label(&self) -> String {
        walk_forward_duration_label(self.train_window_days, self.train_window_hours)
    }

    pub fn test_window_label(&self) -> String {
        walk_forward_duration_label(self.test_window_days, self.test_window_hours)
    }

    pub fn step_label(&self) -> String {
        walk_forward_duration_label(self.step_days, self.step_hours)
    }
}

fn walk_forward_duration(days: i64, hours: Option<i64>) -> Duration {
    match hours {
        Some(hours) => Duration::hours(hours.max(1)),
        None => Duration::days(days.max(1)),
    }
}

fn walk_forward_duration_label(days: i64, hours: Option<i64>) -> String {
    match hours {
        Some(hours) => format!("{}h", hours.max(1)),
        None => format!("{}d", days.max(1)),
    }
}

#[derive(Debug, Clone)]
pub struct FactorSelectionMetrics {
    pub n: usize,
    pub selected_n: usize,
    pub executable_fill_rate: f64,
    pub rejection_rate: f64,
    pub total_pnl_after_cost: f64,
    pub avg_pnl_after_cost: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub by_symbol_positive_ratio: f64,
    pub by_time_bucket_positive_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardWindow {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub direction: f64,
    pub threshold: f64,
    pub train_settlement_rank_ic: f64,
    pub train_executable_pnl_rank_ic: f64,
    pub train: FactorSelectionMetrics,
    pub test: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardAggregate {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
}

#[derive(Debug, Clone)]
pub struct FactorWalkForwardReport {
    pub options: FactorWalkForwardOptions,
    pub health: DataHealthReport,
    pub windows: Vec<FactorWalkForwardWindow>,
    pub aggregates: Vec<FactorWalkForwardAggregate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactorStabilityDecision {
    Candidate,
    Watchlist,
    Reject,
}

impl FactorStabilityDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            FactorStabilityDecision::Candidate => "candidate",
            FactorStabilityDecision::Watchlist => "watchlist",
            FactorStabilityDecision::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorStabilityOptions {
    pub min_windows: usize,
    pub min_positive_window_ratio: f64,
    pub min_total_test_pnl_after_cost: f64,
    pub min_avg_fill_rate: f64,
    pub max_avg_rejection_rate: f64,
    pub min_by_symbol_positive_ratio: f64,
    pub min_by_time_bucket_positive_ratio: f64,
    pub min_abs_executable_pnl_icir: f64,
}

impl Default for FactorStabilityOptions {
    fn default() -> Self {
        Self {
            min_windows: 8,
            min_positive_window_ratio: 0.65,
            min_total_test_pnl_after_cost: 0.0,
            min_avg_fill_rate: 0.08,
            max_avg_rejection_rate: 0.92,
            min_by_symbol_positive_ratio: 0.5,
            min_by_time_bucket_positive_ratio: 0.5,
            min_abs_executable_pnl_icir: 0.25,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorStabilityRow {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub windows: usize,
    pub settlement_rank_ic_mean: f64,
    pub settlement_rank_icir: f64,
    pub executable_pnl_rank_ic_mean: f64,
    pub executable_pnl_rank_icir: f64,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
    pub avg_by_symbol_positive_ratio: f64,
    pub avg_by_time_bucket_positive_ratio: f64,
    pub decision: FactorStabilityDecision,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct FactorStabilityReport {
    pub options: FactorStabilityOptions,
    pub rows: Vec<FactorStabilityRow>,
}

#[derive(Debug, Clone)]
pub struct RepricingIcOptions {
    pub review: FactorReviewOptions,
    pub min_window_observations: usize,
    pub bucket_count: usize,
    pub factor_name_filter: Option<String>,
}

impl Default for RepricingIcOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions {
                min_observations: 50,
                ..Default::default()
            },
            min_window_observations: 50,
            bucket_count: 5,
            factor_name_filter: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RepricingIcReport {
    pub options: RepricingIcOptions,
    pub health: DataHealthReport,
    pub rows: Vec<RepricingIcRow>,
}

#[derive(Debug, Clone)]
pub struct RepricingIcRow {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub factor_role: &'static str,
    pub target: String,
    pub target_group: &'static str,
    pub n: usize,
    pub pearson_ic: f64,
    pub spearman_ic: f64,
    pub window_count: usize,
    pub window_ic_mean: f64,
    pub icir: f64,
    pub positive_window_ratio: f64,
    pub bottom_bucket_n: usize,
    pub bottom_bucket_avg_label: f64,
    pub top_bucket_n: usize,
    pub top_bucket_avg_label: f64,
    pub top_bucket_positive_label_rate: f64,
    pub bucket_avg_labels: Vec<f64>,
    pub monotonic_non_decreasing: bool,
    pub avg_entry_ask: f64,
    pub avg_pm_spread_bps: f64,
    pub entry_fill_rate: f64,
    pub exit_fill_rate: f64,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Options {
    pub walk_forward: FactorWalkForwardOptions,
    pub max_factors_per_family: usize,
    pub max_total_factors: usize,
    pub min_abs_train_executable_pnl_rank_ic: f64,
}

impl Default for FactorComboV1Options {
    fn default() -> Self {
        Self {
            walk_forward: FactorWalkForwardOptions::default(),
            max_factors_per_family: 2,
            max_total_factors: 12,
            min_abs_train_executable_pnl_rank_ic: 0.03,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FactorComboComponent {
    pub factor: String,
    pub family: FactorFamily,
    pub layer: ThreeLayerArchive,
    pub accessor: fn(&FactorObservationV2) -> f64,
    pub direction: f64,
    pub train_executable_pnl_rank_ic: f64,
    pub train_mean: f64,
    pub train_std: f64,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Window {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub threshold: f64,
    pub components: Vec<FactorComboComponent>,
    pub train: FactorSelectionMetrics,
    pub test: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Aggregate {
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
    pub avg_component_count: f64,
}

#[derive(Debug, Clone)]
pub struct FactorComboV1Report {
    pub options: FactorComboV1Options,
    pub health: DataHealthReport,
    pub windows: Vec<FactorComboV1Window>,
    pub aggregate: FactorComboV1Aggregate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillabilityDecision {
    Candidate,
    Watchlist,
    Reject,
}

impl FillabilityDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            FillabilityDecision::Candidate => "candidate",
            FillabilityDecision::Watchlist => "watchlist",
            FillabilityDecision::Reject => "reject",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FillabilityReviewOptions {
    pub review: FactorReviewOptions,
    pub min_bucket_observations: usize,
    pub min_entry_fill_rate: f64,
    pub min_roundtrip_fill_rate: f64,
    pub max_rejection_rate: f64,
}

impl Default for FillabilityReviewOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            min_bucket_observations: 50,
            min_entry_fill_rate: 0.30,
            min_roundtrip_fill_rate: 0.20,
            max_rejection_rate: 0.70,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FillabilityBucketRow {
    pub dimension: String,
    pub bucket: String,
    pub n: usize,
    pub coverage: f64,
    pub entry_fill_rate: f64,
    pub exit_fill_rate: f64,
    pub roundtrip_fill_rate: f64,
    pub rejection_rate: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_entry_liquidity_usd: f64,
    pub avg_exit_liquidity_usd: f64,
    pub avg_pm_spread_bps: f64,
    pub avg_pm_lag_secs: f64,
    pub avg_slippage_to_fill_bps: f64,
    pub avg_roundtrip_cost_usd: f64,
    pub total_executable_pnl_after_cost: f64,
    pub avg_executable_pnl_after_cost: f64,
    pub decision: FillabilityDecision,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct FillabilityReviewReport {
    pub options: FillabilityReviewOptions,
    pub health: DataHealthReport,
    pub rows: Vec<FillabilityBucketRow>,
}

#[derive(Debug, Clone)]
pub struct LiquidityGateV1Options {
    pub review: FactorReviewOptions,
    pub min_entry_capacity_ratio: f64,
    pub min_exit_capacity_ratio: f64,
    pub max_pm_lag_secs: f64,
    pub max_pm_spread_bps: f64,
    pub min_time_remaining_secs: i64,
    pub min_entry_ask: f64,
    pub max_entry_ask: f64,
}

impl Default for LiquidityGateV1Options {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            min_entry_capacity_ratio: 1.0,
            min_exit_capacity_ratio: 1.0,
            max_pm_lag_secs: 30.0,
            max_pm_spread_bps: 3_000.0,
            min_time_remaining_secs: 45,
            min_entry_ask: 0.02,
            max_entry_ask: 0.95,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiquidityGateV1Report {
    pub options: LiquidityGateV1Options,
    pub health: DataHealthReport,
    pub selected_n: usize,
    pub coverage: f64,
    pub entry_fill_rate: f64,
    pub exit_fill_rate: f64,
    pub roundtrip_fill_rate: f64,
    pub rejection_rate: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_pm_spread_bps: f64,
    pub avg_pm_lag_secs: f64,
    pub avg_roundtrip_cost_usd: f64,
    pub metrics: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct LiquidityGatedAlphaV1Options {
    pub gate: LiquidityGateV1Options,
    pub walk_forward: FactorWalkForwardOptions,
}

impl Default for LiquidityGatedAlphaV1Options {
    fn default() -> Self {
        Self {
            gate: LiquidityGateV1Options::default(),
            walk_forward: FactorWalkForwardOptions::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiquidityGatedAlphaV1Report {
    pub options: LiquidityGatedAlphaV1Options,
    pub baseline_health: DataHealthReport,
    pub gate: LiquidityGateV1Report,
    pub review: FactorReviewV2Report,
    pub walk_forward: FactorWalkForwardReport,
    pub stability: FactorStabilityReport,
}

#[derive(Debug, Clone)]
pub struct TradeFormationReviewOptions {
    pub review: FactorReviewOptions,
    pub gate: LiquidityGateV1Options,
    pub min_path_observations: usize,
    pub top_n: usize,
}

impl Default for TradeFormationReviewOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            gate: LiquidityGateV1Options::default(),
            min_path_observations: 20,
            top_n: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TradeFormationPathRow {
    pub path: String,
    pub n: usize,
    pub coverage: f64,
    pub executable_rows: usize,
    pub win_rate: f64,
    pub settlement_win_rate: f64,
    pub total_pnl_after_cost: f64,
    pub avg_pnl_after_cost: f64,
    pub sharpe: f64,
    pub avg_side_model_prob: f64,
    pub avg_side_distance_over_sigma: f64,
    pub avg_obi_10_side: f64,
    pub avg_obi_persistence_30s_side: f64,
    pub avg_cex_continuation_score_side: f64,
    pub avg_entry_capacity_ratio: f64,
    pub avg_exit_capacity_ratio: f64,
    pub avg_pm_spread_bps: f64,
    pub avg_future_exit_pnl_30s: f64,
    pub avg_future_exit_bid_change_30s: f64,
    pub avg_future_exit_bid_change_60s: f64,
}

#[derive(Debug, Clone)]
pub struct TradeFormationRuleRow {
    pub rule: String,
    pub n: usize,
    pub coverage: f64,
    pub win_rate: f64,
    pub total_pnl_after_cost: f64,
    pub avg_pnl_after_cost: f64,
    pub sharpe: f64,
    pub avg_future_exit_pnl_30s: f64,
}

#[derive(Debug, Clone)]
pub struct TradeFormationReviewReport {
    pub options: TradeFormationReviewOptions,
    pub health: DataHealthReport,
    pub gate: LiquidityGateV1Report,
    pub gated_rows: usize,
    pub profitable_gated_rows: usize,
    pub losing_gated_rows: usize,
    pub missed_winner_rows: usize,
    pub profitable_paths: Vec<TradeFormationPathRow>,
    pub losing_paths: Vec<TradeFormationPathRow>,
    pub missed_winner_paths: Vec<TradeFormationPathRow>,
    pub meta_label_rules: Vec<TradeFormationRuleRow>,
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardOptions {
    pub review: FactorReviewOptions,
    pub gate: LiquidityGateV1Options,
    pub train_window_days: i64,
    pub test_window_days: i64,
    pub step_days: i64,
    pub min_rule_observations: usize,
    pub min_candidate_windows: usize,
    pub min_candidate_positive_window_ratio: f64,
    pub min_candidate_total_test_pnl_after_cost: f64,
    pub min_candidate_avg_test_selected: f64,
    pub min_candidate_avg_fill_rate: f64,
    pub max_candidate_avg_rejection_rate: f64,
    pub min_candidate_worst_window_pnl_after_cost: f64,
    pub top_n: usize,
}

impl Default for MetaLabelWalkForwardOptions {
    fn default() -> Self {
        Self {
            review: FactorReviewOptions::default(),
            gate: LiquidityGateV1Options::default(),
            train_window_days: 2,
            test_window_days: 1,
            step_days: 1,
            min_rule_observations: 20,
            min_candidate_windows: 8,
            min_candidate_positive_window_ratio: 0.65,
            min_candidate_total_test_pnl_after_cost: 0.0,
            min_candidate_avg_test_selected: 30.0,
            min_candidate_avg_fill_rate: 0.95,
            max_candidate_avg_rejection_rate: 0.05,
            min_candidate_worst_window_pnl_after_cost: -150.0,
            top_n: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardWindow {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub rule: String,
    pub train: FactorSelectionMetrics,
    pub test: FactorSelectionMetrics,
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardAggregate {
    pub rule: String,
    pub windows: usize,
    pub positive_window_ratio: f64,
    pub total_test_pnl_after_cost: f64,
    pub avg_test_pnl_per_window: f64,
    pub min_test_pnl_after_cost: f64,
    pub avg_train_selected: f64,
    pub avg_test_selected: f64,
    pub avg_test_fill_rate: f64,
    pub avg_test_rejection_rate: f64,
    pub decision: FactorStabilityDecision,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct MetaLabelWalkForwardReport {
    pub options: MetaLabelWalkForwardOptions,
    pub health: DataHealthReport,
    pub gate: LiquidityGateV1Report,
    pub windows: Vec<MetaLabelWalkForwardWindow>,
    pub aggregates: Vec<MetaLabelWalkForwardAggregate>,
}

pub fn build_factor_observations_v2(
    rows: &[FactorObservation],
    options: &FactorReviewOptions,
) -> Vec<FactorObservationV2> {
    build_factor_observations_v2_with_deribit(rows, &[], options)
}

pub fn build_factor_observations_v2_with_deribit(
    rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: &FactorReviewOptions,
) -> Vec<FactorObservationV2> {
    build_factor_observations_v2_with_deribit_and_pm_books(rows, deribit, &[], options)
}

pub fn build_factor_observations_v2_with_deribit_and_pm_books(
    rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: &FactorReviewOptions,
) -> Vec<FactorObservationV2> {
    let stake_usd = options.stake_usd;
    let book_index = build_pm_book_index(pm_books);
    let mut out = Vec::with_capacity(rows.len() * 2);
    for row in rows {
        let up_book = latest_pm_book(
            &book_index,
            &row.event_id,
            &row.up_token_id,
            ReviewSide::Up,
            row.tick_ts,
            options.max_quote_age_secs,
        );
        let down_book = latest_pm_book(
            &book_index,
            &row.event_id,
            &row.down_token_id,
            ReviewSide::Down,
            row.tick_ts,
            options.max_quote_age_secs,
        );
        out.push(side_row(row, ReviewSide::Up, stake_usd, up_book));
        out.push(side_row(row, ReviewSide::Down, stake_usd, down_book));
    }
    enrich_rolling_features(
        &mut out,
        deribit,
        Some(&book_index),
        options.max_quote_age_secs,
    );
    out
}

pub fn factor_v2_descriptors() -> Vec<FactorV2Descriptor> {
    vec![
        descriptor(
            "side_model_edge",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_model_edge,
        ),
        descriptor(
            "side_model_prob",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_model_prob,
        ),
        descriptor(
            "side_fair_prob",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_fair_prob,
        ),
        descriptor(
            "side_fair_edge",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| {
                if valid_price(r.entry_ask) && r.side_fair_prob.is_finite() {
                    r.side_fair_prob - r.entry_ask - crypto_fee_cost(r.entry_ask)
                } else {
                    f64::NAN
                }
            },
        ),
        descriptor(
            "side_distance_over_sigma",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_distance_over_sigma,
        ),
        descriptor(
            "abs_distance_to_beat",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.abs_distance_to_beat,
        ),
        descriptor(
            "drift_10s",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.drift_10s * r.side.multiplier(),
        ),
        descriptor(
            "drift_30s",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.drift_30s * r.side.multiplier(),
        ),
        descriptor(
            "post_flip_drift",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.post_flip_drift,
        ),
        descriptor(
            "sigma_horizon",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.sigma_horizon,
        ),
        descriptor(
            "vol_gap",
            FactorFamily::Alpha,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.vol_gap,
        ),
        descriptor(
            "obi_10_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_10 * r.side.multiplier(),
        ),
        descriptor(
            "depth_imbalance_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.depth_imbalance * r.side.multiplier(),
        ),
        descriptor(
            "depth_acceleration_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.depth_acceleration * r.side.multiplier(),
        ),
        descriptor(
            "microprice_offset_side_bps",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.microprice_offset_bps * r.side.multiplier(),
        ),
        descriptor(
            "cex_spread_bps",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_spread_bps,
        ),
        descriptor(
            "cum_mprice_drift_5m_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cum_mprice_drift_5m * r.side.multiplier(),
        ),
        descriptor(
            "cum_trade_imbalance_5m_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cum_trade_imbalance_5m * r.side.multiplier(),
        ),
        descriptor(
            "obi_delta_10s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_delta_10s_side,
        ),
        descriptor(
            "obi_delta_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_delta_30s_side,
        ),
        descriptor(
            "obi_persistence_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_persistence_30s_side,
        ),
        descriptor(
            "obi_flip_count_60s",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.obi_flip_count_60s,
        ),
        descriptor(
            "depth_imbalance_delta_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.depth_imbalance_delta_30s_side,
        ),
        descriptor(
            "microprice_momentum_30s_side",
            FactorFamily::CexLob,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.microprice_momentum_30s_side,
        ),
        descriptor(
            "trade_imbalance_delta_10s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.trade_imbalance_delta_10s_side,
        ),
        descriptor(
            "trade_imbalance_delta_30s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.trade_imbalance_delta_30s_side,
        ),
        descriptor(
            "cex_bar_return_30s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_return_30s * r.side.multiplier(),
        ),
        descriptor(
            "cex_bar_return_60s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_return_60s * r.side.multiplier(),
        ),
        descriptor(
            "cex_bar_volume_ratio_30s",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_volume_ratio_30s,
        ),
        descriptor(
            "cex_bar_volume_trend_3",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_bar_volume_trend_3,
        ),
        descriptor(
            "cex_signed_volume_ratio_30s_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_signed_volume_ratio_30s * r.side.multiplier(),
        ),
        descriptor(
            "cex_consecutive_bar_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_consecutive_bar_side,
        ),
        descriptor(
            "cex_breakout_volume_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_breakout_volume_side,
        ),
        descriptor(
            "cex_continuation_score_side",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_continuation_score_side,
        ),
        descriptor(
            "cex_continuation_edge_gate",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::CexMicrostructureConfirmation,
            |r| r.cex_continuation_edge_gate,
        ),
        descriptor(
            "cex_continuation_liquidity_gate",
            FactorFamily::CexAggTrade,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.cex_continuation_liquidity_gate,
        ),
        descriptor(
            "entry_ask",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask,
        ),
        descriptor(
            "exit_bid",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_bid,
        ),
        descriptor(
            "pm_spread_bps",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_spread_bps,
        ),
        descriptor(
            "entry_ask_size",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask_size,
        ),
        descriptor(
            "exit_bid_size",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_bid_size,
        ),
        descriptor(
            "entry_sweep_slippage_bps",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_sweep_slippage_bps,
        ),
        descriptor(
            "exit_sweep_slippage_bps",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_sweep_slippage_bps,
        ),
        descriptor(
            "entry_sweep_levels_15u",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_sweep_levels_15u,
        ),
        descriptor(
            "exit_sweep_levels_15u",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_sweep_levels_15u,
        ),
        descriptor(
            "entry_full_depth_fillable_15u",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| bool_num(r.label_full_depth_entry_fillable),
        ),
        descriptor(
            "exit_full_depth_fillable_15u",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| bool_num(r.label_full_depth_exit_fillable),
        ),
        descriptor(
            "up_down_ask_sum",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.up_down_ask_sum,
        ),
        descriptor(
            "pm_lag_secs",
            FactorFamily::PmLiquidity,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_lag_secs,
        ),
        descriptor(
            "entry_ask_change_10s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask_change_10s,
        ),
        descriptor(
            "entry_ask_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_ask_change_30s,
        ),
        descriptor(
            "exit_bid_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_bid_change_30s,
        ),
        descriptor(
            "pm_spread_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_spread_change_30s,
        ),
        descriptor(
            "entry_size_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_size_change_30s,
        ),
        descriptor(
            "up_down_ask_sum_change_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.up_down_ask_sum_change_30s,
        ),
        descriptor(
            "pm_reprice_speed_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_reprice_speed_30s,
        ),
        descriptor(
            "pm_quote_stability_30s",
            FactorFamily::PmDynamics,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.pm_quote_stability_30s,
        ),
        descriptor(
            "deribit_mark_iv",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_mark_iv,
        ),
        descriptor(
            "deribit_iv_spread",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_spread,
        ),
        descriptor(
            "deribit_iv_lag_secs",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_lag_secs,
        ),
        descriptor(
            "deribit_iv_horizon",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_horizon,
        ),
        descriptor(
            "deribit_iv_gap_horizon",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_gap_horizon,
        ),
        descriptor(
            "deribit_iv_change_30s",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_change_30s,
        ),
        descriptor(
            "deribit_iv_change_60s",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_iv_change_60s,
        ),
        descriptor(
            "deribit_underlying_basis_bps",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_underlying_basis_bps,
        ),
        descriptor(
            "deribit_delta_side",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_delta * r.side.multiplier(),
        ),
        descriptor(
            "deribit_gamma",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_gamma,
        ),
        descriptor(
            "deribit_vega",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_vega,
        ),
        descriptor(
            "deribit_theta",
            FactorFamily::DeribitVol,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.deribit_theta,
        ),
        descriptor(
            "entry_capacity_ratio",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_capacity_ratio,
        ),
        descriptor(
            "exit_capacity_ratio",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_capacity_ratio,
        ),
        descriptor(
            "entry_liquidity_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.entry_liquidity_usd,
        ),
        descriptor(
            "exit_liquidity_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.exit_liquidity_usd,
        ),
        descriptor(
            "liquidity_shortfall_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.liquidity_shortfall_usd,
        ),
        descriptor(
            "slippage_to_fill_15u_bps",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.slippage_to_fill_15u_bps,
        ),
        descriptor(
            "roundtrip_cost_usd",
            FactorFamily::Execution,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.roundtrip_cost_usd,
        ),
        descriptor(
            "time_remaining_secs",
            FactorFamily::Regime,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.time_remaining_secs as f64,
        ),
        descriptor(
            "roundtrip_pnl_now_15u",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.roundtrip_pnl_now_15u.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_bid_change_5s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_bid_change_5s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_bid_change_10s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_bid_change_10s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_bid_change_30s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_bid_change_30s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_bid_change_60s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_bid_change_60s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_pnl_5s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_pnl_5s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_pnl_10s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_pnl_10s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_pnl_30s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_pnl_30s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_pnl_60s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_pnl_60s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_fillable_5s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_fillable_5s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_fillable_10s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_fillable_10s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_fillable_30s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_fillable_30s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "future_exit_fillable_60s",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.label_future_exit_fillable_60s.unwrap_or(f64::NAN),
        ),
        descriptor(
            "exit_fillable",
            FactorFamily::Exit,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| bool_num(r.label_exit_fillable),
        ),
        descriptor(
            "portfolio_stake_usd",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.portfolio_stake_usd,
        ),
        descriptor(
            "portfolio_event_exposure_usd",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.portfolio_event_exposure_usd,
        ),
        descriptor(
            "same_event_observation_count",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.same_event_observation_count,
        ),
        descriptor(
            "same_event_side_observation_count",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            |r| r.same_event_side_observation_count,
        ),
        descriptor(
            "side_is_up",
            FactorFamily::PortfolioRisk,
            ThreeLayerArchive::DirectionProbabilityEdge,
            |r| r.side_is_up,
        ),
    ]
}

pub fn build_data_health_report(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
) -> DataHealthReport {
    DataHealthReport {
        source_observations: source_rows.len(),
        v2_rows: v2_rows.len(),
        settlement_label_rows: v2_rows
            .iter()
            .filter(|row| row.label_settlement_win.is_some_and(f64::is_finite))
            .count(),
        entry_quote_rows: v2_rows
            .iter()
            .filter(|row| valid_price(row.entry_ask))
            .count(),
        exit_quote_rows: v2_rows
            .iter()
            .filter(|row| valid_price(row.exit_bid))
            .count(),
        entry_size_rows: v2_rows
            .iter()
            .filter(|row| row.entry_ask_size.is_finite() && row.entry_ask_size > 0.0)
            .count(),
        exit_size_rows: v2_rows
            .iter()
            .filter(|row| row.exit_bid_size.is_finite() && row.exit_bid_size > 0.0)
            .count(),
        entry_fillable_rows: v2_rows
            .iter()
            .filter(|row| row.label_executable_fillable)
            .count(),
        exit_fillable_rows: v2_rows.iter().filter(|row| row.label_exit_fillable).count(),
        entry_full_depth_fillable_rows: v2_rows
            .iter()
            .filter(|row| row.label_full_depth_entry_fillable)
            .count(),
        exit_full_depth_fillable_rows: v2_rows
            .iter()
            .filter(|row| row.label_full_depth_exit_fillable)
            .count(),
        executable_pnl_rows: v2_rows
            .iter()
            .filter(|row| row.label_executable_pnl_15u.is_some_and(f64::is_finite))
            .count(),
        full_depth_executable_pnl_rows: v2_rows
            .iter()
            .filter(|row| {
                row.label_full_depth_executable_pnl_15u
                    .is_some_and(f64::is_finite)
            })
            .count(),
        avg_pm_lag_secs: mean(v2_rows.iter().map(|row| row.pm_lag_secs)),
        avg_entry_capacity_ratio: mean(v2_rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(v2_rows.iter().map(|row| row.exit_capacity_ratio)),
        avg_entry_sweep_slippage_bps: mean(v2_rows.iter().map(|row| row.entry_sweep_slippage_bps)),
        avg_exit_sweep_slippage_bps: mean(v2_rows.iter().map(|row| row.exit_sweep_slippage_bps)),
        deribit_rows: v2_rows
            .iter()
            .filter(|row| row.deribit_mark_iv.is_finite())
            .count(),
    }
}

pub fn review_factors_v2(
    source_rows: &[FactorObservation],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factors_v2_with_deribit(source_rows, &[], options)
}

pub fn review_factors_v2_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options);
    review_factor_rows(source_rows, &v2_rows, options)
}

pub fn review_factors_v2_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factors_v2_with_deribit_and_pm_books_filtered(
        source_rows,
        deribit,
        pm_books,
        options,
        None,
    )
}

pub fn review_factors_v2_with_deribit_and_pm_books_filtered(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: FactorReviewOptions,
    factor_name_filter: Option<&str>,
) -> FactorReviewV2Report {
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options,
    );
    review_factor_rows_with_name_filter(source_rows, &v2_rows, options, factor_name_filter)
}

fn review_factor_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factor_rows_with_name_filter(source_rows, v2_rows, options, None)
}

fn review_factor_rows_with_name_filter(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
    factor_name_filter: Option<&str>,
) -> FactorReviewV2Report {
    let factor_name_filter = factor_name_filter.map(ToOwned::to_owned);
    review_factor_rows_with_descriptor_filter(source_rows, v2_rows, options, |descriptor| {
        factor_name_matches_filter(descriptor.name, &factor_name_filter)
    })
}

fn review_candidate_factor_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
) -> FactorReviewV2Report {
    review_factor_rows_with_descriptor_filter(
        source_rows,
        v2_rows,
        options,
        is_walk_forward_candidate_descriptor,
    )
}

fn review_factor_rows_with_descriptor_filter(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FactorReviewOptions,
    descriptor_filter: impl Fn(&FactorV2Descriptor) -> bool,
) -> FactorReviewV2Report {
    let health = build_data_health_report(source_rows, v2_rows);
    let mut reviews: Vec<SingleFactorReview> = factor_v2_descriptors()
        .into_iter()
        .filter(descriptor_filter)
        .filter_map(|descriptor| review_one_factor(v2_rows, descriptor, &options))
        .collect();
    reviews.sort_by(|a, b| {
        b.selected_total_pnl_after_cost
            .total_cmp(&a.selected_total_pnl_after_cost)
            .then_with(|| {
                b.executable_pnl_rank_ic
                    .abs()
                    .total_cmp(&a.executable_pnl_rank_ic.abs())
            })
    });
    let executable_ev_buckets = build_executable_ev_buckets(v2_rows, &options);
    let direction_side_audit = build_direction_side_audit(v2_rows, &options);
    let binance_direction_audit = build_binance_direction_audit(v2_rows, &options);
    FactorReviewV2Report {
        options,
        health,
        reviews,
        executable_ev_buckets,
        direction_side_audit,
        binance_direction_audit,
    }
}

pub fn walk_forward_factors_v2_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorWalkForwardOptions,
) -> FactorWalkForwardReport {
    let mut v2_rows =
        build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    walk_forward_factor_rows(source_rows, &mut v2_rows, start, end, options)
}

pub fn walk_forward_factors_v2_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorWalkForwardOptions,
) -> FactorWalkForwardReport {
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    walk_forward_factor_rows(source_rows, &mut v2_rows, start, end, options)
}

fn walk_forward_factor_rows(
    source_rows: &[FactorObservation],
    v2_rows: &mut [FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorWalkForwardOptions,
) -> FactorWalkForwardReport {
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, v2_rows);
    let train_duration = options.train_duration();
    let test_duration = options.test_duration();
    let step_duration = options.step_duration();
    let descriptors: Vec<FactorV2Descriptor> = factor_v2_descriptors()
        .into_iter()
        .filter(is_walk_forward_candidate_descriptor)
        .filter(|descriptor| {
            factor_name_matches_filter(descriptor.name, &options.factor_name_filter)
        })
        .collect();
    let event_ends = inferred_event_ends(v2_rows);

    let mut windows = Vec::new();
    let mut train_start = start;
    let mut window_index = 0usize;
    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let (train_rows, test_rows) = event_disjoint_walk_forward_slices(
            v2_rows,
            &event_ends,
            train_start,
            train_end,
            test_start,
            test_end,
        );

        if train_rows.len() >= options.review.min_observations
            && test_rows.len() >= options.review.min_observations
        {
            let mut fitted: Vec<FactorWalkForwardWindow> = descriptors
                .iter()
                .filter_map(|descriptor| {
                    fit_walk_forward_factor(
                        &train_rows,
                        &test_rows,
                        *descriptor,
                        &options.review,
                        window_index,
                        train_start,
                        train_end,
                        test_start,
                        test_end,
                    )
                })
                .collect();
            fitted.sort_by(|a, b| {
                b.test
                    .total_pnl_after_cost
                    .total_cmp(&a.test.total_pnl_after_cost)
                    .then_with(|| b.test.sharpe.total_cmp(&a.test.sharpe))
            });
            windows.extend(fitted);
        }

        window_index += 1;
        train_start += step_duration;
    }

    let aggregates = aggregate_walk_forward_windows(&windows);
    FactorWalkForwardReport {
        options,
        health,
        windows,
        aggregates,
    }
}

pub fn build_factor_stability_report(
    report: &FactorWalkForwardReport,
    options: FactorStabilityOptions,
) -> FactorStabilityReport {
    let mut grouped: BTreeMap<&str, Vec<&FactorWalkForwardWindow>> = BTreeMap::new();
    for window in &report.windows {
        grouped.entry(&window.factor).or_default().push(window);
    }

    let mut rows = Vec::with_capacity(grouped.len());
    for (factor, windows) in grouped {
        let Some(first) = windows.first() else {
            continue;
        };
        let total_test_pnl_after_cost = windows
            .iter()
            .map(|window| window.test.total_pnl_after_cost)
            .sum::<f64>();
        let min_test_pnl_after_cost = windows
            .iter()
            .map(|window| window.test.total_pnl_after_cost)
            .fold(f64::INFINITY, f64::min);
        let positive_windows = windows
            .iter()
            .filter(|window| window.test.total_pnl_after_cost > 0.0)
            .count();
        let settlement_ics = windows
            .iter()
            .map(|window| window.train_settlement_rank_ic)
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        let executable_ics = windows
            .iter()
            .map(|window| window.train_executable_pnl_rank_ic)
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        let avg_by_symbol_positive_ratio = mean(
            windows
                .iter()
                .map(|window| window.test.by_symbol_positive_ratio),
        );
        let avg_by_time_bucket_positive_ratio = mean(
            windows
                .iter()
                .map(|window| window.test.by_time_bucket_positive_ratio),
        );
        let avg_test_fill_rate = mean(
            windows
                .iter()
                .map(|window| window.test.executable_fill_rate),
        );
        let avg_test_rejection_rate = mean(windows.iter().map(|window| window.test.rejection_rate));
        let positive_window_ratio = ratio(positive_windows, windows.len());
        let settlement_rank_ic_mean = mean(settlement_ics.iter().copied());
        let executable_pnl_rank_ic_mean = mean(executable_ics.iter().copied());
        let settlement_rank_icir = icir(&settlement_ics);
        let executable_pnl_rank_icir = icir(&executable_ics);
        let (decision, reason) = stability_decision(
            windows.len(),
            positive_window_ratio,
            total_test_pnl_after_cost,
            avg_test_fill_rate,
            avg_test_rejection_rate,
            avg_by_symbol_positive_ratio,
            avg_by_time_bucket_positive_ratio,
            executable_pnl_rank_icir,
            &options,
        );
        rows.push(FactorStabilityRow {
            factor: factor.to_string(),
            family: first.family,
            layer: first.layer,
            windows: windows.len(),
            settlement_rank_ic_mean,
            settlement_rank_icir,
            executable_pnl_rank_ic_mean,
            executable_pnl_rank_icir,
            positive_window_ratio,
            total_test_pnl_after_cost,
            avg_test_pnl_per_window: total_test_pnl_after_cost / windows.len() as f64,
            min_test_pnl_after_cost,
            avg_test_fill_rate,
            avg_test_rejection_rate,
            avg_by_symbol_positive_ratio,
            avg_by_time_bucket_positive_ratio,
            decision,
            reason,
        });
    }
    rows.sort_by(|a, b| {
        decision_rank(b.decision)
            .cmp(&decision_rank(a.decision))
            .then_with(|| {
                b.total_test_pnl_after_cost
                    .total_cmp(&a.total_test_pnl_after_cost)
            })
            .then_with(|| b.positive_window_ratio.total_cmp(&a.positive_window_ratio))
    });
    FactorStabilityReport { options, rows }
}

pub fn review_repricing_ic_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: RepricingIcOptions,
) -> RepricingIcReport {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    review_repricing_ic_rows(source_rows, &v2_rows, options)
}

pub fn review_repricing_ic_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: RepricingIcOptions,
) -> RepricingIcReport {
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    review_repricing_ic_rows(source_rows, &v2_rows, options)
}

fn review_repricing_ic_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: RepricingIcOptions,
) -> RepricingIcReport {
    let health = build_data_health_report(source_rows, v2_rows);
    let descriptors: Vec<FactorV2Descriptor> = factor_v2_descriptors()
        .into_iter()
        .filter(is_walk_forward_candidate_descriptor)
        .filter(|descriptor| {
            factor_name_matches_filter(descriptor.name, &options.factor_name_filter)
        })
        .collect();
    let targets = repricing_ic_targets();
    let mut rows = Vec::new();
    for descriptor in descriptors {
        for target in &targets {
            if let Some(row) =
                review_one_repricing_ic_target(v2_rows, descriptor, *target, &options)
            {
                rows.push(row);
            }
        }
    }
    rows.sort_by(|a, b| {
        target_group_rank(a.target_group)
            .cmp(&target_group_rank(b.target_group))
            .then_with(|| b.spearman_ic.abs().total_cmp(&a.spearman_ic.abs()))
            .then_with(|| b.top_bucket_avg_label.total_cmp(&a.top_bucket_avg_label))
    });
    RepricingIcReport {
        options,
        health,
        rows,
    }
}

fn review_one_repricing_ic_target(
    rows: &[FactorObservationV2],
    descriptor: FactorV2Descriptor,
    target: RepricingIcTargetDescriptor,
    options: &RepricingIcOptions,
) -> Option<RepricingIcRow> {
    let scored: Vec<(&FactorObservationV2, f64, f64)> = rows
        .iter()
        .filter_map(|row| {
            let score = (descriptor.accessor)(row);
            let label = (target.accessor)(row)?;
            (score.is_finite() && label.is_finite()).then_some((row, score, label))
        })
        .collect();
    if scored.len() < options.review.min_observations {
        return None;
    }

    let pairs: Vec<(f64, f64)> = scored
        .iter()
        .map(|(_, score, label)| (*score, *label))
        .collect();
    let pearson_ic = pair_pearson(&pairs);
    let spearman_ic = pair_spearman(&pairs);

    let mut grouped: BTreeMap<String, Vec<(f64, f64)>> = BTreeMap::new();
    for (row, score, label) in &scored {
        grouped
            .entry(repricing_ic_window_key(row))
            .or_default()
            .push((*score, *label));
    }
    let window_ics = grouped
        .values()
        .filter(|pairs| pairs.len() >= options.min_window_observations)
        .map(|pairs| pair_spearman(pairs))
        .filter(|ic| ic.is_finite())
        .collect::<Vec<_>>();
    let positive_windows = window_ics.iter().filter(|ic| **ic > 0.0).count();
    let window_ic_mean = mean(window_ics.iter().copied());
    let repricing_icir = icir(&window_ics);
    let positive_window_ratio = ratio(positive_windows, window_ics.len());

    let buckets = build_repricing_ic_buckets(&scored, options.bucket_count);
    let bottom = buckets.first();
    let top = buckets.last();
    let bucket_avg_labels = buckets
        .iter()
        .map(|bucket| bucket.avg_label)
        .collect::<Vec<_>>();
    let monotonic_non_decreasing = bucket_avg_labels
        .windows(2)
        .all(|window| window[1] + EPS >= window[0]);

    Some(RepricingIcRow {
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        factor_role: repricing_factor_role(descriptor.family),
        target: target.name.to_string(),
        target_group: target.group,
        n: scored.len(),
        pearson_ic,
        spearman_ic,
        window_count: window_ics.len(),
        window_ic_mean,
        icir: repricing_icir,
        positive_window_ratio,
        bottom_bucket_n: bottom.map(|bucket| bucket.n).unwrap_or(0),
        bottom_bucket_avg_label: bottom.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top_bucket_n: top.map(|bucket| bucket.n).unwrap_or(0),
        top_bucket_avg_label: top.map(|bucket| bucket.avg_label).unwrap_or(f64::NAN),
        top_bucket_positive_label_rate: top
            .map(|bucket| bucket.positive_label_rate)
            .unwrap_or(f64::NAN),
        bucket_avg_labels,
        monotonic_non_decreasing,
        avg_entry_ask: mean(scored.iter().map(|(row, _, _)| row.entry_ask)),
        avg_pm_spread_bps: mean(scored.iter().map(|(row, _, _)| row.pm_spread_bps)),
        entry_fill_rate: ratio(
            scored
                .iter()
                .filter(|(row, _, _)| entry_fillable(row))
                .count(),
            scored.len(),
        ),
        exit_fill_rate: ratio(
            scored
                .iter()
                .filter(|(row, _, _)| exit_fillable(row))
                .count(),
            scored.len(),
        ),
    })
}

pub fn format_repricing_ic_report(report: &RepricingIcReport, top_n: usize) -> String {
    let mut out = String::new();
    out.push_str("=== Repricing IC Report Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} settlement_labels={} executable_pnl_rows={} entry_fill={:.2}% exit_fill={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.settlement_label_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.exit_fill_rate() * 100.0,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "min_obs={} min_window_obs={} bucket_count={} stake_usd={:.2}\n",
        report.options.review.min_observations,
        report.options.min_window_observations,
        report.options.bucket_count,
        report.options.review.stake_usd,
    ));
    out.push_str(
        "labels are side-aligned candidate trades: BUY_YES and BUY_NO rows. full_depth_reprice_pnl_h sweeps entry asks now and future bids at h; legacy reprice_pnl_h uses top-book future bids for debug only.\n",
    );
    out.push_str(
        "future_exit_* fields are labels/diagnostics only and are excluded from factor candidates. execution_filter rows explain tradability, not alpha.\n",
    );

    let top_n = top_n.max(1);
    for group in [
        "full_depth_reprice_pnl",
        "reprice_pnl",
        "reprice_bid_change",
        "volatility",
        "settlement",
        "execution",
    ] {
        let group_rows = report
            .rows
            .iter()
            .filter(|row| row.target_group == group)
            .take(top_n)
            .collect::<Vec<_>>();
        if group_rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n=== Repricing IC Target Group: {group} ===\n"));
        out.push_str("target,factor,role,family,layer,n,pearson_ic,spearman_ic,window_count,window_ic_mean,icir,pos_window_ratio,bottom_n,bottom_avg,top_n,top_avg,top_pos_rate,monotonic,avg_entry_ask,avg_pm_spread_bps,entry_fill,exit_fill,bucket_avgs\n");
        for row in group_rows {
            out.push_str(&format!(
                "{},{},{},{},{},{},{:.4},{:.4},{},{:.4},{:.4},{:.4},{},{:.4},{},{:.4},{:.4},{},{:.4},{:.2},{:.4},{:.4},{}\n",
                row.target,
                row.factor,
                row.factor_role,
                row.family.as_str(),
                row.layer.as_str(),
                row.n,
                row.pearson_ic,
                row.spearman_ic,
                row.window_count,
                row.window_ic_mean,
                row.icir,
                row.positive_window_ratio,
                row.bottom_bucket_n,
                row.bottom_bucket_avg_label,
                row.top_bucket_n,
                row.top_bucket_avg_label,
                row.top_bucket_positive_label_rate,
                row.monotonic_non_decreasing,
                row.avg_entry_ask,
                row.avg_pm_spread_bps,
                row.entry_fill_rate,
                row.exit_fill_rate,
                format_bucket_avgs(&row.bucket_avg_labels),
            ));
        }
    }
    out
}

pub fn format_full_depth_execution_matrix_report(
    report: &FullDepthExecutionMatrixReport,
    top_n: usize,
) -> String {
    let mut out = String::new();
    out.push_str("=== Full-Depth Execution Matrix ===\n");
    out.push_str(&format!(
        "stakes_usd={} visible_depth_haircut={:.2} max_levels={} min_bucket_obs={} buckets={}\n",
        report
            .options
            .stakes_usd
            .iter()
            .map(|stake| format!("{stake:.2}"))
            .collect::<Vec<_>>()
            .join("|"),
        report.options.visible_depth_haircut,
        report
            .options
            .max_levels
            .map(|levels| levels.to_string())
            .unwrap_or_else(|| "all".to_string()),
        report.options.min_bucket_observations,
        report.rows.len(),
    ));
    out.push_str("Full-depth matrix is the execution gate: entry sweeps asks by stake, repricing exits sweep future bids by entry shares, settlement uses entry sweep only.\n");
    out.push_str("stake,symbol,side,time_bucket,distance_bucket,entry_price_bucket,spread_bucket,quote_age_bucket,count,entry_fill,entry_avg_price,entry_avg_slip_bps,entry_p50_slip_bps,entry_p90_slip_bps,entry_avg_levels,exit_5s_fill,exit_10s_fill,exit_30s_fill,exit_10s_slip_bps,exit_30s_slip_bps,roundtrip_5s,roundtrip_10s,roundtrip_30s,avg_settlement_pnl,avg_reprice_pnl_5s,avg_reprice_pnl_10s,avg_reprice_pnl_30s\n");
    for row in report.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{:.2},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.2},{:.2},{:.2},{:.2},{:.4},{:.4},{:.4},{:.2},{:.2},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.stake_usd,
            row.symbol,
            row.side.as_str(),
            row.time_bucket,
            row.distance_bucket,
            row.entry_price_bucket,
            row.spread_bucket,
            row.quote_age_bucket,
            row.count,
            row.entry_fill_rate,
            row.entry_avg_price_mean,
            row.entry_avg_slippage_bps,
            row.entry_p50_slippage_bps,
            row.entry_p90_slippage_bps,
            row.entry_avg_levels_used,
            row.exit_5s_fill_rate,
            row.exit_10s_fill_rate,
            row.exit_30s_fill_rate,
            row.exit_10s_avg_slippage_bps,
            row.exit_30s_avg_slippage_bps,
            row.roundtrip_fill_rate_5s,
            row.roundtrip_fill_rate_10s,
            row.roundtrip_fill_rate_30s,
            row.avg_settlement_pnl,
            row.avg_reprice_pnl_5s,
            row.avg_reprice_pnl_10s,
            row.avg_reprice_pnl_30s,
        ));
    }
    out
}

pub fn build_settlement_probability_report(
    rows: &[FactorObservationV2],
    options: SettlementProbabilityReportOptions,
) -> SettlementProbabilityReport {
    build_settlement_probability_report_with_prior(rows, None, options)
}

pub fn build_settlement_probability_report_with_prior(
    rows: &[FactorObservationV2],
    prior: Option<&LlmPriorSpec>,
    options: SettlementProbabilityReportOptions,
) -> SettlementProbabilityReport {
    let rows = rows.iter().collect::<Vec<_>>();
    build_settlement_probability_report_with_surface(&rows, &rows, prior, options)
}

fn build_settlement_probability_report_with_surface<'a>(
    evaluation_rows: &[&'a FactorObservationV2],
    surface_rows: &[&'a FactorObservationV2],
    prior: Option<&LlmPriorSpec>,
    options: SettlementProbabilityReportOptions,
) -> SettlementProbabilityReport {
    let options = normalize_settlement_probability_report_options(options);
    let surface_eligible_rows = settlement_probability_eligible_rows(surface_rows);
    let eligible_rows = settlement_probability_eligible_rows(evaluation_rows);

    let event_surface = EventVolSurface::fit(
        &surface_eligible_rows,
        options.event_surface_min_bucket_observations,
        options.event_surface_shrinkage_observations,
    );

    let raw_chainlink_support_complete = eligible_rows
        .iter()
        .all(|(row, ..)| valid_probability(row.side_chainlink_prob));
    let typed_chainlink_support_complete = eligible_rows
        .iter()
        .filter(|(row, ..)| prior.is_none_or(|prior| prediction_prior_matches_row(prior, row)))
        .all(|(row, ..)| valid_probability(row.side_chainlink_prob));
    let typed_blends = validated_probability_blends(prior)
        .into_iter()
        .filter(|(_, blend)| {
            blend.chainlink_digital_weight <= EPS || typed_chainlink_support_complete
        })
        .collect::<Vec<_>>();
    let mut by_model: BTreeMap<String, Vec<SettlementProbabilitySample>> = BTreeMap::new();
    for (row, win, pnl, conservative_pnl) in eligible_rows {
        for (model, q) in settlement_probability_models(row) {
            if model == "q_chainlink_digital" && !raw_chainlink_support_complete {
                continue;
            }
            push_settlement_probability_sample(
                &mut by_model,
                model,
                row,
                win,
                pnl,
                conservative_pnl,
                q,
            );
        }
        let event_q = event_surface.predict(row);
        if let Some(q) = event_q {
            push_settlement_probability_sample(
                &mut by_model,
                "q_event_surface_empirical",
                row,
                win,
                pnl,
                conservative_pnl,
                q,
            );
        }
        if let Some(q) = settlement_probability_final_blend(row, event_q) {
            push_settlement_probability_sample(
                &mut by_model,
                "q_final_logit_blend",
                row,
                win,
                pnl,
                conservative_pnl,
                q,
            );
        }
        if prior.is_none_or(|prior| prediction_prior_matches_row(prior, row)) {
            for (model, blend) in &typed_blends {
                if let Some(q) = settlement_probability_weighted_blend(row, event_q, blend) {
                    push_settlement_probability_sample(
                        &mut by_model,
                        model.as_str(),
                        row,
                        win,
                        pnl,
                        conservative_pnl,
                        q,
                    );
                }
            }
        }
    }

    settlement_probability_report_from_model_samples(by_model, options)
}

fn normalize_settlement_probability_report_options(
    options: SettlementProbabilityReportOptions,
) -> SettlementProbabilityReportOptions {
    SettlementProbabilityReportOptions {
        bucket_count: options.bucket_count.max(2),
        min_bucket_observations: options.min_bucket_observations.max(1),
        top_edge_quantile: options.top_edge_quantile.clamp(0.01, 1.0),
        event_surface_min_bucket_observations: options.event_surface_min_bucket_observations.max(1),
        event_surface_shrinkage_observations: options.event_surface_shrinkage_observations.max(1),
    }
}

fn settlement_probability_eligible_rows<'a>(
    rows: &[&'a FactorObservationV2],
) -> Vec<(&'a FactorObservationV2, f64, f64, Option<f64>)> {
    let mut eligible_rows = Vec::new();
    for &row in rows {
        let Some(win) = row.label_settlement_win.filter(|win| win.is_finite()) else {
            continue;
        };
        let Some(pnl) = row
            .label_full_depth_executable_pnl_15u
            .filter(|pnl| pnl.is_finite())
        else {
            continue;
        };
        if !row.label_full_depth_entry_fillable || !valid_price(row.entry_sweep_avg_price_15u) {
            continue;
        }
        let conservative_pnl = row
            .label_conservative_entry_fillable
            .then_some(row.label_conservative_executable_pnl_15u)
            .flatten()
            .filter(|pnl| pnl.is_finite());
        eligible_rows.push((row, win, pnl, conservative_pnl));
    }
    eligible_rows
}

fn settlement_probability_report_from_model_samples(
    by_model: BTreeMap<String, Vec<SettlementProbabilitySample>>,
    options: SettlementProbabilityReportOptions,
) -> SettlementProbabilityReport {
    let mut baselines = Vec::new();
    let mut calibration = Vec::new();
    let mut edge_buckets = Vec::new();
    let mut anti_overfit = Vec::new();
    let mut symbol_holdouts = Vec::new();
    for (model, samples) in by_model {
        if samples.len() < options.min_bucket_observations {
            continue;
        }
        let model_calibration =
            build_probability_calibration_rows(&model, &samples, options.bucket_count);
        let model_edge_buckets = build_probability_edge_bucket_rows(
            &model,
            &samples,
            options.bucket_count,
            options.min_bucket_observations,
        );
        baselines.push(build_probability_baseline_row(
            &model,
            &samples,
            &model_calibration,
            &model_edge_buckets,
            options.top_edge_quantile,
        ));
        calibration.extend(
            model_calibration
                .into_iter()
                .filter(|row| row.count >= options.min_bucket_observations),
        );
        anti_overfit.extend(build_probability_anti_overfit_rows(
            &model,
            &samples,
            options.top_edge_quantile,
        ));
        symbol_holdouts.extend(build_probability_symbol_holdout_rows(
            &model,
            &samples,
            options.top_edge_quantile,
            options.min_bucket_observations,
        ));
        edge_buckets.extend(model_edge_buckets);
    }
    baselines.sort_by(|a, b| {
        b.top_edge_avg_full_depth_settlement_pnl
            .total_cmp(&a.top_edge_avg_full_depth_settlement_pnl)
            .then_with(|| a.brier_score.total_cmp(&b.brier_score))
    });
    let ablations = build_settlement_probability_ablation_rows(&baselines);
    SettlementProbabilityReport {
        options,
        baselines,
        calibration,
        edge_buckets,
        anti_overfit,
        symbol_holdouts,
        ablations,
    }
}

pub fn walk_forward_settlement_probability_report(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: SettlementProbabilityWalkForwardOptions,
) -> SettlementProbabilityWalkForwardReport {
    walk_forward_settlement_probability_report_with_prior(rows, start, end, None, options)
}

pub fn walk_forward_settlement_probability_report_with_prior(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    prior: Option<&LlmPriorSpec>,
    options: SettlementProbabilityWalkForwardOptions,
) -> SettlementProbabilityWalkForwardReport {
    let mut rows = rows.to_vec();
    rows.sort_by_key(|row| row.tick_ts);
    let train_duration = options.walk_forward.train_duration();
    let test_duration = options.walk_forward.test_duration();
    let step_duration = options.walk_forward.step_duration();
    let probability_options =
        normalize_settlement_probability_report_options(options.probability.clone());
    let event_ends = inferred_event_ends(&rows);

    let mut windows = Vec::new();
    let mut train_start = start;
    let mut window_index = 0usize;
    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let (train_rows, test_rows) = event_disjoint_walk_forward_slices(
            &rows,
            &event_ends,
            train_start,
            train_end,
            test_start,
            test_end,
        );

        if train_rows.len() >= options.walk_forward.review.min_observations
            && test_rows.len() >= options.walk_forward.review.min_observations
        {
            let train_report = build_settlement_probability_report_with_surface(
                &train_rows,
                &train_rows,
                prior,
                probability_options.clone(),
            );
            let test_report = build_settlement_probability_report_with_surface(
                &test_rows,
                &train_rows,
                prior,
                probability_options.clone(),
            );
            windows.extend(settlement_probability_walk_forward_windows(
                ProbabilityWalkForwardWindowBounds {
                    window_index,
                    train_start,
                    train_end,
                    test_start,
                    test_end,
                },
                &train_report,
                &test_report,
                &options,
            ));
        }

        window_index += 1;
        train_start += step_duration;
    }
    let aggregates = aggregate_settlement_probability_walk_forward_windows(&windows);
    SettlementProbabilityWalkForwardReport {
        options,
        windows,
        aggregates,
    }
}

#[derive(Debug, Clone, Copy)]
struct ProbabilityWalkForwardWindowBounds {
    window_index: usize,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    test_start: DateTime<Utc>,
    test_end: DateTime<Utc>,
}

fn settlement_probability_walk_forward_windows(
    bounds: ProbabilityWalkForwardWindowBounds,
    train_report: &SettlementProbabilityReport,
    test_report: &SettlementProbabilityReport,
    options: &SettlementProbabilityWalkForwardOptions,
) -> Vec<SettlementProbabilityWalkForwardWindow> {
    let train_by_model: BTreeMap<&str, &SettlementProbabilityBaselineRow> = train_report
        .baselines
        .iter()
        .map(|row| (row.model.as_str(), row))
        .collect();
    let mut windows = Vec::new();
    for test in &test_report.baselines {
        let Some(train) = train_by_model.get(test.model.as_str()) else {
            continue;
        };
        windows.push(SettlementProbabilityWalkForwardWindow {
            window_index: bounds.window_index,
            model: test.model.clone(),
            train_start: bounds.train_start,
            train_end: bounds.train_end,
            test_start: bounds.test_start,
            test_end: bounds.test_end,
            train_n: train.n,
            test_n: test.n,
            train_brier_score: train.brier_score,
            test_brier_score: test.brier_score,
            train_log_loss: train.log_loss,
            test_log_loss: test.log_loss,
            train_expected_calibration_error: train.expected_calibration_error,
            test_expected_calibration_error: test.expected_calibration_error,
            train_top_edge_avg_full_depth_settlement_pnl: train
                .top_edge_avg_full_depth_settlement_pnl,
            test_top_edge_avg_full_depth_settlement_pnl: test
                .top_edge_avg_full_depth_settlement_pnl,
            test_top_edge_avg_conservative_settlement_pnl: test
                .top_edge_avg_conservative_settlement_pnl,
            train_top_edge_conservative_coverage_rate: train.top_edge_conservative_coverage_rate,
            test_top_edge_conservative_coverage_rate: test.top_edge_conservative_coverage_rate,
            test_edge_bucket_monotonic_non_decreasing: test.edge_bucket_monotonic_non_decreasing,
            pass: test.top_edge_avg_full_depth_settlement_pnl > 0.0
                && test.top_edge_avg_conservative_settlement_pnl > 0.0
                && train.top_edge_conservative_coverage_rate >= 1.0
                && test.top_edge_conservative_coverage_rate >= 1.0
                && test.brier_score.is_finite()
                && test.brier_score <= options.max_test_brier_score
                && test.log_loss.is_finite()
                && test.log_loss <= options.max_test_log_loss
                && test.expected_calibration_error.is_finite()
                && test.expected_calibration_error <= options.max_test_expected_calibration_error,
        });
    }
    windows
}

fn aggregate_settlement_probability_walk_forward_windows(
    windows: &[SettlementProbabilityWalkForwardWindow],
) -> Vec<SettlementProbabilityWalkForwardAggregate> {
    let mut grouped: BTreeMap<&str, Vec<&SettlementProbabilityWalkForwardWindow>> = BTreeMap::new();
    for window in windows {
        grouped
            .entry(window.model.as_str())
            .or_default()
            .push(window);
    }
    let mut aggregates = grouped
        .into_iter()
        .map(|(model, rows)| {
            let windows = rows.len();
            let positive = rows
                .iter()
                .filter(|row| row.test_top_edge_avg_full_depth_settlement_pnl > 0.0)
                .count();
            let passed = rows.iter().filter(|row| row.pass).count();
            SettlementProbabilityWalkForwardAggregate {
                model: model.to_string(),
                windows,
                positive_window_ratio: ratio(positive, windows),
                pass_window_ratio: ratio(passed, windows),
                avg_test_brier_score: mean(rows.iter().map(|row| row.test_brier_score)),
                avg_test_log_loss: mean(rows.iter().map(|row| row.test_log_loss)),
                avg_test_expected_calibration_error: mean(
                    rows.iter().map(|row| row.test_expected_calibration_error),
                ),
                avg_test_top_edge_avg_full_depth_settlement_pnl: mean(
                    rows.iter()
                        .map(|row| row.test_top_edge_avg_full_depth_settlement_pnl),
                ),
                min_test_top_edge_avg_full_depth_settlement_pnl: rows
                    .iter()
                    .map(|row| row.test_top_edge_avg_full_depth_settlement_pnl)
                    .fold(f64::INFINITY, f64::min),
                avg_test_top_edge_avg_conservative_settlement_pnl: mean(
                    rows.iter()
                        .map(|row| row.test_top_edge_avg_conservative_settlement_pnl),
                ),
                min_test_top_edge_avg_conservative_settlement_pnl: rows
                    .iter()
                    .map(|row| row.test_top_edge_avg_conservative_settlement_pnl)
                    .fold(f64::INFINITY, f64::min),
                min_top_edge_conservative_coverage_rate: rows
                    .iter()
                    .map(|row| {
                        row.train_top_edge_conservative_coverage_rate
                            .min(row.test_top_edge_conservative_coverage_rate)
                    })
                    .fold(f64::INFINITY, f64::min),
            }
        })
        .collect::<Vec<_>>();
    aggregates.sort_by(|a, b| {
        b.avg_test_top_edge_avg_full_depth_settlement_pnl
            .total_cmp(&a.avg_test_top_edge_avg_full_depth_settlement_pnl)
            .then_with(|| b.positive_window_ratio.total_cmp(&a.positive_window_ratio))
    });
    aggregates
}

pub fn build_settlement_probability_promotion_gate_report(
    probability: &SettlementProbabilityReport,
    walk_forward: &SettlementProbabilityWalkForwardReport,
    execution: &FullDepthExecutionMatrixReport,
    conservative_execution: &FullDepthExecutionMatrixReport,
    options: SettlementProbabilityPromotionGateOptions,
) -> SettlementProbabilityPromotionGateReport {
    let mut gates = Vec::new();

    let data_status = options
        .data_audit_status
        .as_deref()
        .unwrap_or("<not-recorded>");
    let (data_quality_passed, data_quality_evidence) = match options.data_quality_mode {
        SettlementProbabilityDataQualityMode::StrictContinuous => (
            data_status.eq_ignore_ascii_case("ok"),
            format!(
                "mode={} snapshot_data_audit_status={data_status}",
                options.data_quality_mode.as_str()
            ),
        ),
        SettlementProbabilityDataQualityMode::EventComplete => {
            let passed = options.event_complete_events >= options.min_event_complete_events
                && options.event_complete_rows >= options.min_event_complete_rows;
            (
                passed,
                format!(
                    "mode={} snapshot_data_audit_status={} event_complete_events={} min_events={} event_complete_rows={} min_rows={}",
                    options.data_quality_mode.as_str(),
                    data_status,
                    options.event_complete_events,
                    options.min_event_complete_events,
                    options.event_complete_rows,
                    options.min_event_complete_rows
                ),
            )
        }
    };
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "data_quality".to_string(),
        passed: data_quality_passed,
        evidence: data_quality_evidence,
    });

    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "deribit_vol_surface".to_string(),
        passed: !options.require_deribit || options.include_deribit,
        evidence: format!(
            "require_deribit={} include_deribit={}",
            options.require_deribit, options.include_deribit
        ),
    });

    let entry_fill = max_entry_fill_rate(execution, options.stake_usd);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "full_depth_entry_capacity".to_string(),
        passed: entry_fill >= options.min_entry_fill_rate,
        evidence: format!(
            "stake_usd={:.2} max_entry_fill_rate={:.4} min_required={:.4}",
            options.stake_usd, entry_fill, options.min_entry_fill_rate
        ),
    });

    let conservative_entry_fill = max_entry_fill_rate(conservative_execution, options.stake_usd);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "conservative_entry_capacity".to_string(),
        passed: conservative_entry_fill >= options.min_entry_fill_rate,
        evidence: format!(
            "stake_usd={:.2} max_conservative_entry_fill_rate={:.4} min_required={:.4}",
            options.stake_usd, conservative_entry_fill, options.min_entry_fill_rate
        ),
    });

    let global_full_depth_entry_fill = options
        .global_full_depth_entry_fill_rate
        .filter(|value| value.is_finite());
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "global_full_depth_entry_fillability".to_string(),
        passed: global_full_depth_entry_fill
            .map(|rate| rate >= options.min_entry_fill_rate)
            .unwrap_or(true),
        evidence: global_full_depth_entry_fill.map_or_else(
            || {
                format!(
                    "global_full_depth_entry_fill_rate=<not-recorded> min_required={:.4}",
                    options.min_entry_fill_rate
                )
            },
            |rate| {
                format!(
                    "global_full_depth_entry_fill_rate={:.4} min_required={:.4}",
                    rate, options.min_entry_fill_rate
                )
            },
        ),
    });

    let best_calibrated =
        best_calibrated_probability_model(probability, options.max_expected_calibration_error);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "probability_calibration".to_string(),
        passed: best_calibrated.is_some(),
        evidence: best_calibrated.map_or_else(
            || {
                format!(
                    "no non-naive model ece <= {:.4}",
                    options.max_expected_calibration_error
                )
            },
            |row| {
                format!(
                    "model={} ece={:.6} max_allowed={:.6}",
                    row.model,
                    row.expected_calibration_error,
                    options.max_expected_calibration_error
                )
            },
        ),
    });

    let best_full_depth_edge = best_full_depth_edge_model(probability);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "full_depth_settlement_edge".to_string(),
        passed: best_full_depth_edge.is_some(),
        evidence: best_full_depth_edge.map_or_else(
            || "no non-naive model has positive top-edge full-depth settlement PnL".to_string(),
            |row| {
                format!(
                    "model={} top_edge_full_depth_pnl={:.4}",
                    row.model, row.top_edge_avg_full_depth_settlement_pnl
                )
            },
        ),
    });

    let best_conservative_edge = best_conservative_edge_model(probability);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "conservative_settlement_edge".to_string(),
        passed: best_conservative_edge.is_some(),
        evidence: best_conservative_edge.map_or_else(
            || "no non-naive model has positive top-edge conservative settlement PnL".to_string(),
            |row| {
                format!(
                    "model={} top_edge_conservative_pnl={:.4}",
                    row.model, row.top_edge_avg_conservative_settlement_pnl
                )
            },
        ),
    });

    let anti_overfit_model = model_with_all_anti_overfit_tests_passing(probability);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "anti_overfit_diagnostics".to_string(),
        passed: anti_overfit_model.is_some(),
        evidence: anti_overfit_model.map_or_else(
            || "no non-naive model passes all deterministic anti-overfit diagnostics".to_string(),
            |(model, passed, total)| format!("model={model} passed_tests={passed}/{total}"),
        ),
    });

    let holdout_model = model_with_all_symbol_holdouts_passing(probability);
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "symbol_holdout".to_string(),
        passed: holdout_model.is_some(),
        evidence: holdout_model.map_or_else(
            || "no non-naive model passes all symbol holdouts".to_string(),
            |(model, passed, total)| format!("model={model} passed_symbols={passed}/{total}"),
        ),
    });

    let oos_model = best_walk_forward_oos_model(walk_forward, options.min_positive_window_ratio);
    let oos_candidate = walk_forward
        .aggregates
        .iter()
        .filter(|row| is_settlement_probability_candidate_model(&row.model))
        .filter(|row| row.windows > 0)
        .max_by(|a, b| {
            a.pass_window_ratio
                .total_cmp(&b.pass_window_ratio)
                .then_with(|| {
                    a.avg_test_top_edge_avg_full_depth_settlement_pnl
                        .total_cmp(&b.avg_test_top_edge_avg_full_depth_settlement_pnl)
                })
        });
    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "walk_forward_oos".to_string(),
        passed: oos_model.is_some(),
        evidence: oos_model.map_or_else(
            || {
                oos_candidate.map_or_else(
                    || "no non-naive model has non-empty OOS windows".to_string(),
                    |row| {
                        format!(
                            "rejected model={} windows={} positive_window_ratio={:.4} pass_window_ratio={:.4} min_ratio={:.4} avg_test_brier={:.6} max_brier={:.6} avg_test_log_loss={:.6} max_log_loss={:.6} avg_test_ece={:.6} max_ece={:.6} min_test_top_edge_pnl={:.4} min_test_top_edge_conservative_pnl={:.4}",
                            row.model,
                            row.windows,
                            row.positive_window_ratio,
                            row.pass_window_ratio,
                            options.min_positive_window_ratio,
                            row.avg_test_brier_score,
                            walk_forward.options.max_test_brier_score,
                            row.avg_test_log_loss,
                            walk_forward.options.max_test_log_loss,
                            row.avg_test_expected_calibration_error,
                            walk_forward.options.max_test_expected_calibration_error,
                            row.min_test_top_edge_avg_full_depth_settlement_pnl,
                            row.min_test_top_edge_avg_conservative_settlement_pnl,
                        )
                    },
                )
            },
            |row| {
                format!(
                    "model={} windows={} positive_window_ratio={:.4} pass_window_ratio={:.4} min_ratio={:.4} avg_test_brier={:.6} max_brier={:.6} avg_test_log_loss={:.6} max_log_loss={:.6} avg_test_ece={:.6} max_ece={:.6} min_test_top_edge_pnl={:.4} min_test_top_edge_conservative_pnl={:.4}",
                    row.model,
                    row.windows,
                    row.positive_window_ratio,
                    row.pass_window_ratio,
                    options.min_positive_window_ratio,
                    row.avg_test_brier_score,
                    walk_forward.options.max_test_brier_score,
                    row.avg_test_log_loss,
                    walk_forward.options.max_test_log_loss,
                    row.avg_test_expected_calibration_error,
                    walk_forward.options.max_test_expected_calibration_error,
                    row.min_test_top_edge_avg_full_depth_settlement_pnl,
                    row.min_test_top_edge_avg_conservative_settlement_pnl
                )
            },
        ),
    });

    gates.push(SettlementProbabilityPromotionGateRow {
        gate: "recorded_replay_parity".to_string(),
        passed: options.replay_parity_ready,
        evidence: options.replay_parity_evidence.clone().unwrap_or_else(|| {
            if options.replay_parity_ready {
                "recorded replay parity marked ready by caller".to_string()
            } else {
                "post-dry-run gate pending: no recorded replay parity artifact was supplied to this report"
                    .to_string()
            }
        }),
    });

    let ready_for_dry_run_handoff = gates
        .iter()
        .filter(|gate| gate.gate != "recorded_replay_parity")
        .all(|gate| gate.passed);
    SettlementProbabilityPromotionGateReport {
        options,
        ready_for_dry_run_handoff,
        gates,
    }
}

fn is_settlement_probability_candidate_model(model: &str) -> bool {
    model != "q_naive_50_50"
}

fn max_entry_fill_rate(report: &FullDepthExecutionMatrixReport, stake_usd: f64) -> f64 {
    finite_max(report.rows.iter().filter_map(|row| {
        if (row.stake_usd - stake_usd).abs() < 1e-6 {
            Some(row.entry_fill_rate)
        } else {
            None
        }
    }))
}

fn best_calibrated_probability_model(
    report: &SettlementProbabilityReport,
    max_ece: f64,
) -> Option<&SettlementProbabilityBaselineRow> {
    report
        .baselines
        .iter()
        .filter(|row| is_settlement_probability_candidate_model(&row.model))
        .filter(|row| row.expected_calibration_error.is_finite())
        .filter(|row| row.expected_calibration_error <= max_ece)
        .min_by(|a, b| {
            a.expected_calibration_error
                .total_cmp(&b.expected_calibration_error)
                .then_with(|| {
                    b.top_edge_avg_full_depth_settlement_pnl
                        .total_cmp(&a.top_edge_avg_full_depth_settlement_pnl)
                })
        })
}

fn best_full_depth_edge_model(
    report: &SettlementProbabilityReport,
) -> Option<&SettlementProbabilityBaselineRow> {
    report
        .baselines
        .iter()
        .filter(|row| is_settlement_probability_candidate_model(&row.model))
        .filter(|row| row.top_edge_avg_full_depth_settlement_pnl > 0.0)
        .max_by(|a, b| {
            a.top_edge_avg_full_depth_settlement_pnl
                .total_cmp(&b.top_edge_avg_full_depth_settlement_pnl)
        })
}

fn best_conservative_edge_model(
    report: &SettlementProbabilityReport,
) -> Option<&SettlementProbabilityBaselineRow> {
    report
        .baselines
        .iter()
        .filter(|row| is_settlement_probability_candidate_model(&row.model))
        .filter(|row| row.top_edge_conservative_coverage_rate >= 1.0)
        .filter(|row| row.top_edge_avg_conservative_settlement_pnl > 0.0)
        .max_by(|a, b| {
            a.top_edge_avg_conservative_settlement_pnl
                .total_cmp(&b.top_edge_avg_conservative_settlement_pnl)
        })
}

fn model_with_all_anti_overfit_tests_passing(
    report: &SettlementProbabilityReport,
) -> Option<(&str, usize, usize)> {
    let mut by_model: BTreeMap<&str, Vec<&SettlementProbabilityAntiOverfitRow>> = BTreeMap::new();
    for row in &report.anti_overfit {
        if is_settlement_probability_candidate_model(&row.model) {
            by_model.entry(row.model.as_str()).or_default().push(row);
        }
    }
    by_model
        .into_iter()
        .filter_map(|(model, rows)| {
            let total = rows.len();
            let passed = rows.iter().filter(|row| row.pass).count();
            if total > 0 && passed == total {
                Some((model, passed, total))
            } else {
                None
            }
        })
        .max_by_key(|(_, passed, _)| *passed)
}

fn model_with_all_symbol_holdouts_passing(
    report: &SettlementProbabilityReport,
) -> Option<(&str, usize, usize)> {
    let mut by_model: BTreeMap<&str, Vec<&SettlementProbabilitySymbolHoldoutRow>> = BTreeMap::new();
    for row in &report.symbol_holdouts {
        if is_settlement_probability_candidate_model(&row.model) {
            by_model.entry(row.model.as_str()).or_default().push(row);
        }
    }
    by_model
        .into_iter()
        .filter_map(|(model, rows)| {
            let total = rows.len();
            let passed = rows.iter().filter(|row| row.pass).count();
            if total > 0 && passed == total {
                Some((model, passed, total))
            } else {
                None
            }
        })
        .max_by_key(|(_, passed, _)| *passed)
}

fn best_walk_forward_oos_model(
    report: &SettlementProbabilityWalkForwardReport,
    min_positive_window_ratio: f64,
) -> Option<&SettlementProbabilityWalkForwardAggregate> {
    report
        .aggregates
        .iter()
        .filter(|row| is_settlement_probability_candidate_model(&row.model))
        .filter(|row| row.windows > 0)
        .filter(|row| row.positive_window_ratio >= min_positive_window_ratio)
        .filter(|row| row.pass_window_ratio >= min_positive_window_ratio)
        .filter(|row| {
            row.avg_test_brier_score.is_finite()
                && row.avg_test_brier_score <= report.options.max_test_brier_score
        })
        .filter(|row| {
            row.avg_test_log_loss.is_finite()
                && row.avg_test_log_loss <= report.options.max_test_log_loss
        })
        .filter(|row| {
            row.avg_test_expected_calibration_error.is_finite()
                && row.avg_test_expected_calibration_error
                    <= report.options.max_test_expected_calibration_error
        })
        .filter(|row| row.min_test_top_edge_avg_full_depth_settlement_pnl > 0.0)
        .filter(|row| row.min_test_top_edge_avg_conservative_settlement_pnl > 0.0)
        .filter(|row| row.min_top_edge_conservative_coverage_rate >= 1.0)
        .max_by(|a, b| {
            a.pass_window_ratio
                .total_cmp(&b.pass_window_ratio)
                .then_with(|| {
                    a.avg_test_top_edge_avg_full_depth_settlement_pnl
                        .total_cmp(&b.avg_test_top_edge_avg_full_depth_settlement_pnl)
                })
        })
}

pub fn build_prediction_research_feedback(
    prior: &LlmPriorSpec,
    report: &SettlementProbabilityWalkForwardReport,
    min_positive_window_ratio: f64,
) -> Option<PredictionResearchFeedback> {
    if validate_prediction_research_prior(prior).is_err() {
        return None;
    }
    let mission_id = prior.mission_id.as_deref()?.trim();
    if mission_id.is_empty() {
        return None;
    }
    let allowed_models = validated_probability_blends(Some(prior))
        .into_iter()
        .map(|(model, blend)| (model, blend.clone()))
        .collect::<BTreeMap<_, _>>();
    let aggregates = report
        .aggregates
        .iter()
        .map(|aggregate| (aggregate.model.as_str(), aggregate))
        .collect::<BTreeMap<_, _>>();
    let baseline_aggregates = report
        .aggregates
        .iter()
        .filter(|aggregate| {
            aggregate.windows > 0
                && !aggregate.model.starts_with("q_llm_")
                && aggregate.model != "q_naive_50_50"
        })
        .collect::<Vec<_>>();
    let best_baseline_brier = finite_min(
        baseline_aggregates
            .iter()
            .map(|aggregate| aggregate.avg_test_brier_score),
    );
    let best_baseline_log_loss = finite_min(
        baseline_aggregates
            .iter()
            .map(|aggregate| aggregate.avg_test_log_loss),
    );
    let best_baseline_conservative_pnl = finite_max(
        baseline_aggregates
            .iter()
            .map(|aggregate| aggregate.avg_test_top_edge_avg_conservative_settlement_pnl),
    );
    let min_ratio = min_positive_window_ratio.clamp(0.0, 1.0);
    let candidates = allowed_models
        .into_iter()
        .map(|(model, probability_blend)| {
            let hypothesis = probability_blend.hypothesis.trim().to_string();
            let Some(aggregate) = aggregates.get(model.as_str()).copied() else {
                return PredictionResearchCandidateFeedback {
                    model,
                    hypothesis,
                    probability_blend,
                    verdict: "discard".to_string(),
                    reason_codes: vec!["no_oos_windows".to_string()],
                    metrics: serde_json::json!({"windows": 0}),
                };
            };
            let mut reason_codes = Vec::new();
            if aggregate.positive_window_ratio < min_ratio {
                reason_codes.push("insufficient_positive_windows".to_string());
            }
            if aggregate.pass_window_ratio < min_ratio {
                reason_codes.push("insufficient_passing_windows".to_string());
            }
            if !aggregate.avg_test_brier_score.is_finite()
                || aggregate.avg_test_brier_score > report.options.max_test_brier_score
            {
                reason_codes.push("brier_gate_failed".to_string());
            }
            if !aggregate.avg_test_log_loss.is_finite()
                || aggregate.avg_test_log_loss > report.options.max_test_log_loss
            {
                reason_codes.push("log_loss_gate_failed".to_string());
            }
            if !aggregate.avg_test_expected_calibration_error.is_finite()
                || aggregate.avg_test_expected_calibration_error
                    > report.options.max_test_expected_calibration_error
            {
                reason_codes.push("calibration_gate_failed".to_string());
            }
            if aggregate.min_test_top_edge_avg_full_depth_settlement_pnl <= 0.0 {
                reason_codes.push("nonpositive_oos_settlement_pnl".to_string());
            }
            if aggregate.min_test_top_edge_avg_conservative_settlement_pnl <= 0.0 {
                reason_codes.push("nonpositive_oos_conservative_capacity_pnl".to_string());
            }
            if aggregate.min_top_edge_conservative_coverage_rate < 1.0 {
                reason_codes
                    .push("incomplete_oos_top_edge_conservative_capacity_coverage".to_string());
            }
            if !best_baseline_brier.is_finite()
                || !best_baseline_log_loss.is_finite()
                || !best_baseline_conservative_pnl.is_finite()
            {
                reason_codes.push("missing_baseline_comparison".to_string());
            } else {
                if aggregate.avg_test_brier_score >= best_baseline_brier - 1e-9 {
                    reason_codes.push("no_incremental_brier_vs_baseline".to_string());
                }
                if aggregate.avg_test_log_loss >= best_baseline_log_loss - 1e-9 {
                    reason_codes.push("no_incremental_log_loss_vs_baseline".to_string());
                }
                if aggregate.avg_test_top_edge_avg_conservative_settlement_pnl
                    <= best_baseline_conservative_pnl + 1e-9
                {
                    reason_codes
                        .push("no_incremental_conservative_pnl_vs_baseline".to_string());
                }
            }
            PredictionResearchCandidateFeedback {
                model,
                hypothesis,
                probability_blend,
                verdict: if reason_codes.is_empty() {
                    "keep".to_string()
                } else {
                    "discard".to_string()
                },
                reason_codes,
                metrics: serde_json::json!({
                    "windows": aggregate.windows,
                    "positive_window_ratio": aggregate.positive_window_ratio,
                    "pass_window_ratio": aggregate.pass_window_ratio,
                    "avg_test_brier_score": aggregate.avg_test_brier_score,
                    "avg_test_log_loss": aggregate.avg_test_log_loss,
                    "avg_test_expected_calibration_error": aggregate.avg_test_expected_calibration_error,
                    "avg_test_top_edge_avg_full_depth_settlement_pnl": aggregate.avg_test_top_edge_avg_full_depth_settlement_pnl,
                    "min_test_top_edge_avg_full_depth_settlement_pnl": aggregate.min_test_top_edge_avg_full_depth_settlement_pnl,
                    "avg_test_top_edge_avg_conservative_settlement_pnl": aggregate.avg_test_top_edge_avg_conservative_settlement_pnl,
                    "min_test_top_edge_avg_conservative_settlement_pnl": aggregate.min_test_top_edge_avg_conservative_settlement_pnl,
                    "min_top_edge_conservative_coverage_rate": aggregate.min_top_edge_conservative_coverage_rate,
                    "best_baseline_brier_score": best_baseline_brier.is_finite().then_some(best_baseline_brier),
                    "best_baseline_log_loss": best_baseline_log_loss.is_finite().then_some(best_baseline_log_loss),
                    "best_baseline_avg_conservative_settlement_pnl": best_baseline_conservative_pnl.is_finite().then_some(best_baseline_conservative_pnl),
                }),
            }
        })
        .collect::<Vec<_>>();
    Some(PredictionResearchFeedback {
        schema_version: "prediction_research_feedback.v1".to_string(),
        mission_id: mission_id.to_string(),
        target: prior.target.clone().expect("validated prediction target"),
        symbols: prior.symbols.clone(),
        horizon: prior.horizon.clone().expect("validated prediction horizon"),
        data_snapshot_id: prior
            .data_snapshot_id
            .clone()
            .expect("validated data snapshot"),
        prompt_snapshot_id: prior
            .prompt_snapshot_id
            .clone()
            .expect("validated prompt snapshot"),
        search_policy_snapshot_id: prior
            .search_policy_snapshot_id
            .clone()
            .expect("validated search policy snapshot"),
        candidates,
    })
}

pub fn format_settlement_probability_report(report: &SettlementProbabilityReport) -> String {
    let mut out = String::new();
    out.push_str("=== Settlement Probability Report ===\n");
    out.push_str(&format!(
        "bucket_count={} min_bucket_obs={} top_edge_quantile={:.2} event_surface_min_bucket_obs={} event_surface_shrinkage_obs={}\n",
        report.options.bucket_count,
        report.options.min_bucket_observations,
        report.options.top_edge_quantile,
        report.options.event_surface_min_bucket_observations,
        report.options.event_surface_shrinkage_observations,
    ));
    out.push_str("Population is full-depth entry-fillable candidate rows with settled labels. Edge is q_side - full_depth_entry_sweep_avg_price; PnL is full-depth settlement PnL after crypto fee. Conservative PnL uses 50% visible depth and max 3 CLOB levels; uncovered rows remain in the denominator with zero conservative PnL, and promoted top-edge selections require 100% conservative coverage.\n");
    out.push_str("\n--- Baseline Comparison ---\n");
    out.push_str("model,n,avg_q,actual_win,brier,log_loss,ece,avg_edge,avg_full_depth_settlement_pnl,avg_conservative_settlement_pnl,conservative_coverage_rate,profit_factor,edge_bucket_monotonic,top_edge_count,top_edge_avg_edge,top_edge_win,top_edge_avg_full_depth_settlement_pnl,top_edge_avg_conservative_settlement_pnl,top_edge_conservative_coverage_rate\n");
    for row in &report.baselines {
        out.push_str(&format!(
            "{},{},{:.4},{:.4},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4},{},{},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.model,
            row.n,
            row.avg_predicted_q,
            row.actual_win_rate,
            row.brier_score,
            row.log_loss,
            row.expected_calibration_error,
            row.avg_edge,
            row.avg_full_depth_settlement_pnl,
            row.avg_conservative_settlement_pnl,
            row.conservative_coverage_rate,
            row.profit_factor,
            row.edge_bucket_monotonic_non_decreasing,
            row.top_edge_count,
            row.top_edge_avg_edge,
            row.top_edge_win_rate,
            row.top_edge_avg_full_depth_settlement_pnl,
            row.top_edge_avg_conservative_settlement_pnl,
            row.top_edge_conservative_coverage_rate,
        ));
    }
    out.push_str("\n--- Calibration Buckets ---\n");
    out.push_str("model,q_bucket,count,avg_q,actual_win,calibration_error,avg_edge,avg_full_depth_settlement_pnl,avg_conservative_settlement_pnl\n");
    for row in &report.calibration {
        out.push_str(&format!(
            "{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.model,
            row.q_bucket,
            row.count,
            row.avg_predicted_q,
            row.actual_win_rate,
            row.calibration_error,
            row.avg_edge,
            row.avg_full_depth_settlement_pnl,
            row.avg_conservative_settlement_pnl,
        ));
    }
    out.push_str("\n--- Edge Buckets ---\n");
    out.push_str("model,edge_bucket,count,avg_edge,avg_q,actual_win,avg_entry_price,avg_full_depth_settlement_pnl,avg_conservative_settlement_pnl,profit_factor,conservative_profit_factor\n");
    for row in &report.edge_buckets {
        out.push_str(&format!(
            "{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.model,
            row.edge_bucket,
            row.count,
            row.avg_edge,
            row.avg_predicted_q,
            row.actual_win_rate,
            row.avg_full_depth_entry_price,
            row.avg_full_depth_settlement_pnl,
            row.avg_conservative_settlement_pnl,
            row.profit_factor,
            row.conservative_profit_factor,
        ));
    }
    out.push_str("\n--- Anti-Overfit Diagnostics ---\n");
    out.push_str("model,test,n,observed_edge_win_rank_ic,perturbed_edge_win_rank_ic,observed_top_edge_avg_full_depth_settlement_pnl,perturbed_top_edge_avg_full_depth_settlement_pnl,pass\n");
    for row in &report.anti_overfit {
        out.push_str(&format!(
            "{},{},{},{:.6},{:.6},{:.4},{:.4},{}\n",
            row.model,
            row.test,
            row.n,
            row.observed_edge_win_rank_ic,
            row.perturbed_edge_win_rank_ic,
            row.observed_top_edge_avg_full_depth_settlement_pnl,
            row.perturbed_top_edge_avg_full_depth_settlement_pnl,
            row.pass,
        ));
    }
    out.push_str("\n--- Symbol Holdout Diagnostics ---\n");
    out.push_str("model,symbol,n,edge_win_rank_ic,top_edge_avg_full_depth_settlement_pnl,pass\n");
    for row in &report.symbol_holdouts {
        out.push_str(&format!(
            "{},{},{},{:.6},{:.4},{}\n",
            row.model,
            row.symbol,
            row.n,
            row.edge_win_rank_ic,
            row.top_edge_avg_full_depth_settlement_pnl,
            row.pass,
        ));
    }
    out.push_str("\n--- Baseline Ablations ---\n");
    out.push_str("model,reference_model,n,delta_brier,delta_log_loss,delta_ece,delta_top_edge_avg_full_depth_settlement_pnl,improves_error,improves_top_edge_pnl\n");
    for row in &report.ablations {
        out.push_str(&format!(
            "{},{},{},{:.6},{:.6},{:.6},{:.4},{},{}\n",
            row.model,
            row.reference_model,
            row.n,
            row.delta_brier_score,
            row.delta_log_loss,
            row.delta_expected_calibration_error,
            row.delta_top_edge_avg_full_depth_settlement_pnl,
            row.improves_error,
            row.improves_top_edge_pnl,
        ));
    }
    out
}

pub fn format_settlement_probability_walk_forward_report(
    report: &SettlementProbabilityWalkForwardReport,
) -> String {
    let mut out = String::new();
    out.push_str("=== Settlement Probability Walk-Forward Report ===\n");
    out.push_str(&format!(
        "train_window={} test_window={} step={} min_obs={} probability_min_bucket_obs={} top_edge_quantile={:.2} max_test_brier={:.4} max_test_log_loss={:.6} max_test_ece={:.4}\n",
        report.options.walk_forward.train_window_label(),
        report.options.walk_forward.test_window_label(),
        report.options.walk_forward.step_label(),
        report.options.walk_forward.review.min_observations,
        report.options.probability.min_bucket_observations,
        report.options.probability.top_edge_quantile,
        report.options.max_test_brier_score,
        report.options.max_test_log_loss,
        report.options.max_test_expected_calibration_error,
    ));
    out.push_str("Test windows evaluate formula probabilities on future rows; EventVolSurface and q_final use only the train window as empirical prior.\n");
    out.push_str("\n--- Aggregate ---\n");
    out.push_str("model,windows,positive_window_ratio,pass_window_ratio,avg_test_brier,avg_test_log_loss,avg_test_ece,avg_test_top_edge_full_depth_pnl,min_test_top_edge_full_depth_pnl,avg_test_top_edge_conservative_pnl,min_test_top_edge_conservative_pnl,min_top_edge_conservative_coverage_rate\n");
    for row in &report.aggregates {
        out.push_str(&format!(
            "{},{},{:.4},{:.4},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.model,
            row.windows,
            row.positive_window_ratio,
            row.pass_window_ratio,
            row.avg_test_brier_score,
            row.avg_test_log_loss,
            row.avg_test_expected_calibration_error,
            row.avg_test_top_edge_avg_full_depth_settlement_pnl,
            row.min_test_top_edge_avg_full_depth_settlement_pnl,
            row.avg_test_top_edge_avg_conservative_settlement_pnl,
            row.min_test_top_edge_avg_conservative_settlement_pnl,
            row.min_top_edge_conservative_coverage_rate,
        ));
    }
    out.push_str("\n--- Windows ---\n");
    out.push_str("window,model,train_start,train_end,test_start,test_end,train_n,test_n,train_brier,test_brier,train_log_loss,test_log_loss,train_ece,test_ece,train_top_edge_full_depth_pnl,test_top_edge_full_depth_pnl,test_top_edge_conservative_pnl,train_top_edge_conservative_coverage_rate,test_top_edge_conservative_coverage_rate,test_edge_monotonic,pass\n");
    for row in &report.windows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4},{},{}\n",
            row.window_index,
            row.model,
            row.train_start,
            row.train_end,
            row.test_start,
            row.test_end,
            row.train_n,
            row.test_n,
            row.train_brier_score,
            row.test_brier_score,
            row.train_log_loss,
            row.test_log_loss,
            row.train_expected_calibration_error,
            row.test_expected_calibration_error,
            row.train_top_edge_avg_full_depth_settlement_pnl,
            row.test_top_edge_avg_full_depth_settlement_pnl,
            row.test_top_edge_avg_conservative_settlement_pnl,
            row.train_top_edge_conservative_coverage_rate,
            row.test_top_edge_conservative_coverage_rate,
            row.test_edge_bucket_monotonic_non_decreasing,
            row.pass,
        ));
    }
    out
}

pub fn format_settlement_probability_promotion_gate_report(
    report: &SettlementProbabilityPromotionGateReport,
) -> String {
    let mut out = String::new();
    out.push_str("=== Settlement Probability PRD Promotion Gate ===\n");
    out.push_str(&format!(
        "ready_for_dry_run_handoff={} stake_usd={:.2} min_entry_fill_rate={:.4} max_ece={:.4} min_positive_window_ratio={:.2} require_deribit={} include_deribit={} data_quality_mode={} event_complete_events={} event_complete_rows={} replay_parity_ready={}\n",
        report.ready_for_dry_run_handoff,
        report.options.stake_usd,
        report.options.min_entry_fill_rate,
        report.options.max_expected_calibration_error,
        report.options.min_positive_window_ratio,
        report.options.require_deribit,
        report.options.include_deribit,
        report.options.data_quality_mode.as_str(),
        report.options.event_complete_events,
        report.options.event_complete_rows,
        report.options.replay_parity_ready,
    ));
    out.push_str(
        "This is a promotion blocker report. A short-window smoke can validate workflow shape, but dry-run handoff stays blocked until every gate passes.\n",
    );
    out.push_str("gate,passed,evidence\n");
    for row in &report.gates {
        out.push_str(&format!(
            "{},{},{}\n",
            row.gate,
            row.passed,
            row.evidence.replace(',', ";")
        ));
    }
    out
}

pub fn walk_forward_factor_combo_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorComboV1Options,
) -> FactorComboV1Report {
    let mut v2_rows = build_factor_observations_v2_with_deribit(
        source_rows,
        deribit,
        &options.walk_forward.review,
    );
    walk_forward_factor_combo_v1_rows(source_rows, &mut v2_rows, start, end, options)
}

pub fn walk_forward_factor_combo_v1_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorComboV1Options,
) -> FactorComboV1Report {
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.walk_forward.review,
    );
    walk_forward_factor_combo_v1_rows(source_rows, &mut v2_rows, start, end, options)
}

fn walk_forward_factor_combo_v1_rows(
    source_rows: &[FactorObservation],
    v2_rows: &mut [FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorComboV1Options,
) -> FactorComboV1Report {
    v2_rows.sort_by_key(|row| row.tick_ts);
    let descriptors: Vec<FactorV2Descriptor> = factor_v2_descriptors()
        .into_iter()
        .filter(is_walk_forward_candidate_descriptor)
        .filter(|descriptor| {
            factor_name_matches_filter(descriptor.name, &options.walk_forward.factor_name_filter)
        })
        .collect();

    walk_forward_factor_combo_from_v2_rows(source_rows, v2_rows, &descriptors, start, end, options)
}

fn walk_forward_factor_combo_from_v2_rows(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    descriptors: &[FactorV2Descriptor],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: FactorComboV1Options,
) -> FactorComboV1Report {
    let health = build_data_health_report(source_rows, v2_rows);
    let mut windows = Vec::new();
    let train_duration = options.walk_forward.train_duration();
    let test_duration = options.walk_forward.test_duration();
    let step_duration = options.walk_forward.step_duration();
    let event_ends = inferred_event_ends(v2_rows);
    let mut train_start = start;
    let mut window_index = 0usize;
    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let (train_rows, test_rows) = event_disjoint_walk_forward_slices(
            v2_rows,
            &event_ends,
            train_start,
            train_end,
            test_start,
            test_end,
        );

        if train_rows.len() >= options.walk_forward.review.min_observations
            && test_rows.len() >= options.walk_forward.review.min_observations
        {
            if let Some(window) = fit_combo_v1_window(
                &train_rows,
                &test_rows,
                descriptors,
                &options,
                window_index,
                train_start,
                train_end,
                test_start,
                test_end,
            ) {
                windows.push(window);
            }
        }

        window_index += 1;
        train_start += step_duration;
    }
    let aggregate = aggregate_combo_v1_windows(&windows);
    FactorComboV1Report {
        options,
        health,
        windows,
        aggregate,
    }
}

pub fn review_fillability_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: FillabilityReviewOptions,
) -> FillabilityReviewReport {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    build_fillability_review_v1_report(source_rows, &v2_rows, options)
}

pub fn review_fillability_v1_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: FillabilityReviewOptions,
) -> FillabilityReviewReport {
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    build_fillability_review_v1_report(source_rows, &v2_rows, options)
}

fn build_fillability_review_v1_report(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: FillabilityReviewOptions,
) -> FillabilityReviewReport {
    let health = build_data_health_report(source_rows, v2_rows);
    let mut rows = Vec::new();
    for spec in fillability_bucket_specs() {
        let mut buckets: BTreeMap<String, Vec<&FactorObservationV2>> = BTreeMap::new();
        for row in v2_rows {
            if let Some(bucket) = (spec.bucket)(row) {
                buckets.entry(bucket).or_default().push(row);
            }
        }
        for (bucket, bucket_rows) in buckets {
            rows.push(build_fillability_bucket_row(
                spec.dimension,
                bucket,
                &bucket_rows,
                v2_rows.len(),
                &options,
            ));
        }
    }
    rows.sort_by(|a, b| {
        fillability_decision_rank(b.decision)
            .cmp(&fillability_decision_rank(a.decision))
            .then_with(|| b.roundtrip_fill_rate.total_cmp(&a.roundtrip_fill_rate))
            .then_with(|| b.entry_fill_rate.total_cmp(&a.entry_fill_rate))
            .then_with(|| {
                b.total_executable_pnl_after_cost
                    .total_cmp(&a.total_executable_pnl_after_cost)
            })
    });
    FillabilityReviewReport {
        options,
        health,
        rows,
    }
}

pub fn liquidity_gate_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    options: LiquidityGateV1Options,
) -> LiquidityGateV1Report {
    let v2_rows = build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    build_liquidity_gate_v1_report(source_rows, &v2_rows, options)
}

pub fn liquidity_gate_v1_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    options: LiquidityGateV1Options,
) -> LiquidityGateV1Report {
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    build_liquidity_gate_v1_report(source_rows, &v2_rows, options)
}

fn build_liquidity_gate_v1_report(
    source_rows: &[FactorObservation],
    v2_rows: &[FactorObservationV2],
    options: LiquidityGateV1Options,
) -> LiquidityGateV1Report {
    let health = build_data_health_report(source_rows, v2_rows);
    let selected = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options))
        .collect::<Vec<_>>();
    let metrics = selection_metrics_for_rows(v2_rows.len(), &selected);
    LiquidityGateV1Report {
        options,
        health,
        selected_n: selected.len(),
        coverage: ratio(selected.len(), v2_rows.len()),
        entry_fill_rate: ratio(
            selected
                .iter()
                .filter(|row| row.label_executable_fillable)
                .count(),
            selected.len(),
        ),
        exit_fill_rate: ratio(
            selected
                .iter()
                .filter(|row| row.label_exit_fillable)
                .count(),
            selected.len(),
        ),
        roundtrip_fill_rate: ratio(
            selected
                .iter()
                .filter(|row| row.label_executable_fillable && row.label_exit_fillable)
                .count(),
            selected.len(),
        ),
        rejection_rate: ratio(
            selected
                .iter()
                .filter(|row| !row.label_executable_fillable)
                .count(),
            selected.len(),
        ),
        avg_entry_capacity_ratio: mean(selected.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(selected.iter().map(|row| row.exit_capacity_ratio)),
        avg_pm_spread_bps: mean(selected.iter().map(|row| row.pm_spread_bps)),
        avg_pm_lag_secs: mean(selected.iter().map(|row| row.pm_lag_secs)),
        avg_roundtrip_cost_usd: mean(selected.iter().map(|row| row.roundtrip_cost_usd)),
        metrics,
    }
}

pub fn liquidity_gated_alpha_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    mut options: LiquidityGatedAlphaV1Options,
) -> LiquidityGatedAlphaV1Report {
    options.gate.review = options.walk_forward.review.clone();
    let v2_rows = build_factor_observations_v2_with_deribit(
        source_rows,
        deribit,
        &options.walk_forward.review,
    );
    let baseline_health = build_data_health_report(source_rows, &v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, &v2_rows, options.gate.clone());
    let gated_rows = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
        .cloned()
        .collect::<Vec<_>>();
    let review = review_candidate_factor_rows(
        source_rows,
        &gated_rows,
        options.walk_forward.review.clone(),
    );
    let mut walk_rows = gated_rows.clone();
    let walk_forward = walk_forward_factor_rows(
        source_rows,
        &mut walk_rows,
        start,
        end,
        options.walk_forward.clone(),
    );
    let stability = build_factor_stability_report(&walk_forward, FactorStabilityOptions::default());
    LiquidityGatedAlphaV1Report {
        options,
        baseline_health,
        gate,
        review,
        walk_forward,
        stability,
    }
}

pub fn liquidity_gated_alpha_v1_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    mut options: LiquidityGatedAlphaV1Options,
) -> LiquidityGatedAlphaV1Report {
    options.gate.review = options.walk_forward.review.clone();
    let v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.walk_forward.review,
    );
    let baseline_health = build_data_health_report(source_rows, &v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, &v2_rows, options.gate.clone());
    let gated_rows = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
        .cloned()
        .collect::<Vec<_>>();
    let review = review_candidate_factor_rows(
        source_rows,
        &gated_rows,
        options.walk_forward.review.clone(),
    );
    let mut walk_rows = gated_rows.clone();
    let walk_forward = walk_forward_factor_rows(
        source_rows,
        &mut walk_rows,
        start,
        end,
        options.walk_forward.clone(),
    );
    let stability = build_factor_stability_report(&walk_forward, FactorStabilityOptions::default());
    LiquidityGatedAlphaV1Report {
        options,
        baseline_health,
        gate,
        review,
        walk_forward,
        stability,
    }
}

pub fn review_trade_formation_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    mut options: TradeFormationReviewOptions,
) -> TradeFormationReviewReport {
    options.gate.review = options.review.clone();
    let mut v2_rows =
        build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    build_trade_formation_v1_report(source_rows, &mut v2_rows, options)
}

pub fn review_trade_formation_v1_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    mut options: TradeFormationReviewOptions,
) -> TradeFormationReviewReport {
    options.gate.review = options.review.clone();
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    build_trade_formation_v1_report(source_rows, &mut v2_rows, options)
}

fn build_trade_formation_v1_report(
    source_rows: &[FactorObservation],
    v2_rows: &mut [FactorObservationV2],
    options: TradeFormationReviewOptions,
) -> TradeFormationReviewReport {
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, v2_rows, options.gate.clone());
    let gated = v2_rows
        .iter()
        .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
        .collect::<Vec<_>>();
    let rejected = v2_rows
        .iter()
        .filter(|row| !liquidity_gate_v1_accepts(row, &options.gate))
        .collect::<Vec<_>>();
    let profitable_gated = gated
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some_and(|pnl| pnl > 0.0))
        .collect::<Vec<_>>();
    let losing_gated = gated
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some_and(|pnl| pnl <= 0.0))
        .collect::<Vec<_>>();
    let missed_winners = rejected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some_and(|pnl| pnl > 0.0))
        .collect::<Vec<_>>();
    let profitable_paths = build_trade_formation_path_rows(
        &profitable_gated,
        gated.len(),
        options.min_path_observations,
    );
    let losing_paths =
        build_trade_formation_path_rows(&losing_gated, gated.len(), options.min_path_observations);
    let missed_winner_paths = build_trade_formation_path_rows(
        &missed_winners,
        rejected.len(),
        options.min_path_observations,
    );
    let meta_label_rules = build_trade_formation_rule_rows(&gated, options.min_path_observations);

    TradeFormationReviewReport {
        options,
        health,
        gate,
        gated_rows: gated.len(),
        profitable_gated_rows: profitable_gated.len(),
        losing_gated_rows: losing_gated.len(),
        missed_winner_rows: missed_winners.len(),
        profitable_paths,
        losing_paths,
        missed_winner_paths,
        meta_label_rules,
    }
}

pub fn walk_forward_meta_label_v1_with_deribit(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    mut options: MetaLabelWalkForwardOptions,
) -> MetaLabelWalkForwardReport {
    options.gate.review = options.review.clone();
    let mut v2_rows =
        build_factor_observations_v2_with_deribit(source_rows, deribit, &options.review);
    walk_forward_meta_label_v1_rows(source_rows, &mut v2_rows, start, end, options)
}

pub fn walk_forward_meta_label_v1_with_deribit_and_pm_books(
    source_rows: &[FactorObservation],
    deribit: &[DeribitFeatureSnapshot],
    pm_books: &[ResearchPmBookSnapshot],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    mut options: MetaLabelWalkForwardOptions,
) -> MetaLabelWalkForwardReport {
    options.gate.review = options.review.clone();
    let mut v2_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        source_rows,
        deribit,
        pm_books,
        &options.review,
    );
    walk_forward_meta_label_v1_rows(source_rows, &mut v2_rows, start, end, options)
}

fn walk_forward_meta_label_v1_rows(
    source_rows: &[FactorObservation],
    v2_rows: &mut [FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    options: MetaLabelWalkForwardOptions,
) -> MetaLabelWalkForwardReport {
    v2_rows.sort_by_key(|row| row.tick_ts);
    let health = build_data_health_report(source_rows, v2_rows);
    let gate = build_liquidity_gate_v1_report(source_rows, v2_rows, options.gate.clone());
    let mut windows = Vec::new();
    let train_duration = Duration::days(options.train_window_days.max(1));
    let test_duration = Duration::days(options.test_window_days.max(1));
    let step_duration = Duration::days(options.step_days.max(1));
    let event_ends = inferred_event_ends(v2_rows);
    let mut train_start = start;
    let mut window_index = 0usize;

    while train_start + train_duration + test_duration <= end + Duration::seconds(1) {
        let train_end = train_start + train_duration;
        let test_start = train_end;
        let test_end = test_start + test_duration;
        let (train_slice, test_slice) = event_disjoint_walk_forward_slices(
            v2_rows,
            &event_ends,
            train_start,
            train_end,
            test_start,
            test_end,
        );
        let train_gated = train_slice
            .into_iter()
            .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
            .collect::<Vec<_>>();
        let test_gated = test_slice
            .into_iter()
            .filter(|row| liquidity_gate_v1_accepts(row, &options.gate))
            .collect::<Vec<_>>();

        if train_gated.len() >= options.review.min_observations
            && test_gated.len() >= options.review.min_observations
        {
            for spec in meta_label_rule_specs() {
                let train = evaluate_meta_label_rule(&train_gated, spec.predicate);
                if train.selected_n < options.min_rule_observations {
                    continue;
                }
                let test = evaluate_meta_label_rule(&test_gated, spec.predicate);
                windows.push(MetaLabelWalkForwardWindow {
                    window_index,
                    train_start,
                    train_end,
                    test_start,
                    test_end,
                    rule: spec.name.to_string(),
                    train,
                    test,
                });
            }
        }

        window_index += 1;
        train_start += step_duration;
    }

    let aggregates = aggregate_meta_label_windows(&windows, &options);
    MetaLabelWalkForwardReport {
        options,
        health,
        gate,
        windows,
        aggregates,
    }
}

pub fn format_factor_walk_forward_v2_report(report: &FactorWalkForwardReport) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Walk-Forward V2 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} settlement_labels={} executable_pnl_rows={} deribit_rows={}\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.settlement_label_rows,
        report.health.executable_pnl_rows,
        report.health.deribit_rows,
    ));
    out.push_str(&format!(
        "entry_fill_rate={:.2}% rejection_rate={:.2}% exit_fill_rate={:.2}% avg_pm_lag_secs={:.2}\n",
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
        report.health.exit_fill_rate() * 100.0,
        report.health.avg_pm_lag_secs,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "stake_usd={:.2} train_window={} test_window={} step={} top_quantile={:.2} factor_name_filter={}\n\n",
        report.options.review.stake_usd,
        report.options.train_window_label(),
        report.options.test_window_label(),
        report.options.step_label(),
        report.options.review.top_quantile,
        report
            .options
            .factor_name_filter
            .as_deref()
            .unwrap_or("<none>"),
    ));

    out.push_str("=== Walk-Forward Aggregates By Test PnL ===\n");
    out.push_str("factor,family,layer,windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate\n");
    for aggregate in report.aggregates.iter().take(report.options.top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            aggregate.factor,
            aggregate.family.as_str(),
            aggregate.layer.as_str(),
            aggregate.windows,
            aggregate.positive_window_ratio,
            aggregate.total_test_pnl_after_cost,
            aggregate.avg_test_pnl_per_window,
            aggregate.min_test_pnl_after_cost,
            aggregate.avg_test_fill_rate,
            aggregate.avg_test_rejection_rate,
        ));
    }

    out.push_str("\n=== Walk-Forward Windows ===\n");
    out.push_str("window,train_start,train_end,test_start,test_end,factor,family,layer,direction,threshold,train_pnl_rank_ic,train_settle_rank_ic,train_selected,train_pnl,test_selected,test_fill,test_reject,test_pnl,test_avg_pnl,test_sharpe,test_max_dd,symbol_pos,time_bucket_pos\n");
    let mut displayed_by_window: BTreeMap<usize, usize> = BTreeMap::new();
    for window in &report.windows {
        let displayed = displayed_by_window.entry(window.window_index).or_insert(0);
        if *displayed >= report.options.top_n.max(1) {
            continue;
        }
        *displayed += 1;
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{:.0},{:.8},{:.4},{:.4},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            window.window_index,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            window.factor,
            window.family.as_str(),
            window.layer.as_str(),
            window.direction,
            window.threshold,
            window.train_executable_pnl_rank_ic,
            window.train_settlement_rank_ic,
            window.train.selected_n,
            window.train.total_pnl_after_cost,
            window.test.selected_n,
            window.test.executable_fill_rate,
            window.test.rejection_rate,
            window.test.total_pnl_after_cost,
            window.test.avg_pnl_after_cost,
            window.test.sharpe,
            window.test.max_drawdown,
            window.test.by_symbol_positive_ratio,
            window.test.by_time_bucket_positive_ratio,
        ));
    }
    out
}

pub fn format_factor_stability_report(report: &FactorStabilityReport, top_n: usize) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Stability Report ===\n");
    out.push_str(&format!(
        "min_windows={} min_pos_window_ratio={:.2} min_fill_rate={:.2} max_reject_rate={:.2} min_abs_pnl_icir={:.2}\n\n",
        report.options.min_windows,
        report.options.min_positive_window_ratio,
        report.options.min_avg_fill_rate,
        report.options.max_avg_rejection_rate,
        report.options.min_abs_executable_pnl_icir,
    ));
    out.push_str("decision,factor,family,layer,windows,settle_ic_mean,settle_icir,pnl_ic_mean,pnl_icir,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate,symbol_pos,time_bucket_pos,reason\n");
    for row in report.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{}\n",
            row.decision.as_str(),
            row.factor,
            row.family.as_str(),
            row.layer.as_str(),
            row.windows,
            row.settlement_rank_ic_mean,
            row.settlement_rank_icir,
            row.executable_pnl_rank_ic_mean,
            row.executable_pnl_rank_icir,
            row.positive_window_ratio,
            row.total_test_pnl_after_cost,
            row.avg_test_pnl_per_window,
            row.min_test_pnl_after_cost,
            row.avg_test_fill_rate,
            row.avg_test_rejection_rate,
            row.avg_by_symbol_positive_ratio,
            row.avg_by_time_bucket_positive_ratio,
            row.reason,
        ));
    }
    out
}

pub fn format_factor_combo_v1_report(report: &FactorComboV1Report) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Combo V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} deribit_rows={} entry_fill_rate={:.2}% rejection_rate={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.deribit_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "train_window={} test_window={} step={} top_quantile={:.2} max_family={} max_total={} min_abs_train_pnl_ic={:.4} factor_name_filter={}\n\n",
        report.options.walk_forward.train_window_label(),
        report.options.walk_forward.test_window_label(),
        report.options.walk_forward.step_label(),
        report.options.walk_forward.review.top_quantile,
        report.options.max_factors_per_family,
        report.options.max_total_factors,
        report.options.min_abs_train_executable_pnl_rank_ic,
        report
            .options
            .walk_forward
            .factor_name_filter
            .as_deref()
            .unwrap_or("<none>"),
    ));
    out.push_str("=== Combo V1 Aggregate ===\n");
    out.push_str("windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate,avg_component_count\n");
    out.push_str(&format!(
        "{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2}\n\n",
        report.aggregate.windows,
        report.aggregate.positive_window_ratio,
        report.aggregate.total_test_pnl_after_cost,
        report.aggregate.avg_test_pnl_per_window,
        report.aggregate.min_test_pnl_after_cost,
        report.aggregate.avg_test_fill_rate,
        report.aggregate.avg_test_rejection_rate,
        report.aggregate.avg_component_count,
    ));

    out.push_str("=== Combo V1 Windows ===\n");
    out.push_str("window,train_start,train_end,test_start,test_end,threshold,components,train_selected,train_pnl,test_selected,test_fill,test_reject,test_pnl,test_avg_pnl,test_sharpe,test_max_dd,symbol_pos,time_bucket_pos\n");
    for window in &report.windows {
        let components = window
            .components
            .iter()
            .map(|component| {
                format!(
                    "{}:{:.3}",
                    component.factor, component.train_executable_pnl_rank_ic
                )
            })
            .collect::<Vec<_>>()
            .join("+");
        out.push_str(&format!(
            "{},{},{},{},{},{:.8},{},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            window.window_index,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            window.threshold,
            components,
            window.train.selected_n,
            window.train.total_pnl_after_cost,
            window.test.selected_n,
            window.test.executable_fill_rate,
            window.test.rejection_rate,
            window.test.total_pnl_after_cost,
            window.test.avg_pnl_after_cost,
            window.test.sharpe,
            window.test.max_drawdown,
            window.test.by_symbol_positive_ratio,
            window.test.by_time_bucket_positive_ratio,
        ));
    }
    out
}

pub fn format_fillability_review_v1_report(
    report: &FillabilityReviewReport,
    top_n: usize,
) -> String {
    let mut out = String::new();
    out.push_str("=== Fillability Review V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} entry_fill_rate={:.2}% exit_fill_rate={:.2}% rejection_rate={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.exit_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "stake_usd={:.2} min_bucket_obs={} min_entry_fill={:.2} min_roundtrip_fill={:.2} max_reject={:.2}\n\n",
        report.options.review.stake_usd,
        report.options.min_bucket_observations,
        report.options.min_entry_fill_rate,
        report.options.min_roundtrip_fill_rate,
        report.options.max_rejection_rate,
    ));
    out.push_str("decision,dimension,bucket,n,coverage,entry_fill,exit_fill,roundtrip_fill,reject_rate,avg_entry_cap,avg_exit_cap,avg_entry_liq,avg_exit_liq,avg_pm_spread_bps,avg_pm_lag_secs,avg_slippage_bps,avg_roundtrip_cost,total_pnl,avg_pnl,reason\n");
    for row in report.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.2},{:.4},{:.4},{:.4},{}\n",
            row.decision.as_str(),
            row.dimension,
            row.bucket,
            row.n,
            row.coverage,
            row.entry_fill_rate,
            row.exit_fill_rate,
            row.roundtrip_fill_rate,
            row.rejection_rate,
            row.avg_entry_capacity_ratio,
            row.avg_exit_capacity_ratio,
            row.avg_entry_liquidity_usd,
            row.avg_exit_liquidity_usd,
            row.avg_pm_spread_bps,
            row.avg_pm_lag_secs,
            row.avg_slippage_to_fill_bps,
            row.avg_roundtrip_cost_usd,
            row.total_executable_pnl_after_cost,
            row.avg_executable_pnl_after_cost,
            row.reason,
        ));
    }
    out
}

pub fn format_liquidity_gate_v1_report(report: &LiquidityGateV1Report) -> String {
    let mut out = String::new();
    out.push_str("=== Liquidity Gate V1 Report ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} selected={} coverage={:.4} stake_usd={:.2}\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.selected_n,
        report.coverage,
        report.options.review.stake_usd,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "min_entry_cap={:.2} min_exit_cap={:.2} max_pm_lag_secs={:.2} max_pm_spread_bps={:.2} min_time_remaining_secs={} entry_ask=[{:.2},{:.2}]\n",
        report.options.min_entry_capacity_ratio,
        report.options.min_exit_capacity_ratio,
        report.options.max_pm_lag_secs,
        report.options.max_pm_spread_bps,
        report.options.min_time_remaining_secs,
        report.options.min_entry_ask,
        report.options.max_entry_ask,
    ));
    out.push_str("entry_fill,exit_fill,roundtrip_fill,reject_rate,avg_entry_cap,avg_exit_cap,avg_pm_spread_bps,avg_pm_lag_secs,avg_roundtrip_cost,total_pnl,avg_pnl,sharpe,max_dd,symbol_pos,time_bucket_pos\n");
    out.push_str(&format!(
        "{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
        report.entry_fill_rate,
        report.exit_fill_rate,
        report.roundtrip_fill_rate,
        report.rejection_rate,
        report.avg_entry_capacity_ratio,
        report.avg_exit_capacity_ratio,
        report.avg_pm_spread_bps,
        report.avg_pm_lag_secs,
        report.avg_roundtrip_cost_usd,
        report.metrics.total_pnl_after_cost,
        report.metrics.avg_pnl_after_cost,
        report.metrics.sharpe,
        report.metrics.max_drawdown,
        report.metrics.by_symbol_positive_ratio,
        report.metrics.by_time_bucket_positive_ratio,
    ));
    out
}

pub fn format_liquidity_gated_alpha_v1_report(
    report: &LiquidityGatedAlphaV1Report,
    top_n: usize,
) -> String {
    let mut out = String::new();
    out.push_str("=== Liquidity-Gated Alpha V1 Data Health ===\n");
    out.push_str(&format!(
        "baseline_v2_rows={} baseline_entry_fill={:.2}% baseline_reject={:.2}% gated_rows={} gate_coverage={:.4} gate_entry_fill={:.2}% gate_roundtrip_fill={:.2}% gate_reject={:.2}%\n",
        report.baseline_health.v2_rows,
        report.baseline_health.entry_fill_rate() * 100.0,
        report.baseline_health.rejection_rate() * 100.0,
        report.gate.selected_n,
        report.gate.coverage,
        report.gate.entry_fill_rate * 100.0,
        report.gate.roundtrip_fill_rate * 100.0,
        report.gate.rejection_rate * 100.0,
    ));
    push_full_depth_health_line(&mut out, &report.baseline_health);
    out.push_str(&format!(
        "stake_usd={:.2} train_window={} test_window={} step={} top_quantile={:.2} factor_name_filter={}\n\n",
        report.options.walk_forward.review.stake_usd,
        report.options.walk_forward.train_window_label(),
        report.options.walk_forward.test_window_label(),
        report.options.walk_forward.step_label(),
        report.options.walk_forward.review.top_quantile,
        report
            .options
            .walk_forward
            .factor_name_filter
            .as_deref()
            .unwrap_or("<none>"),
    ));

    out.push_str("=== Liquidity-Gated Single-Factor Reviews ===\n");
    out.push_str("factor,family,layer,n,coverage,settle_rank_ic,pnl_rank_ic,selected_n,fill_rate,reject_rate,total_pnl,avg_pnl,sharpe,max_dd,symbol_pos,time_bucket_pos\n");
    for review in report.review.reviews.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            review.factor,
            review.family.as_str(),
            review.layer.as_str(),
            review.n,
            review.coverage,
            review.settlement_rank_ic,
            review.executable_pnl_rank_ic,
            review.selected_n,
            review.selected_executable_fill_rate,
            review.selected_rejection_rate,
            review.selected_total_pnl_after_cost,
            review.selected_avg_pnl_after_cost,
            review.selected_sharpe,
            review.selected_max_drawdown,
            review.by_symbol_positive_ratio,
            review.by_time_bucket_positive_ratio,
        ));
    }

    out.push_str("\n=== Liquidity-Gated Walk-Forward Aggregates ===\n");
    out.push_str("factor,family,layer,windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_fill_rate,avg_reject_rate\n");
    for aggregate in report.walk_forward.aggregates.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            aggregate.factor,
            aggregate.family.as_str(),
            aggregate.layer.as_str(),
            aggregate.windows,
            aggregate.positive_window_ratio,
            aggregate.total_test_pnl_after_cost,
            aggregate.avg_test_pnl_per_window,
            aggregate.min_test_pnl_after_cost,
            aggregate.avg_test_fill_rate,
            aggregate.avg_test_rejection_rate,
        ));
    }

    out.push_str("\n=== Liquidity-Gated Factor Stability ===\n");
    out.push_str("decision,factor,family,layer,windows,pnl_ic_mean,pnl_icir,pos_window_ratio,total_test_pnl,avg_fill_rate,avg_reject_rate,symbol_pos,time_bucket_pos,reason\n");
    for row in report.stability.rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{}\n",
            row.decision.as_str(),
            row.factor,
            row.family.as_str(),
            row.layer.as_str(),
            row.windows,
            row.executable_pnl_rank_ic_mean,
            row.executable_pnl_rank_icir,
            row.positive_window_ratio,
            row.total_test_pnl_after_cost,
            row.avg_test_fill_rate,
            row.avg_test_rejection_rate,
            row.avg_by_symbol_positive_ratio,
            row.avg_by_time_bucket_positive_ratio,
            row.reason,
        ));
    }
    out
}

pub fn format_trade_formation_v1_report(report: &TradeFormationReviewReport) -> String {
    let mut out = String::new();
    out.push_str("=== Trade Formation Review V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} baseline_entry_fill={:.2}% baseline_reject={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "gated_rows={} gate_coverage={:.4} gate_entry_fill={:.2}% gate_roundtrip_fill={:.2}% gate_reject={:.2}% profitable_gated={} losing_gated={} missed_winners={}\n",
        report.gated_rows,
        report.gate.coverage,
        report.gate.entry_fill_rate * 100.0,
        report.gate.roundtrip_fill_rate * 100.0,
        report.gate.rejection_rate * 100.0,
        report.profitable_gated_rows,
        report.losing_gated_rows,
        report.missed_winner_rows,
    ));
    out.push_str(&format!(
        "stake_usd={:.2} min_path_obs={} top_n={}\n\n",
        report.options.review.stake_usd, report.options.min_path_observations, report.options.top_n,
    ));
    push_trade_formation_path_section(
        &mut out,
        "Profitable Paths",
        &report.profitable_paths,
        report.options.top_n,
    );
    push_trade_formation_path_section(
        &mut out,
        "Losing Paths",
        &report.losing_paths,
        report.options.top_n,
    );
    push_trade_formation_path_section(
        &mut out,
        "Missed Winner Paths",
        &report.missed_winner_paths,
        report.options.top_n,
    );

    out.push_str("\n=== Meta-Label Rule Candidates ===\n");
    out.push_str("rule,n,coverage,win_rate,total_pnl,avg_pnl,sharpe,avg_future_exit_pnl_30s\n");
    for row in report
        .meta_label_rules
        .iter()
        .take(report.options.top_n.max(1))
    {
        out.push_str(&format!(
            "{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            row.rule,
            row.n,
            row.coverage,
            row.win_rate,
            row.total_pnl_after_cost,
            row.avg_pnl_after_cost,
            row.sharpe,
            row.avg_future_exit_pnl_30s,
        ));
    }
    out
}

fn push_trade_formation_path_section(
    out: &mut String,
    title: &str,
    rows: &[TradeFormationPathRow],
    top_n: usize,
) {
    out.push_str(&format!("\n=== {title} ===\n"));
    out.push_str("path,n,coverage,executable_rows,win_rate,settle_win_rate,total_pnl,avg_pnl,sharpe,avg_model_prob,avg_distance_sigma,avg_obi10,avg_obi_persist,avg_continuation,avg_entry_cap,avg_exit_cap,avg_pm_spread,avg_future_exit_pnl_30s,avg_future_bid_chg_30s,avg_future_bid_chg_60s\n");
    for row in rows.iter().take(top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.2},{:.4},{:.4},{:.4}\n",
            row.path,
            row.n,
            row.coverage,
            row.executable_rows,
            row.win_rate,
            row.settlement_win_rate,
            row.total_pnl_after_cost,
            row.avg_pnl_after_cost,
            row.sharpe,
            row.avg_side_model_prob,
            row.avg_side_distance_over_sigma,
            row.avg_obi_10_side,
            row.avg_obi_persistence_30s_side,
            row.avg_cex_continuation_score_side,
            row.avg_entry_capacity_ratio,
            row.avg_exit_capacity_ratio,
            row.avg_pm_spread_bps,
            row.avg_future_exit_pnl_30s,
            row.avg_future_exit_bid_change_30s,
            row.avg_future_exit_bid_change_60s,
        ));
    }
}

pub fn format_meta_label_walk_forward_v1_report(report: &MetaLabelWalkForwardReport) -> String {
    let mut out = String::new();
    out.push_str("=== Meta-Label Walk-Forward V1 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} executable_pnl_rows={} baseline_entry_fill={:.2}% baseline_reject={:.2}%\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.executable_pnl_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "gate_selected={} gate_coverage={:.4} gate_entry_fill={:.2}% gate_roundtrip_fill={:.2}% gate_reject={:.2}%\n",
        report.gate.selected_n,
        report.gate.coverage,
        report.gate.entry_fill_rate * 100.0,
        report.gate.roundtrip_fill_rate * 100.0,
        report.gate.rejection_rate * 100.0,
    ));
    out.push_str(&format!(
        "train_days={} test_days={} step_days={} min_rule_obs={} min_candidate_windows={} min_candidate_pos_window_ratio={:.2} min_candidate_avg_selected={:.2} stake_usd={:.2}\n\n",
        report.options.train_window_days,
        report.options.test_window_days,
        report.options.step_days,
        report.options.min_rule_observations,
        report.options.min_candidate_windows,
        report.options.min_candidate_positive_window_ratio,
        report.options.min_candidate_avg_test_selected,
        report.options.review.stake_usd,
    ));

    out.push_str("=== Meta-Label Walk-Forward Aggregates ===\n");
    out.push_str("rule,decision,reason,windows,pos_window_ratio,total_test_pnl,avg_window_pnl,min_window_pnl,avg_train_selected,avg_test_selected,avg_test_fill,avg_test_reject\n");
    for aggregate in report.aggregates.iter().take(report.options.top_n.max(1)) {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.2},{:.2},{:.4},{:.4}\n",
            aggregate.rule,
            aggregate.decision.as_str(),
            aggregate.reason,
            aggregate.windows,
            aggregate.positive_window_ratio,
            aggregate.total_test_pnl_after_cost,
            aggregate.avg_test_pnl_per_window,
            aggregate.min_test_pnl_after_cost,
            aggregate.avg_train_selected,
            aggregate.avg_test_selected,
            aggregate.avg_test_fill_rate,
            aggregate.avg_test_rejection_rate,
        ));
    }

    out.push_str("\n=== Meta-Label Walk-Forward Windows ===\n");
    out.push_str("window,rule,train_start,train_end,test_start,test_end,train_selected,train_pnl,train_avg_pnl,train_sharpe,test_selected,test_fill,test_reject,test_pnl,test_avg_pnl,test_sharpe,test_max_dd,symbol_pos,time_bucket_pos\n");
    for window in &report.windows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            window.window_index,
            window.rule,
            window.train_start,
            window.train_end,
            window.test_start,
            window.test_end,
            window.train.selected_n,
            window.train.total_pnl_after_cost,
            window.train.avg_pnl_after_cost,
            window.train.sharpe,
            window.test.selected_n,
            window.test.executable_fill_rate,
            window.test.rejection_rate,
            window.test.total_pnl_after_cost,
            window.test.avg_pnl_after_cost,
            window.test.sharpe,
            window.test.max_drawdown,
            window.test.by_symbol_positive_ratio,
            window.test.by_time_bucket_positive_ratio,
        ));
    }
    out
}

pub fn format_factor_review_v2_report(report: &FactorReviewV2Report, top_n: usize) -> String {
    let mut out = String::new();
    out.push_str("=== Factor Review V2 Data Health ===\n");
    out.push_str(&format!(
        "source_obs={} v2_rows={} settlement_labels={} executable_pnl_rows={} deribit_rows={}\n",
        report.health.source_observations,
        report.health.v2_rows,
        report.health.settlement_label_rows,
        report.health.executable_pnl_rows,
        report.health.deribit_rows,
    ));
    out.push_str(&format!(
        "entry_quote_rows={} entry_size_rows={} entry_fill_rate={:.2}% rejection_rate={:.2}%\n",
        report.health.entry_quote_rows,
        report.health.entry_size_rows,
        report.health.entry_fill_rate() * 100.0,
        report.health.rejection_rate() * 100.0,
    ));
    out.push_str(&format!(
        "exit_quote_rows={} exit_size_rows={} exit_fill_rate={:.2}% avg_pm_lag_secs={:.2}\n",
        report.health.exit_quote_rows,
        report.health.exit_size_rows,
        report.health.exit_fill_rate() * 100.0,
        report.health.avg_pm_lag_secs,
    ));
    push_full_depth_health_line(&mut out, &report.health);
    out.push_str(&format!(
        "stake_usd={:.2} top_quantile={:.2} min_observations={}\n\n",
        report.options.stake_usd, report.options.top_quantile, report.options.min_observations,
    ));

    let top_n = top_n.max(1);
    out.push_str("=== Top Tradable Single-Factor Reviews By Executable PnL ===\n");
    push_single_factor_review_rows(
        &mut out,
        report
            .reviews
            .iter()
            .filter(|review| !is_future_exit_diagnostic_factor(&review.factor))
            .take(top_n),
    );

    let diagnostics = report
        .reviews
        .iter()
        .filter(|review| is_future_exit_diagnostic_factor(&review.factor))
        .take(top_n.min(10))
        .collect::<Vec<_>>();
    if !diagnostics.is_empty() {
        out.push_str("\n=== Future Exit Diagnostics Not Tradable Factors ===\n");
        push_single_factor_review_rows(&mut out, diagnostics);
    }
    out.push_str("\n=== Executable EV Buckets: Best Non-Underpowered Avg PnL ===\n");
    push_executable_ev_bucket_rows(
        &mut out,
        sorted_executable_ev_buckets(report, true)
            .into_iter()
            .take(top_n),
    );
    out.push_str("\n=== Executable EV Buckets: Worst Avg PnL ===\n");
    push_executable_ev_bucket_rows(
        &mut out,
        sorted_executable_ev_buckets(report, false)
            .into_iter()
            .take(top_n),
    );
    out.push_str("\n=== Direction Side Audit: Favored vs Opposite Executable EV ===\n");
    push_direction_side_audit_rows(&mut out, report.direction_side_audit.iter().take(top_n * 2));
    out.push_str("\n=== Binance/CEX Direction Audit: Settlement Predictive Buckets ===\n");
    push_binance_direction_audit_rows(
        &mut out,
        sorted_binance_direction_audit(report)
            .into_iter()
            .take(top_n * 2),
    );
    out
}

fn push_full_depth_health_line(out: &mut String, health: &DataHealthReport) {
    out.push_str(&format!(
        "full_depth_entry_fill_rate={:.2}% full_depth_exit_fill_rate={:.2}% full_depth_pnl_rows={} avg_entry_sweep_slip_bps={:.2} avg_exit_sweep_slip_bps={:.2}\n",
        health.full_depth_entry_fill_rate() * 100.0,
        health.full_depth_exit_fill_rate() * 100.0,
        health.full_depth_executable_pnl_rows,
        health.avg_entry_sweep_slippage_bps,
        health.avg_exit_sweep_slippage_bps,
    ));
}

fn push_single_factor_review_rows<'a, I>(out: &mut String, reviews: I)
where
    I: IntoIterator<Item = &'a SingleFactorReview>,
{
    out.push_str("factor,family,layer,n,coverage,settle_rank_ic,pnl_rank_ic,selected_n,fill_rate,reject_rate,total_pnl,avg_pnl,sharpe,max_dd,symbol_pos,time_bucket_pos\n");
    for review in reviews {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            review.factor,
            review.family.as_str(),
            review.layer.as_str(),
            review.n,
            review.coverage,
            review.settlement_rank_ic,
            review.executable_pnl_rank_ic,
            review.selected_n,
            review.selected_executable_fill_rate,
            review.selected_rejection_rate,
            review.selected_total_pnl_after_cost,
            review.selected_avg_pnl_after_cost,
            review.selected_sharpe,
            review.selected_max_drawdown,
            review.by_symbol_positive_ratio,
            review.by_time_bucket_positive_ratio,
        ));
    }
}

fn sorted_executable_ev_buckets(
    report: &FactorReviewV2Report,
    best_first: bool,
) -> Vec<&ExecutableEvBucketSummary> {
    let mut buckets: Vec<&ExecutableEvBucketSummary> = report
        .executable_ev_buckets
        .iter()
        .filter(|bucket| bucket.avg_pnl_15u.is_finite())
        .filter(|bucket| !best_first || !bucket.underpowered)
        .collect();
    buckets.sort_by(|a, b| {
        let ordering = if best_first {
            b.avg_pnl_15u.total_cmp(&a.avg_pnl_15u)
        } else {
            a.avg_pnl_15u.total_cmp(&b.avg_pnl_15u)
        };
        ordering
            .then_with(|| a.dimension.cmp(&b.dimension))
            .then_with(|| a.bucket.cmp(&b.bucket))
    });
    buckets
}

fn push_executable_ev_bucket_rows<'a, I>(out: &mut String, buckets: I)
where
    I: IntoIterator<Item = &'a ExecutableEvBucketSummary>,
{
    out.push_str("dimension,bucket,rows,fillable,fill_rate,pnl_rows,total_pnl,avg_pnl,roi,t_stat,underpowered,avg_prob,avg_edge,avg_entry,avg_capacity,avg_entry_liquidity,avg_shortfall,avg_slippage,avg_roundtrip_cost\n");
    for bucket in buckets {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{},{:.4},{:.4},{:.4},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            bucket.dimension,
            bucket.bucket,
            bucket.rows,
            bucket.fillable_rows,
            bucket.fill_rate,
            bucket.pnl_rows,
            bucket.total_pnl_15u,
            bucket.avg_pnl_15u,
            bucket.roi_on_stake,
            bucket.t_stat,
            bucket.underpowered,
            bucket.avg_side_model_prob,
            bucket.avg_side_model_edge,
            bucket.avg_entry_ask,
            bucket.avg_entry_capacity_ratio,
            bucket.avg_entry_liquidity_usd,
            bucket.avg_liquidity_shortfall_usd,
            bucket.avg_slippage_to_fill_bps,
            bucket.avg_roundtrip_cost_usd,
        ));
    }
}

fn push_direction_side_audit_rows<'a, I>(out: &mut String, summaries: I)
where
    I: IntoIterator<Item = &'a DirectionSideAuditSummary>,
{
    out.push_str("selector,bucket,pairs,pnl_pairs,avg_margin,favored_fill_rate,favored_settle_win,favored_pnl_rows,favored_avg_pnl,favored_t_stat,favored_supported,opposite_fill_rate,opposite_settle_win,opposite_pnl_rows,opposite_avg_pnl,opposite_t_stat,pnl_delta\n");
    for summary in summaries {
        out.push_str(&format!(
            "{},{},{},{},{:.4},{:.4},{:.4},{},{:.4},{:.4},{},{:.4},{:.4},{},{:.4},{:.4},{:.4}\n",
            summary.selector,
            summary.bucket,
            summary.pairs,
            summary.pnl_pair_rows,
            summary.avg_selector_margin,
            summary.favored.fill_rate,
            summary.favored.settlement_win_rate,
            summary.favored.pnl_rows,
            summary.favored.avg_pnl_15u,
            summary.favored.t_stat,
            summary.favored.statistically_supported,
            summary.opposite.fill_rate,
            summary.opposite.settlement_win_rate,
            summary.opposite.pnl_rows,
            summary.opposite.avg_pnl_15u,
            summary.opposite.t_stat,
            summary.avg_pnl_delta_15u,
        ));
    }
}

fn sorted_binance_direction_audit(
    report: &FactorReviewV2Report,
) -> Vec<&BinanceDirectionBucketSummary> {
    let mut rows = report
        .binance_direction_audit
        .iter()
        .filter(|summary| summary.settlement_win_rate.is_finite())
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.t_stat_vs_coinflip
            .total_cmp(&a.t_stat_vs_coinflip)
            .then_with(|| b.lift_vs_coinflip.total_cmp(&a.lift_vs_coinflip))
            .then_with(|| a.factor.cmp(&b.factor))
            .then_with(|| a.bucket.cmp(&b.bucket))
    });
    rows
}

fn push_binance_direction_audit_rows<'a, I>(out: &mut String, summaries: I)
where
    I: IntoIterator<Item = &'a BinanceDirectionBucketSummary>,
{
    out.push_str("factor,family,layer,bucket,rows,fillable,fill_rate,settlement_rows,win_rate,lift,t_stat,dir_supported,pnl_rows,total_pnl,avg_pnl,roi,pnl_t_stat,positive_ev,ev_supported,avg_value,min_value,max_value,avg_entry,avg_capacity,avg_entry_liquidity,avg_sweep_slippage,symbol_pos,time_bucket_pos\n");
    for summary in summaries {
        out.push_str(&format!(
            "{},{},{},{},{},{},{:.4},{},{:.4},{:.4},{:.4},{},{},{:.4},{:.4},{:.4},{:.4},{},{},{:.6},{:.6},{:.6},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4}\n",
            summary.factor,
            summary.family.as_str(),
            summary.layer.as_str(),
            summary.bucket,
            summary.rows,
            summary.fillable_rows,
            summary.fill_rate,
            summary.settlement_rows,
            summary.settlement_win_rate,
            summary.lift_vs_coinflip,
            summary.t_stat_vs_coinflip,
            summary.statistically_supported,
            summary.pnl_rows,
            summary.total_pnl_15u,
            summary.avg_pnl_15u,
            summary.roi_on_stake,
            summary.pnl_t_stat,
            summary.positive_ev,
            summary.executable_ev_supported,
            summary.avg_factor_value,
            summary.min_factor_value,
            summary.max_factor_value,
            summary.avg_entry_ask,
            summary.avg_entry_capacity_ratio,
            summary.avg_entry_liquidity_usd,
            summary.avg_entry_sweep_slippage_bps,
            summary.by_symbol_positive_ratio,
            summary.by_time_bucket_positive_ratio,
        ));
    }
}

fn build_binance_direction_audit(
    rows: &[FactorObservationV2],
    options: &FactorReviewOptions,
) -> Vec<BinanceDirectionBucketSummary> {
    let min_sample = options.min_observations.max(30);
    let mut summaries = Vec::new();
    for descriptor in factor_v2_descriptors()
        .into_iter()
        .filter(is_binance_direction_descriptor)
    {
        append_binance_direction_factor(
            &mut summaries,
            rows,
            descriptor,
            options,
            options.stake_usd,
            min_sample,
        );
    }
    summaries.sort_by(|a, b| {
        a.factor
            .cmp(&b.factor)
            .then_with(|| audit_bucket_sort_key(&a.bucket).cmp(&audit_bucket_sort_key(&b.bucket)))
            .then_with(|| a.bucket.cmp(&b.bucket))
    });
    summaries
}

fn is_binance_direction_descriptor(descriptor: &FactorV2Descriptor) -> bool {
    matches!(
        descriptor.name,
        "side_model_prob"
            | "side_distance_over_sigma"
            | "drift_10s"
            | "drift_30s"
            | "obi_10_side"
            | "depth_imbalance_side"
            | "depth_acceleration_side"
            | "microprice_offset_side_bps"
            | "cum_mprice_drift_5m_side"
            | "cum_trade_imbalance_5m_side"
            | "obi_delta_10s_side"
            | "obi_delta_30s_side"
            | "obi_persistence_30s_side"
            | "depth_imbalance_delta_30s_side"
            | "microprice_momentum_30s_side"
            | "trade_imbalance_delta_10s_side"
            | "trade_imbalance_delta_30s_side"
            | "cex_bar_return_30s_side"
            | "cex_bar_return_60s_side"
            | "cex_signed_volume_ratio_30s_side"
            | "cex_consecutive_bar_side"
            | "cex_breakout_volume_side"
            | "cex_continuation_score_side"
    )
}

fn append_binance_direction_factor(
    summaries: &mut Vec<BinanceDirectionBucketSummary>,
    rows: &[FactorObservationV2],
    descriptor: FactorV2Descriptor,
    options: &FactorReviewOptions,
    stake_usd: f64,
    min_sample: usize,
) {
    let mut scored = rows
        .iter()
        .filter_map(|row| {
            let value = (descriptor.accessor)(row);
            (value.is_finite() && row.label_settlement_win.is_some()).then_some((row, value))
        })
        .collect::<Vec<_>>();
    if scored.len() < min_sample {
        return;
    }
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    let selected_n = ((scored.len() as f64) * options.top_quantile.clamp(0.01, 1.0))
        .ceil()
        .max(1.0) as usize;
    let selected_n = selected_n.min(scored.len());
    let bottom = scored.iter().take(selected_n).copied().collect::<Vec<_>>();
    let top = scored
        .iter()
        .rev()
        .take(selected_n)
        .copied()
        .collect::<Vec<_>>();

    summaries.push(summarize_binance_direction_bucket(
        descriptor,
        "bottom_quantile",
        &bottom,
        stake_usd,
        min_sample,
    ));
    summaries.push(summarize_binance_direction_bucket(
        descriptor,
        "top_quantile",
        &top,
        stake_usd,
        min_sample,
    ));
}

fn summarize_binance_direction_bucket(
    descriptor: FactorV2Descriptor,
    bucket: &str,
    rows: &[(&FactorObservationV2, f64)],
    stake_usd: f64,
    min_sample: usize,
) -> BinanceDirectionBucketSummary {
    let labels = rows
        .iter()
        .filter_map(|(row, _)| row.label_settlement_win)
        .filter(|label| label.is_finite())
        .collect::<Vec<_>>();
    let settlement_win_rows = labels.iter().filter(|label| **label >= 0.5).count();
    let settlement_win_rate = ratio(settlement_win_rows, labels.len());
    let t_stat_vs_coinflip = settlement_t_stat_vs_coinflip(settlement_win_rows, labels.len());
    let pnl_values = rows
        .iter()
        .filter_map(|(row, _)| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl_15u = pnl_values.iter().sum::<f64>();
    let avg_pnl_15u = if pnl_values.is_empty() {
        f64::NAN
    } else {
        total_pnl_15u / pnl_values.len() as f64
    };
    let roi_on_stake = if stake_usd > 0.0 && !pnl_values.is_empty() {
        total_pnl_15u / (stake_usd * pnl_values.len() as f64)
    } else {
        f64::NAN
    };
    let pnl_t_stat = trade_t_stat(&pnl_values);
    let positive_ev = avg_pnl_15u.is_finite() && avg_pnl_15u > 0.0;
    let executable_ev_supported = positive_ev
        && pnl_values.len() >= min_sample
        && pnl_t_stat.is_finite()
        && pnl_t_stat >= 2.0;
    let values = rows.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    let selected_rows = rows.iter().map(|(row, _)| *row).collect::<Vec<_>>();
    BinanceDirectionBucketSummary {
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        bucket: bucket.to_string(),
        rows: rows.len(),
        fillable_rows: selected_rows
            .iter()
            .filter(|row| entry_fillable(row))
            .count(),
        fill_rate: ratio(
            selected_rows
                .iter()
                .filter(|row| entry_fillable(row))
                .count(),
            selected_rows.len(),
        ),
        settlement_rows: labels.len(),
        settlement_win_rows,
        settlement_win_rate,
        lift_vs_coinflip: settlement_win_rate - 0.5,
        t_stat_vs_coinflip,
        pnl_rows: pnl_values.len(),
        total_pnl_15u,
        avg_pnl_15u,
        roi_on_stake,
        pnl_t_stat,
        positive_ev,
        executable_ev_supported,
        avg_factor_value: mean(values.iter().copied()),
        min_factor_value: finite_min(values.iter().copied()),
        max_factor_value: finite_max(values.iter().copied()),
        avg_entry_ask: mean(selected_rows.iter().map(|row| row.entry_ask)),
        avg_entry_capacity_ratio: mean(selected_rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_entry_liquidity_usd: mean(selected_rows.iter().map(|row| row.entry_liquidity_usd)),
        avg_entry_sweep_slippage_bps: mean(
            selected_rows.iter().map(|row| row.entry_sweep_slippage_bps),
        ),
        by_symbol_positive_ratio: settlement_positive_group_ratio(&selected_rows, |row| {
            row.symbol.clone()
        }),
        by_time_bucket_positive_ratio: settlement_positive_group_ratio(&selected_rows, |row| {
            row.regime.as_str().to_string()
        }),
        statistically_supported: labels.len() >= min_sample
            && settlement_win_rate.is_finite()
            && settlement_win_rate > 0.5
            && t_stat_vs_coinflip.is_finite()
            && t_stat_vs_coinflip >= 2.0,
    }
}

fn settlement_t_stat_vs_coinflip(wins: usize, total: usize) -> f64 {
    if total == 0 {
        return f64::NAN;
    }
    let p_hat = wins as f64 / total as f64;
    (p_hat - 0.5) / (0.25 / total as f64).sqrt()
}

fn build_executable_ev_buckets(
    rows: &[FactorObservationV2],
    options: &FactorReviewOptions,
) -> Vec<ExecutableEvBucketSummary> {
    let min_sample = options.min_observations.max(30);
    let mut summaries = Vec::new();
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "direction_probability",
        |row| probability_bucket(row.side_model_prob),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "model_edge_after_fee",
        |row| model_edge_bucket(row.side_model_edge),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "entry_price",
        |row| price_bucket(row.entry_ask),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "time_regime",
        |row| Some(row.regime.as_str().to_string()),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "symbol",
        |row| (!row.symbol.is_empty()).then(|| row.symbol.clone()),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "side",
        |row| Some(row.side.as_str().to_string()),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "pm_lag_secs",
        |row| pm_lag_bucket(row.pm_lag_secs),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "entry_capacity_ratio",
        |row| capacity_bucket(row.entry_capacity_ratio),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "entry_liquidity_usd",
        |row| liquidity_bucket(row.entry_liquidity_usd),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "liquidity_shortfall_usd",
        |row| shortfall_bucket(row.liquidity_shortfall_usd),
    );
    append_executable_ev_dimension(
        &mut summaries,
        rows,
        options.stake_usd,
        min_sample,
        "entry_sweep_slippage_bps",
        |row| slippage_bucket(row.entry_sweep_slippage_bps),
    );
    summaries.sort_by(|a, b| {
        a.dimension
            .cmp(&b.dimension)
            .then_with(|| a.bucket.cmp(&b.bucket))
    });
    summaries
}

fn append_executable_ev_dimension<F>(
    summaries: &mut Vec<ExecutableEvBucketSummary>,
    rows: &[FactorObservationV2],
    stake_usd: f64,
    min_sample: usize,
    dimension: &str,
    bucket_fn: F,
) where
    F: Fn(&FactorObservationV2) -> Option<String>,
{
    let mut groups: BTreeMap<String, Vec<&FactorObservationV2>> = BTreeMap::new();
    for row in rows {
        if let Some(bucket) = bucket_fn(row) {
            groups.entry(bucket).or_default().push(row);
        }
    }
    for (bucket, bucket_rows) in groups {
        summaries.push(summarize_executable_ev_bucket(
            dimension,
            &bucket,
            &bucket_rows,
            stake_usd,
            min_sample,
        ));
    }
}

fn summarize_executable_ev_bucket(
    dimension: &str,
    bucket: &str,
    rows: &[&FactorObservationV2],
    stake_usd: f64,
    min_sample: usize,
) -> ExecutableEvBucketSummary {
    let pnl_values: Vec<f64> = rows.iter().filter_map(|row| executable_pnl(row)).collect();
    let total_pnl_15u = pnl_values.iter().sum::<f64>();
    let avg_pnl_15u = if pnl_values.is_empty() {
        f64::NAN
    } else {
        total_pnl_15u / pnl_values.len() as f64
    };
    let roi_on_stake = if stake_usd > 0.0 && !pnl_values.is_empty() {
        total_pnl_15u / (stake_usd * pnl_values.len() as f64)
    } else {
        f64::NAN
    };
    let t_stat = trade_t_stat(&pnl_values);
    let underpowered = pnl_values.len() < min_sample;
    let positive_ev = avg_pnl_15u.is_finite() && avg_pnl_15u > 0.0;
    let statistically_supported =
        positive_ev && !underpowered && t_stat.is_finite() && t_stat >= 2.0;

    ExecutableEvBucketSummary {
        dimension: dimension.to_string(),
        bucket: bucket.to_string(),
        rows: rows.len(),
        fillable_rows: rows.iter().filter(|row| entry_fillable(row)).count(),
        fill_rate: ratio(
            rows.iter().filter(|row| entry_fillable(row)).count(),
            rows.len(),
        ),
        pnl_rows: pnl_values.len(),
        total_pnl_15u,
        avg_pnl_15u,
        roi_on_stake,
        t_stat,
        underpowered,
        positive_ev,
        statistically_supported,
        avg_side_model_prob: mean(rows.iter().map(|row| row.side_model_prob)),
        avg_side_model_edge: mean(rows.iter().map(|row| row.side_model_edge)),
        avg_entry_ask: mean(rows.iter().map(|row| row.entry_ask)),
        avg_exit_bid: mean(rows.iter().map(|row| row.exit_bid)),
        avg_pm_lag_secs: mean(rows.iter().map(|row| row.pm_lag_secs)),
        avg_entry_capacity_ratio: mean(rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_entry_liquidity_usd: mean(rows.iter().map(|row| row.entry_liquidity_usd)),
        avg_exit_liquidity_usd: mean(rows.iter().map(|row| row.exit_liquidity_usd)),
        avg_liquidity_shortfall_usd: mean(rows.iter().map(|row| row.liquidity_shortfall_usd)),
        avg_slippage_to_fill_bps: mean(rows.iter().map(|row| row.slippage_to_fill_15u_bps)),
        avg_entry_sweep_slippage_bps: mean(rows.iter().map(|row| row.entry_sweep_slippage_bps)),
        avg_exit_sweep_slippage_bps: mean(rows.iter().map(|row| row.exit_sweep_slippage_bps)),
        avg_roundtrip_cost_usd: mean(rows.iter().map(|row| row.roundtrip_cost_usd)),
    }
}

#[derive(Default)]
struct DirectionPair<'a> {
    up: Option<&'a FactorObservationV2>,
    down: Option<&'a FactorObservationV2>,
}

struct DirectionSideSelection<'a> {
    favored: &'a FactorObservationV2,
    opposite: &'a FactorObservationV2,
    margin: f64,
}

fn build_direction_side_audit(
    rows: &[FactorObservationV2],
    options: &FactorReviewOptions,
) -> Vec<DirectionSideAuditSummary> {
    let min_sample = options.min_observations.max(30);
    let pairs = paired_direction_rows(rows);
    let mut summaries = Vec::new();
    append_direction_side_selector(
        &mut summaries,
        "model_probability",
        &pairs,
        options.stake_usd,
        min_sample,
        select_probability_favored_side,
        |selection| probability_bucket(selection.favored.side_model_prob),
    );
    append_direction_side_selector(
        &mut summaries,
        "model_edge",
        &pairs,
        options.stake_usd,
        min_sample,
        select_edge_favored_side,
        |selection| model_edge_bucket(selection.favored.side_model_edge),
    );
    summaries.sort_by(|a, b| {
        a.selector
            .cmp(&b.selector)
            .then_with(|| audit_bucket_sort_key(&a.bucket).cmp(&audit_bucket_sort_key(&b.bucket)))
            .then_with(|| a.bucket.cmp(&b.bucket))
    });
    summaries
}

fn paired_direction_rows(rows: &[FactorObservationV2]) -> Vec<DirectionPair<'_>> {
    let mut groups: BTreeMap<(String, String, DateTime<Utc>), DirectionPair<'_>> = BTreeMap::new();
    for row in rows {
        let pair = groups
            .entry((row.event_id.clone(), row.symbol.clone(), row.tick_ts))
            .or_default();
        match row.side {
            ReviewSide::Up => pair.up = Some(row),
            ReviewSide::Down => pair.down = Some(row),
        }
    }
    groups
        .into_values()
        .filter(|pair| pair.up.is_some() && pair.down.is_some())
        .collect()
}

fn append_direction_side_selector<F, B>(
    summaries: &mut Vec<DirectionSideAuditSummary>,
    selector: &str,
    pairs: &[DirectionPair<'_>],
    stake_usd: f64,
    min_sample: usize,
    select_fn: F,
    bucket_fn: B,
) where
    F: for<'a> Fn(&'a DirectionPair<'a>) -> Option<DirectionSideSelection<'a>>,
    B: for<'a> Fn(&DirectionSideSelection<'a>) -> Option<String>,
{
    let selections = pairs
        .iter()
        .filter_map(select_fn)
        .collect::<Vec<DirectionSideSelection<'_>>>();
    if selections.is_empty() {
        return;
    }
    summaries.push(summarize_direction_side_audit(
        selector,
        "all",
        &selections,
        stake_usd,
        min_sample,
    ));

    let mut groups: BTreeMap<String, Vec<DirectionSideSelection<'_>>> = BTreeMap::new();
    for selection in selections {
        if let Some(bucket) = bucket_fn(&selection) {
            groups.entry(bucket).or_default().push(selection);
        }
    }
    for (bucket, bucket_selections) in groups {
        summaries.push(summarize_direction_side_audit(
            selector,
            &bucket,
            &bucket_selections,
            stake_usd,
            min_sample,
        ));
    }
}

fn select_probability_favored_side<'a>(
    pair: &'a DirectionPair<'a>,
) -> Option<DirectionSideSelection<'a>> {
    let up = pair.up?;
    let down = pair.down?;
    select_higher_metric_side(up, down, up.side_model_prob, down.side_model_prob)
}

fn select_edge_favored_side<'a>(pair: &'a DirectionPair<'a>) -> Option<DirectionSideSelection<'a>> {
    let up = pair.up?;
    let down = pair.down?;
    select_higher_metric_side(up, down, up.side_model_edge, down.side_model_edge)
}

fn select_higher_metric_side<'a>(
    up: &'a FactorObservationV2,
    down: &'a FactorObservationV2,
    up_value: f64,
    down_value: f64,
) -> Option<DirectionSideSelection<'a>> {
    if !up_value.is_finite() || !down_value.is_finite() {
        return None;
    }
    if (up_value - down_value).abs() <= EPS {
        return None;
    }
    if up_value > down_value {
        Some(DirectionSideSelection {
            favored: up,
            opposite: down,
            margin: up_value - down_value,
        })
    } else {
        Some(DirectionSideSelection {
            favored: down,
            opposite: up,
            margin: down_value - up_value,
        })
    }
}

fn summarize_direction_side_audit(
    selector: &str,
    bucket: &str,
    selections: &[DirectionSideSelection<'_>],
    stake_usd: f64,
    min_sample: usize,
) -> DirectionSideAuditSummary {
    let favored_rows = selections
        .iter()
        .map(|selection| selection.favored)
        .collect::<Vec<_>>();
    let opposite_rows = selections
        .iter()
        .map(|selection| selection.opposite)
        .collect::<Vec<_>>();
    let pnl_deltas = selections
        .iter()
        .filter_map(|selection| {
            match (
                executable_pnl(selection.favored),
                executable_pnl(selection.opposite),
            ) {
                (Some(favored), Some(opposite)) => Some(favored - opposite),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let total_pnl_delta_15u = pnl_deltas.iter().sum::<f64>();
    let avg_pnl_delta_15u = if pnl_deltas.is_empty() {
        f64::NAN
    } else {
        total_pnl_delta_15u / pnl_deltas.len() as f64
    };
    DirectionSideAuditSummary {
        selector: selector.to_string(),
        bucket: bucket.to_string(),
        pairs: selections.len(),
        pnl_pair_rows: pnl_deltas.len(),
        total_pnl_delta_15u,
        avg_pnl_delta_15u,
        avg_selector_margin: mean(selections.iter().map(|selection| selection.margin)),
        favored: summarize_direction_side_leg(&favored_rows, stake_usd, min_sample),
        opposite: summarize_direction_side_leg(&opposite_rows, stake_usd, min_sample),
    }
}

fn summarize_direction_side_leg(
    rows: &[&FactorObservationV2],
    stake_usd: f64,
    min_sample: usize,
) -> DirectionSideAuditLegSummary {
    let pnl_values = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl_15u = pnl_values.iter().sum::<f64>();
    let avg_pnl_15u = if pnl_values.is_empty() {
        f64::NAN
    } else {
        total_pnl_15u / pnl_values.len() as f64
    };
    let roi_on_stake = if stake_usd > 0.0 && !pnl_values.is_empty() {
        total_pnl_15u / (stake_usd * pnl_values.len() as f64)
    } else {
        f64::NAN
    };
    let t_stat = trade_t_stat(&pnl_values);
    let underpowered = pnl_values.len() < min_sample;
    let positive_ev = avg_pnl_15u.is_finite() && avg_pnl_15u > 0.0;
    let statistically_supported =
        positive_ev && !underpowered && t_stat.is_finite() && t_stat >= 2.0;
    let settlement_win_rows = rows
        .iter()
        .filter(|row| row.label_settlement_win.is_some_and(|label| label >= 0.5))
        .count();
    DirectionSideAuditLegSummary {
        rows: rows.len(),
        fillable_rows: rows.iter().filter(|row| entry_fillable(row)).count(),
        fill_rate: ratio(
            rows.iter().filter(|row| entry_fillable(row)).count(),
            rows.len(),
        ),
        settlement_win_rows,
        settlement_win_rate: ratio(settlement_win_rows, rows.len()),
        pnl_rows: pnl_values.len(),
        total_pnl_15u,
        avg_pnl_15u,
        roi_on_stake,
        t_stat,
        underpowered,
        positive_ev,
        statistically_supported,
        avg_side_model_prob: mean(rows.iter().map(|row| row.side_model_prob)),
        avg_side_model_edge: mean(rows.iter().map(|row| row.side_model_edge)),
        avg_entry_ask: mean(rows.iter().map(|row| row.entry_ask)),
        avg_entry_capacity_ratio: mean(rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_entry_liquidity_usd: mean(rows.iter().map(|row| row.entry_liquidity_usd)),
        avg_entry_sweep_slippage_bps: mean(rows.iter().map(|row| row.entry_sweep_slippage_bps)),
    }
}

fn audit_bucket_sort_key(bucket: &str) -> usize {
    if bucket == "all" {
        0
    } else {
        1
    }
}

fn probability_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (0.50, "<0.50"),
            (0.55, "0.50..0.55"),
            (0.60, "0.55..0.60"),
            (0.65, "0.60..0.65"),
            (0.70, "0.65..0.70"),
        ],
        ">=0.70",
    )
}

fn model_edge_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (-0.05, "<-0.05"),
            (0.00, "-0.05..0.00"),
            (0.02, "0.00..0.02"),
            (0.05, "0.02..0.05"),
            (0.10, "0.05..0.10"),
        ],
        ">=0.10",
    )
}

fn price_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (0.35, "<0.35"),
            (0.45, "0.35..0.45"),
            (0.55, "0.45..0.55"),
            (0.65, "0.55..0.65"),
        ],
        ">=0.65",
    )
}

fn pm_lag_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (2.0, "<2s"),
            (5.0, "2..5s"),
            (15.0, "5..15s"),
            (30.0, "15..30s"),
        ],
        ">=30s",
    )
}

fn capacity_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (0.50, "<0.50x"),
            (1.00, "0.50..1.00x"),
            (2.00, "1.00..2.00x"),
            (5.00, "2.00..5.00x"),
        ],
        ">=5.00x",
    )
}

fn liquidity_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (15.0, "<15u"),
            (50.0, "15..50u"),
            (150.0, "50..150u"),
            (500.0, "150..500u"),
        ],
        ">=500u",
    )
}

fn shortfall_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (EPS, "0u"),
            (5.0, "0..5u"),
            (15.0, "5..15u"),
            (50.0, "15..50u"),
        ],
        ">=50u",
    )
}

fn slippage_bucket(value: f64) -> Option<String> {
    finite_bucket(
        value,
        &[
            (EPS, "0bps"),
            (25.0, "0..25bps"),
            (100.0, "25..100bps"),
            (500.0, "100..500bps"),
        ],
        ">=500bps",
    )
}

fn finite_bucket(value: f64, thresholds: &[(f64, &str)], upper_label: &str) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    for (threshold, label) in thresholds {
        if value < *threshold {
            return Some((*label).to_string());
        }
    }
    Some(upper_label.to_string())
}

fn build_trade_formation_path_rows(
    rows: &[&FactorObservationV2],
    denominator: usize,
    min_observations: usize,
) -> Vec<TradeFormationPathRow> {
    let mut buckets: BTreeMap<String, Vec<&FactorObservationV2>> = BTreeMap::new();
    for row in rows {
        buckets
            .entry(trade_formation_path(row))
            .or_default()
            .push(row);
    }
    let mut out = buckets
        .into_iter()
        .filter_map(|(path, bucket_rows)| {
            (bucket_rows.len() >= min_observations)
                .then(|| build_trade_formation_path_row(path, &bucket_rows, denominator))
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.total_pnl_after_cost
            .total_cmp(&a.total_pnl_after_cost)
            .then_with(|| b.n.cmp(&a.n))
    });
    out
}

fn build_trade_formation_path_row(
    path: String,
    rows: &[&FactorObservationV2],
    denominator: usize,
) -> TradeFormationPathRow {
    let pnl_values = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    let wins = pnl_values.iter().filter(|pnl| **pnl > 0.0).count();
    let settlement_wins = rows
        .iter()
        .filter(|row| row.label_settlement_win.is_some_and(|label| label >= 0.5))
        .count();
    TradeFormationPathRow {
        path,
        n: rows.len(),
        coverage: ratio(rows.len(), denominator),
        executable_rows: pnl_values.len(),
        win_rate: ratio(wins, pnl_values.len()),
        settlement_win_rate: ratio(settlement_wins, rows.len()),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        avg_side_model_prob: mean(rows.iter().map(|row| row.side_model_prob)),
        avg_side_distance_over_sigma: mean(rows.iter().map(|row| row.side_distance_over_sigma)),
        avg_obi_10_side: mean(rows.iter().map(|row| row.obi_10 * row.side.multiplier())),
        avg_obi_persistence_30s_side: mean(rows.iter().map(|row| row.obi_persistence_30s_side)),
        avg_cex_continuation_score_side: mean(
            rows.iter().map(|row| row.cex_continuation_score_side),
        ),
        avg_entry_capacity_ratio: mean(rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(rows.iter().map(|row| row.exit_capacity_ratio)),
        avg_pm_spread_bps: mean(rows.iter().map(|row| row.pm_spread_bps)),
        avg_future_exit_pnl_30s: mean(rows.iter().filter_map(|row| row.label_future_exit_pnl_30s)),
        avg_future_exit_bid_change_30s: mean(
            rows.iter()
                .filter_map(|row| row.label_future_exit_bid_change_30s),
        ),
        avg_future_exit_bid_change_60s: mean(
            rows.iter()
                .filter_map(|row| row.label_future_exit_bid_change_60s),
        ),
    }
}

fn build_trade_formation_rule_rows(
    rows: &[&FactorObservationV2],
    min_observations: usize,
) -> Vec<TradeFormationRuleRow> {
    let mut out = meta_label_rule_specs()
        .into_iter()
        .filter_map(|spec| {
            let selected = rows
                .iter()
                .copied()
                .filter(|row| (spec.predicate)(row))
                .collect::<Vec<_>>();
            (selected.len() >= min_observations).then(|| {
                build_trade_formation_rule_row(spec.name.to_string(), &selected, rows.len())
            })
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.total_pnl_after_cost
            .total_cmp(&a.total_pnl_after_cost)
            .then_with(|| b.win_rate.total_cmp(&a.win_rate))
    });
    out
}

#[derive(Clone, Copy)]
struct MetaLabelRuleSpec {
    name: &'static str,
    predicate: fn(&FactorObservationV2) -> bool,
}

fn meta_label_rule_specs() -> Vec<MetaLabelRuleSpec> {
    vec![
        MetaLabelRuleSpec {
            name: "liquidity_gate_only",
            predicate: |_| true,
        },
        MetaLabelRuleSpec {
            name: "strong_direction",
            predicate: strong_direction_rule,
        },
        MetaLabelRuleSpec {
            name: "cex_obi_confirmation",
            predicate: cex_obi_confirmation_rule,
        },
        MetaLabelRuleSpec {
            name: "continuation_confirmation",
            predicate: continuation_confirmation_rule,
        },
        MetaLabelRuleSpec {
            name: "deribit_iv_confirmation",
            predicate: deribit_iv_confirmation_rule,
        },
        MetaLabelRuleSpec {
            name: "strong_direction_and_cex_obi",
            predicate: |row| strong_direction_rule(row) && cex_obi_confirmation_rule(row),
        },
        MetaLabelRuleSpec {
            name: "strong_direction_and_continuation",
            predicate: |row| strong_direction_rule(row) && continuation_confirmation_rule(row),
        },
        MetaLabelRuleSpec {
            name: "cex_obi_and_continuation",
            predicate: |row| cex_obi_confirmation_rule(row) && continuation_confirmation_rule(row),
        },
        MetaLabelRuleSpec {
            name: "strong_direction_cex_and_continuation",
            predicate: |row| {
                strong_direction_rule(row)
                    && cex_obi_confirmation_rule(row)
                    && continuation_confirmation_rule(row)
            },
        },
    ]
}

fn build_trade_formation_rule_row(
    rule: String,
    rows: &[&FactorObservationV2],
    denominator: usize,
) -> TradeFormationRuleRow {
    let pnl_values = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    let wins = pnl_values.iter().filter(|pnl| **pnl > 0.0).count();
    TradeFormationRuleRow {
        rule,
        n: rows.len(),
        coverage: ratio(rows.len(), denominator),
        win_rate: ratio(wins, pnl_values.len()),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        avg_future_exit_pnl_30s: mean(rows.iter().filter_map(|row| row.label_future_exit_pnl_30s)),
    }
}

fn trade_formation_path(row: &FactorObservationV2) -> String {
    format!(
        "direction={}|cex={}|continuation={}|pm={}|deribit={}|time={}",
        direction_bin(row),
        cex_obi_bin(row),
        continuation_bin(row),
        pm_execution_bin(row),
        deribit_bin(row),
        time_remaining_bin(row),
    )
}

fn executable_pnl(row: &FactorObservationV2) -> Option<f64> {
    row.label_full_depth_executable_pnl_15u
        .or(row.label_executable_pnl_15u)
        .filter(|pnl| pnl.is_finite())
}

fn entry_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_entry_fillable || row.label_executable_fillable
}

fn exit_fillable(row: &FactorObservationV2) -> bool {
    row.label_full_depth_exit_fillable || row.label_exit_fillable
}

fn roundtrip_fillable(row: &FactorObservationV2) -> bool {
    (row.label_full_depth_entry_fillable && row.label_full_depth_exit_fillable)
        || (row.label_executable_fillable && row.label_exit_fillable)
}

fn direction_bin(row: &FactorObservationV2) -> &'static str {
    let distance = row.side_distance_over_sigma;
    if row.side_model_prob >= 0.70 || distance >= 1.0 {
        "strong_model"
    } else if row.side_model_prob >= 0.60 || distance >= 0.50 {
        "medium_model"
    } else {
        "weak_model"
    }
}

fn cex_obi_bin(row: &FactorObservationV2) -> &'static str {
    let obi_side = row.obi_10 * row.side.multiplier();
    if row.obi_persistence_30s_side >= 0.75 && obi_side > 0.0 {
        "persistent_obi"
    } else if row.depth_imbalance * row.side.multiplier() >= 0.25 {
        "depth_confirmed"
    } else if row.obi_flip_count_60s >= 2.0 {
        "obi_unstable"
    } else {
        "obi_neutral"
    }
}

fn continuation_bin(row: &FactorObservationV2) -> &'static str {
    if row.cex_continuation_score_side >= 0.75
        || (row.cex_bar_return_30s * row.side.multiplier() > 0.0
            && row.cex_signed_volume_ratio_30s * row.side.multiplier() > 0.0)
    {
        "continuation_confirmed"
    } else if row.cex_continuation_score_side <= -0.75 {
        "continuation_against"
    } else {
        "continuation_neutral"
    }
}

fn pm_execution_bin(row: &FactorObservationV2) -> &'static str {
    let min_cap = row.entry_capacity_ratio.min(row.exit_capacity_ratio);
    if min_cap >= 5.0 && row.pm_spread_bps <= 300.0 {
        "deep_tight"
    } else if min_cap >= 1.0 && row.pm_spread_bps <= 500.0 {
        "fillable_tight"
    } else if min_cap >= 1.0 {
        "fillable_wide"
    } else {
        "thin"
    }
}

fn deribit_bin(row: &FactorObservationV2) -> &'static str {
    if !row.deribit_iv_change_60s.is_finite() {
        "deribit_missing"
    } else if row.deribit_iv_change_60s >= 0.02 {
        "iv_up"
    } else if row.deribit_iv_change_60s <= -0.02 {
        "iv_down"
    } else {
        "iv_flat"
    }
}

fn time_remaining_bin(row: &FactorObservationV2) -> &'static str {
    if row.time_remaining_secs >= 240 {
        "early"
    } else if row.time_remaining_secs >= 120 {
        "middle"
    } else {
        "late"
    }
}

fn strong_direction_rule(row: &FactorObservationV2) -> bool {
    matches!(direction_bin(row), "strong_model")
}

fn cex_obi_confirmation_rule(row: &FactorObservationV2) -> bool {
    matches!(cex_obi_bin(row), "persistent_obi" | "depth_confirmed")
}

fn continuation_confirmation_rule(row: &FactorObservationV2) -> bool {
    matches!(continuation_bin(row), "continuation_confirmed")
}

fn deribit_iv_confirmation_rule(row: &FactorObservationV2) -> bool {
    matches!(deribit_bin(row), "iv_up" | "iv_down")
}

struct FillabilityBucketSpec {
    dimension: &'static str,
    bucket: fn(&FactorObservationV2) -> Option<String>,
}

fn fillability_bucket_specs() -> Vec<FillabilityBucketSpec> {
    vec![
        FillabilityBucketSpec {
            dimension: "symbol",
            bucket: |row| Some(row.symbol.clone()),
        },
        FillabilityBucketSpec {
            dimension: "side",
            bucket: |row| Some(row.side.as_str().to_string()),
        },
        FillabilityBucketSpec {
            dimension: "regime",
            bucket: |row| Some(row.regime.as_str().to_string()),
        },
        FillabilityBucketSpec {
            dimension: "time_remaining_secs",
            bucket: |row| {
                bucket_value(row.time_remaining_secs as f64, &[60.0, 120.0, 180.0, 240.0])
            },
        },
        FillabilityBucketSpec {
            dimension: "entry_ask",
            bucket: |row| bucket_value(row.entry_ask, &[0.05, 0.10, 0.15, 0.25, 0.40, 0.60, 0.80]),
        },
        FillabilityBucketSpec {
            dimension: "pm_spread_bps",
            bucket: |row| {
                bucket_value(
                    row.pm_spread_bps,
                    &[250.0, 500.0, 1_000.0, 2_000.0, 4_000.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "pm_lag_secs",
            bucket: |row| bucket_value(row.pm_lag_secs, &[1.0, 3.0, 10.0, 30.0, 60.0]),
        },
        FillabilityBucketSpec {
            dimension: "entry_capacity_ratio",
            bucket: |row| bucket_value(row.entry_capacity_ratio, &[0.25, 0.50, 1.0, 2.0, 5.0]),
        },
        FillabilityBucketSpec {
            dimension: "exit_capacity_ratio",
            bucket: |row| bucket_value(row.exit_capacity_ratio, &[0.25, 0.50, 1.0, 2.0, 5.0]),
        },
        FillabilityBucketSpec {
            dimension: "min_entry_exit_capacity_ratio",
            bucket: |row| {
                bucket_value(
                    row.entry_capacity_ratio.min(row.exit_capacity_ratio),
                    &[0.25, 0.50, 1.0, 2.0, 5.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "entry_liquidity_usd",
            bucket: |row| {
                bucket_value(
                    row.entry_liquidity_usd,
                    &[5.0, 10.0, 15.0, 30.0, 75.0, 150.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "exit_liquidity_usd",
            bucket: |row| {
                bucket_value(
                    row.exit_liquidity_usd,
                    &[5.0, 10.0, 15.0, 30.0, 75.0, 150.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "cex_spread_bps",
            bucket: |row| bucket_value(row.cex_spread_bps, &[1.0, 3.0, 5.0, 10.0, 20.0]),
        },
        FillabilityBucketSpec {
            dimension: "obi_10_side",
            bucket: |row| {
                bucket_value(
                    row.obi_10 * row.side.multiplier(),
                    &[-0.5, -0.1, 0.0, 0.1, 0.5],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "depth_imbalance_side",
            bucket: |row| {
                bucket_value(
                    row.depth_imbalance * row.side.multiplier(),
                    &[-0.5, -0.1, 0.0, 0.1, 0.5],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "cex_continuation_score_side",
            bucket: |row| {
                bucket_value(
                    row.cex_continuation_score_side,
                    &[-1.0, -0.25, 0.0, 0.25, 1.0, 2.0],
                )
            },
        },
        FillabilityBucketSpec {
            dimension: "cex_bar_volume_ratio_30s",
            bucket: |row| bucket_value(row.cex_bar_volume_ratio_30s, &[0.5, 1.0, 1.5, 2.5, 5.0]),
        },
        FillabilityBucketSpec {
            dimension: "deribit_mark_iv",
            bucket: |row| bucket_value(row.deribit_mark_iv, &[0.30, 0.50, 0.75, 1.00, 1.50]),
        },
        FillabilityBucketSpec {
            dimension: "deribit_iv_change_30s",
            bucket: |row| bucket_value(row.deribit_iv_change_30s, &[-0.05, -0.01, 0.0, 0.01, 0.05]),
        },
    ]
}

fn build_fillability_bucket_row(
    dimension: &str,
    bucket: String,
    rows: &[&FactorObservationV2],
    total_rows: usize,
    options: &FillabilityReviewOptions,
) -> FillabilityBucketRow {
    let entry_filled = rows.iter().filter(|row| entry_fillable(row)).count();
    let exit_filled = rows.iter().filter(|row| exit_fillable(row)).count();
    let roundtrip_filled = rows.iter().filter(|row| roundtrip_fillable(row)).count();
    let executable_pnls = rows
        .iter()
        .filter_map(|row| executable_pnl(row))
        .collect::<Vec<_>>();
    let total_executable_pnl_after_cost = executable_pnls.iter().sum::<f64>();
    let entry_fill_rate = ratio(entry_filled, rows.len());
    let roundtrip_fill_rate = ratio(roundtrip_filled, rows.len());
    let rejection_rate = 1.0 - entry_fill_rate;
    let (decision, reason) = fillability_decision(
        rows.len(),
        entry_fill_rate,
        roundtrip_fill_rate,
        rejection_rate,
        options,
    );
    FillabilityBucketRow {
        dimension: dimension.to_string(),
        bucket,
        n: rows.len(),
        coverage: ratio(rows.len(), total_rows),
        entry_fill_rate,
        exit_fill_rate: ratio(exit_filled, rows.len()),
        roundtrip_fill_rate,
        rejection_rate,
        avg_entry_capacity_ratio: mean(rows.iter().map(|row| row.entry_capacity_ratio)),
        avg_exit_capacity_ratio: mean(rows.iter().map(|row| row.exit_capacity_ratio)),
        avg_entry_liquidity_usd: mean(rows.iter().map(|row| row.entry_liquidity_usd)),
        avg_exit_liquidity_usd: mean(rows.iter().map(|row| row.exit_liquidity_usd)),
        avg_pm_spread_bps: mean(rows.iter().map(|row| row.pm_spread_bps)),
        avg_pm_lag_secs: mean(rows.iter().map(|row| row.pm_lag_secs)),
        avg_slippage_to_fill_bps: mean(rows.iter().map(|row| row.slippage_to_fill_15u_bps)),
        avg_roundtrip_cost_usd: mean(rows.iter().map(|row| row.roundtrip_cost_usd)),
        total_executable_pnl_after_cost,
        avg_executable_pnl_after_cost: if executable_pnls.is_empty() {
            f64::NAN
        } else {
            total_executable_pnl_after_cost / executable_pnls.len() as f64
        },
        decision,
        reason,
    }
}

fn fillability_decision(
    n: usize,
    entry_fill_rate: f64,
    roundtrip_fill_rate: f64,
    rejection_rate: f64,
    options: &FillabilityReviewOptions,
) -> (FillabilityDecision, String) {
    if n < options.min_bucket_observations {
        return (
            FillabilityDecision::Reject,
            "too_few_observations".to_string(),
        );
    }
    if entry_fill_rate < options.min_entry_fill_rate {
        return (
            FillabilityDecision::Reject,
            "low_entry_fill_rate".to_string(),
        );
    }
    if rejection_rate > options.max_rejection_rate {
        return (
            FillabilityDecision::Reject,
            "high_rejection_rate".to_string(),
        );
    }
    if roundtrip_fill_rate < options.min_roundtrip_fill_rate {
        return (
            FillabilityDecision::Watchlist,
            "entry_fill_ok_but_roundtrip_weak".to_string(),
        );
    }
    (FillabilityDecision::Candidate, "passed".to_string())
}

fn fillability_decision_rank(decision: FillabilityDecision) -> usize {
    match decision {
        FillabilityDecision::Candidate => 3,
        FillabilityDecision::Watchlist => 2,
        FillabilityDecision::Reject => 1,
    }
}

fn liquidity_gate_v1_accepts(row: &FactorObservationV2, options: &LiquidityGateV1Options) -> bool {
    valid_price(row.entry_ask)
        && row.entry_ask >= options.min_entry_ask
        && row.entry_ask <= options.max_entry_ask
        && row.time_remaining_secs >= options.min_time_remaining_secs
        && row.pm_lag_secs.is_finite()
        && row.pm_lag_secs <= options.max_pm_lag_secs
        && row.pm_spread_bps.is_finite()
        && row.pm_spread_bps <= options.max_pm_spread_bps
        && row.entry_capacity_ratio.is_finite()
        && row.entry_capacity_ratio >= options.min_entry_capacity_ratio
        && row.exit_capacity_ratio.is_finite()
        && row.exit_capacity_ratio >= options.min_exit_capacity_ratio
}

fn selection_metrics_for_rows(
    total_rows: usize,
    selected: &[&FactorObservationV2],
) -> FactorSelectionMetrics {
    let filled = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect::<Vec<_>>();
    let pnls = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect::<Vec<_>>();
    let pnl_values = pnls.iter().map(|(_, pnl)| *pnl).collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: total_rows,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn bucket_value(value: f64, edges: &[f64]) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let first = *edges.first()?;
    if value < first {
        return Some(format!("lt_{}", bucket_num(first)));
    }
    for pair in edges.windows(2) {
        let lo = pair[0];
        let hi = pair[1];
        if value >= lo && value < hi {
            return Some(format!("{}_{}", bucket_num(lo), bucket_num(hi)));
        }
    }
    edges
        .last()
        .map(|last| format!("gte_{}", bucket_num(*last)))
}

fn bucket_num(value: f64) -> String {
    let raw = if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    };
    raw.replace('-', "m").replace('.', "p")
}

fn stability_decision(
    windows: usize,
    positive_window_ratio: f64,
    total_test_pnl_after_cost: f64,
    avg_test_fill_rate: f64,
    avg_test_rejection_rate: f64,
    avg_by_symbol_positive_ratio: f64,
    avg_by_time_bucket_positive_ratio: f64,
    executable_pnl_rank_icir: f64,
    options: &FactorStabilityOptions,
) -> (FactorStabilityDecision, String) {
    if windows < options.min_windows {
        return if total_test_pnl_after_cost > options.min_total_test_pnl_after_cost {
            (
                FactorStabilityDecision::Watchlist,
                "too_few_windows_positive_pnl".to_string(),
            )
        } else {
            (
                FactorStabilityDecision::Reject,
                "too_few_windows_nonpositive_pnl".to_string(),
            )
        };
    }
    if total_test_pnl_after_cost <= options.min_total_test_pnl_after_cost {
        return (
            FactorStabilityDecision::Reject,
            "nonpositive_executable_pnl".to_string(),
        );
    }
    if positive_window_ratio < options.min_positive_window_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_unstable_windows".to_string(),
        );
    }
    if avg_test_fill_rate < options.min_avg_fill_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_fill_rate".to_string(),
        );
    }
    if avg_test_rejection_rate > options.max_avg_rejection_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_high_rejection".to_string(),
        );
    }
    if avg_by_symbol_positive_ratio < options.min_by_symbol_positive_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_symbol_unstable".to_string(),
        );
    }
    if avg_by_time_bucket_positive_ratio < options.min_by_time_bucket_positive_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_regime_unstable".to_string(),
        );
    }
    if !executable_pnl_rank_icir.is_finite()
        || executable_pnl_rank_icir.abs() < options.min_abs_executable_pnl_icir
    {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_executable_icir".to_string(),
        );
    }
    (FactorStabilityDecision::Candidate, "passed".to_string())
}

fn decision_rank(decision: FactorStabilityDecision) -> usize {
    match decision {
        FactorStabilityDecision::Candidate => 3,
        FactorStabilityDecision::Watchlist => 2,
        FactorStabilityDecision::Reject => 1,
    }
}

fn fit_combo_v1_window(
    train_rows: &[&FactorObservationV2],
    test_rows: &[&FactorObservationV2],
    descriptors: &[FactorV2Descriptor],
    options: &FactorComboV1Options,
    window_index: usize,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    test_start: DateTime<Utc>,
    test_end: DateTime<Utc>,
) -> Option<FactorComboV1Window> {
    let mut by_family: BTreeMap<&'static str, Vec<FactorComboComponent>> = BTreeMap::new();
    for descriptor in descriptors {
        if let Some(component) = fit_combo_component(
            train_rows,
            *descriptor,
            options.walk_forward.review.min_observations,
            options.min_abs_train_executable_pnl_rank_ic,
        ) {
            by_family
                .entry(component.family.as_str())
                .or_default()
                .push(component);
        }
    }

    let mut components = Vec::new();
    for family_components in by_family.values_mut() {
        family_components.sort_by(|a, b| {
            b.train_executable_pnl_rank_ic
                .abs()
                .total_cmp(&a.train_executable_pnl_rank_ic.abs())
        });
        components.extend(
            family_components
                .iter()
                .take(options.max_factors_per_family.max(1))
                .cloned(),
        );
    }
    components.sort_by(|a, b| {
        b.train_executable_pnl_rank_ic
            .abs()
            .total_cmp(&a.train_executable_pnl_rank_ic.abs())
    });
    components.truncate(options.max_total_factors.max(1));
    if components.is_empty() {
        return None;
    }

    let mut train_scores = train_rows
        .iter()
        .filter_map(|row| combo_v1_score(row, &components))
        .filter(|score| score.is_finite())
        .collect::<Vec<_>>();
    if train_scores.len() < options.walk_forward.review.min_observations {
        return None;
    }
    train_scores.sort_by(|a, b| b.total_cmp(a));
    let selected_n = ((train_scores.len() as f64)
        * options.walk_forward.review.top_quantile.clamp(0.01, 1.0))
    .ceil()
    .max(1.0) as usize;
    let threshold = train_scores[selected_n.min(train_scores.len()) - 1];
    let train = evaluate_combo_v1_threshold(train_rows, &components, threshold);
    let test = evaluate_combo_v1_threshold(test_rows, &components, threshold);
    Some(FactorComboV1Window {
        window_index,
        train_start,
        train_end,
        test_start,
        test_end,
        threshold,
        components,
        train,
        test,
    })
}

fn fit_combo_component(
    rows: &[&FactorObservationV2],
    descriptor: FactorV2Descriptor,
    min_observations: usize,
    min_abs_train_ic: f64,
) -> Option<FactorComboComponent> {
    let scored: Vec<(&FactorObservationV2, f64)> = rows
        .iter()
        .filter_map(|row| {
            let value = (descriptor.accessor)(row);
            value.is_finite().then_some((*row, value))
        })
        .collect();
    if scored.len() < min_observations {
        return None;
    }
    let executable_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(row, score)| executable_pnl(row).map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    if executable_pairs.len() < min_observations {
        return None;
    }
    let train_executable_pnl_rank_ic = pair_spearman(&executable_pairs);
    if !train_executable_pnl_rank_ic.is_finite()
        || train_executable_pnl_rank_ic.abs() < min_abs_train_ic
    {
        return None;
    }
    let values = scored.iter().map(|(_, value)| *value).collect::<Vec<_>>();
    let train_mean = mean(values.iter().copied());
    let train_std = stddev(&values);
    if !train_std.is_finite() || train_std <= EPS {
        return None;
    }
    Some(FactorComboComponent {
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        accessor: descriptor.accessor,
        direction: train_executable_pnl_rank_ic.signum(),
        train_executable_pnl_rank_ic,
        train_mean,
        train_std,
    })
}

fn evaluate_combo_v1_threshold(
    rows: &[&FactorObservationV2],
    components: &[FactorComboComponent],
    threshold: f64,
) -> FactorSelectionMetrics {
    let scored_n = rows
        .iter()
        .filter(|row| combo_v1_score(row, components).is_some())
        .count();
    let selected: Vec<&FactorObservationV2> = rows
        .iter()
        .copied()
        .filter(|row| {
            combo_v1_score(row, components)
                .map(|score| score >= threshold)
                .unwrap_or(false)
        })
        .collect();
    let filled: Vec<&FactorObservationV2> = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: scored_n,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn combo_v1_score(row: &FactorObservationV2, components: &[FactorComboComponent]) -> Option<f64> {
    let mut by_family: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    for component in components {
        let value = (component.accessor)(row);
        if !value.is_finite() || !component.train_std.is_finite() || component.train_std <= EPS {
            continue;
        }
        let z = ((value - component.train_mean) / component.train_std).clamp(-5.0, 5.0);
        by_family
            .entry(component.family.as_str())
            .or_default()
            .push(z * component.direction);
    }
    if by_family.is_empty() {
        return None;
    }
    let family_scores = by_family
        .values()
        .map(|values| mean(values.iter().copied()))
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if family_scores.is_empty() {
        None
    } else {
        Some(mean(family_scores.iter().copied()))
    }
}

fn aggregate_combo_v1_windows(windows: &[FactorComboV1Window]) -> FactorComboV1Aggregate {
    if windows.is_empty() {
        return FactorComboV1Aggregate {
            windows: 0,
            positive_window_ratio: f64::NAN,
            total_test_pnl_after_cost: 0.0,
            avg_test_pnl_per_window: f64::NAN,
            min_test_pnl_after_cost: f64::NAN,
            avg_test_fill_rate: f64::NAN,
            avg_test_rejection_rate: f64::NAN,
            avg_component_count: f64::NAN,
        };
    }
    let total_test_pnl_after_cost = windows
        .iter()
        .map(|window| window.test.total_pnl_after_cost)
        .sum::<f64>();
    let min_test_pnl_after_cost = windows
        .iter()
        .map(|window| window.test.total_pnl_after_cost)
        .fold(f64::INFINITY, f64::min);
    let positive_windows = windows
        .iter()
        .filter(|window| window.test.total_pnl_after_cost > 0.0)
        .count();
    FactorComboV1Aggregate {
        windows: windows.len(),
        positive_window_ratio: ratio(positive_windows, windows.len()),
        total_test_pnl_after_cost,
        avg_test_pnl_per_window: total_test_pnl_after_cost / windows.len() as f64,
        min_test_pnl_after_cost,
        avg_test_fill_rate: mean(
            windows
                .iter()
                .map(|window| window.test.executable_fill_rate),
        ),
        avg_test_rejection_rate: mean(windows.iter().map(|window| window.test.rejection_rate)),
        avg_component_count: mean(windows.iter().map(|window| window.components.len() as f64)),
    }
}

fn evaluate_meta_label_rule(
    rows: &[&FactorObservationV2],
    predicate: fn(&FactorObservationV2) -> bool,
) -> FactorSelectionMetrics {
    let selected = rows
        .iter()
        .copied()
        .filter(|row| predicate(row))
        .collect::<Vec<_>>();
    let filled = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect::<Vec<_>>();
    let pnls = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect::<Vec<_>>();
    let pnl_values = pnls.iter().map(|(_, pnl)| *pnl).collect::<Vec<_>>();
    let total_pnl = pnl_values.iter().sum::<f64>();

    FactorSelectionMetrics {
        n: rows.len(),
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn aggregate_meta_label_windows(
    windows: &[MetaLabelWalkForwardWindow],
    options: &MetaLabelWalkForwardOptions,
) -> Vec<MetaLabelWalkForwardAggregate> {
    let mut grouped: BTreeMap<&str, Vec<&MetaLabelWalkForwardWindow>> = BTreeMap::new();
    for window in windows {
        grouped
            .entry(window.rule.as_str())
            .or_default()
            .push(window);
    }

    let mut out = grouped
        .into_iter()
        .map(|(rule, rule_windows)| {
            let total_test_pnl_after_cost = rule_windows
                .iter()
                .map(|window| window.test.total_pnl_after_cost)
                .sum::<f64>();
            let min_test_pnl_after_cost = rule_windows
                .iter()
                .map(|window| window.test.total_pnl_after_cost)
                .fold(f64::INFINITY, f64::min);
            let positive_windows = rule_windows
                .iter()
                .filter(|window| window.test.total_pnl_after_cost > 0.0)
                .count();
            let avg_test_selected = mean(
                rule_windows
                    .iter()
                    .map(|window| window.test.selected_n as f64),
            );
            let avg_test_fill_rate = mean(
                rule_windows
                    .iter()
                    .map(|window| window.test.executable_fill_rate),
            );
            let avg_test_rejection_rate =
                mean(rule_windows.iter().map(|window| window.test.rejection_rate));
            let (decision, reason) = meta_label_readiness_decision(
                rule_windows.len(),
                positive_windows,
                total_test_pnl_after_cost,
                min_test_pnl_after_cost,
                avg_test_selected,
                avg_test_fill_rate,
                avg_test_rejection_rate,
                options,
            );
            MetaLabelWalkForwardAggregate {
                rule: rule.to_string(),
                windows: rule_windows.len(),
                positive_window_ratio: ratio(positive_windows, rule_windows.len()),
                total_test_pnl_after_cost,
                avg_test_pnl_per_window: total_test_pnl_after_cost / rule_windows.len() as f64,
                min_test_pnl_after_cost,
                avg_train_selected: mean(
                    rule_windows
                        .iter()
                        .map(|window| window.train.selected_n as f64),
                ),
                avg_test_selected,
                avg_test_fill_rate,
                avg_test_rejection_rate,
                decision,
                reason,
            }
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        decision_rank(b.decision)
            .cmp(&decision_rank(a.decision))
            .then_with(|| {
                b.total_test_pnl_after_cost
                    .total_cmp(&a.total_test_pnl_after_cost)
            })
            .then_with(|| b.positive_window_ratio.total_cmp(&a.positive_window_ratio))
    });
    out
}

fn meta_label_readiness_decision(
    windows: usize,
    positive_windows: usize,
    total_test_pnl_after_cost: f64,
    min_test_pnl_after_cost: f64,
    avg_test_selected: f64,
    avg_test_fill_rate: f64,
    avg_test_rejection_rate: f64,
    options: &MetaLabelWalkForwardOptions,
) -> (FactorStabilityDecision, String) {
    if windows < options.min_candidate_windows {
        return if total_test_pnl_after_cost > options.min_candidate_total_test_pnl_after_cost {
            (
                FactorStabilityDecision::Watchlist,
                "too_few_oos_windows_positive_pnl".to_string(),
            )
        } else {
            (
                FactorStabilityDecision::Reject,
                "too_few_oos_windows_nonpositive_pnl".to_string(),
            )
        };
    }
    if total_test_pnl_after_cost <= options.min_candidate_total_test_pnl_after_cost {
        return (
            FactorStabilityDecision::Reject,
            "nonpositive_oos_executable_pnl".to_string(),
        );
    }
    if ratio(positive_windows, windows) < options.min_candidate_positive_window_ratio {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_unstable_oos_windows".to_string(),
        );
    }
    if avg_test_selected < options.min_candidate_avg_test_selected {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_oos_sample".to_string(),
        );
    }
    if avg_test_fill_rate < options.min_candidate_avg_fill_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_low_fill_rate".to_string(),
        );
    }
    if avg_test_rejection_rate > options.max_candidate_avg_rejection_rate {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_high_rejection".to_string(),
        );
    }
    if min_test_pnl_after_cost < options.min_candidate_worst_window_pnl_after_cost {
        return (
            FactorStabilityDecision::Watchlist,
            "positive_pnl_but_large_worst_window_loss".to_string(),
        );
    }
    (FactorStabilityDecision::Candidate, "passed".to_string())
}

fn fit_walk_forward_factor(
    train_rows: &[&FactorObservationV2],
    test_rows: &[&FactorObservationV2],
    descriptor: FactorV2Descriptor,
    options: &FactorReviewOptions,
    window_index: usize,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    test_start: DateTime<Utc>,
    test_end: DateTime<Utc>,
) -> Option<FactorWalkForwardWindow> {
    let scored: Vec<(&FactorObservationV2, f64)> = train_rows
        .iter()
        .filter_map(|row| {
            let value = (descriptor.accessor)(row);
            value.is_finite().then_some((*row, value))
        })
        .collect();
    if scored.len() < options.min_observations {
        return None;
    }

    let settlement_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(row, score)| row.label_settlement_win.map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    let executable_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(row, score)| executable_pnl(row).map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    let train_settlement_rank_ic = pair_spearman(&settlement_pairs);
    let train_executable_pnl_rank_ic = pair_spearman(&executable_pairs);
    let direction = if train_executable_pnl_rank_ic.is_finite() {
        train_executable_pnl_rank_ic.signum()
    } else if train_settlement_rank_ic.is_finite() {
        train_settlement_rank_ic.signum()
    } else {
        1.0
    };
    let direction = if direction.abs() <= EPS {
        1.0
    } else {
        direction
    };

    let mut directed_scores: Vec<f64> = scored
        .iter()
        .map(|(_, score)| *score * direction)
        .filter(|score| score.is_finite())
        .collect();
    if directed_scores.is_empty() {
        return None;
    }
    directed_scores.sort_by(|a, b| b.total_cmp(a));
    let selected_n = ((directed_scores.len() as f64) * options.top_quantile.clamp(0.01, 1.0))
        .ceil()
        .max(1.0) as usize;
    let threshold = directed_scores[selected_n.min(directed_scores.len()) - 1];

    let train = evaluate_factor_threshold(train_rows, descriptor, direction, threshold);
    let test = evaluate_factor_threshold(test_rows, descriptor, direction, threshold);
    Some(FactorWalkForwardWindow {
        window_index,
        train_start,
        train_end,
        test_start,
        test_end,
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        direction,
        threshold,
        train_settlement_rank_ic,
        train_executable_pnl_rank_ic,
        train,
        test,
    })
}

fn is_walk_forward_candidate_descriptor(descriptor: &FactorV2Descriptor) -> bool {
    // `future_exit_*` descriptors are diagnostic labels for exit feasibility.
    // They are intentionally reviewed in the single-window report, but using
    // them as train-time selection factors would leak future PM quote movement.
    !is_future_exit_diagnostic_factor(descriptor.name)
}

fn is_future_exit_diagnostic_factor(name: &str) -> bool {
    name.starts_with("future_exit_")
}

fn factor_name_matches_filter(name: &str, filter: &Option<String>) -> bool {
    let Some(filter) = filter.as_deref() else {
        return true;
    };
    filter
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .any(|part| name.contains(part))
}

fn walk_forward_time_slice(
    rows: &[FactorObservationV2],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> &[FactorObservationV2] {
    let lo = rows.partition_point(|row| row.tick_ts < start);
    let hi = rows.partition_point(|row| row.tick_ts < end);
    &rows[lo..hi]
}

type EventEndIndex<'a> = HashMap<&'a str, Option<DateTime<Utc>>>;

fn inferred_event_ends(rows: &[FactorObservationV2]) -> EventEndIndex<'_> {
    let mut event_ends = HashMap::new();
    for row in rows {
        let inferred = inferred_event_end(row);
        match event_ends.entry(row.event_id.as_str()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(inferred);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let aggregated = match (entry.get().as_ref(), inferred) {
                    (Some(current), Some(next)) => Some((*current).max(next)),
                    _ => None,
                };
                entry.insert(aggregated);
            }
        }
    }
    event_ends
}

fn event_disjoint_walk_forward_slices<'a>(
    rows: &'a [FactorObservationV2],
    event_ends: &EventEndIndex<'_>,
    train_start: DateTime<Utc>,
    train_end: DateTime<Utc>,
    test_start: DateTime<Utc>,
    test_end: DateTime<Utc>,
) -> (Vec<&'a FactorObservationV2>, Vec<&'a FactorObservationV2>) {
    let train = walk_forward_time_slice(rows, train_start, train_end);
    let test = walk_forward_time_slice(rows, test_start, test_end);
    let ends_in = |row: &&FactorObservationV2, start, end| {
        event_ends
            .get(row.event_id.as_str())
            .and_then(Option::as_ref)
            .is_some_and(|event_end| *event_end >= start && *event_end < end)
    };

    (
        train
            .iter()
            .filter(|row| ends_in(row, train_start, train_end))
            .collect(),
        test.iter()
            .filter(|row| ends_in(row, test_start, test_end))
            .collect(),
    )
}

fn inferred_event_end(row: &FactorObservationV2) -> Option<DateTime<Utc>> {
    if row.time_remaining_secs < 0 {
        return None;
    }
    Duration::try_seconds(row.time_remaining_secs)
        .and_then(|remaining| row.tick_ts.checked_add_signed(remaining))
}

fn evaluate_factor_threshold(
    rows: &[&FactorObservationV2],
    descriptor: FactorV2Descriptor,
    direction: f64,
    threshold: f64,
) -> FactorSelectionMetrics {
    let scored_n = rows
        .iter()
        .filter(|row| (descriptor.accessor)(row).is_finite())
        .count();
    let selected: Vec<&FactorObservationV2> = rows
        .iter()
        .copied()
        .filter(|row| {
            let value = (descriptor.accessor)(row);
            value.is_finite() && value * direction >= threshold
        })
        .collect();
    let filled: Vec<&FactorObservationV2> = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();
    FactorSelectionMetrics {
        n: scored_n,
        selected_n: selected.len(),
        executable_fill_rate: ratio(filled.len(), selected.len()),
        rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        total_pnl_after_cost: total_pnl,
        avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        sharpe: trade_sharpe(&pnl_values),
        max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    }
}

fn aggregate_walk_forward_windows(
    windows: &[FactorWalkForwardWindow],
) -> Vec<FactorWalkForwardAggregate> {
    let mut grouped: BTreeMap<String, Vec<&FactorWalkForwardWindow>> = BTreeMap::new();
    for window in windows {
        grouped
            .entry(window.factor.clone())
            .or_default()
            .push(window);
    }
    let mut aggregates = Vec::with_capacity(grouped.len());
    for (factor, rows) in grouped {
        let Some(first) = rows.first() else {
            continue;
        };
        let total_test_pnl_after_cost = rows
            .iter()
            .map(|row| row.test.total_pnl_after_cost)
            .sum::<f64>();
        let min_test_pnl_after_cost = rows
            .iter()
            .map(|row| row.test.total_pnl_after_cost)
            .fold(f64::INFINITY, f64::min);
        let positive_windows = rows
            .iter()
            .filter(|row| row.test.total_pnl_after_cost > 0.0)
            .count();
        aggregates.push(FactorWalkForwardAggregate {
            factor,
            family: first.family,
            layer: first.layer,
            windows: rows.len(),
            positive_window_ratio: ratio(positive_windows, rows.len()),
            total_test_pnl_after_cost,
            avg_test_pnl_per_window: total_test_pnl_after_cost / rows.len() as f64,
            min_test_pnl_after_cost,
            avg_test_fill_rate: mean(rows.iter().map(|row| row.test.executable_fill_rate)),
            avg_test_rejection_rate: mean(rows.iter().map(|row| row.test.rejection_rate)),
        });
    }
    aggregates.sort_by(|a, b| {
        b.total_test_pnl_after_cost
            .total_cmp(&a.total_test_pnl_after_cost)
            .then_with(|| b.positive_window_ratio.total_cmp(&a.positive_window_ratio))
    });
    aggregates
}

fn review_one_factor(
    rows: &[FactorObservationV2],
    descriptor: FactorV2Descriptor,
    options: &FactorReviewOptions,
) -> Option<SingleFactorReview> {
    let scored: Vec<(usize, f64)> = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| {
            let value = (descriptor.accessor)(row);
            value.is_finite().then_some((idx, value))
        })
        .collect();
    if scored.len() < options.min_observations {
        return None;
    }

    let settlement_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(idx, score)| rows[*idx].label_settlement_win.map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();
    let executable_pairs: Vec<(f64, f64)> = scored
        .iter()
        .filter_map(|(idx, score)| executable_pnl(&rows[*idx]).map(|label| (*score, label)))
        .filter(|(score, label)| score.is_finite() && label.is_finite())
        .collect();

    let settlement_pearson_ic = pair_pearson(&settlement_pairs);
    let settlement_rank_ic = pair_spearman(&settlement_pairs);
    let executable_pnl_pearson_ic = pair_pearson(&executable_pairs);
    let executable_pnl_rank_ic = pair_spearman(&executable_pairs);
    let direction = if executable_pnl_rank_ic.is_finite() {
        executable_pnl_rank_ic.signum()
    } else if settlement_rank_ic.is_finite() {
        settlement_rank_ic.signum()
    } else {
        1.0
    };
    let direction = if direction.abs() <= EPS {
        1.0
    } else {
        direction
    };

    let mut directed = scored;
    directed.sort_by(|a, b| (b.1 * direction).total_cmp(&(a.1 * direction)));
    let selected_n = ((directed.len() as f64) * options.top_quantile.clamp(0.01, 1.0))
        .ceil()
        .max(1.0) as usize;
    let selected: Vec<&FactorObservationV2> = directed
        .iter()
        .take(selected_n)
        .map(|(idx, _)| &rows[*idx])
        .collect();
    let filled: Vec<&FactorObservationV2> = selected
        .iter()
        .copied()
        .filter(|row| executable_pnl(row).is_some())
        .collect();
    let pnls: Vec<(DateTime<Utc>, f64)> = filled
        .iter()
        .filter_map(|row| executable_pnl(row).map(|pnl| (row.tick_ts, pnl)))
        .collect();
    let pnl_values: Vec<f64> = pnls.iter().map(|(_, pnl)| *pnl).collect();
    let total_pnl = pnl_values.iter().sum::<f64>();

    Some(SingleFactorReview {
        factor: descriptor.name.to_string(),
        family: descriptor.family,
        layer: descriptor.layer,
        n: directed.len(),
        coverage: directed.len() as f64 / rows.len().max(1) as f64,
        settlement_pearson_ic,
        settlement_rank_ic,
        executable_pnl_pearson_ic,
        executable_pnl_rank_ic,
        selected_n: selected.len(),
        selected_rejection_rate: ratio(
            selected.iter().filter(|row| !entry_fillable(row)).count(),
            selected.len(),
        ),
        selected_executable_fill_rate: ratio(filled.len(), selected.len()),
        selected_avg_slippage_bps: mean(selected.iter().map(|row| row.slippage_to_fill_15u_bps)),
        selected_total_pnl_after_cost: total_pnl,
        selected_avg_pnl_after_cost: if pnl_values.is_empty() {
            f64::NAN
        } else {
            total_pnl / pnl_values.len() as f64
        },
        selected_sharpe: trade_sharpe(&pnl_values),
        selected_max_drawdown: max_drawdown(&pnls),
        by_symbol_positive_ratio: positive_group_ratio(&filled, |row| row.symbol.clone()),
        by_time_bucket_positive_ratio: positive_group_ratio(&filled, |row| {
            row.regime.as_str().to_string()
        }),
    })
}

type PmBookIndex<'a> = HashMap<(String, String, String), Vec<&'a ResearchPmBookSnapshot>>;

#[derive(Debug, Clone, Copy)]
struct SweepFill {
    fillable: bool,
    avg_price: f64,
    shares: f64,
    fee_usd: f64,
    levels_used: f64,
    slippage_bps: f64,
}

fn nan_f64() -> f64 {
    f64::NAN
}

impl Default for SweepFill {
    fn default() -> Self {
        Self {
            fillable: false,
            avg_price: f64::NAN,
            shares: f64::NAN,
            fee_usd: f64::NAN,
            levels_used: f64::NAN,
            slippage_bps: f64::NAN,
        }
    }
}

fn build_pm_book_index(pm_books: &[ResearchPmBookSnapshot]) -> PmBookIndex<'_> {
    let mut index: PmBookIndex<'_> = HashMap::new();
    for book in pm_books {
        index
            .entry((
                book.event_id.clone(),
                book.side.to_ascii_uppercase(),
                book.token_id.clone(),
            ))
            .or_default()
            .push(book);
    }
    for books in index.values_mut() {
        books.sort_by_key(|book| book.ts);
    }
    index
}

fn latest_pm_book<'a>(
    index: &'a PmBookIndex<'a>,
    event_id: &str,
    token_id: &str,
    side: ReviewSide,
    tick_ts: DateTime<Utc>,
    max_age_secs: i64,
) -> Option<&'a ResearchPmBookSnapshot> {
    let key = (
        event_id.to_string(),
        book_side_key(side).to_string(),
        token_id.to_string(),
    );
    let books = index.get(&key)?;
    let pos = books.partition_point(|book| book.ts <= tick_ts);
    let book = *books.get(pos.checked_sub(1)?)?;
    let age_millis = (tick_ts - book.ts).num_milliseconds();
    let max_age_millis = max_age_secs.max(0).saturating_mul(1_000);
    (age_millis >= 0 && age_millis <= max_age_millis).then_some(book)
}

fn side_token_id(row: &FactorObservation, side: ReviewSide) -> &str {
    match side {
        ReviewSide::Up => &row.up_token_id,
        ReviewSide::Down => &row.down_token_id,
    }
}

fn book_side_key(side: ReviewSide) -> &'static str {
    match side {
        ReviewSide::Up => "UP",
        ReviewSide::Down => "DOWN",
    }
}

fn sweep_buy_to_stake(
    levels: &[crate::factors::ResearchPmBookLevel],
    reference_price: f64,
    stake_usd: f64,
) -> SweepFill {
    sweep_buy_to_stake_with_config(levels, reference_price, stake_usd, 1.0, None)
}

fn sweep_buy_to_stake_with_config(
    levels: &[crate::factors::ResearchPmBookLevel],
    reference_price: f64,
    stake_usd: f64,
    visible_depth_haircut: f64,
    max_levels: Option<usize>,
) -> SweepFill {
    if !valid_price(reference_price) || !stake_usd.is_finite() || stake_usd <= 0.0 {
        return SweepFill::default();
    }
    let haircut = visible_depth_haircut.clamp(0.0, 1.0);
    if haircut <= EPS {
        return SweepFill::default();
    }
    let mut remaining = stake_usd;
    let mut spent = 0.0;
    let mut shares = 0.0;
    let mut fee_usd = 0.0;
    let mut levels_used = 0.0;
    for level in levels
        .iter()
        .take(max_levels.unwrap_or(usize::MAX))
        .filter(|level| valid_price(level.price) && level.size.is_finite() && level.size > 0.0)
    {
        if remaining <= EPS {
            break;
        }
        let level_notional = level.price * level.size * haircut;
        let take_notional = remaining.min(level_notional);
        if take_notional <= EPS {
            continue;
        }
        spent += take_notional;
        let take_shares = take_notional / level.price;
        shares += take_shares;
        fee_usd +=
            ploy_market_contracts::polymarket_crypto_taker_fee_cost(take_shares, level.price);
        remaining -= take_notional;
        levels_used += 1.0;
    }
    if remaining > EPS || shares <= EPS {
        return SweepFill::default();
    }
    let avg_price = spent / shares;
    SweepFill {
        fillable: true,
        avg_price,
        shares,
        fee_usd,
        levels_used,
        slippage_bps: ((avg_price - reference_price).max(0.0) / reference_price) * 10_000.0,
    }
}

fn sweep_sell_shares(
    levels: &[crate::factors::ResearchPmBookLevel],
    reference_price: f64,
    shares_to_sell: f64,
) -> SweepFill {
    sweep_sell_shares_with_config(levels, reference_price, shares_to_sell, 1.0, None)
}

fn sweep_sell_shares_with_config(
    levels: &[crate::factors::ResearchPmBookLevel],
    reference_price: f64,
    shares_to_sell: f64,
    visible_depth_haircut: f64,
    max_levels: Option<usize>,
) -> SweepFill {
    if !valid_price(reference_price) || !shares_to_sell.is_finite() || shares_to_sell <= 0.0 {
        return SweepFill::default();
    }
    let haircut = visible_depth_haircut.clamp(0.0, 1.0);
    if haircut <= EPS {
        return SweepFill::default();
    }
    let mut remaining = shares_to_sell;
    let mut proceeds = 0.0;
    let mut sold = 0.0;
    let mut fee_usd = 0.0;
    let mut levels_used = 0.0;
    for level in levels
        .iter()
        .take(max_levels.unwrap_or(usize::MAX))
        .filter(|level| valid_price(level.price) && level.size.is_finite() && level.size > 0.0)
    {
        if remaining <= EPS {
            break;
        }
        let take_shares = remaining.min(level.size * haircut);
        proceeds += take_shares * level.price;
        sold += take_shares;
        fee_usd +=
            ploy_market_contracts::polymarket_crypto_taker_fee_cost(take_shares, level.price);
        remaining -= take_shares;
        levels_used += 1.0;
    }
    if remaining > EPS || sold <= EPS {
        return SweepFill::default();
    }
    let avg_price = proceeds / sold;
    SweepFill {
        fillable: true,
        avg_price,
        shares: sold,
        fee_usd,
        levels_used,
        slippage_bps: ((reference_price - avg_price).max(0.0) / reference_price) * 10_000.0,
    }
}

#[derive(Default)]
struct FullDepthExecutionSample {
    entry_fillable: bool,
    entry_avg_price: f64,
    entry_slippage_bps: f64,
    entry_levels_used: f64,
    exit_5s_fillable: bool,
    exit_10s_fillable: bool,
    exit_30s_fillable: bool,
    exit_10s_slippage_bps: f64,
    exit_30s_slippage_bps: f64,
    reprice_pnl_5s: Option<f64>,
    reprice_pnl_10s: Option<f64>,
    reprice_pnl_30s: Option<f64>,
    settlement_pnl: Option<f64>,
}

pub fn build_full_depth_execution_matrix(
    source_rows: &[FactorObservation],
    pm_books: &[ResearchPmBookSnapshot],
    options: FullDepthExecutionMatrixOptions,
) -> FullDepthExecutionMatrixReport {
    let book_index = build_pm_book_index(pm_books);
    let mut by_event: BTreeMap<String, Vec<&FactorObservation>> = BTreeMap::new();
    for row in source_rows {
        by_event.entry(row.event_id.clone()).or_default().push(row);
    }
    for rows in by_event.values_mut() {
        rows.sort_by_key(|row| row.tick_ts);
    }

    let mut groups: BTreeMap<
        (
            String,
            &'static str,
            String,
            String,
            String,
            String,
            String,
            String,
        ),
        Vec<FullDepthExecutionSample>,
    > = BTreeMap::new();
    let stakes = if options.stakes_usd.is_empty() {
        vec![DEFAULT_STAKE_USD]
    } else {
        options.stakes_usd.clone()
    };
    for source in source_rows {
        for side in [ReviewSide::Up, ReviewSide::Down] {
            let (entry_ask, exit_bid, settlement_win) = side_market_values(source, side);
            let pm_spread_bps = if valid_price(entry_ask) && valid_price(exit_bid) {
                ((entry_ask - exit_bid).max(0.0) / entry_ask) * 10_000.0
            } else {
                f64::NAN
            };
            let current_book = latest_pm_book(
                &book_index,
                &source.event_id,
                side_token_id(source, side),
                side,
                source.tick_ts,
                options.max_quote_age_secs,
            );
            let quote_age_secs = current_book
                .map(|book| (source.tick_ts - book.ts).num_milliseconds() as f64 / 1000.0)
                .unwrap_or(f64::NAN);
            for stake_usd in stakes.iter().copied().filter(|stake| *stake > 0.0) {
                let key = (
                    format!("{stake_usd:.2}"),
                    side.as_str(),
                    source.symbol.clone(),
                    Regime::from_secs(source.time_remaining_secs)
                        .as_str()
                        .to_string(),
                    matrix_distance_bucket(source.distance_over_sigma * side.multiplier()),
                    price_bucket(entry_ask).unwrap_or_else(|| "unknown".to_string()),
                    matrix_spread_bucket(pm_spread_bps),
                    pm_lag_bucket(quote_age_secs).unwrap_or_else(|| "missing".to_string()),
                );
                let sample = build_full_depth_execution_sample(
                    source,
                    side,
                    stake_usd,
                    entry_ask,
                    settlement_win,
                    current_book,
                    by_event.get(&source.event_id).map(Vec::as_slice),
                    &book_index,
                    &options,
                );
                groups.entry(key).or_default().push(sample);
            }
        }
    }

    let mut rows = groups
        .into_iter()
        .filter_map(|(key, samples)| {
            if samples.len() < options.min_bucket_observations {
                return None;
            }
            let (
                stake_key,
                side_key,
                symbol,
                time_bucket,
                distance_bucket,
                entry_price_bucket,
                spread_bucket,
                quote_age_bucket,
            ) = key;
            let stake_usd = stake_key.parse::<f64>().unwrap_or(f64::NAN);
            Some(summarize_full_depth_execution_group(
                stake_usd,
                symbol,
                if side_key == "up" {
                    ReviewSide::Up
                } else {
                    ReviewSide::Down
                },
                time_bucket,
                distance_bucket,
                entry_price_bucket,
                spread_bucket,
                quote_age_bucket,
                &samples,
            ))
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a.stake_usd
            .total_cmp(&b.stake_usd)
            .then_with(|| a.symbol.cmp(&b.symbol))
            .then_with(|| a.side.as_str().cmp(b.side.as_str()))
            .then_with(|| {
                b.roundtrip_fill_rate_10s
                    .total_cmp(&a.roundtrip_fill_rate_10s)
            })
            .then_with(|| b.avg_reprice_pnl_10s.total_cmp(&a.avg_reprice_pnl_10s))
    });
    FullDepthExecutionMatrixReport { options, rows }
}

#[allow(clippy::too_many_arguments)]
fn build_full_depth_execution_sample(
    source: &FactorObservation,
    side: ReviewSide,
    stake_usd: f64,
    entry_ask: f64,
    settlement_win: f64,
    current_book: Option<&ResearchPmBookSnapshot>,
    event_rows: Option<&[&FactorObservation]>,
    book_index: &PmBookIndex<'_>,
    options: &FullDepthExecutionMatrixOptions,
) -> FullDepthExecutionSample {
    let entry_sweep = current_book
        .map(|book| {
            sweep_buy_to_stake_with_config(
                &book.asks,
                entry_ask,
                stake_usd,
                options.visible_depth_haircut,
                options.max_levels,
            )
        })
        .unwrap_or_default();
    let entry_fee = if entry_sweep.fillable && entry_sweep.fee_usd.is_finite() {
        entry_sweep.fee_usd
    } else {
        f64::NAN
    };
    let settlement_pnl =
        if entry_sweep.fillable && settlement_win.is_finite() && entry_fee.is_finite() {
            let payout = if settlement_win >= 0.5 {
                entry_sweep.shares
            } else {
                0.0
            };
            Some(payout - stake_usd - entry_fee)
        } else {
            None
        };

    let mut sample = FullDepthExecutionSample {
        entry_fillable: entry_sweep.fillable,
        entry_avg_price: entry_sweep.avg_price,
        entry_slippage_bps: entry_sweep.slippage_bps,
        entry_levels_used: entry_sweep.levels_used,
        settlement_pnl,
        ..Default::default()
    };

    if !entry_sweep.fillable || !entry_fee.is_finite() {
        return sample;
    }

    for horizon_secs in [5, 10, 30] {
        let future = event_rows.and_then(|rows| {
            let target = source.tick_ts + chrono::Duration::seconds(horizon_secs);
            rows.iter().copied().find(|row| row.tick_ts >= target)
        });
        let Some(future) = future else {
            continue;
        };
        let (_, future_bid, _) = side_market_values(future, side);
        let exit_sweep = latest_pm_book(
            book_index,
            &future.event_id,
            side_token_id(future, side),
            side,
            future.tick_ts,
            options.max_quote_age_secs,
        )
        .map(|book| {
            sweep_sell_shares_with_config(
                &book.bids,
                future_bid,
                entry_sweep.shares,
                options.visible_depth_haircut,
                options.max_levels,
            )
        })
        .unwrap_or_default();
        let reprice_pnl = exit_sweep.fillable.then_some(
            exit_sweep.shares * exit_sweep.avg_price - stake_usd - entry_fee - exit_sweep.fee_usd,
        );
        match horizon_secs {
            5 => {
                sample.exit_5s_fillable = exit_sweep.fillable;
                sample.reprice_pnl_5s = reprice_pnl;
            }
            10 => {
                sample.exit_10s_fillable = exit_sweep.fillable;
                sample.exit_10s_slippage_bps = exit_sweep.slippage_bps;
                sample.reprice_pnl_10s = reprice_pnl;
            }
            30 => {
                sample.exit_30s_fillable = exit_sweep.fillable;
                sample.exit_30s_slippage_bps = exit_sweep.slippage_bps;
                sample.reprice_pnl_30s = reprice_pnl;
            }
            _ => {}
        }
    }
    sample
}

#[allow(clippy::too_many_arguments)]
fn summarize_full_depth_execution_group(
    stake_usd: f64,
    symbol: String,
    side: ReviewSide,
    time_bucket: String,
    distance_bucket: String,
    entry_price_bucket: String,
    spread_bucket: String,
    quote_age_bucket: String,
    samples: &[FullDepthExecutionSample],
) -> FullDepthExecutionMatrixRow {
    let entry_slippages = samples
        .iter()
        .filter(|sample| sample.entry_fillable)
        .map(|sample| sample.entry_slippage_bps)
        .collect::<Vec<_>>();
    FullDepthExecutionMatrixRow {
        stake_usd,
        symbol,
        side,
        time_bucket,
        distance_bucket,
        entry_price_bucket,
        spread_bucket,
        quote_age_bucket,
        count: samples.len(),
        entry_fill_rate: ratio(
            samples
                .iter()
                .filter(|sample| sample.entry_fillable)
                .count(),
            samples.len(),
        ),
        entry_avg_price_mean: mean(
            samples
                .iter()
                .filter(|sample| sample.entry_fillable)
                .map(|sample| sample.entry_avg_price),
        ),
        entry_avg_slippage_bps: mean(entry_slippages.iter().copied()),
        entry_p50_slippage_bps: percentile(entry_slippages.clone(), 0.50),
        entry_p90_slippage_bps: percentile(entry_slippages, 0.90),
        entry_avg_levels_used: mean(
            samples
                .iter()
                .filter(|sample| sample.entry_fillable)
                .map(|sample| sample.entry_levels_used),
        ),
        exit_5s_fill_rate: ratio(
            samples
                .iter()
                .filter(|sample| sample.exit_5s_fillable)
                .count(),
            samples.len(),
        ),
        exit_10s_fill_rate: ratio(
            samples
                .iter()
                .filter(|sample| sample.exit_10s_fillable)
                .count(),
            samples.len(),
        ),
        exit_30s_fill_rate: ratio(
            samples
                .iter()
                .filter(|sample| sample.exit_30s_fillable)
                .count(),
            samples.len(),
        ),
        exit_10s_avg_slippage_bps: mean(
            samples
                .iter()
                .filter(|sample| sample.exit_10s_fillable)
                .map(|sample| sample.exit_10s_slippage_bps),
        ),
        exit_30s_avg_slippage_bps: mean(
            samples
                .iter()
                .filter(|sample| sample.exit_30s_fillable)
                .map(|sample| sample.exit_30s_slippage_bps),
        ),
        roundtrip_fill_rate_5s: ratio(
            samples
                .iter()
                .filter(|sample| sample.entry_fillable && sample.exit_5s_fillable)
                .count(),
            samples.len(),
        ),
        roundtrip_fill_rate_10s: ratio(
            samples
                .iter()
                .filter(|sample| sample.entry_fillable && sample.exit_10s_fillable)
                .count(),
            samples.len(),
        ),
        roundtrip_fill_rate_30s: ratio(
            samples
                .iter()
                .filter(|sample| sample.entry_fillable && sample.exit_30s_fillable)
                .count(),
            samples.len(),
        ),
        avg_settlement_pnl: mean(samples.iter().filter_map(|sample| sample.settlement_pnl)),
        avg_reprice_pnl_5s: mean(samples.iter().filter_map(|sample| sample.reprice_pnl_5s)),
        avg_reprice_pnl_10s: mean(samples.iter().filter_map(|sample| sample.reprice_pnl_10s)),
        avg_reprice_pnl_30s: mean(samples.iter().filter_map(|sample| sample.reprice_pnl_30s)),
    }
}

fn side_market_values(row: &FactorObservation, side: ReviewSide) -> (f64, f64, f64) {
    match side {
        ReviewSide::Up => (row.pm_up_ask, row.pm_up_bid, row.settlement_up),
        ReviewSide::Down => (row.pm_down_ask, row.pm_down_bid, 1.0 - row.settlement_up),
    }
}

#[derive(Clone)]
struct SettlementProbabilitySample {
    symbol: String,
    q: f64,
    win: f64,
    entry_price: f64,
    edge: f64,
    pnl: f64,
    conservative_pnl: f64,
    conservative_covered: bool,
}

fn push_settlement_probability_sample(
    by_model: &mut BTreeMap<String, Vec<SettlementProbabilitySample>>,
    model: &str,
    row: &FactorObservationV2,
    win: f64,
    pnl: f64,
    conservative_pnl: Option<f64>,
    q: f64,
) {
    let q = clamp_probability(q);
    let entry_price = row.entry_sweep_avg_price_15u;
    by_model
        .entry(model.to_string())
        .or_default()
        .push(SettlementProbabilitySample {
            symbol: row.symbol.clone(),
            q,
            win,
            entry_price,
            edge: q - entry_price,
            pnl,
            conservative_pnl: conservative_pnl.unwrap_or(0.0),
            conservative_covered: conservative_pnl.is_some(),
        });
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EventVolSurfaceKey {
    symbol: String,
    time_bucket: &'static str,
    distance_bucket: &'static str,
}

#[derive(Debug, Clone, Copy, Default)]
struct EventVolSurfaceStats {
    count: usize,
    wins: f64,
}

impl EventVolSurfaceStats {
    fn add(&mut self, win: f64) {
        self.count += 1;
        self.wins += win;
    }

    fn subtract(self, other: Option<Self>) -> Self {
        let Some(other) = other else {
            return self;
        };
        Self {
            count: self.count.saturating_sub(other.count),
            wins: self.wins - other.wins,
        }
    }

    fn mean(self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.wins / self.count as f64)
        }
    }
}

#[derive(Debug, Clone, Default)]
struct EventVolSurfaceEventStats {
    global: EventVolSurfaceStats,
    buckets: HashMap<EventVolSurfaceKey, EventVolSurfaceStats>,
}

#[derive(Debug, Clone)]
struct EventVolSurface {
    min_bucket_observations: usize,
    shrinkage_observations: usize,
    global: EventVolSurfaceStats,
    buckets: HashMap<EventVolSurfaceKey, EventVolSurfaceStats>,
    events: HashMap<String, EventVolSurfaceEventStats>,
}

impl EventVolSurface {
    fn fit(
        rows: &[(&FactorObservationV2, f64, f64, Option<f64>)],
        min_bucket_observations: usize,
        shrinkage_observations: usize,
    ) -> Self {
        let mut surface = Self {
            min_bucket_observations,
            shrinkage_observations,
            global: EventVolSurfaceStats::default(),
            buckets: HashMap::new(),
            events: HashMap::new(),
        };
        for (row, win, _, _) in rows {
            if !win.is_finite() {
                continue;
            }
            surface.global.add(*win);
            let event_stats = surface.events.entry(row.event_id.clone()).or_default();
            event_stats.global.add(*win);
            if let Some(key) = event_vol_surface_key(row) {
                surface.buckets.entry(key.clone()).or_default().add(*win);
                event_stats.buckets.entry(key).or_default().add(*win);
            }
        }
        surface
    }

    fn predict(&self, row: &FactorObservationV2) -> Option<f64> {
        let event_stats = self.events.get(&row.event_id);
        let global = self.global.subtract(event_stats.map(|stats| stats.global));
        let global_mean = global.mean()?;
        let Some(key) = event_vol_surface_key(row) else {
            return Some(global_mean);
        };
        let bucket = self
            .buckets
            .get(&key)
            .copied()
            .unwrap_or_default()
            .subtract(event_stats.and_then(|stats| stats.buckets.get(&key).copied()));
        let Some(bucket_mean) = bucket.mean() else {
            return Some(global_mean);
        };
        let mut bucket_weight =
            bucket.count as f64 / (bucket.count + self.shrinkage_observations) as f64;
        if bucket.count < self.min_bucket_observations {
            bucket_weight *= bucket.count as f64 / self.min_bucket_observations as f64;
        }
        Some(bucket_weight * bucket_mean + (1.0 - bucket_weight) * global_mean)
    }
}

fn event_vol_surface_key(row: &FactorObservationV2) -> Option<EventVolSurfaceKey> {
    if !row.side_distance_over_sigma.is_finite() {
        return None;
    }
    Some(EventVolSurfaceKey {
        symbol: row.symbol.clone(),
        time_bucket: event_surface_time_bucket(row.time_remaining_secs),
        distance_bucket: event_surface_distance_bucket(row.side_distance_over_sigma),
    })
}

fn event_surface_time_bucket(time_remaining_secs: i64) -> &'static str {
    if time_remaining_secs < 60 {
        "<60s"
    } else if time_remaining_secs < 180 {
        "60..180s"
    } else if time_remaining_secs < 300 {
        "180..300s"
    } else {
        ">=300s"
    }
}

fn event_surface_distance_bucket(distance_z: f64) -> &'static str {
    if distance_z < -1.5 {
        "<-1.5z"
    } else if distance_z < -0.5 {
        "-1.5..-0.5z"
    } else if distance_z <= 0.5 {
        "-0.5..0.5z"
    } else if distance_z <= 1.5 {
        "0.5..1.5z"
    } else {
        ">1.5z"
    }
}

fn settlement_probability_models(row: &FactorObservationV2) -> Vec<(&'static str, f64)> {
    let mut models = Vec::with_capacity(9);
    models.push(("q_naive_50_50", 0.5));
    if let Some(q) = settlement_market_midpoint_probability(row) {
        models.push(("q_market_midpoint", q));
    }
    if valid_probability(row.side_chainlink_prob) {
        models.push(("q_chainlink_digital", row.side_chainlink_prob));
    }
    if row.side_distance_over_sigma.is_finite() {
        let base_z = row.side_distance_over_sigma;
        models.push(("q_base_distance_phi", normal_cdf(base_z)));
        let drift_z = side_lob_drift_z(row);
        if drift_z.is_finite() {
            models.push(("q_distance_lob_drift_phi", normal_cdf(base_z + drift_z)));
        }
        let vol_adjusted_z = volatility_adjusted_distance_z(row);
        if vol_adjusted_z.is_finite() {
            models.push(("q_distance_vol_adjusted_phi", normal_cdf(vol_adjusted_z)));
        }
        if drift_z.is_finite() && vol_adjusted_z.is_finite() {
            models.push((
                "q_distance_lob_vol_phi",
                normal_cdf(vol_adjusted_z + drift_z),
            ));
        }
    }
    if valid_probability(row.side_fair_prob) {
        models.push(("q_existing_fair_prob", row.side_fair_prob));
    }
    if valid_probability(row.side_model_prob) {
        models.push(("q_existing_model_prob", row.side_model_prob));
    }
    models
}

fn settlement_probability_final_blend(
    row: &FactorObservationV2,
    event_surface_q: Option<f64>,
) -> Option<f64> {
    settlement_probability_weighted_components(row, event_surface_q, 0.45, 0.0, 0.35, 0.20, 0.0)
}

fn validated_probability_blends(
    prior: Option<&LlmPriorSpec>,
) -> Vec<(String, &LlmProbabilityBlendSpec)> {
    let mut by_name = BTreeMap::new();
    let Some(prior) = prior.filter(|prior| validate_prediction_research_prior(prior).is_ok())
    else {
        return Vec::new();
    };
    for blend in &prior.probability_blends {
        by_name
            .entry(format!("q_llm_{}", blend.name))
            .or_insert(blend);
    }
    by_name.into_iter().collect()
}

pub fn validate_prediction_research_prior(prior: &LlmPriorSpec) -> Result<(), &'static str> {
    if prior.target.as_deref() != Some("full_depth_settlement_executable_pnl") {
        return Err("target must be full_depth_settlement_executable_pnl");
    }
    for (value, message) in [
        (prior.mission_id.as_deref(), "mission_id is required"),
        (
            prior.data_snapshot_id.as_deref(),
            "data_snapshot_id is required",
        ),
        (
            prior.prompt_snapshot_id.as_deref(),
            "prompt_snapshot_id is required",
        ),
        (
            prior.search_policy_snapshot_id.as_deref(),
            "search_policy_snapshot_id is required",
        ),
    ] {
        if !value.is_some_and(|value| {
            !value.trim().is_empty() && !value.trim().starts_with("REPLACE_WITH_")
        }) {
            return Err(message);
        }
    }
    if prior.symbols.len() != 1 || prior.symbols[0].trim().is_empty() {
        return Err("exactly one symbol is required");
    }
    if prior.horizon.as_deref() != Some("5m") {
        return Err("horizon must be 5m");
    }
    if !prior.mutations.is_empty() {
        return Err("prediction prior may not contain factor mutations");
    }
    if prior.probability_blends.is_empty() {
        return Err("prediction prior must contain at least one probability blend");
    }
    let mut names = BTreeSet::new();
    for blend in &prior.probability_blends {
        if !probability_blend_schema_valid(blend) {
            return Err("probability blend is invalid");
        }
        if !names.insert(blend.name.as_str()) {
            return Err("probability blend names must be unique");
        }
    }
    Ok(())
}

fn prediction_prior_matches_row(prior: &LlmPriorSpec, row: &FactorObservationV2) -> bool {
    if prior.symbols.is_empty() {
        return true;
    }
    let row_symbol = normalized_underlying_symbol(&row.symbol);
    prior
        .symbols
        .iter()
        .any(|symbol| normalized_underlying_symbol(symbol) == row_symbol)
}

fn probability_blend_weights_valid(blend: &LlmProbabilityBlendSpec) -> bool {
    let weights = [
        blend.market_midpoint_weight,
        blend.chainlink_digital_weight,
        blend.distance_lob_vol_weight,
        blend.event_surface_weight,
        blend.existing_model_weight,
    ];
    let total = weights.iter().sum::<f64>();
    weights
        .iter()
        .all(|weight| weight.is_finite() && *weight >= 0.0)
        && total.is_finite()
        && total > EPS
}

fn probability_blend_schema_valid(blend: &LlmProbabilityBlendSpec) -> bool {
    !blend.name.is_empty()
        && blend.name.len() <= 80
        && !blend.hypothesis.trim().is_empty()
        && blend.hypothesis.chars().count() <= 500
        && blend
            .name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        && probability_blend_weights_valid(blend)
}

fn settlement_probability_weighted_blend(
    row: &FactorObservationV2,
    event_surface_q: Option<f64>,
    blend: &LlmProbabilityBlendSpec,
) -> Option<f64> {
    if !probability_blend_weights_valid(blend) {
        return None;
    }
    if blend.chainlink_digital_weight > EPS && !valid_probability(row.side_chainlink_prob) {
        return None;
    }
    settlement_probability_weighted_components(
        row,
        event_surface_q,
        blend.market_midpoint_weight,
        blend.chainlink_digital_weight,
        blend.distance_lob_vol_weight,
        blend.event_surface_weight,
        blend.existing_model_weight,
    )
}

fn settlement_probability_weighted_components(
    row: &FactorObservationV2,
    event_surface_q: Option<f64>,
    market_midpoint_weight: f64,
    chainlink_digital_weight: f64,
    distance_lob_vol_weight: f64,
    event_surface_weight: f64,
    existing_model_weight: f64,
) -> Option<f64> {
    let mut logit_sum = 0.0;
    let mut total_weight = 0.0;
    add_probability_component(
        &mut logit_sum,
        &mut total_weight,
        settlement_market_midpoint_probability(row),
        market_midpoint_weight,
    );
    add_probability_component(
        &mut logit_sum,
        &mut total_weight,
        valid_probability(row.side_chainlink_prob).then_some(row.side_chainlink_prob),
        chainlink_digital_weight,
    );
    add_probability_component(
        &mut logit_sum,
        &mut total_weight,
        settlement_distance_lob_vol_probability(row),
        distance_lob_vol_weight,
    );
    add_probability_component(
        &mut logit_sum,
        &mut total_weight,
        event_surface_q,
        event_surface_weight,
    );
    add_probability_component(
        &mut logit_sum,
        &mut total_weight,
        valid_probability(row.side_model_prob).then_some(row.side_model_prob),
        existing_model_weight,
    );
    if total_weight <= EPS {
        None
    } else {
        Some(inverse_logit(logit_sum / total_weight))
    }
}

fn settlement_market_midpoint_probability(row: &FactorObservationV2) -> Option<f64> {
    if valid_price(row.entry_ask) && valid_price(row.exit_bid) {
        Some((row.entry_ask + row.exit_bid) * 0.5)
    } else {
        None
    }
}

fn settlement_distance_lob_vol_probability(row: &FactorObservationV2) -> Option<f64> {
    if !row.side_distance_over_sigma.is_finite() {
        return None;
    }
    let base_z = row.side_distance_over_sigma;
    let drift_z = side_lob_drift_z(row);
    let vol_adjusted_z = volatility_adjusted_distance_z(row);
    let z = match (drift_z.is_finite(), vol_adjusted_z.is_finite()) {
        (true, true) => vol_adjusted_z + drift_z,
        (true, false) => base_z + drift_z,
        (false, true) => vol_adjusted_z,
        (false, false) => base_z,
    };
    Some(normal_cdf(z))
}

fn add_probability_component(
    logit_sum: &mut f64,
    total_weight: &mut f64,
    q: Option<f64>,
    weight: f64,
) {
    let Some(q) = q.filter(|value| valid_probability(*value)) else {
        return;
    };
    if weight <= 0.0 {
        return;
    }
    *logit_sum += probability_logit(q) * weight;
    *total_weight += weight;
}

fn probability_logit(q: f64) -> f64 {
    let q = clamp_probability(q);
    (q / (1.0 - q)).ln()
}

fn inverse_logit(x: f64) -> f64 {
    if x >= 0.0 {
        let z = (-x).exp();
        1.0 / (1.0 + z)
    } else {
        let z = x.exp();
        z / (1.0 + z)
    }
}

fn side_lob_drift_z(row: &FactorObservationV2) -> f64 {
    let mut score = 0.0;
    let mut weight = 0.0;
    add_finite_component(
        &mut score,
        &mut weight,
        row.obi_10 * row.side.multiplier(),
        0.20,
    );
    add_finite_component(
        &mut score,
        &mut weight,
        row.depth_imbalance * row.side.multiplier(),
        0.15,
    );
    add_finite_component(
        &mut score,
        &mut weight,
        row.microprice_offset_bps * row.side.multiplier() / 5.0,
        0.15,
    );
    add_finite_component(&mut score, &mut weight, row.obi_persistence_30s_side, 0.20);
    add_finite_component(
        &mut score,
        &mut weight,
        row.microprice_momentum_30s_side / 5.0,
        0.10,
    );
    add_finite_component(
        &mut score,
        &mut weight,
        row.cex_signed_volume_ratio_30s * row.side.multiplier(),
        0.20,
    );
    add_finite_component(&mut score, &mut weight, row.cex_breakout_volume_side, 0.15);
    if weight <= EPS {
        return f64::NAN;
    }
    (score / weight).tanh() * 0.35
}

fn volatility_adjusted_distance_z(row: &FactorObservationV2) -> f64 {
    if !row.side_distance_over_sigma.is_finite() {
        return f64::NAN;
    }
    let mut vol_shock = 0.0f64;
    if row.vol_gap.is_finite() {
        vol_shock = vol_shock.max(row.vol_gap);
    }
    if row.deribit_iv_gap_horizon.is_finite() {
        vol_shock = vol_shock.max(row.deribit_iv_gap_horizon);
    }
    if row.deribit_iv_change_60s.is_finite() {
        vol_shock = vol_shock.max(row.deribit_iv_change_60s.abs() * 0.5);
    }
    let denominator = 1.0 + vol_shock.max(0.0).clamp(0.0, 2.0);
    row.side_distance_over_sigma / denominator
}

fn add_finite_component(score: &mut f64, weight: &mut f64, value: f64, component_weight: f64) {
    if value.is_finite() && component_weight > 0.0 {
        *score += value.clamp(-5.0, 5.0) * component_weight;
        *weight += component_weight;
    }
}

fn build_probability_baseline_row(
    model: &str,
    samples: &[SettlementProbabilitySample],
    calibration: &[SettlementProbabilityCalibrationRow],
    edge_buckets: &[SettlementProbabilityEdgeBucketRow],
    top_edge_quantile: f64,
) -> SettlementProbabilityBaselineRow {
    let n = samples.len();
    let top_n = ((n as f64) * top_edge_quantile).ceil().max(1.0) as usize;
    let mut by_edge = samples.to_vec();
    by_edge.sort_by(|a, b| b.edge.total_cmp(&a.edge));
    let top = by_edge.into_iter().take(top_n).collect::<Vec<_>>();
    SettlementProbabilityBaselineRow {
        model: model.to_string(),
        n,
        avg_predicted_q: mean(samples.iter().map(|sample| sample.q)),
        actual_win_rate: mean(samples.iter().map(|sample| sample.win)),
        brier_score: mean(samples.iter().map(|sample| (sample.q - sample.win).powi(2))),
        log_loss: mean(samples.iter().map(|sample| {
            let q = clamp_probability(sample.q);
            -(sample.win * q.ln() + (1.0 - sample.win) * (1.0 - q).ln())
        })),
        expected_calibration_error: expected_calibration_error(calibration, n),
        avg_edge: mean(samples.iter().map(|sample| sample.edge)),
        avg_full_depth_settlement_pnl: mean(samples.iter().map(|sample| sample.pnl)),
        avg_conservative_settlement_pnl: mean(samples.iter().map(|sample| sample.conservative_pnl)),
        conservative_coverage_rate: ratio(
            samples
                .iter()
                .filter(|sample| sample.conservative_covered)
                .count(),
            n,
        ),
        profit_factor: profit_factor(samples.iter().map(|sample| sample.pnl)),
        edge_bucket_monotonic_non_decreasing: edge_bucket_monotonic(edge_buckets),
        top_edge_count: top.len(),
        top_edge_avg_edge: mean(top.iter().map(|sample| sample.edge)),
        top_edge_win_rate: mean(top.iter().map(|sample| sample.win)),
        top_edge_avg_full_depth_settlement_pnl: mean(top.iter().map(|sample| sample.pnl)),
        top_edge_avg_conservative_settlement_pnl: mean(
            top.iter().map(|sample| sample.conservative_pnl),
        ),
        top_edge_conservative_coverage_rate: ratio(
            top.iter()
                .filter(|sample| sample.conservative_covered)
                .count(),
            top.len(),
        ),
    }
}

fn build_probability_calibration_rows(
    model: &str,
    samples: &[SettlementProbabilitySample],
    bucket_count: usize,
) -> Vec<SettlementProbabilityCalibrationRow> {
    let mut buckets: BTreeMap<usize, Vec<SettlementProbabilitySample>> = BTreeMap::new();
    for sample in samples {
        buckets
            .entry(probability_bucket_index(sample.q, bucket_count))
            .or_default()
            .push(sample.clone());
    }
    buckets
        .into_iter()
        .map(|(idx, bucket_samples)| {
            let avg_q = mean(bucket_samples.iter().map(|sample| sample.q));
            let win_rate = mean(bucket_samples.iter().map(|sample| sample.win));
            SettlementProbabilityCalibrationRow {
                model: model.to_string(),
                q_bucket: probability_bucket_label(idx, bucket_count),
                count: bucket_samples.len(),
                avg_predicted_q: avg_q,
                actual_win_rate: win_rate,
                calibration_error: (avg_q - win_rate).abs(),
                avg_edge: mean(bucket_samples.iter().map(|sample| sample.edge)),
                avg_full_depth_settlement_pnl: mean(bucket_samples.iter().map(|sample| sample.pnl)),
                avg_conservative_settlement_pnl: mean(
                    bucket_samples.iter().map(|sample| sample.conservative_pnl),
                ),
            }
        })
        .collect()
}

fn build_probability_edge_bucket_rows(
    model: &str,
    samples: &[SettlementProbabilitySample],
    bucket_count: usize,
    min_bucket_observations: usize,
) -> Vec<SettlementProbabilityEdgeBucketRow> {
    let mut by_edge = samples.to_vec();
    by_edge.sort_by(|a, b| a.edge.total_cmp(&b.edge));
    let n = by_edge.len();
    let mut rows = Vec::new();
    for bucket_idx in 0..bucket_count {
        let start = bucket_idx * n / bucket_count;
        let end = ((bucket_idx + 1) * n / bucket_count).min(n);
        if end <= start {
            continue;
        }
        let bucket_samples = &by_edge[start..end];
        if bucket_samples.len() < min_bucket_observations {
            continue;
        }
        rows.push(SettlementProbabilityEdgeBucketRow {
            model: model.to_string(),
            edge_bucket: format!("Q{}", bucket_idx + 1),
            count: bucket_samples.len(),
            avg_edge: mean(bucket_samples.iter().map(|sample| sample.edge)),
            avg_predicted_q: mean(bucket_samples.iter().map(|sample| sample.q)),
            actual_win_rate: mean(bucket_samples.iter().map(|sample| sample.win)),
            avg_full_depth_entry_price: mean(
                bucket_samples.iter().map(|sample| sample.entry_price),
            ),
            avg_full_depth_settlement_pnl: mean(bucket_samples.iter().map(|sample| sample.pnl)),
            avg_conservative_settlement_pnl: mean(
                bucket_samples.iter().map(|sample| sample.conservative_pnl),
            ),
            profit_factor: profit_factor(bucket_samples.iter().map(|sample| sample.pnl)),
            conservative_profit_factor: profit_factor(
                bucket_samples.iter().map(|sample| sample.conservative_pnl),
            ),
        });
    }
    rows
}

fn build_probability_anti_overfit_rows(
    model: &str,
    samples: &[SettlementProbabilitySample],
    top_edge_quantile: f64,
) -> Vec<SettlementProbabilityAntiOverfitRow> {
    if samples.len() < 3 {
        return Vec::new();
    }
    let observed_rank_ic =
        probability_edge_win_rank_ic(samples.iter().map(|sample| (sample.edge, sample.win)));
    let observed_top_pnl = top_edge_avg_pnl(
        samples
            .iter()
            .enumerate()
            .map(|(idx, sample)| (idx, sample.edge, sample.pnl)),
        top_edge_quantile,
    );
    let half_shift = (samples.len() / 2).max(1);
    let label_shift_rank_ic =
        probability_edge_win_rank_ic(samples.iter().enumerate().map(|(idx, sample)| {
            let shifted = &samples[(idx + half_shift) % samples.len()];
            (sample.edge, shifted.win)
        }));
    let label_shift_top_pnl = top_edge_avg_pnl(
        samples.iter().enumerate().map(|(idx, sample)| {
            let shifted = &samples[(idx + half_shift) % samples.len()];
            (idx, sample.edge, shifted.pnl)
        }),
        top_edge_quantile,
    );
    let prediction_shift_rank_ic =
        probability_edge_win_rank_ic(samples.iter().enumerate().map(|(idx, sample)| {
            let shifted_prediction = &samples[(idx + 1) % samples.len()];
            (shifted_prediction.edge, sample.win)
        }));
    let prediction_shift_top_pnl = top_edge_avg_pnl(
        samples.iter().enumerate().map(|(idx, sample)| {
            let shifted_prediction = &samples[(idx + 1) % samples.len()];
            (idx, shifted_prediction.edge, sample.pnl)
        }),
        top_edge_quantile,
    );
    let permutation_stride = coprime_stride(samples.len());
    let permutation_offset = samples.len() / 3;
    let permutation_rank_ic =
        probability_edge_win_rank_ic(samples.iter().enumerate().map(|(idx, sample)| {
            let permuted =
                &samples[(idx * permutation_stride + permutation_offset) % samples.len()];
            (sample.edge, permuted.win)
        }));
    let permutation_top_pnl = top_edge_avg_pnl(
        samples.iter().enumerate().map(|(idx, sample)| {
            let permuted =
                &samples[(idx * permutation_stride + permutation_offset) % samples.len()];
            (idx, sample.edge, permuted.pnl)
        }),
        top_edge_quantile,
    );

    vec![
        SettlementProbabilityAntiOverfitRow {
            model: model.to_string(),
            test: "label_cyclic_shift_half".to_string(),
            n: samples.len(),
            observed_edge_win_rank_ic: observed_rank_ic,
            perturbed_edge_win_rank_ic: label_shift_rank_ic,
            observed_top_edge_avg_full_depth_settlement_pnl: observed_top_pnl,
            perturbed_top_edge_avg_full_depth_settlement_pnl: label_shift_top_pnl,
            pass: anti_overfit_pass(
                observed_rank_ic,
                label_shift_rank_ic,
                observed_top_pnl,
                label_shift_top_pnl,
            ),
        },
        SettlementProbabilityAntiOverfitRow {
            model: model.to_string(),
            test: "prediction_one_step_shift".to_string(),
            n: samples.len(),
            observed_edge_win_rank_ic: observed_rank_ic,
            perturbed_edge_win_rank_ic: prediction_shift_rank_ic,
            observed_top_edge_avg_full_depth_settlement_pnl: observed_top_pnl,
            perturbed_top_edge_avg_full_depth_settlement_pnl: prediction_shift_top_pnl,
            pass: anti_overfit_no_improvement_pass(
                observed_rank_ic,
                prediction_shift_rank_ic,
                observed_top_pnl,
                prediction_shift_top_pnl,
            ),
        },
        SettlementProbabilityAntiOverfitRow {
            model: model.to_string(),
            test: "label_deterministic_permutation".to_string(),
            n: samples.len(),
            observed_edge_win_rank_ic: observed_rank_ic,
            perturbed_edge_win_rank_ic: permutation_rank_ic,
            observed_top_edge_avg_full_depth_settlement_pnl: observed_top_pnl,
            perturbed_top_edge_avg_full_depth_settlement_pnl: permutation_top_pnl,
            pass: anti_overfit_pass(
                observed_rank_ic,
                permutation_rank_ic,
                observed_top_pnl,
                permutation_top_pnl,
            ),
        },
    ]
}

fn coprime_stride(len: usize) -> usize {
    let mut stride = (len / 3).max(2);
    while gcd_usize(stride, len) != 1 {
        stride += 1;
    }
    stride
}

fn gcd_usize(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

fn probability_edge_win_rank_ic<I>(rows: I) -> f64
where
    I: IntoIterator<Item = (f64, f64)>,
{
    let pairs: Vec<(f64, f64)> = rows
        .into_iter()
        .filter_map(|(edge, win)| {
            if edge.is_finite() && win.is_finite() {
                Some((edge, win))
            } else {
                None
            }
        })
        .collect();
    pair_spearman(&pairs)
}

fn top_edge_avg_pnl<I>(rows: I, top_edge_quantile: f64) -> f64
where
    I: IntoIterator<Item = (usize, f64, f64)>,
{
    let mut scored: Vec<(usize, f64, f64)> = rows
        .into_iter()
        .filter(|(_, edge, pnl)| edge.is_finite() && pnl.is_finite())
        .collect();
    if scored.is_empty() {
        return f64::NAN;
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let top_n = ((scored.len() as f64) * top_edge_quantile.clamp(0.01, 1.0))
        .ceil()
        .max(1.0) as usize;
    mean(scored.into_iter().take(top_n).map(|(_, _, pnl)| pnl))
}

fn anti_overfit_pass(
    observed_rank_ic: f64,
    perturbed_rank_ic: f64,
    observed_top_pnl: f64,
    perturbed_top_pnl: f64,
) -> bool {
    let ic_ok = !observed_rank_ic.is_finite()
        || !perturbed_rank_ic.is_finite()
        || perturbed_rank_ic.abs() <= (observed_rank_ic.abs() * 0.75).max(0.05);
    let pnl_ok = !observed_top_pnl.is_finite()
        || !perturbed_top_pnl.is_finite()
        || observed_top_pnl <= 0.0
        || perturbed_top_pnl <= observed_top_pnl * 0.5;
    ic_ok && pnl_ok
}

fn anti_overfit_no_improvement_pass(
    observed_rank_ic: f64,
    perturbed_rank_ic: f64,
    observed_top_pnl: f64,
    perturbed_top_pnl: f64,
) -> bool {
    let eps = 1e-9;
    let ic_ok = !observed_rank_ic.is_finite()
        || !perturbed_rank_ic.is_finite()
        || perturbed_rank_ic.abs() <= observed_rank_ic.abs() + eps;
    let pnl_ok = !observed_top_pnl.is_finite()
        || !perturbed_top_pnl.is_finite()
        || perturbed_top_pnl <= observed_top_pnl + eps;
    ic_ok && pnl_ok
}

fn build_probability_symbol_holdout_rows(
    model: &str,
    samples: &[SettlementProbabilitySample],
    top_edge_quantile: f64,
    min_observations: usize,
) -> Vec<SettlementProbabilitySymbolHoldoutRow> {
    let mut by_symbol: BTreeMap<&str, Vec<&SettlementProbabilitySample>> = BTreeMap::new();
    for sample in samples {
        by_symbol
            .entry(sample.symbol.as_str())
            .or_default()
            .push(sample);
    }

    let mut rows = Vec::new();
    for (symbol, symbol_samples) in by_symbol {
        if symbol_samples.len() < min_observations {
            continue;
        }
        let edge_win_rank_ic = probability_edge_win_rank_ic(
            symbol_samples
                .iter()
                .map(|sample| (sample.edge, sample.win)),
        );
        let top_edge_avg_pnl = top_edge_avg_pnl(
            symbol_samples
                .iter()
                .enumerate()
                .map(|(idx, sample)| (idx, sample.edge, sample.pnl)),
            top_edge_quantile,
        );
        rows.push(SettlementProbabilitySymbolHoldoutRow {
            model: model.to_string(),
            symbol: symbol.to_string(),
            n: symbol_samples.len(),
            edge_win_rank_ic,
            top_edge_avg_full_depth_settlement_pnl: top_edge_avg_pnl,
            pass: edge_win_rank_ic.is_finite()
                && edge_win_rank_ic >= 0.0
                && top_edge_avg_pnl.is_finite()
                && top_edge_avg_pnl > 0.0,
        });
    }
    rows.sort_by(|a, b| a.model.cmp(&b.model).then_with(|| a.symbol.cmp(&b.symbol)));
    rows
}

fn build_settlement_probability_ablation_rows(
    baselines: &[SettlementProbabilityBaselineRow],
) -> Vec<SettlementProbabilityAblationRow> {
    let by_model: BTreeMap<&str, &SettlementProbabilityBaselineRow> = baselines
        .iter()
        .map(|row| (row.model.as_str(), row))
        .collect();
    let mut rows = Vec::new();
    for reference_model in ["q_base_distance_phi", "q_market_midpoint"] {
        let Some(reference) = by_model.get(reference_model).copied() else {
            continue;
        };
        for candidate in baselines {
            if candidate.model == reference.model
                || candidate.n != reference.n
                || !is_ablation_candidate(&candidate.model, reference_model)
            {
                continue;
            }
            rows.push(SettlementProbabilityAblationRow {
                model: candidate.model.clone(),
                reference_model: reference.model.clone(),
                n: candidate.n,
                delta_brier_score: candidate.brier_score - reference.brier_score,
                delta_log_loss: candidate.log_loss - reference.log_loss,
                delta_expected_calibration_error: candidate.expected_calibration_error
                    - reference.expected_calibration_error,
                delta_top_edge_avg_full_depth_settlement_pnl: candidate
                    .top_edge_avg_full_depth_settlement_pnl
                    - reference.top_edge_avg_full_depth_settlement_pnl,
                improves_error: candidate.brier_score <= reference.brier_score
                    && candidate.log_loss <= reference.log_loss,
                improves_top_edge_pnl: candidate.top_edge_avg_full_depth_settlement_pnl
                    > reference.top_edge_avg_full_depth_settlement_pnl,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.reference_model
            .cmp(&b.reference_model)
            .then_with(|| a.model.cmp(&b.model))
    });
    rows
}

fn is_ablation_candidate(model: &str, reference_model: &str) -> bool {
    match reference_model {
        "q_base_distance_phi" => matches!(
            model,
            "q_distance_lob_drift_phi" | "q_distance_vol_adjusted_phi" | "q_distance_lob_vol_phi"
        ),
        "q_market_midpoint" => model != "q_naive_50_50",
        _ => false,
    }
}

fn edge_bucket_monotonic(edge_buckets: &[SettlementProbabilityEdgeBucketRow]) -> bool {
    let values = edge_buckets
        .iter()
        .map(|row| row.avg_full_depth_settlement_pnl)
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.len() < 2 {
        return false;
    }
    values.windows(2).all(|pair| pair[1] + EPS >= pair[0])
}

fn expected_calibration_error(
    calibration: &[SettlementProbabilityCalibrationRow],
    total_count: usize,
) -> f64 {
    if total_count == 0 {
        return f64::NAN;
    }
    calibration
        .iter()
        .map(|row| row.calibration_error * row.count as f64 / total_count as f64)
        .sum()
}

fn probability_bucket_index(q: f64, bucket_count: usize) -> usize {
    let q = clamp_probability(q);
    ((q * bucket_count as f64).floor() as usize).min(bucket_count.saturating_sub(1))
}

fn probability_bucket_label(idx: usize, bucket_count: usize) -> String {
    let lo = idx as f64 / bucket_count as f64;
    let hi = (idx + 1) as f64 / bucket_count as f64;
    format!("{lo:.1}..{hi:.1}")
}

fn valid_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn clamp_probability(value: f64) -> f64 {
    value.clamp(1e-6, 1.0 - 1e-6)
}

fn normal_cdf(x: f64) -> f64 {
    // Abramowitz-Stegun style approximation, sufficient for bucketed research
    // diagnostics without introducing a stats dependency.
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * z);
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let erf = sign * (1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-z * z).exp());
    0.5 * (1.0 + erf)
}

fn matrix_distance_bucket(value: f64) -> String {
    bucket_value(value, &[-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0])
        .unwrap_or_else(|| "unknown".to_string())
}

fn matrix_spread_bucket(value: f64) -> String {
    finite_bucket(
        value,
        &[
            (100.0, "<100bps"),
            (300.0, "100..300bps"),
            (600.0, "300..600bps"),
            (1_000.0, "600..1000bps"),
        ],
        ">=1000bps",
    )
    .unwrap_or_else(|| "unknown".to_string())
}

fn side_row(
    row: &FactorObservation,
    side: ReviewSide,
    stake_usd: f64,
    pm_book: Option<&ResearchPmBookSnapshot>,
) -> FactorObservationV2 {
    let (
        side_model_prob,
        side_fair_prob,
        side_model_edge,
        side_distance_over_sigma,
        entry_ask,
        exit_bid,
        entry_ask_size,
        exit_bid_size,
        opposite_ask,
        opposite_bid,
        settlement_win,
    ) = match side {
        ReviewSide::Up => (
            row.model_prob_up,
            row.fair_prob_up_clean,
            row.model_edge_up,
            row.distance_over_sigma,
            row.pm_up_ask,
            row.pm_up_bid,
            row.pm_up_ask_size,
            row.pm_up_bid_size,
            row.pm_down_ask,
            row.pm_down_bid,
            row.settlement_up,
        ),
        ReviewSide::Down => {
            let model_prob_down = if row.model_prob_up.is_finite() {
                1.0 - row.model_prob_up
            } else {
                f64::NAN
            };
            let fair_prob_down = if row.fair_prob_up_clean.is_finite() {
                1.0 - row.fair_prob_up_clean
            } else {
                f64::NAN
            };
            let edge_down = if valid_price(row.pm_down_ask) {
                model_prob_down - row.pm_down_ask - crypto_fee_cost(row.pm_down_ask)
            } else {
                f64::NAN
            };
            (
                model_prob_down,
                fair_prob_down,
                edge_down,
                -row.distance_over_sigma,
                row.pm_down_ask,
                row.pm_down_bid,
                row.pm_down_ask_size,
                row.pm_down_bid_size,
                row.pm_up_ask,
                row.pm_up_bid,
                1.0 - row.settlement_up,
            )
        }
    };
    let side_chainlink_prob = match side {
        ReviewSide::Up => row.chainlink_prob_up,
        ReviewSide::Down if row.chainlink_prob_up.is_finite() => 1.0 - row.chainlink_prob_up,
        ReviewSide::Down => f64::NAN,
    };

    let entry_shares = if valid_price(entry_ask) {
        stake_usd / entry_ask
    } else {
        f64::NAN
    };
    let entry_fee_usd = if entry_shares.is_finite() {
        ploy_market_contracts::polymarket_crypto_taker_fee_cost(entry_shares, entry_ask)
    } else {
        f64::NAN
    };
    let entry_fillable = entry_shares.is_finite()
        && entry_ask_size.is_finite()
        && entry_ask_size + EPS >= entry_shares;
    let exit_fillable = entry_shares.is_finite()
        && exit_bid_size.is_finite()
        && exit_bid_size + EPS >= entry_shares;
    let entry_capacity_ratio = if entry_shares.is_finite() && entry_shares > 0.0 {
        entry_ask_size / entry_shares
    } else {
        f64::NAN
    };
    let exit_capacity_ratio = if entry_shares.is_finite() && entry_shares > 0.0 {
        exit_bid_size / entry_shares
    } else {
        f64::NAN
    };
    let entry_liquidity_usd = if entry_ask_size.is_finite() && valid_price(entry_ask) {
        entry_ask_size * entry_ask
    } else {
        f64::NAN
    };
    let exit_liquidity_usd = if exit_bid_size.is_finite() && valid_price(exit_bid) {
        exit_bid_size * exit_bid
    } else {
        f64::NAN
    };
    let liquidity_shortfall_usd = if entry_shares.is_finite() && entry_ask_size.is_finite() {
        ((entry_shares - entry_ask_size).max(0.0)) * entry_ask
    } else {
        f64::NAN
    };
    let entry_sweep = pm_book
        .map(|book| sweep_buy_to_stake(&book.asks, entry_ask, stake_usd))
        .unwrap_or_default();
    let conservative_entry_sweep = pm_book
        .map(|book| {
            sweep_buy_to_stake_with_config(
                &book.asks,
                entry_ask,
                stake_usd,
                CONSERVATIVE_VISIBLE_DEPTH_HAIRCUT,
                Some(CONSERVATIVE_MAX_SWEEP_LEVELS),
            )
        })
        .unwrap_or_default();
    let exit_sweep = if entry_sweep.fillable {
        pm_book
            .map(|book| sweep_sell_shares(&book.bids, exit_bid, entry_sweep.shares))
            .unwrap_or_default()
    } else {
        SweepFill::default()
    };
    let slippage_to_fill_15u_bps = if entry_sweep.fillable {
        entry_sweep.slippage_bps
    } else if entry_fillable {
        0.0
    } else {
        f64::NAN
    };
    let roundtrip_pnl_now_15u = if entry_fillable && exit_fillable && valid_price(exit_bid) {
        let exit_fee_usd =
            ploy_market_contracts::polymarket_crypto_taker_fee_cost(entry_shares, exit_bid);
        Some(entry_shares * exit_bid - stake_usd - entry_fee_usd - exit_fee_usd)
    } else {
        None
    };
    let full_depth_entry_fee_usd = if entry_sweep.fillable && entry_sweep.fee_usd.is_finite() {
        entry_sweep.fee_usd
    } else {
        f64::NAN
    };
    let roundtrip_pnl_now_full_depth_15u =
        if entry_sweep.fillable && exit_sweep.fillable && full_depth_entry_fee_usd.is_finite() {
            Some(
                entry_sweep.shares * exit_sweep.avg_price
                    - stake_usd
                    - full_depth_entry_fee_usd
                    - exit_sweep.fee_usd,
            )
        } else {
            None
        };
    let roundtrip_cost_usd = if let Some(pnl) = roundtrip_pnl_now_15u {
        -pnl
    } else if valid_price(entry_ask) && valid_price(exit_bid) && entry_shares.is_finite() {
        let exit_fee_usd =
            ploy_market_contracts::polymarket_crypto_taker_fee_cost(entry_shares, exit_bid);
        (stake_usd - entry_shares * exit_bid + entry_fee_usd + exit_fee_usd).max(0.0)
    } else {
        f64::NAN
    };
    let executable_pnl = if entry_fillable && settlement_win.is_finite() {
        Some(if settlement_win >= 0.5 {
            stake_usd * (1.0 / entry_ask - 1.0) - entry_fee_usd
        } else {
            -stake_usd - entry_fee_usd
        })
    } else {
        None
    };
    let full_depth_executable_pnl = if entry_sweep.fillable
        && settlement_win.is_finite()
        && full_depth_entry_fee_usd.is_finite()
    {
        Some(if settlement_win >= 0.5 {
            entry_sweep.shares - stake_usd - full_depth_entry_fee_usd
        } else {
            -stake_usd - full_depth_entry_fee_usd
        })
    } else {
        None
    };
    let conservative_entry_fee_usd =
        if conservative_entry_sweep.fillable && conservative_entry_sweep.fee_usd.is_finite() {
            conservative_entry_sweep.fee_usd
        } else {
            f64::NAN
        };
    let conservative_executable_pnl = if conservative_entry_sweep.fillable
        && settlement_win.is_finite()
        && conservative_entry_fee_usd.is_finite()
    {
        Some(if settlement_win >= 0.5 {
            conservative_entry_sweep.shares - stake_usd - conservative_entry_fee_usd
        } else {
            -stake_usd - conservative_entry_fee_usd
        })
    } else {
        None
    };
    let up_down_ask_sum = if valid_price(row.pm_up_ask) && valid_price(row.pm_down_ask) {
        row.pm_up_ask + row.pm_down_ask
    } else {
        f64::NAN
    };
    let pm_spread_bps = if valid_price(entry_ask) && valid_price(exit_bid) {
        ((entry_ask - exit_bid).max(0.0) / entry_ask) * 10_000.0
    } else {
        f64::NAN
    };
    let side_mult = side.multiplier();
    let cex_consecutive_bar_side = match side {
        ReviewSide::Up => row.cex_consecutive_up_bars,
        ReviewSide::Down => row.cex_consecutive_down_bars,
    };
    let cex_breakout_volume_side = row.cex_breakout_volume_score * side_mult;
    let cex_continuation_score_side = continuation_score(
        row.cex_bar_return_30s * side_mult,
        row.cex_bar_return_60s * side_mult,
        row.cex_signed_volume_ratio_30s * side_mult,
        row.cex_bar_volume_ratio_30s,
        row.cex_bar_volume_trend_3,
        cex_consecutive_bar_side,
    );
    let cex_continuation_edge_gate =
        if side_model_edge.is_finite() && cex_continuation_score_side.is_finite() {
            side_model_edge * cex_continuation_score_side.max(0.0)
        } else {
            f64::NAN
        };
    let executable_capacity_gate = entry_capacity_ratio.min(exit_capacity_ratio).min(2.0);
    let cex_continuation_liquidity_gate =
        if cex_continuation_score_side.is_finite() && executable_capacity_gate.is_finite() {
            cex_continuation_score_side * executable_capacity_gate.max(0.0)
        } else {
            f64::NAN
        };

    FactorObservationV2 {
        event_id: row.event_id.clone(),
        symbol: row.symbol.clone(),
        tick_ts: row.tick_ts,
        time_remaining_secs: row.time_remaining_secs,
        regime: Regime::from_secs(row.time_remaining_secs),
        side,
        pm_token_id: side_token_id(row, side).to_string(),
        side_model_prob,
        side_chainlink_prob,
        side_fair_prob,
        side_model_edge,
        side_distance_over_sigma,
        abs_distance_to_beat: row.abs_distance_to_beat,
        drift_10s: row.drift_10s,
        drift_30s: row.drift_30s,
        post_flip_drift: row.post_flip_drift,
        sigma_horizon: row.sigma_horizon,
        vol_gap: row.vol_gap,
        obi_10: row.obi_10,
        depth_imbalance: row.depth_imbalance,
        depth_acceleration: row.depth_acceleration,
        microprice_offset_bps: row.microprice_offset_bps,
        cex_spread_bps: row.spread_bps,
        cum_mprice_drift_5m: row.cum_mprice_drift_5m,
        cum_trade_imbalance_5m: row.cum_trade_imbalance_5m,
        obi_delta_10s_side: f64::NAN,
        obi_delta_30s_side: f64::NAN,
        obi_persistence_30s_side: f64::NAN,
        obi_flip_count_60s: f64::NAN,
        depth_imbalance_delta_30s_side: f64::NAN,
        microprice_momentum_30s_side: f64::NAN,
        trade_imbalance_delta_10s_side: f64::NAN,
        trade_imbalance_delta_30s_side: f64::NAN,
        cex_bar_return_30s: row.cex_bar_return_30s,
        cex_bar_return_60s: row.cex_bar_return_60s,
        cex_bar_volume_ratio_30s: row.cex_bar_volume_ratio_30s,
        cex_bar_volume_trend_3: row.cex_bar_volume_trend_3,
        cex_signed_volume_ratio_30s: row.cex_signed_volume_ratio_30s,
        cex_consecutive_bar_side,
        cex_breakout_volume_side,
        cex_continuation_score_side,
        cex_continuation_edge_gate,
        cex_continuation_liquidity_gate,
        entry_ask,
        exit_bid,
        entry_ask_size,
        exit_bid_size,
        opposite_ask,
        opposite_bid,
        up_down_ask_sum,
        pm_spread_bps,
        pm_lag_secs: row.pm_lag_secs,
        entry_ask_change_10s: f64::NAN,
        entry_ask_change_30s: f64::NAN,
        exit_bid_change_30s: f64::NAN,
        pm_spread_change_30s: f64::NAN,
        entry_size_change_30s: f64::NAN,
        up_down_ask_sum_change_30s: f64::NAN,
        pm_reprice_speed_30s: f64::NAN,
        pm_quote_stability_30s: f64::NAN,
        deribit_mark_iv: f64::NAN,
        deribit_bid_iv: f64::NAN,
        deribit_ask_iv: f64::NAN,
        deribit_iv_spread: f64::NAN,
        deribit_iv_lag_secs: f64::NAN,
        deribit_iv_horizon: f64::NAN,
        deribit_iv_gap_horizon: f64::NAN,
        deribit_iv_change_30s: f64::NAN,
        deribit_iv_change_60s: f64::NAN,
        deribit_underlying_basis_bps: f64::NAN,
        deribit_delta: f64::NAN,
        deribit_gamma: f64::NAN,
        deribit_vega: f64::NAN,
        deribit_theta: f64::NAN,
        stake_usd,
        entry_shares,
        entry_fee_usd,
        entry_capacity_ratio,
        exit_capacity_ratio,
        entry_liquidity_usd,
        exit_liquidity_usd,
        liquidity_shortfall_usd,
        slippage_to_fill_15u_bps,
        entry_sweep_avg_price_15u: entry_sweep.avg_price,
        exit_sweep_avg_price_15u: exit_sweep.avg_price,
        entry_sweep_shares_15u: entry_sweep.shares,
        entry_sweep_fee_usd_15u: entry_sweep.fee_usd,
        exit_sweep_shares_15u: exit_sweep.shares,
        entry_sweep_levels_15u: entry_sweep.levels_used,
        exit_sweep_levels_15u: exit_sweep.levels_used,
        entry_sweep_slippage_bps: entry_sweep.slippage_bps,
        exit_sweep_slippage_bps: exit_sweep.slippage_bps,
        conservative_entry_sweep_avg_price_15u: conservative_entry_sweep.avg_price,
        conservative_entry_sweep_shares_15u: conservative_entry_sweep.shares,
        conservative_entry_sweep_levels_15u: conservative_entry_sweep.levels_used,
        conservative_entry_sweep_slippage_bps: conservative_entry_sweep.slippage_bps,
        roundtrip_cost_usd,
        roundtrip_pnl_now_15u,
        roundtrip_pnl_now_full_depth_15u,
        portfolio_stake_usd: stake_usd,
        portfolio_event_exposure_usd: stake_usd,
        same_event_observation_count: f64::NAN,
        same_event_side_observation_count: f64::NAN,
        side_is_up: bool_num(side == ReviewSide::Up),
        label_settlement_win: settlement_win.is_finite().then_some(settlement_win),
        label_executable_pnl_15u: executable_pnl,
        label_full_depth_executable_pnl_15u: full_depth_executable_pnl,
        label_conservative_executable_pnl_15u: conservative_executable_pnl,
        label_executable_fillable: entry_fillable,
        label_exit_fillable: exit_fillable,
        label_full_depth_entry_fillable: entry_sweep.fillable,
        label_full_depth_exit_fillable: exit_sweep.fillable,
        label_conservative_entry_fillable: conservative_entry_sweep.fillable,
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

fn enrich_rolling_features(
    rows: &mut [FactorObservationV2],
    deribit: &[DeribitFeatureSnapshot],
    pm_book_index: Option<&PmBookIndex<'_>>,
    max_quote_age_secs: i64,
) {
    rows.sort_by_key(|row| {
        (
            row.event_id.clone(),
            row.side.as_str().to_string(),
            row.tick_ts,
        )
    });

    let mut deribit_by_symbol: HashMap<String, Vec<&DeribitFeatureSnapshot>> = HashMap::new();
    for snapshot in deribit {
        deribit_by_symbol
            .entry(normalize_symbol(&snapshot.symbol))
            .or_default()
            .push(snapshot);
    }
    for snapshots in deribit_by_symbol.values_mut() {
        snapshots.sort_by_key(|snapshot| snapshot.ts);
    }

    let mut event_counts: HashMap<String, usize> = HashMap::new();
    let mut event_side_counts: HashMap<(String, &'static str), usize> = HashMap::new();
    let mut groups: BTreeMap<(String, &'static str), Vec<usize>> = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        groups
            .entry((row.event_id.clone(), row.side.as_str()))
            .or_default()
            .push(idx);
    }

    for indexes in groups.values_mut() {
        indexes.sort_by_key(|idx| rows[*idx].tick_ts);
        for pos in 0..indexes.len() {
            let idx = indexes[pos];
            let ts = rows[idx].tick_ts;
            let event_count = event_counts
                .entry(rows[idx].event_id.clone())
                .and_modify(|count| *count += 1)
                .or_insert(1);
            rows[idx].same_event_observation_count = *event_count as f64;

            let event_side_count = event_side_counts
                .entry((rows[idx].event_id.clone(), rows[idx].side.as_str()))
                .and_modify(|count| *count += 1)
                .or_insert(1);
            rows[idx].same_event_side_observation_count = *event_side_count as f64;

            if let Some(prev_idx) =
                previous_idx_at_or_before(rows, indexes, pos, ts - chrono::Duration::seconds(10))
            {
                let prev = rows[prev_idx].clone();
                rows[idx].entry_ask_change_10s = diff(rows[idx].entry_ask, prev.entry_ask);
                rows[idx].obi_delta_10s_side =
                    diff(rows[idx].obi_10, prev.obi_10) * rows[idx].side.multiplier();
                rows[idx].trade_imbalance_delta_10s_side = diff(
                    rows[idx].cum_trade_imbalance_5m,
                    prev.cum_trade_imbalance_5m,
                ) * rows[idx].side.multiplier();
            }
            if let Some(prev_idx) =
                previous_idx_at_or_before(rows, indexes, pos, ts - chrono::Duration::seconds(30))
            {
                let prev = rows[prev_idx].clone();
                rows[idx].entry_ask_change_30s = diff(rows[idx].entry_ask, prev.entry_ask);
                rows[idx].exit_bid_change_30s = diff(rows[idx].exit_bid, prev.exit_bid);
                rows[idx].pm_spread_change_30s = diff(rows[idx].pm_spread_bps, prev.pm_spread_bps);
                rows[idx].entry_size_change_30s =
                    diff(rows[idx].entry_ask_size, prev.entry_ask_size);
                rows[idx].up_down_ask_sum_change_30s =
                    diff(rows[idx].up_down_ask_sum, prev.up_down_ask_sum);
                rows[idx].pm_reprice_speed_30s = rows[idx].entry_ask_change_30s / 30.0;
                rows[idx].obi_delta_30s_side =
                    diff(rows[idx].obi_10, prev.obi_10) * rows[idx].side.multiplier();
                rows[idx].depth_imbalance_delta_30s_side =
                    diff(rows[idx].depth_imbalance, prev.depth_imbalance)
                        * rows[idx].side.multiplier();
                rows[idx].microprice_momentum_30s_side =
                    diff(rows[idx].microprice_offset_bps, prev.microprice_offset_bps)
                        * rows[idx].side.multiplier();
                rows[idx].trade_imbalance_delta_30s_side = diff(
                    rows[idx].cum_trade_imbalance_5m,
                    prev.cum_trade_imbalance_5m,
                ) * rows[idx].side.multiplier();
            }
            for horizon_secs in [5, 10, 30, 60] {
                if let Some(future_idx) = future_idx_at_or_after(
                    rows,
                    indexes,
                    pos,
                    ts + chrono::Duration::seconds(horizon_secs),
                ) {
                    let future = rows[future_idx].clone();
                    let future_book = pm_book_index.and_then(|index| {
                        latest_pm_book(
                            index,
                            &future.event_id,
                            &future.pm_token_id,
                            future.side,
                            future.tick_ts,
                            max_quote_age_secs,
                        )
                    });
                    set_future_exit_labels(rows, idx, &future, future_book, horizon_secs);
                }
            }

            rows[idx].pm_quote_stability_30s = quote_stability(rows, indexes, pos, 30);
            rows[idx].obi_persistence_30s_side = side_persistence(rows, indexes, pos, 30, |row| {
                row.obi_10 * row.side.multiplier()
            });
            rows[idx].obi_flip_count_60s = flip_count(rows, indexes, pos, 60, |row| row.obi_10);

            if let Some(snapshot) =
                latest_deribit_snapshot(&deribit_by_symbol, &rows[idx].symbol, rows[idx].tick_ts)
            {
                apply_deribit_snapshot(&mut rows[idx], snapshot);
                if let Some(prev) = latest_deribit_before(
                    &deribit_by_symbol,
                    &rows[idx].symbol,
                    rows[idx].tick_ts - chrono::Duration::seconds(30),
                ) {
                    rows[idx].deribit_iv_change_30s =
                        diff(rows[idx].deribit_mark_iv, normalized_iv(prev.mark_iv));
                }
                if let Some(prev) = latest_deribit_before(
                    &deribit_by_symbol,
                    &rows[idx].symbol,
                    rows[idx].tick_ts - chrono::Duration::seconds(60),
                ) {
                    rows[idx].deribit_iv_change_60s =
                        diff(rows[idx].deribit_mark_iv, normalized_iv(prev.mark_iv));
                }
            }
        }
    }
}

fn set_future_exit_labels(
    rows: &mut [FactorObservationV2],
    idx: usize,
    future: &FactorObservationV2,
    future_book: Option<&ResearchPmBookSnapshot>,
    horizon_secs: i64,
) {
    let bid_change = finite_diff(future.exit_bid, rows[idx].exit_bid);
    let fillable = Some(bool_num(future.label_exit_fillable));
    let pnl = if rows[idx].entry_shares.is_finite() && valid_price(future.exit_bid) {
        let exit_fee = ploy_market_contracts::polymarket_crypto_taker_fee_cost(
            rows[idx].entry_shares,
            future.exit_bid,
        );
        Some(
            rows[idx].entry_shares * future.exit_bid
                - rows[idx].stake_usd
                - rows[idx].entry_fee_usd
                - exit_fee,
        )
    } else {
        None
    };
    let full_depth_exit = if rows[idx].label_full_depth_entry_fillable {
        future_book
            .map(|book| {
                sweep_sell_shares(
                    &book.bids,
                    future.exit_bid,
                    rows[idx].entry_sweep_shares_15u,
                )
            })
            .unwrap_or_default()
    } else {
        SweepFill::default()
    };
    let full_depth_value = full_depth_exit
        .fillable
        .then_some(full_depth_exit.shares * full_depth_exit.avg_price);
    let full_depth_pnl = full_depth_value.and_then(|value| {
        if rows[idx].stake_usd.is_finite() && rows[idx].entry_sweep_fee_usd_15u.is_finite() {
            Some(
                value
                    - rows[idx].stake_usd
                    - rows[idx].entry_sweep_fee_usd_15u
                    - full_depth_exit.fee_usd,
            )
        } else {
            None
        }
    });
    let full_depth_fillable = Some(bool_num(full_depth_exit.fillable));
    match horizon_secs {
        5 => {
            rows[idx].label_future_exit_bid_change_5s = bid_change;
            rows[idx].label_future_exit_fillable_5s = fillable;
            rows[idx].label_future_exit_pnl_5s = pnl;
            rows[idx].label_future_exit_full_depth_fillable_5s = full_depth_fillable;
            rows[idx].label_future_exit_full_depth_value_5s = full_depth_value;
            rows[idx].label_future_exit_full_depth_pnl_5s = full_depth_pnl;
        }
        10 => {
            rows[idx].label_future_exit_bid_change_10s = bid_change;
            rows[idx].label_future_exit_fillable_10s = fillable;
            rows[idx].label_future_exit_pnl_10s = pnl;
            rows[idx].label_future_exit_full_depth_fillable_10s = full_depth_fillable;
            rows[idx].label_future_exit_full_depth_value_10s = full_depth_value;
            rows[idx].label_future_exit_full_depth_pnl_10s = full_depth_pnl;
        }
        30 => {
            rows[idx].label_future_exit_bid_change_30s = bid_change;
            rows[idx].label_future_exit_fillable_30s = fillable;
            rows[idx].label_future_exit_pnl_30s = pnl;
            rows[idx].label_future_exit_full_depth_fillable_30s = full_depth_fillable;
            rows[idx].label_future_exit_full_depth_value_30s = full_depth_value;
            rows[idx].label_future_exit_full_depth_pnl_30s = full_depth_pnl;
        }
        60 => {
            rows[idx].label_future_exit_bid_change_60s = bid_change;
            rows[idx].label_future_exit_fillable_60s = fillable;
            rows[idx].label_future_exit_pnl_60s = pnl;
            rows[idx].label_future_exit_full_depth_fillable_60s = full_depth_fillable;
            rows[idx].label_future_exit_full_depth_value_60s = full_depth_value;
            rows[idx].label_future_exit_full_depth_pnl_60s = full_depth_pnl;
        }
        _ => {}
    }
}

fn previous_idx_at_or_before(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    target: DateTime<Utc>,
) -> Option<usize> {
    indexes[..pos]
        .iter()
        .rev()
        .copied()
        .find(|idx| rows[*idx].tick_ts <= target)
}

fn future_idx_at_or_after(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    target: DateTime<Utc>,
) -> Option<usize> {
    indexes[pos + 1..]
        .iter()
        .copied()
        .find(|idx| rows[*idx].tick_ts >= target)
}

fn quote_stability(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    window_secs: i64,
) -> f64 {
    let current = rows[indexes[pos]].entry_ask;
    if !valid_price(current) {
        return f64::NAN;
    }
    let cutoff = rows[indexes[pos]].tick_ts - chrono::Duration::seconds(window_secs);
    let mut total = 0usize;
    let mut stable = 0usize;
    for idx in indexes[..=pos].iter().rev() {
        let row = &rows[*idx];
        if row.tick_ts < cutoff {
            break;
        }
        if valid_price(row.entry_ask) {
            total += 1;
            if (row.entry_ask - current).abs() <= 0.01 {
                stable += 1;
            }
        }
    }
    ratio(stable, total)
}

fn side_persistence<F>(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    window_secs: i64,
    value_fn: F,
) -> f64
where
    F: Fn(&FactorObservationV2) -> f64,
{
    let cutoff = rows[indexes[pos]].tick_ts - chrono::Duration::seconds(window_secs);
    let mut total = 0usize;
    let mut favorable = 0usize;
    for idx in indexes[..=pos].iter().rev() {
        let row = &rows[*idx];
        if row.tick_ts < cutoff {
            break;
        }
        let value = value_fn(row);
        if value.is_finite() {
            total += 1;
            if value > 0.0 {
                favorable += 1;
            }
        }
    }
    ratio(favorable, total)
}

fn flip_count<F>(
    rows: &[FactorObservationV2],
    indexes: &[usize],
    pos: usize,
    window_secs: i64,
    value_fn: F,
) -> f64
where
    F: Fn(&FactorObservationV2) -> f64,
{
    let cutoff = rows[indexes[pos]].tick_ts - chrono::Duration::seconds(window_secs);
    let mut values: Vec<f64> = indexes[..=pos]
        .iter()
        .rev()
        .map(|idx| &rows[*idx])
        .take_while(|row| row.tick_ts >= cutoff)
        .filter_map(|row| {
            let value = signum(value_fn(row));
            (value != 0.0).then_some(value)
        })
        .collect();
    values.reverse();
    values
        .windows(2)
        .filter(|window| window[0] != window[1])
        .count() as f64
}

fn latest_deribit_snapshot<'a>(
    by_symbol: &'a HashMap<String, Vec<&'a DeribitFeatureSnapshot>>,
    symbol: &str,
    ts: DateTime<Utc>,
) -> Option<&'a DeribitFeatureSnapshot> {
    latest_deribit_before(by_symbol, symbol, ts)
}

fn latest_deribit_before<'a>(
    by_symbol: &'a HashMap<String, Vec<&'a DeribitFeatureSnapshot>>,
    symbol: &str,
    ts: DateTime<Utc>,
) -> Option<&'a DeribitFeatureSnapshot> {
    by_symbol
        .get(&normalize_symbol(symbol))?
        .iter()
        .rev()
        .copied()
        .find(|snapshot| snapshot.ts <= ts)
}

fn apply_deribit_snapshot(row: &mut FactorObservationV2, snapshot: &DeribitFeatureSnapshot) {
    let mark_iv = normalized_iv(snapshot.mark_iv);
    let bid_iv = normalized_iv(snapshot.bid_iv);
    let ask_iv = normalized_iv(snapshot.ask_iv);
    row.deribit_mark_iv = mark_iv;
    row.deribit_bid_iv = bid_iv;
    row.deribit_ask_iv = ask_iv;
    row.deribit_iv_spread = finite_diff(ask_iv, bid_iv).unwrap_or(f64::NAN);
    row.deribit_iv_lag_secs = (row.tick_ts - snapshot.ts).num_milliseconds() as f64 / 1000.0;
    row.deribit_iv_horizon = if mark_iv.is_finite() && row.time_remaining_secs > 0 {
        mark_iv * (row.time_remaining_secs as f64 / 31_536_000.0).sqrt()
    } else {
        f64::NAN
    };
    row.deribit_iv_gap_horizon = diff(row.deribit_iv_horizon, row.sigma_horizon);
    row.deribit_underlying_basis_bps = if snapshot.underlying_price.is_finite()
        && snapshot.underlying_price > 0.0
        && row.abs_distance_to_beat.is_finite()
    {
        // FactorObservation no longer carries spot level, so this is intentionally
        // left as a placeholder unless the Deribit snapshot carries a comparable
        // reference in a future schema.
        f64::NAN
    } else {
        f64::NAN
    };
    row.deribit_delta = snapshot.delta;
    row.deribit_gamma = snapshot.gamma;
    row.deribit_vega = snapshot.vega;
    row.deribit_theta = snapshot.theta;
}

fn normalize_symbol(symbol: &str) -> String {
    let upper = symbol.trim().to_ascii_uppercase();
    match upper.as_str() {
        "BTC" | "BTC-PERPETUAL" => "BTCUSDT".to_string(),
        "ETH" | "ETH-PERPETUAL" => "ETHUSDT".to_string(),
        "SOL" | "SOL-PERPETUAL" => "SOLUSDT".to_string(),
        other => other.to_string(),
    }
}

fn normalized_iv(value: f64) -> f64 {
    if !value.is_finite() {
        return f64::NAN;
    }
    if value > 2.0 {
        value / 100.0
    } else {
        value
    }
}

fn finite_diff(now: f64, before: f64) -> Option<f64> {
    (now.is_finite() && before.is_finite()).then_some(now - before)
}

fn diff(now: f64, before: f64) -> f64 {
    finite_diff(now, before).unwrap_or(f64::NAN)
}

fn continuation_score(
    return_30s_side: f64,
    return_60s_side: f64,
    signed_volume_side: f64,
    volume_ratio_30s: f64,
    volume_trend_3: f64,
    consecutive_bar_side: f64,
) -> f64 {
    let mut score = 0.0;
    let mut weight = 0.0;
    if return_30s_side.is_finite() {
        score += 3_000.0 * return_30s_side;
        weight += 1.0;
    }
    if return_60s_side.is_finite() {
        score += 1_500.0 * return_60s_side;
        weight += 1.0;
    }
    if signed_volume_side.is_finite() {
        score += signed_volume_side;
        weight += 1.0;
    }
    if volume_ratio_30s.is_finite() {
        score += (volume_ratio_30s - 1.0).clamp(-2.0, 4.0) * 0.5;
        weight += 1.0;
    }
    if volume_trend_3.is_finite() {
        score += volume_trend_3 * 0.5;
        weight += 1.0;
    }
    if consecutive_bar_side.is_finite() {
        score += consecutive_bar_side.min(4.0) * 0.25;
        weight += 1.0;
    }
    if weight > 0.0 {
        score / weight
    } else {
        f64::NAN
    }
}

#[derive(Clone, Copy)]
struct RepricingIcTargetDescriptor {
    name: &'static str,
    group: &'static str,
    accessor: fn(&FactorObservationV2) -> Option<f64>,
}

struct RepricingIcBucketSummary {
    n: usize,
    avg_label: f64,
    positive_label_rate: f64,
}

fn repricing_ic_targets() -> Vec<RepricingIcTargetDescriptor> {
    vec![
        repricing_target("reprice_pnl_5s", "reprice_pnl", |row| {
            row.label_future_exit_pnl_5s
        }),
        repricing_target("reprice_pnl_10s", "reprice_pnl", |row| {
            row.label_future_exit_pnl_10s
        }),
        repricing_target("reprice_pnl_30s", "reprice_pnl", |row| {
            row.label_future_exit_pnl_30s
        }),
        repricing_target("reprice_pnl_60s", "reprice_pnl", |row| {
            row.label_future_exit_pnl_60s
        }),
        repricing_target(
            "full_depth_reprice_pnl_5s",
            "full_depth_reprice_pnl",
            |row| row.label_future_exit_full_depth_pnl_5s,
        ),
        repricing_target(
            "full_depth_reprice_pnl_10s",
            "full_depth_reprice_pnl",
            |row| row.label_future_exit_full_depth_pnl_10s,
        ),
        repricing_target(
            "full_depth_reprice_pnl_30s",
            "full_depth_reprice_pnl",
            |row| row.label_future_exit_full_depth_pnl_30s,
        ),
        repricing_target(
            "full_depth_reprice_pnl_60s",
            "full_depth_reprice_pnl",
            |row| row.label_future_exit_full_depth_pnl_60s,
        ),
        repricing_target("reprice_bid_change_5s", "reprice_bid_change", |row| {
            row.label_future_exit_bid_change_5s
        }),
        repricing_target("reprice_bid_change_10s", "reprice_bid_change", |row| {
            row.label_future_exit_bid_change_10s
        }),
        repricing_target("reprice_bid_change_30s", "reprice_bid_change", |row| {
            row.label_future_exit_bid_change_30s
        }),
        repricing_target("reprice_bid_change_60s", "reprice_bid_change", |row| {
            row.label_future_exit_bid_change_60s
        }),
        repricing_target("abs_reprice_bid_change_5s", "volatility", |row| {
            row.label_future_exit_bid_change_5s.map(f64::abs)
        }),
        repricing_target("abs_reprice_bid_change_10s", "volatility", |row| {
            row.label_future_exit_bid_change_10s.map(f64::abs)
        }),
        repricing_target("abs_reprice_bid_change_30s", "volatility", |row| {
            row.label_future_exit_bid_change_30s.map(f64::abs)
        }),
        repricing_target("abs_reprice_bid_change_60s", "volatility", |row| {
            row.label_future_exit_bid_change_60s.map(f64::abs)
        }),
        repricing_target("settlement_executable_pnl", "settlement", executable_pnl),
        repricing_target("settlement_win", "settlement", |row| {
            row.label_settlement_win
        }),
        repricing_target("execution_entry_fillable", "execution", |row| {
            Some(bool_num(entry_fillable(row)))
        }),
        repricing_target("execution_exit_fillable", "execution", |row| {
            Some(bool_num(exit_fillable(row)))
        }),
        repricing_target("execution_future_exit_fillable_30s", "execution", |row| {
            row.label_future_exit_fillable_30s
        }),
        repricing_target(
            "execution_full_depth_future_exit_fillable_30s",
            "execution",
            |row| row.label_future_exit_full_depth_fillable_30s,
        ),
        repricing_target("execution_pm_spread_bps", "execution", |row| {
            row.pm_spread_bps.is_finite().then_some(row.pm_spread_bps)
        }),
        repricing_target("execution_pm_spread_change_30s", "execution", |row| {
            row.pm_spread_change_30s
                .is_finite()
                .then_some(row.pm_spread_change_30s)
        }),
    ]
}

fn repricing_target(
    name: &'static str,
    group: &'static str,
    accessor: fn(&FactorObservationV2) -> Option<f64>,
) -> RepricingIcTargetDescriptor {
    RepricingIcTargetDescriptor {
        name,
        group,
        accessor,
    }
}

fn build_repricing_ic_buckets(
    scored: &[(&FactorObservationV2, f64, f64)],
    bucket_count: usize,
) -> Vec<RepricingIcBucketSummary> {
    let mut sorted = scored.to_vec();
    sorted.sort_by(|a, b| a.1.total_cmp(&b.1));
    let bucket_count = bucket_count.clamp(2, sorted.len().max(2));
    let mut buckets = Vec::with_capacity(bucket_count);
    for bucket_idx in 0..bucket_count {
        let start = bucket_idx * sorted.len() / bucket_count;
        let end = ((bucket_idx + 1) * sorted.len() / bucket_count).max(start + 1);
        let slice = &sorted[start..end.min(sorted.len())];
        let positives = slice.iter().filter(|(_, _, label)| *label > 0.0).count();
        buckets.push(RepricingIcBucketSummary {
            n: slice.len(),
            avg_label: mean(slice.iter().map(|(_, _, label)| *label)),
            positive_label_rate: ratio(positives, slice.len()),
        });
    }
    buckets
}

fn repricing_ic_window_key(row: &FactorObservationV2) -> String {
    format!(
        "{}|{}|{}|{}",
        normalize_symbol(&row.symbol),
        row.regime.as_str(),
        distance_sigma_bucket(row),
        time_remaining_bin(row),
    )
}

fn distance_sigma_bucket(row: &FactorObservationV2) -> &'static str {
    let distance = row.side_distance_over_sigma.abs();
    if !distance.is_finite() {
        "distance_unknown"
    } else if distance < 0.5 {
        "distance_lt_0_5"
    } else if distance < 1.0 {
        "distance_0_5_1"
    } else if distance < 2.0 {
        "distance_1_2"
    } else {
        "distance_gte_2"
    }
}

fn repricing_factor_role(family: FactorFamily) -> &'static str {
    match family {
        FactorFamily::Execution
        | FactorFamily::PmLiquidity
        | FactorFamily::Exit
        | FactorFamily::PortfolioRisk => "execution_filter",
        FactorFamily::Alpha
        | FactorFamily::CexLob
        | FactorFamily::CexAggTrade
        | FactorFamily::PmDynamics
        | FactorFamily::DeribitVol
        | FactorFamily::Regime => "alpha_or_repricing",
    }
}

fn target_group_rank(group: &str) -> usize {
    match group {
        "reprice_pnl" => 0,
        "reprice_bid_change" => 1,
        "volatility" => 2,
        "settlement" => 3,
        "execution" => 4,
        _ => 5,
    }
}

fn format_bucket_avgs(values: &[f64]) -> String {
    values
        .iter()
        .map(|value| {
            if value.is_finite() {
                format!("{value:.4}")
            } else {
                "nan".to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn descriptor(
    name: &'static str,
    family: FactorFamily,
    layer: ThreeLayerArchive,
    accessor: fn(&FactorObservationV2) -> f64,
) -> FactorV2Descriptor {
    FactorV2Descriptor {
        name,
        family,
        layer,
        accessor,
    }
}

fn pair_pearson(pairs: &[(f64, f64)]) -> f64 {
    if pairs.len() < 2 {
        return f64::NAN;
    }
    let xs: Vec<f64> = pairs.iter().map(|(x, _)| *x).collect();
    let ys: Vec<f64> = pairs.iter().map(|(_, y)| *y).collect();
    pearson_ic(&xs, &ys)
}

fn pair_spearman(pairs: &[(f64, f64)]) -> f64 {
    if pairs.len() < 2 {
        return f64::NAN;
    }
    let xs: Vec<f64> = pairs.iter().map(|(x, _)| *x).collect();
    let ys: Vec<f64> = pairs.iter().map(|(_, y)| *y).collect();
    spearman_ic(&xs, &ys)
}

fn positive_group_ratio<F>(rows: &[&FactorObservationV2], key_fn: F) -> f64
where
    F: Fn(&FactorObservationV2) -> String,
{
    let mut groups: BTreeMap<String, f64> = BTreeMap::new();
    for row in rows {
        if let Some(pnl) = executable_pnl(row) {
            *groups.entry(key_fn(row)).or_default() += pnl;
        }
    }
    if groups.is_empty() {
        return f64::NAN;
    }
    let positive = groups.values().filter(|pnl| **pnl > 0.0).count();
    positive as f64 / groups.len() as f64
}

fn settlement_positive_group_ratio<F>(rows: &[&FactorObservationV2], key_fn: F) -> f64
where
    F: Fn(&FactorObservationV2) -> String,
{
    let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for row in rows {
        if let Some(label) = row.label_settlement_win.filter(|label| label.is_finite()) {
            let entry = groups.entry(key_fn(row)).or_default();
            entry.1 += 1;
            if label >= 0.5 {
                entry.0 += 1;
            }
        }
    }
    if groups.is_empty() {
        return f64::NAN;
    }
    let positive = groups
        .values()
        .filter(|(wins, total)| *total > 0 && (*wins as f64 / *total as f64) > 0.5)
        .count();
    positive as f64 / groups.len() as f64
}

fn trade_sharpe(pnls: &[f64]) -> f64 {
    if pnls.len() < 2 {
        return f64::NAN;
    }
    let avg = pnls.iter().sum::<f64>() / pnls.len() as f64;
    let var = pnls.iter().map(|pnl| (pnl - avg).powi(2)).sum::<f64>() / pnls.len() as f64;
    let std = var.sqrt();
    if std <= EPS {
        f64::NAN
    } else {
        avg / std * (pnls.len() as f64).sqrt()
    }
}

fn profit_factor(values: impl Iterator<Item = f64>) -> f64 {
    let mut gains = 0.0;
    let mut losses = 0.0;
    for value in values.filter(|value| value.is_finite()) {
        if value > 0.0 {
            gains += value;
        } else if value < 0.0 {
            losses += value.abs();
        }
    }
    if losses <= EPS {
        if gains > 0.0 {
            f64::INFINITY
        } else {
            f64::NAN
        }
    } else {
        gains / losses
    }
}

fn trade_t_stat(pnls: &[f64]) -> f64 {
    if pnls.len() < 2 {
        return f64::NAN;
    }
    let avg = pnls.iter().sum::<f64>() / pnls.len() as f64;
    let var = pnls.iter().map(|pnl| (pnl - avg).powi(2)).sum::<f64>() / pnls.len() as f64;
    let std = var.sqrt();
    if std <= EPS {
        f64::NAN
    } else {
        avg / (std / (pnls.len() as f64).sqrt())
    }
}

fn max_drawdown(pnls: &[(DateTime<Utc>, f64)]) -> f64 {
    let mut ordered = pnls.to_vec();
    ordered.sort_by_key(|(ts, _)| *ts);
    let mut equity = 0.0;
    let mut peak = 0.0;
    let mut max_dd = 0.0;
    for (_, pnl) in ordered {
        equity += pnl;
        if equity > peak {
            peak = equity;
        }
        let dd = peak - equity;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    max_dd
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let vals: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if vals.is_empty() {
        f64::NAN
    } else {
        vals.iter().sum::<f64>() / vals.len() as f64
    }
}

fn percentile(mut values: Vec<f64>, q: f64) -> f64 {
    values.retain(|value| value.is_finite());
    if values.is_empty() {
        return f64::NAN;
    }
    values.sort_by(f64::total_cmp);
    let idx = ((values.len() - 1) as f64 * q.clamp(0.0, 1.0)).round() as usize;
    values[idx]
}

fn finite_min(values: impl Iterator<Item = f64>) -> f64 {
    values
        .filter(|value| value.is_finite())
        .fold(None, |min: Option<f64>, value| {
            Some(min.map_or(value, |current| current.min(value)))
        })
        .unwrap_or(f64::NAN)
}

fn finite_max(values: impl Iterator<Item = f64>) -> f64 {
    values
        .filter(|value| value.is_finite())
        .fold(None, |max: Option<f64>, value| {
            Some(max.map_or(value, |current| current.max(value)))
        })
        .unwrap_or(f64::NAN)
}

fn stddev(values: &[f64]) -> f64 {
    let vals = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if vals.len() < 2 {
        return f64::NAN;
    }
    let avg = vals.iter().sum::<f64>() / vals.len() as f64;
    let var = vals.iter().map(|value| (value - avg).powi(2)).sum::<f64>() / vals.len() as f64;
    var.sqrt()
}

fn icir(values: &[f64]) -> f64 {
    let avg = mean(values.iter().copied());
    let std = stddev(values);
    if avg.is_finite() && std.is_finite() && std > EPS {
        avg / std
    } else {
        f64::NAN
    }
}

fn ratio(num: usize, denom: usize) -> f64 {
    if denom == 0 {
        f64::NAN
    } else {
        num as f64 / denom as f64
    }
}

fn valid_price(value: f64) -> bool {
    value.is_finite() && value > 0.0 && value < 1.0
}

fn signum(value: f64) -> f64 {
    if value > 1e-9 {
        1.0
    } else if value < -1e-9 {
        -1.0
    } else {
        0.0
    }
}

fn bool_num(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

fn crypto_fee_cost(entry_price: f64) -> f64 {
    ploy_market_contracts::polymarket_crypto_taker_fee_per_share(entry_price)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn prediction_shift_anti_overfit_passes_when_not_better() {
        assert!(anti_overfit_no_improvement_pass(0.18, 0.16, 2.12, 2.05));
        assert!(!anti_overfit_no_improvement_pass(0.18, 0.20, 2.12, 2.05));
        assert!(!anti_overfit_no_improvement_pass(0.18, 0.16, 2.12, 2.20));
    }

    #[test]
    fn label_perturbation_anti_overfit_still_requires_decay() {
        assert!(anti_overfit_pass(0.18, 0.03, 2.12, 0.90));
        assert!(!anti_overfit_pass(0.18, 0.16, 2.12, 2.05));
    }

    fn base_obs() -> FactorObservation {
        FactorObservation {
            event_id: "evt".into(),
            symbol: "BTCUSDT".into(),
            tick_ts: Utc::now(),
            up_token_id: "up-token".into(),
            down_token_id: "down-token".into(),
            chainlink_reference_fresh: false,
            binance_spot_fresh: false,
            binance_lob_fresh: false,
            binance_agg_trade_fresh: false,
            event_window_secs: 300,
            time_remaining_secs: 220,
            signed_distance_to_beat: 0.01,
            abs_distance_to_beat: 0.01,
            drift_10s: 0.001,
            drift_30s: 0.002,
            flip_age_secs: 10.0,
            post_flip_drift: 0.002,
            sigma_horizon: 0.01,
            fair_prob_up: 0.6,
            fair_prob_up_clean: 0.62,
            prob_disagreement: 0.0,
            implied_sigma_horizon: 0.01,
            vol_gap: 0.0,
            distance_over_sigma: 1.0,
            model_prob_up: 0.7,
            chainlink_prob_up: f64::NAN,
            model_edge_up: 0.2,
            reward_risk_up: 1.0,
            reward_risk_down: 1.0,
            obi: 0.1,
            spread_bps: 2.0,
            microprice_offset_bps: 0.5,
            bid_depth_near: 10.0,
            ask_depth_near: 8.0,
            depth_ratio: 1.25,
            depth_imbalance: 0.1,
            depth_far_ratio: 1.1,
            depth_acceleration: 0.05,
            obi_10: 0.2,
            pm_up_bid: 0.48,
            pm_up_ask: 0.50,
            pm_up_bid_size: 40.0,
            pm_up_ask_size: 40.0,
            pm_down_bid: 0.48,
            pm_down_ask: 0.50,
            pm_down_bid_size: 40.0,
            pm_down_ask_size: 40.0,
            pm_lag_secs: 1.0,
            settlement_up: 1.0,
            future_up_ask_change_30s: Some(0.05),
            future_up_ask_change_60s: Some(0.08),
            cum_obi_delta_5m: 0.1,
            cum_depth_delta_5m: 0.1,
            cum_mprice_drift_5m: 0.2,
            cum_trade_imbalance_5m: 1.0,
            cex_bar_return_30s: 0.002,
            cex_bar_return_60s: 0.003,
            cex_bar_volume_ratio_30s: 2.0,
            cex_bar_volume_trend_3: 1.0,
            cex_signed_volume_ratio_30s: 0.6,
            cex_consecutive_up_bars: 2.0,
            cex_consecutive_down_bars: 0.0,
            cex_breakout_volume_score: 1.2,
        }
    }

    fn make_settlement_probability_eligible(rows: &mut [FactorObservationV2]) {
        for row in rows {
            row.label_full_depth_entry_fillable = true;
            row.label_conservative_entry_fillable = true;
            row.entry_sweep_avg_price_15u = row.entry_ask;
            row.entry_sweep_shares_15u = 15.0 / row.entry_sweep_avg_price_15u;
            let pnl = row
                .label_settlement_win
                .map(|win| win * row.entry_sweep_shares_15u - 15.0);
            row.label_full_depth_executable_pnl_15u = pnl;
            row.label_conservative_executable_pnl_15u = pnl;
        }
    }

    fn minimal_health() -> DataHealthReport {
        DataHealthReport {
            source_observations: 0,
            v2_rows: 0,
            settlement_label_rows: 0,
            entry_quote_rows: 0,
            exit_quote_rows: 0,
            entry_size_rows: 0,
            exit_size_rows: 0,
            entry_fillable_rows: 0,
            exit_fillable_rows: 0,
            entry_full_depth_fillable_rows: 0,
            exit_full_depth_fillable_rows: 0,
            executable_pnl_rows: 0,
            full_depth_executable_pnl_rows: 0,
            deribit_rows: 0,
            avg_pm_lag_secs: f64::NAN,
            avg_entry_capacity_ratio: f64::NAN,
            avg_exit_capacity_ratio: f64::NAN,
            avg_entry_sweep_slippage_bps: f64::NAN,
            avg_exit_sweep_slippage_bps: f64::NAN,
        }
    }

    fn test_ev_bucket(
        dimension: &str,
        bucket: &str,
        avg_pnl_15u: f64,
        underpowered: bool,
        statistically_supported: bool,
    ) -> ExecutableEvBucketSummary {
        ExecutableEvBucketSummary {
            dimension: dimension.to_string(),
            bucket: bucket.to_string(),
            rows: 40,
            fillable_rows: 40,
            fill_rate: 1.0,
            pnl_rows: 40,
            total_pnl_15u: avg_pnl_15u * 40.0,
            avg_pnl_15u,
            roi_on_stake: avg_pnl_15u / 15.0,
            t_stat: if statistically_supported { 2.5 } else { 0.5 },
            underpowered,
            positive_ev: avg_pnl_15u > 0.0,
            statistically_supported,
            avg_side_model_prob: 0.7,
            avg_side_model_edge: 0.1,
            avg_entry_ask: 0.45,
            avg_exit_bid: 0.44,
            avg_pm_lag_secs: 1.0,
            avg_entry_capacity_ratio: 2.0,
            avg_entry_liquidity_usd: 100.0,
            avg_exit_liquidity_usd: 100.0,
            avg_liquidity_shortfall_usd: 0.0,
            avg_slippage_to_fill_bps: 10.0,
            avg_entry_sweep_slippage_bps: 10.0,
            avg_exit_sweep_slippage_bps: 10.0,
            avg_roundtrip_cost_usd: 0.1,
        }
    }

    #[test]
    fn executable_ev_bucket_helpers_use_stable_threshold_edges() {
        assert_eq!(probability_bucket(0.499), Some("<0.50".to_string()));
        assert_eq!(probability_bucket(0.50), Some("0.50..0.55".to_string()));
        assert_eq!(probability_bucket(0.70), Some(">=0.70".to_string()));
        assert_eq!(model_edge_bucket(0.02), Some("0.02..0.05".to_string()));
        assert_eq!(price_bucket(0.55), Some("0.55..0.65".to_string()));
        assert_eq!(pm_lag_bucket(30.0), Some(">=30s".to_string()));
        assert_eq!(capacity_bucket(1.0), Some("1.00..2.00x".to_string()));
        assert_eq!(liquidity_bucket(15.0), Some("15..50u".to_string()));
        assert_eq!(shortfall_bucket(0.0), Some("0u".to_string()));
        assert_eq!(slippage_bucket(100.0), Some("100..500bps".to_string()));
        assert_eq!(probability_bucket(f64::NAN), None);
    }

    #[test]
    fn executable_ev_bucket_report_sorts_and_excludes_underpowered_best_rows() {
        let report = FactorReviewV2Report {
            options: FactorReviewOptions::default(),
            health: minimal_health(),
            reviews: Vec::new(),
            executable_ev_buckets: vec![
                test_ev_bucket("direction_probability", "0.60..0.65", 0.5, false, false),
                test_ev_bucket("direction_probability", ">=0.70", 2.0, true, false),
                test_ev_bucket("entry_price", "0.35..0.45", 1.0, false, true),
                test_ev_bucket("entry_price", ">=0.65", -1.0, false, false),
            ],
            direction_side_audit: Vec::new(),
            binance_direction_audit: Vec::new(),
        };

        let best = sorted_executable_ev_buckets(&report, true);
        assert_eq!(best[0].bucket, "0.35..0.45");
        assert!(best.iter().all(|bucket| !bucket.underpowered));

        let worst = sorted_executable_ev_buckets(&report, false);
        assert_eq!(worst[0].bucket, ">=0.65");

        let text = format_factor_review_v2_report(&report, 10);
        assert!(text.contains("dimension,bucket,rows,fillable,fill_rate,pnl_rows,total_pnl"));
        let best_section = text
            .split("=== Executable EV Buckets: Best Non-Underpowered Avg PnL ===")
            .nth(1)
            .expect("best section");
        let best_section = best_section
            .split("=== Executable EV Buckets: Worst Avg PnL ===")
            .next()
            .expect("best rows");
        assert!(best_section.contains("entry_price,0.35..0.45"));
        assert!(!best_section.contains("direction_probability,>=0.70"));
    }

    #[test]
    fn v2_rows_are_side_aware_and_use_fixed_stake_liquidity() {
        let options = FactorReviewOptions::default();
        let rows = build_factor_observations_v2(&[base_obs()], &options);
        assert_eq!(rows.len(), 2);

        let up = rows.iter().find(|row| row.side == ReviewSide::Up).unwrap();
        assert!(up.label_executable_fillable);
        assert!(up.label_exit_fillable);
        assert_eq!(up.label_settlement_win, Some(1.0));
        assert!(up.label_executable_pnl_15u.unwrap() > 0.0);

        let down = rows
            .iter()
            .find(|row| row.side == ReviewSide::Down)
            .unwrap();
        assert_eq!(down.label_settlement_win, Some(0.0));
        assert!(down.label_executable_pnl_15u.unwrap() < 0.0);
    }

    #[test]
    fn settlement_probability_report_uses_full_depth_entry_population() {
        let options = FactorReviewOptions::default();
        let mut rows = build_factor_observations_v2(&[base_obs()], &options);
        make_settlement_probability_eligible(&mut rows);
        let base_rows = rows.clone();
        rows.extend(base_rows.iter().cloned());
        rows.extend(base_rows.iter().cloned());

        let report = build_settlement_probability_report(
            &rows,
            SettlementProbabilityReportOptions {
                bucket_count: 2,
                min_bucket_observations: 1,
                top_edge_quantile: 0.5,
                event_surface_min_bucket_observations: 1,
                event_surface_shrinkage_observations: 1,
            },
        );

        let base = report
            .baselines
            .iter()
            .find(|row| row.model == "q_base_distance_phi")
            .expect("q_base baseline");
        assert_eq!(base.n, 6);
        assert!(base.brier_score.is_finite());
        assert!(base.log_loss.is_finite());
        assert!(base.expected_calibration_error.is_finite());
        assert!(report
            .edge_buckets
            .iter()
            .any(|row| row.model == "q_base_distance_phi" && row.edge_bucket == "Q2"));
        assert!(
            report
                .anti_overfit
                .iter()
                .any(|row| row.model == "q_base_distance_phi"
                    && row.test == "label_cyclic_shift_half")
        );
        assert!(report.anti_overfit.iter().any(
            |row| row.model == "q_base_distance_phi" && row.test == "prediction_one_step_shift"
        ));
        assert!(report
            .anti_overfit
            .iter()
            .any(|row| row.model == "q_base_distance_phi"
                && row.test == "label_deterministic_permutation"));
        assert!(report
            .symbol_holdouts
            .iter()
            .any(|row| row.model == "q_base_distance_phi" && row.symbol == "BTCUSDT"));
        assert!(report
            .ablations
            .iter()
            .any(|row| row.reference_model == "q_base_distance_phi"));

        let text = format_settlement_probability_report(&report);
        assert!(text.contains("Settlement Probability Report"));
        assert!(text.contains("q_base_distance_phi"));
        assert!(text.contains("q_final_logit_blend"));
        assert!(text.contains("full_depth_settlement_pnl"));
        assert!(text.contains("Anti-Overfit Diagnostics"));
        assert!(text.contains("Symbol Holdout Diagnostics"));
        assert!(text.contains("Baseline Ablations"));
    }

    #[test]
    fn settlement_probability_event_surface_excludes_current_event() {
        let options = FactorReviewOptions::default();
        let source_rows = (0..3)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("evt-{idx}");
                obs.tick_ts = Utc::now() + Duration::seconds(idx);
                obs.settlement_up = if idx == 0 { 0.0 } else { 1.0 };
                obs.pm_up_ask = 0.50;
                obs.pm_up_bid = 0.48;
                obs.pm_down_ask = 0.50;
                obs.pm_down_bid = 0.48;
                obs.distance_over_sigma = 1.0;
                obs
            })
            .collect::<Vec<_>>();
        let mut rows = build_factor_observations_v2(&source_rows, &options);
        rows.retain(|row| row.side == ReviewSide::Up);
        make_settlement_probability_eligible(&mut rows);

        let report = build_settlement_probability_report(
            &rows,
            SettlementProbabilityReportOptions {
                bucket_count: 2,
                min_bucket_observations: 1,
                top_edge_quantile: 0.5,
                event_surface_min_bucket_observations: 1,
                event_surface_shrinkage_observations: 1,
            },
        );

        let event_surface = report
            .baselines
            .iter()
            .find(|row| row.model == "q_event_surface_empirical")
            .expect("event surface baseline");
        assert_eq!(event_surface.n, 3);
        assert!(event_surface.avg_predicted_q > 0.0);
        assert!(event_surface.avg_predicted_q < 1.0);
        assert!(event_surface.brier_score.is_finite());
        assert!(format_settlement_probability_report(&report).contains("q_event_surface_empirical"));
    }

    #[test]
    fn settlement_probability_final_blend_uses_market_distance_and_event_surface() {
        let options = FactorReviewOptions::default();
        let source_rows = (0..4)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("blend-evt-{idx}");
                obs.tick_ts = Utc::now() + Duration::seconds(idx);
                obs.settlement_up = if idx < 2 { 0.0 } else { 1.0 };
                obs.pm_up_bid = 0.18;
                obs.pm_up_ask = 0.22;
                obs.pm_down_bid = 0.76;
                obs.pm_down_ask = 0.80;
                obs.distance_over_sigma = 1.0;
                obs.obi_10 = 0.2;
                obs.depth_imbalance = 0.1;
                obs
            })
            .collect::<Vec<_>>();
        let mut rows = build_factor_observations_v2(&source_rows, &options);
        rows.retain(|row| row.side == ReviewSide::Up);
        make_settlement_probability_eligible(&mut rows);

        let event_surface = EventVolSurface::fit(
            &rows
                .iter()
                .map(|row| {
                    (
                        row,
                        row.label_settlement_win.unwrap(),
                        row.label_full_depth_executable_pnl_15u.unwrap(),
                        row.label_conservative_executable_pnl_15u,
                    )
                })
                .collect::<Vec<_>>(),
            1,
            1,
        );
        let first_row = &rows[0];
        let market_q = settlement_market_midpoint_probability(first_row).unwrap();
        let distance_q = settlement_distance_lob_vol_probability(first_row).unwrap();
        let event_q = event_surface.predict(first_row).unwrap();
        let final_q = settlement_probability_final_blend(first_row, Some(event_q)).unwrap();

        assert!(valid_probability(final_q));
        assert!((final_q - market_q).abs() > 1e-6);
        assert!((final_q - distance_q).abs() > 1e-6);
        assert!((final_q - event_q).abs() > 1e-6);

        let report = build_settlement_probability_report(
            &rows,
            SettlementProbabilityReportOptions {
                bucket_count: 2,
                min_bucket_observations: 1,
                top_edge_quantile: 0.5,
                event_surface_min_bucket_observations: 1,
                event_surface_shrinkage_observations: 1,
            },
        );
        assert!(report
            .baselines
            .iter()
            .any(|row| row.model == "q_final_logit_blend"));
    }

    #[test]
    fn settlement_probability_requires_complete_conservative_capacity_labels() {
        let source_rows = (0..4)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("conservative-{idx}");
                obs.tick_ts += Duration::seconds(idx);
                obs
            })
            .collect::<Vec<_>>();
        let mut rows = build_factor_observations_v2(
            &source_rows,
            &FactorReviewOptions {
                min_observations: 1,
                ..Default::default()
            },
        );
        rows.retain(|row| row.side == ReviewSide::Up);
        make_settlement_probability_eligible(&mut rows);
        rows[1].label_conservative_executable_pnl_15u = None;
        rows[2].label_conservative_executable_pnl_15u = Some(f64::NAN);
        rows[3].label_conservative_entry_fillable = false;

        let report = build_settlement_probability_report(
            &rows,
            SettlementProbabilityReportOptions {
                min_bucket_observations: 1,
                top_edge_quantile: 1.0,
                event_surface_min_bucket_observations: 1,
                event_surface_shrinkage_observations: 1,
                ..Default::default()
            },
        );

        assert!(!report.baselines.is_empty());
        assert!(report.baselines.iter().all(|row| row.n == 4));
        assert!(report
            .baselines
            .iter()
            .all(|row| row.avg_conservative_settlement_pnl.is_finite()));
        assert!(report
            .baselines
            .iter()
            .all(|row| (row.conservative_coverage_rate - 0.25).abs() < 1e-9));
        assert!(report
            .baselines
            .iter()
            .all(|row| (row.top_edge_conservative_coverage_rate - 0.25).abs() < 1e-9));
        assert!(best_conservative_edge_model(&report).is_none());

        for row in &mut rows {
            row.label_conservative_executable_pnl_15u = None;
        }
        let uncovered = build_settlement_probability_report(
            &rows,
            SettlementProbabilityReportOptions {
                min_bucket_observations: 1,
                top_edge_quantile: 1.0,
                ..Default::default()
            },
        );
        assert!(uncovered.baselines.iter().all(|row| row.n == 4));
        assert!(uncovered.baselines.iter().all(|row| {
            row.conservative_coverage_rate == 0.0 && row.avg_conservative_settlement_pnl == 0.0
        }));
        assert!(best_conservative_edge_model(&uncovered).is_none());
    }

    #[test]
    fn typed_prior_probability_blend_enters_probability_evaluator() {
        let options = FactorReviewOptions::default();
        let mut source_rows = (0..8)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("prior-blend-{idx}");
                obs.tick_ts = Utc::now() + Duration::hours(idx);
                obs.settlement_up = if idx % 2 == 0 { 1.0 } else { 0.0 };
                obs.pm_up_bid = 0.35 + idx as f64 * 0.02;
                obs.pm_up_ask = obs.pm_up_bid + 0.02;
                obs.distance_over_sigma = if idx % 2 == 0 { 0.8 } else { -0.6 };
                obs
            })
            .collect::<Vec<_>>();
        let mut sol = source_rows[0].clone();
        sol.event_id = "prior-blend-sol".to_string();
        sol.symbol = "SOLUSDT".to_string();
        sol.tick_ts += Duration::minutes(30);
        source_rows.push(sol);
        let mut rows = build_factor_observations_v2(&source_rows, &options);
        rows.retain(|row| row.side == ReviewSide::Up);
        make_settlement_probability_eligible(&mut rows);
        let prior: crate::autofactor::LlmPriorSpec = serde_json::from_value(serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "polymarket-btc-5m-v1",
            "data_snapshot_id": "sha256:btc-data",
            "prompt_snapshot_id": "sha256:prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["BTC"],
            "horizon": "5m",
            "probability_blends": [{
                "name": "microstructure",
                "hypothesis": "CEX-informed components improve BTC calibration.",
                "market_midpoint_weight": 0.4,
                "chainlink_digital_weight": 0.0,
                "distance_lob_vol_weight": 0.4,
                "event_surface_weight": 0.2,
                "existing_model_weight": 0.0
            }]
        }))
        .expect("typed prior");

        let report = build_settlement_probability_report_with_prior(
            &rows,
            Some(&prior),
            SettlementProbabilityReportOptions {
                bucket_count: 2,
                min_bucket_observations: 1,
                top_edge_quantile: 0.5,
                event_surface_min_bucket_observations: 1,
                event_surface_shrinkage_observations: 1,
            },
        );

        let candidate = report
            .baselines
            .iter()
            .find(|row| row.model == "q_llm_microstructure")
            .expect("prior blend should be evaluated as probability");
        assert!(candidate.brier_score.is_finite());
        assert!(candidate.log_loss.is_finite());
        assert!(candidate.expected_calibration_error.is_finite());
        assert_eq!(candidate.n, 8, "SOL rows must stay outside the BTC mission");
    }

    #[test]
    fn chainlink_only_typed_blend_preserves_binary_complements() {
        let mut observation = base_obs();
        observation.chainlink_prob_up = 0.73;
        let rows = build_factor_observations_v2(&[observation], &FactorReviewOptions::default());
        let blend = crate::autofactor::LlmProbabilityBlendSpec {
            name: "chainlink_only".to_string(),
            hypothesis: "Fresh Chainlink endpoint probability is the sole component.".to_string(),
            market_midpoint_weight: 0.0,
            chainlink_digital_weight: 1.0,
            distance_lob_vol_weight: 0.0,
            event_surface_weight: 0.0,
            existing_model_weight: 0.0,
        };

        for (side, expected) in [(ReviewSide::Up, 0.73), (ReviewSide::Down, 0.27)] {
            let row = rows
                .iter()
                .find(|row| row.side == side)
                .expect("both binary sides");
            let probability = settlement_probability_weighted_blend(row, None, &blend)
                .expect("valid Chainlink-only blend");
            assert!((probability - expected).abs() < 1e-12);
        }

        let mut missing_chainlink = rows[0].clone();
        missing_chainlink.side_chainlink_prob = f64::NAN;
        assert!(settlement_probability_weighted_blend(&missing_chainlink, None, &blend).is_none());

        let prior = crate::autofactor::LlmPriorSpec {
            target: Some("full_depth_settlement_executable_pnl".to_string()),
            mission_id: Some("polymarket-btc-5m-v1".to_string()),
            data_snapshot_id: Some("sha256:data".to_string()),
            prompt_snapshot_id: Some("sha256:prompt".to_string()),
            search_policy_snapshot_id: Some("sha256:policy".to_string()),
            symbols: vec!["BTC".to_string()],
            horizon: Some("5m".to_string()),
            probability_blends: vec![blend.clone()],
            ..Default::default()
        };
        let mut eligible_rows = rows.clone();
        make_settlement_probability_eligible(&mut eligible_rows);
        let report = build_settlement_probability_report_with_prior(
            &eligible_rows,
            Some(&prior),
            SettlementProbabilityReportOptions {
                min_bucket_observations: 1,
                top_edge_quantile: 1.0,
                ..Default::default()
            },
        );
        assert!(report
            .baselines
            .iter()
            .any(|row| row.model == "q_llm_chainlink_only"));
        assert!(report
            .baselines
            .iter()
            .any(|row| row.model == "q_chainlink_digital"));

        eligible_rows[0].side_chainlink_prob = f64::NAN;
        let incomplete = build_settlement_probability_report_with_prior(
            &eligible_rows,
            Some(&prior),
            SettlementProbabilityReportOptions {
                min_bucket_observations: 1,
                top_edge_quantile: 1.0,
                ..Default::default()
            },
        );
        assert!(incomplete
            .baselines
            .iter()
            .all(|row| row.model != "q_llm_chainlink_only"));
        assert!(incomplete
            .baselines
            .iter()
            .all(|row| row.model != "q_chainlink_digital"));

        let legacy: crate::autofactor::LlmProbabilityBlendSpec =
            serde_json::from_value(serde_json::json!({
                "name": "legacy",
                "hypothesis": "An old prior keeps its original blend.",
                "market_midpoint_weight": 1.0,
                "distance_lob_vol_weight": 0.0,
                "event_surface_weight": 0.0,
                "existing_model_weight": 0.0
            }))
            .expect("legacy blend");
        assert_eq!(legacy.chainlink_digital_weight, 0.0);
    }

    #[test]
    fn invalid_typed_probability_blends_fail_closed() {
        let options = FactorReviewOptions::default();
        let mut rows = build_factor_observations_v2(&[base_obs()], &options);
        make_settlement_probability_eligible(&mut rows);
        let prior: crate::autofactor::LlmPriorSpec = serde_json::from_value(serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "polymarket-btc-5m-invalid",
            "data_snapshot_id": "sha256:btc-data",
            "prompt_snapshot_id": "sha256:btc-prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["BTC"],
            "horizon": "5m",
            "probability_blends": [
                {
                    "name": "negative",
                    "hypothesis": "Negative weights must be rejected.",
                    "market_midpoint_weight": -1.0,
                    "chainlink_digital_weight": 0.0,
                    "distance_lob_vol_weight": 1.0,
                    "event_surface_weight": 0.0,
                    "existing_model_weight": 0.0
                },
                {
                    "name": "all_zero",
                    "hypothesis": "Zero total weight must be rejected.",
                    "market_midpoint_weight": 0.0,
                    "chainlink_digital_weight": 0.0,
                    "distance_lob_vol_weight": 0.0,
                    "event_surface_weight": 0.0,
                    "existing_model_weight": 0.0
                },
                {
                    "name": "overflow",
                    "hypothesis": "Overflowing total weight must be rejected.",
                    "market_midpoint_weight": 1e308,
                    "chainlink_digital_weight": 0.0,
                    "distance_lob_vol_weight": 1e308,
                    "event_surface_weight": 0.0,
                    "existing_model_weight": 0.0
                }
            ]
        }))
        .expect("typed prior");

        let report = build_settlement_probability_report_with_prior(
            &rows,
            Some(&prior),
            SettlementProbabilityReportOptions {
                min_bucket_observations: 1,
                ..Default::default()
            },
        );

        assert!(!report
            .baselines
            .iter()
            .any(|row| row.model.starts_with("q_llm_")));
    }

    #[test]
    fn multi_symbol_probability_prior_fails_closed() {
        let prior: crate::autofactor::LlmPriorSpec = serde_json::from_value(serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "mixed-symbol-mission",
            "data_snapshot_id": "sha256:mixed-data",
            "prompt_snapshot_id": "sha256:mixed-prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["BTC", "SOL"],
            "horizon": "5m",
            "probability_blends": [{
                "name": "mixed",
                "hypothesis": "A mixed-symbol candidate must not be evaluated.",
                "market_midpoint_weight": 1.0,
                "chainlink_digital_weight": 0.0,
                "distance_lob_vol_weight": 0.0,
                "event_surface_weight": 0.0,
                "existing_model_weight": 0.0
            }]
        }))
        .expect("typed prior");

        assert!(validate_prediction_research_prior(&prior).is_err());
        assert!(validated_probability_blends(Some(&prior)).is_empty());
    }

    #[test]
    fn prediction_prior_rejects_formula_mutations_and_empty_blends() {
        let base = serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "blend-only-mission",
            "data_snapshot_id": "sha256:btc-data",
            "prompt_snapshot_id": "sha256:btc-prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["BTC"],
            "horizon": "5m"
        });
        let empty: crate::autofactor::LlmPriorSpec =
            serde_json::from_value(base.clone()).expect("empty typed prior");
        assert_eq!(
            validate_prediction_research_prior(&empty),
            Err("prediction prior must contain at least one probability blend")
        );

        let mut formula = base;
        formula["mutations"] = serde_json::json!([{
            "base_factor": "auto_settlement_model_full_depth_settlement_edge",
            "mutation_type": "add_capacity_gate",
            "hypothesis": "A formula mutation must not cross into the blend-only loop."
        }]);
        formula["probability_blends"] = serde_json::json!([{
            "name": "otherwise_valid",
            "hypothesis": "The blend itself is valid.",
            "market_midpoint_weight": 1.0,
            "chainlink_digital_weight": 0.0,
            "distance_lob_vol_weight": 0.0,
            "event_surface_weight": 0.0,
            "existing_model_weight": 0.0
        }]);
        let formula: crate::autofactor::LlmPriorSpec =
            serde_json::from_value(formula).expect("formula typed prior");
        assert_eq!(
            validate_prediction_research_prior(&formula),
            Err("prediction prior may not contain factor mutations")
        );
        assert!(validated_probability_blends(Some(&formula)).is_empty());
    }

    #[test]
    fn prediction_feedback_is_candidate_specific_and_machine_readable() {
        let prior: crate::autofactor::LlmPriorSpec = serde_json::from_value(serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "polymarket-sol-5m-v1",
            "data_snapshot_id": "sha256:sol-data",
            "prompt_snapshot_id": "sha256:sol-prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["SOL"],
            "horizon": "5m",
            "probability_blends": [
                {
                    "name": "sol_flow",
                    "hypothesis": "SOL flow components improve OOS calibration.",
                    "market_midpoint_weight": 0.5,
                    "chainlink_digital_weight": 0.0,
                    "distance_lob_vol_weight": 0.3,
                    "event_surface_weight": 0.2,
                    "existing_model_weight": 0.0
                },
                {
                    "name": "no_oos",
                    "hypothesis": "This candidate still requires OOS evidence.",
                    "market_midpoint_weight": 1.0,
                    "chainlink_digital_weight": 0.0,
                    "distance_lob_vol_weight": 0.0,
                    "event_surface_weight": 0.0,
                    "existing_model_weight": 0.0
                }
            ]
        }))
        .expect("typed prior");
        let report = SettlementProbabilityWalkForwardReport {
            options: SettlementProbabilityWalkForwardOptions::default(),
            windows: Vec::new(),
            aggregates: vec![SettlementProbabilityWalkForwardAggregate {
                model: "q_llm_sol_flow".to_string(),
                windows: 3,
                positive_window_ratio: 1.0,
                pass_window_ratio: 0.0,
                avg_test_brier_score: 0.40,
                avg_test_log_loss: 0.80,
                avg_test_expected_calibration_error: 0.20,
                avg_test_top_edge_avg_full_depth_settlement_pnl: -1.0,
                min_test_top_edge_avg_full_depth_settlement_pnl: -2.0,
                avg_test_top_edge_avg_conservative_settlement_pnl: -1.5,
                min_test_top_edge_avg_conservative_settlement_pnl: -2.5,
                min_top_edge_conservative_coverage_rate: 1.0,
            }],
        };

        let feedback =
            build_prediction_research_feedback(&prior, &report, 0.6).expect("mission feedback");

        assert_eq!(feedback.mission_id, "polymarket-sol-5m-v1");
        assert_eq!(feedback.target, "full_depth_settlement_executable_pnl");
        assert_eq!(feedback.symbols, ["SOL"]);
        assert_eq!(feedback.horizon, "5m");
        assert_eq!(feedback.data_snapshot_id, "sha256:sol-data");
        assert_eq!(feedback.candidates.len(), 2);
        let sol_feedback = feedback
            .candidates
            .iter()
            .find(|candidate| candidate.model == "q_llm_sol_flow")
            .expect("SOL feedback");
        assert!(sol_feedback.hypothesis.contains("SOL flow"));
        assert_eq!(sol_feedback.probability_blend.name, "sol_flow");
        assert_eq!(sol_feedback.verdict, "discard");
        assert!(sol_feedback
            .reason_codes
            .contains(&"calibration_gate_failed".to_string()));
        assert!(sol_feedback
            .reason_codes
            .contains(&"nonpositive_oos_conservative_capacity_pnl".to_string()));
        assert_eq!(
            sol_feedback.metrics["avg_test_brier_score"],
            serde_json::json!(0.40)
        );
        let no_oos = feedback
            .candidates
            .iter()
            .find(|candidate| candidate.model == "q_llm_no_oos")
            .expect("no-OOS feedback");
        assert_eq!(no_oos.verdict, "discard");
        assert_eq!(no_oos.reason_codes, ["no_oos_windows"]);

        let mut incomplete = report.clone();
        incomplete.aggregates[0].min_top_edge_conservative_coverage_rate = 0.5;
        let incomplete_feedback = build_prediction_research_feedback(&prior, &incomplete, 0.6)
            .expect("incomplete capacity feedback");
        assert!(incomplete_feedback
            .candidates
            .iter()
            .find(|candidate| candidate.model == "q_llm_sol_flow")
            .expect("SOL feedback")
            .reason_codes
            .contains(&"incomplete_oos_top_edge_conservative_capacity_coverage".to_string()));
    }

    #[test]
    fn prediction_feedback_requires_incremental_value_over_existing_baselines() {
        let prior: crate::autofactor::LlmPriorSpec = serde_json::from_value(serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "polymarket-btc-5m-v1",
            "data_snapshot_id": "sha256:btc-data",
            "prompt_snapshot_id": "sha256:btc-prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["BTC"],
            "horizon": "5m",
            "probability_blends": [{
                "name": "binance_incremental",
                "hypothesis": "Binance flow adds incremental OOS value.",
                "market_midpoint_weight": 0.5,
                "chainlink_digital_weight": 0.2,
                "distance_lob_vol_weight": 0.3,
                "event_surface_weight": 0.0,
                "existing_model_weight": 0.0
            }]
        }))
        .expect("typed prior");
        let aggregate = |model: &str, brier: f64, log_loss: f64, conservative_pnl: f64| {
            SettlementProbabilityWalkForwardAggregate {
                model: model.to_string(),
                windows: 3,
                positive_window_ratio: 1.0,
                pass_window_ratio: 1.0,
                avg_test_brier_score: brier,
                avg_test_log_loss: log_loss,
                avg_test_expected_calibration_error: 0.05,
                avg_test_top_edge_avg_full_depth_settlement_pnl: conservative_pnl + 0.5,
                min_test_top_edge_avg_full_depth_settlement_pnl: 0.5,
                avg_test_top_edge_avg_conservative_settlement_pnl: conservative_pnl,
                min_test_top_edge_avg_conservative_settlement_pnl: 0.25,
                min_top_edge_conservative_coverage_rate: 1.0,
            }
        };
        let mut report = SettlementProbabilityWalkForwardReport {
            options: SettlementProbabilityWalkForwardOptions::default(),
            windows: Vec::new(),
            aggregates: vec![
                aggregate("q_market_midpoint", 0.18, 0.45, 1.6),
                aggregate("q_llm_binance_incremental", 0.20, 0.50, 1.5),
            ],
        };
        let worse = build_prediction_research_feedback(&prior, &report, 0.6)
            .expect("worse candidate feedback");
        assert_eq!(worse.candidates[0].verdict, "discard");
        assert!(worse.candidates[0]
            .reason_codes
            .contains(&"no_incremental_brier_vs_baseline".to_string()));

        report.aggregates[1] = aggregate("q_llm_binance_incremental", 0.16, 0.40, 2.0);
        let better = build_prediction_research_feedback(&prior, &report, 0.6)
            .expect("better candidate feedback");
        assert_eq!(better.candidates[0].verdict, "keep");
        assert!(better.candidates[0].reason_codes.is_empty());
    }

    #[test]
    fn settlement_probability_walk_forward_uses_train_surface_for_test_window() {
        let start = Utc::now();
        let source_rows = (0..8)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("wf-evt-{idx}");
                obs.tick_ts = start + Duration::hours(idx * 12);
                obs.settlement_up = if idx % 3 == 0 { 0.0 } else { 1.0 };
                obs.pm_up_bid = 0.42;
                obs.pm_up_ask = 0.46;
                obs.pm_down_bid = 0.52;
                obs.pm_down_ask = 0.56;
                obs.distance_over_sigma = if idx % 2 == 0 { 0.6 } else { -0.4 };
                obs
            })
            .collect::<Vec<_>>();
        let mut rows = build_factor_observations_v2(&source_rows, &FactorReviewOptions::default());
        rows.retain(|row| row.side == ReviewSide::Up);
        make_settlement_probability_eligible(&mut rows);

        let prior: crate::autofactor::LlmPriorSpec = serde_json::from_value(serde_json::json!({
            "target": "full_depth_settlement_executable_pnl",
            "mission_id": "polymarket-btc-5m-v1",
            "data_snapshot_id": "sha256:btc-data",
            "prompt_snapshot_id": "sha256:btc-prompt",
            "search_policy_snapshot_id": "prediction-probability-blend-v1",
            "symbols": ["BTC"],
            "horizon": "5m",
            "probability_blends": [{
                "name": "walk_forward",
                "hypothesis": "The blend improves event-disjoint OOS calibration.",
                "market_midpoint_weight": 0.4,
                "chainlink_digital_weight": 0.0,
                "distance_lob_vol_weight": 0.4,
                "event_surface_weight": 0.2,
                "existing_model_weight": 0.0
            }]
        }))
        .expect("typed prior");
        let report = walk_forward_settlement_probability_report_with_prior(
            &rows,
            start,
            start + Duration::days(4),
            Some(&prior),
            SettlementProbabilityWalkForwardOptions {
                walk_forward: FactorWalkForwardOptions {
                    review: FactorReviewOptions {
                        min_observations: 1,
                        ..Default::default()
                    },
                    train_window_days: 1,
                    test_window_days: 1,
                    step_days: 1,
                    ..Default::default()
                },
                probability: SettlementProbabilityReportOptions {
                    min_bucket_observations: 1,
                    event_surface_min_bucket_observations: 1,
                    event_surface_shrinkage_observations: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert!(report
            .windows
            .iter()
            .any(|row| row.model == "q_final_logit_blend"));
        assert!(report
            .aggregates
            .iter()
            .any(|row| row.model == "q_final_logit_blend"));
        assert!(report
            .aggregates
            .iter()
            .any(|row| row.model == "q_llm_walk_forward"));
        let text = format_settlement_probability_walk_forward_report(&report);
        assert!(text.contains("Settlement Probability Walk-Forward Report"));
        assert!(text.contains("EventVolSurface and q_final use only the train window"));
    }

    #[test]
    fn settlement_probability_walk_forward_assigns_crossing_event_to_its_end_window() {
        let start = Utc::now();
        let boundary = start + Duration::hours(12);
        let mut source_rows = Vec::new();
        for (event_id, tick_ts) in [
            ("train-only", start + Duration::hours(1)),
            ("crossing", boundary - Duration::minutes(1)),
            ("crossing", boundary + Duration::minutes(1)),
            ("test-only", boundary + Duration::hours(1)),
        ] {
            let mut row = base_obs();
            row.event_id = event_id.to_string();
            row.tick_ts = tick_ts;
            source_rows.push(row);
        }
        let mut rows = build_factor_observations_v2(
            &source_rows,
            &FactorReviewOptions {
                min_observations: 1,
                ..Default::default()
            },
        );
        rows.retain(|row| row.side == ReviewSide::Up);
        rows.sort_by_key(|row| row.tick_ts);

        let (train, test) = event_disjoint_walk_forward_slices(
            &rows,
            &inferred_event_ends(&rows),
            start,
            boundary,
            boundary,
            boundary + Duration::hours(12),
        );

        assert_eq!(
            train
                .iter()
                .map(|row| row.event_id.as_str())
                .collect::<Vec<_>>(),
            ["train-only"]
        );
        assert_eq!(
            test.iter()
                .map(|row| row.event_id.as_str())
                .collect::<Vec<_>>(),
            ["crossing", "test-only"]
        );
    }

    #[test]
    fn walk_forward_excludes_unsettled_train_event_without_test_ticks() {
        let start = Utc::now();
        let boundary = start + Duration::hours(12);
        let mut source = base_obs();
        source.event_id = "unsettled".to_string();
        source.tick_ts = boundary - Duration::minutes(1);
        source.time_remaining_secs = 120;
        let mut rows = build_factor_observations_v2(&[source], &FactorReviewOptions::default());
        rows.retain(|row| row.side == ReviewSide::Up);
        rows.sort_by_key(|row| row.tick_ts);

        let (train, test) = event_disjoint_walk_forward_slices(
            &rows,
            &inferred_event_ends(&rows),
            start,
            boundary,
            boundary,
            boundary + Duration::hours(12),
        );

        assert!(train.is_empty());
        assert!(test.is_empty());
    }

    #[test]
    fn settlement_probability_walk_forward_supports_intraday_oos_windows() {
        let start = Utc::now();
        let source_rows = (0..24)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("intraday-oos-evt-{idx}");
                obs.tick_ts = start + Duration::hours(idx);
                obs.settlement_up = if idx % 2 == 0 { 1.0 } else { 0.0 };
                obs.pm_up_bid = 0.42;
                obs.pm_up_ask = 0.46;
                obs.pm_down_bid = 0.52;
                obs.pm_down_ask = 0.56;
                obs.distance_over_sigma = if idx % 2 == 0 { 0.6 } else { -0.4 };
                obs
            })
            .collect::<Vec<_>>();
        let mut rows = build_factor_observations_v2(&source_rows, &FactorReviewOptions::default());
        rows.retain(|row| row.side == ReviewSide::Up);
        make_settlement_probability_eligible(&mut rows);

        let report = walk_forward_settlement_probability_report(
            &rows,
            start,
            start + Duration::hours(24),
            SettlementProbabilityWalkForwardOptions {
                walk_forward: FactorWalkForwardOptions {
                    review: FactorReviewOptions {
                        min_observations: 1,
                        ..Default::default()
                    },
                    train_window_hours: Some(12),
                    test_window_hours: Some(12),
                    step_hours: Some(12),
                    ..Default::default()
                },
                probability: SettlementProbabilityReportOptions {
                    min_bucket_observations: 1,
                    event_surface_min_bucket_observations: 1,
                    event_surface_shrinkage_observations: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert!(report
            .windows
            .iter()
            .any(|row| row.model == "q_final_logit_blend"));
        assert!(report
            .aggregates
            .iter()
            .any(|row| { row.model == "q_final_logit_blend" && row.windows >= 1 }));
        let text = format_settlement_probability_walk_forward_report(&report);
        assert!(text.contains("train_window=12h test_window=12h step=12h"));
    }

    #[test]
    fn settlement_probability_oos_requires_all_probability_quality_gates() {
        let mut report = SettlementProbabilityWalkForwardReport {
            options: SettlementProbabilityWalkForwardOptions::default(),
            windows: Vec::new(),
            aggregates: vec![SettlementProbabilityWalkForwardAggregate {
                model: "q_final_logit_blend".to_string(),
                windows: 3,
                positive_window_ratio: 1.0,
                pass_window_ratio: 1.0,
                avg_test_brier_score: 0.1,
                avg_test_log_loss: 0.2,
                avg_test_expected_calibration_error: 0.01,
                avg_test_top_edge_avg_full_depth_settlement_pnl: 2.0,
                min_test_top_edge_avg_full_depth_settlement_pnl: 1.0,
                avg_test_top_edge_avg_conservative_settlement_pnl: 1.5,
                min_test_top_edge_avg_conservative_settlement_pnl: 0.5,
                min_top_edge_conservative_coverage_rate: 1.0,
            }],
        };

        assert!(best_walk_forward_oos_model(&report, 0.6).is_some());
        report.aggregates[0].avg_test_brier_score = 0.3;
        assert!(best_walk_forward_oos_model(&report, 0.6).is_none());
        report.aggregates[0].avg_test_brier_score = 0.1;
        report.aggregates[0].avg_test_log_loss = 0.8;
        assert!(best_walk_forward_oos_model(&report, 0.6).is_none());
        report.aggregates[0].avg_test_log_loss = 0.2;
        report.aggregates[0].avg_test_expected_calibration_error = 0.1;
        assert!(best_walk_forward_oos_model(&report, 0.6).is_none());
        report.aggregates[0].avg_test_expected_calibration_error = 0.01;
        report.aggregates[0].min_top_edge_conservative_coverage_rate = 0.99;
        assert!(best_walk_forward_oos_model(&report, 0.6).is_none());
    }

    #[test]
    fn settlement_probability_promotion_gate_tracks_pending_post_dryrun_replay_parity() {
        let probability = SettlementProbabilityReport {
            options: SettlementProbabilityReportOptions::default(),
            baselines: vec![SettlementProbabilityBaselineRow {
                model: "q_final_logit_blend".to_string(),
                n: 100,
                avg_predicted_q: 0.7,
                actual_win_rate: 0.7,
                brier_score: 0.1,
                log_loss: 0.2,
                expected_calibration_error: 0.01,
                avg_edge: 0.05,
                avg_full_depth_settlement_pnl: 1.0,
                avg_conservative_settlement_pnl: 0.8,
                conservative_coverage_rate: 1.0,
                profit_factor: 1.2,
                edge_bucket_monotonic_non_decreasing: true,
                top_edge_count: 20,
                top_edge_avg_edge: 0.1,
                top_edge_win_rate: 0.8,
                top_edge_avg_full_depth_settlement_pnl: 2.0,
                top_edge_avg_conservative_settlement_pnl: 1.5,
                top_edge_conservative_coverage_rate: 1.0,
            }],
            calibration: Vec::new(),
            edge_buckets: Vec::new(),
            anti_overfit: vec![
                SettlementProbabilityAntiOverfitRow {
                    model: "q_final_logit_blend".to_string(),
                    test: "label_cyclic_shift".to_string(),
                    n: 100,
                    observed_edge_win_rank_ic: 0.1,
                    perturbed_edge_win_rank_ic: 0.0,
                    observed_top_edge_avg_full_depth_settlement_pnl: 2.0,
                    perturbed_top_edge_avg_full_depth_settlement_pnl: -1.0,
                    pass: true,
                },
                SettlementProbabilityAntiOverfitRow {
                    model: "q_final_logit_blend".to_string(),
                    test: "prediction_one_step_shift".to_string(),
                    n: 100,
                    observed_edge_win_rank_ic: 0.1,
                    perturbed_edge_win_rank_ic: 0.0,
                    observed_top_edge_avg_full_depth_settlement_pnl: 2.0,
                    perturbed_top_edge_avg_full_depth_settlement_pnl: -1.0,
                    pass: true,
                },
            ],
            symbol_holdouts: vec![SettlementProbabilitySymbolHoldoutRow {
                model: "q_final_logit_blend".to_string(),
                symbol: "BTCUSDT".to_string(),
                n: 100,
                edge_win_rank_ic: 0.1,
                top_edge_avg_full_depth_settlement_pnl: 2.0,
                pass: true,
            }],
            ablations: Vec::new(),
        };
        let walk_forward = SettlementProbabilityWalkForwardReport {
            options: SettlementProbabilityWalkForwardOptions::default(),
            windows: Vec::new(),
            aggregates: vec![SettlementProbabilityWalkForwardAggregate {
                model: "q_final_logit_blend".to_string(),
                windows: 3,
                positive_window_ratio: 1.0,
                pass_window_ratio: 1.0,
                avg_test_brier_score: 0.1,
                avg_test_log_loss: 0.2,
                avg_test_expected_calibration_error: 0.01,
                avg_test_top_edge_avg_full_depth_settlement_pnl: 2.0,
                min_test_top_edge_avg_full_depth_settlement_pnl: 1.0,
                avg_test_top_edge_avg_conservative_settlement_pnl: 1.5,
                min_test_top_edge_avg_conservative_settlement_pnl: 0.5,
                min_top_edge_conservative_coverage_rate: 1.0,
            }],
        };
        let execution = FullDepthExecutionMatrixReport {
            options: FullDepthExecutionMatrixOptions::default(),
            rows: vec![FullDepthExecutionMatrixRow {
                stake_usd: DEFAULT_STAKE_USD,
                symbol: "BTCUSDT".to_string(),
                side: ReviewSide::Up,
                time_bucket: "30-90s".to_string(),
                distance_bucket: "near".to_string(),
                entry_price_bucket: "mid".to_string(),
                spread_bucket: "tight".to_string(),
                quote_age_bucket: "fresh".to_string(),
                count: 100,
                entry_fill_rate: 0.5,
                entry_avg_price_mean: 0.6,
                entry_avg_slippage_bps: 10.0,
                entry_p50_slippage_bps: 5.0,
                entry_p90_slippage_bps: 20.0,
                entry_avg_levels_used: 1.0,
                exit_5s_fill_rate: 0.5,
                exit_10s_fill_rate: 0.5,
                exit_30s_fill_rate: 0.5,
                exit_10s_avg_slippage_bps: 10.0,
                exit_30s_avg_slippage_bps: 10.0,
                roundtrip_fill_rate_5s: 0.5,
                roundtrip_fill_rate_10s: 0.5,
                roundtrip_fill_rate_30s: 0.5,
                avg_settlement_pnl: 1.0,
                avg_reprice_pnl_5s: 0.1,
                avg_reprice_pnl_10s: 0.1,
                avg_reprice_pnl_30s: 0.1,
            }],
        };

        let pending_parity = build_settlement_probability_promotion_gate_report(
            &probability,
            &walk_forward,
            &execution,
            &execution,
            SettlementProbabilityPromotionGateOptions {
                include_deribit: false,
                data_audit_status: Some("ok".to_string()),
                replay_parity_ready: false,
                ..Default::default()
            },
        );
        assert!(pending_parity.ready_for_dry_run_handoff);
        assert!(pending_parity
            .gates
            .iter()
            .any(|gate| gate.gate == "recorded_replay_parity"
                && !gate.passed
                && gate.evidence.contains("post-dry-run gate pending")));

        let ready = build_settlement_probability_promotion_gate_report(
            &probability,
            &walk_forward,
            &execution,
            &execution,
            SettlementProbabilityPromotionGateOptions {
                include_deribit: false,
                data_audit_status: Some("ok".to_string()),
                replay_parity_ready: true,
                ..Default::default()
            },
        );
        assert!(ready.ready_for_dry_run_handoff);
        let deribit_gate = ready
            .gates
            .iter()
            .find(|gate| gate.gate == "deribit_vol_surface")
            .expect("deribit_vol_surface gate");
        assert!(deribit_gate.passed);
        assert!(deribit_gate.evidence.contains("require_deribit=false"));

        let globally_unfillable = build_settlement_probability_promotion_gate_report(
            &probability,
            &walk_forward,
            &execution,
            &execution,
            SettlementProbabilityPromotionGateOptions {
                include_deribit: false,
                data_audit_status: Some("ok".to_string()),
                global_full_depth_entry_fill_rate: Some(0.1458),
                replay_parity_ready: true,
                ..Default::default()
            },
        );
        assert!(!globally_unfillable.ready_for_dry_run_handoff);
        assert!(globally_unfillable.gates.iter().any(|gate| {
            gate.gate == "global_full_depth_entry_fillability"
                && !gate.passed
                && gate
                    .evidence
                    .contains("global_full_depth_entry_fill_rate=0.1458")
        }));

        let deribit_required = build_settlement_probability_promotion_gate_report(
            &probability,
            &walk_forward,
            &execution,
            &execution,
            SettlementProbabilityPromotionGateOptions {
                require_deribit: true,
                include_deribit: false,
                data_audit_status: Some("ok".to_string()),
                replay_parity_ready: true,
                ..Default::default()
            },
        );
        assert!(!deribit_required.ready_for_dry_run_handoff);
        assert!(deribit_required.gates.iter().any(|gate| {
            gate.gate == "deribit_vol_surface"
                && !gate.passed
                && gate
                    .evidence
                    .contains("require_deribit=true include_deribit=false")
        }));

        let text = format_settlement_probability_promotion_gate_report(&deribit_required);
        assert!(text.contains("Settlement Probability PRD Promotion Gate"));
        assert!(text.contains("ready_for_dry_run_handoff=false"));
    }

    #[test]
    fn settlement_probability_event_complete_data_quality_allows_critical_global_audit() {
        let probability = SettlementProbabilityReport {
            options: SettlementProbabilityReportOptions::default(),
            baselines: Vec::new(),
            calibration: Vec::new(),
            edge_buckets: Vec::new(),
            anti_overfit: Vec::new(),
            symbol_holdouts: Vec::new(),
            ablations: Vec::new(),
        };
        let walk_forward = SettlementProbabilityWalkForwardReport {
            options: SettlementProbabilityWalkForwardOptions::default(),
            windows: Vec::new(),
            aggregates: Vec::new(),
        };
        let execution = FullDepthExecutionMatrixReport {
            options: FullDepthExecutionMatrixOptions::default(),
            rows: Vec::new(),
        };

        let report = build_settlement_probability_promotion_gate_report(
            &probability,
            &walk_forward,
            &execution,
            &execution,
            SettlementProbabilityPromotionGateOptions {
                include_deribit: true,
                data_audit_status: Some("critical".to_string()),
                data_quality_mode: SettlementProbabilityDataQualityMode::EventComplete,
                event_complete_events: 20,
                event_complete_rows: 40,
                replay_parity_ready: true,
                ..Default::default()
            },
        );

        let data_gate = report
            .gates
            .iter()
            .find(|gate| gate.gate == "data_quality")
            .expect("data_quality gate");
        assert!(data_gate.passed);
        assert!(data_gate.evidence.contains("mode=event_complete"));
        assert!(data_gate
            .evidence
            .contains("snapshot_data_audit_status=critical"));
    }

    #[test]
    fn health_report_counts_missing_size_as_rejected() {
        let mut obs = base_obs();
        obs.pm_up_ask_size = 1.0;
        obs.pm_down_ask_size = 1.0;
        let options = FactorReviewOptions::default();
        let v2 = build_factor_observations_v2(&[obs.clone()], &options);
        let health = build_data_health_report(&[obs], &v2);
        assert_eq!(health.v2_rows, 2);
        assert_eq!(health.entry_fillable_rows, 0);
        assert_eq!(health.executable_pnl_rows, 0);
        assert!((health.rejection_rate() - 1.0).abs() < EPS);
    }

    #[test]
    fn full_depth_sweep_labels_cross_multiple_pm_levels_without_changing_top_book_label() {
        let mut obs = base_obs();
        obs.pm_up_ask_size = 10.0;
        obs.pm_down_ask_size = 10.0;
        let ts = obs.tick_ts;
        let books = vec![ResearchPmBookSnapshot {
            event_id: "evt".into(),
            token_id: "up-token".into(),
            side: "UP".into(),
            ts,
            bids: vec![crate::factors::ResearchPmBookLevel {
                price: 0.49,
                size: 40.0,
            }],
            asks: vec![
                crate::factors::ResearchPmBookLevel {
                    price: 0.50,
                    size: 10.0,
                },
                crate::factors::ResearchPmBookLevel {
                    price: 0.51,
                    size: 25.0,
                },
            ],
        }];

        let rows = build_factor_observations_v2_with_deribit_and_pm_books(
            &[obs],
            &[],
            &books,
            &FactorReviewOptions::default(),
        );
        let up = rows.iter().find(|row| row.side == ReviewSide::Up).unwrap();

        assert!(!up.label_executable_fillable);
        assert!(up.label_full_depth_entry_fillable);
        assert!(up.label_full_depth_exit_fillable);
        assert!(up.label_executable_pnl_15u.is_none());
        assert!(up.label_full_depth_executable_pnl_15u.unwrap() > 0.0);
        assert_eq!(up.entry_sweep_levels_15u, 2.0);
        assert!(up.entry_sweep_slippage_bps > 0.0);
    }

    #[test]
    fn full_depth_lookup_ignores_newer_wrong_token_for_the_same_side() {
        let mut obs = base_obs();
        obs.pm_up_ask_size = 1.0;
        let correct_book = ResearchPmBookSnapshot {
            event_id: obs.event_id.clone(),
            token_id: obs.up_token_id.clone(),
            side: "UP".into(),
            ts: obs.tick_ts - chrono::Duration::seconds(1),
            bids: vec![crate::factors::ResearchPmBookLevel {
                price: 0.49,
                size: 40.0,
            }],
            asks: vec![crate::factors::ResearchPmBookLevel {
                price: 0.50,
                size: 40.0,
            }],
        };
        let mut wrong_token_book = correct_book.clone();
        wrong_token_book.token_id = "wrong-token".into();
        wrong_token_book.ts = obs.tick_ts;
        wrong_token_book.asks[0].size = 1.0;

        let rows = build_factor_observations_v2_with_deribit_and_pm_books(
            &[obs],
            &[],
            &[correct_book, wrong_token_book],
            &FactorReviewOptions::default(),
        );
        let up = rows.iter().find(|row| row.side == ReviewSide::Up).unwrap();

        assert_eq!(up.pm_token_id, "up-token");
        assert!(up.label_full_depth_entry_fillable);
    }

    #[test]
    fn full_depth_lookup_enforces_quote_age_to_the_millisecond() {
        let obs = base_obs();
        let mut book = ResearchPmBookSnapshot {
            event_id: obs.event_id.clone(),
            token_id: obs.up_token_id.clone(),
            side: "UP".into(),
            ts: obs.tick_ts - chrono::Duration::milliseconds(5_000),
            bids: vec![crate::factors::ResearchPmBookLevel {
                price: 0.49,
                size: 40.0,
            }],
            asks: vec![crate::factors::ResearchPmBookLevel {
                price: 0.50,
                size: 40.0,
            }],
        };
        let options = FactorReviewOptions {
            max_quote_age_secs: 5,
            ..Default::default()
        };
        let boundary = build_factor_observations_v2_with_deribit_and_pm_books(
            &[obs.clone()],
            &[],
            &[book.clone()],
            &options,
        );
        assert!(
            boundary
                .iter()
                .find(|row| row.side == ReviewSide::Up)
                .unwrap()
                .label_full_depth_entry_fillable
        );

        book.ts -= chrono::Duration::milliseconds(1);
        let stale =
            build_factor_observations_v2_with_deribit_and_pm_books(&[obs], &[], &[book], &options);
        assert!(
            !stale
                .iter()
                .find(|row| row.side == ReviewSide::Up)
                .unwrap()
                .label_full_depth_entry_fillable
        );
    }

    #[test]
    fn full_depth_sweep_charges_probability_fee_per_level() {
        let levels = vec![
            crate::factors::ResearchPmBookLevel {
                price: 0.20,
                size: 1.0,
            },
            crate::factors::ResearchPmBookLevel {
                price: 0.80,
                size: 1.0,
            },
        ];

        let fill = sweep_buy_to_stake(&levels, 0.20, 1.0);

        assert!(fill.fillable);
        assert!((fill.avg_price - 0.50).abs() < EPS);
        assert!((fill.shares - 2.0).abs() < EPS);
        assert!((fill.fee_usd - 0.0224).abs() < EPS);
    }

    #[test]
    fn future_repricing_full_depth_requires_future_bid_depth() {
        let mut current = base_obs();
        current.pm_up_ask_size = 10.0;
        current.pm_up_bid_size = 100.0;
        let mut future = current.clone();
        future.tick_ts = current.tick_ts + chrono::Duration::seconds(10);
        future.pm_up_bid = 0.70;
        future.pm_up_bid_size = 100.0;
        let books = vec![
            ResearchPmBookSnapshot {
                event_id: "evt".into(),
                token_id: "up-token".into(),
                side: "UP".into(),
                ts: current.tick_ts,
                bids: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.49,
                    size: 40.0,
                }],
                asks: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.50,
                    size: 40.0,
                }],
            },
            ResearchPmBookSnapshot {
                event_id: "evt".into(),
                token_id: "up-token".into(),
                side: "UP".into(),
                ts: future.tick_ts,
                bids: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.70,
                    size: 5.0,
                }],
                asks: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.72,
                    size: 40.0,
                }],
            },
        ];

        let rows = build_factor_observations_v2_with_deribit_and_pm_books(
            &[current.clone(), future],
            &[],
            &books,
            &FactorReviewOptions::default(),
        );
        let up = rows
            .iter()
            .find(|row| row.side == ReviewSide::Up && row.tick_ts == current.tick_ts)
            .unwrap();

        assert!(up.label_full_depth_entry_fillable);
        assert!(up.label_future_exit_pnl_10s.unwrap() > 0.0);
        assert_eq!(up.label_future_exit_full_depth_fillable_10s, Some(0.0));
        assert!(up.label_future_exit_full_depth_pnl_10s.is_none());
        assert!(up.label_full_depth_executable_pnl_15u.is_some());
    }

    #[test]
    fn execution_matrix_separates_stake_size_and_settlement_from_repricing_exit() {
        let mut current = base_obs();
        current.pm_up_ask = 0.50;
        current.pm_up_bid = 0.49;
        current.pm_up_ask_size = 100.0;
        current.pm_up_bid_size = 100.0;
        current.settlement_up = 1.0;
        let mut future = current.clone();
        future.tick_ts = current.tick_ts + chrono::Duration::seconds(10);
        future.pm_up_bid = 0.70;
        let books = vec![
            ResearchPmBookSnapshot {
                event_id: "evt".into(),
                token_id: "up-token".into(),
                side: "UP".into(),
                ts: current.tick_ts,
                bids: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.49,
                    size: 5.0,
                }],
                asks: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.50,
                    size: 10.0,
                }],
            },
            ResearchPmBookSnapshot {
                event_id: "evt".into(),
                token_id: "up-token".into(),
                side: "UP".into(),
                ts: future.tick_ts,
                bids: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.70,
                    size: 3.0,
                }],
                asks: vec![crate::factors::ResearchPmBookLevel {
                    price: 0.72,
                    size: 10.0,
                }],
            },
        ];

        let report = build_full_depth_execution_matrix(
            &[current, future],
            &books,
            FullDepthExecutionMatrixOptions {
                stakes_usd: vec![1.0, 15.0],
                min_bucket_observations: 1,
                ..Default::default()
            },
        );
        let up_1u = report
            .rows
            .iter()
            .find(|row| {
                row.symbol == "BTCUSDT" && row.side == ReviewSide::Up && row.stake_usd == 1.0
            })
            .unwrap();
        let up_15u = report
            .rows
            .iter()
            .find(|row| {
                row.symbol == "BTCUSDT" && row.side == ReviewSide::Up && row.stake_usd == 15.0
            })
            .unwrap();

        assert!(up_1u.entry_fill_rate > up_15u.entry_fill_rate);
        assert!(up_1u.avg_settlement_pnl > 0.0);
        assert!(up_1u.roundtrip_fill_rate_10s > up_15u.roundtrip_fill_rate_10s);
        assert!(format_full_depth_execution_matrix_report(&report, 5)
            .contains("Full-depth matrix is the execution gate"));
    }

    #[test]
    fn fillability_review_marks_capacity_bucket_candidate() {
        let observations = (0..40).map(|_| base_obs()).collect::<Vec<_>>();
        let report = review_fillability_v1_with_deribit(
            &observations,
            &[],
            FillabilityReviewOptions {
                min_bucket_observations: 10,
                min_entry_fill_rate: 0.30,
                min_roundtrip_fill_rate: 0.20,
                max_rejection_rate: 0.70,
                ..Default::default()
            },
        );

        let capacity = report
            .rows
            .iter()
            .find(|row| {
                row.dimension == "min_entry_exit_capacity_ratio" && row.bucket == "1p00_2p00"
            })
            .expect("capacity bucket");
        assert_eq!(capacity.decision, FillabilityDecision::Candidate);
        assert!((capacity.entry_fill_rate - 1.0).abs() < EPS);
        assert!((capacity.roundtrip_fill_rate - 1.0).abs() < EPS);
    }

    #[test]
    fn liquidity_gate_v1_selects_only_point_in_time_capacity() {
        let observations = (0..20)
            .map(|idx| {
                let mut obs = base_obs();
                if idx % 2 == 1 {
                    obs.pm_up_ask_size = 1.0;
                    obs.pm_up_bid_size = 1.0;
                    obs.pm_down_ask_size = 1.0;
                    obs.pm_down_bid_size = 1.0;
                }
                obs
            })
            .collect::<Vec<_>>();
        let report = liquidity_gate_v1_with_deribit(
            &observations,
            &[],
            LiquidityGateV1Options {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        assert_eq!(report.health.v2_rows, 40);
        assert_eq!(report.selected_n, 20);
        assert!((report.coverage - 0.5).abs() < EPS);
        assert!((report.entry_fill_rate - 1.0).abs() < EPS);
        assert!((report.roundtrip_fill_rate - 1.0).abs() < EPS);
    }

    #[test]
    fn liquidity_gated_alpha_v1_reviews_only_tradeable_rows() {
        let base = Utc::now();
        let observations = (0..96)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("event-{idx}");
                obs.tick_ts = base + chrono::Duration::hours(idx);
                obs.model_prob_up = if idx % 2 == 0 { 0.80 } else { 0.20 };
                obs.model_edge_up =
                    obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
                obs.settlement_up = if idx % 2 == 0 { 1.0 } else { 0.0 };
                if idx % 3 == 0 {
                    obs.pm_up_ask_size = 1.0;
                    obs.pm_up_bid_size = 1.0;
                    obs.pm_down_ask_size = 1.0;
                    obs.pm_down_bid_size = 1.0;
                }
                obs
            })
            .collect::<Vec<_>>();

        let report = liquidity_gated_alpha_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            LiquidityGatedAlphaV1Options {
                gate: LiquidityGateV1Options::default(),
                walk_forward: FactorWalkForwardOptions {
                    review: FactorReviewOptions {
                        stake_usd: 15.0,
                        min_observations: 10,
                        top_quantile: 0.2,
                        max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
                    },
                    train_window_days: 2,
                    test_window_days: 1,
                    step_days: 1,
                    train_window_hours: None,
                    test_window_hours: None,
                    step_hours: None,
                    top_n: 10,
                    factor_name_filter: Some("side_model_prob".to_string()),
                },
            },
        );

        assert!(report.gate.selected_n < report.baseline_health.v2_rows);
        assert!((report.gate.entry_fill_rate - 1.0).abs() < EPS);
        assert!(!report.review.reviews.is_empty());
        assert!(report
            .review
            .reviews
            .iter()
            .all(|review| !review.factor.starts_with("future_exit_")));
        assert!(!report.walk_forward.windows.is_empty());
        assert!(report
            .stability
            .rows
            .iter()
            .any(|row| row.factor == "side_model_prob"));
    }

    #[test]
    fn trade_formation_review_discovers_profitable_paths_and_meta_rules() {
        let base = Utc::now();
        let observations = (0..96)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("event-{idx}");
                obs.tick_ts = base + chrono::Duration::seconds(idx * 30);
                let up_wins = idx % 2 == 0;
                obs.model_prob_up = if up_wins { 0.80 } else { 0.20 };
                obs.model_edge_up =
                    obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
                obs.distance_over_sigma = if up_wins { 1.10 } else { -1.10 };
                obs.obi_10 = if up_wins { 0.40 } else { -0.40 };
                obs.depth_imbalance = if up_wins { 0.40 } else { -0.40 };
                obs.cex_bar_return_30s = if up_wins { 0.004 } else { -0.004 };
                obs.cex_bar_return_60s = if up_wins { 0.006 } else { -0.006 };
                obs.cex_signed_volume_ratio_30s = if up_wins { 0.70 } else { -0.70 };
                obs.cex_consecutive_up_bars = if up_wins { 3.0 } else { 0.0 };
                obs.cex_consecutive_down_bars = if up_wins { 0.0 } else { 3.0 };
                obs.cex_breakout_volume_score = if up_wins { 1.5 } else { -1.5 };
                obs.settlement_up = if up_wins { 1.0 } else { 0.0 };
                obs
            })
            .collect::<Vec<_>>();

        let report = review_trade_formation_v1_with_deribit(
            &observations,
            &[],
            TradeFormationReviewOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                    max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
                },
                min_path_observations: 10,
                top_n: 10,
                ..Default::default()
            },
        );

        assert_eq!(report.gated_rows, 192);
        assert_eq!(report.profitable_gated_rows, 96);
        assert_eq!(report.losing_gated_rows, 96);
        assert!(report.missed_winner_paths.is_empty());
        assert!(report.profitable_paths.iter().any(|row| {
            row.path.contains("direction=strong_model")
                && (row.path.contains("cex=depth_confirmed")
                    || row.path.contains("cex=persistent_obi"))
                && row.path.contains("continuation=continuation_confirmed")
                && row.total_pnl_after_cost > 0.0
        }));
        assert!(report.losing_paths.iter().any(|row| {
            row.path.contains("direction=weak_model")
                && row.path.contains("continuation=continuation_against")
                && row.total_pnl_after_cost < 0.0
        }));
        assert!(report.meta_label_rules.iter().any(|row| {
            row.rule == "strong_direction_cex_and_continuation" && row.win_rate > 0.99
        }));

        let formatted = format_trade_formation_v1_report(&report);
        assert!(formatted.contains("Trade Formation Review V1"));
        assert!(formatted.contains("Meta-Label Rule Candidates"));
    }

    #[test]
    fn meta_label_walk_forward_tests_fixed_rules_out_of_sample() {
        let base = Utc::now();
        let observations = (0..96)
            .map(|idx| {
                let mut obs = base_obs();
                obs.event_id = format!("event-{idx}");
                obs.tick_ts = base + chrono::Duration::hours(idx);
                let up_wins = idx % 2 == 0;
                obs.model_prob_up = if up_wins { 0.80 } else { 0.20 };
                obs.model_edge_up =
                    obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
                obs.distance_over_sigma = if up_wins { 1.10 } else { -1.10 };
                obs.obi_10 = if up_wins { 0.40 } else { -0.40 };
                obs.depth_imbalance = if up_wins { 0.40 } else { -0.40 };
                obs.cex_bar_return_30s = if up_wins { 0.004 } else { -0.004 };
                obs.cex_bar_return_60s = if up_wins { 0.006 } else { -0.006 };
                obs.cex_signed_volume_ratio_30s = if up_wins { 0.70 } else { -0.70 };
                obs.cex_consecutive_up_bars = if up_wins { 3.0 } else { 0.0 };
                obs.cex_consecutive_down_bars = if up_wins { 0.0 } else { 3.0 };
                obs.cex_breakout_volume_score = if up_wins { 1.5 } else { -1.5 };
                obs.settlement_up = if up_wins { 1.0 } else { 0.0 };
                obs
            })
            .collect::<Vec<_>>();

        let report = walk_forward_meta_label_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            MetaLabelWalkForwardOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                    max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
                },
                train_window_days: 2,
                test_window_days: 1,
                step_days: 1,
                min_rule_observations: 10,
                top_n: 10,
                ..Default::default()
            },
        );

        let continuation = report
            .aggregates
            .iter()
            .find(|row| row.rule == "continuation_confirmation")
            .expect("continuation aggregate");
        assert_eq!(continuation.windows, 2);
        assert!((continuation.positive_window_ratio - 1.0).abs() < EPS);
        assert!(continuation.total_test_pnl_after_cost > 0.0);
        assert_eq!(continuation.decision, FactorStabilityDecision::Watchlist);
        assert_eq!(continuation.reason, "too_few_oos_windows_positive_pnl");
        assert!(report.windows.iter().any(|window| {
            window.rule == "continuation_confirmation"
                && window.test.selected_n > 0
                && window.test.total_pnl_after_cost > 0.0
        }));

        let formatted = format_meta_label_walk_forward_v1_report(&report);
        assert!(formatted.contains("Meta-Label Walk-Forward V1"));
        assert!(formatted.contains("continuation_confirmation"));
        assert!(formatted.contains("decision"));
        assert!(formatted.contains("too_few_oos_windows_positive_pnl"));

        let candidate_report = walk_forward_meta_label_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            MetaLabelWalkForwardOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                    max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
                },
                train_window_days: 2,
                test_window_days: 1,
                step_days: 1,
                min_rule_observations: 10,
                min_candidate_windows: 2,
                min_candidate_avg_test_selected: 10.0,
                top_n: 10,
                ..Default::default()
            },
        );
        let candidate_continuation = candidate_report
            .aggregates
            .iter()
            .find(|row| row.rule == "continuation_confirmation")
            .expect("candidate continuation aggregate");
        assert_eq!(
            candidate_continuation.decision,
            FactorStabilityDecision::Candidate
        );
        assert_eq!(candidate_continuation.reason, "passed");
    }

    #[test]
    fn review_reports_single_factor_execution_metrics() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.model_prob_up = if i % 2 == 0 { 0.75 } else { 0.25 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }

        let report = review_factors_v2(&observations, FactorReviewOptions::default());
        assert!(report.health.entry_fill_rate() > 0.99);
        assert!(report
            .reviews
            .iter()
            .any(|review| review.factor == "side_model_edge"));
        assert!(report
            .reviews
            .iter()
            .any(|review| review.factor == "side_fair_edge"));
    }

    #[test]
    fn review_path_filters_factor_names_when_requested() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-filter-{i}");
            obs.model_prob_up = if i % 2 == 0 { 0.75 } else { 0.25 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }
        let options = FactorReviewOptions::default();
        let v2_rows = build_factor_observations_v2(&observations, &options);

        let report = review_factor_rows_with_name_filter(
            &observations,
            &v2_rows,
            options,
            Some("side_model_prob"),
        );

        assert!(!report.reviews.is_empty());
        assert!(report
            .reviews
            .iter()
            .all(|review| review.factor.contains("side_model_prob")));
    }

    #[test]
    fn review_reports_executable_ev_buckets_for_direction_and_fillability() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.model_prob_up = 0.72;
            obs.pm_up_ask = 0.48 + f64::from(i % 4) * 0.01;
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.pm_up_ask_size = if i % 5 == 0 { 10.0 } else { 40.0 };
            obs.settlement_up = 1.0;
            observations.push(obs);
        }

        let report = review_factors_v2(&observations, FactorReviewOptions::default());
        let high_direction = report
            .executable_ev_buckets
            .iter()
            .find(|bucket| bucket.dimension == "direction_probability" && bucket.bucket == ">=0.70")
            .expect("high direction bucket");
        assert_eq!(high_direction.rows, 40);
        assert_eq!(high_direction.fillable_rows, 32);
        assert_eq!(high_direction.pnl_rows, 32);
        assert!(!high_direction.underpowered);
        assert!(high_direction.positive_ev);
        assert!(high_direction.avg_side_model_prob >= 0.70);

        let low_direction = report
            .executable_ev_buckets
            .iter()
            .find(|bucket| bucket.dimension == "direction_probability" && bucket.bucket == "<0.50")
            .expect("low direction bucket");
        assert_eq!(low_direction.rows, 40);
        assert!(low_direction.avg_pnl_15u < 0.0);

        let text = format_factor_review_v2_report(&report, 5);
        assert!(text.contains("=== Executable EV Buckets: Best Non-Underpowered Avg PnL ==="));
        assert!(text.contains("direction_probability,>=0.70"));
    }

    #[test]
    fn direction_side_audit_reports_aligned_model_side_ev() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.model_prob_up = 0.72;
            obs.pm_up_ask = 0.45 + f64::from(i % 4) * 0.01;
            obs.pm_down_ask = 0.50;
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = 1.0;
            observations.push(obs);
        }

        let report = review_factors_v2(&observations, FactorReviewOptions::default());
        let audit = report
            .direction_side_audit
            .iter()
            .find(|summary| summary.selector == "model_probability" && summary.bucket == "all")
            .expect("probability audit");

        assert_eq!(audit.pairs, 40);
        assert_eq!(audit.favored.settlement_win_rate, 1.0);
        assert_eq!(audit.opposite.settlement_win_rate, 0.0);
        assert!(audit.favored.avg_pnl_15u > 0.0);
        assert!(audit.opposite.avg_pnl_15u < 0.0);
        assert!(audit.avg_pnl_delta_15u > 0.0);

        let text = format_factor_review_v2_report(&report, 5);
        assert!(text.contains("=== Direction Side Audit: Favored vs Opposite Executable EV ==="));
        assert!(text.contains("model_probability,all"));
    }

    #[test]
    fn direction_side_audit_exposes_inverted_model_side_ev() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.model_prob_up = 0.72;
            obs.pm_up_ask = 0.45 + f64::from(i % 4) * 0.01;
            obs.pm_down_ask = 0.50;
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = 0.0;
            observations.push(obs);
        }

        let report = review_factors_v2(&observations, FactorReviewOptions::default());
        let audit = report
            .direction_side_audit
            .iter()
            .find(|summary| summary.selector == "model_probability" && summary.bucket == "all")
            .expect("probability audit");

        assert_eq!(audit.pairs, 40);
        assert_eq!(audit.favored.settlement_win_rate, 0.0);
        assert_eq!(audit.opposite.settlement_win_rate, 1.0);
        assert!(audit.favored.avg_pnl_15u < 0.0);
        assert!(audit.opposite.avg_pnl_15u > 0.0);
        assert!(audit.avg_pnl_delta_15u < 0.0);
    }

    #[test]
    fn binance_direction_audit_reports_predictive_cex_bucket() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.drift_30s = 0.002;
            obs.pm_up_ask = 0.45 + (i % 5) as f64 * 0.01;
            obs.settlement_up = 1.0;
            observations.push(obs);
        }

        let report = review_factors_v2(
            &observations,
            FactorReviewOptions {
                top_quantile: 0.5,
                ..FactorReviewOptions::default()
            },
        );
        let top = report
            .binance_direction_audit
            .iter()
            .find(|summary| summary.factor == "drift_30s" && summary.bucket == "top_quantile")
            .expect("drift top bucket");

        assert_eq!(top.settlement_rows, 40);
        assert_eq!(top.settlement_win_rate, 1.0);
        assert!(top.t_stat_vs_coinflip > 2.0);
        assert!(top.statistically_supported);
        assert_eq!(top.fillable_rows, 40);
        assert_eq!(top.pnl_rows, 40);
        assert!(top.avg_pnl_15u > 0.0);
        assert!(top.executable_ev_supported);

        let text = format_factor_review_v2_report(&report, 5);
        assert!(text.contains("=== Binance/CEX Direction Audit: Settlement Predictive Buckets ==="));
        assert!(text.contains("ev_supported"));
    }

    #[test]
    fn binance_direction_audit_exposes_inverted_cex_bucket() {
        let mut observations = Vec::new();
        for i in 0..40 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.drift_30s = 0.002;
            obs.pm_up_ask = 0.45 + (i % 5) as f64 * 0.01;
            obs.pm_down_ask = 0.45 + (i % 5) as f64 * 0.01;
            obs.settlement_up = 0.0;
            observations.push(obs);
        }

        let report = review_factors_v2(
            &observations,
            FactorReviewOptions {
                top_quantile: 0.5,
                ..FactorReviewOptions::default()
            },
        );
        let top = report
            .binance_direction_audit
            .iter()
            .find(|summary| summary.factor == "drift_30s" && summary.bucket == "top_quantile")
            .expect("drift top bucket");
        let bottom = report
            .binance_direction_audit
            .iter()
            .find(|summary| summary.factor == "drift_30s" && summary.bucket == "bottom_quantile")
            .expect("drift bottom bucket");

        assert_eq!(top.settlement_win_rate, 0.0);
        assert!(top.t_stat_vs_coinflip < -2.0);
        assert!(!top.statistically_supported);
        assert!(top.avg_pnl_15u < 0.0);
        assert!(!top.executable_ev_supported);
        assert_eq!(bottom.settlement_win_rate, 1.0);
        assert!(bottom.statistically_supported);
        assert!(bottom.executable_ev_supported);
    }

    #[test]
    fn factor_review_report_separates_future_exit_diagnostics() {
        let health = DataHealthReport {
            source_observations: 10,
            v2_rows: 20,
            settlement_label_rows: 20,
            entry_quote_rows: 20,
            exit_quote_rows: 20,
            entry_size_rows: 20,
            exit_size_rows: 20,
            entry_fillable_rows: 2,
            exit_fillable_rows: 2,
            entry_full_depth_fillable_rows: 12,
            exit_full_depth_fillable_rows: 8,
            executable_pnl_rows: 2,
            full_depth_executable_pnl_rows: 8,
            deribit_rows: 0,
            avg_pm_lag_secs: 1.0,
            avg_entry_capacity_ratio: 1.0,
            avg_exit_capacity_ratio: 1.0,
            avg_entry_sweep_slippage_bps: 250.0,
            avg_exit_sweep_slippage_bps: 125.0,
        };
        let review = |factor: &str, pnl: f64| SingleFactorReview {
            factor: factor.to_string(),
            family: FactorFamily::Exit,
            layer: ThreeLayerArchive::PmExecutableLiquidityRiskGate,
            n: 20,
            coverage: 1.0,
            settlement_pearson_ic: 0.1,
            settlement_rank_ic: 0.1,
            executable_pnl_pearson_ic: 0.2,
            executable_pnl_rank_ic: 0.2,
            selected_n: 4,
            selected_rejection_rate: 0.1,
            selected_executable_fill_rate: 0.9,
            selected_avg_slippage_bps: 10.0,
            selected_total_pnl_after_cost: pnl,
            selected_avg_pnl_after_cost: pnl / 4.0,
            selected_sharpe: 1.0,
            selected_max_drawdown: 1.0,
            by_symbol_positive_ratio: 1.0,
            by_time_bucket_positive_ratio: 1.0,
        };
        let report = FactorReviewV2Report {
            options: FactorReviewOptions::default(),
            health,
            reviews: vec![
                review("future_exit_pnl_30s", 100.0),
                review("side_model_edge", 10.0),
            ],
            executable_ev_buckets: Vec::new(),
            direction_side_audit: Vec::new(),
            binance_direction_audit: Vec::new(),
        };

        let text = format_factor_review_v2_report(&report, 10);
        let tradable_section = text
            .split("=== Future Exit Diagnostics Not Tradable Factors ===")
            .next()
            .expect("tradable section");
        assert!(tradable_section.contains("side_model_edge"));
        assert!(!tradable_section.contains("future_exit_pnl_30s"));
        assert!(text.contains("=== Future Exit Diagnostics Not Tradable Factors ==="));
        assert!(text.contains("future_exit_pnl_30s"));
        assert!(text.contains("full_depth_pnl_rows=8"));
    }

    #[test]
    fn repricing_ic_report_scores_side_aligned_future_exit_pnl() {
        let mut rows = Vec::new();
        let base_ts = Utc::now();
        for (symbol_idx, symbol) in ["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT"]
            .iter()
            .enumerate()
        {
            for idx in 0..6 {
                let score = idx as f64 + symbol_idx as f64 * 0.1;
                let label = score * 0.25 - 0.5;
                let mut obs = base_obs();
                obs.event_id = format!("evt-{symbol}-{idx}");
                obs.symbol = (*symbol).to_string();
                obs.tick_ts = base_ts + chrono::Duration::seconds((symbol_idx * 100 + idx) as i64);
                obs.model_edge_up = score;
                obs.model_prob_up = (0.45 + score * 0.02).clamp(0.01, 0.99);
                obs.distance_over_sigma = if symbol_idx % 2 == 0 { 0.25 } else { 1.25 };
                let mut row = side_row(&obs, ReviewSide::Up, DEFAULT_STAKE_USD, None);
                row.side_model_edge = score;
                row.label_future_exit_pnl_30s = Some(label);
                row.label_future_exit_bid_change_30s = Some(label / row.entry_shares);
                rows.push(row);
            }
        }

        let report = review_repricing_ic_rows(
            &[],
            &rows,
            RepricingIcOptions {
                review: FactorReviewOptions {
                    min_observations: 20,
                    ..Default::default()
                },
                min_window_observations: 5,
                bucket_count: 5,
                factor_name_filter: Some("side_model_edge,future_exit".to_string()),
            },
        );
        let row = report
            .rows
            .iter()
            .find(|row| row.factor == "side_model_edge" && row.target == "reprice_pnl_30s")
            .expect("side_model_edge repricing row");

        assert_eq!(row.factor_role, "alpha_or_repricing");
        assert!(row.spearman_ic > 0.9);
        assert_eq!(row.window_count, 4);
        assert_eq!(row.positive_window_ratio, 1.0);
        assert!(row.top_bucket_avg_label > row.bottom_bucket_avg_label);
        assert!(row.monotonic_non_decreasing);
        assert!(report
            .rows
            .iter()
            .all(|row| !row.factor.starts_with("future_exit_")));

        let text = format_repricing_ic_report(&report, 10);
        assert!(text.contains("=== Repricing IC Target Group: reprice_pnl ==="));
        assert!(text.contains("future_exit_* fields are labels/diagnostics only"));
    }

    #[test]
    fn walk_forward_report_prints_full_depth_health() {
        let report = FactorWalkForwardReport {
            options: FactorWalkForwardOptions::default(),
            health: DataHealthReport {
                source_observations: 10,
                v2_rows: 20,
                settlement_label_rows: 20,
                entry_quote_rows: 20,
                exit_quote_rows: 20,
                entry_size_rows: 20,
                exit_size_rows: 20,
                entry_fillable_rows: 2,
                exit_fillable_rows: 2,
                entry_full_depth_fillable_rows: 12,
                exit_full_depth_fillable_rows: 8,
                executable_pnl_rows: 2,
                full_depth_executable_pnl_rows: 8,
                deribit_rows: 0,
                avg_pm_lag_secs: 1.0,
                avg_entry_capacity_ratio: 1.0,
                avg_exit_capacity_ratio: 1.0,
                avg_entry_sweep_slippage_bps: 250.0,
                avg_exit_sweep_slippage_bps: 125.0,
            },
            windows: Vec::new(),
            aggregates: Vec::new(),
        };

        let text = format_factor_walk_forward_v2_report(&report);
        assert!(text.contains("full_depth_entry_fill_rate=60.00%"));
        assert!(text.contains("full_depth_exit_fill_rate=40.00%"));
        assert!(text.contains("full_depth_pnl_rows=8"));
    }

    #[test]
    fn walk_forward_uses_train_threshold_on_future_window() {
        let base = Utc::now();
        let mut observations = Vec::new();
        for i in 0..72 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.tick_ts = base + chrono::Duration::hours(i);
            obs.model_prob_up = if i % 2 == 0 { 0.78 } else { 0.22 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }

        let report = walk_forward_factors_v2_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(3) - chrono::Duration::seconds(1),
            FactorWalkForwardOptions {
                review: FactorReviewOptions {
                    stake_usd: 15.0,
                    min_observations: 10,
                    top_quantile: 0.2,
                    max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
                },
                train_window_days: 2,
                test_window_days: 1,
                step_days: 1,
                train_window_hours: None,
                test_window_hours: None,
                step_hours: None,
                top_n: 10,
                factor_name_filter: None,
            },
        );

        assert!(!report.windows.is_empty());
        assert!(report.windows.len() > report.options.top_n);
        let side_model = report
            .windows
            .iter()
            .find(|window| window.factor == "side_model_prob")
            .expect("side_model_prob window");
        assert_eq!(side_model.window_index, 0);
        assert!(side_model.threshold.is_finite());
        assert!(side_model.test.selected_n > 0);
        assert!(side_model.test.total_pnl_after_cost > 0.0);
        assert!(report
            .aggregates
            .iter()
            .any(|aggregate| aggregate.factor == "side_model_prob"));
        assert!(report
            .windows
            .iter()
            .all(|window| !window.factor.starts_with("future_exit_")));
    }

    #[test]
    fn stability_report_rejects_too_short_negative_factors() {
        let base = Utc::now();
        let report = FactorWalkForwardReport {
            options: FactorWalkForwardOptions::default(),
            health: DataHealthReport {
                source_observations: 0,
                v2_rows: 0,
                settlement_label_rows: 0,
                entry_quote_rows: 0,
                exit_quote_rows: 0,
                entry_size_rows: 0,
                exit_size_rows: 0,
                entry_fillable_rows: 0,
                exit_fillable_rows: 0,
                entry_full_depth_fillable_rows: 0,
                exit_full_depth_fillable_rows: 0,
                executable_pnl_rows: 0,
                full_depth_executable_pnl_rows: 0,
                deribit_rows: 0,
                avg_pm_lag_secs: f64::NAN,
                avg_entry_capacity_ratio: f64::NAN,
                avg_exit_capacity_ratio: f64::NAN,
                avg_entry_sweep_slippage_bps: f64::NAN,
                avg_exit_sweep_slippage_bps: f64::NAN,
            },
            windows: vec![FactorWalkForwardWindow {
                window_index: 0,
                train_start: base,
                train_end: base + chrono::Duration::days(1),
                test_start: base + chrono::Duration::days(1),
                test_end: base + chrono::Duration::days(2),
                factor: "bad_factor".to_string(),
                family: FactorFamily::Alpha,
                layer: ThreeLayerArchive::DirectionProbabilityEdge,
                direction: 1.0,
                threshold: 0.0,
                train_settlement_rank_ic: -0.1,
                train_executable_pnl_rank_ic: -0.2,
                train: FactorSelectionMetrics {
                    n: 10,
                    selected_n: 2,
                    executable_fill_rate: 1.0,
                    rejection_rate: 0.0,
                    total_pnl_after_cost: 1.0,
                    avg_pnl_after_cost: 0.5,
                    sharpe: 1.0,
                    max_drawdown: 0.0,
                    by_symbol_positive_ratio: 1.0,
                    by_time_bucket_positive_ratio: 1.0,
                },
                test: FactorSelectionMetrics {
                    n: 10,
                    selected_n: 2,
                    executable_fill_rate: 1.0,
                    rejection_rate: 0.0,
                    total_pnl_after_cost: -1.0,
                    avg_pnl_after_cost: -0.5,
                    sharpe: -1.0,
                    max_drawdown: 1.0,
                    by_symbol_positive_ratio: 0.0,
                    by_time_bucket_positive_ratio: 0.0,
                },
            }],
            aggregates: Vec::new(),
        };

        let stability = build_factor_stability_report(&report, FactorStabilityOptions::default());
        assert_eq!(stability.rows.len(), 1);
        assert_eq!(stability.rows[0].decision, FactorStabilityDecision::Reject);
        assert_eq!(stability.rows[0].reason, "too_few_windows_nonpositive_pnl");
    }

    #[test]
    fn stability_report_watchlists_unstable_icir() {
        let base = Utc::now();
        let metrics = || FactorSelectionMetrics {
            n: 20,
            selected_n: 5,
            executable_fill_rate: 1.0,
            rejection_rate: 0.0,
            total_pnl_after_cost: 2.0,
            avg_pnl_after_cost: 0.4,
            sharpe: 1.0,
            max_drawdown: 0.0,
            by_symbol_positive_ratio: 1.0,
            by_time_bucket_positive_ratio: 1.0,
        };
        let windows = (0..8)
            .map(|idx| FactorWalkForwardWindow {
                window_index: idx,
                train_start: base + chrono::Duration::days(idx as i64),
                train_end: base + chrono::Duration::days(idx as i64 + 1),
                test_start: base + chrono::Duration::days(idx as i64 + 1),
                test_end: base + chrono::Duration::days(idx as i64 + 2),
                factor: "flat_ic_factor".to_string(),
                family: FactorFamily::Alpha,
                layer: ThreeLayerArchive::DirectionProbabilityEdge,
                direction: 1.0,
                threshold: 0.0,
                train_settlement_rank_ic: 0.2,
                train_executable_pnl_rank_ic: 0.2,
                train: metrics(),
                test: metrics(),
            })
            .collect();
        let report = FactorWalkForwardReport {
            options: FactorWalkForwardOptions::default(),
            health: DataHealthReport {
                source_observations: 0,
                v2_rows: 0,
                settlement_label_rows: 0,
                entry_quote_rows: 0,
                exit_quote_rows: 0,
                entry_size_rows: 0,
                exit_size_rows: 0,
                entry_fillable_rows: 0,
                exit_fillable_rows: 0,
                entry_full_depth_fillable_rows: 0,
                exit_full_depth_fillable_rows: 0,
                executable_pnl_rows: 0,
                full_depth_executable_pnl_rows: 0,
                deribit_rows: 0,
                avg_pm_lag_secs: f64::NAN,
                avg_entry_capacity_ratio: f64::NAN,
                avg_exit_capacity_ratio: f64::NAN,
                avg_entry_sweep_slippage_bps: f64::NAN,
                avg_exit_sweep_slippage_bps: f64::NAN,
            },
            windows,
            aggregates: Vec::new(),
        };

        let stability = build_factor_stability_report(&report, FactorStabilityOptions::default());
        assert_eq!(stability.rows.len(), 1);
        assert_eq!(
            stability.rows[0].decision,
            FactorStabilityDecision::Watchlist
        );
        assert_eq!(
            stability.rows[0].reason,
            "positive_pnl_but_low_executable_icir"
        );
    }

    #[test]
    fn combo_v1_uses_train_normalized_factor_scores() {
        let base = Utc::now();
        let mut observations = Vec::new();
        for i in 0..96 {
            let mut obs = base_obs();
            obs.event_id = format!("event-{i}");
            obs.tick_ts = base + chrono::Duration::hours(i);
            obs.model_prob_up = if i % 2 == 0 { 0.80 } else { 0.20 };
            obs.model_edge_up = obs.model_prob_up - obs.pm_up_ask - crypto_fee_cost(obs.pm_up_ask);
            obs.settlement_up = if i % 2 == 0 { 1.0 } else { 0.0 };
            observations.push(obs);
        }

        let report = walk_forward_factor_combo_v1_with_deribit(
            &observations,
            &[],
            base,
            base + chrono::Duration::days(4) - chrono::Duration::seconds(1),
            FactorComboV1Options {
                walk_forward: FactorWalkForwardOptions {
                    review: FactorReviewOptions {
                        stake_usd: 15.0,
                        min_observations: 10,
                        top_quantile: 0.2,
                        max_quote_age_secs: PM_BOOK_MAX_AGE_SECS,
                    },
                    train_window_days: 2,
                    test_window_days: 1,
                    step_days: 1,
                    train_window_hours: None,
                    test_window_hours: None,
                    step_hours: None,
                    top_n: 10,
                    factor_name_filter: Some("side_model_prob".to_string()),
                },
                max_factors_per_family: 1,
                max_total_factors: 2,
                min_abs_train_executable_pnl_rank_ic: 0.01,
            },
        );

        assert!(!report.windows.is_empty());
        assert!(report.aggregate.total_test_pnl_after_cost > 0.0);
        assert!(report
            .windows
            .iter()
            .all(|window| !window.components.is_empty()));
    }
}
