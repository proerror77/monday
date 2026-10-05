//! Scientific sequence model training. Immutable readers live in hft-cex-research-input.
pub mod training;

#[cfg(test)]
mod tests {
    use hft_cex_research_input::sequence::SequenceReader;
    use hft_research_manifest::sequence::{
        SequenceDatasetV1, SequenceFrameV1, SequenceInputSpecV1, SequenceShardV1, SequenceViewV1,
        SEQUENCE_DATASET_SCHEMA,
    };
    use sha2::{Digest, Sha256};
    use std::path::Path;
    fn frames() -> Vec<SequenceFrameV1> {
        (0..100)
            .map(|i| SequenceFrameV1 {
                series_id: 0,
                observed_at_ms: i * 1000,
                feature_max_available_at_ms: i * 1000,
                spread_bps: 1.0,
                channels: vec![i as f32],
                forward_returns: [i as f32 / 10_000.0; 3],
                label_available_at_ms: [i * 1000 + 5000, i * 1000 + 10000, i * 1000 + 30000],
            })
            .collect()
    }

    fn dataset(root: &Path, rows: &[SequenceFrameV1]) -> SequenceDatasetV1 {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(root.join("part.jsonl"), &bytes).unwrap();
        SequenceDatasetV1 {
            schema_version: SEQUENCE_DATASET_SCHEMA.into(),
            venue: "binance-usdm".into(),
            symbol: "SOLUSDT".into(),
            source_manifest_sha256: "a".repeat(64),
            input: SequenceInputSpecV1 {
                ordered_channels: vec!["return_1s".into()],
                context_rows: 3,
                bucket_ms: 1000,
            },
            shards: vec![SequenceShardV1 {
                file: "part.jsonl".into(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                bytes: bytes.len() as u64,
                rows: rows.len() as u64,
                first_observed_at_ms: rows[0].observed_at_ms,
                last_observed_at_ms: rows.last().unwrap().observed_at_ms,
            }],
        }
    }

    fn reader(root: &Path, rows: &[SequenceFrameV1], end_ms: i64) -> SequenceReader {
        let manifest = dataset(root, rows);
        let digest = manifest.digest().unwrap();
        SequenceReader::open(
            root,
            manifest,
            &digest,
            SequenceViewV1 {
                history_start_ms: 0,
                decision_start_ms: 0,
                end_ms,
                decision_stride_ms: 1000,
            },
        )
        .unwrap()
    }

    #[test]
    fn sequence_tcn_trains_reproducibly_and_roundtrips_raw_returns() {
        use super::training::{train_sequence_model, TrainedSequenceModel};
        use hft_research_manifest::portable_sequence::{
            SequenceNeuralKindV1, SequenceTrainingRequestV1,
        };
        let dir = tempfile::tempdir().unwrap();
        let rows = frames();
        let mut data = reader(dir.path(), &rows, 60000);
        let request = SequenceTrainingRequestV1 {
            model_kind: SequenceNeuralKindV1::Tcn,
            dataset_sha256: data.dataset_digest().unwrap(),
            input: data.input_spec().clone(),
            view: data.view(),
            channels: vec![0],
            hidden_channels: 4,
            batch_size: 16,
            updates: 64,
            learning_rate: 0.003,
            seed: 7,
            min_examples: 8,
        };
        let trained = train_sequence_model(&mut data, request.clone()).unwrap();
        let losses = &trained.diagnostics().batch_losses;
        assert!(
            losses[48..].iter().sum::<f64>() < losses[..16].iter().sum::<f64>(),
            "TCN did not learn the controlled temporal target"
        );
        assert_eq!(trained.scaling().examples, 28);
        assert_eq!(trained.diagnostics().completed_updates, 64);
        let sample = [10.0, 11.0, 12.0];
        let predicted = trained.predict(&sample).unwrap();
        let portable = trained.portable().unwrap();
        assert_eq!(
            trained.parameter_digest().unwrap(),
            portable.parameter_digest().unwrap()
        );
        for (original, pure) in predicted.iter().zip(portable.predict(&sample).unwrap()) {
            assert!(
                (original - pure).abs() < 1e-8,
                "pure TCN differs from the trained Burn predictor"
            );
        }
        assert!(predicted.iter().all(|v| v.is_finite() && v.abs() < 0.01));
        let (manifest, weights) = trained.bundle().unwrap();
        let digest = format!("{:x}", Sha256::digest(&manifest));
        let restored =
            TrainedSequenceModel::restore_bundle(&manifest, &digest, weights.clone()).unwrap();
        assert_eq!(predicted, restored.predict(&sample).unwrap());
        assert_eq!(
            trained.parameter_digest().unwrap(),
            restored.parameter_digest().unwrap()
        );
        let mut changed_manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        changed_manifest["scaling"]["target_means"][0] = serde_json::json!(1.0);
        assert!(TrainedSequenceModel::restore_bundle(
            &serde_json::to_vec(&changed_manifest).unwrap(),
            &digest,
            weights
        )
        .is_err());
        // Future labels and inputs cannot fit the train-only normalizer or model.
        let mut changed_rows = rows;
        for row in &mut changed_rows[60..] {
            row.channels[0] = 9999.0;
            row.forward_returns = [5.0; 3];
        }
        let mut second = reader(dir.path(), &changed_rows, 60000);
        let mut second_request = request;
        second_request.dataset_sha256 = second.dataset_digest().unwrap();
        let repeated = train_sequence_model(&mut second, second_request).unwrap();
        assert_eq!(trained.scaling(), repeated.scaling());
        assert_eq!(predicted, repeated.predict(&sample).unwrap());
        assert_eq!(
            trained.parameter_digest().unwrap(),
            repeated.parameter_digest().unwrap()
        );
    }

    #[test]
    fn sequence_mlp_uses_the_same_bound_inputs_and_short_budget_covers_recent_rows() {
        use super::training::{train_sequence_model, TrainedSequenceModel};
        use hft_research_manifest::portable_sequence::{
            SequenceNeuralKindV1, SequenceTrainingRequestV1,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut data = reader(dir.path(), &frames(), 60000);
        let request = SequenceTrainingRequestV1 {
            model_kind: SequenceNeuralKindV1::Mlp,
            dataset_sha256: data.dataset_digest().unwrap(),
            input: data.input_spec().clone(),
            view: data.view(),
            channels: vec![0],
            hidden_channels: 4,
            batch_size: 4,
            updates: 1,
            learning_rate: 0.003,
            seed: 7,
            min_examples: 8,
        };
        let model = train_sequence_model(&mut data, request).unwrap();
        assert_eq!(model.diagnostics().examples_seen, 4);
        assert_eq!(model.diagnostics().last_training_decision_ms, 29000);
        let expected = model.predict(&[1.0, 2.0, 3.0]).unwrap();
        let portable = model.portable().unwrap();
        assert_eq!(
            model.parameter_digest().unwrap(),
            portable.parameter_digest().unwrap()
        );
        for (original, pure) in expected
            .iter()
            .zip(portable.predict(&[1.0, 2.0, 3.0]).unwrap())
        {
            assert!(
                (original - pure).abs() < 1e-8,
                "pure MLP differs from the trained Burn predictor"
            );
        }
        let (manifest, weights) = model.bundle().unwrap();
        let digest = format!("{:x}", Sha256::digest(&manifest));
        let restored = TrainedSequenceModel::restore_bundle(&manifest, &digest, weights).unwrap();
        assert_eq!(expected, restored.predict(&[1.0, 2.0, 3.0]).unwrap());
    }
}
