//! Fail-closed Polymarket reference collection and raw tape archival.

use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use rand::random;
use rust_decimal::Decimal;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

pub const ACTIVE_TAPE: &str = "market-updates.ndjson";
const GAMMA_MARKETS_URL: &str = "https://gamma-api.polymarket.com/markets/keyset";
const GAMMA_MARKET_URL: &str = "https://gamma-api.polymarket.com/markets";
const DATA_TRADES_URL: &str = "https://data-api.polymarket.com/trades";
const USER_AGENT: &str = "monday-polymarket-reference-collector/2.0";
const TRADE_ID_VERSION: &str = "v2";
const CRYPTO_TAG_ID: u64 = 21;
const MIN_SETTLEMENT_LOOKBACK_SECS: i64 = 86_400;
pub const DEFAULT_MAX_MARKETS_PER_LANE: usize = 10_000;
const TARGET_MARKET_WINDOWS_SECS: [usize; 2] = [300, 900];
const SETTLEMENT_PRICE: Decimal = Decimal::from_parts(999, 0, 0, false, 3);
const SETTLEMENT_LOSER_PRICE: Decimal = Decimal::from_parts(1, 0, 0, false, 3);
const SETTLEMENT_SUM_TOLERANCE: Decimal = Decimal::from_parts(1, 0, 0, false, 6);

const SYMBOL_ALIASES: [(&str, &[&str]); 7] = [
    ("BTCUSDT", &["BITCOIN", "BTC"]),
    ("ETHUSDT", &["ETHEREUM", "ETH"]),
    ("SOLUSDT", &["SOLANA", "SOL "]),
    ("XRPUSDT", &["XRP"]),
    ("DOGEUSDT", &["DOGECOIN", "DOGE"]),
    ("HYPEUSDT", &["HYPERLIQUID", "HYPE"]),
    ("BNBUSDT", &["BINANCE COIN", "BNB"]),
];

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

#[derive(Debug, Clone)]
pub struct ReferenceConfig {
    pub spool_dir: PathBuf,
    pub symbols: Vec<String>,
    pub poll_interval: Duration,
    pub market_lookback_secs: i64,
    pub settlement_lookback_secs: i64,
    pub max_markets: usize,
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
            poll_interval: Duration::from_secs(30),
            market_lookback_secs: 7_200,
            settlement_lookback_secs: 86_400,
            max_markets: DEFAULT_MAX_MARKETS_PER_LANE,
            http_timeout: Duration::from_secs(20),
            stale_after: Duration::from_secs(180),
            trade_finalization_lag_secs: 1_800,
            trade_finalization_stable_polls: 3,
            per_market_delay: Duration::from_millis(100),
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
            || self.http_timeout.is_zero()
            || self.stale_after.is_zero()
            || self.trade_finalization_lag_secs <= 0
            || self.trade_finalization_stable_polls == 0
        {
            bail!("reference collector limits must be positive");
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
        Ok(_) => serde_json::from_reader(open_read_regular(path)?)
            .with_context(|| format!("invalid JSON in {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error.into()),
    }
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
    let file = OpenOptions::new()
        .create(true)
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

struct TapeWriter {
    spool_dir: PathBuf,
    active: PathBuf,
    hour: Option<String>,
    sequence: u64,
    file: Option<File>,
}

impl TapeWriter {
    fn new(spool_dir: &Path) -> Result<Self> {
        fs::create_dir_all(spool_dir)?;
        let active = spool_dir.join(ACTIVE_TAPE);
        let mut writer = Self {
            spool_dir: spool_dir.to_path_buf(),
            active,
            hour: None,
            sequence: 0,
            file: None,
        };
        writer.recover_active()?;
        Ok(writer)
    }

    fn recover_active(&mut self) -> Result<()> {
        if !self.active.exists() {
            self.file = Some(open_append(&self.active)?);
            return Ok(());
        }
        let mut reader = BufReader::new(open_read_regular(&self.active)?);
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
            expected += 1;
            valid_bytes += u64::try_from(bytes)?;
        }
        if fs::metadata(&self.active)?.len() != valid_bytes {
            let file = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&self.active)?;
            file.set_len(valid_bytes)?;
            file.sync_all()?;
        }
        self.sequence = expected;
        self.hour = first_recorded.map(|value| value.format("%Y%m%dT%H").to_string());
        self.file = Some(open_append(&self.active)?);
        Ok(())
    }

    fn rotate(&mut self, now: DateTime<Utc>) -> Result<()> {
        let current = self.file.take().context("active tape is closed")?;
        current.sync_all()?;
        drop(current);
        let staged = self
            .spool_dir
            .join(format!(".{ACTIVE_TAPE}.{}.rotate", random::<u64>()));
        let staged_file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staged)?;
        staged_file.sync_all()?;
        drop(staged_file);
        let rotated = self.spool_dir.join(format!(
            "market-updates.{}.ndjson",
            now.format("%Y%m%dT%H%M%S%6f")
        ));
        fs::rename(&self.active, &rotated)?;
        if let Err(error) = fs::rename(&staged, &self.active) {
            let rollback = fs::rename(&rotated, &self.active);
            return Err(anyhow!(
                "failed to publish next tape: {error}; rollback={rollback:?}"
            ));
        }
        File::open(&self.spool_dir)?.sync_all()?;
        self.sequence = 0;
        self.hour = None;
        self.file = Some(open_append(&self.active)?);
        Ok(())
    }

    fn write_updates(&mut self, updates: &[Value], now: DateTime<Utc>) -> Result<()> {
        self.write_updates_with_sync(updates, now, |file| {
            file.sync_all()?;
            Ok(())
        })
    }

    fn write_updates_with_sync<F>(
        &mut self,
        updates: &[Value],
        now: DateTime<Utc>,
        sync: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut File) -> Result<()>,
    {
        if updates.is_empty() {
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
struct GammaDiscovery {
    open: Vec<Value>,
    closed: Vec<Value>,
}

impl GammaDiscovery {
    fn append_page(
        &mut self,
        lane: GammaLane,
        payload: &Value,
        max_markets: usize,
    ) -> Result<Option<String>> {
        let markets = match lane {
            GammaLane::Open => &mut self.open,
            GammaLane::Closed => &mut self.closed,
        };
        append_gamma_page(markets, payload, max_markets)
    }

    fn into_markets(mut self) -> Vec<Value> {
        self.open.append(&mut self.closed);
        self.open
    }
}

fn append_gamma_page(
    markets: &mut Vec<Value>,
    payload: &Value,
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
    let total = markets
        .len()
        .checked_add(page.len())
        .ok_or_else(|| completeness_error("Gamma market count overflow"))?;
    if total > max_markets || (total == max_markets && next_cursor.is_some()) {
        return Err(completeness_error(format!(
            "Gamma market discovery reached max_markets={max_markets} before exhausting its cursor"
        )));
    }
    markets.extend(page.iter().cloned());
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

fn object_rows(page: &[Value]) -> (Vec<Value>, u64) {
    let objects = page
        .iter()
        .filter(|value| value.is_object())
        .cloned()
        .collect::<Vec<_>>();
    let rejected = u64::try_from(page.len() - objects.len()).unwrap_or(u64::MAX);
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
    config: &ReferenceConfig,
    state: &mut CollectorState,
    market_id: &str,
    condition_id: &str,
    symbol: &str,
    window_secs: u64,
    trades: &[Value],
    now: DateTime<Utc>,
) -> (Vec<Value>, BTreeMap<String, u64>) {
    let cutoff = now.timestamp() - config.market_lookback_secs;
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
        let record_id = stable_trade_id(trade);
        if timestamp < cutoff || seen.contains_key(&record_id) {
            continue;
        }
        seen.insert(record_id.clone(), timestamp);
        updates.push(json!({
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
            "received_at": iso_z(now),
            "trade": trade,
        }));
    }
    seen.retain(|_, timestamp| *timestamp >= cutoff);
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

struct ReferenceCollector {
    config: ReferenceConfig,
    symbols: BTreeSet<String>,
    state_path: PathBuf,
    health_path: PathBuf,
    state: CollectorState,
    writer: TapeWriter,
    http: reqwest::Client,
    last_success: Instant,
}

impl ReferenceCollector {
    fn new(config: ReferenceConfig) -> Result<Self> {
        config.validate()?;
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
        if state.trade_id_version.as_deref() != Some(TRADE_ID_VERSION) {
            let active = config.spool_dir.join(ACTIVE_TAPE);
            if active.exists() {
                strict_existing_regular(&active)?;
                if fs::metadata(&active)?.len() > 0 {
                    let quarantine = config.spool_dir.join(format!(
                        "superseded-v1-{ACTIVE_TAPE}.{}",
                        utc_now().timestamp_nanos_opt().unwrap_or_default()
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
            atomic_write_json(&state_path, &state)?;
        }
        let writer = TapeWriter::new(&config.spool_dir)?;
        let http = reqwest::Client::builder()
            .timeout(config.http_timeout)
            .user_agent(USER_AGENT)
            .build()?;
        let mut collector = Self {
            config,
            symbols,
            state_path,
            health_path,
            state,
            writer,
            http,
            last_success: Instant::now(),
        };
        collector.recover_state_from_active_tape()?;
        Ok(collector)
    }

    fn recover_state_from_active_tape(&mut self) -> Result<()> {
        if !self.writer.active.exists() {
            return Ok(());
        }
        let reader = BufReader::new(open_read_regular(&self.writer.active)?);
        for line in reader.split(b'\n') {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            let row: Value = serde_json::from_slice(&line)?;
            let update = row
                .get("update")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let kind = update
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let market_id = update.get("market_id").and_then(Value::as_str);
            let condition_id = update.get("condition_id").and_then(Value::as_str);
            if kind == "polymarket_trade" {
                if update.get("record_id_version").and_then(Value::as_str) == Some(TRADE_ID_VERSION)
                {
                    if let (Some(condition_id), Some(record_id), Some(timestamp)) = (
                        condition_id,
                        update.get("record_id").and_then(Value::as_str),
                        update.get("trade_ts_unix").and_then(Value::as_i64),
                    ) {
                        self.state
                            .trade_seen
                            .entry(condition_id.to_owned())
                            .or_default()
                            .insert(record_id.to_owned(), timestamp);
                    }
                }
            } else if matches!(kind, "market_metadata" | "market_settlement") {
                if let Some(market_id) = market_id {
                    let tracked = self.state.markets.entry(market_id.to_owned()).or_default();
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
        }
        Ok(())
    }

    async fn get_json(&self, url: &str, params: &[(String, String)]) -> Result<Value> {
        Ok(self
            .http
            .get(url)
            .query(params)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn discover_markets(&self, now: DateTime<Utc>) -> Result<Vec<Value>> {
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
                let payload = self.get_json(GAMMA_MARKETS_URL, &params).await?;
                cursor = discovery.append_page(lane, &payload, self.config.max_markets)?;
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
                .get_json(
                    DATA_TRADES_URL,
                    &[
                        ("market".to_owned(), condition_id.to_owned()),
                        ("limit".to_owned(), "10000".to_owned()),
                        ("offset".to_owned(), offset.to_string()),
                        ("takerOnly".to_owned(), "false".to_owned()),
                    ],
                )
                .await?;
            let page = payload
                .as_array()
                .context("Data API trades response is not an array")?;
            let (objects, rejected) = object_rows(page);
            trades.extend(objects);
            non_object_rows = non_object_rows.saturating_add(rejected);
            if page.len() < 10_000 {
                break;
            }
            if offset == 10_000 {
                truncated = true;
            }
        }
        Ok((trades, truncated, non_object_rows))
    }

    async fn collect_once(&mut self) -> Result<Value> {
        let now = utc_now();
        let retrieved_at = iso_z(now);
        let mut updates = Vec::new();
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
        let missing_target_symbols = missing_symbols(&self.symbols, &discovered_target_symbols);

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
            if needs_detail {
                let url = format!("{GAMMA_MARKET_URL}/{market_id}");
                match self.get_json(&url, &[]).await {
                    Ok(market) if market.is_object() => {
                        targets.insert(
                            market_id.clone(),
                            (
                                market,
                                TargetMarket {
                                    symbol: tracked.symbol.clone().unwrap_or_default(),
                                    window_secs: tracked.market_window_secs,
                                },
                            ),
                        );
                        tracked.settlement_failure_since = None;
                        tracked.settlement_last_error = None;
                    }
                    Ok(_) => {
                        let error = "Gamma market detail response is not an object".to_owned();
                        errors.push(format!("settlement {market_id}: {error}"));
                        tracked
                            .settlement_failure_since
                            .get_or_insert_with(|| retrieved_at.clone());
                        tracked.settlement_last_error = Some(error);
                    }
                    Err(error) => {
                        errors.push(format!("settlement {market_id}: {error}"));
                        tracked
                            .settlement_failure_since
                            .get_or_insert_with(|| retrieved_at.clone());
                        tracked.settlement_last_error = Some(error.to_string());
                    }
                }
            }
            next_state.markets.insert(market_id, tracked);
        }

        for (market_id, (market, target)) in &targets {
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
                market,
                target,
                &retrieved_at,
                force_hour_context,
                &mut tracked,
            )? {
                updates.push(metadata);
            }

            let settlement = match settlement_from_market(
                market,
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
            let was_settled = tracked.settled;
            if !condition_id.is_empty() && !tracked.trade_complete {
                trade_polls += 1;
                match self.fetch_trades(&condition_id).await {
                    Ok((trades, truncated, non_object_rows)) => {
                        successful_trade_polls += 1;
                        let (new_updates, mut malformed) = trade_updates(
                            &self.config,
                            &mut next_state,
                            market_id,
                            &condition_id,
                            &target.symbol,
                            target.window_secs,
                            &trades,
                            now,
                        );
                        if non_object_rows > 0 {
                            *malformed.entry("non_object_trade".to_owned()).or_default() +=
                                non_object_rows;
                            non_object_trade_markets.push(condition_id.clone());
                        }
                        let new_trade_count = new_updates.len();
                        updates.extend(new_updates);
                        for (reason, count) in &malformed {
                            *malformed_trade_reasons.entry(reason.clone()).or_default() += count;
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
                        if settlement.is_some()
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
                    updates.push(settlement);
                    tracked.settled = true;
                }
            }
            next_state.markets.insert(market_id.clone(), tracked);
            if !self.config.per_market_delay.is_zero() {
                tokio::time::sleep(self.config.per_market_delay).await;
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

        self.writer.write_updates(&updates, now)?;
        if missing_target_symbols.is_empty() {
            next_state.context_seed_hour = Some(target_hour);
        }
        self.state = next_state;
        atomic_write_json(&self.state_path, &self.state)?;
        self.last_success = Instant::now();

        overdue_unresolved_markets.sort();
        truncated_markets.sort();
        non_object_trade_markets.sort();
        invalid_settlement_markets.sort();
        invalid_end_time_markets.sort();
        stale_trade_markets.sort();
        stale_settlement_markets.sort();
        let record_types = ["market_metadata", "polymarket_trade", "market_settlement"]
            .into_iter()
            .map(|kind| {
                (
                    kind,
                    updates
                        .iter()
                        .filter(|update| update.get("kind").and_then(Value::as_str) == Some(kind))
                        .count(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let health = json!({
            "updated_at": retrieved_at,
            "last_success_at": retrieved_at,
            "target_markets": targets.len(),
            "missing_target_symbols": missing_target_symbols,
            "tracked_markets": self.state.markets.len(),
            "records_written": updates.len(),
            "record_types": record_types,
            "api_errors": errors,
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
        if !overdue_unresolved_markets.is_empty() {
            return Err(DataCompletenessError(format!(
                "overdue unresolved settlements: {overdue_unresolved_markets:?}"
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
}

pub async fn run_reference(config: ReferenceConfig, once: bool) -> Result<()> {
    let poll_interval = config.poll_interval;
    let stale_after = config.stale_after;
    let mut collector = ReferenceCollector::new(config)?;
    if once {
        println!(
            "{}",
            serde_json::to_string(&collector.collect_once().await?)?
        );
        collector.writer.close()?;
        return Ok(());
    }
    loop {
        let started = Instant::now();
        match collector.collect_once().await {
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

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "monday-polymarket-reference-test-{:016x}",
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

    fn fixed_time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
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
        let mut markets = Vec::new();
        let cursor = append_gamma_page(
            &mut markets,
            &json!({"markets": [{"id": "one"}], "next_cursor": "page-2"}),
            2,
        )
        .unwrap();
        assert_eq!(cursor.as_deref(), Some("page-2"));
        assert!(append_gamma_page(
            &mut markets,
            &json!({"markets": [{"id": "two"}], "next_cursor": ""}),
            2,
        )
        .unwrap()
        .is_none());
        assert_eq!(markets.len(), 2);

        let mut capped = Vec::new();
        let error = append_gamma_page(
            &mut capped,
            &json!({"markets": [{"id": "one"}], "next_cursor": "page-2"}),
            1,
        )
        .unwrap_err();
        assert!(error.downcast_ref::<DataCompletenessError>().is_some());
    }

    #[test]
    fn open_and_closed_discovery_lanes_have_independent_caps() {
        let mut discovery = GammaDiscovery::default();
        assert!(discovery
            .append_page(
                GammaLane::Open,
                &json!({"markets": [{"id": "open"}], "next_cursor": ""}),
                1,
            )
            .unwrap()
            .is_none());
        assert!(discovery
            .append_page(
                GammaLane::Closed,
                &json!({"markets": [{"id": "closed"}], "next_cursor": ""}),
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
        default.validate().unwrap();

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
        let mut markets = Vec::new();
        assert!(
            append_gamma_page(&mut markets, &json!({"markets": [{"id": "one"}, null]}), 10,)
                .is_err()
        );

        let (objects, rejected) = object_rows(&[json!({"id": "trade"}), Value::Null]);
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
            &[
                valid_trade(now.timestamp()),
                valid_trade(now.timestamp()),
                formerly_colliding,
                malformed,
            ],
            now,
        );
        assert_eq!(updates.len(), 2);
        assert_ne!(updates[0]["record_id"], updates[1]["record_id"]);
        assert_eq!(reasons.get("invalid_price"), Some(&1));
        assert_eq!(updates[0]["record_id_version"], TRADE_ID_VERSION);
        assert_eq!(updates[0]["trade"]["sourceOnlyField"]["preserved"], true);

        let (again, _) = trade_updates(
            &ReferenceConfig::default(),
            &mut state,
            "market-1",
            "condition-1",
            "BTCUSDT",
            300,
            &[valid_trade(now.timestamp())],
            now,
        );
        assert!(again.is_empty());
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
            .write_updates(
                &[json!({
                    "kind": "polymarket_trade",
                    "condition_id": "condition-1",
                    "record_id": "durable-v2",
                    "record_id_version": "v2",
                    "trade_ts_unix": 1_784_084_400_i64,
                })],
                fixed_time("2026-07-15T03:00:00Z"),
            )
            .unwrap();
        collector.writer.close().unwrap();
        drop(collector);

        let mut restarted = ReferenceCollector::new(config).unwrap();
        assert!(fs::read_to_string(root.path().join(ACTIVE_TAPE))
            .unwrap()
            .contains("durable-v2"));
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
    fn collector_recovers_trade_dedupe_state_from_durable_active_tape() {
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
                        "market_id": "market-1",
                        "condition_id": "condition-1",
                        "record_id": "trade-1",
                        "record_id_version": "v2",
                        "trade_ts_unix": 1_784_084_400_i64,
                    })],
                    fixed_time("2026-07-15T03:00:00Z"),
                )
                .unwrap();
        }
        let config = ReferenceConfig {
            spool_dir: root.path().to_path_buf(),
            symbols: vec!["BTCUSDT".to_owned()],
            ..ReferenceConfig::default()
        };

        let mut collector = ReferenceCollector::new(config).unwrap();
        assert_eq!(
            collector.state.trade_seen["condition-1"]["trade-1"],
            1_784_084_400
        );
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
        assert_eq!(fs::read(target).unwrap(), b"do not touch\n");
    }
}
