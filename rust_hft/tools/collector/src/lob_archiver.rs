use anyhow::Context;
pub use data::binance_lob_replay::{
    source_revision, Market, ReplaySequenceEvent, ReplaySequenceValidator,
};
use data::binance_market_tape::{
    event_type_allowed, market_tape_schema, supported_schema, AggregateTrade,
    AggregateTradeSequenceValidator, AggregateTradeSummary, AggregateTradeSummaryBuilder,
    LobContinuitySummary, LobContinuitySummaryBuilder, AGGREGATE_TRADE_SUMMARY_CONTRACT,
    LEGACY_LOB_TAPE_SCHEMA, MARKET_TAPE_SCHEMA_V2,
};
use engine::binance_md::{parse_fixed_6, BookSync, SequenceDecision, UpdateMeta};
use rand::random;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

pub use data::binance_market_tape::MARKET_TAPE_SCHEMA_V2 as RAW_SCHEMA;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthDiff {
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "U")]
    pub first_update_id: u64,
    #[serde(rename = "u")]
    pub final_update_id: u64,
    #[serde(rename = "pu", default)]
    pub previous_update_id: Option<u64>,
    #[serde(rename = "b", default)]
    pub bids: Vec<[String; 2]>,
    #[serde(rename = "a", default)]
    pub asks: Vec<[String; 2]>,
}

impl DepthDiff {
    pub fn from_frame(frame: &Value) -> anyhow::Result<Self> {
        let data = frame.get("data").unwrap_or(frame);
        let mut diff: Self = serde_json::from_value(data.clone())?;
        if diff.symbol.is_empty() {
            anyhow::bail!("depth frame missing symbol");
        }
        if diff.first_update_id > diff.final_update_id {
            anyhow::bail!(
                "invalid depth sequence {}-{}",
                diff.first_update_id,
                diff.final_update_id
            );
        }
        diff.symbol.make_ascii_uppercase();
        validate_levels(&diff.bids)?;
        validate_levels(&diff.asks)?;
        Ok(diff)
    }

    fn meta(&self) -> UpdateMeta {
        UpdateMeta::new(self.first_update_id, self.final_update_id)
    }
}

fn validate_levels(levels: &[[String; 2]]) -> anyhow::Result<()> {
    for [price, quantity] in levels {
        // Reuse market-core's non-floating parser on its supported fast-path.
        // Binance also emits 8-decimal values, retained losslessly as strings.
        if decimal_places(price) <= 6 {
            parse_fixed_6(price)?;
        } else {
            Decimal::from_str(price)?;
        }
        if decimal_places(quantity) <= 6 {
            parse_fixed_6(quantity)?;
        } else {
            Decimal::from_str(quantity)?;
        }
    }
    Ok(())
}

fn decimal_places(value: &str) -> usize {
    value
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceGap {
    pub symbol: String,
    pub expected: u64,
    pub first_seen: u64,
    pub final_update_id: u64,
}

impl std::fmt::Display for SequenceGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} sequence gap expected={} received={}-{}",
            self.symbol, self.expected, self.first_seen, self.final_update_id
        )
    }
}

impl std::error::Error for SequenceGap {}

#[derive(Debug)]
pub struct PendingBudget {
    limit: usize,
    count: usize,
}

impl PendingBudget {
    pub fn new(limit: usize) -> Self {
        Self { limit, count: 0 }
    }

    fn reserve(&mut self) -> anyhow::Result<()> {
        if self.count >= self.limit {
            anyhow::bail!("pending diff budget exceeded limit={}", self.limit);
        }
        self.count += 1;
        Ok(())
    }

    fn release(&mut self, count: usize) {
        self.count = self.count.saturating_sub(count);
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

#[derive(Debug)]
pub struct OrderBookState {
    pub symbol: String,
    pub market: Market,
    /// Retained bid/ask levels as (price, quantity), kept sorted ascending by
    /// price with unique prices. The compact Decimal pairs (16 bytes each) and
    /// single contiguous Vec allocation replace a HashMap<String, String> that
    /// spent 100+ bytes and a separate heap allocation per level; full-depth
    /// USD-M books (thousands of levels per side) drove the old representation
    /// to ~4.7 GiB and throttled the archiver under memory.high. All levels are
    /// retained; only the in-memory encoding is denser.
    bids: Vec<(Decimal, Decimal)>,
    asks: Vec<(Decimal, Decimal)>,
    sync: BookSync,
    snapshot_installed: bool,
    pub synced: bool,
    pub bridged: bool,
    stream_coverage_verified: bool,
    pending: Vec<DepthDiff>,
}

impl OrderBookState {
    pub fn new(symbol: impl Into<String>, market: Market) -> Self {
        let mut sync = BookSync::new();
        sync.start_buffering();
        Self {
            symbol: symbol.into().to_ascii_uppercase(),
            market,
            bids: Vec::new(),
            asks: Vec::new(),
            sync,
            snapshot_installed: false,
            synced: false,
            bridged: false,
            stream_coverage_verified: false,
            pending: Vec::new(),
        }
    }

    pub fn verify_stream_coverage(&mut self) {
        self.stream_coverage_verified = true;
    }

    pub fn continuity_complete(&self) -> bool {
        self.synced && (self.bridged || (self.snapshot_installed && self.stream_coverage_verified))
    }

    pub fn last_update_id(&self) -> Option<u64> {
        self.snapshot_installed.then(|| self.sync.last_update_id())
    }

    pub fn bid_levels(&self) -> usize {
        self.bids.len()
    }

    pub fn ask_levels(&self) -> usize {
        self.asks.len()
    }

    pub fn apply_diff(
        &mut self,
        diff: DepthDiff,
        budget: &mut PendingBudget,
    ) -> anyhow::Result<bool> {
        if diff.symbol != self.symbol {
            anyhow::bail!("unexpected symbol {} for {}", diff.symbol, self.symbol);
        }
        if !self.snapshot_installed {
            budget.reserve()?;
            self.pending.push(diff);
            return Ok(false);
        }
        self.apply_after_snapshot(&diff)?;
        Ok(true)
    }

    pub fn install_snapshot(
        &mut self,
        snapshot: &Value,
        budget: &mut PendingBudget,
    ) -> anyhow::Result<()> {
        let last_update_id = snapshot
            .get("lastUpdateId")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("snapshot missing lastUpdateId"))?;
        self.bids = parse_snapshot_side(snapshot.get("bids"))?;
        self.asks = parse_snapshot_side(snapshot.get("asks"))?;
        anyhow::ensure!(
            !self.bids.is_empty() && !self.asks.is_empty(),
            "snapshot must contain two non-empty book sides"
        );
        self.sync.load_snapshot_for_replay(last_update_id);
        self.snapshot_installed = true;
        self.synced = true;
        self.bridged = false;

        let pending = std::mem::take(&mut self.pending);
        budget.release(pending.len());
        for diff in pending {
            self.apply_after_snapshot(&diff)?;
        }
        Ok(())
    }

    fn apply_after_snapshot(&mut self, diff: &DepthDiff) -> Result<(), SequenceGap> {
        let previous = self.sync.last_update_id();
        if diff.final_update_id <= previous {
            return Ok(());
        }
        let decision = match self.market {
            Market::Spot => self.sync.classify_live_update(diff.meta()),
            Market::Usdm => {
                let pu = diff.previous_update_id.unwrap_or(u64::MAX);
                let bridges = if self.bridged {
                    pu == previous
                } else {
                    pu == previous
                        || (diff.first_update_id <= previous && previous <= diff.final_update_id)
                };
                if !bridges {
                    self.synced = false;
                    return Err(SequenceGap {
                        symbol: self.symbol.clone(),
                        expected: previous,
                        first_seen: pu,
                        final_update_id: diff.final_update_id,
                    });
                }
                // USD-M continuity is defined by pu. Normalize the already
                // validated update into BookSync's monotonic sequence model.
                self.sync.classify_live_update(UpdateMeta::new(
                    previous.saturating_add(1),
                    diff.final_update_id,
                ))
            }
        };

        match decision {
            SequenceDecision::Apply => {
                update_side(&mut self.bids, &diff.bids);
                update_side(&mut self.asks, &diff.asks);
                self.synced = true;
                self.bridged = true;
                Ok(())
            }
            SequenceDecision::IgnoreStale => Ok(()),
            SequenceDecision::Gap {
                expected,
                first_seen,
            } => {
                self.synced = false;
                Err(SequenceGap {
                    symbol: self.symbol.clone(),
                    expected,
                    first_seen,
                    final_update_id: diff.final_update_id,
                })
            }
            SequenceDecision::InvalidRange => {
                self.synced = false;
                Err(SequenceGap {
                    symbol: self.symbol.clone(),
                    expected: previous.saturating_add(1),
                    first_seen: diff.first_update_id,
                    final_update_id: diff.final_update_id,
                })
            }
        }
    }

    pub fn invalidate_for_resync(&mut self, budget: &mut PendingBudget) {
        budget.release(self.pending.len());
        self.pending.clear();
        self.sync.start_buffering();
        self.snapshot_installed = false;
        self.synced = false;
        self.bridged = false;
        self.stream_coverage_verified = false;
    }

    pub fn checkpoint(&self, session_id: &str) -> anyhow::Result<Checkpoint> {
        let last_update_id = self
            .last_update_id()
            .ok_or_else(|| anyhow::anyhow!("{} has no snapshot", self.symbol))?;
        Ok(Checkpoint {
            session_id: session_id.to_owned(),
            symbol: self.symbol.clone(),
            last_update_id,
            synced: self.synced,
            bridged: self.bridged,
            continuity_complete: self.continuity_complete(),
            stream_coverage_verified: self.stream_coverage_verified,
            bids: sorted_levels(&self.bids, true)?,
            asks: sorted_levels(&self.asks, false)?,
        })
    }
}

fn parse_snapshot_side(value: Option<&Value>) -> anyhow::Result<Vec<(Decimal, Decimal)>> {
    let levels: Vec<[String; 2]> =
        serde_json::from_value(value.cloned().context("snapshot side is missing")?)?;
    validate_levels(&levels)?;
    let mut side: Vec<(Decimal, Decimal)> = levels
        .into_iter()
        .filter_map(|[price, quantity]| {
            let price = Decimal::from_str(&price).ok()?;
            let quantity = Decimal::from_str(&quantity).ok()?;
            if quantity.is_zero() {
                return None;
            }
            Some((price, quantity))
        })
        .collect();
    side.sort_by_key(|(price, _)| *price);
    Ok(side)
}

fn update_side(side: &mut Vec<(Decimal, Decimal)>, levels: &[[String; 2]]) {
    for [price, quantity] in levels {
        let Ok(price) = Decimal::from_str(price) else {
            continue;
        };
        let Ok(quantity) = Decimal::from_str(quantity) else {
            continue;
        };
        if quantity.is_zero() {
            book_remove(side, price);
        } else {
            book_upsert(side, price, quantity);
        }
    }
}

/// Insert or replace the level at `price`, keeping the side sorted ascending
/// by price with unique prices.
fn book_upsert(side: &mut Vec<(Decimal, Decimal)>, price: Decimal, quantity: Decimal) {
    match side.binary_search_by(|(p, _)| p.cmp(&price)) {
        Ok(index) => side[index].1 = quantity,
        Err(index) => side.insert(index, (price, quantity)),
    }
}

fn book_remove(side: &mut Vec<(Decimal, Decimal)>, price: Decimal) {
    if let Ok(index) = side.binary_search_by(|(p, _)| p.cmp(&price)) {
        side.remove(index);
    }
}

/// The side is always stored sorted ascending by price; bids are emitted
/// highest-first (`descending == true`), asks lowest-first (`descending ==
/// false`). Decimal round-trips Binance's fixed-decimal strings losslessly.
fn sorted_levels(
    side: &[(Decimal, Decimal)],
    descending: bool,
) -> anyhow::Result<Vec<[String; 2]>> {
    let mut levels: Vec<[String; 2]> = side
        .iter()
        .map(|(price, quantity)| [price.to_string(), quantity.to_string()])
        .collect();
    if descending {
        levels.reverse();
    }
    Ok(levels)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub session_id: String,
    pub symbol: String,
    pub last_update_id: u64,
    pub synced: bool,
    pub bridged: bool,
    pub continuity_complete: bool,
    pub stream_coverage_verified: bool,
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
}

pub fn checkpoint_event(checkpoint: Checkpoint, reason: &str, replay_safe: bool) -> Value {
    let mut value = serde_json::to_value(checkpoint).expect("Checkpoint is always serializable");
    let object = value
        .as_object_mut()
        .expect("Checkpoint serializes as object");
    object.insert("reason".to_owned(), reason.into());
    object.insert("replay_safe".to_owned(), replay_safe.into());
    value
}

#[derive(Debug, Clone)]
pub struct SegmentConfig {
    pub spool_dir: PathBuf,
    pub market: Market,
    pub dataset: String,
    pub shard_id: String,
    pub symbols: Vec<String>,
    pub security_token_symbols: Vec<String>,
    pub excluded_symbols: Vec<String>,
    pub snapshot_limit: u64,
    pub zstd_timeout: Duration,
    pub stream_types: Vec<String>,
}

pub struct Segment {
    config: SegmentConfig,
    pub start_ns: u64,
    manifest_start_ns: u64,
    pub end_ns: u64,
    path: PathBuf,
    writer: BufWriter<File>,
    counts: BTreeMap<String, u64>,
    trade_summaries: AggregateTradeSummaryBuilder,
    replay_safe: bool,
    raw_trade_incomplete_symbols: BTreeSet<String>,
    snapshot_ready_symbols: BTreeSet<String>,
    bridged_symbols: BTreeSet<String>,
    stream_coverage_verified_symbols: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadinessSummary {
    snapshot_ready_count: usize,
    bridged_count: usize,
    stream_coverage_verified_count: usize,
    snapshot_only_symbols: Vec<String>,
    all_symbols_bridged: bool,
    all_stream_coverage_verified: bool,
}

fn readiness_summary<'a>(
    symbol_count: usize,
    states: impl Iterator<Item = (&'a str, bool, bool, bool)>,
) -> ReadinessSummary {
    let mut snapshot_ready_count = 0;
    let mut bridged_count = 0;
    let mut stream_coverage_verified_count = 0;
    let mut snapshot_only_symbols = Vec::new();
    for (symbol, synced, bridged, stream_coverage_verified) in states {
        snapshot_ready_count += usize::from(synced);
        bridged_count += usize::from(synced && bridged);
        stream_coverage_verified_count += usize::from(stream_coverage_verified);
        if synced && !bridged {
            snapshot_only_symbols.push(symbol.to_owned());
        }
    }
    snapshot_only_symbols.sort();
    ReadinessSummary {
        snapshot_ready_count,
        bridged_count,
        stream_coverage_verified_count,
        snapshot_only_symbols,
        all_symbols_bridged: symbol_count > 0 && bridged_count == symbol_count,
        all_stream_coverage_verified: symbol_count > 0
            && stream_coverage_verified_count == symbol_count,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentArtifacts {
    pub data: PathBuf,
    pub manifest: PathBuf,
    pub success: PathBuf,
    pub sha256: String,
}

impl Segment {
    pub fn create(config: SegmentConfig, start_ns: u64) -> anyhow::Result<Self> {
        let (date, hour) = segment_partition(start_ns)?;
        let directory = config
            .spool_dir
            .join(format!("date={date}"))
            .join(format!("hour={hour}"));
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("part-{start_ns}.jsonl.part"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)?;
        Ok(Self {
            config,
            start_ns,
            manifest_start_ns: start_ns,
            end_ns: 0,
            path,
            writer: BufWriter::with_capacity(1024 * 1024, file),
            counts: BTreeMap::new(),
            trade_summaries: AggregateTradeSummaryBuilder::default(),
            replay_safe: true,
            raw_trade_incomplete_symbols: BTreeSet::new(),
            snapshot_ready_symbols: BTreeSet::new(),
            bridged_symbols: BTreeSet::new(),
            stream_coverage_verified_symbols: BTreeSet::new(),
        })
    }

    pub fn write(
        &mut self,
        event_type: &str,
        payload: Value,
        received_at_ns: u64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            received_at_ns >= self.start_ns,
            "event received at {received_at_ns} predates its start boundary {}",
            self.start_ns
        );
        self.manifest_start_ns = self.manifest_start_ns.min(received_at_ns);
        self.end_ns = self.end_ns.max(received_at_ns);
        let mut envelope = serde_json::Map::new();
        envelope.insert("received_at_ns".to_owned(), received_at_ns.into());
        envelope.insert("type".to_owned(), event_type.into());
        let payload = payload
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("archive payload must be an object"))?;
        if event_type == "stale_raw_trade" {
            self.replay_safe = false;
            if let Some(symbol) = payload.get("symbol").and_then(Value::as_str) {
                self.raw_trade_incomplete_symbols
                    .insert(symbol.to_ascii_uppercase());
            }
        }
        if event_type == "checkpoint" {
            if let Some(symbol) = payload.get("symbol").and_then(Value::as_str) {
                if payload.get("synced").and_then(Value::as_bool) == Some(true) {
                    self.snapshot_ready_symbols.insert(symbol.to_owned());
                }
                if payload.get("continuity_complete").and_then(Value::as_bool) == Some(true) {
                    self.bridged_symbols.insert(symbol.to_owned());
                }
                if payload
                    .get("stream_coverage_verified")
                    .and_then(Value::as_bool)
                    == Some(true)
                {
                    self.stream_coverage_verified_symbols
                        .insert(symbol.to_owned());
                }
            }
        }
        envelope.extend(payload.clone());
        envelope.insert("schema".to_owned(), RAW_SCHEMA.into());
        if event_type == "agg_trade" {
            let trade = AggregateTrade::from_archived_event(&envelope, received_at_ns)?;
            self.trade_summaries.observe(&trade)?;
        }
        serde_json::to_writer(&mut self.writer, &envelope)?;
        self.writer.write_all(b"\n")?;
        *self.counts.entry(event_type.to_owned()).or_default() += 1;
        Ok(())
    }

    pub fn mark_replay_unsafe(&mut self) {
        self.replay_safe = false;
    }

    pub fn is_replay_safe(&self) -> bool {
        self.replay_safe
    }

    pub fn update_catalog(
        &mut self,
        symbols: Vec<String>,
        security_token_symbols: Vec<String>,
        excluded_symbols: Vec<String>,
    ) {
        self.config.symbols = symbols;
        self.config.security_token_symbols = security_token_symbols;
        self.config.excluded_symbols = excluded_symbols;
    }

    pub fn event_count(&self, event_type: &str) -> u64 {
        self.counts.get(event_type).copied().unwrap_or(0)
    }

    pub fn close(mut self) -> anyhow::Result<Option<SegmentArtifacts>> {
        let has_replay_safe_checkpoint = self.replay_safe && self.event_count("checkpoint") > 0;
        let readiness = readiness_summary(
            self.config.symbols.len(),
            self.config.symbols.iter().map(|symbol| {
                (
                    symbol.as_str(),
                    self.snapshot_ready_symbols.contains(symbol),
                    self.bridged_symbols.contains(symbol),
                    self.stream_coverage_verified_symbols.contains(symbol),
                )
            }),
        );
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        let trade_summaries = self.trade_summaries.finish()?;
        if self.path.metadata()?.len() == 0 {
            fs::remove_file(self.path)?;
            return Ok(None);
        }
        finalize_segment(
            &self.config,
            &self.path,
            self.counts,
            trade_summaries,
            RAW_SCHEMA,
            self.start_ns,
            self.manifest_start_ns,
            self.end_ns,
            has_replay_safe_checkpoint,
            self.raw_trade_incomplete_symbols,
            readiness,
        )
        .map(Some)
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_segment(
    config: &SegmentConfig,
    path: &Path,
    counts: BTreeMap<String, u64>,
    trade_summaries: BTreeMap<String, AggregateTradeSummary>,
    schema: &str,
    identity_start_ns: u64,
    start_ns: u64,
    end_ns: u64,
    has_replay_safe_checkpoint: bool,
    raw_trade_incomplete_symbols: BTreeSet<String>,
    readiness: ReadinessSummary,
) -> anyhow::Result<SegmentArtifacts> {
    let replay_scope = match schema {
        LEGACY_LOB_TAPE_SCHEMA => "captured_snapshot_seed_plus_sequence_checked_diffs",
        schema if market_tape_schema(schema) => {
            "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs"
        }
        _ => anyhow::bail!("unsupported recovered tape schema {schema}"),
    };
    let lob_continuity = if market_tape_schema(schema) {
        Some(summarize_lob_continuity(path, config.symbols.clone())?)
    } else {
        None
    };
    let data = path.with_file_name(format!("part-{identity_start_ns}.jsonl.zst"));
    let temporary_data = data.with_extension("zst.tmp");
    let mut command = Command::new("zstd");
    command
        .args(["-q", "-f", "-T1", "-3"])
        .arg(path)
        .arg("-o")
        .arg(&temporary_data);
    let status = command_status_with_timeout(&mut command, config.zstd_timeout)?;
    if !status.success() {
        anyhow::bail!("zstd failed with {status}");
    }
    File::open(&temporary_data)?.sync_all()?;
    fs::rename(&temporary_data, &data)?;
    sync_parent(&data)?;

    let digest = sha256_file(&data)?;
    let (date, hour) = segment_partition(identity_start_ns)?;
    let events: u64 = counts.values().sum();
    let mut metadata = json!({
        "schema": schema,
        "venue": "binance",
        "market": config.market.as_str(),
        "dataset": config.dataset,
        "shard_id": config.shard_id,
        "mode": "diff",
        "symbols": config.symbols,
        "security_token_symbols": config.security_token_symbols,
        "excluded_symbols": config.excluded_symbols,
        "snapshot_limit": config.snapshot_limit,
        "replay_scope": replay_scope,
        "venue_depth_complete": false,
        "events": events,
        "event_types": counts,
        "has_replay_safe_checkpoint": has_replay_safe_checkpoint,
        "raw_trade_incomplete_symbols": raw_trade_incomplete_symbols.into_iter().collect::<Vec<_>>(),
        "snapshot_ready_count": readiness.snapshot_ready_count,
        "bridged_count": readiness.bridged_count,
        "stream_coverage_verified_count": readiness.stream_coverage_verified_count,
        "snapshot_only_symbols": readiness.snapshot_only_symbols,
        "all_symbols_bridged": readiness.all_symbols_bridged,
        "all_stream_coverage_verified": readiness.all_stream_coverage_verified,
        "start_received_at_ns": start_ns,
        "end_received_at_ns": end_ns,
        "date": date,
        "hour": hour,
        "file": data.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
        "bytes": data.metadata()?.len(),
        "sha256": digest,
    });
    if market_tape_schema(schema) {
        let metadata = metadata.as_object_mut().expect("manifest is an object");
        metadata.insert(
            "trade_representation".to_owned(),
            "aggregate_trade_only".into(),
        );
        metadata.insert(
            "price_surface_derivation".to_owned(),
            "latest aggregate trade price".into(),
        );
        metadata.insert(
            "trade_summaries".to_owned(),
            serde_json::to_value(trade_summaries)?,
        );
        metadata.insert(
            "trade_summary_contract".to_owned(),
            AGGREGATE_TRADE_SUMMARY_CONTRACT.into(),
        );
        metadata.insert(
            "lob_continuity".to_owned(),
            serde_json::to_value(lob_continuity.context("market tape has no LOB summary")?)?,
        );
        // The v2 manifest declares its per-symbol stream-type list; v1 tapes
        // never carry the field so old segments verify byte-identically.
        if schema == MARKET_TAPE_SCHEMA_V2 {
            anyhow::ensure!(
                !config.stream_types.is_empty()
                    && config.stream_types.iter().all(|value| !value.is_empty())
                    && config.stream_types.iter().collect::<BTreeSet<_>>().len()
                        == config.stream_types.len(),
                "v2 market tape requires a declared stream type set"
            );
            metadata.insert(
                "stream_types".to_owned(),
                config.stream_types.clone().into(),
            );
        }
    }
    let manifest = data.with_file_name(format!(
        "{}.manifest.json",
        data.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ));
    atomic_write_json(&manifest, &metadata)?;
    let success = data.with_file_name(format!(
        "{}._SUCCESS",
        data.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ));
    fs::remove_file(path)?;
    Ok(SegmentArtifacts {
        data,
        manifest,
        success,
        sha256: digest,
    })
}

fn summarize_lob_continuity(
    path: &Path,
    symbols: Vec<String>,
) -> anyhow::Result<LobContinuitySummary> {
    let mut summary = LobContinuitySummaryBuilder::new(symbols)?;
    for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
        let line = line?;
        let raw: Value = serde_json::from_str(&line)
            .with_context(|| format!("parse LOB continuity row {}", index + 1))?;
        summary.observe(
            raw.as_object()
                .context("LOB continuity row must be an object")?,
        )?;
    }
    summary.finish()
}

pub fn write_success_marker(data: &Path, digest: &str) -> anyhow::Result<PathBuf> {
    let success = data.with_file_name(format!(
        "{}._SUCCESS",
        data.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    ));
    atomic_write(&success, format!("{digest}\n").as_bytes())?;
    Ok(success)
}

pub fn recover_parts(config: &SegmentConfig) -> anyhow::Result<Vec<SegmentArtifacts>> {
    let mut artifacts = Vec::new();
    for path in files_with_suffix(&config.spool_dir, ".jsonl.part")? {
        let bytes = fs::read(&path)?;
        if bytes.is_empty() {
            fs::remove_file(path)?;
            continue;
        }
        let mut counts = BTreeMap::new();
        let mut aggregate_trade_sequence = AggregateTradeSequenceValidator::default();
        let mut trade_summaries = AggregateTradeSummaryBuilder::default();
        let mut start_ns = 0_u64;
        let mut end_ns = 0_u64;
        let mut offset = 0_usize;
        let mut invalid_at = None;
        let mut detected_schema: Option<(bool, String)> = None;
        let mut quarantine = false;
        let mut raw_trade_incomplete_symbols = BTreeSet::new();
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            let complete = line.last() == Some(&b'\n');
            let parsed = serde_json::from_slice::<Value>(line);
            let valid = parsed.as_ref().is_ok_and(|value| {
                value.is_object()
                    && value
                        .get("received_at_ns")
                        .and_then(Value::as_u64)
                        .is_some()
            });
            if !complete || !valid {
                invalid_at = Some((offset, offset + line.len() < bytes.len()));
                break;
            }
            let event = parsed.expect("validated JSON");
            let received = event["received_at_ns"]
                .as_u64()
                .expect("validated timestamp");
            if start_ns == 0 {
                start_ns = received;
            }
            end_ns = received;
            let row_schema = match event.get("schema") {
                None => (false, LEGACY_LOB_TAPE_SCHEMA.to_owned()),
                Some(Value::String(schema)) if supported_schema(schema) => {
                    (true, schema.to_owned())
                }
                Some(_) => {
                    quarantine = true;
                    break;
                }
            };
            if detected_schema
                .as_ref()
                .is_some_and(|detected| detected != &row_schema)
            {
                quarantine = true;
                break;
            }
            detected_schema.get_or_insert_with(|| row_schema.clone());
            let event_type = match event.get("type").and_then(Value::as_str) {
                Some(event_type) if !event_type.is_empty() => event_type,
                None if !row_schema.0 => "diff",
                _ => {
                    quarantine = true;
                    break;
                }
            };
            if !event_type_allowed(&row_schema.1, event_type) {
                quarantine = true;
                break;
            }
            if event_type == "agg_trade" {
                let Some(raw) = event.as_object() else {
                    quarantine = true;
                    break;
                };
                let Ok(trade) = AggregateTrade::from_archived_event(raw, received) else {
                    quarantine = true;
                    break;
                };
                if aggregate_trade_sequence.observe(&trade).is_err()
                    || trade_summaries.observe(&trade).is_err()
                {
                    quarantine = true;
                    break;
                }
            }
            if event_type == "stale_raw_trade" {
                if let Some(symbol) = event.get("symbol").and_then(Value::as_str) {
                    raw_trade_incomplete_symbols.insert(symbol.to_ascii_uppercase());
                } else {
                    quarantine = true;
                    break;
                }
            }
            *counts.entry(event_type.to_owned()).or_default() += 1;
            offset += line.len();
        }
        if quarantine {
            fs::rename(&path, path.with_extension("part.corrupt"))?;
            continue;
        }
        if let Some((valid_bytes, has_following_data)) = invalid_at {
            if has_following_data {
                fs::rename(&path, path.with_extension("part.corrupt"))?;
                continue;
            }
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(u64::try_from(valid_bytes)?)?;
        }
        if counts.is_empty() {
            fs::remove_file(path)?;
            continue;
        }
        let schema = detected_schema
            .map(|(_, schema)| schema)
            .expect("non-empty recovered segment has a detected schema");
        let trade_summaries = trade_summaries.finish()?;
        artifacts.push(finalize_segment(
            config,
            &path,
            counts,
            trade_summaries,
            &schema,
            start_ns,
            start_ns,
            end_ns,
            false,
            raw_trade_incomplete_symbols,
            readiness_summary(
                config.symbols.len(),
                config
                    .symbols
                    .iter()
                    .map(|symbol| (symbol.as_str(), false, false, false)),
            ),
        )?);
    }
    Ok(artifacts)
}

pub fn files_with_suffix(root: &Path, suffix: &str) -> anyhow::Result<Vec<PathBuf>> {
    fn visit(path: &Path, suffix: &str, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            anyhow::bail!("refusing symlink while scanning spool: {}", path.display());
        }
        if !metadata.is_dir() {
            anyhow::bail!("spool scan root is not a directory: {}", path.display());
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                anyhow::bail!("refusing symlink while scanning spool: {}", path.display());
            }
            if file_type.is_dir() {
                visit(&path, suffix, files)?;
            } else if file_type.is_file() && path.to_string_lossy().ends_with(suffix) {
                files.push(path);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(root, suffix, &mut files)?;
    files.sort();
    Ok(files)
}

#[allow(clippy::too_many_arguments)]
pub fn write_health(
    spool_dir: &Path,
    market: Market,
    dataset: &str,
    session_id: &str,
    status: &str,
    sequence_gaps: u64,
    pending_upload_segments: usize,
    queue: QueueHealth,
    states: &HashMap<String, OrderBookState>,
) -> anyhow::Result<()> {
    let upload = read_upload_status(spool_dir);
    let disk_free_gb = disk_free_gb(spool_dir);
    let disk_warning_threshold_gb = std::env::var("MIN_FREE_GB")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(20.0);
    let symbols = states
        .iter()
        .map(|(symbol, state)| {
            (
                symbol.clone(),
                json!({
                    "synced": state.synced,
                    "bridged": state.bridged,
                    "continuity_complete": state.continuity_complete(),
                    "stream_coverage_verified": state.stream_coverage_verified,
                    "last_update_id": state.last_update_id(),
                    "bid_levels": state.bid_levels(),
                    "ask_levels": state.ask_levels(),
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let readiness = readiness_summary(
        states.len(),
        states.iter().map(|(symbol, state)| {
            (
                symbol.as_str(),
                state.synced,
                state.continuity_complete(),
                state.stream_coverage_verified,
            )
        }),
    );
    atomic_write_json(
        &spool_dir.join("health.json"),
        &json!({
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "updated_at_ns": now_ns()?,
            "status": status,
            "market": market.as_str(),
            "dataset": dataset,
            "symbol_count": states.len(),
            "snapshot_ready_count": readiness.snapshot_ready_count,
            "bridged_count": readiness.bridged_count,
            "stream_coverage_verified_count": readiness.stream_coverage_verified_count,
            "snapshot_only_symbols": readiness.snapshot_only_symbols,
            "all_symbols_bridged": readiness.all_symbols_bridged,
            "all_stream_coverage_verified": readiness.all_stream_coverage_verified,
            // The collector only verifies stream coverage after the connected
            // shards match the full declared per-symbol stream-type list, so
            // verified coverage is always full-family coverage; the explicit
            // field lets deploy policies pin that semantics without weakening
            // the depth-only readiness fields above.
            "full_stream_coverage_verified": readiness.all_stream_coverage_verified,
            "session_id": session_id,
            "sequence_gaps": sequence_gaps,
            "pending_upload_segments": pending_upload_segments,
            "disk_free_gb": disk_free_gb,
            "disk_warning": disk_free_gb.is_some_and(|free| free < disk_warning_threshold_gb),
            "disk_warning_threshold_gb": disk_warning_threshold_gb,
            "upload_warning": upload.last_error_at.is_some(),
            "upload_failure_count": upload.failure_count,
            "last_upload_success_at": upload.last_success_at,
            "last_upload_error_at": upload.last_error_at,
            "last_upload_error": upload.last_error,
            "queue_capacity": queue.capacity,
            "queue_remaining_capacity": queue.remaining_capacity,
            "queue_saturated": queue.saturated,
            "symbols": symbols,
        }),
    )
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadStatus {
    pub updated_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub failure_count: u64,
    pub discovery_failed: bool,
    pub pending_batches: Option<u64>,
    pub uploaded_batches: u64,
    pub retried_batches: u64,
    pub failed_batches: Vec<Value>,
    pub last_uploaded_object: Option<String>,
    pub last_uploaded_triplet: Option<Value>,
}

pub fn read_upload_status(spool_dir: &Path) -> UploadStatus {
    File::open(spool_dir.join("upload-status.json"))
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok())
        .unwrap_or_default()
}

pub fn write_upload_status(spool_dir: &Path, status: &UploadStatus) -> anyhow::Result<()> {
    atomic_write_json(
        &spool_dir.join("upload-status.json"),
        &serde_json::to_value(status)?,
    )
}

fn disk_free_gb(path: &Path) -> Option<f64> {
    let output = Command::new("df").args(["-Pk"]).arg(path).output().ok()?;
    let line = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .last()?
        .to_owned();
    let available_kib = line.split_whitespace().nth(3)?.parse::<f64>().ok()?;
    Some((available_kib / 1024.0 / 1024.0 * 10.0).round() / 10.0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueHealth {
    pub capacity: usize,
    pub remaining_capacity: usize,
    pub saturated: bool,
}

impl QueueHealth {
    pub fn from_sender<T>(sender: &mpsc::Sender<T>) -> Self {
        let remaining_capacity = sender.capacity();
        Self {
            capacity: sender.max_capacity(),
            remaining_capacity,
            saturated: remaining_capacity == 0,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendOutcome<T> {
    Sent,
    Shutdown(T),
}

pub static ACTIVE_SENDS: AtomicUsize = AtomicUsize::new(0);

struct ActiveSendGuard;

impl ActiveSendGuard {
    fn acquire() -> Self {
        ACTIVE_SENDS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ActiveSendGuard {
    fn drop(&mut self) {
        ACTIVE_SENDS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Lossless backpressure that remains cancellable when the bounded queue is full.
pub async fn send_or_shutdown<T>(
    sender: &mpsc::Sender<T>,
    item: T,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<SendOutcome<T>> {
    if *shutdown.borrow() {
        return Ok(SendOutcome::Shutdown(item));
    }
    let _active_send = ActiveSendGuard::acquire();
    tokio::select! {
        biased;
        changed = shutdown.changed() => {
            changed.map_err(|_| anyhow::anyhow!("shutdown channel closed"))?;
            Ok(SendOutcome::Shutdown(item))
        }
        permit = sender.reserve() => {
            permit
                .map(|permit| {
                    permit.send(item);
                    SendOutcome::Sent
                })
                .map_err(|_| anyhow::anyhow!("archive queue closed"))
        }
    }
}

pub fn segment_partition(timestamp_ns: u64) -> anyhow::Result<(String, String)> {
    let seconds = i64::try_from(timestamp_ns / 1_000_000_000)?;
    let value = chrono::DateTime::from_timestamp(seconds, 0)
        .ok_or_else(|| anyhow::anyhow!("timestamp out of range"))?;
    Ok((
        value.format("%Y-%m-%d").to_string(),
        value.format("%H").to_string(),
    ))
}

pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
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

/// Run a child process with a hard deadline so compression or cloud uploads
/// cannot wedge segment rotation indefinitely.
pub fn command_status_with_timeout(
    command: &mut Command,
    timeout: Duration,
) -> anyhow::Result<ExitStatus> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            anyhow::bail!("child process timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn atomic_write_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("atomic target has no parent"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let (temporary, mut output) = (0..32)
        .find_map(|_| {
            let temporary = parent.join(format!(".{file_name}.{:016x}.tmp", random::<u64>()));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
            {
                Ok(output) => Some(Ok((temporary, output))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .ok_or_else(|| anyhow::anyhow!("could not allocate exclusive atomic-write temporary"))?;
    let write_result = (|| -> anyhow::Result<()> {
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn now_ns() -> anyhow::Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn recovery_config(spool_dir: PathBuf) -> SegmentConfig {
        SegmentConfig {
            spool_dir,
            market: Market::Spot,
            dataset: "spot_all".into(),
            shard_id: "all".into(),
            symbols: vec!["BTCUSDT".into()],
            security_token_symbols: vec![],
            excluded_symbols: vec![],
            snapshot_limit: 100,
            zstd_timeout: Duration::from_secs(30),
            stream_types: vec![
                "depth@100ms".into(),
                "aggTrade".into(),
                "trade".into(),
                "bookTicker".into(),
            ],
        }
    }

    fn write_recovery_part(config: &SegmentConfig, start_ns: u64, rows: &[Value]) -> PathBuf {
        let (date, hour) = segment_partition(start_ns).unwrap();
        let directory = config
            .spool_dir
            .join(format!("date={date}"))
            .join(format!("hour={hour}"));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("part-{start_ns}.jsonl.part"));
        let mut bytes = rows
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            .into_bytes();
        bytes.push(b'\n');
        fs::write(&path, bytes).unwrap();
        path
    }

    fn diff(symbol: &str, first: u64, final_id: u64, pu: Option<u64>) -> DepthDiff {
        DepthDiff {
            symbol: symbol.to_owned(),
            first_update_id: first,
            final_update_id: final_id,
            previous_update_id: pu,
            bids: Vec::new(),
            asks: Vec::new(),
        }
    }

    fn snapshot(last_update_id: u64) -> Value {
        json!({
            "lastUpdateId": last_update_id,
            "bids": [["100.00000000", "2.00000000"]],
            "asks": [["101.00000000", "3.00000000"]],
        })
    }

    #[test]
    fn compact_book_retains_full_depth_and_roundtrips_decimal_strings() {
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(10);
        // A deep, out-of-order snapshot: all levels are retained and the
        // checkpoint round-trips Binance's fixed-decimal strings losslessly.
        let snapshot = json!({
            "lastUpdateId": 100,
            "bids": [
                ["98.00000000", "3.00000000"],
                ["100.00000000", "1.00000000"],
                ["99.00000000", "2.00000000"],
                ["97.50000000", "4.50000000"],
            ],
            "asks": [
                ["103.00000000", "3.00000000"],
                ["101.00000000", "1.00000000"],
                ["102.00000000", "2.00000000"],
            ],
        });
        state.install_snapshot(&snapshot, &mut budget).unwrap();
        assert_eq!(state.bid_levels(), 4);
        assert_eq!(state.ask_levels(), 3);

        let checkpoint = state.checkpoint("session-1").unwrap();
        // Bids are emitted highest-first, asks lowest-first, with exact
        // decimal formatting preserved.
        assert_eq!(
            checkpoint.bids,
            vec![
                ["100.00000000".to_string(), "1.00000000".to_string()],
                ["99.00000000".to_string(), "2.00000000".to_string()],
                ["98.00000000".to_string(), "3.00000000".to_string()],
                ["97.50000000".to_string(), "4.50000000".to_string()],
            ]
        );
        assert_eq!(
            checkpoint.asks,
            vec![
                ["101.00000000".to_string(), "1.00000000".to_string()],
                ["102.00000000".to_string(), "2.00000000".to_string()],
                ["103.00000000".to_string(), "3.00000000".to_string()],
            ]
        );
    }

    #[test]
    fn compact_book_applies_diffs_with_zero_quantity_removal() {
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(10);
        state
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [
                        ["100.00000000", "1.00000000"],
                        ["99.00000000", "2.00000000"],
                        ["98.00000000", "3.00000000"],
                    ],
                    "asks": [["101.00000000", "1.00000000"]],
                }),
                &mut budget,
            )
            .unwrap();

        // A diff inserts a new mid-price level and removes the best bid.
        let mut update = diff("BTCUSDT", 101, 102, None);
        update.bids = vec![
            ["99.50000000".to_string(), "5.00000000".to_string()],
            ["100.00000000".to_string(), "0.00000000".to_string()],
        ];
        state.apply_diff(update, &mut budget).unwrap();

        let checkpoint = state.checkpoint("session-1").unwrap();
        assert_eq!(
            checkpoint.bids,
            vec![
                ["99.50000000".to_string(), "5.00000000".to_string()],
                ["99.00000000".to_string(), "2.00000000".to_string()],
                ["98.00000000".to_string(), "3.00000000".to_string()],
            ]
        );
        assert_eq!(state.bid_levels(), 3);
    }

    #[test]
    fn spot_bridges_snapshot_and_rejects_gap() {
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(10);
        state
            .apply_diff(diff("BTCUSDT", 101, 102, None), &mut budget)
            .unwrap();
        state.install_snapshot(&snapshot(100), &mut budget).unwrap();
        assert_eq!(state.last_update_id(), Some(102));
        assert!(state.bridged);
        assert_eq!(budget.count(), 0);

        let gap = state
            .apply_diff(diff("BTCUSDT", 104, 104, None), &mut budget)
            .unwrap_err();
        assert!(gap.downcast_ref::<SequenceGap>().is_some());
        assert!(!state.synced);
    }

    #[test]
    fn verified_stream_coverage_seeds_static_book_without_fabricated_diff() {
        let mut state = OrderBookState::new("BNSOLSOL", Market::Spot);
        let mut budget = PendingBudget::new(10);

        state.verify_stream_coverage();
        state.install_snapshot(&snapshot(100), &mut budget).unwrap();

        assert!(state.synced);
        assert!(!state.bridged);
        assert!(state.continuity_complete());
        assert_eq!(state.last_update_id(), Some(100));
        let checkpoint = state.checkpoint("session-static").unwrap();
        assert!(checkpoint.stream_coverage_verified);
        assert!(!checkpoint.bridged);
        assert!(checkpoint.continuity_complete);
    }

    #[test]
    fn usdm_uses_pu_even_when_global_range_moved_past_snapshot() {
        let mut state = OrderBookState::new("1000SHIBUSDT", Market::Usdm);
        let mut budget = PendingBudget::new(10);
        state.install_snapshot(&snapshot(100), &mut budget).unwrap();
        state
            .apply_diff(diff("1000SHIBUSDT", 150, 175, Some(100)), &mut budget)
            .unwrap();
        state
            .apply_diff(diff("1000SHIBUSDT", 176, 180, Some(175)), &mut budget)
            .unwrap();
        assert_eq!(state.last_update_id(), Some(180));

        assert!(state
            .apply_diff(diff("1000SHIBUSDT", 181, 182, Some(174)), &mut budget,)
            .is_err());
    }

    #[test]
    fn verified_coverage_does_not_skip_usdm_initial_overlap_bridge() {
        let mut state = OrderBookState::new("BTCUSDT", Market::Usdm);
        let mut budget = PendingBudget::new(10);
        state.verify_stream_coverage();
        state.install_snapshot(&snapshot(100), &mut budget).unwrap();

        assert!(!state.bridged);
        state
            .apply_diff(diff("BTCUSDT", 95, 105, Some(90)), &mut budget)
            .unwrap();
        assert!(state.bridged);
        assert_eq!(state.last_update_id(), Some(105));
    }

    #[test]
    fn snapshot_requires_two_present_non_empty_array_sides() {
        let cases = [
            json!({"lastUpdateId":100,"asks":[["101","1"]]}),
            json!({"lastUpdateId":100,"bids":{},"asks":[["101","1"]]}),
            json!({"lastUpdateId":100,"bids":[],"asks":[["101","1"]]}),
        ];
        for snapshot in cases {
            let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
            let mut budget = PendingBudget::new(1);
            assert!(state.install_snapshot(&snapshot, &mut budget).is_err());
        }
    }

    #[test]
    fn usdm_ignores_stale_update_before_pu_validation() {
        let mut state = OrderBookState::new("BTCUSDT", Market::Usdm);
        let mut budget = PendingBudget::new(10);
        state.install_snapshot(&snapshot(100), &mut budget).unwrap();
        state
            .apply_diff(diff("BTCUSDT", 90, 100, Some(42)), &mut budget)
            .unwrap();
        assert!(state.synced);
        assert!(!state.bridged);
        assert_eq!(state.last_update_id(), Some(100));
    }

    #[test]
    fn checkpoint_sorts_and_preserves_decimal_strings() {
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(10);
        state.install_snapshot(&snapshot(100), &mut budget).unwrap();
        let mut update = diff("BTCUSDT", 101, 101, None);
        update.bids = vec![["100.00000000".to_string(), "0.00000000".to_string()]];
        update.asks = vec![["102.12345678".to_string(), "1.00000001".to_string()]];
        state.apply_diff(update, &mut budget).unwrap();
        let checkpoint = state.checkpoint("session-1").unwrap();
        assert!(checkpoint.bids.is_empty());
        assert_eq!(checkpoint.asks[1], ["102.12345678", "1.00000001"]);
        let event = checkpoint_event(checkpoint, "scheduled", true);
        assert_eq!(event["session_id"], "session-1");
        assert_eq!(event["last_update_id"], 101);
        assert!(event.get("checkpoint").is_none());
        assert_eq!(event["reason"], "scheduled");
        assert_eq!(event["replay_safe"], true);
    }

    #[tokio::test]
    async fn full_queue_send_is_lossless_and_cancellable() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender.send(1_u64).await.unwrap();
        assert!(QueueHealth::from_sender(&sender).saturated);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let blocked_sender = sender.clone();
        let task = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            send_or_shutdown(&blocked_sender, 2, &mut shutdown_rx).await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!task.is_finished());
        shutdown_tx.send(true).unwrap();
        assert_eq!(task.await.unwrap().unwrap(), SendOutcome::Shutdown(2));
        assert_eq!(ACTIVE_SENDS.load(Ordering::Acquire), 0);
        assert_eq!(receiver.recv().await, Some(1));
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));

        sender.send(3_u64).await.unwrap();
        let blocked_sender = sender.clone();
        let (_abort_shutdown_tx, abort_shutdown_rx) = watch::channel(false);
        let aborted = tokio::spawn(async move {
            let mut shutdown_rx = abort_shutdown_rx;
            send_or_shutdown(&blocked_sender, 4, &mut shutdown_rx).await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(ACTIVE_SENDS.load(Ordering::Acquire) > 0);
        aborted.abort();
        assert!(aborted.await.unwrap_err().is_cancelled());
        assert_eq!(ACTIVE_SENDS.load(Ordering::Acquire), 0);
    }

    #[cfg(unix)]
    #[test]
    fn child_process_timeout_prevents_rotation_wedge() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        let started = Instant::now();
        let error = command_status_with_timeout(&mut command, Duration::from_millis(20))
            .expect_err("sleep must be terminated by the deadline");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn health_exposes_persisted_upload_failure() {
        let root = std::env::temp_dir().join(format!("monday-health-test-{}", now_ns().unwrap()));
        fs::create_dir_all(&root).unwrap();
        write_upload_status(
            &root,
            &UploadStatus {
                last_success_at: None,
                last_error_at: Some("2026-07-14T16:00:00Z".into()),
                last_error: Some("oss down".into()),
                failure_count: 7,
                ..UploadStatus::default()
            },
        )
        .unwrap();
        write_health(
            &root,
            Market::Spot,
            "spot_all",
            "session-1",
            "synced",
            0,
            1,
            QueueHealth {
                capacity: 1,
                remaining_capacity: 1,
                saturated: false,
            },
            &HashMap::new(),
        )
        .unwrap();
        let health: Value =
            serde_json::from_reader(File::open(root.join("health.json")).unwrap()).unwrap();
        assert_eq!(health["upload_warning"], true);
        assert_eq!(health["upload_failure_count"], 7);
        assert_eq!(health["last_upload_error"], "oss down");
        assert_eq!(health["snapshot_ready_count"], 0);
        assert_eq!(health["bridged_count"], 0);
        assert_eq!(health["snapshot_only_symbols"], json!([]));
        assert_eq!(health["all_symbols_bridged"], false);
        assert_eq!(health["all_stream_coverage_verified"], false);
        assert_eq!(health["full_stream_coverage_verified"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn spool_scan_rejects_root_directory_and_file_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("monday-scan-root-{}", now_ns().unwrap()));
        let outside =
            std::env::temp_dir().join(format!("monday-scan-outside-{}", now_ns().unwrap()));
        let root_link =
            std::env::temp_dir().join(format!("monday-scan-root-link-{}", now_ns().unwrap()));
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("outside.manifest.json");
        fs::write(&outside_file, b"do-not-delete\n").unwrap();

        symlink(&outside, &root_link).unwrap();
        let error = files_with_suffix(&root_link, ".manifest.json").unwrap_err();
        assert!(error.to_string().contains("refusing symlink"));
        fs::remove_file(&root_link).unwrap();

        symlink(&outside, root.join("escape")).unwrap();
        let error = files_with_suffix(&root, ".manifest.json").unwrap_err();
        assert!(error.to_string().contains("refusing symlink"));
        fs::remove_file(root.join("escape")).unwrap();

        symlink(&outside_file, root.join("linked.manifest.json")).unwrap();
        let error = files_with_suffix(&root, ".manifest.json").unwrap_err();
        assert!(error.to_string().contains("refusing symlink"));
        assert_eq!(fs::read(&outside_file).unwrap(), b"do-not-delete\n");

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn segment_rejects_event_that_predates_its_start_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let start_ns = 1_700_000_000_000_000_000;
        let mut segment = Segment::create(recovery_config(root.clone()), start_ns).unwrap();

        let error = segment
            .write(
                "session_start",
                json!({"session_id":"session-1"}),
                start_ns - 1,
            )
            .unwrap_err();

        assert!(error.to_string().contains("predates its start boundary"));
        drop(segment);
    }

    #[test]
    fn segment_declares_complete_market_tape_surface() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!("monday-lob-test-{}", now_ns().unwrap()));
        let config = SegmentConfig {
            spool_dir: root.clone(),
            market: Market::Spot,
            dataset: "spot_all".into(),
            shard_id: "all".into(),
            symbols: vec!["BTCUSDT".into()],
            security_token_symbols: vec![],
            excluded_symbols: vec![],
            snapshot_limit: 100,
            zstd_timeout: Duration::from_secs(30),
            stream_types: vec![
                "depth@100ms".into(),
                "aggTrade".into(),
                "trade".into(),
                "bookTicker".into(),
            ],
        };
        let start_ns = 1_700_000_000_000_000_000;
        let mut segment = Segment::create(config, start_ns).unwrap();
        segment
            .write(
                "session_start",
                json!({
                    "session_id":"session-1",
                    "market":"spot",
                    "symbols":1,
                    "websocket_shards":1
                }),
                segment.start_ns,
            )
            .unwrap();
        segment
            .write(
                "snapshot",
                json!({
                    "session_id":"session-1",
                    "symbol":"BTCUSDT",
                    "request_started_at_ns":segment.start_ns + 50_000_000,
                    "snapshot":{
                        "lastUpdateId":100,
                        "bids":[["100","1"]],
                        "asks":[["101","1"]]
                    }
                }),
                segment.start_ns + 100_000_000,
            )
            .unwrap();
        let diff_received_at_ns = segment.start_ns + 200_000_000;
        segment
            .write(
                "diff",
                json!({
                    "session_id":"session-1",
                    "frame":{
                        "stream":"btcusdt@depth@100ms",
                        "data":{
                            "e":"depthUpdate",
                            "E":diff_received_at_ns / 1_000_000,
                            "s":"BTCUSDT",
                            "U":101,
                            "u":101,
                            "b":[["100","2"]],
                            "a":[]
                        }
                    }
                }),
                diff_received_at_ns,
            )
            .unwrap();
        let first_trade_received_at_ns = segment.start_ns + 300_000_000;
        segment
            .write(
                "agg_trade",
                json!({
                    "session_id":"session-1",
                    "frame":{
                        "stream":"btcusdt@aggTrade",
                        "data":{
                            "e":"aggTrade",
                            "E":first_trade_received_at_ns / 1_000_000,
                            "s":"BTCUSDT",
                            "a":10,
                            "p":"100",
                            "q":"2",
                            "f":10,
                            "l":11,
                            "T":first_trade_received_at_ns / 1_000_000,
                            "m":false
                        }
                    }
                }),
                first_trade_received_at_ns,
            )
            .unwrap();
        let last_trade_received_at_ns = segment.start_ns + 400_000_000;
        segment
            .write(
                "agg_trade",
                json!({
                    "session_id":"session-1",
                    "frame":{
                        "stream":"btcusdt@aggTrade",
                        "data":{
                            "e":"aggTrade",
                            "E":last_trade_received_at_ns / 1_000_000,
                            "s":"BTCUSDT",
                            "a":11,
                            "p":"101",
                            "q":"3",
                            "f":12,
                            "l":14,
                            "T":last_trade_received_at_ns / 1_000_000,
                            "m":true
                        }
                    }
                }),
                last_trade_received_at_ns,
            )
            .unwrap();
        segment
            .write(
                "checkpoint",
                json!({
                    "session_id":"session-1",
                    "symbol":"BTCUSDT",
                    "last_update_id":101,
                    "synced":true,
                    "bridged":true,
                    "continuity_complete":true,
                    "bids":[["100","2"]],
                    "asks":[["101","1"]],
                    "reason":"test",
                    "replay_safe":true
                }),
                segment.start_ns + 500_000_000,
            )
            .unwrap();
        let artifacts = segment.close().unwrap().unwrap();
        let manifest: Value =
            serde_json::from_reader(File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(
            manifest["schema"],
            data::binance_market_tape::MARKET_TAPE_SCHEMA_V2
        );
        assert_eq!(
            manifest["stream_types"],
            json!(["depth@100ms", "aggTrade", "trade", "bookTicker"])
        );
        assert_eq!(manifest["start_received_at_ns"], json!(start_ns));
        assert_eq!(
            manifest["event_types"],
            json!({"agg_trade":2,"checkpoint":1,"diff":1,"session_start":1,"snapshot":1})
        );
        assert_eq!(
            manifest["lob_continuity"],
            json!({
                "contract":"binance.lob_continuity.v1",
                "capture_session_id":"session-1",
                "reconnect_boundary":true,
                "sequence_gaps":0,
                "source_time_rollbacks":0,
                "declared_symbol_count":1,
                "covered_symbol_count":1,
                "missing_symbols":[],
                "symbols":{
                    "BTCUSDT":{
                        "snapshot_seed_count":1,
                        "diff_count":1,
                        "checkpoint_count":1,
                        "stream_coverage_verified":false,
                        "first_update_id":101,
                        "last_update_id":101,
                        "first_source_time_ms":diff_received_at_ns / 1_000_000,
                        "last_source_time_ms":diff_received_at_ns / 1_000_000,
                        "first_received_at_ns":start_ns + 100_000_000,
                        "last_received_at_ns":start_ns + 500_000_000,
                        "min_source_latency_ms":0,
                        "max_source_latency_ms":0,
                        "min_bid_levels":1,
                        "max_bid_levels":1,
                        "min_ask_levels":1,
                        "max_ask_levels":1
                    }
                }
            })
        );
        assert_eq!(
            manifest["trade_summaries"]["BTCUSDT"],
            json!({
                "aggregate_trade_count":2,
                "venue_trade_count":5,
                "base_volume":"5",
                "quote_volume":"503",
                "buyer_aggressor_base_volume":"2",
                "buyer_aggressor_quote_volume":"200",
                "seller_aggressor_base_volume":"3",
                "seller_aggressor_quote_volume":"303",
                "vwap":"100.6",
                "first_event_time_ms":first_trade_received_at_ns / 1_000_000,
                "last_event_time_ms":last_trade_received_at_ns / 1_000_000,
                "first_trade_time_ms":first_trade_received_at_ns / 1_000_000,
                "last_trade_time_ms":last_trade_received_at_ns / 1_000_000,
                "first_received_at_ns":first_trade_received_at_ns,
                "last_received_at_ns":last_trade_received_at_ns,
                "first_aggregate_trade_id":10,
                "last_aggregate_trade_id":11,
                "first_trade_id":10,
                "last_trade_id":14
            })
        );
        assert_eq!(
            manifest["trade_summary_contract"],
            data::binance_market_tape::AGGREGATE_TRADE_SUMMARY_CONTRACT
        );
        assert_eq!(
            manifest["replay_scope"],
            "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs"
        );
        assert_eq!(manifest["has_replay_safe_checkpoint"], true);
        assert_eq!(manifest["snapshot_ready_count"], 1);
        assert_eq!(manifest["bridged_count"], 1);
        assert_eq!(manifest["snapshot_only_symbols"], json!([]));
        assert_eq!(manifest["all_symbols_bridged"], true);
        assert_eq!(manifest["sha256"], artifacts.sha256);
        let output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(&artifacts.data)
            .output()
            .unwrap();
        assert!(output.status.success());
        let rows = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(rows.iter().all(|row| row["schema"] == RAW_SCHEMA));
        assert_eq!(
            rows.iter()
                .map(|row| row["type"].as_str().unwrap())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "agg_trade",
                "checkpoint",
                "diff",
                "session_start",
                "snapshot"
            ])
        );
        assert!(!artifacts.success.exists());
        write_success_marker(&artifacts.data, &artifacts.sha256).unwrap();
        assert_eq!(
            fs::read_to_string(&artifacts.success).unwrap(),
            format!("{}\n", artifacts.sha256)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_preserves_legacy_and_current_schema_identity() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let start_ns = 1_700_000_000_000_000_000;
        for (name, row, schema, replay_scope) in [
            (
                "legacy",
                json!({"received_at_ns":start_ns,"type":"diff"}),
                LEGACY_LOB_TAPE_SCHEMA,
                "captured_snapshot_seed_plus_sequence_checked_diffs",
            ),
            (
                "current",
                json!({
                    "schema":RAW_SCHEMA,
                    "received_at_ns":start_ns,
                    "type":"agg_trade",
                    "session_id":"session-1",
                    "frame":{
                        "stream":"btcusdt@aggTrade",
                        "data":{
                            "e":"aggTrade",
                            "E":start_ns / 1_000_000,
                            "s":"BTCUSDT",
                            "a":10,
                            "p":"100.5",
                            "q":"2",
                            "f":10,
                            "l":10,
                            "T":start_ns / 1_000_000,
                            "m":false
                        }
                    }
                }),
                RAW_SCHEMA,
                "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs",
            ),
            (
                "v1",
                json!({
                    "schema":data::binance_market_tape::MARKET_TAPE_SCHEMA,
                    "received_at_ns":start_ns,
                    "type":"agg_trade",
                    "session_id":"session-1",
                    "frame":{
                        "stream":"btcusdt@aggTrade",
                        "data":{
                            "e":"aggTrade",
                            "E":start_ns / 1_000_000,
                            "s":"BTCUSDT",
                            "a":10,
                            "p":"100.5",
                            "q":"2",
                            "f":10,
                            "l":10,
                            "T":start_ns / 1_000_000,
                            "m":false
                        }
                    }
                }),
                data::binance_market_tape::MARKET_TAPE_SCHEMA,
                "captured_aggregate_trades_plus_snapshot_seed_plus_sequence_checked_diffs",
            ),
        ] {
            let root = tempfile::Builder::new()
                .prefix(&format!("monday-recovery-{name}-"))
                .tempdir()
                .unwrap();
            let config = recovery_config(root.path().to_owned());
            write_recovery_part(&config, start_ns, &[row]);

            let artifacts = recover_parts(&config).unwrap();
            let manifest: Value =
                serde_json::from_reader(File::open(&artifacts[0].manifest).unwrap()).unwrap();
            assert_eq!(manifest["schema"], schema, "case={name}");
            assert_eq!(manifest["replay_scope"], replay_scope, "case={name}");
            if market_tape_schema(schema) {
                assert_eq!(manifest["trade_representation"], "aggregate_trade_only");
                assert_eq!(
                    manifest["price_surface_derivation"],
                    "latest aggregate trade price"
                );
                assert_eq!(
                    manifest["trade_summaries"]["BTCUSDT"]["aggregate_trade_count"],
                    1
                );
            } else {
                assert!(manifest.get("trade_representation").is_none());
                assert!(manifest.get("price_surface_derivation").is_none());
            }
            if schema == RAW_SCHEMA {
                assert_eq!(
                    manifest["stream_types"],
                    json!(["depth@100ms", "aggTrade", "trade", "bookTicker"]),
                    "case={name}"
                );
            } else {
                assert!(manifest.get("stream_types").is_none(), "case={name}");
            }
        }
    }

    #[test]
    fn recovery_quarantines_mixed_or_schema_incompatible_parts() {
        let start_ns = 1_700_000_000_000_000_000;
        let cases = [
            (
                "mixed",
                vec![
                    json!({"schema":RAW_SCHEMA,"received_at_ns":start_ns,"type":"diff"}),
                    json!({"received_at_ns":start_ns + 1,"type":"diff"}),
                ],
            ),
            (
                "unknown",
                vec![
                    json!({"schema":"binance.market_tape.v999","received_at_ns":start_ns,"type":"diff"}),
                ],
            ),
            (
                "incompatible",
                vec![
                    json!({"schema":LEGACY_LOB_TAPE_SCHEMA,"received_at_ns":start_ns,"type":"agg_trade"}),
                ],
            ),
            (
                "aggregate_gap",
                vec![
                    json!({
                        "schema":RAW_SCHEMA,"received_at_ns":start_ns,"type":"agg_trade",
                        "session_id":"session-1","frame":{"stream":"btcusdt@aggTrade","data":{
                            "e":"aggTrade","E":start_ns / 1_000_000,"s":"BTCUSDT","a":10,
                            "p":"100","q":"1","f":10,"l":10,"T":start_ns / 1_000_000,"m":false
                        }}
                    }),
                    json!({
                        "schema":RAW_SCHEMA,"received_at_ns":start_ns + 1_000_000,"type":"agg_trade",
                        "session_id":"session-1","frame":{"stream":"btcusdt@aggTrade","data":{
                            "e":"aggTrade","E":start_ns / 1_000_000 + 1,"s":"BTCUSDT","a":12,
                            "p":"101","q":"1","f":12,"l":12,"T":start_ns / 1_000_000 + 1,"m":true
                        }}
                    }),
                ],
            ),
        ];

        for (name, rows) in cases {
            let root = tempfile::Builder::new()
                .prefix(&format!("monday-recovery-{name}-"))
                .tempdir()
                .unwrap();
            let config = recovery_config(root.path().to_owned());
            let path = write_recovery_part(&config, start_ns, &rows);

            assert!(recover_parts(&config).unwrap().is_empty(), "case={name}");
            assert!(!path.exists(), "case={name}");
            assert!(path.with_extension("part.corrupt").exists(), "case={name}");
            assert!(
                files_with_suffix(root.path(), ".manifest.json")
                    .unwrap()
                    .is_empty(),
                "case={name}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn success_marker_atomic_write_does_not_follow_predictable_temp_symlink() {
        use std::os::unix::fs::symlink;

        let root = tempfile::Builder::new()
            .prefix("monday-marker-test-")
            .tempdir()
            .unwrap();
        let root = root.path();
        let data = root.join("segment.ndjson.zst");
        fs::write(&data, b"compressed").unwrap();
        let victim = root.join("victim");
        fs::write(&victim, b"do-not-touch\n").unwrap();
        symlink(&victim, root.join("segment.ndjson.zst._SUCCESS.tmp")).unwrap();

        let marker = write_success_marker(&data, "abcd").unwrap();
        assert_eq!(fs::read_to_string(marker).unwrap(), "abcd\n");
        assert_eq!(fs::read(&victim).unwrap(), b"do-not-touch\n");
    }
}
