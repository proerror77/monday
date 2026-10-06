//! Verified streaming feature/target readers. Pretraining cannot deserialize labels.
use crate::sequence_storage::{Frame, Frames};
use hft_prepared_market_io::{FeatureParquetReader, TargetParquetReader};
use hft_research_manifest::{market_encoder::*, sequence::SequenceInputSpecV1};
use std::{
    collections::VecDeque,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

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

// The manifest selects a format explicitly. A feature reader never constructs a
// target decoder, including when both artifacts live in the same prepared view.
enum FeatureFrames {
    Jsonl(Box<Frames<MarketFeatureFrameV1>>),
    Parquet(Box<FeatureParquetReader>),
}
impl FeatureFrames {
    fn next(&mut self) -> Result<Option<MarketFeatureFrameV1>, String> {
        match self {
            Self::Jsonl(r) => r.next(),
            Self::Parquet(r) => r.next_frame(),
        }
    }
    fn rewind(&mut self) -> Result<(), String> {
        match self {
            Self::Jsonl(r) => r.rewind(),
            Self::Parquet(r) => r.rewind(),
        }
    }
    fn is_at_start(&self) -> bool {
        match self {
            Self::Jsonl(r) => r.is_at_start(),
            Self::Parquet(r) => r.is_at_start(),
        }
    }
}
enum TargetFrames {
    Jsonl(Box<Frames<MarketTargetFrameV1>>),
    Parquet(Box<TargetParquetReader>),
}
impl TargetFrames {
    fn open(
        root: &Path,
        targets: MarketTargetDatasetV1,
        input: SequenceInputSpecV1,
    ) -> Result<Self, String> {
        match targets.schema_version.as_str() {
            TARGET_SCHEMA => Ok(Self::Jsonl(Box::new(Frames::open(
                root,
                targets.shards,
                input,
            )?))),
            TARGET_PARQUET_SCHEMA => Ok(Self::Parquet(Box::new(TargetParquetReader::open(
                root,
                targets.shards,
            )?))),
            _ => Err("unsupported market target storage schema".into()),
        }
    }
    fn next(&mut self) -> Result<Option<MarketTargetFrameV1>, String> {
        match self {
            Self::Jsonl(r) => r.next(),
            Self::Parquet(r) => r.next_frame(),
        }
    }
    fn rewind(&mut self) -> Result<(), String> {
        match self {
            Self::Jsonl(r) => r.rewind(),
            Self::Parquet(r) => r.rewind(),
        }
    }
    fn is_at_start(&self) -> bool {
        match self {
            Self::Jsonl(r) => r.is_at_start(),
            Self::Parquet(r) => r.is_at_start(),
        }
    }
}

pub struct MarketFeatureReader {
    frames: FeatureFrames,
    history: VecDeque<MarketFeatureFrameV1>,
    request: MarketDataReadRequestV1,
    qualified: Option<Vec<MarketTrainingAnchorV1>>,
    anchor_cursor: usize,
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
        let qualified = if let Some(hash) = &request.qualified_anchors_sha256 {
            let path = root.join(format!("{hash}.market-anchors.json"));
            let metadata = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.file_type().is_file() || metadata.len() > 4 * 1024 * 1024 {
                return Err("invalid qualified market anchor file".into());
            }
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(|e| e.to_string())?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 4 * 1024 * 1024 || bytes_digest(&bytes) != *hash {
                return Err("qualified anchor checksum mismatch".into());
            }
            let anchors: MarketTrainingAnchorSetV1 =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            anchors.validate()?;
            if anchors.feature_dataset_sha256 != request.feature_dataset_sha256
                || anchors.view != request.view
                || anchors.anchor_end_ms != request.anchor_end_ms
            {
                return Err("qualified anchors belong to another input or time view".into());
            }
            Some(anchors.anchors)
        } else {
            None
        };
        let frames = match dataset.schema_version.as_str() {
            FEATURE_SCHEMA => {
                FeatureFrames::Jsonl(Box::new(Frames::open(root, dataset.shards, dataset.input)?))
            }
            FEATURE_PARQUET_SCHEMA => FeatureFrames::Parquet(Box::new(FeatureParquetReader::open(
                root,
                dataset.shards,
                dataset.input,
            )?)),
            _ => return Err("unsupported market feature storage schema".into()),
        };
        Ok(Self {
            frames,
            history: VecDeque::new(),
            request: request.clone(),
            qualified,
            anchor_cursor: 0,
        })
    }
    pub fn request(&self) -> &MarketDataReadRequestV1 {
        &self.request
    }
    pub fn is_at_start(&self) -> bool {
        self.frames.is_at_start() && self.anchor_cursor == 0
    }
    pub fn next_batch(&mut self, size: usize) -> Result<Vec<UnlabeledSequenceExample>, String> {
        if !(1..=256).contains(&size) {
            return Err("invalid market batch size".into());
        }
        let mut result = Vec::with_capacity(size);
        while result.len() < size {
            let Some(row) = self.frames.next()? else {
                if self
                    .qualified
                    .as_ref()
                    .is_some_and(|a| self.anchor_cursor != a.len())
                {
                    return Err("qualified market anchor was not present in the dataset".into());
                }
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
            let eligible = self.history.len() == self.request.input.context_rows
                && row.observed_at_ms >= self.request.view.decision_start_ms
                && row.observed_at_ms < self.request.anchor_end_ms
                && (row.observed_at_ms - self.request.view.decision_start_ms)
                    % self.request.view.decision_stride_ms
                    == 0;
            if let Some(anchors) = &self.qualified {
                let Some(anchor) = anchors.get(self.anchor_cursor) else {
                    continue;
                };
                if anchor.observed_at_ms < row.observed_at_ms {
                    return Err("qualified market anchor crossed a missing observation".into());
                }
                if anchor.observed_at_ms != row.observed_at_ms {
                    continue;
                }
                if !eligible || anchor.series_id != row.series_id {
                    return Err("qualified market anchor lacks its declared causal context".into());
                }
                self.anchor_cursor += 1;
            } else if !eligible {
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
        while !self.next_batch(256)?.is_empty() {}
        self.history.clear();
        Ok(())
    }
    pub fn rewind(&mut self) -> Result<(), String> {
        self.frames.rewind()?;
        self.history.clear();
        self.anchor_cursor = 0;
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
    targets: TargetFrames,
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
        let frames = TargetFrames::open(root, targets, features.request.input.clone())?;
        Ok(Self {
            features,
            targets: frames,
            target_digest: expected_digest.into(),
            next_target: None,
        })
    }
    pub fn feature_request(&self) -> &MarketDataReadRequestV1 {
        self.features.request()
    }
    pub fn is_at_start(&self) -> bool {
        self.features.is_at_start() && self.targets.is_at_start() && self.next_target.is_none()
    }
    pub fn target_digest(&self) -> &str {
        &self.target_digest
    }
    /// Reuses the unique-frame, per-channel scaling of the neural arms.
    pub fn fit_feature_scaling(
        &mut self,
        min: u64,
        max: u64,
    ) -> Result<MarketFeatureScalingV1, String> {
        if !self.is_at_start() {
            return Err("market scaling reader must start at beginning".into());
        }
        fit_market_scaling(&mut self.features, min, max)
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
pub struct Moments {
    count: u64,
    mean: f64,
    m2: f64,
}
impl Moments {
    pub fn push(&mut self, x: f64) {
        self.count += 1;
        let d = x - self.mean;
        self.mean += d / self.count as f64;
        self.m2 += d * (x - self.mean);
    }
    pub fn mean(&self) -> f64 {
        self.mean
    }
    pub fn scale(&self) -> f64 {
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

/// Derive a shared training index from context/target availability only. Target
/// magnitudes never select anchors, and this index must not filter evaluation.
pub fn derive_market_training_anchors(
    features: &mut MarketFeatureReader,
    target_root: &Path,
    targets: MarketTargetDatasetV1,
    expected_target_sha256: &str,
) -> Result<MarketTrainingAnchorSetV1, String> {
    targets.validate()?;
    let request = features.request().clone();
    if !features.is_at_start()
        || request.qualified_anchors_sha256.is_some()
        || targets.digest()? != expected_target_sha256
        || targets.feature_dataset_sha256 != request.feature_dataset_sha256
        || targets.shards.iter().any(|s| {
            s.first_observed_at_ms < request.view.history_start_ms
                || s.last_observed_at_ms
                    .checked_add(TASK_HORIZON_MS)
                    .is_none_or(|end| end >= request.view.end_ms)
        })
    {
        return Err("anchor derivation requires the original grid and bounded target view".into());
    }
    let mut target_reader = TargetFrames::open(target_root, targets, request.input.clone())?;
    let mut current = None::<MarketTargetFrameV1>;
    let mut anchors = Vec::new();
    loop {
        let batch = features.next_batch(256)?;
        if batch.is_empty() {
            break;
        }
        for row in batch {
            loop {
                if current
                    .as_ref()
                    .is_some_and(|t| t.observed_at_ms >= row.observed_at_ms)
                {
                    break;
                }
                current = target_reader.next()?;
                if current
                    .as_ref()
                    .is_some_and(|t| t.available_at_ms >= request.view.end_ms)
                {
                    return Err("anchor target exposes an unavailable future label".into());
                }
                if current.is_none() {
                    break;
                }
            }
            if current.as_ref().is_some_and(|t| {
                t.observed_at_ms == row.observed_at_ms && t.series_id != row.series_id
            }) {
                return Err("anchor target belongs to another recovery series".into());
            }
            if current.as_ref().is_some_and(|t| {
                t.observed_at_ms == row.observed_at_ms && t.series_id == row.series_id
            }) {
                anchors.push(MarketTrainingAnchorV1 {
                    series_id: row.series_id,
                    observed_at_ms: row.observed_at_ms,
                });
                if anchors.len() > 32_768 {
                    return Err("qualified anchor count exceeds bound".into());
                }
            }
        }
    }
    while let Some(row) = target_reader.next()? {
        if row.available_at_ms >= request.view.end_ms {
            return Err("anchor target exposes an unavailable future label".into());
        }
    }
    features.finish_pass()?;
    let result = MarketTrainingAnchorSetV1 {
        schema_version: "monday.market_training_anchors.v1".into(),
        feature_dataset_sha256: request.feature_dataset_sha256,
        view: request.view,
        anchor_end_ms: request.anchor_end_ms,
        anchors,
    };
    result.validate()?;
    Ok(result)
}

/// A format conversion claim, separate from the native receipt/source admission
/// performed by Campaign. Its digest records every decoded clock and bit pattern.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketEquivalenceV1 {
    pub schema_version: String,
    pub source_dataset_sha256: Vec<String>,
    pub prepared_dataset_sha256: String,
    pub decoded_sha256: String,
    pub rows: u64,
}

/// Proves that prepared label-free frames preserve every admitted original row,
/// including exact recovery identities and Float32 bits. It never reads targets.
pub fn verify_prepared_feature_equivalence(
    sources: &[(PathBuf, MarketFeatureDatasetV1)],
    prepared_root: &Path,
    prepared: &MarketFeatureDatasetV1,
) -> Result<PreparedMarketEquivalenceV1, String> {
    use sha2::{Digest, Sha256};
    prepared.validate()?;
    if prepared.schema_version != FEATURE_PARQUET_SCHEMA
        || sources.is_empty()
        || sources.len() > 512
    {
        return Err(
            "feature equivalence requires bounded native sources and prepared Parquet".into(),
        );
    }
    let mut output = FeatureParquetReader::open(
        prepared_root,
        prepared.shards.clone(),
        prepared.input.clone(),
    )?;
    let mut decoded = Sha256::new();
    decoded.update(b"monday.market_feature_equivalence.v1\0");
    let mut source_hashes = Vec::with_capacity(sources.len());
    let mut rows = 0_u64;
    for (root, source) in sources {
        source.validate()?;
        if source.schema_version != FEATURE_SCHEMA
            || source.input != prepared.input
            || source.venue != prepared.venue
            || source.symbol != prepared.symbol
        {
            return Err("prepared feature conversion changed native input or venue".into());
        }
        source_hashes.push(source.digest()?);
        let mut original = Frames::<MarketFeatureFrameV1>::open(
            root,
            source.shards.clone(),
            source.input.clone(),
        )?;
        while let Some(expected) = original.next()? {
            let actual = output
                .next_frame()?
                .ok_or("prepared conversion omitted a native feature")?;
            if expected.series_id != actual.series_id
                || expected.observed_at_ms != actual.observed_at_ms
                || expected.feature_max_available_at_ms != actual.feature_max_available_at_ms
                || expected.channels.len() != actual.channels.len()
                || expected
                    .channels
                    .iter()
                    .zip(&actual.channels)
                    .any(|(left, right)| left.to_bits() != right.to_bits())
            {
                return Err(
                    "prepared conversion changed a native feature clock, series or Float32 value"
                        .into(),
                );
            }
            decoded.update(expected.series_id.to_le_bytes());
            decoded.update(expected.observed_at_ms.to_le_bytes());
            decoded.update(expected.feature_max_available_at_ms.to_le_bytes());
            for value in expected.channels {
                decoded.update(value.to_bits().to_le_bytes());
            }
            rows = rows
                .checked_add(1)
                .ok_or("feature equivalence row overflow")?;
        }
        original.finish()?;
    }
    if output.next_frame()?.is_some() {
        return Err("prepared conversion added a native feature".into());
    }
    output.finish_pass()?;
    Ok(PreparedMarketEquivalenceV1 {
        schema_version: "monday.market_feature_equivalence.v1".into(),
        source_dataset_sha256: source_hashes,
        prepared_dataset_sha256: prepared.digest()?,
        decoded_sha256: format!("{:x}", decoded.finalize()),
        rows,
    })
}

/// Proves exact target values and maturity clocks separately from the feature
/// proof. The caller retains the original native source and holdout checks.
pub fn verify_prepared_target_equivalence(
    sources: &[(PathBuf, MarketTargetDatasetV1)],
    prepared_root: &Path,
    prepared: &MarketTargetDatasetV1,
) -> Result<PreparedMarketEquivalenceV1, String> {
    use sha2::{Digest, Sha256};
    prepared.validate()?;
    if prepared.schema_version != TARGET_PARQUET_SCHEMA || sources.is_empty() || sources.len() > 512
    {
        return Err(
            "target equivalence requires bounded native sources and prepared Parquet".into(),
        );
    }
    let mut output = TargetParquetReader::open(prepared_root, prepared.shards.clone())?;
    let mut decoded = Sha256::new();
    decoded.update(b"monday.market_target_equivalence.v1\0");
    let mut source_hashes = Vec::with_capacity(sources.len());
    let mut rows = 0_u64;
    for (root, source) in sources {
        source.validate()?;
        if source.schema_version != TARGET_SCHEMA {
            return Err("target equivalence source is not original JSONL".into());
        }
        source_hashes.push(source.digest()?);
        let mut original = Frames::<MarketTargetFrameV1>::open(
            root,
            source.shards.clone(),
            SequenceInputSpecV1::sol_lob(),
        )?;
        while let Some(expected) = original.next()? {
            let actual = output
                .next_frame()?
                .ok_or("prepared conversion omitted a native target")?;
            if expected.series_id != actual.series_id
                || expected.observed_at_ms != actual.observed_at_ms
                || expected.available_at_ms != actual.available_at_ms
                || expected.simple_return.to_bits() != actual.simple_return.to_bits()
                || expected.spread_bps.to_bits() != actual.spread_bps.to_bits()
            {
                return Err(
                    "prepared conversion changed a native target value, series or maturity clock"
                        .into(),
                );
            }
            decoded.update(expected.series_id.to_le_bytes());
            decoded.update(expected.observed_at_ms.to_le_bytes());
            decoded.update(expected.available_at_ms.to_le_bytes());
            decoded.update(expected.simple_return.to_bits().to_le_bytes());
            decoded.update(expected.spread_bps.to_bits().to_le_bytes());
            rows = rows
                .checked_add(1)
                .ok_or("target equivalence row overflow")?;
        }
        original.finish()?;
    }
    if output.next_frame()?.is_some() {
        return Err("prepared conversion added a native target".into());
    }
    output.finish_pass()?;
    Ok(PreparedMarketEquivalenceV1 {
        schema_version: "monday.market_target_equivalence.v1".into(),
        source_dataset_sha256: source_hashes,
        prepared_dataset_sha256: prepared.digest()?,
        decoded_sha256: format!("{:x}", decoded.finalize()),
        rows,
    })
}
