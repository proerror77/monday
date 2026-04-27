//! Backpack Exchange 市場數據適配器（WebSocket 版）
//!
//! 透過 `wss://ws.backpack.exchange` 訂閱 `depth.<symbol>` 與 `trade.<symbol>`
//! 將資料轉換為統一的 `MarketEvent`/`Trade` 事件。

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use adapters_common::parse_owned_value;
use async_stream::stream;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{now_micros, HftError, HftResult, Price, Quantity, Side, Symbol, VenueId};
use ports::{
    BookLevel, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream, Trade,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

/// 使用共用的 JSON 解析函數
#[inline]
fn parse_bytes<T: serde::de::DeserializeOwned>(bytes: &mut [u8]) -> Result<T, HftError> {
    adapters_common::parse_bytes(bytes).map_err(Into::into)
}

pub const DEFAULT_WS_URL: &str = "wss://ws.backpack.exchange";

fn default_ws_url() -> String {
    DEFAULT_WS_URL.to_string()
}

fn default_subscribe_depth() -> bool {
    true
}

fn default_subscribe_trades() -> bool {
    true
}

fn default_reconnect_interval_ms() -> u64 {
    5000
}

fn default_symbols() -> Vec<Symbol> {
    vec![Symbol::new("SOL_USDC")]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackpackMarketConfig {
    /// WebSocket 端點
    #[serde(default = "default_ws_url")]
    pub ws_url: String,
    /// 是否訂閱 depth stream
    #[serde(default = "default_subscribe_depth")]
    pub subscribe_depth: bool,
    /// 是否訂閱 trade stream
    #[serde(default = "default_subscribe_trades")]
    pub subscribe_trades: bool,
    /// 重連間隔（毫秒）
    #[serde(default = "default_reconnect_interval_ms")]
    pub reconnect_interval_ms: u64,
    /// 未指定 symbols 時的預設訂閱
    #[serde(default = "default_symbols")]
    pub default_symbols: Vec<Symbol>,
}

impl Default for BackpackMarketConfig {
    fn default() -> Self {
        Self {
            ws_url: default_ws_url(),
            subscribe_depth: default_subscribe_depth(),
            subscribe_trades: default_subscribe_trades(),
            reconnect_interval_ms: default_reconnect_interval_ms(),
            default_symbols: default_symbols(),
        }
    }
}

struct ConnectionState {
    connected: AtomicBool,
    last_heartbeat: AtomicU64,
}

impl ConnectionState {
    fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            last_heartbeat: AtomicU64::new(now_micros()),
        }
    }
}

pub struct BackpackMarketStream {
    config: BackpackMarketConfig,
    state: Arc<ConnectionState>,
}

impl Default for BackpackMarketStream {
    fn default() -> Self {
        Self::new(BackpackMarketConfig::default())
    }
}

impl BackpackMarketStream {
    pub fn new(config: BackpackMarketConfig) -> Self {
        Self {
            config,
            state: Arc::new(ConnectionState::new()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WsEnvelope {
    stream: Option<String>,
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct DepthEvent {
    #[serde(rename = "E")]
    event_time: u64,
    #[serde(rename = "T")]
    engine_time: Option<u64>,
    #[serde(rename = "U")]
    first_update_id: Option<u64>,
    #[serde(rename = "u")]
    final_update_id: Option<u64>,
    #[serde(default, rename = "b")]
    bids: Vec<[String; 2]>,
    #[serde(default, rename = "a")]
    asks: Vec<[String; 2]>,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "e")]
    _event_type: String,
}

#[derive(Debug, Deserialize)]
struct TradeEvent {
    #[serde(rename = "E")]
    _event_time: u64,
    #[serde(rename = "T")]
    trade_time: u64,
    #[serde(rename = "p")]
    price: String,
    #[serde(rename = "q")]
    quantity: String,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "t")]
    trade_id: u64,
    #[serde(rename = "m")]
    buyer_is_maker: bool,
}

fn parse_level(pair: [String; 2], label: &str) -> HftResult<BookLevel> {
    let [price_str, qty_str] = pair;
    let price = price_str
        .parse::<f64>()
        .map_err(|e| HftError::Parse(format!("Backpack {label} price parse error: {e}")))?;
    let qty = qty_str
        .parse::<f64>()
        .map_err(|e| HftError::Parse(format!("Backpack {label} quantity parse error: {e}")))?;

    BookLevel::new(price, qty)
        .map_err(|e| HftError::Parse(format!("Backpack {label} level conversion error: {e}")))
}

fn convert_depth(levels: Vec<[String; 2]>, label: &str) -> HftResult<Vec<BookLevel>> {
    let mut out = Vec::with_capacity(levels.len());
    for pair in levels {
        out.push(parse_level(pair, label)?);
    }
    Ok(out)
}

#[async_trait]
impl MarketStream for BackpackMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let mut effective_symbols = if symbols.is_empty() {
            self.config.default_symbols.clone()
        } else {
            symbols
        };

        if effective_symbols.is_empty() {
            return Err(HftError::Config(
                "BackpackMarketStream requires at least one symbol".into(),
            ));
        }

        effective_symbols.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        effective_symbols.dedup_by(|a, b| a.as_str() == b.as_str());

        let subscribe_depth = self.config.subscribe_depth;
        let subscribe_trades = self.config.subscribe_trades;
        if !subscribe_depth && !subscribe_trades {
            return Err(HftError::Config(
                "Backpack data_config 必須至少啟用 depth 或 trade 訂閱".into(),
            ));
        }

        let symbol_map: HashMap<String, Symbol> = effective_symbols
            .iter()
            .map(|sym| (sym.as_str().to_string(), sym.clone()))
            .collect();

        let streams: Vec<String> = effective_symbols
            .iter()
            .flat_map(|sym| {
                let mut v = Vec::new();
                if subscribe_depth {
                    v.push(format!("depth.{}", sym.as_str()));
                }
                if subscribe_trades {
                    v.push(format!("trade.{}", sym.as_str()));
                }
                v
            })
            .collect();

        let ws_url = self.config.ws_url.clone();
        let reconnect_delay = Duration::from_millis(self.config.reconnect_interval_ms.max(500));
        let state = self.state.clone();

        let stream = stream! {
            let mut snapshot_sent: HashSet<String> = HashSet::new();

            loop {
                match connect_async(&ws_url).await {
                    Ok((mut ws, _)) => {
                        info!("Backpack websocket connected: {}", ws_url);
                        state.connected.store(true, Ordering::SeqCst);
                        state
                            .last_heartbeat
                            .store(now_micros(), Ordering::SeqCst);

                        if !streams.is_empty() {
                            let payload = json!({
                                "method": "SUBSCRIBE",
                                "params": streams,
                            });
                            if let Err(e) = ws.send(Message::Text(payload.to_string().into())).await {
                                state.connected.store(false, Ordering::SeqCst);
                                yield Err(HftError::Network(format!(
                                    "Backpack subscription failed: {e}"
                                )));
                                sleep(reconnect_delay).await;
                                continue;
                            }
                        }

                        while let Some(msg) = ws.next().await {
                            match msg {
                                Ok(Message::Text(text)) => {
                                    state
                                        .last_heartbeat
                                        .store(now_micros(), Ordering::SeqCst);

                                    let mut bytes = text.to_string().into_bytes();
                                    let envelope: WsEnvelope = match parse_bytes(bytes.as_mut_slice()) {
                                        Ok(env) => env,
                                        Err(e) => {
                                            warn!("Backpack envelope parse error: {}", e);
                                            continue;
                                        }
                                    };

                                    let Some(stream_name) = envelope.stream else { continue };
                                    let Some(data) = envelope.data else { continue };

                                    if stream_name.starts_with("depth.") {
                                        match parse_owned_value::<DepthEvent>(data) {
                                            Ok(evt) => {
                                                if let Some(event) = handle_depth_event(
                                                    &evt,
                                                    &symbol_map,
                                                    &mut snapshot_sent,
                                                ) {
                                                    yield Ok(event);
                                                }
                                            }
                                            Err(e) => warn!("Backpack depth parse error: {}", e),
                                        }
                                    } else if stream_name.starts_with("trade.") {
                                        match parse_owned_value::<TradeEvent>(data) {
                                            Ok(evt) => {
                                                if let Some(event) = handle_trade_event(&evt, &symbol_map) {
                                                    yield Ok(event);
                                                }
                                            }
                                            Err(e) => warn!("Backpack trade parse error: {}", e),
                                        }
                                    }
                                }
                                Ok(Message::Ping(payload)) => {
                                    let _ = ws.send(Message::Pong(payload)).await;
                                    state
                                        .last_heartbeat
                                        .store(now_micros(), Ordering::SeqCst);
                                }
                                Ok(Message::Pong(_)) => {
                                    state
                                        .last_heartbeat
                                        .store(now_micros(), Ordering::SeqCst);
                                }
                                Ok(Message::Close(_)) => {
                                    state.connected.store(false, Ordering::SeqCst);
                                    break;
                                }
                                Ok(Message::Binary(_)) => {
                                    // ignore
                                }
                                Ok(Message::Frame(_)) => {
                                    // ignore
                                }
                                Err(e) => {
                                    state.connected.store(false, Ordering::SeqCst);
                                    yield Err(HftError::Network(format!(
                                        "Backpack websocket error: {e}"
                                    )));
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        state.connected.store(false, Ordering::SeqCst);
                        yield Err(HftError::Network(format!(
                            "Backpack websocket connect failed: {e}"
                        )));
                    }
                }

                sleep(reconnect_delay).await;
            }
        };

        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.state.connected.load(Ordering::SeqCst),
            latency_ms: None,
            last_heartbeat: self.state.last_heartbeat.load(Ordering::SeqCst),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.state.connected.store(true, Ordering::SeqCst);
        self.state
            .last_heartbeat
            .store(now_micros(), Ordering::SeqCst);
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        self.state.connected.store(false, Ordering::SeqCst);
        Ok(())
    }
}

fn handle_depth_event(
    evt: &DepthEvent,
    symbol_map: &HashMap<String, Symbol>,
    snapshot_sent: &mut HashSet<String>,
) -> Option<MarketEvent> {
    let sym = symbol_map.get(&evt.symbol)?;
    let bids = convert_depth(evt.bids.clone(), "bid").ok()?;
    let asks = convert_depth(evt.asks.clone(), "ask").ok()?;
    let timestamp = evt.event_time;
    let sequence = evt
        .final_update_id
        .or(evt.first_update_id)
        .unwrap_or_else(|| evt.engine_time.unwrap_or(0));

    if snapshot_sent.insert(evt.symbol.clone()) {
        Some(MarketEvent::Snapshot(MarketSnapshot {
            symbol: sym.clone(),
            timestamp,
            bids,
            asks,
            sequence,
            source_venue: Some(VenueId::BACKPACK),
        }))
    } else {
        if bids.is_empty() && asks.is_empty() {
            return None;
        }
        Some(MarketEvent::Update(ports::BookUpdate {
            symbol: sym.clone(),
            timestamp,
            bids,
            asks,
            sequence,
            is_snapshot: false,
            source_venue: Some(VenueId::BACKPACK),
        }))
    }
}

fn handle_trade_event(
    evt: &TradeEvent,
    symbol_map: &HashMap<String, Symbol>,
) -> Option<MarketEvent> {
    let sym = symbol_map.get(&evt.symbol)?;
    let price = Price::from_str(&evt.price).ok()?;
    let quantity = Quantity::from_str(&evt.quantity).ok()?;
    let side = if evt.buyer_is_maker {
        Side::Sell
    } else {
        Side::Buy
    };

    Some(MarketEvent::Trade(Trade {
        symbol: sym.clone(),
        timestamp: evt.trade_time,
        price,
        quantity,
        side,
        trade_id: evt.trade_id.to_string(),
        source_venue: Some(VenueId::BACKPACK),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_level() {
        let lvl = parse_level(["234.5".to_string(), "1.2".to_string()], "bid").unwrap();
        assert_eq!(lvl.price.to_string(), "234.5");
        assert_eq!(lvl.quantity.to_string(), "1.2");
    }
}
