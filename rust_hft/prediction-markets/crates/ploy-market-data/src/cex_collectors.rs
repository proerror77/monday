//! Public CEX market-data normalization and collection.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adapter_binance_data::{
    BinanceReferenceError, BinanceWebSocket, HttpReferenceSource, ReferenceSource, Symbol,
    TimedJson,
};
use chrono::{DateTime, TimeZone, Utc};
use data::binance_usdm_reference::{
    basis_observation, force_order_observation, mark_index_funding_observations,
    open_interest_observation, BasisObservation, ForceOrderObservation,
    MarkIndexFundingObservation, OpenInterestObservation,
};
use futures::{SinkExt, StreamExt};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

const OKX_WS: &str = "wss://ws.okx.com:8443/ws/v5/public";
const BYBIT_WS: &str = "wss://stream.bybit.com/v5/public/spot";
const COINBASE_WS: &str = "wss://advanced-trade-ws.coinbase.com";
const KRAKEN_WS: &str = "wss://ws.kraken.com/v2";

#[derive(Debug, thiserror::Error)]
pub enum CexCollectorError {
    #[error("invalid CEX payload: {0}")]
    InvalidPayload(String),
    #[error("CEX HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("CEX WebSocket failed: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("CEX JSON payload failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CEX database write failed: {0}")]
    Database(#[from] sqlx::Error),
}

type Result<T> = std::result::Result<T, CexCollectorError>;

fn invalid(message: impl Into<String>) -> CexCollectorError {
    CexCollectorError::InvalidPayload(message.into())
}

#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedTick {
    pub exchange: String,
    pub market_type: String,
    pub symbol: String,
    pub exchange_symbol: String,
    pub kind: String,
    pub event_time: DateTime<Utc>,
    /// Local receipt time. Binance reference rows must carry this explicitly;
    /// other CEX book adapters fill it at their message-processing boundary.
    pub received_at: DateTime<Utc>,
    /// Basis endpoint interval timestamp and local receipt, kept separate from
    /// the mark/index event clock in `event_time`.
    pub basis_event_time: Option<DateTime<Utc>>,
    pub basis_received_at: Option<DateTime<Utc>>,
    pub basis_reference: Option<BasisObservation>,
    pub force_order_reference: Option<ForceOrderObservation>,
    /// Canonical typed reference payload retained beside the original wire
    /// body so downstream readers do not have to re-parse `raw`.
    pub typed_reference: Option<Value>,
    /// Coverage claim attached to a typed reference observation.
    pub coverage: Option<String>,
    pub update_id: Option<i64>,
    pub sequence_id: Option<i64>,
    pub mark_price: Option<Decimal>,
    pub index_price: Option<Decimal>,
    pub funding_rate: Option<Decimal>,
    pub open_interest: Option<Decimal>,
    pub basis: Option<Decimal>,
    pub basis_rate: Option<Decimal>,
    pub annualized_basis_rate: Option<Decimal>,
    pub next_funding_time: Option<DateTime<Utc>>,
    pub side: Option<String>,
    pub price: Option<Decimal>,
    pub quantity: Option<Decimal>,
    pub best_bid: Option<Decimal>,
    pub best_ask: Option<Decimal>,
    pub mid_price: Option<Decimal>,
    pub spread_bps: Option<Decimal>,
    pub obi_5: Option<Decimal>,
    pub obi_10: Option<Decimal>,
    pub bid_volume_5: Option<Decimal>,
    pub ask_volume_5: Option<Decimal>,
    pub bids: Option<Value>,
    pub asks: Option<Value>,
    pub source: String,
    pub dedupe_key: String,
    pub raw: Value,
}

pub fn normalize_binance_reference(
    mark: &MarkIndexFundingObservation,
    open_interest: &OpenInterestObservation,
    basis: &BasisObservation,
    raw: Value,
) -> Result<NormalizedTick> {
    if mark.symbol != open_interest.symbol || mark.symbol != basis.symbol {
        return Err(invalid("Binance reference symbol mismatch"));
    }
    let source_time_ms = mark.source_time_ms;
    let received_at_ns = mark
        .received_at_ns
        .max(open_interest.received_at_ns)
        .max(basis.received_at_ns);
    let event_time = ms_to_utc_u64(source_time_ms)?;
    let received_at = ns_to_utc(received_at_ns)?;
    Ok(NormalizedTick {
        exchange: "binance".to_string(),
        market_type: "perpetual".to_string(),
        symbol: mark.symbol.clone(),
        exchange_symbol: mark.symbol.clone(),
        kind: "derivatives_snapshot".to_string(),
        event_time,
        received_at,
        basis_event_time: Some(ms_to_utc_u64(basis.source_time_ms)?),
        basis_received_at: Some(ns_to_utc(basis.received_at_ns)?),
        basis_reference: Some(basis.clone()),
        force_order_reference: None,
        typed_reference: Some(serde_json::json!({
            "mark_index_funding": mark,
            "open_interest": open_interest,
            "basis": basis,
        })),
        coverage: None,
        update_id: None,
        sequence_id: None,
        mark_price: Some(mark.mark_price),
        index_price: Some(mark.index_price),
        funding_rate: Some(mark.last_funding_rate),
        open_interest: Some(open_interest.open_interest),
        basis: Some(basis.basis),
        basis_rate: Some(basis.basis_rate),
        annualized_basis_rate: basis.annualized_basis_rate,
        next_funding_time: Some(ms_to_utc_u64(mark.next_funding_time_ms)?),
        side: None,
        price: None,
        quantity: None,
        best_bid: None,
        best_ask: None,
        mid_price: None,
        spread_bps: None,
        obi_5: None,
        obi_10: None,
        bid_volume_5: None,
        ask_volume_5: None,
        bids: None,
        asks: None,
        source: "binance_futures_rest".to_string(),
        dedupe_key: format!(
            "{}:{}:{}",
            mark.source_time_ms, basis.source_time_ms, open_interest.source_time_ms
        ),
        raw,
    })
}

pub fn normalize_binance_liquidation(
    observation: &ForceOrderObservation,
    raw: Value,
) -> Result<NormalizedTick> {
    Ok(NormalizedTick {
        exchange: "binance".to_string(),
        market_type: "perpetual".to_string(),
        symbol: observation.symbol.clone(),
        exchange_symbol: observation.symbol.clone(),
        kind: "liquidation".to_string(),
        event_time: ms_to_utc_u64(observation.event_time_ms)?,
        received_at: ns_to_utc(observation.received_at_ns)?,
        basis_event_time: None,
        basis_received_at: None,
        basis_reference: None,
        force_order_reference: Some(observation.clone()),
        typed_reference: Some(serde_json::to_value(observation)?),
        coverage: Some(observation.coverage.clone()),
        update_id: None,
        sequence_id: None,
        mark_price: None,
        index_price: None,
        funding_rate: None,
        open_interest: None,
        basis: None,
        basis_rate: None,
        annualized_basis_rate: None,
        next_funding_time: None,
        side: Some(observation.side.clone()),
        price: Some(observation.average_price),
        // This is the venue-reported cumulative quantity, not a local fill.
        quantity: Some(observation.cumulative_filled_quantity),
        best_bid: None,
        best_ask: None,
        mid_price: None,
        spread_bps: None,
        obi_5: None,
        obi_10: None,
        bid_volume_5: None,
        ask_volume_5: None,
        bids: None,
        asks: None,
        source: "binance_force_order_ws".to_string(),
        dedupe_key: observation.content_identity(),
        raw,
    })
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid(format!("missing {field}")))
}

fn decimal(value: &Value) -> Option<Decimal> {
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_number().map(ToString::to_string))?
        .parse()
        .ok()
}

fn decimal_field(value: &Value, field: &str) -> Option<Decimal> {
    decimal(&value[field])
}

fn ms_to_utc(ms: i64) -> Result<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms)
        .single()
        .ok_or_else(|| invalid(format!("invalid millisecond timestamp {ms}")))
}

fn ms_to_utc_u64(ms: u64) -> Result<DateTime<Utc>> {
    let ms = i64::try_from(ms).map_err(|_| invalid(format!("timestamp exceeds i64: {ms}")))?;
    ms_to_utc(ms)
}

fn ns_to_utc(ns: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(ns / 1_000_000_000)
        .map_err(|_| invalid(format!("nanosecond timestamp exceeds i64: {ns}")))?;
    DateTime::from_timestamp(seconds, (ns % 1_000_000_000) as u32)
        .ok_or_else(|| invalid(format!("invalid nanosecond timestamp {ns}")))
}

fn parse_time(value: &Value) -> Result<DateTime<Utc>> {
    if let Some(ms) = value.as_i64() {
        return ms_to_utc(ms);
    }
    if let Some(raw) = value.as_str() {
        if let Ok(ms) = raw.parse::<i64>() {
            return ms_to_utc(ms);
        }
        return DateTime::parse_from_rfc3339(raw)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| invalid(error.to_string()));
    }
    Err(invalid("missing event timestamp"))
}

#[derive(Debug)]
pub struct CexBook {
    exchange: String,
    market_type: String,
    symbol: String,
    exchange_symbol: String,
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

impl CexBook {
    pub fn new(exchange: &str, market_type: &str, symbol: &str, exchange_symbol: &str) -> Self {
        Self {
            exchange: exchange.to_string(),
            market_type: market_type.to_string(),
            symbol: symbol.to_string(),
            exchange_symbol: exchange_symbol.to_string(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
        }
    }

    pub fn apply_message(&mut self, _message: &Value) -> Result<Option<NormalizedTick>> {
        let update = match self.exchange.as_str() {
            "okx" => parse_okx(_message)?,
            "bybit" => parse_bybit(_message)?,
            "coinbase" => parse_coinbase(_message, &self.exchange_symbol)?,
            "kraken" => parse_kraken(_message)?,
            other => return Err(invalid(format!("unsupported CEX book exchange {other}"))),
        };
        let Some(update) = update else {
            return Ok(None);
        };
        if update.exchange_symbol != self.exchange_symbol {
            return Ok(None);
        }
        if update.snapshot {
            self.bids.clear();
            self.asks.clear();
        }
        apply_levels(&mut self.bids, &update.bids);
        apply_levels(&mut self.asks, &update.asks);
        match self.exchange.as_str() {
            "okx" => truncate_book(&mut self.bids, &mut self.asks, 5),
            "bybit" => truncate_book(&mut self.bids, &mut self.asks, 50),
            "kraken" => truncate_book(&mut self.bids, &mut self.asks, 10),
            _ => {}
        }
        if self.bids.is_empty() || self.asks.is_empty() {
            return Ok(None);
        }
        if self.exchange == "kraken" {
            if let Some(expected) = update.update_id {
                let actual = kraken_checksum(&self.bids, &self.asks) as i64;
                if actual != expected {
                    self.bids.clear();
                    self.asks.clear();
                    return Err(invalid(format!(
                        "Kraken L2 checksum mismatch: expected={expected} actual={actual}"
                    )));
                }
            }
        }
        Ok(Some(self.snapshot(update)))
    }

    fn snapshot(&self, update: BookUpdate) -> NormalizedTick {
        let bids: Vec<(Decimal, Decimal)> = self
            .bids
            .iter()
            .rev()
            .take(50)
            .map(|(price, quantity)| (*price, *quantity))
            .collect();
        let asks: Vec<(Decimal, Decimal)> = self
            .asks
            .iter()
            .take(50)
            .map(|(price, quantity)| (*price, *quantity))
            .collect();
        let best_bid = bids[0].0;
        let best_ask = asks[0].0;
        let mid = (best_bid + best_ask) / Decimal::from(2);
        let bid_5 = volume(&bids, 5);
        let ask_5 = volume(&asks, 5);
        let bid_10 = volume(&bids, 10);
        let ask_10 = volume(&asks, 10);
        let update_key = update
            .update_id
            .or(update.sequence_id)
            .map(|value| value.to_string())
            .unwrap_or_else(|| update.event_time.timestamp_millis().to_string());
        NormalizedTick {
            exchange: self.exchange.clone(),
            market_type: self.market_type.clone(),
            symbol: self.symbol.clone(),
            exchange_symbol: self.exchange_symbol.clone(),
            kind: "lob".to_string(),
            event_time: update.event_time,
            received_at: Utc::now(),
            basis_event_time: None,
            basis_received_at: None,
            basis_reference: None,
            force_order_reference: None,
            typed_reference: None,
            coverage: None,
            update_id: update.update_id,
            sequence_id: update.sequence_id,
            mark_price: None,
            index_price: None,
            funding_rate: None,
            open_interest: None,
            basis: None,
            basis_rate: None,
            annualized_basis_rate: None,
            next_funding_time: None,
            side: None,
            price: None,
            quantity: None,
            best_bid: Some(best_bid),
            best_ask: Some(best_ask),
            mid_price: Some(mid),
            spread_bps: Some((best_ask - best_bid) / mid * Decimal::from(10_000)),
            obi_5: Some(imbalance(bid_5, ask_5)),
            obi_10: (bids.len() >= 10 && asks.len() >= 10).then(|| imbalance(bid_10, ask_10)),
            bid_volume_5: Some(bid_5),
            ask_volume_5: Some(ask_5),
            bids: Some(levels_json(&bids)),
            asks: Some(levels_json(&asks)),
            source: format!("{}_public_ws", self.exchange),
            dedupe_key: update_key,
            raw: update.raw,
        }
    }
}

#[derive(Debug)]
struct BookUpdate {
    exchange_symbol: String,
    event_time: DateTime<Utc>,
    update_id: Option<i64>,
    sequence_id: Option<i64>,
    snapshot: bool,
    bids: Vec<(Decimal, Decimal)>,
    asks: Vec<(Decimal, Decimal)>,
    raw: Value,
}

fn array_levels(value: &Value) -> Vec<(Decimal, Decimal)> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|level| {
            let fields = level.as_array()?;
            Some((decimal(fields.first()?)?, decimal(fields.get(1)?)?))
        })
        .collect()
}

fn object_levels(value: &Value) -> Vec<(Decimal, Decimal)> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|level| Some((decimal(&level["price"])?, decimal(&level["qty"])?)))
        .collect()
}

fn apply_levels(book: &mut BTreeMap<Decimal, Decimal>, levels: &[(Decimal, Decimal)]) {
    for (price, quantity) in levels {
        if *quantity <= Decimal::ZERO {
            book.remove(price);
        } else if *price > Decimal::ZERO {
            book.insert(*price, *quantity);
        }
    }
}

fn truncate_book(
    bids: &mut BTreeMap<Decimal, Decimal>,
    asks: &mut BTreeMap<Decimal, Decimal>,
    depth: usize,
) {
    let bid_keys: Vec<Decimal> = bids
        .keys()
        .take(bids.len().saturating_sub(depth))
        .copied()
        .collect();
    let ask_keys: Vec<Decimal> = asks
        .keys()
        .rev()
        .take(asks.len().saturating_sub(depth))
        .copied()
        .collect();
    for key in bid_keys {
        bids.remove(&key);
    }
    for key in ask_keys {
        asks.remove(&key);
    }
}

fn parse_okx(message: &Value) -> Result<Option<BookUpdate>> {
    let Some(data) = message["data"].as_array().and_then(|rows| rows.first()) else {
        return Ok(None);
    };
    let exchange_symbol = required_str(&message["arg"], "instId")?.to_string();
    Ok(Some(BookUpdate {
        exchange_symbol,
        event_time: parse_time(&data["ts"])?,
        update_id: data["seqId"].as_i64(),
        sequence_id: data["seqId"].as_i64(),
        snapshot: true,
        bids: array_levels(&data["bids"]),
        asks: array_levels(&data["asks"]),
        raw: message.clone(),
    }))
}

fn parse_bybit(message: &Value) -> Result<Option<BookUpdate>> {
    if message["data"].is_null() {
        return Ok(None);
    }
    let data = &message["data"];
    let update_id = data["u"].as_i64();
    Ok(Some(BookUpdate {
        exchange_symbol: required_str(data, "s")?.to_string(),
        event_time: parse_time(&message["ts"])?,
        update_id,
        sequence_id: data["seq"].as_i64(),
        snapshot: message["type"].as_str() == Some("snapshot") || update_id == Some(1),
        bids: array_levels(&data["b"]),
        asks: array_levels(&data["a"]),
        raw: message.clone(),
    }))
}

fn parse_coinbase(message: &Value, exchange_symbol: &str) -> Result<Option<BookUpdate>> {
    if message["channel"].as_str() != Some("l2_data") {
        return Ok(None);
    }
    let event = message["events"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|event| event["product_id"].as_str() == Some(exchange_symbol));
    let Some(event) = event else { return Ok(None) };
    let mut bids = Vec::new();
    let mut asks = Vec::new();
    for update in event["updates"].as_array().into_iter().flatten() {
        let Some(level) =
            decimal_field(update, "price_level").zip(decimal_field(update, "new_quantity"))
        else {
            continue;
        };
        match update["side"].as_str() {
            Some("bid") => bids.push(level),
            Some("offer") => asks.push(level),
            _ => {}
        }
    }
    Ok(Some(BookUpdate {
        exchange_symbol: exchange_symbol.to_string(),
        event_time: parse_time(&message["timestamp"])?,
        update_id: None,
        sequence_id: message["sequence_num"].as_i64(),
        snapshot: event["type"].as_str() == Some("snapshot"),
        bids,
        asks,
        raw: message.clone(),
    }))
}

fn parse_kraken(message: &Value) -> Result<Option<BookUpdate>> {
    if message["channel"].as_str() != Some("book") {
        return Ok(None);
    }
    let Some(data) = message["data"].as_array().and_then(|rows| rows.first()) else {
        return Ok(None);
    };
    let checksum = data["checksum"].as_i64();
    Ok(Some(BookUpdate {
        exchange_symbol: required_str(data, "symbol")?.to_string(),
        event_time: parse_time(&data["timestamp"])?,
        update_id: checksum,
        sequence_id: None,
        snapshot: message["type"].as_str() == Some("snapshot"),
        bids: object_levels(&data["bids"]),
        asks: object_levels(&data["asks"]),
        raw: message.clone(),
    }))
}

fn volume(levels: &[(Decimal, Decimal)], depth: usize) -> Decimal {
    levels
        .iter()
        .take(depth)
        .map(|(_, quantity)| *quantity)
        .sum()
}

fn imbalance(bids: Decimal, asks: Decimal) -> Decimal {
    let total = bids + asks;
    if total.is_zero() {
        Decimal::ZERO
    } else {
        (bids - asks) / total
    }
}

fn levels_json(levels: &[(Decimal, Decimal)]) -> Value {
    Value::Array(levels.iter().map(|(price, quantity)| {
        serde_json::json!({"price": price.to_string(), "size": quantity.to_string()})
    }).collect())
}

fn kraken_checksum(bids: &BTreeMap<Decimal, Decimal>, asks: &BTreeMap<Decimal, Decimal>) -> u32 {
    fn component(value: Decimal) -> String {
        let compact = value.to_string().replace('.', "");
        let trimmed = compact.trim_start_matches('0');
        if trimmed.is_empty() {
            "0".to_string()
        } else {
            trimmed.to_string()
        }
    }
    let mut payload = String::new();
    for (price, quantity) in asks.iter().take(10) {
        payload.push_str(&component(*price));
        payload.push_str(&component(*quantity));
    }
    for (price, quantity) in bids.iter().rev().take(10) {
        payload.push_str(&component(*price));
        payload.push_str(&component(*quantity));
    }
    crc32fast::hash(payload.as_bytes())
}

/// Run the complete public CEX collector until Ctrl-C.
pub async fn collect_cex_public(pool: PgPool, assets_raw: &str, poll_secs: u64, sample_ms: u64) {
    let assets: Vec<String> = assets_raw
        .split(',')
        .map(str::trim)
        .filter(|asset| !asset.is_empty())
        .map(str::to_uppercase)
        .collect();
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        signal.store(false, Ordering::SeqCst);
    });

    info!(assets = ?assets, poll_secs, sample_ms, "starting public CEX collector");
    tokio::join!(
        run_binance_futures(pool.clone(), assets.clone(), poll_secs, running.clone()),
        run_binance_liquidations(pool.clone(), assets.clone(), running.clone()),
        run_book_collector(
            pool.clone(),
            "okx",
            assets.clone(),
            sample_ms,
            running.clone()
        ),
        run_book_collector(
            pool.clone(),
            "bybit",
            assets.clone(),
            sample_ms,
            running.clone()
        ),
        run_book_collector(
            pool.clone(),
            "coinbase",
            assets.clone(),
            sample_ms,
            running.clone()
        ),
        run_book_collector(pool, "kraken", assets, sample_ms, running),
    );
}

async fn run_binance_futures(
    pool: PgPool,
    assets: Vec<String>,
    poll_secs: u64,
    running: Arc<AtomicBool>,
) {
    let source = match HttpReferenceSource::official(Duration::from_secs(15)) {
        Ok(source) => source,
        Err(error) => {
            warn!(%error, "failed to build shared Binance reference source");
            return;
        }
    };
    while running.load(Ordering::SeqCst) {
        let results = match fetch_binance_futures_poll(&source, &assets).await {
            Ok(results) => results,
            Err(error) => {
                warn!(%error, "failed to fetch Binance premium index snapshot");
                tokio::time::sleep(Duration::from_secs(poll_secs.max(1))).await;
                continue;
            }
        };
        for (symbol, result) in results {
            match result {
                Ok(tick) => {
                    if let Err(error) = persist_tick(&pool, &tick).await {
                        error!(%symbol, %error, "failed to persist Binance futures snapshot");
                    }
                }
                Err(error) => warn!(%symbol, %error, "failed to fetch Binance futures snapshot"),
            }
        }
        tokio::time::sleep(Duration::from_secs(poll_secs.max(1))).await;
    }
}

/// Fetch one all-market premium snapshot per poll, then select only the
/// configured symbols while retrieving their symbol-scoped reference rows.
async fn fetch_binance_futures_poll(
    source: &dyn ReferenceSource,
    assets: &[String],
) -> Result<Vec<(String, Result<NormalizedTick>)>> {
    if assets.is_empty() {
        return Ok(Vec::new());
    }
    let premium = source.premium_index().await.map_err(reference_error)?;
    let mut results = Vec::with_capacity(assets.len());
    for asset in assets {
        let symbol = format!("{asset}USDT");
        let result = fetch_binance_futures(source, &premium, &symbol).await;
        results.push((symbol, result));
    }
    Ok(results)
}

async fn fetch_binance_futures(
    source: &dyn ReferenceSource,
    premium: &TimedJson,
    symbol: &str,
) -> Result<NormalizedTick> {
    let open_interest_response = source
        .open_interest(symbol)
        .await
        .map_err(reference_error)?;
    let basis_response = source.basis(symbol, "5m").await.map_err(reference_error)?;
    let expected = std::collections::BTreeSet::from([symbol.to_ascii_uppercase()]);
    let premium_rows = premium
        .value
        .as_array()
        .ok_or_else(|| invalid("Binance premiumIndex response must be an array"))?;
    let raw_premium = premium_rows
        .iter()
        .find(|row| {
            row.get("symbol")
                .and_then(Value::as_str)
                .is_some_and(|row_symbol| row_symbol.eq_ignore_ascii_case(symbol))
        })
        .cloned()
        .ok_or_else(|| invalid("Binance premiumIndex returned no requested symbol"))?;
    let mut marks =
        mark_index_funding_observations(&premium.value, &expected, premium.received_at_ns)
            .map_err(|error| invalid(error.to_string()))?;
    let mark = marks
        .pop()
        .ok_or_else(|| invalid("Binance premiumIndex returned no requested symbol"))?;
    let open_interest = open_interest_observation(
        &open_interest_response.value,
        symbol,
        open_interest_response.received_at_ns,
    )
    .map_err(|error| invalid(error.to_string()))?;
    let basis = basis_observation(
        &basis_response.value,
        symbol,
        "5m",
        basis_response.received_at_ns,
    )
    .map_err(|error| invalid(error.to_string()))?;
    let raw = serde_json::json!({
        "premium_index": {
            "value": raw_premium,
            "received_at_ns": premium.received_at_ns
        },
        "open_interest": {
            "value": open_interest_response.value,
            "received_at_ns": open_interest_response.received_at_ns
        },
        "basis": {
            "value": basis_response.value,
            "received_at_ns": basis_response.received_at_ns
        }
    });
    normalize_binance_reference(&mark, &open_interest, &basis, raw)
}

/// Capture force-order notifications for the configured symbols. Binance's
/// per-symbol streams are a bounded observation sample and cannot establish
/// full-market liquidation coverage or a local execution/fill outcome.
async fn run_binance_liquidations(pool: PgPool, assets: Vec<String>, running: Arc<AtomicBool>) {
    let symbols: Vec<Symbol> = assets
        .iter()
        .map(|asset| Symbol::new(format!("{asset}USDT")))
        .collect();
    while running.load(Ordering::SeqCst) {
        let result = async {
            let mut socket = BinanceWebSocket::new().with_usdm();
            socket
                .connect_and_subscribe_force_orders(symbols.clone())
                .await
                .map_err(|error| invalid(error.to_string()))?;
            while running.load(Ordering::SeqCst) {
                let Some((bytes, metrics)) = socket
                    .receive_message_bytes_with_metrics()
                    .await
                    .map_err(|error| invalid(error.to_string()))?
                else {
                    break;
                };
                let received_at_us = metrics.received_at_unix_us.ok_or_else(|| {
                    invalid("Binance forceOrder frame omitted local receive clock")
                })?;
                let received_at_ns = received_at_us
                    .checked_mul(1_000)
                    .ok_or_else(|| invalid("Binance forceOrder receive clock overflow"))?;
                let raw: Value = serde_json::from_slice(&bytes)?;
                if raw.get("result").is_some() {
                    continue;
                }
                let observation = force_order_observation(&raw, received_at_ns)
                    .map_err(|error| invalid(error.to_string()))?;
                let tick = normalize_binance_liquidation(&observation, raw)?;
                persist_tick(&pool, &tick).await?;
            }
            Ok::<(), CexCollectorError>(())
        }
        .await;
        if let Err(error) = result {
            warn!(%error, "Binance liquidation stream disconnected");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn reference_error(error: BinanceReferenceError) -> CexCollectorError {
    invalid(error.to_string())
}

async fn run_book_collector(
    pool: PgPool,
    exchange: &'static str,
    assets: Vec<String>,
    sample_ms: u64,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::SeqCst) {
        if let Err(error) = run_book_connection(&pool, exchange, &assets, sample_ms, &running).await
        {
            warn!(exchange, %error, "CEX L2 stream disconnected");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

fn exchange_symbol(exchange: &str, asset: &str) -> String {
    match exchange {
        "okx" => format!("{asset}-USDT"),
        "bybit" => format!("{asset}USDT"),
        "coinbase" => format!("{asset}-USD"),
        "kraken" => format!("{asset}/USD"),
        _ => asset.to_string(),
    }
}

fn normalized_symbol(exchange: &str, asset: &str) -> String {
    if matches!(exchange, "coinbase" | "kraken") {
        format!("{asset}USD")
    } else {
        format!("{asset}USDT")
    }
}

async fn run_book_connection(
    pool: &PgPool,
    exchange: &str,
    assets: &[String],
    sample_ms: u64,
    running: &Arc<AtomicBool>,
) -> Result<()> {
    let url = match exchange {
        "okx" => OKX_WS,
        "bybit" => BYBIT_WS,
        "coinbase" => COINBASE_WS,
        "kraken" => KRAKEN_WS,
        _ => return Err(invalid(format!("unsupported exchange {exchange}"))),
    };
    let exchange_symbols: Vec<String> = assets
        .iter()
        .map(|asset| exchange_symbol(exchange, asset))
        .collect();
    let (socket, _) = connect_async(url).await?;
    let (mut write, mut read) = socket.split();
    for subscription in subscriptions(exchange, &exchange_symbols) {
        write
            .send(Message::Text(subscription.to_string().into()))
            .await?;
    }
    let mut books: HashMap<String, CexBook> = assets
        .iter()
        .map(|asset| {
            let venue_symbol = exchange_symbol(exchange, asset);
            (
                venue_symbol.clone(),
                CexBook::new(
                    exchange,
                    "spot",
                    &normalized_symbol(exchange, asset),
                    &venue_symbol,
                ),
            )
        })
        .collect();
    let mut last_persisted: HashMap<String, Instant> = HashMap::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    while running.load(Ordering::SeqCst) {
        tokio::select! {
            _ = heartbeat.tick() => {
                if let Some(ping) = heartbeat_message(exchange) {
                    write.send(ping).await?;
                }
            }
            message = read.next() => {
                let Some(message) = message else { break };
                match message? {
                    Message::Text(text) => {
                        if text.as_str() == "pong" { continue }
                        let raw: Value = serde_json::from_str(&text)?;
                        for (symbol, book) in &mut books {
                            if let Some(tick) = book.apply_message(&raw)? {
                                let due = last_persisted.get(symbol)
                                    .map(|last| last.elapsed() >= Duration::from_millis(sample_ms))
                                    .unwrap_or(true);
                                if due {
                                    persist_tick(pool, &tick).await?;
                                    last_persisted.insert(symbol.clone(), Instant::now());
                                }
                            }
                        }
                    }
                    Message::Ping(payload) => write.send(Message::Pong(payload)).await?,
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn subscriptions(exchange: &str, symbols: &[String]) -> Vec<Value> {
    match exchange {
        "okx" => vec![
            serde_json::json!({"op":"subscribe","args":symbols.iter().map(|symbol| serde_json::json!({"channel":"books5","instId":symbol})).collect::<Vec<_>>()}),
        ],
        "bybit" => vec![
            serde_json::json!({"op":"subscribe","args":symbols.iter().map(|symbol| format!("orderbook.50.{symbol}")).collect::<Vec<_>>()}),
        ],
        "coinbase" => vec![
            serde_json::json!({"type":"subscribe","channel":"level2","product_ids":symbols}),
            serde_json::json!({"type":"subscribe","channel":"heartbeats"}),
        ],
        "kraken" => vec![
            serde_json::json!({"method":"subscribe","params":{"channel":"book","symbol":symbols,"depth":10,"snapshot":true},"req_id":1}),
        ],
        _ => Vec::new(),
    }
}

fn heartbeat_message(exchange: &str) -> Option<Message> {
    match exchange {
        "okx" => Some(Message::Text("ping".into())),
        "bybit" => Some(Message::Text(
            serde_json::json!({"op":"ping"}).to_string().into(),
        )),
        "kraken" => Some(Message::Text(
            serde_json::json!({"method":"ping"}).to_string().into(),
        )),
        _ => None,
    }
}

async fn persist_tick(pool: &PgPool, tick: &NormalizedTick) -> Result<()> {
    let received_at = tick.received_at;
    sqlx::query(
        r#"INSERT INTO cex_public_market_ticks (
            exchange, market_type, symbol, exchange_symbol, kind, event_time,
            update_id, sequence_id, mark_price, index_price, funding_rate,
            open_interest, basis, basis_rate, annualized_basis_rate,
            next_funding_time, side, price, quantity, best_bid, best_ask,
            mid_price, spread_bps, obi_5, obi_10, bid_volume_5, ask_volume_5,
            bids, asks, source, dedupe_key, raw, typed_reference, coverage, received_at
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,
            $17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29,$30,$31,$32,$33,$34,$35
        ) ON CONFLICT (exchange, kind, exchange_symbol, dedupe_key) DO NOTHING"#,
    )
    .bind(&tick.exchange)
    .bind(&tick.market_type)
    .bind(&tick.symbol)
    .bind(&tick.exchange_symbol)
    .bind(&tick.kind)
    .bind(tick.event_time)
    .bind(tick.update_id)
    .bind(tick.sequence_id)
    .bind(tick.mark_price)
    .bind(tick.index_price)
    .bind(tick.funding_rate)
    .bind(tick.open_interest)
    .bind(tick.basis)
    .bind(tick.basis_rate)
    .bind(tick.annualized_basis_rate)
    .bind(tick.next_funding_time)
    .bind(&tick.side)
    .bind(tick.price)
    .bind(tick.quantity)
    .bind(tick.best_bid)
    .bind(tick.best_ask)
    .bind(tick.mid_price)
    .bind(tick.spread_bps)
    .bind(tick.obi_5)
    .bind(tick.obi_10)
    .bind(tick.bid_volume_5)
    .bind(tick.ask_volume_5)
    .bind(&tick.bids)
    .bind(&tick.asks)
    .bind(&tick.source)
    .bind(&tick.dedupe_key)
    .bind(&tick.raw)
    .bind(&tick.typed_reference)
    .bind(&tick.coverage)
    .bind(received_at)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use adapter_binance_data::{ReferenceResult, TimedJson, OFFICIAL_USDM_SOURCE_ORIGIN};
    use async_trait::async_trait;
    use rust_decimal_macros::dec;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use super::*;

    struct CountingReferenceSource {
        premium_calls: AtomicUsize,
        requested_symbols: Mutex<Vec<String>>,
    }

    impl CountingReferenceSource {
        fn timed_at(value: Value, received_at_ns: u64) -> TimedJson {
            TimedJson {
                value,
                received_at_ns,
            }
        }
    }

    #[async_trait]
    impl ReferenceSource for CountingReferenceSource {
        fn source_origin(&self) -> &str {
            OFFICIAL_USDM_SOURCE_ORIGIN
        }

        async fn server_time(&self) -> ReferenceResult<TimedJson> {
            Err(BinanceReferenceError::Unsupported {
                endpoint: "server_time".to_owned(),
            })
        }

        async fn exchange_info(&self) -> ReferenceResult<TimedJson> {
            Err(BinanceReferenceError::Unsupported {
                endpoint: "exchange_info".to_owned(),
            })
        }

        async fn premium_index(&self) -> ReferenceResult<TimedJson> {
            self.premium_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Self::timed_at(
                json!([
                    {"symbol":"BTCUSDT","markPrice":"101","indexPrice":"100","lastFundingRate":"0.0001","interestRate":"0.0001","nextFundingTime":1_700_028_800_000_u64,"time":1_700_000_000_000_u64,"unknownPremiumField":"preserve-me"},
                    {"symbol":"ETHUSDT","markPrice":"201","indexPrice":"200","lastFundingRate":"0.0001","interestRate":"0.0001","nextFundingTime":1_700_028_800_000_u64,"time":1_700_000_000_000_u64}
                ]),
                1_700_000_000_500_000_000,
            ))
        }

        async fn basis(&self, pair: &str, _period: &str) -> ReferenceResult<TimedJson> {
            self.requested_symbols
                .lock()
                .unwrap()
                .push(format!("basis:{pair}"));
            Ok(Self::timed_at(
                json!([{
                    "pair":pair,"contractType":"PERPETUAL","indexPrice":"100","futuresPrice":"101",
                    "basis":"1","basisRate":"0.01","annualizedBasisRate":"","timestamp":1_700_000_000_000_u64,
                    "unknownBasisField":"preserve-me"
                }]),
                1_700_000_000_520_000_000,
            ))
        }

        async fn open_interest(&self, symbol: &str) -> ReferenceResult<TimedJson> {
            self.requested_symbols
                .lock()
                .unwrap()
                .push(format!("open_interest:{symbol}"));
            Ok(Self::timed_at(
                json!({
                    "symbol":symbol,"openInterest":"12.3","time":1_700_000_000_000_u64,
                    "unknownOpenInterestField":"preserve-me"
                }),
                1_700_000_000_540_000_000,
            ))
        }
    }

    #[test]
    fn normalizes_binance_futures_and_liquidation_payloads() {
        let premium = json!([{
            "symbol":"BTCUSDT","markPrice":"11793.63104562","indexPrice":"11781.80495970",
            "lastFundingRate":"0.00038246","interestRate":"0.0001",
            "nextFundingTime":1597392000000_u64,"time":1597370495002_u64
        }]);
        let oi = json!({"openInterest":"10659.509","symbol":"BTCUSDT","time":1597370495002_i64});
        let basis = json!([{"pair":"BTCUSDT","contractType":"PERPETUAL",
            "indexPrice":"11781.80495970","futuresPrice":"11795.74550915",
            "basis":"13.94054945","basisRate":"0.0004","annualizedBasisRate":"0.035",
            "timestamp":1597370495002_u64}]);
        let received_at_ns = 1_597_370_495_500_000_000;
        let expected = std::collections::BTreeSet::from(["BTCUSDT".to_string()]);
        let mark = mark_index_funding_observations(&premium, &expected, received_at_ns)
            .unwrap()
            .pop()
            .unwrap();
        let oi = open_interest_observation(&oi, "BTCUSDT", received_at_ns).unwrap();
        let basis = basis_observation(&basis, "BTCUSDT", "5m", received_at_ns).unwrap();
        let tick = normalize_binance_reference(
            &mark,
            &oi,
            &basis,
            json!({"premium_index": premium, "open_interest": oi.clone(), "basis": basis.clone()}),
        )
        .unwrap();
        assert_eq!(tick.mark_price, Some(dec!(11793.63104562)));
        assert_eq!(tick.open_interest, Some(dec!(10659.509)));
        assert_eq!(tick.basis_rate, Some(dec!(0.0004)));
        assert_eq!(tick.annualized_basis_rate, Some(dec!(0.035)));
        assert_eq!(
            tick.typed_reference.as_ref().unwrap()["basis"]["period"],
            "5m"
        );
        assert_eq!(
            tick.received_at.timestamp_millis(),
            (received_at_ns / 1_000_000) as i64
        );
        assert_eq!(
            tick.basis_event_time.unwrap().timestamp_millis(),
            1597370495002
        );

        let liquidation = json!({"stream":"btcusdt@forceOrder","data":{"e":"forceOrder","E":1568014460893_i64,"o":{
            "s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"0.014","p":"9910",
            "ap":"9910","X":"FILLED","l":"0.014","z":"0.014","T":1568014460893_i64
        }}});
        let observation = force_order_observation(&liquidation, 1_568_014_460_900_000_000).unwrap();
        let tick = normalize_binance_liquidation(&observation, liquidation.clone()).unwrap();
        assert_eq!(tick.kind, "liquidation");
        assert_eq!(tick.side.as_deref(), Some("SELL"));
        assert_eq!(tick.quantity, Some(dec!(0.014)));
        assert_eq!(
            tick.force_order_reference.as_ref().unwrap().status,
            "FILLED"
        );
        assert_eq!(
            tick.coverage.as_deref(),
            Some("configured_symbols_only_not_full_market")
        );
        assert_eq!(
            tick.typed_reference.as_ref().unwrap()["coverage"],
            "configured_symbols_only_not_full_market"
        );

        let mut opposite_side = liquidation.clone();
        opposite_side["data"]["o"]["S"] = json!("BUY");
        let opposite_observation =
            force_order_observation(&opposite_side, 1_568_014_460_900_000_000).unwrap();
        let opposite_tick =
            normalize_binance_liquidation(&opposite_observation, opposite_side).unwrap();
        assert_ne!(tick.dedupe_key, opposite_tick.dedupe_key);

        let replay_observation = force_order_observation(
            &json!({"stream":"btcusdt@forceOrder","data":{"e":"forceOrder","E":1568014460893_i64,"o":{
                "s":"BTCUSDT","S":"SELL","o":"LIMIT","f":"IOC","q":"0.014","p":"9910",
                "ap":"9910","X":"FILLED","l":"0.014","z":"0.014","T":1568014460893_i64
            }}}),
            1_568_014_461_000_000_000,
        )
        .unwrap();
        let replay_tick =
            normalize_binance_liquidation(&replay_observation, json!({"replayed":true})).unwrap();
        assert_eq!(tick.dedupe_key, replay_tick.dedupe_key);
    }

    #[tokio::test]
    async fn futures_poll_fetches_all_market_premium_once_and_selects_requested_symbols() {
        let source = CountingReferenceSource {
            premium_calls: AtomicUsize::new(0),
            requested_symbols: Mutex::new(Vec::new()),
        };
        let assets = vec!["BTC".to_owned(), "ETH".to_owned()];
        let results = fetch_binance_futures_poll(&source, &assets).await.unwrap();

        assert_eq!(source.premium_calls.load(Ordering::SeqCst), 1);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "BTCUSDT");
        assert_eq!(results[1].0, "ETHUSDT");
        assert!(results.iter().all(|(_, result)| result.is_ok()));
        let btc_tick = results[0].1.as_ref().unwrap();
        assert_eq!(btc_tick.raw["premium_index"]["value"]["symbol"], "BTCUSDT");
        assert_eq!(
            btc_tick.raw["premium_index"]["value"]["unknownPremiumField"],
            "preserve-me"
        );
        assert_eq!(
            btc_tick.raw["open_interest"]["value"]["unknownOpenInterestField"],
            "preserve-me"
        );
        assert_eq!(
            btc_tick.raw["basis"]["value"][0]["unknownBasisField"],
            "preserve-me"
        );
        assert_eq!(
            btc_tick.raw["premium_index"]["received_at_ns"],
            1_700_000_000_500_000_000_u64
        );
        assert_eq!(
            btc_tick.raw["open_interest"]["received_at_ns"],
            1_700_000_000_540_000_000_u64
        );
        assert_eq!(
            btc_tick.raw["basis"]["received_at_ns"],
            1_700_000_000_520_000_000_u64
        );
        let typed = btc_tick.typed_reference.as_ref().unwrap();
        assert_eq!(
            typed["mark_index_funding"]["received_at_ns"],
            1_700_000_000_500_000_000_u64
        );
        assert_eq!(
            typed["open_interest"]["received_at_ns"],
            1_700_000_000_540_000_000_u64
        );
        assert_eq!(
            typed["basis"]["received_at_ns"],
            1_700_000_000_520_000_000_u64
        );
        let mut requested = source.requested_symbols.lock().unwrap().clone();
        requested.sort();
        assert_eq!(
            requested,
            vec![
                "basis:BTCUSDT".to_owned(),
                "basis:ETHUSDT".to_owned(),
                "open_interest:BTCUSDT".to_owned(),
                "open_interest:ETHUSDT".to_owned()
            ]
        );
    }

    #[test]
    fn normalizes_okx_and_bybit_books_with_delta_reset() {
        let mut okx = CexBook::new("okx", "spot", "BTCUSDT", "BTC-USDT");
        let tick = okx.apply_message(&json!({"arg":{"channel":"books5","instId":"BTC-USDT"},"data":[{
            "asks":[["101","2","0","1"]],"bids":[["99","3","0","1"]],"ts":"1597026383085","seqId":123
        }]})).unwrap().unwrap();
        assert_eq!(tick.best_bid, Some(dec!(99)));
        assert_eq!(tick.obi_5, Some(dec!(0.2)));

        let mut bybit = CexBook::new("bybit", "spot", "BTCUSDT", "BTCUSDT");
        bybit
            .apply_message(
                &json!({"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1672304484978_i64,
            "data":{"s":"BTCUSDT","b":[["100","2"],["99","1"]],"a":[["101","2"]],"u":10,"seq":20}}),
            )
            .unwrap();
        let tick = bybit
            .apply_message(
                &json!({"topic":"orderbook.50.BTCUSDT","type":"delta","ts":1672304485078_i64,
            "data":{"s":"BTCUSDT","b":[["100","0"],["98","4"]],"a":[],"u":11,"seq":21}}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(tick.best_bid, Some(dec!(99)));
        assert_eq!(tick.bid_volume_5, Some(dec!(5)));
        let tick = bybit
            .apply_message(
                &json!({"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1672304485178_i64,
            "data":{"s":"BTCUSDT","b":[["97","1"]],"a":[["98","1"]],"u":1,"seq":22}}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(tick.best_bid, Some(dec!(97)));
    }

    #[test]
    fn normalizes_coinbase_and_kraken_books() {
        let mut coinbase = CexBook::new("coinbase", "spot", "BTCUSD", "BTC-USD");
        let tick = coinbase.apply_message(&json!({"channel":"l2_data","timestamp":"2023-02-09T20:32:50.714964855Z","sequence_num":1,
            "events":[{"type":"snapshot","product_id":"BTC-USD","updates":[
                {"side":"bid","event_time":"2023-02-09T20:32:50Z","price_level":"99","new_quantity":"3"},
                {"side":"offer","event_time":"2023-02-09T20:32:50Z","price_level":"101","new_quantity":"2"}
            ]}]})).unwrap().unwrap();
        assert_eq!(tick.spread_bps, Some(dec!(200)));

        let mut kraken = CexBook::new("kraken", "spot", "BTCUSD", "BTC/USD");
        let tick = kraken.apply_message(&json!({"channel":"book","type":"snapshot","data":[{
            "symbol":"BTC/USD","bids":[{"price":"99.00","qty":"3.000"}],"asks":[{"price":"101.00","qty":"2.000"}],
            "checksum":4104258857_u32,"timestamp":"2023-10-06T17:35:55.440295Z"
        }]})).unwrap().unwrap();
        assert_eq!(tick.best_ask, Some(dec!(101.0)));
        assert_eq!(tick.update_id, Some(4104258857));
    }
}
