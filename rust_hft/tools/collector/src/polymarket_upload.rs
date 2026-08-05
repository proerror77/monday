//! Validation and fail-closed OSS upload for closed Polymarket raw tapes.

use crate::lob_archiver::{command_status_with_timeout, sha256_file, write_success_marker};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use rand::random;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub(crate) const TRADE_COMPLETION_KIND: &str = "polymarket_trade_collection_complete";
pub(crate) const TRADE_COMPLETION_BASIS: &str =
    "polymarket_data_api_exhausted_after_settlement_and_stable_polls_v1";

const ALLOWED_KINDS: [&str; 12] = [
    "quote",
    "quote_collection_failure",
    "event_discovered",
    "event_expired",
    "reference_price",
    "market_metadata",
    "polymarket_trade",
    "market_settlement",
    TRADE_COMPLETION_KIND,
    // Binance reference kinds recorded by the strategy-runtime tape recorder
    // when its include_kinds policy enables them.
    "spot_price",
    "agg_trade",
    "l2",
];
const SUPPORTED_SYMBOL_ALIASES: [(&str, &[&str]); 7] = [
    ("BTCUSDT", &["BITCOIN", "BTC"]),
    ("ETHUSDT", &["ETHEREUM", "ETH"]),
    ("SOLUSDT", &["SOLANA", "SOL "]),
    ("XRPUSDT", &["XRP"]),
    ("DOGEUSDT", &["DOGECOIN", "DOGE"]),
    ("HYPEUSDT", &["HYPERLIQUID", "HYPE"]),
    ("BNBUSDT", &["BINANCE COIN", "BNB"]),
];
const SETTLEMENT_PRICE: Decimal = Decimal::from_parts(999, 0, 0, false, 3);
const SETTLEMENT_LOSER_PRICE: Decimal = Decimal::from_parts(1, 0, 0, false, 3);
const SETTLEMENT_SUM_TOLERANCE: Decimal = Decimal::from_parts(1, 0, 0, false, 6);
const MAX_FUTURE_RECORDING_SKEW_SECS: i64 = 300;
// Tick-level tapes mix WS hot-path and REST poll quotes for the same token;
// the slower family legitimately regresses per-token source time by a few
// seconds. Tolerate bounded jitter while still rejecting true reordering.
const MAX_QUOTE_SOURCE_REGRESSION_MS: i64 = 30_000;
// Readback-after-upload must tolerate OSS object-visibility lag on this
// endpoint: in production (2026-08-01, three gate invocations) a just-PUT
// object repeatedly returned 404 NoSuchKey for a few seconds past a 3x1s
// retry window before becoming HEAD-able, failing every shadow gate.
// Per-file readback timeout: verified objects are tens of MB on an internal
// endpoint, so a hung download must fail fast instead of inheriting the
// 300s upload timeout (36 downloads x 300s would otherwise cap the retry
// loop at hours).
const OSS_READBACK_FILE_TIMEOUT: Duration = Duration::from_secs(60);
// Wall-clock budget for the entire retry loop, covering command time AND
// backoff sleeps. Must exceed the worst observed object-visibility lag on
// this endpoint: a 109MiB multipart upload took ~150s to become HEAD-able
// (2026-08-01T18:23 CST), failing a 12x5s loop at ~90s.
const OSS_READBACK_MAX_WALL_CLOCK: Duration = Duration::from_secs(600);
// Per-artifact verify-download retry: a just-PUT object can 404 NoSuchKey for
// minutes on this endpoint (2026-08-02 production uploads), so each artifact
// gets a bounded retry budget aligned with the gate script's 30x20s curve.
const OSS_VERIFY_DOWNLOAD_ATTEMPTS: usize = 30;
pub const DEFAULT_MAX_CONCURRENT_UPLOADS: usize = 2;
const MAX_CONCURRENT_UPLOADS: usize = 4;
#[cfg(not(test))]
const OSS_VERIFY_DOWNLOAD_RETRY_DELAY: Duration = Duration::from_secs(20);
#[cfg(test)]
const OSS_VERIFY_DOWNLOAD_RETRY_DELAY: Duration = Duration::from_millis(1);

#[derive(Debug, Clone)]
pub struct UploadConfig {
    pub spool_dir: PathBuf,
    pub dataset: String,
    pub quote_depth_levels: usize,
    pub quote_sample_ms: u64,
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
    pub profile: String,
    pub zstd_timeout: Duration,
    pub oss_timeout: Duration,
    pub max_concurrent_uploads: usize,
    pub zstd_threads: u64,
    pub oss_parallel: u64,
    pub oss_part_size: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct UploadSummary {
    pub uploaded_segments: usize,
    pub canonical_uploaded_segments: usize,
}

impl UploadConfig {
    pub fn validate(&self) -> Result<()> {
        if self.dataset.is_empty()
            || !self.dataset.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
        {
            bail!("dataset must match [a-z0-9_-]+");
        }
        if self.bucket.trim().is_empty()
            || self.endpoint.trim().is_empty()
            || self.region.trim().is_empty()
            || self.profile.trim().is_empty()
            || self.zstd_timeout.is_zero()
            || self.oss_timeout.is_zero()
        {
            bail!("upload destination and timeouts must be non-empty");
        }
        if !(1..=MAX_CONCURRENT_UPLOADS).contains(&self.max_concurrent_uploads) {
            bail!("max concurrent uploads must be between 1 and {MAX_CONCURRENT_UPLOADS}");
        }
        if self.oss_parallel == 0 {
            bail!("oss parallel must be at least 1");
        }
        if self.oss_part_size.trim().is_empty() {
            bail!("oss part size must be non-empty");
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Artifacts {
    source: PathBuf,
    data: PathBuf,
    manifest: PathBuf,
    success: PathBuf,
    object_prefix: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

#[derive(Debug)]
struct ScanResult {
    manifest: Value,
    identity: FileIdentity,
}

#[derive(Debug)]
struct UploadedSegment {
    object: String,
    canonical_complete: bool,
}

pub(crate) struct ExclusiveTempDir(PathBuf);

impl ExclusiveTempDir {
    pub(crate) fn create(parent: &Path, prefix: &str) -> Result<Self> {
        ensure_canonical_directory(parent)?;
        for _ in 0..32 {
            let path = parent.join(format!("{prefix}.{:016x}", random::<u64>()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not allocate an exclusive temporary directory")
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ExclusiveTempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn utc_now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

pub(crate) fn ensure_canonical_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "directory path must be absolute and canonical: {}",
            path.display()
        );
    }
    let mut existing = path;
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| anyhow!("directory has no existing ancestor"))?;
    }
    let metadata = fs::symlink_metadata(existing)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "directory ancestor is a symlink or non-directory: {}",
            existing.display()
        );
    }
    if fs::canonicalize(existing)? != existing {
        bail!("directory has an indirect ancestor: {}", path.display());
    }
    let missing = path.strip_prefix(existing)?;
    let mut current = existing.to_path_buf();
    for component in missing.components() {
        current.push(component.as_os_str());
        match fs::create_dir(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "directory path component is a symlink or non-directory: {}",
                current.display()
            );
        }
    }
    if fs::canonicalize(path)? != path {
        bail!("directory has an indirect ancestor: {}", path.display());
    }
    Ok(())
}

fn regular_identity(path: &Path) -> Result<FileIdentity> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect closed tape {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!(
            "closed tape must be a regular non-symlink file: {}",
            path.display()
        );
    }
    Ok(FileIdentity::from_metadata(&metadata))
}

fn ensure_identity(path: &Path, expected: FileIdentity) -> Result<()> {
    let actual = regular_identity(path)?;
    if actual != expected {
        bail!("tape changed while being validated; refusing to archive an active file");
    }
    Ok(())
}

fn parse_timestamp(
    value: Option<&Value>,
    field: &str,
    line_number: usize,
) -> Result<DateTime<Utc>> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("line {line_number}: {field} must be a string"))?;
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| anyhow!("line {line_number}: invalid {field}: {value}"))
}

fn decimal_or_none(
    value: Option<&Value>,
    field: &str,
    line_number: usize,
) -> Result<Option<Decimal>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    Decimal::from_str(&text)
        .map(Some)
        .map_err(|_| anyhow!("line {line_number}: {field} must be numeric"))
}

fn polymarket_tradeable_price(price: Decimal) -> bool {
    price > Decimal::new(2, 2) && price < Decimal::new(98, 2)
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Null) | None => "None".to_owned(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(value) => value.to_string(),
    }
}

fn canonical_decimal(value: Option<&Value>) -> String {
    let raw = value_text(value);
    Decimal::from_str(&raw)
        .map(|value| value.normalize().to_string())
        .unwrap_or(raw)
}

fn required_object_text<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    object_label: &str,
    line_number: usize,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("line {line_number}: {object_label} requires {field}"))
}

fn raw_trade_timestamp(value: Option<&Value>, line_number: usize) -> Result<i64> {
    let timestamp = match value {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| anyhow!("line {line_number}: raw trade timestamp must be integer seconds"))?;
    DateTime::from_timestamp(timestamp, 0)
        .ok_or_else(|| anyhow!("line {line_number}: raw trade timestamp is out of range"))?;
    Ok(timestamp)
}

fn parse_market_datetime(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let value = value?.as_str()?;
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|parsed| parsed.and_utc())
        })
}

fn contains_window_token(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + token.len()..].chars().next();
        before.is_none_or(|value| matches!(value, '-' | '_' | ' '))
            && after.is_none_or(|value| matches!(value, '-' | '_' | ' '))
    })
}

fn raw_market_window(market: &Map<String, Value>) -> Option<u64> {
    let event = market
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| events.first());
    let start = [
        market.get("eventStartTime"),
        market.get("startDate"),
        event.and_then(|event| event.get("startTime")),
        event.and_then(|event| event.get("startDate")),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| parse_market_datetime(Some(value)));
    if let (Some(start), Some(end)) = (start, parse_market_datetime(market.get("endDate"))) {
        let seconds = (end - start).num_seconds();
        if matches!(seconds, 300 | 900) {
            return u64::try_from(seconds).ok();
        }
    }
    let text = format!(
        "{} {}",
        market
            .get("slug")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        market
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or_default()
    )
    .to_ascii_lowercase();
    if contains_window_token(&text, "15m") || text.contains("15 minute") {
        Some(900)
    } else if contains_window_token(&text, "5m") || text.contains("5 minute") {
        Some(300)
    } else {
        None
    }
}

fn parse_json_array(value: Option<&Value>) -> Option<Vec<Value>> {
    match value {
        Some(Value::Array(values)) => Some(values.clone()),
        Some(Value::String(value)) => serde_json::from_str::<Vec<Value>>(value).ok(),
        _ => None,
    }
}

fn unique_string_pair(
    market: &Map<String, Value>,
    field: &str,
    record_kind: &str,
    line_number: usize,
) -> Result<[String; 2]> {
    let values = parse_json_array(market.get(field)).unwrap_or_default();
    let strings = values
        .iter()
        .map(|value| value.as_str().filter(|value| !value.is_empty()))
        .collect::<Option<Vec<_>>>();
    let Some(strings) = strings else {
        bail!("line {line_number}: {record_kind} raw market requires two unique {field}");
    };
    if strings.len() != 2 || strings[0] == strings[1] {
        bail!("line {line_number}: {record_kind} raw market requires two unique {field}");
    }
    Ok([strings[0].to_owned(), strings[1].to_owned()])
}

pub(crate) fn derived_trade_record_id(trade: &Map<String, Value>) -> String {
    let parts = [
        value_text(trade.get("transactionHash")),
        value_text(trade.get("conditionId")),
        value_text(trade.get("asset")),
        value_text(trade.get("side")),
        value_text(trade.get("timestamp")),
        value_text(trade.get("proxyWallet")),
        canonical_decimal(trade.get("size")),
        canonical_decimal(trade.get("price")),
        value_text(trade.get("outcomeIndex")),
    ];
    hex::encode(Sha256::digest(parts.join("|").as_bytes()))
}

pub(crate) fn trade_record_ids_sha256<'a>(record_ids: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for record_id in record_ids {
        digest.update(record_id.as_bytes());
        digest.update(b"\n");
    }
    hex::encode(digest.finalize())
}

pub(crate) fn validate_canonical_trade(
    update: &Map<String, Value>,
    line_number: usize,
) -> Result<String> {
    if update.get("record_id_version").and_then(Value::as_str) != Some("v2") {
        bail!("line {line_number}: polymarket_trade record_id_version must be v2");
    }
    for field in [
        "record_id",
        "market_id",
        "condition_id",
        "token_id",
        "symbol",
        "side",
        "trade_ts",
        "transaction_hash",
        "proxy_wallet",
        "outcome",
    ] {
        required_text(update, field, line_number)
            .map_err(|_| anyhow!("line {line_number}: polymarket_trade requires {field}"))?;
    }

    let trade = update
        .get("trade")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("line {line_number}: polymarket_trade.trade must be an object"))?;
    let transaction_hash =
        required_object_text(trade, "transactionHash", "raw trade", line_number)?;
    let condition_id = required_object_text(trade, "conditionId", "raw trade", line_number)?;
    let asset = required_object_text(trade, "asset", "raw trade", line_number)?;
    let side = required_object_text(trade, "side", "raw trade", line_number)?;
    if !matches!(side, "BUY" | "SELL") {
        bail!("line {line_number}: raw trade side must be BUY or SELL");
    }
    let proxy_wallet = required_object_text(trade, "proxyWallet", "raw trade", line_number)?;
    let outcome = required_object_text(trade, "outcome", "raw trade", line_number)?;
    let timestamp = raw_trade_timestamp(trade.get("timestamp"), line_number)?;
    let size = decimal_or_none(trade.get("size"), "raw trade size", line_number)?;
    if size.is_none_or(|value| value <= Decimal::ZERO) {
        bail!("line {line_number}: raw trade size must be positive");
    }
    let price = decimal_or_none(trade.get("price"), "raw trade price", line_number)?;
    if !price.is_some_and(|value| (Decimal::ZERO..=Decimal::ONE).contains(&value)) {
        bail!("line {line_number}: raw trade price must be within [0, 1]");
    }
    let outcome_index = trade.get("outcomeIndex").and_then(Value::as_i64);
    if !matches!(outcome_index, Some(0 | 1)) {
        bail!("line {line_number}: raw trade outcomeIndex must be 0 or 1");
    }

    for (field, expected) in [
        ("transaction_hash", transaction_hash),
        ("condition_id", condition_id),
        ("token_id", asset),
        ("side", side),
        ("proxy_wallet", proxy_wallet),
        ("outcome", outcome),
    ] {
        if update.get(field).and_then(Value::as_str) != Some(expected) {
            bail!("line {line_number}: polymarket_trade {field} does not match raw trade");
        }
    }
    if decimal_or_none(update.get("size"), "size", line_number)? != size {
        bail!("line {line_number}: polymarket_trade size does not match raw trade");
    }
    if decimal_or_none(update.get("price"), "price", line_number)? != price {
        bail!("line {line_number}: polymarket_trade price does not match raw trade");
    }
    if update.get("outcome_index").and_then(Value::as_i64) != outcome_index {
        bail!("line {line_number}: polymarket_trade outcome_index does not match raw trade");
    }
    if update.get("trade_ts_unix").and_then(Value::as_i64) != Some(timestamp) {
        bail!("line {line_number}: polymarket_trade trade_ts_unix does not match raw trade");
    }
    let trade_ts = parse_timestamp(update.get("trade_ts"), "trade_ts", line_number)?;
    if trade_ts != DateTime::from_timestamp(timestamp, 0).expect("timestamp was validated") {
        bail!("line {line_number}: polymarket_trade trade_ts does not match raw trade");
    }

    let record_id = required_text(update, "record_id", line_number)?.to_owned();
    if record_id != derived_trade_record_id(trade) {
        bail!("line {line_number}: record_id does not match raw trade");
    }
    Ok(record_id)
}

fn validate_trade_completion(
    update: &Map<String, Value>,
    sequence: u64,
    recorded_at: DateTime<Utc>,
    line_number: usize,
) -> Result<(String, (String, String), Value)> {
    let market_id = required_text(update, "market_id", line_number)?;
    let condition_id = required_text(update, "condition_id", line_number)?;
    let symbol = required_text(update, "symbol", line_number)?;
    let retrieved_at = required_text(update, "retrieved_at", line_number)?;
    let digest = required_text(update, "trade_record_ids_sha256", line_number)?;
    let trade_count = update
        .get("trade_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("line {line_number}: trade completion requires trade_count"))?;
    let finalization_lag_secs = update
        .get("finalization_lag_secs")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            anyhow!("line {line_number}: trade completion requires positive finalization_lag_secs")
        })?;
    let stable_polls_required = update
        .get("stable_polls_required")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            anyhow!("line {line_number}: trade completion requires positive stable_polls_required")
        })?;
    let market_window_secs = update
        .get("market_window_secs")
        .and_then(Value::as_u64)
        .filter(|value| matches!(value, 300 | 900))
        .ok_or_else(|| anyhow!("line {line_number}: trade completion has invalid market window"))?;
    if update.get("record_id_version").and_then(Value::as_str) != Some("v2")
        || update.get("source").and_then(Value::as_str) != Some("polymarket_data_api")
        || update.get("completeness_basis").and_then(Value::as_str) != Some(TRADE_COMPLETION_BASIS)
        || update.get("pagination_exhausted").and_then(Value::as_bool) != Some(true)
        || update.get("settlement_observed").and_then(Value::as_bool) != Some(true)
        || update.get("malformed_trade_rows").and_then(Value::as_u64) != Some(0)
        || !SUPPORTED_SYMBOL_ALIASES
            .iter()
            .any(|(supported, _)| *supported == symbol)
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || parse_timestamp(
            Some(&Value::String(retrieved_at.to_owned())),
            "retrieved_at",
            line_number,
        )? > recorded_at
    {
        bail!("line {line_number}: invalid event-local trade completion proof");
    }
    Ok((
        market_id.to_owned(),
        (market_id.to_owned(), condition_id.to_owned()),
        json!({
            "condition_id": condition_id,
            "symbol": symbol,
            "market_window_secs": market_window_secs,
            "trade_count": trade_count,
            "trade_record_ids_sha256": digest,
            "completion_sequence": sequence,
            "retrieved_at": retrieved_at,
            "completeness_basis": TRADE_COMPLETION_BASIS,
            "finalization_lag_secs": finalization_lag_secs,
            "stable_polls_required": stable_polls_required,
        }),
    ))
}

fn validate_market_context(
    update: &Map<String, Value>,
    record_kind: &str,
    line_number: usize,
) -> Result<([String; 2], [String; 2])> {
    let market_id = required_text(update, "market_id", line_number)
        .map_err(|_| anyhow!("line {line_number}: {record_kind} requires market_id"))?;
    let condition_id = required_text(update, "condition_id", line_number)
        .map_err(|_| anyhow!("line {line_number}: {record_kind} requires condition_id"))?;
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("line {line_number}: {record_kind}.market must be an object"))?;
    let raw_market_id = required_object_text(
        market,
        "id",
        &format!("{record_kind} raw market"),
        line_number,
    )?;
    let raw_condition_id = required_object_text(
        market,
        "conditionId",
        &format!("{record_kind} raw market"),
        line_number,
    )?;
    if market_id != raw_market_id {
        bail!("line {line_number}: {record_kind} market_id does not match raw market");
    }
    if condition_id != raw_condition_id {
        bail!("line {line_number}: {record_kind} condition_id does not match raw market");
    }
    let symbol = required_text(update, "symbol", line_number)
        .map_err(|_| anyhow!("line {line_number}: {record_kind} requires symbol"))?;
    let raw_question = market
        .get("question")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_uppercase();
    let raw_symbol = SUPPORTED_SYMBOL_ALIASES
        .iter()
        .find_map(|(candidate, aliases)| {
            aliases
                .iter()
                .any(|alias| raw_question.contains(alias))
                .then_some(*candidate)
        });
    if raw_symbol != Some(symbol) {
        bail!("line {line_number}: {record_kind} symbol is unsupported or contradicts raw market");
    }
    let window = update.get("market_window_secs").and_then(Value::as_u64);
    if !matches!(window, Some(300 | 900)) || raw_market_window(market) != window {
        bail!("line {line_number}: {record_kind} window is unsupported or contradicts raw market");
    }
    let tokens = unique_string_pair(market, "clobTokenIds", record_kind, line_number)?;
    let outcomes = unique_string_pair(market, "outcomes", record_kind, line_number)?;
    Ok((tokens, outcomes))
}

pub(crate) fn validate_market_metadata(
    update: &Map<String, Value>,
    line_number: usize,
) -> Result<()> {
    for field in ["market_id", "condition_id", "symbol", "retrieved_at"] {
        required_text(update, field, line_number)
            .map_err(|_| anyhow!("line {line_number}: market_metadata requires {field}"))?;
    }
    validate_market_context(update, "market_metadata", line_number)?;
    Ok(())
}

pub(crate) fn validate_market_settlement(
    update: &Map<String, Value>,
    line_number: usize,
) -> Result<()> {
    let (tokens, outcomes) = validate_market_context(update, "market_settlement", line_number)?;
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .expect("market context validation requires an object");
    if market.get("closed").and_then(Value::as_bool) != Some(true) {
        bail!("line {line_number}: market_settlement raw market must be closed");
    }
    let raw_prices = parse_json_array(market.get("outcomePrices")).unwrap_or_default();
    if raw_prices.len() != 2 {
        bail!("line {line_number}: market_settlement requires exactly two outcomePrices");
    }
    let prices = raw_prices
        .iter()
        .map(|value| Decimal::from_str(&value_text(Some(value))).ok())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            anyhow!("line {line_number}: market_settlement outcomePrices must be numeric")
        })?;
    if prices
        .iter()
        .any(|price| !(Decimal::ZERO..=Decimal::ONE).contains(price))
    {
        bail!("line {line_number}: market_settlement outcomePrices must be within [0, 1]");
    }
    let winners = prices
        .iter()
        .enumerate()
        .filter_map(|(index, price)| (*price >= SETTLEMENT_PRICE).then_some(index))
        .collect::<Vec<_>>();
    if winners.len() != 1 {
        bail!("line {line_number}: market_settlement requires exactly one winning price");
    }
    let winner = winners[0];
    let loser = 1 - winner;
    if prices[loser] > SETTLEMENT_LOSER_PRICE {
        bail!("line {line_number}: market_settlement losing price must be near zero");
    }
    let price_sum = prices[0] + prices[1];
    if price_sum < Decimal::ONE - SETTLEMENT_SUM_TOLERANCE
        || price_sum > Decimal::ONE + SETTLEMENT_SUM_TOLERANCE
    {
        bail!("line {line_number}: market_settlement prices must sum to one");
    }
    if update.get("winning_token_id").and_then(Value::as_str) != Some(tokens[winner].as_str()) {
        bail!("line {line_number}: market_settlement winning_token_id does not match raw market");
    }
    if update.get("winning_outcome").and_then(Value::as_str) != Some(outcomes[winner].as_str()) {
        bail!("line {line_number}: market_settlement winning_outcome does not match raw market");
    }
    let outcome_set = outcomes
        .iter()
        .map(|outcome| outcome.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if outcome_set != BTreeSet::from(["down".to_owned(), "up".to_owned()])
        && outcome_set != BTreeSet::from(["no".to_owned(), "yes".to_owned()])
    {
        bail!("line {line_number}: market_settlement outcomes must be Up/Down or Yes/No");
    }
    let resolved_up_won = matches!(outcomes[winner].to_ascii_lowercase().as_str(), "up" | "yes");
    if update.get("resolved_up_won").and_then(Value::as_bool) != Some(resolved_up_won) {
        bail!("line {line_number}: market_settlement resolved_up_won does not match raw market");
    }
    Ok(())
}

fn required_decimal(
    update: &Map<String, Value>,
    field: &str,
    kind: &str,
    line_number: usize,
) -> Result<Decimal> {
    decimal_or_none(update.get(field), field, line_number)?
        .ok_or_else(|| anyhow!("line {line_number}: {kind} requires {field}"))
}

/// Binance reference kinds are fail-closed on malformed records but otherwise
/// validated only for shape: coverage and quality manifest flags stay
/// Polymarket-scoped and never create obligations for these updates.
///
/// `spot_price` accepts a zero price: the direct Binance tick feed emits a
/// zero-price sentinel per symbol while its websocket is reconnecting
/// (`send_unavailable_spot_ticks`), and the recorder persists it. Negative
/// prices, missing fields, and wrong types are malformed.
fn validate_spot_price(update: &Map<String, Value>, line_number: usize) -> Result<()> {
    required_text(update, "symbol", line_number)
        .map_err(|_| anyhow!("line {line_number}: spot_price requires symbol"))?;
    let price = required_decimal(update, "price", "spot_price", line_number)?;
    if price < Decimal::ZERO {
        bail!("line {line_number}: spot_price price must be non-negative");
    }
    parse_timestamp(update.get("ts"), "ts", line_number)?;
    Ok(())
}

/// The Binance feed parser only emits agg trades with positive price and
/// quantity, so a recorded `agg_trade` must preserve those invariants.
fn validate_agg_trade(update: &Map<String, Value>, line_number: usize) -> Result<()> {
    required_text(update, "symbol", line_number)
        .map_err(|_| anyhow!("line {line_number}: agg_trade requires symbol"))?;
    if update.get("agg_trade_id").and_then(Value::as_u64).is_none() {
        bail!("line {line_number}: agg_trade requires agg_trade_id");
    }
    if required_decimal(update, "price", "agg_trade", line_number)? <= Decimal::ZERO {
        bail!("line {line_number}: agg_trade price must be positive");
    }
    if required_decimal(update, "quantity", "agg_trade", line_number)? <= Decimal::ZERO {
        bail!("line {line_number}: agg_trade quantity must be positive");
    }
    if !update.get("is_buyer_maker").is_some_and(Value::is_boolean) {
        bail!("line {line_number}: agg_trade is_buyer_maker must be a boolean");
    }
    parse_timestamp(update.get("ts"), "ts", line_number)?;
    Ok(())
}

/// `obi` is a ratio of near-mid depth totals and therefore bounded by [-1, 1].
fn validate_l2(update: &Map<String, Value>, line_number: usize) -> Result<()> {
    required_text(update, "symbol", line_number)
        .map_err(|_| anyhow!("line {line_number}: l2 requires symbol"))?;
    let obi = update
        .get("obi")
        .and_then(Value::as_f64)
        .filter(|obi| obi.is_finite())
        .ok_or_else(|| anyhow!("line {line_number}: l2 requires a finite obi"))?;
    if !(-1.0..=1.0).contains(&obi) {
        bail!("line {line_number}: l2 obi must be within [-1, 1]");
    }
    if update.get("spread_bps").and_then(Value::as_u64).is_none() {
        bail!("line {line_number}: l2 requires spread_bps");
    }
    parse_timestamp(update.get("ts"), "ts", line_number)?;
    Ok(())
}

fn required_text<'a>(
    update: &'a Map<String, Value>,
    field: &str,
    line_number: usize,
) -> Result<&'a str> {
    update
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("line {line_number}: update requires {field}"))
}

fn reference_context(update: &Map<String, Value>, line_number: usize) -> Result<(String, String)> {
    Ok((
        required_text(update, "market_id", line_number)?.to_owned(),
        required_text(update, "condition_id", line_number)?.to_owned(),
    ))
}

fn increment_nested(counts: &mut BTreeMap<String, BTreeMap<String, u64>>, kind: &str, field: &str) {
    *counts
        .entry(kind.to_owned())
        .or_default()
        .entry(field.to_owned())
        .or_default() += 1;
}

fn strict_rotation_name(name: &str) -> bool {
    let Some(middle) = name
        .strip_prefix("market-updates.")
        .and_then(|name| name.strip_suffix(".ndjson"))
    else {
        return false;
    };
    let mut parts = middle.split('.');
    let Some(stamp) = parts.next() else {
        return false;
    };
    if !strict_timestamp(stamp) {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(uuid), None) => strict_uuid(uuid),
        _ => false,
    }
}

fn strict_timestamp(value: &str) -> bool {
    let format = match value.len() {
        15 => "%Y%m%dT%H%M%S",
        21 => "%Y%m%dT%H%M%S%6f",
        _ => return false,
    };
    NaiveDateTime::parse_from_str(value, format).is_ok()
}

fn strict_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn discover_rotated_tapes(spool_dir: &Path) -> Result<Vec<PathBuf>> {
    ensure_canonical_directory(spool_dir)?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(spool_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("spool entry name is not UTF-8"))?;
        if strict_rotation_name(name) {
            regular_identity(&entry.path())?;
            paths.push(entry.path());
        } else if name != "market-updates.ndjson" && name.starts_with("market-updates.") {
            bail!("invalid rotated tape name: {name}");
        }
    }
    paths.sort();
    Ok(paths)
}

/// Validate a closed tape and return the manifest body used by the uploader.
pub fn scan_tape(
    path: &Path,
    dataset: &str,
    quote_depth_levels: usize,
    quote_sample_ms: u64,
) -> Result<Value> {
    Ok(scan_tape_with_identity(path, dataset, quote_depth_levels, quote_sample_ms)?.manifest)
}

pub(crate) fn validate_reference_tape_for_recovery(
    path: &Path,
    recovery_time: DateTime<Utc>,
) -> Result<()> {
    let scan = scan_tape_with_identity_at(path, "crypto_expiry_reference", 0, 0, recovery_time)?;
    if scan.manifest["start_sequence"].as_u64() != Some(0) {
        bail!(
            "recovered active tape must start at sequence 0; actual={}",
            scan.manifest["start_sequence"]
        );
    }
    // Deliberately narrower than ALLOWED_KINDS: the recovered active tape
    // belongs to the PM reference collector, which never records Binance
    // reference kinds, so their presence here remains a hard failure.
    for kind in scan.manifest["event_types"]
        .as_object()
        .expect("scan manifest has event types")
        .keys()
    {
        if !matches!(
            kind.as_str(),
            "market_metadata" | "polymarket_trade" | "market_settlement" | TRADE_COMPLETION_KIND
        ) {
            bail!("recovered reference tape contains unsupported update kind {kind:?}");
        }
    }
    Ok(())
}

fn scan_tape_with_identity(
    path: &Path,
    dataset: &str,
    quote_depth_levels: usize,
    quote_sample_ms: u64,
) -> Result<ScanResult> {
    scan_tape_with_identity_at(
        path,
        dataset,
        quote_depth_levels,
        quote_sample_ms,
        Utc::now(),
    )
}

fn scan_tape_with_identity_at(
    path: &Path,
    dataset: &str,
    quote_depth_levels: usize,
    quote_sample_ms: u64,
    validation_time: DateTime<Utc>,
) -> Result<ScanResult> {
    let identity = regular_identity(path)?;
    let file = File::open(path).with_context(|| format!("open tape {}", path.display()))?;
    if FileIdentity::from_metadata(&file.metadata()?) != identity {
        bail!("tape changed while being opened; refusing to archive an active file");
    }
    let mut reader = BufReader::new(file);
    let mut event_types = BTreeMap::<String, u64>::new();
    let mut present_fields = BTreeMap::<String, BTreeMap<String, u64>>::new();
    let mut non_null_fields = BTreeMap::<String, BTreeMap<String, u64>>::new();
    let mut symbols = BTreeSet::new();
    let mut token_ids = BTreeSet::new();
    let mut known_event_tokens = BTreeSet::new();
    let mut event_end_times = BTreeMap::<String, DateTime<Utc>>::new();
    let mut event_token_lifecycles = BTreeMap::new();
    let mut expired_before_discovery_tokens = BTreeSet::new();
    let mut lifecycle_integrity_complete = true;
    let mut quoted_token_ids = BTreeSet::new();
    let mut attempted_quote_token_ids = BTreeSet::new();
    let mut contextless_quote_tokens = BTreeSet::new();
    let mut last_quote_source_at = BTreeMap::<String, DateTime<Utc>>::new();
    let mut first_recorded_at: Option<String> = None;
    let mut last_recorded_at: Option<String> = None;
    let mut previous_recorded_at: Option<DateTime<Utc>> = None;
    let mut first_sequence: Option<u64> = None;
    let mut last_sequence: Option<u64> = None;
    let mut expected_sequence: Option<u64> = None;
    let mut crossed_quotes = 0_u64;
    let mut one_sided_quotes = 0_u64;
    let mut empty_quotes = 0_u64;
    let mut non_executable_quotes = 0_u64;
    let mut executable_quotes = 0_u64;
    let mut missing_bid_size = 0_u64;
    let mut missing_ask_size = 0_u64;
    let mut incomplete_quotes = 0_u64;
    let mut max_quote_latency_ms = 0_i64;
    let mut tolerated_quote_source_regressions = 0_u64;
    let mut request_attempts = 0_u64;
    let mut request_failures = 0_u64;
    let mut transport_reconnects = 0_u64;
    // Transport-kind failure rows whose token has not yet proven a later
    // successful quote in tape order; leftovers still count as failures.
    let mut pending_transport_reconnects: BTreeMap<String, u64> = BTreeMap::new();
    let mut max_request_latency_ms = 0_i64;
    let mut out_of_range_prices = 0_u64;
    let mut negative_sizes = 0_u64;
    let mut max_bid_levels = 0_usize;
    let mut max_ask_levels = 0_usize;
    let mut contextless_quotes = 0_u64;
    let mut market_ids = BTreeSet::new();
    let mut condition_ids = BTreeSet::new();
    let mut record_ids = BTreeSet::new();
    let mut record_id_versions = BTreeSet::new();
    let mut metadata_contexts = BTreeSet::new();
    let mut metadata_identities = BTreeMap::new();
    let mut dependent_reference_contexts = BTreeSet::new();
    let mut trade_completions = BTreeMap::<String, Value>::new();
    let mut source_field_presence = BTreeMap::<String, BTreeMap<String, u64>>::new();
    let mut source_field_non_null = BTreeMap::<String, BTreeMap<String, u64>>::new();
    let mut raw_line = Vec::new();
    let mut line_number = 0_usize;

    loop {
        raw_line.clear();
        let read = reader.read_until(b'\n', &mut raw_line)?;
        if read == 0 {
            break;
        }
        line_number += 1;
        if !raw_line.ends_with(b"\n") {
            bail!("line {line_number}: tape ends with an incomplete record");
        }
        let record: Value = serde_json::from_slice(&raw_line)
            .map_err(|_| anyhow!("line {line_number}: invalid JSON"))?;
        let record = record
            .as_object()
            .ok_or_else(|| anyhow!("line {line_number}: record must be an object"))?;
        let sequence = record
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                anyhow!("line {line_number}: sequence must be a non-negative integer")
            })?;
        let expected = expected_sequence.get_or_insert(sequence);
        if sequence != *expected {
            bail!(
                "line {line_number}: sequence gap expected={} actual={sequence}",
                *expected
            );
        }
        let recorded_at = parse_timestamp(record.get("recorded_at"), "recorded_at", line_number)?;
        if recorded_at > validation_time + TimeDelta::seconds(MAX_FUTURE_RECORDING_SKEW_SECS) {
            bail!(
                "line {line_number}: recorded_at is more than {MAX_FUTURE_RECORDING_SKEW_SECS}s in the future"
            );
        }
        if previous_recorded_at.is_some_and(|previous| recorded_at < previous) {
            bail!("line {line_number}: recorded_at moved backwards");
        }
        let update = record
            .get("update")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("line {line_number}: update must be an object"))?;
        let kind = update
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("line {line_number}: unsupported update kind None"))?;
        if !ALLOWED_KINDS.contains(&kind) {
            bail!("line {line_number}: unsupported update kind {kind:?}");
        }

        *event_types.entry(kind.to_owned()).or_default() += 1;
        for (field, value) in update {
            increment_nested(&mut present_fields, kind, field);
            if !value.is_null() {
                increment_nested(&mut non_null_fields, kind, field);
            }
        }
        if let Some(symbol) = update
            .get("symbol")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            symbols.insert(symbol.to_owned());
        }
        let token_id = update.get("token_id").and_then(Value::as_str);
        if let Some(token_id) = token_id.filter(|value| !value.is_empty()) {
            token_ids.insert(token_id.to_owned());
        }
        if kind == "event_discovered" {
            let up_token = update
                .get("up_token")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let down_token = update
                .get("down_token")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let event_id = update
                .get("event_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            let end_time = update
                .get("end_time")
                .and_then(Value::as_str)
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|end_time| end_time.with_timezone(&Utc));
            let token_pair_complete =
                matches!((up_token, down_token), (Some(up), Some(down)) if up != down);
            let malformed_lifecycle =
                !token_pair_complete || event_id.is_none() || end_time.is_none();
            lifecycle_integrity_complete &= !malformed_lifecycle;
            if let (Some(event_id), Some(end_time)) = (event_id.as_ref(), end_time.as_ref()) {
                match event_end_times.get(event_id) {
                    Some(previous) if previous != end_time => {
                        lifecycle_integrity_complete = false;
                    }
                    None => {
                        event_end_times.insert(event_id.clone(), *end_time);
                    }
                    Some(_) => {}
                }
            }
            let expired_before_discovery = !malformed_lifecycle
                && recorded_at <= validation_time
                && end_time
                    .as_ref()
                    .is_some_and(|end_time| end_time < &recorded_at);
            let lifecycle = (event_id, end_time);
            for token in [up_token, down_token].into_iter().flatten() {
                known_event_tokens.insert(token.to_owned());
                match event_token_lifecycles.get(token) {
                    None => {
                        event_token_lifecycles.insert(token.to_owned(), lifecycle.clone());
                        if expired_before_discovery {
                            expired_before_discovery_tokens.insert(token.to_owned());
                        }
                    }
                    Some(previous) if previous != &lifecycle => {
                        // Conflicting event association or end time is not safe to exempt.
                        lifecycle_integrity_complete = false;
                        expired_before_discovery_tokens.remove(token);
                    }
                    Some(_) => {}
                }
            }
        }
        if kind == "event_expired" {
            let event_id = update
                .get("event_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let end_time = update
                .get("end_time")
                .and_then(Value::as_str)
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|end_time| end_time.with_timezone(&Utc));
            lifecycle_integrity_complete &= event_id.is_some() && end_time.is_some();
            if let (Some(event_id), Some(end_time)) = (event_id, end_time) {
                match event_end_times.get(event_id) {
                    Some(previous) if previous != &end_time => {
                        lifecycle_integrity_complete = false;
                    }
                    None => {
                        event_end_times.insert(event_id.to_owned(), end_time);
                    }
                    Some(_) => {}
                }
            }
        }
        if let Some(market_id) = update
            .get("market_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            market_ids.insert(market_id.to_owned());
        }
        if let Some(condition_id) = update
            .get("condition_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            condition_ids.insert(condition_id.to_owned());
        }
        let raw_field = match kind {
            "market_metadata" | "market_settlement" => Some("market"),
            "polymarket_trade" => Some("trade"),
            _ => None,
        };
        if let Some(raw_field) = raw_field {
            let raw_payload = update
                .get(raw_field)
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    anyhow!("line {line_number}: {kind}.{raw_field} must be an object")
                })?;
            for (field, value) in raw_payload {
                increment_nested(&mut source_field_presence, kind, field);
                if !value.is_null() {
                    increment_nested(&mut source_field_non_null, kind, field);
                }
            }
        }

        match kind {
            "market_metadata" => {
                validate_market_metadata(update, line_number)?;
                let context = reference_context(update, line_number)?;
                let identity = (
                    required_text(update, "symbol", line_number)?.to_owned(),
                    update["market_window_secs"]
                        .as_u64()
                        .expect("metadata validation requires a supported window"),
                );
                if metadata_identities
                    .insert(context.clone(), identity.clone())
                    .is_some_and(|previous| previous != identity)
                {
                    bail!("line {line_number}: market_metadata identity changed within segment");
                }
                metadata_contexts.insert(context);
            }
            "polymarket_trade" => {
                let record_id = validate_canonical_trade(update, line_number)?;
                if !record_ids.insert(record_id) {
                    bail!("line {line_number}: duplicate polymarket_trade record_id");
                }
                record_id_versions.insert("v2".to_owned());
                dependent_reference_contexts.insert(reference_context(update, line_number)?);
            }
            "market_settlement" => {
                for field in [
                    "market_id",
                    "condition_id",
                    "symbol",
                    "winning_token_id",
                    "winning_outcome",
                    "resolution_source",
                    "retrieved_at",
                ] {
                    required_text(update, field, line_number).map_err(|_| {
                        anyhow!("line {line_number}: market_settlement requires {field}")
                    })?;
                }
                validate_market_settlement(update, line_number)?;
                dependent_reference_contexts.insert(reference_context(update, line_number)?);
            }
            TRADE_COMPLETION_KIND => {
                let (market_id, context, completion) =
                    validate_trade_completion(update, sequence, recorded_at, line_number)?;
                if trade_completions.insert(market_id, completion).is_some() {
                    bail!("line {line_number}: duplicate event-local trade completion proof");
                }
                dependent_reference_contexts.insert(context);
            }
            "spot_price" => validate_spot_price(update, line_number)?,
            "agg_trade" => validate_agg_trade(update, line_number)?,
            "l2" => validate_l2(update, line_number)?,
            _ => {}
        }

        if matches!(kind, "quote" | "quote_collection_failure") {
            let token = required_text(update, "token_id", line_number)?;
            attempted_quote_token_ids.insert(token.to_owned());
            request_attempts += 1;
            if !known_event_tokens.contains(token) {
                contextless_quotes += 1;
                contextless_quote_tokens.insert(token.to_owned());
            }
        }

        if kind == "quote" {
            if update.get("request_status").and_then(Value::as_str) != Some("success") {
                bail!("line {line_number}: quote requires request_status=success");
            }
            let source_at = parse_timestamp(update.get("ts"), "ts", line_number)
                .map_err(|_| anyhow!("line {line_number}: quote requires ts"))?;
            let quote_latency_ms = recorded_at
                .signed_duration_since(source_at)
                .num_milliseconds();
            if quote_latency_ms < 0 {
                bail!("line {line_number}: quote source time is after received time");
            }
            max_quote_latency_ms = max_quote_latency_ms.max(quote_latency_ms);
            let token = required_text(update, "token_id", line_number)?;
            match last_quote_source_at.get_mut(token) {
                Some(previous) => {
                    if source_at < *previous {
                        let regression = *previous - source_at;
                        if regression > TimeDelta::milliseconds(MAX_QUOTE_SOURCE_REGRESSION_MS) {
                            let regression_ms = regression.num_milliseconds();
                            bail!(
                                "line {line_number}: quote source time moved backwards by {regression_ms}ms"
                            );
                        }
                        tolerated_quote_source_regressions += 1;
                    } else {
                        *previous = source_at;
                    }
                }
                None => {
                    last_quote_source_at.insert(token.to_owned(), source_at);
                }
            }
            quoted_token_ids.insert(token.to_owned());
            // A successful quote proves the token recovered from any earlier
            // transport reconnect, excusing those rows from request_failures.
            pending_transport_reconnects.remove(token);
            let bid = decimal_or_none(update.get("bid"), "bid", line_number)?;
            let ask = decimal_or_none(update.get("ask"), "ask", line_number)?;
            let bid_size = decimal_or_none(update.get("bid_size"), "bid_size", line_number)?;
            let ask_size = decimal_or_none(update.get("ask_size"), "ask_size", line_number)?;
            let bid_levels = quote_levels(update.get("bid_levels"), line_number)?;
            let ask_levels = quote_levels(update.get("ask_levels"), line_number)?;
            let mut all_levels_non_executable = true;
            if bid.is_some() && bid_size.is_none() {
                missing_bid_size += 1;
            }
            if ask.is_some() && ask_size.is_none() {
                missing_ask_size += 1;
            }
            if quote_depth_levels > 0
                && (bid_levels.len() > quote_depth_levels || ask_levels.len() > quote_depth_levels)
            {
                bail!("line {line_number}: quote exceeds configured depth");
            }
            max_bid_levels = max_bid_levels.max(bid_levels.len());
            max_ask_levels = max_ask_levels.max(ask_levels.len());
            for (side, levels) in [("bid_levels", &bid_levels), ("ask_levels", &ask_levels)] {
                for (level_index, level) in levels.iter().enumerate() {
                    let level = level.as_object().ok_or_else(|| {
                        anyhow!("line {line_number}: {side}[{level_index}] must be an object")
                    })?;
                    let level_price = decimal_or_none(
                        level.get("price"),
                        &format!("{side}[{level_index}].price"),
                        line_number,
                    )?;
                    let level_size = decimal_or_none(
                        level.get("size"),
                        &format!("{side}[{level_index}].size"),
                        line_number,
                    )?;
                    let (Some(level_price), Some(level_size)) = (level_price, level_size) else {
                        bail!("line {line_number}: {side}[{level_index}] requires price and size");
                    };
                    if !(Decimal::ZERO..=Decimal::ONE).contains(&level_price) {
                        out_of_range_prices += 1;
                        all_levels_non_executable = false;
                    }
                    if level_size <= Decimal::ZERO {
                        negative_sizes += 1;
                        all_levels_non_executable = false;
                    }
                    if polymarket_tradeable_price(level_price) {
                        all_levels_non_executable = false;
                    }
                }
            }
            if matches!((bid, ask), (Some(bid), Some(ask)) if bid > ask) {
                crossed_quotes += 1;
            }
            for price in [bid, ask].into_iter().flatten() {
                if !(Decimal::ZERO..=Decimal::ONE).contains(&price) {
                    out_of_range_prices += 1;
                }
            }
            for size in [bid_size, ask_size].into_iter().flatten() {
                if size < Decimal::ZERO {
                    negative_sizes += 1;
                }
            }
            let collection_result = match (bid, ask, bid_size, ask_size) {
                (None, None, None, None) if bid_levels.is_empty() && ask_levels.is_empty() => {
                    "empty"
                }
                (None, None, None, None) if all_levels_non_executable => "non_executable",
                (Some(bid), Some(ask), Some(bid_size), Some(ask_size))
                    if bid <= ask
                        && polymarket_tradeable_price(bid)
                        && polymarket_tradeable_price(ask)
                        && bid_size > Decimal::ZERO
                        && ask_size > Decimal::ZERO =>
                {
                    "executable"
                }
                (Some(price), None, Some(size), None) | (None, Some(price), None, Some(size))
                    if polymarket_tradeable_price(price) && size > Decimal::ZERO =>
                {
                    "one_sided"
                }
                _ => "incomplete",
            };
            if update.get("collection_result").and_then(Value::as_str) != Some(collection_result) {
                bail!(
                    "line {line_number}: quote collection_result does not match {collection_result}"
                );
            }
            match collection_result {
                "executable" => executable_quotes += 1,
                "one_sided" => one_sided_quotes += 1,
                "empty" => empty_quotes += 1,
                "non_executable" => non_executable_quotes += 1,
                _ => incomplete_quotes += 1,
            }
        } else if kind == "quote_collection_failure" {
            if update.get("request_status").and_then(Value::as_str) != Some("failure")
                || update.get("collection_result").and_then(Value::as_str) != Some("api_failure")
            {
                bail!("line {line_number}: quote collection failure requires explicit status");
            }
            let request_started_at = parse_timestamp(
                update.get("request_started_at"),
                "request_started_at",
                line_number,
            )?;
            let failed_at = parse_timestamp(update.get("ts"), "ts", line_number)?;
            if request_started_at > failed_at || failed_at > recorded_at {
                bail!("line {line_number}: invalid quote request timing");
            }
            let request_latency_ms = failed_at
                .signed_duration_since(request_started_at)
                .num_milliseconds();
            max_request_latency_ms = max_request_latency_ms.max(request_latency_ms);
            let error_kind = required_text(update, "error_kind", line_number)?;
            if !matches!(
                error_kind,
                "transport"
                    | "transport_reconnect"
                    | "http_status"
                    | "invalid_response"
                    | "websocket_connect"
                    | "websocket_subscription_encode"
                    | "websocket_subscribe"
                    | "websocket_payload"
                    | "websocket_pong"
                    | "websocket_close"
                    | "websocket_receive"
                    | "websocket_eof"
                    | "websocket_heartbeat_timeout"
                    | "websocket_heartbeat_send"
            ) {
                bail!("line {line_number}: unsupported quote collection error_kind");
            }
            match update.get("http_status") {
                Some(Value::Null) | None if error_kind != "http_status" => {}
                Some(status)
                    if error_kind == "http_status"
                        && status
                            .as_u64()
                            .is_some_and(|status| (100..=599).contains(&status)) => {}
                _ => bail!("line {line_number}: invalid quote collection http_status"),
            }
            // Bare transport resets are reconnection lifecycle evidence, not
            // integrity failures, but only once the token proves a later
            // successful quote in tape order; without that recovery the row
            // stays pending and still counts as a request failure below.
            if matches!(error_kind, "websocket_receive" | "transport_reconnect") {
                transport_reconnects += 1;
                let token = required_text(update, "token_id", line_number)?;
                *pending_transport_reconnects
                    .entry(token.to_owned())
                    .or_default() += 1;
            } else {
                request_failures += 1;
            }
        }

        let recorded_at_text = record
            .get("recorded_at")
            .and_then(Value::as_str)
            .expect("recorded_at was validated")
            .to_owned();
        first_recorded_at.get_or_insert_with(|| recorded_at_text.clone());
        last_recorded_at = Some(recorded_at_text);
        first_sequence.get_or_insert(sequence);
        last_sequence = Some(sequence);
        expected_sequence = sequence.checked_add(1);
        if expected_sequence.is_none() {
            bail!("line {line_number}: sequence overflow");
        }
        previous_recorded_at = Some(recorded_at);
    }

    // A transport reconnect whose token never quoted again inside this tape
    // never proved a clean resync, so it still counts as a request failure.
    request_failures += pending_transport_reconnects.values().sum::<u64>();

    let after = FileIdentity::from_metadata(&reader.get_ref().metadata()?);
    if after != identity {
        bail!("tape changed while being validated; refusing to archive an active file");
    }
    ensure_identity(path, identity)?;
    let first_sequence = first_sequence.ok_or_else(|| anyhow!("tape is empty"))?;
    let last_sequence = last_sequence.expect("non-empty tape has a final sequence");
    let first_recorded_at = first_recorded_at.expect("non-empty tape has a first timestamp");
    let last_recorded_at = last_recorded_at.expect("non-empty tape has a final timestamp");
    let partition = DateTime::parse_from_rfc3339(&first_recorded_at)
        .expect("recorded_at was validated")
        .with_timezone(&Utc);
    let event_context_complete = contextless_quotes == 0;
    let quote_obligation_tokens = known_event_tokens
        .difference(&expired_before_discovery_tokens)
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing_quote_tokens = quote_obligation_tokens
        .difference(&quoted_token_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let missing_quote_attempt_tokens = quote_obligation_tokens
        .difference(&attempted_quote_token_ids)
        .cloned()
        .collect::<BTreeSet<_>>();
    let quote_quality_complete = incomplete_quotes == 0
        && crossed_quotes == 0
        && out_of_range_prices == 0
        && negative_sizes == 0;
    let quote_coverage_complete = missing_quote_tokens.is_empty()
        && missing_quote_attempt_tokens.is_empty()
        && request_failures == 0
        && event_context_complete
        && quote_quality_complete;
    let has_quotes = event_types.get("quote").copied().unwrap_or_default() > 0;
    let has_reference_records = [
        "market_metadata",
        "polymarket_trade",
        "market_settlement",
        TRADE_COMPLETION_KIND,
    ]
    .iter()
    .any(|kind| event_types.get(*kind).copied().unwrap_or_default() > 0);
    for (market_id, completion) in &trade_completions {
        let condition_id = completion["condition_id"].as_str().unwrap_or_default();
        let expected = metadata_identities.get(&(market_id.clone(), condition_id.to_owned()));
        let actual = (
            completion["symbol"].as_str().unwrap_or_default().to_owned(),
            completion["market_window_secs"]
                .as_u64()
                .unwrap_or_default(),
        );
        if expected != Some(&actual) {
            bail!("event-local trade completion identity contradicts market metadata");
        }
    }
    let reference_context_complete = dependent_reference_contexts.is_subset(&metadata_contexts);
    let depth_complete = has_quotes && quote_depth_levels == 0;
    let temporal_updates_complete = has_quotes && quote_sample_ms == 0;
    let quote_count = event_types.get("quote").copied().unwrap_or_default();
    let executable_quote_ratio = if quote_count == 0 {
        0.0
    } else {
        executable_quotes as f64 / quote_count as f64
    };
    let replay_scope = if !reference_context_complete {
        "reference_hour_segment_requires_market_metadata_context"
    } else if has_reference_records && !has_quotes {
        "complete_reference_hour_segment"
    } else if event_context_complete {
        if depth_complete {
            if temporal_updates_complete {
                "complete_full_depth_normalized_hour_segment"
            } else {
                "complete_full_depth_sampled_normalized_hour_segment"
            }
        } else {
            "complete_sampled_normalized_hour_segment"
        }
    } else {
        "sampled_normalized_hour_segment_requires_prior_event_context"
    };
    let source_file = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("source file name is not UTF-8"))?;
    let quality = json!({
        "crossed_quotes": crossed_quotes,
        "executable_quotes": executable_quotes,
        "executable_quote_ratio": executable_quote_ratio,
        "missing_bid_size": missing_bid_size,
        "missing_ask_size": missing_ask_size,
        "incomplete_quotes": incomplete_quotes,
        "max_quote_latency_ms": max_quote_latency_ms,
        "tolerated_quote_source_regressions": tolerated_quote_source_regressions,
        "request_attempts": request_attempts,
        "request_successes": quote_count,
        "request_failures": request_failures,
        "transport_reconnects": transport_reconnects,
        "max_request_latency_ms": max_request_latency_ms,
        "one_sided_quotes": one_sided_quotes,
        "empty_quotes": empty_quotes,
        "non_executable_quotes": non_executable_quotes,
        "out_of_range_prices": out_of_range_prices,
        "negative_sizes": negative_sizes,
        "max_bid_levels": max_bid_levels,
        "max_ask_levels": max_ask_levels,
        "contextless_quotes": contextless_quotes,
        "duplicate_record_ids": 0,
    });
    let mut manifest = json!({
        "schema": "monday.polymarket.raw.v1",
        "canonical": reference_context_complete
            && quote_coverage_complete
            && lifecycle_integrity_complete,
        "venue": "polymarket",
        "dataset": dataset,
        "format": "ndjson.zst",
        "replay_scope": replay_scope,
        "venue_depth_complete": depth_complete,
        "temporal_updates_complete": temporal_updates_complete,
        "segment_complete": reference_context_complete
            && quote_coverage_complete
            && lifecycle_integrity_complete,
        "source_session_closed": true,
        "event_context_complete": event_context_complete,
        "quote_coverage_complete": quote_coverage_complete,
        "quote_quality_complete": quote_quality_complete,
        "lifecycle_integrity_complete": lifecycle_integrity_complete,
        "expired_before_discovery_tokens": expired_before_discovery_tokens,
        "missing_quote_tokens": missing_quote_tokens,
        "missing_quote_attempt_tokens": missing_quote_attempt_tokens,
        "contextless_quote_tokens": contextless_quote_tokens,
        "events": last_sequence - first_sequence + 1,
        "event_types": event_types,
        "start_sequence": first_sequence,
        "end_sequence": last_sequence,
        "sequence_gaps": 0,
        "start_recorded_at": first_recorded_at,
        "end_recorded_at": last_recorded_at,
        "date": partition.format("%Y-%m-%d").to_string(),
        "hour": partition.format("%H").to_string(),
        "symbols": symbols,
        "token_count": token_ids.len(),
        "market_count": market_ids.len(),
        "condition_count": condition_ids.len(),
        "record_id_versions": record_id_versions,
        "recording_policy": {
            "quote_sample_ms": quote_sample_ms,
            "quote_depth_levels": quote_depth_levels,
            "event_scoped_quotes": true,
        },
        "field_presence": present_fields,
        "field_non_null": non_null_fields,
        "source_field_presence": source_field_presence,
        "source_field_non_null": source_field_non_null,
        "quality": quality,
        "source_file": source_file,
        "source_bytes": identity.bytes,
    });
    manifest
        .as_object_mut()
        .expect("manifest is an object")
        .insert("trade_completions".to_owned(), json!(trade_completions));
    manifest
        .as_object_mut()
        .expect("manifest is an object")
        .insert(
            "reference_context_complete".to_owned(),
            json!(reference_context_complete),
        );
    // Binance reference kinds are counted separately: quote coverage, event
    // context, and quality flags stay Polymarket-scoped, so `canonical` keeps
    // its PM-only meaning on mixed tapes and Binance presence is reported
    // only here and in `event_types`.
    let binance_reference_counts = ["spot_price", "agg_trade", "l2"]
        .into_iter()
        .map(|kind| {
            (
                kind.to_owned(),
                json!(event_types.get(kind).copied().unwrap_or_default()),
            )
        })
        .collect::<Map<String, Value>>();
    manifest
        .as_object_mut()
        .expect("manifest is an object")
        .insert(
            "binance_reference_counts".to_owned(),
            Value::Object(binance_reference_counts),
        );
    Ok(ScanResult { manifest, identity })
}

fn quote_levels(value: Option<&Value>, line_number: usize) -> Result<&[Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(levels)) => Ok(levels),
        _ => bail!("line {line_number}: quote levels must be arrays"),
    }
}

fn split_tape_by_utc_hour(source: &Path, staging_dir: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(source)?;
    let mut reader = BufReader::new(file);
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("source file name is not UTF-8"))?;
    let mut chunks = Vec::new();
    let mut current_hour: Option<String> = None;
    let mut output: Option<(PathBuf, BufWriter<File>)> = None;
    let mut raw_line = Vec::new();
    let mut line_number = 0_usize;
    loop {
        raw_line.clear();
        let read = reader.read_until(b'\n', &mut raw_line)?;
        if read == 0 {
            break;
        }
        line_number += 1;
        let record: Value = serde_json::from_slice(&raw_line)?;
        let hour = parse_timestamp(record.get("recorded_at"), "recorded_at", line_number)?
            .format("%Y%m%dT%H")
            .to_string();
        if current_hour.as_deref() != Some(&hour) {
            if let Some((path, mut writer)) = output.take() {
                writer.flush()?;
                writer.get_ref().sync_all()?;
                chunks.push(path);
            }
            let path = staging_dir.join(format!("{stem}.{hour}.ndjson"));
            let file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)?;
            output = Some((path, BufWriter::new(file)));
            current_hour = Some(hour);
        }
        output
            .as_mut()
            .expect("a chunk is opened before writing")
            .1
            .write_all(&raw_line)?;
    }
    if let Some((path, mut writer)) = output {
        writer.flush()?;
        writer.get_ref().sync_all()?;
        chunks.push(path);
    }
    File::open(staging_dir)?.sync_all()?;
    Ok(chunks)
}

fn manifest_utc_hour(manifest: &Value, field: &str) -> Result<String> {
    let timestamp = manifest[field]
        .as_str()
        .ok_or_else(|| anyhow!("scan manifest requires {field}"))?;
    Ok(DateTime::parse_from_rfc3339(timestamp)?
        .with_timezone(&Utc)
        .format("%Y%m%dT%H")
        .to_string())
}

fn stage_validated_single_hour(
    source: &Path,
    staging_dir: &Path,
    mut scan: ScanResult,
) -> Result<Option<(PathBuf, ScanResult)>> {
    let hour = manifest_utc_hour(&scan.manifest, "start_recorded_at")?;
    if manifest_utc_hour(&scan.manifest, "end_recorded_at")? != hour {
        return Ok(None);
    }
    ensure_identity(source, scan.identity)?;
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("source file name is not UTF-8"))?;
    let chunk = staging_dir.join(format!("{stem}.{hour}.ndjson"));
    fs::hard_link(source, &chunk)?;
    if regular_identity(&chunk)? != scan.identity {
        bail!("staged tape identity does not match validated source");
    }
    File::open(staging_dir)?.sync_all()?;
    scan.manifest["source_file"] = json!(chunk.file_name().and_then(|name| name.to_str()));
    Ok(Some((chunk, scan)))
}

fn append_name(path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("file name is not UTF-8"))?;
    Ok(path.with_file_name(format!("{name}{suffix}")))
}

fn exclusive_sibling(path: &Path, suffix: &str) -> Result<(PathBuf, File)> {
    let parent = path.parent().ok_or_else(|| anyhow!("file has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("file name is not UTF-8"))?;
    for _ in 0..32 {
        let temporary = parent.join(format!(".{name}.{:016x}{suffix}", random::<u64>()));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    bail!("could not allocate an exclusive temporary file")
}

pub(crate) fn atomic_json(path: &Path, payload: &Value) -> Result<()> {
    let (temporary, mut file) = exclusive_sibling(path, ".tmp")?;
    let result = (|| -> Result<()> {
        serde_json::to_writer(&mut file, payload)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
fn prepare_artifacts(source: &Path, config: &UploadConfig) -> Result<(Artifacts, Value)> {
    let scan = scan_tape_with_identity(
        source,
        &config.dataset,
        config.quote_depth_levels,
        config.quote_sample_ms,
    )?;
    prepare_artifacts_from_scan(source, config, scan)
}

fn prepare_artifacts_from_scan(
    source: &Path,
    config: &UploadConfig,
    scan: ScanResult,
) -> Result<(Artifacts, Value)> {
    let data = append_name(source, ".zst")?;
    let (temporary_data, temporary_file) = exclusive_sibling(&data, ".tmp")?;
    let output = temporary_file.try_clone()?;
    let mut command = zstd_command(source, config);
    command.stdout(Stdio::from(output));
    match command_status_with_timeout(&mut command, config.zstd_timeout) {
        Ok(status) if status.success() => {}
        Ok(status) => {
            let _ = fs::remove_file(&temporary_data);
            bail!("zstd exited with {status}");
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary_data);
            return Err(error);
        }
    }
    temporary_file.sync_all()?;
    if let Err(error) = ensure_identity(source, scan.identity) {
        let _ = fs::remove_file(&temporary_data);
        return Err(error);
    }
    fs::rename(&temporary_data, &data)?;
    let digest = sha256_file(&data)?;
    let mut metadata = scan
        .manifest
        .as_object()
        .cloned()
        .expect("scan manifest is an object");
    metadata.insert(
        "file".to_owned(),
        json!(data.file_name().and_then(|name| name.to_str())),
    );
    metadata.insert("bytes".to_owned(), json!(fs::metadata(&data)?.len()));
    metadata.insert("sha256".to_owned(), json!(digest.clone()));
    let manifest_value = Value::Object(metadata);
    let manifest = append_name(&data, ".manifest.json")?;
    atomic_json(&manifest, &manifest_value)?;
    let success = write_success_marker(&data, &digest)?;
    let date = manifest_value["date"]
        .as_str()
        .expect("scan manifest has a date");
    let hour = manifest_value["hour"]
        .as_str()
        .expect("scan manifest has an hour");
    let object_prefix = format!(
        "lake/raw/venue=polymarket/dataset={}/date={date}/hour={hour}/sha256={digest}",
        config.dataset,
    );
    Ok((
        Artifacts {
            source: source.to_path_buf(),
            data,
            manifest,
            success,
            object_prefix,
        },
        manifest_value,
    ))
}

fn zstd_command(source: &Path, config: &UploadConfig) -> Command {
    let mut command = Command::new("zstd");
    let threads = format!("-T{}", config.zstd_threads);
    command
        .args(["-q", threads.as_str(), "-3", "-c"])
        .arg(source);
    command
}

fn oss_copy_command(source: &str, destination: &str, config: &UploadConfig) -> Command {
    let mut command = Command::new("aliyun");
    command.args([
        "ossutil",
        "cp",
        source,
        destination,
        "--profile",
        &config.profile,
        "--endpoint",
        &config.endpoint,
        "--region",
        &config.region,
    ]);
    command
        .arg("--parallel")
        .arg(config.oss_parallel.to_string())
        .arg("--part-size")
        .arg(&config.oss_part_size);
    command
}

fn oss_upload_command(source: &str, destination: &str, config: &UploadConfig) -> Command {
    let mut command = oss_copy_command(source, destination, config);
    command.arg("--ignore-existing");
    command
}

fn run_checked(command: &mut Command, timeout: Duration) -> Result<ExitStatus> {
    command.stdout(Stdio::null());
    command.stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture child stderr"))?;
    let stderr_reader = std::thread::spawn(move || {
        let mut stderr = stderr;
        let mut output = Vec::new();
        stderr.read_to_end(&mut output).map(|_| output)
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            bail!("child process timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("child stderr reader panicked"))??;
    let detail = String::from_utf8_lossy(&stderr).trim().to_owned();
    if !detail.is_empty() {
        eprintln!("{detail}");
    }
    if !status.success() {
        if detail.is_empty() {
            bail!("child process exited with {status}");
        }
        bail!("child process exited with {status}: {detail}");
    }
    Ok(status)
}

/// A verify download miss is only worth retrying when the child process ran
/// and exited non-zero (the just-PUT object is not visible yet); spawn or
/// configuration failures must surface immediately.
fn is_retryable_download_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let text = cause.to_string().to_ascii_lowercase();
        text.contains("nosuchkey")
            || text.contains("http 404")
            || text.contains("status code: 404")
            || text.contains("status code 404")
    })
}

fn download_remote_artifacts_with<F>(
    artifacts: &Artifacts,
    config: &UploadConfig,
    sources: &[&Path],
    budget_remaining: Duration,
    max_attempts: usize,
    runner: &mut F,
) -> Result<(ExclusiveTempDir, BTreeMap<String, PathBuf>)>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let parent = artifacts
        .source
        .parent()
        .ok_or_else(|| anyhow!("artifact has no parent"))?;
    let verify_dir = ExclusiveTempDir::create(parent, ".oss-verify")?;
    let download_started = std::time::Instant::now();
    let mut downloaded = BTreeMap::new();
    for &source in sources {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("artifact file name is not UTF-8"))?;
        let destination = verify_dir.path().join(name);
        let remote = format!("oss://{}/{}/{name}", config.bucket, artifacts.object_prefix);
        let mut command = oss_copy_command(
            &remote,
            destination
                .to_str()
                .ok_or_else(|| anyhow!("verification path is not UTF-8"))?,
            config,
        );
        // A just-PUT object can 404 for minutes on this endpoint: retry the
        // download on process-level misses, but fail immediately on
        // configuration-style errors.
        for attempt in 1..=max_attempts {
            let file_budget = budget_remaining
                .checked_sub(download_started.elapsed())
                .unwrap_or(Duration::ZERO);
            if file_budget.is_zero() {
                bail!("remote artifact {name} exceeded OSS readback wall-clock budget");
            }
            let miss = match runner(
                &mut command,
                config
                    .oss_timeout
                    .min(OSS_READBACK_FILE_TIMEOUT)
                    .min(file_budget),
            ) {
                Ok(status) if status.success() => break,
                Ok(status) => {
                    bail!("child process exited with {status} without retryable 404 detail")
                }
                Err(error) if is_retryable_download_failure(&error) => error.to_string(),
                Err(error) => return Err(error),
            };
            if attempt == max_attempts {
                bail!(
                    "remote artifact {name} stayed unavailable after {max_attempts} download attempts: {miss}"
                );
            }
            let retry_budget = budget_remaining
                .checked_sub(download_started.elapsed())
                .unwrap_or(Duration::ZERO);
            if retry_budget.is_zero() {
                bail!("remote artifact {name} exceeded OSS readback wall-clock budget");
            }
            eprintln!("oss verify download of {name} missed ({miss}); retrying");
            std::thread::sleep(OSS_VERIFY_DOWNLOAD_RETRY_DELAY.min(retry_budget));
        }
        regular_identity(&destination)?;
        downloaded.insert(name.to_owned(), destination);
    }
    Ok((verify_dir, downloaded))
}

fn verify_downloaded_paths(
    artifacts: &Artifacts,
    downloaded: &BTreeMap<String, PathBuf>,
) -> Result<()> {
    let expected_manifest: Value = serde_json::from_slice(&fs::read(&artifacts.manifest)?)?;
    let expected_bytes = expected_manifest["bytes"]
        .as_u64()
        .ok_or_else(|| anyhow!("manifest bytes must be an integer"))?;
    let expected_sha = expected_manifest["sha256"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest sha256 must be a string"))?;
    let data_name = artifacts
        .data
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    let manifest_name = artifacts
        .manifest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    let success_name = artifacts
        .success
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    match (downloaded.get(data_name), downloaded.get(manifest_name)) {
        (Some(remote_data), Some(remote_manifest)) => {
            if fs::metadata(remote_data)?.len() != expected_bytes {
                bail!("remote data size does not match manifest");
            }
            if sha256_file(remote_data)? != expected_sha {
                bail!("remote data sha256 does not match manifest");
            }
            if fs::read(remote_manifest)? != fs::read(&artifacts.manifest)? {
                bail!("remote manifest does not match local manifest");
            }
        }
        (None, None) => {}
        _ => bail!("remote data and manifest must be downloaded together"),
    }
    if let Some(remote_success) = downloaded.get(success_name) {
        if fs::read_to_string(remote_success)?.trim() != expected_sha {
            bail!("remote _SUCCESS does not match manifest");
        }
    }
    Ok(())
}

fn verify_remote_paths_with<F>(
    artifacts: &Artifacts,
    config: &UploadConfig,
    sources: &[&Path],
    budget_remaining: Duration,
    runner: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let (_verify_dir, downloaded) = download_remote_artifacts_with(
        artifacts,
        config,
        sources,
        budget_remaining,
        OSS_VERIFY_DOWNLOAD_ATTEMPTS,
        runner,
    )?;
    verify_downloaded_paths(artifacts, &downloaded)
}

#[cfg(test)]
fn verify_remote_artifacts_with<F>(
    artifacts: &Artifacts,
    config: &UploadConfig,
    runner: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let sources = [
        artifacts.data.as_path(),
        artifacts.manifest.as_path(),
        artifacts.success.as_path(),
    ];
    verify_remote_paths_with(
        artifacts,
        config,
        &sources,
        OSS_READBACK_MAX_WALL_CLOCK,
        runner,
    )
}

fn remote_artifacts_exist_and_match_with<F>(
    artifacts: &Artifacts,
    config: &UploadConfig,
    runner: &mut F,
) -> Result<bool>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let sources = [
        artifacts.data.as_path(),
        artifacts.manifest.as_path(),
        artifacts.success.as_path(),
    ];
    let Ok((_verify_dir, downloaded)) = download_remote_artifacts_with(
        artifacts,
        config,
        &sources,
        Duration::MAX,
        1,
        runner,
    )
    else {
        return Ok(false);
    };
    verify_downloaded_paths(artifacts, &downloaded)?;
    Ok(true)
}

fn remove_regular(path: &Path) -> Result<()> {
    regular_identity(path)?;
    fs::remove_file(path)?;
    Ok(())
}

fn remove_artifacts(artifacts: &Artifacts) -> Result<()> {
    let paths = [
        artifacts.source.as_path(),
        artifacts.data.as_path(),
        artifacts.manifest.as_path(),
        artifacts.success.as_path(),
    ];
    let identities = paths
        .iter()
        .map(|path| regular_identity(path))
        .collect::<Result<Vec<_>>>()?;
    for (path, identity) in paths.iter().zip(identities) {
        ensure_identity(path, identity)?;
    }
    for path in paths {
        remove_regular(path)?;
    }
    Ok(())
}

fn upload_artifacts(artifacts: &Artifacts, config: &UploadConfig) -> Result<String> {
    upload_artifacts_with(artifacts, config, &mut run_checked)
}

fn upload_artifact_with<F>(
    artifacts: &Artifacts,
    source: &Path,
    config: &UploadConfig,
    runner: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("artifact file name is not UTF-8"))?;
    let destination = format!("oss://{}/{}/{name}", config.bucket, artifacts.object_prefix);
    let mut command = oss_upload_command(
        source
            .to_str()
            .ok_or_else(|| anyhow!("artifact path is not UTF-8"))?,
        &destination,
        config,
    );
    runner(&mut command, config.oss_timeout)?;
    Ok(())
}

fn remaining_success_readback_budget(readback_elapsed: Duration) -> Result<Duration> {
    OSS_READBACK_MAX_WALL_CLOCK
        .checked_sub(readback_elapsed)
        .filter(|budget| !budget.is_zero())
        .ok_or_else(|| anyhow!("remote _SUCCESS exceeded OSS readback wall-clock budget"))
}

fn upload_artifacts_with<F>(
    artifacts: &Artifacts,
    config: &UploadConfig,
    runner: &mut F,
) -> Result<String>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    if !remote_artifacts_exist_and_match_with(artifacts, config, runner)? {
        for source in [&artifacts.data, &artifacts.manifest] {
            upload_artifact_with(artifacts, source, config, runner)?;
        }
        let readback_started = std::time::Instant::now();
        let data_and_manifest = [artifacts.data.as_path(), artifacts.manifest.as_path()];
        verify_remote_paths_with(
            artifacts,
            config,
            &data_and_manifest,
            OSS_READBACK_MAX_WALL_CLOCK,
            runner,
        )?;
        let success_readback_budget =
            remaining_success_readback_budget(readback_started.elapsed())?;
        upload_artifact_with(artifacts, &artifacts.success, config, runner)?;
        let success = [artifacts.success.as_path()];
        verify_remote_paths_with(artifacts, config, &success, success_readback_budget, runner)?;
    }
    remove_artifacts(artifacts)?;
    let data_name = artifacts
        .data
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("artifact file name is not UTF-8"))?;
    Ok(format!(
        "oss://{}/{}/{data_name}",
        config.bucket, artifacts.object_prefix
    ))
}

fn canonical_complete_manifest(manifest: &Value) -> bool {
    manifest.get("canonical").and_then(Value::as_bool) == Some(true)
        && manifest.get("segment_complete").and_then(Value::as_bool) == Some(true)
        && manifest
            .get("event_context_complete")
            .and_then(Value::as_bool)
            == Some(true)
        && manifest
            .get("quote_coverage_complete")
            .and_then(Value::as_bool)
            == Some(true)
        && manifest
            .get("quote_quality_complete")
            .and_then(Value::as_bool)
            == Some(true)
        && manifest
            .get("missing_quote_tokens")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        && manifest
            .get("missing_quote_attempt_tokens")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        && manifest["quality"]["request_failures"].as_u64() == Some(0)
        && manifest["quality"]["incomplete_quotes"].as_u64() == Some(0)
        && [
            "executable_quotes",
            "missing_bid_size",
            "missing_ask_size",
            "incomplete_quotes",
            "max_quote_latency_ms",
            "request_attempts",
            "request_successes",
            "request_failures",
            "max_request_latency_ms",
            "one_sided_quotes",
            "empty_quotes",
            "non_executable_quotes",
        ]
        .into_iter()
        .all(|field| manifest["quality"][field].as_u64().is_some())
}

fn ensure_upload_staging_root(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!("upload staging root must be a plain directory"),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn archive_source(source: &Path, config: &UploadConfig) -> Result<Vec<UploadedSegment>> {
    let source_scan = scan_tape_with_identity(
        source,
        &config.dataset,
        config.quote_depth_levels,
        config.quote_sample_ms,
    )?;
    if source_scan.manifest["start_sequence"].as_u64() != Some(0) {
        bail!(
            "closed source tape must start at sequence 0; actual={}",
            source_scan.manifest["start_sequence"]
        );
    }
    let spool_dir = source
        .parent()
        .ok_or_else(|| anyhow!("source has no parent"))?;
    let staging_root = spool_dir.join(".upload-staging");
    ensure_upload_staging_root(&staging_root)?;
    let staging = ExclusiveTempDir::create(&staging_root, "session")?;
    let source_identity = source_scan.identity;
    let chunks = match stage_validated_single_hour(source, staging.path(), source_scan)? {
        Some(chunk) => vec![chunk],
        None => {
            let paths = split_tape_by_utc_hour(source, staging.path())?;
            ensure_identity(source, source_identity)?;
            paths
                .into_iter()
                .map(|chunk| {
                    let scan = scan_tape_with_identity(
                        &chunk,
                        &config.dataset,
                        config.quote_depth_levels,
                        config.quote_sample_ms,
                    )?;
                    Ok((chunk, scan))
                })
                .collect::<Result<Vec<_>>>()?
        }
    };
    let mut uploaded = Vec::new();
    for (chunk, scan) in chunks {
        let (artifacts, manifest) = prepare_artifacts_from_scan(&chunk, config, scan)?;
        uploaded.push(UploadedSegment {
            object: upload_artifacts(&artifacts, config)?,
            canonical_complete: canonical_complete_manifest(&manifest),
        });
    }
    ensure_identity(source, source_identity)?;
    fs::remove_file(source)?;
    File::open(spool_dir)?.sync_all()?;
    Ok(uploaded)
}

pub(crate) fn read_status(path: &Path) -> Result<Map<String, Value>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            Ok(serde_json::from_slice::<Value>(&fs::read(path)?)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default())
        }
        Ok(_) => bail!("upload status must be a regular non-symlink file"),
        Err(error) => Err(error.into()),
    }
}

/// Upload all closed tapes, continuing past bad segments while returning failure.
pub fn upload_pending(config: &UploadConfig) -> Result<UploadSummary> {
    upload_pending_with(config, archive_source)
}

/// Binary-friendly alias for [`upload_pending`].
pub fn run_upload(config: &UploadConfig) -> Result<UploadSummary> {
    upload_pending(config)
}

pub async fn run_upload_async(config: UploadConfig) -> Result<UploadSummary> {
    upload_pending_async_with(&config, archive_source).await
}

async fn upload_pending_async_with<F>(config: &UploadConfig, archive: F) -> Result<UploadSummary>
where
    F: Fn(&Path, &UploadConfig) -> Result<Vec<UploadedSegment>> + Send + Sync + 'static,
{
    config.validate()?;
    ensure_canonical_directory(&config.spool_dir)?;
    let mut status = read_status(&config.spool_dir.join("upload-status.json"))?;
    let archive = Arc::new(archive);
    let semaphore = Arc::new(Semaphore::new(config.max_concurrent_uploads));
    let sources = discover_rotated_tapes(&config.spool_dir)?;
    let mut tasks = Vec::with_capacity(sources.len());
    for source in sources {
        let permit = semaphore.clone().acquire_owned().await?;
        let archive = archive.clone();
        let config = config.clone();
        let task_source = source.clone();
        let task = tokio::task::spawn_blocking(move || {
            let result = archive(&task_source, &config);
            drop(permit);
            result
        });
        tasks.push((source, task));
    }

    let mut failures = Vec::new();
    let mut uploaded_segments = 0_usize;
    let mut canonical_uploaded_segments = 0_usize;
    for (source, task) in tasks {
        match task.await {
            Ok(result) => record_upload_result(
                &source,
                result,
                &mut status,
                &mut failures,
                &mut uploaded_segments,
                &mut canonical_uploaded_segments,
            ),
            Err(error) => failures.push(json!({
                "source": source.file_name().and_then(|name| name.to_str()),
                "error": format!("async upload task failed: {error}"),
            })),
        }
    }
    finalize_upload_status(
        &config.spool_dir,
        status,
        failures,
        uploaded_segments,
        canonical_uploaded_segments,
    )
}

fn record_upload_result(
    source: &Path,
    result: Result<Vec<UploadedSegment>>,
    status: &mut Map<String, Value>,
    failures: &mut Vec<Value>,
    uploaded_segments: &mut usize,
    canonical_uploaded_segments: &mut usize,
) {
    match result {
        Ok(uploaded) if !uploaded.is_empty() => {
            *uploaded_segments += uploaded.len();
            *canonical_uploaded_segments += uploaded
                .iter()
                .filter(|segment| segment.canonical_complete)
                .count();
            status.insert("last_success_at".to_owned(), json!(utc_now()));
            status.insert(
                "last_uploaded_object".to_owned(),
                json!(&uploaded.last().expect("non-empty upload result").object),
            );
        }
        Ok(_) => failures.push(json!({
            "source": source.file_name().and_then(|name| name.to_str()),
            "error": "closed tape produced no upload artifacts",
        })),
        Err(error) => {
            eprintln!(
                "Polymarket tape upload failed for {}: {error}",
                source.display()
            );
            failures.push(json!({
                "source": source.file_name().and_then(|name| name.to_str()),
                "error": error.to_string(),
            }));
        }
    }
}

fn finalize_upload_status(
    spool_dir: &Path,
    mut status: Map<String, Value>,
    failures: Vec<Value>,
    uploaded_segments: usize,
    canonical_uploaded_segments: usize,
) -> Result<UploadSummary> {
    let now = utc_now();
    let pending = discover_rotated_tapes(spool_dir)?.len();
    status.insert("updated_at".to_owned(), json!(now));
    status.insert("uploaded_segments".to_owned(), json!(uploaded_segments));
    status.insert(
        "canonical_uploaded_segments".to_owned(),
        json!(canonical_uploaded_segments),
    );
    status.insert("pending_segments".to_owned(), json!(pending));
    status.insert("failed_segments".to_owned(), Value::Array(failures.clone()));
    status.insert(
        "last_error_at".to_owned(),
        if failures.is_empty() {
            Value::Null
        } else {
            json!(utc_now())
        },
    );
    status.insert(
        "last_error".to_owned(),
        failures
            .last()
            .and_then(|failure| failure.get("error"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    atomic_json(
        &spool_dir.join("upload-status.json"),
        &Value::Object(status),
    )?;
    if failures.is_empty() {
        Ok(UploadSummary {
            uploaded_segments,
            canonical_uploaded_segments,
        })
    } else {
        bail!(
            "{} closed Polymarket tape segment(s) failed",
            failures.len()
        )
    }
}

fn upload_pending_with<F>(config: &UploadConfig, mut archive: F) -> Result<UploadSummary>
where
    F: FnMut(&Path, &UploadConfig) -> Result<Vec<UploadedSegment>>,
{
    config.validate()?;
    ensure_canonical_directory(&config.spool_dir)?;
    let mut status = read_status(&config.spool_dir.join("upload-status.json"))?;
    let mut failures = Vec::new();
    let mut uploaded_segments = 0_usize;
    let mut canonical_uploaded_segments = 0_usize;
    for source in discover_rotated_tapes(&config.spool_dir)? {
        record_upload_result(
            &source,
            archive(&source, config),
            &mut status,
            &mut failures,
            &mut uploaded_segments,
            &mut canonical_uploaded_segments,
        );
    }
    finalize_upload_status(
        &config.spool_dir,
        status,
        failures,
        uploaded_segments,
        canonical_uploaded_segments,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;
    use std::process::ExitStatus;

    struct TestDir {
        _temp: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let temp = tempfile::Builder::new()
                .prefix("monday-polymarket-upload-test-")
                .tempdir()
                .unwrap();
            let path = fs::canonicalize(temp.path()).unwrap();
            Self { _temp: temp, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn record(sequence: u64, recorded_at: &str, update: Value) -> Value {
        json!({"sequence": sequence, "recorded_at": recorded_at, "update": update})
    }

    fn quote_record(sequence: u64, recorded_at: &str, token_id: &str) -> Value {
        record(
            sequence,
            recorded_at,
            json!({
                "kind": "quote", "token_id": token_id, "bid": "0.49", "ask": "0.51",
                "bid_size": "10", "ask_size": "11",
                "bid_levels": [{"price": "0.49", "size": "10"}],
                "ask_levels": [{"price": "0.51", "size": "11"}],
                "request_status": "success", "collection_result": "executable",
                "ts": recorded_at,
            }),
        )
    }

    fn sample_rows() -> Vec<Value> {
        vec![
            record(
                0,
                "2026-07-15T01:00:00.000000000Z",
                json!({
                    "kind": "event_discovered", "event_id": "event-1", "symbol": "BTCUSDT",
                    "up_token": "up-1", "down_token": "down-1",
                    "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                    "price_to_beat": "100", "resolved_up_won": null,
                }),
            ),
            record(
                1,
                "2026-07-15T01:00:01.000000000Z",
                json!({
                    "kind": "quote", "token_id": "up-1", "bid": "0.49", "ask": "0.51",
                    "bid_size": "10", "ask_size": "11",
                    "bid_levels": [{"price": "0.49", "size": "10"}],
                    "ask_levels": [{"price": "0.51", "size": "11"}],
                    "request_status": "success", "collection_result": "executable",
                    "ts": "2026-07-15T01:00:01Z",
                }),
            ),
            record(
                2,
                "2026-07-15T01:00:02.000000000Z",
                json!({
                    "kind": "reference_price", "symbol": "BTCUSDT", "source": "binance",
                    "asset_class": "crypto", "price": "100", "full_accuracy_value": null,
                    "is_carried_forward": false, "ts": "2026-07-15T01:00:02Z",
                }),
            ),
        ]
    }

    #[test]
    fn oss_runner_keeps_machine_readable_stdout_clean() {
        let root = TestDir::new();
        let captured = root.path().join("child.stdout");
        let output = File::create(&captured).unwrap();
        let mut command = Command::new("sh");
        command
            .args(["-c", "printf 'ossutil progress'"])
            .stdout(Stdio::from(output));

        run_checked(&mut command, Duration::from_secs(1)).unwrap();

        assert_eq!(fs::read(captured).unwrap(), b"");
    }

    #[test]
    fn only_explicit_not_found_errors_are_retryable() {
        assert!(is_retryable_download_failure(&anyhow!(
            "child process exited with status code: 404"
        )));
        assert!(is_retryable_download_failure(&anyhow!(
            "child process exited with NoSuchKey"
        )));
        assert!(!is_retryable_download_failure(&anyhow!(
            "child process exited with config version 404"
        )));
    }

    fn valid_v2_trade_update() -> Value {
        json!({
            "kind": "polymarket_trade",
            "record_id": "6a476c3be58fcb8d789224feba3e079f372bc933e838cd0dbb76435d4fad9cbe",
            "record_id_version": "v2",
            "market_id": "market-1",
            "condition_id": "0xcondition",
            "token_id": "up-token",
            "symbol": "BTCUSDT",
            "market_window_secs": 300,
            "side": "BUY",
            "size": "10.0",
            "price": "0.780",
            "trade_ts": "2026-07-15T03:09:55Z",
            "trade_ts_unix": 1_784_084_995,
            "transaction_hash": "0xtx",
            "proxy_wallet": "0xwallet",
            "outcome": "Up",
            "outcome_index": 0,
            "source": "polymarket_data_api",
            "received_at": "2026-07-15T03:10:00Z",
            "trade": {
                "transactionHash": "0xtx",
                "conditionId": "0xcondition",
                "asset": "up-token",
                "side": "BUY",
                "timestamp": 1_784_084_995,
                "proxyWallet": "0xwallet",
                "size": "10.0",
                "price": "0.780",
                "outcome": "Up",
                "outcomeIndex": 0,
                "sourceOnlyField": {"preserved": true},
            },
        })
    }

    fn valid_market_metadata_update() -> Value {
        json!({
            "kind": "market_metadata",
            "market_id": "market-1",
            "condition_id": "0xcondition",
            "symbol": "BTCUSDT",
            "market_window_secs": 300,
            "source": "gamma_api",
            "retrieved_at": "2026-07-15T03:00:00Z",
            "market": {
                "id": "market-1",
                "conditionId": "0xcondition",
                "question": "Bitcoin Up or Down - 5 minutes",
                "slug": "btc-updown-5m-1784084400",
                "startDate": "2026-07-15T03:00:00Z",
                "endDate": "2026-07-15T03:05:00Z",
                "clobTokenIds": "[\"up-token\",\"down-token\"]",
                "outcomes": "[\"Up\",\"Down\"]",
                "makerBaseFee": 1000,
                "takerBaseFee": 1000,
            },
        })
    }

    fn valid_market_settlement_update() -> Value {
        let mut update = valid_market_metadata_update();
        update["kind"] = json!("market_settlement");
        update["winning_token_id"] = json!("up-token");
        update["winning_outcome"] = json!("Up");
        update["resolved_up_won"] = json!(true);
        update["resolution_source"] = json!("gamma_api_closed_market");
        update["market"]["closed"] = json!(true);
        update["market"]["outcomePrices"] = json!("[\"0.999\",\"0.001\"]");
        update
    }

    #[rustfmt::skip]
    fn valid_trade_completion_update() -> Value {
        let record_id = valid_v2_trade_update()["record_id"].as_str().unwrap().to_owned();
        json!({
            "kind":TRADE_COMPLETION_KIND,"market_id":"market-1","condition_id":"0xcondition","symbol":"BTCUSDT","market_window_secs":300,
            "record_id_version":"v2","trade_count":1,"trade_record_ids_sha256":trade_record_ids_sha256([record_id.as_str()]),
            "source":"polymarket_data_api","retrieved_at":"2026-07-15T03:10:00Z","completeness_basis":TRADE_COMPLETION_BASIS,
            "pagination_exhausted":true,"settlement_observed":true,"malformed_trade_rows":0,"finalization_lag_secs":60,"stable_polls_required":2,
        })
    }

    fn write_tape(root: &Path, name: &str, rows: &[Value]) -> PathBuf {
        let path = root.join(name);
        let mut file = File::create(&path).unwrap();
        for row in rows {
            serde_json::to_writer(&mut file, row).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.sync_all().unwrap();
        path
    }

    fn config(root: &Path) -> UploadConfig {
        UploadConfig {
            spool_dir: root.to_path_buf(),
            dataset: "crypto_expiry".to_owned(),
            quote_depth_levels: 1,
            quote_sample_ms: 1_000,
            bucket: "bucket".to_owned(),
            endpoint: "endpoint".to_owned(),
            region: "region".to_owned(),
            profile: "profile".to_owned(),
            zstd_timeout: Duration::from_secs(30),
            oss_timeout: Duration::from_secs(30),
            max_concurrent_uploads: 2,
            zstd_threads: 0,
            oss_parallel: 8,
            oss_part_size: "32Mi".to_owned(),
        }
    }

    #[test]
    fn rejects_out_of_range_upload_concurrency() {
        let root = TestDir::new();
        let mut config = config(root.path());
        config.max_concurrent_uploads = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("max concurrent uploads must be between 1 and 4"));
        config.max_concurrent_uploads = MAX_CONCURRENT_UPLOADS + 1;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("max concurrent uploads must be between 1 and 4"));
    }

    #[test]
    fn reserves_success_readback_budget_before_publishing_marker() {
        assert_eq!(
            remaining_success_readback_budget(Duration::from_secs(1)).unwrap(),
            OSS_READBACK_MAX_WALL_CLOCK - Duration::from_secs(1)
        );
        assert!(remaining_success_readback_budget(OSS_READBACK_MAX_WALL_CLOCK).is_err());
    }

    fn command_args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn zstd_command_defaults_to_auto_threads() {
        let root = TestDir::new();
        let config = config(root.path());
        let args = command_args(&zstd_command(Path::new("tape.ndjson"), &config));
        assert_eq!(args, ["-q", "-T0", "-3", "-c", "tape.ndjson"]);
    }

    #[test]
    fn zstd_command_threads_are_configurable() {
        let root = TestDir::new();
        let mut config = config(root.path());
        config.zstd_threads = 16;
        let args = command_args(&zstd_command(Path::new("tape.ndjson"), &config));
        assert_eq!(args, ["-q", "-T16", "-3", "-c", "tape.ndjson"]);
    }

    #[test]
    fn oss_copy_command_includes_multipart_tuning() {
        let root = TestDir::new();
        let config = config(root.path());
        let args = command_args(&oss_copy_command("src", "dst", &config));
        assert_eq!(&args[..4], ["ossutil", "cp", "src", "dst"]);
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--parallel", "8"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--part-size", "32Mi"]));
    }

    #[test]
    fn oss_copy_command_tuning_is_configurable() {
        let root = TestDir::new();
        let mut config = config(root.path());
        config.oss_parallel = 12;
        config.oss_part_size = "64Mi".to_owned();
        let args = command_args(&oss_copy_command("src", "dst", &config));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--parallel", "12"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--part-size", "64Mi"]));
    }

    #[test]
    fn oss_upload_command_keeps_no_clobber_with_tuning() {
        let root = TestDir::new();
        let config = config(root.path());
        let args = command_args(&oss_upload_command("src", "dst", &config));
        assert!(args.iter().any(|arg| arg == "--ignore-existing"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--parallel", "8"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--part-size", "32Mi"]));
    }

    #[test]
    fn rejects_invalid_oss_copy_tuning() {
        let root = TestDir::new();
        let mut config = config(root.path());
        config.oss_parallel = 0;
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("oss parallel must be at least 1"));
        config.oss_parallel = 8;
        config.oss_part_size = " ".to_owned();
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("oss part size must be non-empty"));
    }

    #[test]
    fn staging_root_creation_is_idempotent_under_concurrency() {
        let root = TestDir::new();
        let staging_root = root.path().join(".upload-staging");
        std::thread::scope(|scope| {
            let first = scope.spawn(|| ensure_upload_staging_root(&staging_root));
            let second = scope.spawn(|| ensure_upload_staging_root(&staging_root));
            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });
        assert!(staging_root.is_dir());
    }

    #[test]
    fn validates_manifest_quality_and_reference_fields() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();
        assert_eq!(manifest["events"], 3);
        assert_eq!(manifest["event_types"]["quote"], 1);
        assert_eq!(manifest["quality"]["max_bid_levels"], 1);
        assert_eq!(manifest["field_non_null"]["quote"]["bid"], 1);
        assert_eq!(manifest["field_non_null"]["reference_price"]["price"], 1);
    }

    #[test]
    fn classifies_quote_executability() {
        let root = TestDir::new();
        let mut executable = sample_rows()[1].clone();
        executable["sequence"] = json!(0);
        let mut missing_ask_size = executable.clone();
        missing_ask_size["sequence"] = json!(1);
        missing_ask_size["update"]["ask_size"] = Value::Null;
        missing_ask_size["update"]["ask_levels"] = json!([]);
        missing_ask_size["update"]["collection_result"] = json!("incomplete");
        let mut one_sided = executable.clone();
        one_sided["sequence"] = json!(2);
        one_sided["update"]["ask"] = Value::Null;
        one_sided["update"]["ask_size"] = Value::Null;
        one_sided["update"]["ask_levels"] = json!([]);
        one_sided["update"]["collection_result"] = json!("one_sided");
        let mut empty = executable.clone();
        empty["sequence"] = json!(3);
        empty["recorded_at"] = json!("2026-07-15T01:00:04Z");
        for field in ["bid", "ask", "bid_size", "ask_size"] {
            empty["update"][field] = Value::Null;
        }
        empty["update"]["bid_levels"] = json!([]);
        empty["update"]["ask_levels"] = json!([]);
        empty["update"]["collection_result"] = json!("empty");
        let mut non_executable = empty.clone();
        non_executable["sequence"] = json!(4);
        non_executable["recorded_at"] = json!("2026-07-15T01:00:05Z");
        non_executable["update"]["bid_levels"] = json!([{"price": "0.01", "size": "5"}]);
        non_executable["update"]["ask_levels"] = json!([{"price": "0.99", "size": "6"}]);
        non_executable["update"]["collection_result"] = json!("non_executable");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[
                executable,
                missing_ask_size,
                one_sided,
                empty,
                non_executable,
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quality"]["executable_quotes"], 1);
        assert_eq!(manifest["quality"]["missing_bid_size"], 0);
        assert_eq!(manifest["quality"]["missing_ask_size"], 1);
        assert_eq!(manifest["quality"]["one_sided_quotes"], 1);
        assert_eq!(manifest["quality"]["empty_quotes"], 1);
        assert_eq!(manifest["quality"]["non_executable_quotes"], 1);
        assert_eq!(manifest["quality"]["incomplete_quotes"], 1);
        assert_eq!(manifest["quality"]["max_quote_latency_ms"], 4_000);
        assert_eq!(
            manifest["quality"]["executable_quotes"].as_u64().unwrap()
                + manifest["quality"]["one_sided_quotes"].as_u64().unwrap()
                + manifest["quality"]["empty_quotes"].as_u64().unwrap()
                + manifest["quality"]["non_executable_quotes"]
                    .as_u64()
                    .unwrap()
                + manifest["quality"]["incomplete_quotes"].as_u64().unwrap(),
            manifest["event_types"]["quote"].as_u64().unwrap()
        );
    }

    #[test]
    fn rejects_quote_without_explicit_collection_status() {
        let root = TestDir::new();
        let mut quote = sample_rows()[1].clone();
        quote["sequence"] = json!(0);
        quote["update"]
            .as_object_mut()
            .unwrap()
            .remove("request_status");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[quote],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error.to_string().contains("quote requires request_status"));
    }

    #[test]
    fn rejects_non_executable_label_when_tradeable_levels_are_present() {
        let root = TestDir::new();
        let mut quote = sample_rows()[1].clone();
        quote["sequence"] = json!(0);
        for field in ["bid", "ask", "bid_size", "ask_size"] {
            quote["update"][field] = Value::Null;
        }
        quote["update"]["collection_result"] = json!("non_executable");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[quote],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();
        assert!(error
            .to_string()
            .contains("collection_result does not match incomplete"));
    }

    #[test]
    fn retains_request_failure_but_marks_segment_incomplete() {
        let root = TestDir::new();
        let rows = vec![
            sample_rows()[0].clone(),
            record(
                1,
                "2026-07-15T01:00:01Z",
                json!({
                    "kind": "quote_collection_failure", "token_id": "up-1",
                    "request_status": "failure", "collection_result": "api_failure",
                    "request_started_at": "2026-07-15T01:00:00.900Z",
                    "http_status": null, "error_kind": "websocket_connect",
                    "ts": "2026-07-15T01:00:01Z"
                }),
            ),
            record(
                2,
                "2026-07-15T01:00:02Z",
                json!({
                    "kind": "quote_collection_failure", "token_id": "down-1",
                    "request_status": "failure", "collection_result": "api_failure",
                    "request_started_at": "2026-07-15T01:00:01.900Z",
                    "http_status": null, "error_kind": "websocket_receive",
                    "ts": "2026-07-15T01:00:02Z"
                }),
            ),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quality"]["request_attempts"], 2);
        // The websocket_connect row is integrity-domain, and the
        // websocket_receive row never proves a recovery inside this tape, so
        // both count as request failures.
        assert_eq!(manifest["quality"]["request_failures"], 2);
        assert_eq!(manifest["quality"]["transport_reconnects"], 1);
        assert_eq!(manifest["quality"]["max_request_latency_ms"], 100);
        assert_eq!(manifest["quote_coverage_complete"], false);
        assert_eq!(manifest["segment_complete"], false);
    }

    #[test]
    fn transport_reconnects_with_recovery_do_not_block_complete_segment() {
        let root = TestDir::new();
        let rows = vec![
            sample_rows()[0].clone(),
            sample_rows()[1].clone(),
            record(
                2,
                "2026-07-15T01:00:02.000000000Z",
                json!({
                    "kind": "quote", "token_id": "down-1", "bid": "0.49", "ask": "0.51",
                    "bid_size": "10", "ask_size": "11",
                    "bid_levels": [{"price": "0.49", "size": "10"}],
                    "ask_levels": [{"price": "0.51", "size": "11"}],
                    "request_status": "success", "collection_result": "executable",
                    "ts": "2026-07-15T01:00:02Z",
                }),
            ),
            record(
                3,
                "2026-07-15T01:00:03.000000000Z",
                json!({
                    "kind": "quote_collection_failure", "token_id": "up-1",
                    "request_status": "failure", "collection_result": "api_failure",
                    "request_started_at": "2026-07-15T01:00:02.900Z",
                    "http_status": null, "error_kind": "websocket_receive",
                    "ts": "2026-07-15T01:00:03Z"
                }),
            ),
            record(
                4,
                "2026-07-15T01:00:04.000000000Z",
                json!({
                    "kind": "quote_collection_failure", "token_id": "down-1",
                    "request_status": "failure", "collection_result": "api_failure",
                    "request_started_at": "2026-07-15T01:00:03.900Z",
                    "http_status": null, "error_kind": "transport_reconnect",
                    "ts": "2026-07-15T01:00:04Z"
                }),
            ),
            record(
                5,
                "2026-07-15T01:00:05.000000000Z",
                json!({
                    "kind": "quote", "token_id": "up-1", "bid": "0.50", "ask": "0.52",
                    "bid_size": "10", "ask_size": "11",
                    "bid_levels": [{"price": "0.50", "size": "10"}],
                    "ask_levels": [{"price": "0.52", "size": "11"}],
                    "request_status": "success", "collection_result": "executable",
                    "ts": "2026-07-15T01:00:05Z",
                }),
            ),
            record(
                6,
                "2026-07-15T01:00:06.000000000Z",
                json!({
                    "kind": "quote", "token_id": "down-1", "bid": "0.48", "ask": "0.50",
                    "bid_size": "10", "ask_size": "11",
                    "bid_levels": [{"price": "0.48", "size": "10"}],
                    "ask_levels": [{"price": "0.50", "size": "11"}],
                    "request_status": "success", "collection_result": "executable",
                    "ts": "2026-07-15T01:00:06Z",
                }),
            ),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quality"]["request_attempts"], 6);
        assert_eq!(manifest["quality"]["request_failures"], 0);
        assert_eq!(manifest["quality"]["transport_reconnects"], 2);
        assert_eq!(manifest["quote_coverage_complete"], true);
        assert_eq!(manifest["segment_complete"], true);
        assert_eq!(manifest["canonical"], true);
    }

    #[test]
    fn transport_reconnect_without_recovery_counts_as_request_failure() {
        let root = TestDir::new();
        let rows = vec![
            sample_rows()[0].clone(),
            sample_rows()[1].clone(),
            record(
                2,
                "2026-07-15T01:00:02.000000000Z",
                json!({
                    "kind": "quote", "token_id": "down-1", "bid": "0.49", "ask": "0.51",
                    "bid_size": "10", "ask_size": "11",
                    "bid_levels": [{"price": "0.49", "size": "10"}],
                    "ask_levels": [{"price": "0.51", "size": "11"}],
                    "request_status": "success", "collection_result": "executable",
                    "ts": "2026-07-15T01:00:02Z",
                }),
            ),
            record(
                3,
                "2026-07-15T01:00:03.000000000Z",
                json!({
                    "kind": "quote_collection_failure", "token_id": "down-1",
                    "request_status": "failure", "collection_result": "api_failure",
                    "request_started_at": "2026-07-15T01:00:02.900Z",
                    "http_status": null, "error_kind": "websocket_receive",
                    "ts": "2026-07-15T01:00:03Z"
                }),
            ),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quality"]["request_attempts"], 3);
        assert_eq!(manifest["quality"]["request_failures"], 1);
        assert_eq!(manifest["quality"]["transport_reconnects"], 1);
        assert_eq!(manifest["quote_coverage_complete"], false);
        assert_eq!(manifest["segment_complete"], false);
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn quote_collection_failure_does_not_count_as_quote_in_quality_fields() {
        let root = TestDir::new();
        let rows = vec![
            sample_rows()[0].clone(),
            sample_rows()[1].clone(),
            record(
                2,
                "2026-07-15T01:00:02Z",
                json!({
                    "kind": "quote_collection_failure", "token_id": "down-1",
                    "request_status": "failure", "collection_result": "api_failure",
                    "request_started_at": "2026-07-15T01:00:01.900Z",
                    "http_status": 503, "error_kind": "http_status",
                    "ts": "2026-07-15T01:00:02Z"
                }),
            ),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["event_types"]["quote"], 1);
        assert_eq!(manifest["event_types"]["quote_collection_failure"], 1);
        assert_eq!(manifest["quality"]["request_attempts"], 2);
        assert_eq!(manifest["quality"]["request_successes"], 1);
        assert_eq!(manifest["quality"]["request_failures"], 1);
        assert_eq!(manifest["quality"]["transport_reconnects"], 0);
        assert_eq!(manifest["quality"]["executable_quotes"], 1);
        assert_eq!(manifest["quality"]["one_sided_quotes"], 0);
        assert_eq!(manifest["quality"]["empty_quotes"], 0);
        assert_eq!(manifest["quality"]["non_executable_quotes"], 0);
        assert_eq!(manifest["quality"]["incomplete_quotes"], 0);
        assert_eq!(manifest["missing_quote_tokens"], json!(["down-1"]));
        assert_eq!(manifest["missing_quote_attempt_tokens"], json!([]));
        assert_eq!(manifest["quote_coverage_complete"], false);
    }

    #[test]
    fn rejects_quote_collection_failure_without_explicit_status() {
        let root = TestDir::new();
        let mut failure = sample_rows()[0].clone();
        failure["sequence"] = json!(0);
        failure["recorded_at"] = json!("2026-07-15T01:00:01Z");
        failure["update"] = json!({
            "kind": "quote_collection_failure", "token_id": "up-1",
            "collection_result": "api_failure",
            "request_started_at": "2026-07-15T01:00:00.900Z",
            "http_status": null, "error_kind": "websocket_receive",
            "ts": "2026-07-15T01:00:01Z"
        });
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[failure],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error
            .to_string()
            .contains("quote collection failure requires explicit status"));
    }

    #[test]
    fn rejects_quote_collection_failure_with_unknown_error_kind() {
        let root = TestDir::new();
        let mut failure = sample_rows()[0].clone();
        failure["sequence"] = json!(0);
        failure["recorded_at"] = json!("2026-07-15T01:00:01Z");
        failure["update"] = json!({
            "kind": "quote_collection_failure", "token_id": "up-1",
            "request_status": "failure", "collection_result": "api_failure",
            "request_started_at": "2026-07-15T01:00:00.900Z",
            "http_status": null, "error_kind": "dns_failure",
            "ts": "2026-07-15T01:00:01Z"
        });
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[failure],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error
            .to_string()
            .contains("unsupported quote collection error_kind"));
    }

    #[test]
    fn rejects_quote_collection_failure_with_invalid_http_status_pairing() {
        let root = TestDir::new();
        let mut failure = sample_rows()[0].clone();
        failure["sequence"] = json!(0);
        failure["recorded_at"] = json!("2026-07-15T01:00:01Z");
        failure["update"] = json!({
            "kind": "quote_collection_failure", "token_id": "up-1",
            "request_status": "failure", "collection_result": "api_failure",
            "request_started_at": "2026-07-15T01:00:00.900Z",
            "http_status": null, "error_kind": "http_status",
            "ts": "2026-07-15T01:00:01Z"
        });
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[failure],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error
            .to_string()
            .contains("invalid quote collection http_status"));
    }

    #[test]
    fn rejects_quote_collection_failure_with_inverted_request_timing() {
        let root = TestDir::new();
        let mut failure = sample_rows()[0].clone();
        failure["sequence"] = json!(0);
        failure["recorded_at"] = json!("2026-07-15T01:00:01Z");
        failure["update"] = json!({
            "kind": "quote_collection_failure", "token_id": "up-1",
            "request_status": "failure", "collection_result": "api_failure",
            "request_started_at": "2026-07-15T01:00:01.100Z",
            "http_status": null, "error_kind": "websocket_receive",
            "ts": "2026-07-15T01:00:01Z"
        });
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[failure],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error.to_string().contains("invalid quote request timing"));
    }

    #[test]
    fn rejects_quote_collection_failure_without_token_id() {
        let root = TestDir::new();
        let mut failure = sample_rows()[0].clone();
        failure["sequence"] = json!(0);
        failure["recorded_at"] = json!("2026-07-15T01:00:01Z");
        failure["update"] = json!({
            "kind": "quote_collection_failure",
            "request_status": "failure", "collection_result": "api_failure",
            "request_started_at": "2026-07-15T01:00:00.900Z",
            "http_status": null, "error_kind": "websocket_receive",
            "ts": "2026-07-15T01:00:01Z"
        });
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[failure],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error.to_string().contains("token_id"));
    }

    #[test]
    fn rejects_quote_source_time_regression_per_token() {
        let root = TestDir::new();
        let mut first = sample_rows()[1].clone();
        first["sequence"] = json!(0);
        first["recorded_at"] = json!("2026-07-15T01:01:00Z");
        first["update"]["ts"] = json!("2026-07-15T01:01:00Z");
        let mut second = first.clone();
        second["sequence"] = json!(1);
        second["recorded_at"] = json!("2026-07-15T01:01:30Z");
        second["update"]["ts"] = json!("2026-07-15T01:00:00Z");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[first, second],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error
            .to_string()
            .contains("quote source time moved backwards by 60000ms"));
    }

    #[test]
    fn tolerates_bounded_quote_source_time_jitter_per_token() {
        let root = TestDir::new();
        let mut first = sample_rows()[1].clone();
        first["sequence"] = json!(0);
        first["recorded_at"] = json!("2026-07-15T01:00:02Z");
        first["update"]["ts"] = json!("2026-07-15T01:00:02Z");
        let mut second = first.clone();
        second["sequence"] = json!(1);
        second["recorded_at"] = json!("2026-07-15T01:00:03Z");
        second["update"]["ts"] = json!("2026-07-15T01:00:01Z");
        let mut third = first.clone();
        third["sequence"] = json!(2);
        third["recorded_at"] = json!("2026-07-15T01:00:04Z");
        third["update"]["ts"] = json!("2026-07-15T01:00:03Z");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[first, second, third],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(
            manifest["quality"]["tolerated_quote_source_regressions"],
            json!(1)
        );
    }

    #[test]
    fn rejects_quote_without_source_time() {
        let root = TestDir::new();
        let mut quote = sample_rows()[1].clone();
        quote["sequence"] = json!(0);
        quote["update"].as_object_mut().unwrap().remove("ts");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[quote],
        );

        let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();

        assert!(error.to_string().contains("quote requires ts"));
    }

    #[test]
    fn marks_missing_event_token_quote_coverage() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quote_coverage_complete"], false);
        assert_eq!(manifest["missing_quote_tokens"], json!(["down-1"]));
        assert_eq!(manifest["segment_complete"], false);
    }

    #[test]
    fn excludes_events_expired_before_discovery_from_quote_coverage() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[record(
                0,
                "2026-07-15T01:10:00Z",
                json!({
                    "kind": "event_discovered", "event_id": "event-1", "symbol": "BTCUSDT",
                    "up_token": "up-1", "down_token": "down-1",
                    "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                    "price_to_beat": "100", "resolved_up_won": null,
                }),
            )],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(
            manifest["expired_before_discovery_tokens"],
            json!(["down-1", "up-1"])
        );
        assert_eq!(manifest["missing_quote_tokens"], json!([]));
        assert_eq!(manifest["missing_quote_attempt_tokens"], json!([]));
        assert_eq!(manifest["quote_coverage_complete"], true);
        assert_eq!(manifest["segment_complete"], true);
        assert_eq!(manifest["canonical"], true);
    }

    #[test]
    fn keeps_future_skewed_discovery_in_quote_coverage() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[record(
                0,
                "2026-07-15T01:10:00Z",
                json!({
                    "kind": "event_discovered", "event_id": "event-1", "symbol": "BTCUSDT",
                    "up_token": "up-1", "down_token": "down-1",
                    "end_time": "2026-07-15T01:09:00Z", "window_secs": 300,
                }),
            )],
        );
        let validation_time = DateTime::parse_from_rfc3339("2026-07-15T01:08:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let manifest =
            scan_tape_with_identity_at(&tape, "crypto_expiry", 1, 1_000, validation_time)
                .unwrap()
                .manifest;

        assert_eq!(manifest["expired_before_discovery_tokens"], json!([]));
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn keeps_incomplete_or_equal_event_token_pairs_noncanonical() {
        for (up_token, down_token) in [(Some("up-1"), None), (Some("same"), Some("same"))] {
            let root = TestDir::new();
            let mut rows = vec![record(
                0,
                "2026-07-15T01:10:00Z",
                json!({
                    "kind": "event_discovered", "event_id": "event-1", "symbol": "BTCUSDT",
                    "up_token": up_token, "down_token": down_token,
                    "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                }),
            )];
            for (index, token) in [up_token, down_token]
                .into_iter()
                .flatten()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .enumerate()
            {
                rows.push(quote_record(
                    index as u64 + 1,
                    "2026-07-15T01:10:01Z",
                    token,
                ));
            }
            let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

            let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

            assert_eq!(manifest["quote_coverage_complete"], true);
            assert_eq!(manifest["lifecycle_integrity_complete"], false);
            assert_eq!(manifest["expired_before_discovery_tokens"], json!([]));
            assert_eq!(manifest["canonical"], false);
        }
    }

    #[test]
    fn keeps_missing_end_time_noncanonical_with_complete_quotes() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[
                record(
                    0,
                    "2026-07-15T01:10:00Z",
                    json!({
                        "kind": "event_discovered", "event_id": "event-1", "symbol": "BTCUSDT",
                        "up_token": "up-1", "down_token": "down-1",
                        "end_time": null, "window_secs": 300,
                    }),
                ),
                quote_record(1, "2026-07-15T01:10:01Z", "up-1"),
                quote_record(2, "2026-07-15T01:10:02Z", "down-1"),
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quote_coverage_complete"], true);
        assert_eq!(manifest["lifecycle_integrity_complete"], false);
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn keeps_conflicting_event_expiration_time_noncanonical() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[
                record(
                    0,
                    "2026-07-15T01:10:00Z",
                    json!({
                        "kind": "event_discovered", "event_id": "event-1", "symbol": "BTCUSDT",
                        "up_token": "up-1", "down_token": "down-1",
                        "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                    }),
                ),
                record(
                    1,
                    "2026-07-15T01:10:01Z",
                    json!({
                        "kind": "event_expired", "event_id": "event-1",
                        "end_time": "2026-07-15T01:06:00Z",
                    }),
                ),
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quote_coverage_complete"], true);
        assert_eq!(manifest["lifecycle_integrity_complete"], false);
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn classifies_only_active_tokens_as_mixed_lifecycle_obligations() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[
                record(
                    0,
                    "2026-07-15T01:10:00Z",
                    json!({
                        "kind": "event_discovered", "event_id": "expired", "symbol": "BTCUSDT",
                        "up_token": "expired-up", "down_token": "expired-down",
                        "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                    }),
                ),
                record(
                    1,
                    "2026-07-15T01:10:01Z",
                    json!({
                        "kind": "event_discovered", "event_id": "active", "symbol": "BTCUSDT",
                        "up_token": "expired-up", "down_token": "active-down",
                        "end_time": "2026-07-15T01:15:00Z", "window_secs": 300,
                    }),
                ),
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(
            manifest["expired_before_discovery_tokens"],
            json!(["expired-down"])
        );
        assert_eq!(
            manifest["missing_quote_tokens"],
            json!(["active-down", "expired-up"])
        );
        assert_eq!(
            manifest["missing_quote_attempt_tokens"],
            json!(["active-down", "expired-up"])
        );
        assert_eq!(manifest["quote_coverage_complete"], false);
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn keeps_conflicting_token_lifecycle_noncanonical_with_complete_quotes() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[
                record(
                    0,
                    "2026-07-15T01:10:00Z",
                    json!({
                        "kind": "event_discovered", "event_id": "expired", "symbol": "BTCUSDT",
                        "up_token": "shared", "down_token": "expired-down",
                        "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                    }),
                ),
                record(
                    1,
                    "2026-07-15T01:10:01Z",
                    json!({
                        "kind": "event_discovered", "event_id": "active", "symbol": "BTCUSDT",
                        "up_token": "shared", "down_token": "active-down",
                        "end_time": "2026-07-15T01:15:00Z", "window_secs": 300,
                    }),
                ),
                quote_record(2, "2026-07-15T01:10:02Z", "shared"),
                quote_record(3, "2026-07-15T01:10:03Z", "active-down"),
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["quote_coverage_complete"], true);
        assert_eq!(manifest["lifecycle_integrity_complete"], false);
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn keeps_ambiguous_and_missing_end_times_in_quote_coverage() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[
                record(
                    0,
                    "2026-07-15T01:10:00Z",
                    json!({
                        "kind": "event_discovered", "event_id": "equal", "symbol": "BTCUSDT",
                        "up_token": "equal-up", "down_token": "equal-down",
                        "end_time": "2026-07-15T01:10:00Z", "window_secs": 300,
                    }),
                ),
                record(
                    1,
                    "2026-07-15T01:10:01Z",
                    json!({
                        "kind": "event_discovered", "event_id": "missing", "symbol": "BTCUSDT",
                        "up_token": "missing-up", "down_token": "missing-down",
                        "end_time": null, "window_secs": 300,
                    }),
                ),
                record(
                    2,
                    "2026-07-15T01:10:02Z",
                    json!({
                        "kind": "event_discovered", "event_id": "invalid", "symbol": "BTCUSDT",
                        "up_token": "invalid-up", "down_token": "invalid-down",
                        "end_time": "not-a-timestamp", "window_secs": 300,
                    }),
                ),
                record(
                    3,
                    "2026-07-15T01:10:03Z",
                    json!({
                        "kind": "event_discovered", "symbol": "BTCUSDT",
                        "up_token": "unidentified-up", "down_token": "unidentified-down",
                        "end_time": "2026-07-15T01:05:00Z", "window_secs": 300,
                    }),
                ),
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();
        let required = json!([
            "equal-down",
            "equal-up",
            "invalid-down",
            "invalid-up",
            "missing-down",
            "missing-up",
            "unidentified-down",
            "unidentified-up"
        ]);

        assert_eq!(manifest["expired_before_discovery_tokens"], json!([]));
        assert_eq!(manifest["missing_quote_tokens"], required);
        assert_eq!(manifest["missing_quote_attempt_tokens"], required);
        assert_eq!(manifest["quote_coverage_complete"], false);
        assert_eq!(manifest["canonical"], false);
    }

    #[test]
    fn records_event_local_trade_completion_identity() {
        let root = TestDir::new();
        let rows = vec![
            record(0, "2026-07-15T03:00:00Z", valid_market_metadata_update()),
            record(1, "2026-07-15T03:10:00Z", valid_v2_trade_update()),
            record(2, "2026-07-15T03:10:00Z", valid_market_settlement_update()),
            record(3, "2026-07-15T03:10:00Z", valid_trade_completion_update()),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T030000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry_reference", 0, 0).unwrap();
        let completion = &manifest["trade_completions"]["market-1"];

        assert_eq!(completion["trade_count"], 1);
        assert_eq!(completion["completion_sequence"], 3);
        assert_eq!(completion["condition_id"], "0xcondition");
        assert_eq!(manifest["event_types"][TRADE_COMPLETION_KIND], 1);
    }

    #[test]
    fn rejects_trade_completion_identity_that_contradicts_metadata() {
        let root = TestDir::new();
        let mut completion = valid_trade_completion_update();
        completion["symbol"] = json!("ETHUSDT");
        completion["market_window_secs"] = json!(900);
        let rows = vec![
            record(0, "2026-07-15T03:00:00Z", valid_market_metadata_update()),
            record(1, "2026-07-15T03:10:00Z", completion),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T030000.ndjson", &rows);

        let error = scan_tape(&tape, "crypto_expiry_reference", 0, 0).unwrap_err();

        assert!(error.to_string().contains("contradicts market metadata"));
    }

    #[test]
    fn recovery_validation_requires_zero_sequence_and_bounded_recording_time() {
        let root = TestDir::new();
        let recovery_time = DateTime::parse_from_rfc3339("2026-07-15T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let update = valid_market_metadata_update();
        let nonzero = write_tape(
            root.path(),
            "nonzero.ndjson",
            &[record(1, "2026-07-15T01:00:00Z", update.clone())],
        );
        let error = validate_reference_tape_for_recovery(&nonzero, recovery_time).unwrap_err();
        assert!(error.to_string().contains("must start at sequence 0"));

        let future = write_tape(
            root.path(),
            "future.ndjson",
            &[record(0, "2026-07-15T01:05:01Z", update)],
        );
        let error = validate_reference_tape_for_recovery(&future, recovery_time).unwrap_err();
        assert!(error.to_string().contains("more than 300s in the future"));

        let wrong_kind = write_tape(
            root.path(),
            "wrong-kind.ndjson",
            &[record(
                0,
                "2026-07-15T01:00:00Z",
                json!({"kind": "reference_price", "symbol": "BTCUSDT", "price": "100"}),
            )],
        );
        let error = validate_reference_tape_for_recovery(&wrong_kind, recovery_time).unwrap_err();
        assert!(error.to_string().contains("unsupported update kind"));
    }

    #[test]
    fn rejects_sequence_gap_incomplete_numeric_and_depth_errors() {
        let root = TestDir::new();
        let mut rows = sample_rows();
        rows[1]["sequence"] = json!(2);
        let gap = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);
        assert!(scan_tape(&gap, "crypto_expiry", 1, 1_000)
            .unwrap_err()
            .to_string()
            .contains("sequence gap"));

        let incomplete = root.path().join("market-updates.20260715T020000.ndjson");
        fs::write(&incomplete, b"{\"sequence\":0").unwrap();
        assert!(scan_tape(&incomplete, "crypto_expiry", 1, 1_000)
            .unwrap_err()
            .to_string()
            .contains("incomplete record"));

        let mut rows = sample_rows();
        rows[1]["update"]["bid"] = json!("not-a-number");
        let numeric = write_tape(root.path(), "market-updates.20260715T030000.ndjson", &rows);
        assert!(scan_tape(&numeric, "crypto_expiry", 1, 1_000)
            .unwrap_err()
            .to_string()
            .contains("bid must be numeric"));

        let mut rows = sample_rows();
        rows[1]["update"]["bid_levels"][0]["size"] = Value::Null;
        let depth = write_tape(root.path(), "market-updates.20260715T040000.ndjson", &rows);
        assert!(scan_tape(&depth, "crypto_expiry", 1, 1_000)
            .unwrap_err()
            .to_string()
            .contains("requires price and size"));
    }

    #[test]
    fn full_book_context_and_reference_records_match_python_contract() {
        let root = TestDir::new();
        let mut rows = sample_rows();
        rows[1]["update"]["bid_levels"]
            .as_array_mut()
            .unwrap()
            .push(json!({"price": "0.48", "size": "12"}));
        let full = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);
        let manifest = scan_tape(&full, "crypto_expiry", 0, 1_000).unwrap();
        assert_eq!(manifest["venue_depth_complete"], true);
        assert_eq!(manifest["quality"]["max_bid_levels"], 2);

        let mut quote = sample_rows().remove(1);
        quote["sequence"] = json!(0);
        let contextless = write_tape(
            root.path(),
            "market-updates.20260715T020000.ndjson",
            &[quote],
        );
        let manifest = scan_tape(&contextless, "crypto_expiry", 1, 1_000).unwrap();
        assert_eq!(manifest["event_context_complete"], false);
        assert_eq!(manifest["canonical"], false);
        assert_eq!(manifest["segment_complete"], false);
        assert!(manifest["replay_scope"]
            .as_str()
            .unwrap()
            .contains("requires_prior_event_context"));

        let reference = vec![
            record(0, "2026-07-15T03:00:00Z", valid_market_metadata_update()),
            record(1, "2026-07-15T03:10:00Z", valid_v2_trade_update()),
        ];
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &reference,
        );
        let manifest = scan_tape(&tape, "crypto_expiry_reference", 0, 0).unwrap();
        assert_eq!(manifest["replay_scope"], "complete_reference_hour_segment");
        assert_eq!(manifest["record_id_versions"], json!(["v2"]));
        assert_eq!(
            manifest["source_field_non_null"]["market_metadata"]["makerBaseFee"],
            1
        );

        for (stamp, invalid_size) in [("040000", Value::Null), ("050000", json!(-1))] {
            let mut invalid = reference.clone();
            invalid[1]["update"]["size"] = invalid_size.clone();
            invalid[1]["update"]["trade"]["size"] = invalid_size;
            let tape = write_tape(
                root.path(),
                &format!("market-updates.20260715T{stamp}.ndjson"),
                &invalid,
            );
            assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
                .unwrap_err()
                .to_string()
                .contains("size must be positive"));
        }
    }

    #[test]
    fn rejects_market_metadata_wrapper_id_contradiction() {
        let root = TestDir::new();
        let mut update = valid_market_metadata_update();
        update["market_id"] = json!("different-market");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:00:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_metadata market_id does not match raw market"));
    }

    #[test]
    fn rejects_unsupported_market_metadata_symbol() {
        let root = TestDir::new();
        let mut update = valid_market_metadata_update();
        update["symbol"] = json!("ADAUSDT");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:00:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_metadata symbol is unsupported or contradicts raw market"));
    }

    #[test]
    fn rejects_market_metadata_window_contradiction() {
        let root = TestDir::new();
        let mut update = valid_market_metadata_update();
        update["market_window_secs"] = json!(900);
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:00:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_metadata window is unsupported or contradicts raw market"));
    }

    #[test]
    fn rejects_market_metadata_without_two_unique_tokens() {
        let root = TestDir::new();
        let mut update = valid_market_metadata_update();
        update["market"]["clobTokenIds"] = json!(["same-token", "same-token"]);
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:00:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_metadata raw market requires two unique clobTokenIds"));
    }

    #[test]
    fn rejects_market_metadata_without_two_unique_outcomes() {
        let root = TestDir::new();
        let mut update = valid_market_metadata_update();
        update["market"]["outcomes"] = json!(["Up", "Up"]);
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:00:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_metadata raw market requires two unique outcomes"));
    }

    #[test]
    fn rejects_non_object_market_metadata_payload() {
        let root = TestDir::new();
        let mut update = valid_market_metadata_update();
        update["market"] = json!("not-an-object");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:00:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_metadata.market must be an object"));
    }

    #[test]
    fn rejects_settlement_when_raw_market_is_not_closed() {
        let root = TestDir::new();
        let mut update = valid_market_settlement_update();
        update["market"]["closed"] = json!(false);
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:06:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_settlement raw market must be closed"));
    }

    #[test]
    fn rejects_settlement_wrapper_winner_contradiction() {
        let root = TestDir::new();
        let mut update = valid_market_settlement_update();
        update["winning_token_id"] = json!("down-token");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:06:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("market_settlement winning_token_id does not match raw market"));
    }

    #[test]
    fn rejects_invalid_or_ambiguous_settlement_prices() {
        for (index, prices, expected) in [
            (0, json!(["1", "1"]), "exactly one winning price"),
            (1, json!(["1.1", "0"]), "within [0, 1]"),
            (2, json!(["invalid", "0"]), "must be numeric"),
            (3, json!(["0.999", "0.5"]), "losing price must be near zero"),
            (4, json!(["1", "0.001"]), "prices must sum to one"),
        ] {
            let root = TestDir::new();
            let mut update = valid_market_settlement_update();
            update["market"]["outcomePrices"] = prices;
            let tape = write_tape(
                root.path(),
                &format!("market-updates.20260715T03000{index}.ndjson"),
                &[record(0, "2026-07-15T03:06:00Z", update)],
            );
            assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
                .unwrap_err()
                .to_string()
                .contains(expected));
        }
    }

    #[test]
    fn rejects_settlement_outcome_and_resolution_contradictions() {
        for (index, field, value, expected) in [
            (0, "winning_outcome", json!("Down"), "winning_outcome"),
            (1, "resolved_up_won", json!(false), "resolved_up_won"),
        ] {
            let root = TestDir::new();
            let mut update = valid_market_settlement_update();
            update[field] = value;
            let tape = write_tape(
                root.path(),
                &format!("market-updates.20260715T03001{index}.ndjson"),
                &[record(0, "2026-07-15T03:06:00Z", update)],
            );
            assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
                .unwrap_err()
                .to_string()
                .contains(expected));
        }
    }

    #[test]
    fn trade_only_reference_segment_is_not_complete_or_canonical() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:10:00Z", valid_v2_trade_update())],
        );

        let manifest = scan_tape(&tape, "crypto_expiry_reference", 0, 0).unwrap();
        assert_eq!(manifest["canonical"], false);
        assert_eq!(manifest["segment_complete"], false);
        assert_eq!(manifest["reference_context_complete"], false);
        assert_eq!(
            manifest["replay_scope"],
            "reference_hour_segment_requires_market_metadata_context"
        );
    }

    #[test]
    fn settlement_only_reference_segment_is_not_complete_or_canonical() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(
                0,
                "2026-07-15T03:06:00Z",
                valid_market_settlement_update(),
            )],
        );

        let manifest = scan_tape(&tape, "crypto_expiry_reference", 0, 0).unwrap();
        assert_eq!(manifest["canonical"], false);
        assert_eq!(manifest["segment_complete"], false);
        assert_eq!(manifest["reference_context_complete"], false);
    }

    #[test]
    fn metadata_context_makes_settlement_segment_complete() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[
                record(0, "2026-07-15T03:00:00Z", valid_market_metadata_update()),
                record(1, "2026-07-15T03:06:00Z", valid_market_settlement_update()),
            ],
        );

        let manifest = scan_tape(&tape, "crypto_expiry_reference", 0, 0).unwrap();
        assert_eq!(manifest["canonical"], true);
        assert_eq!(manifest["segment_complete"], true);
        assert_eq!(manifest["reference_context_complete"], true);
        assert_eq!(manifest["replay_scope"], "complete_reference_hour_segment");
    }

    #[test]
    fn canonical_upload_count_requires_complete_quote_manifest() {
        let complete = json!({
            "canonical":true,"segment_complete":true,"event_context_complete":true,
            "quote_coverage_complete":true,"quote_quality_complete":true,
            "missing_quote_tokens":[],"missing_quote_attempt_tokens":[],
            "quality":{"executable_quotes":1,
            "missing_bid_size":0,"missing_ask_size":0,"one_sided_quotes":0,
            "empty_quotes":0,"non_executable_quotes":0,"incomplete_quotes":0,"max_quote_latency_ms":0,
            "request_attempts":1,"request_successes":1,"request_failures":0,
            "max_request_latency_ms":0}
        });
        assert!(canonical_complete_manifest(&complete));
        let mut not_canonical = complete.clone();
        not_canonical["canonical"] = json!(false);
        let mut incomplete_segment = complete.clone();
        incomplete_segment["segment_complete"] = json!(false);
        let mut incomplete_coverage = complete.clone();
        incomplete_coverage["quote_coverage_complete"] = json!(false);
        let mut missing_quality = complete.clone();
        missing_quality["quality"]["executable_quotes"] = Value::Null;
        for manifest in [
            not_canonical,
            incomplete_segment,
            incomplete_coverage,
            missing_quality,
        ] {
            assert!(!canonical_complete_manifest(&manifest));
        }
    }

    #[test]
    fn rejects_trade_record_id_not_derived_from_raw_payload() {
        let root = TestDir::new();
        let mut update = valid_v2_trade_update();
        update["record_id"] = json!("0".repeat(64));
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:10:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("record_id does not match raw trade"));
    }

    #[test]
    fn rejects_legacy_trade_id_version() {
        let root = TestDir::new();
        let mut update = valid_v2_trade_update();
        update["record_id_version"] = json!("v1_legacy");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:10:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("record_id_version must be v2"));
    }

    #[test]
    fn rejects_duplicate_trade_record_ids() {
        let root = TestDir::new();
        let update = valid_v2_trade_update();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[
                record(0, "2026-07-15T03:10:00Z", update.clone()),
                record(1, "2026-07-15T03:10:01Z", update),
            ],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("duplicate polymarket_trade record_id"));
    }

    #[test]
    fn rejects_non_object_raw_trade() {
        let root = TestDir::new();
        let mut update = valid_v2_trade_update();
        update["trade"] = json!([]);
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:10:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("polymarket_trade.trade must be an object"));
    }

    #[test]
    fn rejects_invalid_raw_trade_fields() {
        let root = TestDir::new();
        let mut update = valid_v2_trade_update();
        update["trade"]["proxyWallet"] = Value::Null;
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T030000.ndjson",
            &[record(0, "2026-07-15T03:10:00Z", update)],
        );

        assert!(scan_tape(&tape, "crypto_expiry_reference", 0, 0)
            .unwrap_err()
            .to_string()
            .contains("raw trade requires proxyWallet"));
    }

    #[test]
    fn splits_utc_hours_without_rebasing_sequences() {
        let root = TestDir::new();
        let mut rows = sample_rows();
        rows.push(record(
            3,
            "2026-07-15T02:00:00Z",
            json!({"kind": "event_expired", "event_id": "event-1", "end_time": null}),
        ));
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);
        let staging = ExclusiveTempDir::create(root.path(), ".split").unwrap();
        let source_scan =
            scan_tape_with_identity(&tape, "crypto_expiry", 1, 1_000).unwrap();
        assert!(stage_validated_single_hour(&tape, staging.path(), source_scan)
            .unwrap()
            .is_none());
        let chunks = split_tape_by_utc_hour(&tape, staging.path()).unwrap();
        assert_eq!(chunks.len(), 2);
        let first = scan_tape(&chunks[0], "crypto_expiry", 1, 1_000).unwrap();
        let second = scan_tape(&chunks[1], "crypto_expiry", 1, 1_000).unwrap();
        assert_eq!(
            (
                first["start_sequence"].as_u64(),
                first["end_sequence"].as_u64()
            ),
            (Some(0), Some(2))
        );
        assert_eq!(
            (
                second["start_sequence"].as_u64(),
                second["end_sequence"].as_u64()
            ),
            (Some(3), Some(3))
        );
        assert_eq!(
            (first["hour"].as_str(), second["hour"].as_str()),
            (Some("01"), Some("02"))
        );
    }

    #[test]
    fn stages_single_utc_hour_from_the_validated_source() {
        let root = TestDir::new();
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let scan = scan_tape_with_identity(&tape, "crypto_expiry", 1, 1_000).unwrap();
        let source_identity = scan.identity;
        let staging = ExclusiveTempDir::create(root.path(), ".split").unwrap();

        let (chunk, reused_scan) = stage_validated_single_hour(&tape, staging.path(), scan)
            .unwrap()
            .expect("sample tape stays within one UTC hour");

        assert_eq!(regular_identity(&chunk).unwrap(), source_identity);
        assert_eq!(reused_scan.identity, source_identity);
        assert_eq!(
            reused_scan.manifest["source_file"].as_str(),
            chunk.file_name().and_then(|name| name.to_str())
        );
    }

    #[test]
    fn hash_triplet_and_remote_tamper_are_fail_closed() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let upload_config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, manifest) = prepare_artifacts(&source, &upload_config).unwrap();
        assert_eq!(
            fs::read_to_string(&artifacts.success).unwrap().trim(),
            manifest["sha256"].as_str().unwrap()
        );
        assert_eq!(
            artifacts.object_prefix,
            format!(
                "lake/raw/venue=polymarket/dataset=crypto_expiry/date=2026-07-15/hour=01/sha256={}",
                manifest["sha256"].as_str().unwrap()
            )
        );
        let remote = BTreeMap::from([
            (
                artifacts
                    .data
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
                fs::read(&artifacts.data).unwrap(),
            ),
            (
                artifacts
                    .manifest
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
                fs::read(&artifacts.manifest).unwrap(),
            ),
            (
                artifacts
                    .success
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned(),
                fs::read(&artifacts.success).unwrap(),
            ),
        ]);
        let mut good_runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let remote_name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
            fs::write(&args[3], &remote[remote_name])?;
            Ok(success_status())
        };
        verify_remote_artifacts_with(&artifacts, &upload_config, &mut good_runner).unwrap();

        for tampered_suffix in [".zst", ".manifest.json", "._SUCCESS"] {
            let root = TestDir::new();
            let config = config(root.path());
            let source = write_tape(
                root.path(),
                "market-updates.20260715T010000.ndjson",
                &sample_rows(),
            );
            let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
            let mut uploaded_remote = BTreeMap::<String, Vec<u8>>::new();
            let mut bad_runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
                let args = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                if args[2].starts_with("oss://") {
                    let remote_name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                    let Some(remote_bytes) = uploaded_remote.get(remote_name) else {
                        bail!("remote object not found");
                    };
                    let bytes = if remote_name.ends_with(tampered_suffix) {
                        b"tampered".to_vec()
                    } else {
                        remote_bytes.clone()
                    };
                    fs::write(&args[3], bytes)?;
                } else {
                    assert!(args.iter().any(|arg| arg == "--ignore-existing"));
                    assert!(!args.iter().any(|arg| arg == "--force"));
                    let remote_name = Path::new(&args[3])
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .to_owned();
                    uploaded_remote.insert(remote_name, fs::read(&args[2])?);
                }
                Ok(success_status())
            };
            assert!(upload_artifacts_with(&artifacts, &config, &mut bad_runner).is_err());
            for local in [
                &source,
                &artifacts.data,
                &artifacts.manifest,
                &artifacts.success,
            ] {
                assert!(
                    local.exists(),
                    "readback failure deleted {}",
                    local.display()
                );
            }
        }
    }

    #[test]
    fn existing_remote_triplet_is_verified_without_overwrite() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let remote = [&artifacts.data, &artifacts.manifest, &artifacts.success]
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_str().unwrap().to_owned(),
                    fs::read(path).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut uploads = 0;
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args[2].starts_with("oss://") {
                let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                fs::write(&args[3], &remote[name])?;
            } else {
                uploads += 1;
            }
            Ok(success_status())
        };

        upload_artifacts_with(&artifacts, &config, &mut runner).unwrap();

        assert_eq!(uploads, 0);
    }

    #[test]
    fn new_remote_triplet_reads_data_and_manifest_before_publishing_success() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let data_name = artifacts.data.file_name().unwrap().to_str().unwrap().to_owned();
        let manifest_name = artifacts
            .manifest
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let success_name = artifacts
            .success
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let mut remote = BTreeMap::<String, Vec<u8>>::new();
        let mut uploads = 0;
        let mut downloads = 0;
        let mut operations = Vec::new();
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args[2].starts_with("oss://") {
                downloads += 1;
                let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                let bytes = remote
                    .get(name)
                    .ok_or_else(|| anyhow!("remote object not found"))?;
                fs::write(&args[3], bytes)?;
                operations.push(format!("read:{name}"));
            } else {
                assert!(args.iter().any(|arg| arg == "--ignore-existing"));
                let name = Path::new(&args[3])
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                operations.push(format!("upload:{name}"));
                remote.entry(name).or_insert(fs::read(&args[2])?);
                uploads += 1;
            }
            Ok(success_status())
        };

        upload_artifacts_with(&artifacts, &config, &mut runner).unwrap();

        assert_eq!(
            operations,
            vec![
                format!("upload:{data_name}"),
                format!("upload:{manifest_name}"),
                format!("read:{data_name}"),
                format!("read:{manifest_name}"),
                format!("upload:{success_name}"),
                format!("read:{success_name}"),
            ]
        );
        assert_eq!((uploads, downloads), (3, 4));
        assert!(!source.exists());
    }

    #[test]
    fn failed_data_readback_does_not_publish_success_marker() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let data_name = artifacts.data.file_name().unwrap().to_str().unwrap().to_owned();
        let success_name = artifacts
            .success
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let mut remote = BTreeMap::<String, Vec<u8>>::new();
        let mut success_published = false;
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args[2].starts_with("oss://") {
                let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                if name == data_name.as_str() {
                    fs::write(&args[3], b"tampered")?;
                } else {
                    let bytes = remote
                        .get(name)
                        .ok_or_else(|| anyhow!("remote object not found"))?;
                    fs::write(&args[3], bytes)?;
                }
            } else {
                let name = Path::new(&args[3])
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                success_published |= name == success_name.as_str();
                remote.entry(name).or_insert(fs::read(&args[2])?);
            }
            Ok(success_status())
        };

        assert!(upload_artifacts_with(&artifacts, &config, &mut runner).is_err());

        assert!(!success_published);
        assert!(source.exists());
        assert!(artifacts.data.exists());
        assert!(artifacts.manifest.exists());
        assert!(artifacts.success.exists());
    }

    #[test]
    fn newly_uploaded_triplet_retries_transient_readback_miss() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let mut remote = BTreeMap::<String, Vec<u8>>::new();
        let mut uploads = 0;
        let mut post_upload_downloads = 0;
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args[2].starts_with("oss://") {
                if uploads == 0 || post_upload_downloads == 0 {
                    post_upload_downloads += usize::from(uploads > 0);
                    // Production 404s surface through run_checked with the
                    // NoSuchKey detail, which is the retryable class.
                    bail!("child process exited with exit status: 1: 404 NoSuchKey");
                }
                post_upload_downloads += 1;
                let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                fs::write(&args[3], &remote[name])?;
            } else {
                let name = Path::new(&args[3])
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                remote.insert(name, fs::read(&args[2])?);
                uploads += 1;
            }
            Ok(success_status())
        };

        upload_artifacts_with(&artifacts, &config, &mut runner).unwrap();

        assert_eq!(uploads, 3);
        assert_eq!(post_upload_downloads, 4);
        assert!(!source.exists());
    }

    #[test]
    fn pre_upload_probe_is_single_shot_before_upload() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let mut remote = BTreeMap::<String, Vec<u8>>::new();
        let mut probe_attempts = 0;
        let mut uploads = 0;
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args.iter().any(|arg| arg == "--ignore-existing") {
                let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                remote.insert(name.to_owned(), fs::read(&args[2])?);
                uploads += 1;
                return Ok(success_status());
            }
            if uploads == 0 {
                probe_attempts += 1;
                bail!("child process exited with exit status: 1: 404 NoSuchKey");
            }
            let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
            fs::write(&args[3], &remote[name])?;
            Ok(success_status())
        };

        upload_artifacts_with(&artifacts, &config, &mut runner).unwrap();

        assert_eq!(probe_attempts, 1);
        assert_eq!(uploads, 3);
        assert!(!source.exists());
    }

    #[test]
    fn remote_readback_budget_is_checked_before_attempt() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let sources = [
            artifacts.data.as_path(),
            artifacts.manifest.as_path(),
            artifacts.success.as_path(),
        ];
        let mut attempts = 0;
        let error = match download_remote_artifacts_with(
            &artifacts,
            &config,
            &sources,
            Duration::ZERO,
            1,
            &mut |_, _| {
                attempts += 1;
                bail!("child process exited with exit status: 1: 404 NoSuchKey")
            },
        ) {
            Ok(_) => panic!("zero budget must reject before invoking the runner"),
            Err(error) => error,
        };

        assert_eq!(attempts, 0);
        assert!(error.to_string().contains("wall-clock budget"));
        assert!(source.exists());
    }

    #[test]
    fn remote_readback_non_404_exit_fails_without_retrying() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let mut attempts = 0;
        let error = verify_remote_artifacts_with(&artifacts, &config, &mut |_, timeout| {
            attempts += 1;
            let mut command = Command::new("sh");
            command.args(["-c", "printf 'permission denied' >&2; exit 1"]);
            run_checked(&mut command, timeout)
        })
        .unwrap_err();

        assert_eq!(attempts, 1);
        assert!(error.to_string().contains("permission denied"));
        assert!(source.exists());
    }

    #[test]
    fn remote_readback_retries_are_bounded() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let mut attempts = 0;
        let error = verify_remote_artifacts_with(&artifacts, &config, &mut |_, _| {
            attempts += 1;
            bail!("child process exited with exit status: 1: 404 NoSuchKey")
        })
        .unwrap_err();

        assert_eq!(attempts, OSS_VERIFY_DOWNLOAD_ATTEMPTS);
        assert!(error.to_string().contains("stayed unavailable after"));
        assert!(source.exists());
    }

    #[test]
    fn remote_readback_retries_transient_404_until_object_is_visible() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let remote = [&artifacts.data, &artifacts.manifest, &artifacts.success]
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_str().unwrap().to_owned(),
                    fs::read(path).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut attempts = 0;
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            attempts += 1;
            if attempts <= 3 {
                bail!("child process exited with exit status: 1: 404 NoSuchKey");
            }
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
            fs::write(&args[3], &remote[name])?;
            Ok(success_status())
        };

        verify_remote_artifacts_with(&artifacts, &config, &mut runner).unwrap();

        assert_eq!(
            attempts, 6,
            "three transient 404s, then one success per artifact"
        );
    }

    #[test]
    fn remote_readback_config_error_fails_without_retrying() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let mut attempts = 0;
        let error = verify_remote_artifacts_with(&artifacts, &config, &mut |_, _| {
            attempts += 1;
            bail!("ossutil config file not found")
        })
        .unwrap_err();

        assert_eq!(attempts, 1);
        assert!(error.to_string().contains("ossutil config file not found"));
    }

    #[test]
    fn mismatched_existing_remote_is_refused_before_upload() {
        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let root = TestDir::new();
        let config = config(root.path());
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let (artifacts, _) = prepare_artifacts(&source, &config).unwrap();
        let mut remote = [&artifacts.data, &artifacts.manifest, &artifacts.success]
            .into_iter()
            .map(|path| {
                (
                    path.file_name().unwrap().to_str().unwrap().to_owned(),
                    fs::read(path).unwrap(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let manifest_name = artifacts.manifest.file_name().unwrap().to_str().unwrap();
        remote.insert(manifest_name.to_owned(), b"tampered".to_vec());
        let mut uploads = 0;
        let mut runner = |command: &mut Command, _: Duration| -> Result<ExitStatus> {
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            if args[2].starts_with("oss://") {
                let name = Path::new(&args[2]).file_name().unwrap().to_str().unwrap();
                fs::write(&args[3], &remote[name])?;
            } else {
                uploads += 1;
            }
            Ok(success_status())
        };

        assert!(upload_artifacts_with(&artifacts, &config, &mut runner).is_err());
        assert_eq!(uploads, 0);
        for local in [
            &source,
            &artifacts.data,
            &artifacts.manifest,
            &artifacts.success,
        ] {
            assert!(local.exists(), "mismatch deleted {}", local.display());
        }
    }

    #[test]
    fn strict_discovery_rejects_malformed_names_and_symlinks() {
        assert!(strict_rotation_name(
            "market-updates.20260715T010000123456.ndjson"
        ));
        assert!(strict_rotation_name(
            "market-updates.20260715T010000123456.123e4567-e89b-12d3-a456-426614174000.ndjson"
        ));
        assert!(!strict_rotation_name(
            "market-updates.20269999T999999.ndjson"
        ));
        let root = TestDir::new();
        write_tape(root.path(), "market-updates.ndjson", &sample_rows());
        write_tape(root.path(), "market-updates.backup.ndjson", &sample_rows());
        assert!(discover_rotated_tapes(root.path())
            .unwrap_err()
            .to_string()
            .contains("invalid rotated tape name"));
        fs::remove_file(root.path().join("market-updates.backup.ndjson")).unwrap();
        let valid = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let target = root.path().join("target");
        fs::write(&target, b"victim").unwrap();
        let linked = root.path().join("market-updates.20260715T020000.ndjson");
        symlink(&target, &linked).unwrap();
        let error = discover_rotated_tapes(root.path()).unwrap_err().to_string();
        assert!(error.contains("regular non-symlink"));
        fs::remove_file(&linked).unwrap();
        assert_eq!(discover_rotated_tapes(root.path()).unwrap(), vec![valid]);
        assert_eq!(fs::read(target).unwrap(), b"victim");
    }

    #[cfg(unix)]
    #[test]
    fn upload_rejects_symlinked_or_noncanonical_spool_ancestors() {
        let root = TestDir::new();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let linked = root.path().join("linked");
        symlink(&actual, &linked).unwrap();

        for spool_dir in [
            linked.join("spool"),
            actual.join("child").join("..").join("child"),
        ] {
            let upload_config = config(&spool_dir);
            assert!(upload_pending(&upload_config)
                .unwrap_err()
                .to_string()
                .contains("directory"));
        }
    }

    #[test]
    fn continues_after_bad_segment_and_persists_failure() {
        let root = TestDir::new();
        let first = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let second = write_tape(
            root.path(),
            "market-updates.20260715T020000.ndjson",
            &sample_rows(),
        );
        let config = config(root.path());
        let mut visited = Vec::new();
        let result = upload_pending_with(&config, |source, _| {
            visited.push(source.to_path_buf());
            if source == first {
                bail!("bad tape");
            }
            fs::remove_file(source)?;
            Ok(vec![UploadedSegment {
                object: "oss://bucket/second".to_owned(),
                canonical_complete: true,
            }])
        });
        assert!(result.is_err());
        assert_eq!(visited, vec![first.clone(), second]);
        let status: Value =
            serde_json::from_slice(&fs::read(root.path().join("upload-status.json")).unwrap())
                .unwrap();
        assert_eq!(status["pending_segments"], 1);
        assert_eq!(
            status["failed_segments"][0]["source"],
            first.file_name().unwrap().to_str().unwrap()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_uploads_closed_sources_concurrently_with_bound() {
        let root = TestDir::new();
        for name in [
            "market-updates.20260715T010000.ndjson",
            "market-updates.20260715T020000.ndjson",
        ] {
            fs::write(root.path().join(name), b"closed").unwrap();
        }
        let config = config(root.path());
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let archive = {
            let active = active.clone();
            let max_active = max_active.clone();
            move |source: &Path, _config: &UploadConfig| -> Result<Vec<UploadedSegment>> {
                let current = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_active.fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(25));
                fs::remove_file(source)?;
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                Ok(vec![UploadedSegment {
                    object: format!("oss://bucket/{}", source.display()),
                    canonical_complete: true,
                }])
            }
        };

        let summary = upload_pending_async_with(&config, archive).await.unwrap();

        assert_eq!(summary.uploaded_segments, 2);
        assert_eq!(summary.canonical_uploaded_segments, 2);
        assert_eq!(max_active.load(std::sync::atomic::Ordering::SeqCst), 2);
        let status: Value =
            serde_json::from_slice(&fs::read(root.path().join("upload-status.json")).unwrap())
                .unwrap();
        assert_eq!(status["pending_segments"], 0);
    }

    #[test]
    fn reports_uploaded_hour_segments_not_source_files() {
        let root = TestDir::new();
        let source = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &sample_rows(),
        );
        let config = config(root.path());
        let uploaded = upload_pending_with(&config, |source, _| {
            fs::remove_file(source)?;
            Ok(vec![
                UploadedSegment {
                    object: "oss://bucket/hour=01/data".to_owned(),
                    canonical_complete: true,
                },
                UploadedSegment {
                    object: "oss://bucket/hour=02/data".to_owned(),
                    canonical_complete: false,
                },
            ])
        })
        .unwrap();
        assert_eq!(
            uploaded,
            UploadSummary {
                uploaded_segments: 2,
                canonical_uploaded_segments: 1,
            }
        );
        assert!(!source.exists());
        let status: Value =
            serde_json::from_slice(&fs::read(root.path().join("upload-status.json")).unwrap())
                .unwrap();
        assert_eq!(status["canonical_uploaded_segments"], 1);
    }

    fn binance_spot_price_update(ts: &str) -> Value {
        json!({
            "kind": "spot_price", "symbol": "BTCUSDT", "price": "65000.25", "ts": ts,
        })
    }

    fn binance_agg_trade_update(ts: &str) -> Value {
        json!({
            "kind": "agg_trade", "symbol": "ETHUSDT", "agg_trade_id": 987,
            "price": "3500.75", "quantity": "1.25", "is_buyer_maker": true, "ts": ts,
        })
    }

    fn binance_l2_update(ts: &str) -> Value {
        json!({
            "kind": "l2", "symbol": "BTCUSDT", "obi": 0.142857, "spread_bps": 10, "ts": ts,
        })
    }

    #[test]
    fn validates_mixed_tape_with_binance_reference_kinds() {
        let root = TestDir::new();
        let rows = vec![
            sample_rows()[0].clone(),
            sample_rows()[1].clone(),
            record(
                2,
                "2026-07-15T01:00:02.000000000Z",
                json!({
                    "kind": "quote", "token_id": "down-1", "bid": "0.49", "ask": "0.51",
                    "bid_size": "10", "ask_size": "11",
                    "bid_levels": [{"price": "0.49", "size": "10"}],
                    "ask_levels": [{"price": "0.51", "size": "11"}],
                    "request_status": "success", "collection_result": "executable",
                    "ts": "2026-07-15T01:00:02Z",
                }),
            ),
            record(
                3,
                "2026-07-15T01:00:03.000000000Z",
                binance_spot_price_update("2026-07-15T01:00:03Z"),
            ),
            record(
                4,
                "2026-07-15T01:00:04.000000000Z",
                binance_agg_trade_update("2026-07-15T01:00:04Z"),
            ),
            record(
                5,
                "2026-07-15T01:00:05.000000000Z",
                binance_l2_update("2026-07-15T01:00:05Z"),
            ),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["event_types"]["spot_price"], 1);
        assert_eq!(manifest["event_types"]["agg_trade"], 1);
        assert_eq!(manifest["event_types"]["l2"], 1);
        assert_eq!(manifest["binance_reference_counts"]["spot_price"], 1);
        assert_eq!(manifest["binance_reference_counts"]["agg_trade"], 1);
        assert_eq!(manifest["binance_reference_counts"]["l2"], 1);
        assert_eq!(manifest["field_non_null"]["spot_price"]["price"], 1);
        assert_eq!(manifest["quote_coverage_complete"], true);
        assert_eq!(manifest["event_context_complete"], true);
        assert_eq!(manifest["canonical"], true);
        assert_eq!(manifest["segment_complete"], true);
    }

    #[test]
    fn accepts_zero_price_binance_unavailable_sentinel() {
        let root = TestDir::new();
        let mut sentinel = binance_spot_price_update("2026-07-15T01:00:01Z");
        sentinel["price"] = json!("0");
        let tape = write_tape(
            root.path(),
            "market-updates.20260715T010000.ndjson",
            &[record(0, "2026-07-15T01:00:01.000000000Z", sentinel)],
        );

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["binance_reference_counts"]["spot_price"], 1);
    }

    #[test]
    fn rejects_malformed_binance_reference_kinds() {
        let root = TestDir::new();
        let cases: Vec<(&str, Value)> = vec![
            {
                let mut update = binance_spot_price_update("2026-07-15T01:00:01Z");
                update["symbol"] = json!("");
                ("spot_price requires symbol", update)
            },
            {
                let mut update = binance_spot_price_update("2026-07-15T01:00:01Z");
                update["price"] = json!("-1");
                ("spot_price price must be non-negative", update)
            },
            {
                let mut update = binance_spot_price_update("2026-07-15T01:00:01Z");
                update.as_object_mut().unwrap().remove("ts");
                ("ts must be a string", update)
            },
            {
                let mut update = binance_agg_trade_update("2026-07-15T01:00:01Z");
                update.as_object_mut().unwrap().remove("agg_trade_id");
                ("agg_trade requires agg_trade_id", update)
            },
            {
                let mut update = binance_agg_trade_update("2026-07-15T01:00:01Z");
                update["price"] = json!("0");
                ("agg_trade price must be positive", update)
            },
            {
                let mut update = binance_agg_trade_update("2026-07-15T01:00:01Z");
                update["quantity"] = json!("0");
                ("agg_trade quantity must be positive", update)
            },
            {
                let mut update = binance_agg_trade_update("2026-07-15T01:00:01Z");
                update["is_buyer_maker"] = json!("yes");
                ("agg_trade is_buyer_maker must be a boolean", update)
            },
            {
                let mut update = binance_l2_update("2026-07-15T01:00:01Z");
                update["obi"] = json!(1.5);
                ("l2 obi must be within [-1, 1]", update)
            },
            {
                let mut update = binance_l2_update("2026-07-15T01:00:01Z");
                update.as_object_mut().unwrap().remove("spread_bps");
                ("l2 requires spread_bps", update)
            },
        ];
        for (index, (message, update)) in cases.into_iter().enumerate() {
            let tape = write_tape(
                root.path(),
                &format!("market-updates.20260715T01{index:02}00.ndjson"),
                &[record(0, "2026-07-15T01:00:01.000000000Z", update)],
            );
            let error = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap_err();
            assert!(
                error.to_string().contains(message),
                "case {message:?} failed with: {error}"
            );
        }
    }

    #[test]
    fn binance_kinds_do_not_mask_polymarket_coverage_gaps() {
        let root = TestDir::new();
        let rows = vec![
            sample_rows()[0].clone(),
            sample_rows()[1].clone(),
            record(
                2,
                "2026-07-15T01:00:02.000000000Z",
                binance_spot_price_update("2026-07-15T01:00:02Z"),
            ),
            record(
                3,
                "2026-07-15T01:00:03.000000000Z",
                binance_agg_trade_update("2026-07-15T01:00:03Z"),
            ),
            record(
                4,
                "2026-07-15T01:00:04.000000000Z",
                binance_l2_update("2026-07-15T01:00:04Z"),
            ),
        ];
        let tape = write_tape(root.path(), "market-updates.20260715T010000.ndjson", &rows);

        let manifest = scan_tape(&tape, "crypto_expiry", 1, 1_000).unwrap();

        assert_eq!(manifest["missing_quote_tokens"], json!(["down-1"]));
        assert_eq!(manifest["quote_coverage_complete"], false);
        assert_eq!(manifest["canonical"], false);
        assert_eq!(manifest["binance_reference_counts"]["spot_price"], 1);
    }

    #[cfg(unix)]
    fn success_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }
}
