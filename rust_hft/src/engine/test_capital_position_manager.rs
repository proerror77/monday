/*!
 * Test Capital Position Manager for 100U Phase 1 Testing
 * 
 * Specialized position management for Phase 1 with 100U test capital:
 * - Precise capital allocation: 40U per symbol max
 * - Dynamic position sizing with Kelly optimization
 * - Real-time P&L tracking and risk adjustment
 * - Performance metrics for Phase 2 scaling decisions
 * 
 * Target Performance:
 * - 1.5U daily profit (1.5% return)
 * - Max 10U daily loss (10% limit)
 * - 90%+ capital efficiency
 * - >60% win rate with controlled drawdown
 */

use crate::core::types::*;
use crate::engine::{
    EnhancedRiskController,
    RiskAssessment,
    PositionRisk,
    PortfolioRiskMetrics,
};
use crate::ml::{TrendPrediction, TrendClass};
use anyhow::Result;
use std::collections::HashMap;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};
use std::sync::Arc;

/// Configuration for 100U test capital management
#[derive(Debug, Clone)]
pub struct TestCapitalConfig {
    /// Total test capital allocation
    pub test_capital: f64,                  // 100.0 USDT
    
    /// Capital allocation per symbol
    pub capital_per_symbol: f64,            // 40.0 USDT max per symbol
    pub min_position_size: f64,             // 2.0 USDT minimum position
    pub max_position_size: f64,             // 15.0 USDT maximum single position
    
    /// Risk management for testing
    pub daily_loss_limit: f64,              // 10.0 USDT (10% of test capital)
    pub position_loss_limit: f64,           // 3.0 USDT max loss per position
    pub max_drawdown_limit: f64,            // 5.0 USDT (5% max drawdown)
    
    /// Trading parameters
    pub target_daily_profit: f64,           // 1.5 USDT (1.5% daily target)
    pub profit_taking_threshold: f64,       // 2.0 USDT (take profit at 2% gain)
    pub kelly_fraction_conservative: f64,   // 0.15 (conservative Kelly for testing)
    
    /// Position management
    pub max_concurrent_positions: usize,    // 4 positions max (2 per symbol)
    pub position_hold_time_seconds: u64,    // 15-30 seconds target hold time
    pub rebalance_interval_seconds: u64,    // 60 seconds rebalancing
    
    /// Performance tracking
    pub track_trade_performance: bool,      // true
    pub log_position_changes: bool,         // true
    pub calculate_metrics_realtime: bool,   // true
}

impl Default for TestCapitalConfig {
    fn default() -> Self {
        Self {
            test_capital: 100.0,
            capital_per_symbol: 40.0,
            min_position_size: 2.0,
            max_position_size: 15.0,
            daily_loss_limit: 10.0,
            position_loss_limit: 3.0,
            max_drawdown_limit: 5.0,
            target_daily_profit: 1.5,
            profit_taking_threshold: 2.0,
            kelly_fraction_conservative: 0.15,
            max_concurrent_positions: 4,
            position_hold_time_seconds: 20,
            rebalance_interval_seconds: 60,
            track_trade_performance: true,
            log_position_changes: true,
            calculate_metrics_realtime: true,
        }
    }
}

/// Individual position in the test portfolio
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestPosition {
    /// Position identification
    pub id: String,
    pub symbol: String,
    pub side: Side,
    
    /// Position sizing
    pub size_usdt: f64,                     // Position size in USDT
    pub quantity: f64,                      // Asset quantity
    pub entry_price: f64,                   // Entry price
    pub current_price: f64,                 // Current market price
    
    /// P&L tracking
    pub unrealized_pnl: f64,                // Current unrealized P&L
    pub realized_pnl: f64,                  // Realized P&L if closed
    pub max_profit: f64,                    // Maximum profit reached
    pub max_loss: f64,                      // Maximum loss reached
    
    /// Risk metrics
    pub stop_loss: Option<f64>,             // Stop loss price
    pub take_profit: Option<f64>,           // Take profit price
    pub risk_score: f64,                    // Position risk score (0-1)
    
    /// Timing
    pub entry_time: Timestamp,              // Position entry time
    pub target_exit_time: Timestamp,        // Target exit time
    pub last_update: Timestamp,             // Last price update
    
    /// ML context
    pub entry_confidence: f64,              // ML confidence at entry
    pub predicted_trend: TrendClass,        // Predicted trend direction
    pub kelly_fraction: f64,                // Kelly fraction used
}

impl TestPosition {
    /// Calculate current P&L for the position
    pub fn calculate_pnl(&mut self) -> f64 {
        let price_change = self.current_price - self.entry_price;
        let pnl = match self.side {
            Side::Bid => price_change * self.quantity,  // Long position
            Side::Ask => -price_change * self.quantity, // Short position
        };
        
        self.unrealized_pnl = pnl;
        
        // Update max profit/loss tracking
        if pnl > self.max_profit {
            self.max_profit = pnl;
        }
        if pnl < self.max_loss {
            self.max_loss = pnl;
        }
        
        self.last_update = now_micros();
        pnl
    }
    
    /// Check if position should be closed based on risk rules
    pub fn should_close(&self, config: &TestCapitalConfig) -> (bool, String) {
        let current_time = now_micros();
        
        // Check stop loss
        if let Some(stop_loss) = self.stop_loss {
            let should_stop = match self.side {
                Side::Bid => self.current_price <= stop_loss,
                Side::Ask => self.current_price >= stop_loss,
            };
            if should_stop {
                return (true, "Stop loss triggered".to_string());
            }
        }
        
        // Check take profit
        if let Some(take_profit) = self.take_profit {
            let should_take = match self.side {
                Side::Bid => self.current_price >= take_profit,
                Side::Ask => self.current_price <= take_profit,
            };
            if should_take {
                return (true, "Take profit triggered".to_string());
            }
        }
        
        // Check position loss limit
        if self.unrealized_pnl <= -config.position_loss_limit {
            return (true, format!("Position loss limit exceeded: {:.2}", self.unrealized_pnl));
        }
        
        // Check time-based exit
        if current_time >= self.target_exit_time {
            return (true, "Target hold time reached".to_string());
        }
        
        // Check if position has been profitable and should be locked in
        if self.max_profit > config.profit_taking_threshold && 
           self.unrealized_pnl < self.max_profit * 0.5 {
            return (true, "Profit protection triggered".to_string());
        }
        
        (false, String::new())
    }
    
    /// Get position summary for logging
    pub fn get_summary(&self) -> String {
        format!(
            "{} {} {:.2} USDT @ {:.2} | PnL: {:.2} | Age: {}s",
            self.symbol,
            match self.side { Side::Bid => "LONG", Side::Ask => "SHORT" },
            self.size_usdt,
            self.entry_price,
            self.unrealized_pnl,
            (now_micros() - self.entry_time) / 1_000_000
        )
    }
}

/// Portfolio metrics for 100U test capital
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TestPortfolioMetrics {
    /// Capital utilization
    pub total_capital: f64,                 // 100.0 USDT
    pub allocated_capital: f64,             // Currently allocated to positions
    pub available_capital: f64,             // Available for new positions
    pub capital_efficiency: f64,            // % of capital actively used
    
    /// P&L tracking
    pub total_pnl: f64,                     // All-time P&L
    pub daily_pnl: f64,                     // Today's P&L
    pub unrealized_pnl: f64,                // Current unrealized P&L
    pub realized_pnl: f64,                  // Realized P&L from closed positions
    
    /// Performance metrics
    pub total_trades: u64,                  // Total number of trades
    pub winning_trades: u64,                // Number of winning trades
    pub win_rate: f64,                      // Win rate percentage
    pub avg_trade_pnl: f64,                 // Average P&L per trade
    pub avg_winning_trade: f64,             // Average winning trade P&L
    pub avg_losing_trade: f64,              // Average losing trade P&L
    pub profit_factor: f64,                 // Gross profit / gross loss
    
    /// Risk metrics
    pub max_drawdown: f64,                  // Maximum drawdown experienced
    pub current_drawdown: f64,              // Current drawdown
    pub sharpe_ratio: f64,                  // Risk-adjusted returns
    pub daily_var_95: f64,                  // Daily 95% VaR
    
    /// Position metrics
    pub active_positions: usize,            // Number of active positions
    pub avg_position_size: f64,             // Average position size
    pub avg_hold_time_seconds: f64,         // Average position hold time
    pub max_concurrent_positions: usize,    // Maximum concurrent positions
    
    /// Timing
    pub last_update: Timestamp,
    pub session_start: Timestamp,
}

/// Test capital position manager
pub struct TestCapitalPositionManager {
    /// Configuration
    config: TestCapitalConfig,
    
    /// Active positions
    positions: Arc<RwLock<HashMap<String, TestPosition>>>,
    
    /// Portfolio metrics
    metrics: Arc<Mutex<TestPortfolioMetrics>>,
    
    /// Enhanced risk controller integration
    risk_controller: Option<Arc<Mutex<EnhancedRiskController>>>,
    
    /// Trade history for analysis
    trade_history: Arc<Mutex<Vec<ClosedPosition>>>,
    
    /// Performance tracking
    daily_pnl_history: Arc<Mutex<Vec<(Timestamp, f64)>>>,
    position_counter: Arc<Mutex<u64>>,
    
    /// Capital allocation by symbol
    symbol_allocations: Arc<Mutex<HashMap<String, f64>>>,
}

/// Closed position record for analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedPosition {
    pub position_id: String,
    pub symbol: String,
    pub side: Side,
    pub size_usdt: f64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub realized_pnl: f64,
    pub hold_time_seconds: u64,
    pub entry_confidence: f64,
    pub predicted_trend: TrendClass,
    pub actual_trend: Option<TrendClass>,
    pub close_reason: String,
    pub kelly_fraction: f64,
    pub entry_time: Timestamp,
    pub exit_time: Timestamp,
}

impl TestCapitalPositionManager {
    /// Create new test capital position manager
    pub fn new(config: TestCapitalConfig) -> Self {
        let initial_metrics = TestPortfolioMetrics {
            total_capital: config.test_capital,
            available_capital: config.test_capital,
            session_start: now_micros(),
            last_update: now_micros(),
            ..Default::default()
        };
        
        // Initialize symbol allocations
        let mut symbol_allocations = HashMap::new();
        symbol_allocations.insert("BTCUSDT".to_string(), 0.0);
        symbol_allocations.insert("ETHUSDT".to_string(), 0.0);
        
        Self {
            config,
            positions: Arc::new(RwLock::new(HashMap::new())),
            metrics: Arc::new(Mutex::new(initial_metrics)),
            risk_controller: None,
            trade_history: Arc::new(Mutex::new(Vec::new())),
            daily_pnl_history: Arc::new(Mutex::new(Vec::new())),
            position_counter: Arc::new(Mutex::new(0)),
            symbol_allocations: Arc::new(Mutex::new(symbol_allocations)),
        }
    }
    
    /// Set enhanced risk controller for integration
    pub fn set_risk_controller(&mut self, risk_controller: Arc<Mutex<EnhancedRiskController>>) {
        self.risk_controller = Some(risk_controller);
    }
    
    /// Assess if a new position can be opened
    pub async fn assess_new_position(
        &self,
        symbol: &str,
        trend_prediction: &TrendPrediction,
        current_price: f64,
    ) -> Result<PositionAssessment> {
        let metrics = self.metrics.lock().await;
        let positions = self.positions.read().await;
        let symbol_allocations = self.symbol_allocations.lock().await;
        
        // Check basic constraints
        if positions.len() >= self.config.max_concurrent_positions {
            return Ok(PositionAssessment::Rejected {
                reason: "Maximum concurrent positions reached".to_string(),
            });
        }
        
        // Check daily loss limit
        if metrics.daily_pnl <= -self.config.daily_loss_limit {
            return Ok(PositionAssessment::Rejected {
                reason: format!("Daily loss limit exceeded: {:.2}", metrics.daily_pnl),
            });
        }
        
        // Check available capital
        if metrics.available_capital < self.config.min_position_size {
            return Ok(PositionAssessment::Rejected {
                reason: "Insufficient available capital".to_string(),
            });
        }
        
        // Check symbol allocation limit
        let current_symbol_allocation = symbol_allocations.get(symbol).copied().unwrap_or(0.0);
        if current_symbol_allocation >= self.config.capital_per_symbol {
            return Ok(PositionAssessment::Rejected {
                reason: format!("Symbol allocation limit reached: {:.2}", current_symbol_allocation),
            });
        }
        
        // Calculate optimal position size using Kelly with conservative adjustment
        let kelly_size = self.calculate_kelly_position_size(
            symbol, 
            trend_prediction,
            current_price,
            &metrics
        ).await;
        
        // Apply conservative scaling and limits
        let conservative_size = kelly_size * self.config.kelly_fraction_conservative;
        let available_for_symbol = self.config.capital_per_symbol - current_symbol_allocation;
        let max_new_position = available_for_symbol.min(self.config.max_position_size);
        
        let suggested_size = conservative_size
            .max(self.config.min_position_size)
            .min(max_new_position)
            .min(metrics.available_capital);
        
        // Check if suggested size is meaningful
        if suggested_size < self.config.min_position_size {
            return Ok(PositionAssessment::Rejected {
                reason: "Position size too small after constraints".to_string(),
            });
        }
        
        // Calculate risk metrics for the position
        let risk_score = self.calculate_position_risk_score(
            symbol, 
            suggested_size, 
            trend_prediction,
            &metrics
        );
        
        // Determine stop loss and take profit levels
        let (stop_loss, take_profit) = self.calculate_risk_levels(
            current_price,
            trend_prediction,
            suggested_size
        );
        
        Ok(PositionAssessment::Approved {
            suggested_size,
            kelly_fraction: conservative_size / metrics.total_capital,
            risk_score,
            stop_loss,
            take_profit,
            max_hold_time_seconds: self.config.position_hold_time_seconds,
            confidence_requirement: 0.65,
        })
    }
    
    /// Calculate Kelly-optimized position size for test capital
    async fn calculate_kelly_position_size(
        &self,
        symbol: &str,
        prediction: &TrendPrediction,
        current_price: f64,
        metrics: &TestPortfolioMetrics,
    ) -> f64 {
        // Get historical performance for this symbol from trade history
        let trade_history = self.trade_history.lock().await;
        let symbol_trades: Vec<_> = trade_history
            .iter()
            .filter(|trade| &trade.symbol == symbol)
            .rev()
            .take(50) // Last 50 trades for this symbol
            .collect();
        
        if symbol_trades.len() < 5 {
            // Not enough history, use conservative default
            return metrics.total_capital * 0.05; // 5% of capital
        }
        
        // Calculate win rate and average returns
        let winning_trades = symbol_trades.iter().filter(|trade| trade.realized_pnl > 0.0).count();
        let win_rate = winning_trades as f64 / symbol_trades.len() as f64;
        
        let avg_win = if winning_trades > 0 {
            symbol_trades.iter()
                .filter(|trade| trade.realized_pnl > 0.0)
                .map(|trade| trade.realized_pnl / trade.size_usdt)
                .sum::<f64>() / winning_trades as f64
        } else {
            0.02 // Default 2% average win
        };
        
        let losing_trades = symbol_trades.len() - winning_trades;
        let avg_loss = if losing_trades > 0 {
            symbol_trades.iter()
                .filter(|trade| trade.realized_pnl < 0.0)
                .map(|trade| trade.realized_pnl.abs() / trade.size_usdt)
                .sum::<f64>() / losing_trades as f64
        } else {
            0.015 // Default 1.5% average loss
        };
        
        // Kelly formula: f = (bp - q) / b
        // where b = avg_win/avg_loss, p = win_rate, q = lose_rate
        let b = if avg_loss > 0.0 { avg_win / avg_loss } else { 1.33 };
        let p = win_rate;
        let q = 1.0 - win_rate;
        
        let kelly_fraction = ((b * p) - q) / b;
        
        // Apply confidence adjustment
        let confidence_adjusted_kelly = kelly_fraction * prediction.confidence;
        
        // Apply conservative cap and ensure positive
        let capped_kelly = confidence_adjusted_kelly.max(0.0).min(0.2); // Max 20% Kelly
        
        let kelly_size = metrics.total_capital * capped_kelly;
        
        debug!(
            "Kelly calculation for {}: fraction={:.4}, size={:.2} USDT (win_rate={:.3}, avg_win={:.4}, avg_loss={:.4})",
            symbol, capped_kelly, kelly_size, win_rate, avg_win, avg_loss
        );
        
        kelly_size
    }
    
    /// Calculate position risk score
    fn calculate_position_risk_score(
        &self,
        symbol: &str,
        position_size: f64,
        prediction: &TrendPrediction,
        metrics: &TestPortfolioMetrics,
    ) -> f64 {
        let mut risk_score = 0.0;
        
        // Size risk (larger positions = higher risk)
        let size_risk = (position_size / self.config.max_position_size) * 0.25;
        risk_score += size_risk;
        
        // Confidence risk (lower confidence = higher risk)
        let confidence_risk = (1.0 - prediction.confidence) * 0.25;
        risk_score += confidence_risk;
        
        // Portfolio concentration risk
        let concentration_risk = if metrics.total_capital > 0.0 {
            (position_size / metrics.total_capital) * 0.2
        } else {
            0.0
        };
        risk_score += concentration_risk;
        
        // Current drawdown risk
        let drawdown_risk = (metrics.current_drawdown / self.config.max_drawdown_limit).min(1.0) * 0.15;
        risk_score += drawdown_risk;
        
        // Symbol-specific risk (simplified)
        let symbol_risk = match symbol {
            "BTCUSDT" => 0.1, // Lower volatility
            "ETHUSDT" => 0.15, // Higher volatility
            _ => 0.2,
        };
        risk_score += symbol_risk;
        
        risk_score.min(1.0)
    }
    
    /// Calculate stop loss and take profit levels
    fn calculate_risk_levels(
        &self,
        entry_price: f64,
        prediction: &TrendPrediction,
        position_size: f64,
    ) -> (Option<f64>, Option<f64>) {
        // Dynamic stop loss based on position size (smaller positions = tighter stops)
        let stop_loss_pct = if position_size <= 5.0 {
            0.015 // 1.5% for small positions
        } else if position_size <= 10.0 {
            0.012 // 1.2% for medium positions
        } else {
            0.01 // 1.0% for large positions
        };
        
        // Take profit based on prediction confidence and expected change
        let take_profit_pct = (prediction.expected_change_bps.abs() / 10000.0).max(0.02).min(0.04);
        
        let stop_loss = match prediction.trend_class {
            TrendClass::StrongUp | TrendClass::WeakUp => {
                Some(entry_price * (1.0 - stop_loss_pct))
            },
            TrendClass::StrongDown | TrendClass::WeakDown => {
                Some(entry_price * (1.0 + stop_loss_pct))
            },
            TrendClass::Neutral => None, // No stop loss for neutral trades
        };
        
        let take_profit = match prediction.trend_class {
            TrendClass::StrongUp | TrendClass::WeakUp => {
                Some(entry_price * (1.0 + take_profit_pct))
            },
            TrendClass::StrongDown | TrendClass::WeakDown => {
                Some(entry_price * (1.0 - take_profit_pct))
            },
            TrendClass::Neutral => None,
        };
        
        (stop_loss, take_profit)
    }
    
    /// Open a new position
    pub async fn open_position(
        &self,
        symbol: String,
        side: Side,
        size_usdt: f64,
        entry_price: f64,
        prediction: &TrendPrediction,
        kelly_fraction: f64,
        stop_loss: Option<f64>,
        take_profit: Option<f64>,
    ) -> Result<String> {
        let mut positions = self.positions.write().await;
        let mut metrics = self.metrics.lock().await;
        let mut position_counter = self.position_counter.lock().await;
        let mut symbol_allocations = self.symbol_allocations.lock().await;
        
        // Generate position ID
        *position_counter += 1;
        let position_id = format!("TEST_{}_{}", symbol, *position_counter);
        
        // Calculate quantity
        let quantity = size_usdt / entry_price;
        
        // Create position
        let position = TestPosition {
            id: position_id.clone(),
            symbol: symbol.clone(),
            side,
            size_usdt,
            quantity,
            entry_price,
            current_price: entry_price,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            max_profit: 0.0,
            max_loss: 0.0,
            stop_loss,
            take_profit,
            risk_score: 0.0,
            entry_time: now_micros(),
            target_exit_time: now_micros() + (self.config.position_hold_time_seconds * 1_000_000),
            last_update: now_micros(),
            entry_confidence: prediction.confidence,
            predicted_trend: prediction.trend_class,
            kelly_fraction,
        };
        
        // Update allocations
        *symbol_allocations.entry(symbol.clone()).or_insert(0.0) += size_usdt;
        
        // Update portfolio metrics
        metrics.allocated_capital += size_usdt;
        metrics.available_capital = metrics.total_capital - metrics.allocated_capital;
        metrics.active_positions = positions.len() + 1;
        metrics.capital_efficiency = metrics.allocated_capital / metrics.total_capital;
        metrics.last_update = now_micros();
        
        // Store position
        positions.insert(position_id.clone(), position);
        
        info!(
            "Opened position {}: {} {} {:.2} USDT @ {:.2} (Kelly: {:.3}, Confidence: {:.3})",
            position_id,
            symbol,
            match side { Side::Bid => "LONG", Side::Ask => "SHORT" },
            size_usdt,
            entry_price,
            kelly_fraction,
            prediction.confidence
        );
        
        Ok(position_id)
    }
    
    /// Update all positions with current market prices
    pub async fn update_positions(&self, price_updates: HashMap<String, f64>) -> Result<()> {
        let mut positions = self.positions.write().await;
        let mut metrics = self.metrics.lock().await;
        let mut total_unrealized_pnl = 0.0;
        
        for (symbol, current_price) in price_updates {
            // Update all positions for this symbol
            for position in positions.values_mut() {
                if position.symbol == symbol {
                    position.current_price = current_price;
                    let pnl = position.calculate_pnl();
                    total_unrealized_pnl += pnl;
                }
            }
        }
        
        // Update portfolio metrics
        metrics.unrealized_pnl = total_unrealized_pnl;
        metrics.last_update = now_micros();
        
        Ok(())
    }
    
    /// Check and close positions that meet exit criteria
    pub async fn check_position_exits(&self) -> Result<Vec<String>> {
        let mut positions = self.positions.write().await;
        let mut closed_positions = Vec::new();
        let mut positions_to_close = Vec::new();
        
        // Identify positions to close
        for (position_id, position) in positions.iter() {
            let (should_close, reason) = position.should_close(&self.config);
            if should_close {
                positions_to_close.push((position_id.clone(), reason));
            }
        }
        
        // Close identified positions
        for (position_id, close_reason) in positions_to_close {
            if let Some(position) = positions.remove(&position_id) {
                self.close_position_internal(position, close_reason).await?;
                closed_positions.push(position_id);
            }
        }
        
        Ok(closed_positions)
    }
    
    /// Internal method to close a position
    async fn close_position_internal(
        &self,
        mut position: TestPosition,
        close_reason: String,
    ) -> Result<()> {
        let mut metrics = self.metrics.lock().await;
        let mut trade_history = self.trade_history.lock().await;
        let mut symbol_allocations = self.symbol_allocations.lock().await;
        
        // Calculate final P&L
        let realized_pnl = position.calculate_pnl();
        
        // Create closed position record
        let closed_position = ClosedPosition {
            position_id: position.id.clone(),
            symbol: position.symbol.clone(),
            side: position.side,
            size_usdt: position.size_usdt,
            entry_price: position.entry_price,
            exit_price: position.current_price,
            realized_pnl,
            hold_time_seconds: (now_micros() - position.entry_time) / 1_000_000,
            entry_confidence: position.entry_confidence,
            predicted_trend: position.predicted_trend,
            actual_trend: None, // Would be determined by price analysis
            close_reason,
            kelly_fraction: position.kelly_fraction,
            entry_time: position.entry_time,
            exit_time: now_micros(),
        };
        
        // Update symbol allocation
        *symbol_allocations.entry(position.symbol.clone()).or_insert(0.0) -= position.size_usdt;
        
        // Update portfolio metrics
        metrics.allocated_capital -= position.size_usdt;
        metrics.available_capital = metrics.total_capital - metrics.allocated_capital;
        metrics.active_positions -= 1;
        metrics.capital_efficiency = if metrics.total_capital > 0.0 {
            metrics.allocated_capital / metrics.total_capital
        } else {
            0.0
        };
        
        metrics.realized_pnl += realized_pnl;
        metrics.daily_pnl += realized_pnl;
        metrics.total_pnl += realized_pnl;
        metrics.total_trades += 1;
        
        if realized_pnl > 0.0 {
            metrics.winning_trades += 1;
        }
        
        // Update performance metrics
        self.update_performance_metrics(&mut metrics, &trade_history);
        
        // Store trade
        trade_history.push(closed_position.clone());
        
        info!(
            "Closed position {}: {} PnL: {:.2} USDT ({:.2}%) | Reason: {} | Hold: {}s",
            position.id,
            position.symbol,
            realized_pnl,
            (realized_pnl / position.size_usdt) * 100.0,
            closed_position.close_reason,
            closed_position.hold_time_seconds
        );
        
        Ok(())
    }
    
    /// Update performance metrics
    fn update_performance_metrics(
        &self,
        metrics: &mut TestPortfolioMetrics,
        trade_history: &[ClosedPosition],
    ) {
        if trade_history.is_empty() {
            return;
        }
        
        // Calculate win rate
        metrics.win_rate = (metrics.winning_trades as f64 / metrics.total_trades as f64) * 100.0;
        
        // Calculate average trade metrics
        metrics.avg_trade_pnl = trade_history.iter().map(|t| t.realized_pnl).sum::<f64>() / trade_history.len() as f64;
        
        let winning_trades: Vec<_> = trade_history.iter().filter(|t| t.realized_pnl > 0.0).collect();
        let losing_trades: Vec<_> = trade_history.iter().filter(|t| t.realized_pnl < 0.0).collect();
        
        if !winning_trades.is_empty() {
            metrics.avg_winning_trade = winning_trades.iter().map(|t| t.realized_pnl).sum::<f64>() / winning_trades.len() as f64;
        }
        
        if !losing_trades.is_empty() {
            metrics.avg_losing_trade = losing_trades.iter().map(|t| t.realized_pnl).sum::<f64>() / losing_trades.len() as f64;
        }
        
        // Calculate profit factor
        let gross_profit: f64 = winning_trades.iter().map(|t| t.realized_pnl).sum();
        let gross_loss: f64 = losing_trades.iter().map(|t| t.realized_pnl.abs()).sum();
        
        if gross_loss > 0.0 {
            metrics.profit_factor = gross_profit / gross_loss;
        }
        
        // Calculate average hold time
        metrics.avg_hold_time_seconds = trade_history.iter()
            .map(|t| t.hold_time_seconds as f64)
            .sum::<f64>() / trade_history.len() as f64;
        
        // Calculate average position size
        metrics.avg_position_size = trade_history.iter()
            .map(|t| t.size_usdt)
            .sum::<f64>() / trade_history.len() as f64;
        
        // Simple drawdown calculation
        let mut peak = 0.0;
        let mut max_drawdown = 0.0;
        let mut running_pnl = 0.0;
        
        for trade in trade_history {
            running_pnl += trade.realized_pnl;
            if running_pnl > peak {
                peak = running_pnl;
            }
            let drawdown = peak - running_pnl;
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }
        }
        
        metrics.max_drawdown = max_drawdown;
        metrics.current_drawdown = peak - running_pnl;
        
        // Simple Sharpe ratio (assuming risk-free rate = 0)
        if trade_history.len() > 1 {
            let returns: Vec<f64> = trade_history.iter().map(|t| t.realized_pnl / t.size_usdt).collect();
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter()
                .map(|&r| (r - mean_return).powi(2))
                .sum::<f64>() / (returns.len() - 1) as f64;
            let std_dev = variance.sqrt();
            
            if std_dev > 0.0 {
                metrics.sharpe_ratio = mean_return / std_dev;
            }
        }
    }
    
    /// Get current portfolio status
    pub async fn get_portfolio_status(&self) -> TestPortfolioStatus {
        let positions = self.positions.read().await;
        let metrics = self.metrics.lock().await;
        let symbol_allocations = self.symbol_allocations.lock().await;
        
        let active_positions: Vec<_> = positions.values().cloned().collect();
        
        TestPortfolioStatus {
            metrics: metrics.clone(),
            active_positions,
            symbol_allocations: symbol_allocations.clone(),
            performance_summary: self.generate_performance_summary(&metrics).await,
        }
    }
    
    /// Generate performance summary
    async fn generate_performance_summary(&self, metrics: &TestPortfolioMetrics) -> PerformanceSummary {
        let trade_history = self.trade_history.lock().await;
        
        PerformanceSummary {
            total_return_pct: (metrics.total_pnl / metrics.total_capital) * 100.0,
            daily_return_pct: (metrics.daily_pnl / metrics.total_capital) * 100.0,
            target_progress_pct: (metrics.daily_pnl / self.config.target_daily_profit) * 100.0,
            risk_utilization_pct: (metrics.allocated_capital / self.config.test_capital) * 100.0,
            drawdown_pct: (metrics.current_drawdown / metrics.total_capital) * 100.0,
            time_to_target_minutes: if metrics.daily_pnl > 0.0 && metrics.daily_pnl < self.config.target_daily_profit {
                let rate_per_hour = metrics.daily_pnl / ((now_micros() - metrics.session_start) as f64 / 3600.0 / 1_000_000.0);
                let remaining = self.config.target_daily_profit - metrics.daily_pnl;
                Some((remaining / rate_per_hour * 60.0) as u64)
            } else {
                None
            },
            recent_trade_count: trade_history.len().min(10),
            recent_win_rate: if trade_history.len() >= 10 {
                let recent_trades = &trade_history[trade_history.len()-10..];
                let wins = recent_trades.iter().filter(|t| t.realized_pnl > 0.0).count();
                (wins as f64 / 10.0) * 100.0
            } else {
                metrics.win_rate
            },
        }
    }
}

/// Position assessment result
#[derive(Debug, Clone)]
pub enum PositionAssessment {
    Approved {
        suggested_size: f64,
        kelly_fraction: f64,
        risk_score: f64,
        stop_loss: Option<f64>,
        take_profit: Option<f64>,
        max_hold_time_seconds: u64,
        confidence_requirement: f64,
    },
    Rejected {
        reason: String,
    },
}

/// Portfolio status snapshot
#[derive(Debug, Clone)]
pub struct TestPortfolioStatus {
    pub metrics: TestPortfolioMetrics,
    pub active_positions: Vec<TestPosition>,
    pub symbol_allocations: HashMap<String, f64>,
    pub performance_summary: PerformanceSummary,
}

/// Performance summary for quick assessment
#[derive(Debug, Clone)]
pub struct PerformanceSummary {
    pub total_return_pct: f64,
    pub daily_return_pct: f64,
    pub target_progress_pct: f64,
    pub risk_utilization_pct: f64,
    pub drawdown_pct: f64,
    pub time_to_target_minutes: Option<u64>,
    pub recent_trade_count: usize,
    pub recent_win_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::dl_trend_predictor::{TrendPrediction, TrendClass};
    
    #[tokio::test]
    async fn test_position_manager_creation() {
        let config = TestCapitalConfig::default();
        let manager = TestCapitalPositionManager::new(config);
        
        let status = manager.get_portfolio_status().await;
        assert_eq!(status.metrics.total_capital, 100.0);
        assert_eq!(status.metrics.available_capital, 100.0);
        assert_eq!(status.active_positions.len(), 0);
    }
    
    #[tokio::test]
    async fn test_position_assessment() {
        let config = TestCapitalConfig::default();
        let manager = TestCapitalPositionManager::new(config);
        
        let prediction = TrendPrediction {
            trend_class: TrendClass::WeakUp,
            class_probabilities: vec![0.1, 0.2, 0.3, 0.3, 0.1],
            confidence: 0.75,
            expected_change_bps: 8.0,
            inference_latency_us: 500,
            timestamp: now_micros(),
            sequence_length: 30,
        };
        
        let assessment = manager.assess_new_position(
            "BTCUSDT",
            &prediction,
            50000.0
        ).await.unwrap();
        
        match assessment {
            PositionAssessment::Approved { suggested_size, .. } => {
                assert!(suggested_size >= 2.0);
                assert!(suggested_size <= 15.0);
            },
            PositionAssessment::Rejected { .. } => {
                panic!("Expected position to be approved");
            }
        }
    }
    
    #[tokio::test]
    async fn test_position_lifecycle() {
        let config = TestCapitalConfig::default();
        let manager = TestCapitalPositionManager::new(config);
        
        let prediction = TrendPrediction {
            trend_class: TrendClass::WeakUp,
            class_probabilities: vec![0.1, 0.2, 0.3, 0.3, 0.1],
            confidence: 0.8,
            expected_change_bps: 10.0,
            inference_latency_us: 400,
            timestamp: now_micros(),
            sequence_length: 30,
        };
        
        // Open position
        let position_id = manager.open_position(
            "BTCUSDT".to_string(),
            Side::Bid,
            10.0,
            50000.0,
            &prediction,
            0.1,
            Some(49500.0), // Stop loss
            Some(50500.0), // Take profit
        ).await.unwrap();
        
        // Check status
        let status = manager.get_portfolio_status().await;
        assert_eq!(status.active_positions.len(), 1);
        assert_eq!(status.metrics.allocated_capital, 10.0);
        assert_eq!(status.metrics.available_capital, 90.0);
        
        // Update price (profit scenario)
        let mut price_updates = HashMap::new();
        price_updates.insert("BTCUSDT".to_string(), 50600.0);
        manager.update_positions(price_updates).await.unwrap();
        
        // Check for exits (should trigger take profit)
        let closed_positions = manager.check_position_exits().await.unwrap();
        assert!(!closed_positions.is_empty());
        
        // Verify position closed
        let final_status = manager.get_portfolio_status().await;
        assert_eq!(final_status.active_positions.len(), 0);
        assert!(final_status.metrics.total_pnl > 0.0);
    }
}