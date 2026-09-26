//! Bounded causal sequence loading. No future rows enter the predictor input.
pub mod training;
use hft_research_manifest::sequence::{SequenceDatasetV1, SequenceFrameV1, SequenceViewV1};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
};

const MAX_FRAME_BYTES: usize = 32 * 1024;
pub const MAX_SEQUENCE_BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub struct SequenceExample {
    pub observed_at_ms: i64,
    /// Time-major [context, channels], oldest first. Includes no target values.
    pub inputs: Vec<f32>,
    pub targets: [f32; 3],
}

struct ShardReader {
    reader: BufReader<File>,
    digest: Sha256,
    bytes: u64,
    rows: u64,
    first: Option<i64>,
    last: Option<i64>,
}

pub struct SequenceReader {
    dataset: SequenceDatasetV1,
    files: Vec<File>,
    view: SequenceViewV1,
    shard_index: usize,
    shard: Option<ShardReader>,
    history: VecDeque<SequenceFrameV1>,
    previous_clock: Option<i64>,
    finished: bool,
}

impl SequenceReader {
    /// Admission verifies the complete declared bytes before fitting. File handles
    /// remain pinned; every subsequent pass rechecks their bytes as it consumes them.
    pub fn open(
        root: &Path,
        dataset: SequenceDatasetV1,
        expected_digest: &str,
        view: SequenceViewV1,
    ) -> Result<Self, String> {
        dataset.validate()?;
        view.validate()?;
        if dataset.digest()? != expected_digest {
            return Err("sequence dataset digest mismatch".into());
        }
        let mut files = Vec::with_capacity(dataset.shards.len());
        for descriptor in &dataset.shards {
            let path = root.join(&descriptor.file);
            let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.file_type().is_file() || metadata.len() != descriptor.bytes {
                return Err("sequence shard is not a regular file of the declared size".into());
            }
            let mut file = File::open(&path).map_err(|e| e.to_string())?;
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata().map_err(|e| e.to_string())?;
            if opened.len() != descriptor.bytes
                || opened.dev() != metadata.dev()
                || opened.ino() != metadata.ino()
            {
                return Err("sequence shard size changed during admission".into());
            }
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            let mut bytes = 0_u64;
            loop {
                let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                bytes += count as u64;
                if bytes > descriptor.bytes {
                    return Err("sequence shard grew during admission".into());
                }
                digest.update(&buffer[..count]);
            }
            if bytes != descriptor.bytes || format!("{:x}", digest.finalize()) != descriptor.sha256
            {
                return Err("sequence shard checksum mismatch".into());
            }
            files.push(file);
        }
        Ok(Self {
            dataset,
            files,
            view,
            shard_index: 0,
            shard: None,
            history: VecDeque::new(),
            previous_clock: None,
            finished: false,
        })
    }

    pub fn input_spec(&self) -> &hft_research_manifest::sequence::SequenceInputSpecV1 {
        &self.dataset.input
    }

    pub fn rewind(&mut self) -> Result<(), String> {
        if !self.finished {
            return Err("cannot rewind an incompletely verified sequence pass".into());
        }
        self.shard_index = 0;
        self.shard = None;
        self.history.clear();
        self.previous_clock = None;
        self.finished = false;
        Ok(())
    }

    /// Verify any remaining bytes before committing a model after a bounded update budget.
    pub fn finish_pass(&mut self) -> Result<(), String> {
        while self.next_frame()?.is_some() {}
        self.history.clear();
        Ok(())
    }

    pub fn next_batch(&mut self, max_examples: usize) -> Result<Vec<SequenceExample>, String> {
        if max_examples == 0 || max_examples > MAX_SEQUENCE_BATCH {
            return Err("invalid sequence batch bound".into());
        }
        let mut batch = Vec::with_capacity(max_examples);
        while batch.len() < max_examples {
            let Some(frame) = self.next_frame()? else {
                break;
            };
            let discontinuity = self.history.back().is_some_and(|previous| {
                previous.series_id != frame.series_id
                    || previous
                        .observed_at_ms
                        .checked_add(self.dataset.input.bucket_ms as i64)
                        != Some(frame.observed_at_ms)
            });
            if discontinuity {
                self.history.clear();
            }
            if frame.observed_at_ms < self.view.history_start_ms
                || frame.observed_at_ms >= self.view.end_ms
            {
                self.history.clear();
                continue;
            }
            self.history.push_back(frame);
            if self.history.len() > self.dataset.input.context_rows {
                self.history.pop_front();
            }
            let last = self.history.back().expect("pushed frame");
            if self.history.len() != self.dataset.input.context_rows
                || last.observed_at_ms < self.view.decision_start_ms
                || last
                    .label_available_at_ms
                    .iter()
                    .any(|time| *time >= self.view.end_ms)
            {
                continue;
            }
            batch.push(SequenceExample {
                observed_at_ms: last.observed_at_ms,
                inputs: self
                    .history
                    .iter()
                    .flat_map(|row| row.channels.iter().copied())
                    .collect(),
                targets: last.forward_returns,
            });
        }
        Ok(batch)
    }

    fn next_frame(&mut self) -> Result<Option<SequenceFrameV1>, String> {
        use std::io::{Seek, SeekFrom};
        loop {
            if self.shard_index == self.files.len() {
                self.finished = true;
                return Ok(None);
            }
            if self.shard.is_none() {
                let mut file = self.files[self.shard_index]
                    .try_clone()
                    .map_err(|e| e.to_string())?;
                file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                self.shard = Some(ShardReader {
                    reader: BufReader::new(file),
                    digest: Sha256::new(),
                    bytes: 0,
                    rows: 0,
                    first: None,
                    last: None,
                });
            }
            let shard = self.shard.as_mut().expect("opened shard");
            let mut line = Vec::new();
            let count = (&mut shard.reader)
                .take((MAX_FRAME_BYTES + 1) as u64)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if count == 0 {
                let shard = self.shard.take().expect("opened shard");
                let descriptor = &self.dataset.shards[self.shard_index];
                if shard.bytes != descriptor.bytes
                    || shard.rows != descriptor.rows
                    || shard.first != Some(descriptor.first_observed_at_ms)
                    || shard.last != Some(descriptor.last_observed_at_ms)
                    || format!("{:x}", shard.digest.finalize()) != descriptor.sha256
                {
                    return Err("sequence shard content or coverage changed".into());
                }
                self.shard_index += 1;
                continue;
            }
            if count > MAX_FRAME_BYTES || line.last() != Some(&b'\n') {
                return Err("oversized or unterminated sequence frame".into());
            }
            shard.bytes += count as u64;
            if shard.bytes > self.dataset.shards[self.shard_index].bytes {
                return Err("sequence shard exceeded declared bytes".into());
            }
            shard.digest.update(&line);
            let frame: SequenceFrameV1 =
                serde_json::from_slice(&line).map_err(|e| e.to_string())?;
            frame.validate(&self.dataset.input)?;
            if self
                .previous_clock
                .is_some_and(|previous| frame.observed_at_ms <= previous)
            {
                return Err("sequence frames are duplicate or out of order".into());
            }
            self.previous_clock = Some(frame.observed_at_ms);
            shard.first.get_or_insert(frame.observed_at_ms);
            shard.last = Some(frame.observed_at_ms);
            shard.rows += 1;
            return Ok(Some(frame));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_research_manifest::sequence::{
        SequenceInputSpecV1, SequenceShardV1, SEQUENCE_DATASET_SCHEMA,
    };

    fn frames() -> Vec<SequenceFrameV1> {
        (0..100)
            .map(|i| SequenceFrameV1 {
                series_id: 0,
                observed_at_ms: i * 1000,
                feature_max_available_at_ms: i * 1000,
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
            },
        )
        .unwrap()
    }

    #[test]
    fn sequence_windows_are_causal_and_label_maturity_is_strict() {
        let dir = tempfile::tempdir().unwrap();
        let mut input = reader(dir.path(), &frames(), 60000);
        let samples = input.next_batch(256).unwrap();
        assert_eq!(samples.len(), 28);
        assert_eq!(samples[0].inputs, [0.0, 1.0, 2.0]);
        assert_eq!(samples.last().unwrap().observed_at_ms, 29000);
        assert!(input.next_batch(256).unwrap().is_empty());
        input.rewind().unwrap();
        assert_eq!(input.next_batch(256).unwrap(), samples);
        assert!(input.history.len() <= 3);
    }

    #[test]
    fn sequence_future_changes_do_not_change_earlier_inputs() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let rows = frames();
        let a = reader(first.path(), &rows, 100000).next_batch(10).unwrap();
        let mut changed = rows;
        for row in &mut changed[20..] {
            row.channels[0] = -999.0;
            row.forward_returns = [-0.1; 3];
        }
        let b = reader(second.path(), &changed, 100000)
            .next_batch(10)
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn sequence_gap_and_session_reset_the_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut rows = frames();
        rows.remove(10);
        for row in rows.iter_mut().filter(|row| row.observed_at_ms >= 20000) {
            row.series_id = 1;
        }
        let samples = reader(dir.path(), &rows, 100000).next_batch(256).unwrap();
        let clocks: Vec<_> = samples.iter().map(|s| s.observed_at_ms).collect();
        for time in [10000, 11000, 12000, 20000, 21000] {
            assert!(!clocks.contains(&time));
        }
        assert!(clocks.contains(&13000));
        assert!(clocks.contains(&22000));
    }

    #[test]
    fn sequence_rejects_wrong_identity_duplicates_and_changed_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let rows = frames();
        let mut input = reader(dir.path(), &rows, 100000);
        assert!(input.rewind().is_err());
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join("part.jsonl"))
            .unwrap()
            .write_all(b"{}\n")
            .unwrap();
        assert!(input.finish_pass().is_err());
        let mut duplicated = rows;
        duplicated[10] = duplicated[9].clone();
        let mut input = reader(dir.path(), &duplicated, 100000);
        assert!(input.next_batch(256).is_err());
        let manifest = dataset(dir.path(), &frames());
        assert!(SequenceReader::open(
            dir.path(),
            manifest,
            &"b".repeat(64),
            SequenceViewV1 {
                history_start_ms: 0,
                decision_start_ms: 0,
                end_ms: 100000
            }
        )
        .is_err());
    }

    #[test]
    fn sequence_tcn_trains_reproducibly_and_roundtrips_raw_returns() {
        use training::{
            train_sequence_model, SequenceNeuralKindV1, SequenceTrainingRequestV1,
            TrainedSequenceModel,
        };
        let dir = tempfile::tempdir().unwrap();
        let rows = frames();
        let mut data = reader(dir.path(), &rows, 60000);
        let request = SequenceTrainingRequestV1 {
            model_kind: SequenceNeuralKindV1::Tcn,
            dataset_sha256: data.dataset.digest().unwrap(),
            input: data.dataset.input.clone(),
            view: data.view,
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
        assert!(predicted.iter().all(|v| v.is_finite() && v.abs() < 0.01));
        let (manifest, weights) = trained.bundle().unwrap();
        let digest = format!("{:x}", Sha256::digest(&manifest));
        let restored =
            TrainedSequenceModel::restore_bundle(&manifest, &digest, weights.clone()).unwrap();
        assert_eq!(predicted, restored.predict(&sample).unwrap());
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
        second_request.dataset_sha256 = second.dataset.digest().unwrap();
        let repeated = train_sequence_model(&mut second, second_request).unwrap();
        assert_eq!(trained.scaling(), repeated.scaling());
        assert_eq!(predicted, repeated.predict(&sample).unwrap());
    }

    #[test]
    fn sequence_mlp_uses_the_same_bound_inputs_and_short_budget_covers_recent_rows() {
        use training::{
            train_sequence_model, SequenceNeuralKindV1, SequenceTrainingRequestV1,
            TrainedSequenceModel,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut data = reader(dir.path(), &frames(), 60000);
        let request = SequenceTrainingRequestV1 {
            model_kind: SequenceNeuralKindV1::Mlp,
            dataset_sha256: data.dataset.digest().unwrap(),
            input: data.dataset.input.clone(),
            view: data.view,
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
        let (manifest, weights) = model.bundle().unwrap();
        let digest = format!("{:x}", Sha256::digest(&manifest));
        let restored = TrainedSequenceModel::restore_bundle(&manifest, &digest, weights).unwrap();
        assert_eq!(expected, restored.predict(&[1.0, 2.0, 3.0]).unwrap());
    }
}
