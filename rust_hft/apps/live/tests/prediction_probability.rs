#![cfg(feature = "probability-reversal-strategy")]
use alpha_domain::{StrategyBundle, StrategyBundleArtifact};
use chrono::{Duration, Utc};
use governance::{
    deployment_scope_hash, sign_envelope, AllowedIntentType, ApprovalClass, DeploymentEnvelope,
    RuntimeApprovalEvidence, RuntimeEnvelopePolicy,
};
use hft_live::deployment_envelope::{
    ActivationMode, DeploymentIntake, RuntimeAuditLog, RuntimeNonceLedger,
    SystemConfigActivationAdapter,
};
use hft_research_manifest::ManifestId;
use rust_decimal::Decimal;
use std::collections::BTreeMap;

fn specification() -> hft_research_manifest::prediction_probability::ProbabilityReversalSpecV1 {
    use hft_research_manifest::prediction_probability::*;
    let now = hft_core::now_micros();
    ProbabilityReversalSpecV1 {
        schema: PROBABILITY_REVERSAL_SCHEMA.into(),
        episodes: vec![BinaryEpisodeV1 {
            episode_id: "episode".into(),
            condition_id: "condition".into(),
            underlying: "BTCUSDT".into(),
            venue: "POLYMARKET".into(),
            up_token: "123".into(),
            down_token: "456".into(),
            start_us: now - 1_000_000,
            end_us: now + 3_000_000,
        }],
        prev_prob_low: 0.3,
        curr_prob_high: 0.6,
        prev_prob_high: 0.7,
        curr_prob_low: 0.4,
        take_profit_prob: 0.85,
        stop_loss_prob: 0.5,
        min_time_remaining_secs: 1,
        max_time_remaining_secs: 5,
        stake_usd: Decimal::from(10),
        max_positions: 1000,
        max_daily_trades: 1000,
        quote_max_age_us: 500_000,
    }
}

#[tokio::test]
async fn signed_paper_and_shadow_consume_exact_fixed_probability_config() {
    for (intent, class, expected) in [
        (
            AllowedIntentType::StartPaper,
            ApprovalClass::Paper,
            ActivationMode::Paper,
        ),
        (
            AllowedIntentType::StartShadow,
            ApprovalClass::Shadow,
            ActivationMode::Shadow,
        ),
    ] {
        let now = Utc::now();
        let fixed = specification();
        // Synthetic admission evidence tests runtime binding, not real scientific promotion.
        let scientific = StrategyBundle::new(
            "probability-bundle".into(),
            "probability-candidate".into(),
            "1".repeat(64),
            ManifestId::new("fixture-dataset").unwrap(),
            "probability-fixture-not-science".into(),
            "2".repeat(64),
            "3".repeat(64),
            "4".repeat(64),
            "5".repeat(64),
            StrategyBundleArtifact::ProbabilityReversal {
                spec: Box::new(fixed.clone()),
            },
            now,
        )
        .unwrap();
        let bundle=scientific.to_runtime_bundle().unwrap();
        assert_eq!(bundle.source_bundle_hash,scientific.bundle_hash);
        let envelope = DeploymentEnvelope {
            deployment_id: "paper-probability".into(),
            asset_revision_id: bundle.candidate_id.clone(),
            promotion_id: "fixture-promotion".into(),
            promotion_manifest_hash: "6".repeat(64),
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            runtime_config_hash: "7".repeat(64),
            risk_policy_hash: "8".repeat(64),
            account_id: "paper-account".into(),
            venue: "POLYMARKET".into(),
            instruments: vec!["123".into(), "456".into()],
            allowed_intent_types: vec![AllowedIntentType::LoadProbabilityReversal, intent.clone()],
            max_notional: 100.0,
            max_symbol_exposure: 50.0,
            max_order_size: 5.0,
            max_slippage_bps: 25.0,
            valid_from: now - Duration::seconds(1),
            expires_at: now + Duration::minutes(5),
            nonce: "fixture-nonce".into(),
            approval_class: class.clone(),
            approval_signatures: vec!["fixture-approval".into()],
            payload_hash: String::new(),
        };
        let key = ed25519_dalek::SigningKey::from_bytes(&[47; 32]);
        let signed = sign_envelope(envelope.clone(), "fixture-signer", &key).unwrap();
        let policy = RuntimeEnvelopePolicy {
            account_id: envelope.account_id.clone(),
            venue: envelope.venue.clone(),
            allowed_instruments: envelope.instruments.clone(),
            allowed_intent_types: vec![AllowedIntentType::LoadProbabilityReversal, intent],
            runtime_config_hash: envelope.runtime_config_hash.clone(),
            risk_policy_hash: envelope.risk_policy_hash.clone(),
            max_notional: 1000.0,
            max_symbol_exposure: 500.0,
            max_order_size: 100.0,
            max_slippage_bps: 100.0,
            approvals: vec![RuntimeApprovalEvidence {
                approval_id: "fixture-approval".into(),
                approval_class: class,
                subject_id: envelope.promotion_id.clone(),
                scope_hash: deployment_scope_hash(&envelope).unwrap(),
                signer_id: "human-fixture".into(),
                valid_from: envelope.valid_from,
                expires_at: envelope.expires_at,
                revoked_at: None,
            }],
        };
        let root = tempfile::tempdir().unwrap();
        let mut config:runtime::SystemConfig=serde_json::from_value(serde_json::json!({"engine":{},"venues":[{"name":"POLYMARKET","account_id":"paper-account","venue_type":"Polymarket","ws_public":null,"ws_private":null,"rest":null,"api_key":null,"secret":null,"passphrase":null,"execution_mode":"Paper","capabilities":{"ws_order_placement":false,"snapshot_crc":false,"all_in_one_topics":false,"private_ws_heartbeat":false},"symbol_catalog":["123@POLYMARKET","456@POLYMARKET"]}],"strategies":[],"risk":{"risk_type":"Default","global_position_limit":"100","global_notional_limit":"1000"},"quotes_only":true,"router":null,"infra":null})).unwrap();
        config.risk.max_daily_trades = 100;
        config.risk.max_orders_per_second = 100;
        config.risk.staleness_threshold_us = 10_000_000;
        let keys = BTreeMap::from([("fixture-signer".into(), key.verifying_key())]);
        let mut adapter = SystemConfigActivationAdapter::new(&mut config, &bundle, root.path());
        let ledger = RuntimeNonceLedger::open(root.path().join("nonce.jsonl")).unwrap();
        let audit = RuntimeAuditLog::open(root.path().join("audit.jsonl")).unwrap();
        let intake = DeploymentIntake::new(&keys, &policy, false, ledger, audit, &mut adapter);
        let (request, reservation) = intake.prepare(&signed, now).unwrap();
        assert_eq!(request.mode, expected);
        drop(reservation);
        drop(adapter);
        let runtime::StrategyParams::ProbabilityReversal {
            spec,
            max_order_notional,
            max_order_quantity,
        } = &config.strategies[0].params
        else {
            panic!("fixed probability configuration not consumed")
        };
        assert_eq!(**spec, fixed);
        assert_eq!(*max_order_notional, Decimal::from(50));
        assert_eq!(*max_order_quantity, Decimal::from(5));
        let runtime = runtime::SystemBuilder::new(config.clone())
            .register_strategies_from_config_strict()
            .unwrap()
            .register_simulated_execution_client(hft_core::VenueId::POLYMARKET)
            .build();
        let (queues, mut reader) =
            engine::create_execution_queues(engine::ExecutionQueueConfig::default());
        let mut engine = runtime.engine.lock().await;
        engine.set_execution_queues(queues);
        engine.update_cash_balance(Decimal::from(1000)).unwrap();
        let ingester = engine.create_event_ingester_pair();
        let received = hft_core::now_micros();
        for (sequence, ask) in [(1, 25), (2, 65)] {
            ingester
                .lock()
                .unwrap()
                .ingest(ports::MarketEvent::Snapshot(ports::MarketSnapshot {
                    symbol: hft_core::Symbol::new("123"),
                    timestamp: received + sequence,
                    bids: vec![ports::BookLevel {
                        price: hft_core::Price(Decimal::new(ask - 2, 2)),
                        quantity: hft_core::Quantity(Decimal::from(100)),
                    }],
                    asks: vec![ports::BookLevel {
                        price: hft_core::Price(Decimal::new(ask, 2)),
                        quantity: hft_core::Quantity(Decimal::from(100)),
                    }],
                    sequence,
                    source_venue: Some(hft_core::VenueId::POLYMARKET),
                    timestamps: hft_core::MarketDataTimestamps::local_only(
                        hft_core::LocalReceiveTimestamp::new(received + sequence),
                    ),
                    provider_identity: Some(ports::ProviderBookIdentity {
                        market: "condition".into(),
                        book_hash: None,
                    }),
                }))
                .unwrap();
            engine.tick().unwrap();
        }
        let admitted = reader.receive_envelopes();
        assert_eq!(admitted.len(), 1);
        assert_eq!(
            admitted[0].intent.asset_class,
            hft_core::AssetClass::PredictionMarket
        );
        assert_eq!(
            admitted[0].account_id,
            Some(hft_core::AccountId("paper-account".into()))
        );
        assert_eq!(
            admitted[0].intent.target_venue,
            Some(hft_core::VenueId::POLYMARKET)
        );
        assert_eq!(
            admitted[0].intent.quantity,
            hft_core::Quantity(Decimal::from(5))
        );
        assert_eq!(
            admitted[0].lifecycle.max_order_quantity,
            Some(Decimal::from(5))
        );
        assert!(admitted[0].lifecycle.valid_until <= fixed.episodes[0].end_us);
        drop(engine);
        if expected == ActivationMode::Paper {
            for case in 0..4 {
                let mut rejected = envelope.clone();
                rejected.nonce = format!("rejected-{case}");
                match case {
                    0 => rejected.instruments = vec!["123".into(), "999".into()],
                    1 => {
                        rejected.allowed_intent_types =
                            vec![AllowedIntentType::LoadFactor, AllowedIntentType::StartPaper]
                    }
                    2 => {
                        rejected.allowed_intent_types = vec![
                            AllowedIntentType::LoadProbabilityReversal,
                            AllowedIntentType::StartLiveSmall,
                        ];
                        rejected.approval_class = ApprovalClass::HumanApprovedLiveSmall;
                    }
                    3 => {
                        rejected.valid_from = now - Duration::minutes(2);
                        rejected.expires_at = now - Duration::minutes(1);
                    }
                    _ => unreachable!(),
                }
                let mut rejection_policy = policy.clone();
                rejection_policy.allowed_instruments = rejected.instruments.clone();
                rejection_policy.allowed_intent_types = rejected.allowed_intent_types.clone();
                rejection_policy.approvals[0].scope_hash =
                    deployment_scope_hash(&rejected).unwrap();
                rejection_policy.approvals[0].approval_class = rejected.approval_class.clone();
                rejection_policy.approvals[0].valid_from = rejected.valid_from;
                rejection_policy.approvals[0].expires_at = rejected.expires_at;
                let signed = sign_envelope(rejected, "fixture-signer", &key).unwrap();
                let root = tempfile::tempdir().unwrap();
                let mut rejected_config = config.clone();
                rejected_config.strategies.clear();
                // Let the ordinary venue catalog admit the foreign token so the
                // fixed episode guard itself must reject it.
                rejected_config.venues[0]
                    .symbol_catalog
                    .push(serde_json::from_value(serde_json::json!("999@POLYMARKET")).unwrap());
                let mut adapter =
                    SystemConfigActivationAdapter::new(&mut rejected_config, &bundle, root.path());
                let ledger = RuntimeNonceLedger::open(root.path().join("nonce.jsonl")).unwrap();
                let audit = RuntimeAuditLog::open(root.path().join("audit.jsonl")).unwrap();
                let intake = DeploymentIntake::new(
                    &keys,
                    &rejection_policy,
                    false,
                    ledger,
                    audit,
                    &mut adapter,
                );
                assert!(
                    intake.prepare(&signed, now).is_err(),
                    "case {case} must fail closed"
                );
                drop(adapter);
                assert!(rejected_config.strategies.is_empty());
            }
        }
    }
}
