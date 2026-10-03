use super::{request_tests::request, *};
use crate::mission_campaign::sequence::{readback::verify_artifact_inventory, worker as shared};
use alpha_domain::{
    campaign_control::CampaignAttemptOutcomeV1,
    market_encoder_study::{
        MarketTrainingStageKindV1 as Kind, MarketTrainingStagePurposeV1 as Purpose,
    },
};
use alpha_engine::market_encoder_study::MarketPredictionCoverageV1;
use std::collections::BTreeMap;
use worker::{StageReceipt, StageState};

fn authorization(
    request: &MarketRequest,
    result: &worker::FoldResult,
    stage: &alpha_domain::market_encoder_study::MarketTrainingStageV1,
) -> stage_permit::StageAuthorization {
    use alpha_domain::campaign_stage::*;
    let now = chrono::Utc::now();
    let challenge = CampaignStageRequestV1 {
        schema_version: REQUEST_SCHEMA.into(),
        request_sha256: result.request_sha256.clone(),
        attempt_sha256: result.attempt_sha256.clone(),
        root_grant_sha256: result.root_grant_sha256.clone(),
        job_name: "controlled-market-job".into(),
        pod_uid: "controlled-pod-uid".into(),
        stage: stage.key,
        nonce: format!(
            "{:x}",
            Sha256::digest(worker::stage_name(stage.key).as_bytes())
        ),
        requested_at: now,
    };
    let deadline = result
        .job_deadline_at
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(4_000_000_000, 0).unwrap());
    let permit = sign_stage_permit(
        &request.stage_authority,
        challenge,
        "controlled-job-uid".into(),
        deadline,
        now,
        &ed25519_dalek::SigningKey::from_bytes(&[73; 32]),
    )
    .unwrap();
    stage_permit::StageAuthorization {
        schema_version: stage_permit::AUTHORIZATION_SCHEMA.into(),
        permit,
        accepted_at: now,
    }
}
fn execute_stage(
    request: &MarketRequest,
    directory: &Path,
    stage: alpha_domain::market_encoder_study::MarketTrainingStageV1,
    result: &mut worker::FoldResult,
    fitted: &mut BTreeMap<
        alpha_domain::market_encoder_study::MarketTrainingStageKeyV1,
        alpha_engine::market_encoder_study::FittedMarketStage,
    >,
    fit: impl FnOnce(
        &alpha_domain::market_encoder_study::MarketTrainingStageV1,
        Option<&alpha_engine::market_encoder_study::VerifiedMarketStage<'_>>,
    ) -> Result<alpha_engine::market_encoder_study::FittedMarketStage, String>,
) -> anyhow::Result<()> {
    let scope = result.clone();
    worker::execute_stage(
        request,
        directory,
        stage,
        result,
        fitted,
        |stage| Ok(authorization(request, &scope, stage)),
        fit,
    )
}

fn result(request: &MarketRequest) -> worker::FoldResult {
    worker::empty_result(request, &"a".repeat(64), &"b".repeat(64), &"c".repeat(64)).unwrap()
}
fn group(kind: Kind, rmse: f64, state: &str) -> worker::GroupResult {
    worker::GroupResult {
        model_kind: kind,
        state: state.into(),
        verified_members: if kind == Kind::Ridge { 1 } else { 2 },
        coverage: Some(MarketPredictionCoverageV1 {
            expected: 86400,
            emitted: 86400,
            complete: true,
        }),
        replay: None,
        report: Some(shared::SequenceGroupReportV1 {
            schema_version: "monday.sol_market_encoder_group_report.v1".into(),
            horizons: vec![shared::SequenceHorizonDiagnosticV1 {
                horizon_ms: 30000,
                count: 86400,
                mean_absolute_error: rmse,
                rmse,
                mean_absolute_prediction: 1.0,
                mean_absolute_observed: 1.0,
                pearson: None,
                prediction_std: None,
            }],
            blocks: vec![],
            fees: None,
            stresses: vec![],
            largest_block_abs_net_return_share: None,
            uncertainty_note: shared::REPORT_NOTE.into(),
        }),
        seed_diagnostics: Vec::new(),
        diagnostic: None,
    }
}
#[test]
fn market_worker_only_c_strict_increment_qualifies_a_fold() {
    let mut groups = worker::GROUPS
        .into_iter()
        .map(|kind| {
            group(
                kind,
                if kind == Kind::FineTune { 0.5 } else { 1.0 },
                "development_candidate",
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(worker::fold_state(&groups), "development_fold_candidate");
    groups[2].report.as_mut().unwrap().horizons[0].rmse = 1.0;
    assert_eq!(worker::fold_state(&groups), "no_predictive_increment");
    groups[2].report.as_mut().unwrap().horizons[0].rmse = 0.5;
    groups[2].state = "negative_net_return".into();
    assert_eq!(worker::fold_state(&groups), "no_development_candidate");
    let mut result = result(&request());
    result.groups = groups;
    result.state = worker::fold_state(&result.groups).into();
    assert_eq!(
        readback::outcome(&result),
        CampaignAttemptOutcomeV1::NoCandidate
    );
}
#[test]
fn market_worker_missing_control_or_grid_never_passes() {
    let mut groups = worker::GROUPS
        .into_iter()
        .map(|kind| {
            group(
                kind,
                if kind == Kind::FineTune { 0.5 } else { 1.0 },
                "development_candidate",
            )
        })
        .collect::<Vec<_>>();
    groups[3].coverage.as_mut().unwrap().complete = false;
    assert_eq!(worker::fold_state(&groups), "incomplete_comparison");
    groups[3].coverage.as_mut().unwrap().complete = true;
    groups[0].report = None;
    assert_eq!(worker::fold_state(&groups), "incomplete_comparison");
    groups[0] = group(Kind::Scratch, 1.0, "development_candidate");
    groups[2].coverage.as_mut().unwrap().complete = false;
    assert_eq!(worker::fold_state(&groups), "incomplete_comparison");
}
#[test]
fn market_worker_stage_receipts_bind_attempt_source_and_parents() {
    let request = request();
    let result = result(&request);
    let stage = worker::stages(&request).unwrap()[0].clone();
    let binding = worker::binding(&request, &result, stage).unwrap();
    let receipt = StageReceipt {
        binding: binding.clone(),
        authorization_sha256: "d".repeat(64),
        state: StageState::FitFailed,
        attempted: true,
        model: None,
        diagnostic: Some("controlled failure".into()),
    };
    worker::validate_receipt(&receipt, &binding, true).unwrap();
    for field in [
        "attempt_sha256",
        "source_revision",
        "image_identity",
        "study_sha256",
    ] {
        let mut altered = serde_json::to_value(&receipt).unwrap();
        altered["binding"][field] = serde_json::json!("f".repeat(64));
        let changed: StageReceipt = serde_json::from_value(altered).unwrap();
        assert!(worker::validate_receipt(&changed, &binding, true).is_err());
    }
    let mut changed = receipt.clone();
    changed
        .binding
        .dependencies
        .insert("another-parent.receipt.json".into(), "f".repeat(64));
    assert!(worker::validate_receipt(&changed, &binding, true).is_err());
    let mut changed = receipt;
    changed.attempted = false;
    assert!(worker::validate_receipt(&changed, &binding, true).is_err());
}
#[test]
fn market_worker_interrupted_stage_cannot_be_implicitly_retrained() {
    let request = request();
    let stage = worker::stages(&request).unwrap()[0].clone();
    let directory = tempfile::tempdir().unwrap();
    worker::ensure_unstarted(directory.path(), stage.key).unwrap();
    std::fs::write(
        directory
            .path()
            .join(format!("{}.start.json", worker::stage_name(stage.key))),
        b"{}",
    )
    .unwrap();
    let mut result = result(&request);
    let mut fitted = BTreeMap::new();
    let error = execute_stage(
        &request,
        directory.path(),
        stage,
        &mut result,
        &mut fitted,
        |_, _| panic!("must not rerun interrupted fit"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("bounded recovery"));
}
#[test]
fn market_worker_failed_parent_skips_dependencies_and_charges_entire_fold() {
    let request = request();
    let mut result = result(&request);
    let directory = tempfile::tempdir().unwrap();
    let mut fitted = BTreeMap::new();
    let mut called = 0;
    for stage in worker::stages(&request).unwrap() {
        execute_stage(
            &request,
            directory.path(),
            stage,
            &mut result,
            &mut fitted,
            |_, _| {
                called += 1;
                Err("controlled fit failure".into())
            },
        )
        .unwrap();
    }
    assert_eq!(called, 7);
    assert_eq!(result.stages.len(), 22);
    assert_eq!(result.primary_fits_attempted, 7);
    assert_eq!(result.verification_fits_attempted, 0);
    assert_eq!(result.charged_trials, 22);
    assert!(result
        .stages
        .iter()
        .filter(|s| matches!(s.binding.stage.key.kind, Kind::LinearProbe | Kind::FineTune))
        .all(|s| s.state == StageState::SkippedDependency && !s.attempted));
    assert_eq!(readback::outcome(&result), CampaignAttemptOutcomeV1::Failed);
    let mut restored = worker::empty_result(
        &request,
        &result.request_sha256,
        &result.root_grant_sha256,
        &result.attempt_sha256,
    )
    .unwrap();
    for stage in worker::stages(&request).unwrap() {
        execute_stage(
            &request,
            directory.path(),
            stage,
            &mut restored,
            &mut fitted,
            |_, _| panic!("completed failure receipt must not refit"),
        )
        .unwrap();
    }
    assert_eq!(result, restored);
    for kind in worker::GROUPS {
        result.groups.push(group(kind, 1.0, "fit_failed"));
    }
    result.state = worker::fold_state(&result.groups).into();
    readback::validate_result(
        &request,
        &result.request_sha256,
        &result.root_grant_sha256,
        &result.attempt_sha256,
        &result.job_uid,
        &result,
    )
    .unwrap();
    result.primary_fits_attempted = 11;
    assert!(readback::validate_result(
        &request,
        &result.request_sha256,
        &result.root_grant_sha256,
        &result.attempt_sha256,
        &result.job_uid,
        &result
    )
    .is_err());
}
#[test]
fn market_worker_skipped_receipt_requires_failed_dependency_and_no_attempt() {
    let request = request();
    let result = result(&request);
    let binding = worker::binding(
        &request,
        &result,
        worker::stages(&request).unwrap()[0].clone(),
    )
    .unwrap();
    let mut receipt = StageReceipt {
        binding: binding.clone(),
        authorization_sha256: "d".repeat(64),
        state: StageState::SkippedDependency,
        attempted: false,
        model: None,
        diagnostic: Some("dependency failed".into()),
    };
    assert!(worker::validate_receipt(&receipt, &binding, true).is_err());
    worker::validate_receipt(&receipt, &binding, false).unwrap();
    receipt.attempted = true;
    assert!(worker::validate_receipt(&receipt, &binding, false).is_err());
}
#[test]
fn market_worker_archive_bound_and_inventory_follow_stage_plan() {
    let request = request();
    let stages = worker::stages(&request).unwrap();
    assert_eq!(
        stages
            .iter()
            .filter(|s| s.key.purpose == Purpose::Primary)
            .count(),
        11
    );
    assert_eq!(worker::archive_file_limit(&request).unwrap(), 126);
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join(worker::DIRECTORY)).unwrap();
    std::fs::write(directory.path().join("result.json"), b"{}").unwrap();
    std::fs::write(
        directory.path().join(worker::DIRECTORY).join("stage.json"),
        b"model",
    )
    .unwrap();
    let artifacts = BTreeMap::from([(
        "stage.json".into(),
        format!("{:x}", Sha256::digest(b"model")),
    )]);
    let bundle = directory.path().join("results.zip");
    shared::pack_result_artifacts(directory.path(), &bundle, worker::DIRECTORY, &artifacts)
        .unwrap();
    verify_artifact_inventory(&bundle, directory.path(), worker::DIRECTORY, &artifacts).unwrap();
    let mut extra = artifacts.clone();
    extra.insert("unadmitted.json".into(), "a".repeat(64));
    assert!(
        verify_artifact_inventory(&bundle, directory.path(), worker::DIRECTORY, &extra).is_err()
    );
    std::fs::write(
        directory.path().join(worker::DIRECTORY).join("stage.json"),
        b"tampered",
    )
    .unwrap();
    assert!(
        verify_artifact_inventory(&bundle, directory.path(), worker::DIRECTORY, &artifacts)
            .is_err()
    );
}

/// Controlled sparse training windows exercise the actual CPU trainer, artifacts
/// and restore path. These bytes are test inputs and never stand in for real data.
fn training_fixture(root: &Path) -> MarketRequest {
    use hft_research_manifest::{market_encoder::*, sequence::SequenceShardV1};
    use hft_research_ml::market_encoder::data::{
        derive_market_training_anchors, MarketFeatureReader,
    };
    let mut request = request();
    request.plan.training.pretraining_updates = 2;
    request.plan.training.task_updates = 2;
    request.plan.training.compute_control_updates = 4;
    request.plan.training.max_training_examples = 128;
    for fold in &mut request.plan.folds {
        fold.train.view.decision_stride_ms = 5 * 3_600_000;
    }
    let view = request.plan.folds[0].train.view;
    let mut features = Vec::new();
    let mut labels = Vec::new();
    let mut anchor = view.decision_start_ms;
    let mut series = 0;
    while anchor < view.end_ms - 30000 {
        series += 1;
        for i in 0..60 {
            let clock = anchor - 59000 + i * 1000;
            features.push(MarketFeatureFrameV1 {
                series_id: series,
                observed_at_ms: clock,
                feature_max_available_at_ms: clock,
                channels: (0..24)
                    .map(|c| {
                        i as f32 / 60.0
                            + c as f32 / 100.0
                            + if c == 0 && i == 59 {
                                (series % 7) as f32 / 50.0
                            } else {
                                0.0
                            }
                    })
                    .collect(),
            });
        }
        labels.push(MarketTargetFrameV1 {
            series_id: series,
            observed_at_ms: anchor,
            available_at_ms: anchor + 30000,
            simple_return: ((series % 3) as f32 - 1.0) * 0.001,
            spread_bps: 1.0,
        });
        anchor += view.decision_stride_ms;
    }
    fn shard<T: Serialize>(
        root: &Path,
        name: &str,
        rows: &[T],
        clock: impl Fn(&T) -> i64,
    ) -> SequenceShardV1 {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(root.join(name), &bytes).unwrap();
        SequenceShardV1 {
            file: name.into(),
            sha256: bytes_digest(&bytes),
            bytes: bytes.len() as u64,
            rows: rows.len() as u64,
            first_observed_at_ms: clock(rows.first().unwrap()),
            last_observed_at_ms: clock(rows.last().unwrap()),
        }
    }
    let dataset = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: "d".repeat(64),
        input: request.plan.input.clone(),
        shards: vec![shard(root, "features.jsonl", &features, |r| {
            r.observed_at_ms
        })],
    };
    let targets = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: dataset.digest().unwrap(),
        horizon_ms: 30000,
        shards: vec![shard(root, "targets.jsonl", &labels, |r| r.observed_at_ms)],
    };
    request.plan.folds[0].train.features_sha256 = dataset.digest().unwrap();
    request.plan.folds[0].train.targets_sha256 = targets.digest().unwrap();
    let mut grid = worker::reader_request(&request, &request.plan.folds[0].train);
    grid.qualified_anchors_sha256 = None;
    let mut reader = MarketFeatureReader::open(root, dataset.clone(), &grid).unwrap();
    let anchors = derive_market_training_anchors(
        &mut reader,
        root,
        targets.clone(),
        &targets.digest().unwrap(),
    )
    .unwrap();
    let digest = anchors.digest().unwrap();
    std::fs::write(
        root.join(format!("{digest}.market-anchors.json")),
        serde_json::to_vec(&anchors).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("features.json"),
        serde_json::to_vec(&dataset).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("targets.json"),
        serde_json::to_vec(&targets).unwrap(),
    )
    .unwrap();
    request.plan.folds[0].train.qualified_anchors_sha256 = Some(digest.clone());
    request.inputs.train.features = inputs::Artifact {
        file: "features.json".into(),
        sha256: dataset.digest().unwrap(),
    };
    request.inputs.train.targets = inputs::Artifact {
        file: "targets.json".into(),
        sha256: targets.digest().unwrap(),
    };
    request.inputs.train.qualified_anchors = Some(inputs::Artifact {
        file: format!("{digest}.market-anchors.json"),
        sha256: digest,
    });
    let view = request.plan.folds[0].validation.data.view;
    let feature_rows = (0..61)
        .map(|i| MarketFeatureFrameV1 {
            series_id: 1,
            observed_at_ms: view.history_start_ms + i * 1000,
            feature_max_available_at_ms: view.history_start_ms + i * 1000,
            channels: (0..24)
                .map(|c| i as f32 / 180.0 + c as f32 / 100.0)
                .collect(),
        })
        .collect::<Vec<_>>();
    let features = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: "e".repeat(64),
        input: request.plan.input.clone(),
        shards: vec![shard(
            root,
            "validation-features.jsonl",
            &feature_rows,
            |r| r.observed_at_ms,
        )],
    };
    let target_rows = (59..61)
        .map(|i| MarketTargetFrameV1 {
            series_id: 1,
            observed_at_ms: view.history_start_ms + i * 1000,
            available_at_ms: view.history_start_ms + (i + 30) * 1000,
            simple_return: 0.001,
            spread_bps: 1.0,
        })
        .collect::<Vec<_>>();
    let targets = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: features.digest().unwrap(),
        horizon_ms: 30000,
        shards: vec![shard(root, "validation-targets.jsonl", &target_rows, |r| {
            r.observed_at_ms
        })],
    };
    std::fs::write(
        root.join("validation-features.json"),
        serde_json::to_vec(&features).unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join("validation-targets.json"),
        serde_json::to_vec(&targets).unwrap(),
    )
    .unwrap();
    request.inputs.validation.features = inputs::Artifact {
        file: "validation-features.json".into(),
        sha256: features.digest().unwrap(),
    };
    request.inputs.validation.targets = inputs::Artifact {
        file: "validation-targets.json".into(),
        sha256: targets.digest().unwrap(),
    };
    request.plan.folds[0].validation.data.features_sha256 = features.digest().unwrap();
    request.plan.folds[0].validation.data.targets_sha256 = targets.digest().unwrap();
    request_tests::rebind(&mut request);
    request.validate().unwrap();
    request
}

#[test]
fn market_worker_real_stage_bundles_restore_resume_and_reject_tampering() {
    use alpha_engine::market_encoder_study::{fit_market_stage, MarketStageInput};
    let data = tempfile::tempdir().unwrap();
    let request = training_fixture(data.path());
    // Validate the deliberately sparse fixture before paying for any controlled fit.
    let mut validation = inputs::open_task_reader(
        data.path(),
        &request.inputs.validation,
        &worker::reader_request(&request, &request.plan.folds[0].validation.data),
    )
    .unwrap();
    assert_eq!(validation.next_batch(128).unwrap().len(), 2);
    validation.finish_pass().unwrap();
    let output = tempfile::tempdir().unwrap();
    let results_dir = output.path().join(worker::DIRECTORY);
    std::fs::create_dir(&results_dir).unwrap();
    let mut actual = result(&request);
    let mut fitted = BTreeMap::new();
    let mut calls = 0;
    for stage in worker::stages(&request).unwrap() {
        execute_stage(
            &request,
            &results_dir,
            stage,
            &mut actual,
            &mut fitted,
            |stage, parent| {
                calls += 1;
                let data_request = worker::reader_request(&request, &request.plan.folds[0].train);
                if stage.key.kind == Kind::Pretrain {
                    let mut reader = inputs::open_feature_reader(
                        data.path(),
                        &request.inputs.train,
                        &data_request,
                    )
                    .map_err(|e| e.to_string())?;
                    fit_market_stage(
                        &request.plan,
                        stage.key,
                        MarketStageInput::Features(&mut reader),
                        parent,
                    )
                } else {
                    let mut reader =
                        inputs::open_task_reader(data.path(), &request.inputs.train, &data_request)
                            .map_err(|e| e.to_string())?;
                    fit_market_stage(
                        &request.plan,
                        stage.key,
                        MarketStageInput::Targets(&mut reader),
                        parent,
                    )
                }
            },
        )
        .unwrap();
    }
    assert_eq!(calls, 22);
    assert_eq!(actual.primary_fits_attempted, 11);
    assert_eq!(actual.verification_fits_attempted, 11);
    assert!(
        actual
            .stages
            .iter()
            .all(|s| s.state == StageState::Completed),
        "{:?}",
        actual.stages
    );
    let mut resumed = result(&request);
    let mut restored = BTreeMap::new();
    for stage in worker::stages(&request).unwrap() {
        execute_stage(
            &request,
            &results_dir,
            stage,
            &mut resumed,
            &mut restored,
            |_, _| panic!("a complete pinned stage must never refit"),
        )
        .unwrap();
    }
    assert_eq!(actual, resumed);
    assert_eq!(fitted.len(), 22);
    assert_eq!(restored.len(), 22);
    // The actual restored models reproduce every member and the fixed mean;
    // sparse validation remains incomplete even when all model fits succeeded.
    for kind in worker::GROUPS {
        let predict = |models: &BTreeMap<_, _>| {
            use alpha_domain::market_encoder_study::MarketTrainingStageKeyV1 as Key;
            use alpha_engine::market_encoder_study::{
                predict_market_validation, verify_market_stage_pair, MarketStudyEnsemble,
            };
            let seeds = if kind == Kind::Ridge {
                vec![0]
            } else {
                vec![7, 11]
            };
            let witnesses = seeds
                .into_iter()
                .map(|seed| {
                    let key = Key {
                        fold_id: 1,
                        kind,
                        seed,
                        purpose: Purpose::Primary,
                    };
                    verify_market_stage_pair(
                        &models[&key],
                        &models[&Key {
                            purpose: Purpose::Verification,
                            ..key
                        }],
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            let ensemble =
                MarketStudyEnsemble::new(&request.plan, witnesses.iter().collect()).unwrap();
            let mut reader = inputs::open_task_reader(
                data.path(),
                &request.inputs.validation,
                &worker::reader_request(&request, &request.plan.folds[0].validation.data),
            )
            .unwrap();
            let mut rows = Vec::new();
            let coverage =
                predict_market_validation(&request.plan, &ensemble, &mut reader, |row| {
                    let mean = row
                        .member_returns
                        .iter()
                        .map(|(_, value)| *value)
                        .sum::<f64>()
                        / row.member_returns.len() as f64;
                    assert_eq!(row.predicted_return, mean);
                    assert_eq!(
                        row.member_returns
                            .iter()
                            .map(|(seed, _)| *seed)
                            .collect::<Vec<_>>(),
                        if kind == Kind::Ridge {
                            vec![0]
                        } else {
                            vec![7, 11]
                        }
                    );
                    rows.push(row);
                    Ok(())
                })
                .unwrap();
            assert!(!coverage.complete);
            assert_eq!(coverage.expected, 86400);
            assert_eq!(coverage.emitted, 2);
            (coverage, rows)
        };
        assert_eq!(predict(&fitted), predict(&restored));
    }
    let first_predictions = tempfile::tempdir().unwrap();
    let restored_predictions = tempfile::tempdir().unwrap();
    for kind in worker::GROUPS {
        let mut first_artifacts = BTreeMap::new();
        let mut restored_artifacts = BTreeMap::new();
        let first = worker::evaluate_verified_group(
            &request,
            data.path(),
            first_predictions.path(),
            kind,
            &fitted,
            &actual.stages,
            &mut first_artifacts,
        )
        .unwrap();
        let second = worker::evaluate_verified_group(
            &request,
            data.path(),
            restored_predictions.path(),
            kind,
            &restored,
            &resumed.stages,
            &mut restored_artifacts,
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first_artifacts, restored_artifacts);
        assert_eq!(first.state, "incomplete_decision_grid");
        assert!(first.replay.is_none());
        assert_eq!(
            first.seed_diagnostics.len(),
            if kind == Kind::Ridge { 1 } else { 2 }
        );
        assert_eq!(first_artifacts.len(), 1);
    }
    // A genuinely fitted and independently verified P is retained when current
    // authority is revoked before C. Resuming evidence never requests a new fit.
    let revoked_directory = tempfile::tempdir().unwrap();
    let mut prefix = result(&request);
    let mut prefix_models = BTreeMap::new();
    let c_index = actual
        .stages
        .iter()
        .position(|r| {
            r.binding.stage.key.kind == Kind::FineTune
                && r.binding.stage.key.purpose == Purpose::Primary
                && r.binding.stage.key.seed == 7
        })
        .unwrap();
    for receipt in &actual.stages[..c_index] {
        let name = worker::stage_name(receipt.binding.stage.key);
        for suffix in [
            "permit.json",
            "start.json",
            "receipt.json",
            "json",
            "weights",
        ] {
            std::fs::copy(
                results_dir.join(format!("{name}.{suffix}")),
                revoked_directory.path().join(format!("{name}.{suffix}")),
            )
            .unwrap();
        }
        worker::accept_stage(
            &request,
            revoked_directory.path(),
            receipt,
            &mut prefix,
            &mut prefix_models,
        )
        .unwrap();
    }
    let before_revoke = prefix.clone();
    let stage = actual.stages[c_index].binding.stage.clone();
    let c_name = worker::stage_name(stage.key);
    let error = worker::execute_stage(
        &request,
        revoked_directory.path(),
        stage,
        &mut prefix,
        &mut prefix_models,
        |_| Err(anyhow::anyhow!("Study revoked after verified P")),
        |_, _| panic!("revoked C must not train"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("revoked"));
    assert_eq!(prefix, before_revoke);
    assert!(!revoked_directory
        .path()
        .join(format!("{c_name}.start.json"))
        .exists());
    let mut p_only = result(&request);
    worker::execute_stage(
        &request,
        revoked_directory.path(),
        actual.stages[0].binding.stage.clone(),
        &mut p_only,
        &mut BTreeMap::new(),
        |_| panic!("completed P needs no new permit"),
        |_, _| panic!("completed P must not retrain"),
    )
    .unwrap();
    assert_eq!(p_only.primary_fits_attempted, 1);
    assert_eq!(p_only.stages[0], actual.stages[0]);
    // Verification and primary have equal learned values, but purpose remains in artifact identity.
    let p = &actual.stages[0].model.as_ref().unwrap();
    let pv = &actual.stages[1].model.as_ref().unwrap();
    assert_eq!(p.fitted_values_sha256, pv.fitted_values_sha256);
    assert_ne!(p.manifest_sha256, pv.manifest_sha256);
    shared::write_new_json(&output.path().join("result.json"), &actual).unwrap();
    let bundle = output.path().join("results.zip");
    shared::pack_result_artifacts(output.path(), &bundle, worker::DIRECTORY, &actual.artifacts)
        .unwrap();
    let extracted = output.path().join("extracted");
    extract_bundle_with_file_limit(
        &bundle,
        &extracted,
        worker::archive_file_limit(&request).unwrap(),
    )
    .unwrap();
    verify_artifact_inventory(&bundle, &extracted, worker::DIRECTORY, &actual.artifacts).unwrap();
    let key = worker::stages(&request).unwrap()[0].key;
    let file = results_dir.join(format!("{}.weights", worker::stage_name(key)));
    let mut parameters = std::fs::read(&file).unwrap();
    parameters[0] ^= 1;
    std::fs::write(&file, parameters).unwrap();
    let mut bad = result(&request);
    let mut cache = BTreeMap::new();
    assert!(execute_stage(
        &request,
        &results_dir,
        worker::stages(&request).unwrap()[0].clone(),
        &mut bad,
        &mut cache,
        |_, _| panic!("tampered stage must not refit")
    )
    .is_err());
    let mut another = result(&request);
    another.attempt_sha256 = "f".repeat(64);
    assert!(execute_stage(
        &request,
        &results_dir,
        worker::stages(&request).unwrap()[0].clone(),
        &mut another,
        &mut cache,
        |_, _| panic!("another attempt must not reuse stage")
    )
    .is_err());
}

#[test]
fn market_worker_native_missing_validation_grid_stops_before_any_fit() {
    use alpha_domain::campaign_control::*;
    use hft_research_manifest::market_encoder::{MarketFeatureDatasetV1, MarketTargetDatasetV1};
    use hft_research_ml::market_encoder::data::{
        derive_market_training_anchors, MarketFeatureReader,
    };
    let root = tempfile::tempdir().unwrap();
    let output = tempfile::tempdir().unwrap();
    let mut request = request();
    request.plan.training.pretraining_updates = 2;
    request.plan.training.task_updates = 2;
    request.plan.training.compute_control_updates = 4;
    request.plan.training.max_training_examples = 128;
    for fold in &mut request.plan.folds {
        fold.train.view.decision_stride_ms = 5 * 3_600_000;
    }
    let source = inputs::tests::source(
        root.path(),
        request.plan.folds[0].train.view.history_start_ms,
        false,
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&source.receipt.read(root.path(), 4 * 1024 * 1024).unwrap())
            .unwrap();
    for field in ["replay_artifact", "replay_manifest"] {
        std::fs::remove_file(root.path().join(receipt[field]["file"].as_str().unwrap())).unwrap();
    }
    let mut train = inputs::tests::multi_source_location(root.path(), vec![source]);
    request.plan.folds[0].train.features_sha256 = train.features.sha256.clone();
    request.plan.folds[0].train.targets_sha256 = train.targets.sha256.clone();
    let features: MarketFeatureDatasetV1 =
        serde_json::from_slice(&train.features.read(root.path(), 4 * 1024 * 1024).unwrap())
            .unwrap();
    let targets: MarketTargetDatasetV1 =
        serde_json::from_slice(&train.targets.read(root.path(), 4 * 1024 * 1024).unwrap()).unwrap();
    let mut grid = worker::reader_request(&request, &request.plan.folds[0].train);
    grid.qualified_anchors_sha256 = None;
    let mut reader = MarketFeatureReader::open(root.path(), features, &grid).unwrap();
    let anchors =
        derive_market_training_anchors(&mut reader, root.path(), targets, &train.targets.sha256)
            .unwrap();
    let hash = anchors.digest().unwrap();
    let file = format!("{hash}.market-anchors.json");
    std::fs::write(
        root.path().join(&file),
        serde_json::to_vec(&anchors).unwrap(),
    )
    .unwrap();
    train.qualified_anchors = Some(inputs::Artifact {
        file,
        sha256: hash.clone(),
    });
    request.plan.folds[0].train.qualified_anchors_sha256 = Some(hash);
    let source = inputs::tests::source(
        root.path(),
        request.plan.folds[0].validation.data.view.history_start_ms,
        false,
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&source.receipt.read(root.path(), 4 * 1024 * 1024).unwrap())
            .unwrap();
    let validation = inputs::tests::multi_source_location(root.path(), vec![source]);
    request.inputs = inputs::MarketCampaignInputs {
        schema_version: inputs::INPUTS_SCHEMA.into(),
        producer_source_revision: "a".repeat(40),
        producer_image: format!("registry/runner@sha256:{}", "b".repeat(64)),
        pvc_name: "controlled-sol".into(),
        pvc_uid: "controlled-pvc".into(),
        sub_path: "sol-market-encoder/fold-1".into(),
        fold_id: 1,
        train,
        validation,
        replay_artifact: serde_json::from_value(receipt["replay_artifact"].clone()).unwrap(),
        replay_manifest: serde_json::from_value(receipt["replay_manifest"].clone()).unwrap(),
    };
    request.plan.folds[0].validation.data.features_sha256 =
        request.inputs.validation.features.sha256.clone();
    request.plan.folds[0].validation.data.targets_sha256 =
        request.inputs.validation.targets.sha256.clone();
    request.plan.folds[0].validation.replay_manifest_sha256 =
        request.inputs.replay_manifest.sha256.clone();
    request_tests::rebind(&mut request);
    request.validate().unwrap();
    let now = chrono::Utc::now();
    let signing = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
    let signed = sign_campaign_root_grant(
        CampaignRootGrantV1 {
            schema_version: ROOT_GRANT_SCHEMA.into(),
            root_id: "controlled-market-worker".into(),
            family: CampaignFamilyPolicyV1 {
                family_id: request.plan.study_id.clone(),
                definition_sha256: request.plan.content_hash().unwrap(),
                max_trials: 60,
            },
            execution_scope: CampaignExecutionScope::PreHoldout,
            execution: crate::mission_dispatch::sequence_admission::execution_binding(
                &request,
                &format!("registry/controller@sha256:{}", "f".repeat(64)),
            )
            .unwrap(),
            allowed_policy_revision_ids: std::collections::BTreeSet::from([request
                .policy_id()
                .unwrap()]),
            max_follow_ups: 0,
            budget: CampaignRootBudgetV1 {
                max_trials: 44,
                max_job_attempts: 2,
                max_job_seconds: 2400,
                max_llm_tokens: 0,
            },
            valid_from: now - chrono::TimeDelta::minutes(1),
            expires_at: now + chrono::TimeDelta::hours(1),
        },
        "test".into(),
        &signing,
    )
    .unwrap();
    let grant = verify_campaign_root_grant(
        &signed,
        &BTreeMap::from([("test".into(), signing.verifying_key())]),
        now,
    )
    .unwrap();
    let error = worker::run_fold(
        &request,
        &"a".repeat(64),
        &"b".repeat(64),
        &grant,
        root.path(),
        output.path(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("full decision grid")
            || error
                .to_string()
                .contains("missing target for common market anchor"),
        "{error:#}"
    );
    assert_eq!(
        std::fs::read_dir(output.path().join(worker::DIRECTORY))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn market_worker_paired_diagnostics_do_not_select_the_better_seed() {
    let mut groups = worker::GROUPS
        .into_iter()
        .map(|kind| {
            group(
                kind,
                if kind == Kind::FineTune { 0.5 } else { 1.0 },
                "development_candidate",
            )
        })
        .collect::<Vec<_>>();
    for item in &mut groups {
        item.seed_diagnostics = [7, 11]
            .into_iter()
            .map(|seed| {
                let mut prediction = item.report.as_ref().unwrap().horizons[0].clone();
                prediction.rmse = if item.model_kind == Kind::FineTune {
                    if seed == 7 {
                        2.0
                    } else {
                        0.0
                    }
                } else {
                    1.0
                };
                worker::SeedDiagnostic { seed, prediction }
            })
            .collect();
    }
    let report = worker::comparison_report(&groups);
    assert_eq!(report.as_array().unwrap().len(), 3);
    assert_eq!(report[0]["seed_mean_mse_delta"], -0.75);
    assert_eq!(
        report[0]["paired_seed_mse_deltas"],
        serde_json::json!([{"seed":7,"mse_delta":3.0},{"seed":11,"mse_delta":-1.0}])
    );
    assert_eq!(worker::fold_state(&groups), "development_fold_candidate");
}

#[test]
fn market_worker_first_readback_rejects_self_consistent_forged_cache() {
    // Both local JSON and archive agree on a forged failure diagnostic. Only a
    // fresh GET can distinguish them from the independently published result.
    let request = request();
    let cache = tempfile::tempdir().unwrap();
    let directory = cache.path().join(worker::DIRECTORY);
    std::fs::create_dir(&directory).unwrap();
    let mut forged = result(&request);
    let mut models = BTreeMap::new();
    for stage in worker::stages(&request).unwrap() {
        execute_stage(
            &request,
            &directory,
            stage,
            &mut forged,
            &mut models,
            |_, _| Err("forged cached diagnostic".into()),
        )
        .unwrap();
    }
    for kind in worker::GROUPS {
        forged.groups.push(
            worker::evaluate_verified_group(
                &request,
                cache.path(),
                cache.path(),
                kind,
                &models,
                &forged.stages,
                &mut BTreeMap::new(),
            )
            .unwrap(),
        );
    }
    forged.state = worker::fold_state(&forged.groups).into();
    readback::validate_result(
        &request,
        &forged.request_sha256,
        &forged.root_grant_sha256,
        &forged.attempt_sha256,
        &forged.job_uid,
        &forged,
    )
    .unwrap();
    shared::write_new_json(&cache.path().join("result.json"), &forged).unwrap();
    let bundle = cache.path().join("results.zip");
    shared::pack_result_artifacts(cache.path(), &bundle, worker::DIRECTORY, &forged.artifacts)
        .unwrap();
    let extracted = cache.path().join("extracted");
    extract_bundle_with_file_limit(
        &bundle,
        &extracted,
        worker::archive_file_limit(&request).unwrap(),
    )
    .unwrap();
    verify_artifact_inventory(&bundle, &extracted, worker::DIRECTORY, &forged.artifacts).unwrap();
    // Reuse the same genuine signed permits. The local-cache attacker only
    // changes unsigned diagnostic/receipt bytes and their dependent hashes;
    // no additional controller signature is needed for this former attack.
    let published_directory = tempfile::tempdir().unwrap();
    let mut published = result(&request);
    for old in &forged.stages {
        let mut receipt = old.clone();
        receipt.binding = worker::binding(&request, &published, old.binding.stage.clone()).unwrap();
        if receipt.state == StageState::FitFailed {
            receipt.diagnostic = Some("actual published diagnostic".into());
        }
        let name = worker::stage_name(receipt.binding.stage.key);
        std::fs::copy(
            directory.join(format!("{name}.permit.json")),
            published_directory
                .path()
                .join(format!("{name}.permit.json")),
        )
        .unwrap();
        if receipt.attempted {
            shared::write_new_json(
                &published_directory
                    .path()
                    .join(format!("{name}.start.json")),
                &worker::StageStart {
                    binding: receipt.binding.clone(),
                    authorization_sha256: receipt.authorization_sha256.clone(),
                },
            )
            .unwrap();
        }
        shared::write_new_json(
            &published_directory
                .path()
                .join(format!("{name}.receipt.json")),
            &receipt,
        )
        .unwrap();
        worker::verify_stage_record(
            &request,
            published_directory.path(),
            &receipt,
            &mut published,
        )
        .unwrap();
    }
    published.groups = forged.groups.clone();
    published.state = worker::fold_state(&published.groups).into();
    readback::validate_result(
        &request,
        &published.request_sha256,
        &published.root_grant_sha256,
        &published.attempt_sha256,
        &published.job_uid,
        &published,
    )
    .unwrap();
    let payload = serde_json::to_vec(&published).unwrap();
    let expected = format!("{:x}", Sha256::digest(&payload));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/result.json", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0u8; 4096];
            let received = stream.read(&mut request).unwrap();
            assert!(received > 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )
            .unwrap();
            stream.write_all(&payload).unwrap();
        }
    });
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let (download, hash) = readback::fresh_result(&client, &url, cache.path(), None).unwrap();
    assert_eq!(hash, expected);
    assert_eq!(
        read_json::<worker::FoldResult>(download.path()).unwrap(),
        published
    );
    assert!(readback::verify_published_result(&extracted, &hash).is_err());
    let forged_hash =
        crate::mission_runner::sha256_file(&cache.path().join("result.json")).unwrap();
    assert!(readback::fresh_result(&client, &url, cache.path(), Some(&forged_hash)).is_err());
    server.join().unwrap();
}

#[test]
fn market_stage_permit_rejects_old_nonce_expiry_deadline_and_job_drift() {
    let request = request();
    let mut scope = result(&request);
    let stage = worker::stages(&request).unwrap()[0].clone();
    let mut valid = authorization(&request, &scope, &stage);
    // The live protocol also checks the frozen root deadline, independently of the signer.
    let root_deadline = chrono::Utc::now() + chrono::TimeDelta::hours(1);
    valid.permit = alpha_domain::campaign_stage::sign_stage_permit(
        &request.stage_authority,
        valid.permit.request.clone(),
        valid.permit.job_uid.clone(),
        root_deadline,
        valid.accepted_at,
        &ed25519_dalek::SigningKey::from_bytes(&[73; 32]),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("permit.json");
    std::fs::write(&path, serde_json::to_vec(&valid.permit).unwrap()).unwrap();
    stage_permit::await_permit(
        &request,
        &valid.permit.request,
        &path,
        root_deadline,
        Duration::from_millis(50),
        Duration::from_millis(1),
    )
    .unwrap();
    let mut wrong_nonce = valid.permit.request.clone();
    wrong_nonce.nonce = "f".repeat(64);
    assert!(stage_permit::await_permit(
        &request,
        &wrong_nonce,
        &path,
        root_deadline,
        Duration::from_millis(50),
        Duration::from_millis(1)
    )
    .is_err());
    assert!(stage_permit::await_permit(
        &request,
        &valid.permit.request,
        &path,
        root_deadline - chrono::TimeDelta::seconds(1),
        Duration::from_millis(50),
        Duration::from_millis(1)
    )
    .is_err());
    scope.job_name = valid.permit.request.job_name.clone();
    scope.pod_uid = valid.permit.request.pod_uid.clone();
    scope.job_uid = "different-claimed-job".into();
    scope.job_deadline_at = Some(root_deadline);
    assert!(worker::validate_stage_authorization(&request, &scope, &stage, &valid).is_err());
    let old = chrono::Utc::now() - chrono::TimeDelta::seconds(60);
    let mut old_challenge = valid.permit.request.clone();
    old_challenge.requested_at = old;
    let expired = alpha_domain::campaign_stage::sign_stage_permit(
        &request.stage_authority,
        old_challenge.clone(),
        valid.permit.job_uid.clone(),
        root_deadline,
        old,
        &ed25519_dalek::SigningKey::from_bytes(&[73; 32]),
    )
    .unwrap();
    std::fs::write(&path, serde_json::to_vec(&expired).unwrap()).unwrap();
    assert!(stage_permit::await_permit(
        &request,
        &old_challenge,
        &path,
        root_deadline,
        Duration::from_millis(50),
        Duration::from_millis(1)
    )
    .is_err());
    let missing = directory.path().join("absent.json");
    assert!(stage_permit::await_permit(
        &request,
        &valid.permit.request,
        &missing,
        root_deadline,
        Duration::from_millis(2),
        Duration::from_millis(1)
    )
    .is_err());
}

#[test]
fn market_stage_without_current_authority_cannot_write_a_start_or_consume_a_fit() {
    let request = request();
    let mut result = result(&request);
    let directory = tempfile::tempdir().unwrap();
    let mut models = BTreeMap::new();
    let stage = worker::stages(&request).unwrap()[0].clone();
    let error = worker::execute_stage(
        &request,
        directory.path(),
        stage,
        &mut result,
        &mut models,
        |_| Err(anyhow::anyhow!("current Study authority revoked")),
        |_, _| panic!("revoked authority must never reach fit"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("revoked"));
    assert_eq!(result.primary_fits_attempted, 0);
    assert_eq!(result.charged_trials, 22);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[test]
fn market_stage_verified_pretraining_survives_revocation_without_starting_c() {
    use alpha_engine::market_encoder_study::{fit_market_stage, MarketStageInput};
    let data = tempfile::tempdir().unwrap();
    let request = training_fixture(data.path());
    let directory = tempfile::tempdir().unwrap();
    let mut progress = result(&request);
    let mut models = BTreeMap::new();
    let stages = worker::stages(&request).unwrap();
    let c_index = stages
        .iter()
        .position(|s| {
            s.key.kind == Kind::FineTune && s.key.seed == 7 && s.key.purpose == Purpose::Primary
        })
        .unwrap();
    let mut real_pretraining_fits = 0;
    for stage in &stages[..c_index] {
        execute_stage(
            &request,
            directory.path(),
            stage.clone(),
            &mut progress,
            &mut models,
            |stage, parent| {
                if stage.key.kind != Kind::Pretrain {
                    return Err("controlled reference fit failure".into());
                }
                real_pretraining_fits += 1;
                let mut reader = inputs::open_feature_reader(
                    data.path(),
                    &request.inputs.train,
                    &worker::reader_request(&request, &request.plan.folds[0].train),
                )
                .map_err(|e| e.to_string())?;
                fit_market_stage(
                    &request.plan,
                    stage.key,
                    MarketStageInput::Features(&mut reader),
                    parent,
                )
            },
        )
        .unwrap();
    }
    assert_eq!(real_pretraining_fits, 2);
    assert!(progress.stages[..2]
        .iter()
        .all(|s| s.state == StageState::Completed));
    let c = stages[c_index].clone();
    assert!(
        worker::ready(&c, &progress.stages),
        "C has its own verified P; dependency failure must not explain its stop"
    );
    let before = progress.clone();
    let error = worker::execute_stage(
        &request,
        directory.path(),
        c.clone(),
        &mut progress,
        &mut models,
        |_| Err(anyhow::anyhow!("current Study approval revoked after P")),
        |_, _| panic!("revoked C must not fit"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("revoked"));
    assert_eq!(progress, before);
    assert_eq!(progress.charged_trials, 22);
    assert!(!directory
        .path()
        .join(format!("{}.start.json", worker::stage_name(c.key)))
        .exists());
    let mut restored = result(&request);
    worker::execute_stage(
        &request,
        directory.path(),
        stages[0].clone(),
        &mut restored,
        &mut BTreeMap::new(),
        |_| panic!("completed P must not obtain another permit"),
        |_, _| panic!("completed P must not retrain"),
    )
    .unwrap();
    assert_eq!(restored.stages[0], progress.stages[0]);
    assert_eq!(restored.primary_fits_attempted, 1);
}
