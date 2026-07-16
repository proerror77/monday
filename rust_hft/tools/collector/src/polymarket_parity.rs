#[cfg(test)]
use crate::polymarket_upload::derived_trade_record_id;
use crate::polymarket_upload::{
    validate_canonical_trade, validate_market_metadata, validate_market_settlement,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDateTime};
use rand::random;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const ACTIVE_TAPE: &str = "market-updates.ndjson";
const EXPECTED_SYMBOLS: [&str; 7] = [
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "HYPEUSDT", "BNBUSDT",
];
const KINDS: [&str; 3] = ["market_metadata", "polymarket_trade", "market_settlement"];
// Settlement polling is eventually consistent across implementations. Compare
// events that ended far enough before the common cutoff, not the collectors'
// independently scheduled retrieval timestamps.
const SETTLEMENT_EVENT_LOOKBACK_SECONDS: i64 = 900;
const SETTLEMENT_MATURITY_LAG_SECONDS: i64 = 600;
// Trade APIs are eventually consistent and the two collectors poll on
// independent schedules. Keep the full retrieval cutoff, but compare only
// trades whose event time is mature enough to have appeared in both lanes.
const TRADE_MATURITY_LAG_SECONDS: i64 = 600;
const METADATA_CONTRACT_FIELDS: [&str; 17] = [
    "id",
    "conditionId",
    "question",
    "slug",
    "startDate",
    "startDateIso",
    "endDate",
    "endDateIso",
    "eventStartTime",
    "outcomes",
    "clobTokenIds",
    "orderPriceMinTickSize",
    "orderMinSize",
    "makerBaseFee",
    "takerBaseFee",
    "feesEnabled",
    "negRisk",
];

#[derive(Debug, Clone)]
pub struct ShadowParityConfig {
    pub legacy_spool: PathBuf,
    pub rust_spool: PathBuf,
    pub started_at_unix: i64,
    pub ended_at_unix: i64,
    pub output: PathBuf,
}

#[derive(Debug, Clone)]
struct TapeRow {
    recorded_at: i64,
    update: Value,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
struct FileFingerprint {
    device: u64,
    inode: u64,
    bytes: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

fn parse_timestamp(value: Option<&Value>) -> Option<i64> {
    let value = value?.as_str()?;
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.timestamp())
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|parsed| parsed.and_utc().timestamp())
        })
}

fn ensure_direct_directory(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("spool path must be absolute: {}", path.display());
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect spool directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("spool is not a direct directory: {}", path.display());
    }
    if fs::canonicalize(path)? != path {
        bail!("spool has an indirect ancestor: {}", path.display());
    }
    Ok(())
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
    let format = match stamp.len() {
        15 => "%Y%m%dT%H%M%S",
        21 => "%Y%m%dT%H%M%S%6f",
        _ => return false,
    };
    if NaiveDateTime::parse_from_str(stamp, format).is_err() {
        return false;
    }
    match (parts.next(), parts.next()) {
        (None, None) => true,
        (Some(uuid), None) => {
            uuid.len() == 36
                && uuid.bytes().enumerate().all(|(index, byte)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit()
                    }
                })
        }
        _ => false,
    }
}

fn tape_paths(spool: &Path) -> Result<Vec<PathBuf>> {
    ensure_direct_directory(spool)?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(spool)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == ACTIVE_TAPE || strict_rotation_name(name) {
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "tape is not a direct regular file: {}",
                    entry.path().display()
                );
            }
            paths.push(entry.path());
        } else if name.starts_with("market-updates.") && name.ends_with(".ndjson") {
            bail!("invalid rotated tape name: {name}");
        }
    }
    paths.sort();
    if paths.is_empty() {
        bail!("no reference tapes found in {}", spool.display());
    }
    Ok(paths)
}

fn stable_file_bytes(path: &Path) -> Result<Vec<u8>> {
    let mut last_reason = "file changed while being read".to_owned();
    for _ in 0..5 {
        let before = fs::symlink_metadata(path)?;
        if before.file_type().is_symlink() || !before.is_file() {
            bail!("tape is not a direct regular file: {}", path.display());
        }
        let expected = FileFingerprint::from_metadata(&before);
        let mut file = File::open(path)?;
        if FileFingerprint::from_metadata(&file.metadata()?) != expected {
            last_reason = "tape identity changed while being opened".to_owned();
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let after = FileFingerprint::from_metadata(&file.metadata()?);
        if after != expected {
            last_reason = "tape changed while being read".to_owned();
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            last_reason = "tape ends with an incomplete record".to_owned();
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        return Ok(bytes);
    }
    bail!("{}: {last_reason}", path.display())
}

fn load_rows(spool: &Path) -> Result<(Vec<TapeRow>, usize, bool)> {
    for _ in 0..5 {
        let paths = tape_paths(spool)?;
        let mut rows = Vec::new();
        let mut closed_count = 0_usize;
        let mut active_present = false;
        for path in &paths {
            let active = path.file_name().and_then(|name| name.to_str()) == Some(ACTIVE_TAPE);
            active_present |= active;
            closed_count += usize::from(!active);
            let bytes = stable_file_bytes(path)?;
            let mut expected_sequence = 0_u64;
            let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
            for (line_index, raw) in lines.iter().enumerate() {
                if raw.is_empty() {
                    if line_index + 1 == lines.len() {
                        continue;
                    }
                    bail!("blank row in {}:{}", path.display(), line_index + 1);
                }
                let row: Value = serde_json::from_slice(raw).with_context(|| {
                    format!("invalid JSON in {}:{}", path.display(), line_index + 1)
                })?;
                let object = row.as_object().ok_or_else(|| {
                    anyhow!("non-object row in {}:{}", path.display(), line_index + 1)
                })?;
                let sequence = object
                    .get("sequence")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        anyhow!("invalid sequence in {}:{}", path.display(), line_index + 1)
                    })?;
                if sequence != expected_sequence {
                    bail!(
                        "sequence gap in {}:{} expected={expected_sequence} actual={sequence}",
                        path.display(),
                        line_index + 1
                    );
                }
                expected_sequence = expected_sequence
                    .checked_add(1)
                    .context("tape sequence overflow")?;
                let recorded_at = parse_timestamp(object.get("recorded_at")).ok_or_else(|| {
                    anyhow!(
                        "invalid recorded_at in {}:{}",
                        path.display(),
                        line_index + 1
                    )
                })?;
                let update = object
                    .get("update")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("invalid update in {}:{}", path.display(), line_index + 1)
                    })?;
                let kind = update.get("kind").and_then(Value::as_str);
                if !kind.is_some_and(|kind| KINDS.contains(&kind)) {
                    bail!(
                        "invalid update kind in {}:{}",
                        path.display(),
                        line_index + 1
                    );
                }
                rows.push(TapeRow {
                    recorded_at,
                    update,
                });
            }
        }
        if tape_paths(spool)? == paths {
            return Ok((rows, closed_count, active_present));
        }
        thread::sleep(Duration::from_millis(20));
    }
    bail!(
        "spool changed repeatedly while enumerating tapes: {}",
        spool.display()
    )
}

fn normalized_array(value: Option<&Value>, field: &str) -> Result<Vec<Value>> {
    match value {
        Some(Value::Array(values)) => Ok(values.clone()),
        Some(Value::String(value)) => serde_json::from_str::<Vec<Value>>(value)
            .with_context(|| format!("metadata {field} is not a JSON array")),
        _ => bail!("metadata {field} is not an array"),
    }
}

fn required_text<'a>(object: &'a Map<String, Value>, field: &str, label: &str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("{label} {field} is empty"))
}

fn required_f64(object: &Map<String, Value>, field: &str, label: &str) -> Result<f64> {
    object
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow!("{label} {field} is missing or not a finite number"))
}

fn required_bool(object: &Map<String, Value>, field: &str, label: &str) -> Result<bool> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("{label} {field} is missing or not a boolean"))
}

fn metadata_contract(value: &Value) -> Result<Value> {
    let update = value
        .as_object()
        .context("metadata update is not an object")?;
    validate_market_metadata(update, 0)
        .context("metadata violates the governed market context contract")?;
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .context("metadata market is not an object")?;
    let market_id = required_text(update, "market_id", "metadata")?;
    let condition_id = required_text(update, "condition_id", "metadata")?;
    let symbol = required_text(update, "symbol", "metadata")?;
    let source = required_text(update, "source", "metadata")?;
    let retrieved_at = required_text(update, "retrieved_at", "metadata")?;
    if parse_timestamp(update.get("retrieved_at")).is_none() {
        bail!("metadata retrieved_at is invalid");
    }
    let window = update
        .get("market_window_secs")
        .and_then(Value::as_u64)
        .context("metadata market_window_secs is invalid")?;
    if !EXPECTED_SYMBOLS.contains(&symbol) || !matches!(window, 300 | 900) {
        bail!("metadata symbol/window is outside the configured contract");
    }
    if market.get("id").and_then(Value::as_str) != Some(market_id)
        || market.get("conditionId").and_then(Value::as_str) != Some(condition_id)
    {
        bail!("metadata wrapper IDs contradict the embedded market");
    }
    let tick_size = required_f64(market, "orderPriceMinTickSize", "metadata market")?;
    let minimum_size = required_f64(market, "orderMinSize", "metadata market")?;
    let fees_enabled = required_bool(market, "feesEnabled", "metadata market")?;
    required_bool(market, "negRisk", "metadata market")?;
    if !(0.0 < tick_size && tick_size <= 1.0) {
        bail!("metadata market orderPriceMinTickSize is outside (0, 1]");
    }
    if minimum_size <= 0.0 {
        bail!("metadata market orderMinSize must be positive");
    }
    for field in ["makerBaseFee", "takerBaseFee"] {
        match market.get(field) {
            Some(Value::Null) | None if !fees_enabled => {}
            _ if required_f64(market, field, "metadata market")? >= 0.0 => {}
            _ => bail!("metadata market {field} must be non-negative"),
        }
    }
    let outcomes = normalized_array(market.get("outcomes"), "outcomes")?;
    let token_ids = normalized_array(market.get("clobTokenIds"), "clobTokenIds")?;
    for (label, values) in [("outcomes", &outcomes), ("token IDs", &token_ids)] {
        let strings = values
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| anyhow!("metadata {label} must be strings"))?;
        if strings.len() != 2
            || strings.iter().any(|value| value.is_empty())
            || strings[0] == strings[1]
        {
            bail!("metadata requires two unique non-empty {label}");
        }
    }

    let mut raw = Map::new();
    for field in METADATA_CONTRACT_FIELDS {
        if matches!(field, "makerBaseFee" | "takerBaseFee") && !fees_enabled {
            raw.insert(field.to_owned(), Value::Null);
            continue;
        }
        let Some(field_value) = market.get(field) else {
            continue;
        };
        let normalized = match field {
            "outcomes" | "clobTokenIds" => {
                Value::Array(normalized_array(Some(field_value), field)?)
            }
            _ => field_value.clone(),
        };
        raw.insert(field.to_owned(), normalized);
    }
    Ok(json!({
        "kind": "market_metadata",
        "market_id": market_id,
        "condition_id": condition_id,
        "symbol": symbol,
        "market_window_secs": window,
        "source": source,
        "retrieved_at": retrieved_at,
        "market": raw,
    }))
}

fn metadata_map(
    rows: &[TapeRow],
    started_at: i64,
    ended_at: i64,
) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for row in rows.iter().filter(|row| row.recorded_at <= ended_at) {
        if row.update["kind"] != "market_metadata" {
            continue;
        }
        let market = row.update["market"]
            .as_object()
            .context("metadata market is not an object")?;
        let end_epoch = parse_timestamp(market.get("endDate"))
            .or_else(|| parse_timestamp(market.get("endDateIso")))
            .context("metadata has no valid end time")?;
        if !(started_at.saturating_sub(900)..=ended_at.saturating_add(900)).contains(&end_epoch) {
            continue;
        }
        let contract = metadata_contract(&row.update)?;
        let identity = contract["market_id"]
            .as_str()
            .expect("metadata contract has a market_id")
            .to_owned();
        result.insert(identity, contract);
    }
    Ok(result)
}

fn market_end_epoch(update: &Value, label: &str) -> Result<i64> {
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("{label} has no market object"))?;
    parse_timestamp(market.get("endDate"))
        .or_else(|| parse_timestamp(market.get("endDateIso")))
        .ok_or_else(|| anyhow!("{label} has no valid market end time"))
}

fn nonnegative_epoch_sub(epoch: i64, seconds: i64) -> i64 {
    epoch.saturating_sub(seconds).max(0)
}

fn canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut normalized = value.clone();
    if let Some(object) = normalized.as_object_mut() {
        object.remove("retrieved_at");
        object.remove("received_at");
    }
    Ok(serde_json::to_vec(&normalized)?)
}

fn digest_values<'a>(values: impl Iterator<Item = &'a Value>) -> Result<String> {
    let mut digest = Sha256::new();
    let mut first = true;
    for value in values {
        if !first {
            digest.update(b"\n");
        }
        digest.update(canonical_bytes(value)?);
        first = false;
    }
    Ok(hex::encode(digest.finalize()))
}

fn trade_map(
    rows: &[TapeRow],
    started_at: i64,
    event_ended_at: i64,
    recorded_ended_at: i64,
) -> Result<(BTreeMap<String, Value>, Vec<String>)> {
    let mut result = BTreeMap::new();
    let mut counts = BTreeMap::<String, u64>::new();
    for row in rows
        .iter()
        .filter(|row| row.recorded_at <= recorded_ended_at)
    {
        if row.update["kind"] != "polymarket_trade" {
            continue;
        }
        let Some(timestamp) = row.update.get("trade_ts_unix").and_then(Value::as_i64) else {
            bail!("trade has an invalid trade_ts_unix");
        };
        if !(started_at..=event_ended_at).contains(&timestamp) {
            continue;
        }
        let update = row
            .update
            .as_object()
            .expect("loaded updates are validated as objects");
        for field in ["source", "received_at"] {
            required_text(update, field, "trade")?;
        }
        if parse_timestamp(update.get("received_at")).is_none() {
            bail!("trade received_at is invalid");
        }
        let record_id = validate_canonical_trade(update, 0)
            .context("trade violates the governed canonical contract")?;
        *counts.entry(record_id.clone()).or_default() += 1;
        result.insert(record_id, row.update.clone());
    }
    let duplicates = counts
        .into_iter()
        .filter_map(|(identity, count)| (count > 1).then_some(identity))
        .collect();
    Ok((result, duplicates))
}

fn metadata_context_for_trades(
    rows: &[TapeRow],
    ended_at: i64,
    trades: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, Value>> {
    let required_ids = trades
        .values()
        .filter_map(|trade| trade.get("market_id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    let mut result = BTreeMap::new();
    for row in rows.iter().filter(|row| row.recorded_at <= ended_at) {
        if row.update["kind"] != "market_metadata" {
            continue;
        }
        let market_id = row
            .update
            .get("market_id")
            .and_then(Value::as_str)
            .filter(|market_id| required_ids.contains(market_id));
        let Some(market_id) = market_id else {
            continue;
        };
        result.insert(market_id.to_owned(), metadata_contract(&row.update)?);
    }
    Ok(result)
}

fn settlement_map(
    rows: &[TapeRow],
    started_at: i64,
    ended_at: i64,
) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    let event_window_start = nonnegative_epoch_sub(started_at, SETTLEMENT_EVENT_LOOKBACK_SECONDS);
    let event_window_end = nonnegative_epoch_sub(ended_at, SETTLEMENT_MATURITY_LAG_SECONDS);
    for row in rows.iter().filter(|row| row.recorded_at <= ended_at) {
        if row.update["kind"] != "market_settlement" {
            continue;
        }
        // A valid settlement cannot be observed before its market end. Rows
        // recorded before the event window therefore cannot participate and
        // should not make an unrelated current gate parse legacy schemas.
        if row.recorded_at < event_window_start {
            continue;
        }
        let market_id = row
            .update
            .get("market_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("settlement has an empty market_id")?
            .to_owned();
        let end_epoch = market_end_epoch(&row.update, &format!("settlement {market_id}"))?;
        if !(event_window_start..=event_window_end).contains(&end_epoch) {
            continue;
        }
        let contract = settlement_contract(&row.update)
            .with_context(|| format!("settlement {market_id} violates the governed contract"))?;
        result.insert(market_id, contract);
    }
    Ok(result)
}

fn settlement_contract(value: &Value) -> Result<Value> {
    let update = value
        .as_object()
        .context("settlement update is not an object")?;
    validate_market_settlement(update, 0)?;
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .context("settlement market is not an object")?;
    let market_id = required_text(update, "market_id", "settlement")?;
    let condition_id = required_text(update, "condition_id", "settlement")?;
    let symbol = required_text(update, "symbol", "settlement")?;
    let winning_token_id = required_text(update, "winning_token_id", "settlement")?;
    let winning_outcome = required_text(update, "winning_outcome", "settlement")?;
    let resolution_source = required_text(update, "resolution_source", "settlement")?;
    required_text(update, "retrieved_at", "settlement")?;
    if parse_timestamp(update.get("retrieved_at")).is_none() {
        bail!("settlement {market_id} retrieved_at is invalid");
    }
    let market_window_secs = update
        .get("market_window_secs")
        .and_then(Value::as_u64)
        .context("settlement market_window_secs is invalid")?;
    let resolved_up_won = update
        .get("resolved_up_won")
        .and_then(Value::as_bool)
        .context("settlement resolved_up_won is invalid")?;
    let raw_market_id = required_text(market, "id", "settlement market")?;
    let raw_condition_id = required_text(market, "conditionId", "settlement market")?;
    let outcomes = normalized_array(market.get("outcomes"), "outcomes")?;
    let token_ids = normalized_array(market.get("clobTokenIds"), "clobTokenIds")?;
    let end_epoch = market_end_epoch(value, &format!("settlement {market_id}"))?;

    // Provider responses contain polling-time fields such as updatedAt and
    // volume. Validate the complete raw row above, then compare only the
    // immutable settlement decision and its event identity/time bindings.
    Ok(json!({
        "kind": "market_settlement",
        "market_id": market_id,
        "condition_id": condition_id,
        "symbol": symbol,
        "market_window_secs": market_window_secs,
        "winning_token_id": winning_token_id,
        "winning_outcome": winning_outcome,
        "resolved_up_won": resolved_up_won,
        "resolution_source": resolution_source,
        "market_end_epoch": end_epoch,
        "market": {
            "id": raw_market_id,
            "conditionId": raw_condition_id,
            "outcomes": outcomes,
            "clobTokenIds": token_ids,
        },
    }))
}

fn map_ids(values: &BTreeMap<String, Value>) -> BTreeSet<String> {
    values.keys().cloned().collect()
}

fn shared_value_mismatch_ids(
    left: &BTreeMap<String, Value>,
    right: &BTreeMap<String, Value>,
) -> Result<Vec<String>> {
    let mut mismatches = Vec::new();
    for identity in map_ids(left).intersection(&map_ids(right)) {
        if canonical_bytes(&left[identity])? != canonical_bytes(&right[identity])? {
            mismatches.push(identity.clone());
        }
    }
    Ok(mismatches)
}

fn settlement_metadata_context_mismatch_ids(
    settlements: &BTreeMap<String, Value>,
    metadata: &BTreeMap<String, Value>,
) -> Result<Vec<String>> {
    let mut mismatches = Vec::new();
    for (market_id, settlement) in settlements {
        let Some(context) = metadata.get(market_id) else {
            mismatches.push(market_id.clone());
            continue;
        };
        let mut context_matches = true;
        for field in ["condition_id", "symbol", "market_window_secs"] {
            if settlement.get(field) != context.get(field) {
                context_matches = false;
            }
        }
        let settlement_end_epoch = settlement
            .get("market_end_epoch")
            .and_then(Value::as_i64)
            .context("settlement contract has no governed market_end_epoch")?;
        if settlement_end_epoch != market_end_epoch(context, &format!("metadata {market_id}"))? {
            context_matches = false;
        }
        let settlement_market = settlement["market"]
            .as_object()
            .expect("settlement contract requires a market object");
        let metadata_market = context["market"]
            .as_object()
            .expect("metadata contract requires a market object");
        for field in ["outcomes", "clobTokenIds"] {
            if normalized_array(settlement_market.get(field), field)?
                != normalized_array(metadata_market.get(field), field)?
            {
                context_matches = false;
            }
        }
        if !context_matches {
            mismatches.push(market_id.clone());
        }
    }
    Ok(mismatches)
}

fn trade_metadata_context_mismatch_ids(
    trades: &BTreeMap<String, Value>,
    metadata: &BTreeMap<String, Value>,
) -> Result<Vec<String>> {
    let mut mismatches = BTreeSet::new();
    for trade in trades.values() {
        let Some(market_id) = trade.get("market_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(context) = metadata.get(market_id) else {
            mismatches.insert(market_id.to_owned());
            continue;
        };
        let mut context_matches = true;
        for field in ["condition_id", "symbol", "market_window_secs"] {
            if trade.get(field) != context.get(field) {
                context_matches = false;
            }
        }
        let metadata_market = context["market"]
            .as_object()
            .expect("metadata contract requires a market object");
        let tokens = normalized_array(metadata_market.get("clobTokenIds"), "clobTokenIds")?;
        let outcomes = normalized_array(metadata_market.get("outcomes"), "outcomes")?;
        let Some(outcome_index) = trade.get("outcome_index").and_then(Value::as_u64) else {
            mismatches.insert(market_id.to_owned());
            continue;
        };
        let Ok(outcome_index) = usize::try_from(outcome_index) else {
            mismatches.insert(market_id.to_owned());
            continue;
        };
        if tokens.get(outcome_index).and_then(Value::as_str)
            != trade.get("token_id").and_then(Value::as_str)
            || outcomes.get(outcome_index).and_then(Value::as_str)
                != trade.get("outcome").and_then(Value::as_str)
        {
            context_matches = false;
        }
        if !context_matches {
            mismatches.insert(market_id.to_owned());
        }
    }
    Ok(mismatches.into_iter().collect())
}

fn required_fields_present(kind: &str, values: &BTreeMap<String, Value>) -> bool {
    let required: &[&str] = match kind {
        "market_metadata" => &[
            "kind",
            "market_id",
            "condition_id",
            "symbol",
            "market_window_secs",
            "source",
            "retrieved_at",
            "market",
        ],
        "polymarket_trade" => &[
            "kind",
            "record_id",
            "record_id_version",
            "market_id",
            "condition_id",
            "symbol",
            "trade_ts_unix",
            "received_at",
            "trade",
        ],
        "market_settlement" => &[
            "kind",
            "market_id",
            "condition_id",
            "symbol",
            "winning_token_id",
            "winning_outcome",
            "resolution_source",
            "market_end_epoch",
            "market",
        ],
        _ => return false,
    };
    !values.is_empty()
        && values.values().all(|value| {
            value
                .as_object()
                .is_some_and(|object| required.iter().all(|field| object.contains_key(*field)))
        })
}

fn compare(config: &ShadowParityConfig) -> Result<Value> {
    if config.ended_at_unix <= config.started_at_unix {
        bail!("parity window end must be after its start");
    }
    let (legacy_rows, _, legacy_active) = load_rows(&config.legacy_spool)?;
    let (rust_rows, rust_closed, rust_active) = load_rows(&config.rust_spool)?;

    let legacy_metadata = metadata_map(&legacy_rows, config.started_at_unix, config.ended_at_unix)?;
    let rust_metadata = metadata_map(&rust_rows, config.started_at_unix, config.ended_at_unix)?;
    let legacy_metadata_ids = map_ids(&legacy_metadata);
    let rust_metadata_ids = map_ids(&rust_metadata);
    let metadata_shared_value_mismatch_ids =
        shared_value_mismatch_ids(&legacy_metadata, &rust_metadata)?;
    let metadata_shared_values_match = metadata_shared_value_mismatch_ids.is_empty();
    let metadata_parity = !legacy_metadata_ids.is_empty()
        && legacy_metadata_ids.is_subset(&rust_metadata_ids)
        && metadata_shared_values_match;

    let trade_event_window_end =
        nonnegative_epoch_sub(config.ended_at_unix, TRADE_MATURITY_LAG_SECONDS);
    if trade_event_window_end <= config.started_at_unix {
        bail!("trade maturity window end must be after its start");
    }
    let (legacy_trades, legacy_duplicates) = trade_map(
        &legacy_rows,
        config.started_at_unix,
        trade_event_window_end,
        config.ended_at_unix,
    )?;
    let (rust_trades, rust_duplicates) = trade_map(
        &rust_rows,
        config.started_at_unix,
        trade_event_window_end,
        config.ended_at_unix,
    )?;
    let legacy_trade_ids = map_ids(&legacy_trades);
    let rust_trade_ids = map_ids(&rust_trades);
    let legacy_only_trade_ids = legacy_trade_ids
        .difference(&rust_trade_ids)
        .cloned()
        .collect::<Vec<_>>();
    let rust_only_trade_ids = rust_trade_ids
        .difference(&legacy_trade_ids)
        .cloned()
        .collect::<Vec<_>>();
    let trade_shared_value_mismatch_ids = shared_value_mismatch_ids(&legacy_trades, &rust_trades)?;
    let trade_shared_values_match = trade_shared_value_mismatch_ids.is_empty();
    let legacy_trade_metadata =
        metadata_context_for_trades(&legacy_rows, config.ended_at_unix, &legacy_trades)?;
    let rust_trade_metadata =
        metadata_context_for_trades(&rust_rows, config.ended_at_unix, &rust_trades)?;
    let trade_metadata_shared_value_mismatch_market_ids =
        shared_value_mismatch_ids(&legacy_trade_metadata, &rust_trade_metadata)?;
    let trade_metadata_shared_values_match =
        trade_metadata_shared_value_mismatch_market_ids.is_empty();
    let legacy_trade_metadata_context_mismatch_market_ids =
        trade_metadata_context_mismatch_ids(&legacy_trades, &legacy_trade_metadata)?;
    let rust_trade_metadata_context_mismatch_market_ids =
        trade_metadata_context_mismatch_ids(&rust_trades, &rust_trade_metadata)?;
    let legacy_trade_metadata_context_match =
        legacy_trade_metadata_context_mismatch_market_ids.is_empty();
    let rust_trade_metadata_context_match =
        rust_trade_metadata_context_mismatch_market_ids.is_empty();
    let dedupe_parity = legacy_duplicates.is_empty()
        && rust_duplicates.is_empty()
        && !legacy_trade_ids.is_empty()
        && legacy_trade_ids == rust_trade_ids
        && trade_metadata_shared_values_match
        && legacy_trade_metadata_context_match
        && rust_trade_metadata_context_match;

    let legacy_settlements =
        settlement_map(&legacy_rows, config.started_at_unix, config.ended_at_unix)?;
    let rust_settlements =
        settlement_map(&rust_rows, config.started_at_unix, config.ended_at_unix)?;
    let legacy_settlement_ids = map_ids(&legacy_settlements);
    let rust_settlement_ids = map_ids(&rust_settlements);
    let settlement_shared_value_mismatch_ids =
        shared_value_mismatch_ids(&legacy_settlements, &rust_settlements)?;
    let settlement_shared_values_match = settlement_shared_value_mismatch_ids.is_empty();
    let legacy_settlement_metadata_context_mismatch_market_ids =
        settlement_metadata_context_mismatch_ids(&legacy_settlements, &legacy_metadata)?;
    let rust_settlement_metadata_context_mismatch_market_ids =
        settlement_metadata_context_mismatch_ids(&rust_settlements, &rust_metadata)?;
    let legacy_settlement_metadata_context_match =
        legacy_settlement_metadata_context_mismatch_market_ids.is_empty();
    let rust_settlement_metadata_context_match =
        rust_settlement_metadata_context_mismatch_market_ids.is_empty();
    let settlement_parity = !legacy_settlement_ids.is_empty()
        && legacy_settlement_ids.is_subset(&rust_settlement_ids)
        && settlement_shared_values_match
        && legacy_settlement_metadata_context_match
        && rust_settlement_metadata_context_match;
    let field_parity = [
        required_fields_present("market_metadata", &legacy_metadata),
        required_fields_present("market_metadata", &rust_metadata),
        required_fields_present("polymarket_trade", &legacy_trades),
        required_fields_present("polymarket_trade", &rust_trades),
        required_fields_present("market_settlement", &legacy_settlements),
        required_fields_present("market_settlement", &rust_settlements),
    ]
    .into_iter()
    .all(|present| present);

    let rust_symbols = rust_metadata
        .values()
        .filter_map(|value| value.get("symbol").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let asset_parity = EXPECTED_SYMBOLS
        .iter()
        .all(|symbol| rust_symbols.contains(*symbol));
    let rotation_parity = legacy_active && rust_active && rust_closed >= 1;
    let byte_parity =
        metadata_parity && dedupe_parity && trade_shared_values_match && settlement_parity;

    let checks = json!({
        "byte_parity": byte_parity,
        "metadata_parity": metadata_parity,
        "field_parity": field_parity,
        "dedupe_parity": dedupe_parity,
        "settlement_parity": settlement_parity,
        "rotation_parity": rotation_parity,
        "asset_parity": asset_parity,
    });
    let passed = checks
        .as_object()
        .expect("checks is an object")
        .values()
        .all(|value| value == &Value::Bool(true));
    let mut evidence = json!({
        "schema": "monday.polymarket_shadow_parity.v1",
        "passed": passed,
        "checks": checks,
        "metrics": {
            "legacy_trade_count": legacy_trade_ids.len(),
            "rust_trade_count": rust_trade_ids.len(),
            "legacy_metadata_count": legacy_metadata_ids.len(),
            "rust_metadata_count": rust_metadata_ids.len(),
            "legacy_only_metadata_ids": legacy_metadata_ids.difference(&rust_metadata_ids).cloned().collect::<Vec<_>>(),
            "rust_only_metadata_ids": rust_metadata_ids.difference(&legacy_metadata_ids).cloned().collect::<Vec<_>>(),
            "metadata_shared_values_match": metadata_shared_values_match,
            "metadata_shared_value_mismatch_ids": metadata_shared_value_mismatch_ids,
            "legacy_duplicate_trade_ids": legacy_duplicates,
            "rust_duplicate_trade_ids": rust_duplicates,
            "trade_shared_values_match": trade_shared_values_match,
            "trade_shared_value_mismatch_ids": trade_shared_value_mismatch_ids,
            "trade_maturity_lag_seconds": TRADE_MATURITY_LAG_SECONDS,
            "trade_event_window_started_at_unix": config.started_at_unix,
            "trade_event_window_ended_at_unix": trade_event_window_end,
            "legacy_trade_metadata_context_match": legacy_trade_metadata_context_match,
            "rust_trade_metadata_context_match": rust_trade_metadata_context_match,
            "legacy_trade_metadata_context_mismatch_market_ids": legacy_trade_metadata_context_mismatch_market_ids,
            "rust_trade_metadata_context_mismatch_market_ids": rust_trade_metadata_context_mismatch_market_ids,
            "legacy_settlement_count": legacy_settlement_ids.len(),
            "rust_settlement_count": rust_settlement_ids.len(),
            "legacy_only_settlement_ids": legacy_settlement_ids.difference(&rust_settlement_ids).cloned().collect::<Vec<_>>(),
            "rust_only_settlement_ids": rust_settlement_ids.difference(&legacy_settlement_ids).cloned().collect::<Vec<_>>(),
            "settlement_shared_values_match": settlement_shared_values_match,
            "settlement_shared_value_mismatch_ids": settlement_shared_value_mismatch_ids,
            "legacy_settlement_metadata_context_match": legacy_settlement_metadata_context_match,
            "rust_settlement_metadata_context_match": rust_settlement_metadata_context_match,
            "settlement_event_lookback_seconds": SETTLEMENT_EVENT_LOOKBACK_SECONDS,
            "settlement_maturity_lag_seconds": SETTLEMENT_MATURITY_LAG_SECONDS,
            "settlement_event_window_started_at_unix": nonnegative_epoch_sub(config.started_at_unix, SETTLEMENT_EVENT_LOOKBACK_SECONDS),
            "settlement_event_window_ended_at_unix": nonnegative_epoch_sub(config.ended_at_unix, SETTLEMENT_MATURITY_LAG_SECONDS),
            "rust_closed_tape_count": rust_closed,
            "rust_symbols": rust_symbols,
            "normalized_trade_sha256": digest_values(rust_trades.values())?,
            "normalized_metadata_sha256": digest_values(rust_metadata.values())?,
            "normalized_settlement_sha256": digest_values(rust_settlements.values())?,
        },
    });
    let metrics = evidence["metrics"]
        .as_object_mut()
        .expect("evidence metrics is an object");
    metrics.insert(
        "legacy_only_trade_ids".to_owned(),
        json!(legacy_only_trade_ids),
    );
    metrics.insert("rust_only_trade_ids".to_owned(), json!(rust_only_trade_ids));
    metrics.insert(
        "trade_metadata_shared_values_match".to_owned(),
        json!(trade_metadata_shared_values_match),
    );
    metrics.insert(
        "trade_metadata_shared_value_mismatch_market_ids".to_owned(),
        json!(trade_metadata_shared_value_mismatch_market_ids),
    );
    metrics.insert(
        "legacy_settlement_metadata_context_mismatch_market_ids".to_owned(),
        json!(legacy_settlement_metadata_context_mismatch_market_ids),
    );
    metrics.insert(
        "rust_settlement_metadata_context_mismatch_market_ids".to_owned(),
        json!(rust_settlement_metadata_context_mismatch_market_ids),
    );
    Ok(evidence)
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .context("parity output has no parent directory")?;
    ensure_direct_directory(parent)?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("parity output name is not UTF-8")?;
    let (temporary, mut output) = (0..32)
        .find_map(|_| {
            let candidate = parent.join(format!(".{name}.{:016x}.tmp", random::<u64>()));
            match OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&candidate)
            {
                Ok(output) => Some(Ok((candidate, output))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(error)),
            }
        })
        .transpose()?
        .context("could not allocate parity output temporary")?;
    let write_result = (|| -> Result<()> {
        serde_json::to_writer(&mut output, value)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

/// Compare a bounded active Python lane with an isolated Rust shadow, write
/// fail-closed evidence, and return whether every parity check passed.
pub fn verify_shadow_parity(config: &ShadowParityConfig) -> Result<bool> {
    let evidence = compare(config)?;
    let passed = evidence["passed"] == Value::Bool(true);
    atomic_write_json(&config.output, &evidence)?;
    Ok(passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir {
        _temp: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let temp = tempfile::Builder::new()
                .prefix("monday-polymarket-parity-")
                .tempdir()
                .unwrap();
            let path = fs::canonicalize(temp.path()).unwrap();
            Self { _temp: temp, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn metadata(symbol: &str) -> Value {
        let market_name = match symbol {
            "BTCUSDT" => "Bitcoin",
            "ETHUSDT" => "Ethereum",
            "SOLUSDT" => "Solana",
            "XRPUSDT" => "XRP",
            "DOGEUSDT" => "Dogecoin",
            "HYPEUSDT" => "Hyperliquid",
            "BNBUSDT" => "Binance Coin",
            _ => panic!("unsupported fixture symbol"),
        };
        json!({
            "kind":"market_metadata",
            "market_id":format!("market-{symbol}"),
            "condition_id":format!("condition-{symbol}"),
            "symbol":symbol,
            "market_window_secs":300,
            "source":"gamma_api",
            "retrieved_at":"1970-01-01T00:03:20Z",
            "market":{
                "id":format!("market-{symbol}"),
                "conditionId":format!("condition-{symbol}"),
                "question":format!("{market_name} Up or Down"),
                "slug":format!("market-{symbol}"),
                "startDate":"1970-01-01T00:00:00Z",
                "endDate":"1970-01-01T00:05:00Z",
                "outcomes":["Up","Down"],
                "clobTokenIds":[format!("up-{symbol}"),format!("down-{symbol}")],
                "orderPriceMinTickSize":0.01,
                "orderMinSize":5,
                "makerBaseFee":1000,
                "takerBaseFee":1000,
                "feesEnabled":true,
                "negRisk":false
            }
        })
    }

    fn trade(seed: &str) -> Value {
        let mut update = json!({
            "kind":"polymarket_trade","record_id":"pending","record_id_version":"v2",
            "market_id":"market-BTCUSDT","condition_id":"condition-BTCUSDT",
            "token_id":"up-BTCUSDT","symbol":"BTCUSDT","market_window_secs":300,
            "side":"BUY","size":"1","price":"0.5",
            "trade_ts":"1970-01-01T00:03:20Z","trade_ts_unix":200,
            "transaction_hash":format!("0x{seed}"),"proxy_wallet":"0x2","outcome":"Up",
            "outcome_index":0,"source":"polymarket_data_api",
            "received_at":"1970-01-01T00:03:20Z",
            "trade":{"transactionHash":format!("0x{seed}"),"conditionId":"condition-BTCUSDT",
                "asset":"up-BTCUSDT","side":"BUY","timestamp":200,
                "proxyWallet":"0x2","size":"1","price":"0.5",
                "outcomeIndex":0,"outcome":"Up"}
        });
        let record_id = derived_trade_record_id(update["trade"].as_object().unwrap());
        update["record_id"] = Value::String(record_id);
        update
    }

    fn trade_at(seed: &str, timestamp: i64) -> Value {
        let mut update = trade(seed);
        update["trade_ts"] = Value::String(
            DateTime::from_timestamp(timestamp, 0)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        update["trade_ts_unix"] = Value::from(timestamp);
        update["trade"]["timestamp"] = Value::from(timestamp);
        let record_id = derived_trade_record_id(update["trade"].as_object().unwrap());
        update["record_id"] = Value::String(record_id);
        update
    }

    fn settlement() -> Value {
        json!({
            "kind":"market_settlement","market_id":"market-BTCUSDT",
            "condition_id":"condition-BTCUSDT","symbol":"BTCUSDT",
            "market_window_secs":300,"winning_token_id":"up-BTCUSDT",
            "winning_outcome":"Up","resolved_up_won":true,
            "resolution_source":"gamma_api_closed_market",
            "retrieved_at":"1970-01-01T00:03:20Z",
            "market":{"id":"market-BTCUSDT","conditionId":"condition-BTCUSDT",
                "question":"BTCUSDT Up or Down","startDate":"1970-01-01T00:00:00Z",
                "endDate":"1970-01-01T00:05:00Z",
                "closed":true,"outcomes":["Up","Down"],
                "clobTokenIds":["up-BTCUSDT","down-BTCUSDT"],
                "outcomePrices":["1","0"]}
        })
    }

    fn fixture_rows() -> Vec<Value> {
        let mut rows = EXPECTED_SYMBOLS
            .iter()
            .map(|symbol| metadata(symbol))
            .collect::<Vec<_>>();
        rows.push(trade("trade-1"));
        rows.push(settlement());
        rows
    }

    fn write_tape(path: &Path, updates: &[Value], recorded_at: &str) {
        let mut output = File::create(path).unwrap();
        for (sequence, update) in updates.iter().enumerate() {
            serde_json::to_writer(
                &mut output,
                &json!({"sequence":sequence,"recorded_at":recorded_at,"update":update}),
            )
            .unwrap();
            output.write_all(b"\n").unwrap();
        }
        output.sync_all().unwrap();
    }

    fn append_tape(path: &Path, update: &Value, recorded_at: &str) {
        let sequence = fs::read_to_string(path).unwrap().lines().count();
        let mut output = OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(
            &mut output,
            &json!({"sequence":sequence,"recorded_at":recorded_at,"update":update}),
        )
        .unwrap();
        output.write_all(b"\n").unwrap();
        output.sync_all().unwrap();
    }

    fn extra_metadata() -> Value {
        let mut update = metadata("BTCUSDT");
        update["market_id"] = Value::String("market-extra".to_owned());
        update["condition_id"] = Value::String("condition-extra".to_owned());
        update["market"]["id"] = Value::String("market-extra".to_owned());
        update["market"]["conditionId"] = Value::String("condition-extra".to_owned());
        update["market"]["slug"] = Value::String("market-extra".to_owned());
        update
    }

    fn extra_settlement() -> Value {
        let mut update = settlement();
        update["market_id"] = Value::String("market-extra".to_owned());
        update["condition_id"] = Value::String("condition-extra".to_owned());
        update["market"]["id"] = Value::String("market-extra".to_owned());
        update["market"]["conditionId"] = Value::String("condition-extra".to_owned());
        update
    }

    fn fixture() -> (TestDir, ShadowParityConfig) {
        let root = TestDir::new();
        let legacy = root.path().join("legacy");
        let rust = root.path().join("rust");
        fs::create_dir(&legacy).unwrap();
        fs::create_dir(&rust).unwrap();
        let mut legacy_rows = fixture_rows();
        let mut delayed = trade("trade-after-cutoff");
        delayed["post_cutoff_only"] = Value::Bool(true);
        delayed["trade"]["postCutoffOnly"] = Value::Bool(true);
        legacy_rows.push(delayed);
        write_tape(
            &legacy.join(ACTIVE_TAPE),
            &legacy_rows[..legacy_rows.len() - 1],
            "1970-01-01T00:03:20Z",
        );
        let mut legacy_active = OpenOptions::new()
            .append(true)
            .open(legacy.join(ACTIVE_TAPE))
            .unwrap();
        serde_json::to_writer(
            &mut legacy_active,
            &json!({"sequence":legacy_rows.len() - 1,"recorded_at":"1970-01-01T00:16:41Z","update":legacy_rows.last().unwrap()}),
        )
        .unwrap();
        legacy_active.write_all(b"\n").unwrap();
        legacy_active.sync_all().unwrap();

        write_tape(
            &rust.join("market-updates.19700101T000400000000.ndjson"),
            &fixture_rows(),
            "1970-01-01T00:03:20Z",
        );
        File::create(rust.join(ACTIVE_TAPE)).unwrap();
        let config = ShadowParityConfig {
            legacy_spool: legacy,
            rust_spool: rust,
            started_at_unix: 100,
            ended_at_unix: 1000,
            output: root.path().join("parity.json"),
        };
        (root, config)
    }

    #[test]
    fn bounded_parity_ignores_delayed_trade_recorded_after_cutoff() {
        let (_root, config) = fixture();
        let evidence = compare(&config).unwrap();
        let (rust_rows, _, _) = load_rows(&config.rust_spool).unwrap();
        let expected_settlement_digest = digest_values(
            settlement_map(&rust_rows, config.started_at_unix, config.ended_at_unix)
                .unwrap()
                .values(),
        )
        .unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(evidence["checks"]["metadata_parity"], true);
        assert_eq!(evidence["metrics"]["legacy_trade_count"], 1);
        assert_eq!(
            evidence["metrics"]["normalized_settlement_sha256"],
            expected_settlement_digest
        );
        assert_eq!(
            evidence["metrics"]["settlement_event_window_started_at_unix"],
            0
        );
    }

    #[test]
    fn duplicate_rust_trade_fails_dedupe_parity() {
        let (_root, config) = fixture();
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[trade("trade-1")],
            "1970-01-01T00:03:21Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["dedupe_parity"], false);
    }

    #[test]
    fn contradictory_metadata_values_fail_parity() {
        let (_root, config) = fixture();
        let mut rows = fixture_rows();
        rows[0]["market"]["clobTokenIds"] = json!(["tampered-up", "tampered-down"]);
        write_tape(
            &config
                .rust_spool
                .join("market-updates.19700101T000400000000.ndjson"),
            &rows,
            "1970-01-01T00:03:20Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["metadata_parity"], false);
        assert_eq!(
            evidence["metrics"]["metadata_shared_value_mismatch_ids"],
            json!(["market-BTCUSDT"])
        );
    }

    #[test]
    fn independently_polled_metadata_state_does_not_poison_identity_parity() {
        let (_root, config) = fixture();
        let mut legacy_rows = fixture_rows();
        let mut rust_rows = fixture_rows();
        for (field, legacy, rust) in [
            ("active", json!(true), json!(false)),
            ("closed", json!(false), json!(true)),
            ("acceptingOrders", json!(true), json!(false)),
            ("enableOrderBook", json!(true), json!(false)),
            (
                "umaEndDate",
                json!("1970-01-01T00:05:00Z"),
                json!("1970-01-01T00:05:01Z"),
            ),
            (
                "umaEndDateIso",
                json!("1970-01-01T00:05:00Z"),
                json!("1970-01-01T00:05:01Z"),
            ),
        ] {
            legacy_rows[0]["market"][field] = legacy;
            rust_rows[0]["market"][field] = rust;
        }
        write_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &legacy_rows,
            "1970-01-01T00:03:20Z",
        );
        write_tape(
            &config
                .rust_spool
                .join("market-updates.19700101T000400000000.ndjson"),
            &rust_rows,
            "1970-01-01T00:03:20Z",
        );

        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(evidence["checks"]["metadata_parity"], true);
    }

    #[test]
    fn governed_metadata_fields_are_required_and_typed() {
        let mut missing = metadata("BTCUSDT");
        missing["market"]
            .as_object_mut()
            .unwrap()
            .remove("makerBaseFee");
        assert!(metadata_contract(&missing).is_err());

        let mut malformed = metadata("BTCUSDT");
        malformed["market"]["negRisk"] = json!("false");
        assert!(metadata_contract(&malformed).is_err());

        let mut invalid_domain = metadata("BTCUSDT");
        invalid_domain["market"]["orderMinSize"] = json!(0);
        assert!(metadata_contract(&invalid_domain).is_err());

        let mut fees_disabled = metadata("BTCUSDT");
        fees_disabled["market"]["feesEnabled"] = json!(false);
        fees_disabled["market"]
            .as_object_mut()
            .unwrap()
            .remove("makerBaseFee");
        fees_disabled["market"]["takerBaseFee"] = Value::Null;
        let contract = metadata_contract(&fees_disabled).unwrap();
        assert_eq!(contract["market"]["makerBaseFee"], Value::Null);
        assert_eq!(contract["market"]["takerBaseFee"], Value::Null);

        let mut enabled_without_fee = metadata("BTCUSDT");
        enabled_without_fee["market"]["makerBaseFee"] = Value::Null;
        assert!(metadata_contract(&enabled_without_fee).is_err());
    }

    #[test]
    fn trade_parity_uses_mature_event_time_with_the_full_retrieval_cutoff() {
        let (_root, config) = fixture();
        append_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &trade_at("late-provider-trade", 950),
            "1970-01-01T00:16:30Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(evidence["metrics"]["legacy_trade_count"], 1);
        assert_eq!(evidence["metrics"]["rust_trade_count"], 1);
        assert_eq!(evidence["metrics"]["trade_maturity_lag_seconds"], 600);
        assert_eq!(
            evidence["metrics"]["trade_event_window_started_at_unix"],
            100
        );
        assert_eq!(evidence["metrics"]["trade_event_window_ended_at_unix"], 400);

        let (_root, config) = fixture();
        append_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &trade_at("mature-provider-trade", 300),
            "1970-01-01T00:05:50Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["dedupe_parity"], false);
        assert_eq!(
            evidence["metrics"]["legacy_only_trade_ids"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(evidence["metrics"]["rust_only_trade_ids"], json!([]));
    }

    #[test]
    fn trade_context_joins_metadata_by_market_id_outside_event_projection() {
        let (_root, config) = fixture();
        let mut future_metadata = extra_metadata();
        future_metadata["market"]["startDate"] = json!("1970-01-01T00:28:20Z");
        future_metadata["market"]["endDate"] = json!("1970-01-01T00:33:20Z");
        let mut future_trade = trade("future-market-trade");
        future_trade["market_id"] = json!("market-extra");
        future_trade["condition_id"] = json!("condition-extra");
        future_trade["trade"]["conditionId"] = json!("condition-extra");
        let record_id = derived_trade_record_id(future_trade["trade"].as_object().unwrap());
        future_trade["record_id"] = Value::String(record_id);

        for spool in [&config.legacy_spool, &config.rust_spool] {
            append_tape(
                &spool.join(ACTIVE_TAPE),
                &future_metadata,
                "1970-01-01T00:03:21Z",
            );
            append_tape(
                &spool.join(ACTIVE_TAPE),
                &future_trade,
                "1970-01-01T00:03:21Z",
            );
        }

        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(
            evidence["metrics"]["legacy_trade_metadata_context_match"],
            true
        );
        assert_eq!(
            evidence["metrics"]["rust_trade_metadata_context_match"],
            true
        );

        let (_root, config) = fixture();
        append_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &future_metadata,
            "1970-01-01T00:03:21Z",
        );
        for spool in [&config.legacy_spool, &config.rust_spool] {
            append_tape(
                &spool.join(ACTIVE_TAPE),
                &future_trade,
                "1970-01-01T00:03:21Z",
            );
        }
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(
            evidence["metrics"]["legacy_trade_metadata_context_match"],
            true
        );
        assert_eq!(
            evidence["metrics"]["rust_trade_metadata_context_match"],
            false
        );
        assert_eq!(
            evidence["metrics"]["rust_trade_metadata_context_mismatch_market_ids"],
            json!(["market-extra"])
        );
    }

    #[test]
    fn trade_context_compares_governed_metadata_outside_event_projection() {
        let (_root, config) = fixture();
        let mut future_metadata = extra_metadata();
        future_metadata["market"]["startDate"] = json!("1970-01-01T00:28:20Z");
        future_metadata["market"]["endDate"] = json!("1970-01-01T00:33:20Z");
        let mut rust_future_metadata = future_metadata.clone();
        rust_future_metadata["market"]["orderMinSize"] = json!(10);
        let mut future_trade = trade("future-market-governed-mismatch");
        future_trade["market_id"] = json!("market-extra");
        future_trade["condition_id"] = json!("condition-extra");
        future_trade["trade"]["conditionId"] = json!("condition-extra");
        let record_id = derived_trade_record_id(future_trade["trade"].as_object().unwrap());
        future_trade["record_id"] = Value::String(record_id);

        append_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &future_metadata,
            "1970-01-01T00:03:21Z",
        );
        append_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &rust_future_metadata,
            "1970-01-01T00:03:21Z",
        );
        for spool in [&config.legacy_spool, &config.rust_spool] {
            append_tape(
                &spool.join(ACTIVE_TAPE),
                &future_trade,
                "1970-01-01T00:03:21Z",
            );
        }

        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["metadata_parity"], true);
        assert_eq!(evidence["checks"]["dedupe_parity"], false);
        assert_eq!(
            evidence["metrics"]["trade_metadata_shared_values_match"],
            false
        );
        assert_eq!(
            evidence["metrics"]["trade_metadata_shared_value_mismatch_market_ids"],
            json!(["market-extra"])
        );
    }

    #[test]
    fn rust_metadata_superset_is_allowed_but_legacy_omission_is_not() {
        let (_root, config) = fixture();
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[extra_metadata()],
            "1970-01-01T00:03:21Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(
            evidence["metrics"]["rust_only_metadata_ids"],
            json!(["market-extra"])
        );

        let (_root, config) = fixture();
        append_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &extra_metadata(),
            "1970-01-01T00:03:21Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["metadata_parity"], false);
        assert_eq!(
            evidence["metrics"]["legacy_only_metadata_ids"],
            json!(["market-extra"])
        );
    }

    #[test]
    fn settlement_parity_uses_mature_event_time_not_retrieval_time() {
        let rust_row = TapeRow {
            recorded_at: 350,
            update: settlement(),
        };
        let legacy_row = TapeRow {
            recorded_at: 800,
            update: settlement(),
        };
        let rust = settlement_map(&[rust_row], 400, 1000).unwrap();
        let legacy = settlement_map(&[legacy_row], 400, 1000).unwrap();
        assert_eq!(rust, legacy);
        assert_eq!(
            map_ids(&rust),
            BTreeSet::from(["market-BTCUSDT".to_owned()])
        );
    }

    #[test]
    fn stale_settlement_is_skipped_before_parsing_legacy_event_time() {
        let mut stale = settlement();
        stale["market"]["endDate"] = Value::Null;
        stale["market"]["endDateIso"] = Value::Null;
        let rows = [TapeRow {
            recorded_at: 50,
            update: stale,
        }];

        let settlements = settlement_map(&rows, 1_000, 2_000).unwrap();
        assert!(settlements.is_empty());
    }

    #[test]
    fn provider_only_settlement_fields_do_not_poison_governed_parity() {
        let (_root, config) = fixture();
        let mut legacy_rows = fixture_rows();
        let legacy_settlement = legacy_rows.last_mut().unwrap();
        legacy_settlement["provider_poll_sequence"] = json!(41);
        legacy_settlement["market_end_epoch"] = json!(1);
        legacy_settlement["market"]["updatedAt"] = json!("1970-01-01T00:03:20Z");
        legacy_settlement["market"]["volume"] = json!("12.5");
        write_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &legacy_rows,
            "1970-01-01T00:03:20Z",
        );

        let mut rust_rows = fixture_rows();
        let rust_settlement = rust_rows.last_mut().unwrap();
        rust_settlement["provider_poll_sequence"] = json!(42);
        rust_settlement["market_end_epoch"] = json!(999);
        rust_settlement["market"]["updatedAt"] = json!("1970-01-01T00:03:21Z");
        rust_settlement["market"]["volume"] = json!("13.0");
        write_tape(
            &config
                .rust_spool
                .join("market-updates.19700101T000400000000.ndjson"),
            &rust_rows,
            "1970-01-01T00:03:21Z",
        );

        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(evidence["metrics"]["settlement_shared_values_match"], true);
    }

    #[test]
    fn governed_settlement_decision_difference_still_fails_parity() {
        let (_root, config) = fixture();
        let mut rust_rows = fixture_rows();
        rust_rows.last_mut().unwrap()["resolution_source"] = json!("different_governed_source");
        write_tape(
            &config
                .rust_spool
                .join("market-updates.19700101T000400000000.ndjson"),
            &rust_rows,
            "1970-01-01T00:03:21Z",
        );

        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["settlement_parity"], false);
        assert_eq!(evidence["metrics"]["settlement_shared_values_match"], false);
    }

    #[test]
    fn rust_only_settlement_must_be_valid_and_bound_to_metadata() {
        let (_root, config) = fixture();
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[extra_metadata(), extra_settlement()],
            "1970-01-01T00:03:21Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(
            evidence["metrics"]["rust_only_settlement_ids"],
            json!(["market-extra"])
        );

        let (_root, config) = fixture();
        let mut malformed = extra_settlement();
        malformed["retrieved_at"] = Value::Null;
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[extra_metadata(), malformed],
            "1970-01-01T00:03:21Z",
        );
        assert!(compare(&config).is_err());

        let (_root, config) = fixture();
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[extra_settlement()],
            "1970-01-01T00:03:21Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], false);
        assert_eq!(evidence["checks"]["settlement_parity"], false);
        assert_eq!(
            evidence["metrics"]["rust_settlement_metadata_context_mismatch_market_ids"],
            json!(["market-extra"])
        );
    }

    #[test]
    fn historical_legacy_only_raw_field_does_not_poison_contract_parity() {
        let (_root, config) = fixture();
        let mut historical = trade("historical-trade");
        historical["trade_ts_unix"] = Value::from(0);
        historical["trade"]["timestamp"] = Value::from(0);
        historical["legacy_only_historical_field"] = Value::Bool(true);
        append_tape(
            &config.legacy_spool.join(ACTIVE_TAPE),
            &historical,
            "1970-01-01T00:00:50Z",
        );
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(evidence["checks"]["field_parity"], true);
    }

    #[test]
    fn rust_only_metadata_must_match_its_raw_market_context() {
        let (_root, config) = fixture();
        let mut malformed = extra_metadata();
        malformed["market"]["question"] = Value::String("ETHUSDT Up or Down".to_owned());
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[malformed],
            "1970-01-01T00:03:21Z",
        );
        assert!(compare(&config).is_err());
    }

    #[test]
    fn malformed_active_rust_trade_fails_before_rotation() {
        let (_root, config) = fixture();
        let mut malformed = trade("malformed");
        malformed["received_at"] = Value::Null;
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[malformed],
            "1970-01-01T00:03:21Z",
        );
        assert!(compare(&config).is_err());
    }

    #[test]
    fn evidence_is_written_even_when_semantic_parity_fails() {
        let (_root, config) = fixture();
        write_tape(
            &config.rust_spool.join(ACTIVE_TAPE),
            &[trade("trade-1")],
            "1970-01-01T00:03:21Z",
        );
        assert!(!verify_shadow_parity(&config).unwrap());
        let evidence: Value = serde_json::from_slice(&fs::read(&config.output).unwrap()).unwrap();
        assert_eq!(evidence["passed"], false);
    }
}
