/*!
 * Order Execution Engine for Rust HFT
 * 
 * Handles actual order placement and management via Bitget API
 * Optimized for minimal latency and maximum reliability
 */

use crate::core::types::*;
use crate::core::config::Config;
use anyhow::Result;
use crossbeam_channel::Receiver;
use reqwest::Client;
use std::collections::HashMap;
use tracing::{info, error, debug};
use rust_decimal::prelude::ToPrimitive;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use base64::{Engine as _, engine::general_purpose};
use serde_json;

type HmacSha256 = Hmac<Sha256>;

/// Execution statistics
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    pub signals_received: u64,
    pub orders_placed: u64,
    pub orders_filled: u64,
    pub orders_cancelled: u64,
    pub orders_rejected: u64,
    pub avg_execution_latency_us: f64,
    pub total_volume: f64,
    pub total_fees: f64,
    pub api_errors: u64,
}

/// Main execution thread entry point
pub fn run(
    config: Config,
    signal_rx: Receiver<TradingSignal>,
) -> Result<()> {
    info!("🎯 Execution thread starting...");
    
    let mut executor = OrderExecutor::new(config)?;
    
    info!("Execution engine initialized, waiting for signals...");
    
    // Main execution loop
    while let Ok(signal) = signal_rx.recv() {
        let execution_start = now_micros();
        
        match executor.execute_signal(signal, execution_start) {
            Ok(_) => {
                debug!("Signal executed successfully");
            },
            Err(e) => {
                error!("Signal execution error: {}", e);
                executor.stats.api_errors += 1;
            }
        }
    }
    
    info!("🎯 Execution thread shutting down");
    Ok(())
}

/// Order execution engine
struct OrderExecutor {
    /// HTTP client for API calls
    client: Client,
    
    /// Configuration
    config: Config,
    
    /// Execution statistics
    stats: ExecutionStats,
    
    /// Active orders tracking
    active_orders: HashMap<String, Order>,
    
    /// Order ID counter
    order_id_counter: u64,
}

impl OrderExecutor {
    fn new(config: Config) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        
        Ok(Self {
            client,
            config,
            stats: ExecutionStats::default(),
            active_orders: HashMap::new(),
            order_id_counter: 0,
        })
    }
    
    /// Execute trading signal
    fn execute_signal(
        &mut self,
        signal: TradingSignal,
        _execution_start: Timestamp,
    ) -> Result<()> {
        let start_time = now_micros();
        self.stats.signals_received += 1;
        
        // Check if execution is enabled
        if !self.config.execution_enabled() {
            info!("🔄 DRY RUN - Signal: {:?} @ {:.2} (qty: {})", 
                  signal.signal_type, signal.suggested_price, signal.suggested_quantity);
            return Ok(());
        }
        
        // Generate unique order ID
        self.order_id_counter += 1;
        let order_id = format!("rust_hft_{}", self.order_id_counter);
        
        // Create order request
        let order = self.create_order_from_signal(signal, order_id.clone())?;
        
        // Place order via API (using blocking runtime for now)
        let rt = tokio::runtime::Runtime::new()?;
        match rt.block_on(self.place_order(order.clone())) {
            Ok(response) => {
                info!("✅ Order placed: {:?} {} {} @ {}", 
                      order.side, order.quantity, self.config.symbol(), order.price.unwrap_or(0.0.to_price()));
                
                // Store active order
                self.active_orders.insert(order_id, order);
                self.stats.orders_placed += 1;
                
                // Log order details
                debug!("Order response: {:?}", response);
            },
            Err(e) => {
                error!("❌ Order placement failed: {}", e);
                self.stats.orders_rejected += 1;
                return Err(e);
            }
        }
        
        let execution_latency = now_micros() - start_time;
        self.update_execution_stats(execution_latency);
        
        debug!("Order execution latency: {}μs", execution_latency);
        
        Ok(())
    }
    
    /// Create order from trading signal
    fn create_order_from_signal(
        &self,
        signal: TradingSignal,
        order_id: String,
    ) -> Result<Order> {
        let side = match signal.signal_type {
            SignalType::Buy => Side::Bid,
            SignalType::Sell => Side::Ask,
            SignalType::Hold => return Err(anyhow::anyhow!("Cannot create order for Hold signal")),
        };
        
        let order = Order {
            order_id,
            symbol: self.config.symbol().to_string(),
            side,
            order_type: OrderType::Limit,
            quantity: signal.suggested_quantity,
            price: Some(signal.suggested_price),
            time_in_force: TimeInForce::GTC,
            status: OrderStatus::New,
            created_at: now_micros(),
            updated_at: now_micros(),
        };
        
        Ok(order)
    }
    
    /// Place order via Bitget API
    async fn place_order(&self, order: Order) -> Result<OrderResponse> {
        let url = format!("{}/api/v2/spot/trade/place-order", self.config.exchange.rest_api_url);
        let timestamp = chrono::Utc::now().timestamp_millis().to_string();
        
        // Prepare order parameters
        let mut params = serde_json::Map::new();
        params.insert("symbol".to_string(), serde_json::Value::String(order.symbol.clone()));
        params.insert("side".to_string(), serde_json::Value::String(
            match order.side {
                Side::Bid => "buy",
                Side::Ask => "sell",
            }.to_string()
        ));
        params.insert("orderType".to_string(), serde_json::Value::String("limit".to_string()));
        params.insert("force".to_string(), serde_json::Value::String("gtc".to_string()));
        
        if let Some(price) = order.price {
            params.insert("price".to_string(), serde_json::Value::String(
                format!("{:.2}", price.0)
            ));
        }
        
        params.insert("size".to_string(), serde_json::Value::String(
            format!("{:.6}", order.quantity.to_f64().unwrap_or(0.0))
        ));
        
        let body = serde_json::Value::Object(params);
        let body_str = serde_json::to_string(&body)?;
        
        // Generate signature
        let signature = self.generate_signature(&timestamp, "POST", "/api/v2/spot/trade/place-order", &body_str)?;
        
        // Prepare headers
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("ACCESS-KEY", self.config.exchange.api_key.parse()?);
        headers.insert("ACCESS-SIGN", signature.parse()?);
        headers.insert("ACCESS-TIMESTAMP", timestamp.parse()?);
        headers.insert("ACCESS-PASSPHRASE", self.config.exchange.passphrase.parse()?);
        headers.insert("Content-Type", "application/json".parse()?);
        
        // Make API request
        let response = self.client
            .post(&url)
            .headers(headers)
            .body(body_str)
            .send()
            .await?;
        
        let status = response.status();
        let response_text = response.text().await?;
        
        if status.is_success() {
            let api_response: ApiResponse<OrderData> = serde_json::from_str(&response_text)?;
            
            if api_response.code == "00000" {
                Ok(OrderResponse {
                    order_id: api_response.data.order_id,
                    status: "placed".to_string(),
                    message: "Order placed successfully".to_string(),
                })
            } else {
                Err(anyhow::anyhow!("API error: {} - {}", api_response.code, api_response.msg))
            }
        } else {
            Err(anyhow::anyhow!("HTTP error {}: {}", status, response_text))
        }
    }
    
    /// Generate HMAC-SHA256 signature for Bitget API
    fn generate_signature(
        &self,
        timestamp: &str,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<String> {
        let sign_str = format!("{}{}{}{}", timestamp, method.to_uppercase(), path, body);
        
        let mut mac = HmacSha256::new_from_slice(self.config.exchange.api_secret.as_bytes())
            .map_err(|e| anyhow::anyhow!("Invalid secret key: {}", e))?;
        
        mac.update(sign_str.as_bytes());
        let signature = mac.finalize().into_bytes();
        
        Ok(general_purpose::STANDARD.encode(signature))
    }
    
    /// Update execution statistics
    fn update_execution_stats(&mut self, latency_us: u64) {
        if self.stats.orders_placed == 0 {
            self.stats.avg_execution_latency_us = latency_us as f64;
        } else {
            let alpha = 0.1;
            self.stats.avg_execution_latency_us = 
                alpha * latency_us as f64 + (1.0 - alpha) * self.stats.avg_execution_latency_us;
        }
    }
    
    /// Get execution statistics
    #[allow(dead_code)]
    pub fn get_stats(&self) -> ExecutionStats {
        self.stats.clone()
    }
}

/// API response structure
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct ApiResponse<T> {
    code: String,
    msg: String,
    #[serde(rename = "requestTime")]
    request_time: Option<u64>,
    data: T,
}

/// Order data from API response
#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct OrderData {
    #[serde(rename = "orderId")]
    order_id: String,
    #[serde(rename = "clientOid")]
    client_oid: Option<String>,
}

/// Order response
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct OrderResponse {
    order_id: String,
    status: String,
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_executor_creation() {
        let config = Config::default();
        let executor = OrderExecutor::new(config);
        assert!(executor.is_ok());
    }
    
    #[test]
    fn test_order_creation_from_signal() {
        let config = Config::default();
        let executor = OrderExecutor::new(config).unwrap();
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let order = executor.create_order_from_signal(signal, "test_order".to_string());
        assert!(order.is_ok());
        
        let order = order.unwrap();
        assert_eq!(order.side, Side::Bid);
        assert_eq!(order.order_type, OrderType::Limit);
        assert_eq!(order.quantity, 0.001.to_quantity());
    }
    
    #[test]
    fn test_signature_generation() {
        let config = Config::default();
        let executor = OrderExecutor::new(config).unwrap();
        
        let signature = executor.generate_signature(
            "1234567890",
            "POST",
            "/api/v2/spot/trade/place-order",
            r#"{"symbol":"BTCUSDT","side":"buy"}"#
        );
        
        assert!(signature.is_ok());
        assert!(!signature.unwrap().is_empty());
    }
}