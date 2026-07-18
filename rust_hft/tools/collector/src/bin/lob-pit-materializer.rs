use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::{
    source_revision as governed_source_revision, Market as LobMarket, ReplaySequenceEvent,
};
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape, BinanceMarketTapeTriplet,
    BinanceMarketTapeTrustAnchor, ReplayedBinanceBookEvent, VerifiedBinanceMarketTape,
};
use hft_collector::{DataModality, PointInTimeFeatureRow};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MATERIALIZATION_SCHEMA: &str = "binance-lob-pit-v1";

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
}

#[derive(Debug, Parser)]
#[command(
    name = "lob-pit-materializer",
    about = "Replay Binance LOB segments into immutable PIT feature artifacts"
)]
struct Args {
    #[arg(long)]
    mission_id: String,
    #[arg(long)]
    symbol: String,
    #[arg(long, value_enum)]
    market: Market,
    #[arg(long, default_value_t = 1_000)]
    bucket_ms: u64,
    #[arg(long, default_value_t = 5)]
    label_horizon_buckets: usize,
    #[arg(long, default_value_t = 5)]
    top_depth: usize,
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
    path: PathBuf,
    sha256: String,
    collector_manifest_path: PathBuf,
    collector_manifest_sha256: String,
    success_marker_path: PathBuf,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Clone, PartialEq)]
struct BookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

#[derive(Debug, Clone)]
struct BookSample {
    series_id: u64,
    time_ns: u64,
    mid_price: f64,
    spread_bps: f64,
    bid_depth: f64,
    ask_depth: f64,
    top_depth_imbalance: f64,
    book_imbalance: f64,
}

#[derive(Debug, Serialize)]
struct MaterializationReport {
    dataset_kind: String,
    schema_version: String,
    mission_id: String,
    symbol: String,
    market: String,
    bucket_ms: u64,
    label_horizon_buckets: usize,
    top_depth: usize,
    source_revision: String,
    source_segments: Vec<SourceSegmentEvidence>,
    rows: usize,
    first_event_time: DateTime<Utc>,
    last_event_time: DateTime<Utc>,
    artifact_path: PathBuf,
    artifact_sha256: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct PublishedMaterialization {
    report: MaterializationReport,
    report_path: PathBuf,
    report_sha256: String,
}

struct Replay {
    bucket_ns: u64,
    depth: usize,
    state: Option<BookState>,
    samples: Vec<BookSample>,
    next_bucket_ns: Option<u64>,
    series_id: u64,
}

impl Replay {
    fn new(bucket_ns: u64, depth: usize) -> Self {
        Self {
            bucket_ns,
            depth,
            state: None,
            samples: Vec::new(),
            next_bucket_ns: None,
            series_id: 0,
        }
    }

    fn start_series(&mut self, state: BookState, received_at_ns: u64) -> Result<()> {
        self.state = Some(state);
        self.series_id = self
            .series_id
            .checked_add(1)
            .context("series id overflow")?;
        self.next_bucket_ns = Some(ceil_bucket(received_at_ns, self.bucket_ns)?);
        Ok(())
    }

    fn emit_before(&mut self, received_at_ns: u64) -> Result<()> {
        self.emit_until(received_at_ns, false)
    }

    fn emit_at(&mut self, received_at_ns: u64) -> Result<()> {
        self.emit_until(received_at_ns, true)
    }

    fn emit_until(&mut self, received_at_ns: u64, inclusive: bool) -> Result<()> {
        loop {
            let Some(next_bucket_ns) = self.next_bucket_ns else {
                return Ok(());
            };
            let due = if inclusive {
                next_bucket_ns <= received_at_ns
            } else {
                next_bucket_ns < received_at_ns
            };
            if !due {
                return Ok(());
            }
            let state = self.state.as_ref().context("bucket has no replay state")?;
            self.samples.push(sample_book(
                state,
                self.series_id,
                next_bucket_ns,
                self.depth,
            )?);
            self.next_bucket_ns = Some(
                next_bucket_ns
                    .checked_add(self.bucket_ns)
                    .context("bucket time overflow")?,
            );
        }
    }

    fn consume(&mut self, events: &[ReplayedBinanceBookEvent]) -> Result<()> {
        let mut start = 0;
        while start < events.len() {
            let received_at_ns = events[start].received_at_ns();
            let mut end = start + 1;
            while end < events.len() && events[end].received_at_ns() == received_at_ns {
                end += 1;
            }
            self.emit_before(received_at_ns)?;
            for event in &events[start..end] {
                match event {
                    ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                        bids,
                        asks,
                        ..
                    }) => self.start_series(
                        BookState {
                            bids: parse_levels(bids, "bid")?,
                            asks: parse_levels(asks, "ask")?,
                        },
                        received_at_ns,
                    )?,
                    ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff {
                        bids,
                        asks,
                        ..
                    }) => {
                        let state = self.state.as_mut().context("diff has no replay state")?;
                        apply_levels(&mut state.bids, bids, "bid")?;
                        apply_levels(&mut state.asks, asks, "ask")?;
                    }
                    ReplayedBinanceBookEvent::Checkpoint { .. } => {}
                }
            }
            self.emit_at(received_at_ns)?;
            start = end;
        }
        Ok(())
    }
}

fn main() -> Result<()> {
    let published = materialize(&Args::parse())?;
    serde_json::to_writer_pretty(std::io::stdout().lock(), &published)?;
    println!();
    Ok(())
}

fn materialize(args: &Args) -> Result<PublishedMaterialization> {
    let mission_id = args.mission_id.trim();
    let symbol = args.symbol.trim().to_uppercase();
    if mission_id.is_empty() || symbol.is_empty() {
        bail!("mission id and symbol are required");
    }
    if args.bucket_ms == 0 || args.label_horizon_buckets == 0 || args.top_depth == 0 {
        bail!("bucket, label horizon, and top depth must be positive");
    }
    let (verified, segment_paths) = verify_segments(args)?;
    if verified
        .segments()
        .iter()
        .any(|segment| segment.market != args.market.as_lob_market())
    {
        bail!("verified market-tape does not match requested market");
    }
    let source_segments = source_segment_evidence(&verified, segment_paths)?;
    let book = verified
        .replayed_books()
        .iter()
        .find(|book| book.symbol == symbol)
        .with_context(|| format!("verified market-tape does not contain requested symbol {symbol}"))?;

    let bucket_ns = args
        .bucket_ms
        .checked_mul(1_000_000)
        .context("bucket size overflow")?;
    let mut replay = Replay::new(bucket_ns, args.top_depth);
    replay.consume(book.events())?;

    let revision = source_revision(&source_segments);
    let created_at = Utc::now();
    let ingestion_time = datetime_ns(
        source_segments
            .iter()
            .map(|segment| segment.end_received_at_ns)
            .max()
            .context("segments have no end time")?,
    )?;
    let rows = materialize_rows(
        &replay.samples,
        args.market,
        &symbol,
        &revision,
        args.label_horizon_buckets,
        args.top_depth,
        ingestion_time,
    )?;
    let artifact_bytes = encode_rows(&rows)?;
    let artifact_sha256 = hex::encode(Sha256::digest(&artifact_bytes));
    let artifact_path = args.artifact_dir.join(format!("{artifact_sha256}.jsonl"));
    publish_immutable(&artifact_path, &artifact_bytes)?;

    let report = MaterializationReport {
        dataset_kind: "lob_point_in_time_materialization".to_string(),
        schema_version: MATERIALIZATION_SCHEMA.to_string(),
        mission_id: mission_id.to_string(),
        symbol,
        market: args.market.as_str().to_string(),
        bucket_ms: args.bucket_ms,
        label_horizon_buckets: args.label_horizon_buckets,
        top_depth: args.top_depth,
        source_revision: revision,
        source_segments,
        rows: rows.len(),
        first_event_time: rows.first().context("feature rows are empty")?.event_time,
        last_event_time: rows.last().context("feature rows are empty")?.event_time,
        artifact_path,
        artifact_sha256,
        created_at,
    };
    let report_bytes = serde_json::to_vec_pretty(&report)?;
    let report_sha256 = hex::encode(Sha256::digest(&report_bytes));
    let report_path = args
        .artifact_dir
        .join(format!("{report_sha256}.materialization.json"));
    publish_immutable(&report_path, &report_bytes)?;
    Ok(PublishedMaterialization {
        report,
        report_path,
        report_sha256,
    })
}

fn verify_segments(args: &Args) -> Result<(VerifiedBinanceMarketTape, BTreeMap<String, PathBuf>)> {
    let count = args.segment.len();
    if count == 0
        || args.segment_content_sha256.len() != count
        || args.segment_manifest_sha256.len() != count
    {
        bail!(
            "--segment, --segment-content-sha256, and --segment-manifest-sha256 must have equal nonzero lengths"
        );
    }
    let mut paths = BTreeMap::new();
    let mut sealed = Vec::with_capacity(count);
    for ((path, content_sha256), manifest_sha256) in args
        .segment
        .iter()
        .zip(&args.segment_content_sha256)
        .zip(&args.segment_manifest_sha256)
    {
        if paths
            .insert(content_sha256.clone(), path.clone())
            .is_some()
        {
            bail!("duplicate LOB segment supplied");
        }
        let triplet = BinanceMarketTapeTriplet {
            data: path.clone(),
            manifest: sibling(path, ".manifest.json")?,
            success: sibling(path, "._SUCCESS")?,
        };
        let trust = BinanceMarketTapeTrustAnchor::from_lower_hex(
            content_sha256,
            manifest_sha256,
        )?;
        sealed.push(seal_binance_market_tape_triplet(&triplet, &trust)?);
    }
    Ok((verify_binance_market_tape(sealed)?, paths))
}

fn source_segment_evidence(
    verified: &VerifiedBinanceMarketTape,
    mut paths: BTreeMap<String, PathBuf>,
) -> Result<Vec<SourceSegmentEvidence>> {
    verified
        .segments()
        .iter()
        .map(|segment| {
            let path = paths
                .remove(&segment.content_sha256)
                .context("verified segment has no matching CLI path")?;
            Ok(SourceSegmentEvidence {
                collector_manifest_path: sibling(&path, ".manifest.json")?,
                success_marker_path: sibling(&path, "._SUCCESS")?,
                success_marker_sha256: hex::encode(Sha256::digest(format!(
                    "{}\n",
                    segment.content_sha256
                ))),
                path,
                sha256: segment.content_sha256.clone(),
                collector_manifest_sha256: segment.manifest_sha256.clone(),
                start_received_at_ns: segment.start_received_at_ns,
                end_received_at_ns: segment.end_received_at_ns,
                events: segment.events,
            })
        })
        .collect()
}

fn parse_levels(levels: &[[String; 2]], side: &str) -> Result<BTreeMap<Decimal, Decimal>> {
    let mut parsed = BTreeMap::new();
    for (price, quantity) in validated_levels(levels, side)? {
        if !quantity.is_zero() {
            parsed.insert(price, quantity);
        }
    }
    Ok(parsed)
}

fn apply_levels(
    book: &mut BTreeMap<Decimal, Decimal>,
    levels: &[[String; 2]],
    side: &str,
) -> Result<()> {
    for (price, quantity) in validated_levels(levels, side)? {
        if quantity.is_zero() {
            book.remove(&price);
        } else {
            book.insert(price, quantity);
        }
    }
    Ok(())
}

fn validated_levels(levels: &[[String; 2]], side: &str) -> Result<Vec<(Decimal, Decimal)>> {
    levels
        .iter()
        .map(|[price, quantity]| {
            let price = price
                .parse::<Decimal>()
                .with_context(|| format!("invalid decimal in {side} levels"))?;
            let quantity = quantity
                .parse::<Decimal>()
                .with_context(|| format!("invalid decimal in {side} levels"))?;
            if price <= Decimal::ZERO || quantity < Decimal::ZERO {
                bail!("invalid {side} price or quantity");
            }
            Ok((price, quantity))
        })
        .collect()
}

fn sample_book(
    state: &BookState,
    series_id: u64,
    time_ns: u64,
    depth: usize,
) -> Result<BookSample> {
    let bid_levels = state.bids.iter().rev().take(depth).collect::<Vec<_>>();
    let ask_levels = state.asks.iter().take(depth).collect::<Vec<_>>();
    let (best_bid, best_bid_quantity) = bid_levels.first().context("order book has no bids")?;
    let (best_ask, best_ask_quantity) = ask_levels.first().context("order book has no asks")?;
    if best_bid >= best_ask {
        bail!("replayed order book is crossed");
    }
    let mid = (**best_bid + **best_ask) / Decimal::TWO;
    let bid_depth = bid_levels
        .iter()
        .fold(Decimal::ZERO, |total, (_, quantity)| total + **quantity);
    let ask_depth = ask_levels
        .iter()
        .fold(Decimal::ZERO, |total, (_, quantity)| total + **quantity);
    let total_depth = bid_depth + ask_depth;
    if mid <= Decimal::ZERO || total_depth <= Decimal::ZERO {
        bail!("replayed order book has invalid depth");
    }
    Ok(BookSample {
        series_id,
        time_ns,
        mid_price: decimal_f64(mid)?,
        spread_bps: decimal_f64((**best_ask - **best_bid) / mid * Decimal::from(10_000))?,
        bid_depth: decimal_f64(bid_depth)?,
        ask_depth: decimal_f64(ask_depth)?,
        top_depth_imbalance: decimal_f64((bid_depth - ask_depth) / total_depth)?,
        book_imbalance: {
            let bid_size = decimal_f64(**best_bid_quantity)?;
            let ask_size = decimal_f64(**best_ask_quantity)?;
            (bid_size - ask_size) / (bid_size + ask_size)
        },
    })
}

fn materialize_rows(
    samples: &[BookSample],
    market: Market,
    symbol: &str,
    revision: &str,
    horizon: usize,
    depth: usize,
    ingestion_time: DateTime<Utc>,
) -> Result<Vec<PointInTimeFeatureRow>> {
    let mut rows = Vec::new();
    let source_revisions = BTreeMap::from([(
        format!("binance-{}-lob", market.as_str()),
        revision.to_string(),
    )]);
    for index in 1..samples.len().saturating_sub(horizon) {
        let previous = &samples[index - 1];
        let current = &samples[index];
        let future = &samples[index + horizon];
        if previous.series_id != current.series_id || current.series_id != future.series_id {
            continue;
        }
        let previous_total = previous.bid_depth + previous.ask_depth;
        if previous.mid_price <= 0.0 || current.mid_price <= 0.0 || previous_total <= 0.0 {
            continue;
        }
        let label = future.mid_price / current.mid_price - 1.0;
        let features = BTreeMap::from([
            (format!("ask_depth_top{depth}"), current.ask_depth),
            (format!("bid_depth_top{depth}"), current.bid_depth),
            ("book_imbalance".to_string(), current.book_imbalance),
            (
                format!("book_imbalance_top{depth}"),
                current.top_depth_imbalance,
            ),
            ("mid_price".to_string(), current.mid_price),
            (
                "mid_return_1".to_string(),
                current.mid_price / previous.mid_price - 1.0,
            ),
            (
                format!("ofi_top{depth}"),
                ((current.bid_depth - previous.bid_depth)
                    - (current.ask_depth - previous.ask_depth))
                    / previous_total,
            ),
            ("spread_bps".to_string(), current.spread_bps),
        ]);
        if !label.is_finite() || features.values().any(|value| !value.is_finite()) {
            bail!("materialized feature or label is not finite");
        }
        rows.push(PointInTimeFeatureRow {
            event_time: datetime_ns(current.time_ns)?,
            feature_available_time: datetime_ns(current.time_ns)?,
            label_available_time: datetime_ns(future.time_ns)?,
            ingestion_time,
            symbol: symbol.to_string(),
            source_revisions: source_revisions.clone(),
            modalities: BTreeSet::from([DataModality::Lob]),
            features,
            label,
        });
    }
    if rows.len() < 3 {
        bail!("materialization produced fewer than three PIT rows");
    }
    Ok(rows)
}

fn encode_rows(rows: &[PointInTimeFeatureRow]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    Ok(bytes)
}

fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        if sha256_file(path)? != hex::encode(Sha256::digest(bytes)) {
            bail!(
                "immutable artifact already exists with different content: {}",
                path.display()
            );
        }
        return Ok(());
    }
    let file_name = file_name(path)?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if path.exists() => {
            let _ = std::fs::remove_file(&temporary);
            if sha256_file(path)? == hex::encode(Sha256::digest(bytes)) {
                Ok(())
            } else {
                Err(error.into())
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn source_revision(segments: &[SourceSegmentEvidence]) -> String {
    governed_source_revision(
        segments
            .iter()
            .map(|segment| segment.sha256.as_str()),
    )
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

fn decimal_f64(value: Decimal) -> Result<f64> {
    value
        .to_f64()
        .context("decimal cannot be represented as f64")
}

fn ceil_bucket(value: u64, bucket: u64) -> Result<u64> {
    value
        .checked_add(bucket - 1)
        .map(|adjusted| adjusted / bucket * bucket)
        .context("bucket time overflow")
}

fn datetime_ns(value: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(value / 1_000_000_000).context("timestamp seconds overflow")?;
    let nanoseconds = u32::try_from(value % 1_000_000_000).expect("nanoseconds fit u32");
    DateTime::from_timestamp(seconds, nanoseconds).context("timestamp is out of range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::{
        io::{BufRead, BufReader},
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    const START_NS: u64 = 1_783_987_200_000_000_000;
    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn event_ns(milliseconds: u64) -> u64 {
        START_NS + milliseconds * 1_000_000
    }

    fn level(price: &str, quantity: &str) -> [String; 2] {
        [price.to_string(), quantity.to_string()]
    }

    #[rustfmt::skip]
    fn diff(
        milliseconds: u64,
        first: u64,
        final_id: u64,
        previous: u64,
        bids: Value,
        asks: Value,
    ) -> Value {
        let received_at_ns = event_ns(milliseconds);
        json!({
            "schema": "binance.market_tape.v1",
            "received_at_ns": received_at_ns,
            "type": "diff",
            "session_id": "session-1",
            "frame": {"stream": "btcusdt@depth@100ms", "data": {
                "e": "depthUpdate",
                "E": received_at_ns / 1_000_000,
                "T": received_at_ns / 1_000_000,
                "s": "BTCUSDT",
                "U": first,
                "u": final_id,
                "pu": previous,
                "b": bids,
                "a": asks
            }}
        })
    }

    #[rustfmt::skip]
    fn trade(milliseconds: u64) -> Value {
        let received_at_ns = event_ns(milliseconds);
        json!({
            "schema": "binance.market_tape.v1",
            "received_at_ns": received_at_ns,
            "type": "agg_trade",
            "session_id": "session-1",
            "frame": {"stream": "btcusdt@aggTrade", "data": {
                "e": "aggTrade",
                "E": received_at_ns / 1_000_000,
                "s": "BTCUSDT",
                "a": 10,
                "p": "100.5",
                "q": "2",
                "f": 10,
                "l": 10,
                "T": received_at_ns / 1_000_000,
                "m": false
            }}
        })
    }

    #[rustfmt::skip]
    fn valid_rows() -> Vec<Value> {
        vec![
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(0),"type":"session_start","session_id":"session-1","market":"usdm","symbols":1,"websocket_shards":1}),
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(100),"type":"snapshot","session_id":"session-1","symbol":"BTCUSDT","request_started_at_ns":event_ns(50),"snapshot":{"lastUpdateId":100,"bids":[["100","10"],["99","5"]],"asks":[["102","4"],["103","6"]]}}),
            diff(600, 101, 175, 100, json!([["100", "10"]]), json!([["101", "8"]])),
            trade(700),
            diff(1_400, 176, 176, 175, json!([]), json!([["101", "0"]])),
            diff(2_400, 177, 177, 176, json!([["101", "3"]]), json!([])),
            diff(3_400, 178, 178, 177, json!([]), json!([["101.5", "4"]])),
            diff(4_400, 179, 179, 178, json!([["101", "0"]]), json!([])),
            diff(5_400, 180, 180, 179, json!([["100", "12"]]), json!([])),
            diff(6_400, 181, 181, 180, json!([]), json!([["101.5", "5"]])),
            json!({"schema":"binance.market_tape.v1","received_at_ns":event_ns(6_500),"type":"checkpoint","session_id":"session-1","symbol":"BTCUSDT","last_update_id":181,"synced":true,"bridged":true,"bids":[["100","12"],["99","5"]],"asks":[["101.5","5"],["102","4"],["103","6"]],"reason":"test","replay_safe":true}),
        ]
    }

    struct Fixture {
        directory: PathBuf,
        data: PathBuf,
        manifest: PathBuf,
        success: PathBuf,
        content_sha256: String,
        manifest_sha256: String,
    }

    impl Fixture {
        fn new(rows: &[Value]) -> Self {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let requested_directory = std::env::temp_dir()
                .join(format!("lob-pit-materializer-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&requested_directory).unwrap();
            let directory = std::fs::canonicalize(requested_directory).unwrap();
            let raw = directory.join("part-1.jsonl");
            let data = directory.join("part-1.jsonl.zst");
            let mut raw_file = File::create(&raw).unwrap();
            for row in rows {
                serde_json::to_writer(&mut raw_file, row).unwrap();
                raw_file.write_all(b"\n").unwrap();
            }
            assert!(Command::new("zstd")
                .args(["-q", "-f"])
                .arg(&raw)
                .arg("-o")
                .arg(&data)
                .status()
                .unwrap()
                .success());
            std::fs::remove_file(raw).unwrap();

            let content_sha256 = sha256_file(&data).unwrap();
            let event_types = rows.iter().fold(BTreeMap::new(), |mut counts, row| {
                *counts
                    .entry(row["type"].as_str().unwrap().to_string())
                    .or_insert(0_u64) += 1;
                counts
            });
            let manifest_value = json!({
                "schema": "binance.market_tape.v1",
                "venue": "binance",
                "market": "usdm",
                "dataset": "usdm_all",
                "shard_id": "all",
                "mode": "diff",
                "symbols": ["BTCUSDT"],
                "security_token_symbols": [],
                "excluded_symbols": [],
                "snapshot_limit": 1_000,
                "replay_scope": "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs",
                "venue_depth_complete": false,
                "events": rows.len(),
                "event_types": event_types,
                "has_replay_safe_checkpoint": true,
                "snapshot_ready_count": 1,
                "bridged_count": 1,
                "snapshot_only_symbols": [],
                "all_symbols_bridged": true,
                "start_received_at_ns": rows.first().unwrap()["received_at_ns"],
                "end_received_at_ns": rows.last().unwrap()["received_at_ns"],
                "date": "2026-07-14",
                "hour": "00",
                "file": "part-1.jsonl.zst",
                "bytes": data.metadata().unwrap().len(),
                "sha256": content_sha256,
                "trade_representation": "aggregate_trade_only",
                "price_surface_derivation": "latest aggregate trade price"
            });
            let mut manifest_bytes = serde_json::to_vec(&manifest_value).unwrap();
            manifest_bytes.push(b'\n');
            let manifest = sibling(&data, ".manifest.json").unwrap();
            std::fs::write(&manifest, &manifest_bytes).unwrap();
            let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
            let success = sibling(&data, "._SUCCESS").unwrap();
            std::fs::write(&success, format!("{content_sha256}\n")).unwrap();

            Self {
                directory,
                data,
                manifest,
                success,
                content_sha256,
                manifest_sha256,
            }
        }

        fn args(&self) -> Args {
            Args {
                mission_id: "data-btc-usdm-1".to_string(),
                symbol: "BTCUSDT".to_string(),
                market: Market::Usdm,
                bucket_ms: 1_000,
                label_horizon_buckets: 2,
                top_depth: 5,
                segment: vec![self.data.clone()],
                segment_content_sha256: vec![self.content_sha256.clone()],
                segment_manifest_sha256: vec![self.manifest_sha256.clone()],
                artifact_dir: self.directory.join("artifacts"),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn rejects_mismatched_segment_argument_lengths_before_file_access() {
        let args = Args {
            mission_id: "data-btc-usdm-1".to_string(),
            symbol: "BTCUSDT".to_string(),
            market: Market::Usdm,
            bucket_ms: 1_000,
            label_horizon_buckets: 2,
            top_depth: 5,
            segment: vec![PathBuf::from("/does/not/exist.jsonl.zst")],
            segment_content_sha256: Vec::new(),
            segment_manifest_sha256: vec!["0".repeat(64)],
            artifact_dir: PathBuf::from("/does/not/matter"),
        };

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("equal nonzero lengths"));
    }

    #[test]
    fn rejects_bad_external_content_digest() {
        let fixture = Fixture::new(&valid_rows());
        let mut args = fixture.args();
        args.segment_content_sha256[0] = "0".repeat(64);

        let error = materialize(&args).unwrap_err().to_string();

        assert!(error.contains("trusted digest anchor"));
        assert!(!args.artifact_dir.exists());
    }

    #[test]
    fn same_timestamp_snapshot_and_diff_apply_before_one_sample() {
        let received_at_ns = event_ns(1_000);
        let events = vec![
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                received_at_ns,
                bids: vec![level("100", "10")],
                asks: vec![level("102", "4")],
            }),
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff {
                received_at_ns,
                bids: vec![],
                asks: vec![level("102", "0"), level("101", "4")],
            }),
            ReplayedBinanceBookEvent::Checkpoint { received_at_ns },
        ];
        let mut replay = Replay::new(1_000_000_000, 5);

        replay.consume(&events).unwrap();

        assert_eq!(replay.samples.len(), 1);
        assert_eq!(replay.samples[0].time_ns, received_at_ns);
        assert!((replay.samples[0].mid_price - 100.5).abs() < 1e-12);
        assert!((replay.samples[0].ask_depth - 4.0).abs() < 1e-12);
    }

    #[test]
    fn preserves_report_evidence_and_point_in_time_rows() {
        let fixture = Fixture::new(&valid_rows());
        let published = materialize(&fixture.args()).unwrap();
        let output = serde_json::to_value(&published).unwrap();
        let source = &output["report"]["source_segments"][0];
        let rows = BufReader::new(File::open(&published.report.artifact_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(published.report.schema_version, MATERIALIZATION_SCHEMA);
        assert_eq!(published.report.rows, 3);
        assert_eq!(
            source,
            &json!({
                "path": fixture.data,
                "sha256": fixture.content_sha256,
                "collector_manifest_path": fixture.manifest,
                "collector_manifest_sha256": fixture.manifest_sha256,
                "success_marker_path": fixture.success,
                "success_marker_sha256": hex::encode(Sha256::digest(format!("{}\n", fixture.content_sha256))),
                "start_received_at_ns": event_ns(0),
                "end_received_at_ns": event_ns(6_500),
                "events": valid_rows().len()
            })
        );
        assert_eq!(rows[0].symbol, "BTCUSDT");
        assert_eq!(rows[0].modalities, BTreeSet::from([DataModality::Lob]));
        assert_eq!(rows[0].event_time.to_rfc3339(), "2026-07-14T00:00:02+00:00");
        assert_eq!(
            rows[0].label_available_time.to_rfc3339(),
            "2026-07-14T00:00:04+00:00"
        );
        assert!((rows[0].features["mid_price"] - 101.0).abs() < 1e-12);
        assert!((rows[0].label - (101.25 / 101.0 - 1.0)).abs() < 1e-12);
    }
}
