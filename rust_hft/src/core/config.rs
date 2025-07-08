/*!
 * Configuration management for Rust HFT system
 */

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub exchange: ExchangeConfig,
    pub trading: TradingConfig,
    pub ml: MLConfig,
    pub performance: PerformanceConfig,
    pub risk: RiskConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConfig {
    pub name: String,
    pub api_key: String,
    pub api_secret: String,
    pub passphrase: String,
    pub sandbox: bool,
    pub websocket_url: String,
    pub rest_api_url: String,
    pub symbol: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    pub order_size: f64,
    pub max_position: f64,
    pub min_spread_bps: f64,
    pub max_spread_bps: f64,
    pub order_timeout_ms: u64,
    pub enable_execution: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MLConfig {
    pub model_path: String,
    pub feature_window_size: usize,
    pub prediction_threshold: f64,
    pub buy_threshold: f64,
    pub sell_threshold: f64,
    pub retrain_interval_hours: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    pub target_latency_us: u64,
    pub max_queue_depth: usize,
    pub enable_metrics: bool,
    pub metrics_interval_ms: u64,
    pub log_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    pub max_daily_loss: f64,
    pub max_position_value: f64,
    pub stop_loss_pct: f64,
    pub max_order_rate: u64,  // orders per second
    pub circuit_breaker_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            exchange: ExchangeConfig {
                name: "bitget".to_string(),
                api_key: std::env::var("BITGET_API_KEY").unwrap_or_default(),
                api_secret: std::env::var("BITGET_API_SECRET").unwrap_or_default(),
                passphrase: std::env::var("BITGET_PASSPHRASE").unwrap_or_default(),
                sandbox: true,
                websocket_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
                rest_api_url: "https://api.bitget.com".to_string(),
                symbol: "BTCUSDT".to_string(),
            },
            trading: TradingConfig {
                order_size: 0.001,      // 0.001 BTC
                max_position: 0.01,     // 0.01 BTC
                min_spread_bps: 1.0,    // 0.01%
                max_spread_bps: 50.0,   // 0.5%
                order_timeout_ms: 1000, // 1 second
                enable_execution: false, // Start with dry-run
            },
            ml: MLConfig {
                model_path: "models/lightgbm_model.txt".to_string(),
                feature_window_size: 100,
                prediction_threshold: 0.6,
                buy_threshold: 0.65,
                sell_threshold: 0.35,
                retrain_interval_hours: 24,
            },
            performance: PerformanceConfig {
                target_latency_us: 100,    // 100μs target
                max_queue_depth: 2048,
                enable_metrics: true,
                metrics_interval_ms: 5000, // 5 seconds
                log_level: "info".to_string(),
            },
            risk: RiskConfig {
                max_daily_loss: 100.0,     // $100 USD
                max_position_value: 1200.0, // $1200 USD
                stop_loss_pct: 0.5,        // 0.5%
                max_order_rate: 10,        // 10 orders per second
                circuit_breaker_enabled: true,
            },
        }
    }
}

impl Config {
    /// Load configuration from environment variables and defaults
    pub fn load() -> Result<Self> {
        // Start with defaults
        let mut config = Self::default();
        
        // Override with environment variables if available
        if let Ok(symbol) = std::env::var("TRADING_SYMBOL") {
            config.exchange.symbol = symbol;
        }
        
        if let Ok(order_size) = std::env::var("ORDER_SIZE") {
            config.trading.order_size = order_size.parse()?;
        }
        
        if let Ok(max_position) = std::env::var("MAX_POSITION") {
            config.trading.max_position = max_position.parse()?;
        }
        
        if let Ok(enable_execution) = std::env::var("ENABLE_EXECUTION") {
            config.trading.enable_execution = enable_execution.parse()?;
        }
        
        if let Ok(model_path) = std::env::var("MODEL_PATH") {
            config.ml.model_path = model_path;
        }
        
        if let Ok(target_latency) = std::env::var("TARGET_LATENCY_US") {
            config.performance.target_latency_us = target_latency.parse()?;
        }
        
        // Validate configuration
        config.validate()?;
        
        Ok(config)
    }
    
    /// Validate configuration parameters
    pub fn validate(&self) -> Result<()> {
        // Exchange validation - Only required for private trading operations
        if self.trading.enable_execution {
            if self.exchange.api_key.is_empty() {
                return Err(anyhow::anyhow!("API key is required for live trading"));
            }
            
            if self.exchange.api_secret.is_empty() {
                return Err(anyhow::anyhow!("API secret is required for live trading"));
            }
        }
        
        // Trading validation
        if self.trading.order_size <= 0.0 {
            return Err(anyhow::anyhow!("Order size must be positive"));
        }
        
        if self.trading.max_position <= 0.0 {
            return Err(anyhow::anyhow!("Max position must be positive"));
        }
        
        if self.trading.min_spread_bps >= self.trading.max_spread_bps {
            return Err(anyhow::anyhow!("Min spread must be less than max spread"));
        }
        
        // ML validation
        if self.ml.buy_threshold <= self.ml.sell_threshold {
            return Err(anyhow::anyhow!("Buy threshold must be greater than sell threshold"));
        }
        
        // Performance validation
        if self.performance.target_latency_us == 0 {
            return Err(anyhow::anyhow!("Target latency must be positive"));
        }
        
        // Risk validation
        if self.risk.max_daily_loss <= 0.0 {
            return Err(anyhow::anyhow!("Max daily loss must be positive"));
        }
        
        if self.risk.stop_loss_pct <= 0.0 || self.risk.stop_loss_pct >= 1.0 {
            return Err(anyhow::anyhow!("Stop loss percentage must be between 0 and 1"));
        }
        
        Ok(())
    }
    
    /// Get Bitget WebSocket URL for public data
    pub fn websocket_url(&self) -> &str {
        &self.exchange.websocket_url
    }
    
    /// Get trading symbol
    pub fn symbol(&self) -> &str {
        &self.exchange.symbol
    }
    
    /// Check if execution is enabled
    pub fn execution_enabled(&self) -> bool {
        self.trading.enable_execution
    }
    
    /// Get target latency in microseconds
    pub fn target_latency_us(&self) -> u64 {
        self.performance.target_latency_us
    }
}