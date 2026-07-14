use alpha_domain::{
    sign_runtime_attribution_event, verify_envelope, AllowedIntentType, ApprovalClass, DomainError,
    RuntimeApprovalEvidence, RuntimeAttributionEvent, RuntimeEnvelopePolicy,
    SignedDeploymentEnvelope, StrategyBundle, StrategyBundleArtifact, VerifiedDeploymentEnvelope,
    MAX_ONNX_ARTIFACT_BYTES, MAX_ONNX_TENSOR_ELEMENTS,
};
use chrono::{DateTime, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
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
    #[error("deployment must request exactly one supported strategy artifact activation")]
    InvalidArtifactIntent,
    #[error("deployment instruments must be unique")]
    DuplicateInstrument,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivationArtifact {
    Formula,
    Onnx,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivationRequest {
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub promotion_id: String,
    pub bundle_id: String,
    pub bundle_hash: String,
    pub account_id: String,
    pub venue: String,
    pub instruments: Vec<String>,
    pub artifact: ActivationArtifact,
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
    pub approvals: Vec<RuntimeApprovalEvidence>,
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
            approvals: self.approvals.clone(),
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
    bundle: &'a StrategyBundle,
    bundle_path: &'a Path,
    applied_modes: Vec<ActivationMode>,
}

impl<'a> SystemConfigActivationAdapter<'a> {
    pub fn new(
        config: &'a mut runtime::SystemConfig,
        bundle: &'a StrategyBundle,
        bundle_path: &'a Path,
    ) -> Self {
        Self {
            config,
            bundle,
            bundle_path,
            applied_modes: Vec::new(),
        }
    }

    pub fn applied_modes(&self) -> &[ActivationMode] {
        &self.applied_modes
    }
}

impl RuntimeActivationAdapter for SystemConfigActivationAdapter<'_> {
    fn activate(&mut self, request: &ActivationRequest) -> Result<(), String> {
        let mut proposed = self.config.clone();
        let venue_matches = proposed
            .venues
            .iter()
            .enumerate()
            .filter(|(_, venue)| {
                venue.name.eq_ignore_ascii_case(&request.venue)
                    && venue.account_id.as_deref().unwrap_or(&venue.name) == request.account_id
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [venue_index] = venue_matches.as_slice() else {
            return Err(
                "deployment account and venue must identify exactly one runtime venue".to_string(),
            );
        };
        validate_instrument_catalog(&proposed.venues[*venue_index], request)?;
        match request.mode {
            ActivationMode::Paper => {
                proposed.venues[*venue_index].execution_mode = Some("Paper".to_string());
                proposed.quotes_only = false;
            }
            ActivationMode::Shadow => {
                proposed.venues[*venue_index].execution_mode = Some("Paper".to_string());
                proposed.quotes_only = false;
            }
            ActivationMode::LiveSmall => {
                if proposed.venues[*venue_index].venue_type != runtime::VenueType::Polymarket {
                    return Err(
                        "live-small activation is restricted to Polymarket venues".to_string()
                    );
                }
                if request.artifact != ActivationArtifact::Formula {
                    return Err(
                        "live-small Polymarket activation requires a Formula artifact".to_string(),
                    );
                }
                proposed.venues[*venue_index].execution_mode = Some("Live".to_string());
                proposed.quotes_only = false;
            }
        }
        apply_strategy_bundle(&mut proposed, request, self.bundle, self.bundle_path)?;
        *self.config = proposed;
        self.applied_modes.push(request.mode);
        Ok(())
    }
}

fn validate_instrument_catalog(
    venue: &runtime::VenueConfig,
    request: &ActivationRequest,
) -> Result<(), String> {
    if venue.symbol_catalog.is_empty() {
        return Err("deployment venue has no governed instrument catalog".to_string());
    }
    for instrument in &request.instruments {
        let present = venue.symbol_catalog.iter().any(|entry| {
            entry
                .symbol()
                .is_some_and(|symbol| symbol.as_str() == instrument)
        });
        if !present {
            return Err(format!(
                "deployment instrument {instrument} is absent from the venue catalog"
            ));
        }
    }
    Ok(())
}

fn apply_strategy_bundle(
    config: &mut runtime::SystemConfig,
    request: &ActivationRequest,
    bundle: &StrategyBundle,
    bundle_path: &Path,
) -> Result<(), String> {
    bundle.validate().map_err(|error| error.to_string())?;
    if bundle.bundle_id != request.bundle_id
        || bundle.bundle_hash != request.bundle_hash
        || bundle.candidate_id != request.asset_revision_id
    {
        return Err("strategy bundle does not match the deployment envelope".to_string());
    }

    let hard_notional = config.risk.global_notional_limit;
    let requested_notional = positive_decimal("max_notional", request.max_notional)?;
    let requested_symbol = positive_decimal("max_symbol_exposure", request.max_symbol_exposure)?;
    let requested_order = positive_decimal("max_order_size", request.max_order_size)?;
    let max_slippage_bps = integer_slippage_bps(request.max_slippage_bps)?;
    if hard_notional <= rust_decimal::Decimal::ZERO {
        return Err("runtime hard notional limit disables strategy activation".to_string());
    }
    let total_notional = hard_notional.min(requested_notional).min(requested_symbol);
    let order_notional = total_notional.min(requested_order);
    let symbols = request
        .instruments
        .iter()
        .map(hft_core::Symbol::new)
        .collect::<Vec<_>>();
    let strategy_name = request.bundle_id.clone();
    let risk_limits = runtime::StrategyRiskLimits {
        max_notional: total_notional,
        max_position: config.risk.global_position_limit,
        daily_loss_limit: config.risk.max_daily_loss,
        cooldown_ms: 100,
    };

    let (strategy, strategy_ids) = match (&request.artifact, &bundle.artifact) {
        (ActivationArtifact::Formula, StrategyBundleArtifact::Formula { ast }) => {
            let ids = symbols
                .iter()
                .map(|symbol| format!("{strategy_name}:{}", symbol.as_str()))
                .collect();
            (
                runtime::StrategyConfig {
                    name: strategy_name.clone(),
                    strategy_type: runtime::StrategyType::Formula,
                    symbols,
                    params: runtime::StrategyParams::Formula {
                        ast: ast.clone(),
                        max_order_notional: order_notional,
                        signal_threshold: 0.0,
                    },
                    risk_limits,
                },
                ids,
            )
        }
        (ActivationArtifact::Onnx, StrategyBundleArtifact::Onnx { model }) => {
            let (model_path, top_n, window_size, checksum) =
                verify_onnx_artifact(model, bundle_path)?;
            (
                runtime::StrategyConfig {
                    name: strategy_name.clone(),
                    strategy_type: runtime::StrategyType::Onnx,
                    symbols,
                    params: runtime::StrategyParams::Onnx {
                        model_path: model_path.to_string_lossy().into_owned(),
                        model_version: bundle.bundle_hash.clone(),
                        model_sha256: checksum,
                        top_n,
                        window_size,
                        max_order_notional: order_notional,
                        output_threshold: 0.0,
                    },
                    risk_limits,
                },
                vec![strategy_name.clone()],
            )
        }
        _ => {
            return Err("deployment artifact intent does not match the strategy bundle".to_string())
        }
    };

    config.engine.intent_max_slippage_bps = Some(max_slippage_bps);
    config.engine.intent_max_order_notional = Some(order_notional);
    config.risk.global_notional_limit = total_notional;
    config.venues.retain(|venue| {
        venue.name.eq_ignore_ascii_case(&request.venue)
            && venue.account_id.as_deref().unwrap_or(&venue.name) == request.account_id
    });
    config.risk.strategy_overrides.clear();
    config.strategy_accounts.clear();
    for strategy_id in strategy_ids {
        config.risk.strategy_overrides.insert(
            strategy_id.clone(),
            runtime::StrategyRiskOverride {
                max_position: Some(config.risk.global_position_limit),
                max_notional: Some(order_notional),
                max_orders_per_second: Some(config.risk.max_orders_per_second),
                order_cooldown_ms: Some(100),
                staleness_threshold_us: Some(config.risk.staleness_threshold_us),
                max_daily_loss: Some(config.risk.max_daily_loss),
                aggressive_mode: Some(false),
                enhanced_overrides: None,
            },
        );
        config
            .strategy_accounts
            .insert(strategy_id, request.account_id.clone());
    }
    config.strategies = vec![strategy];
    Ok(())
}

fn positive_decimal(name: &str, value: f64) -> Result<rust_decimal::Decimal, String> {
    let value = rust_decimal::Decimal::from_f64_retain(value)
        .ok_or_else(|| format!("deployment {name} cannot be represented exactly enough"))?;
    if value <= rust_decimal::Decimal::ZERO {
        return Err(format!("deployment {name} must be positive"));
    }
    Ok(value)
}

fn integer_slippage_bps(value: f64) -> Result<i32, String> {
    if !value.is_finite() || value.trunc() != value || !(1.0..=10_000.0).contains(&value) {
        return Err(
            "deployment max_slippage_bps must be a finite integer in 1..=10000".to_string(),
        );
    }
    Ok(value as i32)
}

fn verify_onnx_artifact(
    model: &alpha_domain::OnnxModelCandidate,
    bundle_path: &Path,
) -> Result<(PathBuf, usize, usize, String), String> {
    let input = match model.inputs.as_slice() {
        [input] => input,
        _ => return Err("ONNX runtime supports exactly one input tensor".to_string()),
    };
    let [Some(batch), Some(channels), Some(window_size), Some(top_n)] = input.dimensions.as_slice()
    else {
        return Err("ONNX input shape must be static [1, 4, L, K]".to_string());
    };
    if *batch != 1 || *channels != 4 || *window_size == 0 || *top_n == 0 {
        return Err("ONNX input shape must be static [1, 4, L, K]".to_string());
    }
    let input_elements = window_size
        .checked_mul(*top_n)
        .and_then(|elements| elements.checked_mul(4))
        .ok_or_else(|| "ONNX input shape exceeds runtime limits".to_string())?;
    if input_elements > MAX_ONNX_TENSOR_ELEMENTS {
        return Err("ONNX input shape exceeds runtime limits".to_string());
    }
    if !matches!(
        model.output.dimensions.as_slice(),
        [Some(1)] | [Some(1), Some(1)]
    ) {
        return Err("ONNX output shape must be [1] or [1, 1]".to_string());
    }

    let uri = model.artifact.uri.as_str();
    if uri.contains("://") {
        return Err("ONNX artifact must use a bundle-relative path".to_string());
    }
    let relative_path = Path::new(uri);
    if relative_path.is_absolute()
        || relative_path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("ONNX artifact must stay inside the strategy bundle directory".to_string());
    }
    let bundle_directory =
        std::fs::canonicalize(bundle_path.parent().unwrap_or_else(|| Path::new(".")))
            .map_err(|error| format!("failed to resolve strategy bundle directory: {error}"))?;
    let unresolved_path = bundle_directory.join(relative_path);
    let metadata = std::fs::symlink_metadata(&unresolved_path).map_err(|error| {
        format!(
            "failed to inspect ONNX artifact {}: {error}",
            unresolved_path.display()
        )
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err("ONNX artifact must be a regular non-symlink file".to_string());
    }
    let path = std::fs::canonicalize(&unresolved_path).map_err(|error| {
        format!(
            "failed to resolve ONNX artifact {}: {error}",
            unresolved_path.display()
        )
    })?;
    if !path.starts_with(&bundle_directory) {
        return Err("ONNX artifact must stay inside the strategy bundle directory".to_string());
    }
    if metadata.len() > MAX_ONNX_ARTIFACT_BYTES {
        return Err("ONNX artifact exceeds the runtime byte limit".to_string());
    }
    if metadata.len() != model.byte_len {
        return Err("ONNX artifact byte length does not match the bundle".to_string());
    }
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("failed to read ONNX artifact {}: {error}", path.display()))?;
    let checksum = hex::encode(Sha256::digest(&bytes));
    if model.artifact.checksum.as_deref() != Some(checksum.as_str()) {
        return Err("ONNX artifact checksum does not match the bundle".to_string());
    }
    Ok((path, *top_n, *window_size, checksum))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NonceRecord {
    nonce: String,
    deployment_id: String,
    accepted_at: DateTime<Utc>,
}

pub struct RuntimeNonceLedger {
    file: File,
    consumed: BTreeSet<String>,
}

impl RuntimeNonceLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IntakeError> {
        let path = path.as_ref().to_path_buf();
        ensure_parent(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .map_err(durable_error)?;
        fs4::FileExt::lock(&file).map_err(durable_error)?;
        file.seek(SeekFrom::Start(0)).map_err(durable_error)?;
        let mut consumed = BTreeSet::new();
        for (line_number, line) in BufReader::new(&mut file).lines().enumerate() {
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
        file.seek(SeekFrom::End(0)).map_err(durable_error)?;
        Ok(Self { file, consumed })
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
        self.file.write_all(&bytes).map_err(durable_error)?;
        self.file.sync_data().map_err(durable_error)?;
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

pub struct RuntimeFeedbackLog {
    path: PathBuf,
    key_id: String,
    signing_key: SigningKey,
}

impl RuntimeFeedbackLog {
    pub fn open(
        path: impl AsRef<Path>,
        key_id: impl Into<String>,
        signing_key: SigningKey,
    ) -> Result<Self, IntakeError> {
        let path = path.as_ref().to_path_buf();
        let key_id = key_id.into();
        if key_id.trim().is_empty() {
            return Err(IntakeError::DurableState(
                "runtime feedback key id cannot be empty".to_string(),
            ));
        }
        ensure_parent(&path)?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(durable_error)?;
        Ok(Self {
            path,
            key_id,
            signing_key,
        })
    }

    pub fn append(&mut self, event: &RuntimeAttributionEvent) -> Result<(), IntakeError> {
        let signed =
            sign_runtime_attribution_event(event.clone(), self.key_id.clone(), &self.signing_key)?;
        let mut bytes = serde_json::to_vec(&signed)
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
    ledger: RuntimeNonceLedger,
    audit: RuntimeAuditLog,
    adapter: &'a mut A,
}

pub struct DeploymentReservation {
    nonce: String,
    deployment_id: String,
    ledger: Option<RuntimeNonceLedger>,
    audit: RuntimeAuditLog,
}

impl DeploymentReservation {
    pub fn commit_configuration(&mut self, at: DateTime<Utc>) -> Result<(), IntakeError> {
        let ledger = self.ledger.as_mut().ok_or_else(|| {
            IntakeError::DurableState("deployment reservation was already committed".to_string())
        })?;
        ledger.consume(&self.nonce, &self.deployment_id, at)?;
        self.ledger.take();
        self.audit.append(&RuntimeAuditEvent {
            deployment_id: self.deployment_id.clone(),
            phase: "configuration".to_string(),
            result: "prepared".to_string(),
            reason: None,
            recorded_at: at,
        })
    }

    pub fn record_startup_failed(
        &mut self,
        at: DateTime<Utc>,
        reason: impl Into<String>,
    ) -> Result<(), IntakeError> {
        self.audit.append(&RuntimeAuditEvent {
            deployment_id: self.deployment_id.clone(),
            phase: "startup".to_string(),
            result: "failed".to_string(),
            reason: Some(reason.into()),
            recorded_at: at,
        })
    }

    pub fn record_activated(&mut self, at: DateTime<Utc>) -> Result<(), IntakeError> {
        if self.ledger.is_some() {
            return Err(IntakeError::DurableState(
                "deployment cannot activate before configuration is committed".to_string(),
            ));
        }
        self.audit.append(&RuntimeAuditEvent {
            deployment_id: self.deployment_id.clone(),
            phase: "runtime".to_string(),
            result: "activated".to_string(),
            reason: None,
            recorded_at: at,
        })
    }
}

impl<'a, A: RuntimeActivationAdapter> DeploymentIntake<'a, A> {
    pub fn new(
        trusted_keys: &'a BTreeMap<String, VerifyingKey>,
        policy: &'a RuntimeEnvelopePolicy,
        runtime_paused: bool,
        ledger: RuntimeNonceLedger,
        audit: RuntimeAuditLog,
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

    pub fn prepare(
        mut self,
        signed: &SignedDeploymentEnvelope,
        now: DateTime<Utc>,
    ) -> Result<(ActivationRequest, DeploymentReservation), IntakeError> {
        let verified = match verify_envelope(signed, self.trusted_keys, self.policy, now, |nonce| {
            self.ledger.contains(nonce)
        }) {
            Ok(verified) => verified,
            Err(error) => {
                append_rejection(
                    &mut self.audit,
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
                    &mut self.audit,
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
            result: "verified".to_string(),
            reason: None,
            recorded_at: now,
        })?;
        if let Err(error) = self.adapter.activate(&request) {
            self.audit.append(&RuntimeAuditEvent {
                deployment_id: request.deployment_id.clone(),
                phase: "configuration".to_string(),
                result: "failed".to_string(),
                reason: Some(error.clone()),
                recorded_at: now,
            })?;
            return Err(IntakeError::Activation(error));
        }
        let reservation = DeploymentReservation {
            nonce: signed.envelope.nonce.clone(),
            deployment_id: signed.envelope.deployment_id.clone(),
            ledger: Some(self.ledger),
            audit: self.audit,
        };
        Ok((request, reservation))
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
    let artifacts = verified
        .0
        .allowed_intent_types
        .iter()
        .filter_map(|intent| match intent {
            AllowedIntentType::LoadFactor => Some(ActivationArtifact::Formula),
            AllowedIntentType::LoadModel => Some(ActivationArtifact::Onnx),
            _ => None,
        })
        .collect::<Vec<_>>();
    let [artifact] = artifacts.as_slice() else {
        return Err(IntakeError::InvalidArtifactIntent);
    };
    if verified
        .0
        .allowed_intent_types
        .contains(&AllowedIntentType::LoadAllocatorPolicy)
    {
        return Err(IntakeError::InvalidArtifactIntent);
    }
    if verified.0.instruments.iter().collect::<BTreeSet<_>>().len() != verified.0.instruments.len()
    {
        return Err(IntakeError::DuplicateInstrument);
    }
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
            ApprovalClass::HumanApprovedLiveSmall
        ),
    };
    if !approved {
        return Err(IntakeError::ApprovalClassMismatch);
    }
    Ok(ActivationRequest {
        deployment_id: verified.0.deployment_id.clone(),
        asset_revision_id: verified.0.asset_revision_id.clone(),
        promotion_id: verified.0.promotion_id.clone(),
        bundle_id: verified.0.bundle_id.clone(),
        bundle_hash: verified.0.bundle_hash.clone(),
        account_id: verified.0.account_id.clone(),
        venue: verified.0.venue.clone(),
        instruments: verified.0.instruments.clone(),
        artifact: *artifact,
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

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::DeploymentEnvelope;
    use chrono::Duration;

    #[test]
    fn same_class_auto_live_small_is_not_runtime_authority() {
        let now = Utc::now();
        let verified = VerifiedDeploymentEnvelope(DeploymentEnvelope {
            deployment_id: "deployment-auto-live".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            promotion_id: "promotion-1".to_string(),
            promotion_manifest_hash: "a".repeat(64),
            bundle_id: "bundle-1".to_string(),
            bundle_hash: "b".repeat(64),
            runtime_config_hash: "c".repeat(64),
            risk_policy_hash: "d".repeat(64),
            account_id: "account-1".to_string(),
            venue: "binance".to_string(),
            instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![
                AllowedIntentType::LoadFactor,
                AllowedIntentType::StartLiveSmall,
            ],
            max_notional: 100.0,
            max_symbol_exposure: 50.0,
            max_order_size: 10.0,
            max_slippage_bps: 2.0,
            valid_from: now - Duration::minutes(1),
            expires_at: now + Duration::minutes(10),
            nonce: "nonce-auto-live".to_string(),
            approval_class: ApprovalClass::SameClassAutoLiveSmall,
            approval_signatures: vec!["approval-1".to_string()],
            payload_hash: "e".repeat(64),
        });

        assert!(matches!(
            activation_request(&verified, false),
            Err(IntakeError::ApprovalClassMismatch)
        ));
    }
}
