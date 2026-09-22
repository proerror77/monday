//! Durable second-level orderflow research contract.
//!
//! P0 admits audit-only, research-only plumbing. Training, sealed holdout,
//! Live/Paper/Shadow, and order authority stay fail-closed.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use thiserror::Error;

pub const SEC_ORDERFLOW_RESEARCH_SCHEMA_V1: &str = "sec-orderflow-research-v1";
pub const SEC_ORDERFLOW_VENUE_BYBIT_LINEAR: &str = "bybit_linear";
pub const SEC_ORDERFLOW_HORIZONS_S: [u16; 5] = [5, 10, 30, 60, 300];
pub const SEC_ORDERFLOW_INSTANT_WINDOWS_S: [u16; 2] = [1, 5];
pub const SEC_ORDERFLOW_CONTEXT_WINDOWS_S: [u16; 3] = [300, 900, 1800];
pub const SEC_ORDERFLOW_PRIMARY_HORIZON_S: u16 = 60;
pub const SEC_ORDERFLOW_DECISION_GRID_MS: u64 = 1_000;
pub const SEC_ORDERFLOW_FULL_DEPENDENCY_GAP_FLOOR_S: u32 = 2_105;
pub const SEC_ORDERFLOW_SAMPLED_MID_MAX_AGE_MS: u32 = 15_000;
pub const SEC_ORDERFLOW_SAMPLED_MID_MIN_HORIZON_S: u16 = 30;
pub const SEC_ORDERFLOW_TAKER_PER_SIDE_BP_ASSUMPTION: &str = "11";
pub const SEC_ORDERFLOW_MAKER_PER_SIDE_BP_ASSUMPTION: &str = "4";
pub const SEC_ORDERFLOW_FEE_STATUS: &str = "historical_assumption_unverified_account";
pub const SEC_ORDERFLOW_MAX_PIPELINE_CONFIGS: u32 = 24;
pub const SEC_ORDERFLOW_SEEDS_PER_CONFIG: u32 = 3;
pub const SEC_ORDERFLOW_INITIAL_NOTIONAL_USDT: &str = "100";
pub const SEC_ORDERFLOW_MAX_HOLD_S: u32 = 300;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SecOrderflowError {
    #[error("second-level orderflow manifest is invalid: {0}")]
    Invalid(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowModeV1 {
    AuditOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowPriceKindV1 {
    MidStrict,
    Last,
    Mark,
    #[serde(rename = "mid_sampled_15s")]
    MidSampled15s,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowTargetKindV1 {
    ReturnBp,
    Direction3,
    Quantiles,
    Mfe,
    Mae,
    Touch,
    FirstTouch,
    MarkReturn,
    BasisChange,
    FundingRealized,
    RealizedClosePnl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowAvailabilityQualityV1 {
    Unknown,
    Verified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowAdmissionV1 {
    DiagnosticOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowGapPolicyV1 {
    Invalid,
    Abstain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowSplitKindV1 {
    ChronologicalNestedWalkForward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecOrderflowBaselineModelV1 {
    Zero,
    Persistence,
    TrainPrior,
    Momentum,
    Reversal,
    Ridge,
    Cart,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowPrimaryV1 {
    pub symbol: String,
    pub horizon_s: u16,
    pub price: SecOrderflowPriceKindV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowFreshnessMsV1 {
    pub mid_event: u32,
    pub mid_receive: u32,
    pub last_upper_bound: u32,
    pub mark_event: u32,
    pub mark_receive: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowLegacyPolicyV1 {
    pub availability_quality: SecOrderflowAvailabilityQualityV1,
    pub admission: SecOrderflowAdmissionV1,
    pub sampled_mid_max_age_ms: u32,
    pub sampled_mid_min_horizon_s: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowMissingPolicyV1 {
    pub unknown_gap: SecOrderflowGapPolicyV1,
    pub prehistory: SecOrderflowGapPolicyV1,
    /// Missing supervised values stay JSON null; numeric stand-ins including 0 are rejected.
    pub missing_target: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowSplitV1 {
    pub kind: SecOrderflowSplitKindV1,
    pub initial_train_days: u32,
    pub inner_validation_days: u32,
    pub outer_days: Vec<u32>,
    pub sealed_days: u32,
    pub full_dependency_gap_floor_s: u32,
    pub holdout_open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowBudgetV1 {
    pub max_pipeline_configs: u32,
    pub seeds_per_config: u32,
    pub approved_grant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowCostAssumptionsV1 {
    pub taker_per_side: String,
    pub maker_per_side: String,
    pub fee_status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowStressV1 {
    pub extra_friction_multipliers: Vec<f64>,
    pub latency_s: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowPolicyV1 {
    pub initial_notional_usdt: String,
    pub per_symbol_max_positions: u32,
    pub max_hold_s: u32,
    pub pyramiding: bool,
    pub maker_eligible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowAuthorityV1 {
    pub orders: bool,
    pub live_risk_change: bool,
    pub runtime_resume: bool,
    pub deployment: bool,
    pub promotion: bool,
    pub production_training: bool,
}

impl SecOrderflowAuthorityV1 {
    pub fn closed() -> Self {
        Self {
            orders: false,
            live_risk_change: false,
            runtime_resume: false,
            deployment: false,
            promotion: false,
            production_training: false,
        }
    }

    pub fn is_closed(&self) -> bool {
        !self.orders
            && !self.live_risk_change
            && !self.runtime_resume
            && !self.deployment
            && !self.promotion
            && !self.production_training
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowUnitsV1 {
    pub clock: String,
    pub price: String,
    pub base_quantity: String,
    pub notional: String,
    pub return_unit: String,
    pub time_unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowSourceQualityV1 {
    pub discard_historical_cvd: bool,
    pub discard_precomputed_delta_windows: bool,
    pub disable_wash_fields: bool,
    pub isolate_big_events: bool,
    pub disable_signed_liquidation: bool,
    pub forbid_forward_join: bool,
    pub forbid_gap_zero_fill: bool,
    pub availability_quality: SecOrderflowAvailabilityQualityV1,
}

/// Frozen experiment manifest. Unknown fields are rejected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecOrderflowExperimentManifestV1 {
    pub schema: String,
    pub mode: SecOrderflowModeV1,
    pub run_enabled: bool,
    pub venue: String,
    pub symbols: Vec<String>,
    pub primary: SecOrderflowPrimaryV1,
    pub decision_grid_ms: u64,
    pub instant_windows_s: Vec<u16>,
    pub context_windows_s: Vec<u16>,
    pub horizons_s: Vec<u16>,
    pub targets: Vec<SecOrderflowTargetKindV1>,
    pub freshness_ms: SecOrderflowFreshnessMsV1,
    pub legacy: SecOrderflowLegacyPolicyV1,
    pub missing: SecOrderflowMissingPolicyV1,
    pub split: SecOrderflowSplitV1,
    pub budget: SecOrderflowBudgetV1,
    pub models_initial: Vec<SecOrderflowBaselineModelV1>,
    pub cost_assumptions_bp: SecOrderflowCostAssumptionsV1,
    pub stress: SecOrderflowStressV1,
    pub policy: SecOrderflowPolicyV1,
    pub authority: SecOrderflowAuthorityV1,
    pub units: SecOrderflowUnitsV1,
    pub source_quality: SecOrderflowSourceQualityV1,
}

impl SecOrderflowExperimentManifestV1 {
    pub fn canonical_audit() -> Self {
        Self {
            schema: SEC_ORDERFLOW_RESEARCH_SCHEMA_V1.to_string(),
            mode: SecOrderflowModeV1::AuditOnly,
            run_enabled: false,
            venue: SEC_ORDERFLOW_VENUE_BYBIT_LINEAR.to_string(),
            symbols: vec![
                "MARSCOINUSDT".to_string(),
                "NIULAIUSDT".to_string(),
                "HAJIMIUSDT".to_string(),
            ],
            primary: SecOrderflowPrimaryV1 {
                symbol: "MARSCOINUSDT".to_string(),
                horizon_s: SEC_ORDERFLOW_PRIMARY_HORIZON_S,
                price: SecOrderflowPriceKindV1::MidStrict,
            },
            decision_grid_ms: SEC_ORDERFLOW_DECISION_GRID_MS,
            instant_windows_s: SEC_ORDERFLOW_INSTANT_WINDOWS_S.to_vec(),
            context_windows_s: SEC_ORDERFLOW_CONTEXT_WINDOWS_S.to_vec(),
            horizons_s: SEC_ORDERFLOW_HORIZONS_S.to_vec(),
            targets: vec![
                SecOrderflowTargetKindV1::ReturnBp,
                SecOrderflowTargetKindV1::Direction3,
                SecOrderflowTargetKindV1::Quantiles,
                SecOrderflowTargetKindV1::Mfe,
                SecOrderflowTargetKindV1::Mae,
                SecOrderflowTargetKindV1::Touch,
                SecOrderflowTargetKindV1::FirstTouch,
                SecOrderflowTargetKindV1::MarkReturn,
                SecOrderflowTargetKindV1::BasisChange,
                SecOrderflowTargetKindV1::FundingRealized,
                SecOrderflowTargetKindV1::RealizedClosePnl,
            ],
            freshness_ms: SecOrderflowFreshnessMsV1 {
                mid_event: 1_000,
                mid_receive: 1_000,
                last_upper_bound: 1_000,
                mark_event: 1_000,
                mark_receive: 1_000,
            },
            legacy: SecOrderflowLegacyPolicyV1 {
                availability_quality: SecOrderflowAvailabilityQualityV1::Unknown,
                admission: SecOrderflowAdmissionV1::DiagnosticOnly,
                sampled_mid_max_age_ms: SEC_ORDERFLOW_SAMPLED_MID_MAX_AGE_MS,
                sampled_mid_min_horizon_s: SEC_ORDERFLOW_SAMPLED_MID_MIN_HORIZON_S,
            },
            missing: SecOrderflowMissingPolicyV1 {
                unknown_gap: SecOrderflowGapPolicyV1::Invalid,
                prehistory: SecOrderflowGapPolicyV1::Abstain,
                missing_target: None,
            },
            split: SecOrderflowSplitV1 {
                kind: SecOrderflowSplitKindV1::ChronologicalNestedWalkForward,
                initial_train_days: 14,
                inner_validation_days: 7,
                outer_days: vec![4, 5, 5],
                sealed_days: 7,
                full_dependency_gap_floor_s: SEC_ORDERFLOW_FULL_DEPENDENCY_GAP_FLOOR_S,
                holdout_open: false,
            },
            budget: SecOrderflowBudgetV1 {
                max_pipeline_configs: SEC_ORDERFLOW_MAX_PIPELINE_CONFIGS,
                seeds_per_config: SEC_ORDERFLOW_SEEDS_PER_CONFIG,
                approved_grant: None,
            },
            models_initial: vec![
                SecOrderflowBaselineModelV1::Zero,
                SecOrderflowBaselineModelV1::Persistence,
                SecOrderflowBaselineModelV1::TrainPrior,
                SecOrderflowBaselineModelV1::Momentum,
                SecOrderflowBaselineModelV1::Reversal,
                SecOrderflowBaselineModelV1::Ridge,
                SecOrderflowBaselineModelV1::Cart,
            ],
            cost_assumptions_bp: SecOrderflowCostAssumptionsV1 {
                taker_per_side: SEC_ORDERFLOW_TAKER_PER_SIDE_BP_ASSUMPTION.to_string(),
                maker_per_side: SEC_ORDERFLOW_MAKER_PER_SIDE_BP_ASSUMPTION.to_string(),
                fee_status: SEC_ORDERFLOW_FEE_STATUS.to_string(),
            },
            stress: SecOrderflowStressV1 {
                extra_friction_multipliers: vec![1.0, 1.5, 2.0],
                latency_s: vec![0.25, 1.0, 2.0, 5.0],
            },
            policy: SecOrderflowPolicyV1 {
                initial_notional_usdt: SEC_ORDERFLOW_INITIAL_NOTIONAL_USDT.to_string(),
                per_symbol_max_positions: 1,
                max_hold_s: SEC_ORDERFLOW_MAX_HOLD_S,
                pyramiding: false,
                maker_eligible: false,
            },
            authority: SecOrderflowAuthorityV1::closed(),
            units: SecOrderflowUnitsV1 {
                clock: "utc_int64_ms".to_string(),
                price: "quote_currency".to_string(),
                base_quantity: "base_asset".to_string(),
                notional: "usdt".to_string(),
                return_unit: "bp".to_string(),
                time_unit: "second".to_string(),
            },
            source_quality: SecOrderflowSourceQualityV1 {
                discard_historical_cvd: true,
                discard_precomputed_delta_windows: true,
                disable_wash_fields: true,
                isolate_big_events: true,
                disable_signed_liquidation: true,
                forbid_forward_join: true,
                forbid_gap_zero_fill: true,
                availability_quality: SecOrderflowAvailabilityQualityV1::Unknown,
            },
        }
    }

    pub fn validate(&self) -> Result<(), SecOrderflowError> {
        let invalid = SecOrderflowError::Invalid;
        if self.schema != SEC_ORDERFLOW_RESEARCH_SCHEMA_V1 {
            return Err(invalid("schema is not sec-orderflow-research-v1"));
        }
        if self.mode != SecOrderflowModeV1::AuditOnly {
            return Err(invalid("mode must be audit_only"));
        }
        if self.run_enabled {
            return Err(invalid("run_enabled must be false"));
        }
        if self.venue != SEC_ORDERFLOW_VENUE_BYBIT_LINEAR {
            return Err(invalid("venue must be bybit_linear"));
        }
        if self.symbols.is_empty()
            || self.symbols.iter().any(|symbol| !valid_symbol(symbol))
            || duplicates(&self.symbols)
        {
            return Err(invalid("symbols are invalid"));
        }
        if !self.symbols.contains(&self.primary.symbol)
            || self.primary.horizon_s != SEC_ORDERFLOW_PRIMARY_HORIZON_S
            || self.primary.price != SecOrderflowPriceKindV1::MidStrict
        {
            return Err(invalid(
                "primary target must be MARSCOIN-class 60s mid_strict",
            ));
        }
        if self.decision_grid_ms != SEC_ORDERFLOW_DECISION_GRID_MS {
            return Err(invalid("decision_grid_ms must be 1000"));
        }
        if self.instant_windows_s != SEC_ORDERFLOW_INSTANT_WINDOWS_S {
            return Err(invalid("instant_windows_s must be [1, 5]"));
        }
        if self.context_windows_s != SEC_ORDERFLOW_CONTEXT_WINDOWS_S {
            return Err(invalid("context_windows_s must be [300, 900, 1800]"));
        }
        if self.horizons_s != SEC_ORDERFLOW_HORIZONS_S {
            return Err(invalid("horizons_s must be [5, 10, 30, 60, 300]"));
        }
        if self.targets.is_empty() || duplicates_copy(&self.targets) {
            return Err(invalid("targets are invalid"));
        }
        if [
            self.freshness_ms.mid_event,
            self.freshness_ms.mid_receive,
            self.freshness_ms.last_upper_bound,
            self.freshness_ms.mark_event,
            self.freshness_ms.mark_receive,
        ]
        .into_iter()
        .any(|age| age == 0 || age > 1_000)
        {
            return Err(invalid("strict freshness windows must be 1s"));
        }
        if self.legacy.admission != SecOrderflowAdmissionV1::DiagnosticOnly
            || self.legacy.sampled_mid_max_age_ms != SEC_ORDERFLOW_SAMPLED_MID_MAX_AGE_MS
            || self.legacy.sampled_mid_min_horizon_s != SEC_ORDERFLOW_SAMPLED_MID_MIN_HORIZON_S
        {
            return Err(invalid("legacy admission policy is invalid"));
        }
        if self.legacy.availability_quality == SecOrderflowAvailabilityQualityV1::Verified {
            return Err(invalid(
                "legacy flow cannot claim verified availability_quality",
            ));
        }
        if self.missing.unknown_gap != SecOrderflowGapPolicyV1::Invalid
            || self.missing.prehistory != SecOrderflowGapPolicyV1::Abstain
            || self.missing.missing_target.is_some()
        {
            return Err(invalid("missing-value policy is invalid"));
        }
        if self.split.kind != SecOrderflowSplitKindV1::ChronologicalNestedWalkForward
            || self.split.initial_train_days != 14
            || self.split.inner_validation_days != 7
            || self.split.outer_days != [4, 5, 5]
            || self.split.sealed_days != 7
            || self.split.full_dependency_gap_floor_s != SEC_ORDERFLOW_FULL_DEPENDENCY_GAP_FLOOR_S
            || self.split.holdout_open
        {
            return Err(invalid("split contract is invalid or holdout is open"));
        }
        if self.budget.max_pipeline_configs != SEC_ORDERFLOW_MAX_PIPELINE_CONFIGS
            || self.budget.seeds_per_config != SEC_ORDERFLOW_SEEDS_PER_CONFIG
            || self.budget.approved_grant.is_some()
        {
            return Err(invalid("budget must be ungated with a null grant"));
        }
        if self.models_initial.is_empty() || duplicates_copy(&self.models_initial) {
            return Err(invalid("initial models are invalid"));
        }
        if self.cost_assumptions_bp.taker_per_side != SEC_ORDERFLOW_TAKER_PER_SIDE_BP_ASSUMPTION
            || self.cost_assumptions_bp.maker_per_side != SEC_ORDERFLOW_MAKER_PER_SIDE_BP_ASSUMPTION
            || self.cost_assumptions_bp.fee_status != SEC_ORDERFLOW_FEE_STATUS
        {
            return Err(invalid(
                "cost assumptions must stay unverified historical bounds",
            ));
        }
        if self.stress.extra_friction_multipliers != [1.0, 1.5, 2.0]
            || self.stress.latency_s != [0.25, 1.0, 2.0, 5.0]
            || self
                .stress
                .extra_friction_multipliers
                .iter()
                .chain(self.stress.latency_s.iter())
                .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(invalid("stress grid is invalid"));
        }
        if self.policy.initial_notional_usdt != SEC_ORDERFLOW_INITIAL_NOTIONAL_USDT
            || self.policy.per_symbol_max_positions != 1
            || self.policy.max_hold_s != SEC_ORDERFLOW_MAX_HOLD_S
            || self.policy.pyramiding
            || self.policy.maker_eligible
        {
            return Err(invalid("research policy is invalid"));
        }
        if !self.authority.is_closed() {
            return Err(invalid("authority gates must all be false"));
        }
        if self.units.clock != "utc_int64_ms"
            || self.units.return_unit != "bp"
            || self.units.time_unit != "second"
        {
            return Err(invalid("units are invalid"));
        }
        if !self.source_quality.discard_historical_cvd
            || !self.source_quality.discard_precomputed_delta_windows
            || !self.source_quality.disable_wash_fields
            || !self.source_quality.isolate_big_events
            || !self.source_quality.disable_signed_liquidation
            || !self.source_quality.forbid_forward_join
            || !self.source_quality.forbid_gap_zero_fill
            || self.source_quality.availability_quality
                != SecOrderflowAvailabilityQualityV1::Unknown
        {
            return Err(invalid("source quality contract is invalid"));
        }
        Ok(())
    }

    pub fn sha256(&self) -> Result<String, SecOrderflowError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|_| SecOrderflowError::Invalid("manifest must serialize"))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

fn valid_symbol(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && value.ends_with("USDT")
}

fn duplicates(values: &[String]) -> bool {
    let unique = values.iter().collect::<BTreeSet<_>>();
    unique.len() != values.len()
}

fn duplicates_copy<T: Copy + Ord>(values: &[T]) -> bool {
    let unique = values.iter().copied().collect::<BTreeSet<_>>();
    unique.len() != values.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_audit_manifest_validates_and_hashes_stably() {
        let manifest = SecOrderflowExperimentManifestV1::canonical_audit();
        manifest.validate().unwrap();
        let digest = manifest.sha256().unwrap();
        assert_eq!(digest.len(), 64);
        assert_eq!(digest, manifest.sha256().unwrap());
        assert!(!manifest.run_enabled);
        assert!(manifest.authority.is_closed());
        assert!(!manifest.split.holdout_open);
        assert!(manifest.budget.approved_grant.is_none());
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut value =
            serde_json::to_value(SecOrderflowExperimentManifestV1::canonical_audit()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("orders".into(), serde_json::json!(true));
        let error = serde_json::from_value::<SecOrderflowExperimentManifestV1>(value).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn rejects_run_enabled_and_open_holdout_and_order_authority() {
        let mut enabled = SecOrderflowExperimentManifestV1::canonical_audit();
        enabled.run_enabled = true;
        assert_eq!(
            enabled.validate().unwrap_err(),
            SecOrderflowError::Invalid("run_enabled must be false")
        );

        let mut holdout = SecOrderflowExperimentManifestV1::canonical_audit();
        holdout.split.holdout_open = true;
        assert_eq!(
            holdout.validate().unwrap_err(),
            SecOrderflowError::Invalid("split contract is invalid or holdout is open")
        );

        let mut orders = SecOrderflowExperimentManifestV1::canonical_audit();
        orders.authority.orders = true;
        assert_eq!(
            orders.validate().unwrap_err(),
            SecOrderflowError::Invalid("authority gates must all be false")
        );
    }

    #[test]
    fn rejects_numeric_missing_target_encoding() {
        let mut manifest = SecOrderflowExperimentManifestV1::canonical_audit();
        manifest.missing.missing_target = Some(0.0);
        assert_eq!(
            manifest.validate().unwrap_err(),
            SecOrderflowError::Invalid("missing-value policy is invalid")
        );
    }

    #[test]
    fn rejects_verified_legacy_availability_claim() {
        let mut manifest = SecOrderflowExperimentManifestV1::canonical_audit();
        manifest.legacy.availability_quality = SecOrderflowAvailabilityQualityV1::Verified;
        assert_eq!(
            manifest.validate().unwrap_err(),
            SecOrderflowError::Invalid("legacy flow cannot claim verified availability_quality")
        );
    }
}
