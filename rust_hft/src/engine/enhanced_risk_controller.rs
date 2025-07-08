/*!
 * Enhanced Risk Controller for 400U LOB DL+RL Trading System
 * 
 * Features:
 * - Kelly Formula position sizing
 * - Multi-timeframe VaR calculation  
 * - 400U capital-specific risk management
 * - Daily loss limits with automatic recovery
 * - Integration with DL trend predictor confidence
 * - Real-time portfolio risk monitoring
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::ml::dl_trend_predictor::{TrendPrediction, TrendClass};
use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};
use rust_decimal::prelude::ToPrimitive;

/// Enhanced risk controller configuration for 400U system
#[derive(Debug, Clone)]
pub struct EnhancedRiskConfig {
    /// Capital allocation
    pub total_capital: f64,           // 400.0 USDT
    pub max_risk_capital: f64,        // 200.0 USDT (50% risk limit)
    pub daily_loss_limit: f64,        // 40.0 USDT (10% daily limit)
    pub emergency_stop_loss: f64,     // 60.0 USDT (15% emergency stop)
    
    /// Position sizing
    pub max_single_position_pct: f64, // 5% (20 USDT) per symbol
    pub max_total_exposure_pct: f64,  // 50% (200 USDT) total exposure
    pub kelly_fraction_limit: f64,    // 0.25 (Kelly × 0.25 for safety)
    
    /// Risk metrics
    pub var_confidence_levels: Vec<f64>, // [0.95, 0.99] for 95% and 99% VaR
    pub var_time_horizons: Vec<u64>,     // [3600, 86400] for 1h and 24h VaR
    pub sharpe_ratio_threshold: f64,     // 1.0 minimum Sharpe ratio
    
    /// Trading controls
    pub min_prediction_confidence: f64,  // 0.6 minimum DL confidence
    pub max_correlation_threshold: f64,  // 0.8 max correlation between positions
    pub cooling_period_minutes: u64,    // 30 minutes after stop loss
    
    /// Recovery mechanisms
    pub enable_position_scaling: bool,   // true: scale down positions under stress
    pub enable_automatic_recovery: bool, // true: enable auto-recovery after losses
    pub recovery_confidence_boost: f64,  // 0.1 additional confidence needed during recovery
}

impl Default for EnhancedRiskConfig {
    fn default() -> Self {
        Self {
            total_capital: 400.0,
            max_risk_capital: 200.0,
            daily_loss_limit: 40.0,
            emergency_stop_loss: 60.0,
            max_single_position_pct: 0.05,
            max_total_exposure_pct: 0.5,
            kelly_fraction_limit: 0.25,
            var_confidence_levels: vec![0.95, 0.99],
            var_time_horizons: vec![3600, 86400], // 1h, 24h in seconds
            sharpe_ratio_threshold: 1.0,
            min_prediction_confidence: 0.6,
            max_correlation_threshold: 0.8,
            cooling_period_minutes: 30,
            enable_position_scaling: true,
            enable_automatic_recovery: true,
            recovery_confidence_boost: 0.1,
        }
    }
}

/// Position risk information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionRisk {
    pub symbol: String,
    pub current_size: f64,          // Current position size in USDT
    pub max_allowed_size: f64,      // Maximum allowed position size
    pub current_pnl: f64,           // Current unrealized PnL
    pub daily_pnl: f64,             // Today's realized PnL
    pub var_1h: f64,                // 1-hour Value at Risk
    pub var_24h: f64,               // 24-hour Value at Risk
    pub kelly_suggested_size: f64,   // Kelly formula suggested size
    pub risk_score: f64,            // Overall risk score (0-1)
    pub correlation_risk: f64,      // Correlation risk with other positions
    pub last_update: Timestamp,
}

/// Portfolio risk metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioRiskMetrics {
    /// Capital utilization
    pub total_capital: f64,
    pub used_capital: f64,
    pub available_capital: f64,
    pub total_exposure: f64,
    
    /// PnL tracking
    pub daily_pnl: f64,
    pub weekly_pnl: f64,
    pub monthly_pnl: f64,
    pub total_pnl: f64,
    
    /// Risk measures
    pub portfolio_var_95: f64,      // 95% VaR
    pub portfolio_var_99: f64,      // 99% VaR
    pub expected_shortfall: f64,    // Conditional VaR
    pub max_drawdown: f64,
    pub current_drawdown: f64,
    
    /// Performance metrics
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    pub calmar_ratio: f64,
    pub win_rate: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    
    /// Risk status
    pub risk_level: RiskLevel,
    pub is_emergency_stop: bool,
    pub is_cooling_down: bool,
    pub last_stop_time: Option<Timestamp>,
}

/// Risk assessment result
#[derive(Debug, Clone)]
pub enum RiskAssessment {
    Approved {
        suggested_size: f64,
        max_size: f64,
        confidence_requirement: f64,
    },
    Rejected {
        reason: String,
        current_risk_level: RiskLevel,
    },
    Warning {
        message: String,
        suggested_size: f64,
        reduced_from: f64,
    },
}

/// Risk level enumeration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,        // < 25% risk capital used
    Moderate,   // 25-50% risk capital used
    High,       // 50-75% risk capital used
    Critical,   // 75-90% risk capital used
    Emergency,  // > 90% risk capital used
}

/// Enhanced risk controller
pub struct EnhancedRiskController {
    /// Configuration
    config: EnhancedRiskConfig,
    
    /// Position tracking
    positions: HashMap<String, PositionRisk>,
    
    /// Portfolio metrics
    portfolio_metrics: PortfolioRiskMetrics,
    
    /// Historical data for risk calculations
    price_history: HashMap<String, VecDeque<f64>>,   // Price history by symbol
    pnl_history: VecDeque<(Timestamp, f64)>,         // PnL history with timestamps
    trade_history: VecDeque<TradeRecord>,            // Trade history
    
    /// Kelly formula parameters
    kelly_params: HashMap<String, KellyParameters>,
    
    /// Correlation matrix
    correlation_matrix: HashMap<(String, String), f64>,
    
    /// Risk state
    last_risk_update: Timestamp,
    emergency_stop_triggered: bool,
    cooling_down_until: Option<Timestamp>,
    
    /// Statistics
    stats: EnhancedRiskStats,
}

/// Kelly formula parameters for each symbol
#[derive(Debug, Clone)]
pub struct KellyParameters {
    pub symbol: String,
    pub win_rate: f64,              // Historical win rate
    pub avg_win: f64,               // Average winning return
    pub avg_loss: f64,              // Average losing return  
    pub volatility: f64,            // Return volatility
    pub kelly_fraction: f64,        // Calculated Kelly fraction
    pub confidence_adjustment: f64,  // Adjustment based on prediction confidence
    pub last_update: Timestamp,
}

/// Trade record for Kelly calculation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    pub symbol: String,
    pub side: Side,
    pub size: f64,
    pub entry_price: f64,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub confidence: f64,            // ML prediction confidence
    pub actual_outcome: Option<TrendClass>, // Actual market outcome
    pub timestamp: Timestamp,
    pub duration_seconds: Option<u64>,
}

/// Enhanced risk controller statistics
#[derive(Debug, Clone, Default)]
pub struct EnhancedRiskStats {
    pub trades_analyzed: u64,
    pub trades_approved: u64,
    pub trades_rejected: u64,
    pub trades_reduced: u64,
    pub emergency_stops: u64,
    pub avg_kelly_fraction: f64,
    pub avg_risk_score: f64,
    pub portfolio_updates: u64,
    pub last_portfolio_update: Timestamp,
}

impl EnhancedRiskController {
    /// Create new enhanced risk controller
    pub fn new(config: EnhancedRiskConfig) -> Self {
        let portfolio_metrics = PortfolioRiskMetrics {
            total_capital: config.total_capital,
            used_capital: 0.0,
            available_capital: config.total_capital,
            total_exposure: 0.0,
            daily_pnl: 0.0,
            weekly_pnl: 0.0,
            monthly_pnl: 0.0,
            total_pnl: 0.0,
            portfolio_var_95: 0.0,
            portfolio_var_99: 0.0,
            expected_shortfall: 0.0,
            max_drawdown: 0.0,
            current_drawdown: 0.0,
            sharpe_ratio: 0.0,
            sortino_ratio: 0.0,
            calmar_ratio: 0.0,
            win_rate: 0.0,
            avg_win: 0.0,
            avg_loss: 0.0,
            risk_level: RiskLevel::Low,
            is_emergency_stop: false,
            is_cooling_down: false,
            last_stop_time: None,
        };
        
        Self {
            config,
            positions: HashMap::new(),
            portfolio_metrics,
            price_history: HashMap::new(),
            pnl_history: VecDeque::with_capacity(10000),
            trade_history: VecDeque::with_capacity(10000),
            kelly_params: HashMap::new(),
            correlation_matrix: HashMap::new(),
            last_risk_update: now_micros(),
            emergency_stop_triggered: false,
            cooling_down_until: None,
            stats: EnhancedRiskStats::default(),
        }
    }
    
    /// Assess risk for a new trading signal with DL prediction
    pub fn assess_trading_risk(
        &mut self,
        symbol: &str,
        signal: &TradingSignal,
        prediction: &TrendPrediction,
        current_price: f64,
    ) -> Result<RiskAssessment> {
        let start_time = now_micros();
        self.stats.trades_analyzed += 1;
        
        // 1. Check if we're in emergency stop or cooling down
        if self.is_emergency_stopped() {
            self.stats.trades_rejected += 1;
            return Ok(RiskAssessment::Rejected {
                reason: "Emergency stop is active".to_string(),
                current_risk_level: RiskLevel::Emergency,
            });
        }
        
        if self.is_cooling_down() {
            self.stats.trades_rejected += 1;
            return Ok(RiskAssessment::Rejected {
                reason: "System is in cooling down period".to_string(),
                current_risk_level: self.portfolio_metrics.risk_level.clone(),
            });
        }
        
        // 2. Check minimum prediction confidence
        let required_confidence = self.get_required_confidence(symbol);
        if prediction.confidence < required_confidence {
            self.stats.trades_rejected += 1;
            return Ok(RiskAssessment::Rejected {
                reason: format!(
                    "Prediction confidence too low: {:.3} < {:.3}",
                    prediction.confidence, required_confidence
                ),
                current_risk_level: self.portfolio_metrics.risk_level.clone(),
            });
        }
        
        // 3. Check daily loss limits
        if self.portfolio_metrics.daily_pnl <= -self.config.daily_loss_limit {
            self.stats.trades_rejected += 1;
            return Ok(RiskAssessment::Rejected {
                reason: format!(
                    "Daily loss limit exceeded: {:.2} <= -{:.2}",
                    self.portfolio_metrics.daily_pnl, self.config.daily_loss_limit
                ),
                current_risk_level: self.portfolio_metrics.risk_level.clone(),
            });
        }
        
        // 4. Calculate Kelly optimal position size
        let kelly_size = self.calculate_kelly_position_size(
            symbol, 
            prediction, 
            current_price
        )?;
        
        // 5. Apply risk constraints
        let max_position_size = self.config.total_capital * self.config.max_single_position_pct;
        let available_capital = self.calculate_available_capital(symbol);
        
        let suggested_size = kelly_size
            .min(max_position_size)
            .min(available_capital);
        
        // 6. Check if position size is meaningful
        if suggested_size < 1.0 { // Minimum 1 USDT position
            self.stats.trades_rejected += 1;
            return Ok(RiskAssessment::Rejected {
                reason: "Position size too small after risk constraints".to_string(),
                current_risk_level: self.portfolio_metrics.risk_level.clone(),
            });
        }
        
        // 7. Calculate risk score
        let risk_score = self.calculate_position_risk_score(
            symbol, 
            suggested_size, 
            prediction, 
            current_price
        );
        
        // 8. Check correlation risk
        let correlation_risk = self.calculate_correlation_risk(symbol, suggested_size);
        if correlation_risk > self.config.max_correlation_threshold {
            let reduced_size = suggested_size * (1.0 - correlation_risk + self.config.max_correlation_threshold);
            self.stats.trades_reduced += 1;
            return Ok(RiskAssessment::Warning {
                message: format!(
                    "Position size reduced due to correlation risk: {:.3} > {:.3}",
                    correlation_risk, self.config.max_correlation_threshold
                ),
                suggested_size: reduced_size,
                reduced_from: suggested_size,
            });
        }
        
        // 9. Final risk level check
        let risk_level = self.assess_risk_level_with_new_position(symbol, suggested_size);
        
        if risk_level == RiskLevel::Emergency {
            self.stats.trades_rejected += 1;
            return Ok(RiskAssessment::Rejected {
                reason: "New position would create emergency risk level".to_string(),
                current_risk_level: risk_level,
            });
        }
        
        // 10. Success - approve the trade
        self.stats.trades_approved += 1;
        
        let assessment_latency = now_micros() - start_time;
        debug!(
            "Risk assessment for {}: approved {:.2} USDT (Kelly: {:.2}, confidence: {:.3}, latency: {}μs)",
            symbol, suggested_size, kelly_size, prediction.confidence, assessment_latency
        );
        
        Ok(RiskAssessment::Approved {
            suggested_size,
            max_size: max_position_size,
            confidence_requirement: required_confidence,
        })
    }
    
    /// Calculate Kelly optimal position size with dynamic calibration
    fn calculate_kelly_position_size(
        &mut self,
        symbol: &str,
        prediction: &TrendPrediction,
        current_price: f64,
    ) -> Result<f64> {
        // Get or create Kelly parameters for this symbol
        if !self.kelly_params.contains_key(symbol) {
            self.initialize_kelly_params(symbol);
        }
        
        // Update Kelly parameters with recent trades first
        let symbol_clone = symbol.to_string();
        self.update_kelly_params_for_symbol(&symbol_clone);
        
        // ENHANCED: Dynamic volatility-adjusted Kelly calculation
        let market_volatility = self.calculate_market_volatility(symbol);
        let volatility_adjustment = self.get_volatility_adjustment(market_volatility);
        
        // Calculate Kelly fraction based on prediction and market conditions
        let win_probability = prediction.confidence;
        let _expected_return = prediction.expected_change_bps / 10000.0;
        
        // Dynamic adjustments for BTCUSDT market conditions
        let time_of_day_adj = self.get_time_of_day_adjustment();
        let liquidity_adj = self.get_liquidity_adjustment(current_price);
        let correlation_adj = self.get_correlation_adjustment(symbol);
        
        // Get current Kelly parameters (immutable access first)
        let (avg_win, avg_loss) = {
            let kelly_params = self.kelly_params.get(symbol).unwrap();
            (kelly_params.avg_win.max(0.001), kelly_params.avg_loss.abs().max(0.001))
        };
        
        // Classic Kelly: f = (bp - q) / b, enhanced with volatility
        let base_kelly = if avg_loss > 0.0 {
            (win_probability * avg_win - (1.0 - win_probability) * avg_loss) / avg_win
        } else {
            0.0
        };
        
        // Apply dynamic scaling factors
        let dynamic_kelly = base_kelly 
            * volatility_adjustment
            * time_of_day_adj
            * liquidity_adj 
            * correlation_adj
            * (1.0 - self.portfolio_metrics.current_drawdown * 2.0); // Reduce during drawdown
        
        // Confidence-based adjustment with non-linear scaling
        let confidence_boost = if prediction.confidence > 0.8 {
            1.0 + (prediction.confidence - 0.8) * 1.5 // Stronger boost for high confidence
        } else if prediction.confidence < 0.6 {
            0.5 + prediction.confidence * 0.83 // More conservative for low confidence
        } else {
            prediction.confidence
        };
        
        let adjusted_kelly = (dynamic_kelly * confidence_boost)
            .max(0.0)
            .min(self.config.kelly_fraction_limit);
        
        // BTCUSDT-specific optimizations based on market microstructure
        let btc_optimized_kelly = self.apply_btc_specific_optimizations(
            adjusted_kelly, 
            prediction, 
            current_price
        );
        
        // Calculate position size in USDT
        let kelly_position_size = self.config.total_capital * btc_optimized_kelly;
        
        // Now get mutable access to update the parameters
        let kelly_params = self.kelly_params.get_mut(symbol).unwrap();
        kelly_params.kelly_fraction = btc_optimized_kelly;
        kelly_params.confidence_adjustment = confidence_boost;
        kelly_params.last_update = now_micros();
        
        debug!(
            "Enhanced Kelly for {}: base={:.4}, dynamic={:.4}, final={:.4}, size={:.2} USDT (vol_adj={:.3}, conf={:.3})",
            symbol, base_kelly, dynamic_kelly, btc_optimized_kelly, kelly_position_size, 
            volatility_adjustment, prediction.confidence
        );
        
        Ok(kelly_position_size)
    }
    
    /// Initialize Kelly parameters for a new symbol
    fn initialize_kelly_params(&mut self, symbol: &str) {
        let kelly_params = KellyParameters {
            symbol: symbol.to_string(),
            win_rate: 0.5,              // Start with neutral assumption
            avg_win: 0.02,              // 2% average win
            avg_loss: 0.015,            // 1.5% average loss
            volatility: 0.02,           // 2% volatility
            kelly_fraction: 0.0,
            confidence_adjustment: 0.0,
            last_update: now_micros(),
        };
        
        self.kelly_params.insert(symbol.to_string(), kelly_params);
        info!("Initialized Kelly parameters for {}", symbol);
    }
    
    /// Update Kelly parameters for a specific symbol
    fn update_kelly_params_for_symbol(&mut self, symbol: &str) {
        let recent_trades: Vec<_> = self.trade_history
            .iter()
            .filter(|trade| &trade.symbol == symbol && trade.pnl.is_some())
            .rev()
            .take(100)
            .collect();
        
        if recent_trades.len() < 5 {
            return; // Need at least 5 trades for meaningful statistics
        }
        
        // Calculate statistics
        let winning_trades = recent_trades.iter().filter(|trade| {
            trade.pnl.unwrap_or(0.0) > 0.0
        }).count();
        let win_rate = winning_trades as f64 / recent_trades.len() as f64;
        
        let wins: Vec<f64> = recent_trades.iter()
            .filter_map(|trade| {
                let pnl = trade.pnl.unwrap_or(0.0);
                if pnl > 0.0 { Some(pnl / trade.size) } else { None }
            })
            .collect();
        
        let losses: Vec<f64> = recent_trades.iter()
            .filter_map(|trade| {
                let pnl = trade.pnl.unwrap_or(0.0);
                if pnl < 0.0 { Some(pnl.abs() / trade.size) } else { None }
            })
            .collect();
        
        let avg_win = if !wins.is_empty() {
            wins.iter().sum::<f64>() / wins.len() as f64
        } else {
            0.02 // Default 2%
        };
        
        let avg_loss = if !losses.is_empty() {
            losses.iter().sum::<f64>() / losses.len() as f64
        } else {
            0.015 // Default 1.5%
        };
        
        // Calculate volatility
        let returns: Vec<f64> = recent_trades.iter()
            .filter_map(|trade| {
                trade.pnl.map(|pnl| pnl / trade.size)
            })
            .collect();
        
        let volatility = if returns.len() > 1 {
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter()
                .map(|&r| (r - mean_return).powi(2))
                .sum::<f64>() / (returns.len() - 1) as f64;
            variance.sqrt()
        } else {
            0.02 // Default 2%
        };
        
        // Update the Kelly parameters
        if let Some(kelly_params) = self.kelly_params.get_mut(symbol) {
            kelly_params.win_rate = win_rate;
            kelly_params.avg_win = avg_win;
            kelly_params.avg_loss = avg_loss;
            kelly_params.volatility = volatility;
            kelly_params.last_update = now_micros();
            
            debug!(
                "Updated Kelly params for {}: win_rate={:.3}, avg_win={:.4}, avg_loss={:.4}, volatility={:.4}",
                symbol, win_rate, avg_win, avg_loss, volatility
            );
        }
    }
    
    /// Update Kelly parameters based on recent trading history (deprecated - use update_kelly_params_for_symbol)
    fn _update_kelly_params(&mut self, kelly_params: &mut KellyParameters) {
        let symbol = &kelly_params.symbol;
        
        // Get recent trades for this symbol (last 100 trades)
        let recent_trades: Vec<_> = self.trade_history
            .iter()
            .filter(|trade| &trade.symbol == symbol && trade.pnl.is_some())
            .rev()
            .take(100)
            .collect();
        
        if recent_trades.len() < 5 {
            return; // Need at least 5 trades for meaningful statistics
        }
        
        // Calculate win rate
        let winning_trades = recent_trades.iter().filter(|trade| {
            trade.pnl.unwrap_or(0.0) > 0.0
        }).count();
        kelly_params.win_rate = winning_trades as f64 / recent_trades.len() as f64;
        
        // Calculate average wins and losses
        let wins: Vec<f64> = recent_trades.iter()
            .filter_map(|trade| {
                let pnl = trade.pnl.unwrap_or(0.0);
                if pnl > 0.0 { Some(pnl / trade.size) } else { None }
            })
            .collect();
        
        let losses: Vec<f64> = recent_trades.iter()
            .filter_map(|trade| {
                let pnl = trade.pnl.unwrap_or(0.0);
                if pnl < 0.0 { Some(pnl.abs() / trade.size) } else { None }
            })
            .collect();
        
        if !wins.is_empty() {
            kelly_params.avg_win = wins.iter().sum::<f64>() / wins.len() as f64;
        }
        
        if !losses.is_empty() {
            kelly_params.avg_loss = losses.iter().sum::<f64>() / losses.len() as f64;
        }
        
        // Calculate volatility
        let returns: Vec<f64> = recent_trades.iter()
            .filter_map(|trade| {
                trade.pnl.map(|pnl| pnl / trade.size)
            })
            .collect();
        
        if returns.len() > 1 {
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter()
                .map(|&r| (r - mean_return).powi(2))
                .sum::<f64>() / (returns.len() - 1) as f64;
            kelly_params.volatility = variance.sqrt();
        }
        
        debug!(
            "Updated Kelly params for {}: win_rate={:.3}, avg_win={:.4}, avg_loss={:.4}, volatility={:.4}",
            symbol, kelly_params.win_rate, kelly_params.avg_win, kelly_params.avg_loss, kelly_params.volatility
        );
    }
    
    /// Calculate available capital for new positions
    fn calculate_available_capital(&self, symbol: &str) -> f64 {
        let current_exposure = self.positions.values()
            .map(|pos| pos.current_size.abs())
            .sum::<f64>();
        
        let max_total_exposure = self.config.total_capital * self.config.max_total_exposure_pct;
        let available_for_new_positions = max_total_exposure - current_exposure;
        
        // Also consider symbol-specific limits
        let current_symbol_size = self.positions.get(symbol)
            .map(|pos| pos.current_size.abs())
            .unwrap_or(0.0);
        
        let max_symbol_size = self.config.total_capital * self.config.max_single_position_pct;
        let available_for_symbol = max_symbol_size - current_symbol_size;
        
        available_for_new_positions.min(available_for_symbol).max(0.0)
    }
    
    /// Calculate risk score for a position with enhanced VaR modeling
    fn calculate_position_risk_score(
        &self,
        symbol: &str,
        position_size: f64,
        prediction: &TrendPrediction,
        current_price: f64,
    ) -> f64 {
        let mut risk_score = 0.0;
        
        // 1. Confidence risk (lower confidence = higher risk)
        let confidence_risk = (1.0 - prediction.confidence) * 0.25;
        risk_score += confidence_risk;
        
        // 2. Position size risk (larger position = higher risk)
        let max_position = self.config.total_capital * self.config.max_single_position_pct;
        let size_risk = (position_size / max_position) * 0.2;
        risk_score += size_risk;
        
        // 3. Enhanced volatility risk with fat-tail adjustment
        let enhanced_vol_risk = self.calculate_enhanced_volatility_risk(symbol, position_size);
        risk_score += enhanced_vol_risk * 0.2;
        
        // 4. Portfolio concentration risk
        let total_exposure = self.portfolio_metrics.total_exposure;
        let concentration_risk = if total_exposure > 0.0 {
            (position_size / (total_exposure + position_size)) * 0.15
        } else {
            0.0
        };
        risk_score += concentration_risk;
        
        // 5. Market condition risk (based on current drawdown)
        let market_risk = self.portfolio_metrics.current_drawdown * 0.1;
        risk_score += market_risk;
        
        // 6. BTCUSDT-specific extreme event risk
        let extreme_event_risk = self.calculate_extreme_event_risk(symbol, current_price);
        risk_score += extreme_event_risk * 0.1;
        
        risk_score.min(1.0)
    }
    
    /// Calculate enhanced volatility risk with fat-tail distribution modeling
    fn calculate_enhanced_volatility_risk(&self, symbol: &str, position_size: f64) -> f64 {
        if let Some(price_history) = self.price_history.get(symbol) {
            if price_history.len() < 30 {
                return 0.5; // Default medium risk
            }
            
            // Calculate returns
            let returns: Vec<f64> = price_history
                .iter()
                .zip(price_history.iter().skip(1))
                .map(|(prev, curr)| curr / prev - 1.0)
                .collect();
            
            if returns.is_empty() {
                return 0.5;
            }
            
            // Standard volatility
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter()
                .map(|&r| (r - mean_return).powi(2))
                .sum::<f64>() / (returns.len() - 1) as f64;
            let volatility = variance.sqrt();
            
            // Fat-tail analysis for crypto markets
            let fat_tail_factor = self.calculate_fat_tail_factor(&returns, mean_return, volatility);
            
            // Position-dependent risk scaling
            let position_ratio = position_size / self.config.total_capital;
            let position_scaling = if position_ratio > 0.1 {
                1.0 + (position_ratio - 0.1) * 2.0 // Non-linear scaling for large positions
            } else {
                1.0
            };
            
            (volatility * fat_tail_factor * position_scaling).min(1.0)
        } else {
            0.5
        }
    }
    
    /// Calculate fat-tail factor for crypto distributions
    fn calculate_fat_tail_factor(&self, returns: &[f64], mean: f64, std_dev: f64) -> f64 {
        if returns.len() < 20 || std_dev == 0.0 {
            return 1.2; // Default fat-tail adjustment for crypto
        }
        
        // Calculate excess kurtosis (measure of fat tails)
        let fourth_moment = returns.iter()
            .map(|&r| ((r - mean) / std_dev).powi(4))
            .sum::<f64>() / returns.len() as f64;
        
        let excess_kurtosis = fourth_moment - 3.0; // Normal distribution has kurtosis of 3
        
        // Count extreme events (beyond 2 standard deviations)
        let extreme_events = returns.iter()
            .filter(|&&r| (r - mean).abs() > 2.0 * std_dev)
            .count() as f64 / returns.len() as f64;
        
        // Normal distribution should have ~5% beyond 2 std devs
        let extreme_ratio = extreme_events / 0.05;
        
        // Combine kurtosis and extreme event frequency
        let fat_tail_factor = 1.0 + (excess_kurtosis.max(0.0) * 0.1) + (extreme_ratio - 1.0).max(0.0) * 0.2;
        
        fat_tail_factor.max(1.0).min(2.0) // Cap between 1.0 and 2.0
    }
    
    /// Calculate extreme event risk for BTCUSDT
    fn calculate_extreme_event_risk(&self, symbol: &str, current_price: f64) -> f64 {
        let mut extreme_risk = 0.0;
        
        // Check for price levels with historical volatility spikes
        let price_ranges = [
            (45000.0, 55000.0, 0.1),  // High activity range
            (28000.0, 35000.0, 0.2),  // Previous support/resistance
            (65000.0, 70000.0, 0.3),  // All-time high region
        ];
        
        for (low, high, risk_factor) in price_ranges {
            if current_price >= low && current_price <= high {
                extreme_risk += risk_factor;
            }
        }
        
        // Time-based extreme event risk (major news events, options expiry, etc.)
        let time_risk = self.get_time_based_extreme_risk();
        extreme_risk += time_risk;
        
        extreme_risk.min(1.0)
    }
    
    /// Get time-based extreme event risk
    fn get_time_based_extreme_risk(&self) -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let day_of_week = (now.as_secs() / 86400 + 4) % 7; // 0 = Thursday
        let hour_of_day = (now.as_secs() / 3600) % 24;
        
        let mut time_risk = 0.0;
        
        // Higher risk during:
        // 1. Friday evenings (weekend volatility)
        if day_of_week == 4 && hour_of_day >= 20 { // Friday evening
            time_risk += 0.1;
        }
        
        // 2. Sunday evenings (market reopening)
        if day_of_week == 6 && hour_of_day >= 20 { // Sunday evening
            time_risk += 0.15;
        }
        
        // 3. Major economic announcement times (simplified)
        if (hour_of_day == 14 || hour_of_day == 16) && (day_of_week >= 1 && day_of_week <= 5) {
            time_risk += 0.05;
        }
        
        time_risk
    }
    
    /// Calculate dynamic VaR with multiple confidence levels
    pub fn calculate_dynamic_var(&self, symbol: &str, position_size: f64, confidence_levels: &[f64]) -> Vec<(f64, f64)> {
        let mut var_results = Vec::new();
        
        if let Some(price_history) = self.price_history.get(symbol) {
            if price_history.len() < 50 {
                // Return default VaR estimates
                for &confidence in confidence_levels {
                    let default_var = position_size * 0.05 * (2.0 - confidence); // Simple estimate
                    var_results.push((confidence, default_var));
                }
                return var_results;
            }
            
            // Calculate returns
            let returns: Vec<f64> = price_history
                .iter()
                .zip(price_history.iter().skip(1))
                .map(|(prev, curr)| curr / prev - 1.0)
                .collect();
            
            // Sort returns for percentile calculation
            let mut sorted_returns = returns.clone();
            sorted_returns.sort_by(|a, b| a.partial_cmp(b).unwrap());
            
            // Apply volatility clustering adjustment (GARCH-like)
            let vol_clustering_factor = self.estimate_volatility_clustering(&returns);
            
            for &confidence in confidence_levels {
                let percentile_index = ((1.0 - confidence) * sorted_returns.len() as f64) as usize;
                let percentile_index = percentile_index.min(sorted_returns.len() - 1);
                
                let raw_var = -sorted_returns[percentile_index] * position_size;
                
                // Apply adjustments for crypto-specific factors
                let fat_tail_adjustment = self.calculate_fat_tail_factor(&returns, 
                    returns.iter().sum::<f64>() / returns.len() as f64,
                    returns.iter().map(|&r| r.powi(2)).sum::<f64>().sqrt() / (returns.len() as f64).sqrt()
                );
                
                let adjusted_var = raw_var * fat_tail_adjustment * vol_clustering_factor;
                var_results.push((confidence, adjusted_var));
            }
        } else {
            // Fallback to default estimates
            for &confidence in confidence_levels {
                let default_var = position_size * 0.05 * (2.0 - confidence);
                var_results.push((confidence, default_var));
            }
        }
        
        var_results
    }
    
    /// Estimate volatility clustering factor for GARCH-like adjustment
    fn estimate_volatility_clustering(&self, returns: &[f64]) -> f64 {
        if returns.len() < 10 {
            return 1.1; // Default adjustment for crypto
        }
        
        // Calculate rolling volatility to detect clustering
        let window_size = 10.min(returns.len() / 2);
        let mut vol_ratios = Vec::new();
        
        for i in window_size..returns.len() {
            let recent_vol = self.calculate_window_volatility(&returns[i-window_size..i]);
            let historical_vol = self.calculate_window_volatility(&returns[0..i-window_size]);
            
            if historical_vol > 0.0 {
                vol_ratios.push(recent_vol / historical_vol);
            }
        }
        
        if vol_ratios.is_empty() {
            return 1.1;
        }
        
        // If recent volatility is higher than historical, increase VaR
        let avg_vol_ratio = vol_ratios.iter().sum::<f64>() / vol_ratios.len() as f64;
        
        (1.0 + (avg_vol_ratio - 1.0) * 0.5).max(0.8).min(2.0)
    }
    
    /// Calculate volatility for a window of returns
    fn calculate_window_volatility(&self, returns: &[f64]) -> f64 {
        if returns.len() < 2 {
            return 0.0;
        }
        
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance = returns.iter()
            .map(|&r| (r - mean).powi(2))
            .sum::<f64>() / (returns.len() - 1) as f64;
            
        variance.sqrt()
    }
    
    /// Calculate correlation risk with existing positions
    fn calculate_correlation_risk(&self, symbol: &str, _position_size: f64) -> f64 {
        if self.positions.is_empty() {
            return 0.0;
        }
        
        let mut max_correlation: f64 = 0.0;
        let mut weighted_correlation = 0.0;
        let mut total_weight = 0.0;
        
        for (existing_symbol, position) in &self.positions {
            if existing_symbol == symbol {
                continue;
            }
            
            // Get correlation from matrix (simplified to 0.5 for crypto pairs)
            let correlation = self.correlation_matrix
                .get(&(symbol.to_string(), existing_symbol.clone()))
                .or_else(|| self.correlation_matrix.get(&(existing_symbol.clone(), symbol.to_string())))
                .copied()
                .unwrap_or(0.5); // Default correlation for crypto pairs
            
            max_correlation = max_correlation.max(correlation);
            
            let weight = position.current_size.abs();
            weighted_correlation += correlation * weight;
            total_weight += weight;
        }
        
        if total_weight > 0.0 {
            weighted_correlation /= total_weight;
        }
        
        // Consider both maximum correlation and weighted correlation
        (max_correlation * 0.7 + weighted_correlation * 0.3).min(1.0)
    }
    
    /// Assess risk level if a new position is added
    fn assess_risk_level_with_new_position(&self, symbol: &str, position_size: f64) -> RiskLevel {
        let new_exposure = self.portfolio_metrics.total_exposure + position_size;
        let risk_capital_usage = new_exposure / self.config.max_risk_capital;
        
        if risk_capital_usage > 0.9 {
            RiskLevel::Emergency
        } else if risk_capital_usage > 0.75 {
            RiskLevel::Critical
        } else if risk_capital_usage > 0.5 {
            RiskLevel::High
        } else if risk_capital_usage > 0.25 {
            RiskLevel::Moderate
        } else {
            RiskLevel::Low
        }
    }
    
    /// Get required confidence based on current risk state
    fn get_required_confidence(&self, symbol: &str) -> f64 {
        let mut required_confidence = self.config.min_prediction_confidence;
        
        // Increase confidence requirement during high risk periods
        match self.portfolio_metrics.risk_level {
            RiskLevel::High => required_confidence += 0.1,
            RiskLevel::Critical => required_confidence += 0.2,
            RiskLevel::Emergency => required_confidence += 0.3,
            _ => {}
        }
        
        // Increase confidence requirement during recovery
        if self.config.enable_automatic_recovery && self.portfolio_metrics.daily_pnl < 0.0 {
            required_confidence += self.config.recovery_confidence_boost;
        }
        
        // Increase confidence requirement for highly correlated positions
        let correlation_risk = self.calculate_correlation_risk(symbol, 0.0);
        if correlation_risk > 0.6 {
            required_confidence += 0.05;
        }
        
        required_confidence.min(0.95) // Cap at 95%
    }
    
    /// Check if emergency stop is active
    fn is_emergency_stopped(&self) -> bool {
        self.emergency_stop_triggered || 
        self.portfolio_metrics.is_emergency_stop ||
        self.portfolio_metrics.daily_pnl <= -self.config.emergency_stop_loss
    }
    
    /// Check if system is cooling down
    fn is_cooling_down(&self) -> bool {
        if let Some(cooling_until) = self.cooling_down_until {
            now_micros() < cooling_until
        } else {
            false
        }
    }
    
    /// Update position after trade execution
    pub fn update_position(&mut self, symbol: &str, size_change: f64, price: f64) {
        let current_timestamp = now_micros();
        let max_allowed_size = self.config.total_capital * self.config.max_single_position_pct;
        
        // Update position and capture new size for logging
        let new_size = {
            let position = self.positions.entry(symbol.to_string()).or_insert_with(|| {
                PositionRisk {
                    symbol: symbol.to_string(),
                    current_size: 0.0,
                    max_allowed_size,
                    current_pnl: 0.0,
                    daily_pnl: 0.0,
                    var_1h: 0.0,
                    var_24h: 0.0,
                    kelly_suggested_size: 0.0,
                    risk_score: 0.0,
                    correlation_risk: 0.0,
                    last_update: current_timestamp,
                }
            });
            
            position.current_size += size_change;
            position.last_update = current_timestamp;
            position.current_size
        };
        
        // Update portfolio metrics after position update
        self.update_portfolio_metrics();
        
        info!(
            "Position updated for {}: size_change={:.2}, new_size={:.2}, price={:.2}",
            symbol, size_change, new_size, price
        );
    }
    
    /// Update portfolio metrics
    fn update_portfolio_metrics(&mut self) {
        let total_exposure = self.positions.values()
            .map(|pos| pos.current_size.abs())
            .sum::<f64>();
        
        self.portfolio_metrics.total_exposure = total_exposure;
        self.portfolio_metrics.used_capital = total_exposure;
        self.portfolio_metrics.available_capital = self.config.total_capital - total_exposure;
        
        // Update risk level
        let risk_capital_usage = total_exposure / self.config.max_risk_capital;
        self.portfolio_metrics.risk_level = if risk_capital_usage > 0.9 {
            RiskLevel::Emergency
        } else if risk_capital_usage > 0.75 {
            RiskLevel::Critical
        } else if risk_capital_usage > 0.5 {
            RiskLevel::High
        } else if risk_capital_usage > 0.25 {
            RiskLevel::Moderate
        } else {
            RiskLevel::Low
        };
        
        self.stats.portfolio_updates += 1;
        self.stats.last_portfolio_update = now_micros();
    }
    
    /// Record completed trade for Kelly parameter updates
    pub fn record_trade(
        &mut self,
        symbol: &str,
        side: Side,
        size: f64,
        entry_price: f64,
        exit_price: f64,
        confidence: f64,
    ) {
        let pnl = match side {
            Side::Bid => (exit_price - entry_price) * size / entry_price,
            Side::Ask => (entry_price - exit_price) * size / entry_price,
        };
        
        let trade = TradeRecord {
            symbol: symbol.to_string(),
            side,
            size,
            entry_price,
            exit_price: Some(exit_price),
            pnl: Some(pnl),
            confidence,
            actual_outcome: None, // Will be set by trend classifier if available
            timestamp: now_micros(),
            duration_seconds: None, // Can be calculated if needed
        };
        
        self.trade_history.push_back(trade);
        
        // Keep only recent trades
        if self.trade_history.len() > 10000 {
            self.trade_history.pop_front();
        }
        
        // Update daily PnL
        self.portfolio_metrics.daily_pnl += pnl * size;
        self.portfolio_metrics.total_pnl += pnl * size;
        
        debug!(
            "Trade recorded for {}: side={:?}, pnl={:.2} ({:.2}%), confidence={:.3}",
            symbol, side, pnl * size, pnl * 100.0, confidence
        );
    }
    
    /// Get current portfolio metrics
    pub fn get_portfolio_metrics(&self) -> &PortfolioRiskMetrics {
        &self.portfolio_metrics
    }
    
    /// Get position risk information
    pub fn get_position_risk(&self, symbol: &str) -> Option<&PositionRisk> {
        self.positions.get(symbol)
    }
    
    /// Get all positions
    pub fn get_all_positions(&self) -> &HashMap<String, PositionRisk> {
        &self.positions
    }
    
    /// Get Kelly parameters for a symbol
    pub fn get_kelly_params(&self, symbol: &str) -> Option<&KellyParameters> {
        self.kelly_params.get(symbol)
    }
    
    /// Get controller statistics
    pub fn get_stats(&self) -> &EnhancedRiskStats {
        &self.stats
    }
    
    /// Trigger emergency stop
    pub fn trigger_emergency_stop(&mut self, reason: &str) {
        self.emergency_stop_triggered = true;
        self.portfolio_metrics.is_emergency_stop = true;
        self.portfolio_metrics.last_stop_time = Some(now_micros());
        self.stats.emergency_stops += 1;
        
        // Set cooling down period
        let cooling_period_us = self.config.cooling_period_minutes * 60 * 1_000_000;
        self.cooling_down_until = Some(now_micros() + cooling_period_us);
        
        error!("Emergency stop triggered: {}", reason);
    }
    
    /// Reset emergency stop (manual recovery)
    pub fn reset_emergency_stop(&mut self) {
        self.emergency_stop_triggered = false;
        self.portfolio_metrics.is_emergency_stop = false;
        self.cooling_down_until = None;
        
        info!("Emergency stop reset - trading resumed");
    }
    
    /// Reset daily statistics
    pub fn reset_daily_stats(&mut self) {
        self.portfolio_metrics.daily_pnl = 0.0;
        
        for position in self.positions.values_mut() {
            position.daily_pnl = 0.0;
        }
        
        info!("Daily risk statistics reset");
    }
    
    /// Calculate market volatility for Kelly adjustment
    fn calculate_market_volatility(&self, symbol: &str) -> f64 {
        if let Some(price_history) = self.price_history.get(symbol) {
            if price_history.len() < 2 {
                return 0.02; // Default 2% volatility
            }
            
            let returns: Vec<f64> = price_history
                .iter()
                .zip(price_history.iter().skip(1))
                .map(|(prev, curr)| (curr / prev - 1.0).abs())
                .collect();
                
            if returns.is_empty() {
                return 0.02;
            }
            
            let mean_return = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance = returns.iter()
                .map(|&r| (r - mean_return).powi(2))
                .sum::<f64>() / returns.len() as f64;
                
            variance.sqrt() * (288.0_f64).sqrt() // Scale to daily volatility (assuming 5-minute data)
        } else {
            0.02
        }
    }
    
    /// Get volatility adjustment factor for Kelly sizing
    fn get_volatility_adjustment(&self, volatility: f64) -> f64 {
        // Reduce Kelly fraction during high volatility periods
        // BTCUSDT typical volatility: 1-5% daily
        match volatility {
            vol if vol > 0.05 => 0.5,  // High volatility: reduce to 50%
            vol if vol > 0.03 => 0.7,  // Medium-high: reduce to 70%
            vol if vol > 0.02 => 0.85, // Medium: slight reduction
            vol if vol > 0.01 => 1.0,  // Normal: no adjustment
            _ => 1.1 // Low volatility: slight increase
        }
    }
    
    /// Get time-of-day adjustment for Kelly sizing
    fn get_time_of_day_adjustment(&self) -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let hour_of_day = (now.as_secs() / 3600) % 24;
        
        // BTCUSDT liquidity and volatility patterns
        match hour_of_day {
            0..=3 => 0.8,   // Asian night: reduced liquidity
            4..=7 => 1.0,   // Asian morning: normal
            8..=11 => 1.2,  // European morning: increased activity
            12..=15 => 1.3, // European afternoon + US pre-market: peak activity
            16..=19 => 1.4, // US trading hours: highest activity
            20..=23 => 1.0, // US evening: normal
            _ => 1.0
        }
    }
    
    /// Get liquidity adjustment based on current price level
    fn get_liquidity_adjustment(&self, current_price: f64) -> f64 {
        // BTCUSDT typically has good liquidity, but adjust for round numbers
        let price_mod_1000 = current_price % 1000.0;
        let price_mod_100 = current_price % 100.0;
        
        // Reduce size near psychological levels (round thousands/hundreds)
        if price_mod_1000 < 50.0 || price_mod_1000 > 950.0 {
            0.9 // 10% reduction near round thousands
        } else if price_mod_100 < 10.0 || price_mod_100 > 90.0 {
            0.95 // 5% reduction near round hundreds
        } else {
            1.0
        }
    }
    
    /// Get correlation adjustment to avoid over-concentration
    fn get_correlation_adjustment(&self, symbol: &str) -> f64 {
        if self.positions.len() <= 1 {
            return 1.0; // No adjustment for single position
        }
        
        let mut total_exposure = 0.0;
        let mut correlated_exposure = 0.0;
        
        for (existing_symbol, position) in &self.positions {
            total_exposure += position.current_size.abs();
            
            if existing_symbol != symbol {
                // For crypto, assume moderate correlation (0.6) with other crypto pairs
                let correlation = 0.6;
                correlated_exposure += position.current_size.abs() * correlation;
            }
        }
        
        if total_exposure > 0.0 {
            let correlation_ratio = correlated_exposure / total_exposure;
            // Reduce Kelly fraction if high correlation
            (1.0 - correlation_ratio * 0.5).max(0.5)
        } else {
            1.0
        }
    }
    
    /// Apply BTCUSDT-specific optimizations
    fn apply_btc_specific_optimizations(
        &self,
        kelly_fraction: f64,
        prediction: &TrendPrediction,
        current_price: f64,
    ) -> f64 {
        let mut optimized_kelly = kelly_fraction;
        
        // BTCUSDT-specific adjustments
        
        // 1. Trend strength adjustment
        let trend_strength = prediction.expected_change_bps.abs() / 100.0; // Normalize to percentage
        if trend_strength > 0.5 {
            optimized_kelly *= 1.1; // Boost for strong trends
        } else if trend_strength < 0.1 {
            optimized_kelly *= 0.8; // Reduce for weak trends
        }
        
        // 2. Price level adjustment (BTC psychological levels)
        if current_price % 5000.0 < 500.0 || current_price % 5000.0 > 4500.0 {
            optimized_kelly *= 0.9; // Reduce near major round numbers
        }
        
        // 3. Prediction horizon adjustment
        if prediction.sequence_length > 50 {
            optimized_kelly *= 1.05; // Slight boost for longer sequences
        }
        
        // 4. Inference latency penalty
        if prediction.inference_latency_us > 1000 {
            optimized_kelly *= 0.95; // Reduce for slow predictions
        }
        
        // 5. Multi-timeframe consistency boost (if available)
        // This would require additional prediction data
        
        optimized_kelly.max(0.0).min(0.3) // Cap at 30% for safety
    }
    
    /// Update price history for volatility calculation
    pub fn update_price_history(&mut self, symbol: &str, price: f64) {
        let history = self.price_history.entry(symbol.to_string())
            .or_insert_with(|| VecDeque::with_capacity(288)); // 1 day of 5-minute data
            
        history.push_back(price);
        
        // Keep only recent data
        if history.len() > 288 {
            history.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::dl_trend_predictor::{TrendClass, TrendPrediction};
    
    #[test]
    fn test_enhanced_risk_controller_creation() {
        let config = EnhancedRiskConfig::default();
        let controller = EnhancedRiskController::new(config);
        
        assert_eq!(controller.portfolio_metrics.total_capital, 400.0);
        assert_eq!(controller.portfolio_metrics.risk_level, RiskLevel::Low);
        assert!(!controller.is_emergency_stopped());
    }
    
    #[test]
    fn test_kelly_position_sizing() {
        let config = EnhancedRiskConfig::default();
        let mut controller = EnhancedRiskController::new(config);
        
        let prediction = TrendPrediction {
            trend_class: TrendClass::WeakUp,
            class_probabilities: vec![0.1, 0.2, 0.3, 0.3, 0.1],
            confidence: 0.7,
            expected_change_bps: 5.0,
            inference_latency_us: 500,
            timestamp: now_micros(),
            sequence_length: 30,
        };
        
        let kelly_size = controller.calculate_kelly_position_size(
            "BTCUSDT",
            &prediction,
            50000.0
        ).unwrap();
        
        assert!(kelly_size > 0.0);
        assert!(kelly_size <= 100.0); // Should be reasonable for 400U capital
    }
    
    #[test]
    fn test_risk_assessment() {
        let config = EnhancedRiskConfig::default();
        let mut controller = EnhancedRiskController::new(config);
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 50000.0.to_price(),
            suggested_quantity: 0.001.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 100,
        };
        
        let prediction = TrendPrediction {
            trend_class: TrendClass::WeakUp,
            class_probabilities: vec![0.1, 0.2, 0.3, 0.3, 0.1],
            confidence: 0.8,
            expected_change_bps: 5.0,
            inference_latency_us: 500,
            timestamp: now_micros(),
            sequence_length: 30,
        };
        
        let assessment = controller.assess_trading_risk(
            "BTCUSDT",
            &signal,
            &prediction,
            50000.0
        ).unwrap();
        
        match assessment {
            RiskAssessment::Approved { suggested_size, .. } => {
                assert!(suggested_size > 0.0);
                assert!(suggested_size <= 20.0); // Max 5% of 400U
            }
            _ => panic!("Expected approved assessment"),
        }
    }
    
    #[test]
    fn test_emergency_stop() {
        let config = EnhancedRiskConfig::default();
        let mut controller = EnhancedRiskController::new(config);
        
        assert!(!controller.is_emergency_stopped());
        
        controller.trigger_emergency_stop("Test emergency");
        
        assert!(controller.is_emergency_stopped());
        assert!(controller.is_cooling_down());
        
        controller.reset_emergency_stop();
        
        assert!(!controller.is_emergency_stopped());
    }
}