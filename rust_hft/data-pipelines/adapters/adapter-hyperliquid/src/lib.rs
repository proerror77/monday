//! Hyperliquid 市场数据适配器
//!
//! 功能：
//! - 公共 WebSocket 订阅 orderbook/trades
//! - 解析生成统一 MarketEvent 事件
//! - 支持 BTC-PERP, ETH-PERP, SOL-PERP, SUI-PERP

use adapters_common::parse_owned_value;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use hft_core::{now_micros, HftError, HftResult, Price, Quantity, Side, Symbol, VenueId};
use ports::{
    BookLevel as PortsBookLevel, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot,
    MarketStream, Trade,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};
use url::Url;

#[derive(Debug, Clone)]
pub struct HyperliquidMarketConfig {
    pub ws_base_url: String,
    pub symbols: Vec<Symbol>,
    pub reconnect_interval_ms: u64,
    pub heartbeat_interval_ms: u64,
}

impl Default for HyperliquidMarketConfig {
    fn default() -> Self {
        Self {
            ws_base_url: "wss://api.hyperliquid.xyz/ws".to_string(),
            symbols: vec![
                Symbol::new("BTC-PERP"),
                Symbol::new("ETH-PERP"),
                Symbol::new("SOL-PERP"),
                Symbol::new("SUI-PERP"),
            ],
            reconnect_interval_ms: 5000,
            heartbeat_interval_ms: 30000,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SubscribeRequest {
    method: String,
    subscription: Subscription,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum Subscription {
    #[serde(rename = "l2Book")]
    L2Book { coin: String },
    #[serde(rename = "trades")]
    Trades { coin: String },
}

#[derive(Debug, Serialize, Deserialize)]
struct SubscriptionResponse {
    channel: String,
    data: Value,
}

/// 使用共用的 JSON 解析函數
#[inline]
fn parse_json<T: DeserializeOwned>(text: &str) -> HftResult<T> {
    adapters_common::parse_json(text).map_err(Into::into)
}

// Hyperliquid L2 订单簿数据结构
#[derive(Debug, Serialize, Deserialize)]
struct L2BookData {
    coin: String,
    levels: Vec<Vec<HlBookLevel>>, // [bids, asks]
    time: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct HlBookLevel {
    px: String, // price
    sz: String, // size
    n: u32,     // number of orders
}

// Hyperliquid 交易数据结构
#[derive(Debug, Serialize, Deserialize)]
struct TradeData {
    coin: String,
    side: String, // "A" for buy, "B" for sell
    px: String,   // price
    sz: String,   // size
    time: u64,
    hash: Option<String>,
}

pub struct HyperliquidMarketStream {
    config: HyperliquidMarketConfig,
    ws_stream: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    connected: bool,
    last_heartbeat: u64,
    symbol_mapping: HashMap<String, Symbol>, // hyperliquid symbol -> our symbol
}

impl HyperliquidMarketStream {
    pub fn new(config: HyperliquidMarketConfig) -> Self {
        let mut symbol_mapping = HashMap::new();

        // 映射 Hyperliquid 的资产名称
        symbol_mapping.insert("BTC".to_string(), Symbol::new("BTC-PERP"));
        symbol_mapping.insert("ETH".to_string(), Symbol::new("ETH-PERP"));
        symbol_mapping.insert("SOL".to_string(), Symbol::new("SOL-PERP"));
        symbol_mapping.insert("SUI".to_string(), Symbol::new("SUI-PERP"));

        Self {
            config,
            ws_stream: None,
            connected: false,
            last_heartbeat: now_micros(),
            symbol_mapping,
        }
    }

    async fn establish_connection(&mut self) -> HftResult<()> {
        let url = Url::parse(&self.config.ws_base_url)
            .map_err(|e| HftError::Config(format!("无效的 WebSocket URL: {}", e)))?;

        debug!("连接到 Hyperliquid WebSocket: {}", url);

        let (ws_stream, _) = connect_async(url.as_str())
            .await
            .map_err(|e| HftError::Network(format!("WebSocket 连接失败: {}", e)))?;

        self.ws_stream = Some(ws_stream);
        self.connected = true;
        self.last_heartbeat = now_micros();

        info!("Hyperliquid WebSocket 连接已建立");
        Ok(())
    }

    async fn subscribe_symbols(&mut self) -> HftResult<()> {
        let symbols = self.config.symbols.clone();
        if let Some(ref mut ws) = self.ws_stream {
            for symbol in &symbols {
                let coin = extract_coin_from_symbol(symbol)?;

                // 订阅 L2 订单簿
                let l2_request = SubscribeRequest {
                    method: "subscribe".to_string(),
                    subscription: Subscription::L2Book { coin: coin.clone() },
                };

                let l2_message = serde_json::to_string(&l2_request)
                    .map_err(|e| HftError::Serialization(format!("序列化订阅请求失败: {}", e)))?;

                ws.send(Message::Text(l2_message.into()))
                    .await
                    .map_err(|e| HftError::Network(format!("发送订阅请求失败: {}", e)))?;

                debug!("已订阅 {} L2Book", symbol);

                // 订阅交易流
                let trades_request = SubscribeRequest {
                    method: "subscribe".to_string(),
                    subscription: Subscription::Trades { coin: coin.clone() },
                };

                let trades_message = serde_json::to_string(&trades_request).map_err(|e| {
                    HftError::Serialization(format!("序列化交易订阅请求失败: {}", e))
                })?;

                ws.send(Message::Text(trades_message.into()))
                    .await
                    .map_err(|e| HftError::Network(format!("发送交易订阅请求失败: {}", e)))?;

                debug!("已订阅 {} Trades", symbol);
            }
        }

        Ok(())
    }
}

fn extract_coin_from_symbol(symbol: &Symbol) -> HftResult<String> {
    let symbol_str = &symbol.as_str();
    if let Some(coin) = symbol_str.strip_suffix("-PERP") {
        Ok(coin.to_string())
    } else {
        Err(HftError::Config(format!(
            "无效的永续合约符号: {}",
            symbol_str
        )))
    }
}

impl HyperliquidMarketStream {
    fn parse_message(&self, message: &str) -> Option<MarketEvent> {
        let parsed: Result<SubscriptionResponse, _> = parse_json(message);

        match parsed {
            Ok(response) => match response.channel.as_str() {
                "l2Book" => self.parse_l2_book(&response.data),
                "trades" => self.parse_trades(&response.data),
                "subscriptionResponse" => {
                    debug!("收到订阅确认: {:?}", response.data);
                    None
                }
                _ => {
                    debug!("未知消息类型: {}", response.channel);
                    None
                }
            },
            Err(e) => {
                warn!("解析 WebSocket 消息失败: {} | 消息: {}", e, message);
                None
            }
        }
    }

    fn parse_l2_book(&self, data: &Value) -> Option<MarketEvent> {
        match parse_owned_value::<L2BookData>(data.clone()) {
            Ok(book) => {
                let symbol = self.symbol_mapping.get(&book.coin)?.clone();

                if let Some(_levels) = book.levels.first() {
                    // bids
                    // 構建完整的快照而不是只處理最佳價位

                    let mut bids = Vec::new();
                    let mut asks = Vec::new();

                    // 解析 bids (first array)
                    if let Some(bid_levels) = book.levels.first() {
                        for level in bid_levels {
                            if let (Ok(price), Ok(qty)) =
                                (level.px.parse::<f64>(), level.sz.parse::<f64>())
                            {
                                if let Ok(book_level) = PortsBookLevel::new(price, qty) {
                                    bids.push(book_level);
                                }
                            }
                        }
                    }

                    // 解析 asks (second array)
                    if let Some(ask_levels) = book.levels.get(1) {
                        for level in ask_levels {
                            if let (Ok(price), Ok(qty)) =
                                (level.px.parse::<f64>(), level.sz.parse::<f64>())
                            {
                                if let Ok(book_level) = PortsBookLevel::new(price, qty) {
                                    asks.push(book_level);
                                }
                            }
                        }
                    }

                    let snapshot = MarketSnapshot {
                        symbol,
                        // Hyperliquid 提供毫秒時間戳，轉為微秒
                        timestamp: book.time.saturating_mul(1000),
                        bids,
                        asks,
                        sequence: book.time, // 使用時間戳作為序列號
                        source_venue: Some(VenueId::HYPERLIQUID),
                    };

                    return Some(MarketEvent::Snapshot(snapshot));
                }

                None
            }
            Err(e) => {
                warn!("解析 L2 Book 数据失败: {}", e);
                None
            }
        }
    }

    fn parse_trades(&self, data: &Value) -> Option<MarketEvent> {
        // Hyperliquid 交易数据可能是数组
        if let Ok(trades) = parse_owned_value::<Vec<TradeData>>(data.clone()) {
            if let Some(trade) = trades.first() {
                let symbol = self.symbol_mapping.get(&trade.coin)?.clone();
                let price = trade.px.parse::<f64>().ok()?;
                let quantity = trade.sz.parse::<f64>().ok()?;
                let side = match trade.side.as_str() {
                    "A" => Side::Buy,  // Aggressive buy
                    "B" => Side::Sell, // Aggressive sell
                    _ => return None,
                };

                let trade_event = Trade {
                    symbol,
                    // 交易時間 (ms) → μs
                    timestamp: trade.time.saturating_mul(1000),
                    price: Price::from_f64(price).unwrap_or(Price::from_f64(0.0).unwrap()),
                    quantity: Quantity::from_f64(quantity)
                        .unwrap_or(Quantity::from_f64(0.0).unwrap()),
                    side,
                    trade_id: trade
                        .hash
                        .clone()
                        .unwrap_or_else(|| format!("HL_{}", trade.time)),
                    source_venue: Some(VenueId::HYPERLIQUID),
                };

                return Some(MarketEvent::Trade(trade_event));
            }
        }

        None
    }
}

#[async_trait]
impl MarketStream for HyperliquidMarketStream {
    async fn subscribe(&self, _symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        let mut stream_clone = HyperliquidMarketStream::new(self.config.clone());

        // 建立连接并订阅
        stream_clone.connect().await?;

        let stream = async_stream::stream! {
            loop {
                if let Some(ref mut ws) = stream_clone.ws_stream {
                    match ws.next().await {
                        Some(Ok(Message::Text(text))) => {
                            if let Some(event) = stream_clone.parse_message(&text) {
                                yield Ok(event);
                            }
                        },
                        Some(Ok(Message::Ping(data))) => {
                            // 回应 ping
                            if let Err(e) = ws.send(Message::Pong(data)).await {
                                error!("发送 pong 失败: {}", e);
                                break;
                            }
                            stream_clone.last_heartbeat = now_micros();
                        },
                        Some(Ok(Message::Close(_))) => {
                            warn!("WebSocket 连接被关闭");
                            stream_clone.connected = false;
                            break;
                        },
                        Some(Err(e)) => {
                            error!("WebSocket 错误: {}", e);
                            stream_clone.connected = false;
                            break;
                        },
                        None => {
                            warn!("WebSocket 流结束");
                            break;
                        },
                        _ => {
                            // 忽略其他消息类型
                        }
                    }
                } else {
                    // 尝试重连
                    match stream_clone.establish_connection().await {
                        Ok(_) => {
                            if let Err(e) = stream_clone.subscribe_symbols().await {
                                error!("重新订阅失败: {}", e);
                                tokio::time::sleep(tokio::time::Duration::from_millis(
                                    stream_clone.config.reconnect_interval_ms
                                )).await;
                            }
                        },
                        Err(e) => {
                            error!("重连失败: {}", e);
                            tokio::time::sleep(tokio::time::Duration::from_millis(
                                stream_clone.config.reconnect_interval_ms
                            )).await;
                        }
                    }
                }
            }
        };

        Ok(Box::pin(stream))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.connected,
            // 延遲需要主動 ping 測量，目前僅被動回應 ping，無法計算 RTT
            latency_ms: None,
            last_heartbeat: self.last_heartbeat,
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.establish_connection().await?;
        self.subscribe_symbols().await?;
        info!("Hyperliquid MarketStream 连接成功");
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        if let Some(mut ws) = self.ws_stream.take() {
            let _ = ws.send(Message::Close(None)).await;
            let _ = ws.close(None).await;
        }
        self.connected = false;
        info!("Hyperliquid MarketStream 已断开连接");
        Ok(())
    }
}

// 便捷构造函数
impl HyperliquidMarketStream {
    pub fn with_symbols(symbols: Vec<Symbol>) -> Self {
        let config = HyperliquidMarketConfig {
            symbols,
            ..HyperliquidMarketConfig::default()
        };
        Self::new(config)
    }

    pub fn mainnet() -> Self {
        Self::new(HyperliquidMarketConfig::default())
    }

    pub fn testnet() -> Self {
        let config = HyperliquidMarketConfig {
            ws_base_url: "wss://api.hyperliquid-testnet.xyz/ws".to_string(),
            ..Default::default()
        };
        Self::new(config)
    }
}

#[cfg(test)]
mod tests;
