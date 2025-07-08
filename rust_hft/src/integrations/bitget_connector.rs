/*!
 * Bitget WebSocket V2 API Connector
 * 
 * Based on the official Bitget V2 API documentation and working Python implementation
 * Supports public market data streams (orderbook, trades) with proper V2 message format
 */

use crate::types::*;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};

/// Reconnection strategy for different data types
#[derive(Debug, Clone)]
pub enum ReconnectStrategy {
    /// Fast reconnect for ticker data (100ms-2s)
    Fast,
    /// Standard reconnect for orderbook data (1s-5s)
    Standard,
    /// Custom reconnect with user-defined delays
    Custom {
        initial_delay_ms: u64,
        max_delay_ms: u64,
        multiplier: f64,
    },
}

impl Default for ReconnectStrategy {
    fn default() -> Self {
        ReconnectStrategy::Standard
    }
}

/// Bitget V2 WebSocket configuration
#[derive(Debug, Clone)]
pub struct BitgetConfig {
    /// Public WebSocket URL for market data
    pub public_ws_url: String,
    
    /// Private WebSocket URL for trading (requires authentication)
    pub private_ws_url: String,
    
    /// Connection timeout in seconds
    pub timeout_seconds: u64,
    
    /// Auto-reconnect on connection loss
    pub auto_reconnect: bool,
    
    /// Maximum reconnection attempts
    pub max_reconnect_attempts: usize,
    
    /// Reconnection strategy
    pub reconnect_strategy: ReconnectStrategy,
}

impl Default for BitgetConfig {
    fn default() -> Self {
        Self {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 3,
            reconnect_strategy: ReconnectStrategy::default(),
        }
    }
}

impl BitgetConfig {
    /// Create a config optimized for ticker data collection
    pub fn for_ticker_data() -> Self {
        Self {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 5, // More attempts for ticker data
            reconnect_strategy: ReconnectStrategy::Fast,
        }
    }
    
    /// Create a config optimized for orderbook data collection
    pub fn for_orderbook_data() -> Self {
        Self {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 3,
            reconnect_strategy: ReconnectStrategy::Standard,
        }
    }
    
    /// Create a config with custom reconnection strategy
    pub fn with_custom_reconnect(initial_delay_ms: u64, max_delay_ms: u64, multiplier: f64) -> Self {
        Self {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 3,
            reconnect_strategy: ReconnectStrategy::Custom {
                initial_delay_ms,
                max_delay_ms,
                multiplier,
            },
        }
    }
}

/// Bitget V2 subscription types
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BitgetChannel {
    /// books5 - 5-level orderbook depth
    Books5,
    /// books1 - Best bid/ask only  
    Books1,
    /// books15 - 15-level orderbook depth
    Books15,
    /// ticker - 24hr ticker statistics
    Ticker,
    /// trade - Real-time trades
    Trade,
    /// candle1m - 1-minute candlestick data
    Candle1m,
}

impl BitgetChannel {
    fn as_str(&self) -> &str {
        match self {
            BitgetChannel::Books5 => "books5",
            BitgetChannel::Books1 => "books1", 
            BitgetChannel::Books15 => "books15",
            BitgetChannel::Ticker => "ticker",
            BitgetChannel::Trade => "trade",
            BitgetChannel::Candle1m => "candle1m",
        }
    }
}

/// Bitget V2 WebSocket connector
#[derive(Debug, Clone)]
pub struct BitgetConnector {
    config: BitgetConfig,
    subscriptions: HashMap<String, Vec<BitgetChannel>>,
}

impl BitgetConnector {
    pub fn new(config: BitgetConfig) -> Self {
        Self {
            config,
            subscriptions: HashMap::new(),
        }
    }
    
    /// Add a subscription for a trading pair
    pub fn add_subscription(&mut self, symbol: String, channel: BitgetChannel) {
        self.subscriptions.entry(symbol).or_default().push(channel);
    }
    
    /// Connect to Bitget public WebSocket and start streaming
    pub async fn connect_public(&self, mut message_handler: impl FnMut(BitgetMessage) + Send + 'static) -> Result<()> {
        let mut reconnect_attempts = 0;
        let mut consecutive_failures = 0;
        
        loop {
            match self.try_connect_public(&mut message_handler).await {
                Ok(_) => {
                    info!("Bitget public WebSocket disconnected normally");
                    consecutive_failures = 0; // 重置失败计数
                    
                    // 正常断开后等待一下再重连
                    if self.config.auto_reconnect {
                        warn!("Connection lost, auto-reconnecting in 2s...");
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                        continue;
                    } else {
                        return Ok(());
                    }
                }
                Err(e) => {
                    reconnect_attempts += 1;
                    consecutive_failures += 1;
                    error!("Bitget connection attempt {} failed: {}", reconnect_attempts, e);
                    
                    if self.config.auto_reconnect {
                        let delay_ms = self.calculate_adaptive_reconnect_delay(consecutive_failures);
                        warn!("Retrying connection in {}ms... (attempt: {}, consecutive failures: {})", 
                              delay_ms, reconnect_attempts, consecutive_failures);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                        
                        // 如果连续失败太多次，增加额外延迟
                        if consecutive_failures >= 10 {
                            warn!("Too many consecutive failures, adding extra delay...");
                            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                            consecutive_failures = 0; // 重置计数
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }
    }
    
    /// Calculate adaptive reconnection delay based on consecutive failures
    fn calculate_adaptive_reconnect_delay(&self, consecutive_failures: usize) -> u64 {
        match &self.config.reconnect_strategy {
            ReconnectStrategy::Fast => {
                // 快速策略：针对ticker数据优化
                match consecutive_failures {
                    1 => 200,      // 200ms - 第一次失败快速重试
                    2 => 500,      // 500ms
                    3 => 1000,     // 1s
                    4 => 2000,     // 2s
                    5 => 3000,     // 3s
                    6..=9 => 5000, // 5s
                    _ => 10000,    // 10s - 最大延迟
                }
            }
            ReconnectStrategy::Standard => {
                // 标准策略：指数退避
                let base_delay = match consecutive_failures {
                    1 => 1000,     // 1s
                    2 => 2000,     // 2s
                    3 => 4000,     // 4s
                    4 => 8000,     // 8s
                    5 => 15000,    // 15s
                    6..=9 => 30000, // 30s
                    _ => 60000,    // 60s - 最大延迟
                };
                base_delay
            }
            ReconnectStrategy::Conservative => {
                // 保守策略：较长延迟避免服务器过载
                match consecutive_failures {
                    1 => 5000,     // 5s
                    2 => 10000,    // 10s
                    3 => 20000,    // 20s
                    4 => 30000,    // 30s
                    5 => 60000,    // 60s
                    _ => 120000,   // 120s - 最大延迟
                }
            }
        }
    }

    /// Calculate adaptive reconnection delay based on strategy (旧方法保留兼容性)
    fn calculate_reconnect_delay(&self, attempt: usize) -> u64 {
        match &self.config.reconnect_strategy {
            ReconnectStrategy::Fast => {
                // Fast strategy for ticker data: 100ms -> 500ms -> 1s -> 2s (max)
                match attempt {
                    1 => 100,
                    2 => 500,
                    3 => 1000,
                    _ => 2000,
                }
            }
            ReconnectStrategy::Standard => {
                // Standard strategy: 1s -> 3s -> 5s
                match attempt {
                    1 => 1000,
                    2 => 3000,
                    _ => 5000,
                }
            }
            ReconnectStrategy::Custom { initial_delay_ms, max_delay_ms, multiplier } => {
                // Custom exponential backoff
                let delay = (*initial_delay_ms as f64 * multiplier.powi(attempt as i32 - 1)) as u64;
                delay.min(*max_delay_ms)
            }
        }
    }
    
    async fn try_connect_public(&self, message_handler: &mut (impl FnMut(BitgetMessage) + Send + 'static)) -> Result<()> {
        info!("Connecting to Bitget V2 public WebSocket: {}", self.config.public_ws_url);
        
        let (ws_stream, _) = connect_async(&self.config.public_ws_url).await?;
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        
        info!("Bitget V2 WebSocket connected successfully");
        
        // Send subscriptions
        for (symbol, channels) in &self.subscriptions {
            for channel in channels {
                let subscription = self.create_subscription_message(symbol, channel)?;
                ws_sender.send(Message::Text(subscription)).await?;
                info!("Subscribed to {} {} on Bitget V2", channel.as_str(), symbol);
            }
        }
        
        // Set up heartbeat mechanism
        let ping_interval = tokio::time::Duration::from_secs(30);
        let mut ping_timer = tokio::time::interval(ping_interval);
        let mut last_activity = tokio::time::Instant::now();
        
        // Main message handling loop with heartbeat
        loop {
            tokio::select! {
                // Handle incoming messages
                message = ws_receiver.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            last_activity = tokio::time::Instant::now();
                            match self.parse_message(&text) {
                                Ok(Some(bitget_msg)) => {
                                    message_handler(bitget_msg);
                                }
                                Ok(None) => {
                                    debug!("Received non-data message: {}", text);
                                }
                                Err(e) => {
                                    warn!("Failed to parse message: {}, content: {}", e, text);
                                }
                            }
                        }
                        Some(Ok(Message::Binary(data))) => {
                            last_activity = tokio::time::Instant::now();
                            if let Ok(text) = String::from_utf8(data) {
                                debug!("Received binary message: {}", text);
                            }
                        }
                        Some(Ok(Message::Close(close_frame))) => {
                            info!("Bitget WebSocket connection closed: {:?}", close_frame);
                            break;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            last_activity = tokio::time::Instant::now();
                            debug!("Received ping, sending pong");
                            if let Err(e) = ws_sender.send(Message::Pong(data)).await {
                                error!("Failed to send pong: {}", e);
                                break;
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            last_activity = tokio::time::Instant::now();
                            debug!("Received pong");
                        }
                        Some(Ok(Message::Frame(_))) => {
                            debug!("Received raw frame message");
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            warn!("WebSocket stream ended unexpectedly");
                            break;
                        }
                    }
                }
                
                // Send periodic ping messages
                _ = ping_timer.tick() => {
                    if let Err(e) = ws_sender.send(Message::Ping(b"heartbeat".to_vec())).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                    debug!("Sent heartbeat ping");
                }
                
                // Check for connection timeout
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {
                    if last_activity.elapsed() > tokio::time::Duration::from_secs(90) {
                        warn!("Connection timeout - no activity for 90 seconds");
                        break;
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Create Bitget V2 subscription message (matches Python implementation)
    fn create_subscription_message(&self, symbol: &str, channel: &BitgetChannel) -> Result<String> {
        let message = serde_json::json!({
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": channel.as_str(),
                "instId": symbol
            }]
        });
        
        Ok(message.to_string())
    }
    
    /// Parse Bitget V2 WebSocket message
    fn parse_message(&self, text: &str) -> Result<Option<BitgetMessage>> {
        let json: Value = serde_json::from_str(text)?;
        
        // Handle event messages (subscription confirmations, etc.)
        if let Some(event) = json.get("event") {
            debug!("Received event: {}", event);
            return Ok(None);
        }
        
        // Handle data messages
        if let (Some(arg), Some(data)) = (json.get("arg"), json.get("data")) {
            let channel = arg.get("channel").and_then(|v| v.as_str()).unwrap_or("");
            let symbol = arg.get("instId").and_then(|v| v.as_str()).unwrap_or("");
            let action = json.get("action").and_then(|v| v.as_str());
            
            match channel {
                "books5" | "books1" | "books15" => {
                    return Ok(Some(BitgetMessage::OrderBook {
                        symbol: symbol.to_string(),
                        data: data.clone(),
                        action: action.map(|s| s.to_string()),
                        timestamp: now_micros(),
                    }));
                }
                "trade" => {
                    return Ok(Some(BitgetMessage::Trade {
                        symbol: symbol.to_string(),
                        data: data.clone(),
                        timestamp: now_micros(),
                    }));
                }
                "ticker" => {
                    return Ok(Some(BitgetMessage::Ticker {
                        symbol: symbol.to_string(),
                        data: data.clone(),
                        timestamp: now_micros(),
                    }));
                }
                _ => {
                    debug!("Unknown channel: {}", channel);
                }
            }
        }
        
        Ok(None)
    }
}

/// Bitget V2 message types
#[derive(Debug, Clone)]
pub enum BitgetMessage {
    OrderBook {
        symbol: String,
        data: Value,
        action: Option<String>, // "snapshot" or "update"
        timestamp: Timestamp,
    },
    Trade {
        symbol: String,
        data: Value,
        timestamp: Timestamp,
    },
    Ticker {
        symbol: String,
        data: Value,
        timestamp: Timestamp,
    },
}

impl BitgetMessage {
    /// Convert to OrderBookUpdate if this is an orderbook message
    pub fn to_orderbook_update(&self) -> Result<Option<OrderBookUpdate>> {
        match self {
            BitgetMessage::OrderBook { symbol, data, action, timestamp } => {
                if let Some(data_array) = data.as_array() {
                    if let Some(first_item) = data_array.first() {
                        return Ok(Some(parse_bitget_orderbook(symbol, first_item, action.as_deref(), *timestamp)?));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }
}

/// Parse Bitget V2 orderbook data into OrderBookUpdate
pub fn parse_bitget_orderbook(symbol: &str, data: &Value, action: Option<&str>, timestamp: Timestamp) -> Result<OrderBookUpdate> {
    let empty_vec = Vec::new();
    let bids_data = data.get("bids").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
    let asks_data = data.get("asks").and_then(|v| v.as_array()).unwrap_or(&empty_vec);
    
    let mut bids = Vec::new();
    for bid in bids_data {
        if let Some(bid_array) = bid.as_array() {
            if bid_array.len() >= 2 {
                let price = bid_array[0].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid bid price format"))?
                    .parse::<f64>()?;
                let quantity_str = bid_array[1].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid bid quantity format"))?;
                
                bids.push(PriceLevel {
                    price: price.to_price(),
                    quantity: quantity_str.to_quantity(),
                    side: Side::Bid,
                });
            }
        }
    }
    
    let mut asks = Vec::new();
    for ask in asks_data {
        if let Some(ask_array) = ask.as_array() {
            if ask_array.len() >= 2 {
                let price = ask_array[0].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid ask price format"))?
                    .parse::<f64>()?;
                let quantity_str = ask_array[1].as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid ask quantity format"))?;
                
                asks.push(PriceLevel {
                    price: price.to_price(),
                    quantity: quantity_str.to_quantity(),
                    side: Side::Ask,
                });
            }
        }
    }
    
    let is_snapshot = action == Some("snapshot");
    
    // Use timestamp from data if available, otherwise use provided timestamp
    let data_timestamp = data.get("ts")
        .and_then(|v| v.as_u64())
        .map(|ts| ts * 1000) // Convert ms to μs
        .unwrap_or(timestamp);
    
    Ok(OrderBookUpdate {
        symbol: symbol.to_string(),
        bids,
        asks,
        timestamp: data_timestamp,
        sequence_start: 0, // Bitget doesn't provide sequence numbers
        sequence_end: 0,
        is_snapshot,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    
    #[test]
    fn test_subscription_message() {
        let connector = BitgetConnector::new(BitgetConfig::default());
        let msg = connector.create_subscription_message("BTCUSDT", &BitgetChannel::Books5).unwrap();
        
        let expected = json!({
            "op": "subscribe",
            "args": [{
                "instType": "SPOT",
                "channel": "books5",
                "instId": "BTCUSDT"
            }]
        });
        
        assert_eq!(msg, expected.to_string());
    }
    
    #[test]
    fn test_orderbook_parsing() {
        let connector = BitgetConnector::new(BitgetConfig::default());
        
        let test_message = json!({
            "arg": {
                "instType": "SPOT",
                "channel": "books5",
                "instId": "BTCUSDT"
            },
            "action": "snapshot",
            "data": [{
                "bids": [
                    ["50000.00", "0.1"],
                    ["49999.00", "0.2"]
                ],
                "asks": [
                    ["50001.00", "0.15"],
                    ["50002.00", "0.25"]
                ],
                "ts": 1640000000000i64
            }]
        });
        
        let result = connector.parse_message(&test_message.to_string()).unwrap();
        assert!(result.is_some());
        
        if let Some(BitgetMessage::OrderBook { symbol, action, .. }) = result {
            assert_eq!(symbol, "BTCUSDT");
            assert_eq!(action, Some("snapshot".to_string()));
        } else {
            panic!("Expected OrderBook message");
        }
    }
}