//! Deterministic factor evaluation contracts and gates.

use hft_factor_bank::{FactorAsset, FactorMetrics};
use hft_research_manifest::{ManifestError, ManifestRef};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FactorEvalError {
    #[error("dataset manifest is invalid: {0}")]
    InvalidDatasetManifest(#[from] ManifestError),
    #[error("available_time coverage is required for evaluation")]
    MissingAvailableTime,
    #[error("replay csv has no rows")]
    EmptyReplay,
    #[error("replay csv header is missing column: {0}")]
    MissingReplayColumn(String),
    #[error("replay csv value is invalid in {column}: {value}")]
    InvalidReplayValue { column: String, value: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationInput {
    pub dataset_manifest: ManifestRef,
    pub has_available_time: bool,
    pub sample_count: u64,
    pub metrics: FactorMetrics,
}

impl EvaluationInput {
    pub fn validate(&self) -> Result<(), FactorEvalError> {
        self.dataset_manifest.validate()?;
        if !self.has_available_time {
            return Err(FactorEvalError::MissingAvailableTime);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationThresholds {
    pub min_sample_count: u64,
    pub min_rank_ic: f64,
    pub min_net_sharpe: f64,
    pub max_drawdown: f64,
    pub max_correlation: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvaluationFailure {
    InvalidInput,
    InsufficientSample,
    MissingRankIc,
    MissingNetSharpe,
    MissingMaxDrawdown,
    RankIcBelowFloor,
    NetSharpeBelowFloor,
    MaxDrawdownAboveCeiling,
    CorrelationTooHigh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationDecision {
    pub passed: bool,
    pub failures: Vec<EvaluationFailure>,
}

pub fn evaluate_factor(
    input: &EvaluationInput,
    thresholds: &EvaluationThresholds,
    max_observed_correlation: Option<f64>,
) -> EvaluationDecision {
    let mut failures = Vec::new();

    if input.validate().is_err() {
        failures.push(EvaluationFailure::InvalidInput);
    }
    if input.sample_count < thresholds.min_sample_count {
        failures.push(EvaluationFailure::InsufficientSample);
    }

    match input.metrics.rank_ic {
        Some(value) if value >= thresholds.min_rank_ic => {}
        Some(_) => failures.push(EvaluationFailure::RankIcBelowFloor),
        None => failures.push(EvaluationFailure::MissingRankIc),
    }
    match input.metrics.net_sharpe {
        Some(value) if value >= thresholds.min_net_sharpe => {}
        Some(_) => failures.push(EvaluationFailure::NetSharpeBelowFloor),
        None => failures.push(EvaluationFailure::MissingNetSharpe),
    }
    match input.metrics.max_drawdown {
        Some(value) if value <= thresholds.max_drawdown => {}
        Some(_) => failures.push(EvaluationFailure::MaxDrawdownAboveCeiling),
        None => failures.push(EvaluationFailure::MissingMaxDrawdown),
    }
    if max_observed_correlation
        .map(|value| value > thresholds.max_correlation)
        .unwrap_or(false)
    {
        failures.push(EvaluationFailure::CorrelationTooHigh);
    }

    EvaluationDecision {
        passed: failures.is_empty(),
        failures,
    }
}

pub fn asset_metrics_input(asset: &FactorAsset, dataset_manifest: ManifestRef) -> EvaluationInput {
    EvaluationInput {
        dataset_manifest,
        has_available_time: true,
        sample_count: 0,
        metrics: asset.metrics.clone(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCsvConfig {
    pub dataset_manifest: ManifestRef,
    pub signal_column: String,
    pub label_column: String,
    pub available_time_column: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayEvaluationReport {
    pub metrics: FactorMetrics,
    pub input: EvaluationInput,
    pub decision: EvaluationDecision,
}

pub fn evaluate_replay_csv(
    csv: &str,
    config: &ReplayCsvConfig,
    thresholds: &EvaluationThresholds,
    max_observed_correlation: Option<f64>,
) -> Result<ReplayEvaluationReport, FactorEvalError> {
    config.dataset_manifest.validate()?;
    let rows = parse_replay_rows(csv, config)?;
    if rows.is_empty() {
        return Err(FactorEvalError::EmptyReplay);
    }

    let has_available_time = rows.iter().all(|row| row.available_time);
    let signals = rows.iter().map(|row| row.signal).collect::<Vec<_>>();
    let labels = rows.iter().map(|row| row.label).collect::<Vec<_>>();
    let pnl = rows
        .iter()
        .map(|row| {
            if row.signal >= 0.0 {
                row.label
            } else {
                -row.label
            }
        })
        .collect::<Vec<_>>();
    let mut custom = std::collections::BTreeMap::new();
    custom.insert("mean_return".to_string(), mean(&pnl));

    let metrics = FactorMetrics {
        rank_ic: rank_correlation(&signals, &labels),
        icir: None,
        net_sharpe: sharpe(&pnl),
        max_drawdown: Some(max_drawdown(&pnl)),
        turnover: turnover(&signals),
        custom,
    };
    let input = EvaluationInput {
        dataset_manifest: config.dataset_manifest.clone(),
        has_available_time,
        sample_count: rows.len() as u64,
        metrics: metrics.clone(),
    };
    let decision = evaluate_factor(&input, thresholds, max_observed_correlation);

    Ok(ReplayEvaluationReport {
        metrics,
        input,
        decision,
    })
}

struct ReplayRow {
    signal: f64,
    label: f64,
    available_time: bool,
}

fn parse_replay_rows(
    csv: &str,
    config: &ReplayCsvConfig,
) -> Result<Vec<ReplayRow>, FactorEvalError> {
    let mut lines = csv.lines().filter(|line| !line.trim().is_empty());
    let header = lines.next().ok_or(FactorEvalError::EmptyReplay)?;
    // ponytail: simple CSV for generated/replay fixtures; use csv crate when quoted fields appear.
    let columns = header.split(',').map(str::trim).collect::<Vec<_>>();
    let signal_idx = column_index(&columns, &config.signal_column)?;
    let label_idx = column_index(&columns, &config.label_column)?;
    let available_idx = config
        .available_time_column
        .as_ref()
        .map(|name| column_index(&columns, name))
        .transpose()?;

    lines
        .map(|line| {
            let values = line.split(',').map(str::trim).collect::<Vec<_>>();
            Ok(ReplayRow {
                signal: parse_f64(values.get(signal_idx), &config.signal_column)?,
                label: parse_f64(values.get(label_idx), &config.label_column)?,
                available_time: match available_idx {
                    Some(idx) => parse_bool(
                        values.get(idx),
                        config.available_time_column.as_deref().unwrap(),
                    )?,
                    None => true,
                },
            })
        })
        .collect()
}

fn column_index(columns: &[&str], name: &str) -> Result<usize, FactorEvalError> {
    columns
        .iter()
        .position(|column| *column == name)
        .ok_or_else(|| FactorEvalError::MissingReplayColumn(name.to_string()))
}

fn parse_f64(value: Option<&&str>, column: &str) -> Result<f64, FactorEvalError> {
    let value = value.copied().unwrap_or("");
    value
        .parse::<f64>()
        .map_err(|_| FactorEvalError::InvalidReplayValue {
            column: column.to_string(),
            value: value.to_string(),
        })
}

fn parse_bool(value: Option<&&str>, column: &str) -> Result<bool, FactorEvalError> {
    match value.copied().unwrap_or("") {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        value => Err(FactorEvalError::InvalidReplayValue {
            column: column.to_string(),
            value: value.to_string(),
        }),
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn sharpe(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let avg = mean(values);
    let variance = values
        .iter()
        .map(|value| (value - avg).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64;
    let std = variance.sqrt();
    (std > 0.0).then(|| avg / std * (values.len() as f64).sqrt())
}

fn max_drawdown(returns: &[f64]) -> f64 {
    let mut equity = 1.0;
    let mut peak = equity;
    let mut max_dd = 0.0;
    for value in returns {
        equity += value;
        if equity > peak {
            peak = equity;
        }
        if peak > 0.0 {
            let dd = (peak - equity) / peak;
            if dd > max_dd {
                max_dd = dd;
            }
        }
    }
    max_dd
}

fn turnover(signals: &[f64]) -> Option<f64> {
    if signals.len() < 2 {
        return None;
    }
    let changes = signals
        .windows(2)
        .filter(|window| window[0].signum() != window[1].signum())
        .count();
    Some(changes as f64 / (signals.len() - 1) as f64)
}

fn rank_correlation(left: &[f64], right: &[f64]) -> Option<f64> {
    if left.len() != right.len() || left.len() < 2 {
        return None;
    }
    pearson(&ranks(left), &ranks(right))
}

fn ranks(values: &[f64]) -> Vec<f64> {
    let mut indexed = values.iter().copied().enumerate().collect::<Vec<_>>();
    indexed.sort_by(|left, right| left.1.total_cmp(&right.1));
    let mut ranks = vec![0.0; values.len()];
    let mut start = 0;
    while start < indexed.len() {
        let mut end = start + 1;
        while end < indexed.len() && indexed[end].1 == indexed[start].1 {
            end += 1;
        }
        let rank = (start + 1 + end) as f64 / 2.0;
        for idx in start..end {
            ranks[indexed[idx].0] = rank;
        }
        start = end;
    }
    ranks
}

fn pearson(left: &[f64], right: &[f64]) -> Option<f64> {
    let left_mean = mean(left);
    let right_mean = mean(right);
    let mut numerator = 0.0;
    let mut left_ss = 0.0;
    let mut right_ss = 0.0;
    for (left, right) in left.iter().zip(right) {
        let left_delta = left - left_mean;
        let right_delta = right - right_mean;
        numerator += left_delta * right_delta;
        left_ss += left_delta.powi(2);
        right_ss += right_delta.powi(2);
    }
    let denominator = left_ss.sqrt() * right_ss.sqrt();
    (denominator > 0.0).then_some(numerator / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_research_manifest::{ManifestId, ManifestRef};
    use std::collections::BTreeMap;

    fn metrics() -> FactorMetrics {
        FactorMetrics {
            rank_ic: Some(0.04),
            icir: Some(1.3),
            net_sharpe: Some(1.5),
            max_drawdown: Some(0.04),
            turnover: None,
            custom: BTreeMap::new(),
        }
    }

    fn input() -> EvaluationInput {
        EvaluationInput {
            dataset_manifest: ManifestRef::new(ManifestId::new("data-1").unwrap(), "data_manifest")
                .unwrap(),
            has_available_time: true,
            sample_count: 100,
            metrics: metrics(),
        }
    }

    fn thresholds() -> EvaluationThresholds {
        EvaluationThresholds {
            min_sample_count: 50,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown: 0.05,
            max_correlation: 0.8,
        }
    }

    #[test]
    fn passes_when_metrics_and_pit_requirements_pass() {
        assert_eq!(
            evaluate_factor(&input(), &thresholds(), Some(0.2)),
            EvaluationDecision {
                passed: true,
                failures: vec![]
            }
        );
    }

    #[test]
    fn rejects_missing_available_time() {
        let mut input = input();
        input.has_available_time = false;
        let decision = evaluate_factor(&input, &thresholds(), None);
        assert!(!decision.passed);
        assert!(decision.failures.contains(&EvaluationFailure::InvalidInput));
    }

    #[test]
    fn replay_csv_produces_real_metrics_and_gate_decision() {
        let csv = "oi_delta_5m,forward_return,available_time
1.0,0.030,true
0.5,0.020,true
-0.5,-0.020,true
-1.0,-0.030,true
";
        let report = evaluate_replay_csv(
            csv,
            &ReplayCsvConfig {
                dataset_manifest: ManifestRef::new(
                    ManifestId::new("data-replay-1").unwrap(),
                    "data_manifest",
                )
                .unwrap(),
                signal_column: "oi_delta_5m".to_string(),
                label_column: "forward_return".to_string(),
                available_time_column: Some("available_time".to_string()),
            },
            &EvaluationThresholds {
                min_sample_count: 4,
                min_rank_ic: 0.03,
                min_net_sharpe: 1.0,
                max_drawdown: 0.05,
                max_correlation: 0.8,
            },
            Some(0.2),
        )
        .unwrap();

        assert!(report.decision.passed);
        assert_eq!(report.input.sample_count, 4);
        assert!((report.metrics.rank_ic.unwrap() - 1.0).abs() < 1e-12);
        assert_eq!(report.metrics.max_drawdown, Some(0.0));
    }
}
