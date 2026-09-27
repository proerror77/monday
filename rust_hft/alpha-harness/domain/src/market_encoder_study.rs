//! Fixed two-stage SOL experiment. This protocol is neither a grant nor a holdout claim.
use crate::{canonical_json_hash, EvaluationCostsV1};
use hft_research_manifest::{
    market_encoder::{MarketEncoderSpecV1, MarketFitRequestV1},
    sequence::{valid_sha256, SequenceInputSpecV1, SequenceViewV1},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
const DAY_MS: i64 = 86_400_000;
pub const MARKET_ENCODER_STUDY_SCHEMA: &str = "monday.sol_market_encoder_study.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDataViewV1 {
    pub features_sha256: String,
    pub targets_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_anchors_sha256: Option<String>,
    pub view: SequenceViewV1,
}
impl MarketDataViewV1 {
    fn validate(&self) -> Result<(), String> {
        self.view.validate()?;
        if !valid_sha256(&self.features_sha256)
            || !valid_sha256(&self.targets_sha256)
            || self.features_sha256 == self.targets_sha256
            || self
                .qualified_anchors_sha256
                .as_deref()
                .is_some_and(|hash| !valid_sha256(hash))
            || self.view.decision_start_ms - self.view.history_start_ms < 59_000
            || self.view.end_ms - self.view.decision_start_ms <= 30_000
        {
            return Err("market data view has invalid identities or maturity boundaries".into());
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEvaluationViewV1 {
    pub data: MarketDataViewV1,
    pub replay_manifest_sha256: String,
}
impl MarketEvaluationViewV1 {
    fn validate(&self) -> Result<(), String> {
        self.data.validate()?;
        if self.data.qualified_anchors_sha256.is_some()
            || !valid_sha256(&self.replay_manifest_sha256)
            || self.data.view.decision_stride_ms != 1000
            || self.data.view.end_ms - self.data.view.decision_start_ms - 30_000 < DAY_MS
        {
            return Err(
                "market evaluation requires a bound replay and at least 24h of decisions".into(),
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEncoderFoldV1 {
    pub fold_id: u8,
    pub train: MarketDataViewV1,
    pub validation: MarketEvaluationViewV1,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEncoderTrainingV1 {
    pub hidden_channels: usize,
    pub batch_size: usize,
    pub pretraining_updates: usize,
    pub task_updates: usize,
    pub compute_control_updates: usize,
    pub learning_rate: f64,
    pub max_training_examples: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEncoderStudyV1 {
    pub schema_version: String,
    pub study_id: String,
    pub input: SequenceInputSpecV1,
    pub seeds: Vec<u64>,
    pub folds: Vec<MarketEncoderFoldV1>,
    pub independent_selection: MarketEvaluationViewV1,
    pub sealed: MarketEvaluationViewV1,
    pub training: MarketEncoderTrainingV1,
    pub max_primary_fits: u32,
    pub max_verification_fits: u32,
    /// Original task ceiling, not a statement that this much money remains.
    pub max_cost_fen: u32,
    pub costs: EvaluationCostsV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketTrainingStageKindV1 {
    Pretrain,
    Scratch,
    LinearProbe,
    FineTune,
    ScratchCompute,
    Ridge,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketTrainingStagePurposeV1 {
    Primary,
    Verification,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTrainingStageKeyV1 {
    pub fold_id: u8,
    pub kind: MarketTrainingStageKindV1,
    pub seed: u64,
    pub purpose: MarketTrainingStagePurposeV1,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketTrainingStageV1 {
    pub key: MarketTrainingStageKeyV1,
    /// Only this primary checkpoint may initialize inherited parameters.
    pub parent: Option<MarketTrainingStageKeyV1>,
    /// Verified primary stage whose fitted parameter values must be reproduced.
    pub verification_of: Option<MarketTrainingStageKeyV1>,
}
impl MarketTrainingStageV1 {
    pub fn prerequisites(&self) -> Vec<MarketTrainingStageKeyV1> {
        let mut needed = Vec::new();
        if let Some(parent) = self.parent {
            needed.push(parent);
            needed.push(MarketTrainingStageKeyV1 {
                purpose: MarketTrainingStagePurposeV1::Verification,
                ..parent
            });
        }
        if let Some(primary) = self.verification_of {
            needed.push(primary);
        }
        needed.sort();
        needed.dedup();
        needed
    }
}

impl MarketEncoderStudyV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != MARKET_ENCODER_STUDY_SCHEMA
            || self.study_id.is_empty()
            || self.study_id.len() > 128
            || !self
                .study_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
            || self.input != SequenceInputSpecV1::sol_lob()
            || self.seeds != [7, 11]
            || self.folds.iter().map(|f| f.fold_id).collect::<Vec<_>>() != [1, 2]
            || self.max_primary_fits != 30
            || self.max_verification_fits != 30
            || self.max_cost_fen != 10_000
            || self.training.max_training_examples < 64
            || self.training.max_training_examples > 32_768
            || self.training.task_updates < 2
            || self.training.compute_control_updates < self.training.task_updates
        {
            return Err(
                "market encoder study differs from its fixed task, groups or budget".into(),
            );
        }
        self.independent_selection.validate()?;
        self.sealed.validate()?;
        if self.independent_selection.data.view.end_ms >= self.sealed.data.view.history_start_ms {
            return Err("market independent selection overlaps sealed history".into());
        }
        self.costs.validate().map_err(|e| e.to_string())?;
        if !self.costs.cross_spread
            || !self.costs.capacity_enabled()
            || self.costs.position_notional_usd <= 0.0
        {
            return Err("market study requires explicit taker costs and capacity".into());
        }
        let mut features = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for data in self
            .folds
            .iter()
            .flat_map(|f| [&f.train, &f.validation.data])
            .chain([&self.independent_selection.data, &self.sealed.data])
        {
            data.validate()?;
            if !features.insert(&data.features_sha256) || !targets.insert(&data.targets_sha256) {
                return Err(
                    "market study reuses a development or withheld dataset identity".into(),
                );
            }
        }
        for fold in &self.folds {
            fold.validation.validate()?;
            let train = fold.train.view;
            if fold.train.qualified_anchors_sha256.is_none()
                || train.end_ms - train.history_start_ms != 14 * DAY_MS
                || fold.validation.data.view.history_start_ms - train.end_ms < 30_000
                || fold.validation.data.view.end_ms
                    >= self.independent_selection.data.view.history_start_ms
            {
                return Err(
                    "market training, validation or withheld time boundary overlaps".into(),
                );
            }
            let anchors = (train.end_ms - 30_000 - train.decision_start_ms - 1)
                / train.decision_stride_ms
                + 1;
            if anchors < 64 || anchors as u64 > self.training.max_training_examples {
                return Err("market training anchor grid exceeds declared sample budget".into());
            }
            for updates in [
                self.training.pretraining_updates,
                self.training.task_updates,
                self.training.compute_control_updates,
            ] {
                self.fit_request_unchecked(fold, self.seeds[0], updates)
                    .validate()?;
            }
        }
        if self.folds[0].train.view.end_ms >= self.folds[1].train.view.end_ms
            || self.folds[0].validation.data.view.end_ms
                > self.folds[1].validation.data.view.history_start_ms
        {
            return Err("market development folds overlap or are out of order".into());
        }
        Ok(())
    }
    pub fn content_hash(&self) -> Result<String, String> {
        self.validate()?;
        canonical_json_hash(self).map_err(|e| e.to_string())
    }
    fn fit_request_unchecked(
        &self,
        fold: &MarketEncoderFoldV1,
        seed: u64,
        updates: usize,
    ) -> MarketFitRequestV1 {
        MarketFitRequestV1 {
            feature_dataset_sha256: fold.train.features_sha256.clone(),
            qualified_anchors_sha256: fold.train.qualified_anchors_sha256.clone(),
            spec: MarketEncoderSpecV1 {
                input: self.input.clone(),
                hidden_channels: self.training.hidden_channels,
            },
            view: fold.train.view,
            anchor_end_ms: fold.train.view.end_ms - 30_000,
            seed,
            batch_size: self.training.batch_size,
            updates,
            learning_rate: self.training.learning_rate,
            min_examples: 64,
            max_examples: self.training.max_training_examples,
        }
    }
    pub fn fit_request(&self, key: MarketTrainingStageKeyV1) -> Result<MarketFitRequestV1, String> {
        self.validate()?;
        if !self.stages_unchecked().iter().any(|s| s.key == key)
            || key.kind == MarketTrainingStageKindV1::Ridge
        {
            return Err("unregistered neural market stage".into());
        }
        let fold = self
            .folds
            .iter()
            .find(|f| f.fold_id == key.fold_id)
            .ok_or("missing market fold")?;
        let updates = match key.kind {
            MarketTrainingStageKindV1::Pretrain => self.training.pretraining_updates,
            MarketTrainingStageKindV1::ScratchCompute => self.training.compute_control_updates,
            _ => self.training.task_updates,
        };
        Ok(self.fit_request_unchecked(fold, key.seed, updates))
    }
    fn stages_unchecked(&self) -> Vec<MarketTrainingStageV1> {
        use MarketTrainingStageKindV1::*;
        use MarketTrainingStagePurposeV1::*;
        let mut stages = Vec::new();
        for fold in &self.folds {
            for (kind, seed) in self
                .seeds
                .iter()
                .flat_map(|seed| {
                    [Pretrain, Scratch, LinearProbe, FineTune, ScratchCompute]
                        .map(|kind| (kind, *seed))
                })
                .chain([(Ridge, 0)])
            {
                let primary = MarketTrainingStageKeyV1 {
                    fold_id: fold.fold_id,
                    kind,
                    seed,
                    purpose: Primary,
                };
                let parent =
                    matches!(kind, LinearProbe | FineTune).then_some(MarketTrainingStageKeyV1 {
                        kind: Pretrain,
                        ..primary
                    });
                stages.push(MarketTrainingStageV1 {
                    key: primary,
                    parent,
                    verification_of: None,
                });
                stages.push(MarketTrainingStageV1 {
                    key: MarketTrainingStageKeyV1 {
                        purpose: Verification,
                        ..primary
                    },
                    parent,
                    verification_of: Some(primary),
                });
            }
        }
        stages
    }
    pub fn development_stages(&self) -> Result<Vec<MarketTrainingStageV1>, String> {
        self.validate()?;
        Ok(self.stages_unchecked())
    }
    pub fn development_primary_fits(&self) -> Result<u32, String> {
        Ok(self
            .development_stages()?
            .iter()
            .filter(|s| s.key.purpose == MarketTrainingStagePurposeV1::Primary)
            .count() as u32)
    }
    pub fn final_primary_reserve(&self) -> u32 {
        (self.seeds.len() * 2) as u32
    }
    /// Call with authenticated cumulative ledger consumption, including related
    /// earlier attempts. Zeroes supplied by a caller are not a budget proof.
    pub fn check_remaining_fit_budget(
        &self,
        primary_consumed: u32,
        verification_consumed: u32,
    ) -> Result<(), String> {
        let wanted = self
            .development_primary_fits()?
            .checked_add(self.final_primary_reserve())
            .ok_or("market fit count overflow")?;
        if primary_consumed
            .checked_add(wanted)
            .is_none_or(|n| n > self.max_primary_fits)
            || verification_consumed
                .checked_add(wanted)
                .is_none_or(|n| n > self.max_verification_fits)
        {
            return Err("market encoder study exceeds the remaining cumulative fit budget".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hash(id: u8) -> String {
        format!("{id:064x}")
    }
    fn evaluation(day: i64, id: u8) -> MarketEvaluationViewV1 {
        let start = day * DAY_MS;
        MarketEvaluationViewV1 {
            data: MarketDataViewV1 {
                features_sha256: hash(id),
                targets_sha256: hash(id + 20),
                qualified_anchors_sha256: None,
                view: SequenceViewV1 {
                    history_start_ms: start,
                    decision_start_ms: start + 59000,
                    end_ms: start + 59000 + DAY_MS + 30000,
                    decision_stride_ms: 1000,
                },
            },
            replay_manifest_sha256: hash(id + 40),
        }
    }
    fn plan() -> MarketEncoderStudyV1 {
        MarketEncoderStudyV1 {
            schema_version: MARKET_ENCODER_STUDY_SCHEMA.into(),
            study_id: "sol-market-encoder-controlled-test".into(),
            input: SequenceInputSpecV1::sol_lob(),
            seeds: vec![7, 11],
            folds: [(1, 15, 1, 3), (2, 18, 2, 4)]
                .into_iter()
                .map(|(fold_id, day, id, val)| {
                    let end = day * DAY_MS;
                    MarketEncoderFoldV1 {
                        fold_id,
                        train: MarketDataViewV1 {
                            features_sha256: hash(id),
                            targets_sha256: hash(id + 20),
                            qualified_anchors_sha256: Some(hash(id + 60)),
                            view: SequenceViewV1 {
                                history_start_ms: end - 14 * DAY_MS,
                                decision_start_ms: end - 14 * DAY_MS + 59000,
                                end_ms: end,
                                decision_stride_ms: 60000,
                            },
                        },
                        validation: evaluation(day + 1, val),
                    }
                })
                .collect(),
            independent_selection: evaluation(22, 5),
            sealed: evaluation(25, 6),
            training: MarketEncoderTrainingV1 {
                hidden_channels: 16,
                batch_size: 64,
                pretraining_updates: 1024,
                task_updates: 512,
                compute_control_updates: 1536,
                learning_rate: 0.001,
                max_training_examples: 32768,
            },
            max_primary_fits: 30,
            max_verification_fits: 30,
            max_cost_fen: 10000,
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
    fn market_encoder_stage_budget_counts_pretraining_and_independent_verification() {
        let plan = plan();
        plan.validate().unwrap();
        let stages = plan.development_stages().unwrap();
        assert_eq!(stages.len(), 44);
        assert_eq!(plan.development_primary_fits().unwrap(), 22);
        assert_eq!(plan.final_primary_reserve(), 4);
        plan.check_remaining_fit_budget(4, 4).unwrap();
        assert!(plan.check_remaining_fit_budget(5, 0).is_err());
        assert!(plan.check_remaining_fit_budget(0, 5).is_err());
        let mut completed = BTreeSet::new();
        for stage in stages {
            assert!(stage
                .prerequisites()
                .iter()
                .all(|key| completed.contains(key)));
            assert!(completed.insert(stage.key));
            if stage.key.kind != MarketTrainingStageKindV1::Ridge {
                plan.fit_request(stage.key).unwrap().validate().unwrap();
            }
            if matches!(
                stage.key.kind,
                MarketTrainingStageKindV1::LinearProbe | MarketTrainingStageKindV1::FineTune
            ) {
                let parent = stage.parent.unwrap();
                assert_eq!(parent.seed, stage.key.seed);
                assert_eq!(parent.fold_id, stage.key.fold_id);
                assert_eq!(parent.kind, MarketTrainingStageKindV1::Pretrain);
                assert_eq!(parent.purpose, MarketTrainingStagePurposeV1::Primary);
            }
        }
    }
    #[test]
    fn market_encoder_study_rejects_scope_expansion_and_withheld_time_leakage() {
        let base = plan();
        let mut p = base.clone();
        p.seeds.push(23);
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.max_primary_fits = 60;
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.folds[0].train.features_sha256 = p.sealed.data.features_sha256.clone();
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.folds[0].train.view.end_ms = p.folds[0].validation.data.view.history_start_ms;
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.independent_selection = p.sealed.clone();
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.training.pretraining_updates = 1;
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.folds[0].train.view.decision_stride_ms = 1000;
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.folds[0].validation.data.view.end_ms -= 1000;
        assert!(p.validate().is_err());
        let mut p = base.clone();
        p.costs.fee_bps = f64::NAN;
        assert!(p.validate().is_err());
    }
    #[test]
    fn market_encoder_study_identity_binds_compute_controls_and_data() {
        let base = plan();
        let hash = base.content_hash().unwrap();
        let mut p = base.clone();
        p.training.compute_control_updates += 1;
        assert_ne!(hash, p.content_hash().unwrap());
        let mut p = base.clone();
        p.folds[0].train.targets_sha256 = "f".repeat(64);
        assert_ne!(hash, p.content_hash().unwrap());
        let mut p = base;
        p.costs.fee_bps += 1.0;
        assert_ne!(hash, p.content_hash().unwrap());
    }
}
