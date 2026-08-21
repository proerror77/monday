//! Signed runtime admission contracts shared by governance producers and hft-live.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum GovernanceError {
    #[error("{0} cannot be empty")]
    EmptyField(&'static str),
    #[error("deployment hashes must be lowercase SHA-256 values")]
    InvalidDeploymentHash,
    #[error("deployment limits must be finite and non-negative")]
    InvalidDeploymentLimit,
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
    #[error("evidence hash must be a lowercase SHA-256 value")]
    InvalidEvidenceHash,
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
    pub fn validate(&self) -> Result<(), GovernanceError> {
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
            return Err(GovernanceError::ApprovalEvidenceMismatch);
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
    pub fn validate(&self) -> Result<(), GovernanceError> {
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
    pub fn validate(&self) -> Result<(), GovernanceError> {
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
            return Err(GovernanceError::EmptyField("instruments"));
        }
        if self.allowed_intent_types.is_empty() {
            return Err(GovernanceError::EmptyField("allowed_intent_types"));
        }
        if self.approval_signatures.is_empty()
            || self
                .approval_signatures
                .iter()
                .any(|item| item.trim().is_empty())
        {
            return Err(GovernanceError::EmptyField("approval_signatures"));
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
            return Err(GovernanceError::InvalidDeploymentLimit);
        }
        if self.valid_from >= self.expires_at {
            return Err(GovernanceError::InvalidValidityWindow);
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
) -> Result<SignedDeploymentEnvelope, GovernanceError> {
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
) -> Result<VerifiedDeploymentEnvelope, GovernanceError> {
    signed.envelope.validate()?;
    validate_runtime_policy(policy)?;
    require_text("key_id", &signed.key_id)?;
    let expected_hash = hex::encode(Sha256::digest(canonical_payload(&signed.envelope)?));
    if expected_hash != signed.envelope.payload_hash {
        return Err(GovernanceError::PayloadHashMismatch);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(GovernanceError::UnknownSigningKey)?;
    let signature_bytes = hex::decode(&signed.signature_hex)
        .map_err(|_| GovernanceError::InvalidSignatureEncoding)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| GovernanceError::InvalidSignatureEncoding)?;
    key.verify(signed.envelope.payload_hash.as_bytes(), &signature)
        .map_err(|_| GovernanceError::InvalidSignature)?;
    if now < signed.envelope.valid_from {
        return Err(GovernanceError::NotYetValid);
    }
    if now >= signed.envelope.expires_at {
        return Err(GovernanceError::Expired);
    }
    if nonce_consumed(&signed.envelope.nonce) {
        return Err(GovernanceError::NonceReplay);
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
        return Err(GovernanceError::RuntimeBindingMismatch);
    }
    if signed.envelope.max_notional > policy.max_notional
        || signed.envelope.max_symbol_exposure > policy.max_symbol_exposure
        || signed.envelope.max_order_size > policy.max_order_size
        || signed.envelope.max_slippage_bps > policy.max_slippage_bps
    {
        return Err(GovernanceError::RuntimeLimitExceeded);
    }
    verify_runtime_approvals(&signed.envelope, policy, now)?;
    Ok(VerifiedDeploymentEnvelope(signed.envelope.clone()))
}

fn verify_runtime_approvals(
    envelope: &DeploymentEnvelope,
    policy: &RuntimeEnvelopePolicy,
    now: DateTime<Utc>,
) -> Result<(), GovernanceError> {
    if envelope.approval_class == ApprovalClass::SameClassAutoLiveSmall {
        return Err(GovernanceError::ApprovalEvidenceMismatch);
    }
    let scope_hash = deployment_scope_hash(envelope)?;
    for approval_id in &envelope.approval_signatures {
        let approval = policy
            .approvals
            .iter()
            .find(|approval| approval.approval_id == *approval_id)
            .ok_or(GovernanceError::ApprovalEvidenceMismatch)?;
        if !approval.is_active_at(now)
            || approval.approval_class != envelope.approval_class
            || approval.subject_id != envelope.promotion_id
            || approval.scope_hash != scope_hash
        {
            return Err(GovernanceError::ApprovalEvidenceMismatch);
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

fn canonical_payload(envelope: &DeploymentEnvelope) -> Result<Vec<u8>, GovernanceError> {
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
    .map_err(|_| GovernanceError::CanonicalSerialization)
}

pub fn canonical_json_hash(value: &impl Serialize) -> Result<String, GovernanceError> {
    struct HashWriter(Sha256);

    impl std::io::Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, value)
        .map_err(|_| GovernanceError::CanonicalSerialization)?;
    Ok(hex::encode(writer.0.finalize()))
}

pub fn deployment_scope_hash(envelope: &DeploymentEnvelope) -> Result<String, GovernanceError> {
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

fn validate_deployment_hash(value: &str) -> Result<(), GovernanceError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(GovernanceError::InvalidDeploymentHash);
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), GovernanceError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(GovernanceError::InvalidEvidenceHash);
    }
    Ok(())
}

fn require_text(name: &'static str, value: &str) -> Result<(), GovernanceError> {
    if value.trim().is_empty() {
        return Err(GovernanceError::EmptyField(name));
    }
    Ok(())
}

fn validate_runtime_policy(policy: &RuntimeEnvelopePolicy) -> Result<(), GovernanceError> {
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
        return Err(GovernanceError::RuntimeBindingMismatch);
    }
    let mut approval_ids = BTreeSet::new();
    for approval in &policy.approvals {
        approval.validate()?;
        if !approval_ids.insert(&approval.approval_id) {
            return Err(GovernanceError::ApprovalEvidenceMismatch);
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
        return Err(GovernanceError::InvalidDeploymentLimit);
    }
    Ok(())
}
