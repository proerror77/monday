use alpha_domain::{
    verify_envelope, AllowedIntentType, ApprovalClass, DomainError, RuntimeEnvelopePolicy,
    SignedDeploymentEnvelope, VerifiedDeploymentEnvelope,
};
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IntakeError {
    #[error("deployment envelope rejected: {0}")]
    Domain(#[from] DomainError),
    #[error("runtime is paused and cannot be resumed by a deployment envelope")]
    RuntimePaused,
    #[error("deployment must request exactly one paper, shadow, or live-small activation")]
    InvalidActivationIntent,
    #[error("deployment approval class does not authorize the requested activation")]
    ApprovalClassMismatch,
    #[error("runtime nonce ledger already contains this nonce")]
    NonceReplay,
    #[error("runtime durable state failed: {0}")]
    DurableState(String),
    #[error("runtime activation adapter failed: {0}")]
    Activation(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationMode {
    Paper,
    Shadow,
    LiveSmall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivationRequest {
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub account_id: String,
    pub venue: String,
    pub instruments: Vec<String>,
    pub mode: ActivationMode,
    pub max_notional: f64,
    pub max_symbol_exposure: f64,
    pub max_order_size: f64,
    pub max_slippage_bps: f64,
}

pub trait RuntimeActivationAdapter {
    fn activate(&mut self, request: &ActivationRequest) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePolicyDocument {
    pub account_id: String,
    pub venue: String,
    pub allowed_instruments: Vec<String>,
    pub allowed_intent_types: Vec<AllowedIntentType>,
    pub max_notional: f64,
    pub max_symbol_exposure: f64,
    pub max_order_size: f64,
    pub max_slippage_bps: f64,
    #[serde(default)]
    pub runtime_paused: bool,
}

impl RuntimePolicyDocument {
    pub fn bind(
        &self,
        runtime_config_hash: impl Into<String>,
        risk_policy_hash: impl Into<String>,
    ) -> RuntimeEnvelopePolicy {
        RuntimeEnvelopePolicy {
            account_id: self.account_id.clone(),
            venue: self.venue.clone(),
            allowed_instruments: self.allowed_instruments.clone(),
            allowed_intent_types: self.allowed_intent_types.clone(),
            runtime_config_hash: runtime_config_hash.into(),
            risk_policy_hash: risk_policy_hash.into(),
            max_notional: self.max_notional,
            max_symbol_exposure: self.max_symbol_exposure,
            max_order_size: self.max_order_size,
            max_slippage_bps: self.max_slippage_bps,
        }
    }
}

pub fn decode_trusted_keys(
    encoded: BTreeMap<String, String>,
) -> Result<BTreeMap<String, VerifyingKey>, IntakeError> {
    encoded
        .into_iter()
        .map(|(key_id, value)| {
            let bytes = hex::decode(value).map_err(|error| {
                IntakeError::DurableState(format!("trusted key {key_id} is not hex: {error}"))
            })?;
            let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
                IntakeError::DurableState(format!(
                    "trusted key {key_id} must contain exactly 32 bytes"
                ))
            })?;
            let key = VerifyingKey::from_bytes(&bytes).map_err(|error| {
                IntakeError::DurableState(format!("trusted key {key_id} is invalid: {error}"))
            })?;
            Ok((key_id, key))
        })
        .collect()
}

pub struct SystemConfigActivationAdapter<'a> {
    config: &'a mut runtime::SystemConfig,
    applied_modes: Vec<ActivationMode>,
}

impl<'a> SystemConfigActivationAdapter<'a> {
    pub fn new(config: &'a mut runtime::SystemConfig) -> Self {
        Self {
            config,
            applied_modes: Vec::new(),
        }
    }

    pub fn applied_modes(&self) -> &[ActivationMode] {
        &self.applied_modes
    }
}

impl RuntimeActivationAdapter for SystemConfigActivationAdapter<'_> {
    fn activate(&mut self, request: &ActivationRequest) -> Result<(), String> {
        let venue = self
            .config
            .venues
            .iter_mut()
            .find(|venue| {
                venue.name.eq_ignore_ascii_case(&request.venue)
                    && venue.account_id.as_deref().unwrap_or(&venue.name) == request.account_id
            })
            .ok_or_else(|| {
                "deployment account and venue are not present in runtime config".to_string()
            })?;
        match request.mode {
            ActivationMode::Paper => {
                venue.execution_mode = Some("Paper".to_string());
                self.config.quotes_only = false;
            }
            ActivationMode::Shadow => {
                venue.execution_mode = Some("Paper".to_string());
                self.config.quotes_only = true;
            }
            ActivationMode::LiveSmall => {
                return Err(
                    "live-small activation is disabled until universal order-size and slippage gates consume the envelope limits"
                        .to_string(),
                )
            }
        }
        self.applied_modes.push(request.mode);
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NonceRecord {
    nonce: String,
    deployment_id: String,
    accepted_at: DateTime<Utc>,
}

pub struct RuntimeNonceLedger {
    path: PathBuf,
    consumed: BTreeSet<String>,
}

impl RuntimeNonceLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IntakeError> {
        let path = path.as_ref().to_path_buf();
        ensure_parent(&path)?;
        let mut consumed = BTreeSet::new();
        if path.exists() {
            let file = File::open(&path).map_err(durable_error)?;
            for (line_number, line) in BufReader::new(file).lines().enumerate() {
                let line = line.map_err(durable_error)?;
                if line.trim().is_empty() {
                    continue;
                }
                let record: NonceRecord = serde_json::from_str(&line).map_err(|error| {
                    IntakeError::DurableState(format!(
                        "nonce ledger line {} is invalid: {error}",
                        line_number + 1
                    ))
                })?;
                if !consumed.insert(record.nonce) {
                    return Err(IntakeError::DurableState(
                        "nonce ledger contains a duplicate record".to_string(),
                    ));
                }
            }
        }
        Ok(Self { path, consumed })
    }

    pub fn contains(&self, nonce: &str) -> bool {
        self.consumed.contains(nonce)
    }

    fn consume(
        &mut self,
        nonce: &str,
        deployment_id: &str,
        at: DateTime<Utc>,
    ) -> Result<(), IntakeError> {
        if self.contains(nonce) {
            return Err(IntakeError::NonceReplay);
        }
        let record = NonceRecord {
            nonce: nonce.to_string(),
            deployment_id: deployment_id.to_string(),
            accepted_at: at,
        };
        let mut bytes = serde_json::to_vec(&record)
            .map_err(|error| IntakeError::DurableState(error.to_string()))?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(durable_error)?;
        file.write_all(&bytes).map_err(durable_error)?;
        file.sync_data().map_err(durable_error)?;
        self.consumed.insert(nonce.to_string());
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeAuditEvent {
    pub deployment_id: String,
    pub phase: String,
    pub result: String,
    pub reason: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

pub struct RuntimeAuditLog {
    path: PathBuf,
}

impl RuntimeAuditLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IntakeError> {
        let path = path.as_ref().to_path_buf();
        ensure_parent(&path)?;
        Ok(Self { path })
    }

    pub fn append(&mut self, event: &RuntimeAuditEvent) -> Result<(), IntakeError> {
        let mut bytes = serde_json::to_vec(event)
            .map_err(|error| IntakeError::DurableState(error.to_string()))?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(durable_error)?;
        file.write_all(&bytes).map_err(durable_error)?;
        file.sync_data().map_err(durable_error)
    }
}

pub struct DeploymentIntake<'a, A> {
    trusted_keys: &'a BTreeMap<String, VerifyingKey>,
    policy: &'a RuntimeEnvelopePolicy,
    runtime_paused: bool,
    ledger: &'a mut RuntimeNonceLedger,
    audit: &'a mut RuntimeAuditLog,
    adapter: &'a mut A,
}

impl<'a, A: RuntimeActivationAdapter> DeploymentIntake<'a, A> {
    pub fn new(
        trusted_keys: &'a BTreeMap<String, VerifyingKey>,
        policy: &'a RuntimeEnvelopePolicy,
        runtime_paused: bool,
        ledger: &'a mut RuntimeNonceLedger,
        audit: &'a mut RuntimeAuditLog,
        adapter: &'a mut A,
    ) -> Self {
        Self {
            trusted_keys,
            policy,
            runtime_paused,
            ledger,
            audit,
            adapter,
        }
    }

    pub fn accept(
        &mut self,
        signed: &SignedDeploymentEnvelope,
        now: DateTime<Utc>,
    ) -> Result<ActivationRequest, IntakeError> {
        let verified = match verify_envelope(signed, self.trusted_keys, self.policy, now, |nonce| {
            self.ledger.contains(nonce)
        }) {
            Ok(verified) => verified,
            Err(error) => {
                append_rejection(
                    self.audit,
                    &signed.envelope.deployment_id,
                    now,
                    &error.to_string(),
                )?;
                return Err(error.into());
            }
        };
        let request = match activation_request(&verified, self.runtime_paused) {
            Ok(request) => request,
            Err(error) => {
                append_rejection(
                    self.audit,
                    &signed.envelope.deployment_id,
                    now,
                    &error.to_string(),
                )?;
                return Err(error);
            }
        };
        self.audit.append(&RuntimeAuditEvent {
            deployment_id: request.deployment_id.clone(),
            phase: "pre_activation".to_string(),
            result: "accepted".to_string(),
            reason: None,
            recorded_at: now,
        })?;
        self.ledger
            .consume(&signed.envelope.nonce, &signed.envelope.deployment_id, now)?;
        if let Err(error) = self.adapter.activate(&request) {
            self.audit.append(&RuntimeAuditEvent {
                deployment_id: request.deployment_id.clone(),
                phase: "activation".to_string(),
                result: "failed".to_string(),
                reason: Some(error.clone()),
                recorded_at: now,
            })?;
            return Err(IntakeError::Activation(error));
        }
        self.audit.append(&RuntimeAuditEvent {
            deployment_id: request.deployment_id.clone(),
            phase: "activation".to_string(),
            result: "activated".to_string(),
            reason: None,
            recorded_at: now,
        })?;
        Ok(request)
    }
}

fn activation_request(
    verified: &VerifiedDeploymentEnvelope,
    runtime_paused: bool,
) -> Result<ActivationRequest, IntakeError> {
    if runtime_paused {
        return Err(IntakeError::RuntimePaused);
    }
    let modes = verified
        .0
        .allowed_intent_types
        .iter()
        .filter_map(|intent| match intent {
            AllowedIntentType::StartPaper => Some(ActivationMode::Paper),
            AllowedIntentType::StartShadow => Some(ActivationMode::Shadow),
            AllowedIntentType::StartLiveSmall => Some(ActivationMode::LiveSmall),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [mode] = modes.as_slice() else {
        return Err(IntakeError::InvalidActivationIntent);
    };
    let approved = match mode {
        ActivationMode::Paper => matches!(
            verified.0.approval_class,
            ApprovalClass::Paper | ApprovalClass::Shadow | ApprovalClass::HumanApprovedLiveSmall
        ),
        ActivationMode::Shadow => matches!(
            verified.0.approval_class,
            ApprovalClass::Shadow | ApprovalClass::HumanApprovedLiveSmall
        ),
        ActivationMode::LiveSmall => matches!(
            verified.0.approval_class,
            ApprovalClass::HumanApprovedLiveSmall | ApprovalClass::SameClassAutoLiveSmall
        ),
    };
    if !approved {
        return Err(IntakeError::ApprovalClassMismatch);
    }
    Ok(ActivationRequest {
        deployment_id: verified.0.deployment_id.clone(),
        asset_revision_id: verified.0.asset_revision_id.clone(),
        account_id: verified.0.account_id.clone(),
        venue: verified.0.venue.clone(),
        instruments: verified.0.instruments.clone(),
        mode: *mode,
        max_notional: verified.0.max_notional,
        max_symbol_exposure: verified.0.max_symbol_exposure,
        max_order_size: verified.0.max_order_size,
        max_slippage_bps: verified.0.max_slippage_bps,
    })
}

fn append_rejection(
    audit: &mut RuntimeAuditLog,
    deployment_id: &str,
    now: DateTime<Utc>,
    reason: &str,
) -> Result<(), IntakeError> {
    audit.append(&RuntimeAuditEvent {
        deployment_id: deployment_id.to_string(),
        phase: "verification".to_string(),
        result: "rejected".to_string(),
        reason: Some(reason.to_string()),
        recorded_at: now,
    })
}

fn ensure_parent(path: &Path) -> Result<(), IntakeError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(durable_error)?;
    }
    Ok(())
}

fn durable_error(error: std::io::Error) -> IntakeError {
    IntakeError::DurableState(error.to_string())
}
