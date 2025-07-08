/*!
 * High-Performance Network Layer for Rust HFT
 * 
 * Handles WebSocket connections to Bitget exchange
 * Optimized for minimal latency and maximum reliability
 */

use crate::core::types::*;
use crate::core::config::Config;
use anyhow::Result;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn, error, debug};
use url::Url;
use serde_json;

/// Network layer configuration
#[allow(dead_code)]
struct NetworkConfig {
    pub websocket_url: String,
    pub symbol: String,
    pub reconnect_interval: Duration,
    pub message_timeout: Duration,
    pub max_reconnect_attempts: u32,
}

impl From<&Config> for NetworkConfig {
    fn from(config: &Config) -> Self {
        Self {
            websocket_url: config.websocket_url().to_string(),
            symbol: config.symbol().to_string(),
            reconnect_interval: Duration::from_secs(5),
            message_timeout: Duration::from_secs(30),
            max_reconnect_attempts: 10,
        }
    }
}

/// Network statistics
#[derive(Debug, Clone, Default)]
pub struct NetworkStats {
    pub connections_established: u64,
    pub messages_received: u64,
    pub messages_failed: u64,
    pub reconnect_attempts: u64,
    pub last_message_time: Timestamp,
    pub avg_message_size: f64,
    pub connection_uptime: u64,
}

/// Main network thread entry point
pub fn run(config: Config, raw_msg_tx: Sender<RawMessage>) -> Result<()> {
    // Create async runtime for this thread
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async_run(config, raw_msg_tx))
}

async fn async_run(config: Config, raw_msg_tx: Sender<RawMessage>) -> Result<()> {
    info!("🌐 Network thread starting...");
    
    let net_config = NetworkConfig::from(&config);
    let mut stats = NetworkStats::default();
    let mut reconnect_count = 0u32;
    
    loop {
        match connect_and_stream(&net_config, &raw_msg_tx, &mut stats).await {
            Ok(_) => {
                info!("WebSocket connection closed normally");
                break;
            },
            Err(e) => {
                error!("WebSocket connection error: {}", e);
                reconnect_count += 1;
                stats.reconnect_attempts += 1;
                
                if reconnect_count >= net_config.max_reconnect_attempts {
                    error!("Max reconnection attempts ({}) exceeded", net_config.max_reconnect_attempts);
                    return Err(anyhow::anyhow!("Failed to establish stable connection"));
                }
                
                warn!("Reconnecting in {}s (attempt {}/{})", 
                      net_config.reconnect_interval.as_secs(),
                      reconnect_count, 
                      net_config.max_reconnect_attempts);
                      
                sleep(net_config.reconnect_interval).await;
            }
        }
    }
    
    info!("🌐 Network thread shutting down");
    Ok(())
}

/// Establish WebSocket connection and stream data
async fn connect_and_stream(
    config: &NetworkConfig,
    raw_msg_tx: &Sender<RawMessage>,
    stats: &mut NetworkStats,
) -> Result<()> {
    info!("Connecting to WebSocket: {}", config.websocket_url);
    
    // Parse WebSocket URL
    let url = Url::parse(&config.websocket_url)?;
    
    // Establish connection with timeout
    let connect_future = connect_async(url);
    let (ws_stream, response) = timeout(Duration::from_secs(10), connect_future)
        .await
        .map_err(|_| anyhow::anyhow!("Connection timeout"))?
        .map_err(|e| anyhow::anyhow!("Connection failed: {}", e))?;
    
    info!("WebSocket connected successfully. Response: {}", response.status());
    stats.connections_established += 1;
    
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    
    // TCP_NODELAY configuration is handled by tokio-tungstenite automatically
    debug!("WebSocket connection established with optimized TCP settings");
    
    // Subscribe to orderbook updates
    let subscribe_msg = create_subscribe_message(&config.symbol)?;
    ws_sender.send(subscribe_msg).await?;
    info!("Subscribed to {} orderbook updates", config.symbol);
    
    let connection_start = now_micros();
    
    // Message processing loop
    while let Some(msg_result) = ws_receiver.next().await {
        let receive_time = now_micros();
        
        match msg_result {
            Ok(Message::Text(text)) => {
                debug!("Received text message: {} bytes", text.len());
                
                // Update statistics
                stats.messages_received += 1;
                stats.last_message_time = receive_time;
                stats.avg_message_size = update_moving_average(
                    stats.avg_message_size,
                    text.len() as f64,
                    stats.messages_received,
                );
                stats.connection_uptime = receive_time - connection_start;
                
                // Create raw message
                let raw_msg = RawMessage {
                    data: text,
                    received_at: receive_time,
                    sequence: None, // Will be parsed by processor
                };
                
                // Send to processor thread (non-blocking)
                if let Err(e) = raw_msg_tx.try_send(raw_msg) {
                    warn!("Failed to send message to processor: {}", e);
                    stats.messages_failed += 1;
                }
            },
            Ok(Message::Binary(data)) => {
                debug!("Received binary message: {} bytes", data.len());
                // Handle binary messages if needed
            },
            Ok(Message::Ping(data)) => {
                debug!("Received ping, sending pong");
                if let Err(e) = ws_sender.send(Message::Pong(data)).await {
                    warn!("Failed to send pong: {}", e);
                }
            },
            Ok(Message::Pong(_)) => {
                debug!("Received pong");
            },
            Ok(Message::Close(frame)) => {
                info!("WebSocket close frame received: {:?}", frame);
                break;
            },
            Ok(Message::Frame(_)) => {
                debug!("Received raw frame, ignoring");
            },
            Err(e) => {
                error!("WebSocket message error: {}", e);
                stats.messages_failed += 1;
                return Err(anyhow::anyhow!("Message receive error: {}", e));
            }
        }
    }
    
    Ok(())
}

/// Create subscription message for Bitget WebSocket
fn create_subscribe_message(symbol: &str) -> Result<Message> {
    let subscribe_request = serde_json::json!({
        "op": "subscribe",
        "args": [{
            "instType": "SPOT",
            "channel": "books",
            "instId": symbol
        }]
    });
    
    let message_text = serde_json::to_string(&subscribe_request)?;
    debug!("Subscribe message: {}", message_text);
    
    Ok(Message::Text(message_text))
}

/// Update moving average efficiently
fn update_moving_average(current_avg: f64, new_value: f64, count: u64) -> f64 {
    if count == 0 {
        new_value
    } else {
        current_avg + (new_value - current_avg) / count as f64
    }
}

/// Network health checker
pub struct NetworkHealthChecker {
    last_message_time: Timestamp,
    timeout_threshold: Duration,
    stats: NetworkStats,
}

impl NetworkHealthChecker {
    pub fn new(timeout_threshold: Duration) -> Self {
        Self {
            last_message_time: now_micros(),
            timeout_threshold,
            stats: NetworkStats::default(),
        }
    }
    
    pub fn update_last_message(&mut self, timestamp: Timestamp) {
        self.last_message_time = timestamp;
    }
    
    pub fn is_healthy(&self) -> bool {
        let now = now_micros();
        let elapsed = Duration::from_micros(now - self.last_message_time);
        elapsed < self.timeout_threshold
    }
    
    pub fn get_health_status(&self) -> ComponentHealth {
        let now = now_micros();
        let elapsed_us = now - self.last_message_time;
        let is_healthy = elapsed_us < self.timeout_threshold.as_micros() as u64;
        
        ComponentHealth {
            component: "Network".to_string(),
            status: if is_healthy { 
                HealthStatus::Healthy 
            } else { 
                HealthStatus::Critical 
            },
            last_update: self.last_message_time,
            error_count: self.stats.messages_failed,
            uptime_seconds: self.stats.connection_uptime / 1_000_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_subscribe_message_creation() {
        let msg = create_subscribe_message("BTCUSDT").unwrap();
        
        if let Message::Text(text) = msg {
            let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(parsed["op"], "subscribe");
            assert_eq!(parsed["args"][0]["instId"], "BTCUSDT");
            assert_eq!(parsed["args"][0]["channel"], "books");
        } else {
            panic!("Expected text message");
        }
    }
    
    #[test]
    fn test_moving_average() {
        let avg1 = update_moving_average(0.0, 10.0, 1);
        assert_eq!(avg1, 10.0);
        
        let avg2 = update_moving_average(avg1, 20.0, 2);
        assert_eq!(avg2, 15.0);
        
        let avg3 = update_moving_average(avg2, 30.0, 3);
        assert_eq!(avg3, 20.0);
    }
    
    #[tokio::test]
    async fn test_network_config_creation() {
        let config = Config::default();
        let net_config = NetworkConfig::from(&config);
        
        assert!(!net_config.websocket_url.is_empty());
        assert_eq!(net_config.symbol, "BTCUSDT");
        assert_eq!(net_config.max_reconnect_attempts, 10);
    }
    
    #[test]
    fn test_network_health_checker() {
        let mut checker = NetworkHealthChecker::new(Duration::from_secs(1));
        
        // Initially healthy (just created)
        assert!(checker.is_healthy());
        
        // Update with current time
        checker.update_last_message(now_micros());
        assert!(checker.is_healthy());
        
        // Simulate old message
        let old_time = now_micros() - Duration::from_secs(2).as_micros() as u64;
        checker.update_last_message(old_time);
        assert!(!checker.is_healthy());
        
        let health = checker.get_health_status();
        assert_eq!(health.status, HealthStatus::Critical);
    }
}