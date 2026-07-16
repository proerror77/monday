use std::fs;
use std::path::Path;

use anyhow::{bail, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

#[derive(Debug, Clone, Deserialize)]
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
        if self.risk.slippage_limit_ticks <= 0.0 {
            self.risk.slippage_limit_ticks = self.execution.max_slippage_ticks;
        }
        if self.strategy.support_count == 0 {
            self.strategy.support_count = 3;
        }
        if self.strategy.resistance_count == 0 {
            self.strategy.resistance_count = 3;
        }
    }

    pub fn validate_data_artifact(&self) -> anyhow::Result<()> {
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

        let bytes = fs::read(resolve_path(&self.data.path))
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
        validate_event_tape(&self.data.path, &manifest)?;
        self.validate_execution_model()
    }

    fn validate_execution_model(&self) -> anyhow::Result<()> {
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

#[derive(Debug, Clone, Deserialize)]
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
    mode: String,
    replay_scope: String,
    events: u64,
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
            || self.schema_version.trim().is_empty()
            || self.mission_id.trim().is_empty()
            || !self.point_in_time
            || self.rows == 0
            || self.source_segments.is_empty()
        {
            bail!("backtest manifest is not a complete point-in-time event tape");
        }
        valid_sha256(&self.artifact_sha256, "manifest.artifact_sha256")?;
        valid_sha256(&self.source_revision, "manifest.source_revision")?;
        if self.first_event_time_us > self.last_event_time_us
            || self.sequence_start > self.sequence_end
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
    let artifact_rows = parse_ndjson_values(artifact_bytes, "backtest event tape")?;
    let mut materialized_rows = Vec::with_capacity(artifact_rows.len());
    for segment in &manifest.source_segments {
        let expected = valid_sha256(&segment.sha256, "source segment sha256")?;
        if !unique.insert(expected.to_string())
            || segment.events == 0
            || segment.start_received_at_ns > segment.end_received_at_ns
        {
            bail!("backtest source segment evidence is incomplete or duplicated");
        }
        let bytes = fs::read(resolve_path(&segment.path))
            .with_context(|| format!("无法读取源数据 segment: {}", segment.path))?;
        let actual = hex::encode(Sha256::digest(&bytes));
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
        validate_collector_manifest(segment, &collector, expected)?;

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

        let raw_rows = parse_ndjson_values(&bytes, "collector raw segment")?;
        if raw_rows.len() as u64 != segment.events {
            bail!("collector raw row count does not match source evidence");
        }
        let mut previous_received_at = None;
        let mut observed_types = std::collections::HashMap::<String, u64>::new();
        for raw in raw_rows {
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
            if event_type == "checkpoint" {
                if raw.get("synced").and_then(serde_json::Value::as_bool) != Some(true)
                    || raw.get("bridged").and_then(serde_json::Value::as_bool) != Some(true)
                    || raw
                        .get("symbol")
                        .and_then(serde_json::Value::as_str)
                        .map(str::is_empty)
                        .unwrap_or(true)
                {
                    bail!("collector replay checkpoint is incomplete");
                }
                continue;
            }
            if received_at_ns % 1_000 != 0 {
                bail!("materialized collector row does not have microsecond-aligned receive time");
            }
            raw.insert(
                "timestamp".to_owned(),
                serde_json::Value::from((received_at_ns / 1_000) as i64),
            );
            raw.insert("event".to_owned(), serde_json::Value::from(event_type));
            materialized_rows.push(serde_json::Value::Object(raw));
        }
        if observed_types != collector.event_types {
            bail!("collector raw event types do not match its manifest");
        }
        hashes.push(expected.to_string());
    }
    hashes.sort();
    let mut revision = Sha256::new();
    for hash in hashes {
        revision.update(hash.as_bytes());
    }
    let actual_revision = hex::encode(revision.finalize());
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
    segment: &SourceSegmentEvidence,
    collector: &CollectorRawManifest,
    expected_source_sha: &str,
) -> anyhow::Result<()> {
    let source_file = Path::new(&segment.path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if collector.schema != "binance.lob_tape.v2"
        || collector.venue != "binance"
        || collector.market.trim().is_empty()
        || collector.dataset.trim().is_empty()
        || collector.mode != "diff"
        || collector.replay_scope != "captured_snapshot_seed_plus_sequence_checked_diffs"
        || !collector.has_replay_safe_checkpoint
        || !collector.all_symbols_bridged
        || collector.events != segment.events
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
    {
        bail!("collector raw manifest is incomplete or does not match source evidence");
    }
    Ok(())
}

fn parse_ndjson_values(bytes: &[u8], label: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    std::str::from_utf8(bytes)
        .with_context(|| format!("{label} is not UTF-8"))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("invalid {label} row")))
        .collect()
}

fn validate_event_tape(path: &str, manifest: &BacktestDataManifest) -> anyhow::Result<()> {
    let contents = fs::read_to_string(resolve_path(path))?;
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

    #[test]
    fn default_backtest_fixture_has_verified_pit_provenance() {
        let config =
            BacktestConfig::from_file(resolve_path("config/backtest/default.yaml")).unwrap();
        config.validate_data_artifact().unwrap();

        let manifest_path = config.data.manifest_path.as_deref().unwrap();
        let manifest: BacktestDataManifest =
            serde_json::from_slice(&fs::read(resolve_path(manifest_path)).unwrap()).unwrap();
        assert_ne!(
            canonical_path(&manifest.source_segments[0].path).unwrap(),
            canonical_path(&manifest.artifact_path).unwrap()
        );
        assert!(!manifest.source_segments[0]
            .collector_manifest_path
            .is_empty());
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OutputConfig {
    #[serde(default = "default_trade_csv")]
    pub trades_csv: String,
    #[serde(default = "default_summary_csv")]
    pub summary_csv: String,
    #[serde(default)]
    pub metrics_json: Option<String>,
}

fn default_trade_csv() -> String {
    "backtest_trades.csv".to_string()
}

fn default_summary_csv() -> String {
    "backtest_summary.csv".to_string()
}
