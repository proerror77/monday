/*!
 * 交易所连接测试 - 统一版本
 * 
 * 整合功能：
 * - 基础WebSocket连接测试
 * - 连接健康检查和重连机制
 * - 多种数据类型接收验证
 * - 网络延迟监控
 */

use rust_hft::core::config::Config;
use rust_hft::core::types::*;
use rust_hft::integrations::bitget_connector::*;
use anyhow::Result;
use clap::Parser;
use tracing::{info, warn, error, debug};
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::sleep;

#[derive(Parser)]
#[command(name = "connect_to_exchange")]
#[command(about = "Test connection to exchange with health monitoring")]
struct Args {
    /// Trading symbol
    #[arg(short, long, default_value = "BTCUSDT")]
    symbol: String,

    /// Test duration in seconds
    #[arg(short, long, default_value_t = 60)]
    duration: u64,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Connection timeout in seconds
    #[arg(long, default_value_t = 30)]
    timeout: u64,

    /// Enable reconnection testing
    #[arg(long)]
    test_reconnect: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize tracing
    let log_level = if args.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(format!("rust_hft={}", log_level))
        .init();

    info!("🚀 Starting Exchange Connection Test");
    info!("Symbol: {}, Duration: {}s", args.symbol, args.duration);

    // Load configuration
    let config = Config::load()?;
    
    // Statistics tracking
    let stats = Arc::new(ConnectionStats::new());
    let stats_clone = Arc::clone(&stats);
    
    // Start connection test
    let test_result = run_connection_test(&config, &args, stats_clone).await?;
    
    // Print final statistics
    print_connection_stats(&stats, &test_result);
    
    Ok(())
}

#[derive(Debug)]
struct ConnectionStats {
    messages_received: AtomicU64,
    orderbook_updates: AtomicU64,
    trade_updates: AtomicU64,
    ticker_updates: AtomicU64,
    connection_errors: AtomicU64,
    avg_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
    min_latency_us: AtomicU64,
}

impl ConnectionStats {
    fn new() -> Self {
        Self {
            messages_received: AtomicU64::new(0),
            orderbook_updates: AtomicU64::new(0),
            trade_updates: AtomicU64::new(0),
            ticker_updates: AtomicU64::new(0),
            connection_errors: AtomicU64::new(0),
            avg_latency_us: AtomicU64::new(0),
            max_latency_us: AtomicU64::new(0),
            min_latency_us: AtomicU64::new(u64::MAX),
        }
    }

    fn record_message(&self, message_type: &str, latency_us: u64) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        
        match message_type {
            "orderbook" => self.orderbook_updates.fetch_add(1, Ordering::Relaxed),
            "trade" => self.trade_updates.fetch_add(1, Ordering::Relaxed),
            "ticker" => self.ticker_updates.fetch_add(1, Ordering::Relaxed),
            _ => 0,
        };

        // Update latency statistics
        self.update_latency(latency_us);
    }

    fn update_latency(&self, latency_us: u64) {
        // Update max latency
        let mut max_latency = self.max_latency_us.load(Ordering::Relaxed);
        while latency_us > max_latency {
            match self.max_latency_us.compare_exchange_weak(
                max_latency,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => max_latency = current,
            }
        }

        // Update min latency
        let mut min_latency = self.min_latency_us.load(Ordering::Relaxed);
        while latency_us < min_latency {
            match self.min_latency_us.compare_exchange_weak(
                min_latency,
                latency_us,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => min_latency = current,
            }
        }

        // Update average latency (simplified)
        let current_avg = self.avg_latency_us.load(Ordering::Relaxed);
        let new_avg = (current_avg + latency_us) / 2;
        self.avg_latency_us.store(new_avg, Ordering::Relaxed);
    }

    fn record_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
struct TestResult {
    success: bool,
    duration: Duration,
    reconnections: u32,
    errors: Vec<String>,
}

async fn run_connection_test(
    config: &Config,
    args: &Args,
    stats: Arc<ConnectionStats>,
) -> Result<TestResult> {
    let start_time = Instant::now();
    let mut errors = Vec::new();
    let mut reconnections = 0;
    
    info!("🔌 Connecting to: {}", config.websocket_url());
    
    // Create connector
    let mut connector = BitgetConnector::new(config.clone()).await?;
    
    // Subscribe to symbol
    connector.subscribe_orderbook(&args.symbol).await?;
    connector.subscribe_trades(&args.symbol).await?;
    
    info!("📡 Subscribed to {} data streams", args.symbol);
    
    // Start message processing loop
    let test_duration = Duration::from_secs(args.duration);
    let mut last_message_time = Instant::now();
    
    while start_time.elapsed() < test_duration {
        tokio::select! {
            message_result = connector.next_message() => {
                match message_result {
                    Ok(message) => {
                        let receive_time = now_micros();
                        last_message_time = Instant::now();
                        
                        process_message(message, receive_time, &stats);
                    }
                    Err(e) => {
                        error!("Connection error: {}", e);
                        errors.push(e.to_string());
                        stats.record_error();
                        
                        if args.test_reconnect {
                            info!("🔄 Attempting reconnection...");
                            match connector.reconnect().await {
                                Ok(_) => {
                                    info!("✅ Reconnected successfully");
                                    reconnections += 1;
                                    
                                    // Re-subscribe after reconnection
                                    connector.subscribe_orderbook(&args.symbol).await?;
                                    connector.subscribe_trades(&args.symbol).await?;
                                }
                                Err(reconnect_error) => {
                                    error!("❌ Reconnection failed: {}", reconnect_error);
                                    break;
                                }
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
            
            _ = sleep(Duration::from_millis(100)) => {
                // Check for connection timeout
                if last_message_time.elapsed() > Duration::from_secs(args.timeout) {
                    warn!("⚠️ Connection timeout - no messages received for {}s", args.timeout);
                    errors.push("Connection timeout".to_string());
                    break;
                }
            }
        }
        
        // Print periodic statistics
        if start_time.elapsed().as_secs() % 10 == 0 {
            print_periodic_stats(&stats, start_time.elapsed());
        }
    }
    
    let duration = start_time.elapsed();
    let success = errors.is_empty() && stats.messages_received.load(Ordering::Relaxed) > 0;
    
    Ok(TestResult {
        success,
        duration,
        reconnections,
        errors,
    })
}

fn process_message(message: BitgetMessage, receive_time: Timestamp, stats: &ConnectionStats) {
    let latency_us = now_micros() - receive_time;
    
    match message {
        BitgetMessage::OrderBook { symbol, timestamp, bids, asks } => {
            stats.record_message("orderbook", latency_us);
            debug!("📊 OrderBook for {}: {} bids, {} asks", symbol, bids.len(), asks.len());
            
            // Validate orderbook data
            if !bids.is_empty() && !asks.is_empty() {
                let best_bid = bids.first().map(|b| b.price).unwrap_or(0.0);
                let best_ask = asks.first().map(|a| a.price).unwrap_or(0.0);
                
                if best_bid > 0.0 && best_ask > 0.0 {
                    let spread = best_ask - best_bid;
                    let spread_bps = (spread / ((best_bid + best_ask) / 2.0)) * 10000.0;
                    debug!("💰 Spread: {:.2} bps", spread_bps);
                }
            }
        }
        
        BitgetMessage::Trade { symbol, price, size, side, timestamp } => {
            stats.record_message("trade", latency_us);
            debug!("✅ Trade for {}: {} {} @ {}", symbol, size, side, price);
        }
        
        BitgetMessage::Ticker { symbol, best_bid, best_ask, .. } => {
            stats.record_message("ticker", latency_us);
            debug!("📈 Ticker for {}: bid={}, ask={}", symbol, best_bid, best_ask);
        }
        
        BitgetMessage::Error { code, message } => {
            error!("❌ Exchange error {}: {}", code, message);
            stats.record_error();
        }
    }
}

fn print_periodic_stats(stats: &ConnectionStats, elapsed: Duration) {
    let messages = stats.messages_received.load(Ordering::Relaxed);
    let orderbook = stats.orderbook_updates.load(Ordering::Relaxed);
    let trades = stats.trade_updates.load(Ordering::Relaxed);
    let errors = stats.connection_errors.load(Ordering::Relaxed);
    let avg_latency = stats.avg_latency_us.load(Ordering::Relaxed);
    
    info!("⏱️ {}s: {} msgs ({} OB, {} trades, {} errors), avg latency: {}μs",
          elapsed.as_secs(), messages, orderbook, trades, errors, avg_latency);
}

fn print_connection_stats(stats: &ConnectionStats, result: &TestResult) {
    let messages = stats.messages_received.load(Ordering::Relaxed);
    let orderbook = stats.orderbook_updates.load(Ordering::Relaxed);
    let trades = stats.trade_updates.load(Ordering::Relaxed);
    let ticker = stats.ticker_updates.load(Ordering::Relaxed);
    let errors = stats.connection_errors.load(Ordering::Relaxed);
    let avg_latency = stats.avg_latency_us.load(Ordering::Relaxed);
    let max_latency = stats.max_latency_us.load(Ordering::Relaxed);
    let min_latency = stats.min_latency_us.load(Ordering::Relaxed);
    
    info!("📊 ========== Connection Test Results ==========");
    info!("✅ Success: {}", result.success);
    info!("⏱️ Duration: {:.2}s", result.duration.as_secs_f64());
    info!("🔄 Reconnections: {}", result.reconnections);
    info!("📨 Messages Received: {}", messages);
    info!("  - OrderBook updates: {}", orderbook);
    info!("  - Trade updates: {}", trades);
    info!("  - Ticker updates: {}", ticker);
    info!("❌ Errors: {}", errors);
    info!("🕐 Latency Statistics:");
    info!("  - Average: {}μs", avg_latency);
    info!("  - Min: {}μs", if min_latency == u64::MAX { 0 } else { min_latency });
    info!("  - Max: {}μs", max_latency);
    
    if !result.errors.is_empty() {
        info!("❌ Error Details:");
        for error in &result.errors {
            info!("  - {}", error);
        }
    }
    
    // Performance assessment
    let msg_rate = messages as f64 / result.duration.as_secs_f64();
    info!("📈 Message Rate: {:.2} msg/s", msg_rate);
    
    if avg_latency < 1000 {
        info!("🚀 Excellent latency performance (<1ms)");
    } else if avg_latency < 10000 {
        info!("✅ Good latency performance (<10ms)");
    } else {
        warn!("⚠️ High latency detected (>10ms)");
    }
    
    info!("================================================");
}