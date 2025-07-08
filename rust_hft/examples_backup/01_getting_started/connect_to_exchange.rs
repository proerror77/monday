/*!
 * Demo: Bitget WebSocket V2 API Connection
 * 
 * This example demonstrates the correct usage of Bitget V2 WebSocket API
 * based on the working Python implementation provided by the user.
 */

use rust_hft::integrations::bitget_connector::*;
use tracing::{info, error};
use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging with explicit level
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🚀 Starting Bitget V2 WebSocket API Demo");
    
    // Create Bitget V2 configuration
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };
    
    info!("📡 Using Bitget V2 endpoints:");
    info!("   Public:  {}", config.public_ws_url);
    info!("   Private: {}", config.private_ws_url);
    
    // Create connector and add subscriptions
    let mut connector = BitgetConnector::new(config);
    
    // Add multiple symbol subscriptions (like in the Python code)
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);
    connector.add_subscription("ETHUSDT".to_string(), BitgetChannel::Books5);
    connector.add_subscription("ADAUSDT".to_string(), BitgetChannel::Books5);
    
    info!("📊 Subscriptions configured:");
    info!("   BTCUSDT -> books5 (5-level orderbook)");
    info!("   ETHUSDT -> books5 (5-level orderbook)");
    info!("   ADAUSDT -> books5 (5-level orderbook)");
    
    // Message statistics (using Arc<Mutex> for shared state)
    let message_count = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let orderbook_updates = std::sync::Arc::new(std::sync::Mutex::new(0u64));
    let start_time = std::sync::Arc::new(std::time::Instant::now());
    
    // Clone references for the closure
    let msg_count_clone = message_count.clone();
    let ob_updates_clone = orderbook_updates.clone();
    let start_time_clone = start_time.clone();
    
    // Create message handler
    let message_handler = move |message: BitgetMessage| {
        println!("📨 Received message!"); // Add debug print
        {
            let mut count = msg_count_clone.lock().unwrap();
            *count += 1;
        }
        
        match message {
            BitgetMessage::OrderBook { symbol, action, data, timestamp } => {
                let mut updates = ob_updates_clone.lock().unwrap();
                *updates += 1;
                let current_updates = *updates;
                
                // Parse orderbook data
                if let Some(data_array) = data.as_array() {
                    if let Some(first_item) = data_array.first() {
                        let bids = first_item.get("bids").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                        let asks = first_item.get("asks").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                        
                        info!("📈 {} OrderBook {} - {} bids, {} asks (timestamp: {})", 
                              symbol, 
                              action.as_deref().unwrap_or("update"),
                              bids, 
                              asks,
                              timestamp);
                        
                        // Show best bid/ask if available
                        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
                            if let Some(best_bid) = bid_data.first().and_then(|v| v.as_array()) {
                                if best_bid.len() >= 2 {
                                    let price = best_bid[0].as_str().unwrap_or("0");
                                    let qty = best_bid[1].as_str().unwrap_or("0");
                                    info!("   Best bid: {} @ {}", price, qty);
                                }
                            }
                        }
                        
                        if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
                            if let Some(best_ask) = ask_data.first().and_then(|v| v.as_array()) {
                                if best_ask.len() >= 2 {
                                    let price = best_ask[0].as_str().unwrap_or("0");
                                    let qty = best_ask[1].as_str().unwrap_or("0");
                                    info!("   Best ask: {} @ {}", price, qty);
                                }
                            }
                        }
                    }
                }
                
                // Show statistics every 10 updates
                if current_updates % 10 == 0 {
                    let elapsed = start_time_clone.elapsed();
                    let rate = current_updates as f64 / elapsed.as_secs_f64();
                    let msg_count = msg_count_clone.lock().unwrap();
                    info!("📊 Statistics: {} messages, {} orderbook updates, {:.1} updates/sec", 
                          *msg_count, current_updates, rate);
                }
            }
            BitgetMessage::Trade { symbol, data: _, timestamp } => {
                info!("💱 {} Trade - timestamp: {}", symbol, timestamp);
            }
            BitgetMessage::Ticker { symbol, data: _, timestamp } => {
                info!("📈 {} Ticker - timestamp: {}", symbol, timestamp);
            }
        }
    };
    
    info!("🔌 Connecting to Bitget V2 WebSocket...");
    
    // Add timeout to prevent hanging
    let timeout = tokio::time::Duration::from_secs(30);
    
    // Connect and start streaming with timeout
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ Bitget V2 connection completed successfully");
        }
        Ok(Err(e)) => {
            error!("❌ Bitget V2 connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            error!("⏰ Bitget V2 connection timed out after 30 seconds");
            return Err(anyhow::anyhow!("Connection timeout"));
        }
    }
    
    info!("🏁 Demo completed");
    Ok(())
}