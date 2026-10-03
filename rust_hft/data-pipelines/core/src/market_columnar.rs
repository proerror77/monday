//! Bounded, source-addressed normalized market events. This is not a feature
//! matrix, a complete venue book, or a second interpretation of raw protocols.
use crate::{
    binance_lob_replay::ReplaySequenceEvent,
    binance_market_tape_artifact::{ReplayedBinanceBookEvent, VerifiedBinanceMarketTapeSeries},
};
use anyhow::{ensure, Context, Result};
use parquet::{
    basic::Compression,
    data_type::{BoolType, ByteArray, ByteArrayType, Int64Type},
    file::{
        properties::WriterProperties,
        reader::{FileReader, SerializedFileReader},
        writer::{SerializedFileWriter, SerializedRowGroupWriter},
    },
    record::Field,
    schema::parser::parse_message_type,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs::File, io::Read, path::Path, str::FromStr, sync::Arc};

pub const MAX_ROWS: usize = 100_000;
pub const MAX_LEVEL_VALUES: usize = 2_000_000;
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
pub const SCHEMA: &str = include_str!("market_events_v1.parquet-schema");
pub const SURFACE: &str = "captured_seed_and_sequence_checked_deltas_plus_aggregate_trades";

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn normalizer_sha256() -> String {
    sha256(
        concat!(
            include_str!("market_columnar.rs"),
            include_str!("market_events_v1.parquet-schema"),
            include_str!("binance_lob_replay.rs"),
            include_str!("binance_market_tape.rs"),
            include_str!("binance_market_tape_artifact.rs"),
            include_str!("../Cargo.toml"),
            include_str!("../../Cargo.lock")
        )
        .as_bytes(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub content_sha256: String,
    pub manifest_sha256: String,
    pub start_received_ns: u64,
    pub end_received_ns: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub schema: u16,
    pub venue: String,
    pub market: String,
    pub instrument: String,
    pub session: String,
    pub normalizer_sha256: String,
    pub schema_sha256: String,
    pub sources: Vec<Source>,
}
impl Spec {
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256(&serde_json::to_vec(self)?))
    }
    pub fn scope(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256(&serde_json::to_vec(&(
            self.schema,
            &self.venue,
            &self.market,
            &self.instrument,
            &self.normalizer_sha256,
            &self.schema_sha256,
        ))?))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.venue == "binance"
                && matches!(self.market.as_str(), "usdm" | "spot"),
            "unsupported normalized market scope"
        );
        ensure!(
            !self.instrument.is_empty()
                && self.instrument.len() <= 32
                && self
                    .instrument
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()),
            "invalid instrument"
        );
        ensure!(
            !self.session.is_empty()
                && self.session.len() <= 256
                && !self.session.chars().any(char::is_control),
            "invalid capture session"
        );
        ensure!(
            digest(&self.normalizer_sha256) && self.schema_sha256 == sha256(SCHEMA.as_bytes()),
            "normalizer/schema identity missing"
        );
        ensure!(
            !self.sources.is_empty() && self.sources.len() <= 64,
            "source count outside pilot bound"
        );
        let mut hashes = BTreeSet::new();
        for (i, source) in self.sources.iter().enumerate() {
            ensure!(
                digest(&source.content_sha256)
                    && digest(&source.manifest_sha256)
                    && hashes.insert(&source.content_sha256),
                "invalid/duplicate raw identity"
            );
            ensure!(
                source.start_received_ns > 0
                    && source.end_received_ns >= source.start_received_ns
                    && source.end_received_ns <= i64::MAX as u64,
                "invalid source clock bounds"
            );
            if i > 0 {
                ensure!(
                    source.start_received_ns == self.sources[i - 1].end_received_ns,
                    "source recording boundary gap/overlap"
                );
            }
        }
        Ok(())
    }
}

/// Decimal fields store the exact Decimal(18,8) coefficient internally. Parquet
/// and CH carry the Decimal logical type; these are never floating point prices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketRow {
    pub ordinal: u64,
    pub source_sha256: String,
    pub source_row: u64,
    pub event_kind: String,
    pub source_received_ns: u64,
    pub available_ns: u64,
    pub exchange_event_ns: Option<u64>,
    pub transaction_ns: Option<u64>,
    pub first_update_id: Option<u64>,
    pub final_update_id: Option<u64>,
    pub previous_update_id: Option<u64>,
    pub aggregate_trade_id: Option<u64>,
    pub first_trade_id: Option<u64>,
    pub last_trade_id: Option<u64>,
    pub price_units: Option<i64>,
    pub quantity_units: Option<i64>,
    pub is_buyer_maker: Option<bool>,
    pub bid_price_units: Vec<i64>,
    pub bid_quantity_units: Vec<i64>,
    pub ask_price_units: Vec<i64>,
    pub ask_quantity_units: Vec<i64>,
}

pub fn decimal_units(value: Decimal) -> Result<i64> {
    let value = value.normalize();
    ensure!(
        value.scale() <= 8,
        "price/quantity exceeds Decimal(18,8) scale; refusing rounding"
    );
    let units = value
        .mantissa()
        .checked_mul(10i128.pow(8 - value.scale()))
        .context("Decimal coefficient overflow")?;
    ensure!(
        units.abs() < 1_000_000_000_000_000_000,
        "price/quantity exceeds Decimal(18,8) precision"
    );
    Ok(i64::try_from(units)?)
}
fn ns(ms: Option<u64>) -> Result<Option<u64>> {
    ms.map(|ms| {
        ms.checked_mul(1_000_000)
            .filter(|ns| *ns <= i64::MAX as u64)
            .context("venue clock overflow")
    })
    .transpose()
}
fn levels(values: &[[String; 2]]) -> Result<(Vec<i64>, Vec<i64>)> {
    let mut prices = Vec::with_capacity(values.len());
    let mut quantities = Vec::with_capacity(values.len());
    for [p, q] in values {
        let p = decimal_units(Decimal::from_str(p)?)?;
        let q = decimal_units(Decimal::from_str(q)?)?;
        ensure!(p > 0 && q >= 0, "invalid normalized level");
        prices.push(p);
        quantities.push(q);
    }
    Ok((prices, quantities))
}

#[derive(Debug, Clone)]
pub struct Batch {
    pub spec: Spec,
    pub rows: Vec<MarketRow>,
}
impl Batch {
    /// Only the externally sealed protocol validator can produce the input.
    /// Checkpoints that audit an existing book remain in raw evidence; only a
    /// validated checkpoint used as a seed becomes a normalized snapshot row.
    pub fn from_verified(
        series: &[VerifiedBinanceMarketTapeSeries],
        instrument: &str,
    ) -> Result<Self> {
        ensure!(
            series.len() == 1,
            "pilot requires one continuous capture session"
        );
        let series = &series[0];
        let verified = series.verified();
        let first = verified
            .segments()
            .first()
            .context("verified sources are empty")?;
        let spec = Spec {
            schema: 1,
            venue: "binance".into(),
            market: first.market.as_str().into(),
            instrument: instrument.into(),
            session: series.session_id().into(),
            normalizer_sha256: normalizer_sha256(),
            schema_sha256: sha256(SCHEMA.as_bytes()),
            sources: verified
                .segments()
                .iter()
                .map(|s| Source {
                    content_sha256: s.content_sha256.clone(),
                    manifest_sha256: s.manifest_sha256.clone(),
                    start_received_ns: s.start_received_at_ns,
                    end_received_ns: s.end_received_at_ns,
                })
                .collect(),
        };
        spec.validate()?;
        let book = verified
            .replayed_books()
            .iter()
            .find(|b| b.symbol == instrument)
            .context("instrument has no verified book")?;
        let mut rows = Vec::new();
        for event in book.events() {
            let (received, clock, bids, asks, snapshot) = match event {
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                    received_at_ns,
                    clock,
                    bids,
                    asks,
                }) => (*received_at_ns, clock, bids, asks, true),
                ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff {
                    received_at_ns,
                    clock,
                    bids,
                    asks,
                }) => (*received_at_ns, clock, bids, asks, false),
                ReplayedBinanceBookEvent::Checkpoint { .. } => continue,
            };
            let clock = clock
                .as_ref()
                .context("verified replay lost original clock")?;
            let source = clock
                .source
                .as_ref()
                .context("verified replay lost sealed source row")?;
            let (bp, bq) = levels(bids)?;
            let (ap, aq) = levels(asks)?;
            rows.push(MarketRow {
                ordinal: 0,
                source_sha256: source.content_sha256.clone(),
                source_row: source.row,
                event_kind: if snapshot {
                    if clock.checkpoint_seed {
                        "checkpoint_seed"
                    } else {
                        "snapshot"
                    }
                } else {
                    "diff"
                }
                .into(),
                source_received_ns: clock.raw_received_at_ns,
                available_ns: received,
                exchange_event_ns: ns(clock.exchange_event_time_ms)?,
                transaction_ns: ns(clock.transaction_time_ms)?,
                first_update_id: clock.first_update_id,
                final_update_id: Some(clock.final_update_id),
                previous_update_id: clock.previous_update_id,
                aggregate_trade_id: None,
                first_trade_id: None,
                last_trade_id: None,
                price_units: None,
                quantity_units: None,
                is_buyer_maker: None,
                bid_price_units: bp,
                bid_quantity_units: bq,
                ask_price_units: ap,
                ask_quantity_units: aq,
            });
        }
        for (trade, source) in verified
            .aggregate_trade_rows()
            .filter(|(trade, _)| trade.symbol == instrument)
        {
            rows.push(MarketRow {
                ordinal: 0,
                source_sha256: source.content_sha256.clone(),
                source_row: source.row,
                event_kind: "aggregate_trade".into(),
                source_received_ns: trade.received_at_ns,
                available_ns: trade.received_at_ns,
                exchange_event_ns: ns(Some(trade.event_time_ms))?,
                transaction_ns: ns(Some(trade.trade_time_ms))?,
                first_update_id: None,
                final_update_id: None,
                previous_update_id: None,
                aggregate_trade_id: Some(trade.aggregate_trade_id),
                first_trade_id: Some(trade.first_trade_id),
                last_trade_id: Some(trade.last_trade_id),
                price_units: Some(decimal_units(trade.price)?),
                quantity_units: Some(decimal_units(trade.quantity)?),
                is_buyer_maker: Some(trade.is_buyer_maker),
                bid_price_units: vec![],
                bid_quantity_units: vec![],
                ask_price_units: vec![],
                ask_quantity_units: vec![],
            });
        }
        let source_order: std::collections::BTreeMap<_, _> = spec
            .sources
            .iter()
            .enumerate()
            .map(|(i, s)| (s.content_sha256.as_str(), i))
            .collect();
        // Equal availability keeps seed-before-buffered-delta semantics from the
        // validator. Raw source IDs and timestamps remain independent columns.
        rows.sort_by_key(|r| {
            (
                r.available_ns,
                if matches!(r.event_kind.as_str(), "snapshot" | "checkpoint_seed") {
                    0
                } else {
                    1
                },
                source_order[r.source_sha256.as_str()],
                r.source_row,
            )
        });
        for (i, row) in rows.iter_mut().enumerate() {
            row.ordinal = i as u64 + 1;
        }
        let batch = Self { spec, rows };
        batch.validate()?;
        Ok(batch)
    }
    pub fn logical_sha256(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256(&serde_json::to_vec(&self.rows)?))
    }
    pub fn validate(&self) -> Result<()> {
        self.spec.validate()?;
        ensure!(
            !self.rows.is_empty() && self.rows.len() <= MAX_ROWS,
            "normalized row count outside pilot bound"
        );
        let sources: std::collections::BTreeMap<_, _> = self
            .spec
            .sources
            .iter()
            .map(|s| (s.content_sha256.as_str(), s))
            .collect();
        let mut keys = BTreeSet::new();
        let mut level_values = 0;
        for (i, r) in self.rows.iter().enumerate() {
            let source = sources
                .get(r.source_sha256.as_str())
                .context("row references unsealed source")?;
            ensure!(
                r.ordinal == i as u64 + 1
                    && r.source_row > 0
                    && r.source_row <= 10_000_000
                    && keys.insert((&r.source_sha256, r.source_row)),
                "duplicate/noncanonical source row"
            );
            ensure!(
                r.source_received_ns >= source.start_received_ns
                    && r.source_received_ns <= source.end_received_ns
                    && r.available_ns >= r.source_received_ns
                    && r.available_ns <= self.spec.sources.last().unwrap().end_received_ns,
                "row clock outside sealed source"
            );
            if i > 0 {
                ensure!(
                    r.available_ns >= self.rows[i - 1].available_ns,
                    "availability rollback"
                );
            }
            for id in [
                r.exchange_event_ns,
                r.transaction_ns,
                r.first_update_id,
                r.final_update_id,
                r.previous_update_id,
                r.aggregate_trade_id,
                r.first_trade_id,
                r.last_trade_id,
            ]
            .into_iter()
            .flatten()
            {
                ensure!(
                    id <= i64::MAX as u64,
                    "clock/sequence exceeds supported UInt63 range"
                );
            }
            ensure!(
                r.bid_price_units.len() == r.bid_quantity_units.len()
                    && r.ask_price_units.len() == r.ask_quantity_units.len(),
                "price/quantity array mismatch"
            );
            for prices in [&r.bid_price_units, &r.ask_price_units] {
                ensure!(
                    prices
                        .iter()
                        .all(|p| *p > 0 && *p < 1_000_000_000_000_000_000),
                    "invalid Decimal level price"
                );
            }
            for quantities in [&r.bid_quantity_units, &r.ask_quantity_units] {
                ensure!(
                    quantities
                        .iter()
                        .all(|q| *q >= 0 && *q < 1_000_000_000_000_000_000),
                    "invalid Decimal level quantity"
                );
            }
            level_values += r.bid_price_units.len() + r.ask_price_units.len();
            match r.event_kind.as_str() {
                "aggregate_trade" => ensure!(
                    r.aggregate_trade_id.is_some()
                        && r.first_trade_id
                            .zip(r.last_trade_id)
                            .is_some_and(|(f, l)| f <= l)
                        && r.price_units
                            .is_some_and(|p| p > 0 && p < 1_000_000_000_000_000_000)
                        && r.quantity_units
                            .is_some_and(|q| q > 0 && q < 1_000_000_000_000_000_000)
                        && r.is_buyer_maker.is_some()
                        && r.exchange_event_ns.is_some()
                        && r.transaction_ns.is_some()
                        && r.final_update_id.is_none()
                        && r.bid_price_units.is_empty()
                        && r.ask_price_units.is_empty(),
                    "invalid aggregate trade columns"
                ),
                "snapshot" | "checkpoint_seed" | "diff" => {
                    ensure!(
                        r.final_update_id.is_some()
                            && r.aggregate_trade_id.is_none()
                            && r.first_trade_id.is_none()
                            && r.last_trade_id.is_none()
                            && r.price_units.is_none()
                            && r.quantity_units.is_none()
                            && r.is_buyer_maker.is_none(),
                        "invalid book columns"
                    );
                    if r.event_kind == "diff" {
                        ensure!(
                            r.exchange_event_ns.is_some()
                                && r.first_update_id
                                    .zip(r.final_update_id)
                                    .is_some_and(|(f, l)| f <= l),
                            "delta lost original venue clock/sequence"
                        );
                    } else {
                        ensure!(
                            r.exchange_event_ns.is_none()
                                && r.transaction_ns.is_none()
                                && r.first_update_id.is_none()
                                && !r.bid_price_units.is_empty()
                                && !r.ask_price_units.is_empty(),
                            "seed fabricated venue clock or lost book"
                        );
                    }
                }
                _ => anyhow::bail!("unsupported normalized event"),
            }
        }
        ensure!(
            level_values <= MAX_LEVEL_VALUES,
            "normalized level count outside pilot bound"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub spec: Spec,
    pub batch_id: String,
    pub surface: String,
    pub parquet_sha256: String,
    pub parquet_bytes: u64,
    pub logical_sha256: String,
    pub rows: u64,
    pub first_available_ns: u64,
    pub last_available_ns: u64,
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.batch_id == self.spec.id()?
                && self.surface == SURFACE
                && digest(&self.parquet_sha256)
                && digest(&self.logical_sha256),
            "invalid normalized manifest identities"
        );
        ensure!(
            self.parquet_bytes > 0
                && self.parquet_bytes <= MAX_FILE_BYTES
                && self.rows > 0
                && self.rows <= MAX_ROWS as u64
                && self.first_available_ns <= self.last_available_ns,
            "invalid normalized manifest bounds"
        );
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        Ok(sha256(&serde_json::to_vec(self)?))
    }
    pub fn verify_file(&self, path: &Path) -> Result<Batch> {
        self.validate()?;
        self.verify_bytes(&read_parquet_bytes(path)?)
    }
    pub fn verify_bytes(&self, bytes: &bytes::Bytes) -> Result<Batch> {
        self.validate()?;
        ensure!(
            bytes.len() as u64 == self.parquet_bytes && sha256(bytes) == self.parquet_sha256,
            "Parquet size/content hash changed"
        );
        let reader = SerializedFileReader::new(bytes.clone())?;
        ensure!(
            reader
                .metadata()
                .file_metadata()
                .schema_descr()
                .root_schema()
                == &parse_message_type(SCHEMA)?,
            "Parquet physical/logical schema mismatch"
        );
        ensure!(
            reader.metadata().file_metadata().num_rows() == self.rows as i64,
            "Parquet footer row count mismatch"
        );
        let mut decoded_bytes = 0u64;
        for column in reader
            .metadata()
            .row_groups()
            .iter()
            .flat_map(|g| g.columns())
        {
            ensure!(
                column.num_values() >= 0
                    && column.num_values() as usize <= MAX_LEVEL_VALUES + MAX_ROWS
                    && column.uncompressed_size() >= 0,
                "Parquet column decode budget exceeded"
            );
            decoded_bytes = decoded_bytes
                .checked_add(column.uncompressed_size() as u64)
                .context("Parquet decode size overflow")?;
        }
        ensure!(
            decoded_bytes <= 512 * 1024 * 1024,
            "Parquet uncompressed data exceeds pilot bound"
        );
        let mut rows = Vec::new();
        for row in reader.get_row_iter(None)? {
            ensure!(rows.len() < MAX_ROWS, "Parquet decode row budget exceeded");
            rows.push(decode(
                row?.get_column_iter().map(|(_, f)| f).collect(),
                &self.spec,
            )?);
        }
        let batch = Batch {
            spec: self.spec.clone(),
            rows,
        };
        batch.validate()?;
        ensure!(
            batch.rows.len() as u64 == self.rows
                && batch.logical_sha256()? == self.logical_sha256
                && batch.rows[0].available_ns == self.first_available_ns
                && batch.rows.last().unwrap().available_ns == self.last_available_ns,
            "Parquet logical readback mismatch"
        );
        Ok(batch)
    }
}
fn read_parquet_bytes(path: &Path) -> Result<bytes::Bytes> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    ensure!(
        file.metadata()?.is_file() && file.metadata()?.len() <= MAX_FILE_BYTES,
        "Parquet file type/size outside bound"
    );
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_FILE_BYTES,
        "Parquet changed beyond bound"
    );
    Ok(bytes.into())
}
/// Keep the exact verified bytes in memory through CH transmission; a workspace
/// path cannot change between hash verification, decode and INSERT.
pub struct SealedParquet {
    manifest: Manifest,
    batch: Batch,
    bytes: bytes::Bytes,
}
impl SealedParquet {
    pub fn open(manifest: Manifest, expected_manifest_sha256: &str, path: &Path) -> Result<Self> {
        ensure!(
            manifest.id()? == expected_manifest_sha256,
            "manifest is not the externally selected identity"
        );
        let bytes = read_parquet_bytes(path)?;
        let batch = manifest.verify_bytes(&bytes)?;
        Ok(Self {
            manifest,
            batch,
            bytes,
        })
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn rows(&self) -> &[MarketRow] {
        &self.batch.rows
    }
    pub fn bytes(&self) -> bytes::Bytes {
        self.bytes.clone()
    }
}
fn file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut chunk = [0; 65536];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        hash.update(&chunk[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Create-new output only. An interrupted file cannot receive a success manifest.
pub fn write_parquet(batch: &Batch, path: &Path) -> Result<Manifest> {
    batch.validate()?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_created_by("monday-market-events-v1".into())
        .build();
    let mut writer = SerializedFileWriter::new(
        file,
        Arc::new(parse_message_type(SCHEMA)?),
        Arc::new(properties),
    )?;
    for rows in batch.rows.chunks(4096) {
        write_group(&mut writer, &batch.spec, rows)?;
    }
    writer.close()?;
    File::open(path)?.sync_all()?;
    let manifest = Manifest {
        spec: batch.spec.clone(),
        batch_id: batch.spec.id()?,
        surface: SURFACE.into(),
        parquet_sha256: file_sha256(path)?,
        parquet_bytes: std::fs::metadata(path)?.len(),
        logical_sha256: batch.logical_sha256()?,
        rows: batch.rows.len() as u64,
        first_available_ns: batch.rows[0].available_ns,
        last_available_ns: batch.rows.last().unwrap().available_ns,
    };
    // A real independent footer/row decoder verifies the written columns.
    manifest.verify_file(path)?;
    Ok(manifest)
}
fn write_group(
    writer: &mut SerializedFileWriter<File>,
    spec: &Spec,
    rows: &[MarketRow],
) -> Result<()> {
    let mut group = writer.next_row_group()?;
    let mut strings = |values: Vec<String>| -> Result<()> {
        let values: Vec<_> = values.iter().map(|s| ByteArray::from(s.as_str())).collect();
        let mut column = group.next_column()?.context("missing string column")?;
        column
            .typed::<ByteArrayType>()
            .write_batch(&values, None, None)?;
        column.close()?;
        Ok(())
    };
    for value in [
        spec.id()?,
        spec.venue.clone(),
        spec.market.clone(),
        spec.instrument.clone(),
        spec.session.clone(),
        spec.normalizer_sha256.clone(),
    ] {
        strings(vec![value; rows.len()])?;
    }
    strings(rows.iter().map(|r| r.source_sha256.clone()).collect())?;
    strings(rows.iter().map(|r| r.event_kind.clone()).collect())?;
    for values in [
        rows.iter().map(|r| r.ordinal).collect::<Vec<_>>(),
        rows.iter().map(|r| r.source_row).collect(),
        rows.iter().map(|r| r.source_received_ns).collect(),
        rows.iter().map(|r| r.available_ns).collect(),
    ] {
        let values: Vec<i64> = values
            .into_iter()
            .map(i64::try_from)
            .collect::<std::result::Result<_, _>>()?;
        integers(&mut group, &values, None, None)?;
    }
    for values in [
        rows.iter().map(|r| r.exchange_event_ns).collect::<Vec<_>>(),
        rows.iter().map(|r| r.transaction_ns).collect(),
        rows.iter().map(|r| r.first_update_id).collect(),
        rows.iter().map(|r| r.final_update_id).collect(),
        rows.iter().map(|r| r.previous_update_id).collect(),
        rows.iter().map(|r| r.aggregate_trade_id).collect(),
        rows.iter().map(|r| r.first_trade_id).collect(),
        rows.iter().map(|r| r.last_trade_id).collect(),
    ] {
        let defs: Vec<i16> = values.iter().map(|v| i16::from(v.is_some())).collect();
        let values: Vec<i64> = values
            .into_iter()
            .flatten()
            .map(i64::try_from)
            .collect::<std::result::Result<_, _>>()?;
        integers(&mut group, &values, Some(&defs), None)?;
    }
    for values in [
        rows.iter().map(|r| r.price_units).collect::<Vec<_>>(),
        rows.iter().map(|r| r.quantity_units).collect(),
    ] {
        let defs: Vec<i16> = values.iter().map(|v| i16::from(v.is_some())).collect();
        let values: Vec<i64> = values.into_iter().flatten().collect();
        integers(&mut group, &values, Some(&defs), None)?;
    }
    let defs: Vec<i16> = rows
        .iter()
        .map(|r| i16::from(r.is_buyer_maker.is_some()))
        .collect();
    let values: Vec<bool> = rows.iter().filter_map(|r| r.is_buyer_maker).collect();
    let mut column = group.next_column()?.context("missing maker column")?;
    column
        .typed::<BoolType>()
        .write_batch(&values, Some(&defs), None)?;
    column.close()?;
    for selector in [
        (|r: &MarketRow| &r.bid_price_units) as fn(&MarketRow) -> &Vec<i64>,
        |r| &r.bid_quantity_units,
        |r| &r.ask_price_units,
        |r| &r.ask_quantity_units,
    ] {
        let mut values = Vec::new();
        let mut defs = Vec::new();
        let mut reps = Vec::new();
        for row in rows {
            let list = selector(row);
            if list.is_empty() {
                defs.push(0);
                reps.push(0);
            } else {
                for (i, value) in list.iter().enumerate() {
                    values.push(*value);
                    defs.push(1);
                    reps.push(i16::from(i > 0));
                }
            }
        }
        integers(&mut group, &values, Some(&defs), Some(&reps))?;
    }
    ensure!(
        group.next_column()?.is_none(),
        "unwritten normalized column"
    );
    group.close()?;
    Ok(())
}
fn integers(
    group: &mut SerializedRowGroupWriter<'_, File>,
    values: &[i64],
    defs: Option<&[i16]>,
    reps: Option<&[i16]>,
) -> Result<()> {
    let mut column = group
        .next_column()?
        .context("missing integer/decimal column")?;
    column
        .typed::<Int64Type>()
        .write_batch(values, defs, reps)?;
    column.close()?;
    Ok(())
}
fn number(field: &Field) -> Result<u64> {
    if let Field::Long(n) = field {
        Ok(u64::try_from(*n)?)
    } else {
        anyhow::bail!("wrong integer field")
    }
}
fn optional(field: &Field) -> Result<Option<u64>> {
    if matches!(field, Field::Null) {
        Ok(None)
    } else {
        Ok(Some(number(field)?))
    }
}
fn decimal(field: &Field) -> Result<i64> {
    if let Field::Decimal(d) = field {
        ensure!(
            d.precision() == 18 && d.scale() == 8 && d.data().len() == 8,
            "wrong Decimal logical type"
        );
        Ok(i64::from_be_bytes(d.data().try_into()?))
    } else {
        anyhow::bail!("wrong Decimal field")
    }
}
fn optional_decimal(field: &Field) -> Result<Option<i64>> {
    if matches!(field, Field::Null) {
        Ok(None)
    } else {
        Ok(Some(decimal(field)?))
    }
}
fn list(field: &Field) -> Result<Vec<i64>> {
    if let Field::ListInternal(list) = field {
        list.elements().iter().map(decimal).collect()
    } else {
        anyhow::bail!("wrong Decimal LIST field")
    }
}
fn string(field: &Field) -> Result<String> {
    if let Field::Str(s) = field {
        Ok(s.clone())
    } else {
        anyhow::bail!("wrong string field")
    }
}
fn decode(fields: Vec<&Field>, spec: &Spec) -> Result<MarketRow> {
    ensure!(fields.len() == 27, "normalized column count mismatch");
    for (field, expected) in fields[..6].iter().zip([
        spec.id()?,
        spec.venue.clone(),
        spec.market.clone(),
        spec.instrument.clone(),
        spec.session.clone(),
        spec.normalizer_sha256.clone(),
    ]) {
        ensure!(string(field)? == expected, "Parquet scope column changed");
    }
    let fields = &fields[6..];
    Ok(MarketRow {
        source_sha256: string(fields[0])?,
        event_kind: string(fields[1])?,
        ordinal: number(fields[2])?,
        source_row: number(fields[3])?,
        source_received_ns: number(fields[4])?,
        available_ns: number(fields[5])?,
        exchange_event_ns: optional(fields[6])?,
        transaction_ns: optional(fields[7])?,
        first_update_id: optional(fields[8])?,
        final_update_id: optional(fields[9])?,
        previous_update_id: optional(fields[10])?,
        aggregate_trade_id: optional(fields[11])?,
        first_trade_id: optional(fields[12])?,
        last_trade_id: optional(fields[13])?,
        price_units: optional_decimal(fields[14])?,
        quantity_units: optional_decimal(fields[15])?,
        is_buyer_maker: match fields[16] {
            Field::Null => None,
            Field::Bool(b) => Some(*b),
            _ => anyhow::bail!("wrong maker column"),
        },
        bid_price_units: list(fields[17])?,
        bid_quantity_units: list(fields[18])?,
        ask_price_units: list(fields[19])?,
        ask_quantity_units: list(fields[20])?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimal_contract_preserves_exact_precision_and_refuses_rounding() {
        assert_eq!(
            decimal_units(Decimal::from_str("100.50000000").unwrap()).unwrap(),
            10_050_000_000
        );
        assert_eq!(
            decimal_units(Decimal::from_str("0.00000001").unwrap()).unwrap(),
            1
        );
        assert!(decimal_units(Decimal::from_str("0.000000001").unwrap()).is_err());
        assert!(decimal_units(Decimal::from_str("10000000000").unwrap()).is_err());
        assert!(ns(Some(u64::MAX)).is_err());
    }
}
