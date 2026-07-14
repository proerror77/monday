//! Polymarket public CLOB market data.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, Price, Quantity, Side, Symbol, VenueId};
use polymarket_client_sdk::clob::types::Side as PolymarketSide;
use polymarket_client_sdk::clob::ws::types::response::WsMessage;
use polymarket_client_sdk::clob::ws::SubscriptionRequest;
use polymarket_client_sdk::types::U256;
use ports::{
    BookLevel, BookUpdate, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream,
    Trade,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, MissedTickBehavior};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::Url;

pub const DEFAULT_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com";
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
        }
    }

    #[must_use]
    pub fn with_ws_url(mut self, url: impl Into<String>) -> Self {
        self.ws_url = url.into();
        self
    }
}

#[derive(Default)]
struct BookState {
    ready: HashSet<String>,
    sequences: HashMap<String, u64>,
}

impl BookState {
    fn reset(&mut self) {
        self.ready.clear();
        self.sequences.clear();
    }

    fn next_sequence(&mut self, symbol: &str) -> u64 {
        let sequence = self.sequences.entry(symbol.to_string()).or_default();
        *sequence = sequence.saturating_add(1);
        *sequence
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

fn venue_disconnect(reason: impl Into<String>) -> MarketEvent {
    MarketEvent::Disconnect {
        reason: reason.into(),
        source_venue: Some(VenueId::POLYMARKET),
        symbol: None,
    }
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
            state.ready.insert(token.clone());
            let sequence = state.next_sequence(&token);
            Ok(vec![MarketEvent::Snapshot(MarketSnapshot {
                symbol,
                timestamp,
                bids,
                asks,
                sequence,
                source_venue: Some(VenueId::POLYMARKET),
            })])
        }
        WsMessage::PriceChange(change) => {
            let timestamp = timestamp_micros(change.timestamp)?;
            let mut events = Vec::new();
            for change in change.price_changes {
                let token = change.asset_id.to_string();
                let Some(symbol) = symbols.get(&token).cloned() else {
                    continue;
                };
                if !state.ready.contains(&token) {
                    return Ok(vec![venue_disconnect(format!(
                        "Polymarket delta arrived before snapshot for {token}"
                    ))]);
                }
                let Some(size) = change.size else {
                    return Ok(vec![venue_disconnect(format!(
                        "Polymarket delta omitted size for {token}"
                    ))]);
                };
                let changed_level = level(change.price, size)?;
                let sequence = state.next_sequence(&token);
                let (bids, asks) = match side(change.side)? {
                    Side::Buy => (vec![changed_level], Vec::new()),
                    Side::Sell => (Vec::new(), vec![changed_level]),
                };
                events.push(MarketEvent::Update(BookUpdate {
                    symbol,
                    timestamp,
                    bids,
                    asks,
                    first_sequence: None,
                    sequence,
                    is_snapshot: false,
                    source_venue: Some(VenueId::POLYMARKET),
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
            Ok(vec![MarketEvent::Trade(Trade {
                symbol,
                timestamp,
                price: Price(trade.price),
                quantity: Quantity(quantity),
                side,
                // The market channel does not expose a venue trade ID. Include every stable
                // execution field so same-millisecond, same-price trades with different size or
                // side do not collapse in downstream dedupe.
                trade_id: format!(
                    "{token}:{}:{}:{quantity}:{side:?}",
                    trade.timestamp, trade.price
                ),
                source_venue: Some(VenueId::POLYMARKET),
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
                if let Err(error) = ws.send(Message::Text(request.clone().into())).await {
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
                                            match convert_message(message, &symbols, &mut book_state) {
                                                Ok(events) => {
                                                    let reconnect = events.iter().any(|event| {
                                                        matches!(event, MarketEvent::Disconnect { .. })
                                                    });
                                                    for event in events {
                                                        if tx.send(Ok(event)).await.is_err() {
                                                            return;
                                                        }
                                                    }
                                                    if reconnect {
                                                        break 'socket None;
                                                    }
                                                }
                                                Err(error) => {
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

                state.connected.store(false, Ordering::Release);
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
            state.connected.store(false, Ordering::Release);
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

    fn parse_one(json: &str) -> WsMessage {
        parse_messages(&json.replace(
            "$MARKET",
            &polymarket_client_sdk::types::B256::ZERO.to_string(),
        ))
        .unwrap()
        .pop()
        .unwrap()
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
