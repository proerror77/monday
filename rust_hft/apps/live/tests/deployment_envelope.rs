use alpha_domain::{
    sign_envelope, AllowedIntentType, ApprovalClass, DeploymentEnvelope, RuntimeEnvelopePolicy,
    StrategyBundle,
};
use chrono::{Duration, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
#[cfg(any(
    not(feature = "formula-strategy"),
    all(feature = "formula-strategy", feature = "binance")
))]
use hft_live::deployment_envelope::ActivationMode;
use hft_live::deployment_envelope::{
    ActivationRequest, DeploymentIntake, DeploymentReservation, RuntimeActivationAdapter,
    RuntimeAuditLog, RuntimeNonceLedger, SystemConfigActivationAdapter,
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
            AllowedIntentType::LoadModel,
            AllowedIntentType::StartPaper,
            AllowedIntentType::StartShadow,
            AllowedIntentType::StartLiveSmall,
        ],
        runtime_config_hash: "c".repeat(64),
        risk_policy_hash: "d".repeat(64),
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
    let bundle = formula_bundle(now);
    DeploymentEnvelope {
        deployment_id: id.to_string(),
        asset_revision_id: bundle.candidate_id.clone(),
        promotion_id: "promotion-1".to_string(),
        promotion_manifest_hash: "a".repeat(64),
        bundle_id: bundle.bundle_id,
        bundle_hash: bundle.bundle_hash,
        runtime_config_hash: "c".repeat(64),
        risk_policy_hash: "d".repeat(64),
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

fn formula_bundle(now: chrono::DateTime<Utc>) -> StrategyBundle {
    let mut bundle: StrategyBundle = serde_json::from_value(serde_json::json!({
        "bundle_id": "bundle-1",
        "candidate_id": "candidate-1",
        "candidate_content_hash": "1".repeat(64),
        "dataset_manifest_id": "dataset-1",
        "evaluator_version": "sealed-holdout-v2",
        "evaluator_config_hash": "3".repeat(64),
        "evaluation_metrics_hash": "4".repeat(64),
        "sealed_evaluation_hash": "2".repeat(64),
        "artifact": {
            "Formula": {
                "ast": {"Terminal": {"Field": "book_imbalance"}}
            }
        },
        "bundle_hash": "",
        "created_at": now,
    }))
    .unwrap();
    bundle.bundle_hash = bundle.calculated_hash().unwrap();
    bundle
}

fn onnx_bundle(
    now: chrono::DateTime<Utc>,
    uri: &str,
    byte_len: u64,
    checksum: &str,
    channels: usize,
) -> StrategyBundle {
    let mut bundle: StrategyBundle = serde_json::from_value(serde_json::json!({
        "bundle_id": "bundle-onnx",
        "candidate_id": "candidate-onnx",
        "candidate_content_hash": "3".repeat(64),
        "dataset_manifest_id": "dataset-1",
        "evaluator_version": "onnx-fixture-v1",
        "evaluator_config_hash": "5".repeat(64),
        "evaluation_metrics_hash": "6".repeat(64),
        "sealed_evaluation_hash": "4".repeat(64),
        "artifact": {
            "Onnx": {
                "model": {
                    "artifact": {
                        "uri": uri,
                        "content_type": "application/onnx",
                        "checksum": checksum
                    },
                    "byte_len": byte_len,
                    "opset": 17,
                    "inputs": [{
                        "name": "lob",
                        "element_type": "Float32",
                        "dimensions": [1, channels, 2, 2]
                    }],
                    "output": {
                        "name": "signal",
                        "element_type": "Float32",
                        "dimensions": [1, 1]
                    }
                }
            }
        },
        "bundle_hash": "",
        "created_at": now,
    }))
    .unwrap();
    bundle.bundle_hash = bundle.calculated_hash().unwrap();
    bundle
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
    let (request, mut reservation) = prepare_intake(signed, keys, policy, now, directory, adapter)?;
    reservation.commit_configuration(now)?;
    Ok(request)
}

fn prepare_intake<A: RuntimeActivationAdapter>(
    signed: &alpha_domain::SignedDeploymentEnvelope,
    keys: &BTreeMap<String, VerifyingKey>,
    policy: &RuntimeEnvelopePolicy,
    now: chrono::DateTime<Utc>,
    directory: &std::path::Path,
    adapter: &mut A,
) -> Result<(ActivationRequest, DeploymentReservation), hft_live::deployment_envelope::IntakeError>
{
    let ledger = RuntimeNonceLedger::open(directory.join("nonces.jsonl")).unwrap();
    let audit = RuntimeAuditLog::open(directory.join("audit.jsonl")).unwrap();
    DeploymentIntake::new(keys, policy, false, ledger, audit, adapter).prepare(signed, now)
}

fn configured_runtime() -> runtime::SystemConfig {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config/dev/binance_quotes_only.yaml");
    let builder = runtime::SystemBuilder::from_yaml(path.to_str().unwrap()).unwrap();
    let mut config = builder.config().clone();
    config.venues[0].account_id = Some("account-1".to_string());
    config.risk.global_position_limit = rust_decimal::Decimal::from(10);
    config.risk.global_notional_limit = rust_decimal::Decimal::from(2_000);
    config.risk.max_orders_per_second = 10;
    config.risk.max_daily_loss = rust_decimal::Decimal::from(100);
    config
}

#[test]
#[cfg(all(feature = "formula-strategy", feature = "binance"))]
fn accepted_paper_shadow_handoff_reaches_both_runtime_adapters() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("accepted-handoff");
    let mut config = configured_runtime();
    let bundle = formula_bundle(now);
    let bundle_path = directory.join("bundle.json");
    let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);

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
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.strategies[0].name, bundle.bundle_id);
    let runtime = runtime::SystemBuilder::new(config.clone())
        .auto_register_adapters_strict()
        .unwrap()
        .build();
    assert_eq!(
        runtime.engine.try_lock().unwrap().strategy_instance_ids(),
        vec!["bundle-1:BTCUSDT".to_string()]
    );
    let mut duplicate = config.clone();
    duplicate.strategies.push(config.strategies[0].clone());
    assert!(runtime::SystemBuilder::new(duplicate)
        .auto_register_adapters_strict()
        .is_err());
    assert_eq!(
        std::fs::read_to_string(directory.join("nonces.jsonl"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let audit = std::fs::read_to_string(directory.join("audit.jsonl")).unwrap();
    assert_eq!(audit.lines().count(), 4);
    assert!(audit.contains("\"phase\":\"configuration\""));
    assert!(audit.contains("\"result\":\"prepared\""));
    assert!(!audit.contains("\"result\":\"activated\""));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(not(feature = "formula-strategy"))]
fn featureless_runtime_rejects_formula_strategy_startup() {
    let now = Utc::now();
    let mut config = configured_runtime();
    let bundle = formula_bundle(now);
    let bundle_path = directory("featureless").join("bundle.json");
    let request = ActivationRequest {
        deployment_id: "deployment-featureless".to_string(),
        asset_revision_id: bundle.candidate_id.clone(),
        promotion_id: "promotion-1".to_string(),
        bundle_id: bundle.bundle_id.clone(),
        bundle_hash: bundle.bundle_hash.clone(),
        account_id: "account-1".to_string(),
        venue: "binance".to_string(),
        instruments: vec!["BTCUSDT".to_string()],
        artifact: hft_live::deployment_envelope::ActivationArtifact::Formula,
        mode: ActivationMode::Paper,
        max_notional: 1_000.0,
        max_symbol_exposure: 500.0,
        max_order_size: 100.0,
        max_slippage_bps: 5.0,
    };
    SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path)
        .activate(&request)
        .unwrap();

    assert!(runtime::SystemBuilder::new(config)
        .auto_register_adapters_strict()
        .is_err());
    std::fs::remove_dir_all(bundle_path.parent().unwrap()).unwrap();
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
            2 => changed.runtime_config_hash = "e".repeat(64),
            3 => changed.risk_policy_hash = "f".repeat(64),
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
fn concurrent_startups_serialize_nonce_reservation_and_recheck_the_ledger() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("nonce-concurrent");
    let signed = sign_envelope(
        envelope(
            now,
            "paper-concurrent",
            "nonce-concurrent",
            AllowedIntentType::StartPaper,
            ApprovalClass::Paper,
        ),
        "key-1",
        &key,
    )
    .unwrap();
    let mut first_adapter = RecordingAdapter::default();
    let (_, mut first_reservation) = prepare_intake(
        &signed,
        &trusted,
        &policy(),
        now,
        &directory,
        &mut first_adapter,
    )
    .unwrap();

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let second_directory = directory.clone();
    let second_signed = signed.clone();
    let second_trusted = trusted.clone();
    let handle = std::thread::spawn(move || {
        let mut adapter = RecordingAdapter::default();
        started_tx.send(()).unwrap();
        let result = prepare_intake(
            &second_signed,
            &second_trusted,
            &policy(),
            now,
            &second_directory,
            &mut adapter,
        );
        result_tx.send(result.is_err()).unwrap();
    });
    started_rx.recv().unwrap();
    assert!(result_rx
        .recv_timeout(std::time::Duration::from_millis(50))
        .is_err());

    first_reservation.commit_configuration(now).unwrap();
    drop(first_reservation);
    assert!(result_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap());
    handle.join().unwrap();
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
    let bundle = formula_bundle(now);
    let bundle_path = directory.join("bundle.json");
    let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);
    assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter,).is_err());
    assert!(!RuntimeNonceLedger::open(directory.join("nonces.jsonl"))
        .unwrap()
        .contains("nonce-live-small"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn onnx_handoff_rejects_bad_schema_and_checksum_before_runtime_build() {
    use sha2::{Digest, Sha256};

    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("onnx-validation");
    let model_path = directory.join("model.onnx");
    let bytes = b"not an onnx model";
    std::fs::write(&model_path, bytes).unwrap();

    for (suffix, checksum, channels) in [
        ("checksum", "f".repeat(64), 4),
        ("schema", hex::encode(Sha256::digest(bytes)), 3),
    ] {
        let bundle = onnx_bundle(
            now,
            model_path.to_str().unwrap(),
            bytes.len() as u64,
            &checksum,
            channels,
        );
        let mut unsigned = envelope(
            now,
            &format!("onnx-{suffix}"),
            &format!("nonce-onnx-{suffix}"),
            AllowedIntentType::StartPaper,
            ApprovalClass::Paper,
        );
        unsigned.asset_revision_id = bundle.candidate_id.clone();
        unsigned.bundle_id = bundle.bundle_id.clone();
        unsigned.bundle_hash = bundle.bundle_hash.clone();
        unsigned.allowed_intent_types =
            vec![AllowedIntentType::LoadModel, AllowedIntentType::StartPaper];
        let signed = sign_envelope(unsigned, "key-1", &key).unwrap();
        let mut config = configured_runtime();
        let bundle_path = directory.join(format!("bundle-{suffix}.json"));
        let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);

        assert!(intake(&signed, &trusted, &policy(), now, &directory, &mut adapter,).is_err());
        drop(adapter);
        assert!(config.strategies.is_empty());
    }

    let checksum = hex::encode(Sha256::digest(bytes));
    let bundle = onnx_bundle(
        now,
        model_path.to_str().unwrap(),
        bytes.len() as u64,
        &checksum,
        4,
    );
    let mut unsigned = envelope(
        now,
        "onnx-malformed",
        "nonce-onnx-malformed",
        AllowedIntentType::StartPaper,
        ApprovalClass::Paper,
    );
    unsigned.asset_revision_id = bundle.candidate_id.clone();
    unsigned.bundle_id = bundle.bundle_id.clone();
    unsigned.bundle_hash = bundle.bundle_hash.clone();
    unsigned.allowed_intent_types =
        vec![AllowedIntentType::LoadModel, AllowedIntentType::StartPaper];
    let signed = sign_envelope(unsigned, "key-1", &key).unwrap();
    let mut config = configured_runtime();
    let bundle_path = directory.join("bundle-valid-metadata.json");
    let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);
    let (_, reservation) =
        prepare_intake(&signed, &trusted, &policy(), now, &directory, &mut adapter).unwrap();
    drop(adapter);
    assert!(matches!(
        config.strategies[0].strategy_type,
        runtime::StrategyType::Onnx
    ));
    #[cfg(feature = "dl-strategy")]
    {
        let mut reservation = reservation;
        let error = runtime::SystemBuilder::new(config)
            .auto_register_adapters_strict()
            .err()
            .unwrap();
        reservation
            .record_startup_failed(now, error.to_string())
            .unwrap();
        drop(reservation);
        assert!(!RuntimeNonceLedger::open(directory.join("nonces.jsonl"))
            .unwrap()
            .contains("nonce-onnx-malformed"));
        let audit = std::fs::read_to_string(directory.join("audit.jsonl")).unwrap();
        assert!(audit.contains("\"phase\":\"startup\""));
        assert!(!audit.contains("\"result\":\"prepared\""));
    }
    #[cfg(not(feature = "dl-strategy"))]
    drop(reservation);

    std::fs::remove_dir_all(directory).unwrap();
}
