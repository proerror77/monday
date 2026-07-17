use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, ValueEnum};
use data::binance_lob_replay::{
    source_revision as governed_source_revision, Market as LobMarket, ReplaySequenceValidator,
};
use hft_collector::{DataModality, PointInTimeFeatureRow};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const RAW_SCHEMA: &str = "binance.lob_tape.v2";
const MATERIALIZATION_SCHEMA: &str = "binance-lob-pit-v1";
const MAX_PENDING_DIFFS: usize = 100_000;

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
    #[arg(long)]
    artifact_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RawSegmentManifest {
    schema: String,
    venue: String,
    market: String,
    file: String,
    bytes: u64,
    sha256: String,
    events: u64,
    event_types: BTreeMap<String, u64>,
    has_replay_safe_checkpoint: bool,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    symbols: Vec<String>,
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

#[derive(Debug, Clone)]
struct VerifiedSegment {
    evidence: SourceSegmentEvidence,
}

impl VerifiedSegment {
    fn path(&self) -> &Path {
        &self.evidence.path
    }
}

#[derive(Debug, Clone, PartialEq)]
struct BookState {
    session_id: String,
    last_update_id: u64,
    bridged: bool,
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
    market: Market,
    symbol: String,
    bucket_ns: u64,
    depth: usize,
    state: Option<BookState>,
    pending_diffs: Vec<Value>,
    samples: Vec<BookSample>,
    next_bucket_ns: Option<u64>,
    series_id: u64,
    saw_seed: bool,
    sequence_validator: ReplaySequenceValidator,
}

impl Replay {
    fn new(market: Market, symbol: String, bucket_ns: u64, depth: usize) -> Result<Self> {
        let sequence_validator = ReplaySequenceValidator::new(
            match market {
                Market::Spot => LobMarket::Spot,
                Market::Usdm => LobMarket::Usdm,
            },
            &symbol,
        )?;
        Ok(Self {
            market,
            symbol,
            bucket_ns,
            depth,
            state: None,
            pending_diffs: Vec::new(),
            samples: Vec::new(),
            next_bucket_ns: None,
            series_id: 0,
            saw_seed: false,
            sequence_validator,
        })
    }

    fn start_series(&mut self, state: BookState, received_at_ns: u64) -> Result<()> {
        self.state = Some(state);
        self.pending_diffs.clear();
        self.series_id = self
            .series_id
            .checked_add(1)
            .context("series id overflow")?;
        self.saw_seed = true;
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

    fn ensure_series_can_be_replaced(&self) -> Result<()> {
        if self.state.as_ref().is_some_and(|state| !state.bridged) {
            bail!("snapshot-only replay series cannot be replaced before its first valid diff");
        }
        Ok(())
    }

    fn process_event(&mut self, event: Value) -> Result<()> {
        let received_at_ns = json_u64(&event, "received_at_ns")?;
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .context("event has no type")?;
        self.sequence_validator.observe(
            event_type,
            event.as_object().context("event is not an object")?,
            received_at_ns,
        )?;
        match event_type {
            "sequence_gap" => bail!("LOB tape contains a sequence gap event"),
            "snapshot" => self.process_snapshot(event, received_at_ns),
            "checkpoint" => self.process_checkpoint(event, received_at_ns),
            "diff" => self.process_diff(event, received_at_ns),
            _ => Ok(()),
        }
    }

    fn process_snapshot(&mut self, event: Value, received_at_ns: u64) -> Result<()> {
        let mut snapshot = install_snapshot(&event)?;
        for pending in &self.pending_diffs {
            if event_session_id(pending)? != snapshot.session_id {
                bail!("buffered diff session does not match its snapshot");
            }
            apply_diff(&mut snapshot, pending, &self.symbol, self.market)?;
        }
        if self.state.is_some() {
            self.ensure_series_can_be_replaced()?;
            self.emit_before(received_at_ns)?;
        }
        self.start_series(snapshot, received_at_ns)?;
        self.emit_at(received_at_ns)
    }

    fn process_checkpoint(&mut self, event: Value, received_at_ns: u64) -> Result<()> {
        let checkpoint = install_checkpoint(&event)?;
        match self.state.as_ref() {
            None if !self.pending_diffs.is_empty() => bail!(
                "diffs arrived before replay seed; include a segment with the opening snapshot"
            ),
            None => self.start_series(checkpoint, received_at_ns)?,
            Some(state) if state.session_id != checkpoint.session_id => {
                self.ensure_series_can_be_replaced()?;
                self.start_series(checkpoint, received_at_ns)?
            }
            Some(_) => {
                self.emit_before(received_at_ns)?;
                if self.state.as_ref() != Some(&checkpoint) {
                    bail!("checkpoint does not match replayed order book");
                }
            }
        }
        self.emit_at(received_at_ns)
    }

    fn process_diff(&mut self, event: Value, received_at_ns: u64) -> Result<()> {
        let session_id = event_session_id(&event)?.to_string();
        let Some(current_session) = self.state.as_ref().map(|state| state.session_id.clone())
        else {
            self.pending_diffs.push(event);
            if self.pending_diffs.len() > MAX_PENDING_DIFFS {
                bail!("too many diffs buffered before replay seed");
            }
            return Ok(());
        };
        if session_id != current_session {
            self.ensure_series_can_be_replaced()?;
            self.state = None;
            self.next_bucket_ns = None;
            self.pending_diffs = vec![event];
            return Ok(());
        }
        self.emit_before(received_at_ns)?;
        apply_diff(
            self.state.as_mut().context("diff has no replay state")?,
            &event,
            &self.symbol,
            self.market,
        )?;
        self.emit_at(received_at_ns)
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
    let mut segments = args
        .segment
        .iter()
        .map(|path| verify_segment(path, args.market, &symbol))
        .collect::<Result<Vec<_>>>()?;
    segments.sort_by_key(|segment| segment.evidence.start_received_at_ns);
    if segments.is_empty() {
        bail!("at least one LOB segment is required");
    }
    let mut unique_hashes = BTreeSet::new();
    if segments
        .iter()
        .any(|segment| !unique_hashes.insert(segment.evidence.sha256.clone()))
    {
        bail!("duplicate LOB segment supplied");
    }

    let bucket_ns = args
        .bucket_ms
        .checked_mul(1_000_000)
        .context("bucket size overflow")?;
    let mut replay = Replay::new(args.market, symbol.clone(), bucket_ns, args.top_depth)?;
    for segment in &segments {
        replay_segment(segment.path(), &symbol, &mut replay)?;
    }
    if !replay.saw_seed {
        bail!("no replay seed found for {symbol}");
    }
    if replay.state.as_ref().is_some_and(|state| !state.bridged) {
        bail!("snapshot-only replay series never received a valid first diff");
    }
    replay.sequence_validator.finish()?;

    let revision = source_revision(&segments);
    let created_at = Utc::now();
    let ingestion_time = datetime_ns(
        segments
            .iter()
            .map(|segment| segment.evidence.end_received_at_ns)
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
        source_segments: segments
            .into_iter()
            .map(|segment| segment.evidence)
            .collect(),
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

fn verify_segment(path: &Path, market: Market, symbol: &str) -> Result<VerifiedSegment> {
    let manifest_path = sibling(path, ".manifest.json")?;
    let success_path = sibling(path, "._SUCCESS")?;
    if !path.is_file() || !manifest_path.is_file() || !success_path.is_file() {
        bail!(
            "segment requires data, manifest, and _SUCCESS files: {}",
            path.display()
        );
    }
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    let manifest: RawSegmentManifest = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("invalid segment manifest {}", manifest_path.display()))?;
    if manifest.schema != RAW_SCHEMA
        || manifest.venue != "binance"
        || manifest.market != market.as_str()
        || manifest.file != file_name(path)?
        || manifest.bytes != path.metadata()?.len()
    {
        bail!("segment manifest identity mismatch: {}", path.display());
    }
    if !manifest
        .symbols
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(symbol))
    {
        bail!(
            "segment does not declare symbol {symbol}: {}",
            path.display()
        );
    }
    if manifest
        .event_types
        .get("sequence_gap")
        .copied()
        .unwrap_or(0)
        > 0
    {
        bail!(
            "segment manifest contains a sequence gap: {}",
            path.display()
        );
    }
    if !manifest.has_replay_safe_checkpoint {
        bail!("segment is not marked replay safe: {}", path.display());
    }
    let success_bytes = std::fs::read(&success_path)
        .with_context(|| format!("failed to read {}", success_path.display()))?;
    let success_marker_sha256 = hex::encode(Sha256::digest(&success_bytes));
    let success_hash = std::str::from_utf8(&success_bytes)
        .with_context(|| format!("invalid _SUCCESS marker {}", success_path.display()))?
        .trim();
    if success_hash != manifest.sha256 {
        bail!(
            "_SUCCESS marker does not match manifest: {}",
            path.display()
        );
    }
    let actual_hash = sha256_file(path)?;
    if actual_hash != manifest.sha256 {
        bail!("segment SHA256 does not match manifest: {}", path.display());
    }
    if manifest.events == 0 || manifest.end_received_at_ns < manifest.start_received_at_ns {
        bail!(
            "segment time bounds or event count are invalid: {}",
            path.display()
        );
    }
    Ok(VerifiedSegment {
        evidence: SourceSegmentEvidence {
            path: path.to_path_buf(),
            sha256: actual_hash,
            collector_manifest_path: manifest_path,
            collector_manifest_sha256: manifest_sha256,
            success_marker_path: success_path,
            success_marker_sha256,
            start_received_at_ns: manifest.start_received_at_ns,
            end_received_at_ns: manifest.end_received_at_ns,
            events: manifest.events,
        },
    })
}

fn replay_segment(segment: &Path, symbol: &str, replay: &mut Replay) -> Result<()> {
    let mut child = Command::new("zstd")
        .args(["-q", "-dc"])
        .arg(segment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to start zstd for {}", segment.display()))?;
    let stdout = child.stdout.take().context("zstd stdout is unavailable")?;
    let result: Result<()> = (|| {
        for (index, line) in BufReader::new(stdout).lines().enumerate() {
            let line = line.with_context(|| {
                format!("failed to read {} line {}", segment.display(), index + 1)
            })?;
            if !line.contains(symbol) {
                continue;
            }
            let event: Value = serde_json::from_str(&line).with_context(|| {
                format!("invalid JSON in {} line {}", segment.display(), index + 1)
            })?;
            if event_symbol(&event).as_deref() == Some(symbol) {
                replay.process_event(event)?;
            }
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output()?;
    result?;
    if !output.status.success() {
        bail!(
            "zstd failed for {}: {}",
            segment.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn install_snapshot(event: &Value) -> Result<BookState> {
    let snapshot = event
        .get("snapshot")
        .and_then(Value::as_object)
        .context("snapshot event has no snapshot payload")?;
    Ok(BookState {
        session_id: event_session_id(event)?.to_string(),
        last_update_id: object_u64(snapshot, "lastUpdateId")?,
        bridged: false,
        bids: parse_levels(snapshot.get("bids"), "bid")?,
        asks: parse_levels(snapshot.get("asks"), "ask")?,
    })
}

fn install_checkpoint(event: &Value) -> Result<BookState> {
    if event.get("replay_safe").and_then(Value::as_bool) != Some(true)
        || event.get("synced").and_then(Value::as_bool) != Some(true)
    {
        bail!("checkpoint is not replay safe");
    }
    Ok(BookState {
        session_id: event_session_id(event)?.to_string(),
        last_update_id: json_u64(event, "last_update_id")?,
        bridged: event
            .get("bridged")
            .and_then(Value::as_bool)
            .context("checkpoint has no bridged state")?,
        bids: parse_levels(event.get("bids"), "bid")?,
        asks: parse_levels(event.get("asks"), "ask")?,
    })
}

fn apply_diff(state: &mut BookState, event: &Value, symbol: &str, market: Market) -> Result<()> {
    let frame = event
        .get("frame")
        .and_then(Value::as_object)
        .context("diff event has no frame")?;
    let data = frame
        .get("data")
        .and_then(Value::as_object)
        .context("diff event has no data")?;
    if object_str(data, "s")?.to_uppercase() != symbol {
        bail!("diff event identity is invalid");
    }
    if event_session_id(event)? != state.session_id {
        bail!("diff session does not match replay state");
    }
    let first_update_id = object_u64(data, "U")?;
    let final_update_id = object_u64(data, "u")?;
    if final_update_id <= state.last_update_id {
        return Ok(());
    }
    if first_update_id > final_update_id {
        bail!("diff update range is reversed");
    }
    let previous_update_id = data.get("pu").and_then(Value::as_u64);
    let mut expected = state.last_update_id + u64::from(market == Market::Spot);
    let accepted = if !state.bridged {
        let previous_matches = market == Market::Usdm
            && previous_update_id.is_some_and(|previous| previous == state.last_update_id);
        previous_matches || first_update_id <= expected && expected <= final_update_id
    } else if market == Market::Usdm {
        expected = state.last_update_id;
        previous_update_id.is_some_and(|previous| previous == state.last_update_id)
    } else {
        first_update_id <= expected && expected <= final_update_id
    };
    if !accepted {
        bail!(
            "Binance sequence gap: expected {expected}, received {first_update_id}-{final_update_id}"
        );
    }
    apply_levels(&mut state.bids, data.get("b"), "bid")?;
    apply_levels(&mut state.asks, data.get("a"), "ask")?;
    state.last_update_id = final_update_id;
    state.bridged = true;
    Ok(())
}

fn parse_levels(value: Option<&Value>, side: &str) -> Result<BTreeMap<Decimal, Decimal>> {
    let mut parsed = BTreeMap::new();
    for (price, quantity) in validated_levels(value, side)? {
        if !quantity.is_zero() {
            parsed.insert(price, quantity);
        }
    }
    Ok(parsed)
}

fn apply_levels(
    book: &mut BTreeMap<Decimal, Decimal>,
    value: Option<&Value>,
    side: &str,
) -> Result<()> {
    for (price, quantity) in validated_levels(value, side)? {
        if quantity.is_zero() {
            book.remove(&price);
        } else {
            book.insert(price, quantity);
        }
    }
    Ok(())
}

fn validated_levels(value: Option<&Value>, side: &str) -> Result<Vec<(Decimal, Decimal)>> {
    let levels = value
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("{side} levels are not an array"))?;
    levels
        .iter()
        .map(|level| {
            let pair = level
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| anyhow!("invalid {side} price level"))?;
            let price = decimal_value(&pair[0], side)?;
            let quantity = decimal_value(&pair[1], side)?;
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

fn source_revision(segments: &[VerifiedSegment]) -> String {
    governed_source_revision(
        segments
            .iter()
            .map(|segment| segment.evidence.sha256.as_str()),
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

fn json_u64(value: &Value, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{field} is not an unsigned integer"))
}

fn object_u64(value: &serde_json::Map<String, Value>, field: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("{field} is not an unsigned integer"))
}

fn object_str<'a>(value: &'a serde_json::Map<String, Value>, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{field} is not text"))
}

fn event_session_id(event: &Value) -> Result<&str> {
    event
        .get("session_id")
        .and_then(Value::as_str)
        .context("event session id is invalid")
}

fn event_symbol(event: &Value) -> Option<String> {
    match event.get("type").and_then(Value::as_str) {
        Some("diff") => event
            .get("frame")?
            .get("data")?
            .get("s")?
            .as_str()
            .map(str::to_uppercase),
        Some("snapshot" | "checkpoint" | "sequence_gap") => {
            event.get("symbol")?.as_str().map(str::to_uppercase)
        }
        _ => None,
    }
}

fn decimal_value(value: &Value, side: &str) -> Result<Decimal> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow!("{side} decimal is not text"))?;
    text.parse()
        .with_context(|| format!("invalid decimal in {side} levels"))
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
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn event_ns(second: f64) -> u64 {
        1_783_987_200_000_000_000 + (second * 1_000_000_000.0) as u64
    }

    fn diff(
        second: f64,
        first: u64,
        final_id: u64,
        previous: u64,
        bids: Value,
        asks: Value,
    ) -> Value {
        json!({
            "received_at_ns": event_ns(second),
            "type": "diff",
            "session_id": "session-1",
            "frame": {"stream": "btcusdt@depth@100ms", "data": {
                "e": "depthUpdate", "E": event_ns(second) / 1_000_000,
                "s": "BTCUSDT", "U": first, "u": final_id, "pu": previous,
                "b": bids, "a": asks
            }}
        })
    }

    fn snapshot() -> Value {
        json!({
            "received_at_ns": event_ns(0.1),
            "type": "snapshot",
            "session_id": "session-1",
            "symbol": "BTCUSDT",
            "snapshot": {"lastUpdateId": 100, "bids": [["100", "10"], ["99", "5"]], "asks": [["102", "4"], ["103", "6"]]}
        })
    }

    fn checkpoint() -> Value {
        json!({
            "received_at_ns": event_ns(6.5),
            "type": "checkpoint",
            "reason": "scheduled",
            "replay_safe": true,
            "session_id": "session-1",
            "symbol": "BTCUSDT",
            "last_update_id": 181,
            "synced": true,
            "bridged": true,
            "bids": [["100", "12"], ["99", "5"]],
            "asks": [["101.5", "5"], ["102", "4"], ["103", "6"]]
        })
    }

    fn snapshot_only_checkpoint() -> Value {
        json!({
            "received_at_ns": event_ns(0.2),
            "type": "checkpoint",
            "reason": "scheduled",
            "replay_safe": true,
            "session_id": "session-1",
            "symbol": "BTCUSDT",
            "last_update_id": 100,
            "synced": true,
            "bridged": false,
            "bids": [["100", "10"], ["99", "5"]],
            "asks": [["102", "4"], ["103", "6"]]
        })
    }

    fn valid_events() -> Vec<Value> {
        vec![
            diff(0.05, 99, 101, 98, json!([]), json!([["101", "8"]])),
            snapshot(),
            diff(0.6, 150, 175, 101, json!([["100", "10"]]), json!([])),
            diff(1.4, 176, 176, 175, json!([]), json!([["101", "0"]])),
            diff(2.4, 177, 177, 176, json!([["101", "3"]]), json!([])),
            diff(3.4, 178, 178, 177, json!([]), json!([["101.5", "4"]])),
            diff(4.4, 179, 179, 178, json!([["101", "0"]]), json!([])),
            diff(5.4, 180, 180, 179, json!([["100", "12"]]), json!([])),
            diff(6.4, 181, 181, 180, json!([]), json!([["101.5", "5"]])),
            checkpoint(),
        ]
    }

    struct Fixture {
        directory: PathBuf,
        data: PathBuf,
        success: PathBuf,
    }

    impl Fixture {
        fn new(events: &[Value]) -> Self {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir()
                .join(format!("lob-pit-materializer-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&directory).unwrap();
            let raw = directory.join("part-1.jsonl");
            let data = directory.join("part-1.jsonl.zst");
            let mut raw_file = File::create(&raw).unwrap();
            for event in events {
                serde_json::to_writer(&mut raw_file, event).unwrap();
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
            if raw.exists() {
                std::fs::remove_file(raw).unwrap();
            }
            let hash = sha256_file(&data).unwrap();
            let event_types = events.iter().fold(BTreeMap::new(), |mut counts, event| {
                *counts
                    .entry(event["type"].as_str().unwrap().to_string())
                    .or_insert(0_u64) += 1;
                counts
            });
            let manifest = json!({
                "schema": RAW_SCHEMA,
                "venue": "binance",
                "market": "usdm",
                "file": "part-1.jsonl.zst",
                "bytes": data.metadata().unwrap().len(),
                "sha256": hash,
                "events": events.len(),
                "event_types": event_types,
                "has_replay_safe_checkpoint": true,
                "start_received_at_ns": events.first().unwrap()["received_at_ns"],
                "end_received_at_ns": events.last().unwrap()["received_at_ns"],
                "symbols": ["btcusdt"]
            });
            std::fs::write(
                sibling(&data, ".manifest.json").unwrap(),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            let success = sibling(&data, "._SUCCESS").unwrap();
            std::fs::write(&success, format!("{hash}\n")).unwrap();
            Self {
                directory,
                data,
                success,
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
    fn rejects_segment_without_matching_success_marker() {
        let fixture = Fixture::new(&valid_events());
        std::fs::write(&fixture.success, format!("{}\n", "0".repeat(64))).unwrap();

        let error = materialize(&fixture.args()).unwrap_err().to_string();

        assert!(error.contains("_SUCCESS marker"));
    }

    #[test]
    fn rejects_usdm_previous_update_gap_after_snapshot_bridge() {
        let mut events = valid_events();
        events[3]["frame"]["data"]["pu"] = json!(174);
        let fixture = Fixture::new(&events);

        let error = materialize(&fixture.args()).unwrap_err().to_string();

        assert!(error.contains("Binance sequence gap"));
    }

    #[test]
    fn rejects_closing_checkpoint_that_would_drop_unseeded_diffs() {
        let fixture = Fixture::new(&[
            diff(0.05, 99, 101, 98, json!([]), json!([["101", "8"]])),
            checkpoint(),
        ]);

        let error = materialize(&fixture.args()).unwrap_err().to_string();

        assert!(error.contains("diffs arrived before replay seed"));
    }

    #[test]
    fn delayed_spot_diff_bridges_snapshot_only_checkpoint() {
        let mut state = install_checkpoint(&snapshot_only_checkpoint()).unwrap();
        assert!(!state.bridged);

        apply_diff(
            &mut state,
            &diff(0.3, 101, 102, 100, json!([]), json!([["102", "3"]])),
            "BTCUSDT",
            Market::Spot,
        )
        .unwrap();

        assert!(state.bridged);
        assert_eq!(state.last_update_id, 102);
    }

    #[test]
    fn delayed_usdm_diff_bridges_snapshot_only_checkpoint() {
        let mut state = install_checkpoint(&snapshot_only_checkpoint()).unwrap();
        assert!(!state.bridged);

        apply_diff(
            &mut state,
            &diff(0.3, 101, 102, 100, json!([]), json!([["102", "3"]])),
            "BTCUSDT",
            Market::Usdm,
        )
        .unwrap();

        assert!(state.bridged);
        assert_eq!(state.last_update_id, 102);
    }

    #[test]
    fn invalid_delayed_spot_diff_rejects_snapshot_only_checkpoint() {
        let mut state = install_checkpoint(&snapshot_only_checkpoint()).unwrap();

        let error = apply_diff(
            &mut state,
            &diff(0.3, 102, 102, 100, json!([]), json!([["102", "3"]])),
            "BTCUSDT",
            Market::Spot,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Binance sequence gap"));
        assert!(!state.bridged);
    }

    #[test]
    fn invalid_delayed_usdm_diff_rejects_snapshot_only_checkpoint() {
        let mut state = install_checkpoint(&snapshot_only_checkpoint()).unwrap();

        let error = apply_diff(
            &mut state,
            &diff(0.3, 102, 102, 99, json!([]), json!([["102", "3"]])),
            "BTCUSDT",
            Market::Usdm,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("Binance sequence gap"));
        assert!(!state.bridged);
    }

    #[test]
    fn snapshot_only_checkpoints_never_publish_without_a_valid_first_diff() {
        let mut events = Vec::new();
        for second in [0.2, 3.2, 6.2] {
            let mut checkpoint = snapshot_only_checkpoint();
            checkpoint["received_at_ns"] = json!(event_ns(second));
            events.push(checkpoint);
        }
        let fixture = Fixture::new(&events);

        let error = materialize(&fixture.args()).unwrap_err().to_string();

        assert!(error.contains("never received a valid first diff"));
    }

    #[test]
    fn publishes_live_imbalance_and_binds_raw_sidecar_bytes() {
        let fixture = Fixture::new(&valid_events());
        let published = materialize(&fixture.args()).unwrap();
        let first_row = BufReader::new(File::open(&published.report.artifact_path).unwrap())
            .lines()
            .next()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .unwrap();
        let manifest_path = sibling(&fixture.data, ".manifest.json").unwrap();
        let output = serde_json::to_value(&published).unwrap();
        let source = &output["report"]["source_segments"][0];

        assert_eq!(
            json!({
                "book_imbalance": first_row.features.get("book_imbalance").copied(),
                "book_imbalance_top5": first_row.features.get("book_imbalance_top5").copied(),
                "collector_manifest_path": source["collector_manifest_path"],
                "collector_manifest_sha256": source["collector_manifest_sha256"],
                "success_marker_path": source["success_marker_path"],
                "success_marker_sha256": source["success_marker_sha256"],
            }),
            json!({
                // Fixture row 0 has best-bid quantity 10 and best-ask quantity 4.
                "book_imbalance": 0.428_571_428_571_428_55,
                "book_imbalance_top5": 0.2,
                "collector_manifest_path": manifest_path,
                "collector_manifest_sha256": hex::encode(Sha256::digest(std::fs::read(&manifest_path).unwrap())),
                "success_marker_path": fixture.success,
                "success_marker_sha256": hex::encode(Sha256::digest(std::fs::read(&fixture.success).unwrap())),
            })
        );
    }

    #[test]
    fn publishes_content_addressed_pit_rows_with_delayed_labels() {
        let fixture = Fixture::new(&valid_events());
        let first = materialize(&fixture.args()).unwrap();
        let second = materialize(&fixture.args()).unwrap();

        assert_eq!(first.report.rows, 3);
        assert_eq!(first.report.artifact_path, second.report.artifact_path);
        assert!(first
            .report
            .artifact_path
            .ends_with(format!("{}.jsonl", first.report.artifact_sha256)));
        let rows = BufReader::new(File::open(&first.report.artifact_path).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
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
