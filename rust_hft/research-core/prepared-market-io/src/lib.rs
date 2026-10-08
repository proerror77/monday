//! Immutable, bounded numerical market shard IO. Feature files contain no targets.
//! Each pass verifies one compressed shard (16 MiB maximum) and retains one
//! typed row group (4,096 rows maximum). Request/receipt contracts remain pure.
use bytes::Bytes;
use hft_research_manifest::{
    market_encoder::{MarketFeatureFrameV1, MarketTargetFrameV1},
    prepared_market::{
        MAX_GROUP_UNCOMPRESSED_BYTES, MAX_PREPARED_GROUP_ROWS, MAX_PREPARED_SHARD_BYTES,
    },
    sequence::{validate_sequence_shards_with_extension, SequenceInputSpecV1, SequenceShardV1},
};
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
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

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
}
