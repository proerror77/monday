//! Descriptive comparisons of completed supervised evaluations.
//!
//! This module neither trains nor selects a model. Native metric definitions
//! remain unchanged; calendar-day statistics have separate names and explicit
//! availability. Source-file hashes are supplied by the artifact reader.

use crate::{
    baselines::CexSupervisedModelEvaluationV2,
    formula_evaluator::{PositionEvaluationPoint, ReturnAccountingBasis},
};
use alpha_domain::{
    canonical_json_hash, CexBaselineModelKindV1, CexResearchContentRefV1, EvaluationMetrics,
    EvaluationProtocolV1, FoldPredictiveMetrics,
};
use chrono::{NaiveDate, Timelike};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MODEL_METRICS_SCHEMA: &str = "cex-supervised-model-metrics-v1";
const DAY_MILLIS: u64 = 86_400_000;
const BPS: f64 = 10_000.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    NoPositionChanges,
    InsufficientCompleteUtcDays,
    PredictiveFoldIsNotOneCompleteUtcDay,
    UndefinedDailyCorrelation,
    ZeroVariance,
    NoDownsideReturns,
    MissingBenchmark,
    SelfBenchmark,
    BenchmarkDayCoverageMismatch,
    NonFiniteStatistic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReportedMetric {
    Available { value: f64 },
    Unavailable { reason: UnavailableReason },
}

impl ReportedMetric {
    fn available(value: f64) -> Self {
        if value.is_finite() {
            Self::Available { value }
        } else {
            Self::unavailable(UnavailableReason::NonFiniteStatistic)
        }
    }

    fn unavailable(reason: UnavailableReason) -> Self {
        Self::Unavailable { reason }
    }

    pub fn value(&self) -> Option<f64> {
        match self {
            Self::Available { value } => Some(*value),
            Self::Unavailable { .. } => None,
        }
    }

    fn status_label(&self) -> String {
        match self {
            Self::Available { .. } => "available".into(),
            Self::Unavailable { reason } => json_label(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricReportDefinitions {
    pub purpose: String,
    pub source_validation: String,
    pub pnl_accounting: String,
    pub daily_period: String,
    pub complete_day_definition: String,
    pub daily_standard_deviation: String,
    pub daily_risk_free_return: f64,
    pub daily_downside_target: f64,
    pub daily_sortino_definition: String,
    pub daily_annualization: String,
    pub daily_information_ratio_definition: String,
}

impl Default for MetricReportDefinitions {
    fn default() -> Self {
        Self {
            purpose: "descriptive_comparison_not_selection_or_deployment_authority".into(),
            source_validation: "evaluation_internal_consistency_and_source_file_sha256;_not_a_settlement_receipt".into(),
            pnl_accounting: "fixed_notional_additive_research_returns_not_account_compounding".into(),
            daily_period: "utc_calendar_day_by_ledger_booking_timestamp".into(),
            complete_day_definition: "one_aligned_observation_at_every_declared_cadence_in_[00:00,next_00:00)".into(),
            daily_standard_deviation: "sample_n_minus_one;_at_least_two_complete_days".into(),
            daily_risk_free_return: 0.0,
            daily_downside_target: 0.0,
            daily_sortino_definition: "mean_daily_net_return_over_root_mean_squared_shortfall_below_zero;_all_complete_days_in_denominator".into(),
            daily_annualization: "none".into(),
            daily_information_ratio_definition: "mean_paired_daily_net_return_difference_over_sample_stddev;_same_mission_protocol_and_ledger_scope".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetricCohort {
    pub mission_id: String,
    pub research_dataset: CexResearchContentRefV1,
    pub walk_forward_partition: CexResearchContentRefV1,
    pub evaluation_policy: CexResearchContentRefV1,
    pub evaluation_protocol: EvaluationProtocolV1,
    pub evaluation_protocol_sha256: String,
    pub return_accounting: ReturnAccountingBasis,
    pub ledger_scope_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DailyModelMetrics {
    pub date: NaiveDate,
    pub observed_rows: usize,
    pub expected_rows: Option<usize>,
    pub complete: bool,
    pub cumulative_net_return: f64,
    pub time_series_ic: ReportedMetric,
    pub time_series_rank_ic: ReportedMetric,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionActions {
    pub entries: usize,
    pub exits: usize,
    pub reversals: usize,
    pub increases: usize,
    pub reductions: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionMetricSummary {
    pub actions: PositionActions,
    pub peak_abs_target_position: f64,
    pub peak_rebalance_fraction: f64,
    pub position_notional_usd: Option<f64>,
    pub peak_abs_target_notional_usd: Option<f64>,
    pub peak_rebalance_notional_usd: Option<f64>,
    pub gross_return_sum: f64,
    pub transaction_cost_sum: f64,
    pub funding_cost_sum: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionAmplitude {
    pub absolute_p50_bps: f64,
    pub absolute_p95_bps: f64,
    pub absolute_max_bps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetricRow {
    pub model_kind: CexBaselineModelKindV1,
    pub candidate: CexResearchContentRefV1,
    pub model_artifact: CexResearchContentRefV1,
    pub backtest_sha256: String,
    pub factor_bank_revision_id: String,
    pub decision_policy_sha256: String,
    pub selected: Option<bool>,
    pub passed: bool,
    pub failure_reasons: Vec<String>,
    pub native: EvaluationMetrics,
    pub native_sharpe_interpretation: ReportedMetric,
    pub prediction_amplitude: PredictionAmplitude,
    pub position: PositionMetricSummary,
    pub days: Vec<DailyModelMetrics>,
    pub complete_utc_days: usize,
    pub daily_icir: ReportedMetric,
    pub daily_rank_icir: ReportedMetric,
    pub daily_net_sharpe: ReportedMetric,
    pub daily_net_sortino: ReportedMetric,
    pub benchmark_candidate: Option<CexResearchContentRefV1>,
    pub daily_information_ratio: ReportedMetric,
}

/// Constructed from a validated evaluation, without retaining its large vectors.
pub struct ModelMetricInput {
    cohort: ModelMetricCohort,
    model: ModelMetricRow,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetricGroup {
    pub cohort: ModelMetricCohort,
    pub models: Vec<ModelMetricRow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexModelMetricsReportV1 {
    pub schema_version: String,
    pub report_id: String,
    pub definitions: MetricReportDefinitions,
    pub benchmark_model_kind: Option<CexBaselineModelKindV1>,
    pub groups: Vec<ModelMetricGroup>,
}

pub fn model_artifact_stem(kind: CexBaselineModelKindV1) -> &'static str {
    match kind {
        CexBaselineModelKindV1::Ridge => "ridge",
        CexBaselineModelKindV1::ShallowCart => "cart",
        CexBaselineModelKindV1::BurnMlp => "burn_mlp",
    }
}

pub fn summarize_model_evaluation(
    evaluation: &CexSupervisedModelEvaluationV2,
    backtest_sha256: &str,
) -> Result<ModelMetricInput, String> {
    evaluation.validate()?;
    if !valid_sha256(backtest_sha256) {
        return Err("model metrics require the source backtest file SHA256".into());
    }
    let candidate = &evaluation.candidate;
    let report = &evaluation.report;
    let protocol = candidate
        .evaluation
        .evaluation_protocol
        .as_ref()
        .ok_or("model metrics require a bound evaluation protocol")?;
    let protocol_sha256 = protocol.content_hash().map_err(|error| error.to_string())?;
    if candidate.evaluation.evaluation_protocol_hash.as_deref() != Some(&protocol_sha256) {
        return Err("model metrics evaluation protocol identity drifted".into());
    }
    validate_ledger(report)?;
    let days = daily_metrics(
        &report.ledger,
        &report.evaluation.metrics.predictive.folds,
        protocol.labels.observation_frequency_millis,
    );
    if days
        .iter()
        .any(|day| !day.cumulative_net_return.is_finite())
    {
        return Err("daily model return aggregation overflowed".into());
    }
    let complete: Vec<_> = days.iter().filter(|day| day.complete).collect();
    let complete_returns: Vec<_> = complete
        .iter()
        .map(|day| day.cumulative_net_return)
        .collect();
    let no_trades = report.evaluation.metrics.trade_count == 0;
    let net_sharpe = if no_trades {
        ReportedMetric::unavailable(UnavailableReason::NoPositionChanges)
    } else {
        mean_over_sample_stddev(&complete_returns)
    };
    let sortino = if no_trades {
        ReportedMetric::unavailable(UnavailableReason::NoPositionChanges)
    } else {
        daily_sortino(&complete_returns)
    };
    let mut absolute_predictions: Vec<_> = report
        .ledger
        .iter()
        .map(|point| point.prediction.abs() * BPS)
        .collect();
    if absolute_predictions.iter().any(|value| !value.is_finite()) {
        return Err("prediction amplitude overflowed its basis-point unit".into());
    }
    absolute_predictions.sort_by(f64::total_cmp);
    let count = absolute_predictions.len();
    let median = if count.is_multiple_of(2) {
        absolute_predictions[count / 2 - 1] / 2.0 + absolute_predictions[count / 2] / 2.0
    } else {
        absolute_predictions[count / 2]
    };
    let p95_index = count - count / 20 - 1;
    let daily_icir = daily_correlation_ratio(&complete, false);
    let daily_rank_icir = daily_correlation_ratio(&complete, true);
    Ok(ModelMetricInput {
        cohort: ModelMetricCohort {
            mission_id: candidate.mission_id.clone(),
            research_dataset: candidate.research_dataset.clone(),
            walk_forward_partition: candidate.walk_forward_partition.clone(),
            evaluation_policy: candidate.evaluation_policy.clone(),
            evaluation_protocol: protocol.clone(),
            evaluation_protocol_sha256: protocol_sha256,
            return_accounting: report.return_accounting,
            ledger_scope_sha256: ledger_scope_sha256(&report.ledger),
        },
        model: ModelMetricRow {
            model_kind: candidate.model_kind,
            candidate: CexResearchContentRefV1 {
                id: candidate.artifact_id.clone(),
                content_sha256: canonical_json_hash(candidate)
                    .map_err(|error| error.to_string())?,
            },
            model_artifact: candidate.model_artifact.clone(),
            backtest_sha256: backtest_sha256.into(),
            factor_bank_revision_id: candidate.factor_bank_revision_id.clone(),
            decision_policy_sha256: candidate.decision_policy.content_hash()?,
            selected: None,
            passed: report.evaluation.passed,
            failure_reasons: report.evaluation.failure_reasons.clone(),
            native: report.evaluation.metrics.clone(),
            native_sharpe_interpretation: if no_trades {
                ReportedMetric::unavailable(UnavailableReason::NoPositionChanges)
            } else {
                ReportedMetric::available(report.evaluation.metrics.net_sharpe)
            },
            prediction_amplitude: PredictionAmplitude {
                absolute_p50_bps: median,
                absolute_p95_bps: absolute_predictions[p95_index],
                absolute_max_bps: absolute_predictions[count - 1],
            },
            position: position_metrics(report, protocol.costs.position_notional_usd)?,
            complete_utc_days: complete.len(),
            daily_icir,
            daily_rank_icir,
            daily_net_sharpe: net_sharpe,
            daily_net_sortino: sortino,
            benchmark_candidate: None,
            daily_information_ratio: ReportedMetric::unavailable(
                UnavailableReason::MissingBenchmark,
            ),
            days,
        },
    })
}

impl CexModelMetricsReportV1 {
    pub fn new(
        inputs: Vec<ModelMetricInput>,
        selections: &BTreeMap<String, CexResearchContentRefV1>,
        benchmark_model_kind: Option<CexBaselineModelKindV1>,
    ) -> Result<Self, String> {
        if inputs.is_empty() {
            return Err("model comparison requires at least one evaluated model".into());
        }
        let mut groups = BTreeMap::<String, ModelMetricGroup>::new();
        let mut candidates = BTreeSet::new();
        for input in inputs {
            if !candidates.insert(input.model.candidate.id.clone()) {
                return Err("model comparison contains a duplicate candidate".into());
            }
            let group = groups
                .entry(input.cohort.mission_id.clone())
                .or_insert_with(|| ModelMetricGroup {
                    cohort: input.cohort.clone(),
                    models: Vec::new(),
                });
            if group.cohort != input.cohort {
                return Err("models in one Mission have different evaluation cohorts".into());
            }
            group.models.push(input.model);
        }
        if selections
            .keys()
            .any(|mission| !groups.contains_key(mission))
        {
            return Err("selection references a Mission outside the comparison".into());
        }
        for (mission, group) in &mut groups {
            group.models.sort_by(|left, right| {
                model_artifact_stem(left.model_kind)
                    .cmp(model_artifact_stem(right.model_kind))
                    .then_with(|| left.candidate.id.cmp(&right.candidate.id))
            });
            if let Some(selected) = selections.get(mission) {
                selected.validate().map_err(|error| error.to_string())?;
                if !group
                    .models
                    .iter()
                    .any(|model| model.candidate == *selected)
                {
                    return Err("selection candidate identity is absent from the comparison".into());
                }
                for model in &mut group.models {
                    model.selected = Some(model.candidate == *selected);
                }
            }
            if let Some(kind) = benchmark_model_kind {
                let matches: Vec<_> = group
                    .models
                    .iter()
                    .enumerate()
                    .filter_map(|(index, model)| (model.model_kind == kind).then_some(index))
                    .collect();
                if matches.len() > 1 {
                    return Err("benchmark model kind is ambiguous within a Mission".into());
                }
                if let Some(&index) = matches.first() {
                    let benchmark = group.models[index].clone();
                    for model in &mut group.models {
                        model.benchmark_candidate = Some(benchmark.candidate.clone());
                        model.daily_information_ratio =
                            benchmark_information_ratio(model, &benchmark);
                    }
                }
            }
        }
        let mut result = Self {
            schema_version: MODEL_METRICS_SCHEMA.into(),
            report_id: String::new(),
            definitions: MetricReportDefinitions::default(),
            benchmark_model_kind,
            groups: groups.into_values().collect(),
        };
        result.report_id = format!(
            "cex-model-metrics-{}",
            canonical_json_hash(&result).map_err(|error| error.to_string())?
        );
        Ok(result)
    }

    pub fn model_count(&self) -> usize {
        self.groups.iter().map(|group| group.models.len()).sum()
    }

    pub fn to_csv(&self) -> String {
        let headers = [
            "mission_id",
            "model_kind",
            "candidate_id",
            "candidate_sha256",
            "backtest_sha256",
            "selected",
            "benchmark_candidate_id",
            "rows",
            "folds",
            "ic",
            "rank_ic",
            "fold_icir",
            "fold_rank_icir",
            "positive_ic_ratio",
            "native_net_return",
            "native_net_sharpe",
            "native_sharpe_status",
            "native_max_fold_drawdown",
            "turnover",
            "position_changes",
            "prediction_abs_p50_bps",
            "prediction_abs_p95_bps",
            "prediction_abs_max_bps",
            "position_notional_usd",
            "peak_abs_target_notional_usd",
            "peak_rebalance_notional_usd",
            "entries",
            "exits",
            "reversals",
            "increases",
            "reductions",
            "gross_return_sum",
            "transaction_cost_sum",
            "funding_cost_sum",
            "complete_utc_days",
            "observed_utc_days",
            "daily_icir",
            "daily_icir_status",
            "daily_rank_icir",
            "daily_rank_icir_status",
            "daily_net_sharpe",
            "daily_net_sharpe_status",
            "daily_net_sortino",
            "daily_net_sortino_status",
            "strategy_information_ratio",
            "strategy_information_ratio_status",
            "return_accounting",
            "evaluation_protocol_sha256",
            "native_icir_definition",
            "native_sharpe_definition",
            "passed",
            "failure_reasons",
        ];
        let mut output = headers.join(",");
        output.push('\n');
        for group in &self.groups {
            for row in &group.models {
                let predictive = &row.native.predictive;
                let position = &row.position;
                let fields = vec![
                    group.cohort.mission_id.clone(),
                    json_label(&row.model_kind),
                    row.candidate.id.clone(),
                    row.candidate.content_sha256.clone(),
                    row.backtest_sha256.clone(),
                    row.selected
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    row.benchmark_candidate
                        .as_ref()
                        .map(|value| value.id.clone())
                        .unwrap_or_default(),
                    row.native.row_count.to_string(),
                    row.native.folds.len().to_string(),
                    optional_number(predictive.time_series_ic),
                    optional_number(predictive.time_series_rank_ic),
                    optional_number(predictive.time_series_icir),
                    optional_number(predictive.time_series_rank_icir),
                    predictive.positive_ic_ratio.to_string(),
                    row.native.cumulative_net_return.to_string(),
                    row.native.net_sharpe.to_string(),
                    row.native_sharpe_interpretation.status_label(),
                    row.native.max_drawdown.to_string(),
                    row.native.total_turnover.to_string(),
                    row.native.trade_count.to_string(),
                    row.prediction_amplitude.absolute_p50_bps.to_string(),
                    row.prediction_amplitude.absolute_p95_bps.to_string(),
                    row.prediction_amplitude.absolute_max_bps.to_string(),
                    optional_number(position.position_notional_usd),
                    optional_number(position.peak_abs_target_notional_usd),
                    optional_number(position.peak_rebalance_notional_usd),
                    position.actions.entries.to_string(),
                    position.actions.exits.to_string(),
                    position.actions.reversals.to_string(),
                    position.actions.increases.to_string(),
                    position.actions.reductions.to_string(),
                    position.gross_return_sum.to_string(),
                    position.transaction_cost_sum.to_string(),
                    position.funding_cost_sum.to_string(),
                    row.complete_utc_days.to_string(),
                    row.days.len().to_string(),
                    optional_number(row.daily_icir.value()),
                    row.daily_icir.status_label(),
                    optional_number(row.daily_rank_icir.value()),
                    row.daily_rank_icir.status_label(),
                    optional_number(row.daily_net_sharpe.value()),
                    row.daily_net_sharpe.status_label(),
                    optional_number(row.daily_net_sortino.value()),
                    row.daily_net_sortino.status_label(),
                    optional_number(row.daily_information_ratio.value()),
                    row.daily_information_ratio.status_label(),
                    json_label(&group.cohort.return_accounting),
                    group.cohort.evaluation_protocol_sha256.clone(),
                    json_label(&group.cohort.evaluation_protocol.metrics.icir),
                    json_label(&group.cohort.evaluation_protocol.metrics.sharpe),
                    row.passed.to_string(),
                    serde_json::to_string(&row.failure_reasons).expect("string list serialization"),
                ];
                debug_assert_eq!(headers.len(), fields.len());
                output.push_str(
                    &fields
                        .iter()
                        .map(|field| csv_escape(field))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                output.push('\n');
            }
        }
        output
    }
}

fn validate_ledger(
    report: &crate::formula_evaluator::PositionEvaluationReport,
) -> Result<(), String> {
    if report.ledger.is_empty() || report.ledger.len() != report.evaluation.metrics.row_count {
        return Err("model ledger row count differs from native evaluation".into());
    }
    let mut fold_rows = BTreeMap::new();
    let mut fold_returns = BTreeMap::<usize, f64>::new();
    let mut previous_index = None;
    let mut equity = 1.0_f64;
    for point in &report.ledger {
        if previous_index.is_some_and(|index| point.row_index <= index)
            || [
                point.prediction,
                point.target_position,
                point.gross_return,
                point.transaction_cost,
                point.funding_cost,
                point.net_return,
                point.equity,
            ]
            .iter()
            .any(|value| !value.is_finite())
        {
            return Err("model ledger has non-finite values or unordered row indices".into());
        }
        equity += point.net_return;
        if !close_number(
            point.net_return,
            point.gross_return - point.transaction_cost - point.funding_cost,
        ) || !close_number(point.equity, equity)
        {
            return Err("model ledger accounting differs from its costs or additive equity".into());
        }
        *fold_rows.entry(point.fold_index).or_insert(0_usize) += 1;
        *fold_returns.entry(point.fold_index).or_default() += point.net_return;
        previous_index = Some(point.row_index);
    }
    if fold_rows.len() != report.evaluation.metrics.folds.len()
        || report
            .evaluation
            .metrics
            .folds
            .iter()
            .any(|fold| fold_rows.get(&fold.fold_index).copied() != Some(fold.row_count))
    {
        return Err("model ledger folds differ from native evaluation".into());
    }
    for fold in &report.evaluation.metrics.folds {
        let total = fold_returns[&fold.fold_index];
        if !close_number(total, fold.cumulative_net_return)
            || !close_number(total / fold.row_count as f64, fold.mean_net_return)
        {
            return Err("model ledger accounting differs from native fold returns".into());
        }
    }
    Ok(())
}

fn close_number(left: f64, right: f64) -> bool {
    left.is_finite()
        && right.is_finite()
        && (left - right).abs() <= 1.0e-10 * left.abs().max(right.abs()).max(1.0)
}

fn ledger_scope_sha256(ledger: &[PositionEvaluationPoint]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"cex-model-ledger-scope-v1\0");
    for point in ledger {
        hash.update((point.row_index as u64).to_le_bytes());
        hash.update(point.series_id.to_le_bytes());
        hash.update((point.fold_index as u64).to_le_bytes());
        hash.update(point.available_time.timestamp().to_le_bytes());
        hash.update(point.available_time.timestamp_subsec_nanos().to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn position_metrics(
    report: &crate::formula_evaluator::PositionEvaluationReport,
    notional: f64,
) -> Result<PositionMetricSummary, String> {
    let mut actions = PositionActions::default();
    let mut previous = 0.0_f64;
    let mut previous_group = None;
    let mut peak_target = 0.0_f64;
    let mut peak_rebalance = 0.0_f64;
    let mut total_turnover = 0.0_f64;
    for (index, point) in report.ledger.iter().enumerate() {
        let group = (point.fold_index, point.series_id);
        if previous_group != Some(group) {
            previous = 0.0;
        }
        let current = point.target_position;
        let delta = (current - previous).abs();
        total_turnover += delta;
        peak_target = peak_target.max(current.abs());
        peak_rebalance = peak_rebalance.max(delta);
        if delta > f64::EPSILON {
            if previous.abs() <= f64::EPSILON {
                actions.entries += 1;
            } else if current.abs() <= f64::EPSILON {
                actions.exits += 1;
            } else if current.signum() != previous.signum() {
                actions.reversals += 1;
            } else if current.abs() > previous.abs() {
                actions.increases += 1;
            } else {
                actions.reductions += 1;
            }
        }
        let terminal = report
            .ledger
            .get(index + 1)
            .is_none_or(|next| (next.fold_index, next.series_id) != group);
        if report.return_accounting == ReturnAccountingBasis::OneStepLabel
            && terminal
            && current.abs() > f64::EPSILON
        {
            actions.exits += 1;
            peak_rebalance = peak_rebalance.max(current.abs());
            total_turnover += current.abs();
        }
        previous = current;
        previous_group = Some(group);
    }
    if actions.entries + actions.exits + actions.reversals + actions.increases + actions.reductions
        != report.evaluation.metrics.trade_count
        || !close_number(total_turnover, report.evaluation.metrics.total_turnover)
    {
        return Err("model position changes differ from native turnover or counts".into());
    }
    let position_notional = (notional > 0.0).then_some(notional);
    let result = PositionMetricSummary {
        actions,
        peak_abs_target_position: peak_target,
        peak_rebalance_fraction: peak_rebalance,
        position_notional_usd: position_notional,
        peak_abs_target_notional_usd: position_notional.map(|value| value * peak_target),
        peak_rebalance_notional_usd: position_notional.map(|value| value * peak_rebalance),
        gross_return_sum: report.ledger.iter().map(|point| point.gross_return).sum(),
        transaction_cost_sum: report
            .ledger
            .iter()
            .map(|point| point.transaction_cost)
            .sum(),
        funding_cost_sum: report.ledger.iter().map(|point| point.funding_cost).sum(),
    };
    if [
        result.peak_abs_target_notional_usd.unwrap_or(0.0),
        result.peak_rebalance_notional_usd.unwrap_or(0.0),
        result.gross_return_sum,
        result.transaction_cost_sum,
        result.funding_cost_sum,
    ]
    .iter()
    .any(|value| !value.is_finite())
    {
        return Err("model position metric aggregation overflowed".into());
    }
    Ok(result)
}

fn daily_metrics(
    ledger: &[PositionEvaluationPoint],
    folds: &[FoldPredictiveMetrics],
    frequency_millis: u64,
) -> Vec<DailyModelMetrics> {
    let mut days = BTreeMap::<NaiveDate, Vec<&PositionEvaluationPoint>>::new();
    for point in ledger {
        days.entry(point.available_time.date_naive())
            .or_default()
            .push(point);
    }
    let expected_rows = (frequency_millis > 0 && DAY_MILLIS.is_multiple_of(frequency_millis))
        .then(|| (DAY_MILLIS / frequency_millis) as usize);
    days.into_iter()
        .map(|(date, mut points)| {
            points.sort_by_key(|point| point.available_time);
            let complete = expected_rows == Some(points.len())
                && points.iter().enumerate().all(|(index, point)| {
                    let time = point.available_time.time();
                    let millis = u64::from(time.num_seconds_from_midnight()) * 1_000
                        + u64::from(time.nanosecond() / 1_000_000);
                    time.nanosecond().is_multiple_of(1_000_000)
                        && millis == index as u64 * frequency_millis
                });
            let fold = complete.then(|| points[0].fold_index).and_then(|index| {
                folds
                    .iter()
                    .find(|fold| fold.fold_index == index && fold.row_count == points.len())
                    .filter(|_| points.iter().all(|point| point.fold_index == index))
            });
            let correlation = |rank: bool| match fold {
                Some(fold) => {
                    let value = if rank {
                        fold.time_series_rank_ic
                    } else {
                        fold.time_series_ic
                    };
                    value.map(ReportedMetric::available).unwrap_or_else(|| {
                        ReportedMetric::unavailable(UnavailableReason::UndefinedDailyCorrelation)
                    })
                }
                None => ReportedMetric::unavailable(
                    UnavailableReason::PredictiveFoldIsNotOneCompleteUtcDay,
                ),
            };
            DailyModelMetrics {
                date,
                observed_rows: points.len(),
                expected_rows,
                complete,
                cumulative_net_return: points.iter().map(|point| point.net_return).sum(),
                time_series_ic: correlation(false),
                time_series_rank_ic: correlation(true),
            }
        })
        .collect()
}

fn daily_correlation_ratio(days: &[&DailyModelMetrics], rank: bool) -> ReportedMetric {
    if days.len() < 2 {
        return ReportedMetric::unavailable(UnavailableReason::InsufficientCompleteUtcDays);
    }
    let mut values = Vec::with_capacity(days.len());
    for day in days {
        let metric = if rank {
            &day.time_series_rank_ic
        } else {
            &day.time_series_ic
        };
        match metric {
            ReportedMetric::Available { value } => values.push(*value),
            unavailable => return unavailable.clone(),
        }
    }
    mean_over_sample_stddev(&values)
}

fn mean_over_sample_stddev(values: &[f64]) -> ReportedMetric {
    if values.len() < 2 {
        return ReportedMetric::unavailable(UnavailableReason::InsufficientCompleteUtcDays);
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    if !mean.is_finite() || !variance.is_finite() {
        return ReportedMetric::unavailable(UnavailableReason::NonFiniteStatistic);
    }
    if variance == 0.0 {
        return ReportedMetric::unavailable(UnavailableReason::ZeroVariance);
    }
    ReportedMetric::available(mean / variance.sqrt())
}

fn daily_sortino(values: &[f64]) -> ReportedMetric {
    if values.len() < 2 {
        return ReportedMetric::unavailable(UnavailableReason::InsufficientCompleteUtcDays);
    }
    let downside = (values
        .iter()
        .map(|value| value.min(0.0).powi(2))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt();
    if !downside.is_finite() {
        return ReportedMetric::unavailable(UnavailableReason::NonFiniteStatistic);
    }
    if downside == 0.0 {
        return ReportedMetric::unavailable(UnavailableReason::NoDownsideReturns);
    }
    ReportedMetric::available(values.iter().sum::<f64>() / values.len() as f64 / downside)
}

fn benchmark_information_ratio(
    model: &ModelMetricRow,
    benchmark: &ModelMetricRow,
) -> ReportedMetric {
    if model.candidate == benchmark.candidate {
        return ReportedMetric::unavailable(UnavailableReason::SelfBenchmark);
    }
    paired_daily_information_ratio(&model.days, &benchmark.days)
}

fn paired_daily_information_ratio(
    model_days: &[DailyModelMetrics],
    benchmark_days: &[DailyModelMetrics],
) -> ReportedMetric {
    let days: Vec<_> = model_days.iter().filter(|day| day.complete).collect();
    let reference_days: Vec<_> = benchmark_days.iter().filter(|day| day.complete).collect();
    if days
        .iter()
        .map(|day| day.date)
        .ne(reference_days.iter().map(|day| day.date))
    {
        return ReportedMetric::unavailable(UnavailableReason::BenchmarkDayCoverageMismatch);
    }
    let active_returns: Vec<_> = days
        .iter()
        .zip(reference_days)
        .map(|(day, reference)| day.cumulative_net_return - reference.cumulative_net_return)
        .collect();
    mean_over_sample_stddev(&active_returns)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn optional_number(value: Option<f64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn json_label(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .expect("metric enum serialization")
        .as_str()
        .expect("metric enum has a string representation")
        .to_string()
}

fn csv_escape(value: &str) -> String {
    let protected;
    let value =
        if value.starts_with(['=', '+', '-', '@', '\t', '\r']) && value.parse::<f64>().is_err() {
            protected = format!("'{value}");
            protected.as_str()
        } else {
            value
        };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn point(index: usize, date: u32, hour: u32, fold: usize, net: f64) -> PositionEvaluationPoint {
        PositionEvaluationPoint {
            row_index: index,
            series_id: 1,
            available_time: Utc.with_ymd_and_hms(2026, 9, date, hour, 0, 0).unwrap(),
            fold_index: fold,
            prediction: 0.001,
            target_position: 0.0,
            gross_return: net,
            transaction_cost: 0.0,
            funding_cost: 0.0,
            net_return: net,
            equity: 1.0 + net,
        }
    }

    #[test]
    fn daily_coverage_requires_each_declared_clock_point() {
        let ledger = vec![point(0, 1, 0, 1, 0.01), point(1, 1, 12, 1, -0.02)];
        let fold = FoldPredictiveMetrics {
            fold_index: 1,
            row_count: 2,
            time_series_ic: Some(0.2),
            time_series_rank_ic: Some(0.3),
        };
        let days = daily_metrics(&ledger, std::slice::from_ref(&fold), 43_200_000);
        assert_eq!(days.len(), 1);
        assert!(days[0].complete);
        assert_eq!(days[0].cumulative_net_return, -0.01);
        assert_eq!(days[0].time_series_ic.value(), Some(0.2));
        assert!(!daily_metrics(&ledger[..1], std::slice::from_ref(&fold), 43_200_000)[0].complete);
        let duplicate = vec![ledger[0].clone(), ledger[0].clone()];
        assert!(!daily_metrics(&duplicate, &[fold], 43_200_000)[0].complete);
        assert!(!daily_metrics(&ledger, &[], 0)[0].complete);
    }

    #[test]
    fn daily_ic_cannot_be_invented_from_folds_spanning_multiple_dates() {
        let ledger = vec![point(0, 1, 0, 1, 0.01), point(1, 1, 12, 1, -0.02)];
        let fold = FoldPredictiveMetrics {
            fold_index: 1,
            row_count: 4,
            time_series_ic: Some(0.2),
            time_series_rank_ic: Some(0.3),
        };
        let days = daily_metrics(&ledger, &[fold], 43_200_000);
        assert!(days[0].complete);
        assert_eq!(
            days[0].time_series_ic,
            ReportedMetric::unavailable(UnavailableReason::PredictiveFoldIsNotOneCompleteUtcDay,)
        );
    }

    #[test]
    fn daily_ratios_use_sample_deviation_without_annualization_or_epsilon_success() {
        assert_eq!(mean_over_sample_stddev(&[1.0, 2.0, 3.0]).value(), Some(2.0));
        assert_eq!(
            mean_over_sample_stddev(&[0.0, 0.0]),
            ReportedMetric::unavailable(UnavailableReason::ZeroVariance,)
        );
        assert_eq!(
            mean_over_sample_stddev(&[1.0]),
            ReportedMetric::unavailable(UnavailableReason::InsufficientCompleteUtcDays,)
        );
        assert_eq!(
            daily_sortino(&[0.1, 0.2]),
            ReportedMetric::unavailable(UnavailableReason::NoDownsideReturns,)
        );
        assert_eq!(daily_sortino(&[-1.0, 1.0]).value(), Some(0.0));
        assert_eq!(
            mean_over_sample_stddev(&[-1.0e200, 1.0e200]),
            ReportedMetric::unavailable(UnavailableReason::NonFiniteStatistic,)
        );
    }

    #[test]
    fn ledger_identity_includes_clock_and_series_but_not_model_predictions() {
        let ledger = vec![point(0, 1, 0, 1, 0.01), point(1, 1, 12, 1, -0.02)];
        let mut changed = ledger.clone();
        changed[0].prediction = 0.5;
        assert_eq!(ledger_scope_sha256(&ledger), ledger_scope_sha256(&changed));
        changed[0].series_id = 2;
        assert_ne!(ledger_scope_sha256(&ledger), ledger_scope_sha256(&changed));
        changed = ledger.clone();
        changed[1].available_time += chrono::Duration::milliseconds(1);
        assert_ne!(ledger_scope_sha256(&ledger), ledger_scope_sha256(&changed));
    }

    #[test]
    fn csv_quotes_fields_without_losing_failure_details() {
        assert_eq!(csv_escape("ridge"), "ridge");
        assert_eq!(csv_escape("a,\"b\"\nc"), "\"a,\"\"b\"\"\nc\"");
        assert_eq!(csv_escape("=1+2"), "'=1+2");
        assert_eq!(csv_escape("-0.01"), "-0.01");
    }

    #[test]
    fn information_ratio_uses_paired_active_returns_without_intersecting_days() {
        let day = |date, value| DailyModelMetrics {
            date: NaiveDate::from_ymd_opt(2026, 9, date).unwrap(),
            observed_rows: 2,
            expected_rows: Some(2),
            complete: true,
            cumulative_net_return: value,
            time_series_ic: ReportedMetric::available(0.1),
            time_series_rank_ic: ReportedMetric::available(0.2),
        };
        let model = vec![day(1, 0.02), day(2, 0.04), day(3, 0.06)];
        let benchmark = vec![day(1, 0.01), day(2, 0.02), day(3, 0.03)];
        let value = paired_daily_information_ratio(&model, &benchmark)
            .value()
            .unwrap();
        assert!((value - 2.0).abs() < 1.0e-12);
        assert_eq!(
            paired_daily_information_ratio(&model, &benchmark[..2]),
            ReportedMetric::unavailable(UnavailableReason::BenchmarkDayCoverageMismatch)
        );
        assert_eq!(
            paired_daily_information_ratio(&model, &model),
            ReportedMetric::unavailable(UnavailableReason::ZeroVariance)
        );
    }
}
