//! Verified streaming feature/target readers. Pretraining cannot deserialize labels.
use crate::sequence_storage::{Frame, Frames};
use hft_research_manifest::{market_encoder::*, sequence::SequenceInputSpecV1};
use std::{collections::VecDeque, path::Path};

impl Frame for MarketFeatureFrameV1 {
    fn clock(&self) -> i64 {
        self.observed_at_ms
    }
    fn validate(&self, input: &SequenceInputSpecV1) -> Result<(), String> {
        self.validate(input)
    }
}
impl Frame for MarketTargetFrameV1 {
    fn clock(&self) -> i64 {
        self.observed_at_ms
    }
    fn validate(&self, _: &SequenceInputSpecV1) -> Result<(), String> {
        self.validate()
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct UnlabeledSequenceExample {
    pub series_id: u64,
    pub observed_at_ms: i64,
    /// Time-major, oldest first. Contains no spread, forward price or target.
    pub inputs: Vec<f32>,
}

pub struct MarketFeatureReader {
    frames: Frames<MarketFeatureFrameV1>,
    history: VecDeque<MarketFeatureFrameV1>,
    request: MarketDataReadRequestV1,
}
impl MarketFeatureReader {
    pub fn open(
        root: &Path,
        dataset: MarketFeatureDatasetV1,
        request: &MarketDataReadRequestV1,
    ) -> Result<Self, String> {
        request.validate()?;
        dataset.validate()?;
        if dataset.digest()? != request.feature_dataset_sha256
            || dataset.input != request.input
            || dataset.shards.iter().any(|s| {
                s.first_observed_at_ms < request.view.history_start_ms
                    || s.last_observed_at_ms >= request.view.end_ms
            })
        {
            return Err("market features expose another dataset, input or time view".into());
        }
        Ok(Self {
            frames: Frames::open(root, dataset.shards, dataset.input)?,
            history: VecDeque::new(),
            request: request.clone(),
        })
    }
    pub fn request(&self) -> &MarketDataReadRequestV1 {
        &self.request
    }
    pub fn is_at_start(&self) -> bool {
        self.frames.is_at_start()
    }
    pub fn next_batch(&mut self, size: usize) -> Result<Vec<UnlabeledSequenceExample>, String> {
        if !(1..=256).contains(&size) {
            return Err("invalid market batch size".into());
        }
        let mut result = Vec::with_capacity(size);
        while result.len() < size {
            let Some(row) = self.frames.next()? else {
                break;
            };
            if row.observed_at_ms < self.request.view.history_start_ms
                || row.observed_at_ms >= self.request.view.end_ms
            {
                return Err("market frame escaped view".into());
            }
            if self.history.back().is_some_and(|old| {
                old.series_id != row.series_id
                    || old.observed_at_ms.checked_add(1000) != Some(row.observed_at_ms)
            }) {
                self.history.clear();
            }
            self.history.push_back(row);
            if self.history.len() > self.request.input.context_rows {
                self.history.pop_front();
            }
            let row = self.history.back().expect("pushed frame");
            if self.history.len() != self.request.input.context_rows
                || row.observed_at_ms < self.request.view.decision_start_ms
                || row.observed_at_ms >= self.request.anchor_end_ms
                || (row.observed_at_ms - self.request.view.decision_start_ms)
                    % self.request.view.decision_stride_ms
                    != 0
            {
                continue;
            }
            result.push(UnlabeledSequenceExample {
                series_id: row.series_id,
                observed_at_ms: row.observed_at_ms,
                inputs: self
                    .history
                    .iter()
                    .flat_map(|r| r.channels.iter().copied())
                    .collect(),
            });
        }
        Ok(result)
    }
    pub fn finish_pass(&mut self) -> Result<(), String> {
        self.frames.finish()?;
        self.history.clear();
        Ok(())
    }
    pub fn rewind(&mut self) -> Result<(), String> {
        self.frames.rewind()?;
        self.history.clear();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct MarketTaskExample {
    pub features: UnlabeledSequenceExample,
    pub target: MarketTargetFrameV1,
}

pub struct MarketTaskReader {
    pub(crate) features: MarketFeatureReader,
    targets: Frames<MarketTargetFrameV1>,
    target_digest: String,
    next_target: Option<MarketTargetFrameV1>,
}
impl MarketTaskReader {
    pub fn open(
        features: MarketFeatureReader,
        root: &Path,
        targets: MarketTargetDatasetV1,
        expected_digest: &str,
    ) -> Result<Self, String> {
        targets.validate()?;
        if targets.digest()? != expected_digest
            || targets.feature_dataset_sha256 != features.request.feature_dataset_sha256
            || !features.is_at_start()
            || targets.shards.iter().any(|s| {
                s.first_observed_at_ms < features.request.view.history_start_ms
                    || s.last_observed_at_ms
                        .checked_add(TASK_HORIZON_MS)
                        .is_none_or(|t| t >= features.request.view.end_ms)
            })
        {
            return Err("market targets expose another dataset or future view".into());
        }
        let frames = Frames::open(root, targets.shards, features.request.input.clone())?;
        Ok(Self {
            features,
            targets: frames,
            target_digest: expected_digest.into(),
            next_target: None,
        })
    }
    pub fn target_digest(&self) -> &str {
        &self.target_digest
    }
    fn read_target(&mut self) -> Result<Option<MarketTargetFrameV1>, String> {
        let row = self.targets.next()?;
        if row
            .as_ref()
            .is_some_and(|r| r.available_at_ms >= self.features.request.view.end_ms)
        {
            return Err("target file exposes an unavailable future label".into());
        }
        Ok(row)
    }
    pub fn next_batch(&mut self, size: usize) -> Result<Vec<MarketTaskExample>, String> {
        let mut result = Vec::new();
        for features in self.features.next_batch(size)? {
            loop {
                if self.next_target.is_none() {
                    self.next_target = self.read_target()?;
                }
                let target = self
                    .next_target
                    .as_ref()
                    .ok_or("missing target for common market anchor")?;
                if target.observed_at_ms >= features.observed_at_ms {
                    break;
                }
                self.next_target = None;
            }
            let target = self.next_target.take().ok_or("missing market target")?;
            if target.observed_at_ms != features.observed_at_ms
                || target.series_id != features.series_id
                || target.available_at_ms >= self.features.request.view.end_ms
            {
                return Err("market target is missing, immature or from another series".into());
            }
            result.push(MarketTaskExample { features, target });
        }
        Ok(result)
    }
    pub fn finish_pass(&mut self) -> Result<(), String> {
        self.features.finish_pass()?;
        while self.read_target()?.is_some() {}
        self.next_target = None;
        Ok(())
    }
    pub fn rewind(&mut self) -> Result<(), String> {
        self.features.rewind()?;
        self.targets.rewind()?;
        self.next_target = None;
        Ok(())
    }
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Moments {
    count: u64,
    mean: f64,
    m2: f64,
}
impl Moments {
    pub(crate) fn push(&mut self, x: f64) {
        self.count += 1;
        let d = x - self.mean;
        self.mean += d / self.count as f64;
        self.m2 += d * (x - self.mean);
    }
    pub(crate) fn mean(&self) -> f64 {
        self.mean
    }
    pub(crate) fn scale(&self) -> f64 {
        let sd = (self.m2 / self.count.max(1) as f64).max(0.0).sqrt();
        if sd > 0.0 {
            sd
        } else {
            1.0
        }
    }
}

pub fn fit_market_scaling(
    reader: &mut MarketFeatureReader,
    min_examples: u64,
    max_examples: u64,
) -> Result<MarketFeatureScalingV1, String> {
    if min_examples < 2 || max_examples < min_examples || max_examples > 32_768 {
        return Err("invalid market scaling sample budget".into());
    }
    if !reader.is_at_start() {
        return Err("scaling reader must start at beginning".into());
    }
    let request = reader.request.clone();
    let n = request.input.ordered_channels.len();
    let mut moments = vec![Moments::default(); n];
    let mut last = None;
    let mut unique = 0;
    let mut examples = 0;
    loop {
        let batch = reader.next_batch(256)?;
        if batch.is_empty() {
            break;
        }
        for item in batch {
            examples += 1;
            if examples > max_examples {
                return Err("market anchor budget exceeded".into());
            }
            for (index, frame) in item.inputs.chunks_exact(n).enumerate() {
                let clock =
                    item.observed_at_ms - ((request.input.context_rows - index - 1) * 1000) as i64;
                if last.is_some_and(|t| clock <= t) {
                    continue;
                }
                for (m, x) in moments.iter_mut().zip(frame) {
                    m.push(f64::from(*x));
                }
                last = Some(clock);
                unique += 1;
            }
        }
    }
    reader.finish_pass()?;
    reader.rewind()?;
    let scaling = MarketFeatureScalingV1 {
        means: moments.iter().map(Moments::mean).collect(),
        scales: moments.iter().map(Moments::scale).collect(),
        unique_frames: unique,
        examples,
    };
    scaling.validate_for_data(&request, min_examples, max_examples)?;
    Ok(scaling)
}
