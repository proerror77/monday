//! Cross-exchange arbitrage strategy demonstration
//!
//! This example shows how to set up and run the cross-exchange arbitrage strategy
//! with real-time market data from multiple exchanges.

use hft_strategy::{
    CrossExchangeArbitrageStrategy, ArbitrageConfig,
    MultiExchangeArbitrageManager, MultiExchangeArbitrageConfig, ExchangeConfig,
    Strategy, StrategyContext
};
use hft_data::{
    MarketDataMessage, MessageType,
    exchange::{MultiExchangeManager, MultiExchangeConfig},
    bitget::{BitgetConnector, BitgetConfig},
    binance::{BinanceConnector, BinanceConfig, BinanceMarketType, BinanceStreamType, DepthSpeed}
};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt::init();
    
    info!("Starting Cross-Exchange Arbitrage Strategy Demo");
    
    // Setup arbitrage configuration
    let arbitrage_config = ArbitrageConfig {
        min_profit_bps: 15.0,  // 0.15% minimum profit after fees
        max_position_size: Decimal::from(1000),  // $1000 max position
        max_opportunity_age_ms: 300,  // 300ms max opportunity age
        min_quantity: 0.01,
        max_concurrent_arbitrages: 3,
        max_execution_latency_us: 40000,  // 40ms max execution latency
        ..Default::default()
    };
    
    // Setup exchanges configuration
    let mut instruments = HashSet::new();
    instruments.insert("BTCUSDT".to_string());
    instruments.insert("ETHUSDT".to_string());
    
    let multi_exchange_config = MultiExchangeArbitrageConfig {
        arbitrage_config,
        exchanges: vec![
            ExchangeConfig {
                name: "binance".to_string(),
                weight: 0.6,
                max_position_ratio: 0.4,
                fee_bps: 10.0,  // 0.1%
                latency_target_us: 15000,  // 15ms target
            },
            ExchangeConfig {
                name: "bitget".to_string(),
                weight: 0.4,
                max_position_ratio: 0.4,
                fee_bps: 10.0,  // 0.1%
                latency_target_us: 20000,  // 20ms target
            },
        ],
        instruments,
        max_latency_tolerance_us: 50000,
        min_execution_spread_bps: 5.0,
        rebalancing_interval_sec: 30,
        ..Default::default()
    };
    
    // Create multi-exchange manager
    let exchange_manager_config = MultiExchangeConfig::default();
    let exchange_manager = Arc::new(
        MultiExchangeManager::new(exchange_manager_config).await?
    );
    
    // Create arbitrage manager
    let arbitrage_manager = MultiExchangeArbitrageManager::new(
        multi_exchange_config.clone(),
        exchange_manager.clone(),
    ).await?;
    
    info!("Arbitrage manager created successfully");
    
    // Start the arbitrage strategy
    arbitrage_manager.start().await?;
    info!("Arbitrage strategy started");
    
    // Setup market data connections
    setup_market_data_feeds(&multi_exchange_config).await?;
    
    // Run for demonstration period
    let demo_duration = Duration::from_secs(300); // 5 minutes
    info!("Running arbitrage strategy for {} seconds", demo_duration.as_secs());
    
    // Monitor and report statistics
    let monitor_handle = tokio::spawn({
        let arbitrage_manager = Arc::new(arbitrage_manager);
        async move {
            monitor_arbitrage_performance(arbitrage_manager).await;
        }
    });
    
    // Wait for demo duration
    sleep(demo_duration).await;
    
    info!("Demo completed, shutting down...");
    monitor_handle.abort();
    
    // Show final statistics
    // Note: The arbitrage_manager is moved into the monitor task, 
    // so we can't access it here. In a real implementation, you'd use Arc<RwLock<>> or similar.
    
    info!("Cross-Exchange Arbitrage Strategy Demo completed");
    Ok(())
}

async fn setup_market_data_feeds(
    config: &MultiExchangeArbitrageConfig
) -> Result<(), Box<dyn std::error::Error>> {
    info!("Setting up market data feeds for {} exchanges", config.exchanges.len());
    
    // Setup Binance connection
    if config.exchanges.iter().any(|e| e.name == "binance") {
        info!("Setting up Binance market data feed");
        
        let binance_config = BinanceConfig {
            market_type: BinanceMarketType::Spot,
            ..Default::default()
        };
        
        let binance_connector = BinanceConnector::new(binance_config).await?;
        binance_connector.start().await?;
        
        // Subscribe to depth streams for configured instruments
        for instrument in &config.instruments {
            let stream = BinanceStreamType::Depth {
                symbol: instrument.clone(),
                speed: DepthSpeed::Ms100,
            };
            binance_connector.subscribe(stream).await?;
            info!("Subscribed to Binance depth stream for {}", instrument);
        }
    }
    
    // Setup Bitget connection
    if config.exchanges.iter().any(|e| e.name == "bitget") {
        info!("Setting up Bitget market data feed");
        
        let bitget_config = BitgetConfig::default();
        let mut bitget_connector = BitgetConnector::new(bitget_config);
        
        // Connect and subscribe
        let _data_rx = bitget_connector.connect().await?;
        
        for instrument in &config.instruments {
            info!("Would subscribe to Bitget streams for {}", instrument);
            // Note: Actual subscription would be implemented here
        }
    }
    
    info!("Market data feeds setup completed");
    Ok(())
}

async fn monitor_arbitrage_performance(
    arbitrage_manager: Arc<MultiExchangeArbitrageManager>
) {
    let mut last_stats_time = std::time::Instant::now();
    let stats_interval = Duration::from_secs(30);
    
    loop {
        if last_stats_time.elapsed() >= stats_interval {
            // Get current statistics
            let stats = arbitrage_manager.get_stats().await;
            let risk_violations = arbitrage_manager.get_risk_violations().await;
            let emergency_stopped = arbitrage_manager.is_emergency_stopped().await;
            
            // Report performance metrics
            info!("=== Arbitrage Performance Report ===");
            info!("Total opportunities identified: {}", stats.total_opportunities);
            info!("Opportunities executed: {}", stats.executed_opportunities);
            info!("Success rate: {:.2}%", stats.success_rate * 100.0);
            info!("Total PnL: ${:.2}", stats.total_pnl);
            info!("Average execution latency: {:.2}µs", stats.avg_execution_latency_us);
            
            if !risk_violations.is_empty() {
                warn!("Risk violations detected: {}", risk_violations.len());
                for violation in &risk_violations {
                    warn!("  - {}: {}", violation.violation_type, violation.description);
                }
            }
            
            if emergency_stopped {
                error!("EMERGENCY STOP ACTIVE");
            }
            
            // Exchange-specific statistics
            for (exchange, exchange_stats) in &stats.exchange_stats {
                info!("Exchange {}: {} trades, {:.2}µs avg latency, {} errors",
                     exchange, 
                     exchange_stats.trades_executed,
                     exchange_stats.avg_latency_us,
                     exchange_stats.error_count);
            }
            
            info!("=====================================");
            
            last_stats_time = std::time::Instant::now();
        }
        
        sleep(Duration::from_secs(1)).await;
    }
}

/// Example of manual arbitrage opportunity detection
async fn demonstrate_manual_arbitrage_detection() -> Result<(), Box<dyn std::error::Error>> {
    info!("Demonstrating manual arbitrage opportunity detection");
    
    let strategy = CrossExchangeArbitrageStrategy::new(ArbitrageConfig::default());
    
    // Simulate market data from two exchanges
    let binance_data = create_test_market_data("binance", "BTCUSDT", 50000.0, 50100.0);
    let bitget_data = create_test_market_data("bitget", "BTCUSDT", 50150.0, 50200.0);
    
    let context = StrategyContext {
        positions: HashMap::new(),
        market_data: HashMap::new(),
        balance: Decimal::from(100000),
        config: HashMap::new(),
    };
    
    // Process market data and look for signals
    let mut strategy = strategy;
    let signals1 = strategy.on_market_data(&binance_data, &context).await?;
    let signals2 = strategy.on_market_data(&bitget_data, &context).await?;
    
    info!("Signals from Binance data: {}", signals1.len());
    info!("Signals from Bitget data: {}", signals2.len());
    
    // Execute any arbitrage signals
    let all_signals = [signals1, signals2].concat();
    if !all_signals.is_empty() {
        let orders = strategy.execute_signals(all_signals, &context).await?;
        info!("Generated {} arbitrage orders", orders.len());
        
        for order in orders {
            info!("Order: {:?} {} {} @ {}",
                 order.side,
                 order.instrument_id,
                 order.quantity,
                 order.price);
        }
    }
    
    Ok(())
}

fn create_test_market_data(exchange: &str, symbol: &str, bid: f64, ask: f64) -> MarketDataMessage {
    MarketDataMessage {
        message_type: MessageType::OrderBook,
        exchange: exchange.to_string(),
        symbol: symbol.to_string(),
        timestamp: chrono::Utc::now(),
        local_timestamp: Some(chrono::Utc::now()),
        data: serde_json::json!({
            "bids": [[bid.to_string(), "1.0"]],
            "asks": [[ask.to_string(), "1.0"]]
        }),
    }
}

/// Configuration validation example
fn validate_arbitrage_configuration(config: &ArbitrageConfig) -> Result<(), String> {
    if config.min_profit_bps <= 0.0 {
        return Err("Minimum profit must be positive".to_string());
    }
    
    if config.max_position_size <= Decimal::ZERO {
        return Err("Maximum position size must be positive".to_string());
    }
    
    if config.max_opportunity_age_ms > 1000 {
        warn!("Opportunity age threshold is quite high ({}ms), consider reducing for better execution", 
              config.max_opportunity_age_ms);
    }
    
    if config.max_execution_latency_us > 100000 {
        warn!("Execution latency threshold is high ({}µs), may impact profitability", 
              config.max_execution_latency_us);
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_config_validation() {
        let valid_config = ArbitrageConfig::default();
        assert!(validate_arbitrage_configuration(&valid_config).is_ok());
        
        let invalid_config = ArbitrageConfig {
            min_profit_bps: -1.0,
            ..Default::default()
        };
        assert!(validate_arbitrage_configuration(&invalid_config).is_err());
    }
    
    #[tokio::test]
    async fn test_manual_arbitrage_detection() {
        assert!(demonstrate_manual_arbitrage_detection().await.is_ok());
    }
}