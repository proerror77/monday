use alpha_domain::{
    deployment_scope_hash, sign_envelope, AllowedIntentType, ApprovalClass, DeploymentEnvelope,
    RuntimeApprovalEvidence, RuntimeEnvelopePolicy, StrategyBundle,
};
use chrono::{Duration, Utc};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hft_live::deployment_envelope::ActivationMode;
#[cfg(feature = "formula-strategy")]
use hft_live::deployment_envelope::RuntimeFeedbackLog;
use hft_live::deployment_envelope::{
    ActivationRequest, DeploymentIntake, DeploymentReservation, RuntimeActivationAdapter,
    RuntimeAuditLog, RuntimeNonceLedger, SystemConfigActivationAdapter,
};
#[cfg(feature = "formula-strategy")]
use hft_live::runtime_attribution::RuntimeAttributionObserver;
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

fn policy(envelope: &DeploymentEnvelope) -> RuntimeEnvelopePolicy {
    RuntimeEnvelopePolicy {
        account_id: envelope.account_id.clone(),
        venue: envelope.venue.clone(),
        allowed_instruments: envelope.instruments.clone(),
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
        approvals: vec![RuntimeApprovalEvidence {
            approval_id: envelope.approval_signatures[0].clone(),
            approval_class: envelope.approval_class.clone(),
            subject_id: envelope.promotion_id.clone(),
            scope_hash: deployment_scope_hash(envelope).unwrap(),
            signer_id: "risk-officer-1".to_string(),
            valid_from: envelope.valid_from,
            expires_at: envelope.expires_at,
            revoked_at: None,
        }],
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
                    "preprocessing_version": "lob-relative-price-log-size-v1",
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
    config.engine.intent_max_latency_us = 1_000_000;
    config
}

fn configured_polymarket_runtime() -> runtime::SystemConfig {
    let mut config = configured_runtime();
    let venue = &mut config.venues[0];
    venue.name = "polymarket-main".to_string();
    venue.venue_type = runtime::VenueType::Polymarket;
    venue.symbol_catalog[0].0 = "123456789@POLYMARKET".to_string();
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
        intake(
            &paper,
            &trusted,
            &policy(&paper.envelope),
            now,
            &directory,
            &mut adapter,
        )
        .unwrap()
        .mode,
        ActivationMode::Paper
    );
    assert_eq!(
        intake(
            &shadow,
            &trusted,
            &policy(&shadow.envelope),
            now,
            &directory,
            &mut adapter,
        )
        .unwrap()
        .mode,
        ActivationMode::Shadow
    );
    assert_eq!(
        adapter.applied_modes(),
        &[ActivationMode::Paper, ActivationMode::Shadow]
    );
    drop(adapter);
    assert!(!config.quotes_only);
    assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Paper"));
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.engine.intent_max_slippage_bps, Some(5));
    assert_eq!(
        config.engine.intent_max_order_notional,
        Some(rust_decimal::Decimal::from(100))
    );
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

#[tokio::test]
#[cfg(feature = "formula-strategy")]
async fn shadow_activation_waits_for_market_then_produces_loop_consumable_evidence() {
    use alpha_domain::{
        runtime_stage_is_healthy, verify_runtime_attribution_event, AttributionKind,
        AttributionMode, AttributionOutcome, RuntimeAttributionEvent,
        SignedRuntimeAttributionEvent,
    };
    use engine::dataflow::{EventIngester, IngestionConfig};
    use hft_core::{Symbol, VenueId};
    use ports::{BookLevel, MarketEvent, MarketSnapshot};
    use std::collections::BTreeMap;
    use tokio::sync::oneshot;

    let now = Utc::now();
    let envelope_key = SigningKey::from_bytes(&[7_u8; 32]);
    let signed = sign_envelope(
        envelope(
            now,
            "shadow-runtime-1",
            "nonce-shadow-runtime-1",
            AllowedIntentType::StartShadow,
            ApprovalClass::Shadow,
        ),
        "key-1",
        &envelope_key,
    )
    .unwrap();
    let directory = directory("shadow-runtime-evidence");
    let bundle = formula_bundle(now);
    let bundle_path = directory.join("bundle.json");
    let mut config = configured_runtime();
    let request = {
        let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);
        intake(
            &signed,
            &trusted(&envelope_key),
            &policy(&signed.envelope),
            now,
            &directory,
            &mut adapter,
        )
        .unwrap()
    };
    assert_eq!(request.mode, ActivationMode::Shadow);
    assert!(!config.quotes_only);
    assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Paper"));

    let (mut ingester, consumer) = EventIngester::new(IngestionConfig {
        stale_threshold_us: u64::MAX,
        ..IngestionConfig::default()
    });
    let mut system = runtime::SystemBuilder::new(config)
        .register_event_consumer(consumer)
        .register_simulated_execution_client(VenueId::BINANCE)
        .register_strategies_from_config_strict()
        .unwrap()
        .build();
    let (execution_receiver, mut diagnostic_receiver, market_reader, notify) = {
        let engine = system.engine.lock().await;
        (
            engine.subscribe_execution_events(),
            engine.subscribe_execution_events(),
            engine.market_reader(),
            engine.get_wakeup_notify(),
        )
    };
    ingester.set_engine_notify(notify);

    let feedback_key = SigningKey::from_bytes(&[13_u8; 32]);
    let feedback_path = directory.join("runtime-feedback.jsonl");
    let mut feedback_log =
        RuntimeFeedbackLog::open(&feedback_path, "runtime-feedback-1", feedback_key.clone())
            .unwrap();
    feedback_log
        .append(&RuntimeAttributionEvent {
            event_id: format!("activation:{}", request.deployment_id),
            deployment_id: request.deployment_id.clone(),
            asset_revision_id: request.asset_revision_id.clone(),
            mission_id: None,
            mode: AttributionMode::Shadow,
            outcome: AttributionOutcome::Activated,
            kind: AttributionKind::Activation,
            strategy_id: None,
            order_id: None,
            account_id: Some(request.account_id.clone()),
            venue: Some(request.venue.clone()),
            symbol: None,
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        })
        .unwrap();
    let observer = RuntimeAttributionObserver::new(
        execution_receiver,
        market_reader,
        request.clone(),
        feedback_log,
        u64::MAX,
    );
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let observer_handle = tokio::spawn(observer.run(shutdown_rx));

    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let startup_events = std::fs::read_to_string(&feedback_path).unwrap();
    assert!(!startup_events
        .lines()
        .filter_map(|line| serde_json::from_str::<SignedRuntimeAttributionEvent>(line).ok())
        .any(|signed| signed.event.kind == AttributionKind::PortfolioSnapshot));

    system.start().await.unwrap();
    ingester
        .ingest(MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: hft_core::now_micros(),
            bids: vec![BookLevel::new_unchecked(99.0, 3.0)],
            asks: vec![BookLevel::new_unchecked(100.0, 1.0)],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
        }))
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let has_fill = std::fs::read_to_string(&feedback_path)
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<SignedRuntimeAttributionEvent>(line).ok())
            .any(|signed| signed.event.kind == AttributionKind::Fill);
        if has_fill {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let statistics = system.engine.lock().await.get_statistics();
            let mut execution_events = Vec::new();
            while let Ok(event) = diagnostic_receiver.try_recv() {
                execution_events.push(event);
            }
            panic!(
                "shadow paper execution did not emit a fill: {statistics:?}; events={execution_events:?}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    shutdown_tx.send(()).unwrap();
    observer_handle.await.unwrap().unwrap();
    system.stop().await.unwrap();

    let trusted_feedback = BTreeMap::from([(
        "runtime-feedback-1".to_string(),
        feedback_key.verifying_key(),
    )]);
    let events = std::fs::read_to_string(&feedback_path)
        .unwrap()
        .lines()
        .map(|line| {
            let signed: SignedRuntimeAttributionEvent = serde_json::from_str(line).unwrap();
            verify_runtime_attribution_event(&signed, &trusted_feedback).unwrap()
        })
        .collect::<Vec<_>>();
    assert!(events
        .iter()
        .any(|event| event.kind == AttributionKind::Fill));
    assert!(events
        .iter()
        .any(|event| event.kind == AttributionKind::PortfolioSnapshot));
    assert!(runtime_stage_is_healthy(
        &events,
        &request.asset_revision_id,
        AttributionMode::Shadow,
    ));

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
    let runtime_policy = policy(&base);

    let mut forged = sign_envelope(base.clone(), "key-1", &key).unwrap();
    forged.envelope.max_notional += 1.0;
    assert!(intake(
        &forged,
        &trusted,
        &runtime_policy,
        now,
        &directory,
        &mut adapter
    )
    .is_err());

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
        &runtime_policy,
        now,
        &directory,
        &mut adapter
    )
    .is_err());

    let signed = sign_envelope(base.clone(), "key-1", &key).unwrap();
    assert!(intake(
        &signed,
        &BTreeMap::new(),
        &runtime_policy,
        now,
        &directory,
        &mut adapter
    )
    .is_err());

    let mut expired = base.clone();
    expired.valid_from = now - Duration::minutes(2);
    expired.expires_at = now - Duration::minutes(1);
    let signed = sign_envelope(expired, "key-1", &key).unwrap();
    assert!(intake(
        &signed,
        &trusted,
        &runtime_policy,
        now,
        &directory,
        &mut adapter
    )
    .is_err());

    let mut early = base.clone();
    early.valid_from = now + Duration::minutes(1);
    early.expires_at = now + Duration::minutes(2);
    let signed = sign_envelope(early, "key-1", &key).unwrap();
    assert!(intake(
        &signed,
        &trusted,
        &runtime_policy,
        now,
        &directory,
        &mut adapter
    )
    .is_err());

    for mutation in 0..5 {
        let mut changed = base.clone();
        changed.nonce = format!("nonce-binding-{mutation}");
        match mutation {
            0 => changed.account_id = "wrong-account".to_string(),
            1 => changed.venue = "wrong-venue".to_string(),
            2 => changed.runtime_config_hash = "e".repeat(64),
            3 => changed.risk_policy_hash = "f".repeat(64),
            _ => changed.max_order_size = runtime_policy.max_order_size + 1.0,
        }
        let signed = sign_envelope(changed, "key-1", &key).unwrap();
        assert!(intake(
            &signed,
            &trusted,
            &runtime_policy,
            now,
            &directory,
            &mut adapter
        )
        .is_err());
    }
    assert!(adapter.requests.is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn runtime_rejects_a_valid_envelope_without_exact_operator_approval_evidence() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let signed = sign_envelope(
        envelope(
            now,
            "approval-rejection",
            "nonce-approval-rejection",
            AllowedIntentType::StartShadow,
            ApprovalClass::Shadow,
        ),
        "key-1",
        &key,
    )
    .unwrap();
    let trusted = trusted(&key);
    let directory = directory("approval-rejection");

    for mutation in 0..5 {
        let mut runtime_policy = policy(&signed.envelope);
        match mutation {
            0 => runtime_policy.approvals.clear(),
            1 => runtime_policy.approvals[0].approval_class = ApprovalClass::Paper,
            2 => runtime_policy.approvals[0].subject_id = "promotion-forged".to_string(),
            3 => runtime_policy.approvals[0].scope_hash = "f".repeat(64),
            _ => runtime_policy.approvals[0].revoked_at = Some(now),
        }
        let mut adapter = RecordingAdapter::default();
        let error = intake(
            &signed,
            &trusted,
            &runtime_policy,
            now,
            &directory,
            &mut adapter,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("approval evidence"));
        assert!(adapter.requests.is_empty());
    }

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
    let runtime_policy = policy(&signed.envelope);
    intake(
        &signed,
        &trusted,
        &runtime_policy,
        now,
        &directory,
        &mut adapter,
    )
    .unwrap();
    assert!(intake(
        &signed,
        &trusted,
        &runtime_policy,
        now,
        &directory,
        &mut adapter
    )
    .is_err());
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
        &policy(&signed.envelope),
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
            &policy(&second_signed.envelope),
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
fn live_small_polymarket_formula_activation_is_allowed() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("live-small-disabled");
    let mut unsigned = envelope(
        now,
        "live-small-1",
        "nonce-live-small",
        AllowedIntentType::StartLiveSmall,
        ApprovalClass::HumanApprovedLiveSmall,
    );
    unsigned.venue = "polymarket-main".to_string();
    unsigned.instruments = vec!["123456789".to_string()];
    let signed = sign_envelope(unsigned, "key-1", &key).unwrap();
    let mut config = configured_polymarket_runtime();
    let bundle = formula_bundle(now);
    let bundle_path = directory.join("bundle.json");
    let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);
    let request = intake(
        &signed,
        &trusted,
        &policy(&signed.envelope),
        now,
        &directory,
        &mut adapter,
    )
    .expect("human-approved Polymarket Formula LiveSmall");
    assert_eq!(request.mode, ActivationMode::LiveSmall);
    drop(adapter);
    assert!(!config.quotes_only);
    assert_eq!(config.venues[0].execution_mode.as_deref(), Some("Live"));
    assert_eq!(config.strategies.len(), 1);
    assert_eq!(config.engine.intent_max_slippage_bps, Some(5));
    assert_eq!(
        config.engine.intent_max_order_notional,
        Some(rust_decimal::Decimal::from(100))
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn live_small_rejects_other_venues_and_onnx_artifacts() {
    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("live-small-boundary");

    let formula = formula_bundle(now);
    let mut unsigned = envelope(
        now,
        "live-small-binance",
        "nonce-live-small-binance",
        AllowedIntentType::StartLiveSmall,
        ApprovalClass::HumanApprovedLiveSmall,
    );
    unsigned.venue = "polymarket-main".to_string();
    unsigned.instruments = vec!["123456789".to_string()];
    let non_polymarket = sign_envelope(unsigned, "key-1", &key).unwrap();
    let mut binance_config = configured_runtime();
    binance_config.venues[0].name = "polymarket-main".to_string();
    binance_config.venues[0].symbol_catalog[0].0 = "123456789@BINANCE".to_string();
    let formula_bundle_path = directory.join("formula-bundle.json");
    let mut binance_adapter =
        SystemConfigActivationAdapter::new(&mut binance_config, &formula, &formula_bundle_path);
    let error = intake(
        &non_polymarket,
        &trusted,
        &policy(&non_polymarket.envelope),
        now,
        &directory,
        &mut binance_adapter,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("restricted to Polymarket"));

    let onnx = onnx_bundle(now, "model.onnx", 1, &"f".repeat(64), 4);
    let mut unsigned = envelope(
        now,
        "live-small-onnx",
        "nonce-live-small-onnx",
        AllowedIntentType::StartLiveSmall,
        ApprovalClass::HumanApprovedLiveSmall,
    );
    unsigned.asset_revision_id = onnx.candidate_id.clone();
    unsigned.bundle_id = onnx.bundle_id.clone();
    unsigned.bundle_hash = onnx.bundle_hash.clone();
    unsigned.venue = "polymarket-main".to_string();
    unsigned.instruments = vec!["123456789".to_string()];
    unsigned.allowed_intent_types = vec![
        AllowedIntentType::LoadModel,
        AllowedIntentType::StartLiveSmall,
    ];
    let onnx_request = sign_envelope(unsigned, "key-1", &key).unwrap();
    let mut polymarket_config = configured_polymarket_runtime();
    let onnx_bundle_path = directory.join("onnx-bundle.json");
    let mut onnx_adapter =
        SystemConfigActivationAdapter::new(&mut polymarket_config, &onnx, &onnx_bundle_path);
    let error = intake(
        &onnx_request,
        &trusted,
        &policy(&onnx_request.envelope),
        now,
        &directory,
        &mut onnx_adapter,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("requires a Formula artifact"));

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn deployment_slippage_requires_a_finite_integer_bps() {
    let now = Utc::now();
    let bundle = formula_bundle(now);
    let directory = directory("slippage-bps-validation");
    let bundle_path = directory.join("bundle.json");

    for value in [0.0, 1.5, 10_001.0, f64::INFINITY, f64::NAN] {
        let mut config = configured_runtime();
        let request = ActivationRequest {
            deployment_id: "slippage-validation".to_string(),
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
            max_slippage_bps: value,
        };
        let error = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path)
            .activate(&request)
            .unwrap_err();
        assert!(error.contains("finite integer in 1..=10000"));
        assert_eq!(config.engine.intent_max_slippage_bps, None);
        assert_eq!(config.engine.intent_max_order_notional, None);
    }

    for value in [1.0, 10_000.0] {
        let mut config = configured_runtime();
        let request = ActivationRequest {
            deployment_id: "slippage-boundary".to_string(),
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
            max_slippage_bps: value,
        };
        SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path)
            .activate(&request)
            .expect("inclusive slippage boundary");
        assert_eq!(config.engine.intent_max_slippage_bps, Some(value as i32));
        assert_eq!(
            config.engine.intent_max_order_notional,
            Some(rust_decimal::Decimal::from(100))
        );
    }

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
        let bundle = onnx_bundle(now, "model.onnx", bytes.len() as u64, &checksum, channels);
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

        assert!(intake(
            &signed,
            &trusted,
            &policy(&signed.envelope),
            now,
            &directory,
            &mut adapter,
        )
        .is_err());
        drop(adapter);
        assert!(config.strategies.is_empty());
    }

    let checksum = hex::encode(Sha256::digest(bytes));
    let bundle = onnx_bundle(now, "model.onnx", bytes.len() as u64, &checksum, 4);
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
    let (_, reservation) = prepare_intake(
        &signed,
        &trusted,
        &policy(&signed.envelope),
        now,
        &directory,
        &mut adapter,
    )
    .unwrap();
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

#[test]
fn onnx_handoff_rejects_absolute_file_and_parent_paths() {
    use sha2::{Digest, Sha256};

    let now = Utc::now();
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let trusted = trusted(&key);
    let directory = directory("onnx-containment");
    let bundle_directory = directory.join("bundle");
    std::fs::create_dir_all(&bundle_directory).unwrap();
    let outside_path = directory.join("outside.onnx");
    let bytes = b"outside bundle";
    std::fs::write(&outside_path, bytes).unwrap();
    let checksum = hex::encode(Sha256::digest(bytes));

    for (suffix, uri) in [
        ("parent", "../outside.onnx".to_string()),
        ("absolute", outside_path.to_string_lossy().into_owned()),
        (
            "file-uri",
            format!("file://{}", outside_path.to_string_lossy()),
        ),
    ] {
        let bundle = onnx_bundle(now, &uri, bytes.len() as u64, &checksum, 4);
        let mut unsigned = envelope(
            now,
            &format!("onnx-containment-{suffix}"),
            &format!("nonce-onnx-containment-{suffix}"),
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
        let bundle_path = bundle_directory.join(format!("bundle-{suffix}.json"));
        let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, &bundle_path);

        let error = intake(
            &signed,
            &trusted,
            &policy(&signed.envelope),
            now,
            &directory,
            &mut adapter,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("bundle-relative") || error.contains("bundle directory"),
            "unexpected containment error: {error}"
        );
        drop(adapter);
        assert!(config.strategies.is_empty());
    }

    std::fs::remove_dir_all(directory).unwrap();
}
