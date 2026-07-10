use alpha_domain::{
    sign_envelope, AllowedIntentType, ApprovalClass, DeploymentEnvelope, RuntimeEnvelopePolicy,
};
use chrono::{Duration, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hft_live::deployment_envelope::{
    ActivationMode, ActivationRequest, DeploymentIntake, RuntimeActivationAdapter, RuntimeAuditLog,
    RuntimeNonceLedger, SystemConfigActivationAdapter,
};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Default)]
struct RecordingAdapter {
    requests: Vec<ActivationRequest>,
}

impl RuntimeActivationAdapter for RecordingAdapter {
    fn activate(&mut self, request: &ActivationRequest) -> Result<(), String> {
        self.requests.push(request.clone());
        Ok(())
    }
}

fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "hft-live-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn policy() -> RuntimeEnvelopePolicy {
    RuntimeEnvelopePolicy {
        account_id: "account-1".to_string(),
        venue: "binance".to_string(),
        allowed_instruments: vec!["BTCUSDT".to_string()],
        allowed_intent_types: vec![
            AllowedIntentType::LoadFactor,
            AllowedIntentType::StartPaper,
            AllowedIntentType::StartShadow,
            AllowedIntentType::StartLiveSmall,
        ],
        runtime_config_hash: "runtime-hash".to_string(),
        risk_policy_hash: "risk-hash".to_string(),
        max_notional: 1_000.0,
        max_symbol_exposure: 500.0,
        max_order_size: 100.0,
        max_slippage_bps: 5.0,
    }
}

fn envelope(
    now: chrono::DateTime<Utc>,
    id: &str,
    nonce: &str,
    mode: AllowedIntentType,
    approval: ApprovalClass,
) -> DeploymentEnvelope {
    DeploymentEnvelope {
        deployment_id: id.to_string(),
        asset_revision_id: "factor-1@3".to_string(),
        promotion_manifest_hash: "promotion-hash".to_string(),
        runtime_config_hash: "runtime-hash".to_string(),
        risk_policy_hash: "risk-hash".to_string(),
        account_id: "account-1".to_string(),
        venue: "binance".to_string(),
        instruments: vec!["BTCUSDT".to_string()],
        allowed_intent_types: vec![AllowedIntentType::LoadFactor, mode],
        max_notional: 1_000.0,
        max_symbol_exposure: 500.0,
        max_order_size: 100.0,
        max_slippage_bps: 5.0,
        valid_from: now - Duration::minutes(1),
        expires_at: now + Duration::minutes(10),
        nonce: nonce.to_string(),
        approval_class: approval,
        approval_signatures: vec!["approval-1".to_string()],
        payload_hash: String::new(),
    }
}

fn trusted(key: &SigningKey) -> BTreeMap<String, VerifyingKey> {
    BTreeMap::from([("key-1".to_string(), key.verifying_key())])
}

fn intake<A: RuntimeActivationAdapter>(
    signed: &alpha_domain::SignedDeploymentEnvelope,
    keys: &BTreeMap<String, VerifyingKey>,
    policy: &RuntimeEnvelopePolicy,
    now: chrono::DateTime<Utc>,
    directory: &std::path::Path,
    adapter: &mut A,
) -> Result<ActivationRequest, hft_live::deployment_envelope::IntakeError> {
    let mut ledger = RuntimeNonceLedger::open(directory.join("nonces.jsonl")).unwrap();
    let mut audit = RuntimeAuditLog::open(directory.join("audit.jsonl")).unwrap();
    DeploymentIntake::new(keys, policy, false, &mut ledger, &mut audit, adapter).accept(signed, now)
}

fn configured_runtime() -> runtime::SystemConfig {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config/dev/binance_quotes_only.yaml");
    let builder = runtime::SystemBuilder::from_yaml(path.to_str().unwrap()).unwrap();
    let mut config = builder.config().clone();
    config.venues[0].account_id = Some("account-1".to_string());
    config
}

#[test]
fn accepted_paper_shadow_handoff_reaches_both_runtime_adapters() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("accepted-handoff");
    let mut config = configured_runtime();
    let mut adapter = SystemConfigActivationAdapter::new(&mut config);

    let paper = sign_envelope(
        envelope(
            now,
            "paper-1",
            "nonce-paper",
            AllowedIntentType::StartPaper,
            ApprovalClass::Paper,
        ),
        "key-1",
        &key,
    )
    .unwrap();
    let shadow = sign_envelope(
        envelope(
            now,
            "shadow-1",
            "nonce-shadow",
            AllowedIntentType::StartShadow,
            ApprovalClass::Shadow,
        ),
        "key-1",
        &key,
    )
    .unwrap();
    assert_eq!(
        intake(&paper, &trusted, &policy(), now, &directory, &mut adapter)
            .unwrap()
            .mode,
        ActivationMode::Paper
    );
    assert_eq!(
        intake(&shadow, &trusted, &policy(), now, &directory, &mut adapter,)
            .unwrap()
            .mode,
        ActivationMode::Shadow
    );
    assert_eq!(
        adapter.applied_modes(),
        &[ActivationMode::Paper, ActivationMode::Shadow]
    );
    drop(adapter);
    assert!(config.quotes_only);
    assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Paper"));
    assert_eq!(
        std::fs::read_to_string(directory.join("nonces.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("audit.jsonl"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn runtime_rejects_forgery_time_binding_key_and_limit_failures() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("rejections");
    let base = envelope(
        now,
        "reject-1",
        "nonce-reject",
        AllowedIntentType::StartPaper,
        ApprovalClass::Paper,
    );
    let mut adapter = RecordingAdapter::default();

    let mut forged = sign_envelope(base.clone(), "key-1", &key).unwrap();
    forged.envelope.max_notional += 1.0;
    assert!(intake(&forged, &trusted, &policy(), now, &directory, &mut adapter).is_err());

    let mut invalid_signature = sign_envelope(base.clone(), "key-1", &key).unwrap();
    let replacement = if invalid_signature.signature_hex.starts_with('0') {
        "1"
    } else {
        "0"
    };
    invalid_signature
        .signature_hex
        .replace_range(0..1, replacement);
    assert!(intake(
        &invalid_signature,
        &trusted,
        &policy(),
        now,
        &directory,
        &mut adapter
    )
    .is_err());

    let signed = sign_envelope(base.clone(), "key-1", &key).unwrap();
    assert!(intake(
        &signed,
        &BTreeMap::new(),
        &policy(),
        now,
        &directory,
        &mut adapter
    )
    .is_err());

    let mut expired = base.clone();
    expired.valid_from = now - Duration::minutes(2);
    expired.expires_at = now - Duration::minutes(1);
    let signed = sign_envelope(expired, "key-1", &key).unwrap();
    assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter).is_err());

    let mut early = base.clone();
    early.valid_from = now + Duration::minutes(1);
    early.expires_at = now + Duration::minutes(2);
    let signed = sign_envelope(early, "key-1", &key).unwrap();
    assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter).is_err());

    for mutation in 0..5 {
        let mut changed = base.clone();
        changed.nonce = format!("nonce-binding-{mutation}");
        match mutation {
            0 => changed.account_id = "wrong-account".to_string(),
            1 => changed.venue = "wrong-venue".to_string(),
            2 => changed.runtime_config_hash = "wrong-runtime".to_string(),
            3 => changed.risk_policy_hash = "wrong-risk".to_string(),
            _ => changed.max_order_size = policy().max_order_size + 1.0,
        }
        let signed = sign_envelope(changed, "key-1", &key).unwrap();
        assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter).is_err());
    }
    assert!(adapter.requests.is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn nonce_replay_is_rejected_after_ledger_restart() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("nonce-restart");
    let signed = sign_envelope(
        envelope(
            now,
            "paper-replay",
            "nonce-replay",
            AllowedIntentType::StartPaper,
            ApprovalClass::Paper,
        ),
        "key-1",
        &key,
    )
    .unwrap();
    let mut adapter = RecordingAdapter::default();
    intake(&signed, &trusted, &policy(), now, &directory, &mut adapter).unwrap();
    assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter).is_err());
    assert_eq!(adapter.requests.len(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn live_small_fails_closed_until_order_limits_are_in_the_hot_path() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("live-small-disabled");
    let signed = sign_envelope(
        envelope(
            now,
            "live-small-1",
            "nonce-live-small",
            AllowedIntentType::StartLiveSmall,
            ApprovalClass::HumanApprovedLiveSmall,
        ),
        "key-1",
        &key,
    )
    .unwrap();
    let mut config = configured_runtime();
    let mut adapter = SystemConfigActivationAdapter::new(&mut config);
    assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter,).is_err());
    assert!(RuntimeNonceLedger::open(directory.join("nonces.jsonl"))
        .unwrap()
        .contains("nonce-live-small"));
    std::fs::remove_dir_all(directory).unwrap();
}
