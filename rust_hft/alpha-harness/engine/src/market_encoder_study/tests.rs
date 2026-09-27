use super::*;
use hft_research_manifest::sequence::{SequenceShardV1, SequenceViewV1};
use std::path::Path;
const DAY: i64 = 86_400_000;
fn hash(id: u8) -> String {
    format!("{id:064x}")
}
fn evaluation(day: i64, id: u8) -> MarketEvaluationViewV1 {
    let start = day * DAY;
    MarketEvaluationViewV1 {
        data: MarketDataViewV1 {
            features_sha256: hash(id),
            targets_sha256: hash(id + 20),
            view: SequenceViewV1 {
                history_start_ms: start,
                decision_start_ms: start + 59000,
                end_ms: start + 59000 + DAY + 30000,
                decision_stride_ms: 1000,
            },
        },
        replay_manifest_sha256: hash(id + 40),
    }
}
fn study() -> MarketEncoderStudyV1 {
    MarketEncoderStudyV1 {
        schema_version: MARKET_ENCODER_STUDY_SCHEMA.into(),
        study_id: "sol-encoder-engine-test".into(),
        input: SequenceInputSpecV1::sol_lob(),
        seeds: vec![7, 11],
        folds: [(1, 15, 1, 3), (2, 18, 2, 4)]
            .into_iter()
            .map(|(fold_id, day, id, val)| {
                let end = day * DAY;
                MarketEncoderFoldV1 {
                    fold_id,
                    train: MarketDataViewV1 {
                        features_sha256: hash(id),
                        targets_sha256: hash(id + 20),
                        view: SequenceViewV1 {
                            history_start_ms: end - 14 * DAY,
                            decision_start_ms: end - 14 * DAY + 59000,
                            end_ms: end,
                            decision_stride_ms: 5 * 3_600_000,
                        },
                    },
                    validation: evaluation(day + 1, val),
                }
            })
            .collect(),
        independent_selection: evaluation(22, 5),
        sealed: evaluation(25, 6),
        training: MarketEncoderTrainingV1 {
            hidden_channels: 16,
            batch_size: 64,
            pretraining_updates: 2,
            task_updates: 4,
            compute_control_updates: 6,
            learning_rate: 0.001,
            max_training_examples: 128,
        },
        max_primary_fits: 30,
        max_verification_fits: 30,
        max_cost_fen: 10000,
        costs: alpha_domain::EvaluationCostsV1 {
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
    }
}
fn write<T: Serialize>(
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
fn fixture(
    root: &Path,
    plan: &mut MarketEncoderStudyV1,
) -> (MarketFeatureDatasetV1, MarketTargetDatasetV1) {
    let view = plan.folds[0].train.view;
    let mut features = Vec::new();
    let mut targets = Vec::new();
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
                    .map(|c| i as f32 / 60.0 + c as f32 / 100.0)
                    .collect(),
            });
        }
        targets.push(MarketTargetFrameV1 {
            series_id: series,
            observed_at_ms: anchor,
            available_at_ms: anchor + 30000,
            simple_return: ((series % 3) as f32 - 1.0) * 0.001,
            spread_bps: 1.0,
        });
        anchor += view.decision_stride_ms;
    }
    let data = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: hash(90),
        input: plan.input.clone(),
        shards: vec![write(root, "features.jsonl", &features, |r| {
            r.observed_at_ms
        })],
    };
    let labels = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: data.digest().unwrap(),
        horizon_ms: 30000,
        shards: vec![write(root, "targets.jsonl", &targets, |r| r.observed_at_ms)],
    };
    plan.folds[0].train.features_sha256 = data.digest().unwrap();
    plan.folds[0].train.targets_sha256 = labels.digest().unwrap();
    plan.validate().unwrap();
    (data, labels)
}
fn key(
    kind: MarketTrainingStageKindV1,
    purpose: MarketTrainingStagePurposeV1,
) -> MarketTrainingStageKeyV1 {
    MarketTrainingStageKeyV1 {
        fold_id: 1,
        kind,
        seed: if kind == MarketTrainingStageKindV1::Ridge {
            0
        } else {
            7
        },
        purpose,
    }
}
fn fit(
    plan: &MarketEncoderStudyV1,
    root: &Path,
    data: &MarketFeatureDatasetV1,
    labels: &MarketTargetDatasetV1,
    key: MarketTrainingStageKeyV1,
    parent: Option<&VerifiedMarketStage<'_>>,
) -> Result<FittedMarketStage, String> {
    let mut features = MarketFeatureReader::open(
        root,
        data.clone(),
        &read_request(plan, &plan.folds[0].train),
    )?;
    if key.kind == MarketTrainingStageKindV1::Pretrain {
        fit_market_stage(plan, key, MarketStageInput::Features(&mut features), parent)
    } else {
        let mut reader = MarketTaskReader::open(features, root, labels.clone(), &labels.digest()?)?;
        fit_market_stage(plan, key, MarketStageInput::Targets(&mut reader), parent)
    }
}

#[test]
fn market_study_requires_verified_parent_and_restores_the_inherited_task() {
    use MarketTrainingStageKindV1::*;
    use MarketTrainingStagePurposeV1::*;
    let root = tempfile::tempdir().unwrap();
    let mut plan = study();
    let (data, labels) = fixture(root.path(), &mut plan);
    let p = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(Pretrain, Primary),
        None,
    )
    .unwrap();
    let pv = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(Pretrain, Verification),
        None,
    )
    .unwrap();
    let verified = verify_market_stage_pair(&p, &pv).unwrap();
    assert!(fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(FineTune, Primary),
        None
    )
    .is_err());
    let c = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(FineTune, Primary),
        Some(&verified),
    )
    .unwrap();
    let cv = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(FineTune, Verification),
        Some(&verified),
    )
    .unwrap();
    let verified_c = verify_market_stage_pair(&c, &cv).unwrap();
    // One seed is insufficient even when that seed has passed independent refit.
    assert!(MarketStudyEnsemble::new(&plan, vec![&verified_c]).is_err());
    let key11 = |kind, purpose| MarketTrainingStageKeyV1 {
        seed: 11,
        ..key(kind, purpose)
    };
    let p11 = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key11(Pretrain, Primary),
        None,
    )
    .unwrap();
    let pv11 = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key11(Pretrain, Verification),
        None,
    )
    .unwrap();
    let verified11 = verify_market_stage_pair(&p11, &pv11).unwrap();
    assert!(parent_encoder(&plan, key11(FineTune, Primary), Some(&verified)).is_err());
    let c11 = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key11(FineTune, Primary),
        Some(&verified11),
    )
    .unwrap();
    let cv11 = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key11(FineTune, Verification),
        Some(&verified11),
    )
    .unwrap();
    let verified_c11 = verify_market_stage_pair(&c11, &cv11).unwrap();
    let ensemble = MarketStudyEnsemble::new(&plan, vec![&verified_c11, &verified_c]).unwrap();
    let input = MarketFeatureReader::open(
        root.path(),
        data.clone(),
        &read_request(&plan, &plan.folds[0].train),
    )
    .unwrap()
    .next_batch(1)
    .unwrap()
    .remove(0)
    .inputs;
    assert_eq!(
        ensemble.predict(&input).unwrap(),
        (c.predict(&input).unwrap() + c11.predict(&input).unwrap()) / 2.0
    );
    assert!(p.predict(&input).is_err());
    assert!(MarketStudyEnsemble::new(&plan, vec![&verified]).is_err());
    let (manifest, weights) = c.bundle().unwrap();
    let restored = FittedMarketStage::restore(
        &plan,
        key(FineTune, Primary),
        &manifest,
        &bytes_digest(&manifest),
        weights.clone(),
        Some(&verified),
    )
    .unwrap();
    assert_eq!(
        restored.fitted_values_digest().unwrap(),
        c.fitted_values_digest().unwrap()
    );
    assert!(FittedMarketStage::restore(
        &plan,
        key(FineTune, Primary),
        &manifest,
        &bytes_digest(&manifest),
        weights,
        None
    )
    .is_err());
    let mut wrong = plan.clone();
    wrong.costs.fee_bps += 1.0;
    assert!(parent_encoder(&wrong, key(FineTune, Primary), Some(&verified)).is_err());
    assert!(verify_market_stage_pair(&p, &cv).is_err());
}

#[test]
fn market_study_ridge_roundtrip_rejects_changed_groups_costs_and_duplicate_seed() {
    use MarketTrainingStageKindV1::*;
    use MarketTrainingStagePurposeV1::*;
    let root = tempfile::tempdir().unwrap();
    let mut plan = study();
    let (data, labels) = fixture(root.path(), &mut plan);
    let validation_root = root.path().join("validation");
    std::fs::create_dir(&validation_root).unwrap();
    let view = plan.folds[0].validation.data.view;
    let validation_rows = (0..90)
        .map(|i| MarketFeatureFrameV1 {
            series_id: 999,
            observed_at_ms: view.history_start_ms + i * 1000,
            feature_max_available_at_ms: view.history_start_ms + i * 1000,
            channels: vec![0.0; 24],
        })
        .collect::<Vec<_>>();
    let validation_targets = validation_rows
        .iter()
        .map(|r| MarketTargetFrameV1 {
            series_id: r.series_id,
            observed_at_ms: r.observed_at_ms,
            available_at_ms: r.observed_at_ms + 30000,
            simple_return: 0.0,
            spread_bps: 1.0,
        })
        .collect::<Vec<_>>();
    let validation_data = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: hash(91),
        input: plan.input.clone(),
        shards: vec![write(
            &validation_root,
            "features.jsonl",
            &validation_rows,
            |r| r.observed_at_ms,
        )],
    };
    let validation_labels = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: validation_data.digest().unwrap(),
        horizon_ms: 30000,
        shards: vec![write(
            &validation_root,
            "targets.jsonl",
            &validation_targets,
            |r| r.observed_at_ms,
        )],
    };
    plan.folds[0].validation.data.features_sha256 = validation_data.digest().unwrap();
    plan.folds[0].validation.data.targets_sha256 = validation_labels.digest().unwrap();
    let p = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(Ridge, Primary),
        None,
    )
    .unwrap();
    let v = fit(
        &plan,
        root.path(),
        &data,
        &labels,
        key(Ridge, Verification),
        None,
    )
    .unwrap();
    let verified = verify_market_stage_pair(&p, &v).unwrap();
    assert!(parent_encoder(&plan, key(FineTune, Primary), Some(&verified)).is_err());
    let ensemble = MarketStudyEnsemble::new(&plan, vec![&verified]).unwrap();
    let mut row = MarketPredictionV1 {
        observed_at_ms: plan.folds[0].validation.data.view.decision_start_ms,
        spread_bps: 1.0,
        predicted_return: 0.001,
        observed_return: 0.0,
    };
    let decision = ensemble.entry_decision(&plan, &row).unwrap();
    assert_eq!(decision.entry_target, Some(1.0));
    assert_eq!(decision.round_trip_gate_bps, 6.0);
    row.observed_return = -999.0;
    assert_eq!(decision, ensemble.entry_decision(&plan, &row).unwrap());
    row.predicted_return = 0.0005;
    assert_eq!(
        ensemble.entry_decision(&plan, &row).unwrap().entry_target,
        None
    );
    row.predicted_return = 0.001;
    row.observed_at_ms = plan.folds[0].validation.data.view.end_ms - 30000;
    assert_eq!(
        ensemble.entry_decision(&plan, &row).unwrap().entry_target,
        None
    );

    let sample = (0..60)
        .flat_map(|i| (0..24).map(move |c| i as f32 / 60.0 + c as f32 / 100.0))
        .collect::<Vec<_>>();
    let expected = ensemble.predict(&sample).unwrap();
    let mut validation_reader = MarketTaskReader::open(
        MarketFeatureReader::open(
            &validation_root,
            validation_data,
            &read_request(&plan, &plan.folds[0].validation.data),
        )
        .unwrap(),
        &validation_root,
        validation_labels.clone(),
        &validation_labels.digest().unwrap(),
    )
    .unwrap();
    let coverage =
        predict_market_validation(&plan, &ensemble, &mut validation_reader, |_| Ok(())).unwrap();
    assert_eq!(coverage.expected, 86400);
    assert_eq!(coverage.emitted, 31);
    assert!(!coverage.complete);
    let mut wrong_view = MarketTaskReader::open(
        MarketFeatureReader::open(
            root.path(),
            data.clone(),
            &read_request(&plan, &plan.folds[0].train),
        )
        .unwrap(),
        root.path(),
        labels.clone(),
        &labels.digest().unwrap(),
    )
    .unwrap();
    assert!(predict_market_validation(&plan, &ensemble, &mut wrong_view, |_| Ok(())).is_err());
    let (metadata, weights) = p.bundle().unwrap();
    let restored = FittedMarketStage::restore(
        &plan,
        key(Ridge, Primary),
        &metadata,
        &bytes_digest(&metadata),
        weights.clone(),
        None,
    )
    .unwrap();
    assert_eq!(restored.predict(&sample).unwrap(), expected);
    assert!(MarketStudyEnsemble::new(&plan, vec![&verified, &verified]).is_err());
    assert!(FittedMarketStage::restore(
        &plan,
        key(Scratch, Primary),
        &metadata,
        &bytes_digest(&metadata),
        weights.clone(),
        None
    )
    .is_err());
    let mut changed = plan;
    changed.costs.fee_bps += 1.0;
    assert!(FittedMarketStage::restore(
        &changed,
        key(Ridge, Primary),
        &metadata,
        &bytes_digest(&metadata),
        weights,
        None
    )
    .is_err());
}
