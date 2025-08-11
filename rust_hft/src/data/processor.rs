/*!
 * Data Processor for Rust HFT System
 * 
 * Processes raw WebSocket messages into OrderBook updates
 * Extracts features for ML model consumption
 * Optimized for sub-microsecond processing latency
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::core::orderbook::OrderBook;
use crate::ml::features::FeatureExtractor;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use simd_json::prelude::*;
use tracing::{info, warn, error, debug};
use redis::{Client as RedisClient, Commands};
use serde_json;

/// Processor statistics
#[derive(Debug, Clone, Default)]
pub struct ProcessorStats {
    pub messages_processed: u64,
    pub parse_errors: u64,
    pub sequence_errors: u64,
    pub orderbook_updates: u64,
    pub features_generated: u64,
    pub avg_processing_latency_us: f64,
    pub last_sequence: u64,
    pub data_quality_score: f64,
}

/// Main processor thread entry point
pub fn run(
    config: Config,
    raw_msg_rx: Receiver<RawMessage>,
    features_tx: Sender<FeatureSet>,
) -> Result<()> {
    info!("⚙️ Processor thread starting...");
    
    let mut processor = MessageProcessor::new(config)?;
    
    info!("Processor initialized, waiting for messages...");
    
    // Main processing loop
    while let Ok(raw_msg) = raw_msg_rx.recv() {
        let processing_start = now_micros();
        
        match processor.process_message(raw_msg, processing_start) {
            Ok(Some(features)) => {
                // Send features to strategy thread
                if let Err(e) = features_tx.try_send(features) {
                    warn!("Failed to send features to strategy: {}", e);
                }
            },
            Ok(None) => {
                // Valid message but no features generated (e.g., subscription confirmation)
                debug!("Message processed but no features generated");
            },
            Err(e) => {
                error!("Message processing error: {}", e);
                processor.stats.parse_errors += 1;
            }
        }
    }
    
    info!("⚙️ Processor thread shutting down");
    Ok(())
}

/// Message processor with orderbook and feature extraction
struct MessageProcessor {
    /// OrderBook instance
    orderbook: OrderBook,
    
    /// Feature extractor
    feature_extractor: FeatureExtractor,
    
    /// Configuration
    config: Config,
    
    /// Processing statistics
    stats: ProcessorStats,
    
    /// Expected next sequence number
    next_expected_sequence: Option<u64>,
    
    /// Snapshot refresh flag
    needs_snapshot_refresh: bool,
    
    /// Redis client for publishing fast OrderBook data
    redis_client: Option<RedisClient>,
}

impl MessageProcessor {
    fn new(config: Config) -> Result<Self> {
        let symbol = config.symbol().to_string();
        let window_size = config.ml.feature_window_size;
        
        // Initialize Redis client for publishing fast data
        let redis_client = match RedisClient::open("redis://127.0.0.1:6379/") {
            Ok(client) => {
                info!("✅ Redis client connected for fast OrderBook publishing");
                Some(client)
            },
            Err(e) => {
                warn!("⚠️ Redis connection failed: {}, continuing without Redis publishing", e);
                None
            }
        };
        
        Ok(Self {
            orderbook: OrderBook::new(symbol),
            feature_extractor: FeatureExtractor::new(window_size),
            config,
            stats: ProcessorStats::default(),
            next_expected_sequence: None,
            needs_snapshot_refresh: true,
            redis_client,
        })
    }
    
    /// Process a single raw message
    fn process_message(
        &mut self,
        raw_msg: RawMessage,
        processing_start: Timestamp,
    ) -> Result<Option<FeatureSet>> {
        let start_time = now_micros();
        
        // Calculate network latency
        let network_latency = processing_start - raw_msg.received_at;
        
        // Parse JSON message
        let parsed_msg = self.parse_websocket_message(&raw_msg.data)?;
        
        // Handle different message types
        match parsed_msg {
            WebSocketMessage::OrderBookUpdate(update) => {
                // Process orderbook update
                self.process_orderbook_update(update)?;
                
                // Extract features if orderbook is valid
                if self.orderbook.is_valid {
                    let features = self.feature_extractor.extract_features(
                        &self.orderbook,
                        network_latency,
                        processing_start,
                    )?;
                    
                    self.stats.features_generated += 1;
                    
                    // Update processing latency statistics
                    let processing_latency = now_micros() - start_time;
                    self.update_latency_stats(processing_latency);
                    
                    debug!("Features extracted: latency={}μs, OBI_L10={:.3}", 
                           processing_latency, features.obi_l10);
                    
                    Ok(Some(features))
                } else {
                    warn!("OrderBook is invalid, skipping feature extraction");
                    Ok(None)
                }
            },
            WebSocketMessage::Subscription(resp) => {
                info!("Subscription response: {:?}", resp);
                Ok(None)
            },
            WebSocketMessage::Error(err) => {
                error!("WebSocket error message: {}", err);
                Err(anyhow::anyhow!("WebSocket error: {}", err))
            },
            WebSocketMessage::Unknown => {
                debug!("Unknown message type, ignoring");
                Ok(None)
            }
        }
    }
    
    /// Parse WebSocket message from Bitget
    fn parse_websocket_message(&self, data: &str) -> Result<WebSocketMessage> {
        let start_time = now_micros();
        
        // Use simd-json for high-performance parsing
        let mut data_bytes = data.as_bytes().to_vec();
        let parsed = simd_json::to_borrowed_value(&mut data_bytes)
            .map_err(|e| anyhow::anyhow!("JSON parse error: {}", e))?;
        
        let parse_latency = now_micros() - start_time;
        debug!("JSON parsing latency: {}μs", parse_latency);
        
        // Check for subscription response
        if let Some(event) = parsed.get("event") {
            if event == "subscribe" {
                return Ok(WebSocketMessage::Subscription(
                    SubscriptionResponse {
                        event: event.as_str().unwrap_or("").to_string(),
                        code: parsed.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string(),
                    }
                ));
            }
        }
        
        // Check for error messages
        if let Some(error) = parsed.get("error") {
            return Ok(WebSocketMessage::Error(
                error.as_str().unwrap_or("Unknown error").to_string()
            ));
        }
        
        // Parse orderbook data
        if let (Some(arg), Some(data)) = (parsed.get("arg"), parsed.get("data")) {
            let channel = arg.get("channel").and_then(|c| c.as_str());
            let action = parsed.get("action").and_then(|a| a.as_str());
            
            if channel == Some("books") {
                return self.parse_orderbook_data(data, action);
            }
        }
        
        Ok(WebSocketMessage::Unknown)
    }
    
    /// Parse orderbook data from WebSocket message
    fn parse_orderbook_data(
        &self,
        data: &simd_json::BorrowedValue,
        action: Option<&str>,
    ) -> Result<WebSocketMessage> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Expected data array"))?;
        
        if data_array.is_empty() {
            return Err(anyhow::anyhow!("Empty data array"));
        }
        
        let ob_data = &data_array[0];
        let timestamp = now_micros(); // Use current time if not provided
        
        // Parse bids
        let mut bids = Vec::new();
        if let Some(bids_array) = ob_data.get("bids").and_then(|b| b.as_array()) {
            for bid in bids_array {
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        let price_str = bid_array[0].as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid bid price"))?;
                        let qty_str = bid_array[1].as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid bid quantity"))?;
                        
                        let price: f64 = price_str.parse()?;
                        let quantity = qty_str.to_quantity();
                        
                        bids.push(PriceLevel {
                            price: price.to_price(),
                            quantity,
                            side: Side::Bid,
                        });
                    }
                }
            }
        }
        
        // Parse asks
        let mut asks = Vec::new();
        if let Some(asks_array) = ob_data.get("asks").and_then(|a| a.as_array()) {
            for ask in asks_array {
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        let price_str = ask_array[0].as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid ask price"))?;
                        let qty_str = ask_array[1].as_str()
                            .ok_or_else(|| anyhow::anyhow!("Invalid ask quantity"))?;
                        
                        let price: f64 = price_str.parse()?;
                        let quantity = qty_str.to_quantity();
                        
                        asks.push(PriceLevel {
                            price: price.to_price(),
                            quantity,
                            side: Side::Ask,
                        });
                    }
                }
            }
        }
        
        // Get sequence numbers (if available)
        let sequence_start = ob_data.get("seqId")
            .and_then(|s| s.as_u64())
            .unwrap_or(0);
        let sequence_end = sequence_start;
        
        // Determine if this is a snapshot
        let is_snapshot = action == Some("snapshot") || self.needs_snapshot_refresh;
        
        let update = OrderBookUpdate {
            symbol: self.config.symbol().to_string(),
            bids,
            asks,
            timestamp,
            sequence_start,
            sequence_end,
            is_snapshot,
        };
        
        Ok(WebSocketMessage::OrderBookUpdate(update))
    }
    
    /// Process orderbook update
    fn process_orderbook_update(&mut self, update: OrderBookUpdate) -> Result<()> {
        let start_time = now_micros();
        
        // Validate sequence if we have expectations
        if let Some(expected) = self.next_expected_sequence {
            if update.sequence_start != expected {
                warn!("Sequence gap: expected {}, got {}", expected, update.sequence_start);
                self.stats.sequence_errors += 1;
                self.needs_snapshot_refresh = true;
                
                // For now, process anyway but mark for refresh
                // In production, you might want to request a new snapshot
            }
        }
        
        // Store sequence info before moving update
        let sequence_end = update.sequence_end;
        
        // Apply update to orderbook
        if update.is_snapshot || self.needs_snapshot_refresh {
            self.orderbook.init_snapshot(update)?;
            self.needs_snapshot_refresh = false;
            info!("OrderBook initialized with snapshot");
        } else {
            self.orderbook.apply_update(update)?;
        }
        
        // Update sequence tracking
        self.next_expected_sequence = Some(sequence_end + 1);
        self.stats.last_sequence = sequence_end;
        self.stats.orderbook_updates += 1;
        self.stats.messages_processed += 1;
        
        let processing_latency = now_micros() - start_time;
        debug!("OrderBook update processed in {}μs", processing_latency);
        
        // 🚀 Publish fast OrderBook data to Redis for Python agents
        self.publish_orderbook_to_redis()?;
        
        Ok(())
    }
    
    /// Update latency statistics
    fn update_latency_stats(&mut self, latency_us: u64) {
        if self.stats.messages_processed == 0 {
            self.stats.avg_processing_latency_us = latency_us as f64;
        } else {
            // Exponential moving average
            let alpha = 0.1;
            self.stats.avg_processing_latency_us = 
                alpha * latency_us as f64 + (1.0 - alpha) * self.stats.avg_processing_latency_us;
        }
    }
    
    /// Get processor statistics
    #[allow(dead_code)]
    pub fn get_stats(&self) -> ProcessorStats {
        self.stats.clone()
    }
    
    /// Publish OrderBook data to Redis for Python agents
    /// P1 修復：統一使用 integrations/redis_bridge 的 market.* 模式
    fn publish_orderbook_to_redis(&mut self) -> Result<()> {
        if let Some(ref mut redis_client) = self.redis_client {
            if self.orderbook.is_valid {
                let mut con = redis_client.get_connection()
                    .map_err(|e| anyhow::anyhow!("Redis connection error: {}", e))?;
                
                // 統一使用 market.orderbook 域名模式，與 integrations/redis_bridge 一致
                let message = serde_json::json!({
                    "channel": "orderbook",
                    "data": {
                        "symbol": self.orderbook.symbol,
                        "mid_price": self.orderbook.mid_price().map(|p| p.0),
                        "best_bid": self.orderbook.best_bid().map(|p| p.0),
                        "best_ask": self.orderbook.best_ask().map(|p| p.0),
                        "spread": self.orderbook.spread(),
                        "spread_bps": self.orderbook.spread_bps(),
                        "bid_levels": self.orderbook.bids.len(),
                        "ask_levels": self.orderbook.asks.len(),
                        "last_sequence": self.orderbook.last_sequence,
                        "is_valid": self.orderbook.is_valid,
                        "data_quality_score": self.orderbook.data_quality_score,
                        "source": "rust_fast_processor"
                    },
                    "timestamp": now_micros()
                });
                
                // 統一使用 market.orderbook 通道，與 integrations/redis_bridge 一致
                let _: Result<(), redis::RedisError> = con.publish("market.orderbook", message.to_string());
                
                debug!("📡 Published OrderBook to unified market.orderbook channel: {}", self.orderbook.symbol);
            }
        }
        Ok(())
    }
}

/// WebSocket message types
#[derive(Debug)]
enum WebSocketMessage {
    OrderBookUpdate(OrderBookUpdate),
    Subscription(SubscriptionResponse),
    Error(String),
    Unknown,
}

/// Subscription response
#[derive(Debug)]
#[allow(dead_code)]
struct SubscriptionResponse {
    event: String,
    code: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    // use crossbeam_channel::bounded;
    
    #[test]
    fn test_message_processor_creation() {
        let config = Config::default();
        let processor = MessageProcessor::new(config);
        assert!(processor.is_ok());
    }
    
    #[test]
    fn test_orderbook_update_parsing() {
        let config = Config::default();
        let processor = MessageProcessor::new(config).unwrap();
        
        let json_data = r#"{
            "arg": {
                "instType": "SPOT",
                "channel": "books",
                "instId": "BTCUSDT"
            },
            "action": "snapshot",
            "data": [{
                "bids": [["100.0", "1.0"], ["99.0", "2.0"]],
                "asks": [["101.0", "1.5"], ["102.0", "2.5"]],
                "seqId": 123456
            }]
        }"#;
        
        let result = processor.parse_websocket_message(json_data);
        assert!(result.is_ok());
        
        if let Ok(WebSocketMessage::OrderBookUpdate(update)) = result {
            assert_eq!(update.bids.len(), 2);
            assert_eq!(update.asks.len(), 2);
            assert_eq!(update.sequence_start, 123456);
            assert!(update.is_snapshot);
        } else {
            panic!("Expected OrderBookUpdate");
        }
    }
    
    #[test]
    fn test_subscription_response_parsing() {
        let config = Config::default();
        let processor = MessageProcessor::new(config).unwrap();
        
        let json_data = r#"{
            "event": "subscribe",
            "code": "0"
        }"#;
        
        let result = processor.parse_websocket_message(json_data);
        assert!(result.is_ok());
        
        if let Ok(WebSocketMessage::Subscription(resp)) = result {
            assert_eq!(resp.event, "subscribe");
            assert_eq!(resp.code, "0");
        } else {
            panic!("Expected Subscription response");
        }
    }
}