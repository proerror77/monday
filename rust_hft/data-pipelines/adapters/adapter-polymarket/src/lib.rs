//! Polymarket public CLOB market data.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use polymarket_client_sdk::clob::types::Side as PolymarketSide;
use polymarket_client_sdk::clob::ws::types::response::{PriceChangeBatchEntry, WsMessage};
use polymarket_client_sdk::clob::ws::SubscriptionRequest;
use polymarket_client_sdk::types::U256;
use ports::Trade;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, MissedTickBehavior};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

pub use hft_core::{
    HftError, HftResult, MarketDataTimestamps, Price, Quantity, Side, Symbol, VenueId,
};
pub use ports::{
    BookLevel, BookUpdate, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream,
};

pub const DEFAULT_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com";
pub const FAILURE_CAPTURE_ENV: &str = "MONDAY_POLYMARKET_CLOB_FAILURE_CAPTURE_PATH";
const MAX_FAILURE_CAPTURE_BYTES: usize = 1_048_576;
const EVENT_QUEUE_CAPACITY: usize = 4_096;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct ConnectionState {
    connected: AtomicBool,
    enabled: AtomicBool,
    generation: AtomicU64,
    last_heartbeat: AtomicU64,
}

/// Monday-native Polymarket market stream. Symbols are decimal outcome token IDs.
pub struct PolymarketMarketStream {
    ws_url: String,
    state: Arc<ConnectionState>,
    failure_capture_path: Option<PathBuf>,
}

impl Default for PolymarketMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl PolymarketMarketStream {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ws_url: DEFAULT_WS_URL.to_string(),
            state: Arc::new(ConnectionState::default()),
            failure_capture_path: std::env::var_os(FAILURE_CAPTURE_ENV).map(PathBuf::from),
        }
    }

    #[must_use]
    pub fn with_ws_url(mut self, url: impl Into<String>) -> Self {
        self.ws_url = url.into();
        self
    }

    /// Configure an optional, bounded, create-once capture for a malformed
    /// crossed-book frame. This is diagnostic evidence only and never changes
    /// the stream's fail-closed reconnect behavior.
    #[must_use]
    pub fn with_failure_capture_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.failure_capture_path = Some(path.into());
        self
    }
}

#[derive(Default)]
struct BookState {
    ready: HashSet<String>,
    sequences: HashMap<String, u64>,
    timestamps: HashMap<String, i64>,
    books: HashMap<String, PolymarketBook>,
}

impl BookState {
    fn reset(&mut self) {
        self.ready.clear();
        self.sequences.clear();
        self.timestamps.clear();
        self.books.clear();
    }

    #[cfg(test)]
    fn next_sequence(&mut self, symbol: &str) -> u64 {
        let sequence = self.sequences.entry(symbol.to_string()).or_default();
        *sequence = sequence.saturating_add(1);
        *sequence
    }

    fn next_sequence_value(&self, symbol: &str) -> u64 {
        self.sequences
            .get(symbol)
            .copied()
            .unwrap_or_default()
            .saturating_add(1)
    }

    fn commit_sequence(&mut self, symbol: &str, sequence: u64) {
        self.sequences.insert(symbol.to_string(), sequence);
    }
}

/// Stateful book projection for one Polymarket outcome token.
///
/// `MarketStream` deliberately emits snapshots and level deltas so the shared
/// engine can apply the venue-native event contract. Consumers that need a
/// complete depth image (for example the prediction-market DB sink) use this
/// canonical projection instead of maintaining a second CLOB parser or merge
/// state machine.
#[derive(Debug, Clone, Default)]
pub struct PolymarketBook {
    symbol: Option<Symbol>,
    bids: BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
    asks: BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
    sequence: u64,
    ready: bool,
    dirty: bool,
}

impl PolymarketBook {
    /// Whether a fresh snapshot has initialized this token's book.
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        self.ready
    }

    /// Whether this token has been invalidated by an authoritative provider
    /// BBA that could not be reconciled with the cached depth.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Drop all depth and synchronization state for this token.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Invalidate the cached depth while retaining a dirty marker. A dirty
    /// token remains unusable until a fresh full snapshot arrives.
    pub fn invalidate(&mut self) {
        self.bids.clear();
        self.asks.clear();
        self.ready = false;
        self.dirty = true;
        self.sequence = 0;
    }

    fn validate_level(level: &BookLevel) -> HftResult<()> {
        if !(rust_decimal::Decimal::ZERO..=rust_decimal::Decimal::ONE).contains(&level.price.0)
            || level.quantity.0 < rust_decimal::Decimal::ZERO
        {
            return Err(HftError::Parse(format!(
                "Polymarket book level is out of range price={} quantity={}",
                level.price, level.quantity
            )));
        }
        Ok(())
    }

    fn apply_levels(
        levels: &[BookLevel],
        target: &mut BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
    ) -> HftResult<()> {
        for level in levels {
            Self::validate_level(level)?;
            if level.quantity.0.is_zero() {
                target.remove(&level.price.0);
            } else {
                target.insert(level.price.0, level.quantity.0);
            }
        }
        Ok(())
    }

    fn validate_not_crossed(
        bids: &BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
        asks: &BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
    ) -> HftResult<()> {
        if let Some((best_bid, best_ask)) = bids
            .last_key_value()
            .zip(asks.first_key_value())
            .map(|((bid, _), (ask, _))| (*bid, *ask))
        {
            if best_bid > best_ask {
                return Err(HftError::Parse(format!(
                    "Polymarket book is crossed bid={best_bid} ask={best_ask}"
                )));
            }
        }
        Ok(())
    }

    fn requires_reported_bid_level(price: rust_decimal::Decimal) -> bool {
        // The market channel uses zero as the empty-bid sentinel. Every other
        // value in the protocol range, including 0.01, is an authoritative
        // best bid that must be present in the projected depth.
        price != rust_decimal::Decimal::ZERO
    }

    fn requires_reported_ask_level(price: rust_decimal::Decimal) -> bool {
        // The market channel uses one as the empty-ask sentinel. Every other
        // value in the protocol range, including 0.99, is an authoritative
        // best ask that must be present in the projected depth.
        price != rust_decimal::Decimal::ONE
    }

    fn reconcile_reported_bba(
        bids: &mut BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
        asks: &mut BTreeMap<rust_decimal::Decimal, rust_decimal::Decimal>,
        best_bid: Option<rust_decimal::Decimal>,
        best_ask: Option<rust_decimal::Decimal>,
    ) -> bool {
        if let Some(best_bid) = best_bid {
            bids.retain(|price, _| *price <= best_bid);
        }
        if let Some(best_ask) = best_ask {
            asks.retain(|price, _| *price >= best_ask);
        }

        let bid_missing = best_bid.is_some_and(|price| {
            Self::requires_reported_bid_level(price) && !bids.contains_key(&price)
        });
        let ask_missing = best_ask.is_some_and(|price| {
            Self::requires_reported_ask_level(price) && !asks.contains_key(&price)
        });
        !bid_missing && !ask_missing
    }

    fn snapshot(
        &self,
        timestamp: u64,
        sequence: u64,
        source_venue: Option<VenueId>,
        timestamps: MarketDataTimestamps,
    ) -> MarketSnapshot {
        MarketSnapshot {
            symbol: self
                .symbol
                .clone()
                .expect("a ready Polymarket book always has a symbol"),
            timestamp,
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(price, quantity)| BookLevel {
                    price: Price(*price),
                    quantity: Quantity(*quantity),
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(price, quantity)| BookLevel {
                    price: Price(*price),
                    quantity: Quantity(*quantity),
                })
                .collect(),
            sequence,
            source_venue,
            timestamps,
        }
    }

    /// Apply one canonical event and return the complete post-event depth image.
    ///
    /// A delta before a snapshot, a sequence regression, an invalid level, or
    /// a crossed result is rejected fail-closed. Disconnects clear the token;
    /// callers must wait for the next snapshot before publishing the returned
    /// image as usable.
    pub fn apply(&mut self, event: &MarketEvent) -> HftResult<Option<MarketSnapshot>> {
        match event {
            MarketEvent::Snapshot(snapshot) => {
                let mut bids = BTreeMap::new();
                let mut asks = BTreeMap::new();
                Self::apply_levels(&snapshot.bids, &mut bids)?;
                Self::apply_levels(&snapshot.asks, &mut asks)?;
                Self::validate_not_crossed(&bids, &asks)?;
                self.symbol = Some(snapshot.symbol.clone());
                self.bids = bids;
                self.asks = asks;
                self.sequence = snapshot.sequence;
                self.ready = true;
                self.dirty = false;
                Ok(Some(self.snapshot(
                    snapshot.timestamp,
                    snapshot.sequence,
                    snapshot.source_venue,
                    snapshot.timestamps,
                )))
            }
            MarketEvent::Update(update) => {
                if !self.ready || self.dirty {
                    return Err(HftError::Network(format!(
                        "Polymarket delta arrived before snapshot for {}",
                        update.symbol
                    )));
                }
                if self.symbol.as_ref() != Some(&update.symbol) {
                    return Err(HftError::Network(format!(
                        "Polymarket delta symbol changed from {} to {}",
                        self.symbol.as_ref().expect("ready book has symbol"),
                        update.symbol
                    )));
                }
                if update.sequence <= self.sequence {
                    return Err(HftError::Network(format!(
                        "Polymarket book sequence regressed from {} to {}",
                        self.sequence, update.sequence
                    )));
                }
                let mut bids = self.bids.clone();
                let mut asks = self.asks.clone();
                Self::apply_levels(&update.bids, &mut bids)?;
                Self::apply_levels(&update.asks, &mut asks)?;
                Self::validate_not_crossed(&bids, &asks)?;
                self.bids = bids;
                self.asks = asks;
                self.sequence = update.sequence;
                Ok(Some(self.snapshot(
                    update.timestamp,
                    update.sequence,
                    update.source_venue,
                    update.timestamps,
                )))
            }
            MarketEvent::Disconnect { .. } => {
                self.clear();
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Apply all level changes for one wire price-change frame atomically.
    ///
    /// The provider's BBA is reconciled only after every change in the frame
    /// has been staged. This permits a valid batch to pass through a transient
    /// crossed intermediate state while still invalidating a token when the
    /// final authoritative BBA is missing from the staged depth.
    pub fn apply_batch(
        &mut self,
        update: &BookUpdate,
        best_bid: Option<rust_decimal::Decimal>,
        best_ask: Option<rust_decimal::Decimal>,
    ) -> HftResult<Option<MarketSnapshot>> {
        if !self.ready || self.dirty {
            return Err(HftError::Network(format!(
                "Polymarket delta arrived before a fresh snapshot for {}",
                update.symbol
            )));
        }
        if self.symbol.as_ref() != Some(&update.symbol) {
            return Err(HftError::Network(format!(
                "Polymarket delta symbol changed from {} to {}",
                self.symbol.as_ref().expect("ready book has symbol"),
                update.symbol
            )));
        }
        if update.sequence <= self.sequence {
            return Err(HftError::Network(format!(
                "Polymarket book sequence regressed from {} to {}",
                self.sequence, update.sequence
            )));
        }
        if best_bid.is_some_and(|price| {
            !(rust_decimal::Decimal::ZERO..=rust_decimal::Decimal::ONE).contains(&price)
        }) || best_ask.is_some_and(|price| {
            !(rust_decimal::Decimal::ZERO..=rust_decimal::Decimal::ONE).contains(&price)
        }) {
            return Err(HftError::Parse(
                "Polymarket reported BBA is out of range".to_string(),
            ));
        }
        if best_bid.zip(best_ask).is_some_and(|(bid, ask)| bid > ask) {
            return Err(HftError::Parse(
                "Polymarket price-change batch produced a crossed book".to_string(),
            ));
        }

        let mut bids = self.bids.clone();
        let mut asks = self.asks.clone();
        Self::apply_levels(&update.bids, &mut bids)?;
        Self::apply_levels(&update.asks, &mut asks)?;
        if !Self::reconcile_reported_bba(&mut bids, &mut asks, best_bid, best_ask) {
            self.invalidate();
            return Ok(None);
        }
        Self::validate_not_crossed(&bids, &asks)?;
        self.bids = bids;
        self.asks = asks;
        self.sequence = update.sequence;
        Ok(Some(self.snapshot(
            update.timestamp,
            update.sequence,
            update.source_venue,
            update.timestamps,
        )))
    }

    fn delta_from(&self, previous: &Self) -> (Vec<BookLevel>, Vec<BookLevel>) {
        let mut bids = self
            .bids
            .iter()
            .filter(|(price, quantity)| previous.bids.get(price) != Some(quantity))
            .map(|(price, quantity)| BookLevel {
                price: Price(*price),
                quantity: Quantity(*quantity),
            })
            .collect::<Vec<_>>();
        bids.extend(
            previous
                .bids
                .keys()
                .filter(|price| !self.bids.contains_key(price))
                .map(|price| BookLevel {
                    price: Price(*price),
                    quantity: Quantity(rust_decimal::Decimal::ZERO),
                }),
        );
        bids.sort_by_key(|level| Reverse(level.price));

        let mut asks = self
            .asks
            .iter()
            .filter(|(price, quantity)| previous.asks.get(price) != Some(quantity))
            .map(|(price, quantity)| BookLevel {
                price: Price(*price),
                quantity: Quantity(*quantity),
            })
            .collect::<Vec<_>>();
        asks.extend(
            previous
                .asks
                .keys()
                .filter(|price| !self.asks.contains_key(price))
                .map(|price| BookLevel {
                    price: Price(*price),
                    quantity: Quantity(rust_decimal::Decimal::ZERO),
                }),
        );
        asks.sort_by_key(|level| level.price);
        (bids, asks)
    }
}

fn market_ws_url(endpoint: &str) -> HftResult<String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(HftError::Config(
            "Polymarket WebSocket endpoint is empty".to_string(),
        ));
    }
    let mut url = Url::parse(endpoint).map_err(|error| {
        HftError::Config(format!("Polymarket WebSocket endpoint is invalid: {error}"))
    })?;
    if url.scheme() != "wss" || url.host_str().is_none() {
        return Err(HftError::Config(
            "Polymarket WebSocket endpoint must be a wss URL with a host".to_string(),
        ));
    }
    url.set_path("/ws/market");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

fn token_ids(symbols: &[Symbol]) -> HftResult<(Vec<U256>, HashMap<String, Symbol>)> {
    let mut ids = Vec::with_capacity(symbols.len());
    let mut by_id = HashMap::with_capacity(symbols.len());
    for symbol in symbols {
        let raw = symbol.as_str().trim();
        let id = U256::from_str(raw).map_err(|error| {
            HftError::Config(format!(
                "Polymarket symbol must be a decimal outcome token ID ({raw}): {error}"
            ))
        })?;
        if id.to_string() != raw {
            return Err(HftError::Config(format!(
                "Polymarket token ID must use canonical decimal form: {raw}"
            )));
        }
        if by_id.insert(raw.to_string(), symbol.clone()).is_none() {
            ids.push(id);
        }
    }
    Ok((ids, by_id))
}

fn timestamp_micros(milliseconds: i64) -> HftResult<u64> {
    u64::try_from(milliseconds)
        .ok()
        .and_then(|value| value.checked_mul(1_000))
        .ok_or_else(|| HftError::Parse("Polymarket timestamp is out of range".to_string()))
}

fn level(price: rust_decimal::Decimal, size: rust_decimal::Decimal) -> HftResult<BookLevel> {
    if price < rust_decimal::Decimal::ZERO
        || price > rust_decimal::Decimal::ONE
        || size < rust_decimal::Decimal::ZERO
    {
        return Err(HftError::Parse(format!(
            "Polymarket invalid book level price={price} size={size}"
        )));
    }
    Ok(BookLevel {
        price: Price(price),
        quantity: Quantity(size),
    })
}

fn side(side: PolymarketSide) -> HftResult<Side> {
    match side {
        PolymarketSide::Buy => Ok(Side::Buy),
        PolymarketSide::Sell => Ok(Side::Sell),
        PolymarketSide::Unknown => Err(HftError::Parse(
            "Polymarket returned an unknown side".to_string(),
        )),
        _ => Err(HftError::Parse(
            "Polymarket returned an unsupported side".to_string(),
        )),
    }
}

/// Strictly parses the three market messages Monday consumes. Invalid book/delta/trade frames are
/// surfaced instead of being silently skipped; unrelated CLOB event types are ignored.
fn parse_messages(text: &str) -> HftResult<Vec<WsMessage>> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| HftError::Parse(format!("Polymarket WS JSON: {error}")))?;
    let values: Vec<Value> = match value {
        Value::Array(values) => values,
        value @ Value::Object(_) => vec![value],
        _ => return Ok(Vec::new()),
    };

    values
        .into_iter()
        .filter_map(|value| {
            let event_type = value.get("event_type").and_then(Value::as_str)?;
            matches!(event_type, "book" | "price_change" | "last_trade_price").then_some(value)
        })
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| HftError::Parse(format!("Polymarket WS market message: {error}")))
        })
        .collect()
}

fn validate_price_change(entry: &PriceChangeBatchEntry) -> HftResult<()> {
    let Some(size) = entry.size else {
        // Keep the missing-size path in convert_message so it can emit a venue
        // disconnect and force a fresh snapshot, matching the stream contract.
        return Ok(());
    };
    let invalid_best = entry.best_bid.is_some_and(|price| {
        !(rust_decimal::Decimal::ZERO..=rust_decimal::Decimal::ONE).contains(&price)
    }) || entry.best_ask.is_some_and(|price| {
        !(rust_decimal::Decimal::ZERO..=rust_decimal::Decimal::ONE).contains(&price)
    });
    if size < rust_decimal::Decimal::ZERO
        || !(rust_decimal::Decimal::ZERO..=rust_decimal::Decimal::ONE).contains(&entry.price)
        || invalid_best
    {
        return Err(HftError::Parse(format!(
            "Polymarket price change contains an invalid field for {}",
            entry.asset_id
        )));
    }
    if entry
        .best_bid
        .zip(entry.best_ask)
        .is_some_and(|(best_bid, best_ask)| best_bid > best_ask)
    {
        return Err(HftError::Parse(
            "Polymarket price-change batch produced a crossed book".to_string(),
        ));
    }
    match side(entry.side)? {
        Side::Buy
            if size > rust_decimal::Decimal::ZERO
                && entry.best_bid.is_some_and(|best| entry.price > best) =>
        {
            return Err(HftError::Parse(format!(
                "Polymarket buy price exceeds reported best bid for {}",
                entry.asset_id
            )));
        }
        Side::Sell
            if size > rust_decimal::Decimal::ZERO
                && entry.best_ask.is_some_and(|best| entry.price < best) =>
        {
            return Err(HftError::Parse(format!(
                "Polymarket sell price is below reported best ask for {}",
                entry.asset_id
            )));
        }
        _ => {}
    }
    Ok(())
}

fn persist_failure_payload(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FAILURE_CAPTURE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("capture payload exceeds {MAX_FAILURE_CAPTURE_BYTES} bytes"),
        ));
    }
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture path must be absolute",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture path has no parent directory",
        )
    })?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || fs::canonicalize(parent)? != parent
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture parent must be a direct canonical directory",
        ));
    }

    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(payload)?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn capture_crossed_failure(path: Option<&Path>, payload: &[u8], error: &HftError) {
    if !error.to_string().contains("crossed") {
        return;
    }
    let Some(path) = path else {
        return;
    };
    let _ = persist_failure_payload(path, payload);
}

fn venue_disconnect(reason: impl Into<String>) -> MarketEvent {
    MarketEvent::Disconnect {
        reason: reason.into(),
        source_venue: Some(VenueId::POLYMARKET),
        symbol: None,
    }
}

fn token_disconnect(symbol: Symbol, reason: impl Into<String>) -> MarketEvent {
    MarketEvent::Disconnect {
        reason: reason.into(),
        source_venue: Some(VenueId::POLYMARKET),
        symbol: Some(symbol),
    }
}

struct PriceChangeBatch {
    token: String,
    symbol: Symbol,
    bids: Vec<BookLevel>,
    asks: Vec<BookLevel>,
    best_bid: Option<rust_decimal::Decimal>,
    best_ask: Option<rust_decimal::Decimal>,
}

struct StagedPriceChangeBatch {
    batch: PriceChangeBatch,
    previous: PolymarketBook,
    staged: PolymarketBook,
    sequence: u64,
}

fn convert_message(
    message: WsMessage,
    symbols: &HashMap<String, Symbol>,
    state: &mut BookState,
) -> HftResult<Vec<MarketEvent>> {
    match message {
        WsMessage::Book(book) => {
            let token = book.asset_id.to_string();
            let Some(symbol) = symbols.get(&token).cloned() else {
                return Ok(Vec::new());
            };
            let timestamp = timestamp_micros(book.timestamp)?;
            let mut bids = book
                .bids
                .into_iter()
                .map(|value| level(value.price, value.size))
                .collect::<HftResult<Vec<_>>>()?;
            let mut asks = book
                .asks
                .into_iter()
                .map(|value| level(value.price, value.size))
                .collect::<HftResult<Vec<_>>>()?;
            bids.sort_by_key(|value| Reverse(value.price));
            asks.sort_by_key(|value| value.price);
            let sequence = state.next_sequence_value(&token);
            let event = MarketEvent::Snapshot(MarketSnapshot {
                symbol,
                timestamp,
                bids,
                asks,
                sequence,
                source_venue: Some(VenueId::POLYMARKET),
                timestamps: Default::default(),
            });
            state
                .books
                .entry(token.clone())
                .or_default()
                .apply(&event)?;
            state.ready.insert(token.clone());
            state.timestamps.insert(token.clone(), book.timestamp);
            state.commit_sequence(&token, sequence);
            Ok(vec![event])
        }
        WsMessage::PriceChange(change) => {
            let timestamp_ms = change.timestamp;
            let timestamp = timestamp_micros(timestamp_ms)?;
            for entry in &change.price_changes {
                if entry.size.is_none() {
                    return Ok(vec![venue_disconnect(format!(
                        "Polymarket delta omitted size for {}",
                        entry.asset_id
                    ))]);
                }
                validate_price_change(entry)?;
            }

            let mut batches = Vec::new();
            let mut batch_indices = HashMap::new();
            for entry in change.price_changes {
                let token = entry.asset_id.to_string();
                let Some(symbol) = symbols.get(&token).cloned() else {
                    continue;
                };
                let Some(book) = state.books.get(&token) else {
                    return Ok(vec![venue_disconnect(format!(
                        "Polymarket delta arrived before snapshot for {token}"
                    ))]);
                };
                if !book.is_ready() && !book.is_dirty() {
                    return Ok(vec![venue_disconnect(format!(
                        "Polymarket delta arrived before snapshot for {token}"
                    ))]);
                }
                if state
                    .timestamps
                    .get(&token)
                    .is_some_and(|last| timestamp_ms < *last)
                {
                    return Err(HftError::Parse(format!(
                        "Polymarket price-change source time moved backwards for {token}"
                    )));
                }
                let size = entry
                    .size
                    .expect("missing-size price changes return before batching");
                let changed_level = level(entry.price, size)?;
                let index = match batch_indices.get(&token).copied() {
                    Some(index) => index,
                    None => {
                        let index = batches.len();
                        batch_indices.insert(token.clone(), index);
                        batches.push(PriceChangeBatch {
                            token: token.clone(),
                            symbol,
                            bids: Vec::new(),
                            asks: Vec::new(),
                            best_bid: None,
                            best_ask: None,
                        });
                        index
                    }
                };
                let batch = &mut batches[index];
                match side(entry.side)? {
                    Side::Buy => batch.bids.push(changed_level),
                    Side::Sell => batch.asks.push(changed_level),
                }
                if entry.best_bid.is_some() {
                    batch.best_bid = entry.best_bid;
                }
                if entry.best_ask.is_some() {
                    batch.best_ask = entry.best_ask;
                };
            }

            let mut staged_batches = Vec::with_capacity(batches.len());
            for batch in batches {
                let current = state
                    .books
                    .get(&batch.token)
                    .expect("book readiness was checked before staging");
                let previous = current.clone();
                let mut staged = previous.clone();
                let sequence = state.next_sequence_value(&batch.token);
                if !current.is_dirty() {
                    let update = BookUpdate {
                        symbol: batch.symbol.clone(),
                        timestamp,
                        bids: batch.bids.clone(),
                        asks: batch.asks.clone(),
                        first_sequence: None,
                        sequence,
                        is_snapshot: false,
                        source_venue: Some(VenueId::POLYMARKET),
                        timestamps: Default::default(),
                    };
                    let _ = staged.apply_batch(&update, batch.best_bid, batch.best_ask)?;
                }
                staged_batches.push(StagedPriceChangeBatch {
                    batch,
                    previous,
                    staged,
                    sequence,
                });
            }

            let mut events = Vec::with_capacity(staged_batches.len());
            for staged_batch in staged_batches {
                let StagedPriceChangeBatch {
                    batch,
                    previous,
                    staged,
                    sequence,
                } = staged_batch;
                if staged.is_dirty() || !staged.is_ready() {
                    state.ready.remove(&batch.token);
                    state.books.insert(batch.token, staged);
                    events.push(token_disconnect(
                        batch.symbol,
                        "Polymarket reported BBA is absent from the canonical depth",
                    ));
                    continue;
                }
                let (bids, asks) = staged.delta_from(&previous);
                state.ready.insert(batch.token.clone());
                state.books.insert(batch.token.clone(), staged);
                state.commit_sequence(&batch.token, sequence);
                state.timestamps.insert(batch.token.clone(), timestamp_ms);
                events.push(MarketEvent::Update(BookUpdate {
                    symbol: batch.symbol,
                    timestamp,
                    bids,
                    asks,
                    first_sequence: None,
                    sequence,
                    is_snapshot: false,
                    source_venue: Some(VenueId::POLYMARKET),
                    timestamps: Default::default(),
                }));
            }
            Ok(events)
        }
        WsMessage::LastTradePrice(trade) => {
            let token = trade.asset_id.to_string();
            let Some(symbol) = symbols.get(&token).cloned() else {
                return Ok(Vec::new());
            };
            let quantity = trade.size.ok_or_else(|| {
                HftError::Parse(format!("Polymarket last trade omitted size for {token}"))
            })?;
            let trade_side = trade.side.ok_or_else(|| {
                HftError::Parse(format!("Polymarket last trade omitted side for {token}"))
            })?;
            if quantity <= rust_decimal::Decimal::ZERO {
                return Err(HftError::Parse(format!(
                    "Polymarket last trade has non-positive size for {token}"
                )));
            }
            if trade.price < rust_decimal::Decimal::ZERO || trade.price > rust_decimal::Decimal::ONE
            {
                return Err(HftError::Parse(format!(
                    "Polymarket last trade has invalid price for {token}: {}",
                    trade.price
                )));
            }
            let timestamp = timestamp_micros(trade.timestamp)?;
            let side = side(trade_side)?;
            let fee_rate = trade
                .fee_rate_bps
                .map(|rate| rate.normalize().to_string())
                .unwrap_or_else(|| "none".to_string());
            Ok(vec![MarketEvent::Trade(Trade {
                symbol,
                timestamp,
                price: Price(trade.price),
                quantity: Quantity(quantity),
                side,
                // The market channel does not expose a venue trade ID. Include every stable
                // market and execution field so distinct fills do not collapse in downstream
                // dedupe.
                trade_id: format!(
                    "{}:{token}:{}:{}:{quantity}:{side:?}:{fee_rate}",
                    trade.market, trade.timestamp, trade.price
                ),
                source_venue: Some(VenueId::POLYMARKET),
                timestamps: Default::default(),
            })])
        }
        _ => Ok(Vec::new()),
    }
}

#[async_trait]
impl MarketStream for PolymarketMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if symbols.is_empty() {
            return Err(HftError::Config(
                "Polymarket requires at least one outcome token ID".to_string(),
            ));
        }
        let (asset_ids, symbols) = token_ids(&symbols)?;
        let request = serde_json::to_string(&SubscriptionRequest::market(asset_ids))
            .map_err(|error| HftError::Serialization(error.to_string()))?;
        let ws_url = market_ws_url(&self.ws_url)?;
        let state = Arc::clone(&self.state);
        let failure_capture_path = self.failure_capture_path.clone();
        state.enabled.store(true, Ordering::Release);
        let generation = state.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (tx, rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);

        tokio::spawn(async move {
            let active = || {
                state.enabled.load(Ordering::Acquire)
                    && state.generation.load(Ordering::Acquire) == generation
                    && !tx.is_closed()
            };
            let mut book_state = BookState::default();
            while active() {
                let (mut ws, _) = match connect_async(&ws_url).await {
                    Ok(connection) => connection,
                    Err(error) => {
                        if !active() {
                            return;
                        }
                        if tx
                            .send(Err(HftError::Network(format!(
                                "Polymarket market WebSocket connect failed: {error}"
                            ))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        sleep(RECONNECT_DELAY).await;
                        continue;
                    }
                };
                if !active() {
                    return;
                }
                if let Err(error) = ws.send(Message::Text(request.clone().into())).await {
                    if !active() {
                        return;
                    }
                    if tx
                        .send(Err(HftError::Network(format!(
                            "Polymarket market subscription failed: {error}"
                        ))))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    sleep(RECONNECT_DELAY).await;
                    continue;
                }
                if !active() {
                    return;
                }
                state.connected.store(true, Ordering::Release);
                state
                    .last_heartbeat
                    .store(hft_core::now_micros(), Ordering::Release);
                let mut heartbeat = interval(HEARTBEAT_INTERVAL);
                heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
                heartbeat.tick().await;
                let mut last_frame = Instant::now();

                let reason = 'socket: loop {
                    tokio::select! {
                        _ = heartbeat.tick() => {
                            if last_frame.elapsed() > HEARTBEAT_TIMEOUT {
                                break Some("heartbeat timed out".to_string());
                            }
                            if let Err(error) = ws.send(Message::Text("PING".into())).await {
                                break Some(format!("heartbeat send failed: {error}"));
                            }
                        }
                        frame = ws.next() => match frame {
                            Some(Ok(Message::Text(text))) if text == "PONG" => {
                                last_frame = Instant::now();
                                state.last_heartbeat.store(hft_core::now_micros(), Ordering::Release);
                            }
                            Some(Ok(Message::Text(text))) => {
                                last_frame = Instant::now();
                                state.last_heartbeat.store(hft_core::now_micros(), Ordering::Release);
                                match parse_messages(&text) {
                                    Ok(messages) => {
                                        for message in messages {
                                            if !active() {
                                                break 'socket None;
                                            }
                                            match convert_message(message, &symbols, &mut book_state) {
                                                Ok(events) => {
                                                    let reconnect = events.iter().any(|event| {
                                                        matches!(event, MarketEvent::Disconnect { .. })
                                                    });
                                                    for event in events {
                                                        if !active() {
                                                            break 'socket None;
                                                        }
                                                        if tx.send(Ok(event)).await.is_err() {
                                                            return;
                                                        }
                                                    }
                                                    if reconnect {
                                                        break 'socket None;
                                                    }
                                                }
                                                Err(error) => {
                                                    capture_crossed_failure(
                                                        failure_capture_path.as_deref(),
                                                        text.as_bytes(),
                                                        &error,
                                                    );
                                                    let reason = format!("invalid market message: {error}");
                                                    if tx.send(Err(error)).await.is_err() {
                                                        return;
                                                    }
                                                    break 'socket Some(reason);
                                                }
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        capture_crossed_failure(
                                            failure_capture_path.as_deref(),
                                            text.as_bytes(),
                                            &error,
                                        );
                                        let reason = format!("invalid market frame: {error}");
                                        if tx.send(Err(error)).await.is_err() {
                                            return;
                                        }
                                        break 'socket Some(reason);
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                last_frame = Instant::now();
                                if let Err(error) = ws.send(Message::Pong(payload)).await {
                                    break Some(format!("protocol pong failed: {error}"));
                                }
                            }
                            Some(Ok(Message::Close(frame))) => break Some(format!("server closed stream: {frame:?}")),
                            Some(Ok(_)) => {}
                            Some(Err(error)) => break Some(format!("stream error: {error}")),
                            None => break Some("stream ended".to_string()),
                        }
                    }
                    if !active() {
                        break Some("stream stopped".to_string());
                    }
                };

                if state.generation.load(Ordering::Acquire) == generation {
                    state.connected.store(false, Ordering::Release);
                }
                book_state.reset();
                if active() {
                    if let Some(reason) = reason {
                        if tx
                            .send(Ok(venue_disconnect(format!(
                                "Polymarket market WebSocket disconnected: {reason}"
                            ))))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    sleep(RECONNECT_DELAY).await;
                }
            }
            if state.generation.load(Ordering::Acquire) == generation {
                state.connected.store(false, Ordering::Release);
            }
        });

        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.state.connected.load(Ordering::Acquire),
            latency_ms: None,
            last_heartbeat: self.state.last_heartbeat.load(Ordering::Acquire),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        market_ws_url(&self.ws_url)?;
        self.state.enabled.store(true, Ordering::Release);
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.state.enabled.store(false, Ordering::Release);
        self.state.generation.fetch_add(1, Ordering::AcqRel);
        self.state.connected.store(false, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn symbols() -> HashMap<String, Symbol> {
        HashMap::from([("123".to_string(), Symbol::new("123"))])
    }

    fn multiple_symbols() -> HashMap<String, Symbol> {
        HashMap::from([
            ("123".to_string(), Symbol::new("123")),
            ("456".to_string(), Symbol::new("456")),
        ])
    }

    fn three_symbols() -> HashMap<String, Symbol> {
        HashMap::from([
            ("123".to_string(), Symbol::new("123")),
            ("456".to_string(), Symbol::new("456")),
            ("789".to_string(), Symbol::new("789")),
        ])
    }

    fn parse_one(json: &str) -> WsMessage {
        parse_messages(&json.replace(
            "$MARKET",
            &polymarket_client_sdk::types::B256::ZERO.to_string(),
        ))
        .unwrap()
        .pop()
        .unwrap()
    }

    fn canonical_snapshot(sequence: u64) -> MarketEvent {
        MarketEvent::Snapshot(MarketSnapshot {
            symbol: Symbol::new("123"),
            timestamp: 1_000,
            bids: vec![BookLevel::new(0.4, 2.0).unwrap()],
            asks: vec![BookLevel::new(0.6, 3.0).unwrap()],
            sequence,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        })
    }

    #[test]
    fn canonical_book_requires_snapshot_and_applies_delta_deletion() {
        let mut book = PolymarketBook::default();
        let delta = MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("123"),
            timestamp: 1_001_000,
            bids: vec![BookLevel {
                price: Price(Decimal::new(4, 1)),
                quantity: Quantity(Decimal::new(1, 0)),
            }],
            asks: Vec::new(),
            first_sequence: None,
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        });

        assert!(book.apply(&delta).is_err());
        let snapshot = book
            .apply(&canonical_snapshot(1))
            .unwrap()
            .expect("snapshot projection");
        assert!(book.is_ready());
        assert_eq!(snapshot.bids.len(), 1);
        assert_eq!(snapshot.asks.len(), 1);

        let deleted = MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("123"),
            timestamp: 1_002_000,
            bids: vec![BookLevel {
                price: Price(Decimal::new(4, 1)),
                quantity: Quantity(Decimal::ZERO),
            }],
            asks: Vec::new(),
            first_sequence: None,
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        });
        let projected = book.apply(&deleted).unwrap().expect("delta projection");
        assert!(projected.bids.is_empty());
        assert_eq!(projected.asks.len(), 1);

        book.apply(&venue_disconnect("test")).unwrap();
        assert!(!book.is_ready());
        assert!(book.apply(&delta).is_err());
    }

    #[test]
    fn canonical_book_rejects_crossed_projection_and_sequence_regression() {
        let mut book = PolymarketBook::default();
        book.apply(&canonical_snapshot(4)).unwrap();
        let regressed = MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("123"),
            timestamp: 1_001_000,
            bids: vec![BookLevel::new(0.41, 2.0).unwrap()],
            asks: Vec::new(),
            first_sequence: None,
            sequence: 4,
            is_snapshot: false,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        });
        assert!(book.apply(&regressed).is_err());

        let crossed = MarketEvent::Update(BookUpdate {
            symbol: Symbol::new("123"),
            timestamp: 1_002_000,
            bids: vec![BookLevel::new(0.7, 2.0).unwrap()],
            asks: Vec::new(),
            first_sequence: None,
            sequence: 5,
            is_snapshot: false,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        });
        assert!(book.apply(&crossed).is_err());
        assert!(
            book.is_ready(),
            "rejected updates must not poison prior state"
        );
    }

    #[test]
    fn price_change_batch_applies_same_token_atomically_after_transient_cross() {
        let symbols = symbols();
        let mut state = BookState::default();
        let snapshot = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"1000","bids":[{"price":"0.8","size":"5"},{"price":"0.4","size":"2"}],"asks":[{"price":"0.9","size":"3"}]}"#,
        );
        convert_message(snapshot, &symbols, &mut state).unwrap();

        let batch = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"2000","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"SELL","best_bid":"0.4","best_ask":"0.5"},{"asset_id":"123","price":"0.8","size":"0","side":"BUY","best_bid":"0.4","best_ask":"0.5"}]}"#,
        );
        let events = convert_message(batch, &symbols, &mut state).unwrap();
        assert!(matches!(events.as_slice(), [MarketEvent::Update(_)]));
        let MarketEvent::Update(update) = &events[0] else {
            panic!("expected one consolidated update");
        };
        assert_eq!(update.bids.len(), 1);
        assert_eq!(update.bids[0].price.0, Decimal::new(8, 1));
        assert_eq!(update.bids[0].quantity.0, Decimal::ZERO);
        assert_eq!(update.asks[0].price.0, Decimal::new(5, 1));
        assert_eq!(state.books["123"].bids.get(&Decimal::new(8, 1)), None);
        assert!(state.books["123"].bids.contains_key(&Decimal::new(4, 1)));
        assert!(state.books["123"].is_ready());
    }

    #[test]
    fn invalid_second_entry_leaves_the_entire_price_change_frame_unapplied() {
        let symbols = symbols();
        let mut state = BookState::default();
        let snapshot = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"1000","bids":[{"price":"0.4","size":"2"}],"asks":[{"price":"0.6","size":"3"}]}"#,
        );
        convert_message(snapshot, &symbols, &mut state).unwrap();

        let invalid_batch = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"2000","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"BUY","best_bid":"0.5","best_ask":"0.6"},{"asset_id":"123","price":"1.1","size":"1","side":"BUY","best_bid":"1.1","best_ask":"0.6"}]}"#,
        );
        assert!(convert_message(invalid_batch, &symbols, &mut state).is_err());
        assert_eq!(state.sequences["123"], 1);
        assert_eq!(state.timestamps["123"], 1_000);
        assert!(!state.books["123"].bids.contains_key(&Decimal::new(5, 1)));
        assert!(state.books["123"].is_ready());
    }

    #[test]
    fn reported_bba_reconciles_stale_depth_before_publishing() {
        let symbols = symbols();
        let mut state = BookState::default();
        let snapshot = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"1000","bids":[{"price":"0.8","size":"5"},{"price":"0.6","size":"2"}],"asks":[{"price":"0.9","size":"3"}]}"#,
        );
        convert_message(snapshot, &symbols, &mut state).unwrap();

        let change = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"2000","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"BUY","best_bid":"0.6","best_ask":"0.9"}]}"#,
        );
        let events = convert_message(change, &symbols, &mut state).unwrap();
        assert!(matches!(events.as_slice(), [MarketEvent::Update(_)]));
        let MarketEvent::Update(update) = &events[0] else {
            panic!("expected consolidated update");
        };
        assert!(update.bids.iter().any(|level| {
            level.price.0 == Decimal::new(8, 1) && level.quantity.0 == Decimal::ZERO
        }));
        assert!(!state.books["123"].bids.contains_key(&Decimal::new(8, 1)));
        assert!(state.books["123"].bids.contains_key(&Decimal::new(6, 1)));
        assert!(state.books["123"].bids.contains_key(&Decimal::new(5, 1)));
    }

    #[test]
    fn missing_lower_or_higher_reported_bba_dirties_only_that_token_until_snapshot() {
        let symbols = three_symbols();
        let mut state = BookState::default();
        for (token, bid, ask) in [
            ("123", "0.8", "0.9"),
            ("456", "0.4", "0.6"),
            ("789", "0.4", "0.6"),
        ] {
            let snapshot = parse_one(&format!(
                r#"{{"event_type":"book","asset_id":"{token}","market":"$MARKET","timestamp":"1000","bids":[{{"price":"{bid}","size":"5"}}],"asks":[{{"price":"{ask}","size":"3"}}]}}"#
            ));
            convert_message(snapshot, &symbols, &mut state).unwrap();
        }

        let dirty_batch = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"2000","price_changes":[{"asset_id":"123","price":"0.8","size":"0","side":"BUY","best_bid":"0.01","best_ask":"0.9"},{"asset_id":"456","price":"0.6","size":"0","side":"SELL","best_bid":"0.4","best_ask":"0.99"},{"asset_id":"789","price":"0.5","size":"2","side":"BUY","best_bid":"0.5","best_ask":"0.6"}]}"#,
        );
        let events = convert_message(dirty_batch, &symbols, &mut state).unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                MarketEvent::Disconnect { symbol: Some(first), .. },
                MarketEvent::Disconnect { symbol: Some(second), .. },
                MarketEvent::Update(update)
            ] if first.as_str() == "123"
                && second.as_str() == "456"
                && update.symbol.as_str() == "789"
        ));
        assert!(state.books["123"].is_dirty());
        assert!(state.books["456"].is_dirty());
        assert!(state.books["123"].bids.is_empty());
        assert!(state.books["456"].bids.is_empty());
        assert!(state.books["789"].is_ready());
        assert!(!state.books["789"].is_dirty());
        assert!(state.books["789"].bids.contains_key(&Decimal::new(5, 1)));

        let healing = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"3000","bids":[{"price":"0.6","size":"2"}],"asks":[{"price":"0.9","size":"3"}]}"#,
        );
        convert_message(healing, &symbols, &mut state).unwrap();
        assert!(state.books["123"].is_ready());
        assert!(!state.books["123"].is_dirty());
        assert!(state.books["456"].is_dirty());

        let healed_update = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"4000","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"BUY","best_bid":"0.6","best_ask":"0.9"}]}"#,
        );
        assert!(matches!(
            convert_message(healed_update, &symbols, &mut state)
                .unwrap()
                .as_slice(),
            [MarketEvent::Update(_)]
        ));

        let dirty_follow_up = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"4000","price_changes":[{"asset_id":"456","price":"0.5","size":"1","side":"BUY","best_bid":"0.5","best_ask":"0.6"}]}"#,
        );
        assert!(matches!(
            convert_message(dirty_follow_up, &symbols, &mut state).unwrap().as_slice(),
            [MarketEvent::Disconnect { symbol: Some(symbol), .. }] if symbol.as_str() == "456"
        ));
    }

    #[test]
    fn empty_side_bba_sentinels_are_only_zero_bid_and_one_ask() {
        assert!(!PolymarketBook::requires_reported_bid_level(Decimal::ZERO));
        assert!(PolymarketBook::requires_reported_bid_level(Decimal::new(
            1, 2
        )));
        assert!(PolymarketBook::requires_reported_bid_level(Decimal::ONE));
        assert!(PolymarketBook::requires_reported_ask_level(Decimal::ZERO));
        assert!(PolymarketBook::requires_reported_ask_level(Decimal::new(
            99, 2
        )));
        assert!(!PolymarketBook::requires_reported_ask_level(Decimal::ONE));
    }

    #[test]
    fn price_change_accepts_protocol_empty_side_sentinels() {
        let symbols = symbols();
        let mut state = BookState::default();
        let snapshot = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"1000","bids":[{"price":"0.4","size":"2"}],"asks":[{"price":"0.6","size":"3"}]}"#,
        );
        convert_message(snapshot, &symbols, &mut state).unwrap();

        // Polymarket reports best_bid=0 when the bid side is empty and
        // best_ask=1 when the ask side is empty.
        let empty_sides = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"2000","price_changes":[{"asset_id":"123","price":"0.4","size":"0","side":"BUY","best_bid":"0","best_ask":"1"}]}"#,
        );
        let events = convert_message(empty_sides, &symbols, &mut state).unwrap();
        assert!(matches!(events.as_slice(), [MarketEvent::Update(_)]));
        assert!(state.books["123"].is_ready());
        assert!(!state.books["123"].is_dirty());
        assert!(state.books["123"].bids.is_empty());
        assert!(state.books["123"].asks.is_empty());
    }

    #[test]
    fn stale_price_change_is_rejected_without_mutating_the_book() {
        let symbols = symbols();
        let mut state = BookState::default();
        let snapshot = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"2000","bids":[{"price":"0.4","size":"2"}],"asks":[{"price":"0.6","size":"3"}]}"#,
        );
        convert_message(snapshot, &symbols, &mut state).unwrap();
        let stale = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"1000","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"BUY","best_bid":"0.5","best_ask":"0.6"}]}"#,
        );
        assert!(convert_message(stale, &symbols, &mut state).is_err());
        assert_eq!(state.sequences["123"], 1);
        assert_eq!(state.timestamps["123"], 2_000);
        assert!(!state.books["123"].bids.contains_key(&Decimal::new(5, 1)));
    }

    #[test]
    fn accepts_canonical_long_token_ids_without_lossy_conversion() {
        let token =
            "106585164761922456203746651621390029417453862034640469075081961934906147433548";
        let (ids, symbols) = token_ids(&[Symbol::new(token)]).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].to_string(), token);
        assert_eq!(symbols[token].as_str(), token);
    }

    #[test]
    fn multi_token_delta_preserves_each_token_and_global_disconnect_clears_projection() {
        let symbols = multiple_symbols();
        let mut state = BookState::default();
        for token in ["123", "456"] {
            let event = MarketEvent::Snapshot(MarketSnapshot {
                symbol: Symbol::new(token),
                timestamp: 1_000,
                bids: vec![BookLevel::new(0.4, 1.0).unwrap()],
                asks: vec![BookLevel::new(0.6, 1.0).unwrap()],
                sequence: state.next_sequence(token),
                source_venue: Some(VenueId::POLYMARKET),
                timestamps: Default::default(),
            });
            state
                .books
                .entry(token.to_string())
                .or_default()
                .apply(&event)
                .unwrap();
            state.ready.insert(token.to_string());
        }
        assert_eq!(state.books.len(), 2);

        let message = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"1001","price_changes":[{"asset_id":"123","price":"0.5","size":"2","side":"BUY"},{"asset_id":"456","price":"0.55","size":"2","side":"SELL"}]}"#,
        );
        let events = convert_message(message, &symbols, &mut state).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], MarketEvent::Update(_)));
        assert!(matches!(events[1], MarketEvent::Update(_)));
        assert!(state.books["123"].is_ready());
        assert!(state.books["456"].is_ready());

        state.reset();
        assert!(state.books.is_empty());
        assert!(state.ready.is_empty());
    }

    #[test]
    fn snapshot_delta_delete_and_reconnect_gate_are_lossless() {
        let mut state = BookState::default();
        let book = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"1000","bids":[{"price":"0.4","size":"2"},{"price":"0.5","size":"1"}],"asks":[{"price":"0.7","size":"3"},{"price":"0.6","size":"4"}]}"#,
        );
        let events = convert_message(book, &symbols(), &mut state).unwrap();
        let MarketEvent::Snapshot(snapshot) = &events[0] else {
            panic!("expected snapshot")
        };
        assert_eq!(snapshot.bids[0].price.0, Decimal::new(5, 1));
        assert_eq!(snapshot.asks[0].price.0, Decimal::new(6, 1));
        assert_eq!(snapshot.source_venue, Some(VenueId::POLYMARKET));

        let delta = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"1001","price_changes":[{"asset_id":"123","price":"0.5","size":"0","side":"BUY","hash":"h"}]}"#,
        );
        let events = convert_message(delta, &symbols(), &mut state).unwrap();
        let MarketEvent::Update(update) = &events[0] else {
            panic!("expected delta")
        };
        assert_eq!(update.bids[0].quantity.0, Decimal::ZERO);
        assert!(!update.is_snapshot);

        state.reset();
        let delta = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"1002","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"BUY"}]}"#,
        );
        assert!(matches!(
            convert_message(delta, &symbols(), &mut state).unwrap()[0],
            MarketEvent::Disconnect { .. }
        ));

        let book = parse_one(
            r#"{"event_type":"book","asset_id":"123","market":"$MARKET","timestamp":"1003","bids":[],"asks":[]}"#,
        );
        convert_message(book, &symbols(), &mut state).unwrap();
        let missing_size = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"1004","price_changes":[{"asset_id":"123","price":"0.5","side":"BUY"}]}"#,
        );
        let events = convert_message(missing_size, &symbols(), &mut state).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], MarketEvent::Disconnect { .. }));
    }

    #[test]
    fn reconnect_invalidation_is_venue_global_for_multi_token_subscription() {
        let delta = parse_one(
            r#"{"event_type":"price_change","market":"$MARKET","timestamp":"1002","price_changes":[{"asset_id":"123","price":"0.5","size":"1","side":"BUY"}]}"#,
        );

        let events =
            convert_message(delta, &multiple_symbols(), &mut BookState::default()).unwrap();

        assert!(matches!(
            events.as_slice(),
            [MarketEvent::Disconnect {
                source_venue: Some(VenueId::POLYMARKET),
                symbol: None,
                ..
            }]
        ));
    }

    #[test]
    fn last_trade_is_preserved() {
        let trade = parse_one(
            r#"{"event_type":"last_trade_price","asset_id":"123","market":"$MARKET","price":"0.61","side":"SELL","size":"7","timestamp":"1003"}"#,
        );
        let events = convert_message(trade, &symbols(), &mut BookState::default()).unwrap();
        let MarketEvent::Trade(trade) = &events[0] else {
            panic!("expected trade")
        };
        assert_eq!(trade.side, Side::Sell);
        assert_eq!(trade.quantity.0, Decimal::from(7));
        assert_eq!(trade.source_venue, Some(VenueId::POLYMARKET));
        let first_id = trade.trade_id.clone();

        let same_millisecond_different_size = parse_one(
            r#"{"event_type":"last_trade_price","asset_id":"123","market":"$MARKET","price":"0.61","side":"SELL","size":"8","timestamp":"1003"}"#,
        );
        let events = convert_message(
            same_millisecond_different_size,
            &symbols(),
            &mut BookState::default(),
        )
        .unwrap();
        let MarketEvent::Trade(second) = &events[0] else {
            panic!("expected trade")
        };
        assert_ne!(first_id, second.trade_id);

        let invalid = parse_one(
            r#"{"event_type":"last_trade_price","asset_id":"123","market":"$MARKET","price":"1.01","side":"SELL","size":"7","timestamp":"1004"}"#,
        );
        assert!(convert_message(invalid, &symbols(), &mut BookState::default()).is_err());
    }

    #[test]
    fn last_trade_id_includes_market_and_fee_schedule_identity() {
        let base = parse_one(
            r#"{"event_type":"last_trade_price","asset_id":"123","market":"$MARKET","price":"0.61","side":"SELL","size":"7","fee_rate_bps":"100","timestamp":"1003"}"#,
        );
        let different_market = parse_one(
            r#"{"event_type":"last_trade_price","asset_id":"123","market":"0x1111111111111111111111111111111111111111111111111111111111111111","price":"0.61","side":"SELL","size":"7","fee_rate_bps":"100","timestamp":"1003"}"#,
        );
        let different_fee_rate = parse_one(
            r#"{"event_type":"last_trade_price","asset_id":"123","market":"$MARKET","price":"0.61","side":"SELL","size":"7","fee_rate_bps":"200","timestamp":"1003"}"#,
        );

        let trade_id = |message| {
            let events = convert_message(message, &symbols(), &mut BookState::default()).unwrap();
            let MarketEvent::Trade(trade) = &events[0] else {
                panic!("expected trade")
            };
            trade.trade_id.clone()
        };

        let base_id = trade_id(base);
        assert_ne!(base_id, trade_id(different_market));
        assert_ne!(base_id, trade_id(different_fee_rate));
    }

    #[test]
    fn validates_token_identity_and_endpoint_shape() {
        assert!(token_ids(&[Symbol::new("not-a-token")]).is_err());
        assert!(token_ids(&[Symbol::new("00123")]).is_err());
        assert!(market_ws_url("https://ws-subscriptions-clob.polymarket.com").is_err());
        assert!(market_ws_url("wss://").is_err());
        assert_eq!(
            market_ws_url("wss://ws-subscriptions-clob.polymarket.com/ws/user").unwrap(),
            "wss://ws-subscriptions-clob.polymarket.com/ws/market"
        );
    }

    #[tokio::test]
    async fn invalid_endpoint_fails_before_enabling_stream() {
        let stream = PolymarketMarketStream::new().with_ws_url("http://localhost");
        assert!(stream.subscribe(vec![Symbol::new("123")]).await.is_err());
        assert!(!stream.state.enabled.load(Ordering::Acquire));
        assert_eq!(stream.state.generation.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn disconnect_invalidates_the_active_generation() {
        let mut stream = PolymarketMarketStream::new();
        stream.connect().await.unwrap();
        assert!(stream.state.enabled.load(Ordering::Acquire));
        let before = stream.state.generation.load(Ordering::Acquire);
        stream.disconnect().await.unwrap();
        assert!(!stream.state.enabled.load(Ordering::Acquire));
        assert!(!stream.state.connected.load(Ordering::Acquire));
        assert!(stream.state.generation.load(Ordering::Acquire) > before);
    }

    #[test]
    fn crossed_failure_capture_is_bounded_and_create_once() {
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("polymarket-capture-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let error = HftError::Parse("Polymarket book is crossed".to_string());
        capture_crossed_failure(Some(&path), b"crossed-frame", &error);
        assert_eq!(std::fs::read(&path).unwrap(), b"crossed-frame");
        assert_eq!(
            persist_failure_payload(&path, b"second-frame")
                .expect_err("capture must be create-once")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            persist_failure_payload(&path, &vec![0_u8; MAX_FAILURE_CAPTURE_BYTES + 1])
                .expect_err("capture must stay bounded")
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    #[ignore = "public network smoke test; set POLYMARKET_TOKEN_ID"]
    async fn live_public_stream_smoke() {
        let token = std::env::var("POLYMARKET_TOKEN_ID").expect("POLYMARKET_TOKEN_ID");
        let stream = PolymarketMarketStream::new();
        let mut events = stream.subscribe(vec![Symbol::new(token)]).await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(Ok(event @ MarketEvent::Snapshot(_))) = events.next().await {
                    break event;
                }
            }
        })
        .await
        .expect("Polymarket snapshot within 15 seconds");
        assert!(matches!(event, MarketEvent::Snapshot(_)));
    }
}
