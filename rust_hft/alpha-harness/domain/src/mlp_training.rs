//! Frozen CEX MLP experiments. Optimizer updates are not search-trial counts.
use hft_research_manifest::mlp_training::{MlpPredictionDiagnosticsV1, MlpTargetScaleV1};
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
pub struct CexMlpTrainingPlanV1 {
    pub schema_version: String,
    pub updates: usize,
    pub target_scale: MlpTargetScaleV1,
    /// Campaign round seed -> actual initialization seed for each chronological fold.
    pub initializations: BTreeMap<u64, CexMlpInitializationV1>,
}

impl CexMlpTrainingPlanV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != "cex-mlp-training-plan-v1"
            || ![8, 64, 256].contains(&self.updates)
            || self.initializations.is_empty()
            || self.initializations.len() > 16
        {
            return Err("CEX MLP training plan exceeds its bounded experiment contract".into());
        }
        for init in self.initializations.values() {
            init.validate()?;
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
    pub initialization: CexMlpInitializationV1,
}

impl CexMlpTrainingProfileV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != "cex-mlp-training-profile-v1"
            || ![8, 64, 256].contains(&self.updates)
        {
            return Err("CEX MLP training profile is invalid".into());
        }
        self.initialization.validate()
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
