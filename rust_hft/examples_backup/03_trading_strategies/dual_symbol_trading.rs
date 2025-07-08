/*!
 * Dual Symbol Trading Framework Demo
 * 
 * Demonstrates Phase 1 implementation with:
 * - BTCUSDT and ETHUSDT trading
 * - LOB time series feature extraction
 * - DL trend prediction (10-second horizon)
 * - Enhanced risk management with Kelly formula
 * - 100U test capital allocation
 * 
 * Target Performance:
 * - 1.5% daily returns
 * - 50U max risk exposure
 * - <1ms prediction latency
 * - Kelly-optimized position sizing
 */

use rust_hft::{
    core::{
        config::Config,
        types::*,
    },
    engine::{
        DualSymbolTradingFramework,
        DualSymbolTradingConfig,
        FrameworkStatus,
    },
};
use anyhow::Result;
use tokio::time::{Duration, sleep};
use tracing::{info, warn, debug};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();
    
    info!("=== Dual Symbol Trading Framework Demo ===");
    info!("Phase 1: BTCUSDT + ETHUSDT with 100U test capital");
    
    // Load base configuration
    let base_config = Config::load()
        .unwrap_or_else(|_| {
            warn!("Using default config - create config/config.toml for production");
            Config::default()
        });
    
    // Configure dual symbol trading for Phase 1 testing
    let trading_config = DualSymbolTradingConfig {
        symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
        total_capital: 400.0,          // Full capital available
        test_capital: 100.0,           // Phase 1 test allocation
        capital_per_symbol_pct: 0.4,   // Max 40% per symbol
        prediction_interval_ms: 500,   // 500ms prediction cycle
        min_confidence_threshold: 0.65, // 65% minimum confidence
        max_positions_per_symbol: 2,   // Max 2 positions per symbol
        daily_loss_limit_pct: 0.1,     // 10% daily loss limit (10U)
        emergency_stop_pct: 0.15,      // 15% emergency stop (15U)
        position_hold_time_seconds: 15, // 15-second average hold
        target_daily_return_pct: 0.015, // 1.5% daily target
        max_drawdown_pct: 0.05,        // 5% max drawdown
        min_sharpe_ratio: 1.0,         // Minimum Sharpe ratio
    };
    
    // Create and initialize trading framework
    info!("Initializing dual symbol trading framework...");
    let mut framework = DualSymbolTradingFramework::new(
        trading_config.clone(),
        base_config,
    ).await?;
    
    // Display initial configuration
    print_framework_config(&trading_config);
    
    // Start the trading framework
    info!("Starting trading framework...");
    framework.start().await?;
    
    // Run for demo period
    let demo_duration = Duration::from_secs(300); // 5 minutes demo
    info!("Running demo for {} seconds...", demo_duration.as_secs());
    
    let start_time = std::time::Instant::now();
    let mut status_interval = tokio::time::interval(Duration::from_secs(30));
    
    // Main demo loop
    loop {
        tokio::select! {
            _ = status_interval.tick() => {
                // Print status every 30 seconds
                let status = framework.get_status().await;
                print_framework_status(&status);
                
                // Check if demo time is up
                if start_time.elapsed() >= demo_duration {
                    info!("Demo time completed");
                    break;
                }
            }
            
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received, stopping demo...");
                break;
            }
        }
    }
    
    // Stop the framework
    info!("Stopping trading framework...");
    framework.stop().await?;
    
    // Final status report
    let final_status = framework.get_status().await;
    print_final_report(&final_status, &trading_config);
    
    info!("Demo completed successfully");
    Ok(())
}

/// Print framework configuration
fn print_framework_config(config: &DualSymbolTradingConfig) {
    info!("=== Trading Configuration ===");
    info!("Symbols: {:?}", config.symbols);
    info!("Test Capital: {:.2} USDT", config.test_capital);
    info!("Max Risk: {:.2} USDT ({:.1}%)", 
          config.test_capital * 0.5, 50.0);
    info!("Daily Loss Limit: {:.2} USDT ({:.1}%)", 
          config.test_capital * config.daily_loss_limit_pct,
          config.daily_loss_limit_pct * 100.0);
    info!("Emergency Stop: {:.2} USDT ({:.1}%)", 
          config.test_capital * config.emergency_stop_pct,
          config.emergency_stop_pct * 100.0);
    info!("Target Daily Return: {:.1}% ({:.2} USDT)", 
          config.target_daily_return_pct * 100.0,
          config.test_capital * config.target_daily_return_pct);
    info!("Prediction Interval: {}ms", config.prediction_interval_ms);
    info!("Min Confidence: {:.1}%", config.min_confidence_threshold * 100.0);
    info!("================================");
}

/// Print framework status
fn print_framework_status(status: &FrameworkStatus) {
    info!("=== Framework Status ===");
    info!("Running: {} | Uptime: {}s", 
          status.is_running, status.uptime_seconds);
    
    // Portfolio metrics
    let portfolio = &status.portfolio_metrics;
    info!("Portfolio - Capital Used: {:.2}/{:.2} USDT ({:.1}%)",
          portfolio.used_capital,
          portfolio.total_capital,
          (portfolio.used_capital / portfolio.total_capital) * 100.0);
    info!("Portfolio - Daily PnL: {:.2} USDT | Total PnL: {:.2} USDT",
          portfolio.daily_pnl, portfolio.total_pnl);
    info!("Portfolio - Risk Level: {:?} | Emergency Stop: {}",
          portfolio.risk_level, portfolio.is_emergency_stop);
    
    // Symbol-specific status
    for (symbol, symbol_status) in &status.symbols {
        let perf = &symbol_status.performance;
        info!("{} - Active: {} | Positions: {} | Trades: {} | PnL: {:.2} USDT",
              symbol,
              symbol_status.is_active,
              symbol_status.current_positions,
              perf.total_trades,
              perf.daily_pnl);
        
        if perf.total_trades > 0 {
            let win_rate = (perf.winning_trades as f64 / perf.total_trades as f64) * 100.0;
            info!("{} - Win Rate: {:.1}% | Avg Confidence: {:.3} | Avg Hold: {:.1}s",
                  symbol, win_rate, perf.avg_confidence, perf.avg_hold_time_seconds);
        }
    }
    
    // Framework metrics
    let framework = &status.framework_metrics;
    info!("Framework - Signals: {} generated, {} executed | Avg Latency: {:.1}μs",
          framework.total_signals_generated,
          framework.total_signals_executed,
          framework.avg_signal_latency_us);
    
    if framework.total_signals_generated > 0 {
        let execution_rate = (framework.total_signals_executed as f64 / 
                            framework.total_signals_generated as f64) * 100.0;
        info!("Framework - Execution Rate: {:.1}% | Portfolio Sharpe: {:.3}",
              execution_rate, framework.portfolio_sharpe_ratio);
    }
    
    info!("========================");
}

/// Print final performance report
fn print_final_report(status: &FrameworkStatus, config: &DualSymbolTradingConfig) {
    info!("=== FINAL PERFORMANCE REPORT ===");
    
    let runtime_hours = status.uptime_seconds as f64 / 3600.0;
    let portfolio = &status.portfolio_metrics;
    
    // Overall performance
    info!("Runtime: {:.2} hours", runtime_hours);
    info!("Total Capital: {:.2} USDT", portfolio.total_capital);
    info!("Capital Used: {:.2} USDT ({:.1}%)", 
          portfolio.used_capital,
          (portfolio.used_capital / portfolio.total_capital) * 100.0);
    
    // PnL Analysis
    info!("Total PnL: {:.2} USDT ({:.2}%)", 
          portfolio.total_pnl,
          (portfolio.total_pnl / portfolio.total_capital) * 100.0);
    info!("Daily PnL: {:.2} USDT ({:.2}%)", 
          portfolio.daily_pnl,
          (portfolio.daily_pnl / portfolio.total_capital) * 100.0);
    
    // Performance vs targets
    let daily_return_pct = (portfolio.daily_pnl / portfolio.total_capital) * 100.0;
    let target_daily_pct = config.target_daily_return_pct * 100.0;
    let target_status = if daily_return_pct >= target_daily_pct {
        "✅ TARGET MET"
    } else {
        "❌ BELOW TARGET"
    };
    
    info!("Daily Return: {:.2}% (Target: {:.1}%) {}", 
          daily_return_pct, target_daily_pct, target_status);
    
    // Risk metrics
    info!("Max Drawdown: {:.2}% (Limit: {:.1}%)", 
          portfolio.max_drawdown * 100.0,
          config.max_drawdown_pct * 100.0);
    info!("Sharpe Ratio: {:.3} (Target: {:.1})", 
          portfolio.sharpe_ratio, config.min_sharpe_ratio);
    
    // Symbol breakdown
    for (symbol, symbol_status) in &status.symbols {
        let perf = &symbol_status.performance;
        if perf.total_trades > 0 {
            let win_rate = (perf.winning_trades as f64 / perf.total_trades as f64) * 100.0;
            info!("{} Summary:", symbol);
            info!("  Trades: {} | Win Rate: {:.1}% | PnL: {:.2} USDT", 
                  perf.total_trades, win_rate, perf.daily_pnl);
            info!("  Avg Hold: {:.1}s | Avg Confidence: {:.3} | Max DD: {:.2}%",
                  perf.avg_hold_time_seconds, perf.avg_confidence, perf.max_drawdown * 100.0);
        }
    }
    
    // Framework metrics
    let framework = &status.framework_metrics;
    if framework.total_signals_generated > 0 {
        let execution_rate = (framework.total_signals_executed as f64 / 
                            framework.total_signals_generated as f64) * 100.0;
        info!("Signal Execution Rate: {:.1}%", execution_rate);
        info!("Average Signal Latency: {:.1}μs", framework.avg_signal_latency_us);
        info!("BTC-ETH Correlation: {:.3}", framework.correlation_btc_eth);
    }
    
    // System health
    let health_score = calculate_health_score(status, config);
    info!("System Health Score: {:.1}/10.0", health_score);
    
    // Recommendations
    print_recommendations(status, config);
    
    info!("================================");
}

/// Calculate overall system health score
fn calculate_health_score(status: &FrameworkStatus, config: &DualSymbolTradingConfig) -> f64 {
    let mut score = 0.0;
    let portfolio = &status.portfolio_metrics;
    
    // PnL performance (30%)
    let daily_return = portfolio.daily_pnl / portfolio.total_capital;
    let target_return = config.target_daily_return_pct;
    let pnl_score = if daily_return >= target_return {
        3.0
    } else if daily_return >= 0.0 {
        3.0 * (daily_return / target_return)
    } else {
        0.0 // Negative returns
    };
    score += pnl_score;
    
    // Risk management (25%)
    let risk_score = if portfolio.max_drawdown <= config.max_drawdown_pct {
        2.5
    } else {
        2.5 * (config.max_drawdown_pct / portfolio.max_drawdown).min(1.0)
    };
    score += risk_score;
    
    // Execution quality (20%)
    let framework = &status.framework_metrics;
    let execution_rate = if framework.total_signals_generated > 0 {
        framework.total_signals_executed as f64 / framework.total_signals_generated as f64
    } else {
        0.0
    };
    score += execution_rate * 2.0;
    
    // System reliability (15%)
    let uptime_score = if status.uptime_seconds > 60 { 1.5 } else { 0.0 };
    score += uptime_score;
    
    // Sharpe ratio (10%)
    let sharpe_score = if portfolio.sharpe_ratio >= config.min_sharpe_ratio {
        1.0
    } else {
        (portfolio.sharpe_ratio / config.min_sharpe_ratio).max(0.0)
    };
    score += sharpe_score;
    
    score
}

/// Print optimization recommendations
fn print_recommendations(status: &FrameworkStatus, config: &DualSymbolTradingConfig) {
    info!("=== RECOMMENDATIONS ===");
    
    let portfolio = &status.portfolio_metrics;
    let framework = &status.framework_metrics;
    
    // Performance recommendations
    let daily_return = portfolio.daily_pnl / portfolio.total_capital;
    if daily_return < config.target_daily_return_pct {
        info!("💡 Consider increasing position sizes or lowering confidence threshold");
        info!("💡 Current daily return {:.2}% is below target {:.1}%", 
              daily_return * 100.0, config.target_daily_return_pct * 100.0);
    }
    
    // Risk recommendations
    if portfolio.max_drawdown > config.max_drawdown_pct * 0.8 {
        warn!("⚠️  Drawdown is approaching limit - consider tighter risk controls");
    }
    
    // Execution recommendations
    if framework.total_signals_generated > 0 {
        let execution_rate = framework.total_signals_executed as f64 / 
                            framework.total_signals_generated as f64;
        if execution_rate < 0.5 {
            info!("💡 Low execution rate ({:.1}%) - consider lowering confidence threshold", 
                  execution_rate * 100.0);
        }
    }
    
    // Latency recommendations
    if framework.avg_signal_latency_us > 1000.0 {
        warn!("⚠️  High signal latency ({:.1}μs) - optimize prediction pipeline", 
              framework.avg_signal_latency_us);
    }
    
    // Symbol-specific recommendations
    for (symbol, symbol_status) in &status.symbols {
        let perf = &symbol_status.performance;
        if perf.total_trades > 10 {
            let win_rate = perf.winning_trades as f64 / perf.total_trades as f64;
            if win_rate < 0.5 {
                warn!("⚠️  {} win rate is low ({:.1}%) - review prediction model", 
                      symbol, win_rate * 100.0);
            }
        }
    }
    
    // Next phase recommendations
    info!("🚀 Ready for Phase 2:");
    info!("   - Scale to 250U capital");
    info!("   - Add 3-5 more trading symbols");
    info!("   - Implement rule-based scalping strategy");
    info!("   - Add cross-symbol arbitrage detection");
    
    info!("========================");
}

/// Helper function to create a mock trading signal for demonstration
#[allow(dead_code)]
fn create_demo_signal(symbol: &str, confidence: f64) -> Result<()> {
    debug!("Creating demo signal for {} with confidence {:.3}", symbol, confidence);
    // This would integrate with the actual signal generation in a real implementation
    Ok(())
}

/// Simulate market conditions for demo
#[allow(dead_code)]
async fn simulate_market_volatility() {
    // In a real implementation, this could inject test market data
    // to demonstrate different trading scenarios
    sleep(Duration::from_millis(100)).await;
}