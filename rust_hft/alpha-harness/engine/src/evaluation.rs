use alpha_domain::{EvaluationProtocolV1, ResearchMission};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum EvaluationError {
    #[error(
        "walk-forward configuration must use positive train, validation, holdout, and fold sizes"
    )]
    InvalidConfiguration,
    #[error("dataset does not contain enough rows for the requested folds")]
    InsufficientRows,
    #[error("dataset available_time is not monotonic")]
    NonMonotonicAvailability,
    #[error("dataset contains a non-finite numeric value")]
    NonFiniteValue,
    #[error("dataset feature schema is empty, inconsistent, or contains an invalid field")]
    InvalidFeatureSchema,
    #[error("dataset costs do not match the bound evaluation protocol")]
    ProtocolMismatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchRow {
    pub available_time: DateTime<Utc>,
    pub signal: f64,
    #[serde(default)]
    pub features: BTreeMap<String, f64>,
    pub label: f64,
    pub fee_bps: f64,
    pub funding_bps: f64,
    pub latency_bps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardFold {
    pub train: Range<usize>,
    pub purge: Range<usize>,
    pub validation: Range<usize>,
    pub embargo: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardPlan {
    pub folds: Vec<WalkForwardFold>,
    pub sealed_holdout: Range<usize>,
}

pub struct EngineContext<'a> {
    rows: &'a [ResearchRow],
    folds: &'a [WalkForwardFold],
    sealed_holdout_id: &'a str,
    protocol: &'a EvaluationProtocolV1,
}

impl<'a> EngineContext<'a> {
    pub fn rows(&self) -> &'a [ResearchRow] {
        self.rows
    }

    pub fn folds(&self) -> &'a [WalkForwardFold] {
        self.folds
    }

    pub fn sealed_holdout_id(&self) -> &str {
        self.sealed_holdout_id
    }

    pub fn protocol(&self) -> &EvaluationProtocolV1 {
        self.protocol
    }
}

pub struct ProposalContext<'a> {
    row_count: usize,
    fold_count: usize,
    sealed_holdout_id: &'a str,
    latest_signal: Option<f64>,
    objective: Option<&'a str>,
    hypothesis_scope: Option<&'a str>,
    mutable_scope: Option<&'a [String]>,
    prompt_snapshot_id: Option<&'a str>,
}

impl ProposalContext<'_> {
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    pub fn fold_count(&self) -> usize {
        self.fold_count
    }

    pub fn sealed_holdout_id(&self) -> &str {
        self.sealed_holdout_id
    }

    pub fn latest_signal(&self) -> Option<f64> {
        self.latest_signal
    }

    pub fn objective(&self) -> Option<&str> {
        self.objective
    }

    pub fn hypothesis_scope(&self) -> Option<&str> {
        self.hypothesis_scope
    }

    pub fn mutable_scope(&self) -> Option<&[String]> {
        self.mutable_scope
    }

    pub fn prompt_snapshot_id(&self) -> Option<&str> {
        self.prompt_snapshot_id
    }
}

#[derive(Debug)]
pub struct PreparedDataset {
    rows: Vec<ResearchRow>,
    feature_names: Vec<String>,
    plan: WalkForwardPlan,
    sealed_holdout_id: String,
    protocol: EvaluationProtocolV1,
}

impl PreparedDataset {
    pub fn proposal_context(&self) -> ProposalContext<'_> {
        let research_rows = &self.rows[..self.plan.sealed_holdout.start];
        ProposalContext {
            row_count: research_rows.len(),
            fold_count: self.plan.folds.len(),
            sealed_holdout_id: &self.sealed_holdout_id,
            latest_signal: research_rows.last().map(|row| row.signal),
            objective: None,
            hypothesis_scope: None,
            mutable_scope: None,
            prompt_snapshot_id: None,
        }
    }

    pub fn proposal_context_for_mission<'a>(
        &'a self,
        mission: &'a ResearchMission,
    ) -> ProposalContext<'a> {
        let mut context = self.proposal_context();
        context.objective = Some(&mission.objective);
        context.hypothesis_scope = Some(&mission.hypothesis_scope);
        context.mutable_scope = Some(&mission.mutable_scope);
        context.prompt_snapshot_id = mission.prompt_snapshot_id.as_deref();
        context
    }

    pub fn engine_context(&self) -> EngineContext<'_> {
        EngineContext {
            rows: &self.rows[..self.plan.sealed_holdout.start],
            folds: &self.plan.folds,
            sealed_holdout_id: &self.sealed_holdout_id,
            protocol: &self.protocol,
        }
    }

    pub fn plan(&self) -> &WalkForwardPlan {
        &self.plan
    }

    pub fn feature_names(&self) -> &[String] {
        &self.feature_names
    }

    pub fn protocol(&self) -> &EvaluationProtocolV1 {
        &self.protocol
    }
}

pub fn prepare_dataset(
    rows: Vec<ResearchRow>,
    protocol: &EvaluationProtocolV1,
    sealed_holdout_id: impl Into<String>,
) -> Result<PreparedDataset, EvaluationError> {
    protocol
        .validate()
        .map_err(|_| EvaluationError::InvalidConfiguration)?;
    let config = &protocol.walk_forward;
    if config.initial_train_rows == 0
        || config.validation_rows == 0
        || config.fold_count == 0
        || config.sealed_holdout_rows == 0
    {
        return Err(EvaluationError::InvalidConfiguration);
    }
    if rows.iter().any(|row| {
        [
            row.signal,
            row.label,
            row.fee_bps,
            row.funding_bps,
            row.latency_bps,
        ]
        .iter()
        .any(|value| !value.is_finite())
            || row.features.values().any(|value| !value.is_finite())
    }) {
        return Err(EvaluationError::NonFiniteValue);
    }
    if rows.iter().any(|row| {
        row.fee_bps.to_bits() != protocol.costs.fee_bps.to_bits()
            || row.funding_bps.to_bits() != protocol.costs.funding_bps.to_bits()
            || row.latency_bps.to_bits() != protocol.costs.latency_bps.to_bits()
    }) {
        return Err(EvaluationError::ProtocolMismatch);
    }
    let feature_names = rows
        .first()
        .map(|row| row.features.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if feature_names
        .iter()
        .any(|name| name.trim().is_empty() || name == "signal")
        || rows
            .iter()
            .any(|row| row.features.keys().cloned().collect::<Vec<_>>() != feature_names)
    {
        return Err(EvaluationError::InvalidFeatureSchema);
    }
    let mut registered_features = vec!["signal".to_string()];
    registered_features.extend(feature_names);
    if rows
        .windows(2)
        .any(|window| window[0].available_time >= window[1].available_time)
    {
        return Err(EvaluationError::NonMonotonicAvailability);
    }
    if rows.len() <= config.sealed_holdout_rows {
        return Err(EvaluationError::InsufficientRows);
    }
    let holdout_start = rows.len() - config.sealed_holdout_rows;
    let mut folds = Vec::with_capacity(config.fold_count);
    for fold_index in 0..config.fold_count {
        let train_end = config.initial_train_rows + fold_index * config.validation_rows;
        let validation_start = train_end
            .checked_add(config.purge_rows)
            .ok_or(EvaluationError::InsufficientRows)?;
        let validation_end = validation_start
            .checked_add(config.validation_rows)
            .ok_or(EvaluationError::InsufficientRows)?;
        let embargo_end = validation_end
            .checked_add(config.embargo_rows)
            .ok_or(EvaluationError::InsufficientRows)?;
        if embargo_end > holdout_start {
            return Err(EvaluationError::InsufficientRows);
        }
        folds.push(WalkForwardFold {
            train: 0..train_end,
            purge: train_end..validation_start,
            validation: validation_start..validation_end,
            embargo: validation_end..embargo_end,
        });
    }
    Ok(PreparedDataset {
        rows,
        feature_names: registered_features,
        plan: WalkForwardPlan {
            folds,
            sealed_holdout: holdout_start..holdout_start + config.sealed_holdout_rows,
        },
        sealed_holdout_id: sealed_holdout_id.into(),
        protocol: protocol.clone(),
    })
}

pub fn evaluate_sealed_holdout<T>(
    dataset: &PreparedDataset,
    evaluator: impl FnOnce(&[ResearchRow]) -> T,
) -> T {
    evaluator(&dataset.rows[dataset.plan.sealed_holdout.clone()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationWalkForwardV1};
    use chrono::Duration;

    fn rows(count: usize) -> Vec<ResearchRow> {
        let start = Utc::now();
        (0..count)
            .map(|index| ResearchRow {
                available_time: start + Duration::seconds(index as i64),
                signal: index as f64,
                features: BTreeMap::new(),
                label: index as f64 * 0.01,
                fee_bps: 1.0,
                funding_bps: 0.1,
                latency_bps: 0.2,
            })
            .collect()
    }

    fn protocol() -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 20,
                validation_rows: 5,
                fold_count: 3,
                purge_rows: 2,
                embargo_rows: 1,
                sealed_holdout_rows: 10,
            },
            EvaluationCostsV1 {
                fee_bps: 1.0,
                funding_bps: 0.1,
                latency_bps: 0.2,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 1,
                observation_frequency_millis: 1_000,
            },
        )
        .unwrap()
    }

    #[test]
    fn walk_forward_builds_purged_embargoed_folds() {
        let dataset = prepare_dataset(rows(50), &protocol(), "holdout-1").unwrap();
        assert_eq!(dataset.plan().folds.len(), 3);
        assert_eq!(dataset.plan().folds[0].train, 0..20);
        assert_eq!(dataset.plan().folds[0].purge, 20..22);
        assert_eq!(dataset.plan().folds[0].validation, 22..27);
        assert_eq!(dataset.plan().folds[0].embargo, 27..28);
        assert_eq!(dataset.plan().sealed_holdout, 40..50);
    }

    #[test]
    fn engine_context_cannot_read_sealed_holdout_rows() {
        let dataset = prepare_dataset(rows(50), &protocol(), "holdout-1").unwrap();
        let context = dataset.engine_context();
        assert_eq!(context.rows().len(), 40);
        assert_eq!(evaluate_sealed_holdout(&dataset, |rows| rows.len()), 10);
    }

    #[test]
    fn proposal_context_exposes_only_label_free_metadata() {
        let dataset = prepare_dataset(rows(50), &protocol(), "holdout-1").unwrap();
        let context = dataset.proposal_context();

        assert_eq!(context.row_count(), 40);
        assert_eq!(context.fold_count(), 3);
        assert_eq!(context.sealed_holdout_id(), "holdout-1");
        assert_eq!(context.latest_signal(), Some(39.0));
    }

    #[test]
    fn walk_forward_rejects_non_monotonic_availability() {
        let mut rows = rows(50);
        rows.swap(1, 2);
        assert_eq!(
            prepare_dataset(rows, &protocol(), "holdout-1").unwrap_err(),
            EvaluationError::NonMonotonicAvailability
        );
    }

    #[test]
    fn walk_forward_rejects_duplicate_availability() {
        let mut rows = rows(50);
        rows[2].available_time = rows[1].available_time;
        assert_eq!(
            prepare_dataset(rows, &protocol(), "holdout-1").unwrap_err(),
            EvaluationError::NonMonotonicAvailability
        );
    }

    #[test]
    fn walk_forward_rejects_feature_schema_drift() {
        let mut rows = rows(50);
        rows[0].features.insert("lob_imbalance".to_string(), 0.1);

        assert_eq!(
            prepare_dataset(rows, &protocol(), "holdout-1").unwrap_err(),
            EvaluationError::InvalidFeatureSchema
        );
    }

    #[test]
    fn dataset_rejects_costs_that_do_not_match_the_bound_protocol() {
        let mut rows = rows(50);
        rows[0].fee_bps = 2.0;

        assert_eq!(
            prepare_dataset(rows, &protocol(), "holdout-1").unwrap_err(),
            EvaluationError::ProtocolMismatch
        );
    }
}
