use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::ops::Range;
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchRow {
    pub available_time: DateTime<Utc>,
    pub signal: f64,
    pub label: f64,
    pub fee_bps: f64,
    pub funding_bps: f64,
    pub latency_bps: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalkForwardConfig {
    pub initial_train_rows: usize,
    pub validation_rows: usize,
    pub fold_count: usize,
    pub purge_rows: usize,
    pub embargo_rows: usize,
    pub sealed_holdout_rows: usize,
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
}

#[derive(Debug)]
pub struct PreparedDataset {
    rows: Vec<ResearchRow>,
    plan: WalkForwardPlan,
    sealed_holdout_id: String,
}

impl PreparedDataset {
    pub fn engine_context(&self) -> EngineContext<'_> {
        EngineContext {
            rows: &self.rows[..self.plan.sealed_holdout.start],
            folds: &self.plan.folds,
            sealed_holdout_id: &self.sealed_holdout_id,
        }
    }

    pub fn plan(&self) -> &WalkForwardPlan {
        &self.plan
    }
}

pub fn prepare_dataset(
    rows: Vec<ResearchRow>,
    config: &WalkForwardConfig,
    sealed_holdout_id: impl Into<String>,
) -> Result<PreparedDataset, EvaluationError> {
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
    }) {
        return Err(EvaluationError::NonFiniteValue);
    }
    if rows
        .windows(2)
        .any(|window| window[0].available_time > window[1].available_time)
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
        plan: WalkForwardPlan {
            folds,
            sealed_holdout: holdout_start..holdout_start + config.sealed_holdout_rows,
        },
        sealed_holdout_id: sealed_holdout_id.into(),
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
    use chrono::Duration;

    fn rows(count: usize) -> Vec<ResearchRow> {
        let start = Utc::now();
        (0..count)
            .map(|index| ResearchRow {
                available_time: start + Duration::seconds(index as i64),
                signal: index as f64,
                label: index as f64 * 0.01,
                fee_bps: 1.0,
                funding_bps: 0.1,
                latency_bps: 0.2,
            })
            .collect()
    }

    fn config() -> WalkForwardConfig {
        WalkForwardConfig {
            initial_train_rows: 20,
            validation_rows: 5,
            fold_count: 3,
            purge_rows: 2,
            embargo_rows: 1,
            sealed_holdout_rows: 10,
        }
    }

    #[test]
    fn walk_forward_builds_purged_embargoed_folds() {
        let dataset = prepare_dataset(rows(50), &config(), "holdout-1").unwrap();
        assert_eq!(dataset.plan().folds.len(), 3);
        assert_eq!(dataset.plan().folds[0].train, 0..20);
        assert_eq!(dataset.plan().folds[0].purge, 20..22);
        assert_eq!(dataset.plan().folds[0].validation, 22..27);
        assert_eq!(dataset.plan().folds[0].embargo, 27..28);
        assert_eq!(dataset.plan().sealed_holdout, 40..50);
    }

    #[test]
    fn engine_context_cannot_read_sealed_holdout_rows() {
        let dataset = prepare_dataset(rows(50), &config(), "holdout-1").unwrap();
        let context = dataset.engine_context();
        assert_eq!(context.rows().len(), 40);
        assert_eq!(evaluate_sealed_holdout(&dataset, |rows| rows.len()), 10);
    }

    #[test]
    fn walk_forward_rejects_non_monotonic_availability() {
        let mut rows = rows(50);
        rows.swap(1, 2);
        assert_eq!(
            prepare_dataset(rows, &config(), "holdout-1").unwrap_err(),
            EvaluationError::NonMonotonicAvailability
        );
    }
}
