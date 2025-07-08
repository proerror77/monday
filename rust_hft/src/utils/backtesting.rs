/*!
 * Comprehensive Backtesting Framework for HFT Strategies
 * 
 * High-performance historical data validation and strategy testing
 * Supports multiple ML models, realistic market simulation, and performance analysis
 */

use crate::types::*;
use crate::types::FeatureSet;
use anyhow::Result;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use tracing::{info, warn, debug};
use serde::{Serialize, Deserialize};

/// Backtesting configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    pub start_time: Timestamp,
    pub end_time: Timestamp,
    pub initial_balance: f64,
    pub commission_rate: f64,
    pub slippage_bps: f64,
    pub max_position_size: f64,
    pub max_daily_trades: usize,
    pub benchmark_symbol: String,
    pub risk_free_rate: f64,
    pub output_path: String,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            start_time: 0,
            end_time: now_micros(),
            initial_balance: 100000.0,
            commission_rate: 0.001, // 0.1%
            slippage_bps: 2.0,      // 0.02%
            max_position_size: 10000.0,
            max_daily_trades: 1000,
            benchmark_symbol: "BTCUSDT".to_string(),
            risk_free_rate: 0.02,   // 2% annual
            output_path: "backtest_results".to_string(),
        }
    }
}

/// Market order simulation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedOrder {
    pub order_id: String,
    pub timestamp: Timestamp,
    pub symbol: String,
    pub side: Side,
    pub quantity: f64,
    pub price: f64,
    pub fill_price: f64,
    pub commission: f64,
    pub slippage: f64,
    pub status: OrderStatus,
}

/// Portfolio position tracking
#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: String,
    pub quantity: f64,
    pub avg_price: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub last_update: Timestamp,
}

impl Position {
    pub fn new(symbol: String) -> Self {
        Self {
            symbol,
            quantity: 0.0,
            avg_price: 0.0,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            last_update: now_micros(),
        }
    }
    
    pub fn update_position(&mut self, quantity: f64, price: f64, current_price: f64) {
        if self.quantity == 0.0 {
            // Opening new position
            self.quantity = quantity;
            self.avg_price = price;
        } else if (self.quantity > 0.0) == (quantity > 0.0) {
            // Adding to existing position
            let total_value = self.quantity * self.avg_price + quantity * price;
            self.quantity += quantity;
            self.avg_price = total_value / self.quantity;
        } else {
            // Closing or reducing position
            let closed_quantity = quantity.abs().min(self.quantity.abs());
            self.realized_pnl += closed_quantity * (price - self.avg_price) * self.quantity.signum();
            self.quantity += quantity;
            
            if self.quantity.abs() < 1e-8 {
                self.quantity = 0.0;
                self.avg_price = 0.0;
            }
        }
        
        // Update unrealized PnL
        if self.quantity != 0.0 {
            self.unrealized_pnl = self.quantity * (current_price - self.avg_price);
        } else {
            self.unrealized_pnl = 0.0;
        }
        
        self.last_update = now_micros();
    }
    
    pub fn total_pnl(&self) -> f64 {
        self.realized_pnl + self.unrealized_pnl
    }
}

/// Portfolio manager for backtesting
#[derive(Debug)]
pub struct BacktestPortfolio {
    pub cash: f64,
    pub positions: HashMap<String, Position>,
    pub total_commission: f64,
    pub total_slippage: f64,
    pub trade_count: u64,
    pub max_drawdown: f64,
    pub peak_equity: f64,
    pub daily_returns: Vec<f64>,
    pub equity_curve: Vec<(Timestamp, f64)>,
}

impl BacktestPortfolio {
    pub fn new(initial_cash: f64) -> Self {
        Self {
            cash: initial_cash,
            positions: HashMap::new(),
            total_commission: 0.0,
            total_slippage: 0.0,
            trade_count: 0,
            max_drawdown: 0.0,
            peak_equity: initial_cash,
            daily_returns: Vec::new(),
            equity_curve: Vec::new(),
        }
    }
    
    pub fn execute_order(&mut self, order: &SimulatedOrder, current_price: f64) -> Result<()> {
        let symbol = &order.symbol;
        let position = self.positions.entry(symbol.clone()).or_insert_with(|| Position::new(symbol.clone()));
        
        // Calculate order value
        let order_value = order.quantity * order.fill_price;
        let total_cost = order_value + order.commission + order.slippage;
        
        // Check cash availability for buy orders
        if order.side == Side::Bid && self.cash < total_cost {
            warn!("Insufficient cash for order: {} required, {} available", total_cost, self.cash);
            return Ok(());
        }
        
        // Update position
        let quantity_signed = match order.side {
            Side::Bid => order.quantity,
            Side::Ask => -order.quantity,
        };
        
        position.update_position(quantity_signed, order.fill_price, current_price);
        
        // Update cash
        match order.side {
            Side::Bid => self.cash -= total_cost,
            Side::Ask => self.cash += order_value - order.commission - order.slippage,
        }
        
        // Update statistics
        self.total_commission += order.commission;
        self.total_slippage += order.slippage;
        self.trade_count += 1;
        
        debug!("Executed order: {} {} @ {}, cash: {}", 
               order.side, order.quantity, order.fill_price, self.cash);
        
        Ok(())
    }
    
    pub fn update_equity(&mut self, current_prices: &HashMap<String, f64>, timestamp: Timestamp) {
        // Update unrealized PnL for all positions
        for (symbol, position) in &mut self.positions {
            if let Some(&current_price) = current_prices.get(symbol) {
                if position.quantity != 0.0 {
                    position.unrealized_pnl = position.quantity * (current_price - position.avg_price);
                }
            }
        }
        
        let total_equity = self.total_equity();
        
        // Update drawdown
        if total_equity > self.peak_equity {
            self.peak_equity = total_equity;
        } else {
            let drawdown = (self.peak_equity - total_equity) / self.peak_equity;
            if drawdown > self.max_drawdown {
                self.max_drawdown = drawdown;
            }
        }
        
        // Record equity curve
        self.equity_curve.push((timestamp, total_equity));
    }
    
    pub fn total_equity(&self) -> f64 {
        let position_value: f64 = self.positions.values()
            .map(|p| p.realized_pnl + p.unrealized_pnl)
            .sum();
        
        self.cash + position_value
    }
    
    pub fn get_position(&self, symbol: &str) -> Option<&Position> {
        self.positions.get(symbol)
    }
}

/// Market simulator for realistic order execution
pub struct MarketSimulator {
    config: BacktestConfig,
    current_prices: HashMap<String, f64>,
    order_book_depth: HashMap<String, (f64, f64)>, // (bid_depth, ask_depth)
}

impl MarketSimulator {
    pub fn new(config: BacktestConfig) -> Self {
        Self {
            config,
            current_prices: HashMap::new(),
            order_book_depth: HashMap::new(),
        }
    }
    
    pub fn update_market_data(&mut self, features: &FeatureSet) {
        let symbol = self.config.benchmark_symbol.clone();
        self.current_prices.insert(symbol.clone(), features.mid_price.0);
        
        // Estimate depth from features
        let _total_depth = features.bid_depth_l5 + features.ask_depth_l5;
        self.order_book_depth.insert(symbol, (features.bid_depth_l5, features.ask_depth_l5));
    }
    
    pub fn simulate_order_execution(&self, signal: &TradingSignal) -> Result<SimulatedOrder> {
        let symbol = self.config.benchmark_symbol.clone();
        let order_id = format!("backtest_{}", now_micros());
        
        // Get current market price
        let current_price = self.current_prices.get(&symbol)
            .ok_or_else(|| anyhow::anyhow!("No price data for symbol: {}", symbol))?;
        
        // Calculate slippage based on order size and market depth
        let slippage_rate = self.calculate_slippage(signal, &symbol);
        
        let fill_price = match signal.signal_type {
            SignalType::Buy => current_price * (1.0 + slippage_rate),
            SignalType::Sell => current_price * (1.0 - slippage_rate),
            SignalType::Hold => *current_price,
        };
        
        let quantity = signal.suggested_quantity.to_string().parse::<f64>()
            .unwrap_or(0.0);
        
        let commission = quantity * fill_price * self.config.commission_rate;
        let slippage_cost = quantity * fill_price * slippage_rate;
        
        Ok(SimulatedOrder {
            order_id,
            timestamp: signal.timestamp,
            symbol,
            side: match signal.signal_type {
                SignalType::Buy => Side::Bid,
                SignalType::Sell => Side::Ask,
                SignalType::Hold => Side::Bid, // Shouldn't happen
            },
            quantity,
            price: signal.suggested_price.0,
            fill_price,
            commission,
            slippage: slippage_cost,
            status: OrderStatus::Filled,
        })
    }
    
    fn calculate_slippage(&self, signal: &TradingSignal, symbol: &str) -> f64 {
        let base_slippage = self.config.slippage_bps / 10000.0;
        
        // Adjust slippage based on order size and market depth
        if let Some((bid_depth, ask_depth)) = self.order_book_depth.get(symbol) {
            let quantity = signal.suggested_quantity.to_string().parse::<f64>().unwrap_or(0.0);
            let relevant_depth = match signal.signal_type {
                SignalType::Buy => *ask_depth,
                SignalType::Sell => *bid_depth,
                SignalType::Hold => (*bid_depth + *ask_depth) / 2.0,
            };
            
            if relevant_depth > 0.0 {
                let depth_impact = (quantity / relevant_depth).min(1.0);
                return base_slippage * (1.0 + depth_impact);
            }
        }
        
        base_slippage
    }
}

/// Comprehensive backtesting engine
pub struct BacktestEngine {
    config: BacktestConfig,
    portfolio: BacktestPortfolio,
    simulator: MarketSimulator,
    results: BacktestResults,
    trade_log: Vec<SimulatedOrder>,
}

impl BacktestEngine {
    pub fn new(config: BacktestConfig) -> Self {
        let portfolio = BacktestPortfolio::new(config.initial_balance);
        let simulator = MarketSimulator::new(config.clone());
        
        Self {
            config: config.clone(),
            portfolio,
            simulator,
            results: BacktestResults::new(config),
            trade_log: Vec::new(),
        }
    }
    
    /// Run backtest with feature data
    pub fn run_backtest(
        &mut self,
        features: &[FeatureSet],
        strategy: &mut dyn BacktestStrategy,
    ) -> Result<BacktestResults> {
        info!("Starting backtest with {} data points", features.len());
        
        let start_time = now_micros();
        let mut processed_count = 0;
        
        for feature_set in features.iter() {
            // Skip data outside time range
            if feature_set.timestamp < self.config.start_time || 
               feature_set.timestamp > self.config.end_time {
                continue;
            }
            
            // Update market data
            self.simulator.update_market_data(feature_set);
            
            // Generate trading signal
            if let Some(signal) = strategy.generate_signal(feature_set, &self.portfolio)? {
                // Check trading limits
                if self.portfolio.trade_count >= self.config.max_daily_trades as u64 {
                    debug!("Daily trade limit reached");
                    continue;
                }
                
                // Simulate order execution
                let simulated_order = self.simulator.simulate_order_execution(&signal)?;
                
                // Execute in portfolio
                let current_price = feature_set.mid_price.0;
                self.portfolio.execute_order(&simulated_order, current_price)?;
                
                // Log trade
                self.trade_log.push(simulated_order);
            }
            
            // Update portfolio equity
            let prices = std::iter::once((
                self.config.benchmark_symbol.clone(),
                feature_set.mid_price.0
            )).collect();
            
            self.portfolio.update_equity(&prices, feature_set.timestamp);
            
            processed_count += 1;
            
            if processed_count % 10000 == 0 {
                debug!("Processed {} data points", processed_count);
            }
        }
        
        let elapsed = now_micros() - start_time;
        info!("Backtest completed in {}ms, processed {} points", 
              elapsed / 1000, processed_count);
        
        // Calculate final results
        self.calculate_results()?;
        
        Ok(self.results.clone())
    }
    
    fn calculate_results(&mut self) -> Result<()> {
        let final_equity = self.portfolio.total_equity();
        let total_return = (final_equity - self.config.initial_balance) / self.config.initial_balance;
        
        // Calculate Sharpe ratio
        let sharpe_ratio = self.calculate_sharpe_ratio();
        
        // Calculate win rate
        let (win_rate, profit_factor) = self.calculate_win_metrics();
        
        self.results = BacktestResults {
            config: self.config.clone(),
            initial_balance: self.config.initial_balance,
            final_balance: final_equity,
            total_return,
            max_drawdown: self.portfolio.max_drawdown,
            sharpe_ratio,
            win_rate,
            profit_factor,
            total_trades: self.portfolio.trade_count,
            total_commission: self.portfolio.total_commission,
            total_slippage: self.portfolio.total_slippage,
            equity_curve: self.portfolio.equity_curve.clone(),
            trade_log: self.trade_log.clone(),
            summary_stats: self.calculate_summary_stats()?,
        };
        
        Ok(())
    }
    
    fn calculate_sharpe_ratio(&self) -> f64 {
        if self.portfolio.daily_returns.len() < 2 {
            return 0.0;
        }
        
        let mean_return: f64 = self.portfolio.daily_returns.iter().sum::<f64>() / self.portfolio.daily_returns.len() as f64;
        let variance: f64 = self.portfolio.daily_returns.iter()
            .map(|r| (r - mean_return).powi(2))
            .sum::<f64>() / (self.portfolio.daily_returns.len() - 1) as f64;
        
        let std_dev = variance.sqrt();
        
        if std_dev == 0.0 {
            0.0
        } else {
            let excess_return = mean_return - self.config.risk_free_rate / 365.0; // Daily risk-free rate
            excess_return / std_dev * (365.0_f64).sqrt() // Annualized
        }
    }
    
    fn calculate_win_metrics(&self) -> (f64, f64) {
        if self.trade_log.is_empty() {
            return (0.0, 0.0);
        }
        
        let mut wins = 0;
        let mut total_profit = 0.0;
        let mut total_loss = 0.0;
        
        // Simplified P&L calculation based on fill price vs entry
        for _trade in &self.trade_log {
            // This is a simplified calculation - in reality you'd need to track position P&L
            let pnl: f64 = 0.0; // Placeholder
            
            if pnl > 0.0 {
                wins += 1;
                total_profit += pnl;
            } else if pnl < 0.0 {
                total_loss += pnl.abs();
            }
        }
        
        let win_rate = wins as f64 / self.trade_log.len() as f64;
        let profit_factor = if total_loss > 0.0 {
            total_profit / total_loss
        } else {
            0.0
        };
        
        (win_rate, profit_factor)
    }
    
    fn calculate_summary_stats(&self) -> Result<SummaryStats> {
        let equity_values: Vec<f64> = self.portfolio.equity_curve.iter().map(|(_, equity)| *equity).collect();
        
        if equity_values.is_empty() {
            return Ok(SummaryStats::default());
        }
        
        let mean_equity = equity_values.iter().sum::<f64>() / equity_values.len() as f64;
        let min_equity = equity_values.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let max_equity = equity_values.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        
        Ok(SummaryStats {
            mean_equity,
            min_equity,
            max_equity,
            volatility: 0.0, // Calculate if needed
            calmar_ratio: if self.portfolio.max_drawdown > 0.0 {
                self.results.total_return / self.portfolio.max_drawdown
            } else {
                0.0
            },
        })
    }
    
    /// Export results to files
    pub fn export_results(&self, base_path: &str) -> Result<()> {
        std::fs::create_dir_all(base_path)?;
        
        // Export summary
        let summary_path = format!("{base_path}/summary.json");
        let summary_json = serde_json::to_string_pretty(&self.results)?;
        std::fs::write(summary_path, summary_json)?;
        
        // Export trade log
        let trades_path = format!("{base_path}/trades.csv");
        let mut trades_file = File::create(trades_path)?;
        
        writeln!(trades_file, "timestamp,symbol,side,quantity,price,fill_price,commission,slippage")?;
        for trade in &self.trade_log {
            writeln!(
                trades_file,
                "{},{},{},{},{},{},{},{}",
                trade.timestamp, trade.symbol, trade.side, trade.quantity,
                trade.price, trade.fill_price, trade.commission, trade.slippage
            )?;
        }
        
        // Export equity curve
        let equity_path = format!("{base_path}/equity_curve.csv");
        let mut equity_file = File::create(equity_path)?;
        
        writeln!(equity_file, "timestamp,equity")?;
        for (timestamp, equity) in &self.portfolio.equity_curve {
            writeln!(equity_file, "{timestamp},{equity}")?;
        }
        
        info!("Backtest results exported to: {}", base_path);
        Ok(())
    }
}

/// Strategy trait for backtesting
pub trait BacktestStrategy {
    fn generate_signal(&mut self, features: &FeatureSet, portfolio: &BacktestPortfolio) -> Result<Option<TradingSignal>>;
}

/// Simple buy-and-hold strategy for benchmarking
pub struct BuyAndHoldStrategy {
    position_opened: bool,
    order_size: f64,
}

impl BuyAndHoldStrategy {
    pub fn new(order_size: f64) -> Self {
        Self {
            position_opened: false,
            order_size,
        }
    }
}

impl BacktestStrategy for BuyAndHoldStrategy {
    fn generate_signal(&mut self, features: &FeatureSet, _portfolio: &BacktestPortfolio) -> Result<Option<TradingSignal>> {
        if !self.position_opened {
            self.position_opened = true;
            return Ok(Some(TradingSignal {
                signal_type: SignalType::Buy,
                confidence: 1.0,
                suggested_price: features.mid_price,
                suggested_quantity: self.order_size.to_quantity(),
                timestamp: features.timestamp,
                features_timestamp: features.timestamp,
                signal_latency_us: 0,
            }));
        }
        
        Ok(None)
    }
}

/// Backtesting results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    pub config: BacktestConfig,
    pub initial_balance: f64,
    pub final_balance: f64,
    pub total_return: f64,
    pub max_drawdown: f64,
    pub sharpe_ratio: f64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub total_trades: u64,
    pub total_commission: f64,
    pub total_slippage: f64,
    pub equity_curve: Vec<(Timestamp, f64)>,
    pub trade_log: Vec<SimulatedOrder>,
    pub summary_stats: SummaryStats,
}

impl BacktestResults {
    fn new(config: BacktestConfig) -> Self {
        Self {
            config,
            initial_balance: 0.0,
            final_balance: 0.0,
            total_return: 0.0,
            max_drawdown: 0.0,
            sharpe_ratio: 0.0,
            win_rate: 0.0,
            profit_factor: 0.0,
            total_trades: 0,
            total_commission: 0.0,
            total_slippage: 0.0,
            equity_curve: Vec::new(),
            trade_log: Vec::new(),
            summary_stats: SummaryStats::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SummaryStats {
    pub mean_equity: f64,
    pub min_equity: f64,
    pub max_equity: f64,
    pub volatility: f64,
    pub calmar_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml::model_training_simple::generate_synthetic_data;
    
    #[test]
    fn test_portfolio_operations() {
        let mut portfolio = BacktestPortfolio::new(100000.0);
        
        let order = SimulatedOrder {
            order_id: "test_1".to_string(),
            timestamp: now_micros(),
            symbol: "BTCUSDT".to_string(),
            side: Side::Bid,
            quantity: 1.0,
            price: 50000.0,
            fill_price: 50010.0,
            commission: 50.0,
            slippage: 10.0,
            status: OrderStatus::Filled,
        };
        
        portfolio.execute_order(&order, 50020.0).unwrap();
        
        assert!(portfolio.cash < 100000.0);
        assert_eq!(portfolio.trade_count, 1);
        
        let position = portfolio.get_position("BTCUSDT").unwrap();
        assert_eq!(position.quantity, 1.0);
        assert!(position.unrealized_pnl > 0.0); // Price went up
    }
    
    #[test]
    fn test_backtest_engine() {
        let config = BacktestConfig {
            initial_balance: 10000.0,
            start_time: 0,
            end_time: u64::MAX, // Accept all timestamps
            ..Default::default()
        };
        
        let mut engine = BacktestEngine::new(config);
        
        // Generate synthetic data
        let data = generate_synthetic_data(100, 10).unwrap();
        let mut strategy = BuyAndHoldStrategy::new(0.1);
        
        let results = engine.run_backtest(&data.features, &mut strategy).unwrap();
        
        assert!(results.total_trades >= 1);
        assert_ne!(results.final_balance, results.initial_balance);
    }
    
    #[test]
    fn test_market_simulator() {
        let config = BacktestConfig::default();
        let mut simulator = MarketSimulator::new(config);
        
        let mut features = FeatureSet::default_enhanced();
        features.timestamp = now_micros();
        features.latency_network_us = 10;
        features.latency_processing_us = 20;
        features.best_bid = 100.0.to_price();
        features.best_ask = 101.0.to_price();
        features.mid_price = 100.5.to_price();
        features.spread = 1.0;
        features.spread_bps = 100.0;
        features.obi_l1 = 0.1;
        features.obi_l5 = 0.05;
        features.obi_l10 = 0.02;
        features.obi_l20 = 0.01;
        features.bid_depth_l5 = 1000.0;
        features.ask_depth_l5 = 1100.0;
        features.bid_depth_l10 = 2000.0;
        features.ask_depth_l10 = 2200.0;
        features.bid_depth_l20 = 4000.0;
        features.ask_depth_l20 = 4400.0;
        features.depth_imbalance_l5 = -0.05;
        features.depth_imbalance_l10 = -0.05;
        features.depth_imbalance_l20 = -0.05;
        features.bid_slope = 0.1;
        features.ask_slope = 0.1;
        features.total_bid_levels = 10;
        features.total_ask_levels = 12;
        features.price_momentum = 0.001;
        features.volume_imbalance = 0.05;
        features.is_valid = true;
        features.data_quality_score = 0.95;
        
        simulator.update_market_data(&features);
        
        let signal = TradingSignal {
            signal_type: SignalType::Buy,
            confidence: 0.8,
            suggested_price: 100.0.to_price(),
            suggested_quantity: 1.0.to_quantity(),
            timestamp: now_micros(),
            features_timestamp: now_micros(),
            signal_latency_us: 50,
        };
        
        let order = simulator.simulate_order_execution(&signal).unwrap();
        assert_eq!(order.side, Side::Bid);
        assert!(order.commission > 0.0);
    }
}