//! Fail-closed Polymarket reference collection and raw tape archival.

use crate::polymarket_upload::{
    ensure_canonical_directory, trade_record_ids_sha256, validate_reference_tape_for_recovery,
    TRADE_COMPLETION_BASIS, TRADE_COMPLETION_KIND,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use futures::{stream, StreamExt};
use rand::random;
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const ACTIVE_TAPE: &str = "market-updates.ndjson";
const REFERENCE_SPOOL_LOCK: &str = ".polymarket-reference.lock";
const GAMMA_MARKETS_URL: &str = "https://gamma-api.polymarket.com/markets/keyset";
const GAMMA_MARKET_URL: &str = "https://gamma-api.polymarket.com/markets";
const DATA_TRADES_URL: &str = "https://data-api.polymarket.com/trades";
const USER_AGENT: &str = "monday-polymarket-reference-collector/2.0";
const TRADE_ID_VERSION: &str = "v2";
const TRADE_COMPLETION_VERSION: &str = "v1";
const CRYPTO_TAG_ID: u64 = 21;
const MIN_SETTLEMENT_LOOKBACK_SECS: i64 = 86_400;
const MAX_CYCLE_DURATION: Duration = Duration::from_secs(180);
const HARD_CYCLE_WATCHDOG_EXIT_CODE: i32 = 124;
const HTTP_GET_ATTEMPTS: usize = 3;
const HTTP_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const HTTP_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
// Default cap on retained trade IDs for dedupe state. Raised from 1M to 4M
// after the 2026-08-01 shadow gate failure: at the current catalog (2765+
// markets) a full backfill at poll budget 200 accumulates ~42k IDs/min, so 1M
// is reached in ~15 minutes and the collector bails. At ~150-200B/ID in
// memory, 4M IDs cost well under the units' 1536M/2048M watermarks.
pub const DEFAULT_MAX_RETAINED_TRADE_IDS: usize = 4_000_000;
pub const DEFAULT_MAX_MARKETS_PER_LANE: usize = 10_000;
pub const DEFAULT_MAX_TRADE_POLLS_PER_CYCLE: usize = 112;
pub const DEFAULT_MAX_CONCURRENT_TRADE_POLLS: usize = 4;
const MIN_TRADE_REQUEST_SPACING: Duration = Duration::from_millis(100);
const TARGET_MARKET_WINDOWS_SECS: [usize; 2] = [300, 900];
const SETTLEMENT_PRICE: Decimal = Decimal::from_parts(999, 0, 0, false, 3);
const SETTLEMENT_LOSER_PRICE: Decimal = Decimal::from_parts(1, 0, 0, false, 3);
const SETTLEMENT_SUM_TOLERANCE: Decimal = Decimal::from_parts(1, 0, 0, false, 6);

struct ReferenceEndpoints {
    gamma_markets: String,
    gamma_market: String,
    data_trades: String,
}

impl Default for ReferenceEndpoints {
    fn default() -> Self {
        Self {
            gamma_markets: GAMMA_MARKETS_URL.to_owned(),
            gamma_market: GAMMA_MARKET_URL.to_owned(),
            data_trades: DATA_TRADES_URL.to_owned(),
        }
    }
}

const SYMBOL_ALIASES: [(&str, &[&str]); 7] = [
    ("BTCUSDT", &["BITCOIN", "BTC"]),
    ("ETHUSDT", &["ETHEREUM", "ETH"]),
    ("SOLUSDT", &["SOLANA", "SOL "]),
    ("XRPUSDT", &["XRP"]),
    ("DOGEUSDT", &["DOGECOIN", "DOGE"]),
    ("HYPEUSDT", &["HYPERLIQUID", "HYPE"]),
    ("BNBUSDT", &["BINANCE COIN", "BNB"]),
];

struct CycleWatchdog {
    completed: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl CycleWatchdog {
    fn arm(timeout: Duration) -> std::io::Result<Self> {
        let completed = Arc::new((Mutex::new(false), Condvar::new()));
        let watcher = Arc::clone(&completed);
        let handle = std::thread::Builder::new()
            .name("polymarket-cycle-watchdog".to_owned())
            .spawn(move || {
                let (lock, condition) = &*watcher;
                let completed = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let (completed, wait) = condition
                    .wait_timeout_while(completed, timeout, |completed| !*completed)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if wait.timed_out() && !*completed {
                    // A hard watchdog must not wait on stderr's global lock or
                    // a backpressured log sink. systemd records exit status 124.
                    std::process::exit(HARD_CYCLE_WATCHDOG_EXIT_CODE);
                }
            })?;
        Ok(Self {
            completed,
            handle: Some(handle),
        })
    }
}

impl Drop for CycleWatchdog {
    fn drop(&mut self) {
        let (lock, condition) = &*self.completed;
        let mut completed = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *completed = true;
        condition.notify_one();
        drop(completed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug)]
struct DataCompletenessError(String);

impl std::fmt::Display for DataCompletenessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DataCompletenessError {}

fn completeness_error(message: impl Into<String>) -> anyhow::Error {
    DataCompletenessError(message.into()).into()
}

fn retryable_http_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

fn retryable_http_error(error: &reqwest::Error) -> bool {
    error.is_timeout()
        || error.is_connect()
        || error.is_request()
        || error.is_body()
        || error.is_decode()
}

fn http_retry_delay(
    retry_after: Option<&str>,
    attempt: usize,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let Some(retry_after) = retry_after else {
        return Some(
            HTTP_RETRY_BASE_DELAY
                .saturating_mul(1_u32 << attempt.min(3))
                .min(HTTP_RETRY_MAX_DELAY),
        );
    };
    let retry_after = retry_after.trim();
    let delay = if let Ok(seconds) = retry_after.parse::<u64>() {
        Duration::from_secs(seconds)
    } else {
        let retry_at = DateTime::parse_from_rfc2822(retry_after)
            .ok()?
            .with_timezone(&Utc);
        retry_at
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO)
    };
    (delay <= HTTP_RETRY_MAX_DELAY).then_some(delay)
}

#[derive(Debug, Clone)]
pub struct ReferenceConfig {
    pub spool_dir: PathBuf,
    pub symbols: Vec<String>,
    pub market_ids: BTreeSet<String>,
    pub poll_interval: Duration,
    pub market_lookback_secs: i64,
    pub settlement_lookback_secs: i64,
    pub max_markets: usize,
    pub max_retained_trade_ids: usize,
    pub max_trade_polls_per_cycle: usize,
    pub max_concurrent_trade_polls: usize,
    pub http_timeout: Duration,
    pub stale_after: Duration,
    pub trade_finalization_lag_secs: i64,
    pub trade_finalization_stable_polls: u64,
    pub per_market_delay: Duration,
}

impl Default for ReferenceConfig {
    fn default() -> Self {
        Self {
            spool_dir: PathBuf::from("/data/monday/spool/polymarket-reference"),
            symbols: SYMBOL_ALIASES
                .iter()
                .map(|(symbol, _)| (*symbol).to_owned())
                .collect(),
            market_ids: BTreeSet::new(),
            poll_interval: Duration::from_secs(30),
            market_lookback_secs: 7_200,
            settlement_lookback_secs: 86_400,
            max_markets: DEFAULT_MAX_MARKETS_PER_LANE,
            max_retained_trade_ids: DEFAULT_MAX_RETAINED_TRADE_IDS,
            max_trade_polls_per_cycle: DEFAULT_MAX_TRADE_POLLS_PER_CYCLE,
            max_concurrent_trade_polls: DEFAULT_MAX_CONCURRENT_TRADE_POLLS,
            http_timeout: Duration::from_secs(20),
            stale_after: Duration::from_secs(180),
            trade_finalization_lag_secs: 1_800,
            trade_finalization_stable_polls: 3,
            per_market_delay: MIN_TRADE_REQUEST_SPACING,
        }
    }
}

impl ReferenceConfig {
    pub fn validate(&self) -> Result<()> {
        if self.symbols.is_empty()
            || self.poll_interval.is_zero()
            || self.market_lookback_secs <= 0
            || self.settlement_lookback_secs <= 0
            || self.max_markets == 0
            || self.max_retained_trade_ids == 0
            || self.max_trade_polls_per_cycle == 0
            || self.max_concurrent_trade_polls == 0
            || self.http_timeout.is_zero()
            || self.stale_after.is_zero()
            || self.trade_finalization_lag_secs <= 0
            || self.trade_finalization_stable_polls == 0
        {
            bail!("reference collector limits must be positive");
        }
        if self.market_ids.iter().any(|market_id| {
            market_id.is_empty() || market_id.trim() != market_id
        }) {
            bail!("market IDs must be non-empty, whitespace-free identifiers");
        }
        if self.market_ids.len() > self.max_markets {
            bail!(
                "requested market IDs={} exceed max_markets={}",
                self.market_ids.len(),
                self.max_markets
            );
        }
        if self.max_concurrent_trade_polls > DEFAULT_MAX_CONCURRENT_TRADE_POLLS {
            bail!(
                "concurrent trade polls cannot exceed the hard limit of {DEFAULT_MAX_CONCURRENT_TRADE_POLLS}"
            );
        }
        if self.max_concurrent_trade_polls > self.max_trade_polls_per_cycle {
            bail!("concurrent trade polls cannot exceed the per-cycle trade poll budget");
        }
        if self.per_market_delay < MIN_TRADE_REQUEST_SPACING {
            bail!(
                "trade request spacing must be at least {}ms",
                MIN_TRADE_REQUEST_SPACING.as_millis()
            );
        }
        if self.settlement_lookback_secs < MIN_SETTLEMENT_LOOKBACK_SECS {
            bail!("settlement lookback must cover at least {MIN_SETTLEMENT_LOOKBACK_SECS} seconds");
        }
        let symbol_count = self
            .symbols
            .iter()
            .map(|symbol| symbol.trim().to_ascii_uppercase())
            .filter(|symbol| !symbol.is_empty())
            .collect::<BTreeSet<_>>()
            .len();
        let settlement_lookback = usize::try_from(self.settlement_lookback_secs)
            .context("settlement lookback exceeds this platform's capacity")?;
        let markets_per_symbol = TARGET_MARKET_WINDOWS_SECS
            .iter()
            .try_fold(0_usize, |total, window| {
                total.checked_add(settlement_lookback.div_ceil(*window))
            })
            .context("settlement discovery capacity overflow")?;
        let required_markets = symbol_count
            .checked_mul(markets_per_symbol)
            .context("settlement discovery capacity overflow")?;
        if self.max_markets < required_markets {
            bail!(
                "max_markets={} cannot cover the settlement lane minimum of {required_markets} markets for {symbol_count} symbols",
                self.max_markets
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CollectorState {
    #[serde(default)]
    trade_id_version: Option<String>,
    #[serde(default)]
    trade_completion_version: Option<String>,
    #[serde(default)]
    context_seed_hour: Option<String>,
    #[serde(default)]
    markets: BTreeMap<String, TrackedMarket>,
    #[serde(default)]
    trade_seen: BTreeMap<String, BTreeMap<String, i64>>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TrackedMarket {
    #[serde(default)]
    condition_id: Option<String>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    market_window_secs: u64,
    #[serde(default)]
    end_time: Option<String>,
    #[serde(default)]
    settled: bool,
    #[serde(default)]
    trade_complete: bool,
    #[serde(default)]
    last_metadata_hash: Option<String>,
    #[serde(default)]
    settlement_failure_since: Option<String>,
    #[serde(default)]
    settlement_last_error: Option<String>,
    #[serde(default)]
    trade_failure_since: Option<String>,
    #[serde(default)]
    trade_last_error: Option<String>,
    #[serde(default)]
    last_trade_success_at: Option<String>,
    #[serde(default)]
    last_market_detail_attempt_at: Option<String>,
    #[serde(default)]
    settlement_seen_at: Option<String>,
    #[serde(default)]
    last_trade_change_at: Option<String>,
    #[serde(default)]
    trade_finalization_stable_polls: u64,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
struct TargetMarket {
    symbol: String,
    window_secs: u64,
}

#[derive(Debug, Clone)]
struct TradePollCandidate {
    market_id: String,
    priority: bool,
    last_success_at: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TradePollPlan {
    selected: BTreeSet<String>,
    eligible: usize,
    priority: usize,
    deferred: usize,
    priority_deferred: usize,
}

fn compare_trade_poll_candidates(
    left: &TradePollCandidate,
    right: &TradePollCandidate,
) -> Ordering {
    match (left.last_success_at, right.last_success_at) {
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(left), Some(right)) => left.cmp(&right),
        (None, None) => Ordering::Equal,
    }
    .then_with(|| right.end_time.cmp(&left.end_time))
    .then_with(|| left.market_id.cmp(&right.market_id))
}

fn plan_trade_polls(candidates: Vec<TradePollCandidate>, max_trade_polls: usize) -> TradePollPlan {
    let eligible = candidates.len();
    let mut priority = candidates
        .iter()
        .filter(|candidate| candidate.priority)
        .cloned()
        .collect::<Vec<_>>();
    let mut backfill = candidates
        .into_iter()
        .filter(|candidate| !candidate.priority)
        .collect::<Vec<_>>();
    priority.sort_by(compare_trade_poll_candidates);
    backfill.sort_by(compare_trade_poll_candidates);
    let priority_count = priority.len();
    let selected = priority
        .into_iter()
        .chain(backfill)
        .take(max_trade_polls)
        .map(|candidate| candidate.market_id)
        .collect::<BTreeSet<_>>();
    let selected_count = selected.len();
    TradePollPlan {
        selected,
        eligible,
        priority: priority_count,
        deferred: eligible.saturating_sub(selected_count),
        priority_deferred: priority_count.saturating_sub(max_trade_polls),
    }
}

fn plan_market_detail_fetches(
    markets: &BTreeMap<String, TrackedMarket>,
    target_ids: &BTreeSet<String>,
    now: DateTime<Utc>,
    max_fetches: usize,
) -> TradePollPlan {
    plan_trade_polls(
        markets
            .iter()
            .filter_map(|(market_id, tracked)| {
                let end_time = parse_optional_datetime(tracked.end_time.as_deref());
                (!target_ids.contains(market_id)
                    && end_time.is_some_and(|end| end <= now)
                    && !(tracked.settled && tracked.trade_complete))
                    .then(|| TradePollCandidate {
                        market_id: market_id.clone(),
                        priority: !tracked.settled || tracked.settlement_failure_since.is_some(),
                        last_success_at: parse_optional_datetime(
                            tracked.last_market_detail_attempt_at.as_deref(),
                        ),
                        end_time,
                    })
            })
            .collect(),
        max_fetches,
    )
}

fn remaining_trade_poll_budget(
    max_requests_per_cycle: usize,
    selected_market_details: usize,
) -> usize {
    max_requests_per_cycle.saturating_sub(selected_market_details)
}

fn shared_market_detail_budget(
    max_requests_per_cycle: usize,
    max_concurrent_requests: usize,
    priority_trades: usize,
) -> usize {
    max_concurrent_requests.min(
        max_requests_per_cycle
            .saturating_sub(priority_trades)
            / 2,
    )
}

fn trade_poll_candidates(
    targets: &BTreeMap<String, (Value, TargetMarket)>,
    state: &CollectorState,
    priority_cutoff: DateTime<Utc>,
) -> Vec<TradePollCandidate> {
    targets
        .iter()
        .filter_map(|(market_id, (market, _))| {
            let tracked = state.markets.get(market_id)?;
            let condition_id = market
                .get("conditionId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if condition_id.is_empty() || tracked.trade_complete {
                return None;
            }
            let end_time = market
                .get("endDate")
                .and_then(parse_datetime)
                .or_else(|| parse_optional_datetime(tracked.end_time.as_deref()));
            Some(TradePollCandidate {
                market_id: market_id.clone(),
                priority: tracked.trade_failure_since.is_some()
                    || end_time.is_none_or(|end| end >= priority_cutoff),
                last_success_at: parse_optional_datetime(
                    tracked.last_trade_success_at.as_deref(),
                ),
                end_time,
            })
        })
        .collect()
}

fn target_processing_chunks<'a>(
    target_ids: impl IntoIterator<Item = &'a String>,
    selected: &BTreeSet<String>,
    max_selected_per_chunk: usize,
) -> Vec<Vec<&'a String>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut selected_in_current = 0_usize;
    for market_id in target_ids {
        current.push(market_id);
        if selected.contains(market_id) {
            selected_in_current += 1;
        }
        if selected_in_current == max_selected_per_chunk {
            chunks.push(std::mem::take(&mut current));
            selected_in_current = 0;
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

fn iso_z(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
}

fn hour_key(value: DateTime<Utc>) -> String {
    value.format("%Y%m%dT%H").to_string()
}

fn parse_datetime(value: &Value) -> Option<DateTime<Utc>> {
    let value = value.as_str()?;
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|parsed| parsed.and_utc())
        })
}

fn parse_optional_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    value.and_then(|value| parse_datetime(&Value::String(value.to_owned())))
}

fn parse_json_array(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(values)) => values.clone(),
        Some(Value::String(value)) => serde_json::from_str::<Vec<Value>>(value).unwrap_or_default(),
        _ => Vec::new(),
    }
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

fn stable_trade_id(trade: &Value) -> String {
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

fn stable_payload_hash(payload: &Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(payload)?)))
}

fn infer_symbol(question: Option<&Value>) -> Option<&'static str> {
    let upper = question?.as_str()?.to_ascii_uppercase();
    SYMBOL_ALIASES.iter().find_map(|(symbol, aliases)| {
        aliases
            .iter()
            .any(|alias| upper.contains(alias))
            .then_some(*symbol)
    })
}

fn market_start_time(market: &Value) -> Option<DateTime<Utc>> {
    let event = market
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| events.first());
    [
        market.get("eventStartTime"),
        market.get("startDate"),
        event.and_then(|event| event.get("startTime")),
        event.and_then(|event| event.get("startDate")),
    ]
    .into_iter()
    .flatten()
    .find_map(parse_datetime)
}

fn infer_window_seconds(market: &Value) -> Option<u64> {
    if let (Some(start), Some(end)) = (
        market_start_time(market),
        market.get("endDate").and_then(parse_datetime),
    ) {
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

fn contains_window_token(text: &str, token: &str) -> bool {
    text.match_indices(token).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + token.len()..].chars().next();
        before.is_none_or(|value| matches!(value, '-' | '_' | ' '))
            && after.is_none_or(|value| matches!(value, '-' | '_' | ' '))
    })
}

fn target_market(market: &Value, symbols: &BTreeSet<String>) -> Option<TargetMarket> {
    let symbol = infer_symbol(market.get("question"))?;
    let window_secs = infer_window_seconds(market)?;
    let condition = market.get("conditionId")?.as_str()?;
    let token_ids = parse_json_array(market.get("clobTokenIds"));
    if !symbols.contains(symbol)
        || !matches!(window_secs, 300 | 900)
        || condition.is_empty()
        || token_ids.len() != 2
    {
        return None;
    }
    Some(TargetMarket {
        symbol: symbol.to_owned(),
        window_secs,
    })
}

fn recovered_target(
    market_id: &str,
    market: &Value,
    symbols: &BTreeSet<String>,
) -> Result<TargetMarket> {
    if market.get("id").and_then(Value::as_str) != Some(market_id) {
        bail!("Gamma market detail ID does not match requested market");
    }
    target_market(market, symbols)
        .ok_or_else(|| completeness_error("Gamma market detail is not a supported target market"))
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("atomic target has no parent")?;
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let (temporary, mut output) = (0..32)
        .find_map(|_| {
            let temporary = parent.join(format!(".{name}.{:016x}.tmp", random::<u64>()));
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
        .context("could not allocate exclusive atomic-write temporary")?;
    let result = (|| -> Result<()> {
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_optional_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match fs::symlink_metadata(path) {
        Ok(_) => deserialize_json(open_read_regular(path)?)
            .with_context(|| format!("invalid JSON in {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error.into()),
    }
}

fn deserialize_json<T: DeserializeOwned>(reader: impl Read) -> serde_json::Result<T> {
    serde_json::from_reader(BufReader::new(reader))
}

fn open_read_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open regular file {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("refusing non-regular file {}", path.display());
    }
    Ok(file)
}

fn strict_existing_regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("refusing non-regular file {}", path.display());
    }
    Ok(())
}

fn open_append(path: &Path) -> Result<File> {
    open_append_with_create(path, true)
}

fn open_append_with_create(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .create(create)
        .read(true)
        .append(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("failed to open append-only tape {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("refusing non-regular tape {}", path.display());
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn release_clean_file_cache(file: &File) -> Result<()> {
    use std::os::fd::AsRawFd;

    // cgroup v2 charges clean tape pages to the service's memory.current.
    // SAFETY: `file` owns a valid descriptor for the duration of this call.
    let result = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
    if result != 0 {
        return Err(std::io::Error::from_raw_os_error(result))
            .context("could not release clean tape pages");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn release_clean_file_cache(_file: &File) -> Result<()> {
    Ok(())
}

fn path_c_string(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow!("path contains a NUL byte: {}", path.display()))
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = path_c_string(source)?;
    let target = path_c_string(target)?;
    // SAFETY: both C strings are NUL-terminated and live for the call.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("no-clobber rename failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = path_c_string(source)?;
    let target = path_c_string(target)?;
    // SAFETY: both C strings are NUL-terminated and live for the call.
    if unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("no-clobber rename failed");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace(_source: &Path, _target: &Path) -> Result<()> {
    bail!("atomic no-clobber rename is unsupported on this platform")
}

struct TapeWriter {
    spool_dir: PathBuf,
    active: PathBuf,
    hour: Option<String>,
    sequence: u64,
    file: Option<File>,
}

struct ReferenceSpoolLock {
    _file: File,
}

impl ReferenceSpoolLock {
    fn acquire(spool_dir: &Path) -> Result<Self> {
        ensure_canonical_directory(spool_dir)?;
        let path = spool_dir.join(REFERENCE_SPOOL_LOCK);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| format!("open Polymarket reference spool lock {}", path.display()))?;
        if !file.metadata()?.file_type().is_file() {
            bail!("Polymarket reference spool lock is not a regular file");
        }
        fs4::FileExt::try_lock(&file)
            .map_err(|error| anyhow!("Polymarket reference spool is already locked: {error}"))?;
        Ok(Self { _file: file })
    }
}

struct PendingUpdates {
    file: File,
    len: usize,
    record_types: BTreeMap<&'static str, usize>,
}

impl PendingUpdates {
    fn new(spool_dir: &Path) -> Result<Self> {
        let (path, file) = (0..32)
            .find_map(|_| {
                let path =
                    spool_dir.join(format!(".pending-updates.{:016x}.ndjson", random::<u64>()));
                match OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .mode(0o600)
                    .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                    .open(&path)
                {
                    Ok(file) => Some(Ok((path, file))),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()?
            .context("could not allocate exclusive pending-update spill")?;
        fs::remove_file(&path).context("could not unlink private pending-update spill")?;
        Ok(Self {
            file,
            len: 0,
            record_types: BTreeMap::new(),
        })
    }

    fn len(&self) -> usize {
        self.len
    }

    fn record_types(&self) -> BTreeMap<&'static str, usize> {
        [
            "market_metadata",
            "polymarket_trade",
            "market_settlement",
            TRADE_COMPLETION_KIND,
        ]
        .into_iter()
        .map(|kind| {
            (
                kind,
                self.record_types.get(kind).copied().unwrap_or_default(),
            )
        })
        .collect()
    }

    fn push(&mut self, update: Value) -> Result<()> {
        let kind = match update.get("kind").and_then(Value::as_str) {
            Some("market_metadata") => Some("market_metadata"),
            Some("polymarket_trade") => Some("polymarket_trade"),
            Some("market_settlement") => Some("market_settlement"),
            Some(TRADE_COMPLETION_KIND) => Some(TRADE_COMPLETION_KIND),
            _ => None,
        };
        let mut encoded = serde_json::to_vec(&update)?;
        encoded.push(b'\n');
        self.file.write_all(&encoded)?;
        self.len += 1;
        if let Some(kind) = kind {
            *self.record_types.entry(kind).or_default() += 1;
        }
        Ok(())
    }

    fn replay(self, writer: &mut TapeWriter, now: DateTime<Utc>) -> Result<()> {
        self.replay_with_sync(writer, now, |file| {
            file.sync_all()?;
            Ok(())
        })
    }

    fn replay_with_sync<F>(
        mut self,
        writer: &mut TapeWriter,
        now: DateTime<Utc>,
        sync: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut File) -> Result<()>,
    {
        self.file.flush()?;
        self.file.sync_all()?;
        release_clean_file_cache(&self.file)?;
        self.file.seek(SeekFrom::Start(0))?;
        writer.write_update_results_with_sync(
            PendingUpdateReader::new(self.file, self.len),
            now,
            sync,
        )
    }
}

struct PendingUpdateReader {
    reader: BufReader<File>,
    expected: usize,
    read: usize,
    done: bool,
}

impl PendingUpdateReader {
    fn new(file: File, expected: usize) -> Self {
        Self {
            reader: BufReader::new(file),
            expected,
            read: 0,
            done: false,
        }
    }
}

impl Iterator for PendingUpdateReader {
    type Item = Result<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let mut line = Vec::new();
        let bytes = match self.reader.read_until(b'\n', &mut line) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.done = true;
                return Some(Err(error.into()));
            }
        };
        if bytes == 0 {
            self.done = true;
            return (self.read != self.expected).then(|| {
                Err(anyhow!(
                    "pending-update spill ended after {} of {} records",
                    self.read,
                    self.expected
                ))
            });
        }
        if !line.ends_with(b"\n") {
            self.done = true;
            return Some(Err(anyhow!(
                "pending-update spill has an incomplete final record"
            )));
        }
        if self.read == self.expected {
            self.done = true;
            return Some(Err(anyhow!(
                "pending-update spill contains more than {} records",
                self.expected
            )));
        }
        line.pop();
        self.read += 1;
        Some(serde_json::from_slice(&line).context("pending-update spill contains invalid JSON"))
    }
}

impl TapeWriter {
    #[cfg(test)]
    fn new(spool_dir: &Path) -> Result<Self> {
        Self::new_with_recovery(spool_dir, |_| Ok(()))
    }

    fn new_with_recovery<F>(spool_dir: &Path, recover: F) -> Result<Self>
    where
        F: FnMut(&Value) -> Result<()>,
    {
        Self::new_with_recovery_expected(spool_dir, recover, None)
    }

    fn new_with_recovery_expected<F>(
        spool_dir: &Path,
        recover: F,
        expected_active: Option<&fs::Metadata>,
    ) -> Result<Self>
    where
        F: FnMut(&Value) -> Result<()>,
    {
        fs::create_dir_all(spool_dir)?;
        let active = spool_dir.join(ACTIVE_TAPE);
        let mut writer = Self {
            spool_dir: spool_dir.to_path_buf(),
            active,
            hour: None,
            sequence: 0,
            file: None,
        };
        writer.recover_active(recover, expected_active)?;
        Ok(writer)
    }

    fn recover_active<F>(
        &mut self,
        mut recover: F,
        expected_active: Option<&fs::Metadata>,
    ) -> Result<()>
    where
        F: FnMut(&Value) -> Result<()>,
    {
        if !self.active.exists() {
            if expected_active.is_some() {
                bail!("Polymarket reference active tape changed after validation");
            }
            self.file = Some(open_append(&self.active)?);
            return Ok(());
        }
        let mut file = open_append_with_create(&self.active, false)?;
        if let Some(expected) = expected_active {
            let opened = file.metadata()?;
            if opened.dev() != expected.dev() || opened.ino() != expected.ino() {
                bail!("Polymarket reference active tape changed after validation");
            }
        }
        let mut reader = BufReader::new(file.try_clone()?);
        let mut expected = 0_u64;
        let mut valid_bytes = 0_u64;
        let mut first_recorded = None;
        loop {
            let mut line = Vec::new();
            let bytes = reader.read_until(b'\n', &mut line)?;
            if bytes == 0 {
                break;
            }
            if !line.ends_with(b"\n") {
                break;
            }
            let row: Value = serde_json::from_slice(&line)?;
            if row.get("sequence").and_then(Value::as_u64) != Some(expected) {
                bail!("active tape sequence gap expected={expected}");
            }
            if first_recorded.is_none() {
                first_recorded = row.get("recorded_at").and_then(parse_datetime);
            }
            recover(&row)?;
            expected += 1;
            valid_bytes += u64::try_from(bytes)?;
        }
        if file.metadata()?.len() != valid_bytes {
            file.set_len(valid_bytes)?;
            file.sync_all()?;
        }
        release_clean_file_cache(reader.get_ref())?;
        file.seek(SeekFrom::End(0))?;
        self.sequence = expected;
        self.hour = first_recorded.map(|value| value.format("%Y%m%dT%H").to_string());
        self.file = Some(file);
        Ok(())
    }

    fn rotate(&mut self, now: DateTime<Utc>) -> Result<PathBuf> {
        self.rotate_with_rename(now, rename_noreplace)
    }

    fn rotate_with_rename<F>(&mut self, now: DateTime<Utc>, mut rename: F) -> Result<PathBuf>
    where
        F: FnMut(&Path, &Path) -> Result<()>,
    {
        if self.file.is_none() {
            bail!("active tape is closed");
        }
        let staged = self
            .spool_dir
            .join(format!(".{ACTIVE_TAPE}.{}.rotate", random::<u64>()));
        let staged_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        if let Err(error) = staged_file.sync_all() {
            drop(staged_file);
            let cleanup = fs::remove_file(&staged);
            return Err(anyhow!(
                "failed to sync staged next tape: {error}; cleanup={cleanup:?}"
            ));
        }
        drop(staged_file);
        let active_file = self.file.as_ref().expect("active tape was checked above");
        if let Err(error) = active_file.sync_all() {
            let cleanup = fs::remove_file(&staged);
            return Err(anyhow!(
                "failed to sync active tape before rotation: {error}; cleanup={cleanup:?}"
            ));
        }
        let active_metadata = active_file.metadata()?;
        drop(self.file.take().expect("active tape was checked above"));
        let rotated = self.spool_dir.join(format!(
            "market-updates.{}.ndjson",
            now.format("%Y%m%dT%H%M%S%6f")
        ));
        if let Err(error) = rename(&self.active, &rotated) {
            let cleanup = fs::remove_file(&staged);
            let directory_sync = File::open(&self.spool_dir).and_then(|file| file.sync_all());
            let reopen = directory_sync
                .is_ok()
                .then(|| open_append(&self.active).map(|file| self.file = Some(file)));
            return Err(anyhow!(
                "refusing to replace closed tape {}: {error:#}; cleanup={cleanup:?}; directory_sync={directory_sync:?}; reopen={reopen:?}",
                rotated.display()
            ));
        }
        let rotated_metadata = fs::symlink_metadata(&rotated)?;
        if rotated_metadata.dev() != active_metadata.dev()
            || rotated_metadata.ino() != active_metadata.ino()
        {
            let rollback = rename(&rotated, &self.active);
            let cleanup = fs::remove_file(&staged);
            let directory_sync = File::open(&self.spool_dir).and_then(|file| file.sync_all());
            let reopen = (rollback.is_ok() && directory_sync.is_ok())
                .then(|| open_append(&self.active).map(|file| self.file = Some(file)));
            bail!(
                "rotated tape identity did not match open active tape; rollback={rollback:?}; cleanup={cleanup:?}; directory_sync={directory_sync:?}; reopen={reopen:?}"
            );
        }
        if let Err(error) = rename(&staged, &self.active) {
            let rollback = rename(&rotated, &self.active);
            let cleanup = fs::remove_file(&staged);
            let directory_sync = File::open(&self.spool_dir).and_then(|file| file.sync_all());
            let reopen = (rollback.is_ok() && directory_sync.is_ok())
                .then(|| open_append(&self.active).map(|file| self.file = Some(file)));
            return Err(anyhow!(
                "failed to publish next tape: {error}; rollback={rollback:?}; cleanup={cleanup:?}; directory_sync={directory_sync:?}; reopen={reopen:?}"
            ));
        }
        File::open(&self.spool_dir)?.sync_all()?;
        self.sequence = 0;
        self.hour = None;
        self.file = Some(open_append(&self.active)?);
        Ok(rotated)
    }

    #[cfg(test)]
    fn write_updates(&mut self, updates: &[Value], now: DateTime<Utc>) -> Result<()> {
        self.write_updates_with_sync(updates, now, |file| {
            file.sync_all()?;
            Ok(())
        })
    }

    #[cfg(test)]
    fn write_updates_with_sync<F>(
        &mut self,
        updates: &[Value],
        now: DateTime<Utc>,
        sync: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut File) -> Result<()>,
    {
        self.write_update_results_with_sync(updates.iter().cloned().map(Ok), now, sync)
    }

    fn write_update_results_with_sync<I, F>(
        &mut self,
        updates: I,
        now: DateTime<Utc>,
        sync: F,
    ) -> Result<()>
    where
        I: IntoIterator<Item = Result<Value>>,
        F: FnOnce(&mut File) -> Result<()>,
    {
        let mut updates = updates.into_iter().peekable();
        if updates.peek().is_none() {
            return Ok(());
        }
        let target_hour = hour_key(now);
        if self.hour.as_ref().is_some_and(|hour| hour != &target_hour) {
            self.rotate(now)?;
        }
        let start_hour = self.hour.clone();
        let start_sequence = self.sequence;
        let start_offset = self
            .file
            .as_ref()
            .context("active tape is closed")?
            .metadata()?
            .len();
        self.hour = Some(target_hour);
        let recorded_at = iso_z(now);
        let result = (|| -> Result<()> {
            let file = self.file.as_mut().context("active tape is closed")?;
            for update in updates {
                let update = update?;
                let mut encoded = serde_json::to_vec(&json!({
                    "sequence": self.sequence,
                    "recorded_at": recorded_at,
                    "update": update,
                }))?;
                encoded.push(b'\n');
                file.write_all(&encoded)?;
                self.sequence += 1;
            }
            sync(file)?;
            release_clean_file_cache(file)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.hour = start_hour;
            self.sequence = start_sequence;
            let file = self.file.as_mut().context("active tape is closed")?;
            file.set_len(start_offset)?;
            file.seek(SeekFrom::End(0))?;
            file.sync_all()?;
            return Err(error);
        }
        Ok(())
    }

    fn needs_hour_context(&self, now: DateTime<Utc>) -> bool {
        self.hour.as_deref() != Some(hour_key(now).as_str())
    }

    fn close(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        Ok(())
    }
}

fn active_reference_tape_metadata(active: &Path, effective_uid: u32) -> Result<fs::Metadata> {
    let active_metadata = fs::symlink_metadata(active)?;
    if !active_metadata.file_type().is_file() {
        bail!("Polymarket reference active tape is not a regular file");
    }
    if active_metadata.uid() != effective_uid {
        bail!(
            "Polymarket reference finalizer must run as active tape owner uid={} effective_uid={effective_uid}",
            active_metadata.uid()
        );
    }
    Ok(active_metadata)
}

fn finalize_reference_tape_at_as(
    spool_dir: &Path,
    now: DateTime<Utc>,
    effective_uid: u32,
) -> Result<PathBuf> {
    finalize_reference_tape_at_as_with(spool_dir, now, effective_uid, || Ok(()))
}

fn finalize_reference_tape_at_as_with<F>(
    spool_dir: &Path,
    now: DateTime<Utc>,
    effective_uid: u32,
    after_validation: F,
) -> Result<PathBuf>
where
    F: FnOnce() -> Result<()>,
{
    let active = spool_dir.join(ACTIVE_TAPE);
    let active_before_lock = active_reference_tape_metadata(&active, effective_uid)?;
    let _lock = ReferenceSpoolLock::acquire(spool_dir)?;
    let active_metadata = active_reference_tape_metadata(&active, effective_uid)?;
    if active_metadata.dev() != active_before_lock.dev()
        || active_metadata.ino() != active_before_lock.ino()
    {
        bail!("Polymarket reference active tape changed while acquiring spool lock");
    }
    if active_metadata.len() == 0 {
        bail!("Polymarket reference active tape is empty");
    }
    validate_reference_tape_for_recovery(&active, now)
        .context("active reference tape failed uploader validation")?;
    after_validation()?;
    let mut writer =
        TapeWriter::new_with_recovery_expected(spool_dir, |_| Ok(()), Some(&active_metadata))?;
    writer.rotate(now)
}

fn finalize_reference_tape_at(spool_dir: &Path, now: DateTime<Utc>) -> Result<PathBuf> {
    finalize_reference_tape_at_as(spool_dir, now, rustix::process::geteuid().as_raw())
}

pub fn finalize_reference_tape(spool_dir: &Path) -> Result<PathBuf> {
    finalize_reference_tape_at(spool_dir, utc_now())
}

impl Drop for TapeWriter {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn context_seed_required(state: &CollectorState, writer: &TapeWriter, now: DateTime<Utc>) -> bool {
    state.context_seed_hour.as_deref() != Some(hour_key(now).as_str())
        || writer.needs_hour_context(now)
}

fn missing_symbols(configured: &BTreeSet<String>, discovered: &BTreeSet<String>) -> Vec<String> {
    configured.difference(discovered).cloned().collect()
}

fn validate_requested_market_ids(
    requested: &BTreeSet<String>,
    discovered: &BTreeSet<String>,
) -> Result<()> {
    let missing = requested
        .difference(discovered)
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!("requested Polymarket market IDs were absent from Gamma discovery: {missing:?}");
    }
    Ok(())
}

fn retain_requested_market_state(
    state: &mut CollectorState,
    requested: &BTreeSet<String>,
) -> bool {
    if requested.is_empty() {
        return false;
    }
    let prior_market_count = state.markets.len();
    state.markets.retain(|market_id, _| requested.contains(market_id));
    let retained_conditions = state
        .markets
        .values()
        .filter_map(|tracked| tracked.condition_id.as_ref())
        .collect::<BTreeSet<_>>();
    let prior_trade_condition_count = state.trade_seen.len();
    state
        .trade_seen
        .retain(|condition_id, _| retained_conditions.contains(condition_id));
    state.markets.len() != prior_market_count
        || state.trade_seen.len() != prior_trade_condition_count
}

fn settlement_from_market(
    market: &Value,
    symbol: &str,
    window_secs: u64,
    retrieved_at: &str,
) -> Result<Option<Value>> {
    if market.get("closed").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let outcomes = parse_json_array(market.get("outcomes"));
    let token_ids = parse_json_array(market.get("clobTokenIds"));
    let raw_prices = parse_json_array(market.get("outcomePrices"));
    if outcomes.len() != 2 || token_ids.len() != 2 || raw_prices.len() != 2 {
        return Err(completeness_error(
            "closed market is missing its two-outcome settlement vectors",
        ));
    }
    let market_id = market
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| completeness_error("closed market id is missing"))?;
    let condition_id = market
        .get("conditionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| completeness_error("closed market condition id is missing"))?;
    let outcome_names = outcomes
        .iter()
        .map(|value| value.as_str().filter(|value| !value.is_empty()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| completeness_error("closed market outcomes are missing"))?;
    let outcome_set = outcome_names
        .iter()
        .map(|value| value.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if outcome_set != BTreeSet::from(["down".to_owned(), "up".to_owned()])
        && outcome_set != BTreeSet::from(["no".to_owned(), "yes".to_owned()])
    {
        return Err(completeness_error(
            "closed market outcomes are not a binary up/down or yes/no pair",
        ));
    }
    let settlement_token_ids = token_ids
        .iter()
        .map(|value| value.as_str().filter(|value| !value.is_empty()))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| completeness_error("closed market token ids are missing"))?;
    if settlement_token_ids[0] == settlement_token_ids[1] {
        return Err(completeness_error(
            "closed market settlement token ids are not unique",
        ));
    }
    let prices = raw_prices
        .iter()
        .map(|value| Decimal::from_str(&value_text(Some(value))).ok())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| completeness_error("closed market has invalid settlement prices"))?;
    if prices
        .iter()
        .any(|price| *price < Decimal::ZERO || *price > Decimal::ONE)
    {
        return Err(completeness_error(
            "closed market settlement prices are outside [0, 1]",
        ));
    }
    let winners = prices
        .iter()
        .enumerate()
        .filter_map(|(index, price)| (*price >= SETTLEMENT_PRICE).then_some(index))
        .collect::<Vec<_>>();
    if winners.len() != 1 {
        return Err(completeness_error(format!(
            "closed market has {} settlement winners",
            winners.len()
        )));
    }
    let winner = winners[0];
    let loser = 1 - winner;
    if prices[loser] > SETTLEMENT_LOSER_PRICE {
        return Err(completeness_error(
            "closed market losing settlement price must be near zero",
        ));
    }
    let price_sum = prices[0] + prices[1];
    if price_sum < Decimal::ONE - SETTLEMENT_SUM_TOLERANCE
        || price_sum > Decimal::ONE + SETTLEMENT_SUM_TOLERANCE
    {
        return Err(completeness_error(
            "closed market settlement prices must sum to one",
        ));
    }
    let winning_token_id = settlement_token_ids[winner];
    let winning_outcome = outcome_names[winner];
    let resolved_up_won = match winning_outcome.to_ascii_lowercase().as_str() {
        "up" | "yes" => true,
        "down" | "no" => false,
        _ => {
            return Err(completeness_error(format!(
                "closed market winner outcome is unsupported: {winning_outcome}"
            )))
        }
    };
    Ok(Some(json!({
        "kind": "market_settlement",
        "market_id": market_id,
        "condition_id": condition_id,
        "symbol": symbol,
        "market_window_secs": window_secs,
        "winning_token_id": winning_token_id,
        "winning_outcome": winning_outcome,
        "resolved_up_won": resolved_up_won,
        "resolution_source": "gamma_api_closed_market",
        "retrieved_at": retrieved_at,
        "market": market,
    })))
}

fn settlement_is_overdue(
    tracked: &TrackedMarket,
    now: DateTime<Utc>,
    lookback_secs: i64,
) -> Result<bool> {
    let end_time = parse_optional_datetime(tracked.end_time.as_deref())
        .ok_or_else(|| completeness_error("tracked market end time is missing or invalid"))?;
    Ok(!tracked.settled && end_time < now - TimeDelta::seconds(lookback_secs))
}

#[derive(Clone, Copy)]
enum GammaLane {
    Open,
    Closed,
}

impl GammaLane {
    fn is_closed(self) -> bool {
        matches!(self, Self::Closed)
    }
}

#[derive(Default)]
struct GammaLaneDiscovery {
    seen: usize,
    targets: Vec<Value>,
}

#[derive(Default)]
struct GammaDiscovery {
    open: GammaLaneDiscovery,
    closed: GammaLaneDiscovery,
}

impl GammaDiscovery {
    fn append_page(
        &mut self,
        lane: GammaLane,
        payload: &Value,
        symbols: &BTreeSet<String>,
        max_markets: usize,
    ) -> Result<Option<String>> {
        let discovery = match lane {
            GammaLane::Open => &mut self.open,
            GammaLane::Closed => &mut self.closed,
        };
        append_gamma_page(discovery, payload, symbols, max_markets)
    }

    fn into_markets(mut self) -> Vec<Value> {
        self.open.targets.append(&mut self.closed.targets);
        self.open.targets
    }
}

fn append_gamma_page(
    discovery: &mut GammaLaneDiscovery,
    payload: &Value,
    symbols: &BTreeSet<String>,
    max_markets: usize,
) -> Result<Option<String>> {
    let page = payload
        .get("markets")
        .and_then(Value::as_array)
        .ok_or_else(|| completeness_error("Gamma keyset response is missing markets"))?;
    if page.iter().any(|value| !value.is_object()) {
        return Err(completeness_error(
            "Gamma keyset response contains a non-object market",
        ));
    }
    let next_cursor = match payload.get("next_cursor") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) if value.is_empty() => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(completeness_error(
                "Gamma keyset response has an invalid next_cursor",
            ))
        }
    };
    let total = discovery
        .seen
        .checked_add(page.len())
        .ok_or_else(|| completeness_error("Gamma market count overflow"))?;
    if total > max_markets || (total == max_markets && next_cursor.is_some()) {
        return Err(completeness_error(format!(
            "Gamma market discovery reached max_markets={max_markets} before exhausting its cursor"
        )));
    }
    discovery.seen = total;
    discovery.targets.extend(
        page.iter()
            .filter(|market| target_market(market, symbols).is_some())
            .cloned(),
    );
    Ok(next_cursor)
}

fn gamma_discovery_params(
    config: &ReferenceConfig,
    now: DateTime<Utc>,
    closed: bool,
) -> Vec<(String, String)> {
    let lookback_secs = if closed {
        config.settlement_lookback_secs
    } else {
        config.market_lookback_secs
    };
    vec![
        (
            "end_date_min".to_owned(),
            iso_z(now - TimeDelta::seconds(lookback_secs)),
        ),
        (
            "end_date_max".to_owned(),
            iso_z(now + TimeDelta::minutes(30)),
        ),
        ("closed".to_owned(), closed.to_string()),
        ("tag_id".to_owned(), CRYPTO_TAG_ID.to_string()),
        ("related_tags".to_owned(), "false".to_owned()),
        ("limit".to_owned(), "100".to_owned()),
    ]
}

fn object_rows(page: Vec<Value>) -> (Vec<Value>, u64) {
    let page_len = page.len();
    let objects = page
        .into_iter()
        .filter(|value| value.is_object())
        .collect::<Vec<_>>();
    let rejected = u64::try_from(page_len - objects.len()).unwrap_or(u64::MAX);
    (objects, rejected)
}

fn market_metadata_update(
    market_id: &str,
    market: &Value,
    target: &TargetMarket,
    retrieved_at: &str,
    force_context: bool,
    tracked: &mut TrackedMarket,
) -> Result<Option<Value>> {
    let metadata_hash = stable_payload_hash(market)?;
    if !force_context && tracked.last_metadata_hash.as_deref() == Some(&metadata_hash) {
        return Ok(None);
    }
    tracked.last_metadata_hash = Some(metadata_hash);
    Ok(Some(json!({
        "kind": "market_metadata",
        "market_id": market_id,
        "condition_id": market.get("conditionId").and_then(Value::as_str).unwrap_or_default(),
        "symbol": target.symbol,
        "market_window_secs": target.window_secs,
        "source": "gamma_api",
        "retrieved_at": retrieved_at,
        "market": market,
    })))
}

fn trade_timestamp(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn positive_decimal(value: Option<&Value>) -> Option<Decimal> {
    Decimal::from_str(&value_text(value))
        .ok()
        .filter(|value| *value > Decimal::ZERO)
}

fn probability_decimal(value: Option<&Value>) -> Option<Decimal> {
    Decimal::from_str(&value_text(value))
        .ok()
        .filter(|value| *value >= Decimal::ZERO && *value <= Decimal::ONE)
}

fn reject(counter: &mut BTreeMap<String, u64>, reason: &str) {
    *counter.entry(reason.to_owned()).or_default() += 1;
}

#[allow(clippy::too_many_arguments)]
fn trade_updates(
    _config: &ReferenceConfig,
    state: &mut CollectorState,
    market_id: &str,
    condition_id: &str,
    symbol: &str,
    window_secs: u64,
    trades: Vec<Value>,
    _cutoff_at: DateTime<Utc>,
    received_at: DateTime<Utc>,
) -> (Vec<Value>, BTreeMap<String, u64>) {
    let mut parsed = Vec::new();
    let mut malformed = BTreeMap::new();
    for trade in trades {
        let Some(timestamp) = trade_timestamp(trade.get("timestamp")) else {
            reject(&mut malformed, "invalid_timestamp");
            continue;
        };
        if trade.get("conditionId").and_then(Value::as_str) != Some(condition_id) {
            reject(&mut malformed, "condition_mismatch");
            continue;
        }
        if trade
            .get("transactionHash")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            reject(&mut malformed, "missing_transaction_hash");
            continue;
        }
        if trade
            .get("asset")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            reject(&mut malformed, "missing_asset");
            continue;
        }
        if !matches!(
            trade.get("side").and_then(Value::as_str),
            Some("BUY" | "SELL")
        ) {
            reject(&mut malformed, "invalid_side");
            continue;
        }
        if trade
            .get("proxyWallet")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            reject(&mut malformed, "missing_proxy_wallet");
            continue;
        }
        if positive_decimal(trade.get("size")).is_none() {
            reject(&mut malformed, "invalid_size");
            continue;
        }
        if probability_decimal(trade.get("price")).is_none() {
            reject(&mut malformed, "invalid_price");
            continue;
        }
        if !matches!(
            trade.get("outcomeIndex").and_then(Value::as_i64),
            Some(0 | 1)
        ) {
            reject(&mut malformed, "invalid_outcome_index");
            continue;
        }
        if trade
            .get("outcome")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            reject(&mut malformed, "missing_outcome");
            continue;
        }
        parsed.push((timestamp, trade));
    }
    parsed.sort_by_key(|(timestamp, _)| *timestamp);

    let seen = state.trade_seen.entry(condition_id.to_owned()).or_default();
    let mut updates = Vec::new();
    for (timestamp, trade) in parsed {
        let record_id = stable_trade_id(&trade);
        if seen.contains_key(&record_id) {
            continue;
        }
        seen.insert(record_id.clone(), timestamp);
        let mut update = json!({
            "kind": "polymarket_trade",
            "record_id": record_id,
            "record_id_version": TRADE_ID_VERSION,
            "market_id": market_id,
            "condition_id": condition_id,
            "token_id": trade.get("asset").and_then(Value::as_str).unwrap_or_default(),
            "symbol": symbol,
            "market_window_secs": window_secs,
            "side": trade.get("side"),
            "size": trade.get("size"),
            "price": trade.get("price"),
            "trade_ts": iso_z(DateTime::from_timestamp(timestamp, 0).unwrap_or(DateTime::<Utc>::MIN_UTC)),
            "trade_ts_unix": timestamp,
            "transaction_hash": trade.get("transactionHash"),
            "proxy_wallet": trade.get("proxyWallet"),
            "outcome": trade.get("outcome"),
            "outcome_index": trade.get("outcomeIndex"),
            "source": "polymarket_data_api",
            "received_at": iso_z(received_at),
            "trade": null,
        });
        update
            .as_object_mut()
            .expect("trade update must be an object")
            .insert("trade".to_owned(), trade);
        updates.push(update);
    }
    (updates, malformed)
}

#[allow(clippy::too_many_arguments)]
fn advance_trade_finalization(
    tracked: &mut TrackedMarket,
    now: DateTime<Utc>,
    retrieved_at: &str,
    new_trade_count: usize,
    truncated: bool,
    was_settled: bool,
    lag_secs: i64,
    stable_polls_required: u64,
) -> bool {
    if tracked.settlement_seen_at.is_none() {
        tracked.settlement_seen_at = Some(retrieved_at.to_owned());
    }
    if new_trade_count > 0 {
        tracked.last_trade_change_at = Some(retrieved_at.to_owned());
        tracked.trade_finalization_stable_polls = 0;
    }
    let latest_anchor = [
        parse_optional_datetime(tracked.settlement_seen_at.as_deref()),
        parse_optional_datetime(tracked.last_trade_change_at.as_deref()),
    ]
    .into_iter()
    .flatten()
    .max();
    let lag_elapsed = latest_anchor.is_some_and(|anchor| (now - anchor).num_seconds() >= lag_secs);
    if !lag_elapsed || truncated || new_trade_count > 0 || !was_settled {
        tracked.trade_finalization_stable_polls = 0;
        return false;
    }
    tracked.trade_finalization_stable_polls += 1;
    tracked.trade_finalization_stable_polls >= stable_polls_required
}

#[allow(clippy::too_many_arguments)]
fn trade_completion_update(
    state: &CollectorState,
    market_id: &str,
    condition_id: &str,
    symbol: &str,
    market_window_secs: u64,
    retrieved_at: DateTime<Utc>,
    finalization_lag_secs: i64,
    stable_polls_required: u64,
) -> Value {
    let (trade_count, trade_record_ids_sha256) =
        if let Some(record_ids) = state.trade_seen.get(condition_id) {
            (
                record_ids.len(),
                trade_record_ids_sha256(record_ids.keys().map(String::as_str)),
            )
        } else {
            (0, trade_record_ids_sha256(std::iter::empty::<&str>()))
        };
    json!({
        "kind": TRADE_COMPLETION_KIND,
        "market_id": market_id,
        "condition_id": condition_id,
        "symbol": symbol,
        "market_window_secs": market_window_secs,
        "record_id_version": TRADE_ID_VERSION,
        "trade_count": trade_count,
        "trade_record_ids_sha256": trade_record_ids_sha256,
        "source": "polymarket_data_api",
        "retrieved_at": iso_z(retrieved_at),
        "completeness_basis": TRADE_COMPLETION_BASIS,
        "pagination_exhausted": true,
        "settlement_observed": true,
        "malformed_trade_rows": 0,
        "finalization_lag_secs": finalization_lag_secs,
        "stable_polls_required": stable_polls_required,
    })
}

struct ReferenceCollector {
    config: ReferenceConfig,
    symbols: BTreeSet<String>,
    state_path: PathBuf,
    health_path: PathBuf,
    state: CollectorState,
    writer: TapeWriter,
    http: reqwest::Client,
    endpoints: ReferenceEndpoints,
    clock: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
    trade_request_started_at: tokio::sync::Mutex<Option<Instant>>,
    #[cfg(test)]
    trade_request_attempts_started_at: std::sync::Mutex<Vec<Instant>>,
    last_success: Instant,
}

fn recover_state_from_tape_row(
    state: &mut CollectorState,
    row: &Value,
    _trade_cutoff: i64,
) -> Result<()> {
    let Some(update) = row.get("update").and_then(Value::as_object) else {
        return Ok(());
    };
    let kind = update
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let market_id = update.get("market_id").and_then(Value::as_str);
    let condition_id = update.get("condition_id").and_then(Value::as_str);
    if kind == "polymarket_trade" {
        if update.get("record_id_version").and_then(Value::as_str) == Some(TRADE_ID_VERSION) {
            if let (Some(condition_id), Some(record_id), Some(timestamp)) = (
                condition_id,
                update.get("record_id").and_then(Value::as_str),
                update.get("trade_ts_unix").and_then(Value::as_i64),
            ) {
                state
                    .trade_seen
                    .entry(condition_id.to_owned())
                    .or_default()
                    .insert(record_id.to_owned(), timestamp);
            }
        }
    } else if kind == TRADE_COMPLETION_KIND {
        if let Some(market_id) = market_id {
            let tracked = state.markets.entry(market_id.to_owned()).or_default();
            tracked.condition_id = condition_id.map(str::to_owned);
            tracked.symbol = update
                .get("symbol")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(window) = update.get("market_window_secs").and_then(Value::as_u64) {
                tracked.market_window_secs = window;
            }
            tracked.settled = true;
            tracked.trade_complete = true;
        }
    } else if matches!(kind, "market_metadata" | "market_settlement") {
        if let Some(market_id) = market_id {
            let tracked = state.markets.entry(market_id.to_owned()).or_default();
            tracked.condition_id = condition_id.map(str::to_owned);
            tracked.symbol = update
                .get("symbol")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(window) = update.get("market_window_secs").and_then(Value::as_u64) {
                tracked.market_window_secs = window;
            }
            if let Some(market) = update.get("market") {
                tracked.end_time = market
                    .get("endDate")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                tracked.last_metadata_hash = Some(stable_payload_hash(market)?);
            }
            if kind == "market_settlement" {
                tracked.settled = true;
            }
        }
    }
    Ok(())
}

fn compact_trade_dedupe(state: &mut CollectorState, cutoff: i64) -> bool {
    let incomplete = state
        .markets
        .values()
        .filter(|market| !market.trade_complete)
        .filter_map(|market| market.condition_id.clone())
        .collect::<BTreeSet<_>>();
    let mut changed = false;
    state.trade_seen.retain(|condition_id, seen| {
        if incomplete.contains(condition_id) {
            return true;
        }
        let before = seen.len();
        seen.retain(|_, timestamp| *timestamp >= cutoff);
        changed |= seen.len() != before || seen.is_empty();
        !seen.is_empty()
    });
    changed
}

/// Incomplete markets retain every trade ID until their completion proof is
/// emitted, so at high trade volume the retained-ID cap becomes a guaranteed
/// fail-closed crash once completion falls behind. Keep a headroom margin
/// below the cap instead: evict the oldest incomplete-market IDs (the least
/// re-fetchable) before the cap is reached. `validate_state_bounds` stays the
/// fail-closed boundary for accumulation eviction cannot cover.
const TRADE_ID_BUDGET_DIVISOR: usize = 4;

fn trade_id_budget(max_retained_trade_ids: usize) -> usize {
    max_retained_trade_ids - max_retained_trade_ids / TRADE_ID_BUDGET_DIVISOR
}

fn retained_trade_id_count(state: &CollectorState) -> usize {
    state.trade_seen.values().map(BTreeMap::len).sum()
}

fn evict_oldest_incomplete_trade_ids(state: &mut CollectorState, budget: usize) -> usize {
    let excess = retained_trade_id_count(state).saturating_sub(budget);
    if excess == 0 {
        return 0;
    }
    let incomplete = state
        .markets
        .values()
        .filter(|market| !market.trade_complete)
        .filter_map(|market| market.condition_id.clone())
        .collect::<BTreeSet<_>>();
    // Pass 1: find the timestamp of the excess-th oldest incomplete ID. Only
    // i64 timestamps are collected (~8 bytes each) so a 4M-entry state does
    // not duplicate every condition/trade ID string during recovery.
    let mut timestamps: Vec<i64> = Vec::new();
    for (condition_id, seen) in &state.trade_seen {
        if !incomplete.contains(condition_id) {
            continue;
        }
        timestamps.extend(seen.values().copied());
    }
    timestamps.sort_unstable();
    let Some(&cutoff_ts) = timestamps.get(
        excess
            .saturating_sub(1)
            .min(timestamps.len().saturating_sub(1)),
    ) else {
        // Every retained ID belongs to a completed market; eviction never
        // covers those, and validate_state_bounds stays fail-closed for them.
        return 0;
    };
    // Pass 2: evict strictly older IDs, then ties in deterministic BTreeMap order.
    let mut evicted = 0_usize;
    let mut emptied = Vec::new();
    for (condition_id, seen) in state.trade_seen.iter_mut() {
        if !incomplete.contains(condition_id) {
            continue;
        }
        let before = seen.len();
        seen.retain(|_, timestamp| *timestamp >= cutoff_ts);
        evicted += before - seen.len();
        if seen.is_empty() {
            emptied.push(condition_id.clone());
        }
    }
    if evicted < excess {
        'outer: for (condition_id, seen) in state.trade_seen.iter_mut() {
            if !incomplete.contains(condition_id) {
                continue;
            }
            let ties: Vec<String> = seen
                .iter()
                .filter(|(_, timestamp)| **timestamp == cutoff_ts)
                .map(|(trade_id, _)| trade_id.clone())
                .collect();
            for trade_id in ties {
                seen.remove(&trade_id);
                evicted += 1;
                if evicted >= excess {
                    if seen.is_empty() {
                        emptied.push(condition_id.clone());
                    }
                    break 'outer;
                }
            }
            if seen.is_empty() {
                emptied.push(condition_id.clone());
            }
        }
    }
    for condition_id in emptied {
        state.trade_seen.remove(&condition_id);
    }
    evicted
}

fn validate_state_bounds(
    state: &CollectorState,
    max_markets: usize,
    max_retained_trade_ids: usize,
) -> Result<()> {
    let retained_trade_ids = state.trade_seen.values().try_fold(0_usize, |total, seen| {
        total.checked_add(seen.len()).context("trade ID count overflow")
    })?;
    if state.markets.len() > max_markets
        || state.trade_seen.len() > max_markets
        || retained_trade_ids > max_retained_trade_ids
    {
        bail!("collector state exceeds configured market or retained trade ID limit");
    }
    Ok(())
}

fn validate_cycle_hour(target_hour: &str, recorded_at: DateTime<Utc>) -> Result<()> {
    if hour_key(recorded_at) != target_hour {
        bail!("collector cycle crossed a UTC-hour boundary; retrying with fresh context");
    }
    Ok(())
}

impl ReferenceCollector {
    fn new(config: ReferenceConfig) -> Result<Self> {
        config.validate()?;
        let startup_at = utc_now();
        let symbols = config
            .symbols
            .iter()
            .map(|symbol| symbol.trim().to_ascii_uppercase())
            .filter(|symbol| !symbol.is_empty())
            .collect::<BTreeSet<_>>();
        let supported = SYMBOL_ALIASES
            .iter()
            .map(|(symbol, _)| (*symbol).to_owned())
            .collect::<BTreeSet<_>>();
        if symbols.is_empty() || !symbols.is_subset(&supported) {
            bail!("symbols must be a non-empty subset of the supported Polymarket catalog");
        }
        ensure_canonical_directory(&config.spool_dir)
            .with_context(|| format!("invalid spool directory {}", config.spool_dir.display()))?;
        let state_path = config.spool_dir.join("collector-state.json");
        let health_path = config.spool_dir.join("health.json");
        let mut state: CollectorState = read_optional_json(&state_path)?;
        let mut state_migrated = false;
        if state.trade_id_version.as_deref() != Some(TRADE_ID_VERSION) {
            let active = config.spool_dir.join(ACTIVE_TAPE);
            if active.exists() {
                strict_existing_regular(&active)?;
                if fs::metadata(&active)?.len() > 0 {
                    let quarantine = config.spool_dir.join(format!(
                        "superseded-v1-{ACTIVE_TAPE}.{}",
                        startup_at.timestamp_nanos_opt().unwrap_or_default()
                    ));
                    fs::rename(&active, quarantine)?;
                    File::open(&config.spool_dir)?.sync_all()?;
                }
            }
            state.trade_seen.clear();
            for tracked in state.markets.values_mut() {
                tracked.trade_complete = false;
            }
            state.trade_id_version = Some(TRADE_ID_VERSION.to_owned());
            state_migrated = true;
        }
        if state.trade_completion_version.as_deref() != Some(TRADE_COMPLETION_VERSION) {
            for tracked in state.markets.values_mut() {
                tracked.trade_complete = false;
                tracked.trade_finalization_stable_polls = 0;
            }
            state.trade_completion_version = Some(TRADE_COMPLETION_VERSION.to_owned());
            state_migrated = true;
        }
        if state_migrated {
            atomic_write_json(&state_path, &state)?;
        }
        // Completed or orphaned conditions may be compacted; incomplete
        // markets retain every ID until their proof is emitted.
        let trade_cutoff = startup_at.timestamp() - config.settlement_lookback_secs;
        let state_compacted = compact_trade_dedupe(&mut state, trade_cutoff);
        let mut writer = TapeWriter::new_with_recovery(&config.spool_dir, |row| {
            recover_state_from_tape_row(&mut state, row, trade_cutoff)
        })?;
        let recovered_active = writer.sequence > 0;
        if recovered_active {
            let validation = validate_reference_tape_for_recovery(&writer.active, startup_at)
                .context("recovered active tape failed uploader validation");
            let cache_release = writer
                .file
                .as_ref()
                .context("recovered active tape is closed")
                .and_then(release_clean_file_cache);
            validation?;
            cache_release?;
        }
        let state_scoped = retain_requested_market_state(&mut state, &config.market_ids);
        let startup_evicted = evict_oldest_incomplete_trade_ids(
            &mut state,
            trade_id_budget(config.max_retained_trade_ids),
        );
        validate_state_bounds(&state, config.max_markets, config.max_retained_trade_ids)?;
        if state_compacted || startup_evicted > 0 || recovered_active || state_scoped {
            // The state checkpoint must be durable before the recovered segment
            // stops being the active crash-recovery source.
            atomic_write_json(&state_path, &state)?;
        }
        if recovered_active {
            writer.rotate(startup_at)?;
        }
        let http = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self {
            config,
            symbols,
            state_path,
            health_path,
            state,
            writer,
            http,
            endpoints: ReferenceEndpoints::default(),
            clock: Box::new(utc_now),
            trade_request_started_at: tokio::sync::Mutex::new(None),
            #[cfg(test)]
            trade_request_attempts_started_at: std::sync::Mutex::new(Vec::new()),
            last_success: Instant::now(),
        })
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    async fn get_json(&self, url: &str, params: &[(String, String)]) -> Result<Value> {
        self.get_json_with_request_spacing(url, params, false).await
    }

    async fn get_json_rate_limited(&self, url: &str, params: &[(String, String)]) -> Result<Value> {
        self.get_json_with_request_spacing(url, params, true).await
    }

    async fn get_json_with_request_spacing(
        &self,
        url: &str,
        params: &[(String, String)],
        rate_limited: bool,
    ) -> Result<Value> {
        for attempt in 0..HTTP_GET_ATTEMPTS {
            if rate_limited {
                self.wait_for_trade_request_slot().await;
            }
            let response = match self.http.get(url).query(params).send().await {
                Ok(response) => response,
                Err(error) => {
                    if attempt + 1 == HTTP_GET_ATTEMPTS || !retryable_http_error(&error) {
                        return Err(error.into());
                    }
                    tokio::time::sleep(
                        http_retry_delay(None, attempt, Utc::now())
                            .expect("retry delay without Retry-After must be bounded"),
                    )
                    .await;
                    continue;
                }
            };
            let status = response.status();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .cloned();
            if !status.is_success() {
                let error = match response.error_for_status() {
                    Ok(_) => unreachable!("non-success response passed error_for_status"),
                    Err(error) => error,
                };
                if attempt + 1 == HTTP_GET_ATTEMPTS || !retryable_http_status(status) {
                    return Err(error.into());
                }
                let retry_after = retry_after
                    .as_ref()
                    .map(|value| value.to_str())
                    .transpose()
                    .context("server returned a non-text Retry-After header")?;
                let Some(delay) = http_retry_delay(retry_after, attempt, Utc::now()) else {
                    return Err(error).context(
                        "server Retry-After cannot be honored within the bounded retry budget",
                    );
                };
                tokio::time::sleep(delay).await;
                continue;
            }
            match response.json().await {
                Ok(value) => return Ok(value),
                Err(error) => {
                    if attempt + 1 == HTTP_GET_ATTEMPTS || !retryable_http_error(&error) {
                        return Err(error.into());
                    }
                    tokio::time::sleep(
                        http_retry_delay(None, attempt, Utc::now())
                            .expect("retry delay without Retry-After must be bounded"),
                    )
                    .await;
                }
            }
        }
        unreachable!("HTTP_GET_ATTEMPTS is non-zero")
    }

    async fn wait_for_trade_request_slot(&self) {
        let mut last_started = self.trade_request_started_at.lock().await;
        if let Some(previous) = *last_started {
            let remaining = self
                .config
                .per_market_delay
                .saturating_sub(previous.elapsed());
            if !remaining.is_zero() {
                tokio::time::sleep(remaining).await;
            }
        }
        *last_started = Some(Instant::now());
        #[cfg(test)]
        self.trade_request_attempts_started_at
            .lock()
            .expect("trade request attempt history mutex poisoned")
            .push(Instant::now());
    }

    async fn discover_markets(&self, now: DateTime<Utc>) -> Result<Vec<Value>> {
        if !self.config.market_ids.is_empty() {
            let mut markets = Vec::with_capacity(self.config.market_ids.len());
            for market_id in &self.config.market_ids {
                let url = format!(
                    "{}/{}",
                    self.endpoints.gamma_market,
                    urlencoding::encode(market_id)
                );
                let market = self
                    .get_json(&url, &[])
                    .await
                    .with_context(|| format!("Gamma market detail request failed for {market_id}"))?;
                if market.get("id").and_then(Value::as_str) != Some(market_id.as_str()) {
                    bail!("Gamma market detail did not return requested market ID {market_id}");
                }
                markets.push(market);
            }
            return Ok(markets);
        }

        let mut discovery = GammaDiscovery::default();
        for lane in [GammaLane::Open, GammaLane::Closed] {
            let base = gamma_discovery_params(&self.config, now, lane.is_closed());
            let mut cursor: Option<String> = None;
            let mut seen_cursors = BTreeSet::new();
            loop {
                let mut params = base.clone();
                if let Some(cursor) = cursor.as_ref() {
                    params.push(("after_cursor".to_owned(), cursor.clone()));
                }
                let payload = self
                    .get_json(&self.endpoints.gamma_markets, &params)
                    .await?;
                cursor = discovery.append_page(
                    lane,
                    &payload,
                    &self.symbols,
                    self.config.max_markets,
                )?;
                let Some(next_cursor) = cursor.as_ref() else {
                    break;
                };
                if !seen_cursors.insert(next_cursor.clone()) {
                    return Err(completeness_error(format!(
                        "Gamma keyset cursor repeated: {next_cursor}"
                    )));
                }
            }
        }
        Ok(discovery.into_markets())
    }

    async fn fetch_trades(&self, condition_id: &str) -> Result<(Vec<Value>, bool, u64)> {
        let mut trades = Vec::new();
        let mut truncated = false;
        let mut non_object_rows = 0_u64;
        for offset in [0_u64, 10_000] {
            let payload = self
                .get_json_rate_limited(
                    &self.endpoints.data_trades,
                    &[
                        ("market".to_owned(), condition_id.to_owned()),
                        ("limit".to_owned(), "10000".to_owned()),
                        ("offset".to_owned(), offset.to_string()),
                        ("takerOnly".to_owned(), "false".to_owned()),
                    ],
                )
                .await?;
            let Value::Array(page) = payload else {
                bail!("Data API trades response is not an array");
            };
            let page_len = page.len();
            let (objects, rejected) = object_rows(page);
            trades.extend(objects);
            non_object_rows = non_object_rows.saturating_add(rejected);
            if page_len < 10_000 {
                break;
            }
            if offset == 10_000 {
                truncated = true;
            }
        }
        Ok((trades, truncated, non_object_rows))
    }

    async fn collect_once(&mut self) -> Result<Value> {
        let cycle_started = Instant::now();
        let now = self.now();
        let retrieved_at = iso_z(now);
        let mut updates = PendingUpdates::new(&self.config.spool_dir)?;
        let mut errors = Vec::new();
        let mut truncated_markets = Vec::new();
        let mut non_object_trade_markets = Vec::new();
        let mut invalid_settlement_markets = Vec::new();
        let mut trade_polls = 0_u64;
        let mut successful_trade_polls = 0_u64;
        let mut malformed_trade_reasons = BTreeMap::<String, u64>::new();
        let mut next_state = self.state.clone();
        let discovered = self.discover_markets(now).await?;
        let mut targets = BTreeMap::<String, (Value, TargetMarket)>::new();
        let mut discovered_target_symbols = BTreeSet::new();
        let target_hour = hour_key(now);
        let force_hour_context = context_seed_required(&self.state, &self.writer, now);

        for market in discovered {
            let Some(target) = target_market(&market, &self.symbols) else {
                continue;
            };
            let Some(market_id) = market
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !self.config.market_ids.is_empty()
                && !self.config.market_ids.contains(market_id)
            {
                continue;
            }
            let condition_id = market
                .get("conditionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let tracked = next_state.markets.entry(market_id.to_owned()).or_default();
            tracked.condition_id = Some(condition_id);
            tracked.symbol = Some(target.symbol.clone());
            tracked.market_window_secs = target.window_secs;
            tracked.end_time = market
                .get("endDate")
                .and_then(Value::as_str)
                .map(str::to_owned);
            discovered_target_symbols.insert(target.symbol.clone());
            targets.insert(market_id.to_owned(), (market, target));
        }
        let target_ids = targets.keys().cloned().collect::<BTreeSet<_>>();
        validate_requested_market_ids(&self.config.market_ids, &target_ids)?;
        let missing_target_symbols = missing_symbols(&self.symbols, &discovered_target_symbols);
        let priority_cutoff = now - TimeDelta::seconds(self.config.trade_finalization_lag_secs);
        let priority_trade_markets_before_market_details =
            trade_poll_candidates(&targets, &next_state, priority_cutoff)
                .iter()
                .filter(|candidate| candidate.priority)
                .count();
        let market_detail_budget = shared_market_detail_budget(
            self.config.max_trade_polls_per_cycle,
            self.config.max_concurrent_trade_polls,
            priority_trade_markets_before_market_details,
        );
        let market_detail_plan =
            plan_market_detail_fetches(&next_state.markets, &target_ids, now, market_detail_budget);
        let fetcher = &*self;
        let mut market_detail_fetches = stream::iter(market_detail_plan.selected.iter())
            .map(|market_id| async move {
                let url = format!("{}/{market_id}", fetcher.endpoints.gamma_market);
                (market_id.clone(), fetcher.get_json(&url, &[]).await)
            })
            .buffer_unordered(self.config.max_concurrent_trade_polls)
            .collect::<BTreeMap<_, _>>()
            .await;

        let tracked_ids = next_state.markets.keys().cloned().collect::<Vec<_>>();
        for market_id in tracked_ids {
            let Some(mut tracked) = next_state.markets.remove(&market_id) else {
                continue;
            };
            let end_time = parse_optional_datetime(tracked.end_time.as_deref());
            if end_time
                .is_some_and(|end| end < now - TimeDelta::seconds(self.config.market_lookback_secs))
                && tracked.settled
                && tracked.trade_complete
            {
                if let Some(condition_id) = tracked.condition_id.as_ref() {
                    next_state.trade_seen.remove(condition_id);
                }
                continue;
            }
            let needs_detail = !targets.contains_key(&market_id)
                && end_time.is_some_and(|end| end <= now)
                && !(tracked.settled && tracked.trade_complete);
            if needs_detail && market_detail_plan.selected.contains(&market_id) {
                tracked.last_market_detail_attempt_at = Some(retrieved_at.clone());
                match market_detail_fetches
                    .remove(&market_id)
                    .ok_or_else(|| anyhow!("selected market {market_id} has no detail fetch"))?
                {
                    Ok(market) => match recovered_target(&market_id, &market, &self.symbols) {
                        Ok(target) => {
                            targets.insert(market_id.clone(), (market, target));
                            tracked.settlement_failure_since = None;
                            tracked.settlement_last_error = None;
                        }
                        Err(error) => {
                            let error = error.to_string();
                            errors.push(format!("settlement {market_id}: {error}"));
                            tracked
                                .settlement_failure_since
                                .get_or_insert_with(|| retrieved_at.clone());
                            tracked.settlement_last_error = Some(error);
                        }
                    },
                    Err(error) => {
                        let error = error.to_string();
                        errors.push(format!("settlement {market_id}: {error}"));
                        tracked
                            .settlement_failure_since
                            .get_or_insert_with(|| retrieved_at.clone());
                        tracked.settlement_last_error = Some(error);
                    }
                }
            }
            next_state.markets.insert(market_id, tracked);
        }
        if !market_detail_fetches.is_empty() {
            bail!(
                "{} scheduled market detail fetches were not applied",
                market_detail_fetches.len()
            );
        }

        let trade_poll_budget = remaining_trade_poll_budget(
            self.config.max_trade_polls_per_cycle,
            market_detail_plan.selected.len(),
        );
        let trade_poll_plan = plan_trade_polls(
            trade_poll_candidates(&targets, &next_state, priority_cutoff),
            trade_poll_budget,
        );
        let target_count = targets.len();
        let processing_target_ids = targets.keys().cloned().collect::<Vec<_>>();
        let target_chunks = target_processing_chunks(
            processing_target_ids.iter(),
            &trade_poll_plan.selected,
            self.config.max_concurrent_trade_polls,
        );
        for target_chunk in target_chunks {
            let fetcher = &*self;
            let trade_fetches = stream::iter(target_chunk.iter().filter_map(|market_id| {
                if !trade_poll_plan.selected.contains(*market_id) {
                    return None;
                }
                let condition_id = targets
                    .get(*market_id)?
                    .0
                    .get("conditionId")?
                    .as_str()?
                    .to_owned();
                Some(((*market_id).clone(), condition_id))
            }))
            .map(move |(market_id, condition_id)| async move {
                let result = fetcher.fetch_trades(&condition_id).await;
                (market_id, (result, fetcher.now()))
            })
            .buffer_unordered(self.config.max_concurrent_trade_polls)
            .collect::<Vec<_>>()
            .await;
            let mut trade_fetches = trade_fetches.into_iter().collect::<BTreeMap<_, _>>();

            for market_id in target_chunk {
                let (market, target) = targets
                    .remove(market_id)
                    .ok_or_else(|| anyhow!("target market {market_id} disappeared"))?;
                let condition_id = market
                    .get("conditionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let mut tracked = next_state.markets.remove(market_id).unwrap_or_default();
                tracked.condition_id = Some(condition_id.clone());
                tracked.symbol = Some(target.symbol.clone());
                tracked.market_window_secs = target.window_secs;
                if let Some(end_time) = market.get("endDate").and_then(Value::as_str) {
                    tracked.end_time = Some(end_time.to_owned());
                }

                if let Some(metadata) = market_metadata_update(
                    market_id,
                    &market,
                    &target,
                    &retrieved_at,
                    force_hour_context,
                    &mut tracked,
                )? {
                    updates.push(metadata)?;
                }

                let settlement = match settlement_from_market(
                    &market,
                    &target.symbol,
                    target.window_secs,
                    &retrieved_at,
                ) {
                    Ok(settlement) => settlement,
                    Err(error) => {
                        let detail = error.to_string();
                        errors.push(format!("settlement {market_id}: {detail}"));
                        invalid_settlement_markets.push(market_id.clone());
                        tracked
                            .settlement_failure_since
                            .get_or_insert_with(|| retrieved_at.clone());
                        tracked.settlement_last_error = Some(detail);
                        None
                    }
                };
                if settlement.is_some() {
                    tracked.settlement_failure_since = None;
                    tracked.settlement_last_error = None;
                }
                let settlement_available = settlement.is_some();
                let was_settled = tracked.settled;
                if !condition_id.is_empty()
                    && !tracked.trade_complete
                    && trade_poll_plan.selected.contains(market_id)
                {
                    trade_polls += 1;
                    let (trade_fetch, trade_received_at) =
                        trade_fetches.remove(market_id).ok_or_else(|| {
                            anyhow!("selected trade market {market_id} has no scheduled fetch")
                        })?;
                    match trade_fetch {
                        Ok((trades, truncated, non_object_rows)) => {
                            successful_trade_polls += 1;
                            let (new_updates, mut malformed) = trade_updates(
                                &self.config,
                                &mut next_state,
                                market_id,
                                &condition_id,
                                &target.symbol,
                                target.window_secs,
                                trades,
                                now,
                                trade_received_at,
                            );
                            if non_object_rows > 0 {
                                *malformed.entry("non_object_trade".to_owned()).or_default() +=
                                    non_object_rows;
                                non_object_trade_markets.push(condition_id.clone());
                            }
                            let new_trade_count = new_updates.len();
                            for update in new_updates {
                                updates.push(update)?;
                            }
                            for (reason, count) in &malformed {
                                *malformed_trade_reasons.entry(reason.clone()).or_default() +=
                                    count;
                            }
                            tracked.last_trade_success_at = Some(retrieved_at.clone());
                            if malformed.is_empty() {
                                tracked.trade_failure_since = None;
                                tracked.trade_last_error = None;
                            } else {
                                let detail = format!("malformed trade rows: {malformed:?}");
                                errors.push(format!("trades {condition_id}: {detail}"));
                                tracked
                                    .trade_failure_since
                                    .get_or_insert_with(|| retrieved_at.clone());
                                tracked.trade_last_error = Some(detail);
                            }
                            if truncated {
                                truncated_markets.push(condition_id.clone());
                            }
                            if settlement_available
                                && advance_trade_finalization(
                                    &mut tracked,
                                    now,
                                    &retrieved_at,
                                    new_trade_count,
                                    truncated || !malformed.is_empty(),
                                    was_settled,
                                    self.config.trade_finalization_lag_secs,
                                    self.config.trade_finalization_stable_polls,
                                )
                            {
                                updates.push(trade_completion_update(
                                    &next_state,
                                    market_id,
                                    &condition_id,
                                    &target.symbol,
                                    target.window_secs,
                                    trade_received_at,
                                    self.config.trade_finalization_lag_secs,
                                    self.config.trade_finalization_stable_polls,
                                ))?;
                                tracked.trade_complete = true;
                            }
                        }
                        Err(error) => {
                            errors.push(format!("trades {condition_id}: {error}"));
                            tracked
                                .trade_failure_since
                                .get_or_insert_with(|| retrieved_at.clone());
                            tracked.trade_last_error = Some(error.to_string());
                        }
                    }
                }
                if let Some(settlement) = settlement {
                    if !tracked.settled {
                        updates.push(settlement)?;
                        tracked.settled = true;
                    }
                }
                next_state.markets.insert(market_id.clone(), tracked);
            }
            if !trade_fetches.is_empty() {
                bail!(
                    "{} scheduled trade fetches were not applied",
                    trade_fetches.len()
                );
            }
        }

        let mut overdue_unresolved_markets = Vec::new();
        let mut invalid_end_time_markets = Vec::new();
        for (market_id, tracked) in &next_state.markets {
            match settlement_is_overdue(tracked, now, self.config.settlement_lookback_secs) {
                Ok(true) => overdue_unresolved_markets.push(market_id.clone()),
                Ok(false) => {}
                Err(_) => invalid_end_time_markets.push(market_id.clone()),
            }
        }
        let mut stale_trade_markets = next_state
            .markets
            .values()
            .filter_map(|tracked| {
                let failed_at = parse_optional_datetime(tracked.trade_failure_since.as_deref())?;
                ((now - failed_at).to_std().ok()? > self.config.stale_after)
                    .then(|| tracked.condition_id.clone().unwrap_or_default())
            })
            .collect::<Vec<_>>();
        let mut stale_settlement_markets = next_state
            .markets
            .iter()
            .filter_map(|(market_id, tracked)| {
                let failed_at =
                    parse_optional_datetime(tracked.settlement_failure_since.as_deref())?;
                ((now - failed_at).to_std().ok()? > self.config.stale_after)
                    .then(|| market_id.clone())
            })
            .collect::<Vec<_>>();

        let records_written = updates.len();
        let record_types = updates.record_types();
        let recorded_at = self.now();
        validate_cycle_hour(&target_hour, recorded_at)?;
        let trade_ids_evicted = evict_oldest_incomplete_trade_ids(
            &mut next_state,
            trade_id_budget(self.config.max_retained_trade_ids),
        );
        validate_state_bounds(
            &next_state,
            self.config.max_markets,
            self.config.max_retained_trade_ids,
        )?;
        updates.replay(&mut self.writer, recorded_at)?;
        if missing_target_symbols.is_empty() {
            next_state.context_seed_hour = Some(target_hour);
        }
        self.state = next_state;
        atomic_write_json(&self.state_path, &self.state)?;
        self.last_success = Instant::now();
        let completed_at = iso_z(self.now());
        let cycle_duration_ms =
            u64::try_from(cycle_started.elapsed().as_millis()).unwrap_or(u64::MAX);

        overdue_unresolved_markets.sort();
        truncated_markets.sort();
        non_object_trade_markets.sort();
        invalid_settlement_markets.sort();
        invalid_end_time_markets.sort();
        stale_trade_markets.sort();
        stale_settlement_markets.sort();
        let health = json!({
            "cycle_started_at": retrieved_at,
            "cycle_duration_ms": cycle_duration_ms,
            "updated_at": completed_at,
            "last_success_at": completed_at,
            "target_markets": target_count,
            "configured_market_ids": self.config.market_ids.len(),
            "missing_target_symbols": missing_target_symbols,
            "tracked_markets": self.state.markets.len(),
            "market_detail_budget": market_detail_budget,
            "market_detail_eligible": market_detail_plan.eligible,
            "market_detail_priority": market_detail_plan.priority,
            "market_detail_selected": market_detail_plan.selected.len(),
            "market_detail_deferred": market_detail_plan.deferred,
            "market_detail_priority_deferred": market_detail_plan.priority_deferred,
            "records_written": records_written,
            "record_types": record_types,
            "trade_ids_evicted": trade_ids_evicted,
            "api_errors": errors,
            "trade_poll_budget": self.config.max_trade_polls_per_cycle,
            "priority_trade_markets_before_market_details": priority_trade_markets_before_market_details,
            "trade_poll_budget_after_market_details": trade_poll_budget,
            "trade_poll_concurrency": self.config.max_concurrent_trade_polls,
            "trade_request_spacing_ms": u64::try_from(self.config.per_market_delay.as_millis()).unwrap_or(u64::MAX),
            "eligible_trade_markets": trade_poll_plan.eligible,
            "priority_trade_markets": trade_poll_plan.priority,
            "selected_trade_markets": trade_poll_plan.selected.len(),
            "deferred_trade_markets": trade_poll_plan.deferred,
            "priority_trade_backlog": trade_poll_plan.priority_deferred,
            "trade_polls": trade_polls,
            "successful_trade_polls": successful_trade_polls,
            "malformed_trade_rows": malformed_trade_reasons.values().sum::<u64>(),
            "malformed_trade_reasons": malformed_trade_reasons,
            "truncated_trade_markets": truncated_markets,
            "non_object_trade_markets": non_object_trade_markets,
            "invalid_settlement_markets": invalid_settlement_markets,
            "invalid_end_time_markets": invalid_end_time_markets,
            "stale_trade_markets": stale_trade_markets,
            "stale_settlement_markets": stale_settlement_markets,
            "overdue_unresolved_markets": overdue_unresolved_markets,
            "active_tape_bytes": fs::metadata(&self.writer.active).map(|value| value.len()).unwrap_or_default(),
            "free_disk_bytes": fs4::available_space(&self.config.spool_dir)?,
        });
        atomic_write_json(&self.health_path, &health)?;

        if !missing_target_symbols.is_empty() {
            return Err(DataCompletenessError(format!(
                "target discovery missed configured symbols: {missing_target_symbols:?}"
            ))
            .into());
        }
        if !truncated_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "trade pagination exceeded API offset limit for {truncated_markets:?}"
            ))
            .into());
        }
        if !non_object_trade_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "non-object trade rows for {non_object_trade_markets:?}"
            ))
            .into());
        }
        if !invalid_settlement_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "invalid settlement payloads for {invalid_settlement_markets:?}"
            ))
            .into());
        }
        if !invalid_end_time_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "tracked markets with missing or invalid end time: {invalid_end_time_markets:?}"
            ))
            .into());
        }
        if !stale_trade_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "stale trade markets: {stale_trade_markets:?}"
            ))
            .into());
        }
        if !stale_settlement_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "stale settlement markets: {stale_settlement_markets:?}"
            ))
            .into());
        }
        Ok(health)
    }

    async fn collect_once_bounded(&mut self) -> Result<Value> {
        let _hard_watchdog = CycleWatchdog::arm(MAX_CYCLE_DURATION).map_err(|error| {
            completeness_error(format!("could not arm the hard cycle watchdog: {error}"))
        })?;
        tokio::time::timeout(MAX_CYCLE_DURATION, self.collect_once())
            .await
            .map_err(|_| {
                completeness_error(format!(
                    "collector cycle exceeded the absolute {}ms deadline",
                    MAX_CYCLE_DURATION.as_millis()
                ))
            })?
    }
}

fn initialize_reference_collector_with_watchdog<F, T>(timeout: Duration, initialize: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let _hard_watchdog = CycleWatchdog::arm(timeout).map_err(|error| {
        completeness_error(format!("could not arm the hard startup watchdog: {error}"))
    })?;
    initialize()
}

pub async fn run_reference(config: ReferenceConfig, once: bool) -> Result<()> {
    let poll_interval = config.poll_interval;
    let stale_after = config.stale_after;
    let (_spool_lock, mut collector) =
        initialize_reference_collector_with_watchdog(MAX_CYCLE_DURATION, || {
            let spool_lock = ReferenceSpoolLock::acquire(&config.spool_dir)?;
            Ok((spool_lock, ReferenceCollector::new(config)?))
        })?;
    if once {
        println!(
            "{}",
            serde_json::to_string(&collector.collect_once_bounded().await?)?
        );
        collector.writer.close()?;
        return Ok(());
    }
    loop {
        let started = Instant::now();
        match collector.collect_once_bounded().await {
            Ok(health) => println!("{}", serde_json::to_string(&health)?),
            Err(error) => {
                eprintln!("Polymarket reference poll failed: {error:#}");
                if error.downcast_ref::<DataCompletenessError>().is_some()
                    || collector.last_success.elapsed() > stale_after
                {
                    collector.writer.close()?;
                    return Err(error);
                }
            }
        }
        tokio::time::sleep(poll_interval.saturating_sub(started.elapsed())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HARD_WATCHDOG_TEST_ENV: &str = "MONDAY_TEST_POLYMARKET_HARD_WATCHDOG";
    const HARD_WATCHDOG_HOLD_STDERR_TEST_ENV: &str =
        "MONDAY_TEST_POLYMARKET_HARD_WATCHDOG_HOLD_STDERR";
    const STARTUP_WATCHDOG_TEST_ENV: &str = "MONDAY_TEST_POLYMARKET_STARTUP_WATCHDOG";

    struct TestDir {
        _temp: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let temp = tempfile::Builder::new()
                .prefix("monday-polymarket-reference-test-")
                .tempdir()
                .unwrap();
            let path = fs::canonicalize(temp.path()).unwrap();
            Self { _temp: temp, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn fixed_time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    #[ignore = "subprocess target for the hard cycle watchdog test"]
    fn hard_cycle_watchdog_child() {
        if std::env::var_os(HARD_WATCHDOG_TEST_ENV).is_none() {
            return;
        }
        let _stderr_lock_holder = if std::env::var_os(HARD_WATCHDOG_HOLD_STDERR_TEST_ENV).is_some()
        {
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let holder = std::thread::spawn(move || {
                let _stderr = std::io::stderr().lock();
                ready_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_secs(10));
            });
            ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            Some(holder)
        } else {
            None
        };
        let _watchdog = CycleWatchdog::arm(Duration::from_millis(100)).unwrap();
        std::thread::sleep(Duration::from_secs(10));
    }

    #[test]
    #[ignore = "subprocess target for the startup watchdog test"]
    fn startup_watchdog_child() {
        if std::env::var_os(STARTUP_WATCHDOG_TEST_ENV).is_none() {
            return;
        }
        let _ = initialize_reference_collector_with_watchdog(
            Duration::from_millis(50),
            || -> Result<ReferenceCollector> {
                std::thread::sleep(Duration::from_millis(300));
                bail!("blocked startup unexpectedly returned")
            },
        );
    }

    #[test]
    fn hard_cycle_watchdog_terminates_non_yielding_work() {
        let started = Instant::now();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("--nocapture")
            .arg("hard_cycle_watchdog_child")
            .env(HARD_WATCHDOG_TEST_ENV, "1")
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(HARD_CYCLE_WATCHDOG_EXIT_CODE));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "hard watchdog did not terminate the child promptly"
        );
    }

    #[test]
    fn startup_watchdog_exits_124_when_initialization_blocks() {
        let started = Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("--nocapture")
            .arg("startup_watchdog_child")
            .env(STARTUP_WATCHDOG_TEST_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();

        assert_eq!(status.code(), Some(HARD_CYCLE_WATCHDOG_EXIT_CODE));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "startup watchdog did not terminate the child promptly"
        );
    }

    #[test]
    fn hard_cycle_watchdog_exits_124_while_stderr_is_locked() {
        let started = Instant::now();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--ignored")
            .arg("--nocapture")
            .arg("hard_cycle_watchdog_child")
            .env(HARD_WATCHDOG_TEST_ENV, "1")
            .env(HARD_WATCHDOG_HOLD_STDERR_TEST_ENV, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();

        assert_eq!(status.code(), Some(HARD_CYCLE_WATCHDOG_EXIT_CODE));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "hard watchdog blocked on the process-wide stderr lock"
        );
    }

    #[test]
    fn hard_cycle_watchdog_disarms_without_waiting_for_deadline() {
        let started = Instant::now();
        {
            let _watchdog = CycleWatchdog::arm(Duration::from_secs(10)).unwrap();
        }
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn systemd_reference_resource_envelope_is_pinned() {
        const UNITS: [&str; 2] = [
            include_str!("../../../../deployment/aliyun/polymarket-reference-collector.service"),
            include_str!(
                "../../../../deployment/aliyun/polymarket-reference-collector-shadow@.service"
            ),
        ];

        fn memory_limit_mebibytes(unit: &str, property: &str) -> u64 {
            let prefix = format!("{property}=");
            let values = unit
                .lines()
                .filter_map(|line| line.strip_prefix(&prefix))
                .collect::<Vec<_>>();
            assert_eq!(values.len(), 1, "{property} must have one assignment");
            values[0].strip_suffix('M').unwrap().parse::<u64>().unwrap()
        }

        fn cpu_quota_percent(unit: &str) -> u64 {
            let values = unit
                .lines()
                .filter_map(|line| line.strip_prefix("CPUQuota="))
                .collect::<Vec<_>>();
            assert_eq!(values.len(), 1, "CPUQuota must have one assignment");
            values[0].strip_suffix('%').unwrap().parse::<u64>().unwrap()
        }

        for unit in UNITS {
            let high = memory_limit_mebibytes(unit, "MemoryHigh");
            let maximum = memory_limit_mebibytes(unit, "MemoryMax");
            let cpu_quota = cpu_quota_percent(unit);
            assert_eq!(high, 1536);
            assert_eq!(maximum, 2048);
            assert!(high < maximum);
            assert_eq!(cpu_quota, 80);
        }
    }

    fn market(question: &str, start: &str, end: &str) -> Value {
        json!({
            "id": "market-1",
            "conditionId": "condition-1",
            "question": question,
            "startDate": start,
            "endDate": end,
            "clobTokenIds": ["up-token", "down-token"],
        })
    }

    fn valid_trade(timestamp: i64) -> Value {
        json!({
            "transactionHash": "0xtx",
            "conditionId": "condition-1",
            "asset": "up-token",
            "side": "BUY",
            "timestamp": timestamp,
            "proxyWallet": "0xwallet",
            "size": "10.0",
            "price": "0.780",
            "outcome": "Up",
            "outcomeIndex": 0,
            "sourceOnlyField": {"preserved": true},
        })
    }

    fn valid_metadata_update(recorded_at: DateTime<Utc>) -> Value {
        json!({
            "kind": "market_metadata",
            "market_id": "market-1",
            "condition_id": "condition-1",
            "symbol": "BTCUSDT",
            "market_window_secs": 300,
            "source": "gamma_api",
            "retrieved_at": iso_z(recorded_at),
            "market": {
                "id": "market-1",
                "conditionId": "condition-1",
                "question": "Bitcoin Up or Down - 5 minutes",
                "startDate": iso_z(recorded_at),
                "endDate": iso_z(recorded_at + TimeDelta::minutes(5)),
                "clobTokenIds": ["up-token", "down-token"],
                "outcomes": ["Up", "Down"],
            },
        })
    }

    fn valid_trade_update(recorded_at: DateTime<Utc>, trade_timestamp: i64) -> Value {
        let config = ReferenceConfig {
            settlement_lookback_secs: recorded_at
                .timestamp()
                .saturating_sub(trade_timestamp)
                .max(0)
                + 1,
            ..ReferenceConfig::default()
        };
        let (mut updates, malformed) = trade_updates(
            &config,
            &mut CollectorState::default(),
            "market-1",
            "condition-1",
            "BTCUSDT",
            300,
            vec![valid_trade(trade_timestamp)],
            recorded_at,
            recorded_at,
        );
        assert!(malformed.is_empty());
        assert_eq!(updates.len(), 1);
        updates.pop().unwrap()
    }

    #[tokio::test]
    async fn collect_once_stamps_trade_batch_after_its_network_response() {
        use std::io::Read as _;
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

        let cycle_started_at = fixed_time("2026-07-18T00:00:00Z");
        let lookback_secs = 7_200;
        let retention_edge_at = cycle_started_at - TimeDelta::seconds(lookback_secs - 5);
        let trade_source_at = cycle_started_at + TimeDelta::seconds(5);
        let post_fetch_at = cycle_started_at + TimeDelta::seconds(10);
        let trade_response_started = Arc::new(AtomicBool::new(false));
        let server_trade_response_started = Arc::clone(&trade_response_started);
        let discovery = serde_json::to_vec(&json!({
            "markets": [market(
                "Bitcoin Up or Down - 5 minutes",
                "2026-07-18T00:00:00Z",
                "2026-07-18T00:05:00Z",
            )],
            "next_cursor": "",
        }))
        .unwrap();
        let trades = serde_json::to_vec(&json!([
            valid_trade(retention_edge_at.timestamp()),
            valid_trade(trade_source_at.timestamp()),
        ]))
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut requests = Vec::new();
            while requests.len() < 3 && Instant::now() < deadline {
                let (mut connection, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                };
                connection.set_nonblocking(false).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut chunk = [0_u8; 1_024];
                    let bytes = connection.read(&mut chunk).unwrap();
                    assert!(bytes > 0, "HTTP request ended before its headers");
                    request.extend_from_slice(&chunk[..bytes]);
                    assert!(
                        request.len() <= 16_384,
                        "HTTP request headers are unbounded"
                    );
                }
                let request = std::str::from_utf8(&request).unwrap();
                let request_line = request.lines().next().unwrap_or_default();
                let (kind, body) = if request_line.starts_with("GET /trades?") {
                    ("trade", &trades)
                } else if request_line.starts_with("GET /markets/keyset?") {
                    ("discovery", &discovery)
                } else {
                    panic!("unexpected test request: {request_line}");
                };
                requests.push(kind);
                if kind == "trade" {
                    server_trade_response_started.store(true, AtomicOrdering::SeqCst);
                }
                write!(
                    connection,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                connection.write_all(body).unwrap();
                connection.flush().unwrap();
            }
            assert_eq!(requests, ["discovery", "discovery", "trade"]);
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            symbols: vec!["BTCUSDT".to_owned()],
            market_lookback_secs: lookback_secs,
            ..ReferenceConfig::default()
        };
        let mut collector = ReferenceCollector::new(config).unwrap();
        let base_url = format!("http://{address}");
        collector.endpoints = ReferenceEndpoints {
            gamma_markets: format!("{base_url}/markets/keyset"),
            gamma_market: format!("{base_url}/markets"),
            data_trades: format!("{base_url}/trades"),
        };
        let clock_trade_response_started = Arc::clone(&trade_response_started);
        collector.clock = Box::new(move || {
            if clock_trade_response_started.load(AtomicOrdering::SeqCst) {
                post_fetch_at
            } else {
                cycle_started_at
            }
        });

        collector.collect_once().await.unwrap();
        server.join().unwrap();
        let rows = fs::read_to_string(temp.path().join(ACTIVE_TAPE)).unwrap();
        let trades = rows
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["update"]["kind"] == "polymarket_trade")
            .collect::<Vec<_>>();
        assert_eq!(
            trades.len(),
            2,
            "post-fetch arrival time must not move the cycle-start retention cutoff"
        );
        let trade = trades
            .iter()
            .find(|row| row["update"]["trade_ts_unix"] == trade_source_at.timestamp())
            .expect("cycle must archive the fetched trade after cycle start");

        assert!(trade_source_at > cycle_started_at);
        assert!(trade_source_at < post_fetch_at);
        assert_eq!(trade["update"]["received_at"], iso_z(post_fetch_at));
        assert_eq!(trade["recorded_at"], iso_z(post_fetch_at));
    }

    #[tokio::test]
    async fn collect_once_reports_historical_overdue_settlement_without_failing_cycle() {
        use std::io::Read as _;

        let now = fixed_time("2026-07-21T03:00:00Z");
        let discovery = serde_json::to_vec(&json!({
            "markets": [market(
                "Bitcoin Up or Down - 5 minutes",
                "2026-07-20T00:00:00Z",
                "2026-07-20T00:05:00Z",
            )],
            "next_cursor": "",
        }))
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut connection, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2_048];
                let _ = connection.read(&mut request).unwrap();
                write!(
                    connection,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    discovery.len()
                )
                .unwrap();
                connection.write_all(&discovery).unwrap();
            }
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };
        let mut collector = ReferenceCollector::new(config).unwrap();
        collector.state.markets.insert(
            "market-1".to_owned(),
            TrackedMarket {
                condition_id: Some("condition-1".to_owned()),
                symbol: Some("BTCUSDT".to_owned()),
                market_window_secs: 300,
                end_time: Some("2026-07-20T00:05:00Z".to_owned()),
                trade_complete: true,
                ..TrackedMarket::default()
            },
        );
        let base_url = format!("http://{address}");
        collector.endpoints = ReferenceEndpoints {
            gamma_markets: format!("{base_url}/markets/keyset"),
            gamma_market: format!("{base_url}/markets"),
            data_trades: format!("{base_url}/trades"),
        };
        collector.clock = Box::new(move || now);

        let health = collector.collect_once().await.unwrap();

        server.join().unwrap();
        assert_eq!(health["overdue_unresolved_markets"], json!(["market-1"]));
        assert_eq!(health["api_errors"], json!([]));
    }

    #[tokio::test]
    async fn transient_http_failure_is_retried_before_health_error() {
        use std::io::Read as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = first.read(&mut request).unwrap();
            first
                .write_all(
                    b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .unwrap();

            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(750);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut second, _)) => {
                        second.set_nonblocking(false).unwrap();
                        let _ = second.read(&mut request).unwrap();
                        second
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}",
                            )
                            .unwrap();
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
            false
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            http_timeout: Duration::from_secs(1),
            ..ReferenceConfig::default()
        };
        let collector = ReferenceCollector::new(config).unwrap();
        let result = collector
            .get_json(&format!("http://{address}/market"), &[])
            .await;
        let retried = server.join().unwrap();

        assert!(retried, "collector did not retry the transient response");
        assert_eq!(result.unwrap(), json!({"ok": true}));
    }

    #[tokio::test]
    async fn concurrent_trade_retries_preserve_global_request_spacing() {
        use std::io::Read as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut accepted = 0;
            while accepted < 4 && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut request = [0_u8; 2048];
                        let _ = stream.read(&mut request).unwrap();
                        accepted += 1;
                        let response = if accepted <= 2 {
                            b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".as_slice()
                        } else {
                            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}".as_slice()
                        };
                        stream.write_all(response).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
            accepted
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            http_timeout: Duration::from_secs(1),
            per_market_delay: Duration::from_millis(200),
            ..ReferenceConfig::default()
        };
        let collector = ReferenceCollector::new(config).unwrap();
        let url = format!("http://{address}/trades");
        let (left, right) = tokio::join!(
            collector.get_json_rate_limited(&url, &[]),
            collector.get_json_rate_limited(&url, &[])
        );
        let accepted = server.join().unwrap();
        let started_at = collector
            .trade_request_attempts_started_at
            .lock()
            .unwrap()
            .clone();

        assert_eq!(left.unwrap(), json!({"ok": true}));
        assert_eq!(right.unwrap(), json!({"ok": true}));
        assert_eq!(accepted, 4);
        assert_eq!(started_at.len(), 4);
        assert!(started_at
            .windows(2)
            .all(|times| { times[1].duration_since(times[0]) >= Duration::from_millis(150) }));
    }

    #[test]
    fn http_retry_policy_is_bounded_and_fail_closed() {
        let now = fixed_time("2015-10-21T07:27:59Z");
        assert!(retryable_http_status(
            reqwest::StatusCode::TOO_MANY_REQUESTS
        ));
        assert!(retryable_http_status(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(!retryable_http_status(reqwest::StatusCode::BAD_REQUEST));
        assert_eq!(
            http_retry_delay(None, 0, now),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            http_retry_delay(None, 1, now),
            Some(Duration::from_millis(500))
        );
        assert_eq!(
            http_retry_delay(Some("2"), 0, now),
            Some(Duration::from_secs(2))
        );
        assert_eq!(http_retry_delay(Some("60"), 0, now), None);
        assert_eq!(
            http_retry_delay(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 0, now),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            http_retry_delay(Some("Wed, 21 Oct 2015 07:29:00 GMT"), 0, now),
            None
        );
        assert_eq!(http_retry_delay(Some("not-a-delay"), 0, now), None);
    }

    #[tokio::test]
    async fn unhonorable_retry_after_fails_without_an_early_retry() {
        use std::io::Read as _;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let server_attempts = Arc::clone(&attempts);
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(750);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut request = [0_u8; 2048];
                        let _ = stream.read(&mut request).unwrap();
                        server_attempts.fetch_add(1, Ordering::SeqCst);
                        stream
                            .write_all(
                                b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                            )
                            .unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            http_timeout: Duration::from_secs(1),
            ..ReferenceConfig::default()
        };
        let collector = ReferenceCollector::new(config).unwrap();
        let result = collector
            .get_json(&format!("http://{address}/market"), &[])
            .await;
        server.join().unwrap();

        assert!(result.is_err(), "unhonorable Retry-After must fail closed");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhausted_http_retries_still_fail_closed() {
        use std::io::Read as _;
        use std::sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        };

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server_attempts = Arc::clone(&attempts);
        let server_stop = Arc::clone(&stop);
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !server_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut request = [0_u8; 2048];
                        let _ = stream.read(&mut request).unwrap();
                        server_attempts.fetch_add(1, Ordering::SeqCst);
                        stream
                            .write_all(
                                b"HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                            )
                            .unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            http_timeout: Duration::from_secs(1),
            ..ReferenceConfig::default()
        };
        let collector = ReferenceCollector::new(config).unwrap();
        let result = collector
            .get_json(&format!("http://{address}/market"), &[])
            .await;
        stop.store(true, Ordering::SeqCst);
        server.join().unwrap();

        assert!(result.is_err(), "exhausted retries must remain an error");
        assert_eq!(attempts.load(Ordering::SeqCst), HTTP_GET_ATTEMPTS);
    }

    #[test]
    fn targets_all_configured_symbols_and_only_five_or_fifteen_minute_markets() {
        let symbols = SYMBOL_ALIASES
            .iter()
            .map(|(symbol, _)| (*symbol).to_owned())
            .collect::<BTreeSet<_>>();
        for (symbol, aliases) in SYMBOL_ALIASES {
            let five = market(
                &format!("Will {} go up?", aliases[0]),
                "2026-07-15T00:00:00Z",
                "2026-07-15T00:05:00Z",
            );
            assert_eq!(target_market(&five, &symbols).unwrap().symbol, symbol);
            let fifteen = market(
                &format!("Will {} go up?", aliases[0]),
                "2026-07-15T00:00:00Z",
                "2026-07-15T00:15:00Z",
            );
            assert_eq!(target_market(&fifteen, &symbols).unwrap().window_secs, 900);
        }
        assert!(target_market(
            &market(
                "Will Bitcoin go up?",
                "2026-07-15T00:00:00Z",
                "2026-07-15T00:10:00Z",
            ),
            &symbols,
        )
        .is_none());
        let misleading = json!({
            "id": "market-2",
            "conditionId": "condition-2",
            "question": "Will Bitcoin go up?",
            "slug": "bitcoin-115ms-sample",
            "clobTokenIds": ["up", "down"],
        });
        assert!(target_market(&misleading, &symbols).is_none());
    }

    #[test]
    fn missing_target_symbols_reports_every_configured_gap() {
        let configured = ["BTCUSDT", "ETHUSDT", "SOLUSDT"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let discovered = ["ETHUSDT"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            missing_symbols(&configured, &discovered),
            vec!["BTCUSDT", "SOLUSDT"]
        );
        assert!(missing_symbols(&configured, &configured).is_empty());
    }

    #[test]
    fn settlement_requires_closed_market_and_single_near_certain_winner() {
        let mut resolved = json!({
            "id": "market-1",
            "conditionId": "condition-1",
            "closed": true,
            "outcomes": ["Up", "Down"],
            "clobTokenIds": ["up-token", "down-token"],
            "outcomePrices": ["0.999", "0.001"],
        });
        let settlement = settlement_from_market(&resolved, "BTCUSDT", 300, "now")
            .unwrap()
            .unwrap();
        assert_eq!(settlement["winning_token_id"], "up-token");
        assert_eq!(settlement["resolved_up_won"], true);

        resolved["closed"] = json!(false);
        assert!(settlement_from_market(&resolved, "BTCUSDT", 300, "now")
            .unwrap()
            .is_none());
        resolved["closed"] = json!(true);
        resolved["outcomePrices"] = json!(["0.9989", "0.0011"]);
        assert!(settlement_from_market(&resolved, "BTCUSDT", 300, "now").is_err());
        resolved["outcomePrices"] = json!(["1", "1"]);
        assert!(settlement_from_market(&resolved, "BTCUSDT", 300, "now").is_err());
        resolved["outcomePrices"] = json!(["0.999", "0.5"]);
        assert!(settlement_from_market(&resolved, "BTCUSDT", 300, "now").is_err());
        resolved["outcomePrices"] = json!(["1", "0.001"]);
        assert!(settlement_from_market(&resolved, "BTCUSDT", 300, "now").is_err());
        resolved["outcomePrices"] = json!(["1", "0"]);
        resolved["outcomes"] = json!(["Maybe", "Never"]);
        assert!(settlement_from_market(&resolved, "BTCUSDT", 300, "now").is_err());
    }

    #[test]
    fn only_unresolved_markets_become_overdue() {
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut tracked = TrackedMarket {
            end_time: Some("2026-07-15T00:00:00Z".to_owned()),
            ..TrackedMarket::default()
        };
        assert!(settlement_is_overdue(&tracked, now, 3_600).unwrap());
        tracked.settled = true;
        assert!(!settlement_is_overdue(&tracked, now, 3_600).unwrap());
    }

    #[test]
    fn missing_or_invalid_market_end_time_is_a_completeness_error() {
        let now = fixed_time("2026-07-15T02:00:00Z");
        for end_time in [None, Some("not-a-time".to_owned())] {
            let tracked = TrackedMarket {
                end_time,
                settled: true,
                ..TrackedMarket::default()
            };
            let error = settlement_is_overdue(&tracked, now, 3_600).unwrap_err();
            assert!(error.downcast_ref::<DataCompletenessError>().is_some());
        }
    }

    #[test]
    fn gamma_cursor_pages_are_exhausted_or_fail_at_the_configured_cap() {
        let symbols = BTreeSet::from(["BTCUSDT".to_owned()]);
        let mut discovery = GammaLaneDiscovery::default();
        let cursor = append_gamma_page(
            &mut discovery,
            &json!({"markets": [{"id": "one"}], "next_cursor": "page-2"}),
            &symbols,
            2,
        )
        .unwrap();
        assert_eq!(cursor.as_deref(), Some("page-2"));
        assert!(append_gamma_page(
            &mut discovery,
            &json!({"markets": [{"id": "two"}], "next_cursor": ""}),
            &symbols,
            2,
        )
        .unwrap()
        .is_none());
        assert_eq!(discovery.seen, 2);
        assert!(discovery.targets.is_empty());

        let mut capped = GammaLaneDiscovery::default();
        let error = append_gamma_page(
            &mut capped,
            &json!({"markets": [{"id": "one"}], "next_cursor": "page-2"}),
            &symbols,
            1,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<DataCompletenessError>().is_some());
    }

    #[test]
    fn gamma_discovery_drops_non_targets_after_validating_and_counting_them() {
        let symbols = BTreeSet::from(["BTCUSDT".to_owned()]);
        let target = market(
            "Will Bitcoin go up?",
            "2026-07-15T00:00:00Z",
            "2026-07-15T00:05:00Z",
        );
        let mut discovery = GammaDiscovery::default();

        let cursor = discovery
            .append_page(
                GammaLane::Open,
                &json!({
                    "markets": [{"id": "other"}, target],
                    "next_cursor": "page-2",
                }),
                &symbols,
                3,
            )
            .unwrap();
        assert_eq!(cursor.as_deref(), Some("page-2"));

        let error = discovery
            .append_page(
                GammaLane::Open,
                &json!({
                    "markets": [{"id": "another-other"}],
                    "next_cursor": "page-3",
                }),
                &symbols,
                3,
            )
            .unwrap_err();
        assert!(error.downcast_ref::<DataCompletenessError>().is_some());

        assert!(discovery
            .append_page(
                GammaLane::Closed,
                &json!({"markets": [{"id": "other"}, null]}),
                &symbols,
                3,
            )
            .is_err());

        let ids = discovery
            .into_markets()
            .into_iter()
            .map(|market| market["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["market-1"]);
    }

    #[test]
    fn open_and_closed_discovery_lanes_have_independent_caps() {
        let symbols = BTreeSet::from(["BTCUSDT".to_owned()]);
        let mut open = market(
            "Will Bitcoin go up?",
            "2026-07-15T00:00:00Z",
            "2026-07-15T00:05:00Z",
        );
        open["id"] = json!("open");
        let mut closed = open.clone();
        closed["id"] = json!("closed");
        let mut discovery = GammaDiscovery::default();
        assert!(discovery
            .append_page(
                GammaLane::Open,
                &json!({"markets": [open], "next_cursor": ""}),
                &symbols,
                1,
            )
            .unwrap()
            .is_none());
        assert!(discovery
            .append_page(
                GammaLane::Closed,
                &json!({"markets": [closed], "next_cursor": ""}),
                &symbols,
                1,
            )
            .unwrap()
            .is_none());

        let ids = discovery
            .into_markets()
            .into_iter()
            .map(|market| market["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["open", "closed"]);
    }

    #[test]
    fn settlement_discovery_capacity_covers_every_configured_five_and_fifteen_minute_market() {
        let default = ReferenceConfig::default();
        assert_eq!(default.max_markets, 10_000);
        assert_eq!(default.max_trade_polls_per_cycle, 112);
        assert_eq!(
            default.max_trade_polls_per_cycle,
            DEFAULT_MAX_TRADE_POLLS_PER_CYCLE
        );
        assert_eq!(
            default.max_concurrent_trade_polls,
            DEFAULT_MAX_CONCURRENT_TRADE_POLLS
        );
        default.validate().unwrap();

        let malformed_market_id = ReferenceConfig {
            market_ids: BTreeSet::from([" 2959141".to_owned()]),
            ..default.clone()
        };
        let error = malformed_market_id.validate().unwrap_err();
        assert!(error.to_string().contains("market IDs"));

        let excessive_requested_market_ids = ReferenceConfig {
            market_ids: BTreeSet::from(["market-1".to_owned(), "market-2".to_owned()]),
            max_markets: 1,
            ..default.clone()
        };
        let error = excessive_requested_market_ids.validate().unwrap_err();
        assert!(error.to_string().contains("requested market IDs"));

        let exact_capacity = ReferenceConfig {
            max_markets: 2_688,
            ..default.clone()
        };
        exact_capacity.validate().unwrap();

        let undersized = ReferenceConfig {
            max_markets: 2_687,
            ..default.clone()
        };
        let error = undersized.validate().unwrap_err();
        assert!(error.to_string().contains("max_markets"));

        let short_settlement_window = ReferenceConfig {
            settlement_lookback_secs: 86_399,
            ..default
        };
        let error = short_settlement_window.validate().unwrap_err();
        assert!(error.to_string().contains("settlement lookback"));

        let zero_trade_budget = ReferenceConfig {
            max_trade_polls_per_cycle: 0,
            ..ReferenceConfig::default()
        };
        assert!(zero_trade_budget.validate().is_err());

        let zero_concurrency = ReferenceConfig {
            max_concurrent_trade_polls: 0,
            ..ReferenceConfig::default()
        };
        assert!(zero_concurrency.validate().is_err());

        let concurrency_above_budget = ReferenceConfig {
            max_trade_polls_per_cycle: 3,
            max_concurrent_trade_polls: 4,
            ..ReferenceConfig::default()
        };
        assert!(concurrency_above_budget.validate().is_err());

        let concurrency_above_hard_limit = ReferenceConfig {
            max_concurrent_trade_polls: DEFAULT_MAX_CONCURRENT_TRADE_POLLS + 1,
            ..ReferenceConfig::default()
        };
        let error = concurrency_above_hard_limit.validate().unwrap_err();
        assert!(error.to_string().contains("hard limit"));

        let insufficient_request_spacing = ReferenceConfig {
            per_market_delay: MIN_TRADE_REQUEST_SPACING - Duration::from_millis(1),
            ..ReferenceConfig::default()
        };
        let error = insufficient_request_spacing.validate().unwrap_err();
        assert!(error.to_string().contains("request spacing"));
    }

    #[test]
    fn trade_poll_plan_prioritizes_recent_markets_and_rotates_historical_backfill() {
        let old_success = fixed_time("2026-07-15T00:00:00Z");
        let new_success = fixed_time("2026-07-15T01:00:00Z");
        let candidates = vec![
            TradePollCandidate {
                market_id: "recent-never-polled".to_owned(),
                priority: true,
                last_success_at: None,
                end_time: Some(fixed_time("2026-07-15T02:00:00Z")),
            },
            TradePollCandidate {
                market_id: "recent-polled".to_owned(),
                priority: true,
                last_success_at: Some(new_success),
                end_time: Some(fixed_time("2026-07-15T02:05:00Z")),
            },
            TradePollCandidate {
                market_id: "historical-never-polled".to_owned(),
                priority: false,
                last_success_at: None,
                end_time: Some(fixed_time("2026-07-14T23:00:00Z")),
            },
            TradePollCandidate {
                market_id: "historical-oldest-success".to_owned(),
                priority: false,
                last_success_at: Some(old_success),
                end_time: Some(fixed_time("2026-07-14T23:05:00Z")),
            },
        ];

        let first = plan_trade_polls(candidates.clone(), 3);
        assert_eq!(first.eligible, 4);
        assert_eq!(first.priority, 2);
        assert_eq!(first.deferred, 1);
        assert_eq!(first.priority_deferred, 0);
        assert_eq!(
            first.selected,
            BTreeSet::from([
                "historical-never-polled".to_owned(),
                "recent-never-polled".to_owned(),
                "recent-polled".to_owned(),
            ])
        );

        let mut rotated = candidates;
        rotated
            .iter_mut()
            .find(|candidate| candidate.market_id == "historical-never-polled")
            .unwrap()
            .last_success_at = Some(new_success);
        let second = plan_trade_polls(rotated, 3);
        assert!(second.selected.contains("historical-oldest-success"));
        assert!(!second.selected.contains("historical-never-polled"));
    }

    #[test]
    fn trade_poll_plan_reports_priority_overflow_fail_closed() {
        let candidates = (0..3)
            .map(|index| TradePollCandidate {
                market_id: format!("recent-{index}"),
                priority: true,
                last_success_at: None,
                end_time: None,
            })
            .collect();
        let plan = plan_trade_polls(candidates, 2);
        assert_eq!(plan.priority_deferred, 1);
        assert_eq!(plan.deferred, 1);
    }

    #[test]
    fn stale_market_detail_recovery_is_bounded_and_rotates_backlog() {
        let now = fixed_time("2026-07-17T05:00:00Z");
        let tracked = |end_time: &str| TrackedMarket {
            end_time: Some(end_time.to_owned()),
            ..TrackedMarket::default()
        };
        let mut markets = BTreeMap::from([
            ("oldest".to_owned(), tracked("2026-07-16T00:00:00Z")),
            ("middle".to_owned(), tracked("2026-07-16T01:00:00Z")),
            ("newest".to_owned(), tracked("2026-07-16T02:00:00Z")),
            (
                "complete".to_owned(),
                TrackedMarket {
                    settled: true,
                    trade_complete: true,
                    ..tracked("2026-07-16T03:00:00Z")
                },
            ),
            ("future".to_owned(), tracked("2026-07-17T06:00:00Z")),
        ]);
        let targets = BTreeSet::from(["current-target".to_owned()]);

        let first = plan_market_detail_fetches(&markets, &targets, now, 2);
        assert_eq!(first.eligible, 3);
        assert_eq!(first.priority, 3);
        assert_eq!(
            first.selected,
            BTreeSet::from(["middle".to_owned(), "newest".to_owned()])
        );

        for market_id in &first.selected {
            markets
                .get_mut(market_id)
                .unwrap()
                .last_market_detail_attempt_at = Some(iso_z(now));
        }
        let second = plan_market_detail_fetches(&markets, &targets, now, 2);
        assert!(second.selected.contains("oldest"));
    }

    #[test]
    fn settled_trade_incomplete_state_requires_authoritative_market_detail() {
        let now = fixed_time("2026-07-17T05:00:00Z");
        let tracked = TrackedMarket {
            condition_id: Some("condition-1".to_owned()),
            symbol: Some("BTCUSDT".to_owned()),
            market_window_secs: 300,
            end_time: Some("2026-07-16T00:05:00Z".to_owned()),
            settled: true,
            trade_complete: false,
            ..TrackedMarket::default()
        };
        let markets = BTreeMap::from([("settled".to_owned(), tracked)]);
        assert_eq!(
            plan_market_detail_fetches(&markets, &BTreeSet::new(), now, 8).selected,
            BTreeSet::from(["settled".to_owned()])
        );
    }

    #[test]
    fn recovered_market_detail_must_match_the_requested_target() {
        let symbols = BTreeSet::from(["BTCUSDT".to_owned()]);
        let valid = market(
            "Bitcoin Up or Down - 5 minutes",
            "2026-07-17T05:00:00Z",
            "2026-07-17T05:05:00Z",
        );
        let market_id = valid["id"].as_str().unwrap();
        assert!(recovered_target(market_id, &valid, &symbols).is_ok());
        assert!(recovered_target("wrong-market", &valid, &symbols).is_err());

        let unsupported = market(
            "Ethereum Up or Down - 5 minutes",
            "2026-07-17T05:00:00Z",
            "2026-07-17T05:05:00Z",
        );
        let unsupported_id = unsupported["id"].as_str().unwrap();
        assert!(recovered_target(unsupported_id, &unsupported, &symbols).is_err());
    }

    #[test]
    fn state_bounds_fail_closed_before_persisting_more_incomplete_work() {
        let mut state = CollectorState::default();
        state.markets.insert("market-1".to_owned(), TrackedMarket::default());
        state.markets.insert("market-2".to_owned(), TrackedMarket::default());
        assert!(validate_state_bounds(&state, 1, 10).is_err());

        state.markets.pop_last();
        state.trade_seen.insert(
            "condition-1".to_owned(),
            BTreeMap::from([("trade-1".to_owned(), 1), ("trade-2".to_owned(), 2)]),
        );
        assert!(validate_state_bounds(&state, 1, 1).is_err());
        assert!(validate_state_bounds(&state, 1, 2).is_ok());
    }

    #[test]
    fn eviction_drops_oldest_incomplete_ids_before_the_cap() {
        let tracked = |condition_id: &str, trade_complete: bool| TrackedMarket {
            condition_id: Some(condition_id.to_owned()),
            trade_complete,
            ..TrackedMarket::default()
        };
        let mut state = CollectorState::default();
        state.markets.insert("m-inc".to_owned(), tracked("c-inc", false));
        state.markets.insert("m-done".to_owned(), tracked("c-done", true));
        state.trade_seen.insert(
            "c-inc".to_owned(),
            BTreeMap::from([
                ("old-1".to_owned(), 1),
                ("old-2".to_owned(), 2),
                ("new-1".to_owned(), 100),
            ]),
        );
        state.trade_seen.insert(
            "c-done".to_owned(),
            BTreeMap::from([("done-1".to_owned(), 3), ("done-2".to_owned(), 4)]),
        );

        let evicted = evict_oldest_incomplete_trade_ids(&mut state, 3);

        assert_eq!(evicted, 2);
        let remaining: Vec<&str> = state.trade_seen["c-inc"].keys().map(String::as_str).collect();
        assert_eq!(remaining, ["new-1"]);
        assert_eq!(state.trade_seen["c-done"].len(), 2);
        assert!(validate_state_bounds(&state, 10, 3).is_ok());
    }

    #[test]
    fn eviction_removes_emptied_conditions_and_is_noop_under_budget() {
        let mut state = CollectorState::default();
        state.markets.insert(
            "m-inc".to_owned(),
            TrackedMarket {
                condition_id: Some("c-inc".to_owned()),
                ..TrackedMarket::default()
            },
        );
        state.trade_seen.insert(
            "c-inc".to_owned(),
            BTreeMap::from([("only".to_owned(), 1)]),
        );

        assert_eq!(evict_oldest_incomplete_trade_ids(&mut state, 10), 0);
        assert!(state.trade_seen.contains_key("c-inc"));
        assert_eq!(evict_oldest_incomplete_trade_ids(&mut state, 0), 1);
        assert!(!state.trade_seen.contains_key("c-inc"));
    }

    #[test]
    fn eviction_cannot_hide_a_complete_market_overflow() {
        let mut state = CollectorState::default();
        state.markets.insert(
            "m-done".to_owned(),
            TrackedMarket {
                condition_id: Some("c-done".to_owned()),
                trade_complete: true,
                ..TrackedMarket::default()
            },
        );
        state.trade_seen.insert(
            "c-done".to_owned(),
            BTreeMap::from([("t-1".to_owned(), 1), ("t-2".to_owned(), 2)]),
        );

        // Complete-market IDs are never evicted, so the fail-closed bound
        // still fires for accumulation eviction cannot cover.
        assert_eq!(evict_oldest_incomplete_trade_ids(&mut state, 1), 0);
        assert!(validate_state_bounds(&state, 10, 1).is_err());
    }

    #[test]
    fn cycle_crossing_an_hour_requires_a_fresh_context_retry() {
        let started = fixed_time("2026-07-17T05:59:59Z");
        assert!(validate_cycle_hour(&hour_key(started), started).is_ok());
        assert!(validate_cycle_hour(
            &hour_key(started),
            fixed_time("2026-07-17T06:00:00Z")
        )
        .is_err());
    }

    #[test]
    fn stale_recovery_preserves_priority_trades_within_one_request_budget() {
        let now = fixed_time("2026-07-17T05:00:00Z");
        let tracked = |market_id: &str| {
            (
                market_id.to_owned(),
                TrackedMarket {
                    settled: false,
                    trade_complete: false,
                    end_time: Some("2026-07-16T00:00:00Z".to_owned()),
                    ..TrackedMarket::default()
                },
            )
        };
        let markets = BTreeMap::from([tracked("stale-a"), tracked("stale-b"), tracked("stale-c")]);
        let priority_trades_before_details = 2;
        let detail_budget = shared_market_detail_budget(4, 2, priority_trades_before_details);
        let detail_plan =
            plan_market_detail_fetches(&markets, &BTreeSet::new(), now, detail_budget);
        let trade_plan = plan_trade_polls(
            (0..priority_trades_before_details + detail_plan.selected.len())
                .map(|index| TradePollCandidate {
                    market_id: format!("trade-{index}"),
                    priority: true,
                    last_success_at: None,
                    end_time: None,
                })
                .collect(),
            remaining_trade_poll_budget(4, detail_plan.selected.len()),
        );

        assert_eq!(detail_budget, 1);
        assert_eq!(detail_plan.selected.len(), 1);
        assert_eq!(detail_plan.deferred, 2);
        assert_eq!(trade_plan.selected.len(), 3);
        assert_eq!(trade_plan.priority_deferred, 0);
        assert_eq!(detail_plan.selected.len() + trade_plan.selected.len(), 4);
        assert_eq!(shared_market_detail_budget(4, 2, 4), 0);
    }

    #[test]
    fn target_chunks_preserve_order_and_bound_retained_selected_results() {
        let target_ids = ["a", "b", "c", "d", "e", "f", "g", "h", "i"].map(str::to_owned);
        let selected = BTreeSet::from([
            "b".to_owned(),
            "c".to_owned(),
            "e".to_owned(),
            "g".to_owned(),
            "h".to_owned(),
        ]);
        let chunks = target_processing_chunks(target_ids.iter(), &selected, 2);
        assert_eq!(
            chunks
                .iter()
                .flatten()
                .map(|market_id| market_id.as_str())
                .collect::<Vec<_>>(),
            target_ids.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(chunks.iter().all(|chunk| {
            chunk
                .iter()
                .filter(|market_id| selected.contains(**market_id))
                .count()
                <= 2
        }));
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| {
                    chunk
                        .iter()
                        .filter(|market_id| selected.contains(**market_id))
                        .count()
                })
                .collect::<Vec<_>>(),
            [2, 2, 1]
        );
    }

    #[test]
    fn closed_discovery_uses_the_24_hour_settlement_lookback() {
        let now = fixed_time("2026-07-15T02:00:00Z");
        let config = ReferenceConfig {
            market_lookback_secs: 7_200,
            settlement_lookback_secs: 86_400,
            ..ReferenceConfig::default()
        };
        let open = gamma_discovery_params(&config, now, false)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let closed = gamma_discovery_params(&config, now, true)
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(open["closed"], "false");
        assert_eq!(closed["closed"], "true");
        assert_eq!(open["tag_id"], "21");
        assert_eq!(closed["tag_id"], "21");
        assert_eq!(open["related_tags"], "false");
        assert_eq!(closed["related_tags"], "false");
        assert_eq!(open["end_date_min"], "2026-07-15T00:00:00.000000Z");
        assert_eq!(closed["end_date_min"], "2026-07-14T02:00:00.000000Z");
    }

    #[tokio::test]
    async fn requested_market_ids_bypass_broad_gamma_discovery() {
        use std::io::Read as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut requested = Vec::new();
            while requested.len() < 2 && Instant::now() < deadline {
                let (mut connection, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                };
                connection.set_nonblocking(false).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut chunk = [0_u8; 1_024];
                    let bytes = connection.read(&mut chunk).unwrap();
                    assert!(bytes > 0, "HTTP request ended before its headers");
                    request.extend_from_slice(&chunk[..bytes]);
                }
                let request = std::str::from_utf8(&request).unwrap();
                let request_line = request.lines().next().unwrap_or_default();
                let market_id = request_line
                    .strip_prefix("GET /markets/")
                    .and_then(|path| path.split_once(" HTTP/"))
                    .map(|(market_id, _)| market_id)
                    .expect("explicit market IDs must not request the Gamma keyset");
                requested.push(market_id.to_owned());
                let body = serde_json::to_vec(&json!({
                    "id": market_id,
                    "conditionId": format!("condition-{market_id}"),
                    "question": "Bitcoin Up or Down - 5 minutes",
                    "startDate": "2026-07-20T08:00:00Z",
                    "endDate": "2026-07-20T08:05:00Z",
                    "clobTokenIds": ["up-token", "down-token"],
                }))
                .unwrap();
                write!(
                    connection,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                connection.write_all(&body).unwrap();
                connection.flush().unwrap();
            }
            requested
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            symbols: vec!["BTCUSDT".to_owned()],
            market_ids: BTreeSet::from(["bounded-1".to_owned(), "bounded-2".to_owned()]),
            ..ReferenceConfig::default()
        };
        let mut collector = ReferenceCollector::new(config).unwrap();
        let base_url = format!("http://{address}");
        collector.endpoints = ReferenceEndpoints {
            gamma_markets: format!("{base_url}/markets/keyset"),
            gamma_market: format!("{base_url}/markets"),
            data_trades: format!("{base_url}/trades"),
        };

        let markets = collector
            .discover_markets(fixed_time("2026-07-20T09:00:00Z"))
            .await
            .unwrap();

        assert_eq!(
            markets
                .iter()
                .filter_map(|market| market.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["bounded-1".to_owned(), "bounded-2".to_owned()])
        );
        assert_eq!(server.join().unwrap(), ["bounded-1", "bounded-2"]);
    }

    #[tokio::test]
    async fn requested_non_target_market_fails_before_completion() {
        use std::io::Read as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut connection, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 1_024];
                let bytes = connection.read(&mut chunk).unwrap();
                assert!(bytes > 0, "HTTP request ended before its headers");
                request.extend_from_slice(&chunk[..bytes]);
            }
            assert!(std::str::from_utf8(&request)
                .unwrap()
                .starts_with("GET /markets/unrelated-market HTTP/"));
            let body = serde_json::to_vec(&json!({
                "id": "unrelated-market",
                "conditionId": "unrelated-condition",
                "question": "Will it rain tomorrow?",
                "startDate": "2026-07-20T08:00:00Z",
                "endDate": "2026-07-20T08:05:00Z",
                "clobTokenIds": ["up-token", "down-token"],
            }))
            .unwrap();
            write!(
                connection,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            connection.write_all(&body).unwrap();
            connection.flush().unwrap();
        });

        let temp = TestDir::new();
        let config = ReferenceConfig {
            spool_dir: temp.path().to_owned(),
            symbols: vec!["BTCUSDT".to_owned()],
            market_ids: BTreeSet::from(["unrelated-market".to_owned()]),
            ..ReferenceConfig::default()
        };
        let mut collector = ReferenceCollector::new(config).unwrap();
        let base_url = format!("http://{address}");
        collector.endpoints = ReferenceEndpoints {
            gamma_markets: format!("{base_url}/markets/keyset"),
            gamma_market: format!("{base_url}/markets"),
            data_trades: format!("{base_url}/trades"),
        };

        let error = collector.collect_once().await.unwrap_err();

        assert!(error.to_string().contains("unrelated-market"));
        assert!(!fs::read_to_string(temp.path().join(ACTIVE_TAPE))
            .unwrap()
            .contains(TRADE_COMPLETION_KIND));
        server.join().unwrap();
    }

    #[test]
    fn requested_market_ids_fail_closed_when_discovery_is_incomplete() {
        let requested = BTreeSet::from(["2959141".to_owned(), "2959146".to_owned()]);
        let discovered = BTreeSet::from(["2959141".to_owned()]);

        let error = validate_requested_market_ids(&requested, &discovered)
            .expect_err("a requested market missing from Gamma discovery must fail");

        assert!(error.to_string().contains("2959146"));
    }

    #[test]
    fn market_allowlist_prunes_unrelated_recovered_state() {
        let mut state = CollectorState::default();
        state.markets.insert(
            "2959141".to_owned(),
            TrackedMarket {
                condition_id: Some("allowed-condition".to_owned()),
                ..TrackedMarket::default()
            },
        );
        state.markets.insert(
            "unrelated".to_owned(),
            TrackedMarket {
                condition_id: Some("unrelated-condition".to_owned()),
                ..TrackedMarket::default()
            },
        );
        state.trade_seen.insert("allowed-condition".to_owned(), BTreeMap::new());
        state.trade_seen.insert("unrelated-condition".to_owned(), BTreeMap::new());

        assert!(retain_requested_market_state(
            &mut state,
            &BTreeSet::from(["2959141".to_owned()]),
        ));
        assert_eq!(state.markets.len(), 1);
        assert!(state.markets.contains_key("2959141"));
        assert_eq!(state.trade_seen.len(), 1);
        assert!(state.trade_seen.contains_key("allowed-condition"));
    }

    #[cfg(unix)]
    #[test]
    fn collector_rejects_symlinked_or_noncanonical_spool_ancestors() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let linked = root.path().join("linked");
        symlink(&actual, &linked).unwrap();
        for spool_dir in [
            linked.join("spool"),
            actual.join("child").join("..").join("child"),
        ] {
            let config = ReferenceConfig {
                spool_dir,
                symbols: vec!["BTCUSDT".to_owned()],
                ..ReferenceConfig::default()
            };
            assert!(ReferenceCollector::new(config)
                .err()
                .expect("indirect spool must fail")
                .to_string()
                .contains("spool"));
        }
    }

    #[test]
    fn gamma_and_trade_pages_reject_schema_loss() {
        let symbols = BTreeSet::from(["BTCUSDT".to_owned()]);
        let mut discovery = GammaLaneDiscovery::default();
        assert!(append_gamma_page(
            &mut discovery,
            &json!({"markets": [{"id": "one"}, null]}),
            &symbols,
            10,
        )
        .is_err());

        let (objects, rejected) = object_rows(vec![json!({"id": "trade"}), Value::Null]);
        assert_eq!(objects.len(), 1);
        assert_eq!(rejected, 1);
    }

    #[test]
    fn unchanged_metadata_is_reseeded_for_a_new_hour() {
        let market = market(
            "Will Bitcoin go up?",
            "2026-07-15T00:00:00Z",
            "2026-07-15T00:05:00Z",
        );
        let target = TargetMarket {
            symbol: "BTCUSDT".to_owned(),
            window_secs: 300,
        };
        let mut tracked = TrackedMarket::default();
        assert!(
            market_metadata_update("market-1", &market, &target, "first", false, &mut tracked,)
                .unwrap()
                .is_some()
        );
        assert!(market_metadata_update(
            "market-1",
            &market,
            &target,
            "same-hour",
            false,
            &mut tracked,
        )
        .unwrap()
        .is_none());
        assert!(market_metadata_update(
            "market-1",
            &market,
            &target,
            "next-hour",
            true,
            &mut tracked,
        )
        .unwrap()
        .is_some());
    }

    #[test]
    fn trade_id_matches_v2_golden_vector() {
        let mut trade = valid_trade(1_784_084_995);
        trade["conditionId"] = json!("0xcondition");
        assert_eq!(
            stable_trade_id(&trade),
            "6a476c3be58fcb8d789224feba3e079f372bc933e838cd0dbb76435d4fad9cbe"
        );
    }

    #[test]
    fn trade_rows_are_validated_deduplicated_and_preserve_raw_payload() {
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut state = CollectorState::default();
        let mut malformed = valid_trade(now.timestamp());
        malformed["price"] = json!("1.01");
        let mut formerly_colliding = valid_trade(now.timestamp());
        formerly_colliding["proxyWallet"] = json!("0xother");
        formerly_colliding["size"] = json!("11");
        formerly_colliding["price"] = json!("0.79");
        let (updates, reasons) = trade_updates(
            &ReferenceConfig::default(),
            &mut state,
            "market-1",
            "condition-1",
            "BTCUSDT",
            300,
            vec![
                valid_trade(now.timestamp()),
                valid_trade(now.timestamp()),
                formerly_colliding,
                malformed,
            ],
            now,
            now,
        );
        assert_eq!(updates.len(), 2);
        assert_ne!(updates[0]["record_id"], updates[1]["record_id"]);
        assert_eq!(reasons.get("invalid_price"), Some(&1));
        assert_eq!(updates[0]["record_id_version"], TRADE_ID_VERSION);
        assert_eq!(updates[0]["trade"], valid_trade(now.timestamp()));

        let (again, _) = trade_updates(
            &ReferenceConfig::default(),
            &mut state,
            "market-1",
            "condition-1",
            "BTCUSDT",
            300,
            vec![valid_trade(now.timestamp())],
            now,
            now,
        );
        assert!(again.is_empty());
    }

    #[test]
    fn trade_rows_are_retained_without_a_time_cutoff_until_completion() {
        let now = fixed_time("2026-07-15T12:00:00Z");
        let config = ReferenceConfig {
            market_lookback_secs: 7_200,
            settlement_lookback_secs: 86_400,
            ..ReferenceConfig::default()
        };
        let (updates, malformed) = trade_updates(
            &config,
            &mut CollectorState::default(),
            "market-1",
            "condition-1",
            "BTCUSDT",
            300,
            vec![
                valid_trade((now - TimeDelta::hours(3)).timestamp()),
                valid_trade((now - TimeDelta::hours(25)).timestamp()),
            ],
            now,
            now,
        );

        assert!(malformed.is_empty());
        assert_eq!(updates.len(), 2);
    }

    #[test]
    fn finalization_requires_lag_and_consecutive_stable_post_settlement_polls() {
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut tracked = TrackedMarket {
            settlement_seen_at: Some("2026-07-15T01:00:00Z".to_owned()),
            ..TrackedMarket::default()
        };
        assert!(!advance_trade_finalization(
            &mut tracked,
            now,
            &iso_z(now),
            0,
            false,
            true,
            1_800,
            2,
        ));
        assert!(advance_trade_finalization(
            &mut tracked,
            now,
            &iso_z(now),
            0,
            false,
            true,
            1_800,
            2,
        ));
        assert!(!advance_trade_finalization(
            &mut tracked,
            now,
            &iso_z(now),
            1,
            false,
            true,
            1_800,
            2,
        ));
        assert_eq!(tracked.trade_finalization_stable_polls, 0);
    }

    #[test]
    #[rustfmt::skip]
    fn trade_completion_binds_sorted_record_ids_and_recovers_final_state() {
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut state = CollectorState::default();
        state.trade_seen.insert("condition-1".to_owned(), BTreeMap::from([("trade-b".to_owned(), 2), ("trade-a".to_owned(), 1)]));
        let completion = trade_completion_update(&state, "market-1", "condition-1", "BTCUSDT", 300, now, 60, 2);
        assert_eq!(completion["kind"], TRADE_COMPLETION_KIND);
        assert_eq!(completion["trade_count"], 2);
        assert_eq!(completion["trade_record_ids_sha256"], trade_record_ids_sha256(["trade-a", "trade-b"]));
        let row = json!({"update": completion});
        let mut recovered = CollectorState::default();
        recover_state_from_tape_row(&mut recovered, &row, 0).unwrap();
        let tracked = &recovered.markets["market-1"];
        assert!(tracked.settled);
        assert!(tracked.trade_complete);
    }

    #[test]
    fn tape_recovery_truncates_incomplete_tail_and_continues_sequence() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        {
            let mut writer = TapeWriter::new(root.path()).unwrap();
            writer
                .write_updates(&[json!({"kind": "first"}), json!({"kind": "second"})], now)
                .unwrap();
        }
        OpenOptions::new()
            .append(true)
            .open(root.path().join(ACTIVE_TAPE))
            .unwrap()
            .write_all(b"{incomplete")
            .unwrap();
        {
            let mut writer = TapeWriter::new(root.path()).unwrap();
            assert_eq!(writer.sequence, 2);
            writer
                .write_updates(&[json!({"kind": "third"})], now)
                .unwrap();
        }
        let rows = BufReader::new(File::open(root.path().join(ACTIVE_TAPE)).unwrap())
            .lines()
            .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2]["sequence"], 2);
    }

    #[test]
    fn rotation_publishes_new_active_tape_starting_at_zero() {
        let root = TestDir::new();
        let first = fixed_time("2026-07-15T01:59:59Z");
        let second = fixed_time("2026-07-15T02:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[json!({"kind": "first"})], first)
            .unwrap();
        assert!(!writer.needs_hour_context(first));
        assert!(writer.needs_hour_context(second));
        writer
            .write_updates(&[json!({"kind": "second"})], second)
            .unwrap();
        assert!(!writer.needs_hour_context(second));
        writer.close().unwrap();
        let active: Value =
            serde_json::from_str(&fs::read_to_string(root.path().join(ACTIVE_TAPE)).unwrap())
                .unwrap();
        assert_eq!(active["sequence"], 0);
        let rotated = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.as_ref() != ACTIVE_TAPE
                    && name.starts_with("market-updates.")
                    && name.ends_with(".ndjson")
            })
            .count();
        assert_eq!(rotated, 1);
    }

    #[test]
    fn finalizes_a_stopped_active_tape_into_one_closed_segment() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();
        let closed_count = || {
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.as_ref() != ACTIVE_TAPE
                        && name.starts_with("market-updates.")
                        && name.ends_with(".ndjson")
                })
                .count()
        };
        assert_eq!(closed_count(), 0);

        let closed = finalize_reference_tape(root.path()).unwrap();

        assert!(closed.is_file());
        assert_eq!(fs::read_to_string(&closed).unwrap().lines().count(), 1);
        assert_eq!(closed_count(), 1);
        assert_eq!(
            fs::metadata(root.path().join(ACTIVE_TAPE)).unwrap().len(),
            0
        );
    }

    #[test]
    fn finalized_reference_tape_passes_closed_segment_validation() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();

        let closed = finalize_reference_tape_at(root.path(), now).unwrap();
        let manifest =
            crate::polymarket_upload::scan_tape(&closed, "crypto_expiry_reference", 0, 0)
                .unwrap();

        assert_eq!(manifest["event_types"]["market_metadata"], 1);
        assert_eq!(manifest["start_sequence"], 0);
        assert_eq!(manifest["end_sequence"], 0);
    }

    #[test]
    fn finalization_fails_while_the_reference_spool_is_locked() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();
        let _lock = ReferenceSpoolLock::acquire(root.path()).unwrap();

        let error = finalize_reference_tape_at(root.path(), now).unwrap_err();

        assert!(error.to_string().contains("already locked"));
    }

    #[test]
    fn collector_refuses_a_spool_held_for_finalization() {
        let root = TestDir::new();
        let _lock = ReferenceSpoolLock::acquire(root.path()).unwrap();
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            ..ReferenceConfig::default()
        };

        let error = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(run_reference(config, true))
            .unwrap_err();

        assert!(error.to_string().contains("already locked"));
    }

    #[test]
    fn finalization_rejects_an_empty_active_tape() {
        let root = TestDir::new();
        let writer = TapeWriter::new(root.path()).unwrap();
        drop(writer);

        let error = finalize_reference_tape(root.path()).unwrap_err();

        assert!(error.to_string().contains("active tape is empty"));
    }

    #[test]
    fn finalization_rejects_an_effective_user_that_does_not_own_the_active_tape() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();
        let active = root.path().join(ACTIVE_TAPE);
        let before = fs::read(&active).unwrap();
        let owner = fs::metadata(&active).unwrap().uid();
        let other_uid = owner.checked_add(1).unwrap_or(owner.saturating_sub(1));

        let error = finalize_reference_tape_at_as(root.path(), now, other_uid).unwrap_err();

        assert!(error.to_string().contains("must run as active tape owner"));
        assert_eq!(fs::read(active).unwrap(), before);
        assert!(!root.path().join(REFERENCE_SPOOL_LOCK).exists());
    }

    #[test]
    fn finalization_rejects_a_semantically_invalid_active_tape() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(
                &[json!({
                    "kind": "polymarket_trade",
                    "record_id": "malformed-trade",
                    "record_id_version": "v2",
                })],
                now,
            )
            .unwrap();
        writer.close().unwrap();
        let active = root.path().join(ACTIVE_TAPE);
        let before = fs::read(&active).unwrap();

        let error = finalize_reference_tape_at(root.path(), now).unwrap_err();

        assert!(format!("{error:#}").contains("polymarket_trade.trade must be an object"));
        assert_eq!(fs::read(active).unwrap(), before);
    }

    #[test]
    fn finalization_rejects_a_partial_tail_without_modifying_the_active_tape() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();
        let active = root.path().join(ACTIVE_TAPE);
        OpenOptions::new()
            .append(true)
            .open(&active)
            .unwrap()
            .write_all(br#"{"sequence":1"#)
            .unwrap();
        let before = fs::read(&active).unwrap();

        let error = finalize_reference_tape_at(root.path(), now).unwrap_err();

        assert!(format!("{error:#}").contains("failed uploader validation"));
        assert_eq!(fs::read(&active).unwrap(), before);
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.as_ref() != ACTIVE_TAPE
                        && name.starts_with("market-updates.")
                        && name.ends_with(".ndjson")
                })
                .count(),
            0
        );
    }

    #[test]
    fn finalization_rejects_an_active_path_swap_after_validation() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();
        let active = root.path().join(ACTIVE_TAPE);
        let displaced = root.path().join("validated-active.ndjson");
        let validated_bytes = fs::read(&active).unwrap();
        let replacement = b"{partial";
        let owner = fs::metadata(&active).unwrap().uid();

        let error = finalize_reference_tape_at_as_with(root.path(), now, owner, || {
            fs::rename(&active, &displaced)?;
            fs::write(&active, replacement)?;
            Ok(())
        })
        .unwrap_err();

        assert!(error.to_string().contains("changed after validation"));
        assert_eq!(fs::read(displaced).unwrap(), validated_bytes);
        assert_eq!(fs::read(active).unwrap(), replacement);
        assert!(!fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.as_ref() != ACTIVE_TAPE
                    && name.starts_with("market-updates.")
                    && name.ends_with(".ndjson")
            }));
    }

    #[test]
    fn finalization_refuses_to_replace_an_existing_closed_tape() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T01:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[valid_metadata_update(now)], now)
            .unwrap();
        writer.close().unwrap();
        let active_before = fs::read(root.path().join(ACTIVE_TAPE)).unwrap();
        fs::write(
            root.path().join(format!(
                "market-updates.{}.ndjson",
                now.format("%Y%m%dT%H%M%S%6f")
            )),
            b"existing-closed-tape\n",
        )
        .unwrap();

        let error = finalize_reference_tape_at(root.path(), now).unwrap_err();

        assert!(error.to_string().contains("refusing to replace closed tape"));
        assert_eq!(fs::read(root.path().join(ACTIVE_TAPE)).unwrap(), active_before);
    }

    #[test]
    fn rotation_refuses_an_existing_closed_tape_without_clobbering_either_file() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let closed = root.path().join(format!(
            "market-updates.{}.ndjson",
            now.format("%Y%m%dT%H%M%S%6f")
        ));
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[json!({"kind": "first"})], now)
            .unwrap();
        let active_before = fs::read(root.path().join(ACTIVE_TAPE)).unwrap();
        fs::write(&closed, b"existing-closed-tape\n").unwrap();

        let error = writer.rotate(now).unwrap_err();

        assert!(error.to_string().contains("exist"));
        assert_eq!(
            fs::read(root.path().join(ACTIVE_TAPE)).unwrap(),
            active_before
        );
        assert_eq!(fs::read(closed).unwrap(), b"existing-closed-tape\n");
        assert!(
            writer.file.is_some(),
            "collision must reopen the active tape"
        );
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".rotate"))
                .count(),
            0
        );
        writer
            .write_updates(&[json!({"kind": "second"})], now)
            .unwrap();
        assert_eq!(writer.sequence, 2);
    }

    #[test]
    fn failed_next_tape_publish_rolls_back_without_leaking_stage_or_closing_writer() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[json!({"kind": "first"})], now)
            .unwrap();
        let active_before = fs::read(root.path().join(ACTIVE_TAPE)).unwrap();
        let mut rename_calls = 0;

        let error = writer
            .rotate_with_rename(now, |source, target| {
                rename_calls += 1;
                if rename_calls == 2 {
                    bail!("injected next-tape publish failure");
                }
                rename_noreplace(source, target)
            })
            .unwrap_err();

        assert!(error
            .to_string()
            .contains("injected next-tape publish failure"));
        assert_eq!(rename_calls, 3);
        assert_eq!(
            fs::read(root.path().join(ACTIVE_TAPE)).unwrap(),
            active_before
        );
        assert!(
            writer.file.is_some(),
            "rollback must reopen the active tape"
        );
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".rotate"))
                .count(),
            0
        );
    }

    #[test]
    fn rotation_rejects_an_active_path_swap_before_the_first_rename() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[json!({"kind": "first"})], now)
            .unwrap();
        let active = root.path().join(ACTIVE_TAPE);
        let displaced = root.path().join("opened-active.ndjson");
        let opened_bytes = fs::read(&active).unwrap();
        let replacement = b"replacement\n";
        let mut rename_calls = 0;

        let error = writer
            .rotate_with_rename(now, |source, target| {
                rename_calls += 1;
                if rename_calls == 1 {
                    fs::rename(source, &displaced)?;
                    fs::write(source, replacement)?;
                }
                rename_noreplace(source, target)
            })
            .unwrap_err();

        assert!(error.to_string().contains("identity did not match"));
        assert_eq!(fs::read(displaced).unwrap(), opened_bytes);
        assert_eq!(fs::read(active).unwrap(), replacement);
        assert_eq!(rename_calls, 2);
        assert!(!fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.as_ref() != ACTIVE_TAPE
                    && name.starts_with("market-updates.")
                    && name.ends_with(".ndjson")
            }));
    }

    #[test]
    fn failed_batch_restores_hour_so_retry_still_requires_context() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        let error = writer
            .write_updates_with_sync(
                &[json!({"kind": "market_metadata", "market_id": "market-1"})],
                now,
                |_| bail!("injected sync failure"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("injected sync failure"));
        assert!(writer.needs_hour_context(now));
        assert_eq!(writer.sequence, 0);
        assert_eq!(
            fs::metadata(root.path().join(ACTIVE_TAPE)).unwrap().len(),
            0
        );

        writer
            .write_updates(
                &[json!({"kind": "market_metadata", "market_id": "market-1"})],
                now,
            )
            .unwrap();
        assert!(!writer.needs_hour_context(now));
    }

    #[cfg(unix)]
    #[test]
    fn pending_update_spill_replays_byte_identically_and_reports_counts() {
        use std::os::unix::fs::PermissionsExt;

        let expected_root = TestDir::new();
        let actual_root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let updates = vec![
            json!({"kind": "market_metadata", "market_id": "market-1"}),
            json!({"kind": "polymarket_trade", "record_id": "trade-1"}),
            json!({"kind": "market_settlement", "market_id": "market-1"}),
            json!({"kind": TRADE_COMPLETION_KIND, "market_id": "market-1"}),
        ];
        let mut expected = TapeWriter::new(expected_root.path()).unwrap();
        expected.write_updates(&updates, now).unwrap();
        expected.close().unwrap();

        let mut pending = PendingUpdates::new(actual_root.path()).unwrap();
        assert_eq!(
            pending.record_types(),
            BTreeMap::from([
                ("market_metadata", 0),
                ("market_settlement", 0),
                ("polymarket_trade", 0),
                (TRADE_COMPLETION_KIND, 0),
            ])
        );
        for update in updates {
            pending.push(update).unwrap();
        }
        assert_eq!(pending.len(), 4);
        assert_eq!(
            pending.record_types(),
            BTreeMap::from([
                ("market_metadata", 1),
                ("market_settlement", 1),
                ("polymarket_trade", 1),
                (TRADE_COMPLETION_KIND, 1),
            ])
        );
        assert_eq!(
            pending.file.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );

        let mut actual = TapeWriter::new(actual_root.path()).unwrap();
        pending.replay(&mut actual, now).unwrap();
        actual.close().unwrap();

        assert_eq!(
            fs::read(expected_root.path().join(ACTIVE_TAPE)).unwrap(),
            fs::read(actual_root.path().join(ACTIVE_TAPE)).unwrap()
        );
    }

    #[test]
    fn pending_update_replay_rolls_back_the_whole_batch_on_sync_failure() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[json!({"kind": "prefix"})], now)
            .unwrap();
        let before = fs::read(root.path().join(ACTIVE_TAPE)).unwrap();

        let mut pending = PendingUpdates::new(root.path()).unwrap();
        pending.push(json!({"kind": "first"})).unwrap();
        pending.push(json!({"kind": "second"})).unwrap();
        let error = pending
            .replay_with_sync(&mut writer, now, |_| bail!("injected replay sync failure"))
            .unwrap_err();

        assert!(error.to_string().contains("injected replay sync failure"));
        assert_eq!(writer.sequence, 1);
        assert_eq!(fs::read(root.path().join(ACTIVE_TAPE)).unwrap(), before);
    }

    #[test]
    fn truncated_pending_update_spill_fails_closed_and_rolls_back_tape() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut pending = PendingUpdates::new(root.path()).unwrap();
        pending.push(json!({"kind": "first"})).unwrap();
        pending.push(json!({"kind": "second"})).unwrap();
        let mut reader = BufReader::new(pending.file.try_clone().unwrap());
        reader.seek(SeekFrom::Start(0)).unwrap();
        let mut first_record = Vec::new();
        reader.read_until(b'\n', &mut first_record).unwrap();
        pending
            .file
            .set_len(u64::try_from(first_record.len()).unwrap())
            .unwrap();

        let mut writer = TapeWriter::new(root.path()).unwrap();
        let error = pending.replay(&mut writer, now).unwrap_err();

        assert!(error.to_string().contains("ended after 1 of 2 records"));
        assert_eq!(writer.sequence, 0);
        assert_eq!(
            fs::metadata(root.path().join(ACTIVE_TAPE)).unwrap().len(),
            0
        );
    }

    #[test]
    fn pending_update_spill_has_no_directory_entry_to_leak_on_crash() {
        let root = TestDir::new();
        let mut pending = PendingUpdates::new(root.path()).unwrap();
        pending.push(json!({"kind": "market_metadata"})).unwrap();
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);

        drop(pending);

        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn clean_page_cache_advice_accepts_active_tape() {
        let root = TestDir::new();
        let now = fixed_time("2026-07-15T02:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(&[json!({"kind": "market_metadata"})], now)
            .unwrap();

        release_clean_file_cache(writer.file.as_ref().unwrap()).unwrap();
    }

    #[test]
    fn state_deserialization_bounds_underlying_reads() {
        struct CountingReader<R> {
            inner: R,
            reads: usize,
        }

        impl<R: Read> Read for CountingReader<R> {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.reads += 1;
                self.inner.read(buffer)
            }
        }

        let input = serde_json::to_vec(&vec!["state"; 16_384]).unwrap();
        let mut reader = CountingReader {
            inner: input.as_slice(),
            reads: 0,
        };

        let state: Vec<String> = deserialize_json(&mut reader).unwrap();

        assert_eq!(state.len(), 16_384);
        assert!(reader.reads <= 256, "{} underlying reads", reader.reads);
    }

    #[test]
    fn context_seed_is_required_for_old_state_same_hour_and_next_hour() {
        let root = TestDir::new();
        let current = fixed_time("2026-07-15T02:00:00Z");
        let next = fixed_time("2026-07-15T03:00:00Z");
        let mut writer = TapeWriter::new(root.path()).unwrap();
        writer
            .write_updates(
                &[json!({"kind": "market_metadata", "market_id": "market-1"})],
                current,
            )
            .unwrap();
        let mut old_state: CollectorState =
            serde_json::from_value(json!({"trade_id_version": "v2"})).unwrap();

        assert!(!writer.needs_hour_context(current));
        assert!(context_seed_required(&old_state, &writer, current));
        old_state.context_seed_hour = Some(hour_key(current));
        let state_path = root.path().join("collector-state.json");
        atomic_write_json(&state_path, &old_state).unwrap();
        let reloaded: CollectorState = read_optional_json(&state_path).unwrap();
        assert!(!context_seed_required(&reloaded, &writer, current));
        assert!(context_seed_required(&reloaded, &writer, next));
    }

    #[test]
    fn invalid_state_fails_closed_before_network_access() {
        let root = TestDir::new();
        fs::write(root.path().join("collector-state.json"), b"not-json\n").unwrap();
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            ..ReferenceConfig::default()
        };
        assert!(ReferenceCollector::new(config)
            .err()
            .expect("invalid state must fail")
            .to_string()
            .contains("invalid JSON"));
    }

    #[test]
    fn empty_or_unknown_symbol_catalog_fails_closed() {
        for symbols in [vec!["   ".to_owned()], vec!["NOT_A_MARKET".to_owned()]] {
            let root = TestDir::new();
            let config = ReferenceConfig {
                spool_dir: root.path().to_path_buf(),
                symbols,
                ..ReferenceConfig::default()
            };
            assert!(ReferenceCollector::new(config)
                .err()
                .expect("unsupported symbols must fail")
                .to_string()
                .contains("supported Polymarket catalog"));
        }
    }

    #[test]
    fn v2_migration_quarantines_active_tape_and_reopens_trade_collection() {
        let root = TestDir::new();
        let trade_recorded_at = utc_now();
        let trade_ts = trade_recorded_at.timestamp();
        let trade_update = valid_trade_update(trade_recorded_at, trade_ts);
        let record_id = trade_update["record_id"].as_str().unwrap().to_owned();
        atomic_write_json(
            &root.path().join("collector-state.json"),
            &json!({
                "markets": {"market-1": {"trade_complete": true}},
                "trade_seen": {"condition-1": {"old-id": 1}},
            }),
        )
        .unwrap();
        fs::write(root.path().join(ACTIVE_TAPE), b"legacy-v1\n").unwrap();
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let mut collector = ReferenceCollector::new(config.clone()).unwrap();
        assert_eq!(collector.state.trade_id_version.as_deref(), Some("v2"));
        assert!(collector.state.trade_seen.is_empty());
        assert!(!collector.state.markets["market-1"].trade_complete);
        let durable_state: CollectorState =
            read_optional_json(&root.path().join("collector-state.json")).unwrap();
        assert_eq!(durable_state.trade_id_version.as_deref(), Some("v2"));
        assert_eq!(
            durable_state.trade_completion_version.as_deref(),
            Some("v1")
        );
        let quarantined = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("superseded-v1-market-updates.ndjson.")
            })
            .count();
        assert_eq!(quarantined, 1);
        collector
            .writer
            .write_updates(&[trade_update], trade_recorded_at)
            .unwrap();
        collector.writer.close().unwrap();
        drop(collector);

        let mut restarted = ReferenceCollector::new(config).unwrap();
        assert_eq!(
            restarted.state.trade_seen["condition-1"][&record_id],
            trade_ts
        );
        let durable_segments = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.as_ref() != ACTIVE_TAPE
                        && name.starts_with("market-updates.")
                        && name.ends_with(".ndjson")
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(durable_segments.len(), 1);
        assert!(fs::read_to_string(&durable_segments[0])
            .unwrap()
            .contains(&record_id));
        let quarantined_after_restart = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("superseded-v1-market-updates.ndjson.")
            })
            .count();
        assert_eq!(quarantined_after_restart, 1);
        restarted.writer.close().unwrap();
    }

    #[test]
    #[rustfmt::skip]
    fn completion_migration_reopens_v2_markets_without_a_proof() {
        let root = TestDir::new();
        atomic_write_json(&root.path().join("collector-state.json"), &json!({"trade_id_version":"v2", "markets":{"market-1":{"trade_complete":true,"trade_finalization_stable_polls":9}}})).unwrap();
        let config = ReferenceConfig { spool_dir: root.path().to_path_buf(), symbols: vec!["BTCUSDT".to_owned()], ..ReferenceConfig::default() };
        let mut collector = ReferenceCollector::new(config).unwrap();
        assert_eq!(collector.state.trade_completion_version.as_deref(), Some("v1"));
        assert!(!collector.state.markets["market-1"].trade_complete);
        assert_eq!(collector.state.markets["market-1"].trade_finalization_stable_polls, 0);
        collector.writer.close().unwrap();
    }

    #[test]
    fn collector_recovers_trade_dedupe_state_from_durable_active_tape() {
        let root = TestDir::new();
        let trade_recorded_at = utc_now();
        let trade_ts = trade_recorded_at.timestamp();
        let trade_update = valid_trade_update(trade_recorded_at, trade_ts);
        let record_id = trade_update["record_id"].as_str().unwrap().to_owned();
        atomic_write_json(
            &root.path().join("collector-state.json"),
            &json!({"trade_id_version": "v2"}),
        )
        .unwrap();
        {
            let mut writer = TapeWriter::new(root.path()).unwrap();
            writer
                .write_updates(&[trade_update], trade_recorded_at)
                .unwrap();
        }
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let mut collector = ReferenceCollector::new(config).unwrap();
        assert_eq!(
            collector.state.trade_seen["condition-1"][&record_id],
            trade_ts
        );
        collector.writer.close().unwrap();
    }

    #[test]
    fn startup_rotates_recovered_active_once_instead_of_rescanning_it() {
        let root = TestDir::new();
        let trade_recorded_at = utc_now();
        let trade_ts = trade_recorded_at.timestamp();
        let trade_update = valid_trade_update(trade_recorded_at, trade_ts);
        let record_id = trade_update["record_id"].as_str().unwrap().to_owned();
        atomic_write_json(
            &root.path().join("collector-state.json"),
            &json!({"trade_id_version": "v2"}),
        )
        .unwrap();
        {
            let mut writer = TapeWriter::new(root.path()).unwrap();
            let mut updates = vec![valid_metadata_update(trade_recorded_at); 32_768];
            updates.push(trade_update);
            writer.write_updates(&updates, trade_recorded_at).unwrap();
        }
        OpenOptions::new()
            .append(true)
            .open(root.path().join(ACTIVE_TAPE))
            .unwrap()
            .write_all(b"{\"sequence\":32769")
            .unwrap();
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let mut collector = ReferenceCollector::new(config.clone()).unwrap();
        assert_eq!(
            collector.state.trade_seen["condition-1"][&record_id],
            trade_ts
        );
        collector.writer.close().unwrap();
        drop(collector);

        let rotated = fs::read_dir(root.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.to_string_lossy();
                    name.as_ref() != ACTIVE_TAPE
                        && name.starts_with("market-updates.")
                        && name.ends_with(".ndjson")
                })
            });
        let historical = rotated.unwrap_or_else(|| root.path().join(ACTIVE_TAPE));
        OpenOptions::new()
            .append(true)
            .open(historical)
            .unwrap()
            .write_all(b"not-json\n")
            .unwrap();

        let mut restarted = ReferenceCollector::new(config)
            .expect("a finalized historical segment must not be rescanned on every restart");
        assert_eq!(
            restarted.state.trade_seen["condition-1"][&record_id],
            trade_ts
        );
        assert_eq!(
            fs::metadata(root.path().join(ACTIVE_TAPE)).unwrap().len(),
            0
        );
        restarted.writer.close().unwrap();
    }

    #[test]
    fn invalid_active_tape_fails_closed_without_rotation() {
        let root = TestDir::new();
        atomic_write_json(
            &root.path().join("collector-state.json"),
            &json!({"trade_id_version": "v2"}),
        )
        .unwrap();
        fs::write(root.path().join(ACTIVE_TAPE), b"not-json\n").unwrap();
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let error = ReferenceCollector::new(config)
            .err()
            .expect("invalid active tape must fail before rotation");

        assert!(error.to_string().contains("expected ident"));
        assert_eq!(
            fs::read(root.path().join(ACTIVE_TAPE)).unwrap(),
            b"not-json\n"
        );
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.as_ref() != ACTIVE_TAPE
                        && name.starts_with("market-updates.")
                        && name.ends_with(".ndjson")
                })
                .count(),
            0
        );
    }

    #[test]
    fn semantically_invalid_active_tape_fails_closed_without_rotation() {
        let root = TestDir::new();
        atomic_write_json(
            &root.path().join("collector-state.json"),
            &json!({"trade_id_version": "v2"}),
        )
        .unwrap();
        {
            let mut writer = TapeWriter::new(root.path()).unwrap();
            writer
                .write_updates(
                    &[json!({
                        "kind": "polymarket_trade",
                        "record_id": "malformed-trade",
                        "record_id_version": "v2",
                    })],
                    utc_now(),
                )
                .unwrap();
        }
        let active = root.path().join(ACTIVE_TAPE);
        let before = fs::read(&active).unwrap();
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let error = ReferenceCollector::new(config)
            .err()
            .expect("uploader-invalid active tape must fail before rotation");

        assert!(format!("{error:#}").contains("polymarket_trade.trade must be an object"));
        assert_eq!(fs::read(active).unwrap(), before);
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    name.as_ref() != ACTIVE_TAPE
                        && name.starts_with("market-updates.")
                        && name.ends_with(".ndjson")
                })
                .count(),
            0
        );
    }

    #[test]
    fn startup_compacts_dedupe_outside_settlement_horizon() {
        let root = TestDir::new();
        let now = utc_now();
        let cutoff = now.timestamp() - ReferenceConfig::default().settlement_lookback_secs;
        atomic_write_json(
            &root.path().join("collector-state.json"),
            &json!({
                "trade_id_version": "v2",
                "trade_seen": {
                    "condition-state": {
                        "expired": cutoff - 60,
                        "recent": cutoff + 60,
                    },
                    "condition-incomplete": {
                        "retained": cutoff - 60,
                    }
                },
                "markets": {
                    "market-incomplete": {
                        "condition_id": "condition-incomplete"
                    }
                },
            }),
        )
        .unwrap();
        let recovered = valid_trade_update(now - TimeDelta::hours(3), cutoff - 60);
        let recovered_id = recovered["record_id"].as_str().unwrap().to_owned();
        {
            let mut writer = TapeWriter::new(root.path()).unwrap();
            writer
                .write_updates(&[recovered], now - TimeDelta::hours(3))
                .unwrap();
        }
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let mut collector = ReferenceCollector::new(config).unwrap();

        assert_eq!(
            collector.state.trade_seen,
            BTreeMap::from([
                (
                    "condition-state".to_owned(),
                    BTreeMap::from([("recent".to_owned(), cutoff + 60)]),
                ),
                (
                    "condition-incomplete".to_owned(),
                    BTreeMap::from([("retained".to_owned(), cutoff - 60)]),
                ),
                (
                    "condition-1".to_owned(),
                    BTreeMap::from([(recovered_id, cutoff - 60)]),
                ),
            ])
        );
        let durable: CollectorState =
            read_optional_json(&root.path().join("collector-state.json")).unwrap();
        assert_eq!(durable.trade_seen, collector.state.trade_seen);
        collector.writer.close().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_active_tape_is_refused() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new();
        let target = root.path().join("victim");
        fs::write(&target, b"do not touch\n").unwrap();
        symlink(&target, root.path().join(ACTIVE_TAPE)).unwrap();
        let error = TapeWriter::new(root.path())
            .err()
            .expect("symlink active tape must fail")
            .to_string();
        assert!(error.contains("non-regular") || error.contains("failed to open"));
        let error = finalize_reference_tape(root.path())
            .expect_err("finalizer must reject a symlink active tape")
            .to_string();
        assert!(error.contains("not a regular file"));
        assert_eq!(fs::read(target).unwrap(), b"do not touch\n");
    }
}
