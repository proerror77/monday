use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::{
    source_revision as governed_source_revision, Market as LobMarket, ReplaySequenceEvent,
};
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape_with_required_lob_continuity,
    BinanceMarketTapeTriplet, BinanceMarketTapeTrustAnchor, ReplayedBinanceBookEvent,
    VerifiedBinanceMarketTape,
};
use parquet::basic::Compression;
use parquet::data_type::{ByteArray, ByteArrayType, Int64Type};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::file::writer::{SerializedFileWriter, SerializedRowGroupWriter};
use parquet::schema::parser::parse_message_type;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const DATASET_KIND: &str = "backtest_canonical_replay_parquet";
const SCHEMA_VERSION: &str = "binance-replay-parquet-v1";
const PARQUET_SCHEMA: &str = "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8";
const PARQUET_MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";
const ROW_GROUP_ROWS: usize = 100_000;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Market {
    Spot,
    Usdm,
}

impl Market {
    fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }

    fn as_lob_market(self) -> LobMarket {
        match self {
            Self::Spot => LobMarket::Spot,
            Self::Usdm => LobMarket::Usdm,
        }
    }

    fn dataset(self) -> &'static str {
        match self {
            Self::Spot => "binance_spot_lob",
            Self::Usdm => "binance_usdm_lob",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "binance-replay-parquet-materializer",
    about = "Verify Binance raw LOB triplets and publish canonical replay Parquet"
)]
struct Args {
    #[arg(long)]
    mission_id: String,
    #[arg(long)]
    symbol: String,
    #[arg(long, value_enum)]
    market: Market,
    #[arg(long, required = true)]
    segment: Vec<PathBuf>,
    #[arg(long, required = true)]
    segment_content_sha256: Vec<String>,
    #[arg(long, required = true)]
    segment_manifest_sha256: Vec<String>,
    #[arg(long)]
    artifact_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct SourceSegmentEvidence {
    file: String,
    sha256: String,
    collector_manifest_sha256: String,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Clone, Serialize)]
struct CanonicalManifest {
    dataset_kind: String,
    schema_version: String,
    format: String,
    parquet_schema: String,
    mission_id: String,
    market: String,
    symbol: String,
    dataset: String,
    modalities: Vec<String>,
    source_revision: String,
    source_segments: Vec<SourceSegmentEvidence>,
    rows: usize,
    first_event_time_us: i64,
    last_event_time_us: i64,
    sequence_start: u64,
    sequence_end: u64,
    artifact_path: PathBuf,
    artifact_sha256: String,
    point_in_time: bool,
}

#[derive(Debug, Serialize)]
struct PublishedMaterialization {
    manifest: CanonicalManifest,
    manifest_path: PathBuf,
    manifest_sha256: String,
}

#[derive(Debug, Clone)]
struct CanonicalEvent {
    timestamp_us: i64,
    sequence: u64,
    event: &'static str,
    payload_json: String,
}

#[derive(Serialize)]
struct ReplayPayload {
    bids: Vec<[f64; 2]>,
    asks: Vec<[f64; 2]>,
}

fn main() -> Result<()> {
    let published = materialize(&Args::parse())?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &published)?;
    println!();
    Ok(())
}

fn materialize(args: &Args) -> Result<PublishedMaterialization> {
    let mission_id = args.mission_id.trim();
    let symbol = args.symbol.trim().to_ascii_uppercase();
    if mission_id.is_empty() || symbol.is_empty() {
        bail!("mission id and symbol are required");
    }

    let verified = verify_segments(args)?;
    if verified
        .segments()
        .iter()
        .any(|segment| segment.market != args.market.as_lob_market())
    {
        bail!("verified market-tape does not match requested market");
    }
    let source_segments = source_segment_evidence(&verified);
    let replayed_book = verified
        .replayed_books()
        .iter()
        .find(|book| book.symbol == symbol)
        .with_context(|| {
            format!("verified market-tape does not contain requested symbol {symbol}")
        })?;
    let rows = canonical_events(replayed_book.events())?;
    let artifact_dir = canonical_output_dir(&args.artifact_dir)?;
    let temporary_artifact = temporary_path(&artifact_dir.join("canonical-replay.parquet"))?;
    if let Err(error) = write_parquet(&temporary_artifact, &rows) {
        let _ = fs::remove_file(&temporary_artifact);
        return Err(error);
    }
    let artifact_sha256 = sha256_file(&temporary_artifact)?;
    let artifact_name = format!("{artifact_sha256}.parquet");
    let artifact_path = artifact_dir.join(&artifact_name);
    publish_temp_immutable(&temporary_artifact, &artifact_path, &artifact_sha256)?;

    let first = rows.first().context("canonical event tape is empty")?;
    let last = rows.last().context("canonical event tape is empty")?;
    let source_revision =
        governed_source_revision(source_segments.iter().map(|source| source.sha256.as_str()));
    let manifest = CanonicalManifest {
        dataset_kind: DATASET_KIND.to_string(),
        schema_version: SCHEMA_VERSION.to_string(),
        format: "parquet".to_string(),
        parquet_schema: PARQUET_SCHEMA.to_string(),
        mission_id: mission_id.to_string(),
        market: args.market.as_str().to_string(),
        symbol,
        dataset: args.market.dataset().to_string(),
        modalities: vec!["lob".to_string()],
        source_revision,
        source_segments,
        rows: rows.len(),
        first_event_time_us: first.timestamp_us,
        last_event_time_us: last.timestamp_us,
        sequence_start: first.sequence,
        sequence_end: last.sequence,
        artifact_path: PathBuf::from(artifact_name),
        artifact_sha256,
        point_in_time: true,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    let manifest_path = artifact_dir.join(format!("{manifest_sha256}.canonical-manifest.json"));
    publish_immutable_bytes(&manifest_path, &manifest_bytes)?;

    Ok(PublishedMaterialization {
        manifest,
        manifest_path,
        manifest_sha256,
    })
}

fn verify_segments(args: &Args) -> Result<VerifiedBinanceMarketTape> {
    let count = args.segment.len();
    if count == 0
        || args.segment_content_sha256.len() != count
        || args.segment_manifest_sha256.len() != count
    {
        bail!(
            "--segment, --segment-content-sha256, and --segment-manifest-sha256 must have equal nonzero lengths"
        );
    }

    let mut content_sha256s = BTreeSet::new();
    let mut sealed = Vec::with_capacity(count);
    for ((input_path, content_sha256), manifest_sha256) in args
        .segment
        .iter()
        .zip(&args.segment_content_sha256)
        .zip(&args.segment_manifest_sha256)
    {
        let path = fs::canonicalize(input_path)
            .with_context(|| format!("cannot resolve segment path {}", input_path.display()))?;
        if !content_sha256s.insert(content_sha256.clone()) {
            bail!("duplicate LOB segment supplied");
        }
        let triplet = BinanceMarketTapeTriplet {
            data: path.clone(),
            manifest: sibling(&path, ".manifest.json")?,
            success: sibling(&path, "._SUCCESS")?,
        };
        let trust = BinanceMarketTapeTrustAnchor::from_lower_hex(content_sha256, manifest_sha256)?;
        sealed.push(seal_binance_market_tape_triplet(&triplet, &trust)?);
    }
    verify_binance_market_tape_with_required_lob_continuity(sealed)
}

fn source_segment_evidence(verified: &VerifiedBinanceMarketTape) -> Vec<SourceSegmentEvidence> {
    verified
        .segments()
        .iter()
        .map(|segment| SourceSegmentEvidence {
            file: segment.file.clone(),
            success_marker_sha256: hex::encode(Sha256::digest(format!(
                "{}\n",
                segment.content_sha256
            ))),
            sha256: segment.content_sha256.clone(),
            collector_manifest_sha256: segment.manifest_sha256.clone(),
            start_received_at_ns: segment.start_received_at_ns,
            end_received_at_ns: segment.end_received_at_ns,
            events: segment.events,
        })
        .collect()
}

fn canonical_events(events: &[ReplayedBinanceBookEvent]) -> Result<Vec<CanonicalEvent>> {
    let mut rows = Vec::new();
    let mut previous_timestamp = None;
    for event in events {
        let ReplayedBinanceBookEvent::Replay(replay) = event else {
            continue;
        };
        let (event_name, received_at_ns, bids, asks) = match replay {
            ReplaySequenceEvent::Snapshot {
                received_at_ns,
                bids,
                asks,
            } => (
                "snapshot",
                *received_at_ns,
                bids.as_slice(),
                asks.as_slice(),
            ),
            ReplaySequenceEvent::Diff {
                received_at_ns,
                bids,
                asks,
            } => (
                "l2_update",
                *received_at_ns,
                bids.as_slice(),
                asks.as_slice(),
            ),
        };
        let timestamp_us = received_at_us(received_at_ns)?;
        if previous_timestamp.is_some_and(|previous| timestamp_us < previous) {
            bail!("verified replay events are not ordered by receive time");
        }
        previous_timestamp = Some(timestamp_us);
        let sequence = u64::try_from(rows.len())
            .context("canonical event sequence overflow")?
            .checked_add(1)
            .context("canonical event sequence overflow")?;
        let payload_json = serde_json::to_string(&ReplayPayload {
            bids: normalize_levels(bids, "bids")?,
            asks: normalize_levels(asks, "asks")?,
        })?;
        rows.push(CanonicalEvent {
            timestamp_us,
            sequence,
            event: event_name,
            payload_json,
        });
    }
    if rows.is_empty() || rows.first().is_none_or(|row| row.event != "snapshot") {
        bail!("verified replay tape has no snapshot-led event sequence");
    }
    Ok(rows)
}

fn normalize_levels(levels: &[[String; 2]], field: &str) -> Result<Vec<[f64; 2]>> {
    levels
        .iter()
        .map(|[price, quantity]| {
            let price = price
                .parse::<f64>()
                .with_context(|| format!("{field} contains a non-numeric price"))?;
            let quantity = quantity
                .parse::<f64>()
                .with_context(|| format!("{field} contains a non-numeric quantity"))?;
            if !price.is_finite() || !quantity.is_finite() || price <= 0.0 || quantity < 0.0 {
                bail!("{field} contains an invalid price or quantity");
            }
            Ok([price, quantity])
        })
        .collect()
}

fn received_at_us(received_at_ns: u64) -> Result<i64> {
    let micros = received_at_ns / 1_000 + u64::from(!received_at_ns.is_multiple_of(1_000));
    i64::try_from(micros).context("receive time exceeds i64 microseconds")
}

fn write_parquet(path: &Path, rows: &[CanonicalEvent]) -> Result<()> {
    let schema = Arc::new(parse_message_type(PARQUET_MESSAGE)?);
    let properties = Arc::new(
        WriterProperties::builder()
            .set_created_by("monday-canonical-replay-parquet-v1".to_string())
            .set_compression(Compression::ZSTD(Default::default()))
            .set_dictionary_enabled(false)
            .set_statistics_enabled(EnabledStatistics::Chunk)
            .set_max_row_group_row_count(Some(ROW_GROUP_ROWS))
            .build(),
    );
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut writer = SerializedFileWriter::new(file, schema, properties)?;
    for chunk in rows.chunks(ROW_GROUP_ROWS) {
        let timestamps = chunk.iter().map(|row| row.timestamp_us).collect::<Vec<_>>();
        let sequences = chunk
            .iter()
            .map(|row| i64::try_from(row.sequence).context("canonical sequence exceeds i64"))
            .collect::<Result<Vec<_>>>()?;
        let events = chunk
            .iter()
            .map(|row| row.event.to_string())
            .collect::<Vec<_>>();
        let payloads = chunk
            .iter()
            .map(|row| row.payload_json.clone())
            .collect::<Vec<_>>();
        let mut group = writer.next_row_group()?;
        write_i64_column(&mut group, &timestamps)?;
        write_i64_column(&mut group, &sequences)?;
        write_utf8_column(&mut group, &events)?;
        write_utf8_column(&mut group, &payloads)?;
        group.close()?;
    }
    writer.close()?;
    Ok(())
}

fn write_i64_column(group: &mut SerializedRowGroupWriter<'_, File>, values: &[i64]) -> Result<()> {
    let mut column = group
        .next_column()?
        .context("canonical Parquet schema is missing an integer column")?;
    column
        .typed::<Int64Type>()
        .write_batch(values, None, None)?;
    column.close()?;
    Ok(())
}

fn write_utf8_column(
    group: &mut SerializedRowGroupWriter<'_, File>,
    values: &[String],
) -> Result<()> {
    let encoded = values
        .iter()
        .map(|value| ByteArray::from(value.as_str()))
        .collect::<Vec<_>>();
    let mut column = group
        .next_column()?
        .context("canonical Parquet schema is missing a UTF-8 column")?;
    column
        .typed::<ByteArrayType>()
        .write_batch(&encoded, None, None)?;
    column.close()?;
    Ok(())
}

fn canonical_output_dir(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path).with_context(|| {
        format!(
            "cannot create canonical artifact directory {}",
            path.display()
        )
    })?;
    fs::canonicalize(path).with_context(|| {
        format!(
            "cannot resolve canonical artifact directory {}",
            path.display()
        )
    })
}

fn publish_immutable_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = temporary_path(path)?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    let expected = hex::encode(Sha256::digest(bytes));
    publish_temp_immutable(&temporary, path, &expected)
}

fn publish_temp_immutable(temporary: &Path, path: &Path, expected_sha256: &str) -> Result<()> {
    match fs::hard_link(temporary, path) {
        Ok(()) => fs::remove_file(temporary)?,
        Err(_error) if path.exists() => {
            let existing = sha256_file(path)?;
            let _ = fs::remove_file(temporary);
            if existing != expected_sha256 {
                bail!(
                    "immutable artifact already exists with different content: {}",
                    path.display()
                );
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    let file_name = file_name(path)?;
    let unique = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    Ok(path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        unique
    )))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut source = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn sibling(path: &Path, suffix: &str) -> Result<PathBuf> {
    Ok(path.with_file_name(format!("{}{suffix}", file_name(path)?)))
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .context("path has no UTF-8 file name")
}
