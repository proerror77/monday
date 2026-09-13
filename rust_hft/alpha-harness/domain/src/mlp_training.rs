//! Frozen CEX MLP experiments. Optimizer updates are not search-trial counts.
use hft_research_manifest::mlp_training::{
    MlpOptimizationControlsV1, MlpPredictionDiagnosticsV1, MlpTargetScaleV1,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMlpInitializationV1 {
    pub fold_seeds: Vec<u64>,
    pub expected_factor_ids: Vec<String>,
    pub expected_factor_columns_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMlpFoldObservationV1 {
    pub validation_prediction: MlpPredictionDiagnosticsV1,
}

impl CexMlpInitializationV1 {
    fn validate(&self) -> Result<(), String> {
        let factors: BTreeSet<_> = self.expected_factor_ids.iter().collect();
        if self.fold_seeds.is_empty()
            || self.fold_seeds.len() > 32
            || self.expected_factor_ids.is_empty()
            || self.expected_factor_ids.len() > 256
            || factors.len() != self.expected_factor_ids.len()
            || self
                .expected_factor_ids
                .windows(2)
                .any(|pair| pair[0] >= pair[1])
            || !crate::valid_content_sha256(&self.expected_factor_columns_sha256)
            || self.expected_factor_ids.iter().any(|id| {
                id.trim().is_empty()
                    || id.len() > 512
                    || id.trim() != id
                    || id.chars().any(char::is_control)
            })
        {
            return Err(
                "CEX MLP paired fold seeds or ordered factor identities are invalid".into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMlpOptimizationV1 {
    pub learning_rate: f64,
    pub controls: MlpOptimizationControlsV1,
}

impl CexMlpOptimizationV1 {
    fn validate(&self, updates: usize, target_scale: MlpTargetScaleV1) -> Result<(), String> {
        if ![0.0003, 0.001, 0.003].contains(&self.learning_rate)
            || target_scale != MlpTargetScaleV1::TrainStandardized
            || updates < self.controls.convergence.minimum_updates
        {
            return Err(
                "CEX stable MLP requires an admitted rate, standardized targets and enough updates"
                    .into(),
            );
        }
        self.controls.validate_for_updates(updates)
    }
}

fn validate_recipe(
    updates: usize,
    target_scale: MlpTargetScaleV1,
    optimization: Option<&CexMlpOptimizationV1>,
) -> Result<(), String> {
    match optimization {
        None if [8, 64, 256].contains(&updates) => Ok(()),
        Some(value) if [2048, 4096, 8192, 16384].contains(&updates) => {
            value.validate(updates, target_scale)
        }
        _ => Err("CEX long MLP training requires an explicit bounded stability recipe".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMlpTrainingPlanV1 {
    pub schema_version: String,
    pub updates: usize,
    pub target_scale: MlpTargetScaleV1,
    /// Absent only in preserved short-budget diagnostic contracts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization: Option<CexMlpOptimizationV1>,
    /// Campaign round seed -> actual initialization seed for each chronological fold.
    pub initializations: BTreeMap<u64, CexMlpInitializationV1>,
}

impl CexMlpTrainingPlanV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != "cex-mlp-training-plan-v1"
            || self.initializations.is_empty()
            || self.initializations.len() > 16
        {
            return Err("CEX MLP training plan exceeds its bounded experiment contract".into());
        }
        validate_recipe(self.updates, self.target_scale, self.optimization.as_ref())?;
        for init in self.initializations.values() {
            init.validate()?;
            if Some(init.fold_seeds.len())
                != self
                    .initializations
                    .values()
                    .next()
                    .map(|first| first.fold_seeds.len())
            {
                return Err("CEX MLP round seeds must declare the same fold count".into());
            }
        }
        Ok(())
    }

    pub fn validate_requested_seeds(&self, seeds: &[u64]) -> Result<(), String> {
        self.validate()?;
        if seeds.is_empty() {
            return Err("CEX MLP Campaign requires declared round seeds".into());
        }
        for seed in seeds {
            self.resolve(*seed)?;
        }
        Ok(())
    }

    pub fn resolve(&self, campaign_seed: u64) -> Result<CexMlpTrainingProfileV1, String> {
        self.validate()?;
        let initialization = self
            .initializations
            .get(&campaign_seed)
            .ok_or("CEX MLP training plan does not declare this Campaign seed")?
            .clone();
        let profile = CexMlpTrainingProfileV1 {
            schema_version: "cex-mlp-training-profile-v1".into(),
            updates: self.updates,
            target_scale: self.target_scale,
            optimization: self.optimization.clone(),
            initialization,
        };
        profile.validate()?;
        Ok(profile)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMlpTrainingProfileV1 {
    pub schema_version: String,
    pub updates: usize,
    pub target_scale: MlpTargetScaleV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optimization: Option<CexMlpOptimizationV1>,
    pub initialization: CexMlpInitializationV1,
}

impl CexMlpTrainingProfileV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != "cex-mlp-training-profile-v1" {
            return Err("CEX MLP training profile is invalid".into());
        }
        validate_recipe(self.updates, self.target_scale, self.optimization.as_ref())?;
        self.initialization.validate()
    }

    pub fn learning_rate(&self) -> f64 {
        self.optimization
            .as_ref()
            .map_or(1e-3, |value| value.learning_rate)
    }

    pub fn optimization_controls(&self) -> Option<&MlpOptimizationControlsV1> {
        self.optimization.as_ref().map(|value| &value.controls)
    }

    pub fn validate_inputs(&self, fold_count: usize, factor_ids: &[String]) -> Result<(), String> {
        self.validate()?;
        if self.initialization.fold_seeds.len() != fold_count
            || self.initialization.expected_factor_ids != factor_ids
        {
            return Err(
                "CEX MLP paired experiment fold count or frozen factor ordering drifted".into(),
            );
        }
        Ok(())
    }

    pub fn seed_for_fold(&self, fold_index: usize) -> Result<u64, String> {
        self.validate()?;
        fold_index
            .checked_sub(1)
            .and_then(|index| self.initialization.fold_seeds.get(index))
            .copied()
            .ok_or_else(|| "CEX MLP paired initialization does not declare this fold".into())
    }

    pub fn validate_factor_bank(
        &self,
        bank: &crate::CexFactorBankRevisionV2,
    ) -> Result<(), String> {
        self.validate()?;
        if factor_columns_sha256(&bank.entries)?
            != self.initialization.expected_factor_columns_sha256
        {
            return Err("CEX MLP frozen factor columns or orientations drifted".into());
        }
        Ok(())
    }
}

/// The factor id authenticates the canonical AST; its orientation also changes
/// the input column and must remain fixed across training treatments.
pub fn factor_columns_sha256(entries: &[crate::CexFactorBankEntryV1]) -> Result<String, String> {
    let mut columns: Vec<_> = entries
        .iter()
        .map(|entry| (entry.factor_id.as_str(), &entry.orientation))
        .collect();
    columns.sort_by(|left, right| left.0.cmp(right.0));
    crate::canonical_json_hash(&columns).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_plan() -> CexMlpTrainingPlanV1 {
        CexMlpTrainingPlanV1 {
            schema_version: "cex-mlp-training-plan-v1".into(),
            updates: 256,
            target_scale: MlpTargetScaleV1::TrainStandardized,
            optimization: None,
            initializations: [(7, vec![71, 72, 73]), (11, vec![111, 112, 113])]
                .into_iter()
                .map(|(seed, fold_seeds)| {
                    (
                        seed,
                        CexMlpInitializationV1 {
                            fold_seeds,
                            expected_factor_ids: vec!["factor-a".into()],
                            expected_factor_columns_sha256: "a".repeat(64),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn stable_long_mlp_recipes_are_explicit_and_keep_paired_initialization() {
        let legacy = legacy_plan();
        let old_json = serde_json::to_value(&legacy).unwrap();
        assert!(old_json.get("optimization").is_none());
        let restored: CexMlpTrainingPlanV1 = serde_json::from_value(old_json.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), old_json);
        let mut identities = BTreeSet::new();
        for learning_rate in [0.0003, 0.001, 0.003] {
            for updates in [4096, 8192] {
                let mut plan = legacy.clone();
                plan.updates = updates;
                plan.optimization = Some(CexMlpOptimizationV1 {
                    learning_rate,
                    controls: MlpOptimizationControlsV1::default(),
                });
                plan.validate_requested_seeds(&[7, 11]).unwrap();
                assert!(identities.insert(crate::canonical_json_hash(&plan).unwrap()));
                for seed in [7, 11] {
                    let profile = plan.resolve(seed).unwrap();
                    assert_eq!(
                        profile.initialization,
                        legacy.resolve(seed).unwrap().initialization
                    );
                    assert_eq!(profile.learning_rate(), learning_rate);
                    assert_eq!(
                        profile.optimization_controls(),
                        Some(&MlpOptimizationControlsV1::default())
                    );
                }
                let mut unguarded = plan.clone();
                unguarded.optimization = None;
                assert!(unguarded.validate().is_err());
                let mut raw = plan.clone();
                raw.target_scale = MlpTargetScaleV1::RawReturn;
                assert!(raw.validate().is_err());
                let mut rate = plan.clone();
                rate.optimization.as_mut().unwrap().learning_rate = 0.01;
                assert!(rate.validate().is_err());
                let mut short = plan.clone();
                short
                    .optimization
                    .as_mut()
                    .unwrap()
                    .controls
                    .convergence
                    .minimum_updates = updates + 1;
                assert!(short.validate().is_err());
                let mut unbounded = plan;
                unbounded.updates = 16385;
                assert!(unbounded.validate().is_err());
            }
        }
        assert_eq!(identities.len(), 6);
    }
}
