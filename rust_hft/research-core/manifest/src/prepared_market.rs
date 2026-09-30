//! Immutable, bounded numerical market inputs. Feature files contain no targets.
//!
//! Parquet is the interchange format, not an optimizer checkpoint. One compressed
//! shard (at most 16 MiB) and one typed row group (at most 4,096 rows) are retained
//! by a reader. Each pass reloads and verifies the bytes before decoding them.
use crate::{
    market_encoder::{
        digest, MarketFeatureDatasetV1, MarketFeatureFrameV1, MarketTargetDatasetV1,
        MarketTargetFrameV1, FEATURE_PARQUET_SCHEMA, TARGET_PARQUET_SCHEMA, TASK_HORIZON_MS,
    },
    sequence::{
        valid_sha256, validate_sequence_shards_with_extension, SequenceInputSpecV1,
        SequenceShardV1, SequenceViewV1, MAX_SEQUENCE_CONTEXT,
    },
};
use bytes::Bytes;
use parquet::{
    basic::Compression,
    column::reader::ColumnReader,
    data_type::{DoubleType, FloatType, Int64Type},
    file::{
        properties::WriterProperties,
        reader::{FileReader, RowGroupReader},
        serialized_reader::SerializedFileReader,
        writer::{SerializedFileWriter, SerializedRowGroupWriter},
    },
    schema::{parser::parse_message_type, types::Type},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const PREPARED_MARKET_VIEW_SCHEMA: &str = "monday.prepared_market_view.v1";
pub const MAX_PREPARED_SHARD_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_PREPARED_GROUP_ROWS: usize = 4_096;
/// Fourteen days of decisions plus bounded causal context and label maturity.
pub const MAX_PREPARED_MARKET_ROWS: u64 = 14 * 86_400 + MAX_SEQUENCE_CONTEXT as u64 + 30;
const MAX_GROUP_UNCOMPRESSED_BYTES: i64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketSourceV1 {
    pub feature_dataset_sha256: String,
    pub target_dataset_sha256: Option<String>,
    pub source_manifest_sha256: String,
    pub transform_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketSeriesV1 {
    pub series_id: u64,
    pub first_observed_at_ms: i64,
    pub last_observed_at_ms: i64,
    pub rows: u64,
}

/// Adjacent observations surrounding a missing interval. Series boundaries are
/// recorded separately and are never silently filled or forward interpolated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketGapV1 {
    pub last_before_ms: i64,
    pub first_after_ms: i64,
}

/// The ordered source union and actual output identities survive changes to a
/// consumer's seed, batch size, optimizer, or experiment name. Splits and eligible
/// anchors are explicit time views; this contract grants no holdout access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMarketViewV1 {
    pub schema_version: String,
    pub sources: Vec<PreparedMarketSourceV1>,
    pub source_feature_dataset_sha256: String,
    pub source_target_dataset_sha256: Option<String>,
    pub source_manifest_sha256: String,
    pub transform_sha256: String,
    pub data_watermark_ms: i64,
    pub view: SequenceViewV1,
    pub feature_dataset_sha256: String,
    pub target_dataset_sha256: Option<String>,
    pub qualified_anchors_sha256: Option<String>,
    pub series: Vec<PreparedMarketSeriesV1>,
    pub gaps: Vec<PreparedMarketGapV1>,
}

/// A single source retains its identity; a union binds its explicit order.
pub fn source_union_digest(hashes: &[String]) -> Result<String, String> {
    if hashes.is_empty() || hashes.iter().any(|v| !valid_sha256(v)) {
        return Err("invalid prepared source union".into());
    }
    if hashes.len() == 1 {
        Ok(hashes[0].clone())
    } else {
        digest(&hashes)
    }
}

impl PreparedMarketViewV1 {
    pub fn validate(&self) -> Result<(), String> {
        self.view.validate()?;
        if self.schema_version != PREPARED_MARKET_VIEW_SCHEMA
            || self.sources.is_empty()
            || self.sources.len() > 4096
            || !valid_sha256(&self.feature_dataset_sha256)
            || !valid_sha256(&self.transform_sha256)
            || self
                .target_dataset_sha256
                .as_deref()
                .is_some_and(|v| !valid_sha256(v))
            || self
                .qualified_anchors_sha256
                .as_deref()
                .is_some_and(|v| !valid_sha256(v))
            || self.data_watermark_ms < self.view.end_ms
            || self.view.end_ms - self.view.history_start_ms
                > MAX_PREPARED_MARKET_ROWS as i64 * 1000
            || self.series.is_empty()
            || self.series.len() as u64 > MAX_PREPARED_MARKET_ROWS
            || self.gaps.len() as u64 > MAX_PREPARED_MARKET_ROWS
        {
            return Err("invalid prepared market view identity or bounds".into());
        }
        let mut identities = BTreeSet::new();
        for source in &self.sources {
            if !valid_sha256(&source.feature_dataset_sha256)
                || !valid_sha256(&source.source_manifest_sha256)
                || source.transform_sha256 != self.transform_sha256
                || source
                    .target_dataset_sha256
                    .as_deref()
                    .is_some_and(|v| !valid_sha256(v))
                || !identities.insert(&source.feature_dataset_sha256)
            {
                return Err("invalid or duplicate prepared market source".into());
            }
        }
        let feature_hashes = self
            .sources
            .iter()
            .map(|s| s.feature_dataset_sha256.clone())
            .collect::<Vec<_>>();
        let manifest_hashes = self
            .sources
            .iter()
            .map(|s| s.source_manifest_sha256.clone())
            .collect::<Vec<_>>();
        let target_hashes = self
            .sources
            .iter()
            .map(|s| s.target_dataset_sha256.clone())
            .collect::<Option<Vec<_>>>();
        let expected_targets = target_hashes
            .as_deref()
            .map(source_union_digest)
            .transpose()?;
        if self.source_feature_dataset_sha256 != source_union_digest(&feature_hashes)?
            || self.source_manifest_sha256 != source_union_digest(&manifest_hashes)?
            || self.source_target_dataset_sha256 != expected_targets
            || (self.target_dataset_sha256.is_some() && expected_targets.is_none())
        {
            return Err("prepared view source union is not bound to its catalog".into());
        }
        let mut previous = None;
        let mut rows = 0_u64;
        for series in &self.series {
            if series.rows == 0
                || series.first_observed_at_ms < self.view.history_start_ms
                || series.last_observed_at_ms >= self.view.end_ms
                || series.first_observed_at_ms > series.last_observed_at_ms
                || series.first_observed_at_ms % 1000 != 0
                || series.last_observed_at_ms % 1000 != 0
                || series.rows
                    > ((series.last_observed_at_ms - series.first_observed_at_ms) / 1000 + 1) as u64
                || previous.is_some_and(|t| series.first_observed_at_ms <= t)
            {
                return Err("invalid actual prepared series coverage".into());
            }
            rows = rows
                .checked_add(series.rows)
                .ok_or("prepared row count overflow")?;
            previous = Some(series.last_observed_at_ms);
        }
        if rows > MAX_PREPARED_MARKET_ROWS {
            return Err("prepared view exceeds 14-day and context row budget".into());
        }
        previous = None;
        for gap in &self.gaps {
            if gap.last_before_ms < self.view.history_start_ms
                || gap.first_after_ms >= self.view.end_ms
                || gap
                    .last_before_ms
                    .checked_add(1000)
                    .is_none_or(|t| gap.first_after_ms <= t)
                || gap.last_before_ms % 1000 != 0
                || gap.first_after_ms % 1000 != 0
                || previous.is_some_and(|t| gap.last_before_ms < t)
            {
                return Err("invalid or overlapping prepared gaps".into());
            }
            previous = Some(gap.first_after_ms);
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String, String> {
        self.validate()?;
        digest(self)
    }

    /// Binds the manifest's actual coverage to its referenced prepared datasets.
    /// Physical checksums and decoded coverage must additionally be verified by
    /// the readers before the caller publishes a ready receipt.
    pub fn validate_datasets(
        &self,
        features: &MarketFeatureDatasetV1,
        targets: Option<&MarketTargetDatasetV1>,
    ) -> Result<(), String> {
        self.validate()?;
        features.validate()?;
        let declared_rows = self.series.iter().map(|s| s.rows).sum::<u64>();
        if features.schema_version != FEATURE_PARQUET_SCHEMA
            || features.digest()? != self.feature_dataset_sha256
            || features.source_manifest_sha256 != self.source_manifest_sha256
            || features.shards.iter().map(|s| s.rows).sum::<u64>() != declared_rows
            || features.shards.first().map(|s| s.first_observed_at_ms)
                != self.series.first().map(|s| s.first_observed_at_ms)
            || features.shards.last().map(|s| s.last_observed_at_ms)
                != self.series.last().map(|s| s.last_observed_at_ms)
            || features.shards.iter().any(|s| {
                s.first_observed_at_ms < self.view.history_start_ms
                    || s.last_observed_at_ms >= self.view.end_ms
            })
        {
            return Err("prepared view does not bind its feature dataset coverage".into());
        }
        match (targets, self.target_dataset_sha256.as_deref()) {
            (None, None) => Ok(()),
            (Some(targets), Some(expected)) => {
                targets.validate()?;
                if targets.schema_version != TARGET_PARQUET_SCHEMA
                    || targets.digest()? != expected
                    || targets.feature_dataset_sha256 != self.feature_dataset_sha256
                    || targets.shards.iter().any(|s| {
                        s.first_observed_at_ms < self.view.history_start_ms
                            || s.last_observed_at_ms
                                .checked_add(TASK_HORIZON_MS)
                                .is_none_or(|t| t >= self.view.end_ms)
                    })
                {
                    return Err("prepared view does not bind mature target dataset".into());
                }
                Ok(())
            }
            _ => Err("prepared target dataset presence mismatch".into()),
        }
    }
}

enum NumericColumn {
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}
trait PreparedFrame: Sized {
    fn schema(input: &SequenceInputSpecV1) -> Result<Arc<Type>, String>;
    fn clock(&self) -> i64;
    fn validate_frame(&self, input: &SequenceInputSpecV1) -> Result<(), String>;
    fn columns(rows: &[Self], input: &SequenceInputSpecV1) -> Vec<NumericColumn>;
    fn decode(columns: &[NumericColumn], row: usize) -> Result<Self, String>;
}

fn schema(features: Option<&SequenceInputSpecV1>) -> Result<Arc<Type>, String> {
    let columns = match features {
        Some(input) => input
            .ordered_channels
            .iter()
            .enumerate()
            .map(|(i, name)| format!("REQUIRED FLOAT channel_{i:03}_{name};"))
            .collect::<String>(),
        None => "REQUIRED FLOAT simple_return; REQUIRED DOUBLE spread_bps;".into(),
    };
    let available = if features.is_some() {
        "feature_max_available_at_ms"
    } else {
        "available_at_ms"
    };
    parse_message_type(&format!("message monday_market {{ REQUIRED INT64 series_id (UINT_64); REQUIRED INT64 observed_at_ms; REQUIRED INT64 {available}; {columns} }}"))
        .map(Arc::new).map_err(|e| e.to_string())
}
fn i64_at(columns: &[NumericColumn], column: usize, row: usize) -> Result<i64, String> {
    match &columns[column] {
        NumericColumn::I64(v) => Ok(v[row]),
        _ => Err("prepared integer type mismatch".into()),
    }
}
fn f32_at(columns: &[NumericColumn], column: usize, row: usize) -> Result<f32, String> {
    match &columns[column] {
        NumericColumn::F32(v) => Ok(v[row]),
        _ => Err("prepared float32 type mismatch".into()),
    }
}
impl PreparedFrame for MarketFeatureFrameV1 {
    fn schema(input: &SequenceInputSpecV1) -> Result<Arc<Type>, String> {
        schema(Some(input))
    }
    fn clock(&self) -> i64 {
        self.observed_at_ms
    }
    fn validate_frame(&self, input: &SequenceInputSpecV1) -> Result<(), String> {
        self.validate(input)
    }
    fn columns(rows: &[Self], input: &SequenceInputSpecV1) -> Vec<NumericColumn> {
        let mut columns = vec![
            NumericColumn::I64(rows.iter().map(|r| r.series_id as i64).collect()),
            NumericColumn::I64(rows.iter().map(|r| r.observed_at_ms).collect()),
            NumericColumn::I64(rows.iter().map(|r| r.feature_max_available_at_ms).collect()),
        ];
        columns.extend(
            (0..input.ordered_channels.len())
                .map(|i| NumericColumn::F32(rows.iter().map(|r| r.channels[i]).collect())),
        );
        columns
    }
    fn decode(columns: &[NumericColumn], row: usize) -> Result<Self, String> {
        Ok(Self {
            series_id: i64_at(columns, 0, row)? as u64,
            observed_at_ms: i64_at(columns, 1, row)?,
            feature_max_available_at_ms: i64_at(columns, 2, row)?,
            channels: (3..columns.len())
                .map(|i| f32_at(columns, i, row))
                .collect::<Result<_, _>>()?,
        })
    }
}
impl PreparedFrame for MarketTargetFrameV1 {
    fn schema(_: &SequenceInputSpecV1) -> Result<Arc<Type>, String> {
        schema(None)
    }
    fn clock(&self) -> i64 {
        self.observed_at_ms
    }
    fn validate_frame(&self, _: &SequenceInputSpecV1) -> Result<(), String> {
        self.validate()
    }
    fn columns(rows: &[Self], _: &SequenceInputSpecV1) -> Vec<NumericColumn> {
        vec![
            NumericColumn::I64(rows.iter().map(|r| r.series_id as i64).collect()),
            NumericColumn::I64(rows.iter().map(|r| r.observed_at_ms).collect()),
            NumericColumn::I64(rows.iter().map(|r| r.available_at_ms).collect()),
            NumericColumn::F32(rows.iter().map(|r| r.simple_return).collect()),
            NumericColumn::F64(rows.iter().map(|r| r.spread_bps).collect()),
        ]
    }
    fn decode(columns: &[NumericColumn], row: usize) -> Result<Self, String> {
        let NumericColumn::F64(spreads) = &columns[4] else {
            return Err("prepared spread type mismatch".into());
        };
        Ok(Self {
            series_id: i64_at(columns, 0, row)? as u64,
            observed_at_ms: i64_at(columns, 1, row)?,
            available_at_ms: i64_at(columns, 2, row)?,
            simple_return: f32_at(columns, 3, row)?,
            spread_bps: spreads[row],
        })
    }
}

fn write_columns(
    group: &mut SerializedRowGroupWriter<'_, File>,
    columns: Vec<NumericColumn>,
) -> Result<(), String> {
    for values in columns {
        let mut column = group
            .next_column()
            .map_err(|e| e.to_string())?
            .ok_or("missing prepared column")?;
        match values {
            NumericColumn::I64(v) => {
                column
                    .typed::<Int64Type>()
                    .write_batch(&v, None, None)
                    .map_err(|e| e.to_string())?;
            }
            NumericColumn::F32(v) => {
                column
                    .typed::<FloatType>()
                    .write_batch(&v, None, None)
                    .map_err(|e| e.to_string())?;
            }
            NumericColumn::F64(v) => {
                column
                    .typed::<DoubleType>()
                    .write_batch(&v, None, None)
                    .map_err(|e| e.to_string())?;
            }
        }
        column.close().map_err(|e| e.to_string())?;
    }
    if group.next_column().map_err(|e| e.to_string())?.is_some() {
        return Err("unwritten prepared column".into());
    }
    Ok(())
}

fn write_shard<F: PreparedFrame>(
    root: &Path,
    basename: &str,
    rows: &[F],
    input: &SequenceInputSpecV1,
) -> Result<SequenceShardV1, String> {
    input.validate()?;
    if rows.is_empty() || rows.len() > 86_400 {
        return Err("invalid prepared shard row budget".into());
    }
    let mut shard = SequenceShardV1 {
        file: basename.into(),
        sha256: "0".repeat(64),
        bytes: 1,
        rows: rows.len() as u64,
        first_observed_at_ms: rows[0].clock(),
        last_observed_at_ms: rows[rows.len() - 1].clock(),
    };
    validate_sequence_shards_with_extension(std::slice::from_ref(&shard), ".parquet")?;
    let mut previous = None;
    for row in rows {
        row.validate_frame(input)?;
        if previous.is_some_and(|t| row.clock() <= t) {
            return Err("unordered prepared writer rows".into());
        }
        previous = Some(row.clock());
    }
    let path = root.join(basename);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        let properties = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .set_max_row_group_row_count(Some(MAX_PREPARED_GROUP_ROWS))
            .build();
        let mut writer = SerializedFileWriter::new(
            file.try_clone().map_err(|e| e.to_string())?,
            F::schema(input)?,
            Arc::new(properties),
        )
        .map_err(|e| e.to_string())?;
        for rows in rows.chunks(MAX_PREPARED_GROUP_ROWS) {
            let mut group = writer.next_row_group().map_err(|e| e.to_string())?;
            write_columns(&mut group, F::columns(rows, input))?;
            group.close().map_err(|e| e.to_string())?;
        }
        writer.close().map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let bytes = verified_bytes(&path, None)?;
        shard.bytes = bytes.len() as u64;
        shard.sha256 = format!("{:x}", Sha256::digest(&bytes));
        Ok(shard)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

/// Writes unique frames, never expanded overlapping model windows.
pub fn write_feature_parquet_shard(
    root: &Path,
    basename: &str,
    rows: &[MarketFeatureFrameV1],
    input: &SequenceInputSpecV1,
) -> Result<SequenceShardV1, String> {
    write_shard(root, basename, rows, input)
}
pub fn write_target_parquet_shard(
    root: &Path,
    basename: &str,
    rows: &[MarketTargetFrameV1],
) -> Result<SequenceShardV1, String> {
    write_shard(root, basename, rows, &SequenceInputSpecV1::sol_lob())
}

fn verified_bytes(path: &Path, expected: Option<&SequenceShardV1>) -> Result<Bytes, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.file_type().is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_PREPARED_SHARD_BYTES
        || expected.is_some_and(|s| metadata.len() != s.bytes)
    {
        return Err("invalid bounded prepared shard file".into());
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    use std::os::unix::fs::MetadataExt;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
        return Err("prepared shard changed during open".into());
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_PREPARED_SHARD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != metadata.len()
        || expected.is_some_and(|s| format!("{:x}", Sha256::digest(&bytes)) != s.sha256)
    {
        return Err("prepared shard checksum or size mismatch".into());
    }
    Ok(Bytes::from(bytes))
}

fn read_columns(group: &dyn RowGroupReader) -> Result<Vec<NumericColumn>, String> {
    let rows = usize::try_from(group.metadata().num_rows()).map_err(|e| e.to_string())?;
    if rows == 0 || rows > MAX_PREPARED_GROUP_ROWS {
        return Err("prepared row group exceeds bounded decoder".into());
    }
    let mut columns = Vec::with_capacity(group.num_columns());
    for i in 0..group.num_columns() {
        let mut reader = group.get_column_reader(i).map_err(|e| e.to_string())?;
        macro_rules! read_column {
            ($reader:expr, $variant:ident) => {{
                let mut values = Vec::with_capacity(rows);
                let (records, count, _) = $reader
                    .read_records(rows, None, None, &mut values)
                    .map_err(|e| e.to_string())?;
                if records != rows || count != rows || values.len() != rows {
                    return Err("prepared required column has missing rows".into());
                }
                let (extra, _, _) = $reader
                    .read_records(1, None, None, &mut Vec::new())
                    .map_err(|e| e.to_string())?;
                if extra != 0 {
                    return Err("prepared column has extra rows".into());
                }
                NumericColumn::$variant(values)
            }};
        }
        columns.push(match &mut reader {
            ColumnReader::Int64ColumnReader(r) => read_column!(r, I64),
            ColumnReader::FloatColumnReader(r) => read_column!(r, F32),
            ColumnReader::DoubleColumnReader(r) => read_column!(r, F64),
            _ => return Err("unsupported prepared column type".into()),
        });
    }
    Ok(columns)
}

struct ParquetFrames<F> {
    root: PathBuf,
    shards: Vec<SequenceShardV1>,
    input: SequenceInputSpecV1,
    shard: usize,
    reader: Option<SerializedFileReader<Bytes>>,
    group: usize,
    columns: Vec<NumericColumn>,
    row: usize,
    group_rows: usize,
    rows: u64,
    first: Option<i64>,
    last: Option<i64>,
    previous: Option<i64>,
    marker: std::marker::PhantomData<F>,
}
impl<F: PreparedFrame> ParquetFrames<F> {
    fn open(
        root: &Path,
        shards: Vec<SequenceShardV1>,
        input: SequenceInputSpecV1,
    ) -> Result<Self, String> {
        input.validate()?;
        validate_sequence_shards_with_extension(&shards, ".parquet")?;
        if shards.iter().any(|s| s.bytes > MAX_PREPARED_SHARD_BYTES) {
            return Err("prepared shard exceeds bounded input buffer".into());
        }
        // Validate every immutable source before exposing the first frame. Buffers
        // are released between files; a pass will reload and recheck each shard.
        for shard in &shards {
            Self::load(root, shard, &input)?;
        }
        Ok(Self {
            root: root.into(),
            shards,
            input,
            shard: 0,
            reader: None,
            group: 0,
            columns: Vec::new(),
            row: 0,
            group_rows: 0,
            rows: 0,
            first: None,
            last: None,
            previous: None,
            marker: std::marker::PhantomData,
        })
    }
    fn load(
        root: &Path,
        shard: &SequenceShardV1,
        input: &SequenceInputSpecV1,
    ) -> Result<SerializedFileReader<Bytes>, String> {
        let reader =
            SerializedFileReader::new(verified_bytes(&root.join(&shard.file), Some(shard))?)
                .map_err(|e| e.to_string())?;
        if reader.metadata().file_metadata().schema() != F::schema(input)?.as_ref()
            || reader.metadata().file_metadata().num_rows() != shard.rows as i64
            || reader.num_row_groups() == 0
            || reader.num_row_groups() > 86_400
        {
            return Err("prepared schema or row coverage mismatch".into());
        }
        for group in reader.metadata().row_groups() {
            if group.num_rows() <= 0
                || group.num_rows() > MAX_PREPARED_GROUP_ROWS as i64
                || group.total_byte_size() <= 0
                || group.total_byte_size() > MAX_GROUP_UNCOMPRESSED_BYTES
                || group.columns().iter().any(|c| {
                    c.uncompressed_size() <= 0
                        || c.uncompressed_size() > MAX_GROUP_UNCOMPRESSED_BYTES
                })
            {
                return Err("prepared metadata exceeds bounded row group".into());
            }
        }
        Ok(reader)
    }
    fn is_at_start(&self) -> bool {
        self.shard == 0 && self.reader.is_none() && self.previous.is_none()
    }
    fn next(&mut self) -> Result<Option<F>, String> {
        loop {
            if self.shard == self.shards.len() {
                return Ok(None);
            }
            if self.reader.is_none() {
                self.reader = Some(Self::load(
                    &self.root,
                    &self.shards[self.shard],
                    &self.input,
                )?);
                self.group = 0;
                self.rows = 0;
                self.first = None;
                self.last = None;
            }
            if self.row < self.group_rows {
                let frame = F::decode(&self.columns, self.row)?;
                frame.validate_frame(&self.input)?;
                let clock = frame.clock();
                if self.previous.is_some_and(|t| clock <= t) {
                    return Err("prepared frames duplicate or out of order".into());
                }
                self.previous = Some(clock);
                self.first.get_or_insert(clock);
                self.last = Some(clock);
                self.rows += 1;
                self.row += 1;
                return Ok(Some(frame));
            }
            self.columns.clear();
            self.group_rows = 0;
            self.row = 0;
            let reader = self.reader.as_ref().expect("opened prepared reader");
            if self.group < reader.num_row_groups() {
                let group = reader
                    .get_row_group(self.group)
                    .map_err(|e| e.to_string())?;
                self.group_rows = group.metadata().num_rows() as usize;
                self.columns = read_columns(group.as_ref())?;
                self.group += 1;
                continue;
            }
            let shard = &self.shards[self.shard];
            if self.rows != shard.rows
                || self.first != Some(shard.first_observed_at_ms)
                || self.last != Some(shard.last_observed_at_ms)
            {
                return Err("prepared shard actual coverage mismatch".into());
            }
            self.reader = None;
            self.shard += 1;
        }
    }
    fn finish(&mut self) -> Result<(), String> {
        while self.next()?.is_some() {}
        Ok(())
    }
    fn rewind(&mut self) -> Result<(), String> {
        if self.shard != self.shards.len() {
            return Err("cannot rewind an unverified prepared pass".into());
        }
        self.shard = 0;
        self.previous = None;
        self.reader = None;
        self.columns.clear();
        self.row = 0;
        self.group_rows = 0;
        Ok(())
    }
}

pub struct FeatureParquetReader {
    frames: ParquetFrames<MarketFeatureFrameV1>,
}
impl FeatureParquetReader {
    pub fn open(
        root: &Path,
        shards: Vec<SequenceShardV1>,
        input: SequenceInputSpecV1,
    ) -> Result<Self, String> {
        Ok(Self {
            frames: ParquetFrames::open(root, shards, input)?,
        })
    }
    pub fn next_frame(&mut self) -> Result<Option<MarketFeatureFrameV1>, String> {
        self.frames.next()
    }
    pub fn finish_pass(&mut self) -> Result<(), String> {
        self.frames.finish()
    }
    pub fn rewind(&mut self) -> Result<(), String> {
        self.frames.rewind()
    }
    pub fn is_at_start(&self) -> bool {
        self.frames.is_at_start()
    }
}
pub struct TargetParquetReader {
    frames: ParquetFrames<MarketTargetFrameV1>,
}
impl TargetParquetReader {
    pub fn open(root: &Path, shards: Vec<SequenceShardV1>) -> Result<Self, String> {
        Ok(Self {
            frames: ParquetFrames::open(root, shards, SequenceInputSpecV1::sol_lob())?,
        })
    }
    pub fn next_frame(&mut self) -> Result<Option<MarketTargetFrameV1>, String> {
        self.frames.next()
    }
    pub fn finish_pass(&mut self) -> Result<(), String> {
        self.frames.finish()
    }
    pub fn rewind(&mut self) -> Result<(), String> {
        self.frames.rewind()
    }
    pub fn is_at_start(&self) -> bool {
        self.frames.is_at_start()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> SequenceInputSpecV1 {
        SequenceInputSpecV1 {
            ordered_channels: vec!["first".into(), "second".into()],
            context_rows: 60,
            bucket_ms: 1000,
        }
    }
    fn rows(count: usize) -> Vec<MarketFeatureFrameV1> {
        (0..count)
            .map(|i| MarketFeatureFrameV1 {
                series_id: u64::MAX,
                observed_at_ms: i as i64 * 1000,
                feature_max_available_at_ms: i as i64 * 1000,
                channels: vec![
                    if i == 0 {
                        -0.0
                    } else if i == 1 {
                        0.0
                    } else {
                        f32::from_bits(0x3e00_0000 + i as u32)
                    },
                    -0.12542158,
                ],
            })
            .collect()
    }
    #[test]
    fn prepared_parquet_preserves_float32_bits_and_cross_group_coverage() {
        let root = tempfile::tempdir().unwrap();
        let rows = rows(MAX_PREPARED_GROUP_ROWS + 4);
        let shard =
            write_feature_parquet_shard(root.path(), "features.parquet", &rows, &input()).unwrap();
        let mut reader = FeatureParquetReader::open(root.path(), vec![shard], input()).unwrap();
        assert!(reader.is_at_start());
        for expected in &rows {
            let actual = reader.next_frame().unwrap().unwrap();
            assert_eq!(actual.series_id, expected.series_id);
            assert_eq!(actual.observed_at_ms, expected.observed_at_ms);
            assert_eq!(
                actual.feature_max_available_at_ms,
                expected.feature_max_available_at_ms
            );
            assert_eq!(
                actual
                    .channels
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>(),
                expected
                    .channels
                    .iter()
                    .map(|v| v.to_bits())
                    .collect::<Vec<_>>()
            );
        }
        assert!(reader.next_frame().unwrap().is_none());
        reader.rewind().unwrap();
        assert!(reader.is_at_start());
        reader.finish_pass().unwrap();
    }
    #[test]
    fn prepared_parquet_rejects_checksum_coverage_and_feature_target_confusion() {
        let root = tempfile::tempdir().unwrap();
        let shard =
            write_feature_parquet_shard(root.path(), "features.parquet", &rows(3), &input())
                .unwrap();
        let mut false_rows = shard.clone();
        false_rows.rows += 1;
        assert!(FeatureParquetReader::open(root.path(), vec![false_rows], input()).is_err());
        let mut false_clock = shard.clone();
        false_clock.last_observed_at_ms += 1000;
        let mut reader =
            FeatureParquetReader::open(root.path(), vec![false_clock], input()).unwrap();
        assert!(reader.finish_pass().is_err());
        let target = MarketTargetFrameV1 {
            series_id: 1,
            observed_at_ms: 0,
            available_at_ms: 30000,
            simple_return: -0.12542158,
            spread_bps: 1.0,
        };
        let labels = write_target_parquet_shard(
            root.path(),
            "targets.parquet",
            std::slice::from_ref(&target),
        )
        .unwrap();
        assert!(FeatureParquetReader::open(root.path(), vec![labels.clone()], input()).is_err());
        let mut labels_reader = TargetParquetReader::open(root.path(), vec![labels]).unwrap();
        assert_eq!(labels_reader.next_frame().unwrap(), Some(target));
        labels_reader.finish_pass().unwrap();
        let path = root.path().join(&shard.file);
        let mut corrupt = std::fs::read(&path).unwrap();
        corrupt[4] ^= 1;
        std::fs::write(path, corrupt).unwrap();
        assert!(FeatureParquetReader::open(root.path(), vec![shard], input()).is_err());
    }
    #[test]
    fn prepared_reader_rechecks_new_pass_and_cannot_rewind_partial_pass() {
        let root = tempfile::tempdir().unwrap();
        let shard =
            write_feature_parquet_shard(root.path(), "features.parquet", &rows(3), &input())
                .unwrap();
        let mut reader =
            FeatureParquetReader::open(root.path(), vec![shard.clone()], input()).unwrap();
        reader.next_frame().unwrap();
        assert!(reader.rewind().is_err());
        let path = root.path().join(&shard.file);
        let mut corrupt = std::fs::read(&path).unwrap();
        corrupt[4] ^= 1;
        std::fs::write(path, corrupt).unwrap();
        // This pass owns already verified immutable bytes, independent of disk.
        reader.finish_pass().unwrap();
        reader.rewind().unwrap();
        assert!(reader.next_frame().is_err());
    }
    #[test]
    fn prepared_writer_is_immutable_and_rejects_future_features_and_traversal() {
        let root = tempfile::tempdir().unwrap();
        let rows = rows(3);
        let shard =
            write_feature_parquet_shard(root.path(), "features.parquet", &rows, &input()).unwrap();
        assert!(
            write_feature_parquet_shard(root.path(), "features.parquet", &rows, &input()).is_err()
        );
        assert!(FeatureParquetReader::open(root.path(), vec![shard], input()).is_ok());
        assert!(
            write_feature_parquet_shard(root.path(), "../escaped.parquet", &rows, &input())
                .is_err()
        );
        let mut invalid = rows;
        invalid[1].feature_max_available_at_ms += 1;
        assert!(
            write_feature_parquet_shard(root.path(), "future.parquet", &invalid, &input()).is_err()
        );
        assert!(!root.path().join("future.parquet").exists());
    }
    #[test]
    fn prepared_reader_rejects_oversized_row_group_before_decoding() {
        let root = tempfile::tempdir().unwrap();
        let rows = rows(MAX_PREPARED_GROUP_ROWS + 1);
        let file = File::create(root.path().join("oversized.parquet")).unwrap();
        let mut writer = SerializedFileWriter::new(
            file,
            MarketFeatureFrameV1::schema(&input()).unwrap(),
            Arc::new(WriterProperties::builder().build()),
        )
        .unwrap();
        let mut group = writer.next_row_group().unwrap();
        write_columns(&mut group, MarketFeatureFrameV1::columns(&rows, &input())).unwrap();
        group.close().unwrap();
        writer.close().unwrap();
        let bytes = std::fs::read(root.path().join("oversized.parquet")).unwrap();
        let shard = SequenceShardV1 {
            file: "oversized.parquet".into(),
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
            rows: rows.len() as u64,
            first_observed_at_ms: 0,
            last_observed_at_ms: (rows.len() - 1) as i64 * 1000,
        };
        assert!(FeatureParquetReader::open(root.path(), vec![shard], input()).is_err());
    }
    #[test]
    fn prepared_view_binds_ordered_union_and_actual_gap_coverage() {
        let sources = vec![
            PreparedMarketSourceV1 {
                feature_dataset_sha256: "a".repeat(64),
                target_dataset_sha256: Some("b".repeat(64)),
                source_manifest_sha256: "c".repeat(64),
                transform_sha256: "d".repeat(64),
            },
            PreparedMarketSourceV1 {
                feature_dataset_sha256: "e".repeat(64),
                target_dataset_sha256: Some("f".repeat(64)),
                source_manifest_sha256: "1".repeat(64),
                transform_sha256: "d".repeat(64),
            },
        ];
        let mut view = PreparedMarketViewV1 {
            schema_version: PREPARED_MARKET_VIEW_SCHEMA.into(),
            source_feature_dataset_sha256: source_union_digest(
                &sources
                    .iter()
                    .map(|s| s.feature_dataset_sha256.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            source_target_dataset_sha256: Some(
                source_union_digest(
                    &sources
                        .iter()
                        .map(|s| s.target_dataset_sha256.clone().unwrap())
                        .collect::<Vec<_>>(),
                )
                .unwrap(),
            ),
            source_manifest_sha256: source_union_digest(
                &sources
                    .iter()
                    .map(|s| s.source_manifest_sha256.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap(),
            sources,
            transform_sha256: "d".repeat(64),
            data_watermark_ms: 100000,
            view: SequenceViewV1 {
                history_start_ms: 0,
                decision_start_ms: 59000,
                end_ms: 100000,
                decision_stride_ms: 1000,
            },
            feature_dataset_sha256: "2".repeat(64),
            target_dataset_sha256: Some("3".repeat(64)),
            qualified_anchors_sha256: None,
            series: vec![
                PreparedMarketSeriesV1 {
                    series_id: 1,
                    first_observed_at_ms: 0,
                    last_observed_at_ms: 3000,
                    rows: 4,
                },
                PreparedMarketSeriesV1 {
                    series_id: 2,
                    first_observed_at_ms: 5000,
                    last_observed_at_ms: 99000,
                    rows: 95,
                },
            ],
            gaps: vec![PreparedMarketGapV1 {
                last_before_ms: 3000,
                first_after_ms: 5000,
            }],
        };
        view.validate().unwrap();
        let features = MarketFeatureDatasetV1 {
            schema_version: FEATURE_PARQUET_SCHEMA.into(),
            venue: "binance-usdm".into(),
            symbol: "SOLUSDT".into(),
            source_manifest_sha256: view.source_manifest_sha256.clone(),
            input: input(),
            shards: vec![SequenceShardV1 {
                file: "features.parquet".into(),
                sha256: "4".repeat(64),
                bytes: 128,
                rows: 99,
                first_observed_at_ms: 0,
                last_observed_at_ms: 99000,
            }],
        };
        view.feature_dataset_sha256 = features.digest().unwrap();
        view.target_dataset_sha256 = None;
        view.validate_datasets(&features, None).unwrap();
        let mut false_coverage = features.clone();
        false_coverage.shards[0].rows -= 1;
        view.feature_dataset_sha256 = false_coverage.digest().unwrap();
        assert!(view.validate_datasets(&false_coverage, None).is_err());
        view.feature_dataset_sha256 = features.digest().unwrap();
        view.sources.reverse();
        assert!(view.validate().is_err());
        view.sources.reverse();
        view.series[1].rows = 96;
        assert!(view.validate().is_err());
    }
}
