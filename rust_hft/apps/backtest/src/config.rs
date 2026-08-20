use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context};
use data::binance_lob_replay::{
    source_revision, Market, ReplaySequenceEvent, ReplaySequenceValidator,
};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use parquet::schema::parser::parse_message_type;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

use crate::{
    engine::{
        TargetPositionDecision, TargetPositionReplay, TargetPositionReplayConfig,
        TargetPositionReplayMetrics,
    },
    event::EventEnvelope,
};

const CANONICAL_PARQUET_SCHEMA: &str =
    "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8";
const CANONICAL_PARQUET_MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacktestConfig {
    pub data: DataConfig,
    pub strategy: StrategyConfig,
    #[serde(default)]
    pub execution: ExecutionConfig,
    #[serde(default)]
    pub risk: RiskConfig,
    #[serde(default)]
    pub output: OutputConfig,
}

impl BacktestConfig {
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let yaml = fs::read_to_string(&path)
            .with_context(|| format!("無法讀取配置檔: {}", path.as_ref().display()))?;
        Self::from_yaml_str(&yaml, &path.as_ref().display().to_string())
    }

    pub fn from_yaml_str(yaml: &str, source: &str) -> anyhow::Result<Self> {
        let mut cfg: BacktestConfig =
            serde_yaml::from_str(yaml).with_context(|| format!("解析配置檔失敗: {source}"))?;
        cfg.normalize();
        Ok(cfg)
    }

    fn normalize(&mut self) {
        // clamp 合理值
        if self.strategy.price_delta_ticks < 1.0 {
            self.strategy.price_delta_ticks = 1.0;
        }
        if self.strategy.liquidity_window_secs < 1.0 {
            self.strategy.liquidity_window_secs = 60.0;
        }
        if self.strategy.breakout_window_secs < 0.2 {
            self.strategy.breakout_window_secs = 0.2;
        }
        if self.execution.base_qty <= 0.0 {
            self.execution.base_qty = self.data.lot_size.max(0.001);
        }
        if self.execution.max_position <= 0.0 {
            self.execution.max_position = self.execution.base_qty * 5.0;
        }
        if self.execution.stop_loss_ticks <= 0.0 {
            self.execution.stop_loss_ticks = 8.0;
        }
        if self.execution.take_profit_ticks <= 0.0 {
            self.execution.take_profit_ticks = 8.0;
        }
        if self.risk.inventory_limit <= 0.0 {
            self.risk.inventory_limit = self.execution.max_position * 2.0;
        }
        if self.strategy.support_count == 0 {
            self.strategy.support_count = 3;
        }
        if self.strategy.resistance_count == 0 {
            self.strategy.resistance_count = 3;
        }
    }

    pub fn validate_data_artifact(&self) -> anyhow::Result<VerifiedBacktestData> {
        if self.data.format.eq_ignore_ascii_case("parquet") {
            return self.validate_parquet_artifact();
        }
        if !self.data.format.eq_ignore_ascii_case("ndjson") {
            bail!("unsupported backtest data format: {}", self.data.format);
        }
        if !self.data.require_sequence {
            bail!("backtests require data.require_sequence=true");
        }
        let manifest_path = self
            .data
            .manifest_path
            .as_deref()
            .context("backtests require data.manifest_path")?;
        let expected_manifest_sha256 = valid_sha256(
            self.data
                .manifest_sha256
                .as_deref()
                .context("backtests require data.manifest_sha256")?,
            "data.manifest_sha256",
        )?;
        let manifest_bytes = fs::read(resolve_path(manifest_path))
            .with_context(|| format!("无法读取回测数据 manifest: {manifest_path}"))?;
        let actual_manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
        if actual_manifest_sha256 != expected_manifest_sha256 {
            bail!(
                "backtest manifest SHA-256 mismatch: expected {expected_manifest_sha256}, actual {actual_manifest_sha256}"
            );
        }
        let manifest: BacktestDataManifest =
            serde_json::from_slice(&manifest_bytes).context("无法解析回测数据 manifest")?;
        manifest.validate()?;
        self.validate_modalities()?;

        let artifact_path = resolve_path(&self.data.path);
        let bytes = fs::read(&artifact_path)
            .with_context(|| format!("无法读取回测数据: {}", self.data.path))?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != manifest.artifact_sha256 {
            bail!(
                "backtest data SHA-256 mismatch: expected {}, actual {actual}",
                manifest.artifact_sha256
            );
        }
        if canonical_path(&self.data.path)? != canonical_path(&manifest.artifact_path)? {
            bail!("data.path does not match manifest artifact_path");
        }
        validate_source_segments(&manifest, &bytes)?;
        validate_event_tape(&bytes, &manifest)?;
        self.validate_execution_model()?;
        Ok(VerifiedBacktestData {
            bytes,
            evidence: BacktestInputEvidence {
                manifest_sha256: actual_manifest_sha256,
                config_sha256: hex::encode(Sha256::digest(serde_json::to_vec(self)?)),
                source_revision: manifest.source_revision,
                replay_rows: manifest.rows,
            },
        })
    }

    fn validate_parquet_artifact(&self) -> anyhow::Result<VerifiedBacktestData> {
        if !self.data.require_sequence {
            bail!("backtests require data.require_sequence=true");
        }
        if self
            .data
            .start_ts
            .zip(self.data.end_ts)
            .is_some_and(|(start, end)| start > end)
        {
            bail!("backtest time window start_ts must not exceed end_ts");
        }
        let manifest_path = self
            .data
            .manifest_path
            .as_deref()
            .context("backtests require data.manifest_path")?;
        let expected_manifest_sha256 = valid_sha256(
            self.data
                .manifest_sha256
                .as_deref()
                .context("backtests require data.manifest_sha256")?,
            "data.manifest_sha256",
        )?;
        self.validate_modalities()?;
        let artifact_path = resolve_path(&self.data.path);
        let verified = verify_canonical_replay_artifact(
            &artifact_path,
            &resolve_path(manifest_path),
            None,
            expected_manifest_sha256,
            self.data.start_ts,
            self.data.end_ts,
        )?;
        self.validate_execution_model()?;
        Ok(VerifiedBacktestData {
            bytes: verified.bytes,
            evidence: BacktestInputEvidence {
                manifest_sha256: verified.evidence.manifest_sha256,
                config_sha256: hex::encode(Sha256::digest(serde_json::to_vec(self)?)),
                source_revision: verified.evidence.source_revision,
                replay_rows: verified.evidence.replay_rows,
            },
        })
    }

    fn validate_modalities(&self) -> anyhow::Result<()> {
        if !self.strategy.volume_factor.is_finite()
            || self.strategy.volume_factor < 0.0
            || !self.strategy.cvd_threshold.is_finite()
        {
            bail!("strategy volume factor must be finite/non-negative and CVD threshold finite");
        }
        if self.strategy.volume_factor > 0.0 || self.strategy.cvd_threshold != 0.0 {
            bail!("strategy requires trade flow but dataset manifest is LOB-only");
        }
        Ok(())
    }

    fn validate_execution_model(&self) -> anyhow::Result<()> {
        if !self.execution.max_slippage_ticks.is_finite()
            || self.execution.max_slippage_ticks < 0.0
            || !self.risk.slippage_limit_ticks.is_finite()
            || self.risk.slippage_limit_ticks < 0.0
            || self.execution.max_slippage_ticks > self.risk.slippage_limit_ticks
        {
            bail!(
                "execution.max_slippage_ticks must be finite/non-negative and within risk.slippage_limit_ticks"
            );
        }
        if !self.execution.fee_bps.is_finite() || self.execution.fee_bps < 0.0 {
            bail!("execution.fee_bps must be finite and non-negative");
        }
        if !self.execution.max_fill_ratio.is_finite()
            || !(0.0..=1.0).contains(&self.execution.max_fill_ratio)
            || self.execution.max_fill_ratio == 0.0
        {
            bail!("execution.max_fill_ratio must be in (0, 1]");
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct VerifiedBacktestData {
    pub bytes: Vec<u8>,
    pub evidence: BacktestInputEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestInputEvidence {
    pub manifest_sha256: String,
    pub config_sha256: String,
    pub source_revision: String,
    pub replay_rows: usize,
}

#[derive(Debug)]
pub struct VerifiedCanonicalReplay {
    pub bytes: Vec<u8>,
    pub evidence: CanonicalReplayEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalReplayEvidence {
    pub manifest_sha256: String,
    pub artifact_sha256: String,
    pub mission_id: String,
    pub market: String,
    pub symbol: String,
    pub dataset: String,
    pub modalities: Vec<String>,
    pub source_revision: String,
    pub source_segments: Vec<CanonicalSourceSegmentEvidence>,
    pub rows: usize,
    pub replay_rows: usize,
    pub first_event_time_us: i64,
    pub last_event_time_us: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DataConfig {
    pub path: String,
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "default_tick_size")]
    pub tick_size: f64,
    #[serde(default = "default_lot_size")]
    pub lot_size: f64,
    #[serde(default = "default_depth_levels")]
    pub max_depth_levels: usize,
    #[serde(default)]
    pub manifest_path: Option<String>,
    #[serde(default)]
    pub manifest_sha256: Option<String>,
    #[serde(default)]
    pub require_sequence: bool,
    #[serde(default)]
    pub start_ts: Option<i64>,
    #[serde(default)]
    pub end_ts: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct BacktestDataManifest {
    dataset_kind: String,
    schema_version: String,
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
    artifact_path: String,
    artifact_sha256: String,
    point_in_time: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CanonicalParquetManifest {
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
    source_segments: Vec<CanonicalSourceSegmentEvidence>,
    rows: usize,
    first_event_time_us: i64,
    last_event_time_us: i64,
    sequence_start: u64,
    sequence_end: u64,
    artifact_path: std::path::PathBuf,
    artifact_sha256: String,
    point_in_time: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CanonicalSourceSegmentEvidence {
    pub file: String,
    pub sha256: String,
    pub collector_manifest_sha256: String,
    pub success_marker_sha256: String,
    pub start_received_at_ns: u64,
    pub end_received_at_ns: u64,
    pub events: u64,
}

impl CanonicalParquetManifest {
    fn validate(&self) -> anyhow::Result<()> {
        if self.dataset_kind != "backtest_canonical_replay_parquet"
            || self.schema_version != "binance-replay-parquet-v1"
            || self.format != "parquet"
            || self.parquet_schema != CANONICAL_PARQUET_SCHEMA
            || self.mission_id.trim().is_empty()
            || self.market.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.dataset.trim().is_empty()
            || self.modalities != vec!["lob".to_string()]
            || !self.point_in_time
            || self.rows == 0
            || self.source_segments.is_empty()
            || self.first_event_time_us > self.last_event_time_us
            || self.sequence_start != 1
            || self.sequence_end < self.sequence_start
            || self.sequence_end - self.sequence_start + 1 != self.rows as u64
            || self.artifact_path.is_absolute()
            || self
                .artifact_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            bail!("canonical Parquet manifest is incomplete or unsupported");
        }
        valid_sha256(&self.artifact_sha256, "manifest.artifact_sha256")?;
        valid_sha256(&self.source_revision, "manifest.source_revision")?;
        if self.artifact_path != std::path::Path::new(&format!("{}.parquet", self.artifact_sha256))
        {
            bail!("canonical Parquet artifact path is not content addressed");
        }
        let mut source_hashes = Vec::with_capacity(self.source_segments.len());
        let mut unique = HashSet::new();
        for segment in &self.source_segments {
            if segment.file.trim().is_empty()
                || segment.events == 0
                || segment.start_received_at_ns > segment.end_received_at_ns
                || !unique.insert(&segment.sha256)
            {
                bail!("canonical source segment evidence is incomplete or duplicated");
            }
            valid_sha256(&segment.sha256, "source segment sha256")?;
            valid_sha256(
                &segment.collector_manifest_sha256,
                "source collector manifest sha256",
            )?;
            valid_sha256(
                &segment.success_marker_sha256,
                "source success marker sha256",
            )?;
            if segment.success_marker_sha256
                != hex::encode(Sha256::digest(format!("{}\n", segment.sha256)))
            {
                bail!("canonical source success marker does not bind its segment");
            }
            source_hashes.push(segment.sha256.as_str());
        }
        if source_revision(source_hashes) != self.source_revision {
            bail!("canonical source revision does not match source segments");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct SourceSegmentEvidence {
    path: String,
    sha256: String,
    collector_manifest_path: String,
    collector_manifest_sha256: String,
    success_marker_path: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Deserialize)]
struct CollectorRawManifest {
    schema: String,
    venue: String,
    market: String,
    dataset: String,
    symbols: Vec<String>,
    mode: String,
    replay_scope: String,
    events: u64,
    bytes: u64,
    event_types: std::collections::HashMap<String, u64>,
    has_replay_safe_checkpoint: bool,
    all_symbols_bridged: bool,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    file: String,
    sha256: String,
}

impl BacktestDataManifest {
    fn validate(&self) -> anyhow::Result<()> {
        if self.dataset_kind != "backtest_point_in_time_event_tape"
            || self.schema_version != "backtest-pit-v1"
            || self.mission_id.trim().is_empty()
            || self.market.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.dataset.trim().is_empty()
            || self.modalities.len() != 1
            || self.modalities[0] != "lob"
            || !self.point_in_time
            || self.rows == 0
            || self.source_segments.is_empty()
        {
            bail!("backtest manifest is not a complete point-in-time event tape");
        }
        valid_sha256(&self.artifact_sha256, "manifest.artifact_sha256")?;
        valid_sha256(&self.source_revision, "manifest.source_revision")?;
        if self.first_event_time_us > self.last_event_time_us
            || self.sequence_start != 1
            || self.sequence_end < self.sequence_start
            || self.sequence_end - self.sequence_start + 1 != self.rows as u64
        {
            bail!("backtest manifest time/sequence coverage is inconsistent");
        }
        Ok(())
    }
}

fn validate_source_segments(
    manifest: &BacktestDataManifest,
    artifact_bytes: &[u8],
) -> anyhow::Result<()> {
    let mut hashes = Vec::with_capacity(manifest.source_segments.len());
    let mut unique = HashSet::new();
    let mut previous_segment_end = None;
    let market = manifest
        .market
        .parse::<Market>()
        .map_err(anyhow::Error::msg)?;
    let mut replay = ReplaySequenceValidator::new(market, &manifest.symbol)?;
    let artifact_rows = parse_ndjson_values(artifact_bytes, "backtest event tape")?;
    let mut materialized_rows = Vec::with_capacity(artifact_rows.len());
    for segment in &manifest.source_segments {
        let expected = valid_sha256(&segment.sha256, "source segment sha256")?;
        if !unique.insert(expected.to_string())
            || segment.events == 0
            || segment.start_received_at_ns > segment.end_received_at_ns
            || previous_segment_end.is_some_and(|previous| segment.start_received_at_ns < previous)
        {
            bail!("backtest source segment evidence is incomplete or duplicated");
        }
        previous_segment_end = Some(segment.end_received_at_ns);
        let source_path = resolve_path(&segment.path);
        let (actual, source, source_bytes) = open_hashed_source(&source_path)
            .with_context(|| format!("无法读取源数据 segment: {}", segment.path))?;
        if actual != expected {
            bail!(
                "source segment SHA-256 mismatch for {}: expected {expected}, actual {actual}",
                segment.path
            );
        }
        if canonical_path(&segment.path)? == canonical_path(&manifest.artifact_path)? {
            bail!("collector raw segment cannot also be the backtest artifact");
        }

        let expected_collector_manifest_sha = valid_sha256(
            &segment.collector_manifest_sha256,
            "source collector manifest sha256",
        )?;
        let collector_manifest_bytes = fs::read(resolve_path(&segment.collector_manifest_path))
            .with_context(|| {
                format!(
                    "无法读取 collector manifest: {}",
                    segment.collector_manifest_path
                )
            })?;
        let actual_collector_manifest_sha = hex::encode(Sha256::digest(&collector_manifest_bytes));
        if actual_collector_manifest_sha != expected_collector_manifest_sha {
            bail!(
                "collector manifest SHA-256 mismatch: expected {expected_collector_manifest_sha}, actual {actual_collector_manifest_sha}"
            );
        }
        let collector: CollectorRawManifest = serde_json::from_slice(&collector_manifest_bytes)
            .context("无法解析 collector raw manifest")?;
        validate_collector_manifest(manifest, segment, &collector, expected, source_bytes)?;

        let success =
            fs::read_to_string(resolve_path(&segment.success_marker_path)).with_context(|| {
                format!(
                    "无法读取 collector success marker: {}",
                    segment.success_marker_path
                )
            })?;
        if success.trim() != expected {
            bail!("collector success marker is not bound to the raw segment digest");
        }

        let mut raw_count = 0_u64;
        let mut previous_received_at = None;
        let mut observed_types = std::collections::HashMap::<String, u64>::new();
        visit_collector_rows(&segment.path, source, |raw| {
            raw_count = raw_count.checked_add(1).context("collector row overflow")?;
            let mut raw = raw
                .as_object()
                .cloned()
                .context("collector raw row must be a JSON object")?;
            let received_at_ns = raw
                .remove("received_at_ns")
                .and_then(|value| value.as_u64())
                .context("collector raw row is missing received_at_ns")?;
            if received_at_ns < segment.start_received_at_ns
                || received_at_ns > segment.end_received_at_ns
                || previous_received_at.is_some_and(|previous| received_at_ns < previous)
            {
                bail!("collector raw rows are not point-in-time ordered");
            }
            previous_received_at = Some(received_at_ns);
            let event_type = raw
                .remove("type")
                .and_then(|value| value.as_str().map(str::to_owned))
                .context("collector raw row is missing type")?;
            *observed_types.entry(event_type.clone()).or_default() += 1;
            validate_collector_event_type(&event_type)?;
            let replay_events = replay.observe(&event_type, &raw, received_at_ns)?;
            if event_type == "checkpoint"
                && (raw.get("replay_safe").and_then(serde_json::Value::as_bool) != Some(true)
                    || raw.get("synced").and_then(serde_json::Value::as_bool) != Some(true)
                    || raw.get("bridged").and_then(serde_json::Value::as_bool) != Some(true)
                    || raw
                        .get("symbol")
                        .and_then(serde_json::Value::as_str)
                        .map(str::is_empty)
                        .unwrap_or(true))
            {
                bail!("collector replay checkpoint is incomplete");
            }
            for event in replay_events {
                materialized_rows.push(materialize_replay_event(
                    event,
                    materialized_rows.len() as u64 + 1,
                )?);
            }
            Ok(())
        })?;
        if raw_count != segment.events {
            bail!("collector raw row count does not match source evidence");
        }
        if observed_types != collector.event_types {
            bail!("collector raw event types do not match its manifest");
        }
        hashes.push(expected.to_string());
    }
    replay.finish()?;
    let actual_revision = source_revision(hashes.iter().map(String::as_str));
    if actual_revision != manifest.source_revision {
        bail!(
            "source revision mismatch: expected {}, actual {actual_revision}",
            manifest.source_revision
        );
    }
    if materialized_rows != artifact_rows {
        bail!("backtest event tape is not the deterministic point-in-time materialization of its collector sources");
    }
    Ok(())
}

fn validate_collector_manifest(
    manifest: &BacktestDataManifest,
    segment: &SourceSegmentEvidence,
    collector: &CollectorRawManifest,
    expected_source_sha: &str,
    source_bytes: u64,
) -> anyhow::Result<()> {
    let source_file = Path::new(&segment.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if collector.schema != "binance.lob_tape.v2"
        || collector.venue != "binance"
        || collector.market != manifest.market
        || collector.dataset != manifest.dataset
        || !collector
            .symbols
            .iter()
            .any(|symbol| symbol == &manifest.symbol)
        || collector.mode != "diff"
        || collector.replay_scope != "captured_snapshot_seed_plus_sequence_checked_diffs"
        || !collector.has_replay_safe_checkpoint
        || !collector.all_symbols_bridged
        || collector.events != segment.events
        || collector.bytes != source_bytes
        || collector.start_received_at_ns != segment.start_received_at_ns
        || collector.end_received_at_ns != segment.end_received_at_ns
        || collector.file != source_file
        || collector.sha256 != expected_source_sha
        || collector.event_types.values().sum::<u64>() != collector.events
        || collector
            .event_types
            .get("checkpoint")
            .copied()
            .unwrap_or(0)
            == 0
        || collector
            .event_types
            .get("sequence_gap")
            .copied()
            .unwrap_or(0)
            != 0
    {
        bail!("collector raw manifest is incomplete or does not match source evidence");
    }
    Ok(())
}

fn materialize_replay_event(
    replay_event: ReplaySequenceEvent,
    sequence: u64,
) -> anyhow::Result<serde_json::Value> {
    let (event, received_at_ns, bids, asks) = match replay_event {
        ReplaySequenceEvent::Snapshot {
            received_at_ns,
            bids,
            asks,
        } => ("snapshot", received_at_ns, bids, asks),
        ReplaySequenceEvent::Diff {
            received_at_ns,
            bids,
            asks,
        } => ("l2_update", received_at_ns, bids, asks),
    };
    let received_at_us = received_at_ns / 1_000 + u64::from(!received_at_ns.is_multiple_of(1_000));
    let timestamp = i64::try_from(received_at_us).context("receive time exceeds i64")?;
    Ok(serde_json::json!({
        "timestamp": timestamp,
        "sequence": sequence,
        "event": event,
        "bids": normalize_replay_levels(bids, "bids")?,
        "asks": normalize_replay_levels(asks, "asks")?,
    }))
}

fn validate_collector_event_type(event_type: &str) -> anyhow::Result<()> {
    match event_type {
        "snapshot" | "diff" | "checkpoint" => Ok(()),
        "trade" => bail!("binance.lob_tape.v2 does not contain trade events"),
        unsupported => bail!("unsupported collector event type: {unsupported}"),
    }
}

fn normalize_replay_levels(levels: Vec<[String; 2]>, field: &str) -> anyhow::Result<Vec<[f64; 2]>> {
    levels
        .into_iter()
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

fn parse_ndjson_values(bytes: &[u8], label: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    std::str::from_utf8(bytes)
        .with_context(|| format!("{label} is not UTF-8"))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("invalid {label} row")))
        .collect()
}

fn open_hashed_source(path: &Path) -> anyhow::Result<(String, File, u64)> {
    let mut source = File::open(path)?;
    let bytes = source.metadata()?.len();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    source.seek(SeekFrom::Start(0))?;
    Ok((hex::encode(digest.finalize()), source, bytes))
}

fn visit_collector_rows(
    path: &str,
    source: File,
    mut visitor: impl FnMut(serde_json::Value) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if path.ends_with(".zst") {
        let mut child = Command::new("zstd")
            .args(["-q", "-dc"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start zstd for collector segment: {path}"))?;
        let mut stdin = child.stdin.take().context("zstd stdin unavailable")?;
        let stdout = child.stdout.take().context("zstd stdout unavailable")?;
        std::thread::scope(|scope| -> anyhow::Result<()> {
            let writer = scope.spawn(move || std::io::copy(&mut &source, &mut stdin));
            let visit_result = visit_ndjson(BufReader::new(stdout), &mut visitor);
            if visit_result.is_err() {
                let _ = child.kill();
            }
            let status = child.wait()?;
            writer
                .join()
                .map_err(|_| anyhow::anyhow!("zstd input writer panicked"))??;
            visit_result?;
            if !status.success() {
                bail!("zstd failed for collector segment {path}: {status}");
            }
            Ok(())
        })?;
    } else {
        visit_ndjson(BufReader::new(source), &mut visitor)?;
    }
    Ok(())
}

fn visit_ndjson(
    reader: impl BufRead,
    visitor: &mut impl FnMut(serde_json::Value) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    for line in reader.lines() {
        let line = line.context("failed to read collector raw segment")?;
        if line.trim().is_empty() {
            continue;
        }
        visitor(serde_json::from_str(&line).context("invalid collector raw segment row")?)?;
    }
    Ok(())
}

fn validate_event_tape(bytes: &[u8], manifest: &BacktestDataManifest) -> anyhow::Result<()> {
    let contents = std::str::from_utf8(bytes).context("event tape is not UTF-8")?;
    let mut rows = 0_usize;
    let mut first_time = None;
    let mut last_time = None;
    let mut first_sequence = None;
    let mut last_sequence = None;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line)?;
        let timestamp = value
            .get("timestamp")
            .and_then(serde_json::Value::as_i64)
            .context("event tape row is missing integer timestamp")?;
        let sequence = value
            .get("sequence")
            .and_then(serde_json::Value::as_u64)
            .context("event tape row is missing integer sequence")?;
        first_time.get_or_insert(timestamp);
        first_sequence.get_or_insert(sequence);
        last_time = Some(timestamp);
        last_sequence = Some(sequence);
        rows += 1;
    }
    if rows != manifest.rows
        || first_time != Some(manifest.first_event_time_us)
        || last_time != Some(manifest.last_event_time_us)
        || first_sequence != Some(manifest.sequence_start)
        || last_sequence != Some(manifest.sequence_end)
    {
        bail!("event tape coverage does not match its manifest");
    }
    Ok(())
}

pub fn verify_canonical_replay_artifact(
    artifact_path: &Path,
    manifest_path: &Path,
    expected_artifact_sha256: Option<&str>,
    expected_manifest_sha256: &str,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
) -> anyhow::Result<VerifiedCanonicalReplay> {
    let (manifest, manifest_sha256, artifact_sha256) = verify_canonical_manifest_and_artifact(
        artifact_path,
        manifest_path,
        expected_artifact_sha256,
        expected_manifest_sha256,
    )?;
    let (bytes, replay_rows) = read_canonical_parquet(artifact_path, &manifest, start_ts, end_ts)?;
    Ok(VerifiedCanonicalReplay {
        bytes,
        evidence: canonical_replay_evidence(
            manifest,
            manifest_sha256,
            artifact_sha256,
            replay_rows,
        ),
    })
}

pub fn verify_canonical_replay_artifact_streaming(
    artifact_path: &Path,
    manifest_path: &Path,
    expected_artifact_sha256: Option<&str>,
    expected_manifest_sha256: &str,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
) -> anyhow::Result<CanonicalReplayEvidence> {
    let (manifest, manifest_sha256, artifact_sha256) = verify_canonical_manifest_and_artifact(
        artifact_path,
        manifest_path,
        expected_artifact_sha256,
        expected_manifest_sha256,
    )?;
    let replay_rows =
        visit_canonical_parquet(artifact_path, &manifest, start_ts, end_ts, |_| Ok(()))?;
    Ok(canonical_replay_evidence(
        manifest,
        manifest_sha256,
        artifact_sha256,
        replay_rows,
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn verify_and_replay_canonical_target_positions(
    artifact_path: &Path,
    manifest_path: &Path,
    expected_artifact_sha256: &str,
    expected_manifest_sha256: &str,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
    decisions: &[TargetPositionDecision],
    config: &TargetPositionReplayConfig,
) -> anyhow::Result<(CanonicalReplayEvidence, TargetPositionReplayMetrics)> {
    let (manifest, manifest_sha256, artifact_sha256) = verify_canonical_manifest_and_artifact(
        artifact_path,
        manifest_path,
        Some(expected_artifact_sha256),
        expected_manifest_sha256,
    )?;
    let mut replay = TargetPositionReplay::new(decisions, config)?;
    let replay_rows =
        visit_canonical_parquet(artifact_path, &manifest, start_ts, end_ts, |event| {
            replay.observe(&event)
        })?;
    let metrics = replay.finish()?;
    Ok((
        canonical_replay_evidence(manifest, manifest_sha256, artifact_sha256, replay_rows),
        metrics,
    ))
}

fn verify_canonical_manifest_and_artifact(
    artifact_path: &Path,
    manifest_path: &Path,
    expected_artifact_sha256: Option<&str>,
    expected_manifest_sha256: &str,
) -> anyhow::Result<(CanonicalParquetManifest, String, String)> {
    let expected_manifest_sha256 = valid_sha256(
        expected_manifest_sha256,
        "canonical replay manifest SHA-256",
    )?;
    let manifest_bytes = fs::read(manifest_path).with_context(|| {
        format!(
            "cannot read canonical replay manifest: {}",
            manifest_path.display()
        )
    })?;
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    if manifest_sha256 != expected_manifest_sha256 {
        bail!(
            "backtest manifest SHA-256 mismatch: expected {expected_manifest_sha256}, actual {manifest_sha256}"
        );
    }
    let manifest: CanonicalParquetManifest = serde_json::from_slice(&manifest_bytes)
        .context("cannot parse canonical Parquet manifest")?;
    manifest.validate()?;
    if !artifact_path.is_file() {
        bail!(
            "cannot read local canonical Parquet: {}",
            artifact_path.display()
        );
    }
    if manifest.artifact_path.file_name() != artifact_path.file_name() {
        bail!("canonical replay path does not match manifest artifact_path");
    }
    let artifact_sha256 = sha256_file(artifact_path)?;
    if artifact_sha256 != manifest.artifact_sha256
        || expected_artifact_sha256
            .map(|expected| valid_sha256(expected, "canonical replay artifact SHA-256"))
            .transpose()?
            .is_some_and(|expected| expected != artifact_sha256)
    {
        bail!("canonical replay artifact SHA-256 mismatch");
    }
    Ok((manifest, manifest_sha256, artifact_sha256))
}

fn canonical_replay_evidence(
    manifest: CanonicalParquetManifest,
    manifest_sha256: String,
    artifact_sha256: String,
    replay_rows: usize,
) -> CanonicalReplayEvidence {
    CanonicalReplayEvidence {
        manifest_sha256,
        artifact_sha256,
        mission_id: manifest.mission_id,
        market: manifest.market,
        symbol: manifest.symbol,
        dataset: manifest.dataset,
        modalities: manifest.modalities,
        source_revision: manifest.source_revision,
        source_segments: manifest.source_segments,
        rows: manifest.rows,
        replay_rows,
        first_event_time_us: manifest.first_event_time_us,
        last_event_time_us: manifest.last_event_time_us,
    }
}

fn read_canonical_parquet(
    artifact_path: &Path,
    manifest: &CanonicalParquetManifest,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
) -> anyhow::Result<(Vec<u8>, usize)> {
    let mut output = Vec::new();
    let replay_rows =
        visit_canonical_parquet(artifact_path, manifest, start_ts, end_ts, |event| {
            serde_json::to_writer(&mut output, &event)?;
            output.push(b'\n');
            Ok(())
        })?;
    Ok((output, replay_rows))
}

fn visit_canonical_parquet(
    artifact_path: &Path,
    manifest: &CanonicalParquetManifest,
    start_ts: Option<i64>,
    end_ts: Option<i64>,
    mut visitor: impl FnMut(EventEnvelope) -> anyhow::Result<()>,
) -> anyhow::Result<usize> {
    let reader = SerializedFileReader::new(File::open(artifact_path)?)?;
    let required_schema = parse_message_type(CANONICAL_PARQUET_MESSAGE)?;
    let schema_descriptor = reader.metadata().file_metadata().schema_descr_ptr();
    let actual_schema = schema_descriptor.root_schema();
    if actual_schema != &required_schema {
        bail!("canonical Parquet schema does not match the manifest contract");
    }

    let mut rows = 0_usize;
    let mut replay_rows = 0_usize;
    let mut first_timestamp = None;
    let mut last_timestamp = None;
    let mut expected_sequence = manifest.sequence_start;
    let mut previous_timestamp = None;
    for row in reader.get_row_iter(Some(required_schema))? {
        let row = row?;
        let timestamp_us = row
            .get_long(0)
            .context("canonical Parquet row timestamp is not int64")?;
        let sequence = row
            .get_long(1)
            .context("canonical Parquet row sequence is not int64")?;
        let sequence =
            u64::try_from(sequence).context("canonical Parquet row sequence is negative")?;
        let event = row
            .get_string(2)
            .context("canonical Parquet row event is not UTF-8")?;
        let payload_json = row
            .get_string(3)
            .context("canonical Parquet row payload is not UTF-8")?;
        if event != "snapshot" && event != "l2_update" {
            bail!("canonical Parquet row has unsupported event {event}");
        }
        if rows == 0 && event != "snapshot" {
            bail!("canonical Parquet replay is not snapshot seeded");
        }
        if sequence != expected_sequence
            || previous_timestamp.is_some_and(|previous| timestamp_us < previous)
        {
            bail!("canonical Parquet rows are not continuous and time ordered");
        }
        let mut value: serde_json::Value = serde_json::from_str(payload_json)
            .context("canonical Parquet payload is invalid JSON")?;
        let object = value
            .as_object_mut()
            .context("canonical Parquet payload must be a JSON object")?;
        object.insert("timestamp".to_string(), timestamp_us.into());
        object.insert("sequence".to_string(), sequence.into());
        object.insert("event".to_string(), event.as_str().into());
        if start_ts.is_none_or(|start| timestamp_us >= start)
            && end_ts.is_none_or(|end| timestamp_us <= end)
        {
            visitor(
                serde_json::from_value(value)
                    .context("canonical Parquet payload does not match its event type")?,
            )?;
            replay_rows = replay_rows
                .checked_add(1)
                .context("canonical Parquet replay row count overflow")?;
        }

        rows = rows
            .checked_add(1)
            .context("canonical Parquet row count overflow")?;
        first_timestamp.get_or_insert(timestamp_us);
        last_timestamp = Some(timestamp_us);
        previous_timestamp = Some(timestamp_us);
        expected_sequence = expected_sequence
            .checked_add(1)
            .context("canonical Parquet sequence overflow")?;
    }
    if rows != manifest.rows
        || first_timestamp != Some(manifest.first_event_time_us)
        || last_timestamp != Some(manifest.last_event_time_us)
        || expected_sequence
            != manifest
                .sequence_end
                .checked_add(1)
                .context("canonical Parquet sequence overflow")?
    {
        bail!("canonical Parquet coverage does not match its manifest");
    }
    Ok(replay_rows)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("cannot read canonical replay: {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn valid_sha256<'a>(value: &'a str, field: &str) -> anyhow::Result<&'a str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must be a 64-character hex SHA-256");
    }
    Ok(value)
}

fn canonical_path(path: &str) -> anyhow::Result<std::path::PathBuf> {
    fs::canonicalize(resolve_path(path)).with_context(|| format!("无法解析路径: {path}"))
}

fn resolve_path(path: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(path);
    if path.is_absolute() || path.exists() {
        path
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_manifest() -> BacktestDataManifest {
        serde_json::from_value(fixture_manifest_value()).unwrap()
    }

    fn fixture_manifest_value() -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(resolve_path("data/backtest/sample.manifest.json")).unwrap(),
        )
        .unwrap()
    }

    fn fixture_collector_manifest_value() -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(resolve_path("data/backtest/sample.raw.manifest.json")).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn default_backtest_fixture_has_verified_pit_provenance() {
        let config =
            BacktestConfig::from_file(resolve_path("config/backtest/default.yaml")).unwrap();
        let verified = config.validate_data_artifact().unwrap();
        assert_eq!(verified.evidence.manifest_sha256.len(), 64);
        assert_eq!(verified.evidence.config_sha256.len(), 64);
        assert_eq!(
            verified.evidence.source_revision,
            fixture_manifest().source_revision
        );

        let manifest = fixture_manifest();
        assert_ne!(
            canonical_path(&manifest.source_segments[0].path).unwrap(),
            canonical_path(&manifest.artifact_path).unwrap()
        );
        assert!(!manifest.source_segments[0]
            .collector_manifest_path
            .is_empty());
        let raw = fs::read_to_string(resolve_path(&manifest.source_segments[0].path)).unwrap();
        assert!(raw.contains("\"type\":\"diff\""));
        assert!(raw.contains("\"frame\":{\"stream\""));
        assert!(raw.contains("\"snapshot\":{\"lastUpdateId\""));
        assert!(!raw.contains("\"type\":\"trade\""));

        let tape = fs::read_to_string(resolve_path(&manifest.artifact_path)).unwrap();
        assert!(tape.contains("\"event\":\"snapshot\""));
        assert!(tape.contains("\"event\":\"l2_update\""));
        assert!(!tape.contains("\"event\":\"trade\""));
    }

    #[test]
    fn rejects_unknown_backtest_schema() {
        let mut manifest = fixture_manifest_value();
        manifest["schema_version"] = "backtest-pit-v2".into();
        let manifest: BacktestDataManifest = serde_json::from_value(manifest).unwrap();

        assert!(manifest.validate().is_err());
    }

    #[test]
    fn rejects_trade_modality_not_produced_by_collector_schema() {
        let mut manifest = fixture_manifest_value();
        manifest["modalities"] = serde_json::json!(["lob", "trade"]);
        let manifest: BacktestDataManifest = serde_json::from_value(manifest).unwrap();

        assert!(manifest.validate().is_err());
    }

    #[test]
    fn lob_only_manifest_rejects_trade_flow_strategy() {
        let mut config =
            BacktestConfig::from_file(resolve_path("config/backtest/default.yaml")).unwrap();
        config.strategy.volume_factor = 1.0;

        assert!(config
            .validate_data_artifact()
            .unwrap_err()
            .to_string()
            .contains("requires trade flow"));
    }

    #[test]
    fn execution_slippage_must_be_finite_and_within_risk_limit() {
        let mut config =
            BacktestConfig::from_file(resolve_path("config/backtest/default.yaml")).unwrap();

        config.execution.max_slippage_ticks = 4.0;
        config.risk.slippage_limit_ticks = 3.0;
        assert!(config.validate_execution_model().is_err());

        config.execution.max_slippage_ticks = f64::NAN;
        assert!(config.validate_execution_model().is_err());

        config.execution.max_slippage_ticks = 2.0;
        config.risk.slippage_limit_ticks = f64::INFINITY;
        assert!(config.validate_execution_model().is_err());

        let yaml = fs::read_to_string(resolve_path("config/backtest/default.yaml"))
            .unwrap()
            .replace("slippage_limit_ticks: 3.0", "slippage_limit_ticks: -1.0");
        let config = BacktestConfig::from_yaml_str(&yaml, "negative risk slippage").unwrap();
        assert!(config.validate_execution_model().is_err());
    }

    #[test]
    fn collector_identity_is_bound_to_backtest_manifest() {
        let manifest = fixture_manifest();
        let segment = &manifest.source_segments[0];
        for (field, replacement) in [
            ("market", serde_json::json!("usdm")),
            ("dataset", serde_json::json!("other-dataset")),
            ("symbols", serde_json::json!(["ETHUSDT"])),
            ("bytes", serde_json::json!(999)),
        ] {
            let mut collector = fixture_collector_manifest_value();
            collector[field] = replacement;
            let collector: CollectorRawManifest = serde_json::from_value(collector).unwrap();
            assert!(validate_collector_manifest(
                &manifest,
                segment,
                &collector,
                &segment.sha256,
                fs::metadata(resolve_path(&segment.path)).unwrap().len(),
            )
            .is_err());
        }
    }

    #[test]
    fn collector_trade_events_are_not_fabricated_into_lob_tape() {
        let error = validate_collector_event_type("trade").unwrap_err();
        assert!(error.to_string().contains("does not contain trade events"));
    }

    #[test]
    fn rejects_collector_sequence_gap() {
        let rows = parse_ndjson_values(
            &fs::read(resolve_path("data/backtest/sample.raw.ndjson")).unwrap(),
            "fixture",
        )
        .unwrap();
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let snapshot = rows[0].as_object().unwrap();
        replay.observe("snapshot", snapshot, 100).unwrap();
        let mut gap = rows[1].clone();
        gap["frame"]["data"]["U"] = 105.into();
        gap["frame"]["data"]["u"] = 105.into();

        assert!(replay
            .observe("diff", gap.as_object().unwrap(), 200)
            .is_err());
    }

    #[test]
    fn governed_replay_buffers_pre_snapshot_diff_and_skips_stale_diff() {
        let rows = parse_ndjson_values(
            &fs::read(resolve_path("data/backtest/sample.raw.ndjson")).unwrap(),
            "fixture",
        )
        .unwrap();
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        let diff = rows[1].as_object().unwrap();
        assert!(replay.observe("diff", diff, 50).unwrap().is_empty());

        let emitted = replay
            .observe("snapshot", rows[0].as_object().unwrap(), 100)
            .unwrap();
        assert!(matches!(
            emitted.as_slice(),
            [
                ReplaySequenceEvent::Snapshot {
                    received_at_ns: 100,
                    ..
                },
                ReplaySequenceEvent::Diff {
                    received_at_ns: 100,
                    ..
                }
            ]
        ));

        let mut stale = rows[1].clone();
        stale["frame"]["data"]["U"] = 90.into();
        stale["frame"]["data"]["u"] = 100.into();
        assert!(replay
            .observe("diff", stale.as_object().unwrap(), 200)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn governed_replay_rejects_checkpoint_book_mismatch() {
        let rows = parse_ndjson_values(
            &fs::read(resolve_path("data/backtest/sample.raw.ndjson")).unwrap(),
            "fixture",
        )
        .unwrap();
        let mut replay = ReplaySequenceValidator::new(Market::Spot, "BTCUSDT").unwrap();
        replay
            .observe("snapshot", rows[0].as_object().unwrap(), 100)
            .unwrap();
        replay
            .observe("diff", rows[1].as_object().unwrap(), 200)
            .unwrap();
        let mut checkpoint = rows[2].clone();
        checkpoint["bids"][0][1] = "999".into();

        assert!(replay
            .observe("checkpoint", checkpoint.as_object().unwrap(), 300)
            .is_err());
    }

    #[test]
    fn receive_time_is_never_materialized_early() {
        let received_at_ns = 1_700_000_000_100_000_001;
        let materialized = materialize_replay_event(
            ReplaySequenceEvent::Snapshot {
                received_at_ns,
                bids: vec![["100".to_string(), "1".to_string()]],
                asks: vec![["101".to_string(), "1".to_string()]],
            },
            1,
        )
        .unwrap();
        assert_eq!(materialized["timestamp"], 1_700_000_000_100_001_i64);
    }

    #[test]
    fn reads_real_zstd_collector_segment() {
        let directory = tempfile::tempdir().unwrap();
        let raw_path = resolve_path("data/backtest/sample.raw.ndjson");
        let compressed_path = directory.path().join("part-1.jsonl.zst");
        assert!(Command::new("zstd")
            .args(["-q", "-f"])
            .arg(&raw_path)
            .arg("-o")
            .arg(&compressed_path)
            .status()
            .unwrap()
            .success());
        let (_, source, _) = open_hashed_source(&compressed_path).unwrap();
        fs::rename(&compressed_path, directory.path().join("verified.zst")).unwrap();
        fs::write(&compressed_path, b"replaced after verified read").unwrap();
        let mut rows = Vec::new();
        visit_collector_rows(compressed_path.to_str().unwrap(), source, |row| {
            rows.push(row);
            Ok(())
        })
        .unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["snapshot"]["lastUpdateId"], 100);
        assert_eq!(rows[1]["frame"]["data"]["s"], "BTCUSDT");
    }

    #[test]
    fn compressed_collector_segment_passes_full_governed_chain() {
        let directory = tempfile::tempdir().unwrap();
        let segment = directory.path().join("part-1.jsonl.zst");
        assert!(Command::new("zstd")
            .args(["-q", "-f"])
            .arg(resolve_path("data/backtest/sample.raw.ndjson"))
            .arg("-o")
            .arg(&segment)
            .status()
            .unwrap()
            .success());
        let segment_bytes = fs::read(&segment).unwrap();
        let segment_sha = hex::encode(Sha256::digest(&segment_bytes));

        let collector_manifest_path = directory.path().join("part-1.jsonl.zst.manifest.json");
        let mut collector = fixture_collector_manifest_value();
        collector["file"] = "part-1.jsonl.zst".into();
        collector["bytes"] = segment_bytes.len().into();
        collector["sha256"] = segment_sha.clone().into();
        let collector_bytes = serde_json::to_vec(&collector).unwrap();
        fs::write(&collector_manifest_path, &collector_bytes).unwrap();
        let collector_sha = hex::encode(Sha256::digest(&collector_bytes));
        let success = directory.path().join("part-1.jsonl.zst._SUCCESS");
        fs::write(&success, format!("{segment_sha}\n")).unwrap();

        let artifact = directory.path().join("backtest.ndjson");
        let artifact_bytes = fs::read(resolve_path("data/backtest/sample.ndjson")).unwrap();
        fs::write(&artifact, &artifact_bytes).unwrap();
        let artifact_sha = hex::encode(Sha256::digest(&artifact_bytes));
        let manifest = directory.path().join("backtest.manifest.json");
        let manifest_bytes = serde_json::to_vec(&serde_json::json!({
            "dataset_kind": "backtest_point_in_time_event_tape",
            "schema_version": "backtest-pit-v1",
            "mission_id": "compressed-fixture",
            "market": "spot",
            "symbol": "BTCUSDT",
            "dataset": "binance_spot_lob",
            "modalities": ["lob"],
            "source_revision": source_revision([segment_sha.as_str()]),
            "source_segments": [{
                "path": segment,
                "sha256": segment_sha,
                "collector_manifest_path": collector_manifest_path,
                "collector_manifest_sha256": collector_sha,
                "success_marker_path": success,
                "start_received_at_ns": 1_700_000_000_100_000_000_u64,
                "end_received_at_ns": 1_700_000_000_500_000_000_u64,
                "events": 5
            }],
            "rows": 4,
            "first_event_time_us": 1_700_000_000_100_000_i64,
            "last_event_time_us": 1_700_000_000_500_000_i64,
            "sequence_start": 1,
            "sequence_end": 4,
            "artifact_path": artifact,
            "artifact_sha256": artifact_sha,
            "point_in_time": true
        }))
        .unwrap();
        fs::write(&manifest, &manifest_bytes).unwrap();
        let manifest_sha = hex::encode(Sha256::digest(&manifest_bytes));
        let yaml = format!(
            "data:\n  path: {}\n  format: ndjson\n  manifest_path: {}\n  manifest_sha256: {}\n  require_sequence: true\nstrategy:\n  volume_factor: 0\n  cvd_threshold: 0\nexecution: {{}}\nrisk: {{}}\noutput: {{}}\n",
            artifact.display(),
            manifest.display(),
            manifest_sha,
        );
        let config = BacktestConfig::from_yaml_str(&yaml, "compressed fixture").unwrap();

        let verified = config.validate_data_artifact().unwrap();
        let verified_contents = verified.bytes.clone();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&artifact)
            .unwrap()
            .write_all(b"malicious append\n")
            .unwrap();
        assert_ne!(fs::read(&artifact).unwrap(), verified_contents);
        assert_eq!(verified.bytes, verified_contents);

        let preserved_artifact = directory.path().join("verified-backtest.ndjson");
        fs::rename(&artifact, &preserved_artifact).unwrap();
        fs::write(&artifact, b"malicious replacement\n").unwrap();
        assert!(std::str::from_utf8(&verified.bytes)
            .unwrap()
            .contains("\"event\":\"snapshot\""));
        fs::remove_file(&artifact).unwrap();
        fs::rename(&preserved_artifact, &artifact).unwrap();
        fs::write(&artifact, &verified_contents).unwrap();
        fs::write(&segment, b"corrupt").unwrap();
        assert!(config.validate_data_artifact().is_err());
    }
}

fn default_format() -> String {
    "ndjson".to_string()
}

fn default_tick_size() -> f64 {
    0.5
}

fn default_lot_size() -> f64 {
    0.001
}

fn default_depth_levels() -> usize {
    20
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StrategyConfig {
    #[serde(default = "default_liquidity_window")]
    pub liquidity_window_secs: f64,
    #[serde(default = "default_breakout_window")]
    pub breakout_window_secs: f64,
    #[serde(default = "default_price_delta_ticks")]
    pub price_delta_ticks: f64,
    #[serde(default = "default_volume_factor")]
    pub volume_factor: f64,
    #[serde(default = "default_cvd_threshold")]
    pub cvd_threshold: f64,
    #[serde(default = "default_ofi_threshold")]
    pub ofi_threshold: f64,
    #[serde(default = "default_support_count")]
    pub support_count: usize,
    #[serde(default = "default_resistance_count")]
    pub resistance_count: usize,
    #[serde(default = "default_smoothing_alpha")]
    pub smoothing_alpha: f64,
}

fn default_liquidity_window() -> f64 {
    900.0 // 15 分
}

fn default_breakout_window() -> f64 {
    5.0
}

fn default_price_delta_ticks() -> f64 {
    2.0
}

fn default_volume_factor() -> f64 {
    1.0
}

fn default_cvd_threshold() -> f64 {
    0.0
}

fn default_ofi_threshold() -> f64 {
    0.0
}

fn default_support_count() -> usize {
    3
}

fn default_resistance_count() -> usize {
    3
}

fn default_smoothing_alpha() -> f64 {
    0.2
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutionConfig {
    #[serde(default = "default_base_qty")]
    pub base_qty: f64,
    #[serde(default = "default_max_position")]
    pub max_position: f64,
    #[serde(default = "default_slippage_ticks")]
    pub max_slippage_ticks: f64,
    #[serde(default = "default_slippage_ticks")]
    pub stop_loss_ticks: f64,
    #[serde(default = "default_slippage_ticks")]
    pub take_profit_ticks: f64,
    #[serde(default)]
    pub hold_secs: Option<f64>,
    #[serde(default)]
    pub fee_bps: f64,
    #[serde(default = "default_max_fill_ratio")]
    pub max_fill_ratio: f64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            base_qty: default_base_qty(),
            max_position: default_max_position(),
            max_slippage_ticks: default_slippage_ticks(),
            stop_loss_ticks: default_slippage_ticks(),
            take_profit_ticks: default_slippage_ticks(),
            hold_secs: Some(900.0),
            fee_bps: 0.0,
            max_fill_ratio: default_max_fill_ratio(),
        }
    }
}

fn default_base_qty() -> f64 {
    0.01
}

fn default_max_position() -> f64 {
    0.05
}

fn default_slippage_ticks() -> f64 {
    2.0
}

fn default_max_fill_ratio() -> f64 {
    0.1
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RiskConfig {
    #[serde(default = "default_inventory_limit")]
    pub inventory_limit: f64,
    #[serde(default = "default_slippage_ticks")]
    pub slippage_limit_ticks: f64,
    #[serde(default = "default_max_consecutive_losses")]
    pub max_consecutive_losses: usize,
    #[serde(default)]
    pub daily_loss_limit: Option<f64>,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            inventory_limit: default_inventory_limit(),
            slippage_limit_ticks: default_slippage_ticks(),
            max_consecutive_losses: default_max_consecutive_losses(),
            daily_loss_limit: None,
        }
    }
}

fn default_inventory_limit() -> f64 {
    0.1
}

fn default_max_consecutive_losses() -> usize {
    3
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct OutputConfig {
    #[serde(default = "default_trade_csv")]
    pub trades_csv: String,
    #[serde(default = "default_summary_csv")]
    pub summary_csv: String,
    #[serde(default)]
    pub metrics_json: Option<String>,
    #[serde(default = "default_evidence_json")]
    pub evidence_json: String,
}

fn default_trade_csv() -> String {
    "backtest_trades.csv".to_string()
}

fn default_summary_csv() -> String {
    "backtest_summary.csv".to_string()
}

fn default_evidence_json() -> String {
    "backtest_evidence.json".to_string()
}
