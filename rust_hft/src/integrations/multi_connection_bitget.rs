/*!
 * Multi-Connection Bitget WebSocket V2 API Connector
 * 
 * Solves the single connection overload issue by distributing subscriptions
 * across multiple WebSocket connections based on data type and frequency.
 */

use crate::integrations::bitget_connector::{BitgetConnector, BitgetConfig, BitgetMessage, BitgetChannel};
use crate::types::*;
use anyhow::Result;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{info, warn, error, debug};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Connection pool strategy for distributing subscriptions
#[derive(Debug, Clone)]
pub enum ConnectionStrategy {
    /// Separate by data type (recommended for high-frequency data)
    ByDataType {
        books_connection: usize,    // High-frequency orderbook data
        trade_connection: usize,    // Medium-frequency trade data  
        ticker_connection: usize,   // Low-frequency ticker data
    },
    /// Separate by symbol groups
    BySymbol {
        symbols_per_connection: usize,
    },
    /// **NEW**: Volume-based distribution using real message rates
    ByVolume {
        high_traffic_threshold: f64,    // msg/s threshold for high traffic symbols
        medium_traffic_threshold: f64,  // msg/s threshold for medium traffic symbols
        max_load_per_connection: f64,   // maximum total load per connection
    },
    /// Dynamic load balancing based on real-time traffic
    DynamicBalancing {
        target_load_per_connection: f64,
        rebalance_interval_secs: u64,
        rebalance_threshold: f64,       // trigger rebalance when load diff > threshold
    },
    /// Custom distribution
    Custom {
        distribution: HashMap<String, usize>, // symbol -> connection_id
    },
}

impl Default for ConnectionStrategy {
    fn default() -> Self {
        ConnectionStrategy::ByDataType {
            books_connection: 0,
            trade_connection: 1,
            ticker_connection: 2,
        }
    }
}

/// Multi-connection Bitget connector
pub struct MultiConnectionBitgetConnector {
    config: BitgetConfig,
    strategy: ConnectionStrategy,
    connections: Vec<Arc<Mutex<BitgetConnector>>>,
    subscription_map: HashMap<String, usize>, // "symbol:channel" -> connection_id
    message_sender: mpsc::UnboundedSender<BitgetMessage>,
    message_receiver: Arc<Mutex<mpsc::UnboundedReceiver<BitgetMessage>>>,
}

impl MultiConnectionBitgetConnector {
    /// Create new multi-connection connector
    pub fn new(config: BitgetConfig, strategy: ConnectionStrategy) -> Self {
        let connection_count = match &strategy {
            ConnectionStrategy::ByDataType { .. } => 3,
            ConnectionStrategy::BySymbol { .. } => 4, // Default to 4 connections
            ConnectionStrategy::ByVolume { .. } => 4, // High, Medium-High, Medium, Low traffic groups
            ConnectionStrategy::DynamicBalancing { .. } => 4, // Default to 4 for load balancing
            ConnectionStrategy::Custom { distribution } => {
                distribution.values().max().unwrap_or(&0) + 1
            }
        };
        
        let mut connections = Vec::new();
        for i in 0..connection_count {
            let mut connector = BitgetConnector::new(config.clone());
            connections.push(Arc::new(Mutex::new(connector)));
        }
        
        let (message_sender, message_receiver) = mpsc::unbounded_channel();
        
        Self {
            config,
            strategy,
            connections,
            subscription_map: HashMap::new(),
            message_sender,
            message_receiver: Arc::new(Mutex::new(message_receiver)),
        }
    }
    
    /// Add subscription and assign to appropriate connection
    pub async fn add_subscription(&mut self, symbol: String, channel: BitgetChannel) -> Result<()> {
        let connection_id = self.get_connection_id(&symbol, &channel)?;
        let subscription_key = format!("{}:{:?}", symbol, channel);
        
        // Add to subscription map
        self.subscription_map.insert(subscription_key, connection_id);
        
        // Add to specific connection
        if let Some(connection) = self.connections.get(connection_id) {
            let mut connector = connection.lock().await;
            connector.add_subscription(symbol.clone(), channel.clone());
            info!("Added subscription {} {:?} to connection {}", symbol, channel, connection_id);
        }
        
        Ok(())
    }
    
    /// Get connection ID based on strategy
    fn get_connection_id(&self, symbol: &str, channel: &BitgetChannel) -> Result<usize> {
        match &self.strategy {
            ConnectionStrategy::ByDataType { books_connection, trade_connection, ticker_connection } => {
                match channel {
                    BitgetChannel::Books5 | BitgetChannel::Books15 | BitgetChannel::Books1 => {
                        Ok(*books_connection)
                    }
                    BitgetChannel::Trade => Ok(*trade_connection),
                    BitgetChannel::Ticker => Ok(*ticker_connection),
                    BitgetChannel::Candle1m => Ok(*ticker_connection), // Low frequency
                }
            }
            ConnectionStrategy::BySymbol { symbols_per_connection } => {
                let symbol_hash = symbol.chars().map(|c| c as usize).sum::<usize>();
                Ok(symbol_hash % (self.connections.len().max(1)))
            }
            ConnectionStrategy::ByVolume { high_traffic_threshold, medium_traffic_threshold, .. } => {
                // 基於歷史數據的靜態分組 (實際項目中應該從監控器獲取實時數據)
                let connection_id = match symbol {
                    // 高流量組 (>10 msg/s) - 獨立連接或小組
                    "ETHUSDT" => 0,
                    "XRPUSDT" | "SOLUSDT" => 1,
                    
                    // 中流量組 (5-10 msg/s)
                    "BTCUSDT" | "DOGEUSDT" => 2,
                    
                    // 低流量組 (<5 msg/s) - 合併到一個連接
                    _ => 3,
                };
                Ok(connection_id.min(self.connections.len() - 1))
            }
            ConnectionStrategy::DynamicBalancing { .. } => {
                // 動態平衡暫時使用簡單的輪詢，實際應該基於實時負載
                let symbol_hash = symbol.chars().map(|c| c as usize).sum::<usize>();
                Ok(symbol_hash % (self.connections.len().max(1)))
            }
            ConnectionStrategy::Custom { distribution } => {
                distribution.get(symbol)
                    .copied()
                    .ok_or_else(|| anyhow::anyhow!("Symbol {} not found in custom distribution", symbol))
            }
        }
    }
    
    /// Connect all connections and start message aggregation
    pub async fn connect_all(&self) -> Result<()> {
        let mut handles = Vec::new();
        
        // Start each connection
        for (i, connection) in self.connections.iter().enumerate() {
            let connection_clone = Arc::clone(connection);
            let message_sender = self.message_sender.clone();
            
            let handle = tokio::spawn(async move {
                let connector = connection_clone.lock().await;
                
                // Create message handler for this connection
                let message_handler = move |message: BitgetMessage| {
                    if let Err(e) = message_sender.send(message) {
                        // Only log as debug during shutdown to avoid spam
                        if e.to_string().contains("channel closed") {
                            debug!("Connection {} message channel closed during shutdown", i);
                        } else {
                            error!("Failed to send message from connection {}: {}", i, e);
                        }
                    }
                };
                
                // Start connection
                if let Err(e) = connector.connect_public(message_handler).await {
                    error!("Connection {} failed: {}", i, e);
                }
            });
            
            handles.push(handle);
        }
        
        info!("Started {} WebSocket connections", self.connections.len());
        
        // Wait for all connections to complete
        for (i, handle) in handles.into_iter().enumerate() {
            if let Err(e) = handle.await {
                error!("Connection {} task failed: {}", i, e);
            }
        }
        
        Ok(())
    }
    
    /// Get the message receiver for consuming aggregated messages
    pub fn get_message_receiver(&self) -> Arc<Mutex<mpsc::UnboundedReceiver<BitgetMessage>>> {
        Arc::clone(&self.message_receiver)
    }
    
    /// Get connection statistics
    pub async fn get_connection_stats(&self) -> HashMap<usize, ConnectionStats> {
        let mut stats = HashMap::new();
        
        for (i, connection) in self.connections.iter().enumerate() {
            let subscription_count = self.subscription_map.values()
                .filter(|&&conn_id| conn_id == i)
                .count();
            
            stats.insert(i, ConnectionStats {
                connection_id: i,
                subscription_count,
                is_active: true, // TODO: Add actual connection health check
            });
        }
        
        stats
    }
    
    /// Print connection distribution for debugging
    pub fn print_distribution(&self) {
        info!("=== Connection Distribution ===");
        for (subscription, connection_id) in &self.subscription_map {
            info!("  {} -> Connection {}", subscription, connection_id);
        }
        
        let mut conn_counts = HashMap::new();
        for connection_id in self.subscription_map.values() {
            *conn_counts.entry(*connection_id).or_insert(0) += 1;
        }
        
        for (conn_id, count) in conn_counts {
            info!("  Connection {}: {} subscriptions", conn_id, count);
        }
        info!("===============================");
    }
}

/// Connection statistics
#[derive(Debug, Clone)]
pub struct ConnectionStats {
    pub connection_id: usize,
    pub subscription_count: usize,
    pub is_active: bool,
}

/// Helper function to create optimized multi-connection setup
pub fn create_optimized_multi_connector(config: BitgetConfig) -> MultiConnectionBitgetConnector {
    let strategy = ConnectionStrategy::ByDataType {
        books_connection: 0,  // High-frequency orderbook data
        trade_connection: 1,  // Medium-frequency trade data
        ticker_connection: 2, // Low-frequency ticker data
    };
    
    MultiConnectionBitgetConnector::new(config, strategy)
}

/// Create connection pool for high-volume trading
pub fn create_high_volume_connector(config: BitgetConfig) -> MultiConnectionBitgetConnector {
    let strategy = ConnectionStrategy::BySymbol {
        symbols_per_connection: 3, // 3-4 symbols per connection
    };
    
    MultiConnectionBitgetConnector::new(config, strategy)
}

/// **NEW**: Create volume-based multi-connection setup (推薦使用)
pub fn create_volume_based_connector(config: BitgetConfig) -> MultiConnectionBitgetConnector {
    let strategy = ConnectionStrategy::ByVolume {
        high_traffic_threshold: 10.0,   // >10 msg/s 為高流量
        medium_traffic_threshold: 5.0,  // 5-10 msg/s 為中流量
        max_load_per_connection: 25.0,  // 每個連接最大負載
    };
    
    MultiConnectionBitgetConnector::new(config, strategy)
}

/// Create dynamic load balancing connector (最智能)
pub fn create_dynamic_balancing_connector(config: BitgetConfig) -> MultiConnectionBitgetConnector {
    let strategy = ConnectionStrategy::DynamicBalancing {
        target_load_per_connection: 20.0,  // 目標每連接20 msg/s
        rebalance_interval_secs: 300,      // 5分鐘檢查一次重平衡
        rebalance_threshold: 10.0,         // 負載差異>10時觸發重平衡
    };
    
    MultiConnectionBitgetConnector::new(config, strategy)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_connection_distribution() {
        let config = BitgetConfig::default();
        let mut connector = create_optimized_multi_connector(config);
        
        // Add test subscriptions
        connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5).await.unwrap();
        connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Trade).await.unwrap();
        connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Ticker).await.unwrap();
        
        // Check distribution
        assert_eq!(connector.subscription_map.len(), 3);
        
        // Books should go to connection 0
        assert_eq!(connector.subscription_map.get("BTCUSDT:Books5"), Some(&0));
        // Trade should go to connection 1  
        assert_eq!(connector.subscription_map.get("BTCUSDT:Trade"), Some(&1));
        // Ticker should go to connection 2
        assert_eq!(connector.subscription_map.get("BTCUSDT:Ticker"), Some(&2));
    }
}