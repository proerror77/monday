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
