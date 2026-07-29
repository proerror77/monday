//! Bybit v5 市場數據適配器（WS 公共流）

use async_trait::async_trait;
use bytes::BytesMut;
use futures::StreamExt;
use hft_core::{
    HftError, HftResult, LatencyStage, LatencyTracker, Price, Quantity, Symbol, VenueId,
};
use integration::ws::{WsClient, WsClientConfig};
use ports::{
    BookLevel, BookUpdate, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream,
    TopOfBook, TrackedMarketEvent, Trade,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{error, warn};

const DEFAULT_EVENT_QUEUE_CAPACITY: usize = 4096;

#[derive(Debug)]
struct QueuedMarketEvent {
    generation: u64,
    event: TrackedMarketEvent,
}

#[derive(Debug, Clone)]
struct StreamInvalidation {
    generation: u64,
    reason: String,
    error: Option<HftError>,
    terminal: bool,
}

fn publish_invalidation(
    generation: &AtomicU64,
    control_tx: &watch::Sender<Option<StreamInvalidation>>,
    reason: String,
    error: Option<HftError>,
    terminal: bool,
) {
    let generation = generation.fetch_add(1, Ordering::AcqRel).saturating_add(1);
    control_tx.send_replace(Some(StreamInvalidation {
        generation,
        reason,
        error,
        terminal,
    }));
}

fn multiplex_market_events(
    mut data_rx: mpsc::Receiver<QueuedMarketEvent>,
    mut control_rx: watch::Receiver<Option<StreamInvalidation>>,
    generation: Arc<AtomicU64>,
) -> BoxStream<TrackedMarketEvent> {
    Box::pin(async_stream::stream! {
        let mut control_open = true;
        loop {
            tokio::select! {
                biased;
                changed = control_rx.changed(), if control_open => {
                    if changed.is_err() {
                        control_open = false;
                        continue;
                    }
                    let invalidation = control_rx.borrow_and_update().clone();
                    let Some(invalidation) = invalidation else {
                        continue;
                    };
                    if invalidation.generation != generation.load(Ordering::Acquire) {
                        continue;
                    }
                    yield Ok(TrackedMarketEvent::new(MarketEvent::Disconnect {
                        reason: invalidation.reason,
                        source_venue: Some(VenueId::BYBIT),
                        symbol: None,
                    }));
                    if let Some(error) = invalidation.error {
                        yield Err(error);
                    }
                    if invalidation.terminal {
                        break;
                    }
                }
                queued = data_rx.recv() => {
                    let Some(queued) = queued else {
                        break;
                    };
                    if queued.generation == generation.load(Ordering::Acquire) {
                        yield Ok(queued.event);
                    }
                }
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Spot,
    Linear,
}

impl Category {
    fn from_env() -> Self {
        match std::env::var("BYBIT_CATEGORY")
            .unwrap_or_else(|_| "spot".to_string())
            .to_lowercase()
            .as_str()
        {
            "linear" | "usdt" | "perp" => Category::Linear,
            _ => Category::Spot,
        }
    }
    fn ws_base(&self) -> &'static str {
        match self {
            Category::Spot => "wss://stream.bybit.com/v5/public/spot",
            Category::Linear => "wss://stream.bybit.com/v5/public/linear",
        }
    }

    fn max_subscription_args(self) -> usize {
        match self {
            Category::Spot => 10,
            Category::Linear => usize::MAX,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BybitWsMsg {
    topic: Option<String>,
    #[serde(rename = "type")]
    ty: Option<String>,
    ts: Option<u64>,
    data: Option<serde_json::Value>,
    success: Option<bool>,
    ret_msg: Option<String>,
    op: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrderBookData {
    s: String,
    b: Vec<[String; 2]>,
    a: Vec<[String; 2]>,
    u: u64,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    cts: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PublicTradeData {
    #[serde(rename = "T")]
    timestamp: u64,
    s: String,
    #[serde(rename = "S")]
    side: String,
    v: String,
    p: String,
    i: String,
}

/// 使用共用的 JSON 解析函數
#[cfg(test)]
#[inline]
fn parse_json<T: DeserializeOwned>(text: &str) -> hft_core::HftResult<T> {
    adapters_common::parse_json(text).map_err(Into::into)
}

#[inline]
fn parse_bytes<T: DeserializeOwned>(bytes: &mut [u8]) -> hft_core::HftResult<T> {
    adapters_common::parse_bytes(bytes).map_err(Into::into)
}

fn convert_levels(levels: Vec<[String; 2]>) -> HftResult<Vec<BookLevel>> {
    levels
        .into_iter()
        .map(|[price, quantity]| {
            Ok(BookLevel {
                price: Price::from_str(&price)
                    .map_err(|error| HftError::Parse(format!("Bybit price {price}: {error}")))?,
                quantity: {
                    let value = rust_decimal::Decimal::from_str(&quantity).map_err(|error| {
                        HftError::Parse(format!("Bybit quantity {quantity}: {error}"))
                    })?;
                    if value < rust_decimal::Decimal::ZERO {
                        return Err(HftError::Parse(format!(
                            "Bybit quantity cannot be negative: {quantity}"
                        )));
                    }
                    Quantity(value)
                },
            })
        })
        .collect()
}

fn convert_message_with_fast_bbo(
    message: BybitWsMsg,
    last_update_ids: &mut HashMap<String, u64>,
    fast_bbo_enabled: bool,
) -> HftResult<Vec<MarketEvent>> {
    if message.success == Some(false) {
        return Err(HftError::Config(format!(
            "Bybit {} failed: {}",
            message.op.as_deref().unwrap_or("request"),
            message.ret_msg.as_deref().unwrap_or("unknown error")
        )));
    }
    let Some(topic) = message.topic else {
        return Ok(Vec::new());
    };
    let Some(data) = message.data else {
        return Ok(Vec::new());
    };

    if topic.starts_with("orderbook.") {
        let data: OrderBookData = serde_json::from_value(data)
            .map_err(|error| HftError::Serialization(error.to_string()))?;
        let symbol = Symbol::from(data.s.clone());
        let timestamp = data
            .cts
            .or(message.ts)
            .unwrap_or_else(|| hft_core::now_micros() / 1000)
            .saturating_mul(1000);
        let cross_sequence = data.seq.unwrap_or(data.u);
        if fast_bbo_enabled && topic.starts_with("orderbook.1.") {
            let bid = convert_levels(data.b)?
                .into_iter()
                .next()
                .ok_or_else(|| HftError::Parse("Bybit L1 quote missing bid".to_string()))?;
            let ask = convert_levels(data.a)?
                .into_iter()
                .next()
                .ok_or_else(|| HftError::Parse("Bybit L1 quote missing ask".to_string()))?;
            if bid.quantity <= Quantity::zero()
                || ask.quantity <= Quantity::zero()
                || bid.price >= ask.price
            {
                return Err(HftError::Parse(format!(
                    "invalid Bybit L1 quote for {}",
                    symbol.as_str()
                )));
            }
            return Ok(vec![MarketEvent::Quote(TopOfBook {
                symbol,
                timestamp,
                sequence: cross_sequence,
                bid,
                ask,
                source_venue: Some(VenueId::BYBIT),
                timestamps: Default::default(),
            })]);
        }
        let is_snapshot = message.ty.as_deref() == Some("snapshot") || data.u == 1;
        if !is_snapshot {
            let Some(previous) = last_update_ids.get(&data.s).copied() else {
                return Ok(Vec::new());
            };
            if data.u <= previous {
                return Ok(Vec::new());
            }
        }
        last_update_ids.insert(data.s, data.u);
        let bids = convert_levels(data.b)?;
        let asks = convert_levels(data.a)?;
        if is_snapshot {
            Ok(vec![MarketEvent::Snapshot(MarketSnapshot {
                symbol,
                timestamp,
                bids,
                asks,
                sequence: cross_sequence,
                source_venue: Some(VenueId::BYBIT),
                timestamps: Default::default(),
            })])
        } else {
            Ok(vec![MarketEvent::Update(BookUpdate {
                symbol,
                timestamp,
                bids,
                asks,
                first_sequence: None,
                sequence: cross_sequence,
                is_snapshot: false,
                source_venue: Some(VenueId::BYBIT),
                timestamps: Default::default(),
            })])
        }
    } else if topic.starts_with("publicTrade.") {
        let trades: Vec<PublicTradeData> = serde_json::from_value(data)
            .map_err(|error| HftError::Serialization(error.to_string()))?;
        trades
            .into_iter()
            .map(|trade| {
                Ok(MarketEvent::Trade(Trade {
                    symbol: Symbol::from(trade.s),
                    timestamp: trade.timestamp.saturating_mul(1000),
                    price: Price::from_str(&trade.p).map_err(|error| {
                        HftError::Parse(format!("Bybit trade price {}: {error}", trade.p))
                    })?,
                    quantity: Quantity::from_str(&trade.v).map_err(|error| {
                        HftError::Parse(format!("Bybit trade volume {}: {error}", trade.v))
                    })?,
                    side: if trade.side.eq_ignore_ascii_case("Buy") {
                        hft_core::Side::Buy
                    } else {
                        hft_core::Side::Sell
                    },
                    trade_id: trade.i,
                    source_venue: Some(VenueId::BYBIT),
                    timestamps: Default::default(),
                }))
            })
            .collect()
    } else {
        Ok(Vec::new())
    }
}

#[cfg(test)]
fn convert_message(
    message: BybitWsMsg,
    last_update_ids: &mut HashMap<String, u64>,
) -> HftResult<Vec<MarketEvent>> {
    convert_message_with_fast_bbo(message, last_update_ids, false)
}

pub struct BybitMarketStream {
    category: Category,
    ws_url: Option<String>,
    depth_levels: usize,
    connected: Arc<AtomicBool>,
    last_heartbeat: Arc<AtomicU64>,
}

impl Default for BybitMarketStream {
    fn default() -> Self {
        Self::new()
    }
}

impl BybitMarketStream {
    pub fn new() -> Self {
        Self {
            category: Category::from_env(),
            ws_url: None,
            depth_levels: std::env::var("BYBIT_DEPTH_LEVELS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(50),
            connected: Arc::new(AtomicBool::new(false)),
            last_heartbeat: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_ws_url(mut self, ws_url: impl Into<String>) -> Self {
        self.ws_url = Some(ws_url.into());
        self
    }
}

#[async_trait]
impl MarketStream for BybitMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let stream = self.subscribe_tracked(symbols).await?;
        Ok(Box::pin(
            stream.map(|result| result.map(|tracked| tracked.event)),
        ))
    }

    async fn subscribe_tracked(
        &self,
        symbols: Vec<Symbol>,
    ) -> HftResult<BoxStream<TrackedMarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::new("Bybit symbols cannot be empty"));
        }
        let capacity = std::env::var("BYBIT_EVENT_QUEUE_CAPACITY")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|capacity| *capacity > 1)
            .unwrap_or(DEFAULT_EVENT_QUEUE_CAPACITY);
        let (tx, rx) = mpsc::channel(capacity);
        let generation = Arc::new(AtomicU64::new(1));
        let (control_tx, control_rx) = watch::channel(None);
        let url = self
            .ws_url
            .clone()
            .unwrap_or_else(|| self.category.ws_base().to_string());
        let max_subscription_args = self.category.max_subscription_args();
        let levels = self.depth_levels;
        if !matches!(levels, 1 | 50 | 200 | 1000) {
            return Err(HftError::Config(format!(
                "unsupported Bybit orderbook depth {levels}; expected 1, 50, 200, or 1000"
            )));
        }
        let connected = Arc::clone(&self.connected);
        let last_heartbeat = Arc::clone(&self.last_heartbeat);
        let task_generation = Arc::clone(&generation);

        tokio::spawn(async move {
            let mut attempts = 0u32;
            loop {
                let mut invalidation_error: Option<HftError> = None;
                let mut invalidation_reason =
                    "Bybit public WebSocket disconnected; awaiting fresh snapshot".to_string();
                let config = WsClientConfig {
                    url: url.clone(),
                    ..Default::default()
                };
                let mut ws = WsClient::new(config);

                match ws.connect().await {
                    Ok(()) => {
                        let fast_bbo_enabled = levels != 1;
                        let args: Vec<String> = symbols
                            .iter()
                            .flat_map(|s| {
                                let mut args = Vec::with_capacity(3);
                                if fast_bbo_enabled {
                                    args.push(format!("orderbook.1.{}", s.as_str()));
                                }
                                args.push(format!("orderbook.{}.{}", levels, s.as_str()));
                                args.push(format!("publicTrade.{}", s.as_str()));
                                args
                            })
                            .collect();
                        let mut subscribe_error = None;
                        for chunk in args.chunks(max_subscription_args) {
                            let sub = serde_json::json!({"op":"subscribe","args":chunk});
                            if let Err(error) = ws.send_message(&sub.to_string()).await {
                                subscribe_error = Some(error);
                                break;
                            }
                        }
                        if let Some(error) = subscribe_error {
                            error!("Bybit subscribe failed: {}", error);
                            invalidation_reason = "Bybit subscription write failed".to_string();
                            invalidation_error = Some(HftError::Network(error.to_string()));
                        } else {
                            connected.store(true, Ordering::Release);
                            last_heartbeat.store(hft_core::now_micros(), Ordering::Release);
                            let live_since = tokio::time::Instant::now();
                            let mut last_update_ids = HashMap::new();
                            let mut next_ping =
                                tokio::time::Instant::now() + std::time::Duration::from_secs(20);
                            'socket: loop {
                                let wait = next_ping
                                    .saturating_duration_since(tokio::time::Instant::now());
                                match tokio::time::timeout(wait, ws.receive_message_bytes()).await {
                                    Err(_) => {
                                        if let Err(error) =
                                            ws.send_message(r#"{"op":"ping"}"#).await
                                        {
                                            error!("Bybit heartbeat failed: {}", error);
                                            invalidation_reason =
                                                "Bybit heartbeat failed".to_string();
                                            invalidation_error =
                                                Some(HftError::Network(error.to_string()));
                                            break;
                                        }
                                        next_ping = tokio::time::Instant::now()
                                            + std::time::Duration::from_secs(20);
                                    }
                                    Ok(Ok(Some((bytes, mut metrics)))) => {
                                        last_heartbeat
                                            .store(hft_core::now_micros(), Ordering::Release);
                                        let mut bytes = match bytes.try_into_mut() {
                                            Ok(bytes) => bytes,
                                            Err(bytes) => BytesMut::from(bytes.as_ref()),
                                        };
                                        match parse_bytes::<BybitWsMsg>(&mut bytes).and_then(
                                            |message| {
                                                convert_message_with_fast_bbo(
                                                    message,
                                                    &mut last_update_ids,
                                                    fast_bbo_enabled,
                                                )
                                            },
                                        ) {
                                            Ok(events) => {
                                                metrics.mark_parsed();
                                                if !events.is_empty()
                                                    && live_since.elapsed()
                                                        >= std::time::Duration::from_secs(30)
                                                {
                                                    attempts = 0;
                                                }
                                                for event in events {
                                                    let mut tracker = LatencyTracker::from_userspace_websocket_message_delivery(
                                                        metrics.received_at_us,
                                                    );
                                                    tracker.record_stage_with_offset(
                                                        LatencyStage::WsReceive,
                                                        0,
                                                    );
                                                    tracker.record_stage_with_offset(
                                                        LatencyStage::Parsing,
                                                        metrics
                                                            .parsed_at_us
                                                            .saturating_sub(metrics.received_at_us),
                                                    );
                                                    let queued = QueuedMarketEvent {
                                                        generation: task_generation
                                                            .load(Ordering::Acquire),
                                                        event: TrackedMarketEvent {
                                                            event,
                                                            tracker,
                                                        },
                                                    };
                                                    match tx.try_send(queued) {
                                                        Ok(()) => {}
                                                        Err(mpsc::error::TrySendError::Closed(
                                                            _,
                                                        )) => {
                                                            return;
                                                        }
                                                        Err(mpsc::error::TrySendError::Full(_)) => {
                                                            invalidation_reason = format!(
                                                                "Bybit event queue exceeded {capacity} events; rebuilding from snapshot"
                                                            );
                                                            invalidation_error =
                                                                Some(HftError::Network(
                                                                    invalidation_reason.clone(),
                                                                ));
                                                            break 'socket;
                                                        }
                                                    }
                                                }
                                            }
                                            Err(error) => {
                                                let terminal =
                                                    matches!(&error, HftError::Config(_));
                                                let reason = if terminal {
                                                    "Bybit subscription rejected"
                                                } else {
                                                    "Bybit market frame rejected; rebuilding from snapshot"
                                                }
                                                .to_string();
                                                if terminal {
                                                    publish_invalidation(
                                                        &task_generation,
                                                        &control_tx,
                                                        reason,
                                                        Some(error),
                                                        true,
                                                    );
                                                    return;
                                                }
                                                invalidation_reason = reason;
                                                invalidation_error = Some(error);
                                                break 'socket;
                                            }
                                        }
                                    }
                                    Ok(Ok(None)) => {
                                        warn!("Bybit WS disconnected");
                                        break;
                                    }
                                    Ok(Err(error)) => {
                                        error!("Bybit WS error: {}", error);
                                        invalidation_error =
                                            Some(HftError::Network(error.to_string()));
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        error!("Bybit connect error: {}", error);
                        invalidation_reason =
                            "Bybit public WebSocket connection failed".to_string();
                        invalidation_error = Some(HftError::Network(error.to_string()));
                    }
                }
                connected.store(false, Ordering::Release);
                publish_invalidation(
                    &task_generation,
                    &control_tx,
                    invalidation_reason,
                    invalidation_error,
                    false,
                );
                if control_tx.is_closed() {
                    return;
                }
                attempts = attempts.saturating_add(1);
                let delay = adapters_common::calculate_exponential_backoff(
                    attempts,
                    adapters_common::ws_helpers::constants::DEFAULT_BASE_DELAY_MS,
                );
                tokio::time::sleep(delay).await;
            }
        });

        Ok(multiplex_market_events(rx, control_rx, generation))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected.load(Ordering::Acquire),
            latency_ms: None,
            last_heartbeat: self.last_heartbeat.load(Ordering::Acquire),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.connected.store(false, Ordering::Release);
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        self.connected.store(false, Ordering::Release);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use ports::MarketStream;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    // Category tests
    #[test]
    fn test_category_default_is_spot() {
        let _guard = env_guard();
        // Clear any env var that might affect the test
        std::env::remove_var("BYBIT_CATEGORY");
        let category = Category::from_env();
        assert_eq!(category, Category::Spot);
    }

    #[test]
    fn test_category_linear_from_env() {
        let _guard = env_guard();
        std::env::set_var("BYBIT_CATEGORY", "linear");
        let category = Category::from_env();
        assert_eq!(category, Category::Linear);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_usdt_maps_to_linear() {
        let _guard = env_guard();
        std::env::set_var("BYBIT_CATEGORY", "usdt");
        let category = Category::from_env();
        assert_eq!(category, Category::Linear);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_perp_maps_to_linear() {
        let _guard = env_guard();
        std::env::set_var("BYBIT_CATEGORY", "perp");
        let category = Category::from_env();
        assert_eq!(category, Category::Linear);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_unknown_defaults_to_spot() {
        let _guard = env_guard();
        std::env::set_var("BYBIT_CATEGORY", "unknown_value");
        let category = Category::from_env();
        assert_eq!(category, Category::Spot);
        std::env::remove_var("BYBIT_CATEGORY");
    }

    #[test]
    fn test_category_ws_base_spot() {
        let category = Category::Spot;
        assert_eq!(category.ws_base(), "wss://stream.bybit.com/v5/public/spot");
        assert_eq!(category.max_subscription_args(), 10);
    }

    #[test]
    fn test_category_ws_base_linear() {
        let category = Category::Linear;
        assert_eq!(
            category.ws_base(),
            "wss://stream.bybit.com/v5/public/linear"
        );
        assert_eq!(category.max_subscription_args(), usize::MAX);
    }

    #[test]
    fn test_category_clone() {
        let original = Category::Linear;
        let cloned = original;
        assert_eq!(cloned, Category::Linear);
    }

    #[test]
    fn test_category_debug() {
        let category = Category::Spot;
        let debug_str = format!("{:?}", category);
        assert!(debug_str.contains("Spot"));
    }

    // BybitMarketStream tests
    #[test]
    fn test_bybit_market_stream_new() {
        let stream = BybitMarketStream::new();
        // BybitMarketStream is a unit struct, just verify construction works
        let _ = stream;
    }

    #[test]
    fn test_bybit_market_stream_default() {
        let stream = BybitMarketStream::default();
        let _ = stream;
    }

    #[tokio::test]
    async fn test_health_check() {
        let stream = BybitMarketStream::new();
        let health = stream.health().await;

        assert!(!health.connected);
        assert!(health.latency_ms.is_none());
        assert_eq!(health.last_heartbeat, 0);
    }

    #[tokio::test]
    async fn test_connect() {
        let mut stream = BybitMarketStream::new();
        let result = stream.connect().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disconnect() {
        let mut stream = BybitMarketStream::new();
        let result = stream.disconnect().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_subscribe_empty_symbols_fails() {
        let stream = BybitMarketStream::new();
        let result = stream.subscribe(vec![]).await;

        assert!(result.is_err());
        if let Err(e) = result {
            let error_msg = format!("{}", e);
            assert!(error_msg.contains("empty"));
        }
    }

    #[tokio::test]
    async fn unsupported_depth_fails_before_opening_a_connection() {
        let mut stream = BybitMarketStream::new();
        stream.depth_levels = 20;

        let result = stream.subscribe(vec![Symbol::new("BTCUSDT")]).await;
        assert!(matches!(result, Err(HftError::Config(message)) if message.contains("depth 20")));
    }

    // BybitWsMsg parsing tests
    #[test]
    fn test_parse_bybit_ws_msg_with_topic() {
        let json = r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","data":{}}"#;
        let result: Result<BybitWsMsg, _> = serde_json::from_str(json);
        assert!(result.is_ok());
        let msg = result.unwrap();
        assert_eq!(msg.topic, Some("orderbook.50.BTCUSDT".to_string()));
        assert_eq!(msg.ty, Some("snapshot".to_string()));
    }

    #[test]
    fn test_parse_bybit_ws_msg_without_topic() {
        let json = r#"{"success":true,"ret_msg":"subscribe"}"#;
        let result: Result<BybitWsMsg, _> = serde_json::from_str(json);
        assert!(result.is_ok());
        let msg = result.unwrap();
        assert!(msg.topic.is_none());
    }

    #[test]
    fn failed_subscription_is_a_terminal_configuration_error() {
        let message: BybitWsMsg = serde_json::from_str(
            r#"{"success":false,"ret_msg":"Invalid symbol","op":"subscribe"}"#,
        )
        .unwrap();

        assert!(matches!(
            convert_message(message, &mut HashMap::new()),
            Err(HftError::Config(message)) if message.contains("Invalid symbol")
        ));
    }

    #[test]
    fn test_parse_json_function() {
        let json = r#"{"topic":"test","type":"delta"}"#;
        let result: HftResult<BybitWsMsg> = parse_json(json);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_json_invalid() {
        let json = "not valid json";
        let result: HftResult<BybitWsMsg> = parse_json(json);
        assert!(result.is_err());
    }

    #[tokio::test]
    #[ignore = "requires live Bybit public WebSocket"]
    async fn live_public_ws_delivers_snapshot_and_incremental_depth() {
        let stream = BybitMarketStream::new();
        let mut events = stream
            .subscribe(vec![Symbol::new("BTCUSDT")])
            .await
            .expect("public subscription");

        let (saw_quote, saw_snapshot, saw_update, saw_trade) =
            tokio::time::timeout(std::time::Duration::from_secs(15), async move {
                let mut saw_quote = false;
                let mut saw_snapshot = false;
                let mut saw_update = false;
                let mut saw_trade = false;
                while let Some(event) = events.next().await {
                    match event.expect("valid public event") {
                        MarketEvent::Quote(_) => saw_quote = true,
                        MarketEvent::Snapshot(_) => saw_snapshot = true,
                        MarketEvent::Update(_) => saw_update = true,
                        MarketEvent::Trade(_) => saw_trade = true,
                        _ => {}
                    }
                    if saw_quote && saw_snapshot && saw_update && saw_trade {
                        break;
                    }
                }
                (saw_quote, saw_snapshot, saw_update, saw_trade)
            })
            .await
            .expect("Bybit public feed timeout");

        assert!(saw_quote, "expected 10 ms WebSocket L1 quote");
        assert!(saw_snapshot, "expected WebSocket L50 snapshot");
        assert!(saw_update, "expected incremental L50 update");
        assert!(saw_trade, "expected real-time public trade");
    }

    #[test]
    fn snapshot_then_delta_remain_incremental_events() {
        let mut last_update_ids = HashMap::new();
        let snapshot: BybitWsMsg = serde_json::from_str(
            r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1000,"data":{"s":"BTCUSDT","b":[["100","2"]],"a":[["101","3"]],"u":10,"seq":20,"cts":999}}"#,
        )
        .unwrap();
        let events = convert_message(snapshot, &mut last_update_ids).unwrap();
        assert!(matches!(events.as_slice(), [MarketEvent::Snapshot(_)]));

        let delta: BybitWsMsg = serde_json::from_str(
            r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":1001,"data":{"s":"BTCUSDT","b":[["100","0"],["100.5","4"]],"a":[],"u":11,"seq":21,"cts":1000}}"#,
        )
        .unwrap();
        let events = convert_message(delta, &mut last_update_ids).unwrap();
        assert!(matches!(events.as_slice(), [MarketEvent::Update(_)]));
    }

    #[test]
    fn monotonic_update_ids_do_not_assume_undocumented_contiguity() {
        let mut last_update_ids = HashMap::new();
        let snapshot: BybitWsMsg = serde_json::from_str(
            r#"{"topic":"orderbook.50.BTCUSDT","type":"snapshot","ts":1000,"data":{"s":"BTCUSDT","b":[["100","2"]],"a":[["101","3"]],"u":10}}"#,
        )
        .unwrap();
        convert_message(snapshot, &mut last_update_ids).unwrap();
        let gap: BybitWsMsg = serde_json::from_str(
            r#"{"topic":"orderbook.50.BTCUSDT","type":"delta","ts":1001,"data":{"s":"BTCUSDT","b":[["100","4"]],"a":[],"u":12}}"#,
        )
        .unwrap();

        let events = convert_message(gap, &mut last_update_ids).unwrap();
        let [MarketEvent::Update(update)] = events.as_slice() else {
            panic!("expected monotonic delta");
        };
        assert_eq!(update.sequence, 12);
        assert_eq!(update.first_sequence, None);
    }

    #[test]
    fn l1_snapshot_is_a_realtime_quote_alongside_l50_depth() {
        let mut last_update_ids = HashMap::new();
        let message: BybitWsMsg = serde_json::from_str(
            r#"{"topic":"orderbook.1.BTCUSDT","type":"snapshot","ts":1000,"data":{"s":"BTCUSDT","b":[["100","2"]],"a":[["101","3"]],"u":10,"seq":20,"cts":999}}"#,
        )
        .unwrap();

        let events = convert_message_with_fast_bbo(message, &mut last_update_ids, true).unwrap();
        let [MarketEvent::Quote(quote)] = events.as_slice() else {
            panic!("expected realtime top-of-book quote");
        };
        assert_eq!(quote.bid.price, Price::from_str("100").unwrap());
        assert_eq!(quote.ask.quantity, Quantity::from_str("3").unwrap());
        assert_eq!(quote.sequence, 20);
        assert!(last_update_ids.is_empty());
    }

    #[tokio::test]
    async fn invalidation_preempts_and_discards_queued_old_generation() {
        let generation = Arc::new(AtomicU64::new(1));
        let (data_tx, data_rx) = mpsc::channel(2);
        let (control_tx, control_rx) = tokio::sync::watch::channel(None);
        let old = MarketEvent::Trade(Trade {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1,
            price: Price::from_str("100").unwrap(),
            quantity: Quantity::from_str("1").unwrap(),
            side: hft_core::Side::Buy,
            trade_id: "old".to_string(),
            source_venue: Some(VenueId::BYBIT),
            timestamps: Default::default(),
        });
        data_tx
            .try_send(QueuedMarketEvent {
                generation: 1,
                event: TrackedMarketEvent::new(old),
            })
            .unwrap();
        publish_invalidation(
            &generation,
            &control_tx,
            "bounded queue overflow".to_string(),
            None,
            false,
        );
        let fresh = MarketEvent::Trade(Trade {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 2,
            price: Price::from_str("101").unwrap(),
            quantity: Quantity::from_str("1").unwrap(),
            side: hft_core::Side::Buy,
            trade_id: "fresh".to_string(),
            source_venue: Some(VenueId::BYBIT),
            timestamps: Default::default(),
        });
        data_tx
            .try_send(QueuedMarketEvent {
                generation: 2,
                event: TrackedMarketEvent::new(fresh),
            })
            .unwrap();
        drop(data_tx);
        drop(control_tx);

        let mut stream = multiplex_market_events(data_rx, control_rx, generation);
        assert!(matches!(
            stream.next().await,
            Some(Ok(TrackedMarketEvent {
                event: MarketEvent::Disconnect { .. },
                ..
            }))
        ));
        let Some(Ok(TrackedMarketEvent {
            event: MarketEvent::Trade(trade),
            ..
        })) = stream.next().await
        else {
            panic!("expected fresh generation after invalidation");
        };
        assert_eq!(trade.trade_id, "fresh");
    }

    #[test]
    fn public_trade_uses_v_as_quantity() {
        let mut last_update_ids = HashMap::new();
        let message: BybitWsMsg = serde_json::from_str(
            r#"{"topic":"publicTrade.BTCUSDT","type":"snapshot","ts":1000,"data":[{"T":1000,"s":"BTCUSDT","S":"Buy","v":"0.25","p":"100.5","i":"trade-1"}]}"#,
        )
        .unwrap();
        let events = convert_message(message, &mut last_update_ids).unwrap();
        let MarketEvent::Trade(trade) = &events[0] else {
            panic!("expected trade event");
        };
        assert_eq!(trade.quantity, Quantity::from_str("0.25").unwrap());
    }
}
