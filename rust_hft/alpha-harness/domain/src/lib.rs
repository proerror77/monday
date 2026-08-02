//! Trust-boundary contracts for bounded Loop Engineer research and runtime deployment.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hft_factor_dsl::{validate_live_formula, FactorAst, LiveFormulaCapabilityError};
use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const MAX_ONNX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ONNX_TENSOR_ELEMENTS: usize = 4 * 1024 * 1024;
pub const SEALED_HOLDOUT_EVALUATOR_VERSION: &str = "sealed-holdout-v4";
pub const WALK_FORWARD_EVALUATOR_VERSION: &str = "purged-walk-forward-v4";
pub const ONNX_WALK_FORWARD_EVALUATOR_VERSION: &str = "onnx-purged-walk-forward-v3";
pub const ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION: &str = "onnx-sealed-holdout-v3";
pub const LOB_ONNX_PREPROCESSING_VERSION: &str = "lob-relative-price-log-size-v1";
pub const EVALUATION_PROTOCOL_VERSION_V1: &str = "evaluation-protocol-v1";
pub const CEX_MCTS_RESEARCH_RECEIPT_VERSION_V1: &str = "cex-mcts-research-receipt-v1";
pub const CEX_RESEARCH_MISSION_SCHEMA_V1: &str = "cex-research-mission-v1";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("{0} cannot be empty")]
    EmptyField(&'static str),
    #[error("search budget must include at least one candidate and one positive limit")]
    InvalidSearchBudget,
    #[error("invalid mission status transition")]
    InvalidMissionTransition,
    #[error("mission completion policy is invalid")]
    InvalidMissionCompletionPolicy,
    #[error("mission terminal reason does not match its status or policy")]
    InvalidMissionTerminalReason,
    #[error("loop run is invalid")]
    InvalidLoopRun,
    #[error("deployment limits must be finite and non-negative")]
    InvalidDeploymentLimit,
    #[error("deployment hashes must be lowercase SHA-256 values")]
    InvalidDeploymentHash,
    #[error("deployment validity window is invalid")]
    InvalidValidityWindow,
    #[error("deployment envelope is not valid yet")]
    NotYetValid,
    #[error("deployment envelope has expired")]
    Expired,
    #[error("deployment payload hash does not match")]
    PayloadHashMismatch,
    #[error("deployment signing key is not trusted")]
    UnknownSigningKey,
    #[error("deployment signature is invalid")]
    InvalidSignature,
    #[error("deployment signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("deployment nonce has already been consumed")]
    NonceReplay,
    #[error("deployment binding does not match the runtime")]
    RuntimeBindingMismatch,
    #[error("deployment exceeds a runtime hard limit")]
    RuntimeLimitExceeded,
    #[error("deployment approval evidence does not match runtime-owned policy")]
    ApprovalEvidenceMismatch,
    #[error("canonical deployment serialization failed")]
    CanonicalSerialization,
    #[error("runtime attribution metrics must be finite")]
    InvalidAttributionMetric,
    #[error("runtime attribution outcome does not match its event kind")]
    InvalidAttributionOutcome,
    #[error("runtime attribution payload hash does not match")]
    AttributionPayloadHashMismatch,
    #[error("runtime attribution signing key is not trusted")]
    UnknownAttributionSigningKey,
    #[error("runtime attribution signature is invalid")]
    InvalidAttributionSignature,
    #[error("runtime attribution signature encoding is invalid")]
    InvalidAttributionSignatureEncoding,
    #[error("search-policy validator scores must be finite")]
    InvalidPolicyScore,
    #[error("candidate artifact is research-only and cannot be promoted")]
    ResearchOnlyArtifact,
    #[error("formula is not live executable: {0}")]
    UnsupportedLiveFormula(#[from] LiveFormulaCapabilityError),
    #[error("strategy bundle is invalid")]
    InvalidStrategyBundle,
    #[error("strategy bundle hash does not match its canonical payload")]
    StrategyBundleHashMismatch,
    #[error("CEX MCTS research receipt is invalid")]
    InvalidCexMctsResearchReceipt,
    #[error("CEX MCTS research receipt hash does not match its canonical payload")]
    CexMctsResearchReceiptHashMismatch,
    #[error("promotion record does not match its candidate, evidence, or bundle")]
    PromotionBindingMismatch,
    #[error("evaluation evidence is inconsistent")]
    InvalidEvaluationEvidence,
    #[error("formula evaluator configuration is invalid")]
    InvalidEvaluatorConfiguration,
    #[error("evaluation protocol is invalid")]
    InvalidEvaluationProtocol,
    #[error("CEX Research Mission is invalid: {0}")]
    InvalidCexResearchMission(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationWalkForwardV1 {
    pub initial_train_rows: usize,
    pub validation_rows: usize,
    pub fold_count: usize,
    pub purge_rows: usize,
    pub embargo_rows: usize,
    pub sealed_holdout_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCostsV1 {
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

impl EvaluationCostsV1 {
    pub fn capacity_enabled(&self) -> bool {
        self.position_notional_usd > 0.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationLabelSpecV1 {
    pub horizon_buckets: usize,
    pub observation_frequency_millis: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IcirDefinitionV1 {
    FoldMeanOverPopulationStddevWithEpsilonFloorV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharpeDefinitionV1 {
    MeanFoldPerObservationPopulationStddevUnannualizedWithEpsilonFloorV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationMetricDefinitionsV1 {
    pub icir: IcirDefinitionV1,
    pub sharpe: SharpeDefinitionV1,
}

impl Default for EvaluationMetricDefinitionsV1 {
    fn default() -> Self {
        Self {
            icir: IcirDefinitionV1::FoldMeanOverPopulationStddevWithEpsilonFloorV1,
            sharpe:
                SharpeDefinitionV1::MeanFoldPerObservationPopulationStddevUnannualizedWithEpsilonFloorV1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationProtocolV1 {
    pub version: String,
    pub walk_forward: EvaluationWalkForwardV1,
    pub costs: EvaluationCostsV1,
    pub labels: EvaluationLabelSpecV1,
    pub metrics: EvaluationMetricDefinitionsV1,
}

impl EvaluationProtocolV1 {
    pub fn new(
        walk_forward: EvaluationWalkForwardV1,
        costs: EvaluationCostsV1,
        labels: EvaluationLabelSpecV1,
    ) -> Result<Self, DomainError> {
        let protocol = Self {
            version: EVALUATION_PROTOCOL_VERSION_V1.to_string(),
            walk_forward,
            costs,
            labels,
            metrics: EvaluationMetricDefinitionsV1::default(),
        };
        protocol.validate()?;
        Ok(protocol)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let fold_step = self
            .walk_forward
            .validation_rows
            .checked_add(self.walk_forward.embargo_rows);
        let schedule_end = fold_step
            .and_then(|rows| rows.checked_mul(self.walk_forward.fold_count))
            .and_then(|rows| rows.checked_add(self.walk_forward.initial_train_rows))
            .and_then(|rows| rows.checked_add(self.walk_forward.purge_rows))
            .and_then(|rows| rows.checked_add(self.walk_forward.sealed_holdout_rows));
        let capacity_disabled = self.costs.position_notional_usd == 0.0
            && self.costs.capacity_depth_levels == 0
            && self.costs.max_book_depth_fraction == 0.0;
        let capacity_enabled = self.costs.position_notional_usd.is_finite()
            && self.costs.position_notional_usd > 0.0
            && self.costs.capacity_depth_levels > 0
            && self.costs.max_book_depth_fraction.is_finite()
            && self.costs.max_book_depth_fraction > 0.0
            && self.costs.max_book_depth_fraction <= 1.0;
        if self.version != EVALUATION_PROTOCOL_VERSION_V1
            || self.walk_forward.initial_train_rows == 0
            || self.walk_forward.validation_rows == 0
            || self.walk_forward.fold_count == 0
            || self.walk_forward.sealed_holdout_rows == 0
            || self.labels.horizon_buckets == 0
            || self.labels.observation_frequency_millis == 0
            || self.walk_forward.purge_rows < self.labels.horizon_buckets
            || schedule_end.is_none()
            || [
                self.costs.fee_bps,
                self.costs.rebate_bps,
                self.costs.funding_bps,
                self.costs.latency_bps,
                self.costs.slippage_bps,
            ]
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
            || !(capacity_disabled || capacity_enabled)
            || self.metrics != EvaluationMetricDefinitionsV1::default()
        {
            return Err(DomainError::InvalidEvaluationProtocol);
        }
        Ok(())
    }

    pub fn content_hash(&self) -> Result<String, DomainError> {
        self.validate()?;
        canonical_json_hash(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexResearchVenueV1 {
    Binance,
}

impl CexResearchVenueV1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Binance => "binance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexResearchMarketV1 {
    Spot,
    Usdm,
}

impl CexResearchMarketV1 {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchContentRefV1 {
    pub id: String,
    pub content_sha256: String,
}

impl CexResearchContentRefV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.id.trim().is_empty() || !valid_content_sha256(&self.content_sha256) {
            return Err(DomainError::InvalidCexResearchMission(
                "content reference is incomplete",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchInstrumentV1 {
    pub venue: CexResearchVenueV1,
    pub market: CexResearchMarketV1,
    pub symbol: String,
    pub horizon: EvaluationLabelSpecV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchHypothesisTargetV1 {
    pub name: String,
    pub horizon: EvaluationLabelSpecV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchFalsificationTestV1 {
    pub test_id: String,
    pub reject_when: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchHypothesisV1 {
    pub hypothesis_id: String,
    pub statement: String,
    pub target: CexResearchHypothesisTargetV1,
    pub required_feature_families: Vec<String>,
    pub required_template_families: Vec<String>,
    pub falsification_tests: Vec<CexResearchFalsificationTestV1>,
    pub source_evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchInputBindingsV1 {
    pub dataset: CexResearchContentRefV1,
    pub snapshot: CexResearchContentRefV1,
    pub partition: CexResearchContentRefV1,
    pub source: CexResearchContentRefV1,
    pub feature: CexResearchContentRefV1,
    pub materialization: CexResearchContentRefV1,
}

impl CexResearchInputBindingsV1 {
    fn validate(&self) -> Result<(), DomainError> {
        for reference in [
            &self.dataset,
            &self.snapshot,
            &self.partition,
            &self.source,
            &self.feature,
            &self.materialization,
        ] {
            reference.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchPolicyBindingsV1 {
    pub gp: CexResearchContentRefV1,
    pub screening: CexResearchContentRefV1,
    pub baseline: CexResearchContentRefV1,
    pub subset_search: CexResearchContentRefV1,
    pub weight: CexResearchContentRefV1,
    pub evaluation: CexResearchContentRefV1,
    pub replay: CexResearchContentRefV1,
    pub holdout: CexResearchContentRefV1,
}

impl CexResearchPolicyBindingsV1 {
    fn validate(&self) -> Result<(), DomainError> {
        for reference in [
            &self.gp,
            &self.screening,
            &self.baseline,
            &self.subset_search,
            &self.weight,
            &self.evaluation,
            &self.replay,
            &self.holdout,
        ] {
            reference.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexResearchEvidenceKindV1 {
    TrainingValidation,
    SignedPaper,
    SignedShadow,
    ExposedHoldout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchEvidenceSignatureV1 {
    pub key_id: String,
    pub signature_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchEvidenceRefV1 {
    pub evidence_id: String,
    pub kind: CexResearchEvidenceKindV1,
    pub source_mission_id: String,
    pub source_search_lineage_id: String,
    pub artifact_sha256: String,
    #[serde(default)]
    pub signature: Option<CexResearchEvidenceSignatureV1>,
    #[serde(default)]
    pub holdout_id: Option<String>,
}

impl CexResearchEvidenceRefV1 {
    fn validate(&self) -> Result<(), DomainError> {
        if [
            self.evidence_id.as_str(),
            self.source_mission_id.as_str(),
            self.source_search_lineage_id.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || !valid_content_sha256(&self.artifact_sha256)
        {
            return Err(DomainError::InvalidCexResearchMission(
                "evidence reference is incomplete",
            ));
        }
        match self.kind {
            CexResearchEvidenceKindV1::TrainingValidation
                if self.signature.is_none() && self.holdout_id.is_none() => {}
            CexResearchEvidenceKindV1::SignedPaper | CexResearchEvidenceKindV1::SignedShadow
                if self.holdout_id.is_none()
                    && self.signature.as_ref().is_some_and(|signature| {
                        !signature.key_id.trim().is_empty()
                            && valid_content_sha256(&signature.signature_sha256)
                    }) => {}
            CexResearchEvidenceKindV1::ExposedHoldout
                if self.signature.is_none()
                    && self
                        .holdout_id
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty()) => {}
            _ => {
                return Err(DomainError::InvalidCexResearchMission(
                    "evidence kind fields are inconsistent",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchSearchPlanV1 {
    pub seed: u64,
    #[serde(deserialize_with = "deserialize_cex_search_budget")]
    pub budget: SearchBudget,
    pub max_new_iterations: usize,
}

fn deserialize_cex_search_budget<'de, D>(deserializer: D) -> Result<SearchBudget, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictSearchBudget {
        max_candidates: usize,
        max_expansions: u64,
        max_tokens: u64,
        max_seconds: u64,
    }

    let budget = StrictSearchBudget::deserialize(deserializer)?;
    Ok(SearchBudget {
        max_candidates: budget.max_candidates,
        max_expansions: budget.max_expansions,
        max_tokens: budget.max_tokens,
        max_seconds: budget.max_seconds,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchHoldoutV1 {
    pub holdout_id: String,
    pub state: CexResearchHoldoutStateV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CexResearchHoldoutStateV1 {
    Unopened,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchMissionSpecV1 {
    pub objective: String,
    pub search_lineage_id: String,
    pub data_mission_id: String,
    pub instrument: CexResearchInstrumentV1,
    pub hypotheses: Vec<CexResearchHypothesisV1>,
    pub inputs: CexResearchInputBindingsV1,
    pub policies: CexResearchPolicyBindingsV1,
    pub evidence: Vec<CexResearchEvidenceRefV1>,
    pub feature_fields: Vec<String>,
    pub search: CexResearchSearchPlanV1,
    pub evaluation_protocol: EvaluationProtocolV1,
    pub holdout: CexResearchHoldoutV1,
}

impl CexResearchMissionSpecV1 {
    fn validate(&self) -> Result<(), DomainError> {
        if [
            self.objective.as_str(),
            self.search_lineage_id.as_str(),
            self.data_mission_id.as_str(),
            self.instrument.symbol.as_str(),
            self.holdout.holdout_id.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || self.instrument.symbol != self.instrument.symbol.to_ascii_uppercase()
            || self.instrument.horizon.horizon_buckets == 0
            || self.instrument.horizon.observation_frequency_millis == 0
        {
            return Err(DomainError::InvalidCexResearchMission(
                "mission scope is incomplete",
            ));
        }
        self.inputs.validate()?;
        self.policies.validate()?;
        self.search.budget.validate()?;
        self.evaluation_protocol.validate()?;
        if self.search.max_new_iterations == 0
            || self.search.budget.max_tokens != 0
            || self.evaluation_protocol.labels != self.instrument.horizon
            || self.policies.evaluation.content_sha256 != self.evaluation_protocol.content_hash()?
            || self.policies.subset_search.content_sha256 != canonical_json_hash(&self.search)?
            || !non_empty_unique(&self.feature_fields)
            || self.hypotheses.is_empty()
            || self.evidence.is_empty()
        {
            return Err(DomainError::InvalidCexResearchMission(
                "mission policies or research plan are inconsistent",
            ));
        }

        let mut evidence_ids = BTreeSet::new();
        for evidence in &self.evidence {
            evidence.validate()?;
            if !evidence_ids.insert(evidence.evidence_id.as_str()) {
                return Err(DomainError::InvalidCexResearchMission(
                    "evidence ids are not unique",
                ));
            }
            if evidence.kind == CexResearchEvidenceKindV1::ExposedHoldout
                && (evidence.source_search_lineage_id == self.search_lineage_id
                    || evidence.holdout_id.as_deref() == Some(self.holdout.holdout_id.as_str()))
            {
                return Err(DomainError::InvalidCexResearchMission(
                    "exposed holdout evidence cannot feed the same search or holdout",
                ));
            }
        }

        let mut hypothesis_ids = BTreeSet::new();
        for hypothesis in &self.hypotheses {
            let mut test_ids = BTreeSet::new();
            if hypothesis.hypothesis_id.trim().is_empty()
                || hypothesis.statement.trim().is_empty()
                || hypothesis.target.name.trim().is_empty()
                || hypothesis.target.horizon != self.instrument.horizon
                || !hypothesis_ids.insert(hypothesis.hypothesis_id.as_str())
                || !non_empty_unique(&hypothesis.required_feature_families)
                || !non_empty_unique(&hypothesis.required_template_families)
                || hypothesis.falsification_tests.is_empty()
                || hypothesis.falsification_tests.iter().any(|test| {
                    test.test_id.trim().is_empty()
                        || test.reject_when.trim().is_empty()
                        || !test_ids.insert(test.test_id.as_str())
                })
                || !non_empty_unique(&hypothesis.source_evidence_ids)
                || hypothesis
                    .source_evidence_ids
                    .iter()
                    .any(|id| !evidence_ids.contains(id.as_str()))
            {
                return Err(DomainError::InvalidCexResearchMission(
                    "hypothesis is incomplete or inconsistent",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CexResearchOperationalMetadataV1 {
    #[serde(default)]
    pub submitted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexResearchMissionArtifactV1 {
    pub schema_version: String,
    pub spec: CexResearchMissionSpecV1,
    #[serde(default)]
    pub operational: CexResearchOperationalMetadataV1,
}

impl CexResearchMissionArtifactV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.schema_version != CEX_RESEARCH_MISSION_SCHEMA_V1 {
            return Err(DomainError::InvalidCexResearchMission(
                "schema version is unsupported",
            ));
        }
        self.spec.validate()?;
        let mission_id = self.semantic_id_unchecked()?;
        if self.spec.evidence.iter().any(|evidence| {
            evidence.kind == CexResearchEvidenceKindV1::ExposedHoldout
                && evidence.source_mission_id == mission_id
        }) {
            return Err(DomainError::InvalidCexResearchMission(
                "exposed holdout evidence cannot feed the same mission",
            ));
        }
        Ok(())
    }

    pub fn semantic_id(&self) -> Result<String, DomainError> {
        self.validate()?;
        self.semantic_id_unchecked()
    }

    fn semantic_id_unchecked(&self) -> Result<String, DomainError> {
        #[derive(Serialize)]
        struct SemanticMission<'a> {
            schema_version: &'a str,
            spec: &'a CexResearchMissionSpecV1,
        }
        let hash = canonical_json_hash(&SemanticMission {
            schema_version: &self.schema_version,
            spec: &self.spec,
        })?;
        Ok(format!("cex-mission-{hash}"))
    }
}

fn non_empty_unique(values: &[String]) -> bool {
    !values.is_empty()
        && values.iter().all(|value| !value.trim().is_empty())
        && values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

fn valid_content_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MultipleTestingAdjustment {
    GaussianExpectedMaximum,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaEvaluatorConfig {
    pub min_validation_rows: usize,
    pub min_trades: usize,
    pub min_time_series_ic: f64,
    pub min_time_series_rank_ic: f64,
    pub min_time_series_icir: f64,
    pub min_time_series_rank_icir: f64,
    pub min_positive_ic_ratio: f64,
    pub min_fold_mean_return: f64,
    pub min_aggregate_score: f64,
    pub max_drawdown: f64,
    pub multiple_testing_trials: usize,
    pub multiple_testing_adjustment: MultipleTestingAdjustment,
    pub max_abs_signal: f64,
}

impl Default for FormulaEvaluatorConfig {
    fn default() -> Self {
        Self {
            min_validation_rows: 30,
            min_trades: 30,
            min_time_series_ic: 0.01,
            min_time_series_rank_ic: 0.01,
            min_time_series_icir: 0.5,
            min_time_series_rank_icir: 0.5,
            min_positive_ic_ratio: 0.6,
            min_fold_mean_return: 0.000_001,
            min_aggregate_score: 2.0,
            max_drawdown: 0.20,
            multiple_testing_trials: 1,
            multiple_testing_adjustment: MultipleTestingAdjustment::GaussianExpectedMaximum,
            max_abs_signal: 1.0e12,
        }
    }
}

impl FormulaEvaluatorConfig {
    pub fn for_trials(multiple_testing_trials: usize) -> Result<Self, DomainError> {
        let config = Self {
            multiple_testing_trials,
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn for_mission(mission: &ResearchMission) -> Result<Self, DomainError> {
        let multiple_testing_trials = match mission.validator_spec.get("multiple_testing_trials") {
            Some(value) => usize::try_from(
                value
                    .as_u64()
                    .ok_or(DomainError::InvalidEvaluatorConfiguration)?,
            )
            .map_err(|_| DomainError::InvalidEvaluatorConfiguration)?,
            None => mission.search_budget.max_candidates,
        };
        if multiple_testing_trials < mission.search_budget.max_candidates {
            return Err(DomainError::InvalidEvaluatorConfiguration);
        }
        let mut config = Self::for_trials(multiple_testing_trials)?;
        if let Some(value) = optional_config_f64(&mission.validator_spec, "min_time_series_ic")? {
            config.min_time_series_ic = value;
        }
        if let Some(value) =
            optional_config_f64(&mission.validator_spec, "min_time_series_rank_ic")?
        {
            config.min_time_series_rank_ic = value;
        }
        if let Some(value) = optional_config_f64(&mission.validator_spec, "min_time_series_icir")? {
            config.min_time_series_icir = value;
        }
        if let Some(value) =
            optional_config_f64(&mission.validator_spec, "min_time_series_rank_icir")?
        {
            config.min_time_series_rank_icir = value;
        }
        if let Some(value) = optional_config_f64(&mission.validator_spec, "min_positive_ic_ratio")?
        {
            config.min_positive_ic_ratio = value;
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.min_validation_rows < 2
            || self.min_trades == 0
            || self.min_trades > self.min_validation_rows
            || !self.min_time_series_ic.is_finite()
            || !(0.0..=1.0).contains(&self.min_time_series_ic)
            || !self.min_time_series_rank_ic.is_finite()
            || !(0.0..=1.0).contains(&self.min_time_series_rank_ic)
            || !self.min_time_series_icir.is_finite()
            || self.min_time_series_icir <= 0.0
            || !self.min_time_series_rank_icir.is_finite()
            || self.min_time_series_rank_icir <= 0.0
            || !self.min_positive_ic_ratio.is_finite()
            || !(0.0..=1.0).contains(&self.min_positive_ic_ratio)
            || self.min_positive_ic_ratio <= 0.0
            || !self.min_fold_mean_return.is_finite()
            || self.min_fold_mean_return <= 0.0
            || !self.min_aggregate_score.is_finite()
            || self.min_aggregate_score <= 0.0
            || !self.max_drawdown.is_finite()
            || !(0.0..=1.0).contains(&self.max_drawdown)
            || self.max_drawdown <= 0.0
            || self.multiple_testing_trials == 0
            || !self.max_abs_signal.is_finite()
            || self.max_abs_signal <= 0.0
        {
            return Err(DomainError::InvalidEvaluatorConfiguration);
        }
        Ok(())
    }

    pub fn adjusted_score(&self, raw_score: f64) -> Result<f64, DomainError> {
        self.validate()?;
        if !raw_score.is_finite() {
            return Err(DomainError::InvalidEvaluationEvidence);
        }
        let penalty = match self.multiple_testing_adjustment {
            MultipleTestingAdjustment::GaussianExpectedMaximum
                if self.multiple_testing_trials <= 1 =>
            {
                0.0
            }
            MultipleTestingAdjustment::GaussianExpectedMaximum => {
                (2.0 * (self.multiple_testing_trials as f64).ln()).sqrt()
            }
        };
        Ok(raw_score - penalty)
    }
}

fn optional_config_f64(spec: &serde_json::Value, key: &str) -> Result<Option<f64>, DomainError> {
    spec.get(key)
        .map(|value| {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or(DomainError::InvalidEvaluatorConfiguration)
        })
        .transpose()
}

fn missing_evaluation_metric() -> f64 {
    f64::NAN
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldEvaluationMetrics {
    pub fold_index: usize,
    pub row_count: usize,
    pub trade_count: usize,
    #[serde(default)]
    pub total_turnover: f64,
    pub mean_net_return: f64,
    pub cumulative_net_return: f64,
    pub max_drawdown: f64,
    #[serde(default = "missing_evaluation_metric")]
    pub net_sharpe: f64,
    pub raw_score: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_book_depth_fraction: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldPredictiveMetrics {
    pub fold_index: usize,
    pub row_count: usize,
    pub time_series_ic: Option<f64>,
    pub time_series_rank_ic: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PredictiveMetrics {
    pub row_count: usize,
    pub time_series_ic: Option<f64>,
    pub time_series_rank_ic: Option<f64>,
    pub time_series_icir: Option<f64>,
    pub time_series_rank_icir: Option<f64>,
    pub positive_ic_ratio: f64,
    pub folds: Vec<FoldPredictiveMetrics>,
}

impl PredictiveMetrics {
    pub fn from_folds(folds: Vec<FoldPredictiveMetrics>) -> Self {
        let row_count = folds
            .iter()
            .fold(0_usize, |total, fold| total.saturating_add(fold.row_count));
        let ics = folds
            .iter()
            .filter_map(|fold| fold.time_series_ic)
            .collect::<Vec<_>>();
        let rank_ics = folds
            .iter()
            .filter_map(|fold| fold.time_series_rank_ic)
            .collect::<Vec<_>>();
        let time_series_ic = complete_mean(&ics, folds.len());
        let time_series_rank_ic = complete_mean(&rank_ics, folds.len());
        let time_series_icir =
            (ics.len() == folds.len() && ics.len() > 1).then(|| information_ratio(&ics));
        let time_series_rank_icir = (rank_ics.len() == folds.len() && rank_ics.len() > 1)
            .then(|| information_ratio(&rank_ics));
        let positive_ic_ratio = if folds.is_empty() {
            0.0
        } else {
            ics.iter().filter(|ic| **ic > 0.0).count() as f64 / folds.len() as f64
        };
        Self {
            row_count,
            time_series_ic,
            time_series_rank_ic,
            time_series_icir,
            time_series_rank_icir,
            positive_ic_ratio,
            folds,
        }
    }

    fn valid_for(&self, trading_folds: &[FoldEvaluationMetrics]) -> bool {
        let expected = Self::from_folds(self.folds.clone());
        self.row_count > 0
            && self.folds.len() == trading_folds.len()
            && self
                .folds
                .iter()
                .zip(trading_folds)
                .all(|(predictive, trading)| {
                    predictive.fold_index == trading.fold_index
                        && predictive.row_count == trading.row_count
                        && predictive.fold_index > 0
                        && predictive.row_count > 0
                        && optional_finite_in_unit_interval(predictive.time_series_ic)
                        && optional_finite_in_unit_interval(predictive.time_series_rank_ic)
                })
            && optional_approximately_equal(self.time_series_ic, expected.time_series_ic)
            && optional_approximately_equal(self.time_series_rank_ic, expected.time_series_rank_ic)
            && optional_approximately_equal(self.time_series_icir, expected.time_series_icir)
            && optional_approximately_equal(
                self.time_series_rank_icir,
                expected.time_series_rank_icir,
            )
            && approximately_equal(self.positive_ic_ratio, expected.positive_ic_ratio)
            && (0.0..=1.0).contains(&self.positive_ic_ratio)
    }

    fn passes(&self, config: &FormulaEvaluatorConfig, require_icir: bool) -> bool {
        self.time_series_ic
            .is_some_and(|ic| ic >= config.min_time_series_ic)
            && self
                .time_series_rank_ic
                .is_some_and(|ic| ic >= config.min_time_series_rank_ic)
            && (!require_icir
                || self
                    .time_series_icir
                    .is_some_and(|icir| icir >= config.min_time_series_icir))
            && (!require_icir
                || self
                    .time_series_rank_icir
                    .is_some_and(|icir| icir >= config.min_time_series_rank_icir))
            && self.positive_ic_ratio >= config.min_positive_ic_ratio
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    #[serde(default)]
    pub predictive: PredictiveMetrics,
    pub row_count: usize,
    pub trade_count: usize,
    #[serde(default)]
    pub total_turnover: f64,
    pub mean_net_return: f64,
    pub cumulative_net_return: f64,
    pub max_drawdown: f64,
    #[serde(default = "missing_evaluation_metric")]
    pub net_sharpe: f64,
    pub raw_score: f64,
    pub adjusted_score: f64,
    pub folds: Vec<FoldEvaluationMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub passed: bool,
    pub score: f64,
    pub failure_reasons: Vec<String>,
    pub evaluator_version: String,
    pub evaluator_config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_protocol: Option<EvaluationProtocolV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_protocol_hash: Option<String>,
    pub metrics: EvaluationMetrics,
}

impl CandidateEvaluation {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_inner(false)
    }

    /// Compatibility-only validation for restoring proposal-engine search state.
    /// Governed storage, promotion, and deployment must continue to call [`Self::validate`].
    pub fn validate_for_historical_search_replay(&self) -> Result<(), DomainError> {
        let legacy_unbound_walk_forward = self.evaluation_protocol.is_none()
            && self.evaluation_protocol_hash.is_none()
            && matches!(
                self.evaluator_version.as_str(),
                WALK_FORWARD_EVALUATOR_VERSION
                    | ONNX_WALK_FORWARD_EVALUATOR_VERSION
                    | "purged-walk-forward-v3"
                    | "onnx-purged-walk-forward-v2"
            );
        self.validate_inner(legacy_unbound_walk_forward)
    }

    fn validate_inner(&self, allow_unbound_historical_replay: bool) -> Result<(), DomainError> {
        let protocol = match self.protocol_binding() {
            Ok((protocol, _)) => Some(protocol),
            Err(_)
                if allow_unbound_historical_replay
                    && self.evaluation_protocol.is_none()
                    && self.evaluation_protocol_hash.is_none() =>
            {
                None
            }
            Err(_) => return Err(DomainError::InvalidEvaluationEvidence),
        };
        let finite_metrics = [
            self.metrics.mean_net_return,
            self.metrics.cumulative_net_return,
            self.metrics.max_drawdown,
            self.metrics.net_sharpe,
            self.metrics.raw_score,
            self.metrics.adjusted_score,
            self.metrics.total_turnover,
        ]
        .iter()
        .all(|value| value.is_finite())
            && self.metrics.folds.iter().all(|fold| {
                [
                    fold.mean_net_return,
                    fold.cumulative_net_return,
                    fold.max_drawdown,
                    fold.net_sharpe,
                    fold.raw_score,
                    fold.total_turnover,
                ]
                .iter()
                .all(|value| value.is_finite())
                    && fold.fold_index > 0
                    && fold.row_count > 0
                    && fold.trade_count <= fold.row_count
                    && fold.total_turnover >= 0.0
                    && fold.max_drawdown >= 0.0
                    && fold
                        .max_book_depth_fraction
                        .is_none_or(|fraction| fraction.is_finite() && fraction >= 0.0)
            });
        let row_count = self
            .metrics
            .folds
            .iter()
            .try_fold(0_usize, |total, fold| total.checked_add(fold.row_count))
            .ok_or(DomainError::InvalidEvaluationEvidence)?;
        let trade_count = self
            .metrics
            .folds
            .iter()
            .try_fold(0_usize, |total, fold| total.checked_add(fold.trade_count))
            .ok_or(DomainError::InvalidEvaluationEvidence)?;
        let total_turnover = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.total_turnover)
            .sum::<f64>();
        let cumulative_net_return = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.cumulative_net_return)
            .sum();
        let weighted_mean = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.mean_net_return * fold.row_count as f64)
            .sum::<f64>()
            / row_count.max(1) as f64;
        let maximum_drawdown = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.max_drawdown)
            .fold(0.0_f64, f64::max);
        let raw_score = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.raw_score)
            .sum::<f64>()
            / self.metrics.folds.len().max(1) as f64;
        let net_sharpe = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.net_sharpe)
            .sum::<f64>()
            / self.metrics.folds.len().max(1) as f64;
        if self.evaluator_version.trim().is_empty()
            || !self.evaluator_config.is_object()
            || self.metrics.row_count == 0
            || self.metrics.folds.is_empty()
            || !self.score.is_finite()
            || !finite_metrics
            || self.metrics.max_drawdown < 0.0
            || self.metrics.total_turnover < 0.0
            || !self.metrics.predictive.valid_for(&self.metrics.folds)
            || self.metrics.predictive.row_count != self.metrics.row_count
            || self.score.to_bits() != self.metrics.adjusted_score.to_bits()
            || self.passed != self.failure_reasons.is_empty()
            || row_count != self.metrics.row_count
            || trade_count != self.metrics.trade_count
            || !approximately_equal(total_turnover, self.metrics.total_turnover)
            || self
                .metrics
                .folds
                .iter()
                .enumerate()
                .any(|(index, fold)| fold.fold_index != index + 1)
            || !approximately_equal(cumulative_net_return, self.metrics.cumulative_net_return)
            || !approximately_equal(weighted_mean, self.metrics.mean_net_return)
            || !approximately_equal(maximum_drawdown, self.metrics.max_drawdown)
            || !approximately_equal(net_sharpe, self.metrics.net_sharpe)
            || !approximately_equal(raw_score, self.metrics.raw_score)
        {
            return Err(DomainError::InvalidEvaluationEvidence);
        }
        if let Some(protocol) = protocol {
            let (expected_folds, expected_rows, expected_fold_rows) =
                match self.evaluator_version.as_str() {
                    WALK_FORWARD_EVALUATOR_VERSION | ONNX_WALK_FORWARD_EVALUATOR_VERSION => (
                        protocol.walk_forward.fold_count,
                        protocol
                            .walk_forward
                            .validation_rows
                            .checked_mul(protocol.walk_forward.fold_count)
                            .ok_or(DomainError::InvalidEvaluationEvidence)?,
                        protocol.walk_forward.validation_rows,
                    ),
                    SEALED_HOLDOUT_EVALUATOR_VERSION | ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION => (
                        1,
                        protocol.walk_forward.sealed_holdout_rows,
                        protocol.walk_forward.sealed_holdout_rows,
                    ),
                    _ => (self.metrics.folds.len(), self.metrics.row_count, 0),
                };
            if self.metrics.folds.len() != expected_folds
                || self.metrics.row_count != expected_rows
                || (expected_fold_rows > 0
                    && self
                        .metrics
                        .folds
                        .iter()
                        .any(|fold| fold.row_count != expected_fold_rows))
                || if protocol.costs.capacity_enabled() {
                    self.metrics
                        .folds
                        .iter()
                        .any(|fold| fold.max_book_depth_fraction.is_none())
                } else {
                    self.metrics
                        .folds
                        .iter()
                        .any(|fold| fold.max_book_depth_fraction.is_some())
                }
            {
                return Err(DomainError::InvalidEvaluationEvidence);
            }
        }
        if matches!(
            self.evaluator_version.as_str(),
            WALK_FORWARD_EVALUATOR_VERSION
                | SEALED_HOLDOUT_EVALUATOR_VERSION
                | ONNX_WALK_FORWARD_EVALUATOR_VERSION
                | ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
        ) {
            let config = self.formula_config()?;
            let require_icir = matches!(
                self.evaluator_version.as_str(),
                WALK_FORWARD_EVALUATOR_VERSION | ONNX_WALK_FORWARD_EVALUATOR_VERSION
            );
            let capacity_passed = protocol.is_none_or(|protocol| {
                !protocol.costs.capacity_enabled()
                    || self.metrics.folds.iter().all(|fold| {
                        fold.max_book_depth_fraction.is_some_and(|fraction| {
                            fraction <= protocol.costs.max_book_depth_fraction
                        })
                    })
            });
            let policy_passed = self.metrics.predictive.passes(&config, require_icir)
                && self.metrics.folds.iter().all(|fold| {
                    fold.row_count >= config.min_validation_rows
                        && fold.trade_count >= config.min_trades
                        && fold.mean_net_return > config.min_fold_mean_return
                        && fold.max_drawdown <= config.max_drawdown
                })
                && capacity_passed
                && self.metrics.adjusted_score >= config.min_aggregate_score;
            if self.passed != policy_passed
                || !approximately_equal(
                    config.adjusted_score(self.metrics.raw_score)?,
                    self.metrics.adjusted_score,
                )
            {
                return Err(DomainError::InvalidEvaluationEvidence);
            }
        }
        Ok(())
    }

    pub fn protocol_binding(&self) -> Result<(&EvaluationProtocolV1, &str), DomainError> {
        let protocol = self
            .evaluation_protocol
            .as_ref()
            .ok_or(DomainError::InvalidEvaluationEvidence)?;
        let protocol_hash = self
            .evaluation_protocol_hash
            .as_deref()
            .ok_or(DomainError::InvalidEvaluationEvidence)?;
        validate_sha256(protocol_hash).map_err(|_| DomainError::InvalidEvaluationEvidence)?;
        if protocol.content_hash()? != protocol_hash {
            return Err(DomainError::InvalidEvaluationEvidence);
        }
        Ok((protocol, protocol_hash))
    }

    pub fn formula_config(&self) -> Result<FormulaEvaluatorConfig, DomainError> {
        let config: FormulaEvaluatorConfig = serde_json::from_value(self.evaluator_config.clone())
            .map_err(|_| DomainError::InvalidEvaluatorConfiguration)?;
        config.validate()?;
        Ok(config)
    }
}

fn approximately_equal(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= f64::EPSILON * 256.0 * scale
}

fn complete_mean(values: &[f64], expected_len: usize) -> Option<f64> {
    (expected_len > 0 && values.len() == expected_len)
        .then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn information_ratio(values: &[f64]) -> f64 {
    let average = values.iter().sum::<f64>() / values.len() as f64;
    let deviation = (values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>()
        / values.len() as f64)
        .sqrt();
    // ponytail: epsilon floor keeps stable factors finite; replace with bootstrap intervals if calibration demands it.
    average / deviation.max(f64::EPSILON)
}

fn optional_finite_in_unit_interval(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite() && (-1.0..=1.0).contains(&value))
}

fn optional_approximately_equal(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => approximately_equal(left, right),
        (None, None) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidatorMode {
    MissionValidator,
    ArchitectArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionStatus {
    Pending,
    Running,
    Paused,
    Completed,
    BudgetExhausted,
    Failed,
}

impl MissionStatus {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Running)
                | (Self::Running, Self::Paused)
                | (Self::Paused, Self::Running)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionCompletionPolicy {
    pub min_kept_candidates: usize,
}

impl Default for MissionCompletionPolicy {
    fn default() -> Self {
        Self {
            min_kept_candidates: 1,
        }
    }
}

impl MissionCompletionPolicy {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.min_kept_candidates == 0 {
            return Err(DomainError::InvalidMissionCompletionPolicy);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchBudgetLimit {
    Candidates,
    Expansions,
    Tokens,
    Time,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionTerminalReason {
    CompletionPolicySatisfied {
        kept_candidates: usize,
    },
    SearchBudgetExhausted {
        exhausted_limits: Vec<SearchBudgetLimit>,
    },
    Failed {
        code: String,
    },
}

impl MissionTerminalReason {
    pub fn status(&self) -> MissionStatus {
        match self {
            Self::CompletionPolicySatisfied { .. } => MissionStatus::Completed,
            Self::SearchBudgetExhausted { .. } => MissionStatus::BudgetExhausted,
            Self::Failed { .. } => MissionStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBudget {
    pub max_candidates: usize,
    pub max_expansions: u64,
    pub max_tokens: u64,
    pub max_seconds: u64,
}

impl SearchBudget {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.max_candidates == 0
            || (self.max_expansions == 0 && self.max_tokens == 0 && self.max_seconds == 0)
        {
            return Err(DomainError::InvalidSearchBudget);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchMission {
    pub mission_id: String,
    pub objective: String,
    pub hypothesis_scope: String,
    pub mutable_scope: Vec<String>,
    pub dataset_manifest_id: ManifestId,
    pub baseline_artifact_id: Option<String>,
    pub validation_mode: ValidatorMode,
    pub validator_spec: serde_json::Value,
    pub search_budget: SearchBudget,
    #[serde(default)]
    pub completion_policy: MissionCompletionPolicy,
    pub prompt_snapshot_id: Option<String>,
    pub search_policy_snapshot_id: String,
    pub status: MissionStatus,
    #[serde(default)]
    pub terminal_reason: Option<MissionTerminalReason>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ResearchMission {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("mission_id", &self.mission_id)?;
        require_text("objective", &self.objective)?;
        require_text("hypothesis_scope", &self.hypothesis_scope)?;
        require_text("search_policy_snapshot_id", &self.search_policy_snapshot_id)?;
        if self.mutable_scope.is_empty()
            || self.mutable_scope.iter().any(|item| item.trim().is_empty())
        {
            return Err(DomainError::EmptyField("mutable_scope"));
        }
        self.search_budget.validate()?;
        self.completion_policy.validate()?;
        self.validate_terminal_state()
    }

    pub fn transition_to(
        &mut self,
        next: MissionStatus,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if !self.status.can_transition_to(&next) {
            return Err(DomainError::InvalidMissionTransition);
        }
        self.status = next;
        self.updated_at = at;
        Ok(())
    }

    pub fn finish(
        &mut self,
        reason: MissionTerminalReason,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if self.status != MissionStatus::Running {
            return Err(DomainError::InvalidMissionTransition);
        }
        validate_mission_terminal_reason(&self.completion_policy, &reason)?;
        self.status = reason.status();
        self.terminal_reason = Some(reason);
        self.updated_at = at;
        Ok(())
    }

    fn validate_terminal_state(&self) -> Result<(), DomainError> {
        match (&self.status, &self.terminal_reason) {
            (MissionStatus::Pending | MissionStatus::Running | MissionStatus::Paused, None) => {
                Ok(())
            }
            (status, Some(reason)) if *status == reason.status() => {
                validate_mission_terminal_reason(&self.completion_policy, reason)
            }
            _ => Err(DomainError::InvalidMissionTerminalReason),
        }
    }
}

fn validate_mission_terminal_reason(
    policy: &MissionCompletionPolicy,
    reason: &MissionTerminalReason,
) -> Result<(), DomainError> {
    match reason {
        MissionTerminalReason::CompletionPolicySatisfied { kept_candidates }
            if *kept_candidates >= policy.min_kept_candidates =>
        {
            Ok(())
        }
        MissionTerminalReason::SearchBudgetExhausted { exhausted_limits }
            if !exhausted_limits.is_empty() =>
        {
            Ok(())
        }
        MissionTerminalReason::Failed { code } if !code.trim().is_empty() => Ok(()),
        _ => Err(DomainError::InvalidMissionTerminalReason),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopTargetStage {
    Researching,
    WalkForwardKept,
    HoldoutPassed,
    PaperHealthy,
    ShadowHealthy,
    LiveSmallEligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopCompletionPolicy {
    pub target_stage: LoopTargetStage,
    pub max_research_missions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopRunStatus {
    Pending,
    Running,
    Paused,
    Completed,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStage {
    Researching,
    WalkForwardKept,
    HoldoutPassed,
    PaperHealthy,
    ShadowHealthy,
    LiveSmallEligible,
    Learning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStageStatus {
    Completed,
    Paused,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopStageRecord {
    pub record_id: String,
    pub mission_id: String,
    pub stage: LoopStage,
    pub status: LoopStageStatus,
    pub reason: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStopReason {
    TargetStageReached {
        mission_id: String,
        stage: LoopTargetStage,
    },
    AwaitingEvidence {
        mission_id: String,
        stage: LoopTargetStage,
    },
    MissionPaused {
        mission_id: String,
    },
    ResearchBudgetExhausted {
        mission_id: String,
    },
    MissionFailed {
        mission_id: String,
        code: String,
    },
    MissionLimitReached {
        attempted: usize,
    },
}

impl LoopStopReason {
    pub fn status(&self) -> LoopRunStatus {
        match self {
            Self::TargetStageReached { .. } => LoopRunStatus::Completed,
            Self::AwaitingEvidence { .. } | Self::MissionPaused { .. } => LoopRunStatus::Paused,
            Self::ResearchBudgetExhausted { .. } | Self::MissionLimitReached { .. } => {
                LoopRunStatus::BudgetExhausted
            }
            Self::MissionFailed { .. } => LoopRunStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRun {
    pub loop_run_id: String,
    pub root_mission_id: String,
    pub completion_policy: LoopCompletionPolicy,
    pub child_mission_ids: Vec<String>,
    pub stage_records: Vec<LoopStageRecord>,
    pub status: LoopRunStatus,
    pub stop_reason: Option<LoopStopReason>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl LoopRun {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("loop_run_id", &self.loop_run_id)?;
        require_text("loop root_mission_id", &self.root_mission_id)?;
        if self.completion_policy.max_research_missions == 0
            || self
                .child_mission_ids
                .iter()
                .any(|mission_id| mission_id.trim().is_empty())
            || self
                .child_mission_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.child_mission_ids.len()
        {
            return Err(DomainError::InvalidLoopRun);
        }
        let mut record_ids = std::collections::BTreeSet::new();
        for record in &self.stage_records {
            if record.record_id.trim().is_empty()
                || record.mission_id.trim().is_empty()
                || record.reason.trim().is_empty()
                || !record_ids.insert(&record.record_id)
                || (record.mission_id != self.root_mission_id
                    && !self.child_mission_ids.contains(&record.mission_id))
            {
                return Err(DomainError::InvalidLoopRun);
            }
        }
        match (&self.status, &self.stop_reason) {
            (LoopRunStatus::Pending | LoopRunStatus::Running, None) => Ok(()),
            (status, Some(reason)) if *status == reason.status() => Ok(()),
            _ => Err(DomainError::InvalidLoopRun),
        }
    }

    pub fn start(&mut self, at: DateTime<Utc>) -> Result<(), DomainError> {
        if !matches!(self.status, LoopRunStatus::Pending | LoopRunStatus::Paused) {
            return Err(DomainError::InvalidLoopRun);
        }
        self.status = LoopRunStatus::Running;
        self.stop_reason = None;
        self.updated_at = at;
        self.validate()
    }

    pub fn add_child_mission(&mut self, mission_id: String) -> Result<(), DomainError> {
        require_text("loop child mission_id", &mission_id)?;
        if mission_id == self.root_mission_id {
            return Err(DomainError::InvalidLoopRun);
        }
        if !self.child_mission_ids.contains(&mission_id) {
            self.child_mission_ids.push(mission_id);
            if let Err(error) = self.validate() {
                self.child_mission_ids.pop();
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn append_stage(&mut self, record: LoopStageRecord) -> Result<(), DomainError> {
        if self.status != LoopRunStatus::Running {
            return Err(DomainError::InvalidLoopRun);
        }
        self.stage_records.push(record);
        if let Err(error) = self.validate() {
            self.stage_records.pop();
            return Err(error);
        }
        Ok(())
    }

    pub fn stop(&mut self, reason: LoopStopReason, at: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != LoopRunStatus::Running {
            return Err(DomainError::InvalidLoopRun);
        }
        self.status = reason.status();
        self.stop_reason = Some(reason);
        self.updated_at = at;
        self.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineKind {
    GeneticProgramming,
    Mcts,
    BayesianOptimizer,
    LlmProposer,
    OfflineReinforcementLearning,
    ManualSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributionMode {
    Paper,
    Shadow,
    LiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributionOutcome {
    Activated,
    Healthy,
    Decayed,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AttributionKind {
    #[default]
    Activation,
    Fill,
    Reject,
    Cancel,
    PortfolioSnapshot,
    StreamGap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAttributionEvent {
    pub event_id: String,
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub mission_id: Option<String>,
    pub mode: AttributionMode,
    pub outcome: AttributionOutcome,
    #[serde(default)]
    pub kind: AttributionKind,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    pub metrics: BTreeMap<String, f64>,
    pub reason: Option<String>,
    pub observed_at: DateTime<Utc>,
}

impl RuntimeAttributionEvent {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("attribution event_id", &self.event_id)?;
        require_text("attribution deployment_id", &self.deployment_id)?;
        require_text("attribution asset_revision_id", &self.asset_revision_id)?;
        if self.metrics.values().any(|value| !value.is_finite()) {
            return Err(DomainError::InvalidAttributionMetric);
        }
        for value in [
            self.strategy_id.as_deref(),
            self.order_id.as_deref(),
            self.account_id.as_deref(),
            self.venue.as_deref(),
            self.symbol.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.trim().is_empty() {
                return Err(DomainError::EmptyField("attribution scope"));
            }
        }
        match self.kind {
            AttributionKind::Fill | AttributionKind::Reject | AttributionKind::Cancel
                if self.strategy_id.is_none()
                    || self.order_id.is_none()
                    || self.account_id.is_none()
                    || self.venue.is_none()
                    || self.symbol.is_none() =>
            {
                return Err(DomainError::EmptyField("attribution order scope"));
            }
            AttributionKind::PortfolioSnapshot
                if self.strategy_id.is_none()
                    || self.account_id.is_none()
                    || self.venue.is_none() =>
            {
                return Err(DomainError::EmptyField("attribution portfolio scope"));
            }
            AttributionKind::StreamGap if self.reason.as_deref().is_none_or(str::is_empty) => {
                return Err(DomainError::EmptyField("attribution stream gap reason"));
            }
            _ => {}
        }
        match (&self.kind, &self.outcome) {
            (AttributionKind::Fill | AttributionKind::Cancel, AttributionOutcome::Healthy)
            | (AttributionKind::Reject | AttributionKind::StreamGap, AttributionOutcome::Failed)
            | (
                AttributionKind::PortfolioSnapshot,
                AttributionOutcome::Healthy
                | AttributionOutcome::Decayed
                | AttributionOutcome::RolledBack
                | AttributionOutcome::Failed,
            )
            | (AttributionKind::Activation, _) => Ok(()),
            _ => Err(DomainError::InvalidAttributionOutcome),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedRuntimeAttributionEvent {
    pub event: RuntimeAttributionEvent,
    pub key_id: String,
    pub content_hash: String,
    pub signature_hex: String,
}

pub fn sign_runtime_attribution_event(
    event: RuntimeAttributionEvent,
    key_id: impl Into<String>,
    signing_key: &SigningKey,
) -> Result<SignedRuntimeAttributionEvent, DomainError> {
    event.validate()?;
    let key_id = key_id.into();
    require_text("runtime attribution key_id", &key_id)?;
    let content_hash = canonical_json_hash(&event)?;
    let signature = signing_key.sign(content_hash.as_bytes());
    Ok(SignedRuntimeAttributionEvent {
        event,
        key_id,
        content_hash,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_runtime_attribution_event(
    signed: &SignedRuntimeAttributionEvent,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
) -> Result<RuntimeAttributionEvent, DomainError> {
    signed.event.validate()?;
    require_text("runtime attribution key_id", &signed.key_id)?;
    let expected_hash = canonical_json_hash(&signed.event)?;
    if expected_hash != signed.content_hash {
        return Err(DomainError::AttributionPayloadHashMismatch);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(DomainError::UnknownAttributionSigningKey)?;
    let signature_bytes = hex::decode(&signed.signature_hex)
        .map_err(|_| DomainError::InvalidAttributionSignatureEncoding)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| DomainError::InvalidAttributionSignatureEncoding)?;
    key.verify(signed.content_hash.as_bytes(), &signature)
        .map_err(|_| DomainError::InvalidAttributionSignature)?;
    Ok(signed.event.clone())
}

pub fn runtime_stage_is_healthy(
    events: &[RuntimeAttributionEvent],
    candidate_id: &str,
    mode: AttributionMode,
) -> bool {
    #[derive(Default)]
    struct DeploymentHealth {
        activated: bool,
        unhealthy: bool,
        strategies: BTreeMap<String, StrategyHealth>,
    }

    #[derive(Default)]
    struct StrategyHealth {
        healthy_snapshot: bool,
        fill: bool,
    }

    let mut by_deployment = BTreeMap::<String, DeploymentHealth>::new();
    for event in events
        .iter()
        .filter(|event| event.asset_revision_id == candidate_id && event.mode == mode)
    {
        let health = by_deployment
            .entry(event.deployment_id.clone())
            .or_default();
        if matches!(
            event.outcome,
            AttributionOutcome::Failed
                | AttributionOutcome::Decayed
                | AttributionOutcome::RolledBack
        ) || event.kind == AttributionKind::StreamGap
        {
            health.unhealthy = true;
            continue;
        }
        if event.kind == AttributionKind::Activation
            && event.outcome == AttributionOutcome::Activated
        {
            health.activated = true;
        }
        if event.outcome == AttributionOutcome::Healthy
            && event.kind == AttributionKind::PortfolioSnapshot
            && (mode != AttributionMode::LiveSmall
                || portfolio_snapshot_has_authoritative_truth(event))
        {
            if let Some(strategy_id) = event.strategy_id.as_ref() {
                health
                    .strategies
                    .entry(strategy_id.clone())
                    .or_default()
                    .healthy_snapshot = true;
            }
        }
        if event.kind == AttributionKind::Fill {
            if let Some(strategy_id) = event.strategy_id.as_ref() {
                health
                    .strategies
                    .entry(strategy_id.clone())
                    .or_default()
                    .fill = true;
            }
        }
    }

    by_deployment.values().any(|health| {
        !health.unhealthy
            && health.activated
            && health
                .strategies
                .values()
                .any(|strategy| strategy.healthy_snapshot && strategy.fill)
    })
}

const MAX_RUNTIME_RECONCILIATION_AGE_US: f64 = 30_000_000.0;

fn portfolio_snapshot_has_authoritative_truth(event: &RuntimeAttributionEvent) -> bool {
    let metric_is_one = |name: &str| {
        event
            .metrics
            .get(name)
            .is_some_and(|value| value.is_finite() && *value >= 1.0)
    };
    let age_is_fresh = event
        .metrics
        .get("venue_reconciliation_age_us")
        .is_some_and(|value| {
            value.is_finite() && *value >= 0.0 && *value <= MAX_RUNTIME_RECONCILIATION_AGE_US
        });
    metric_is_one("authoritative_account_snapshot_coverage")
        && metric_is_one("venue_reconciliation_complete")
        && metric_is_one("venue_reconciliation_healthy")
        && age_is_fresh
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningDirective {
    pub directive_id: String,
    pub mission_id: String,
    pub failure_class: String,
    pub evidence_iteration_ids: Vec<String>,
    #[serde(default)]
    pub runtime_evidence_event_ids: Vec<String>,
    pub follow_up_mission_id: String,
    pub search_policy_revision_id: String,
    pub created_at: DateTime<Utc>,
}

impl LearningDirective {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("directive_id", &self.directive_id)?;
        require_text("directive mission_id", &self.mission_id)?;
        require_text("failure_class", &self.failure_class)?;
        require_text("follow_up_mission_id", &self.follow_up_mission_id)?;
        require_text(
            "directive search_policy_revision_id",
            &self.search_policy_revision_id,
        )?;
        if self.evidence_iteration_ids.is_empty() && self.runtime_evidence_event_ids.is_empty() {
            return Err(DomainError::EmptyField("directive evidence"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPolicyRevision {
    pub revision_id: String,
    pub parent_revision_id: Option<String>,
    pub policy: serde_json::Value,
    pub evidence_event_ids: Vec<String>,
    pub validator_score: f64,
    pub adopted: bool,
    pub rollback_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SearchPolicyRevision {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("search policy revision_id", &self.revision_id)?;
        if !self.validator_score.is_finite() {
            return Err(DomainError::InvalidPolicyScore);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TensorSpec {
    pub name: String,
    pub element_type: TensorElementType,
    pub dimensions: Vec<Option<usize>>,
}

impl TensorSpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("tensor name", &self.name)?;
        if self.dimensions.is_empty() || self.dimensions.contains(&Some(0)) {
            return Err(DomainError::InvalidStrategyBundle);
        }
        let mut known_elements = 1_usize;
        for dimension in self.dimensions.iter().flatten() {
            known_elements = known_elements
                .checked_mul(*dimension)
                .ok_or(DomainError::InvalidStrategyBundle)?;
            if known_elements > MAX_ONNX_TENSOR_ELEMENTS {
                return Err(DomainError::InvalidStrategyBundle);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TensorElementType {
    Float32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnnxModelCandidate {
    pub artifact: ArtifactRef,
    pub byte_len: u64,
    pub opset: u32,
    pub preprocessing_version: String,
    pub inputs: Vec<TensorSpec>,
    pub output: TensorSpec,
}

impl OnnxModelCandidate {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("onnx artifact uri", &self.artifact.uri)?;
        if self.artifact.content_type != "application/onnx"
            || self.byte_len == 0
            || self.byte_len > MAX_ONNX_ARTIFACT_BYTES
            || self.opset == 0
            || self.preprocessing_version != LOB_ONNX_PREPROCESSING_VERSION
            || self.inputs.is_empty()
        {
            return Err(DomainError::InvalidStrategyBundle);
        }
        let checksum = self
            .artifact
            .checksum
            .as_deref()
            .ok_or(DomainError::InvalidStrategyBundle)?;
        validate_sha256(checksum)?;
        self.inputs.iter().try_for_each(TensorSpec::validate)?;
        self.output.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CandidateArtifact {
    Formula(FactorAst),
    OnnxModel(OnnxModelCandidate),
    Program(serde_json::Value),
    ModelConfig(serde_json::Value),
    ModelArtifact(ArtifactRef),
    Ensemble(serde_json::Value),
    AllocatorPolicy(serde_json::Value),
}

impl CandidateArtifact {
    pub fn to_governed_strategy_bundle_artifact(
        &self,
    ) -> Result<StrategyBundleArtifact, DomainError> {
        match self {
            Self::Formula(ast) => {
                validate_live_formula(ast)?;
                Ok(StrategyBundleArtifact::Formula { ast: ast.clone() })
            }
            Self::OnnxModel(model) => {
                model.validate()?;
                Ok(StrategyBundleArtifact::Onnx {
                    model: model.clone(),
                })
            }
            Self::Program(_)
            | Self::ModelConfig(_)
            | Self::ModelArtifact(_)
            | Self::Ensemble(_)
            | Self::AllocatorPolicy(_) => Err(DomainError::ResearchOnlyArtifact),
        }
    }
}

/// Runtime-loadable artifact schema. Governed evaluator v2 currently produces Formula only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StrategyBundleArtifact {
    Formula { ast: FactorAst },
    Onnx { model: OnnxModelCandidate },
}

impl StrategyBundleArtifact {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Formula { ast } => validate_live_formula(ast).map(|_| ()).map_err(Into::into),
            Self::Onnx { model } => model.validate(),
        }
    }

    fn validate_for_readback(&self) -> Result<(), DomainError> {
        match self {
            Self::Formula { ast } => ast
                .validate()
                .map_err(|_| DomainError::InvalidStrategyBundle),
            Self::Onnx { model } => model.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CexMctsResearchReceiptV1 {
    pub schema_version: String,
    pub mission_id: String,
    pub dataset_manifest_id: ManifestId,
    pub search_identity_hash: String,
    pub checkpoint_hash: String,
    pub selected_candidate_id: String,
    pub selected_candidate_content_hash: String,
    pub training_evaluation_id: String,
    pub training_evaluation_hash: String,
    pub evaluation_protocol_hash: String,
    pub sealed_revision_id: String,
    pub sealed_revision_hash: String,
    pub receipt_hash: String,
}

impl CexMctsResearchReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mission_id: String,
        dataset_manifest_id: ManifestId,
        search_identity_hash: String,
        checkpoint_hash: String,
        selected_candidate_id: String,
        selected_candidate_content_hash: String,
        training_evaluation_id: String,
        training_evaluation_hash: String,
        evaluation_protocol_hash: String,
        sealed_revision_id: String,
        sealed_revision_hash: String,
    ) -> Result<Self, DomainError> {
        let mut receipt = Self {
            schema_version: CEX_MCTS_RESEARCH_RECEIPT_VERSION_V1.to_string(),
            mission_id,
            dataset_manifest_id,
            search_identity_hash,
            checkpoint_hash,
            selected_candidate_id,
            selected_candidate_content_hash,
            training_evaluation_id,
            training_evaluation_hash,
            evaluation_protocol_hash,
            sealed_revision_id,
            sealed_revision_hash,
            receipt_hash: String::new(),
        };
        receipt.validate_fields()?;
        receipt.receipt_hash = receipt.calculated_hash()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        validate_cex_receipt_sha256(&self.receipt_hash)?;
        if self.receipt_hash != self.calculated_hash()? {
            return Err(DomainError::CexMctsResearchReceiptHashMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        if self.schema_version != CEX_MCTS_RESEARCH_RECEIPT_VERSION_V1
            || self.mission_id.trim().is_empty()
            || self.selected_candidate_id.trim().is_empty()
            || self.training_evaluation_id.trim().is_empty()
            || self.sealed_revision_id.trim().is_empty()
        {
            return Err(DomainError::InvalidCexMctsResearchReceipt);
        }
        self.dataset_manifest_id
            .validate()
            .map_err(|_| DomainError::InvalidCexMctsResearchReceipt)?;
        for hash in [
            &self.search_identity_hash,
            &self.checkpoint_hash,
            &self.selected_candidate_content_hash,
            &self.training_evaluation_hash,
            &self.evaluation_protocol_hash,
            &self.sealed_revision_hash,
        ] {
            validate_cex_receipt_sha256(hash)?;
        }
        Ok(())
    }

    pub fn calculated_hash(&self) -> Result<String, DomainError> {
        let mut signable = self.clone();
        signable.receipt_hash.clear();
        canonical_json_hash(&signable)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyBundle {
    pub bundle_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub dataset_manifest_id: ManifestId,
    pub evaluator_version: String,
    #[serde(default)]
    pub evaluation_protocol_hash: String,
    pub evaluator_config_hash: String,
    pub evaluation_metrics_hash: String,
    pub sealed_evaluation_hash: String,
    pub artifact: StrategyBundleArtifact,
    pub bundle_hash: String,
    pub created_at: DateTime<Utc>,
}

impl StrategyBundle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bundle_id: String,
        candidate_id: String,
        candidate_content_hash: String,
        dataset_manifest_id: ManifestId,
        evaluator_version: String,
        evaluation_protocol_hash: String,
        evaluator_config_hash: String,
        evaluation_metrics_hash: String,
        sealed_evaluation_hash: String,
        artifact: StrategyBundleArtifact,
        created_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let mut bundle = Self {
            bundle_id,
            candidate_id,
            candidate_content_hash,
            dataset_manifest_id,
            evaluator_version,
            evaluation_protocol_hash,
            evaluator_config_hash,
            evaluation_metrics_hash,
            sealed_evaluation_hash,
            artifact,
            bundle_hash: String::new(),
            created_at,
        };
        bundle.validate_fields()?;
        bundle.bundle_hash = bundle.calculated_hash()?;
        Ok(bundle)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        validate_sha256(&self.bundle_hash)?;
        if self.bundle_hash != self.calculated_hash()? {
            return Err(DomainError::StrategyBundleHashMismatch);
        }
        Ok(())
    }

    /// Validates both canonical bundles and pre-protocol bundles for forensic readback.
    /// Legacy bundles remain ineligible for promotion or deployment because [`Self::validate`]
    /// still requires a protocol binding.
    pub fn validate_for_readback(&self) -> Result<(), DomainError> {
        if !self.evaluation_protocol_hash.is_empty() {
            return self.validate();
        }
        self.validate_common_fields()?;
        self.artifact.validate_for_readback()?;
        validate_sha256(&self.bundle_hash)?;
        if self.bundle_hash != self.calculated_legacy_hash()? {
            return Err(DomainError::StrategyBundleHashMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.validate_common_fields()?;
        self.artifact.validate()?;
        validate_sha256(&self.evaluation_protocol_hash)
    }

    fn validate_common_fields(&self) -> Result<(), DomainError> {
        require_text("bundle_id", &self.bundle_id)?;
        require_text("bundle candidate_id", &self.candidate_id)?;
        require_text("evaluator_version", &self.evaluator_version)?;
        validate_sha256(&self.candidate_content_hash)?;
        validate_sha256(&self.evaluator_config_hash)?;
        validate_sha256(&self.evaluation_metrics_hash)?;
        validate_sha256(&self.sealed_evaluation_hash)?;
        self.dataset_manifest_id
            .validate()
            .map_err(|_| DomainError::InvalidStrategyBundle)?;
        Ok(())
    }

    fn calculated_legacy_hash(&self) -> Result<String, DomainError> {
        #[derive(Serialize)]
        struct LegacySignableBundle<'a> {
            candidate_content_hash: &'a str,
            dataset_manifest_id: &'a ManifestId,
            evaluator_version: &'a str,
            evaluator_config_hash: &'a str,
            evaluation_metrics_hash: &'a str,
            sealed_evaluation_hash: &'a str,
            artifact: &'a StrategyBundleArtifact,
        }
        canonical_json_hash(&LegacySignableBundle {
            candidate_content_hash: &self.candidate_content_hash,
            dataset_manifest_id: &self.dataset_manifest_id,
            evaluator_version: &self.evaluator_version,
            evaluator_config_hash: &self.evaluator_config_hash,
            evaluation_metrics_hash: &self.evaluation_metrics_hash,
            sealed_evaluation_hash: &self.sealed_evaluation_hash,
            artifact: &self.artifact,
        })
    }

    pub fn calculated_hash(&self) -> Result<String, DomainError> {
        #[derive(Serialize)]
        struct SignableBundle<'a> {
            candidate_content_hash: &'a str,
            dataset_manifest_id: &'a ManifestId,
            evaluator_version: &'a str,
            evaluation_protocol_hash: &'a str,
            evaluator_config_hash: &'a str,
            evaluation_metrics_hash: &'a str,
            sealed_evaluation_hash: &'a str,
            artifact: &'a StrategyBundleArtifact,
        }
        canonical_json_hash(&SignableBundle {
            candidate_content_hash: &self.candidate_content_hash,
            dataset_manifest_id: &self.dataset_manifest_id,
            evaluator_version: &self.evaluator_version,
            evaluation_protocol_hash: &self.evaluation_protocol_hash,
            evaluator_config_hash: &self.evaluator_config_hash,
            evaluation_metrics_hash: &self.evaluation_metrics_hash,
            sealed_evaluation_hash: &self.sealed_evaluation_hash,
            artifact: &self.artifact,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionRecord {
    pub promotion_id: String,
    pub mission_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub dataset_manifest_id: ManifestId,
    pub evaluator_version: String,
    #[serde(default)]
    pub evaluation_protocol_hash: String,
    pub evaluator_config_hash: String,
    pub evaluation_metrics_hash: String,
    pub sealed_evaluation_id: String,
    pub sealed_evaluation_hash: String,
    pub bundle_id: String,
    pub bundle_hash: String,
    pub created_at: DateTime<Utc>,
}

impl PromotionRecord {
    pub fn validate(&self, bundle: &StrategyBundle) -> Result<(), DomainError> {
        for (name, value) in [
            ("promotion_id", self.promotion_id.as_str()),
            ("promotion mission_id", self.mission_id.as_str()),
            ("promotion candidate_id", self.candidate_id.as_str()),
            ("sealed_evaluation_id", self.sealed_evaluation_id.as_str()),
            ("promotion bundle_id", self.bundle_id.as_str()),
            (
                "promotion evaluator_version",
                self.evaluator_version.as_str(),
            ),
        ] {
            require_text(name, value)?;
        }
        validate_sha256(&self.candidate_content_hash)?;
        validate_sha256(&self.evaluation_protocol_hash)?;
        validate_sha256(&self.evaluator_config_hash)?;
        validate_sha256(&self.evaluation_metrics_hash)?;
        validate_sha256(&self.sealed_evaluation_hash)?;
        validate_sha256(&self.bundle_hash)?;
        if self.candidate_id != bundle.candidate_id
            || self.candidate_content_hash != bundle.candidate_content_hash
            || self.dataset_manifest_id != bundle.dataset_manifest_id
            || self.evaluator_version != bundle.evaluator_version
            || self.evaluation_protocol_hash != bundle.evaluation_protocol_hash
            || self.evaluator_config_hash != bundle.evaluator_config_hash
            || self.evaluation_metrics_hash != bundle.evaluation_metrics_hash
            || self.sealed_evaluation_hash != bundle.sealed_evaluation_hash
            || self.bundle_id != bundle.bundle_id
            || self.bundle_hash != bundle.bundle_hash
        {
            return Err(DomainError::PromotionBindingMismatch);
        }
        Ok(())
    }

    /// Allows authenticated legacy records to be inspected without making them canonical.
    pub fn validate_for_readback(&self, bundle: &StrategyBundle) -> Result<(), DomainError> {
        if !self.evaluation_protocol_hash.is_empty() || !bundle.evaluation_protocol_hash.is_empty()
        {
            return self.validate(bundle);
        }
        bundle.validate_for_readback()?;
        for (name, value) in [
            ("promotion_id", self.promotion_id.as_str()),
            ("promotion mission_id", self.mission_id.as_str()),
            ("promotion candidate_id", self.candidate_id.as_str()),
            ("sealed_evaluation_id", self.sealed_evaluation_id.as_str()),
            ("promotion bundle_id", self.bundle_id.as_str()),
            (
                "promotion evaluator_version",
                self.evaluator_version.as_str(),
            ),
        ] {
            require_text(name, value)?;
        }
        for hash in [
            self.candidate_content_hash.as_str(),
            self.evaluator_config_hash.as_str(),
            self.evaluation_metrics_hash.as_str(),
            self.sealed_evaluation_hash.as_str(),
            self.bundle_hash.as_str(),
        ] {
            validate_sha256(hash)?;
        }
        if self.candidate_id != bundle.candidate_id
            || self.candidate_content_hash != bundle.candidate_content_hash
            || self.dataset_manifest_id != bundle.dataset_manifest_id
            || self.evaluator_version != bundle.evaluator_version
            || self.evaluator_config_hash != bundle.evaluator_config_hash
            || self.evaluation_metrics_hash != bundle.evaluation_metrics_hash
            || self.sealed_evaluation_hash != bundle.sealed_evaluation_hash
            || self.bundle_id != bundle.bundle_id
            || self.bundle_hash != bundle.bundle_hash
        {
            return Err(DomainError::PromotionBindingMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IterationVerdict {
    Keep,
    Discard,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchIteration {
    pub iteration_id: String,
    pub mission_id: String,
    pub parent_candidate_ids: Vec<String>,
    pub engine: EngineKind,
    pub hypothesis: String,
    pub candidate_artifact_id: Option<String>,
    pub evaluation_artifact_id: Option<String>,
    pub budget_usage: SearchBudgetUsage,
    pub verdict: IterationVerdict,
    pub failure_class: Option<String>,
    pub failure_explanation: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ResearchIteration {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("iteration_id", &self.iteration_id)?;
        require_text("mission_id", &self.mission_id)?;
        require_text("hypothesis", &self.hypothesis)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchBudgetUsage {
    pub candidates: usize,
    pub expansions: u64,
    pub tokens: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowedIntentType {
    LoadFactor,
    LoadModel,
    LoadAllocatorPolicy,
    StartPaper,
    StartShadow,
    StartLiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalClass {
    Paper,
    Shadow,
    HumanApprovedLiveSmall,
    SameClassAutoLiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeApprovalEvidence {
    pub approval_id: String,
    pub approval_class: ApprovalClass,
    pub subject_id: String,
    pub scope_hash: String,
    pub signer_id: String,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl RuntimeApprovalEvidence {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("runtime approval_id", &self.approval_id)?;
        require_text("runtime approval subject_id", &self.subject_id)?;
        require_text("runtime approval signer_id", &self.signer_id)?;
        validate_deployment_hash(&self.scope_hash)?;
        if self.approval_class == ApprovalClass::SameClassAutoLiveSmall
            || self.valid_from >= self.expires_at
            || self
                .revoked_at
                .is_some_and(|revoked_at| revoked_at < self.valid_from)
        {
            return Err(DomainError::ApprovalEvidenceMismatch);
        }
        Ok(())
    }

    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.valid_from
            && now < self.expires_at
            && self.revoked_at.is_none_or(|revoked_at| now < revoked_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSmallEligibilityEvidence {
    pub candidate_id: String,
    pub bundle_id: String,
    pub reconciliation_evidence_sha256: String,
    pub reduce_only_exit_evidence_sha256: String,
    pub shadow_soak_evidence_sha256: String,
}

impl LiveSmallEligibilityEvidence {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("eligibility candidate_id", &self.candidate_id)?;
        require_text("eligibility bundle_id", &self.bundle_id)?;
        for hash in [
            &self.reconciliation_evidence_sha256,
            &self.reduce_only_exit_evidence_sha256,
            &self.shadow_soak_evidence_sha256,
        ] {
            validate_sha256(hash)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentEnvelope {
    pub deployment_id: String,
    pub asset_revision_id: String,
    #[serde(default)]
    pub promotion_id: String,
    pub promotion_manifest_hash: String,
    #[serde(default)]
    pub bundle_id: String,
    #[serde(default)]
    pub bundle_hash: String,
    pub runtime_config_hash: String,
    pub risk_policy_hash: String,
    pub account_id: String,
    pub venue: String,
    pub instruments: Vec<String>,
    pub allowed_intent_types: Vec<AllowedIntentType>,
    pub max_notional: f64,
    pub max_symbol_exposure: f64,
    pub max_order_size: f64,
    pub max_slippage_bps: f64,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub approval_class: ApprovalClass,
    pub approval_signatures: Vec<String>,
    pub payload_hash: String,
}

impl DeploymentEnvelope {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (name, value) in [
            ("deployment_id", self.deployment_id.as_str()),
            ("asset_revision_id", self.asset_revision_id.as_str()),
            ("promotion_id", self.promotion_id.as_str()),
            (
                "promotion_manifest_hash",
                self.promotion_manifest_hash.as_str(),
            ),
            ("bundle_id", self.bundle_id.as_str()),
            ("bundle_hash", self.bundle_hash.as_str()),
            ("runtime_config_hash", self.runtime_config_hash.as_str()),
            ("risk_policy_hash", self.risk_policy_hash.as_str()),
            ("account_id", self.account_id.as_str()),
            ("venue", self.venue.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            require_text(name, value)?;
        }
        for value in [
            self.promotion_manifest_hash.as_str(),
            self.bundle_hash.as_str(),
            self.runtime_config_hash.as_str(),
            self.risk_policy_hash.as_str(),
        ] {
            validate_deployment_hash(value)?;
        }
        if self.instruments.is_empty() || self.instruments.iter().any(|item| item.trim().is_empty())
        {
            return Err(DomainError::EmptyField("instruments"));
        }
        if self.allowed_intent_types.is_empty() {
            return Err(DomainError::EmptyField("allowed_intent_types"));
        }
        if self.approval_signatures.is_empty()
            || self
                .approval_signatures
                .iter()
                .any(|item| item.trim().is_empty())
        {
            return Err(DomainError::EmptyField("approval_signatures"));
        }
        if [
            self.max_notional,
            self.max_symbol_exposure,
            self.max_order_size,
            self.max_slippage_bps,
        ]
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(DomainError::InvalidDeploymentLimit);
        }
        if self.valid_from >= self.expires_at {
            return Err(DomainError::InvalidValidityWindow);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedDeploymentEnvelope {
    pub envelope: DeploymentEnvelope,
    pub key_id: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedDeploymentEnvelope(pub DeploymentEnvelope);

#[derive(Debug, Clone)]
pub struct RuntimeEnvelopePolicy {
    pub account_id: String,
    pub venue: String,
    pub allowed_instruments: Vec<String>,
    pub allowed_intent_types: Vec<AllowedIntentType>,
    pub runtime_config_hash: String,
    pub risk_policy_hash: String,
    pub max_notional: f64,
    pub max_symbol_exposure: f64,
    pub max_order_size: f64,
    pub max_slippage_bps: f64,
    pub approvals: Vec<RuntimeApprovalEvidence>,
}

pub fn sign_envelope(
    mut envelope: DeploymentEnvelope,
    key_id: impl Into<String>,
    signing_key: &SigningKey,
) -> Result<SignedDeploymentEnvelope, DomainError> {
    envelope.validate()?;
    let key_id = key_id.into();
    require_text("key_id", &key_id)?;
    let bytes = canonical_payload(&envelope)?;
    envelope.payload_hash = hex::encode(Sha256::digest(&bytes));
    let signature = signing_key.sign(envelope.payload_hash.as_bytes());
    Ok(SignedDeploymentEnvelope {
        envelope,
        key_id,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_envelope(
    signed: &SignedDeploymentEnvelope,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    policy: &RuntimeEnvelopePolicy,
    now: DateTime<Utc>,
    nonce_consumed: impl Fn(&str) -> bool,
) -> Result<VerifiedDeploymentEnvelope, DomainError> {
    signed.envelope.validate()?;
    validate_runtime_policy(policy)?;
    require_text("key_id", &signed.key_id)?;
    let expected_hash = hex::encode(Sha256::digest(canonical_payload(&signed.envelope)?));
    if expected_hash != signed.envelope.payload_hash {
        return Err(DomainError::PayloadHashMismatch);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(DomainError::UnknownSigningKey)?;
    let signature_bytes =
        hex::decode(&signed.signature_hex).map_err(|_| DomainError::InvalidSignatureEncoding)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| DomainError::InvalidSignatureEncoding)?;
    key.verify(signed.envelope.payload_hash.as_bytes(), &signature)
        .map_err(|_| DomainError::InvalidSignature)?;
    if now < signed.envelope.valid_from {
        return Err(DomainError::NotYetValid);
    }
    if now >= signed.envelope.expires_at {
        return Err(DomainError::Expired);
    }
    if nonce_consumed(&signed.envelope.nonce) {
        return Err(DomainError::NonceReplay);
    }
    if signed.envelope.account_id != policy.account_id
        || signed.envelope.venue != policy.venue
        || signed.envelope.runtime_config_hash != policy.runtime_config_hash
        || signed.envelope.risk_policy_hash != policy.risk_policy_hash
        || signed
            .envelope
            .instruments
            .iter()
            .any(|instrument| !policy.allowed_instruments.contains(instrument))
        || signed
            .envelope
            .allowed_intent_types
            .iter()
            .any(|intent| !policy.allowed_intent_types.contains(intent))
    {
        return Err(DomainError::RuntimeBindingMismatch);
    }
    if signed.envelope.max_notional > policy.max_notional
        || signed.envelope.max_symbol_exposure > policy.max_symbol_exposure
        || signed.envelope.max_order_size > policy.max_order_size
        || signed.envelope.max_slippage_bps > policy.max_slippage_bps
    {
        return Err(DomainError::RuntimeLimitExceeded);
    }
    verify_runtime_approvals(&signed.envelope, policy, now)?;
    Ok(VerifiedDeploymentEnvelope(signed.envelope.clone()))
}

fn verify_runtime_approvals(
    envelope: &DeploymentEnvelope,
    policy: &RuntimeEnvelopePolicy,
    now: DateTime<Utc>,
) -> Result<(), DomainError> {
    if envelope.approval_class == ApprovalClass::SameClassAutoLiveSmall {
        return Err(DomainError::ApprovalEvidenceMismatch);
    }
    let scope_hash = deployment_scope_hash(envelope)?;
    for approval_id in &envelope.approval_signatures {
        let approval = policy
            .approvals
            .iter()
            .find(|approval| approval.approval_id == *approval_id)
            .ok_or(DomainError::ApprovalEvidenceMismatch)?;
        if !approval.is_active_at(now)
            || approval.approval_class != envelope.approval_class
            || approval.subject_id != envelope.promotion_id
            || approval.scope_hash != scope_hash
        {
            return Err(DomainError::ApprovalEvidenceMismatch);
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct SignableEnvelope<'a> {
    deployment_id: &'a str,
    asset_revision_id: &'a str,
    promotion_id: &'a str,
    promotion_manifest_hash: &'a str,
    bundle_id: &'a str,
    bundle_hash: &'a str,
    runtime_config_hash: &'a str,
    risk_policy_hash: &'a str,
    account_id: &'a str,
    venue: &'a str,
    instruments: &'a [String],
    allowed_intent_types: &'a [AllowedIntentType],
    max_notional: f64,
    max_symbol_exposure: f64,
    max_order_size: f64,
    max_slippage_bps: f64,
    valid_from: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    nonce: &'a str,
    approval_class: &'a ApprovalClass,
    approval_signatures: &'a [String],
}

fn canonical_payload(envelope: &DeploymentEnvelope) -> Result<Vec<u8>, DomainError> {
    serde_json::to_vec(&SignableEnvelope {
        deployment_id: &envelope.deployment_id,
        asset_revision_id: &envelope.asset_revision_id,
        promotion_id: &envelope.promotion_id,
        promotion_manifest_hash: &envelope.promotion_manifest_hash,
        bundle_id: &envelope.bundle_id,
        bundle_hash: &envelope.bundle_hash,
        runtime_config_hash: &envelope.runtime_config_hash,
        risk_policy_hash: &envelope.risk_policy_hash,
        account_id: &envelope.account_id,
        venue: &envelope.venue,
        instruments: &envelope.instruments,
        allowed_intent_types: &envelope.allowed_intent_types,
        max_notional: envelope.max_notional,
        max_symbol_exposure: envelope.max_symbol_exposure,
        max_order_size: envelope.max_order_size,
        max_slippage_bps: envelope.max_slippage_bps,
        valid_from: envelope.valid_from,
        expires_at: envelope.expires_at,
        nonce: &envelope.nonce,
        approval_class: &envelope.approval_class,
        approval_signatures: &envelope.approval_signatures,
    })
    .map_err(|_| DomainError::CanonicalSerialization)
}

pub fn canonical_json_hash(value: &impl Serialize) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(value).map_err(|_| DomainError::CanonicalSerialization)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn deployment_scope_hash(envelope: &DeploymentEnvelope) -> Result<String, DomainError> {
    let mut instruments = envelope.instruments.clone();
    instruments.sort();
    instruments.dedup();
    let mut allowed_intent_types = envelope.allowed_intent_types.clone();
    allowed_intent_types.sort_by_key(intent_sort_key);
    allowed_intent_types.dedup();
    let scope = serde_json::json!({
        "account_id": envelope.account_id,
        "venue": envelope.venue,
        "instruments": instruments,
        "allowed_intent_types": allowed_intent_types,
        "max_notional": envelope.max_notional,
        "max_symbol_exposure": envelope.max_symbol_exposure,
        "max_order_size": envelope.max_order_size,
        "max_slippage_bps": envelope.max_slippage_bps,
    });
    canonical_json_hash(&scope)
}

fn intent_sort_key(intent: &AllowedIntentType) -> u8 {
    match intent {
        AllowedIntentType::LoadFactor => 0,
        AllowedIntentType::LoadModel => 1,
        AllowedIntentType::LoadAllocatorPolicy => 2,
        AllowedIntentType::StartPaper => 3,
        AllowedIntentType::StartShadow => 4,
        AllowedIntentType::StartLiveSmall => 5,
    }
}

fn validate_sha256(value: &str) -> Result<(), DomainError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DomainError::InvalidStrategyBundle);
    }
    Ok(())
}

fn validate_cex_receipt_sha256(value: &str) -> Result<(), DomainError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DomainError::InvalidCexMctsResearchReceipt);
    }
    Ok(())
}

fn validate_deployment_hash(value: &str) -> Result<(), DomainError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DomainError::InvalidDeploymentHash);
    }
    Ok(())
}

fn require_text(name: &'static str, value: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(DomainError::EmptyField(name));
    }
    Ok(())
}

fn validate_runtime_policy(policy: &RuntimeEnvelopePolicy) -> Result<(), DomainError> {
    require_text("runtime account_id", &policy.account_id)?;
    require_text("runtime venue", &policy.venue)?;
    require_text("runtime_config_hash", &policy.runtime_config_hash)?;
    require_text("risk_policy_hash", &policy.risk_policy_hash)?;
    validate_deployment_hash(&policy.runtime_config_hash)?;
    validate_deployment_hash(&policy.risk_policy_hash)?;
    if policy.allowed_instruments.is_empty()
        || policy
            .allowed_instruments
            .iter()
            .any(|item| item.trim().is_empty())
        || policy.allowed_intent_types.is_empty()
    {
        return Err(DomainError::RuntimeBindingMismatch);
    }
    let mut approval_ids = BTreeSet::new();
    for approval in &policy.approvals {
        approval.validate()?;
        if !approval_ids.insert(&approval.approval_id) {
            return Err(DomainError::ApprovalEvidenceMismatch);
        }
    }
    if [
        policy.max_notional,
        policy.max_symbol_exposure,
        policy.max_order_size,
        policy.max_slippage_bps,
    ]
    .iter()
    .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(DomainError::InvalidDeploymentLimit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn envelope(now: DateTime<Utc>) -> DeploymentEnvelope {
        DeploymentEnvelope {
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "factor-1@3".to_string(),
            promotion_id: "promotion-1".to_string(),
            promotion_manifest_hash: "a".repeat(64),
            bundle_id: "bundle-1".to_string(),
            bundle_hash: "b".repeat(64),
            runtime_config_hash: "c".repeat(64),
            risk_policy_hash: "d".repeat(64),
            account_id: "account-1".to_string(),
            venue: "binance".to_string(),
            instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![AllowedIntentType::StartPaper],
            max_notional: 1_000.0,
            max_symbol_exposure: 500.0,
            max_order_size: 100.0,
            max_slippage_bps: 5.0,
            valid_from: now - Duration::minutes(1),
            expires_at: now + Duration::minutes(10),
            nonce: "nonce-1".to_string(),
            approval_class: ApprovalClass::Paper,
            approval_signatures: vec!["approval-1".to_string()],
            payload_hash: String::new(),
        }
    }

    fn policy() -> RuntimeEnvelopePolicy {
        let now = Utc::now();
        let approval_envelope = envelope(now);
        RuntimeEnvelopePolicy {
            account_id: "account-1".to_string(),
            venue: "binance".to_string(),
            allowed_instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![AllowedIntentType::StartPaper],
            runtime_config_hash: "c".repeat(64),
            risk_policy_hash: "d".repeat(64),
            max_notional: 1_000.0,
            max_symbol_exposure: 500.0,
            max_order_size: 100.0,
            max_slippage_bps: 5.0,
            approvals: vec![RuntimeApprovalEvidence {
                approval_id: "approval-1".to_string(),
                approval_class: ApprovalClass::Paper,
                subject_id: "promotion-1".to_string(),
                scope_hash: deployment_scope_hash(&approval_envelope).unwrap(),
                signer_id: "risk-officer-1".to_string(),
                valid_from: now - Duration::hours(1),
                expires_at: now + Duration::hours(1),
                revoked_at: None,
            }],
        }
    }

    fn signed(now: DateTime<Utc>) -> (SignedDeploymentEnvelope, BTreeMap<String, VerifyingKey>) {
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let signed = sign_envelope(envelope(now), "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);
        (signed, trusted)
    }

    fn mission(now: DateTime<Utc>) -> ResearchMission {
        ResearchMission {
            mission_id: "mission-1".to_string(),
            objective: "find one stable factor".to_string(),
            hypothesis_scope: "LOB imbalance".to_string(),
            mutable_scope: vec!["factor_ast".to_string()],
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({"metric": "rank_ic"}),
            search_budget: SearchBudget {
                max_candidates: 2,
                max_expansions: 10,
                max_tokens: 0,
                max_seconds: 30,
            },
            completion_policy: MissionCompletionPolicy::default(),
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn cex_mission_artifact(submitted_at: DateTime<Utc>) -> CexResearchMissionArtifactV1 {
        let search = CexResearchSearchPlanV1 {
            seed: 7,
            budget: SearchBudget {
                max_candidates: 2,
                max_expansions: 2,
                max_tokens: 0,
                max_seconds: 30,
            },
            max_new_iterations: 1,
        };
        let evaluation_protocol = evaluation_protocol();
        let reference = |id: &str, byte: char| CexResearchContentRefV1 {
            id: id.to_string(),
            content_sha256: byte.to_string().repeat(64),
        };
        CexResearchMissionArtifactV1 {
            schema_version: CEX_RESEARCH_MISSION_SCHEMA_V1.to_string(),
            spec: CexResearchMissionSpecV1 {
                objective: "Test one falsifiable LOB hypothesis".to_string(),
                search_lineage_id: "search-lineage-1".to_string(),
                data_mission_id: "data-mission-1".to_string(),
                instrument: CexResearchInstrumentV1 {
                    venue: CexResearchVenueV1::Binance,
                    market: CexResearchMarketV1::Usdm,
                    symbol: "BTCUSDT".to_string(),
                    horizon: evaluation_protocol.labels.clone(),
                },
                hypotheses: vec![CexResearchHypothesisV1 {
                    hypothesis_id: "hypothesis-lob-imbalance-1".to_string(),
                    statement: "LOB imbalance predicts the next five buckets".to_string(),
                    target: CexResearchHypothesisTargetV1 {
                        name: "forward_mid_return".to_string(),
                        horizon: evaluation_protocol.labels.clone(),
                    },
                    required_feature_families: vec!["book_imbalance".to_string()],
                    required_template_families: vec!["signed_rolling_imbalance".to_string()],
                    falsification_tests: vec![CexResearchFalsificationTestV1 {
                        test_id: "rank-ic-positive".to_string(),
                        reject_when: "purged validation rank IC is non-positive".to_string(),
                    }],
                    source_evidence_ids: vec!["evidence-training-1".to_string()],
                }],
                inputs: CexResearchInputBindingsV1 {
                    dataset: reference("dataset-cex-replay-a", 'a'),
                    snapshot: reference("snapshot-a", 'b'),
                    partition: reference("partition-a", 'c'),
                    source: reference("source-a", 'd'),
                    feature: reference("feature-a", 'e'),
                    materialization: reference("materialization-a", 'f'),
                },
                policies: CexResearchPolicyBindingsV1 {
                    gp: reference("gp-policy-1", '1'),
                    screening: reference("screening-policy-1", '2'),
                    baseline: reference("baseline-policy-1", '3'),
                    subset_search: CexResearchContentRefV1 {
                        id: "subset-search-policy-1".to_string(),
                        content_sha256: canonical_json_hash(&search).unwrap(),
                    },
                    weight: reference("weight-policy-1", '4'),
                    evaluation: CexResearchContentRefV1 {
                        id: "evaluation-policy-1".to_string(),
                        content_sha256: evaluation_protocol.content_hash().unwrap(),
                    },
                    replay: reference("replay-policy-1", '5'),
                    holdout: reference("holdout-policy-1", '6'),
                },
                evidence: vec![CexResearchEvidenceRefV1 {
                    evidence_id: "evidence-training-1".to_string(),
                    kind: CexResearchEvidenceKindV1::TrainingValidation,
                    source_mission_id: "earlier-mission-1".to_string(),
                    source_search_lineage_id: "earlier-search-lineage-1".to_string(),
                    artifact_sha256: "7".repeat(64),
                    signature: None,
                    holdout_id: None,
                }],
                feature_fields: vec!["book_imbalance".to_string(), "spread_bps".to_string()],
                search,
                evaluation_protocol,
                holdout: CexResearchHoldoutV1 {
                    holdout_id: "holdout-fresh-1".to_string(),
                    state: CexResearchHoldoutStateV1::Unopened,
                },
            },
            operational: CexResearchOperationalMetadataV1 {
                submitted_at: Some(submitted_at),
            },
        }
    }

    #[test]
    fn cex_mission_identity_ignores_operational_timestamp() {
        let now = Utc::now();
        let first = cex_mission_artifact(now);
        let second = cex_mission_artifact(now + Duration::hours(1));

        first.validate().unwrap();
        second.validate().unwrap();
        assert_eq!(first.semantic_id().unwrap(), second.semantic_id().unwrap());
    }

    #[test]
    fn cex_mission_rejects_unknown_schema_or_action_field() {
        let mut mission = cex_mission_artifact(Utc::now());
        mission.schema_version = "cex-research-mission-v2".to_string();
        assert!(mission.validate().is_err());

        let mut value = serde_json::to_value(cex_mission_artifact(Utc::now())).unwrap();
        value["spec"]["actions"] = serde_json::json!(["open_holdout"]);
        assert!(serde_json::from_value::<CexResearchMissionArtifactV1>(value).is_err());
    }

    #[test]
    fn cex_mission_rejects_unknown_nested_search_budget_field() {
        let mut value = serde_json::to_value(cex_mission_artifact(Utc::now())).unwrap();
        value["spec"]["search"]["budget"]["open_holdout"] = serde_json::json!(true);
        let shared_budget = value["spec"]["search"]["budget"].clone();

        assert!(serde_json::from_value::<CexResearchMissionArtifactV1>(value).is_err());
        assert!(serde_json::from_value::<SearchBudget>(shared_budget).is_ok());
    }

    #[test]
    fn cex_mission_semantic_id_rejects_invalid_artifact() {
        let mut mission = cex_mission_artifact(Utc::now());
        mission.schema_version = "cex-research-mission-v2".to_string();

        assert!(mission.semantic_id().is_err());
    }

    #[test]
    fn cex_mission_requires_paper_and_shadow_signature_identity() {
        let mut mission = cex_mission_artifact(Utc::now());
        mission.spec.evidence[0].kind = CexResearchEvidenceKindV1::SignedPaper;

        assert!(mission.validate().is_err());

        mission.spec.evidence[0].signature = Some(CexResearchEvidenceSignatureV1 {
            key_id: "runtime-evidence-key-1".to_string(),
            signature_sha256: "8".repeat(64),
        });
        mission.validate().unwrap();
        mission.spec.evidence[0].kind = CexResearchEvidenceKindV1::SignedShadow;
        mission.validate().unwrap();
    }

    #[test]
    fn cex_mission_accepts_exposed_holdout_only_for_a_new_search_and_holdout() {
        let mut mission = cex_mission_artifact(Utc::now());
        mission.spec.evidence[0].kind = CexResearchEvidenceKindV1::ExposedHoldout;
        mission.spec.evidence[0].holdout_id = Some("earlier-holdout-1".to_string());
        mission.validate().unwrap();

        mission.spec.evidence[0].source_search_lineage_id = mission.spec.search_lineage_id.clone();
        assert!(mission.validate().is_err());

        mission.spec.evidence[0].source_search_lineage_id = "earlier-search-lineage-1".to_string();
        mission.spec.evidence[0].holdout_id = Some(mission.spec.holdout.holdout_id.clone());
        assert!(mission.validate().is_err());
    }

    #[test]
    fn mission_rejects_zero_candidate_budget() {
        let budget = SearchBudget {
            max_candidates: 0,
            max_expansions: 1,
            max_tokens: 0,
            max_seconds: 0,
        };
        assert_eq!(budget.validate(), Err(DomainError::InvalidSearchBudget));
    }

    #[test]
    fn mission_rejects_invalid_terminal_transition() {
        assert!(!MissionStatus::Completed.can_transition_to(&MissionStatus::Running));
    }

    #[test]
    fn mission_finishes_only_with_a_policy_bound_reason() {
        let now = Utc::now();
        let mut mission = mission(now);
        mission.transition_to(MissionStatus::Running, now).unwrap();
        assert_eq!(
            mission.finish(
                MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 0 },
                now,
            ),
            Err(DomainError::InvalidMissionTerminalReason)
        );
        mission
            .finish(
                MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 },
                now,
            )
            .unwrap();
        assert_eq!(mission.status, MissionStatus::Completed);
        assert!(mission.validate().is_ok());
    }

    #[test]
    fn loop_run_persists_stage_history_and_explicit_stop_reason() {
        let now = Utc::now();
        let mut run = LoopRun {
            loop_run_id: "loop-1".to_string(),
            root_mission_id: "mission-1".to_string(),
            completion_policy: LoopCompletionPolicy {
                target_stage: LoopTargetStage::ShadowHealthy,
                max_research_missions: 2,
            },
            child_mission_ids: vec![],
            stage_records: vec![],
            status: LoopRunStatus::Pending,
            stop_reason: None,
            created_at: now,
            updated_at: now,
        };
        run.start(now).unwrap();
        run.append_stage(LoopStageRecord {
            record_id: "loop-1:mission-1:researching".to_string(),
            mission_id: "mission-1".to_string(),
            stage: LoopStage::Researching,
            status: LoopStageStatus::Completed,
            reason: "completion policy satisfied".to_string(),
            recorded_at: now,
        })
        .unwrap();
        run.stop(
            LoopStopReason::TargetStageReached {
                mission_id: "mission-1".to_string(),
                stage: LoopTargetStage::Researching,
            },
            now,
        )
        .unwrap();

        assert_eq!(run.status, LoopRunStatus::Completed);
        assert_eq!(run.stage_records.len(), 1);
        assert!(run.validate().is_ok());
    }

    #[test]
    fn awaiting_evidence_pauses_loop_runs() {
        assert_eq!(
            LoopStopReason::AwaitingEvidence {
                mission_id: "mission-1".to_string(),
                stage: LoopTargetStage::PaperHealthy,
            }
            .status(),
            LoopRunStatus::Paused
        );
    }

    #[test]
    fn order_attribution_requires_complete_strategy_scope() {
        let now = Utc::now();
        let mut event = RuntimeAttributionEvent {
            event_id: "fill-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::Fill,
            strategy_id: None,
            order_id: Some("order-1".to_string()),
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        assert!(event.validate().is_err());
        event.strategy_id = Some("strategy-1".to_string());
        assert!(event.validate().is_ok());
        event.outcome = AttributionOutcome::Activated;
        assert_eq!(
            event.validate(),
            Err(DomainError::InvalidAttributionOutcome)
        );
    }

    #[test]
    fn portfolio_snapshot_requires_strategy_account_and_venue() {
        let now = Utc::now();
        let mut event = RuntimeAttributionEvent {
            event_id: "snapshot-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::PortfolioSnapshot,
            strategy_id: None,
            order_id: None,
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: None,
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        assert!(event.validate().is_err());
        event.strategy_id = Some("strategy-1".to_string());
        assert!(event.validate().is_ok());
    }

    #[test]
    fn runtime_attribution_requires_a_trusted_signature() {
        let key = SigningKey::from_bytes(&[9_u8; 32]);
        let event = RuntimeAttributionEvent {
            event_id: "activation-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Shadow,
            outcome: AttributionOutcome::Activated,
            kind: AttributionKind::Activation,
            strategy_id: None,
            order_id: None,
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: None,
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: Utc::now(),
        };
        let signed = sign_runtime_attribution_event(event.clone(), "feedback-1", &key).unwrap();
        let trusted = BTreeMap::from([("feedback-1".to_string(), key.verifying_key())]);

        assert_eq!(
            verify_runtime_attribution_event(&signed, &trusted).unwrap(),
            event
        );
        assert_eq!(
            verify_runtime_attribution_event(&signed, &BTreeMap::new()).unwrap_err(),
            DomainError::UnknownAttributionSigningKey
        );

        let mut tampered = signed;
        tampered.event.asset_revision_id = "candidate-forged".to_string();
        assert_eq!(
            verify_runtime_attribution_event(&tampered, &trusted).unwrap_err(),
            DomainError::AttributionPayloadHashMismatch
        );
    }

    #[test]
    fn runtime_health_requires_reconciliation_truth_only_for_live_small() {
        let now = Utc::now();
        let scoped = |id: &str,
                      kind: AttributionKind,
                      outcome: AttributionOutcome,
                      strategy_id: Option<&str>| RuntimeAttributionEvent {
            event_id: id.to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Shadow,
            outcome,
            kind,
            strategy_id: strategy_id.map(str::to_string),
            order_id: strategy_id.map(|_| "order-1".to_string()),
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: strategy_id.map(|_| "BTCUSDT".to_string()),
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        let mut events = vec![scoped(
            "activation",
            AttributionKind::Activation,
            AttributionOutcome::Activated,
            None,
        )];
        assert!(!runtime_stage_is_healthy(
            &events,
            "candidate-1",
            AttributionMode::Shadow
        ));
        events.push(scoped(
            "fill",
            AttributionKind::Fill,
            AttributionOutcome::Healthy,
            Some("strategy-1"),
        ));
        events.push(scoped(
            "snapshot",
            AttributionKind::PortfolioSnapshot,
            AttributionOutcome::Healthy,
            Some("strategy-1"),
        ));
        assert!(runtime_stage_is_healthy(
            &events,
            "candidate-1",
            AttributionMode::Shadow
        ));

        let mut live_small_events = events.clone();
        for event in &mut live_small_events {
            event.mode = AttributionMode::LiveSmall;
        }
        assert!(!runtime_stage_is_healthy(
            &live_small_events,
            "candidate-1",
            AttributionMode::LiveSmall
        ));
        let snapshot = live_small_events.last_mut().unwrap();
        snapshot
            .metrics
            .insert("authoritative_account_snapshot_coverage".to_string(), 1.0);
        snapshot
            .metrics
            .insert("venue_reconciliation_complete".to_string(), 1.0);
        snapshot
            .metrics
            .insert("venue_reconciliation_healthy".to_string(), 1.0);
        snapshot
            .metrics
            .insert("venue_reconciliation_age_us".to_string(), 1_000.0);
        assert!(runtime_stage_is_healthy(
            &live_small_events,
            "candidate-1",
            AttributionMode::LiveSmall
        ));
    }

    #[test]
    fn mission_rejects_empty_id() {
        let mut mission = mission(Utc::now());
        mission.mission_id = " ".to_string();
        assert_eq!(
            mission.validate(),
            Err(DomainError::EmptyField("mission_id"))
        );
    }

    fn evaluation_protocol() -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 200,
                validation_rows: 30,
                fold_count: 3,
                purge_rows: 5,
                embargo_rows: 1,
                sealed_holdout_rows: 30,
            },
            EvaluationCostsV1 {
                fee_bps: 1.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            },
        )
        .unwrap()
    }

    #[test]
    fn evaluation_protocol_hash_binds_fee_cost() {
        let protocol = evaluation_protocol();
        let original_hash = protocol.content_hash().unwrap();
        assert_eq!(protocol.content_hash().unwrap(), original_hash);
        let mut changed = protocol;
        changed.costs.fee_bps = 2.0;

        assert_ne!(changed.content_hash().unwrap(), original_hash);

        let mut unknown_field = serde_json::to_value(changed).unwrap();
        unknown_field["unversioned_override"] = serde_json::json!(true);
        assert!(serde_json::from_value::<EvaluationProtocolV1>(unknown_field).is_err());
    }

    #[test]
    fn execution_cost_assumptions_are_explicit_and_bound() {
        let protocol = evaluation_protocol();
        let original_hash = protocol.content_hash().unwrap();
        let protocol_json = serde_json::to_value(&protocol).unwrap();
        for field in [
            "fee_bps",
            "rebate_bps",
            "funding_bps",
            "latency_bps",
            "slippage_bps",
            "cross_spread",
            "position_notional_usd",
            "capacity_depth_levels",
            "max_book_depth_fraction",
        ] {
            let mut incomplete = protocol_json.clone();
            incomplete["costs"].as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<EvaluationProtocolV1>(incomplete).is_err());
        }

        let restored: EvaluationProtocolV1 = serde_json::from_value(protocol_json).unwrap();
        assert_eq!(restored.content_hash().unwrap(), original_hash);

        let mut with_slippage = restored.clone();
        with_slippage.costs.slippage_bps = 0.5;
        assert_ne!(with_slippage.content_hash().unwrap(), original_hash);

        let mut with_rebate = restored.clone();
        with_rebate.costs.rebate_bps = 0.5;
        assert_ne!(with_rebate.content_hash().unwrap(), original_hash);

        let mut with_spread_crossing = restored;
        with_spread_crossing.costs.cross_spread = true;
        assert_ne!(with_spread_crossing.content_hash().unwrap(), original_hash);

        let mut with_capacity = evaluation_protocol();
        with_capacity.costs.position_notional_usd = 10_000.0;
        with_capacity.costs.capacity_depth_levels = 5;
        with_capacity.costs.max_book_depth_fraction = 0.1;
        assert_ne!(with_capacity.content_hash().unwrap(), original_hash);
    }

    #[test]
    fn evaluation_protocol_rejects_insufficient_purge_and_schedule_overflow() {
        let mut insufficient_purge = evaluation_protocol();
        insufficient_purge.walk_forward.purge_rows = 4;
        assert_eq!(
            insufficient_purge.validate(),
            Err(DomainError::InvalidEvaluationProtocol)
        );

        let mut overflowing = evaluation_protocol();
        overflowing.walk_forward.fold_count = usize::MAX;
        assert_eq!(
            overflowing.validate(),
            Err(DomainError::InvalidEvaluationProtocol)
        );

        let mut partial_capacity = evaluation_protocol();
        partial_capacity.costs.position_notional_usd = 10_000.0;
        assert_eq!(
            partial_capacity.validate(),
            Err(DomainError::InvalidEvaluationProtocol)
        );
    }

    #[test]
    fn evaluation_evidence_recomputes_policy_and_adjusted_score() {
        let config = FormulaEvaluatorConfig::for_trials(10).unwrap();
        let evaluation_protocol = evaluation_protocol();
        let evaluation_protocol_hash = evaluation_protocol.content_hash().unwrap();
        let raw_score = 5.0;
        let adjusted_score = config.adjusted_score(raw_score).unwrap();
        let mut evaluation = CandidateEvaluation {
            passed: true,
            score: adjusted_score,
            failure_reasons: vec![],
            evaluator_version: SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            evaluator_config: serde_json::to_value(&config).unwrap(),
            evaluation_protocol: Some(evaluation_protocol),
            evaluation_protocol_hash: Some(evaluation_protocol_hash),
            metrics: EvaluationMetrics {
                predictive: PredictiveMetrics::from_folds(vec![FoldPredictiveMetrics {
                    fold_index: 1,
                    row_count: 30,
                    time_series_ic: Some(0.1),
                    time_series_rank_ic: Some(0.1),
                }]),
                row_count: 30,
                trade_count: 30,
                total_turnover: 30.0,
                mean_net_return: 0.001,
                cumulative_net_return: 0.03,
                max_drawdown: 0.01,
                net_sharpe: 1.0,
                raw_score,
                adjusted_score,
                folds: vec![FoldEvaluationMetrics {
                    fold_index: 1,
                    row_count: 30,
                    trade_count: 30,
                    total_turnover: 30.0,
                    mean_net_return: 0.001,
                    cumulative_net_return: 0.03,
                    max_drawdown: 0.01,
                    net_sharpe: 1.0,
                    raw_score,
                    max_book_depth_fraction: None,
                }],
            },
        };
        assert!(evaluation.validate().is_ok());

        for evaluator_version in [
            SEALED_HOLDOUT_EVALUATOR_VERSION,
            ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION,
        ] {
            let mut canonical = evaluation.clone();
            canonical.evaluator_version = evaluator_version.to_string();
            assert!(canonical.validate().is_ok(), "{evaluator_version}");

            let mut wrong_holdout_size = evaluation.clone();
            wrong_holdout_size.evaluator_version = evaluator_version.to_string();
            wrong_holdout_size
                .evaluation_protocol
                .as_mut()
                .unwrap()
                .walk_forward
                .sealed_holdout_rows = 31;
            wrong_holdout_size.evaluation_protocol_hash = Some(
                wrong_holdout_size
                    .evaluation_protocol
                    .as_ref()
                    .unwrap()
                    .content_hash()
                    .unwrap(),
            );
            assert_eq!(
                wrong_holdout_size.validate(),
                Err(DomainError::InvalidEvaluationEvidence),
                "{evaluator_version} accepted a mismatched sealed row count"
            );
        }

        let mut walk_forward = evaluation.clone();
        walk_forward.metrics.folds.push(FoldEvaluationMetrics {
            fold_index: 2,
            ..walk_forward.metrics.folds[0].clone()
        });
        walk_forward.metrics.predictive = PredictiveMetrics::from_folds(vec![
            FoldPredictiveMetrics {
                fold_index: 1,
                row_count: 30,
                time_series_ic: Some(0.1),
                time_series_rank_ic: Some(0.1),
            },
            FoldPredictiveMetrics {
                fold_index: 2,
                row_count: 30,
                time_series_ic: Some(0.1),
                time_series_rank_ic: Some(0.1),
            },
        ]);
        walk_forward.metrics.row_count = 60;
        walk_forward.metrics.trade_count = 60;
        walk_forward.metrics.total_turnover = 60.0;
        walk_forward.metrics.cumulative_net_return = 0.06;
        walk_forward
            .evaluation_protocol
            .as_mut()
            .unwrap()
            .walk_forward
            .fold_count = 2;
        for evaluator_version in [
            WALK_FORWARD_EVALUATOR_VERSION,
            ONNX_WALK_FORWARD_EVALUATOR_VERSION,
        ] {
            let mut canonical = walk_forward.clone();
            canonical.evaluator_version = evaluator_version.to_string();
            canonical.evaluation_protocol_hash = Some(
                canonical
                    .evaluation_protocol
                    .as_ref()
                    .unwrap()
                    .content_hash()
                    .unwrap(),
            );
            assert!(canonical.validate().is_ok(), "{evaluator_version}");

            let mut wrong_fold_count = canonical;
            let protocol = wrong_fold_count.evaluation_protocol.as_mut().unwrap();
            protocol.walk_forward.fold_count = 3;
            wrong_fold_count.evaluation_protocol_hash = Some(protocol.content_hash().unwrap());
            assert_eq!(
                wrong_fold_count.validate(),
                Err(DomainError::InvalidEvaluationEvidence),
                "{evaluator_version} accepted a mismatched fold count"
            );
        }

        let mut legacy_protocol_value = serde_json::to_value(&evaluation).unwrap();
        legacy_protocol_value
            .as_object_mut()
            .unwrap()
            .remove("evaluation_protocol");
        legacy_protocol_value
            .as_object_mut()
            .unwrap()
            .remove("evaluation_protocol_hash");
        let legacy_protocol: CandidateEvaluation =
            serde_json::from_value(legacy_protocol_value).unwrap();
        assert_eq!(
            legacy_protocol.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        let mut tampered_protocol = evaluation.clone();
        tampered_protocol
            .evaluation_protocol
            .as_mut()
            .unwrap()
            .costs
            .fee_bps = 2.0;
        assert_eq!(
            tampered_protocol.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        let mut capacity_bound = evaluation.clone();
        let capacity_protocol = capacity_bound.evaluation_protocol.as_mut().unwrap();
        capacity_protocol.costs.position_notional_usd = 10_000.0;
        capacity_protocol.costs.capacity_depth_levels = 5;
        capacity_protocol.costs.max_book_depth_fraction = 0.1;
        capacity_bound.evaluation_protocol_hash = Some(capacity_protocol.content_hash().unwrap());
        capacity_bound.metrics.folds[0].max_book_depth_fraction = Some(0.05);
        assert!(capacity_bound.validate().is_ok());

        let mut breached_capacity = capacity_bound.clone();
        breached_capacity.metrics.folds[0].max_book_depth_fraction = Some(0.2);
        assert_eq!(
            breached_capacity.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        let mut missing_capacity_evidence = capacity_bound;
        missing_capacity_evidence.metrics.folds[0].max_book_depth_fraction = None;
        assert_eq!(
            missing_capacity_evidence.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        let mut legacy_value = serde_json::to_value(&evaluation).unwrap();
        legacy_value["evaluator_version"] = serde_json::json!("purged-walk-forward-v2");
        let legacy_metrics = legacy_value["metrics"].as_object_mut().unwrap();
        legacy_metrics.remove("predictive");
        legacy_metrics.remove("net_sharpe");
        for fold in legacy_metrics["folds"].as_array_mut().unwrap() {
            fold.as_object_mut().unwrap().remove("net_sharpe");
        }
        let legacy: CandidateEvaluation = serde_json::from_value(legacy_value).unwrap();
        assert_eq!(legacy.score, adjusted_score);
        assert!(legacy.metrics.net_sharpe.is_nan());
        assert!(legacy.metrics.folds[0].net_sharpe.is_nan());
        assert_eq!(
            legacy.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        let mut tampered_predictive = evaluation.clone();
        tampered_predictive.metrics.predictive.time_series_ic = Some(0.9);
        assert_eq!(
            tampered_predictive.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        let mut tampered_sharpe = evaluation.clone();
        tampered_sharpe.metrics.net_sharpe = 2.0;
        assert_eq!(
            tampered_sharpe.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );

        evaluation.metrics.trade_count = 0;
        evaluation.metrics.folds[0].trade_count = 0;
        assert_eq!(
            evaluation.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );
    }

    #[test]
    fn mission_can_preregister_a_larger_multiple_testing_family() {
        let mut mission = mission(Utc::now());
        mission.validator_spec = serde_json::json!({"multiple_testing_trials": 100});
        assert_eq!(
            FormulaEvaluatorConfig::for_mission(&mission)
                .unwrap()
                .multiple_testing_trials,
            100
        );

        mission.validator_spec = serde_json::json!({"multiple_testing_trials": 1});
        assert_eq!(
            FormulaEvaluatorConfig::for_mission(&mission),
            Err(DomainError::InvalidEvaluatorConfiguration)
        );
    }

    #[test]
    fn mission_can_configure_predictive_quality_gates() {
        let mut mission = mission(Utc::now());
        mission.validator_spec = serde_json::json!({
            "min_time_series_ic": 0.02,
            "min_time_series_rank_ic": 0.03,
            "min_time_series_icir": 0.7,
            "min_time_series_rank_icir": 0.8,
            "min_positive_ic_ratio": 0.75
        });

        let config = FormulaEvaluatorConfig::for_mission(&mission).unwrap();

        assert_eq!(config.min_time_series_ic, 0.02);
        assert_eq!(config.min_time_series_rank_ic, 0.03);
        assert_eq!(config.min_time_series_icir, 0.7);
        assert_eq!(config.min_time_series_rank_icir, 0.8);
        assert_eq!(config.min_positive_ic_ratio, 0.75);
    }

    #[test]
    fn evaluator_versions_identify_predictive_evidence() {
        assert_eq!(WALK_FORWARD_EVALUATOR_VERSION, "purged-walk-forward-v4");
        assert_eq!(SEALED_HOLDOUT_EVALUATOR_VERSION, "sealed-holdout-v4");
        assert_eq!(
            ONNX_WALK_FORWARD_EVALUATOR_VERSION,
            "onnx-purged-walk-forward-v3"
        );
        assert_eq!(
            ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION,
            "onnx-sealed-holdout-v3"
        );
    }

    #[test]
    fn strategy_bundle_hash_detects_tampering() {
        let mut bundle = StrategyBundle::new(
            "bundle-1".to_string(),
            "candidate-1".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "e".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "mid_price".to_string(),
                )),
            },
            Utc::now(),
        )
        .unwrap();
        assert!(bundle.validate().is_ok());

        bundle.evaluator_version = "forged".to_string();
        assert_eq!(
            bundle.validate(),
            Err(DomainError::StrategyBundleHashMismatch)
        );

        let mut protocol_tampered = StrategyBundle::new(
            "bundle-1".to_string(),
            "candidate-1".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "e".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "mid_price".to_string(),
                )),
            },
            Utc::now(),
        )
        .unwrap();
        protocol_tampered.evaluation_protocol_hash = "f".repeat(64);
        assert_eq!(
            protocol_tampered.validate(),
            Err(DomainError::StrategyBundleHashMismatch)
        );

        let mut legacy_value = serde_json::to_value(&protocol_tampered).unwrap();
        legacy_value
            .as_object_mut()
            .unwrap()
            .remove("evaluation_protocol_hash");
        let mut legacy: StrategyBundle = serde_json::from_value(legacy_value).unwrap();
        legacy.bundle_hash = legacy.calculated_legacy_hash().unwrap();
        assert!(legacy.validate_for_readback().is_ok());
        assert_eq!(legacy.validate(), Err(DomainError::InvalidStrategyBundle));
    }

    #[test]
    fn cex_mcts_research_receipt_rejects_tampered_bindings() {
        let mut receipt = CexMctsResearchReceiptV1::new(
            "mission-1".to_string(),
            ManifestId::new("dataset-1").unwrap(),
            "a".repeat(64),
            "b".repeat(64),
            "candidate-1".to_string(),
            "c".repeat(64),
            "evaluation-1".to_string(),
            "d".repeat(64),
            "e".repeat(64),
            "sealed-1".to_string(),
            "f".repeat(64),
        )
        .unwrap();
        assert!(receipt.validate().is_ok());

        receipt.checkpoint_hash = "0".repeat(64);
        assert_eq!(
            receipt.validate(),
            Err(DomainError::CexMctsResearchReceiptHashMismatch)
        );
    }

    #[test]
    fn legacy_non_live_formula_remains_readable_for_forensics() {
        let mut legacy = StrategyBundle::new(
            "bundle-legacy".to_string(),
            "candidate-legacy".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-legacy").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "e".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "mid_price".to_string(),
                )),
            },
            Utc::now(),
        )
        .unwrap();
        legacy.evaluation_protocol_hash.clear();
        legacy.artifact = StrategyBundleArtifact::Formula {
            ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field("signal".to_string())),
        };
        legacy.bundle_hash = legacy.calculated_legacy_hash().unwrap();

        assert!(legacy.validate_for_readback().is_ok());
        assert!(legacy.validate().is_err());
    }

    #[test]
    fn canonical_non_live_formula_is_not_readable_as_valid() {
        let mut canonical = StrategyBundle::new(
            "bundle-canonical".to_string(),
            "candidate-canonical".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-canonical").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "e".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "mid_price".to_string(),
                )),
            },
            Utc::now(),
        )
        .unwrap();
        canonical.artifact = StrategyBundleArtifact::Formula {
            ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field("signal".to_string())),
        };
        canonical.bundle_hash = canonical.calculated_hash().unwrap();

        assert!(canonical.validate_for_readback().is_err());
    }

    #[test]
    fn strategy_bundle_hash_is_stable_across_storage_metadata() {
        let artifact = StrategyBundleArtifact::Formula {
            ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                "mid_price".to_string(),
            )),
        };
        let first = StrategyBundle::new(
            "bundle-1".to_string(),
            "candidate-1".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "e".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            artifact.clone(),
            Utc::now(),
        )
        .unwrap();
        let second = StrategyBundle::new(
            "bundle-2".to_string(),
            "candidate-alias".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "e".repeat(64),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            artifact,
            first.created_at + chrono::Duration::seconds(1),
        )
        .unwrap();

        assert_eq!(first.bundle_hash, second.bundle_hash);
    }

    #[test]
    fn onnx_candidate_rejects_oversized_artifacts_and_tensors() {
        let candidate = |byte_len, dimensions| OnnxModelCandidate {
            artifact: ArtifactRef {
                uri: "model.onnx".to_string(),
                content_type: "application/onnx".to_string(),
                checksum: Some("a".repeat(64)),
            },
            byte_len,
            opset: 17,
            preprocessing_version: LOB_ONNX_PREPROCESSING_VERSION.to_string(),
            inputs: vec![TensorSpec {
                name: "lob".to_string(),
                element_type: TensorElementType::Float32,
                dimensions,
            }],
            output: TensorSpec {
                name: "signal".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1)],
            },
        };

        assert_eq!(
            candidate(
                MAX_ONNX_ARTIFACT_BYTES + 1,
                vec![Some(1), Some(4), Some(2), Some(2)]
            )
            .validate(),
            Err(DomainError::InvalidStrategyBundle)
        );
        assert_eq!(
            candidate(1, vec![Some(1), Some(4), Some(MAX_ONNX_TENSOR_ELEMENTS)]).validate(),
            Err(DomainError::InvalidStrategyBundle)
        );
    }

    #[test]
    fn legacy_deployment_envelope_deserializes_but_cannot_be_activated() {
        let now = Utc::now();
        let mut value = serde_json::to_value(envelope(now)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("promotion_id");
        object.remove("bundle_id");
        object.remove("bundle_hash");

        let legacy: DeploymentEnvelope = serde_json::from_value(value).unwrap();
        assert!(legacy.promotion_id.is_empty());
        assert!(legacy.bundle_id.is_empty());
        assert!(legacy.bundle_hash.is_empty());
        assert_eq!(
            legacy.validate(),
            Err(DomainError::EmptyField("promotion_id"))
        );
    }

    #[test]
    fn opaque_candidate_artifacts_are_research_only() {
        let artifact = CandidateArtifact::ModelArtifact(ArtifactRef {
            uri: "artifact://model".to_string(),
            content_type: "application/onnx".to_string(),
            checksum: Some("a".repeat(64)),
        });

        assert_eq!(
            artifact.to_governed_strategy_bundle_artifact(),
            Err(DomainError::ResearchOnlyArtifact)
        );
        let onnx = CandidateArtifact::OnnxModel(OnnxModelCandidate {
            artifact: ArtifactRef {
                uri: "model.onnx".to_string(),
                content_type: "application/onnx".to_string(),
                checksum: Some("a".repeat(64)),
            },
            byte_len: 1,
            opset: 17,
            preprocessing_version: LOB_ONNX_PREPROCESSING_VERSION.to_string(),
            inputs: vec![TensorSpec {
                name: "input".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(1)],
            }],
            output: TensorSpec {
                name: "output".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(1)],
            },
        });
        assert!(matches!(
            onnx.to_governed_strategy_bundle_artifact(),
            Ok(StrategyBundleArtifact::Onnx { .. })
        ));
    }

    #[test]
    fn formula_candidate_must_be_live_executable_before_bundle_creation() {
        let artifact = CandidateArtifact::Formula(
            FactorAst::call(
                hft_factor_dsl::FactorOperator::Mean,
                vec![
                    FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                        "mid_price".to_string(),
                    )),
                    FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Constant("20".to_string())),
                ],
            )
            .unwrap(),
        );

        assert!(matches!(
            artifact.to_governed_strategy_bundle_artifact(),
            Err(DomainError::UnsupportedLiveFormula(
                hft_factor_dsl::LiveFormulaCapabilityError::UnsupportedOperator(operator)
            )) if operator == "mean"
        ));
    }

    #[test]
    fn deployment_envelope_round_trips_valid_signature() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let verified = verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap();
        assert_eq!(verified.0.deployment_id, "deployment-1");
    }

    #[test]
    fn deployment_envelope_rejects_self_asserted_approval_ids() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut forged = envelope(now);
        forged.approval_signatures = vec!["self-asserted".to_string()];
        let signed = sign_envelope(forged, "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);

        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::ApprovalEvidenceMismatch
        );
    }

    #[test]
    fn deployment_envelope_rejects_wrong_or_inactive_runtime_approval_evidence() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);

        let mut wrong_class = policy();
        wrong_class.approvals[0].approval_class = ApprovalClass::Shadow;
        let mut wrong_subject = policy();
        wrong_subject.approvals[0].subject_id = "promotion-2".to_string();
        let mut wrong_scope = policy();
        wrong_scope.approvals[0].scope_hash = "f".repeat(64);
        let mut expired = policy();
        expired.approvals[0].expires_at = now;
        let mut revoked = policy();
        revoked.approvals[0].revoked_at = Some(now);

        for invalid_policy in [wrong_class, wrong_subject, wrong_scope, expired, revoked] {
            assert_eq!(
                verify_envelope(&signed, &trusted, &invalid_policy, now, |_| false).unwrap_err(),
                DomainError::ApprovalEvidenceMismatch
            );
        }
    }

    #[test]
    fn deployment_envelope_rejects_forged_payload() {
        let now = Utc::now();
        let (mut signed, trusted) = signed(now);
        signed.envelope.max_notional += 1.0;
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::PayloadHashMismatch
        );
    }

    #[test]
    fn deployment_envelope_rejects_invalid_signature() {
        let now = Utc::now();
        let (mut signed, trusted) = signed(now);
        let replacement = if signed.signature_hex.starts_with('0') {
            "1"
        } else {
            "0"
        };
        signed.signature_hex.replace_range(0..1, replacement);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::InvalidSignature
        );
    }

    #[test]
    fn deployment_envelope_rejects_unknown_key() {
        let now = Utc::now();
        let (signed, _) = signed(now);
        assert_eq!(
            verify_envelope(&signed, &BTreeMap::new(), &policy(), now, |_| false).unwrap_err(),
            DomainError::UnknownSigningKey
        );
    }

    #[test]
    fn deployment_envelope_rejects_expiry() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut expired = envelope(now);
        expired.valid_from = now - Duration::minutes(2);
        expired.expires_at = now - Duration::minutes(1);
        let signed = sign_envelope(expired, "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::Expired
        );
    }

    #[test]
    fn deployment_envelope_rejects_replayed_nonce() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |nonce| nonce
                == "nonce-1")
            .unwrap_err(),
            DomainError::NonceReplay
        );
    }

    #[test]
    fn deployment_envelope_rejects_runtime_binding_mismatch() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let mut wrong_policy = policy();
        wrong_policy.account_id = "another-account".to_string();
        assert_eq!(
            verify_envelope(&signed, &trusted, &wrong_policy, now, |_| false).unwrap_err(),
            DomainError::RuntimeBindingMismatch
        );
    }

    #[test]
    fn deployment_envelope_rejects_runtime_limit_increase() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let mut tighter_policy = policy();
        tighter_policy.max_notional = 999.0;
        assert_eq!(
            verify_envelope(&signed, &trusted, &tighter_policy, now, |_| false).unwrap_err(),
            DomainError::RuntimeLimitExceeded
        );
    }

    #[test]
    fn deployment_envelope_rejects_non_finite_runtime_limit() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let mut invalid_policy = policy();
        invalid_policy.max_notional = f64::NAN;
        assert_eq!(
            verify_envelope(&signed, &trusted, &invalid_policy, now, |_| false).unwrap_err(),
            DomainError::InvalidDeploymentLimit
        );
    }

    #[test]
    fn deployment_envelope_rejects_unlisted_instrument() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut unlisted = envelope(now);
        unlisted.instruments = vec!["ETHUSDT".to_string()];
        let signed = sign_envelope(unlisted, "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::RuntimeBindingMismatch
        );
    }
}
