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
}
