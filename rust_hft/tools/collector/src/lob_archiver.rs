use engine::binance_md::{parse_fixed_6, BookSync, SequenceDecision, UpdateMeta};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

pub const RAW_SCHEMA: &str = "binance.lob_tape.v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Market {
    Spot,
    Usdm,
}

impl FromStr for Market {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "spot" => Ok(Self::Spot),
            "usdm" => Ok(Self::Usdm),
            other => Err(format!("unsupported MARKET={other}")),
        }
    }
}

impl Market {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }
}

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
    bids: HashMap<String, String>,
    asks: HashMap<String, String>,
    sync: BookSync,
    snapshot_installed: bool,
    pub synced: bool,
    pub bridged: bool,
    pending: Vec<DepthDiff>,
}

impl OrderBookState {
    pub fn new(symbol: impl Into<String>, market: Market) -> Self {
        let mut sync = BookSync::new();
        sync.start_buffering();
        Self {
            symbol: symbol.into().to_ascii_uppercase(),
            market,
            bids: HashMap::new(),
            asks: HashMap::new(),
            sync,
            snapshot_installed: false,
            synced: false,
            bridged: false,
            pending: Vec::new(),
        }
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
            bids: sorted_levels(&self.bids, true)?,
            asks: sorted_levels(&self.asks, false)?,
        })
    }
}

fn parse_snapshot_side(value: Option<&Value>) -> anyhow::Result<HashMap<String, String>> {
    let levels: Vec<[String; 2]> = serde_json::from_value(value.cloned().unwrap_or(json!([])))?;
    validate_levels(&levels)?;
    Ok(levels.into_iter().map(|[p, q]| (p, q)).collect())
}

fn update_side(side: &mut HashMap<String, String>, levels: &[[String; 2]]) {
    for [price, quantity] in levels {
        if quantity
            .parse::<Decimal>()
            .is_ok_and(|value| value.is_zero())
        {
            side.remove(price);
        } else {
            side.insert(price.clone(), quantity.clone());
        }
    }
}

fn sorted_levels(
    side: &HashMap<String, String>,
    descending: bool,
) -> anyhow::Result<Vec<[String; 2]>> {
    let mut levels = side
        .iter()
        .map(|(price, quantity)| Ok((Decimal::from_str(price)?, [price.clone(), quantity.clone()])))
        .collect::<anyhow::Result<Vec<_>>>()?;
    levels.sort_by(|left, right| {
        if descending {
            right.0.cmp(&left.0)
        } else {
            left.0.cmp(&right.0)
        }
    });
    Ok(levels.into_iter().map(|(_, level)| level).collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub session_id: String,
    pub symbol: String,
    pub last_update_id: u64,
    pub synced: bool,
    pub bridged: bool,
    pub bids: Vec<[String; 2]>,
    pub asks: Vec<[String; 2]>,
}

pub fn checkpoint_event(checkpoint: Checkpoint, reason: &str) -> Value {
    let mut value = serde_json::to_value(checkpoint).expect("Checkpoint is always serializable");
    let object = value
        .as_object_mut()
        .expect("Checkpoint serializes as object");
    object.insert("reason".to_owned(), reason.into());
    object.insert("replay_safe".to_owned(), true.into());
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
}

pub struct Segment {
    config: SegmentConfig,
    pub start_ns: u64,
    pub end_ns: u64,
    path: PathBuf,
    writer: BufWriter<File>,
    counts: BTreeMap<String, u64>,
    replay_safe: bool,
    snapshot_ready_symbols: BTreeSet<String>,
    bridged_symbols: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadinessSummary {
    snapshot_ready_count: usize,
    bridged_count: usize,
    snapshot_only_symbols: Vec<String>,
    all_symbols_bridged: bool,
}

fn readiness_summary<'a>(
    symbol_count: usize,
    states: impl Iterator<Item = (&'a str, bool, bool)>,
) -> ReadinessSummary {
    let mut snapshot_ready_count = 0;
    let mut bridged_count = 0;
    let mut snapshot_only_symbols = Vec::new();
    for (symbol, synced, bridged) in states {
        snapshot_ready_count += usize::from(synced);
        bridged_count += usize::from(synced && bridged);
        if synced && !bridged {
            snapshot_only_symbols.push(symbol.to_owned());
        }
    }
    snapshot_only_symbols.sort();
    ReadinessSummary {
        snapshot_ready_count,
        bridged_count,
        snapshot_only_symbols,
        all_symbols_bridged: symbol_count > 0 && bridged_count == symbol_count,
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
            end_ns: start_ns,
            path,
            writer: BufWriter::with_capacity(1024 * 1024, file),
            counts: BTreeMap::new(),
            replay_safe: true,
            snapshot_ready_symbols: BTreeSet::new(),
            bridged_symbols: BTreeSet::new(),
        })
    }

    pub fn write(
        &mut self,
        event_type: &str,
        payload: Value,
        received_at_ns: u64,
    ) -> anyhow::Result<()> {
        self.end_ns = self.end_ns.max(received_at_ns);
        let mut envelope = serde_json::Map::new();
        envelope.insert("received_at_ns".to_owned(), received_at_ns.into());
        envelope.insert("type".to_owned(), event_type.into());
        let payload = payload
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("archive payload must be an object"))?;
        if event_type == "checkpoint" {
            if let Some(symbol) = payload.get("symbol").and_then(Value::as_str) {
                if payload.get("synced").and_then(Value::as_bool) == Some(true) {
                    self.snapshot_ready_symbols.insert(symbol.to_owned());
                }
                if payload.get("bridged").and_then(Value::as_bool) == Some(true) {
                    self.bridged_symbols.insert(symbol.to_owned());
                }
            }
        }
        envelope.extend(payload.clone());
        serde_json::to_writer(&mut self.writer, &envelope)?;
        self.writer.write_all(b"\n")?;
        *self.counts.entry(event_type.to_owned()).or_default() += 1;
        Ok(())
    }

    pub fn mark_replay_unsafe(&mut self) {
        self.replay_safe = false;
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
                )
            }),
        );
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        if self.path.metadata()?.len() == 0 {
            fs::remove_file(self.path)?;
            return Ok(None);
        }
        finalize_segment(
            &self.config,
            &self.path,
            self.counts,
            self.start_ns,
            self.end_ns,
            has_replay_safe_checkpoint,
            readiness,
        )
        .map(Some)
    }
}

fn finalize_segment(
    config: &SegmentConfig,
    path: &Path,
    counts: BTreeMap<String, u64>,
    start_ns: u64,
    end_ns: u64,
    has_replay_safe_checkpoint: bool,
    readiness: ReadinessSummary,
) -> anyhow::Result<SegmentArtifacts> {
    let data = path.with_file_name(format!("part-{start_ns}.jsonl.zst"));
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
    let (date, hour) = segment_partition(start_ns)?;
    let events: u64 = counts.values().sum();
    let metadata = json!({
        "schema": RAW_SCHEMA,
        "venue": "binance",
        "market": config.market.as_str(),
        "dataset": config.dataset,
        "shard_id": config.shard_id,
        "mode": "diff",
        "symbols": config.symbols,
        "security_token_symbols": config.security_token_symbols,
        "excluded_symbols": config.excluded_symbols,
        "snapshot_limit": config.snapshot_limit,
        "replay_scope": "captured_snapshot_seed_plus_sequence_checked_diffs",
        "venue_depth_complete": false,
        "events": events,
        "event_types": counts,
        "has_replay_safe_checkpoint": has_replay_safe_checkpoint,
        "snapshot_ready_count": readiness.snapshot_ready_count,
        "bridged_count": readiness.bridged_count,
        "snapshot_only_symbols": readiness.snapshot_only_symbols,
        "all_symbols_bridged": readiness.all_symbols_bridged,
        "start_received_at_ns": start_ns,
        "end_received_at_ns": end_ns,
        "date": date,
        "hour": hour,
        "file": data.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
        "bytes": data.metadata()?.len(),
        "sha256": digest,
    });
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
        let mut start_ns = 0_u64;
        let mut end_ns = 0_u64;
        let mut offset = 0_usize;
        let mut invalid_at = None;
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
            let event_type = event["type"].as_str().unwrap_or("diff");
            *counts.entry(event_type.to_owned()).or_default() += 1;
            offset += line.len();
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
        artifacts.push(finalize_segment(
            config,
            &path,
            counts,
            start_ns,
            end_ns,
            false,
            readiness_summary(
                config.symbols.len(),
                config
                    .symbols
                    .iter()
                    .map(|symbol| (symbol.as_str(), false, false)),
            ),
        )?);
    }
    Ok(artifacts)
}

pub fn files_with_suffix(root: &Path, suffix: &str) -> anyhow::Result<Vec<PathBuf>> {
    fn visit(path: &Path, suffix: &str, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, suffix, files)?;
            } else if path.to_string_lossy().ends_with(suffix) {
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
                    "last_update_id": state.last_update_id(),
                    "bid_levels": state.bid_levels(),
                    "ask_levels": state.ask_levels(),
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let readiness = readiness_summary(
        states.len(),
        states
            .iter()
            .map(|(symbol, state)| (symbol.as_str(), state.synced, state.bridged)),
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
            "snapshot_only_symbols": readiness.snapshot_only_symbols,
            "all_symbols_bridged": readiness.all_symbols_bridged,
            "session_id": session_id,
            "sequence_gaps": sequence_gaps,
            "pending_upload_segments": pending_upload_segments,
            "disk_free_gb": disk_free_gb,
            "disk_warning": disk_free_gb.is_some_and(|free| free < disk_warning_threshold_gb),
            "disk_warning_threshold_gb": disk_warning_threshold_gb,
            "upload_warning": upload.last_error_at.is_some(),
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
pub struct UploadStatus {
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
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

/// Lossless backpressure that remains cancellable when the bounded queue is full.
pub async fn send_or_shutdown<T>(
    sender: &mpsc::Sender<T>,
    item: T,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<SendOutcome<T>> {
    if *shutdown.borrow() {
        return Ok(SendOutcome::Shutdown(item));
    }
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
    let temporary = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
    ));
    let mut output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    fs::rename(temporary, path)?;
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
        update.bids = vec![["100.00000000".into(), "0.00000000".into()]];
        update.asks = vec![["102.12345678".into(), "1.00000001".into()]];
        state.apply_diff(update, &mut budget).unwrap();
        let checkpoint = state.checkpoint("session-1").unwrap();
        assert!(checkpoint.bids.is_empty());
        assert_eq!(checkpoint.asks[1], ["102.12345678", "1.00000001"]);
        let event = checkpoint_event(checkpoint, "scheduled");
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
        assert_eq!(receiver.recv().await, Some(1));
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
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
        assert_eq!(health["last_upload_error"], "oss down");
        assert_eq!(health["snapshot_ready_count"], 0);
        assert_eq!(health["bridged_count"], 0);
        assert_eq!(health["snapshot_only_symbols"], json!([]));
        assert_eq!(health["all_symbols_bridged"], false);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn segment_emits_v2_manifest_digest_and_success_marker() {
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
        };
        let mut segment = Segment::create(config, 1_700_000_000_000_000_000).unwrap();
        segment
            .write("snapshot", json!({"symbol":"BTCUSDT"}), segment.start_ns)
            .unwrap();
        segment
            .write(
                "checkpoint",
                json!({"symbol":"BTCUSDT", "synced":true, "bridged":true}),
                segment.start_ns + 1,
            )
            .unwrap();
        let artifacts = segment.close().unwrap().unwrap();
        let manifest: Value =
            serde_json::from_reader(File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["schema"], RAW_SCHEMA);
        assert_eq!(
            manifest["event_types"],
            json!({"checkpoint":1,"snapshot":1})
        );
        assert_eq!(manifest["has_replay_safe_checkpoint"], true);
        assert_eq!(manifest["snapshot_ready_count"], 1);
        assert_eq!(manifest["bridged_count"], 1);
        assert_eq!(manifest["snapshot_only_symbols"], json!([]));
        assert_eq!(manifest["all_symbols_bridged"], true);
        assert_eq!(manifest["sha256"], artifacts.sha256);
        assert!(!artifacts.success.exists());
        write_success_marker(&artifacts.data, &artifacts.sha256).unwrap();
        assert_eq!(
            fs::read_to_string(&artifacts.success).unwrap(),
            format!("{}\n", artifacts.sha256)
        );
        fs::remove_dir_all(root).unwrap();
    }
}
