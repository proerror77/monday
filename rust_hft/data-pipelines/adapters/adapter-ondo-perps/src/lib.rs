//! Ondo Perps public top-of-book market data adapter.

use std::cmp::Reverse;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{HftError, HftResult, Symbol, VenueId};
use ports::{BookLevel, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, MissedTickBehavior};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

pub const DEFAULT_WS_URL: &str = "wss://api.ondoperps.xyz/ws";
const CHANNEL: &str = "topOfBooksPerps";
const EVENT_QUEUE_CAPACITY: usize = 1_024;
const PING_INTERVAL: Duration = Duration::from_secs(30);
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

#[derive(Default)]
struct ConnectionState {
    connected: AtomicBool,
    last_heartbeat: AtomicU64,
}

/// Ondo Perps public market data stream.
pub struct OndoPerpsMarketStream {
    ws_url: String,
    state: Arc<ConnectionState>,
}

impl Default for OndoPerpsMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl OndoPerpsMarketStream {
    pub fn new() -> Self {
        Self {
            ws_url: DEFAULT_WS_URL.to_string(),
            state: Arc::new(ConnectionState::default()),
        }
    }

    pub fn with_ws_url(mut self, url: impl Into<String>) -> Self {
        self.ws_url = url.into();
        self
    }
}

fn normalize_symbol(symbol: &str) -> String {
    let mut symbol = symbol.trim().to_ascii_uppercase().replace('_', "-");
    if let Some(base) = symbol.strip_suffix("-PERP") {
        symbol = base.to_string();
    }
    if !symbol.contains('-') {
        symbol.push_str("-USD");
    }
    if !symbol.ends_with(".P") {
        symbol.push_str(".P");
    }
    symbol
}

fn subscription(markets: &[String]) -> Value {
    json!({"op": "subscribe", "channel": CHANNEL, "markets": markets})
}

fn number(value: &Value, field: &str) -> HftResult<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|v| v.parse().ok()))
        .filter(|v| v.is_finite())
        .ok_or_else(|| HftError::Parse(format!("Ondo Perps invalid {field}: {value}")))
}

fn levels(value: Option<&Value>, side: &str) -> HftResult<Vec<BookLevel>> {
    let rows = value
        .and_then(Value::as_array)
        .ok_or_else(|| HftError::Parse(format!("Ondo Perps missing {side} levels")))?;
    rows.iter()
        .map(|row| {
            let pair = row
                .as_array()
                .filter(|pair| pair.len() >= 2)
                .ok_or_else(|| HftError::Parse(format!("Ondo Perps invalid {side} level")))?;
            BookLevel::new(number(&pair[0], "price")?, number(&pair[1], "quantity")?)
                .map_err(|e| HftError::Parse(format!("Ondo Perps invalid {side} level: {e}")))
        })
        .collect()
}

fn timestamp_micros(value: Option<&Value>) -> HftResult<u64> {
    let value = value.ok_or_else(|| HftError::Parse("Ondo Perps invalid time".to_string()))?;
    if let Some(timestamp) = value.as_str() {
        if let Ok(numeric) = timestamp.parse::<u64>() {
            return numeric_timestamp_micros(numeric);
        }
        return chrono::DateTime::parse_from_rfc3339(timestamp)
            .map_err(|e| HftError::Parse(format!("Ondo Perps invalid time: {e}")))?
            .timestamp_micros()
            .try_into()
            .map_err(|_| HftError::Parse("Ondo Perps time predates Unix epoch".to_string()));
    }
    numeric_timestamp_micros(
        value
            .as_u64()
            .ok_or_else(|| HftError::Parse("Ondo Perps invalid time".to_string()))?,
    )
}

fn numeric_timestamp_micros(value: u64) -> HftResult<u64> {
    Ok(match value {
        0..=99_999_999_999 => value.saturating_mul(1_000_000),
        100_000_000_000..=99_999_999_999_999 => value.saturating_mul(1_000),
        _ => value,
    })
}

fn parse_message(text: &str) -> HftResult<Vec<MarketSnapshot>> {
    let message: Value = serde_json::from_str(text)
        .map_err(|e| HftError::Parse(format!("Ondo Perps JSON parse error: {e}")))?;
    if message.get("type").and_then(Value::as_str) == Some("error") {
        return Err(HftError::Exchange(format!(
            "Ondo Perps websocket error: {}",
            message
                .get("msg")
                .and_then(Value::as_str)
                .unwrap_or("unknown server error")
        )));
    }
    if message.get("type").and_then(Value::as_str) != Some("update")
        || message.get("channel").and_then(Value::as_str) != Some(CHANNEL)
    {
        return Ok(Vec::new());
    }

    let updates = message
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| HftError::Parse("Ondo Perps update missing data".to_string()))?;
    updates
        .iter()
        .map(|update| {
            let market = update
                .get("market")
                .and_then(Value::as_str)
                .filter(|market| !market.is_empty())
                .ok_or_else(|| HftError::Parse("Ondo Perps update missing market".to_string()))?;
            let mut bids = levels(update.get("bids"), "bid")?;
            let mut asks = levels(update.get("asks"), "ask")?;
            bids.sort_by_key(|level| Reverse(level.price));
            asks.sort_by_key(|level| level.price);
            Ok(MarketSnapshot {
                symbol: Symbol::new(normalize_symbol(market)),
                timestamp: timestamp_micros(update.get("time"))?,
                bids,
                asks,
                sequence: 0,
                source_venue: Some(VenueId::ONDO_PERPS),
            })
        })
        .collect()
}

#[async_trait]
impl MarketStream for OndoPerpsMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if symbols.is_empty() {
            return Err(HftError::Config(
                "Ondo Perps requires at least one symbol".to_string(),
            ));
        }
        let markets: Vec<_> = symbols
            .iter()
            .map(|symbol| normalize_symbol(symbol.as_str()))
            .collect();
        let ws_url = self.ws_url.clone();
        let state = self.state.clone();
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);

        tokio::spawn(async move {
            while !tx.is_closed() {
                let (mut ws, _) = match connect_async(&ws_url).await {
                    Ok(connection) => connection,
                    Err(error) => {
                        let _ = tx
                            .send(Err(HftError::Network(format!(
                                "Ondo Perps websocket connection failed: {error}"
                            ))))
                            .await;
                        sleep(RECONNECT_DELAY).await;
                        continue;
                    }
                };
                if let Err(error) = ws
                    .send(Message::Text(subscription(&markets).to_string().into()))
                    .await
                {
                    let _ = tx
                        .send(Err(HftError::Network(format!(
                            "Ondo Perps subscription failed: {error}"
                        ))))
                        .await;
                    sleep(RECONNECT_DELAY).await;
                    continue;
                }
                state.connected.store(true, Ordering::SeqCst);
                state
                    .last_heartbeat
                    .store(hft_core::now_micros(), Ordering::SeqCst);
                let mut ping = interval(PING_INTERVAL);
                ping.set_missed_tick_behavior(MissedTickBehavior::Skip);
                ping.tick().await;

                let disconnect_reason = loop {
                    tokio::select! {
                        _ = ping.tick() => {
                            if let Err(error) = ws
                                .send(Message::Text(json!({"op":"ping"}).to_string().into()))
                                .await
                            {
                                break format!("heartbeat failed: {error}");
                            }
                        }
                        message = ws.next() => match message {
                            Some(Ok(Message::Text(text))) => {
                                state
                                    .last_heartbeat
                                    .store(hft_core::now_micros(), Ordering::SeqCst);
                                match parse_message(&text) {
                                    Ok(snapshots) => {
                                        for snapshot in snapshots {
                                            match tx.try_send(Ok(MarketEvent::Snapshot(snapshot))) {
                                                Ok(()) => {}
                                                Err(mpsc::error::TrySendError::Full(_)) => warn!(
                                                    "Ondo Perps event queue full; dropping stale top-of-book snapshot"
                                                ),
                                                Err(mpsc::error::TrySendError::Closed(_)) => return,
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        if tx.send(Err(error)).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                if ws.send(Message::Pong(payload)).await.is_err() {
                                    break "protocol pong failed".to_string();
                                }
                            }
                            Some(Ok(Message::Pong(_))) => {
                                state
                                    .last_heartbeat
                                    .store(hft_core::now_micros(), Ordering::SeqCst);
                            }
                            Some(Ok(Message::Close(frame))) => break frame
                                .map(|frame| frame.reason.to_string())
                                .unwrap_or_else(|| "websocket closed".to_string()),
                            Some(Ok(_)) => {}
                            Some(Err(error)) => break format!("websocket error: {error}"),
                            None => break "websocket stream ended".to_string(),
                        }
                    }
                };
                state.connected.store(false, Ordering::SeqCst);
                if tx
                    .send(Ok(MarketEvent::Disconnect {
                        reason: disconnect_reason,
                        source_venue: Some(VenueId::ONDO_PERPS),
                        symbol: None,
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
                sleep(RECONNECT_DELAY).await;
            }
            state.connected.store(false, Ordering::SeqCst);
        });

        Ok(Box::pin(futures::stream::poll_fn(move |cx| {
            rx.poll_recv(cx)
        })))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.state.connected.load(Ordering::SeqCst),
            latency_ms: None,
            last_heartbeat: self.state.last_heartbeat.load(Ordering::SeqCst),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        info!("Ondo Perps adapter configured; subscribe establishes the websocket");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.state.connected.store(false, Ordering::SeqCst);
        info!("Ondo Perps adapter disconnected");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_official_subscription() {
        assert_eq!(
            subscription(&["AAPL-USD.P".to_string()]),
            json!({"op":"subscribe","channel":"topOfBooksPerps","markets":["AAPL-USD.P"]})
        );
        assert_eq!(normalize_symbol("aapl-usd.p"), "AAPL-USD.P");
    }

    #[test]
    fn parses_top_of_book_update_as_normalized_snapshot() {
        let snapshots = parse_message(
            r#"{"type":"update","channel":"topOfBooksPerps","data":[{"market":"aapl-usd.p","time":"2025-06-15T15:06:40.123Z","asks":[["201.25","3"],[202.0,1]],"bids":[[200.5,2],["201.00","4"]]}]}"#,
        )
        .unwrap();
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.symbol.as_str(), "AAPL-USD.P");
        assert_eq!(snapshot.timestamp, 1_750_000_000_123_000);
        assert_eq!(snapshot.bids[0].price.to_string(), "201");
        assert_eq!(snapshot.asks[0].price.to_string(), "201.25");
        assert_eq!(snapshot.source_venue, Some(VenueId::ONDO_PERPS));
    }

    #[test]
    fn ignores_non_market_messages_and_rejects_bad_levels() {
        assert!(
            parse_message(r#"{"type":"subscribed","channel":"topOfBooksPerps"}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_message(
            r#"{"type":"update","channel":"topOfBooksPerps","data":[{"market":"AAPL-USD","time":1750000000123,"asks":[["bad","1"]],"bids":[]}]}"#
        )
        .is_err());
        assert!(parse_message(r#"{"type":"error","msg":"bad subscription"}"#).is_err());
    }

    #[tokio::test]
    #[ignore = "requires the live Ondo Perps public websocket"]
    async fn live_public_stream_returns_ondo_top_of_book() {
        let stream = OndoPerpsMarketStream::new();
        let mut events = stream
            .subscribe(vec![Symbol::new("AAPL-USD.P")])
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(15), events.next())
            .await
            .expect("Ondo public websocket timed out")
            .expect("Ondo public websocket ended")
            .expect("Ondo public websocket returned an error");
        let MarketEvent::Snapshot(snapshot) = event else {
            panic!("expected an Ondo market snapshot");
        };
        assert_eq!(snapshot.source_venue, Some(VenueId::ONDO_PERPS));
        assert_eq!(snapshot.symbol.as_str(), "AAPL-USD.P");
        assert!(!snapshot.bids.is_empty());
        assert!(!snapshot.asks.is_empty());
    }
}
