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
const METADATA_CONTRACT_FIELDS: [&str; 23] = [
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
    "active",
    "closed",
    "acceptingOrders",
    "enableOrderBook",
    "negRisk",
    "umaEndDate",
    "umaEndDateIso",
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

fn field_paths(value: &Value, prefix: &str, paths: &mut BTreeSet<String>) {
    let Some(object) = value.as_object() else {
        if !prefix.is_empty() {
            paths.insert(prefix.to_owned());
        }
        return;
    };
    for (key, child) in object {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        paths.insert(path.clone());
        if child.is_object() {
            field_paths(child, &path, paths);
        }
    }
}

fn observed_fields(rows: &[TapeRow], ended_at: i64) -> BTreeMap<String, BTreeSet<String>> {
    let mut result = BTreeMap::<String, BTreeSet<String>>::new();
    for row in rows.iter().filter(|row| row.recorded_at <= ended_at) {
        let kind = row.update["kind"].as_str().expect("kind was validated");
        field_paths(&row.update, "", result.entry(kind.to_owned()).or_default());
    }
    result
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

fn metadata_contract(value: &Value) -> Result<Value> {
    let update = value
        .as_object()
        .context("metadata update is not an object")?;
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .context("metadata market is not an object")?;
    let market_id = required_text(update, "market_id", "metadata")?;
    let condition_id = required_text(update, "condition_id", "metadata")?;
    let symbol = required_text(update, "symbol", "metadata")?;
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
        "market_id": market_id,
        "condition_id": condition_id,
        "symbol": symbol,
        "market_window_secs": window,
        "source": update.get("source").cloned().unwrap_or(Value::Null),
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
        let contract = metadata_contract(&row.update)?;
        let market = row.update["market"]
            .as_object()
            .expect("metadata contract requires a market object");
        let end_epoch = parse_timestamp(market.get("endDate"))
            .or_else(|| parse_timestamp(market.get("endDateIso")))
            .ok_or_else(|| anyhow!("metadata {} has no valid end time", contract["market_id"]))?;
        if !(started_at.saturating_sub(900)..=ended_at.saturating_add(900)).contains(&end_epoch) {
            continue;
        }
        let identity = contract["market_id"]
            .as_str()
            .expect("metadata contract has a market_id")
            .to_owned();
        result.insert(identity, contract);
    }
    Ok(result)
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
    ended_at: i64,
) -> Result<(BTreeMap<String, Value>, Vec<String>)> {
    let mut result = BTreeMap::new();
    let mut counts = BTreeMap::<String, u64>::new();
    for row in rows.iter().filter(|row| row.recorded_at <= ended_at) {
        if row.update["kind"] != "polymarket_trade" {
            continue;
        }
        let Some(timestamp) = row.update.get("trade_ts_unix").and_then(Value::as_i64) else {
            bail!("trade has an invalid trade_ts_unix");
        };
        if !(started_at..=ended_at).contains(&timestamp) {
            continue;
        }
        let record_id = row
            .update
            .get("record_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("trade has an empty record_id")?
            .to_owned();
        *counts.entry(record_id.clone()).or_default() += 1;
        result.insert(record_id, row.update.clone());
    }
    let duplicates = counts
        .into_iter()
        .filter_map(|(identity, count)| (count > 1).then_some(identity))
        .collect();
    Ok((result, duplicates))
}

fn settlement_map(
    rows: &[TapeRow],
    started_at: i64,
    ended_at: i64,
) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|row| (started_at..=ended_at).contains(&row.recorded_at))
    {
        if row.update["kind"] != "market_settlement" {
            continue;
        }
        let market_id = row
            .update
            .get("market_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("settlement has an empty market_id")?
            .to_owned();
        result.insert(market_id, row.update.clone());
    }
    Ok(result)
}

fn map_ids(values: &BTreeMap<String, Value>) -> BTreeSet<String> {
    values.keys().cloned().collect()
}

fn shared_values_match(
    left: &BTreeMap<String, Value>,
    right: &BTreeMap<String, Value>,
) -> Result<bool> {
    for identity in map_ids(left).intersection(&map_ids(right)) {
        if canonical_bytes(&left[identity])? != canonical_bytes(&right[identity])? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn required_fields_present(fields: &BTreeMap<String, BTreeSet<String>>) -> bool {
    let required = [
        (
            "market_metadata",
            [
                "kind",
                "market_id",
                "condition_id",
                "symbol",
                "market_window_secs",
                "source",
                "retrieved_at",
                "market",
            ]
            .as_slice(),
        ),
        (
            "polymarket_trade",
            [
                "kind",
                "record_id",
                "record_id_version",
                "market_id",
                "condition_id",
                "symbol",
                "trade_ts_unix",
                "received_at",
                "trade",
            ]
            .as_slice(),
        ),
        (
            "market_settlement",
            [
                "kind",
                "market_id",
                "condition_id",
                "symbol",
                "winning_token_id",
                "winning_outcome",
                "resolution_source",
                "retrieved_at",
                "market",
            ]
            .as_slice(),
        ),
    ];
    required.into_iter().all(|(kind, required)| {
        fields
            .get(kind)
            .is_some_and(|present| required.iter().all(|field| present.contains(*field)))
    })
}

fn legacy_fields_preserved(
    legacy: &BTreeMap<String, BTreeSet<String>>,
    rust: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    legacy.iter().all(|(kind, fields)| {
        rust.get(kind)
            .is_some_and(|present| fields.is_subset(present))
    })
}

fn compare(config: &ShadowParityConfig) -> Result<Value> {
    if config.ended_at_unix <= config.started_at_unix {
        bail!("parity window end must be after its start");
    }
    let (legacy_rows, _, legacy_active) = load_rows(&config.legacy_spool)?;
    let (rust_rows, rust_closed, rust_active) = load_rows(&config.rust_spool)?;

    let legacy_fields = observed_fields(&legacy_rows, config.ended_at_unix);
    let rust_fields = observed_fields(&rust_rows, config.ended_at_unix);
    let field_parity = required_fields_present(&rust_fields)
        && legacy_fields_preserved(&legacy_fields, &rust_fields);

    let legacy_metadata = metadata_map(&legacy_rows, config.started_at_unix, config.ended_at_unix)?;
    let rust_metadata = metadata_map(&rust_rows, config.started_at_unix, config.ended_at_unix)?;
    let legacy_metadata_ids = map_ids(&legacy_metadata);
    let rust_metadata_ids = map_ids(&rust_metadata);
    let metadata_parity = !legacy_metadata_ids.is_empty()
        && legacy_metadata_ids == rust_metadata_ids
        && shared_values_match(&legacy_metadata, &rust_metadata)?;

    let (legacy_trades, legacy_duplicates) =
        trade_map(&legacy_rows, config.started_at_unix, config.ended_at_unix)?;
    let (rust_trades, rust_duplicates) =
        trade_map(&rust_rows, config.started_at_unix, config.ended_at_unix)?;
    let legacy_trade_ids = map_ids(&legacy_trades);
    let rust_trade_ids = map_ids(&rust_trades);
    let trade_bytes_match = shared_values_match(&legacy_trades, &rust_trades)?;
    let dedupe_parity = legacy_duplicates.is_empty()
        && rust_duplicates.is_empty()
        && !legacy_trade_ids.is_empty()
        && legacy_trade_ids == rust_trade_ids;

    let legacy_settlements =
        settlement_map(&legacy_rows, config.started_at_unix, config.ended_at_unix)?;
    let rust_settlements =
        settlement_map(&rust_rows, config.started_at_unix, config.ended_at_unix)?;
    let legacy_settlement_ids = map_ids(&legacy_settlements);
    let rust_settlement_ids = map_ids(&rust_settlements);
    let settlement_parity = !legacy_settlement_ids.is_empty()
        && legacy_settlement_ids.is_subset(&rust_settlement_ids)
        && shared_values_match(&legacy_settlements, &rust_settlements)?;

    let rust_symbols = rust_metadata
        .values()
        .filter_map(|value| value.get("symbol").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let asset_parity = EXPECTED_SYMBOLS
        .iter()
        .all(|symbol| rust_symbols.contains(*symbol));
    let rotation_parity = legacy_active && rust_active && rust_closed >= 1;
    let byte_parity = metadata_parity && dedupe_parity && trade_bytes_match && settlement_parity;

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
    Ok(json!({
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
            "legacy_duplicate_trade_ids": legacy_duplicates,
            "rust_duplicate_trade_ids": rust_duplicates,
            "legacy_settlement_count": legacy_settlement_ids.len(),
            "rust_settlement_count": rust_settlement_ids.len(),
            "rust_closed_tape_count": rust_closed,
            "rust_symbols": rust_symbols,
            "normalized_trade_sha256": digest_values(rust_trades.values())?,
            "normalized_metadata_sha256": digest_values(rust_metadata.values())?,
        },
    }))
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

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "monday-polymarket-parity-{}-{:016x}",
                std::process::id(),
                random::<u64>()
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn metadata(symbol: &str) -> Value {
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
                "question":format!("{symbol} Up or Down"),
                "slug":format!("market-{symbol}"),
                "startDate":"1970-01-01T00:00:00Z",
                "endDate":"1970-01-01T00:05:00Z",
                "outcomes":["Up","Down"],
                "clobTokenIds":[format!("up-{symbol}"),format!("down-{symbol}")],
                "orderPriceMinTickSize":0.01,
                "orderMinSize":5,
                "feesEnabled":true
            }
        })
    }

    fn trade(record_id: &str) -> Value {
        json!({
            "kind":"polymarket_trade","record_id":record_id,"record_id_version":"v2",
            "market_id":"market-BTCUSDT","condition_id":"condition-BTCUSDT",
            "token_id":"up-BTCUSDT","symbol":"BTCUSDT","market_window_secs":300,
            "side":"BUY","size":"1","price":"0.5",
            "trade_ts":"1970-01-01T00:03:20Z","trade_ts_unix":200,
            "transaction_hash":"0x1","proxy_wallet":"0x2","outcome":"Up",
            "outcome_index":0,"source":"polymarket_data_api",
            "received_at":"1970-01-01T00:03:20Z",
            "trade":{"transactionHash":"0x1","conditionId":"condition-BTCUSDT",
                "asset":"up-BTCUSDT","side":"BUY","timestamp":200,
                "proxyWallet":"0x2","size":"1","price":"0.5",
                "outcomeIndex":0,"outcome":"Up"}
        })
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
            &json!({"sequence":legacy_rows.len() - 1,"recorded_at":"1970-01-01T00:05:01Z","update":legacy_rows.last().unwrap()}),
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
            ended_at_unix: 300,
            output: root.path().join("parity.json"),
        };
        (root, config)
    }

    #[test]
    fn bounded_parity_ignores_delayed_trade_recorded_after_cutoff() {
        let (_root, config) = fixture();
        let evidence = compare(&config).unwrap();
        assert_eq!(evidence["passed"], true);
        assert_eq!(evidence["checks"]["metadata_parity"], true);
        assert_eq!(evidence["metrics"]["legacy_trade_count"], 1);
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
