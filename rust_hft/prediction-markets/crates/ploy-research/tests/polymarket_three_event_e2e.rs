#![cfg(feature = "db")]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use ploy_market_data::polymarket_evidence::{
    aggregate_verified_polymarket_evidence_for_symbols, seal_polymarket_evidence_triplet,
    verify_polymarket_evidence, PolymarketCatalogReason, PolymarketCatalogReceipt,
    PolymarketCatalogReceiptState, PolymarketCatalogVerifier, PolymarketEvidenceTriplet,
    PolymarketEvidenceTrustAnchor, PolymarketReadyEventCatalog, PolymarketResearchTask,
};
use ploy_research::prediction_loop::{
    current_prediction_policy_snapshot_id, PredictionSearchBudget,
};
use ploy_research::prediction_mcts_authenticated::{
    read_authenticated_prediction_experiment_manifest,
    read_authenticated_prediction_result_receipt,
    write_authenticated_prediction_experiment_manifest, AuthenticatedPredictionResultReceiptRef,
    AuthenticatedTaskMetrics,
};
use ploy_research::prediction_mission_v3::{
    PredictionAuthorityProfile, PredictionMissionCapability, PredictionMissionTask,
    PredictionProductIdentity, PredictionProductSymbol, PredictionResearchMissionV3,
    PredictionRunMode, PredictionTaskKind, PredictionTokenSide,
    PREDICTION_MISSION_V3_SCHEMA_VERSION,
};
use ploy_research::research_snapshot::{
    admit_cached_authenticated_research_snapshot, ResearchSnapshotInputArtifact,
};
use ploy_research::{
    admit_extracted_authenticated_research_snapshot, authenticate_ready_event_cohort,
    build_research_snapshot_from_polymarket_chainlink_baseline,
    materialize_authenticated_research_snapshot, read_catalog_partition_artifact,
    write_catalog_partition_artifact, AuthenticatedPartitionView, AuthenticatedResearchSnapshot,
    AuthenticatedSnapshotMaterializationRequest, EventCohortPartition, ResearchSnapshot,
    VerifiedArtifactSnapshotBuildOptions,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

struct EventFixture {
    market_id: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    down_token_id: String,
    up_bids: [f64; 3],
    triplet: PolymarketEvidenceTriplet,
    trust: PolymarketEvidenceTrustAnchor,
    verified_triplet: PolymarketEvidenceTriplet,
    verified_trust: PolymarketEvidenceTrustAnchor,
    qualification: Value,
    qualification_path: PathBuf,
    qualification_sha256: String,
}

#[test]
fn producer_snapshot_smoke_and_three_task_receipts_share_one_partition() {
    let temp = tempfile::tempdir().expect("create fixture root");
    let root = fs::canonicalize(temp.path()).expect("canonical fixture root");
    let first: DateTime<Utc> = "2026-07-17T05:30:00Z".parse().unwrap();
    let events = [
        event_fixture(&root, "train", first),
        event_fixture(&root, "crossing", first + Duration::minutes(5)),
        event_fixture(&root, "held-out", first + Duration::minutes(10)),
    ];
    let verifier = PolymarketCatalogVerifier::new(
        "d".repeat(64),
        "e".repeat(64),
        "f".repeat(64),
        current_prediction_policy_snapshot_id()
            .strip_prefix("sha256:")
            .unwrap()
            .to_string(),
    )
    .unwrap();

    assert_producer_counterexamples(&events[0], &verifier);

    let mut catalog = PolymarketReadyEventCatalog::default();
    for event in &events {
        let receipt = append_event(&mut catalog, event, verifier.clone());
        assert_eq!(receipt.state, PolymarketCatalogReceiptState::Ready);
    }
    let ready = catalog.ready_for(PolymarketResearchTask::Btc5mBacktest);
    assert_eq!(ready.len(), 3);

    let boundary = events[1].start + Duration::seconds(150);
    let partition =
        EventCohortPartition::from_ready_catalog(&catalog, boundary.timestamp_millis()).unwrap();
    assert_eq!(partition.train_market_ids(), [events[0].market_id.clone()]);
    assert_eq!(
        partition.crossing_excluded_market_ids(),
        [events[1].market_id.clone()]
    );
    assert_eq!(
        partition.held_out_market_ids(),
        [events[2].market_id.clone()]
    );

    let partition_ref = write_catalog_partition_artifact(
        &root,
        &root.join("catalog-partition"),
        &catalog,
        &partition,
    )
    .unwrap();
    let persisted = read_catalog_partition_artifact(&root, &partition_ref).unwrap();
    let cohort = authenticate_ready_event_cohort(persisted.catalog(), persisted.partition())
        .expect("authenticate persisted ready cohort");
    let request = AuthenticatedSnapshotMaterializationRequest {
        cache_root: root.join("snapshot-cache"),
        compiler_source_identity: format!("sha256:{}", "1".repeat(64)),
        compiler_image_identity: format!("sha256:{}", "2".repeat(64)),
        build_input_identity: format!("sha256:{}", "3".repeat(64)),
    };
    let snapshot_body = research_snapshot(&events, &catalog);
    let snapshot =
        materialize_authenticated_research_snapshot(&cohort, &request, || Ok(snapshot_body))
            .expect("materialize authenticated three-event snapshot");
    let readback = admit_cached_authenticated_research_snapshot(&cohort, &request)
        .expect("fresh authenticated snapshot readback");
    assert_eq!(
        readback.snapshot_contract_id(),
        snapshot.snapshot_contract_id()
    );
    assert_eq!(readback.snapshot_hash(), snapshot.snapshot_hash());
    assert_eq!(readback.partition_digest(), partition.digest());
    assert_eq!(
        readback.partition_view().common_time_boundary_ms(),
        boundary.timestamp_millis()
    );
    let mut forged_view = serde_json::to_value(snapshot.partition_view()).unwrap();
    forged_view["held_out_market_ids"]
        .as_array_mut()
        .unwrap()
        .push(json!("forged-market"));
    assert!(admit_extracted_authenticated_research_snapshot(
        snapshot.snapshot_dir(),
        snapshot.cohort_manifest_id(),
        snapshot.partition_digest(),
        snapshot.causal_projection_policy_id(),
        serde_json::from_value(forged_view).unwrap(),
        snapshot.snapshot_contract_id(),
        snapshot.snapshot_hash(),
    )
    .is_err());
    let runtime_snapshot = admit_extracted_authenticated_research_snapshot(
        snapshot.snapshot_dir(),
        snapshot.cohort_manifest_id(),
        snapshot.partition_digest(),
        snapshot.causal_projection_policy_id(),
        serde_json::from_slice::<AuthenticatedPartitionView>(
            &serde_json::to_vec(snapshot.partition_view()).unwrap(),
        )
        .unwrap(),
        snapshot.snapshot_contract_id(),
        snapshot.snapshot_hash(),
    )
    .expect("re-admit the exact serialized runtime partition view");

    assert_pipeline_smoke_completed(&root, &snapshot);

    let output = root.join("research-trial");
    let immutable_image_identity = format!("sha256:{}", "4".repeat(64));
    let mut receipts = Vec::new();
    let mut receipt_refs = Vec::new();
    for kind in [
        PredictionTaskKind::SettlementProbability,
        PredictionTaskKind::UpExecution,
        PredictionTaskKind::DownExecution,
    ] {
        let mission = mission(&runtime_snapshot, kind, PredictionRunMode::ResearchTrial);
        let mission_path = root.join(format!("research-trial-{kind:?}.json").to_ascii_lowercase());
        fs::write(&mission_path, serde_json::to_vec_pretty(&mission).unwrap()).unwrap();
        let process = Command::new(env!("CARGO_BIN_EXE_monday-prediction-research"))
            .arg("--research-trial")
            .arg(&mission_path)
            .arg(runtime_snapshot.snapshot_dir())
            .arg(&output)
            .arg("--admitted-cohort-manifest-id")
            .arg(runtime_snapshot.cohort_manifest_id())
            .arg("--admitted-partition-digest")
            .arg(runtime_snapshot.partition_digest())
            .arg("--admitted-policy-identity")
            .arg(runtime_snapshot.causal_projection_policy_id())
            .arg("--admitted-snapshot-contract-id")
            .arg(runtime_snapshot.snapshot_contract_id())
            .arg("--admitted-snapshot-digest")
            .arg(runtime_snapshot.snapshot_hash())
            .arg("--admitted-partition-view-json")
            .arg(serde_json::to_string(runtime_snapshot.partition_view()).unwrap())
            .arg("--immutable-image-identity")
            .arg(&immutable_image_identity)
            .output()
            .expect("run production research trial binary");
        assert!(
            process.status.success(),
            "bounded authenticated {kind:?} research trial: {}",
            String::from_utf8_lossy(&process.stderr)
        );
        let summary: Value = serde_json::from_slice(&process.stdout).unwrap();
        assert_eq!(summary["status"], "completed");
        let receipt_ref: AuthenticatedPredictionResultReceiptRef = serde_json::from_value(json!({
            "path": summary["receipt_path"],
            "artifact_sha256": summary["receipt_artifact_sha256"],
            "receipt_sha256": summary["receipt_sha256"],
        }))
        .unwrap();
        let receipt = read_authenticated_prediction_result_receipt(&output, &receipt_ref).unwrap();
        assert_eq!(receipt.mission.partition_digest, partition.digest());
        assert_eq!(
            metric_event_count(&receipt.metrics),
            1,
            "crossing event leaked"
        );
        assert_rehashes(
            &output.join(receipt_ref.path()),
            receipt_ref.artifact_sha256(),
        );
        receipts.push(receipt);
        receipt_refs.push(receipt_ref);
    }
    let AuthenticatedTaskMetrics::Settlement(settlement) = &receipts[0].metrics else {
        unreachable!("first task is settlement")
    };
    let expected_held_out_brier = events[2]
        .up_bids
        .iter()
        .map(|bid| (bid + 0.01 - 1.0).powi(2))
        .sum::<f64>()
        / 3.0;
    let crossing_brier = events[1]
        .up_bids
        .iter()
        .map(|bid| (bid + 0.01 - 1.0).powi(2))
        .sum::<f64>()
        / 3.0;
    assert!((settlement.mean_brier_score - expected_held_out_brier).abs() < 1e-12);
    assert!((settlement.mean_brier_score - crossing_brier).abs() > 0.1);
    assert!(receipt_refs[0].path().contains("/settlement/"));
    assert!(receipt_refs[1].path().contains("/up-execution-10s/"));
    assert!(receipt_refs[2].path().contains("/down-execution-10s/"));
    assert_eq!(
        receipt_refs
            .iter()
            .map(|receipt| receipt.path())
            .collect::<BTreeSet<_>>()
            .len(),
        3,
        "task checkpoint/result namespaces were shared"
    );

    let manifest_ref =
        write_authenticated_prediction_experiment_manifest(&output, &receipt_refs).unwrap();
    let manifest =
        read_authenticated_prediction_experiment_manifest(&output, &manifest_ref).unwrap();
    assert_eq!(manifest.partition_digest, partition.digest());
    assert_eq!(
        manifest.snapshot_contract_id,
        snapshot.snapshot_contract_id()
    );
    assert_rehashes(
        &output.join(manifest_ref.path()),
        manifest_ref.artifact_sha256(),
    );

    let snapshot_manifest = snapshot.snapshot_dir().join("manifest.json");
    fs::set_permissions(&snapshot_manifest, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&snapshot_manifest, b"{}\n").unwrap();
    let rejection = admit_cached_authenticated_research_snapshot(&cohort, &request)
        .expect_err("mutated snapshot cache must fail closed");
    assert_eq!(rejection.code(), "corrupt_cached_snapshot");
}

fn event_fixture(root: &Path, suffix: &str, start: DateTime<Utc>) -> EventFixture {
    let end = start + Duration::minutes(5);
    let market_id = format!("btc-5m-{suffix}");
    let condition_id = format!("condition-{suffix}");
    let up_token_id = format!("up-{suffix}");
    let down_token_id = format!("down-{suffix}");
    let trade_id = format!("trade-{suffix}");
    let up_bids: [f64; 3] = match suffix {
        "train" => [0.20, 0.25, 0.30],
        "crossing" => [0.35, 0.40, 0.45],
        "held-out" => [0.60, 0.75, 0.80],
        _ => unreachable!("unexpected fixture event"),
    };
    let context = json!({
        "schema":"monday.polymarket.evidence_row.v1", "market_id":market_id,
        "condition_id":condition_id, "symbol":"BTCUSDT", "event_start":timestamp(start),
        "event_end":timestamp(end), "window_secs":300
    });
    let row = |surface: &str, fields: Value| {
        let mut value = context.clone();
        value["surface"] = json!(surface);
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        value
    };
    let mut rows = vec![row(
        "market_contract",
        json!({"source_token_ids":[down_token_id,up_token_id],"source_outcomes":["Down","Up"],"price_to_beat":"63000","resolution_source":"https://data.chain.link/streams/btc-usd","metadata_retrieved_at":timestamp(start-Duration::seconds(2)),"discovery_recorded_at":timestamp(start-Duration::seconds(3)),"metadata_recorded_at":timestamp(start-Duration::seconds(1)),"available_at":timestamp(start-Duration::seconds(1)),"discovery_source_sequence":1,"metadata_source_sequence":2,"source_datasets":["crypto_expiry","crypto_expiry_reference"]}),
    )];
    rows.extend([
        row(
            "chainlink_reference",
            json!({"source":"chainlink","asset_class":"crypto","source_symbol":"btc/usd","price":"63000","full_accuracy_value":null,"is_carried_forward":false,"ts":timestamp(start-Duration::seconds(2)),"received_at":timestamp(start-Duration::seconds(1)),"available_at":timestamp(start-Duration::seconds(1)),"recorded_at":timestamp(start-Duration::seconds(1)),"source_sequence":3,"source_dataset":"crypto_expiry"}),
        ),
        row(
            "chainlink_reference",
            json!({"source":"chainlink","asset_class":"crypto","source_symbol":"btc/usd","price":"63000","full_accuracy_value":null,"is_carried_forward":false,"ts":timestamp(start),"received_at":timestamp(start+Duration::seconds(1)),"available_at":timestamp(start+Duration::seconds(1)),"recorded_at":timestamp(start+Duration::seconds(1)),"source_sequence":4,"source_dataset":"crypto_expiry"}),
        ),
    ]);
    for (index, (offset, up_bid)) in [279, 289, 299].into_iter().zip(up_bids).enumerate() {
        let available_at = start + Duration::seconds(offset);
        let source_time = available_at - Duration::seconds(1);
        let up_ask = up_bid + 0.02;
        let down_bid = 1.0 - up_ask;
        let down_ask = 1.0 - up_bid;
        let sequence = [5, 8, 10][index];
        rows.extend([
            row(
                "orderbook_snapshot",
                json!({"token_id":down_token_id,"ts":timestamp(source_time),"recorded_at":timestamp(available_at),"available_at":timestamp(available_at),"source_sequence":sequence,"source_dataset":"crypto_expiry","bid":down_bid.to_string(),"ask":down_ask.to_string(),"bid_size":"100","ask_size":"100","bid_levels":[{"price":down_bid.to_string(),"size":"100"}],"ask_levels":[{"price":down_ask.to_string(),"size":"100"}]}),
            ),
            row(
                "orderbook_snapshot",
                json!({"token_id":up_token_id,"ts":timestamp(source_time),"recorded_at":timestamp(available_at),"available_at":timestamp(available_at),"source_sequence":sequence+1,"source_dataset":"crypto_expiry","bid":up_bid.to_string(),"ask":up_ask.to_string(),"bid_size":"100","ask_size":"100","bid_levels":[{"price":up_bid.to_string(),"size":"100"}],"ask_levels":[{"price":up_ask.to_string(),"size":"100"}]}),
            ),
        ]);
        if index == 0 {
            rows.push(row(
                "chainlink_reference",
                json!({"source":"chainlink","asset_class":"crypto","source_symbol":"btc/usd","price":"63000","full_accuracy_value":null,"is_carried_forward":false,"ts":timestamp(source_time),"received_at":timestamp(available_at),"available_at":timestamp(available_at),"recorded_at":timestamp(available_at),"source_sequence":7,"source_dataset":"crypto_expiry"}),
            ));
        }
    }
    let trade_time = end - Duration::seconds(1);
    rows.extend([
        row(
            "polymarket_trade",
            json!({"record_id":trade_id,"record_id_version":"v2","token_id":up_token_id,"source_outcome":"Up","outcome_index":1,"side":"BUY","size":"2","price":"0.6","trade_ts":timestamp(trade_time),"trade_ts_unix":trade_time.timestamp(),"transaction_hash":format!("0x{suffix}"),"proxy_wallet":"0xfixture","source":"polymarket_data_api","received_at":timestamp(end),"available_at":timestamp(end+Duration::seconds(1)),"recorded_at":timestamp(end+Duration::seconds(1)),"source_sequence":12,"source_dataset":"crypto_expiry_reference"}),
        ),
        row(
            "official_settlement_evidence",
            json!({"source_token_ids":[down_token_id,up_token_id],"source_outcomes":["Down","Up"],"source_outcome_prices":["0","1"],"winning_token_id":up_token_id,"winning_outcome":"Up","resolution_source":"gamma_api_closed_market","retrieved_at":timestamp(end+Duration::seconds(1)),"available_at":timestamp(end+Duration::seconds(2)),"recorded_at":timestamp(end+Duration::seconds(2)),"source_sequence":13,"source_dataset":"crypto_expiry_reference"}),
        ),
    ]);
    let mut data = Vec::new();
    for row in &rows {
        serde_json::to_writer(&mut data, row).unwrap();
        data.push(b'\n');
    }
    let content_sha256 = sha256(&data);
    let event_root = root.join(format!("event-{suffix}"));
    fs::create_dir(&event_root).unwrap();
    let artifact_root = event_root.join(format!("sha256={content_sha256}"));
    fs::create_dir(&artifact_root).unwrap();
    let name = format!("polymarket-candidate-{suffix}.{content_sha256}.ndjson");
    let triplet = PolymarketEvidenceTriplet {
        data: artifact_root.join(&name),
        manifest: artifact_root.join(format!("{name}.manifest.json")),
        success: artifact_root.join(format!("{name}._SUCCESS")),
    };
    let verified_name = format!("polymarket-verified-{suffix}.{content_sha256}.ndjson");
    let verified_triplet = PolymarketEvidenceTriplet {
        data: artifact_root.join(&verified_name),
        manifest: artifact_root.join(format!("{verified_name}.manifest.json")),
        success: artifact_root.join(format!("{verified_name}._SUCCESS")),
    };
    let trade_ids_sha256 = sha256(format!("{trade_id}\n").as_bytes());
    let segment = |dataset: &str, sample_ms: u64, reference: bool| {
        let (events, event_types, versions, completions) = if reference {
            (
                4,
                json!({"market_metadata":1,"polymarket_trade":1,"market_settlement":1,"polymarket_trade_collection_complete":1}),
                json!(["v2"]),
                json!({market_id.clone():{"condition_id":condition_id,"symbol":"BTCUSDT","market_window_secs":300,"trade_count":1,"trade_record_ids_sha256":trade_ids_sha256,"completion_sequence":4,"retrieved_at":timestamp(end+Duration::seconds(2)),"completeness_basis":"polymarket_data_api_exhausted_after_settlement_and_stable_polls_v1","finalization_lag_secs":60,"stable_polls_required":2}}),
            )
        } else {
            (
                9,
                json!({"quote":6,"reference_price":3}),
                json!([]),
                json!({}),
            )
        };
        json!({
            "schema":"monday.polymarket.raw.v1","venue":"polymarket","dataset":dataset,
            "date":start.format("%Y-%m-%d").to_string(),"hour":start.format("%H").to_string(),
            "file":format!("{dataset}.ndjson.zst"),"bytes":100,"sha256":"1".repeat(64),
            "events":events,"start_sequence":1,"end_sequence":events,"sequence_gaps":0,
            "start_recorded_at":timestamp(start-Duration::seconds(5)),
            "end_recorded_at":timestamp(end+Duration::seconds(3)),
            "source_file":format!("{dataset}.ndjson"),
            "replay_scope":if reference {"complete_reference_hour_segment"} else {"complete_full_depth_sampled_normalized_hour_segment"},
            "record_id_versions":versions,"event_types":event_types,
            "recording_policy":{"quote_sample_ms":sample_ms,"quote_depth_levels":0,"event_scoped_quotes":true},
            "trade_completions":completions
        })
    };
    let validated_inputs = json!({"schema":"monday.polymarket.research_segment_validation.v1","market":segment("crypto_expiry",1000,false),"reference":segment("crypto_expiry_reference",0,true)});
    let orderbook_semantics = json!({"level":"L2","depth":"full visible depth as received","quote_sample_ms":1000,"venue_depth_complete":true,"temporal_updates_complete":false,"l3_order_ids_available":false,"queue_position_modeled":false,"endogenous_impact_modeled":false,"capacity_modeled":false});
    let manifest = json!({
        "schema":"monday.polymarket.candidate_evidence_artifact.v1","file":name,"format":"ndjson",
        "content_sha256":content_sha256,"content_bytes":data.len(),"rows":rows.len(),"contract_rows":1,
        "surface_counts":{"down_book":3,"reference":3,"settlement":1,"trades":1,"up_book":3},
        "event_start_gte":timestamp(start),"event_start_lt":timestamp(end),"market_ids":[market_id],
        "symbols":["BTCUSDT"],"window_secs":300,
        "event_selection":"explicit market_ids constrained to [event_start_gte,event_start_lt)",
        "evidence_scope":"untrusted producer candidate only; not Ready, execution authorization, or evaluator labels",
        "content_digest_semantics":"content_sha256 binds the published NDJSON bytes only; it is not a snapshot_contract_hash",
        "recording_semantics":{"orderbook":orderbook_semantics,"trades":"canonical v2 records when present; a collector completion proof is verified when present but may be absent","references":"typed Chainlink BTC/USD or SOL/USD with source timestamp in [event_start - 30 seconds, event_end)","settlement":"gamma_api_closed_market closed-market evidence joined by exact market_id","availability_clock":"point-in-time rows expose the latest validated recorded or retrieved clock as available_at"},
        "trust_boundary":"untrusted producer candidate carrier only; not Ready, an execution authorization, evaluator labels, or a snapshot_contract_hash",
        "validated_inputs":validated_inputs
    });
    let verified_manifest = json!({
        "schema":"monday.polymarket.evidence_artifact.v3","file":verified_name,"format":"ndjson",
        "content_sha256":content_sha256,"content_bytes":data.len(),"rows":rows.len(),"events":1,
        "surface_counts":{"chainlink_reference":3,"market_contract":1,"official_settlement_evidence":1,"orderbook_snapshot":6,"polymarket_trade":1},
        "event_start_gte":timestamp(start),"event_start_lt":timestamp(end),"market_ids":[market_id],
        "symbols":["BTCUSDT"],"window_secs":300,
        "event_selection":"explicit market_ids constrained to [event_start_gte,event_start_lt)",
        "evidence_scope":"immutable collector evidence only; not an execution authorization or evaluator label artifact",
        "content_digest_semantics":"content_sha256 binds the published NDJSON bytes only; it is not a snapshot_contract_hash",
        "recording_semantics":{"orderbook":orderbook_semantics,"trades":"exact market_id association using canonical v2 records; selected event count and record IDs match a collector completion proof; trade_ts may fall outside the event lifetime","references":"typed Chainlink BTC/USD or SOL/USD with source timestamp in [event_start - 30 seconds, event_end)","settlement":"gamma_api_closed_market closed-market evidence joined by exact market_id","availability_clock":"point-in-time rows expose the latest validated recorded or retrieved clock as available_at"},
        "trust_boundary":"typed collector staging evidence only; not an evaluator label snapshot or snapshot_contract_hash; validated staged triplets and adjacent local supersession markers; omitted remote-prefix markers are not proven absent",
        "validated_inputs":validated_inputs
    });
    for (artifact, manifest) in [(&triplet, manifest), (&verified_triplet, verified_manifest)] {
        publish(&artifact.data, &data);
        publish(
            &artifact.manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()).as_bytes(),
        );
        publish(&artifact.success, format!("{content_sha256}\n").as_bytes());
    }
    let manifest_sha256 = digest_file(&triplet.manifest);
    let trust =
        PolymarketEvidenceTrustAnchor::from_lower_hex(&content_sha256, &manifest_sha256).unwrap();
    let verified_trust = PolymarketEvidenceTrustAnchor::from_lower_hex(
        &content_sha256,
        &digest_file(&verified_triplet.manifest),
    )
    .unwrap();
    let completed_at = end + Duration::seconds(3);
    let qualification = json!({
        "schema":"monday.polymarket.event_qualification.v1",
        "verifier_contract":"monday.polymarket.normalized_evidence.v2","market_id":market_id,
        "symbol":"BTCUSDT","event_start":timestamp(start),"event_end":timestamp(end),
        "up_token_id":up_token_id,"down_token_id":down_token_id,
        "verified_token_ids":[up_token_id,down_token_id],"state":"ready","reasons":[],"retry":false,
        "producer":{"source_sha":"a".repeat(40),"image_digest":format!("sha256:{}","b".repeat(64)),"configuration_sha256":"c".repeat(64)},
        "source_closed":true,"up_book":"complete","down_book":"complete","trades":"complete","reference":"complete","settlement":"complete",
        "request_outcomes":(["up_book","down_book","trades","reference","settlement"].into_iter().map(|surface|json!({"surface":surface,"status":"succeeded","completed_at":timestamp(completed_at)})).collect::<Vec<_>>()),
        "source_clocks":{"opened_at":timestamp(start-Duration::seconds(5)),"closed_at":timestamp(completed_at)},
        "sequence":{"start":1,"end":13,"gaps":0},
        "evidence_digests":{"expected_content_sha256":content_sha256,"expected_manifest_sha256":manifest_sha256},
        "token_identity_matches":true
    });
    let qualification_path = artifact_root.join("qualification-ready.json");
    let qualification_sha256 = publish_json(&qualification_path, &qualification);
    EventFixture {
        market_id,
        start,
        end,
        down_token_id,
        up_bids,
        triplet,
        trust,
        verified_triplet,
        verified_trust,
        qualification,
        qualification_path,
        qualification_sha256,
    }
}

fn append_event(
    catalog: &mut PolymarketReadyEventCatalog,
    event: &EventFixture,
    verifier: PolymarketCatalogVerifier,
) -> PolymarketCatalogReceipt {
    catalog
        .verify_and_append(
            &event.market_id,
            &event.triplet,
            &event.trust,
            &event.qualification_path,
            &event.qualification_sha256,
            verifier,
        )
        .unwrap()
        .clone()
}

fn assert_producer_counterexamples(event: &EventFixture, verifier: &PolymarketCatalogVerifier) {
    let artifact_root = event.triplet.data.parent().unwrap();
    let mut swapped = event.qualification.clone();
    swapped["up_token_id"] = json!(event.down_token_id);
    let swapped_path = artifact_root.join("qualification-swapped.json");
    let swapped_sha = publish_json(&swapped_path, &swapped);
    let mut catalog = PolymarketReadyEventCatalog::default();
    let receipt = catalog
        .verify_and_append(
            &event.market_id,
            &event.triplet,
            &event.trust,
            &swapped_path,
            &swapped_sha,
            verifier.clone(),
        )
        .unwrap();
    assert_eq!(receipt.state, PolymarketCatalogReceiptState::Rejected);
    assert!(receipt
        .reasons
        .contains(&PolymarketCatalogReason::QualificationMismatch));

    let mut missing = event.qualification.clone();
    missing["request_outcomes"].as_array_mut().unwrap().pop();
    let missing_path = artifact_root.join("qualification-missing-request.json");
    let missing_sha = publish_json(&missing_path, &missing);
    let mut catalog = PolymarketReadyEventCatalog::default();
    let receipt = catalog
        .verify_and_append(
            &event.market_id,
            &event.triplet,
            &event.trust,
            &missing_path,
            &missing_sha,
            verifier.clone(),
        )
        .unwrap();
    assert_eq!(receipt.state, PolymarketCatalogReceiptState::Rejected);

    let wrong_trust = PolymarketEvidenceTrustAnchor::from_lower_hex(
        &"0".repeat(64),
        &event.trust.expected_manifest_sha256(),
    )
    .unwrap();
    let mut catalog = PolymarketReadyEventCatalog::default();
    let receipt = catalog
        .verify_and_append(
            &event.market_id,
            &event.triplet,
            &wrong_trust,
            &event.qualification_path,
            &event.qualification_sha256,
            verifier.clone(),
        )
        .unwrap();
    assert_eq!(
        receipt.reasons,
        [PolymarketCatalogReason::EvidenceVerificationFailed]
    );
}

fn research_snapshot(
    events: &[EventFixture; 3],
    catalog: &PolymarketReadyEventCatalog,
) -> ResearchSnapshot {
    let verified = events
        .iter()
        .map(|event| {
            verify_polymarket_evidence(
                seal_polymarket_evidence_triplet(&event.verified_triplet, &event.verified_trust)
                    .unwrap(),
            )
            .unwrap()
        })
        .collect();
    let verified =
        aggregate_verified_polymarket_evidence_for_symbols(verified, &["BTCUSDT".to_string()])
            .unwrap();
    let mut snapshot = build_research_snapshot_from_polymarket_chainlink_baseline(
        &verified,
        VerifiedArtifactSnapshotBuildOptions {
            symbol: "BTCUSDT".to_string(),
            start: events[0].start,
            end: events[2].end,
            lob_sample_secs: 10,
            pm_book_sample_secs: 10,
            observation_sample_secs: 10,
            max_quote_age_secs: 30,
            stake_usd: 15.0,
            optimizer_data_dir: "three-event-ci-fixture".to_string(),
            git_sha: Some("three-event-ci-fixture".to_string()),
        },
    )
    .unwrap();
    let expected_ids = events
        .iter()
        .map(|event| event.market_id.as_str())
        .collect::<BTreeSet<_>>();
    let actual_ids = snapshot
        .observations
        .iter()
        .map(|row| row.event_id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(actual_ids, expected_ids);
    assert!(snapshot
        .observations
        .iter()
        .all(|row| row.chainlink_reference_fresh));
    for event in events {
        let rows = snapshot
            .observations
            .iter()
            .filter(|row| row.event_id == event.market_id)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        for ((row, offset), up_bid) in rows.iter().zip([279, 289, 299]).zip(event.up_bids) {
            assert_eq!(row.tick_ts, event.start + Duration::seconds(offset));
            assert!((row.pm_up_bid - up_bid).abs() < 1e-12);
        }
    }
    snapshot.manifest.input_artifacts.extend(
        catalog
            .ready_for(PolymarketResearchTask::Btc5mBacktest)
            .into_iter()
            .enumerate()
            .map(|(index, receipt)| ResearchSnapshotInputArtifact {
                name: format!("polymarket_evidence_catalog_{index:04}"),
                path: format!(
                    "verified+polymarket://sha256/{}/manifest/{}",
                    receipt.content_sha256, receipt.manifest_sha256
                ),
                content_hash: Some(format!("sha256:{}", receipt.content_sha256)),
                row_count: Some(12),
            }),
    );
    snapshot
}

fn mission(
    snapshot: &AuthenticatedResearchSnapshot,
    kind: PredictionTaskKind,
    run_mode: PredictionRunMode,
) -> PredictionResearchMissionV3 {
    let (side, prediction_horizon_secs) = match kind {
        PredictionTaskKind::SettlementProbability => (None, None),
        PredictionTaskKind::UpExecution => (Some(PredictionTokenSide::Up), Some(10)),
        PredictionTaskKind::DownExecution => (Some(PredictionTokenSide::Down), Some(10)),
    };
    let pipeline_smoke = run_mode == PredictionRunMode::PipelineSmoke;
    PredictionResearchMissionV3 {
        schema_version: PREDICTION_MISSION_V3_SCHEMA_VERSION.to_string(),
        mission_id: format!("three-event-{kind:?}-{run_mode:?}").to_ascii_lowercase(),
        product: PredictionProductIdentity {
            symbol: PredictionProductSymbol::Btc,
            event_horizon_secs: 300,
        },
        task: PredictionMissionTask {
            kind,
            side,
            prediction_horizon_secs,
        },
        run_mode,
        authority_profile: PredictionAuthorityProfile::PolymarketChainlinkBaseline,
        required_capabilities: BTreeSet::from([PredictionMissionCapability::PolymarketChainlink]),
        cohort_manifest_id: snapshot.cohort_manifest_id().to_string(),
        partition_digest: snapshot.partition_digest().to_string(),
        causal_projection_policy_id: snapshot.causal_projection_policy_id().to_string(),
        snapshot_contract_id: snapshot.snapshot_contract_id().to_string(),
        snapshot_hash: snapshot.snapshot_hash().to_string(),
        search_policy_snapshot_id: current_prediction_policy_snapshot_id(),
        search_budget: PredictionSearchBudget {
            max_candidates: usize::from(!pipeline_smoke),
            max_llm_calls: usize::from(!pipeline_smoke),
            max_seconds: 30,
        },
    }
}

fn assert_pipeline_smoke_completed(root: &Path, snapshot: &AuthenticatedResearchSnapshot) {
    let mission = mission(
        snapshot,
        PredictionTaskKind::SettlementProbability,
        PredictionRunMode::PipelineSmoke,
    );
    let mission_path = root.join("pipeline-smoke-mission.json");
    fs::write(&mission_path, serde_json::to_vec_pretty(&mission).unwrap()).unwrap();
    let output_dir = root.join("pipeline-smoke-output");
    let output = Command::new(env!("CARGO_BIN_EXE_monday-prediction-research"))
        .env(
            "MONDAY_PREDICTION_EVALUATOR_BIN",
            env!("CARGO_BIN_EXE_monday-prediction-evaluator"),
        )
        .arg("--pipeline-smoke")
        .arg(&mission_path)
        .arg(snapshot.snapshot_dir())
        .arg(&output_dir)
        .arg("--admitted-cohort-manifest-id")
        .arg(snapshot.cohort_manifest_id())
        .arg("--admitted-partition-digest")
        .arg(snapshot.partition_digest())
        .arg("--admitted-policy-identity")
        .arg(snapshot.causal_projection_policy_id())
        .arg("--admitted-snapshot-contract-id")
        .arg(snapshot.snapshot_contract_id())
        .arg("--admitted-snapshot-digest")
        .arg(snapshot.snapshot_hash())
        .arg("--admitted-partition-view-json")
        .arg(serde_json::to_string(snapshot.partition_view()).unwrap())
        .arg("--immutable-image-identity")
        .arg(format!("sha256:{}", "4".repeat(64)))
        .output()
        .expect("run production pipeline smoke binaries");
    assert!(
        output.status.success(),
        "pipeline smoke failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["status"], "completed");
    assert_eq!(summary["snapshot_digest"], snapshot.snapshot_hash());
    let digest = summary["evaluator_report_sha256"].as_str().unwrap();
    let reports = fs::read_dir(output_dir.join("reports"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(reports.len(), 1);
    assert_eq!(digest_file(&reports[0]), digest);
    assert!(reports[0]
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap()
        .ends_with(digest));
}

fn metric_event_count(metrics: &AuthenticatedTaskMetrics) -> usize {
    match metrics {
        AuthenticatedTaskMetrics::Settlement(metrics) => metrics.event_count,
        AuthenticatedTaskMetrics::UpExecution(metrics)
        | AuthenticatedTaskMetrics::DownExecution(metrics) => metrics.event_count,
    }
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn publish_json(path: &Path, value: &Value) -> String {
    publish(
        path,
        format!("{}\n", serde_json::to_string(value).unwrap()).as_bytes(),
    );
    digest_file(path)
}

fn publish(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
}

fn assert_rehashes(path: &Path, expected: &str) {
    assert_eq!(format!("sha256:{}", digest_file(path)), expected);
}

fn digest_file(path: &Path) -> String {
    sha256(&fs::read(path).unwrap())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
