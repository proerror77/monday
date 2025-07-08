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
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Reconnection strategy for different data types
#[derive(Debug, Clone)]
pub enum ReconnectStrategy {
    /// Fast reconnect for ticker data (100ms-2s)
    Fast,
    /// Standard reconnect for orderbook data (1s-5s)
    Standard,
    /// Conservative reconnect for low-frequency data (5s-120s)
    Conservative,
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

/// Connection health statistics
#[derive(Debug, Default)]
pub struct ConnectionStats {
    pub total_messages: AtomicU64,
    pub connection_attempts: AtomicU64,
    pub successful_connections: AtomicU64,
    pub disconnections: AtomicU64,
    pub reconnections: AtomicU64,
    pub last_connection_time: AtomicU64,
    pub last_message_time: AtomicU64,
}

impl Clone for ConnectionStats {
    fn clone(&self) -> Self {
        Self {
            total_messages: AtomicU64::new(self.total_messages.load(Ordering::Relaxed)),
            connection_attempts: AtomicU64::new(self.connection_attempts.load(Ordering::Relaxed)),
            successful_connections: AtomicU64::new(self.successful_connections.load(Ordering::Relaxed)),
            disconnections: AtomicU64::new(self.disconnections.load(Ordering::Relaxed)),
            reconnections: AtomicU64::new(self.reconnections.load(Ordering::Relaxed)),
            last_connection_time: AtomicU64::new(self.last_connection_time.load(Ordering::Relaxed)),
            last_message_time: AtomicU64::new(self.last_message_time.load(Ordering::Relaxed)),
        }
    }
}

impl ConnectionStats {
    pub fn record_connection_attempt(&self) {
        self.connection_attempts.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_successful_connection(&self) {
        self.successful_connections.fetch_add(1, Ordering::Relaxed);
        self.last_connection_time.store(now_micros(), Ordering::Relaxed);
    }
    
    pub fn record_disconnection(&self) {
        self.disconnections.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_reconnection(&self) {
        self.reconnections.fetch_add(1, Ordering::Relaxed);
    }
    
    pub fn record_message(&self) {
        self.total_messages.fetch_add(1, Ordering::Relaxed);
        self.last_message_time.store(now_micros(), Ordering::Relaxed);
    }
    
    pub fn get_connection_rate(&self) -> f64 {
        let attempts = self.connection_attempts.load(Ordering::Relaxed);
        let successful = self.successful_connections.load(Ordering::Relaxed);
        if attempts > 0 {
            successful as f64 / attempts as f64
        } else {
            0.0
        }
    }
}

/// Bitget V2 WebSocket connector with robust connection management
#[derive(Debug, Clone)]
pub struct BitgetConnector {
    config: BitgetConfig,
    subscriptions: HashMap<String, Vec<BitgetChannel>>,
    stats: Arc<ConnectionStats>,
    is_running: Arc<AtomicBool>,
    consecutive_failures: Arc<AtomicU64>,
}

impl BitgetConnector {
    pub fn new(config: BitgetConfig) -> Self {
        Self {
            config,
            subscriptions: HashMap::new(),
            stats: Arc::new(ConnectionStats::default()),
            is_running: Arc::new(AtomicBool::new(false)),
            consecutive_failures: Arc::new(AtomicU64::new(0)),
        }
    }
    
    /// Start WebSocket connection with robust error handling
    pub async fn run_websocket(&self, message_sender: mpsc::UnboundedSender<BitgetMessage>) -> Result<()> {
        self.is_running.store(true, Ordering::Relaxed);
        
        while self.is_running.load(Ordering::Relaxed) {
            let result = self.connect_and_run(&message_sender).await;
            
            match result {
                Ok(()) => {
                    info!("WebSocket connection closed normally");
                    self.consecutive_failures.store(0, Ordering::Relaxed);
                    break;
                }
                Err(e) => {
                    let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                    self.stats.record_disconnection();
                    
                    error!("WebSocket connection failed (attempt {}): {}", failures, e);
                    
                    if !self.config.auto_reconnect {
                        error!("Auto-reconnect disabled, stopping");
                        break;
                    }
                    
                    if failures >= self.config.max_reconnect_attempts as u64 {
                        error!("Max reconnection attempts reached ({}), stopping", self.config.max_reconnect_attempts);
                        break;
                    }
                    
                    let delay = self.calculate_adaptive_reconnect_delay(failures as usize);
                    warn!("Reconnecting in {} ms (attempt {})", delay, failures);
                    
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    self.stats.record_reconnection();
                }
            }
        }
        
        self.is_running.store(false, Ordering::Relaxed);
        Ok(())
    }
    
    /// Adaptive reconnection delay calculation
    fn calculate_adaptive_reconnect_delay(&self, consecutive_failures: usize) -> u64 {
        match &self.config.reconnect_strategy {
            ReconnectStrategy::Fast => {
                std::cmp::min(100 * (1 << consecutive_failures.min(4)), 2000)
            }
            ReconnectStrategy::Standard => {
                std::cmp::min(1000 * (1 << consecutive_failures.min(3)), 8000)
            }
            ReconnectStrategy::Conservative => {
                std::cmp::min(5000 * (1 << consecutive_failures.min(2)), 30000)
            }
            ReconnectStrategy::Custom { initial_delay_ms, max_delay_ms, multiplier } => {
                let delay = (*initial_delay_ms as f64 * multiplier.powi(consecutive_failures as i32)) as u64;
                std::cmp::min(delay, *max_delay_ms)
            }
        }
    }
    
    /// Main WebSocket connection logic with enhanced error handling
    async fn connect_and_run(&self, message_sender: &mpsc::UnboundedSender<BitgetMessage>) -> Result<()> {
        self.stats.record_connection_attempt();
        
        info!("Connecting to Bitget V2 WebSocket: {}", self.config.public_ws_url);
        
        // Create WebSocket connection with timeout
        let connect_future = connect_async(&self.config.public_ws_url);
        let (ws_stream, _) = tokio::time::timeout(
            Duration::from_secs(self.config.timeout_seconds),
            connect_future
        ).await
            .map_err(|_| anyhow::anyhow!("Connection timeout after {} seconds", self.config.timeout_seconds))?
            .map_err(|e| anyhow::anyhow!("WebSocket connection failed: {}", e))?;
            
        info!("Bitget V2 WebSocket connected successfully");
        self.stats.record_successful_connection();
        self.consecutive_failures.store(0, Ordering::Relaxed);
        
        let (mut ws_sender, mut ws_receiver) = ws_stream.split();
        
        // Send all subscriptions
        for (symbol, channels) in &self.subscriptions {
            for channel in channels {
                let subscription_msg = self.create_subscription_message(symbol, channel)?;
                
                ws_sender.send(Message::Text(subscription_msg)).await
                    .map_err(|e| anyhow::anyhow!("Failed to send subscription: {}", e))?;
                    
                info!("Subscribed to {} {} on Bitget V2", channel.as_str(), symbol);
                
                // Small delay between subscriptions to avoid rate limiting
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        
        // Enhanced message processing loop with keepalive
        let mut last_message_time = Instant::now();
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        
        loop {
            tokio::select! {
                // Process incoming messages
                message = ws_receiver.next() => {
                    match message {
                        Some(Ok(msg)) => {
                            last_message_time = Instant::now();
                            self.stats.record_message();
                            
                            match msg {
                                Message::Text(text) => {
                                    if let Ok(Some(parsed_msg)) = self.parse_message(&text) {
                                        if let Err(e) = message_sender.send(parsed_msg) {
                                            error!("Failed to send parsed message: {}", e);
                                            break;
                                        }
                                    }
                                }
                                Message::Ping(payload) => {
                                    debug!("Received ping, sending pong");
                                    if let Err(e) = ws_sender.send(Message::Pong(payload)).await {
                                        error!("Failed to send pong: {}", e);
                                        break;
                                    }
                                }
                                Message::Pong(_) => {
                                    debug!("Received pong");
                                }
                                Message::Close(frame) => {
                                    info!("WebSocket closed by server: {:?}", frame);
                                    break;
                                }
                                _ => {}
                            }
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            break;
                        }
                        None => {
                            warn!("WebSocket stream ended");
                            break;
                        }
                    }
                }
                
                // Send periodic pings to keep connection alive
                _ = ping_interval.tick() => {
                    if let Err(e) = ws_sender.send(Message::Ping(vec![])).await {
                        error!("Failed to send ping: {}", e);
                        break;
                    }
                    debug!("Sent keepalive ping");
                }
                
                // Check for message timeout (connection might be stale)
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    if last_message_time.elapsed() > Duration::from_secs(120) {
                        warn!("No messages received for 2 minutes, connection might be stale");
                        break;
                    }
                }
                
                // Check if we should stop
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if !self.is_running.load(Ordering::Relaxed) {
                        info!("Stopping WebSocket connection");
                        break;
                    }
                }
            }
        }
        
        Ok(())
    }
    
    /// Stop the WebSocket connection
    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }
    
    /// Get connection statistics
    pub fn get_stats(&self) -> ConnectionStats {
        (*self.stats).clone()
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