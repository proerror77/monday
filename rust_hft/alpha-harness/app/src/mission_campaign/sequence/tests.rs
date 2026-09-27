use super::*;
use alpha_domain::{sequence_study::*, EvaluationCostsV1};
use hft_research_manifest::sequence::{SequenceInputSpecV1, SequenceViewV1};
use inputs::{Artifact, DatasetLocation, INPUTS_SCHEMA};

pub(crate) fn request() -> SequenceRequest {
    let day = 86_400_000;
    let mut folds = Vec::new();
    for fold_id in [1, 2] {
        let end = (16 + i64::from(fold_id) * 2) * day;
        for days in [7, 14] {
            let start = end - i64::from(days) * day;
            folds.push(SequenceFoldV1 {
                fold_id,
                training_window_days: days,
                train_dataset_sha256: "a".repeat(64),
                validation_dataset_sha256: "b".repeat(64),
                replay_manifest_sha256: "c".repeat(64),
                train: SequenceViewV1 {
                    history_start_ms: start,
                    decision_start_ms: start + 60_000,
                    end_ms: end,
                    decision_stride_ms: 300_000,
                },
                validation: SequenceViewV1 {
                    history_start_ms: end + 1000,
                    decision_start_ms: end + 61_000,
                    end_ms: end + day,
                    decision_stride_ms: 1000,
                },
            });
        }
    }
    let plan = SolSequenceStudyV1 {
        schema_version: SOL_SEQUENCE_STUDY_SCHEMA.into(),
        study_id: "sol-sequence-test".into(),
        symbol: "SOLUSDT".into(),
        input: SequenceInputSpecV1::sol_lob(),
        models: vec![
            SequenceStudyModelV1::Ridge,
            SequenceStudyModelV1::FlattenedMlp,
            SequenceStudyModelV1::PriceTcn,
            SequenceStudyModelV1::LobTcn,
        ],
        neural_seeds: vec![7, 11],
        folds,
        sealed_dataset_sha256: "d".repeat(64),
        sealed_view: SequenceViewV1 {
            history_start_ms: 23 * day,
            decision_start_ms: 23 * day + 60_000,
            end_ms: 24 * day,
            decision_stride_ms: 1000,
        },
        primary_horizon_ms: 30000,
        max_primary_fits: 30,
        max_verification_fits: 30,
        neural_updates: 128,
        batch_size: 32,
        hidden_channels: 2,
        learning_rate: 0.0003,
        max_training_examples: 4096,
        costs: EvaluationCostsV1 {
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: true,
            position_notional_usd: 100.0,
            capacity_depth_levels: 5,
            max_book_depth_fraction: 0.05,
        },
    };
    let artifact = |file: &str, hash: &str| Artifact {
        file: file.into(),
        sha256: hash.repeat(64),
    };
    let image = format!("registry/runner@sha256:{}", "e".repeat(64));
    let inputs = SequenceCampaignInputs {
        schema_version: INPUTS_SCHEMA.into(),
        producer_source_revision: BUILD_SOURCE_REVISION.into(),
        producer_image: image.clone(),
        pvc_name: "sol-inputs".into(),
        pvc_uid: "pvc-uid".into(),
        sub_path: "sol-sequence/fold-1-7".into(),
        fold_id: 1,
        training_window_days: 7,
        train: DatasetLocation {
            manifest: artifact("train.json", "a"),
            sources: artifact("train-sources.json", "f"),
        },
        validation: DatasetLocation {
            manifest: artifact("validation.json", "b"),
            sources: artifact("validation-sources.json", "f"),
        },
        replay_artifact: artifact("replay.parquet", "f"),
        replay_manifest: artifact("replay.json", "c"),
    };
    let mut request = SequenceRequest {
        schema_version:REQUEST_SCHEMA.into(),campaign_id:String::new(),build_source_revision:BUILD_SOURCE_REVISION.into(),image,image_identity:"e".repeat(64),plan,
        campaign_inputs_sha256:canonical_json_hash(&inputs).unwrap(),inputs,
        output_root:"https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns/sol-test".into(),
        result_put_url:String::new(),result_readback_url:String::new(),bundle_put_url:String::new(),bundle_readback_url:String::new(),
    };
    rebind(&mut request);
    request
}

pub(crate) fn rebind(request: &mut SequenceRequest) {
    request.campaign_inputs_sha256 = canonical_json_hash(&request.inputs).unwrap();
    request.campaign_id = request.expected_id().unwrap();
    request.result_put_url = format!(
        "{}/{}/result.json",
        request.output_root, request.campaign_id
    );
    request.result_readback_url = request.result_put_url.clone();
    request.bundle_put_url = format!(
        "{}/{}/results.zip",
        request.output_root, request.campaign_id
    );
    request.bundle_readback_url = request.bundle_put_url.clone();
}

#[test]
fn sequence_request_binds_inputs_and_only_allows_signed_query_changes() {
    let original = request();
    original.validate().unwrap();
    let mut changed = original.clone();
    changed.result_put_url.push_str("?signature=fixture");
    changed.validate().unwrap();
    assert_eq!(
        original.expected_id().unwrap(),
        changed.expected_id().unwrap()
    );
    changed.inputs.pvc_uid = "replaced-pvc".into();
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.plan.costs.fee_bps = 0.0;
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed.result_readback_url = changed
        .result_readback_url
        .replace("result.json", "another.json");
    assert!(changed.validate().is_err());
}

#[test]
fn sequence_artifacts_reject_escape_and_mismatched_content() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("input.json"), b"actual").unwrap();
    let mut artifact = Artifact {
        file: "../input.json".into(),
        sha256: "a".repeat(64),
    };
    assert!(artifact.path(root.path()).is_err());
    artifact.file = "input.json".into();
    assert!(artifact.read(root.path(), 1024).is_err());
    artifact.sha256 = format!("{:x}", Sha256::digest(b"actual"));
    assert_eq!(artifact.read(root.path(), 1024).unwrap(), b"actual");
    assert!(artifact.read(root.path(), 2).is_err());
}
