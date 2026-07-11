//! Trust-boundary contracts for Agentic Alpha research and runtime deployment.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hft_factor_dsl::FactorAst;
use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("{0} cannot be empty")]
    EmptyField(&'static str),
    #[error("search budget must include at least one candidate and one positive limit")]
    InvalidSearchBudget,
    #[error("invalid mission status transition")]
    InvalidMissionTransition,
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
    #[error("canonical deployment serialization failed")]
    CanonicalSerialization,
    #[error("runtime attribution metrics must be finite")]
    InvalidAttributionMetric,
    #[error("search-policy validator scores must be finite")]
    InvalidPolicyScore,
    #[error("candidate artifact is research-only and cannot be promoted")]
    ResearchOnlyArtifact,
    #[error("strategy bundle is invalid")]
    InvalidStrategyBundle,
    #[error("strategy bundle hash does not match its canonical payload")]
    StrategyBundleHashMismatch,
    #[error("promotion record does not match its candidate, evidence, or bundle")]
    PromotionBindingMismatch,
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
                | (Self::Running, Self::Completed)
                | (Self::Running, Self::BudgetExhausted)
                | (Self::Running, Self::Failed)
        )
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
    pub prompt_snapshot_id: Option<String>,
    pub search_policy_snapshot_id: String,
    pub status: MissionStatus,
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
        self.search_budget.validate()
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAttributionEvent {
    pub event_id: String,
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub mission_id: Option<String>,
    pub mode: AttributionMode,
    pub outcome: AttributionOutcome,
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
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningDirective {
    pub directive_id: String,
    pub mission_id: String,
    pub failure_class: String,
    pub evidence_iteration_ids: Vec<String>,
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
        if self.evidence_iteration_ids.is_empty() {
            return Err(DomainError::EmptyField("evidence_iteration_ids"));
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
    pub inputs: Vec<TensorSpec>,
    pub output: TensorSpec,
}

impl OnnxModelCandidate {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("onnx artifact uri", &self.artifact.uri)?;
        if self.artifact.content_type != "application/onnx"
            || self.byte_len == 0
            || self.opset == 0
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
    pub fn to_strategy_bundle_artifact(&self) -> Result<StrategyBundleArtifact, DomainError> {
        match self {
            Self::Formula(ast) => {
                ast.validate()
                    .map_err(|_| DomainError::InvalidStrategyBundle)?;
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StrategyBundleArtifact {
    Formula { ast: FactorAst },
    Onnx { model: OnnxModelCandidate },
}

impl StrategyBundleArtifact {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Formula { ast } => ast
                .validate()
                .map_err(|_| DomainError::InvalidStrategyBundle),
            Self::Onnx { model } => model.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyBundle {
    pub bundle_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub dataset_manifest_id: ManifestId,
    pub evaluator_version: String,
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

    fn validate_fields(&self) -> Result<(), DomainError> {
        require_text("bundle_id", &self.bundle_id)?;
        require_text("bundle candidate_id", &self.candidate_id)?;
        require_text("evaluator_version", &self.evaluator_version)?;
        validate_sha256(&self.candidate_content_hash)?;
        validate_sha256(&self.sealed_evaluation_hash)?;
        self.dataset_manifest_id
            .validate()
            .map_err(|_| DomainError::InvalidStrategyBundle)?;
        self.artifact.validate()
    }

    pub fn calculated_hash(&self) -> Result<String, DomainError> {
        #[derive(Serialize)]
        struct SignableBundle<'a> {
            bundle_id: &'a str,
            candidate_id: &'a str,
            candidate_content_hash: &'a str,
            dataset_manifest_id: &'a ManifestId,
            evaluator_version: &'a str,
            sealed_evaluation_hash: &'a str,
            artifact: &'a StrategyBundleArtifact,
            created_at: DateTime<Utc>,
        }
        canonical_json_hash(&SignableBundle {
            bundle_id: &self.bundle_id,
            candidate_id: &self.candidate_id,
            candidate_content_hash: &self.candidate_content_hash,
            dataset_manifest_id: &self.dataset_manifest_id,
            evaluator_version: &self.evaluator_version,
            sealed_evaluation_hash: &self.sealed_evaluation_hash,
            artifact: &self.artifact,
            created_at: self.created_at,
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
        validate_sha256(&self.sealed_evaluation_hash)?;
        validate_sha256(&self.bundle_hash)?;
        if self.candidate_id != bundle.candidate_id
            || self.candidate_content_hash != bundle.candidate_content_hash
            || self.dataset_manifest_id != bundle.dataset_manifest_id
            || self.evaluator_version != bundle.evaluator_version
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentEnvelope {
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub promotion_id: String,
    pub promotion_manifest_hash: String,
    pub bundle_id: String,
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
    Ok(VerifiedDeploymentEnvelope(signed.envelope.clone()))
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
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            created_at: now,
            updated_at: now,
        }
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
    fn mission_rejects_empty_id() {
        let mut mission = mission(Utc::now());
        mission.mission_id = " ".to_string();
        assert_eq!(
            mission.validate(),
            Err(DomainError::EmptyField("mission_id"))
        );
    }

    #[test]
    fn strategy_bundle_hash_detects_tampering() {
        let mut bundle = StrategyBundle::new(
            "bundle-1".to_string(),
            "candidate-1".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            "sealed-holdout-v1".to_string(),
            "b".repeat(64),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "signal".to_string(),
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
    }

    #[test]
    fn opaque_candidate_artifacts_are_research_only() {
        let artifact = CandidateArtifact::ModelArtifact(ArtifactRef {
            uri: "artifact://model".to_string(),
            content_type: "application/onnx".to_string(),
            checksum: Some("a".repeat(64)),
        });

        assert_eq!(
            artifact.to_strategy_bundle_artifact(),
            Err(DomainError::ResearchOnlyArtifact)
        );
    }

    #[test]
    fn deployment_envelope_round_trips_valid_signature() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let verified = verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap();
        assert_eq!(verified.0.deployment_id, "deployment-1");
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
