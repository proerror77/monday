//! Incremental validation contract for closed Polymarket market tapes.

#![recursion_limit = "256"]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const POLYMARKET_TAPE_SEAL_SCHEMA: &str = "monday.polymarket.tape-seal.v1";
pub const POLYMARKET_MARKET_TAPE_DATASET: &str = "crypto_expiry";
pub const POLYMARKET_MARKET_TAPE_QUOTE_DEPTH_LEVELS: usize = 0;
pub const POLYMARKET_MARKET_TAPE_QUOTE_SAMPLE_MS: u64 = 0;
const MAX_FUTURE_RECORDING_SKEW_SECS: i64 = 300;
const MAX_QUOTE_SOURCE_REGRESSION_MS: i64 = 30_000;
const ALLOWED_KINDS: [&str; 8] = [
    "quote",
    "quote_collection_failure",
    "event_discovered",
    "event_expired",
    "reference_price",
    "spot_price",
    "agg_trade",
    "l2",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TapeFileIdentity {
    pub device: u64,
    pub inode: u64,
    pub bytes: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
}

impl TapeFileIdentity {
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

pub fn tape_seal_path(source: &Path) -> std::result::Result<PathBuf, TapeValidationError> {
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| TapeValidationError("source file name is not UTF-8".to_owned()))?;
    Ok(source.with_file_name(format!("{name}.seal.json")))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolymarketTapeSeal {
    pub schema: String,
    pub source_file: String,
    pub source_identity: TapeFileIdentity,
    pub manifest: Value,
}

/// Returns true only for the complete manifest shape emitted by
/// [`MarketTapeManifestBuilder`]. Identity, dataset, and recording-policy
/// bindings remain uploader responsibilities.
pub fn complete_market_tape_manifest_shape(manifest: &Value) -> bool {
    let Some(manifest) = manifest.as_object() else {
        return false;
    };
    let strings = [
        "schema",
        "venue",
        "dataset",
        "format",
        "replay_scope",
        "start_recorded_at",
        "end_recorded_at",
        "date",
        "hour",
        "source_file",
    ];
    let booleans = [
        "canonical",
        "venue_depth_complete",
        "temporal_updates_complete",
        "segment_complete",
        "source_session_closed",
        "event_context_complete",
        "quote_coverage_complete",
        "quote_quality_complete",
        "lifecycle_integrity_complete",
        "reference_context_complete",
    ];
    let unsigned = [
        "events",
        "start_sequence",
        "end_sequence",
        "sequence_gaps",
        "token_count",
        "market_count",
        "condition_count",
        "source_bytes",
    ];
    let arrays = [
        "expired_before_discovery_tokens",
        "missing_quote_tokens",
        "missing_quote_attempt_tokens",
        "contextless_quote_tokens",
        "symbols",
        "record_id_versions",
    ];
    let objects = [
        "event_types",
        "recording_policy",
        "field_presence",
        "field_non_null",
        "source_field_presence",
        "source_field_non_null",
        "quality",
        "trade_completions",
        "binance_reference_counts",
    ];
    if !strings
        .into_iter()
        .all(|field| manifest.get(field).and_then(Value::as_str).is_some())
        || !booleans
            .into_iter()
            .all(|field| manifest.get(field).is_some_and(Value::is_boolean))
        || !unsigned
            .into_iter()
            .all(|field| manifest.get(field).and_then(Value::as_u64).is_some())
        || !arrays
            .into_iter()
            .all(|field| manifest.get(field).is_some_and(Value::is_array))
        || !objects
            .into_iter()
            .all(|field| manifest.get(field).is_some_and(Value::is_object))
    {
        return false;
    }
    if ["start_recorded_at", "end_recorded_at"]
        .into_iter()
        .any(|field| {
            manifest
                .get(field)
                .and_then(Value::as_str)
                .is_none_or(|value| DateTime::parse_from_rfc3339(value).is_err())
        })
    {
        return false;
    }
    let date_is_valid = manifest
        .get("date")
        .and_then(Value::as_str)
        .is_some_and(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok());
    let hour_is_valid = manifest
        .get("hour")
        .and_then(Value::as_str)
        .is_some_and(|value| value.len() == 2 && value.parse::<u8>().is_ok_and(|hour| hour < 24));
    let event_types_are_counts = manifest["event_types"]
        .as_object()
        .is_some_and(|values| values.values().all(|value| value.as_u64().is_some()));
    let quality = &manifest["quality"];
    let quality_is_complete = [
        "crossed_quotes",
        "executable_quotes",
        "missing_bid_size",
        "missing_ask_size",
        "incomplete_quotes",
        "max_quote_latency_ms",
        "tolerated_quote_source_regressions",
        "request_attempts",
        "request_successes",
        "request_failures",
        "transport_reconnects",
        "max_request_latency_ms",
        "one_sided_quotes",
        "empty_quotes",
        "non_executable_quotes",
        "out_of_range_prices",
        "negative_sizes",
        "max_bid_levels",
        "max_ask_levels",
        "contextless_quotes",
        "duplicate_record_ids",
    ]
    .into_iter()
    .all(|field| quality.get(field).and_then(Value::as_u64).is_some())
        && quality
            .get("executable_quote_ratio")
            .and_then(Value::as_f64)
            .is_some();
    date_is_valid && hour_is_valid && event_types_are_counts && quality_is_complete
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct TapeValidationError(String);

type Result<T> = std::result::Result<T, TapeValidationError>;

fn invalid<T>(line: usize, message: impl Into<String>) -> Result<T> {
    Err(TapeValidationError(format!(
        "line {line}: {}",
        message.into()
    )))
}

fn timestamp(value: Option<&Value>, field: &str, line: usize) -> Result<DateTime<Utc>> {
    let Some(value) = value.and_then(Value::as_str) else {
        return invalid(line, format!("{field} must be a string"));
    };
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| TapeValidationError(format!("line {line}: invalid {field}: {value}")))
}

fn text<'a>(update: &'a Map<String, Value>, field: &str, line: usize) -> Result<&'a str> {
    update
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| TapeValidationError(format!("line {line}: update requires {field}")))
}

fn decimal(value: Option<&Value>, field: &str, line: usize) -> Result<Option<Decimal>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let value = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    Decimal::from_str(&value)
        .or_else(|_| Decimal::from_scientific(&value))
        .map(Some)
        .map_err(|_| TapeValidationError(format!("line {line}: {field} must be numeric")))
}

fn tradeable(price: Decimal) -> bool {
    price > Decimal::new(2, 2) && price < Decimal::new(98, 2)
}

fn levels(value: Option<&Value>, line: usize) -> Result<&[Value]> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(levels)) => Ok(levels),
        _ => invalid(line, "quote levels must be arrays"),
    }
}

fn quote_level(level: &Value, side: &str, index: usize, line: usize) -> Result<(Decimal, Decimal)> {
    let Some(level) = level.as_object() else {
        return invalid(line, format!("{side}[{index}] must be an object"));
    };
    let Some(price) = decimal(level.get("price"), "level.price", line)? else {
        return invalid(line, format!("{side}[{index}] requires price and size"));
    };
    let Some(size) = decimal(level.get("size"), "level.size", line)? else {
        return invalid(line, format!("{side}[{index}] requires price and size"));
    };
    Ok((price, size))
}

fn increment(counts: &mut BTreeMap<String, BTreeMap<String, u64>>, kind: &str, field: &str) {
    let fields = if counts.contains_key(kind) {
        counts.get_mut(kind).expect("existing kind")
    } else {
        counts.entry(kind.to_owned()).or_default()
    };
    if let Some(count) = fields.get_mut(field) {
        *count += 1;
    } else {
        fields.insert(field.to_owned(), 1);
    }
}

/// Borrowed quote metadata for the typed recorder. Depth remains an iterator
/// over the original Decimal levels, so validation need not build a JSON tree
/// and parse those same levels back from strings.
#[derive(Clone, Copy)]
pub struct TapeQuote<'a> {
    pub token_id: &'a str,
    pub source_at: DateTime<Utc>,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
    pub bid_size: Option<Decimal>,
    pub ask_size: Option<Decimal>,
    pub collection_result: &'a str,
}

#[derive(Debug, Default)]
pub struct MarketTapeManifestBuilder {
    observed_records: usize,
    event_types: BTreeMap<String, u64>,
    present_fields: BTreeMap<String, BTreeMap<String, u64>>,
    non_null_fields: BTreeMap<String, BTreeMap<String, u64>>,
    symbols: BTreeSet<String>,
    token_ids: BTreeSet<String>,
    known_event_tokens: BTreeSet<String>,
    event_end_times: BTreeMap<String, DateTime<Utc>>,
    event_token_lifecycles: BTreeMap<String, (Option<String>, Option<DateTime<Utc>>)>,
    expired_before_discovery_tokens: BTreeSet<String>,
    lifecycle_integrity_complete: bool,
    quoted_token_ids: BTreeSet<String>,
    attempted_quote_token_ids: BTreeSet<String>,
    contextless_quote_tokens: BTreeSet<String>,
    last_quote_source_at: BTreeMap<String, DateTime<Utc>>,
    first_recorded_at: Option<String>,
    last_recorded_at: Option<String>,
    previous_recorded_at: Option<DateTime<Utc>>,
    first_sequence: Option<u64>,
    last_sequence: Option<u64>,
    expected_sequence: Option<u64>,
    crossed_quotes: u64,
    one_sided_quotes: u64,
    empty_quotes: u64,
    non_executable_quotes: u64,
    executable_quotes: u64,
    missing_bid_size: u64,
    missing_ask_size: u64,
    incomplete_quotes: u64,
    max_quote_latency_ms: i64,
    tolerated_quote_source_regressions: u64,
    request_attempts: u64,
    request_failures: u64,
    transport_reconnects: u64,
    pending_transport_reconnects: BTreeMap<String, u64>,
    max_request_latency_ms: i64,
    out_of_range_prices: u64,
    negative_sizes: u64,
    max_bid_levels: usize,
    max_ask_levels: usize,
    contextless_quotes: u64,
    market_ids: BTreeSet<String>,
    condition_ids: BTreeSet<String>,
}

impl MarketTapeManifestBuilder {
    pub fn new() -> Self {
        Self {
            lifecycle_integrity_complete: true,
            ..Self::default()
        }
    }

    pub fn observe(&mut self, record: &Value, validation_time: DateTime<Utc>) -> Result<()> {
        let line = self.observed_records.saturating_add(1);
        let Some(record) = record.as_object() else {
            return invalid(line, "record must be an object");
        };
        let Some(sequence) = record.get("sequence").and_then(Value::as_u64) else {
            return invalid(line, "sequence must be a non-negative integer");
        };
        let recorded_at = timestamp(record.get("recorded_at"), "recorded_at", line)?;
        self.observe_header(sequence, recorded_at, validation_time)?;
        let Some(update) = record.get("update").and_then(Value::as_object) else {
            return invalid(line, "update must be an object");
        };
        let Some(kind) = update.get("kind").and_then(Value::as_str) else {
            return invalid(line, "unsupported update kind None");
        };
        if !ALLOWED_KINDS.contains(&kind) {
            return invalid(
                line,
                format!("unsupported incremental seal update kind {kind:?}"),
            );
        }

        *self.event_types.entry(kind.to_owned()).or_default() += 1;
        for (field, value) in update {
            increment(&mut self.present_fields, kind, field);
            if !value.is_null() {
                increment(&mut self.non_null_fields, kind, field);
            }
        }
        if let Some(symbol) = update
            .get("symbol")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.symbols.insert(symbol.to_owned());
        }
        let token_id = update.get("token_id").and_then(Value::as_str);
        if let Some(token_id) = token_id.filter(|value| !value.is_empty()) {
            self.token_ids.insert(token_id.to_owned());
        }
        if let Some(market_id) = update
            .get("market_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.market_ids.insert(market_id.to_owned());
        }
        if let Some(condition_id) = update
            .get("condition_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            self.condition_ids.insert(condition_id.to_owned());
        }

        self.observe_lifecycle(kind, update, recorded_at, validation_time);
        match kind {
            "quote" => self.observe_quote(update, recorded_at, line)?,
            "quote_collection_failure" => self.observe_quote_failure(update, recorded_at, line)?,
            "spot_price" => validate_spot_price(update, line)?,
            "agg_trade" => validate_agg_trade(update, line)?,
            "l2" => validate_l2(update, line)?,
            _ => {}
        }

        self.finish_record(
            sequence,
            recorded_at,
            record["recorded_at"]
                .as_str()
                .expect("validated timestamp")
                .to_owned(),
        )
    }

    fn observe_header(
        &mut self,
        sequence: u64,
        recorded_at: DateTime<Utc>,
        validation_time: DateTime<Utc>,
    ) -> Result<()> {
        let line = self.observed_records.saturating_add(1);
        let expected = self.expected_sequence.get_or_insert(sequence);
        if sequence != *expected {
            return invalid(
                line,
                format!("sequence gap expected={} actual={sequence}", *expected),
            );
        }
        if recorded_at > validation_time + TimeDelta::seconds(MAX_FUTURE_RECORDING_SKEW_SECS) {
            return invalid(
                line,
                format!("recorded_at is more than {MAX_FUTURE_RECORDING_SKEW_SECS}s in the future"),
            );
        }
        if self
            .previous_recorded_at
            .is_some_and(|previous| recorded_at < previous)
        {
            return invalid(line, "recorded_at moved backwards");
        }
        Ok(())
    }

    fn finish_record(
        &mut self,
        sequence: u64,
        recorded_at: DateTime<Utc>,
        recorded_at_text: String,
    ) -> Result<()> {
        let line = self.observed_records.saturating_add(1);
        self.first_recorded_at
            .get_or_insert_with(|| recorded_at_text.clone());
        self.last_recorded_at = Some(recorded_at_text);
        self.first_sequence.get_or_insert(sequence);
        self.last_sequence = Some(sequence);
        self.expected_sequence = sequence.checked_add(1);
        self.observed_records = self.observed_records.saturating_add(1);
        if self.expected_sequence.is_none() {
            return invalid(line, "sequence overflow");
        }
        self.previous_recorded_at = Some(recorded_at);
        Ok(())
    }

    /// Observe exactly the quote schema emitted by the recorder, borrowing all
    /// levels. JSON import and this path share the same quote-quality checks.
    pub fn observe_typed_quote<I, J>(
        &mut self,
        sequence: u64,
        recorded_at: DateTime<Utc>,
        validation_time: DateTime<Utc>,
        quote: TapeQuote<'_>,
        bids: I,
        asks: J,
    ) -> Result<()>
    where
        I: ExactSizeIterator<Item = (Decimal, Decimal)>,
        J: ExactSizeIterator<Item = (Decimal, Decimal)>,
    {
        self.observe_header(sequence, recorded_at, validation_time)?;
        if let Some(count) = self.event_types.get_mut("quote") {
            *count += 1;
        } else {
            self.event_types.insert("quote".to_owned(), 1);
        }
        for (field, non_null) in [
            ("kind", true),
            ("token_id", true),
            ("ts", true),
            ("bid", quote.bid.is_some()),
            ("ask", quote.ask.is_some()),
            ("bid_size", quote.bid_size.is_some()),
            ("ask_size", quote.ask_size.is_some()),
            ("bid_levels", true),
            ("ask_levels", true),
            ("request_status", true),
            ("collection_result", true),
        ] {
            increment(&mut self.present_fields, "quote", field);
            if non_null {
                increment(&mut self.non_null_fields, "quote", field);
            }
        }
        if !quote.token_id.is_empty() && !self.token_ids.contains(quote.token_id) {
            self.token_ids.insert(quote.token_id.to_owned());
        }
        let line = self.observed_records.saturating_add(1);
        self.observe_quote_attempt_token(quote.token_id, line)?;
        self.observe_quote_values(quote, recorded_at, line, bids.map(Ok), asks.map(Ok))?;
        self.finish_record(
            sequence,
            recorded_at,
            recorded_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
        )
    }

    fn observe_lifecycle(
        &mut self,
        kind: &str,
        update: &Map<String, Value>,
        recorded_at: DateTime<Utc>,
        validation_time: DateTime<Utc>,
    ) {
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
                .map(|value| value.with_timezone(&Utc));
            let token_pair_complete =
                matches!((up_token, down_token), (Some(up), Some(down)) if up != down);
            let malformed = !token_pair_complete || event_id.is_none() || end_time.is_none();
            self.lifecycle_integrity_complete &= !malformed;
            if let (Some(event_id), Some(end_time)) = (event_id.as_ref(), end_time.as_ref()) {
                match self.event_end_times.get(event_id) {
                    Some(previous) if previous != end_time => {
                        self.lifecycle_integrity_complete = false;
                    }
                    None => {
                        self.event_end_times.insert(event_id.clone(), *end_time);
                    }
                    Some(_) => {}
                }
            }
            let expired_before_discovery = !malformed
                && recorded_at <= validation_time
                && end_time.is_some_and(|end_time| end_time < recorded_at);
            let lifecycle = (event_id, end_time);
            for token in [up_token, down_token].into_iter().flatten() {
                self.known_event_tokens.insert(token.to_owned());
                match self.event_token_lifecycles.get(token) {
                    None => {
                        self.event_token_lifecycles
                            .insert(token.to_owned(), lifecycle.clone());
                        if expired_before_discovery {
                            self.expired_before_discovery_tokens
                                .insert(token.to_owned());
                        }
                    }
                    Some(previous) if previous != &lifecycle => {
                        self.lifecycle_integrity_complete = false;
                        self.expired_before_discovery_tokens.remove(token);
                    }
                    Some(_) => {}
                }
            }
        } else if kind == "event_expired" {
            let event_id = update
                .get("event_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let end_time = update
                .get("end_time")
                .and_then(Value::as_str)
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc));
            self.lifecycle_integrity_complete &= event_id.is_some() && end_time.is_some();
            if let (Some(event_id), Some(end_time)) = (event_id, end_time) {
                match self.event_end_times.get(event_id) {
                    Some(previous) if previous != &end_time => {
                        self.lifecycle_integrity_complete = false;
                    }
                    None => {
                        self.event_end_times.insert(event_id.to_owned(), end_time);
                    }
                    Some(_) => {}
                }
            }
        }
    }

    fn observe_quote(
        &mut self,
        update: &Map<String, Value>,
        recorded_at: DateTime<Utc>,
        line: usize,
    ) -> Result<()> {
        self.observe_quote_attempt(update, line)?;
        if update.get("request_status").and_then(Value::as_str) != Some("success") {
            return invalid(line, "quote requires request_status=success");
        }
        let quote = TapeQuote {
            token_id: text(update, "token_id", line)?,
            source_at: timestamp(update.get("ts"), "ts", line)?,
            bid: decimal(update.get("bid"), "bid", line)?,
            ask: decimal(update.get("ask"), "ask", line)?,
            bid_size: decimal(update.get("bid_size"), "bid_size", line)?,
            ask_size: decimal(update.get("ask_size"), "ask_size", line)?,
            collection_result: update
                .get("collection_result")
                .and_then(Value::as_str)
                .unwrap_or(""),
        };
        let bids = levels(update.get("bid_levels"), line)?
            .iter()
            .enumerate()
            .map(|(index, level)| quote_level(level, "bid_levels", index, line));
        let asks = levels(update.get("ask_levels"), line)?
            .iter()
            .enumerate()
            .map(|(index, level)| quote_level(level, "ask_levels", index, line));
        self.observe_quote_values(quote, recorded_at, line, bids, asks)
    }

    fn observe_quote_values<I, J>(
        &mut self,
        quote: TapeQuote<'_>,
        recorded_at: DateTime<Utc>,
        line: usize,
        bids: I,
        asks: J,
    ) -> Result<()>
    where
        I: ExactSizeIterator<Item = Result<(Decimal, Decimal)>>,
        J: ExactSizeIterator<Item = Result<(Decimal, Decimal)>>,
    {
        let TapeQuote {
            token_id: token,
            source_at,
            bid,
            ask,
            bid_size,
            ask_size,
            collection_result,
        } = quote;
        let latency = recorded_at
            .signed_duration_since(source_at)
            .num_milliseconds();
        if latency < 0 {
            return invalid(line, "quote source time is after received time");
        }
        self.max_quote_latency_ms = self.max_quote_latency_ms.max(latency);
        match self.last_quote_source_at.get_mut(token) {
            Some(previous) if source_at < *previous => {
                let regression = *previous - source_at;
                if regression > TimeDelta::milliseconds(MAX_QUOTE_SOURCE_REGRESSION_MS) {
                    return invalid(
                        line,
                        format!(
                            "quote source time moved backwards by {}ms",
                            regression.num_milliseconds()
                        ),
                    );
                }
                self.tolerated_quote_source_regressions += 1;
            }
            Some(previous) => *previous = source_at,
            None => {
                self.last_quote_source_at
                    .insert(token.to_owned(), source_at);
            }
        }
        if !self.quoted_token_ids.contains(token) {
            self.quoted_token_ids.insert(token.to_owned());
        }
        self.pending_transport_reconnects.remove(token);
        if bid.is_some() && bid_size.is_none() {
            self.missing_bid_size += 1;
        }
        if ask.is_some() && ask_size.is_none() {
            self.missing_ask_size += 1;
        }
        let bid_count = bids.len();
        let ask_count = asks.len();
        self.max_bid_levels = self.max_bid_levels.max(bid_count);
        self.max_ask_levels = self.max_ask_levels.max(ask_count);
        let bids_non_executable = self.observe_level_values(bids)?;
        let asks_non_executable = self.observe_level_values(asks)?;
        let all_levels_non_executable = bids_non_executable && asks_non_executable;
        if matches!((bid, ask), (Some(bid), Some(ask)) if bid > ask) {
            self.crossed_quotes += 1;
        }
        for price in [bid, ask].into_iter().flatten() {
            if !(Decimal::ZERO..=Decimal::ONE).contains(&price) {
                self.out_of_range_prices += 1;
            }
        }
        for size in [bid_size, ask_size].into_iter().flatten() {
            if size < Decimal::ZERO {
                self.negative_sizes += 1;
            }
        }
        let result = match (bid, ask, bid_size, ask_size) {
            (None, None, None, None) if bid_count == 0 && ask_count == 0 => "empty",
            (None, None, None, None) if all_levels_non_executable => "non_executable",
            (Some(bid), Some(ask), Some(bid_size), Some(ask_size))
                if bid <= ask
                    && tradeable(bid)
                    && tradeable(ask)
                    && bid_size > Decimal::ZERO
                    && ask_size > Decimal::ZERO =>
            {
                "executable"
            }
            (Some(price), None, Some(size), None) | (None, Some(price), None, Some(size))
                if tradeable(price) && size > Decimal::ZERO =>
            {
                "one_sided"
            }
            _ => "incomplete",
        };
        if collection_result != result {
            return invalid(
                line,
                format!("quote collection_result does not match {result}"),
            );
        }
        match result {
            "executable" => self.executable_quotes += 1,
            "one_sided" => self.one_sided_quotes += 1,
            "empty" => self.empty_quotes += 1,
            "non_executable" => self.non_executable_quotes += 1,
            _ => self.incomplete_quotes += 1,
        }
        Ok(())
    }

    fn observe_level_values(
        &mut self,
        levels: impl Iterator<Item = Result<(Decimal, Decimal)>>,
    ) -> Result<bool> {
        let mut all_non_executable = true;
        for level in levels {
            let (price, size) = level?;
            if !(Decimal::ZERO..=Decimal::ONE).contains(&price) {
                self.out_of_range_prices += 1;
                all_non_executable = false;
            }
            if size <= Decimal::ZERO {
                self.negative_sizes += 1;
                all_non_executable = false;
            }
            if tradeable(price) {
                all_non_executable = false;
            }
        }
        Ok(all_non_executable)
    }

    fn observe_quote_attempt(&mut self, update: &Map<String, Value>, line: usize) -> Result<()> {
        let token = text(update, "token_id", line)?;
        self.observe_quote_attempt_token(token, line)
    }

    fn observe_quote_attempt_token(&mut self, token: &str, line: usize) -> Result<()> {
        if token.is_empty() {
            return invalid(line, "token_id must be a non-empty string");
        }
        if !self.attempted_quote_token_ids.contains(token) {
            self.attempted_quote_token_ids.insert(token.to_owned());
        }
        self.request_attempts += 1;
        if !self.known_event_tokens.contains(token) {
            self.contextless_quotes += 1;
            if !self.contextless_quote_tokens.contains(token) {
                self.contextless_quote_tokens.insert(token.to_owned());
            }
        }
        Ok(())
    }

    fn observe_quote_failure(
        &mut self,
        update: &Map<String, Value>,
        recorded_at: DateTime<Utc>,
        line: usize,
    ) -> Result<()> {
        self.observe_quote_attempt(update, line)?;
        if update.get("request_status").and_then(Value::as_str) != Some("failure")
            || update.get("collection_result").and_then(Value::as_str) != Some("api_failure")
        {
            return invalid(line, "quote collection failure requires explicit status");
        }
        let started = timestamp(update.get("request_started_at"), "request_started_at", line)?;
        let failed = timestamp(update.get("ts"), "ts", line)?;
        if started > failed || failed > recorded_at {
            return invalid(line, "invalid quote request timing");
        }
        self.max_request_latency_ms = self
            .max_request_latency_ms
            .max(failed.signed_duration_since(started).num_milliseconds());
        let error_kind = text(update, "error_kind", line)?;
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
            return invalid(line, "unsupported quote collection error_kind");
        }
        let http_status_valid = match update.get("http_status") {
            None | Some(Value::Null) => error_kind != "http_status",
            Some(value) if error_kind == "http_status" => value
                .as_u64()
                .is_some_and(|value| (100..=599).contains(&value)),
            _ => false,
        };
        if !http_status_valid {
            return invalid(line, "invalid quote collection http_status");
        }
        if matches!(error_kind, "websocket_receive" | "transport_reconnect") {
            self.transport_reconnects += 1;
            let token = text(update, "token_id", line)?;
            *self
                .pending_transport_reconnects
                .entry(token.to_owned())
                .or_default() += 1;
        } else {
            self.request_failures += 1;
        }
        Ok(())
    }

    pub fn finish(
        mut self,
        dataset: &str,
        quote_depth_levels: usize,
        quote_sample_ms: u64,
        source_file: &str,
        source_bytes: u64,
    ) -> Result<Value> {
        if quote_depth_levels > 0
            && (self.max_bid_levels > quote_depth_levels
                || self.max_ask_levels > quote_depth_levels)
        {
            return invalid(1, "quote exceeds configured depth");
        }
        self.request_failures += self.pending_transport_reconnects.values().sum::<u64>();
        let Some(first_sequence) = self.first_sequence else {
            return invalid(1, "tape is empty");
        };
        let last_sequence = self
            .last_sequence
            .expect("non-empty tape has a final sequence");
        let first_recorded_at = self
            .first_recorded_at
            .expect("non-empty tape has a first timestamp");
        let last_recorded_at = self
            .last_recorded_at
            .expect("non-empty tape has a final timestamp");
        let partition = DateTime::parse_from_rfc3339(&first_recorded_at)
            .expect("recorded_at was validated")
            .with_timezone(&Utc);
        let event_context_complete = self.contextless_quotes == 0;
        let obligations = self
            .known_event_tokens
            .difference(&self.expired_before_discovery_tokens)
            .cloned()
            .collect::<BTreeSet<_>>();
        let missing_quote_tokens = obligations
            .difference(&self.quoted_token_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        let missing_quote_attempt_tokens = obligations
            .difference(&self.attempted_quote_token_ids)
            .cloned()
            .collect::<BTreeSet<_>>();
        let quote_quality_complete = self.incomplete_quotes == 0
            && self.crossed_quotes == 0
            && self.out_of_range_prices == 0
            && self.negative_sizes == 0;
        let quote_coverage_complete = missing_quote_tokens.is_empty()
            && missing_quote_attempt_tokens.is_empty()
            && self.request_failures == 0
            && event_context_complete
            && quote_quality_complete;
        let has_quotes = self.event_types.get("quote").copied().unwrap_or_default() > 0;
        let depth_complete = has_quotes && quote_depth_levels == 0;
        let temporal_updates_complete = has_quotes && quote_sample_ms == 0;
        let quote_count = self.event_types.get("quote").copied().unwrap_or_default();
        let spot_price_count = self
            .event_types
            .get("spot_price")
            .copied()
            .unwrap_or_default();
        let agg_trade_count = self
            .event_types
            .get("agg_trade")
            .copied()
            .unwrap_or_default();
        let l2_count = self.event_types.get("l2").copied().unwrap_or_default();
        let executable_quote_ratio = if quote_count == 0 {
            0.0
        } else {
            self.executable_quotes as f64 / quote_count as f64
        };
        let replay_scope = if event_context_complete {
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
        let quality = json!({
            "crossed_quotes": self.crossed_quotes,
            "executable_quotes": self.executable_quotes,
            "executable_quote_ratio": executable_quote_ratio,
            "missing_bid_size": self.missing_bid_size,
            "missing_ask_size": self.missing_ask_size,
            "incomplete_quotes": self.incomplete_quotes,
            "max_quote_latency_ms": self.max_quote_latency_ms,
            "tolerated_quote_source_regressions": self.tolerated_quote_source_regressions,
            "request_attempts": self.request_attempts,
            "request_successes": quote_count,
            "request_failures": self.request_failures,
            "transport_reconnects": self.transport_reconnects,
            "max_request_latency_ms": self.max_request_latency_ms,
            "one_sided_quotes": self.one_sided_quotes,
            "empty_quotes": self.empty_quotes,
            "non_executable_quotes": self.non_executable_quotes,
            "out_of_range_prices": self.out_of_range_prices,
            "negative_sizes": self.negative_sizes,
            "max_bid_levels": self.max_bid_levels,
            "max_ask_levels": self.max_ask_levels,
            "contextless_quotes": self.contextless_quotes,
            "duplicate_record_ids": 0,
        });
        Ok(json!({
            "schema": "monday.polymarket.raw.v1",
            "canonical": quote_coverage_complete && self.lifecycle_integrity_complete,
            "venue": "polymarket",
            "dataset": dataset,
            "format": "ndjson.zst",
            "replay_scope": replay_scope,
            "venue_depth_complete": depth_complete,
            "temporal_updates_complete": temporal_updates_complete,
            "segment_complete": quote_coverage_complete && self.lifecycle_integrity_complete,
            "source_session_closed": true,
            "event_context_complete": event_context_complete,
            "quote_coverage_complete": quote_coverage_complete,
            "quote_quality_complete": quote_quality_complete,
            "lifecycle_integrity_complete": self.lifecycle_integrity_complete,
            "expired_before_discovery_tokens": self.expired_before_discovery_tokens,
            "missing_quote_tokens": missing_quote_tokens,
            "missing_quote_attempt_tokens": missing_quote_attempt_tokens,
            "contextless_quote_tokens": self.contextless_quote_tokens,
            "events": last_sequence - first_sequence + 1,
            "event_types": self.event_types,
            "start_sequence": first_sequence,
            "end_sequence": last_sequence,
            "sequence_gaps": 0,
            "start_recorded_at": first_recorded_at,
            "end_recorded_at": last_recorded_at,
            "date": partition.format("%Y-%m-%d").to_string(),
            "hour": partition.format("%H").to_string(),
            "symbols": self.symbols,
            "token_count": self.token_ids.len(),
            "market_count": self.market_ids.len(),
            "condition_count": self.condition_ids.len(),
            "record_id_versions": BTreeSet::<String>::new(),
            "recording_policy": {
                "quote_sample_ms": quote_sample_ms,
                "quote_depth_levels": quote_depth_levels,
                "event_scoped_quotes": true,
            },
            "field_presence": self.present_fields,
            "field_non_null": self.non_null_fields,
            "source_field_presence": BTreeMap::<String, BTreeMap<String, u64>>::new(),
            "source_field_non_null": BTreeMap::<String, BTreeMap<String, u64>>::new(),
            "quality": quality,
            "source_file": source_file,
            "source_bytes": source_bytes,
            "trade_completions": BTreeMap::<String, Value>::new(),
            "reference_context_complete": true,
            "binance_reference_counts": {
                "spot_price": spot_price_count,
                "agg_trade": agg_trade_count,
                "l2": l2_count,
            },
        }))
    }
}

fn required_decimal(
    update: &Map<String, Value>,
    field: &str,
    kind: &str,
    line: usize,
) -> Result<Decimal> {
    decimal(update.get(field), field, line)?
        .ok_or_else(|| TapeValidationError(format!("line {line}: {kind} requires {field}")))
}

fn validate_spot_price(update: &Map<String, Value>, line: usize) -> Result<()> {
    text(update, "symbol", line)?;
    if required_decimal(update, "price", "spot_price", line)? < Decimal::ZERO {
        return invalid(line, "spot_price price must be non-negative");
    }
    timestamp(update.get("ts"), "ts", line)?;
    Ok(())
}

fn validate_agg_trade(update: &Map<String, Value>, line: usize) -> Result<()> {
    text(update, "symbol", line)?;
    if update.get("agg_trade_id").and_then(Value::as_u64).is_none() {
        return invalid(line, "agg_trade requires agg_trade_id");
    }
    if required_decimal(update, "price", "agg_trade", line)? <= Decimal::ZERO
        || required_decimal(update, "quantity", "agg_trade", line)? <= Decimal::ZERO
    {
        return invalid(line, "agg_trade price and quantity must be positive");
    }
    if !update.get("is_buyer_maker").is_some_and(Value::is_boolean) {
        return invalid(line, "agg_trade is_buyer_maker must be a boolean");
    }
    timestamp(update.get("ts"), "ts", line)?;
    Ok(())
}

fn validate_l2(update: &Map<String, Value>, line: usize) -> Result<()> {
    text(update, "symbol", line)?;
    let Some(obi) = update
        .get("obi")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
    else {
        return invalid(line, "l2 requires a finite obi");
    };
    if !(-1.0..=1.0).contains(&obi) {
        return invalid(line, "l2 obi must be within [-1, 1]");
    }
    if update.get("spread_bps").and_then(Value::as_u64).is_none() {
        return invalid(line, "l2 requires spread_bps");
    }
    timestamp(update.get("ts"), "ts", line)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote_json(
        sequence: u64,
        recorded_at: DateTime<Utc>,
        quote: TapeQuote<'_>,
        bids: &[(Decimal, Decimal)],
        asks: &[(Decimal, Decimal)],
    ) -> Value {
        json!({"sequence":sequence,"recorded_at":recorded_at,"update":{
            "kind":"quote","token_id":quote.token_id,"ts":quote.source_at,
            "bid":quote.bid,"ask":quote.ask,"bid_size":quote.bid_size,"ask_size":quote.ask_size,
            "bid_levels":bids.iter().map(|(price,size)|json!({"price":price,"size":size})).collect::<Vec<_>>(),
            "ask_levels":asks.iter().map(|(price,size)|json!({"price":price,"size":size})).collect::<Vec<_>>(),
            "request_status":"success","collection_result":quote.collection_result}})
    }

    fn initialized(recorded_at: DateTime<Utc>) -> MarketTapeManifestBuilder {
        let mut builder = MarketTapeManifestBuilder::new();
        builder
            .observe(
                &json!({"sequence":0,"recorded_at":recorded_at,"update":{
            "kind":"event_discovered","event_id":"event","up_token":"up","down_token":"down",
            "end_time":recorded_at+TimeDelta::minutes(5),"symbol":"BTCUSDT"}}),
                recorded_at,
            )
            .unwrap();
        builder
    }

    #[test]
    fn typed_quote_manifest_matches_json_for_full_depth_and_quality_cases() {
        let time = DateTime::parse_from_rfc3339("2026-09-30T09:00:00.123456789Z")
            .unwrap()
            .with_timezone(&Utc);
        let one = Some(Decimal::ONE);
        let cases = [
            (
                Some(Decimal::new(49, 2)),
                Some(Decimal::new(51, 2)),
                one,
                one,
                "executable",
            ),
            (Some(Decimal::new(49, 2)), None, one, None, "one_sided"),
            (None, None, None, None, "empty"),
            (None, None, None, None, "non_executable"),
            (
                Some(Decimal::new(55, 2)),
                Some(Decimal::new(51, 2)),
                one,
                one,
                "incomplete",
            ),
            (
                Some(Decimal::new(49, 2)),
                Some(Decimal::new(51, 2)),
                Some(-Decimal::ONE),
                one,
                "incomplete",
            ),
        ];
        let mut json_builder = initialized(time);
        let mut typed_builder = initialized(time);
        for (index, (bid, ask, bid_size, ask_size, result)) in cases.into_iter().enumerate() {
            let recorded_at = time + TimeDelta::milliseconds(index as i64);
            let quote = TapeQuote {
                token_id: "up",
                source_at: time,
                bid,
                ask,
                bid_size,
                ask_size,
                collection_result: result,
            };
            let bids = match result {
                "empty" => vec![],
                "non_executable" => vec![(Decimal::ZERO, Decimal::ONE)],
                _ => (0..99)
                    .map(|i| (Decimal::new(490 - i, 3), Decimal::ONE))
                    .collect(),
            };
            let asks = if result == "empty" || result == "one_sided" {
                vec![]
            } else if result == "non_executable" {
                vec![(Decimal::ONE, Decimal::ONE)]
            } else {
                (0..99)
                    .map(|i| (Decimal::new(510 + i, 3), Decimal::ONE))
                    .collect()
            };
            let sequence = index as u64 + 1;
            json_builder
                .observe(
                    &quote_json(sequence, recorded_at, quote, &bids, &asks),
                    recorded_at,
                )
                .unwrap();
            typed_builder
                .observe_typed_quote(
                    sequence,
                    recorded_at,
                    recorded_at,
                    quote,
                    bids.iter().copied(),
                    asks.iter().copied(),
                )
                .unwrap();
        }
        assert_eq!(
            json_builder
                .finish("crypto_expiry", 0, 0, "test.ndjson", 100)
                .unwrap(),
            typed_builder
                .finish("crypto_expiry", 0, 0, "test.ndjson", 100)
                .unwrap()
        );
    }

    #[test]
    fn typed_quote_rejects_the_same_clock_sequence_and_status_errors() {
        let now = Utc::now();
        for (sequence, recorded_at, source_at, token, result) in [
            (2, now, now, "up", "empty"),
            (1, now - TimeDelta::seconds(1), now, "up", "empty"),
            (1, now + TimeDelta::seconds(301), now, "up", "empty"),
            (1, now, now + TimeDelta::milliseconds(1), "up", "empty"),
            (1, now, now, "", "empty"),
            (1, now, now, "up", "executable"),
            (u64::MAX, now, now, "up", "empty"),
        ] {
            let quote = TapeQuote {
                token_id: token,
                source_at,
                bid: None,
                ask: None,
                bid_size: None,
                ask_size: None,
                collection_result: result,
            };
            let record = quote_json(sequence, recorded_at, quote, &[], &[]);
            let mut a = initialized(now);
            let mut b = initialized(now);
            assert!(a.observe(&record, now).is_err());
            assert!(b
                .observe_typed_quote(
                    sequence,
                    recorded_at,
                    now,
                    quote,
                    std::iter::empty(),
                    std::iter::empty()
                )
                .is_err());
        }
    }

    #[test]
    #[ignore = "bounded full-depth validation throughput benchmark"]
    fn full_depth_quote_validation_throughput() {
        let now = Utc::now();
        let quote = TapeQuote {
            token_id: "up",
            source_at: now,
            bid: Some(Decimal::new(49, 2)),
            ask: Some(Decimal::new(51, 2)),
            bid_size: Some(Decimal::ONE),
            ask_size: Some(Decimal::ONE),
            collection_result: "executable",
        };
        let bids = (0..99)
            .map(|i| (Decimal::new(490 - i, 3), Decimal::ONE))
            .collect::<Vec<_>>();
        let asks = (0..99)
            .map(|i| (Decimal::new(510 + i, 3), Decimal::ONE))
            .collect::<Vec<_>>();
        const RECORDS: u64 = 20_000;
        let mut old = initialized(now);
        let start = std::time::Instant::now();
        for n in 1..=RECORDS {
            let record = quote_json(n, now, quote, &bids, &asks);
            old.observe(std::hint::black_box(&record), now).unwrap();
        }
        let json_elapsed = start.elapsed();
        let mut typed = initialized(now);
        let start = std::time::Instant::now();
        for n in 1..=RECORDS {
            typed
                .observe_typed_quote(
                    n,
                    now,
                    now,
                    quote,
                    std::hint::black_box(&bids).iter().copied(),
                    std::hint::black_box(&asks).iter().copied(),
                )
                .unwrap();
        }
        let typed_elapsed = start.elapsed();
        assert_eq!(
            old.finish("crypto_expiry", 0, 0, "test.ndjson", 100)
                .unwrap(),
            typed
                .finish("crypto_expiry", 0, 0, "test.ndjson", 100)
                .unwrap()
        );
        eprintln!("full_depth_validation records={RECORDS} levels_per_side=99 json_ms={} typed_ms={} speedup={:.2}",json_elapsed.as_millis(),typed_elapsed.as_millis(),json_elapsed.as_secs_f64()/typed_elapsed.as_secs_f64());
    }

    #[test]
    fn decimal_accepts_scientific_json_numbers_without_weakening_invalid_rejection() {
        assert_eq!(
            decimal(Some(&json!(1e-9)), "price", 1).unwrap(),
            Some(Decimal::from_scientific("1e-9").unwrap())
        );
        assert!(decimal(Some(&json!("not-a-number")), "price", 1).is_err());
    }

    #[test]
    fn canonical_stream_failures_use_the_existing_tape_error_contract() {
        let now = Utc::now();
        let mut builder = initialized(now);
        for (index, kind) in [
            "websocket_subscribe",
            "websocket_payload",
            "websocket_receive",
            "websocket_eof",
        ]
        .into_iter()
        .enumerate()
        {
            let record = json!({"sequence":index+1,"recorded_at":now,"update":{
                "kind":"quote_collection_failure","token_id":"up","request_started_at":now-TimeDelta::milliseconds(1),
                "http_status":null,"error_kind":kind,"ts":now,"request_status":"failure","collection_result":"api_failure"}});
            builder.observe(&record, now).unwrap();
        }
        let manifest = builder
            .finish("crypto_expiry", 0, 0, "test.ndjson", 100)
            .unwrap();
        assert_eq!(manifest["quality"]["request_attempts"], 4);
        assert_eq!(manifest["quality"]["request_failures"], 4);
        assert_eq!(manifest["quality"]["transport_reconnects"], 1);
    }
}
