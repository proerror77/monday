use super::{
    data::*,
    training::{input_tensor, masked_positions},
    *,
};
use crate::{lock_ndarray_backend, CpuBackend};
use hft_research_manifest::{
    market_encoder::*,
    sequence::{SequenceInputSpecV1, SequenceShardV1, SequenceViewV1},
};
use std::path::Path;

fn shard<T: serde::Serialize>(
    root: &Path,
    name: &str,
    rows: &[T],
    first: i64,
    last: i64,
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
        first_observed_at_ms: first,
        last_observed_at_ms: last,
    }
}
fn fixture(
    root: &Path,
) -> (
    MarketFeatureDatasetV1,
    MarketTargetDatasetV1,
    MarketFitRequestV1,
) {
    let rows = (0..150)
        .map(|i| MarketFeatureFrameV1 {
            series_id: 1,
            observed_at_ms: i * 1000,
            feature_max_available_at_ms: i * 1000,
            channels: vec![(i as f32 / 10.0).sin(), (i as f32 / 10.0).cos()],
        })
        .collect::<Vec<_>>();
    let targets = rows[..123]
        .iter()
        .map(|r| MarketTargetFrameV1 {
            series_id: r.series_id,
            observed_at_ms: r.observed_at_ms,
            available_at_ms: r.observed_at_ms + 30000,
            simple_return: r.channels[0] * 0.002,
            spread_bps: 1.0,
        })
        .collect::<Vec<_>>();
    let features = MarketFeatureDatasetV1 {
        schema_version: FEATURE_SCHEMA.into(),
        venue: "binance-usdm".into(),
        symbol: "SOLUSDT".into(),
        source_manifest_sha256: "a".repeat(64),
        input: SequenceInputSpecV1 {
            ordered_channels: vec!["a".into(), "b".into()],
            context_rows: 60,
            bucket_ms: 1000,
        },
        shards: vec![shard(root, "features.jsonl", &rows, 0, 149000)],
    };
    let labels = MarketTargetDatasetV1 {
        schema_version: TARGET_SCHEMA.into(),
        feature_dataset_sha256: features.digest().unwrap(),
        horizon_ms: 30000,
        shards: vec![shard(root, "targets.jsonl", &targets, 0, 122000)],
    };
    let fit = MarketFitRequestV1 {
        feature_dataset_sha256: features.digest().unwrap(),
        qualified_anchors_sha256: None,
        spec: MarketEncoderSpecV1 {
            input: features.input.clone(),
            hidden_channels: 8,
        },
        view: SequenceViewV1 {
            history_start_ms: 0,
            decision_start_ms: 59000,
            end_ms: 154000,
            decision_stride_ms: 1000,
        },
        anchor_end_ms: 123000,
        seed: 7,
        batch_size: 16,
        updates: 80,
        learning_rate: 0.003,
        min_examples: 64,
        max_examples: 64,
    };
    (features, labels, fit)
}
fn reader(
    root: &Path,
    data: &MarketFeatureDatasetV1,
    fit: &MarketFitRequestV1,
) -> MarketFeatureReader {
    MarketFeatureReader::open(root, data.clone(), &fit.read_request()).unwrap()
}
fn task_reader(
    root: &Path,
    data: &MarketFeatureDatasetV1,
    labels: &MarketTargetDatasetV1,
    fit: &MarketFitRequestV1,
) -> MarketTaskReader {
    MarketTaskReader::open(
        reader(root, data, fit),
        root,
        labels.clone(),
        &labels.digest().unwrap(),
    )
    .unwrap()
}

#[test]
fn market_encoder_two_stage_inheritance_freeze_learning_and_roundtrip() {
    let root = tempfile::tempdir().unwrap();
    let (features, targets, fit) = fixture(root.path());
    let parent =
        pretrain_market_encoder(&mut reader(root.path(), &features, &fit), fit.clone()).unwrap();
    assert_ne!(
        parent.diagnostics().initial_encoder_values_sha256,
        parent.diagnostics().final_encoder_values_sha256
    );
    let losses = &parent.diagnostics().losses;
    assert!(
        losses[60..].iter().sum::<f64>() < losses[..20].iter().sum::<f64>(),
        "pretraining did not learn the controlled signal: {losses:?}"
    );
    let (manifest, weights) = parent.bundle().unwrap();
    assert_eq!(parent.identity().unwrap(), bytes_digest(&manifest));
    let restored =
        MarketEncoderCheckpoint::restore(&manifest, &bytes_digest(&manifest), weights.clone())
            .unwrap();
    assert_eq!(parent.diagnostics(), restored.diagnostics());
    assert_eq!(parent.identity().unwrap(), restored.identity().unwrap());
    assert!(restored.bundle().is_ok());
    let (audit, head) = parent.reconstruction_bundle().unwrap().unwrap();
    let mut audited = MarketEncoderCheckpoint::restore(
        &manifest,
        &bytes_digest(&manifest),
        restored.bundle().unwrap().1,
    )
    .unwrap();
    audited
        .attach_reconstruction_audit(&audit, &bytes_digest(&audit), head.clone())
        .unwrap();
    assert_eq!(
        audited.reconstruction_parameter_digest(),
        parent.reconstruction_parameter_digest()
    );
    let mut corrupt = head;
    corrupt[0] ^= 1;
    assert!(audited
        .attach_reconstruction_audit(&audit, &bytes_digest(&audit), corrupt)
        .is_err());
    let sample = reader(root.path(), &features, &fit)
        .next_batch(1)
        .unwrap()
        .remove(0)
        .inputs;
    assert_eq!(
        parent.encode(&sample).unwrap(),
        restored.encode(&sample).unwrap()
    );
    let mut damaged = weights;
    damaged[0] ^= 1;
    assert!(
        MarketEncoderCheckpoint::restore(&manifest, &bytes_digest(&manifest), damaged).is_err()
    );
    for mode in [
        AdaptationModeV1::Scratch,
        AdaptationModeV1::LinearProbe,
        AdaptationModeV1::FullFineTune,
    ] {
        let p = if mode == AdaptationModeV1::Scratch {
            None
        } else {
            Some(&restored)
        };
        let request = MarketAdaptationRequestV1 {
            fit: fit.clone(),
            target_dataset_sha256: targets.digest().unwrap(),
            mode,
            parent_checkpoint_sha256: p.map(|p| p.identity().unwrap()),
            head_seed: 101,
        };
        let trained = adapt_market_encoder(
            &mut task_reader(root.path(), &features, &targets, &fit),
            request.clone(),
            p,
        )
        .unwrap();
        if let Some(p) = p {
            assert_eq!(
                trained.diagnostics().initial_encoder_values_sha256,
                p.diagnostics().final_encoder_values_sha256
            );
        }
        if mode == AdaptationModeV1::LinearProbe {
            assert_eq!(
                trained.diagnostics().initial_encoder_values_sha256,
                trained.diagnostics().final_encoder_values_sha256
            );
        } else {
            assert_ne!(
                trained.diagnostics().initial_encoder_values_sha256,
                trained.diagnostics().final_encoder_values_sha256
            );
        }
        let losses = &trained.diagnostics().losses;
        assert!(
            losses[60..].iter().sum::<f64>() < losses[..20].iter().sum::<f64>(),
            "task did not learn {mode:?}"
        );
        let input = reader(root.path(), &features, &fit)
            .next_batch(1)
            .unwrap()
            .remove(0)
            .inputs;
        let prediction = trained.predict(&input).unwrap();
        let (meta, weights) = trained.bundle().unwrap();
        let model =
            MarketTaskModel::restore(&meta, &bytes_digest(&meta), weights.clone(), p).unwrap();
        assert_eq!(prediction, model.predict(&input).unwrap());
        assert_eq!(trained.parameter_digest(), model.parameter_digest());
        if p.is_some() {
            assert!(MarketTaskModel::restore(&meta, &bytes_digest(&meta), weights, None).is_err());
        }
        let repeated = adapt_market_encoder(
            &mut task_reader(root.path(), &features, &targets, &fit),
            request,
            p,
        )
        .unwrap();
        assert_eq!(trained.parameter_digest(), repeated.parameter_digest());
    }
}

#[test]
fn market_encoder_features_are_label_free_and_scaling_counts_unique_frames() {
    let root = tempfile::tempdir().unwrap();
    let (features, _, fit) = fixture(root.path());
    std::fs::remove_file(root.path().join("targets.jsonl")).unwrap();
    let mut r = reader(root.path(), &features, &fit);
    let scaling = fit_market_scaling(&mut r, fit.min_examples, fit.max_examples).unwrap();
    assert_eq!(scaling.unique_frames, 123);
    assert_eq!(scaling.examples, 64);
    let expected = (0..123)
        .map(|i| f64::from((i as f32 / 10.0).sin()))
        .sum::<f64>()
        / 123.0;
    assert!((scaling.means[0] - expected).abs() < 1e-12);
    let mut row = serde_json::json!({"series_id":1,"observed_at_ms":0,"feature_max_available_at_ms":0,"channels":[1.0,2.0]});
    row["forward_returns"] = serde_json::json!([9.0]);
    assert!(serde_json::from_value::<MarketFeatureFrameV1>(row).is_err());
    let parent = pretrain_market_encoder(&mut r, fit).unwrap();
    assert_eq!(parent.scaling(), &scaling);
}

#[test]
fn market_encoder_masks_every_channel_and_excludes_visible_loss_shortcut() {
    let root = tempfile::tempdir().unwrap();
    let (features, _, fit) = fixture(root.path());
    let mut r = reader(root.path(), &features, &fit);
    let scaling = fit_market_scaling(&mut r, fit.min_examples, fit.max_examples).unwrap();
    let item = r.next_batch(1).unwrap().remove(0);
    let mask = masked_positions(fit.seed, 0, &item);
    assert_eq!(mask.iter().filter(|v| **v).count(), 18);
    assert!(mask[..6].iter().all(|v| !*v));
    let mut changed = item.clone();
    for (i, m) in mask.iter().enumerate() {
        if *m {
            changed.inputs[i * 2] = 9000.0;
            changed.inputs[i * 2 + 1] = -9000.0;
        }
    }
    let _guard = lock_ndarray_backend().unwrap();
    let x = input_tensor::<CpuBackend>(&[item], &fit, &scaling, Some(0))
        .unwrap()
        .into_data()
        .into_vec::<f32>()
        .unwrap();
    let y = input_tensor::<CpuBackend>(&[changed], &fit, &scaling, Some(0))
        .unwrap()
        .into_data()
        .into_vec::<f32>()
        .unwrap();
    assert_eq!(x, y);
    for (i, m) in mask.iter().enumerate() {
        assert_eq!(x[120 + i], if *m { 1.0 } else { 0.0 });
    }
}

#[test]
fn market_encoder_rejects_future_views_tampering_and_wrong_parent() {
    let root = tempfile::tempdir().unwrap();
    let (features, targets, fit) = fixture(root.path());
    let mut future = fit.clone();
    future.view.end_ms = 149000;
    assert!(
        MarketFeatureReader::open(root.path(), features.clone(), &future.read_request()).is_err()
    );
    let parent =
        pretrain_market_encoder(&mut reader(root.path(), &features, &fit), fit.clone()).unwrap();
    let mut request = MarketAdaptationRequestV1 {
        fit: fit.clone(),
        target_dataset_sha256: targets.digest().unwrap(),
        mode: AdaptationModeV1::FullFineTune,
        parent_checkpoint_sha256: Some("b".repeat(64)),
        head_seed: 101,
    };
    assert!(adapt_market_encoder(
        &mut task_reader(root.path(), &features, &targets, &fit),
        request.clone(),
        Some(&parent)
    )
    .is_err());
    request.parent_checkpoint_sha256 = Some(parent.identity().unwrap());
    request.fit.seed = 11;
    assert!(adapt_market_encoder(
        &mut task_reader(root.path(), &features, &targets, &request.fit),
        request.clone(),
        Some(&parent)
    )
    .is_err());
    let mut r = reader(root.path(), &features, &fit);
    let path = root.path().join("features.jsonl");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] = b'!';
    std::fs::write(path, bytes).unwrap();
    assert!(r.next_batch(1).is_err());
}

#[test]
fn market_encoder_gaps_reset_context_and_targets_cannot_skip_common_anchors() {
    let root = tempfile::tempdir().unwrap();
    let (mut features, mut targets, mut fit) = fixture(root.path());
    let text = std::fs::read_to_string(root.path().join("features.jsonl")).unwrap();
    let rows = text
        .lines()
        .map(|l| serde_json::from_str::<MarketFeatureFrameV1>(l).unwrap())
        .filter(|r| r.observed_at_ms != 70000)
        .collect::<Vec<_>>();
    features.shards = vec![shard(root.path(), "features.jsonl", &rows, 0, 149000)];
    fit.feature_dataset_sha256 = features.digest().unwrap();
    let mut r = reader(root.path(), &features, &fit);
    let anchors = r.next_batch(256).unwrap();
    assert_eq!(anchors.len(), 11);
    assert_eq!(anchors.last().unwrap().observed_at_ms, 69000);
    assert!(fit_market_scaling(
        &mut reader(root.path(), &features, &fit),
        fit.min_examples,
        fit.max_examples
    )
    .is_err());
    let (features, _, fit) = fixture(root.path());
    let text = std::fs::read_to_string(root.path().join("targets.jsonl")).unwrap();
    let mut rows = text
        .lines()
        .map(|l| serde_json::from_str::<MarketTargetFrameV1>(l).unwrap())
        .collect::<Vec<_>>();
    rows.retain(|r| r.observed_at_ms != 60000);
    targets.shards = vec![shard(root.path(), "targets.jsonl", &rows, 0, 122000)];
    assert!(task_reader(root.path(), &features, &targets, &fit)
        .next_batch(16)
        .is_err());
    rows[0].available_at_ms = 999000;
    targets.shards = vec![shard(root.path(), "targets.jsonl", &rows, 0, 122000)];
    assert!(task_reader(root.path(), &features, &targets, &fit)
        .next_batch(1)
        .is_err());
}

#[test]
fn market_encoder_rejects_fine_tuning_that_only_updates_the_head() {
    use burn::{
        module::{Module, ModuleMapper, Param},
        tensor::Tensor,
    };
    struct Zero;
    impl ModuleMapper<CpuBackend> for Zero {
        fn map_float<const D: usize>(
            &mut self,
            p: Param<Tensor<CpuBackend, D>>,
        ) -> Param<Tensor<CpuBackend, D>> {
            p.map(|t| Tensor::zeros(t.dims(), &t.device()))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let (features, targets, mut fit) = fixture(root.path());
    fit.updates = 2;
    fit.batch_size = 64;
    let scaling = fit_market_scaling(
        &mut reader(root.path(), &features, &fit),
        fit.min_examples,
        fit.max_examples,
    )
    .unwrap();
    let parent = {
        let _guard = lock_ndarray_backend().unwrap();
        let model =
            network::Encoder::<CpuBackend>::new(&fit.spec, &burn_ndarray::NdArrayDevice::Cpu)
                .map(&mut Zero);
        let weights = network::save(&model).unwrap();
        let hash = network::values_digest(&model).unwrap();
        MarketEncoderCheckpoint {
            model,
            weights: weights.clone(),
            reconstruction: None,
            manifest: artifacts::EncoderManifest {
                schema_version: ENCODER_SCHEMA.into(),
                request: fit.clone(),
                mask_policy: artifacts::MASK_POLICY.into(),
                scaling,
                diagnostics: MarketFitDiagnosticsV1 {
                    updates: 2,
                    example_visits: 128,
                    losses: vec![1.0; 2],
                    gradient_norms: vec![0.0; 2],
                    initial_encoder_values_sha256: hash.clone(),
                    final_encoder_values_sha256: hash,
                },
                weights_sha256: bytes_digest(&weights),
            },
        }
    };
    let request = MarketAdaptationRequestV1 {
        fit: fit.clone(),
        target_dataset_sha256: targets.digest().unwrap(),
        mode: AdaptationModeV1::FullFineTune,
        parent_checkpoint_sha256: Some(parent.identity().unwrap()),
        head_seed: 101,
    };
    let error = adapt_market_encoder(
        &mut task_reader(root.path(), &features, &targets, &fit),
        request,
        Some(&parent),
    )
    .err()
    .unwrap();
    assert!(error.contains("without updating the encoder"), "{error}");
}

#[test]
fn market_encoder_data_views_do_not_invent_an_optimizer_budget() {
    let root = tempfile::tempdir().unwrap();
    let (features, _, mut fit) = fixture(root.path());
    fit.updates = 0;
    assert!(fit.validate().is_err());
    let mut view = fit.read_request();
    view.anchor_end_ms = 150000;
    let mut r = MarketFeatureReader::open(root.path(), features.clone(), &view).unwrap();
    assert_eq!(r.next_batch(256).unwrap().len(), 91);
    // Read access cannot be promoted into an unbudgeted fit.
    let mut r = MarketFeatureReader::open(root.path(), features, &fit.read_request()).unwrap();
    assert!(pretrain_market_encoder(&mut r, fit).is_err());
}

#[test]
fn market_qualified_anchors_ignore_label_values_and_survive_missing_label_file() {
    let root = tempfile::tempdir().unwrap();
    let (features, mut targets, mut fit) = fixture(root.path());
    let text = std::fs::read_to_string(root.path().join("targets.jsonl")).unwrap();
    let mut rows = text
        .lines()
        .map(|l| serde_json::from_str::<MarketTargetFrameV1>(l).unwrap())
        .filter(|r| r.observed_at_ms != 60000)
        .collect::<Vec<_>>();
    targets.shards = vec![shard(root.path(), "targets.jsonl", &rows, 0, 122000)];
    let anchors = derive_market_training_anchors(
        &mut reader(root.path(), &features, &fit),
        root.path(),
        targets.clone(),
        &targets.digest().unwrap(),
    )
    .unwrap();
    assert_eq!(anchors.anchors.len(), 63);
    assert!(anchors.anchors.iter().all(|a| a.observed_at_ms != 60000));
    for row in &mut rows {
        row.simple_return = -row.simple_return + 0.1;
    }
    targets.shards = vec![shard(root.path(), "targets.jsonl", &rows, 0, 122000)];
    let changed = derive_market_training_anchors(
        &mut reader(root.path(), &features, &fit),
        root.path(),
        targets.clone(),
        &targets.digest().unwrap(),
    )
    .unwrap();
    assert_eq!(anchors, changed);
    let hash = anchors.digest().unwrap();
    std::fs::write(
        root.path().join(format!("{hash}.market-anchors.json")),
        serde_json::to_vec(&anchors).unwrap(),
    )
    .unwrap();
    fit.qualified_anchors_sha256 = Some(hash);
    fit.min_examples = 2;
    fit.updates = 3;
    fit.batch_size = 32;
    std::fs::remove_file(root.path().join("targets.jsonl")).unwrap();
    let checkpoint =
        pretrain_market_encoder(&mut reader(root.path(), &features, &fit), fit.clone()).unwrap();
    assert_eq!(checkpoint.scaling().examples, 63);
    let mut invalid = anchors;
    invalid.anchors[0].series_id = 999;
    let hash = invalid.digest().unwrap();
    std::fs::write(
        root.path().join(format!("{hash}.market-anchors.json")),
        serde_json::to_vec(&invalid).unwrap(),
    )
    .unwrap();
    fit.qualified_anchors_sha256 = Some(hash);
    assert!(reader(root.path(), &features, &fit).next_batch(1).is_err());
}

fn sol_diagnostic_fixture(root: &Path) -> (MarketFeatureDatasetV1, MarketFitRequestV1) {
    let (mut features, mut targets, mut fit) = fixture(root);
    let mut rows = std::fs::read_to_string(root.join("features.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<MarketFeatureFrameV1>(line).unwrap())
        .collect::<Vec<_>>();
    for row in &mut rows {
        row.channels = (0..24)
            .map(|c| row.channels[c % 2] * (1.0 + c as f32 / 100.0))
            .collect();
    }
    features.input = SequenceInputSpecV1::sol_lob();
    features.shards = vec![shard(root, "features.jsonl", &rows, 0, 149000)];
    targets.feature_dataset_sha256 = features.digest().unwrap();
    fit.feature_dataset_sha256 = features.digest().unwrap();
    fit.spec.input = features.input.clone();
    fit.spec.hidden_channels = 4;
    fit.batch_size = 64;
    fit.updates = 1;
    let anchors = derive_market_training_anchors(
        &mut reader(root, &features, &fit),
        root,
        targets.clone(),
        &targets.digest().unwrap(),
    )
    .unwrap();
    let hash = anchors.digest().unwrap();
    std::fs::write(
        root.join(format!("{hash}.market-anchors.json")),
        serde_json::to_vec(&anchors).unwrap(),
    )
    .unwrap();
    fit.qualified_anchors_sha256 = Some(hash);
    // The entire P fit and its final diagnostic must survive absent label bytes.
    std::fs::remove_file(root.join("targets.jsonl")).unwrap();
    (features, fit)
}

#[test]
fn market_reconstruction_diagnostic_roundtrip_is_bounded_label_free_and_auxiliary() {
    let root = tempfile::tempdir().unwrap();
    let (features, fit) = sol_diagnostic_fixture(root.path());
    let parent =
        pretrain_market_encoder(&mut reader(root.path(), &features, &fit), fit.clone()).unwrap();
    assert_eq!(parent.diagnostics().updates, 1);
    assert_eq!(parent.diagnostics().example_visits, 64);
    let (audit, head) = parent.reconstruction_bundle().unwrap().unwrap();
    let meta: artifacts::ReconstructionAuditManifest = serde_json::from_slice(&audit).unwrap();
    let report = meta.diagnostics.as_ref().unwrap();
    assert_eq!(report.sampled_ordinals, (0..64).collect::<Vec<_>>());
    assert_eq!(report.cpu_forward_examples, 64);
    assert_eq!(report.cpu_forward_batches, 1);
    assert_eq!(report.cpu_output_scalars, 64 * 60 * 24);
    assert_eq!(report.additional_optimizer_updates, 0);
    assert_eq!(
        report.groups.iter().map(|g| g.channels).collect::<Vec<_>>(),
        [11, 10, 3]
    );
    assert!(meta.diagnostic_elapsed_micros.is_some());
    report.validate(&fit, parent.scaling()).unwrap();
    let (encoder, weights) = parent.bundle().unwrap();
    let encoder_json: serde_json::Value = serde_json::from_slice(&encoder).unwrap();
    assert!(encoder_json.get("reconstruction").is_none());
    assert!(encoder_json.get("diagnostic_elapsed_micros").is_none());
    let mut restored =
        MarketEncoderCheckpoint::restore(&encoder, &bytes_digest(&encoder), weights).unwrap();
    assert!(restored
        .reconstruction_diagnostics_digest()
        .unwrap()
        .is_none());
    restored
        .attach_reconstruction_audit(&audit, &bytes_digest(&audit), head.clone())
        .unwrap();
    assert_eq!(
        parent.reconstruction_diagnostics_digest(),
        restored.reconstruction_diagnostics_digest()
    );
    assert_eq!(parent.identity(), restored.identity());
    let mut changed = meta.clone();
    changed.diagnostic_elapsed_micros = Some(meta.diagnostic_elapsed_micros.unwrap() + 1);
    let changed = serde_json::to_vec(&changed).unwrap();
    restored
        .attach_reconstruction_audit(&changed, &bytes_digest(&changed), head.clone())
        .unwrap();
    assert_eq!(
        parent.reconstruction_diagnostics_digest(),
        restored.reconstruction_diagnostics_digest()
    );
    for tamper in ["sample", "count", "metric"] {
        let mut changed = meta.clone();
        let report = changed.diagnostics.as_mut().unwrap();
        match tamper {
            "sample" => report.sampled_ordinals[1] = 0,
            "count" => report.groups[0].masked_scalar_count += 1,
            _ => report.groups[0].model_mse = -1.0,
        }
        let changed = serde_json::to_vec(&changed).unwrap();
        assert!(restored
            .attach_reconstruction_audit(&changed, &bytes_digest(&changed), head.clone())
            .is_err());
    }
    let mut changed = meta;
    changed.diagnostics.as_mut().unwrap().groups[0].model_mse += 0.5;
    let changed = serde_json::to_vec(&changed).unwrap();
    assert!(restored
        .attach_reconstruction_audit(&changed, &bytes_digest(&audit), head)
        .is_err());
}

#[test]
fn market_reconstruction_diagnostic_uses_registry_groups_and_holds_across_masked_runs() {
    let spec = SequenceInputSpecV1::sol_lob();
    let groups = artifacts::reconstruction_channel_groups(&spec).unwrap();
    assert_eq!(groups[0], vec![0, 1, 3, 5, 7, 9, 11, 13, 15, 17, 19]);
    assert_eq!(groups[1], vec![2, 4, 6, 8, 10, 12, 14, 16, 18, 20]);
    assert_eq!(groups[2], vec![21, 22, 23]);
    let mut bad = spec;
    bad.ordered_channels.swap(0, 1);
    assert!(artifacts::reconstruction_channel_groups(&bad).is_none());
    let scaling = MarketFeatureScalingV1 {
        means: (0..24).map(|i| i as f64).collect(),
        scales: vec![2.0; 24],
        unique_frames: 60,
        examples: 2,
    };
    let mut item = UnlabeledSequenceExample {
        series_id: 1,
        observed_at_ms: 59000,
        inputs: vec![0.0; 60 * 24],
    };
    let mut prediction = vec![0.0; 60 * 24];
    let mut mask = [false; 60];
    mask[6..24].fill(true);
    for (group, channels) in groups.iter().enumerate() {
        let step = (group + 1) as f32;
        for time in 0..60 {
            for &channel in channels {
                item.inputs[time * 24 + channel] = 2.0 * time as f32 * step + channel as f32;
                prediction[time * 24 + channel] = if mask[time] {
                    time as f32 * step + step
                } else {
                    99999.0
                };
            }
        }
    }
    let mut sums: [training::ReconstructionMse; 3] = Default::default();
    training::observe_reconstruction(&item, &mask, &prediction, &scaling, &groups, &mut sums)
        .unwrap();
    for (i, (sum, channels)) in sums.iter().zip(&groups).enumerate() {
        let report = sum
            .finish(["price", "depth", "trade"][i], channels.len())
            .unwrap();
        let squared = ((i + 1) * (i + 1)) as f64;
        assert_eq!(report.masked_scalar_count, 18 * channels.len() as u64);
        assert!((report.model_mse - squared).abs() < 1e-12);
        assert!(
            (report.last_visible_mse
                - (1..=18)
                    .map(|distance| (distance * distance) as f64)
                    .sum::<f64>()
                    / 18.0
                    * squared)
                .abs()
                < 1e-10
        );
    }
    assert!(training::observe_reconstruction(
        &item,
        &mask,
        &prediction[..1439],
        &scaling,
        &groups,
        &mut sums
    )
    .is_err());
    let selected = artifacts::diagnostic_ordinals(10_000);
    assert_eq!(selected.len(), 256);
    assert_eq!(selected[0], 0);
    assert_eq!(selected[255], 9999);
    assert!(selected
        .windows(2)
        .all(|pair| (39..=40).contains(&(pair[1] - pair[0]))));
    assert_eq!(artifacts::diagnostic_ordinals(3), vec![0, 1, 2]);
}
