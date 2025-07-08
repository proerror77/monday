/*!
 * 100U Test Capital Position Management Demo
 * 
 * 展示Phase 1的100U測試資金精確管理：
 * - Kelly公式優化的倉位大小
 * - 動態止損止盈設置
 * - 實時風險監控
 * - 詳細的交易績效分析
 * 
 * 目標績效：
 * - 每日1.5U利潤 (1.5%回報)
 * - 最大10U每日虧損限制 (10%限制)
 * - 90%+資金效率
 * - >60%勝率
 */

use rust_hft::{
    core::{
        config::Config,
        types::*,
    },
    engine::{
        TestCapitalPositionManager,
        TestCapitalConfig,
        PositionAssessment,
        TestPortfolioStatus,
    },
    ml::{
        dl_trend_predictor::{TrendPrediction, TrendClass},
    },
};
use anyhow::Result;
use tokio::time::{Duration, sleep};
use tracing::{info, warn, error};
use tracing_subscriber;
use std::collections::HashMap;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();
    
    info!("=== 100U Test Capital Position Management Demo ===");
    info!("Phase 1: Precise capital allocation and Kelly optimization");
    
    // Configure test capital management
    let test_config = TestCapitalConfig {
        test_capital: 100.0,                 // 100 USDT test allocation
        capital_per_symbol: 40.0,            // 40 USDT max per symbol (BTCUSDT/ETHUSDT)
        min_position_size: 2.0,              // 2 USDT minimum position
        max_position_size: 15.0,             // 15 USDT maximum single position
        daily_loss_limit: 10.0,              // 10 USDT daily loss limit (10%)
        position_loss_limit: 3.0,            // 3 USDT max loss per position
        max_drawdown_limit: 5.0,             // 5 USDT max drawdown (5%)
        target_daily_profit: 1.5,            // 1.5 USDT daily target (1.5%)
        profit_taking_threshold: 2.0,        // Take profit at 2 USDT gain
        kelly_fraction_conservative: 0.15,   // Conservative 15% Kelly fraction
        max_concurrent_positions: 4,         // Max 4 positions (2 per symbol)
        position_hold_time_seconds: 20,      // 20-second average hold time
        rebalance_interval_seconds: 60,      // 60-second rebalancing
        track_trade_performance: true,
        log_position_changes: true,
        calculate_metrics_realtime: true,
    };
    
    // Display configuration
    print_test_config(&test_config);
    
    // Create position manager
    info!("Initializing test capital position manager...");
    let position_manager = TestCapitalPositionManager::new(test_config.clone());
    
    // Simulate trading session
    let demo_duration = Duration::from_secs(180); // 3 minutes demo
    info!("Running 100U test capital demo for {} seconds...", demo_duration.as_secs());
    
    let start_time = std::time::Instant::now();
    let mut status_interval = tokio::time::interval(Duration::from_secs(10));
    let mut trade_interval = tokio::time::interval(Duration::from_secs(3));
    
    // Trading simulation loop
    loop {
        tokio::select! {
            _ = status_interval.tick() => {
                // Print portfolio status every 10 seconds
                let status = position_manager.get_portfolio_status().await;
                print_portfolio_status(&status, &test_config);
                
                // Check if demo time is up
                if start_time.elapsed() >= demo_duration {
                    info!("Demo time completed");
                    break;
                }
            }
            
            _ = trade_interval.tick() => {
                // Simulate trading signals every 3 seconds
                simulate_trading_cycle(&position_manager).await?;
            }
            
            _ = tokio::signal::ctrl_c() => {
                info!("Ctrl+C received, stopping demo...");
                break;
            }
        }
    }
    
    // Final performance analysis
    let final_status = position_manager.get_portfolio_status().await;
    print_final_analysis(&final_status, &test_config, start_time.elapsed());
    
    info!("100U Test Capital Demo completed successfully");
    Ok(())
}

/// Print test configuration
fn print_test_config(config: &TestCapitalConfig) {
    info!("=== Test Capital Configuration ===");
    info!("Total Test Capital: {:.2} USDT", config.test_capital);
    info!("Capital per Symbol: {:.2} USDT (BTCUSDT/ETHUSDT)", config.capital_per_symbol);
    info!("Position Size Range: {:.2} - {:.2} USDT", config.min_position_size, config.max_position_size);
    info!("Daily Loss Limit: {:.2} USDT ({:.1}%)", 
          config.daily_loss_limit, 
          (config.daily_loss_limit / config.test_capital) * 100.0);
    info!("Daily Profit Target: {:.2} USDT ({:.1}%)", 
          config.target_daily_profit,
          (config.target_daily_profit / config.test_capital) * 100.0);
    info!("Max Concurrent Positions: {}", config.max_concurrent_positions);
    info!("Kelly Fraction (Conservative): {:.1}%", config.kelly_fraction_conservative * 100.0);
    info!("Average Hold Time: {}s", config.position_hold_time_seconds);
    info!("=====================================");
}

/// Print portfolio status
fn print_portfolio_status(status: &TestPortfolioStatus, config: &TestCapitalConfig) {
    let metrics = &status.metrics;
    let summary = &status.performance_summary;
    
    info!("=== Portfolio Status ===");
    info!("Capital: Used {:.2}/{:.2} USDT ({:.1}% efficiency)", 
          metrics.allocated_capital, 
          metrics.total_capital,
          metrics.capital_efficiency * 100.0);
    
    info!("P&L: Total {:.2} USDT | Daily {:.2} USDT | Unrealized {:.2} USDT",
          metrics.total_pnl,
          metrics.daily_pnl,
          metrics.unrealized_pnl);
    
    info!("Performance: {:.1}% total return | {:.1}% daily return | Target progress: {:.1}%",
          summary.total_return_pct,
          summary.daily_return_pct,
          summary.target_progress_pct);
    
    info!("Risk: {:.1}% drawdown | {:.1}% risk utilization",
          summary.drawdown_pct,
          summary.risk_utilization_pct);
    
    if metrics.total_trades > 0 {
        info!("Trading: {} trades | {:.1}% win rate | Avg: {:.3} USDT/trade",
              metrics.total_trades,
              metrics.win_rate,
              metrics.avg_trade_pnl);
    }
    
    // Show active positions
    if !status.active_positions.is_empty() {
        info!("Active Positions ({}): ", status.active_positions.len());
        for position in &status.active_positions {
            info!("  {}", position.get_summary());
        }
    }
    
    // Show symbol allocations
    info!("Symbol Allocations:");
    for (symbol, allocation) in &status.symbol_allocations {
        let utilization = (allocation / config.capital_per_symbol) * 100.0;
        info!("  {}: {:.2} USDT ({:.1}% of limit)", symbol, allocation, utilization);
    }
    
    info!("========================");
}

/// Simulate a trading cycle with realistic ML predictions
async fn simulate_trading_cycle(position_manager: &TestCapitalPositionManager) -> Result<()> {
    // Simulate price updates for existing positions
    let mut price_updates = HashMap::new();
    
    // BTCUSDT price simulation (around 50,000 with volatility)
    let btc_base = 50000.0;
    let btc_volatility = 1000.0; // 2% volatility
    let time_factor = (now_micros() % 10000) as f64 / 10000.0;
    let btc_price = btc_base + (time_factor - 0.5) * btc_volatility;
    price_updates.insert("BTCUSDT".to_string(), btc_price);
    
    // ETHUSDT price simulation (around 3,000 with volatility)
    let eth_base = 3000.0;
    let eth_volatility = 100.0; // ~3% volatility  
    let eth_time_factor = ((now_micros() + 5000) % 10000) as f64 / 10000.0;
    let eth_price = eth_base + (eth_time_factor - 0.5) * eth_volatility;
    price_updates.insert("ETHUSDT".to_string(), eth_price);
    
    // Update existing positions
    position_manager.update_positions(price_updates).await?;
    
    // Check for position exits
    let closed_positions = position_manager.check_position_exits().await?;
    if !closed_positions.is_empty() {
        info!("Closed {} positions due to exit criteria", closed_positions.len());
    }
    
    // Generate trading signals based on time (simulate ML predictions)
    if (now_micros() / 1000000) % 3 == 0 { // Every 3rd second approximately
        let symbol = if (now_micros() / 1000) % 2 == 0 { "BTCUSDT" } else { "ETHUSDT" };
        let current_price = if symbol == "BTCUSDT" { btc_price } else { eth_price };
        
        // Generate realistic ML prediction
        let prediction = generate_realistic_prediction();
        
        // Assess if we can open a new position
        let assessment = position_manager.assess_new_position(
            symbol,
            &prediction,
            current_price,
        ).await?;
        
        match assessment {
            PositionAssessment::Approved { 
                suggested_size, 
                kelly_fraction,
                stop_loss,
                take_profit,
                .. 
            } => {
                // Determine trade side
                let side = match prediction.trend_class {
                    TrendClass::StrongUp | TrendClass::WeakUp => Side::Bid,  // Long
                    TrendClass::StrongDown | TrendClass::WeakDown => Side::Ask, // Short
                    TrendClass::Neutral => return Ok(()), // Skip neutral signals
                };
                
                // Open position
                let position_id = position_manager.open_position(
                    symbol.to_string(),
                    side,
                    suggested_size,
                    current_price,
                    &prediction,
                    kelly_fraction,
                    stop_loss,
                    take_profit,
                ).await?;
                
                info!(
                    "🚀 New Signal: {} {} {:.2} USDT @ {:.2} | ID: {} | Kelly: {:.3} | Confidence: {:.3}",
                    symbol,
                    match side { Side::Bid => "LONG", Side::Ask => "SHORT" },
                    suggested_size,
                    current_price,
                    position_id,
                    kelly_fraction,
                    prediction.confidence
                );
            },
            PositionAssessment::Rejected { reason } => {
                info!("❌ Signal rejected for {}: {}", symbol, reason);
            }
        }
    }
    
    Ok(())
}

/// Generate realistic ML trend prediction
fn generate_realistic_prediction() -> TrendPrediction {
    // Generate realistic trend class probabilities
    let trend_classes = [
        TrendClass::StrongDown,
        TrendClass::WeakDown, 
        TrendClass::Neutral,
        TrendClass::WeakUp,
        TrendClass::StrongUp,
    ];
    
    // Create realistic probability distribution (slight bias towards neutral)
    let base_probs = [0.15, 0.20, 0.30, 0.20, 0.15];
    let time_factor = (now_micros() % 10000) as f64 / 10000.0;
    let noise: Vec<f64> = (0..5).map(|i| ((time_factor + i as f64) * 0.1 - 0.05)).collect();
    
    let mut probabilities: Vec<f64> = base_probs.iter()
        .zip(noise.iter())
        .map(|(base, noise)| (base + noise).max(0.05).min(0.5))
        .collect();
    
    // Normalize probabilities
    let sum: f64 = probabilities.iter().sum();
    probabilities.iter_mut().for_each(|p| *p /= sum);
    
    // Find predicted class
    let predicted_class_idx = probabilities
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(idx, _)| idx)
        .unwrap_or(2);
    
    let trend_class = trend_classes[predicted_class_idx];
    let confidence = probabilities[predicted_class_idx];
    
    // Generate expected change based on trend class
    let expected_change_bps = match trend_class {
        TrendClass::StrongUp => 8.0 + (time_factor * 7.0),             // 8-15 bps
        TrendClass::WeakUp => 3.0 + (time_factor * 4.0),               // 3-7 bps
        TrendClass::Neutral => (time_factor - 0.5) * 2.0,              // -1 to +1 bps
        TrendClass::WeakDown => -(3.0 + (time_factor * 4.0)),          // -3 to -7 bps
        TrendClass::StrongDown => -(8.0 + (time_factor * 7.0)),        // -8 to -15 bps
    };
    
    TrendPrediction {
        trend_class,
        class_probabilities: probabilities,
        confidence,
        expected_change_bps,
        inference_latency_us: 500 + (now_micros() % 1000), // 500-1500μs
        timestamp: now_micros(),
        sequence_length: 30,
    }
}

/// Print final performance analysis
fn print_final_analysis(
    status: &TestPortfolioStatus, 
    config: &TestCapitalConfig, 
    runtime: std::time::Duration
) {
    let metrics = &status.metrics;
    let summary = &status.performance_summary;
    
    info!("=== FINAL PERFORMANCE ANALYSIS ===");
    info!("Runtime: {:.1} minutes", runtime.as_secs_f64() / 60.0);
    
    // Capital efficiency analysis
    info!("📊 Capital Efficiency:");
    info!("  Total Capital: {:.2} USDT", metrics.total_capital);
    info!("  Average Utilization: {:.1}%", metrics.capital_efficiency * 100.0);
    info!("  Peak Positions: {}", status.active_positions.len());
    
    // P&L analysis
    info!("💰 P&L Performance:");
    info!("  Total P&L: {:.2} USDT ({:.2}%)", metrics.total_pnl, summary.total_return_pct);
    info!("  Daily P&L: {:.2} USDT ({:.2}%)", metrics.daily_pnl, summary.daily_return_pct);
    info!("  Target Progress: {:.1}% ({:.2}/{:.2} USDT)", 
          summary.target_progress_pct,
          metrics.daily_pnl,
          config.target_daily_profit);
    
    // Performance vs targets
    let performance_grade = if summary.daily_return_pct >= 1.5 {
        "🎯 EXCELLENT"
    } else if summary.daily_return_pct >= 1.0 {
        "✅ GOOD"
    } else if summary.daily_return_pct >= 0.0 {
        "⚠️ BELOW TARGET"
    } else {
        "❌ LOSS"
    };
    info!("  Performance Grade: {}", performance_grade);
    
    // Risk analysis
    info!("⚠️ Risk Metrics:");
    info!("  Max Drawdown: {:.2} USDT ({:.1}%)", metrics.max_drawdown, summary.drawdown_pct);
    info!("  Current Drawdown: {:.2} USDT", metrics.current_drawdown);
    info!("  Risk Limit Usage: {:.1}%", summary.risk_utilization_pct);
    
    let risk_grade = if summary.drawdown_pct <= 2.0 {
        "🛡️ LOW RISK"
    } else if summary.drawdown_pct <= 5.0 {
        "⚠️ MODERATE RISK"
    } else {
        "🚨 HIGH RISK"
    };
    info!("  Risk Grade: {}", risk_grade);
    
    // Trading statistics
    if metrics.total_trades > 0 {
        info!("📈 Trading Statistics:");
        info!("  Total Trades: {}", metrics.total_trades);
        info!("  Win Rate: {:.1}% ({}/{})", 
              metrics.win_rate, 
              metrics.winning_trades, 
              metrics.total_trades);
        info!("  Average Trade: {:.3} USDT ({:.2}%)", 
              metrics.avg_trade_pnl,
              (metrics.avg_trade_pnl / metrics.avg_position_size) * 100.0);
        info!("  Average Win: {:.3} USDT | Average Loss: {:.3} USDT", 
              metrics.avg_winning_trade,
              metrics.avg_losing_trade);
        info!("  Profit Factor: {:.2}", metrics.profit_factor);
        info!("  Average Hold Time: {:.1}s", metrics.avg_hold_time_seconds);
        info!("  Sharpe Ratio: {:.3}", metrics.sharpe_ratio);
        
        let trading_grade = if metrics.win_rate >= 60.0 && metrics.profit_factor >= 1.5 {
            "🏆 EXCELLENT"
        } else if metrics.win_rate >= 50.0 && metrics.profit_factor >= 1.2 {
            "✅ GOOD"
        } else if metrics.win_rate >= 40.0 {
            "⚠️ NEEDS IMPROVEMENT"
        } else {
            "❌ POOR"
        };
        info!("  Trading Grade: {}", trading_grade);
    }
    
    // Symbol breakdown
    info!("🎯 Symbol Performance:");
    for (symbol, allocation) in &status.symbol_allocations {
        let utilization = (allocation / config.capital_per_symbol) * 100.0;
        info!("  {}: {:.2} USDT allocated ({:.1}% of limit)", 
              symbol, allocation, utilization);
    }
    
    // Recommendations for Phase 2
    info!("🚀 Phase 2 Recommendations:");
    if summary.daily_return_pct >= 1.0 && summary.drawdown_pct <= 3.0 {
        info!("  ✅ Ready to scale to 250U capital");
        info!("  ✅ Add 3-5 more trading symbols");
        info!("  ✅ Implement rule-based scalping strategy");
    } else if summary.daily_return_pct >= 0.5 {
        info!("  ⚠️ Optimize position sizing before scaling");
        info!("  ⚠️ Consider tighter risk controls");
    } else {
        info!("  ❌ Improve model accuracy before Phase 2");
        info!("  ❌ Review Kelly fraction and confidence thresholds");
    }
    
    // System health score
    let health_score = calculate_system_health(metrics, summary, config);
    info!("🏥 System Health Score: {:.1}/10.0", health_score);
    
    info!("=====================================");
}

/// Calculate overall system health score
fn calculate_system_health(
    metrics: &rust_hft::engine::TestPortfolioMetrics,
    summary: &rust_hft::engine::PerformanceSummary,
    config: &TestCapitalConfig,
) -> f64 {
    let mut score = 0.0;
    
    // P&L performance (40%)
    if summary.daily_return_pct >= 1.5 {
        score += 4.0;
    } else if summary.daily_return_pct >= 1.0 {
        score += 3.0;
    } else if summary.daily_return_pct >= 0.5 {
        score += 2.0;
    } else if summary.daily_return_pct >= 0.0 {
        score += 1.0;
    }
    
    // Risk management (25%)
    if summary.drawdown_pct <= 2.0 {
        score += 2.5;
    } else if summary.drawdown_pct <= 3.0 {
        score += 2.0;
    } else if summary.drawdown_pct <= 5.0 {
        score += 1.5;
    } else {
        score += 0.5;
    }
    
    // Trading quality (20%)
    if metrics.total_trades > 0 {
        if metrics.win_rate >= 60.0 && metrics.profit_factor >= 1.5 {
            score += 2.0;
        } else if metrics.win_rate >= 50.0 && metrics.profit_factor >= 1.2 {
            score += 1.5;
        } else if metrics.win_rate >= 40.0 {
            score += 1.0;
        } else {
            score += 0.5;
        }
    }
    
    // Capital efficiency (15%)
    if metrics.capital_efficiency >= 0.8 {
        score += 1.5;
    } else if metrics.capital_efficiency >= 0.6 {
        score += 1.0;
    } else if metrics.capital_efficiency >= 0.4 {
        score += 0.5;
    }
    
    score
}