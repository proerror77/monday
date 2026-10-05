//! Fixed executable strategy contracts. Research evidence is verified by the
//! producer; the runtime consumes its sealed projection and deployment approval.
use chrono::{DateTime, Utc};
use hft_factor_dsl::{model_program::FrozenFactorModelV1, validate_live_formula, FactorAst};
use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ONNX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ONNX_TENSOR_ELEMENTS: usize = 4 * 1024 * 1024;
pub const LOB_ONNX_PREPROCESSING_VERSION: &str = "lob-relative-price-log-size-v1";

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RuntimeBundleError {
    #[error("invalid runtime strategy contract")]
    Invalid,
    #[error("runtime bundle hash does not match its canonical payload")]
    HashMismatch,
    #[error("runtime contract serialization failed")]
    Serialization,
}
pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn canonical_hash(value: &impl Serialize) -> Result<String, RuntimeBundleError> {
    struct Writer(Sha256);
    impl std::io::Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Writer(Sha256::new());
    serde_json::to_writer(&mut writer, value).map_err(|_| RuntimeBundleError::Serialization)?;
    Ok(hex::encode(writer.0.finalize()))
}
fn require(valid: bool) -> Result<(), RuntimeBundleError> {
    if valid {
        Ok(())
    } else {
        Err(RuntimeBundleError::Invalid)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCosts {
    pub fee_bps: f64,
    pub rebate_bps: f64,
    pub funding_bps: f64,
    pub latency_bps: f64,
    pub slippage_bps: f64,
    pub cross_spread: bool,
    pub position_notional_usd: f64,
    pub capacity_depth_levels: usize,
    pub max_book_depth_fraction: f64,
}
impl RuntimeCosts {
    pub fn validate(&self) -> Result<(), RuntimeBundleError> {
        let disabled = self.position_notional_usd == 0.0
            && self.capacity_depth_levels == 0
            && self.max_book_depth_fraction == 0.0;
        let enabled = self.position_notional_usd.is_finite()
            && self.position_notional_usd > 0.0
            && self.capacity_depth_levels > 0
            && self.max_book_depth_fraction.is_finite()
            && self.max_book_depth_fraction > 0.0
            && self.max_book_depth_fraction <= 1.0;
        require(
            [
                self.fee_bps,
                self.rebate_bps,
                self.funding_bps,
                self.latency_bps,
                self.slippage_bps,
            ]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
                && (disabled || enabled),
        )
    }
    pub fn capacity_enabled(&self) -> bool {
        self.position_notional_usd > 0.0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeExecutionContract {
    pub zero_epsilon: f64,
    pub observation_frequency_millis: u64,
    pub tick_size: String,
    pub step_size: String,
    pub min_notional: String,
    pub costs: RuntimeCosts,
}
impl RuntimeExecutionContract {
    pub fn validate(&self) -> Result<(), RuntimeBundleError> {
        self.costs.validate()?;
        require(
            self.zero_epsilon.is_finite()
                && self.zero_epsilon >= 0.0
                && self.observation_frequency_millis > 0,
        )?;
        for decimal in [&self.tick_size, &self.step_size, &self.min_notional] {
            require(
                decimal
                    .parse::<rust_decimal::Decimal>()
                    .is_ok_and(|v| v > rust_decimal::Decimal::ZERO),
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCexStrategy {
    pub mission_id: String,
    pub precommit_id: String,
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub executable_formula: FactorAst,
    pub execution: RuntimeExecutionContract,
}
impl RuntimeCexStrategy {
    fn validate(&self) -> Result<(), RuntimeBundleError> {
        require(
            !self.mission_id.trim().is_empty()
                && self.precommit_id == format!("cex-final-precommit:{}", self.mission_id)
                && self.venue == "binance"
                && matches!(self.market.as_str(), "spot" | "usdm")
                && !self.symbol.is_empty()
                && self.symbol == self.symbol.to_ascii_uppercase()
                && self.execution.zero_epsilon.to_bits() == f64::EPSILON.to_bits(),
        )?;
        validate_live_formula(&self.executable_formula).map_err(|_| RuntimeBundleError::Invalid)?;
        self.execution.validate()
    }
    pub fn runtime_contract(&self) -> Result<RuntimeExecutionContract, RuntimeBundleError> {
        self.validate()?;
        Ok(self.execution.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFrozenStrategy {
    pub mission_id: String,
    pub precommit_id: String,
    pub program: FrozenFactorModelV1,
    pub execution: RuntimeExecutionContract,
}
impl RuntimeFrozenStrategy {
    fn validate(&self) -> Result<(), RuntimeBundleError> {
        self.program
            .validate()
            .map_err(|_| RuntimeBundleError::Invalid)?;
        self.execution.validate()?;
        require(
            !self.mission_id.trim().is_empty()
                && self.precommit_id == format!("cex-final-precommit:{}", self.mission_id)
                && self.execution.zero_epsilon == 0.0
                && self.program.observation_frequency_millis
                    == self.execution.observation_frequency_millis
                && self.program.cross_spread == self.execution.costs.cross_spread
                && self.program.base_costs.funding_bps == self.execution.costs.funding_bps
                && self.program.base_costs.one_way_cost_bps
                    == self.execution.costs.fee_bps.max(0.0) - self.execution.costs.rebate_bps
                        + self.execution.costs.latency_bps.max(0.0)
                        + self.execution.costs.slippage_bps,
        )
    }
    pub fn runtime_contract(&self) -> Result<RuntimeExecutionContract, RuntimeBundleError> {
        self.validate()?;
        Ok(self.execution.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TensorElementType {
    Float32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TensorSpec {
    pub name: String,
    pub element_type: TensorElementType,
    pub dimensions: Vec<Option<usize>>,
}
impl TensorSpec {
    pub fn validate(&self) -> Result<(), RuntimeBundleError> {
        require(
            !self.name.trim().is_empty()
                && !self.dimensions.is_empty()
                && !self.dimensions.contains(&Some(0)),
        )?;
        let mut size = 1usize;
        for dimension in self.dimensions.iter().flatten() {
            size = size
                .checked_mul(*dimension)
                .ok_or(RuntimeBundleError::Invalid)?;
            require(size <= MAX_ONNX_TENSOR_ELEMENTS)?;
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeOnnxModel {
    pub artifact: ArtifactRef,
    pub byte_len: u64,
    pub opset: u32,
    pub preprocessing_version: String,
    pub inputs: Vec<TensorSpec>,
    pub output: TensorSpec,
}
impl RuntimeOnnxModel {
    pub fn validate(&self) -> Result<(), RuntimeBundleError> {
        require(
            !self.artifact.uri.trim().is_empty()
                && self.artifact.content_type == "application/onnx"
                && self.byte_len > 0
                && self.byte_len <= MAX_ONNX_ARTIFACT_BYTES
                && self.opset > 0
                && self.preprocessing_version == LOB_ONNX_PREPROCESSING_VERSION
                && !self.inputs.is_empty()
                && self.artifact.checksum.as_ref().is_some_and(|s| digest(s)),
        )?;
        for input in &self.inputs {
            input.validate()?;
        }
        self.output.validate()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RuntimeArtifact {
    ProbabilityReversal {
        spec: Box<hft_research_manifest::prediction_probability::ProbabilityReversalSpecV1>,
    },
    Formula {
        ast: FactorAst,
    },
    Onnx {
        model: RuntimeOnnxModel,
    },
    CexFourStage {
        strategy: RuntimeCexStrategy,
    },
    FrozenModel {
        strategy: Box<RuntimeFrozenStrategy>,
    },
}
impl RuntimeArtifact {
    fn validate(&self) -> Result<(), RuntimeBundleError> {
        match self {
            Self::ProbabilityReversal { spec } => {
                spec.validate().map_err(|_| RuntimeBundleError::Invalid)
            }
            Self::Formula { ast } => require(
                validate_live_formula(ast)
                    .map_err(|_| RuntimeBundleError::Invalid)?
                    .history_rows
                    == 1,
            ),
            Self::Onnx { model } => model.validate(),
            Self::CexFourStage { strategy } => strategy.validate(),
            Self::FrozenModel { strategy } => strategy.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBundle {
    pub schema_version: String,
    pub source_bundle_hash: String,
    pub bundle_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub dataset_manifest_id: ManifestId,
    pub evaluator_version: String,
    pub evaluation_protocol_hash: String,
    pub evaluator_config_hash: String,
    pub evaluation_metrics_hash: String,
    pub sealed_evaluation_hash: String,
    pub artifact: RuntimeArtifact,
    pub bundle_hash: String,
    pub created_at: DateTime<Utc>,
}
impl RuntimeBundle {
    pub fn finalize(mut self) -> Result<Self, RuntimeBundleError> {
        self.validate_fields()?;
        self.bundle_hash = self.calculated_hash()?;
        Ok(self)
    }
    pub fn validate(&self) -> Result<(), RuntimeBundleError> {
        self.validate_fields()?;
        require(digest(&self.bundle_hash))?;
        if self.bundle_hash != self.calculated_hash()? {
            return Err(RuntimeBundleError::HashMismatch);
        }
        Ok(())
    }
    fn validate_fields(&self) -> Result<(), RuntimeBundleError> {
        require(
            self.schema_version == "monday.runtime_bundle.v1"
                && !self.bundle_id.trim().is_empty()
                && !self.candidate_id.trim().is_empty()
                && !self.evaluator_version.trim().is_empty()
                && [
                    &self.source_bundle_hash,
                    &self.candidate_content_hash,
                    &self.evaluation_protocol_hash,
                    &self.evaluator_config_hash,
                    &self.evaluation_metrics_hash,
                    &self.sealed_evaluation_hash,
                ]
                .into_iter()
                .all(|s| digest(s)),
        )?;
        self.dataset_manifest_id
            .validate()
            .map_err(|_| RuntimeBundleError::Invalid)?;
        self.artifact.validate()
    }
    pub fn calculated_hash(&self) -> Result<String, RuntimeBundleError> {
        canonical_hash(&(
            &self.schema_version,
            &self.source_bundle_hash,
            &self.bundle_id,
            &self.candidate_id,
            &self.candidate_content_hash,
            &self.dataset_manifest_id,
            &self.evaluator_version,
            &self.evaluation_protocol_hash,
            &self.evaluator_config_hash,
            &self.evaluation_metrics_hash,
            &self.sealed_evaluation_hash,
            &self.artifact,
            &self.created_at,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> RuntimeBundle {
        serde_json::from_value(serde_json::json!({
            "schema_version":"monday.runtime_bundle.v1","source_bundle_hash":"a".repeat(64),
            "bundle_id":"bundle","candidate_id":"candidate","candidate_content_hash":"b".repeat(64),
            "dataset_manifest_id":"dataset","evaluator_version":"sealed-holdout-v5",
            "evaluation_protocol_hash":"c".repeat(64),"evaluator_config_hash":"d".repeat(64),
            "evaluation_metrics_hash":"e".repeat(64),"sealed_evaluation_hash":"f".repeat(64),
            "artifact":{"Formula":{"ast":{"Terminal":{"Field":"book_imbalance"}}}},
            "bundle_hash":"","created_at":"2026-10-05T00:00:00Z"
        }))
        .unwrap()
    }
    #[test]
    fn changed_executable_or_source_is_not_the_approved_runtime_artifact() {
        let sealed = fixture().finalize().unwrap();
        for field in 0..5 {
            let mut changed = sealed.clone();
            match field {
                0 => changed.source_bundle_hash = "1".repeat(64),
                1 => changed.bundle_id.push_str("-changed"),
                2 => changed.candidate_id.push_str("-changed"),
                3 => changed.created_at += chrono::Duration::seconds(1),
                _ => {
                    changed.artifact = RuntimeArtifact::Formula {
                        ast: serde_json::from_value(
                            serde_json::json!({"Terminal":{"Field":"mid_price"}}),
                        )
                        .unwrap(),
                    }
                }
            }
            assert_eq!(changed.validate(), Err(RuntimeBundleError::HashMismatch));
        }
        let mut old = serde_json::to_value(sealed).unwrap();
        old.as_object_mut().unwrap().remove("schema_version");
        old.as_object_mut().unwrap().remove("source_bundle_hash");
        assert!(serde_json::from_value::<RuntimeBundle>(old).is_err());
    }
    #[test]
    fn runtime_limits_cannot_accept_unusable_precision_or_costs() {
        let mut contract = RuntimeExecutionContract {
            zero_epsilon: f64::EPSILON,
            observation_frequency_millis: 1000,
            tick_size: "0.01".into(),
            step_size: "0.001".into(),
            min_notional: "5".into(),
            costs: RuntimeCosts {
                fee_bps: 1.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.0,
                slippage_bps: 0.0,
                cross_spread: true,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
        };
        contract.validate().unwrap();
        contract.tick_size = "0".into();
        assert!(contract.validate().is_err());
        contract.tick_size = "0.01".into();
        contract.costs.fee_bps = f64::NAN;
        assert!(contract.validate().is_err());
    }
}
