//! Predeclared SOL comparison scope. This plan is not a grant or holdout claim.
use crate::{canonical_json_hash, EvaluationCostsV1};
use hft_research_manifest::sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const SOL_SEQUENCE_STUDY_SCHEMA: &str = "monday.sol_sequence_study.v1";
const DAY_MS: i64 = 86_400_000;

fn mature_anchors(view: SequenceViewV1) -> i64 {
    let span = view.end_ms - view.decision_start_ms - 30_000;
    if span <= 0 {
        0
    } else {
        (span - 1) / view.decision_stride_ms + 1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceStudyModelV1 {
    Ridge,
    FlattenedMlp,
    PriceTcn,
    LobTcn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SequenceFoldV1 {
    pub fold_id: u8,
    pub training_window_days: u8,
    pub train_dataset_sha256: String,
    pub validation_dataset_sha256: String,
    pub replay_manifest_sha256: String,
    pub train: SequenceViewV1,
    pub validation: SequenceViewV1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SolSequenceStudyV1 {
    pub schema_version: String,
    pub study_id: String,
    pub symbol: String,
    pub input: SequenceInputSpecV1,
    pub models: Vec<SequenceStudyModelV1>,
    pub neural_seeds: Vec<u64>,
    pub folds: Vec<SequenceFoldV1>,
    /// A different, unexposed manifest, never included in a development worker's mounts.
    pub sealed_dataset_sha256: String,
    pub sealed_view: SequenceViewV1,
    pub primary_horizon_ms: u64,
    pub max_primary_fits: u32,
    pub max_verification_fits: u32,
    pub neural_updates: usize,
    pub batch_size: usize,
    pub hidden_channels: usize,
    pub learning_rate: f64,
    /// Identical deterministic anchor count bound for all four model groups.
    pub max_training_examples: usize,
    pub costs: EvaluationCostsV1,
}

impl SolSequenceStudyV1 {
    pub fn development_primary_fits(&self) -> u32 {
        self.folds.len() as u32
            * self
                .models
                .iter()
                .map(|model| {
                    if *model == SequenceStudyModelV1::Ridge {
                        1
                    } else {
                        self.neural_seeds.len() as u32
                    }
                })
                .sum::<u32>()
    }

    pub fn validate(&self) -> Result<(), String> {
        self.input.validate()?;
        self.sealed_view.validate()?;
        let expected = BTreeSet::from([
            SequenceStudyModelV1::Ridge,
            SequenceStudyModelV1::FlattenedMlp,
            SequenceStudyModelV1::PriceTcn,
            SequenceStudyModelV1::LobTcn,
        ]);
        if self.schema_version != SOL_SEQUENCE_STUDY_SCHEMA
            || self.symbol != "SOLUSDT"
            || self.study_id.is_empty()
            || self.study_id.len() > 128
            || !self
                .study_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
            || self.input != SequenceInputSpecV1::sol_lob()
            || self.models.len() != 4
            || self.models.iter().copied().collect::<BTreeSet<_>>() != expected
            || self.neural_seeds != [7, 11]
            || self.folds.len() != 4
            || !valid_sha256(&self.sealed_dataset_sha256)
            || self.primary_horizon_ms != 30_000
            || self.sealed_view.decision_start_ms - self.sealed_view.history_start_ms < 59_000
            || mature_anchors(self.sealed_view) == 0
            || self.max_primary_fits != 30
            || self.max_verification_fits != 30
            || !(1..=16_384).contains(&self.neural_updates)
            || !(1..=256).contains(&self.batch_size)
            || !(2..=64).contains(&self.hidden_channels)
            || !(64..=32_768).contains(&self.max_training_examples)
            || self.neural_updates.saturating_mul(self.batch_size) < self.max_training_examples
            || !self.learning_rate.is_finite()
            || self.learning_rate <= 0.0
            || self.learning_rate > 0.01
        {
            return Err(
                "SOL sequence study exceeds its declared model, target or fit scope".into(),
            );
        }
        self.costs.validate().map_err(|e| e.to_string())?;
        if !self.costs.cross_spread
            || self.costs.position_notional_usd <= 0.0
            || !self.costs.capacity_enabled()
        {
            return Err(
                "SOL sequence study requires explicit taker costs, notional and capacity".into(),
            );
        }
        let mut identities = BTreeSet::new();
        for fold in &self.folds {
            fold.train.validate()?;
            fold.validation.validate()?;
            if !matches!(fold.fold_id, 1 | 2)
                || !matches!(fold.training_window_days, 7 | 14)
                || !identities.insert((fold.fold_id, fold.training_window_days))
                || !valid_sha256(&fold.train_dataset_sha256)
                || !valid_sha256(&fold.validation_dataset_sha256)
                || !valid_sha256(&fold.replay_manifest_sha256)
                || fold.train_dataset_sha256 == self.sealed_dataset_sha256
                || fold.validation_dataset_sha256 == self.sealed_dataset_sha256
                || fold.train.end_ms - fold.train.history_start_ms
                    != i64::from(fold.training_window_days) * DAY_MS
                || fold.train.end_ms >= fold.validation.history_start_ms
                || fold.validation.end_ms >= self.sealed_view.history_start_ms
                || fold.validation.decision_stride_ms != 1000
                || mature_anchors(fold.train) < 64
                || mature_anchors(fold.validation) == 0
                || self.sealed_view.decision_stride_ms != 1000
                || (fold.train.end_ms - fold.train.decision_start_ms - 1)
                    / fold.train.decision_stride_ms
                    + 1
                    > self.max_training_examples as i64
                || fold.train.decision_start_ms - fold.train.history_start_ms < 59_000
                || fold.validation.decision_start_ms - fold.validation.history_start_ms < 59_000
            {
                return Err(
                    "SOL sequence fold leaks or drifts from its predeclared clock and inputs"
                        .into(),
                );
            }
        }
        for id in [1, 2] {
            let short = self
                .folds
                .iter()
                .find(|f| f.fold_id == id && f.training_window_days == 7)
                .ok_or("missing short-window fold")?;
            let long = self
                .folds
                .iter()
                .find(|f| f.fold_id == id && f.training_window_days == 14)
                .ok_or("missing long-window fold")?;
            if short.validation != long.validation
                || short.validation_dataset_sha256 != long.validation_dataset_sha256
                || short.replay_manifest_sha256 != long.replay_manifest_sha256
                || short.train.end_ms != long.train.end_ms
            {
                return Err(
                    "SOL training-window comparison must share the same validation and costs"
                        .into(),
                );
            }
        }
        let first = self
            .folds
            .iter()
            .find(|f| f.fold_id == 1)
            .expect("validated first fold");
        let second = self
            .folds
            .iter()
            .find(|f| f.fold_id == 2)
            .expect("validated second fold");
        if first.validation.end_ms > second.validation.history_start_ms
            || first.train.end_ms >= second.train.end_ms
            || self.development_primary_fits() != 28
        {
            return Err(
                "SOL development folds overlap or have an invalid comparison budget".into(),
            );
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<String, String> {
        self.validate()?;
        canonical_json_hash(self).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> SolSequenceStudyV1 {
        let mut folds = Vec::new();
        for fold_id in [1, 2] {
            let end = (16 + i64::from(fold_id) * 2) * DAY_MS;
            for days in [7, 14] {
                let start = end - i64::from(days) * DAY_MS;
                let stride = ((end - start + 32_768_000 - 1) / 32_768_000) * 1000;
                folds.push(SequenceFoldV1 {
                    fold_id,
                    training_window_days: days,
                    train_dataset_sha256: "a".repeat(64),
                    validation_dataset_sha256: "b".repeat(64),
                    replay_manifest_sha256: "c".repeat(64),
                    train: SequenceViewV1 {
                        history_start_ms: start,
                        decision_start_ms: start + 59000,
                        end_ms: end,
                        decision_stride_ms: stride,
                    },
                    validation: SequenceViewV1 {
                        history_start_ms: end + 1000,
                        decision_start_ms: end + 60000,
                        end_ms: end + DAY_MS,
                        decision_stride_ms: 1000,
                    },
                });
            }
        }
        SolSequenceStudyV1 {
            schema_version: SOL_SEQUENCE_STUDY_SCHEMA.into(),
            study_id: "sol-sequence-test".into(),
            symbol: "SOLUSDT".into(),
            input: SequenceInputSpecV1::sol_lob(),
            models: vec![
                SequenceStudyModelV1::Ridge,
                SequenceStudyModelV1::FlattenedMlp,
                SequenceStudyModelV1::PriceTcn,
                SequenceStudyModelV1::LobTcn,
            ],
            neural_seeds: vec![7, 11],
            folds,
            sealed_dataset_sha256: "d".repeat(64),
            sealed_view: SequenceViewV1 {
                history_start_ms: 23 * DAY_MS,
                decision_start_ms: 23 * DAY_MS + 59000,
                end_ms: 28 * DAY_MS,
                decision_stride_ms: 1000,
            },
            primary_horizon_ms: 30000,
            max_primary_fits: 30,
            max_verification_fits: 30,
            neural_updates: 1024,
            batch_size: 32,
            hidden_channels: 16,
            learning_rate: 0.0003,
            max_training_examples: 32768,
            costs: EvaluationCostsV1 {
                fee_bps: 2.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: true,
                position_notional_usd: 100.0,
                capacity_depth_levels: 5,
                max_book_depth_fraction: 0.05,
            },
        }
    }

    #[test]
    fn sequence_study_binds_28_development_fits_and_every_input() {
        let study = plan();
        study.validate().unwrap();
        assert_eq!(study.development_primary_fits(), 28);
        let hash = study.content_hash().unwrap();
        let mut changed = study.clone();
        changed.costs.fee_bps = 3.0;
        assert_ne!(hash, changed.content_hash().unwrap());
        changed.folds[0].train_dataset_sha256 = "e".repeat(64);
        assert_ne!(
            study.content_hash().unwrap(),
            changed.content_hash().unwrap()
        );
    }

    #[test]
    fn sequence_study_rejects_hidden_expansion_and_holdout_exposure() {
        let study = plan();
        let mut changed = study.clone();
        changed.neural_seeds.push(23);
        assert!(changed.validate().is_err());
        let mut changed = study.clone();
        changed.folds[0].validation_dataset_sha256 = changed.sealed_dataset_sha256.clone();
        assert!(changed.validate().is_err());
        let mut changed = study.clone();
        changed.folds[0].train.end_ms = changed.folds[0].validation.decision_start_ms;
        assert!(changed.validate().is_err());
        let mut changed = study.clone();
        changed.folds[0].train.decision_stride_ms = 1000;
        assert!(changed.validate().is_err());
        let mut changed = study.clone();
        changed.folds[0].validation.end_ms -= 1000;
        assert!(changed.validate().is_err());
        let mut changed = study;
        changed.costs.fee_bps = f64::NAN;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn sequence_study_rejects_views_without_mature_anchors() {
        let study = plan();
        let mut changed = study.clone();
        changed.folds[0].train.decision_start_ms = changed.folds[0].train.end_ms - 30_000;
        assert!(changed.validate().is_err());
        changed = study.clone();
        changed.folds[0].train.decision_stride_ms = 86_400_000;
        assert!(changed.validate().is_err());
        changed = study.clone();
        for fold in &mut changed.folds {
            fold.validation.decision_start_ms = fold.validation.end_ms - 30_000;
        }
        assert!(changed.validate().is_err());
        changed = study;
        changed.sealed_view.decision_start_ms = changed.sealed_view.end_ms - 30_000;
        assert!(changed.validate().is_err());
    }
}
