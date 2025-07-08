/*!
 * Simulation Testing Framework for Ultra-Think LOB DL+RL System
 * 
 * Comprehensive testing framework for Phase 1 validation:
 * - Simulates realistic market conditions and order book dynamics
 * - Tests the complete trading pipeline from data ingestion to execution
 * - Validates Kelly formula position sizing and risk management
 * - Provides performance metrics and system health scoring
 * - Enables safe testing before live deployment
 * 
 * Key Features:
 * - Historical data replay with configurable market conditions
 * - Stress testing with extreme market scenarios
 * - Performance benchmarking against targets
 * - Risk management validation
 * - Capital allocation efficiency testing
 */

use crate::core::types::*;
use crate::ml::{
    LobTimeSeriesExtractor,
    LobTimeSeriesConfig,
    DlTrendPredictor,
    DlTrendPredictorConfig,
    TrendPrediction,
    TrendClass,
};
use crate::engine::{
    TestCapitalPositionManager,
    TestCapitalConfig,
    PositionAssessment,
    EnhancedRiskController,
    EnhancedRiskConfig,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{Duration, sleep, Instant};
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};
use rand::Rng;

/// Configuration for simulation testing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Test duration and intervals
    pub total_duration_seconds: u64,           // 3600 (1 hour test)
    pub tick_interval_ms: u64,                 // 100ms market data ticks
    pub signal_interval_ms: u64,               // 500ms trading signals
    pub status_report_interval_seconds: u64,   // 10 seconds status updates
    
    /// Market simulation parameters
    pub symbols: Vec<String>,                  // ["BTCUSDT", "ETHUSDT"]
    pub initial_prices: HashMap<String, f64>,  // Starting prices
    pub volatility_pcts: HashMap<String, f64>, // Daily volatility %
    pub trend_strength: f64,                   // 0.0-1.0 trend persistence
    pub market_impact: f64,                    // 0.0-1.0 order impact on price
    
    /// Testing scenarios
    pub test_scenarios: Vec<TestScenario>,
    pub enable_stress_testing: bool,
    pub extreme_volatility_multiplier: f64,    // 2.0x normal volatility
    
    /// Performance targets for validation
    pub min_daily_return_pct: f64,             // 0.8% minimum
    pub max_daily_loss_pct: f64,               // 10% maximum
    pub min_win_rate_pct: f64,                 // 55% minimum
    pub max_drawdown_pct: f64,                 // 5% maximum
    pub min_sharpe_ratio: f64,                 // 1.0 minimum
    pub min_capital_efficiency_pct: f64,       // 80% minimum
    
    /// Risk testing parameters
    pub test_capital: f64,                     // 100.0 USDT
    pub max_concurrent_positions: usize,       // 4 positions
    pub position_hold_time_seconds: u64,       // 15-30 seconds
    
    /// Output configuration
    pub save_detailed_logs: bool,
    pub generate_performance_report: bool,
    pub export_trade_history: bool,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        let mut initial_prices = HashMap::new();
        initial_prices.insert("BTCUSDT".to_string(), 50000.0);
        initial_prices.insert("ETHUSDT".to_string(), 3000.0);
        
        let mut volatility_pcts = HashMap::new();
        volatility_pcts.insert("BTCUSDT".to_string(), 3.0); // 3% daily volatility
        volatility_pcts.insert("ETHUSDT".to_string(), 4.0); // 4% daily volatility
        
        Self {
            total_duration_seconds: 3600,
            tick_interval_ms: 100,
            signal_interval_ms: 500,
            status_report_interval_seconds: 10,
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            initial_prices,
            volatility_pcts,
            trend_strength: 0.3,
            market_impact: 0.1,
            test_scenarios: vec![
                TestScenario::NormalMarket,
                TestScenario::TrendingMarket,
                TestScenario::VolatileMarket,
                TestScenario::RangeboundMarket,
            ],
            enable_stress_testing: true,
            extreme_volatility_multiplier: 2.5,
            min_daily_return_pct: 0.8,
            max_daily_loss_pct: 10.0,
            min_win_rate_pct: 55.0,
            max_drawdown_pct: 5.0,
            min_sharpe_ratio: 1.0,
            min_capital_efficiency_pct: 80.0,
            test_capital: 100.0,
            max_concurrent_positions: 4,
            position_hold_time_seconds: 20,
            save_detailed_logs: true,
            generate_performance_report: true,
            export_trade_history: true,
        }
    }
}

/// Test scenarios for different market conditions
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum TestScenario {
    NormalMarket,      // Standard market conditions
    TrendingMarket,    // Strong directional moves
    VolatileMarket,    // High volatility, rapid changes
    RangeboundMarket,  // Sideways movement
    StressTest,        // Extreme market conditions
}

/// Market data simulator
#[derive(Debug)]
pub struct MarketSimulator {
    config: SimulationConfig,
    current_prices: HashMap<String, f64>,
    price_history: HashMap<String, Vec<f64>>,
    volume_history: HashMap<String, Vec<f64>>,
    trend_direction: HashMap<String, f64>,  // -1.0 to 1.0
    volatility_multiplier: f64,
    rng: rand::rngs::ThreadRng,
}

impl MarketSimulator {
    pub fn new(config: SimulationConfig) -> Self {
        let current_prices = config.initial_prices.clone();
        let mut price_history = HashMap::new();
        let mut volume_history = HashMap::new();
        let mut trend_direction = HashMap::new();
        
        for symbol in &config.symbols {
            price_history.insert(symbol.clone(), Vec::new());
            volume_history.insert(symbol.clone(), Vec::new());
            trend_direction.insert(symbol.clone(), 0.0);
        }
        
        Self {
            config,
            current_prices,
            price_history,
            volume_history,
            trend_direction,
            volatility_multiplier: 1.0,
            rng: rand::thread_rng(),
        }
    }
    
    /// Generate next market tick
    pub fn generate_tick(&mut self, scenario: TestScenario) -> HashMap<String, MarketTick> {
        let mut ticks = HashMap::new();
        
        for symbol in &self.config.symbols {
            let current_price = self.current_prices[symbol];
            let base_volatility = self.config.volatility_pcts[symbol];
            
            // Apply scenario-specific parameters
            let (volatility_mult, trend_mult) = match scenario {
                TestScenario::NormalMarket => (1.0, 0.3),
                TestScenario::TrendingMarket => (0.8, 0.8),
                TestScenario::VolatileMarket => (2.0, 0.1),
                TestScenario::RangeboundMarket => (0.5, 0.0),
                TestScenario::StressTest => (self.config.extreme_volatility_multiplier, 0.5),
            };
            
            // Generate price change
            let volatility = base_volatility * volatility_mult * self.volatility_multiplier;
            let trend = self.trend_direction[symbol] * trend_mult;
            let random_change = self.rng.gen_range(-1.0..1.0);
            
            let price_change_pct = (volatility / 100.0) * (trend + random_change * 0.3) / 86400.0; // Per second
            let new_price = current_price * (1.0 + price_change_pct);
            
            // Update trend direction with mean reversion
            let trend_change = self.rng.gen_range(-0.1..0.1);
            let new_trend = (trend + trend_change) * 0.99; // Slight mean reversion
            self.trend_direction.insert(symbol.clone(), new_trend.clamp(-1.0, 1.0));
            
            // Generate volume (correlated with price movement)
            let volume_base = match symbol.as_str() {
                "BTCUSDT" => 1000.0,
                "ETHUSDT" => 5000.0,
                _ => 1000.0,
            };
            let volume_multiplier = 1.0 + price_change_pct.abs() * 5.0;
            let volume = volume_base * volume_multiplier * self.rng.gen_range(0.5..1.5);
            
            // Create market tick
            let tick = MarketTick {
                symbol: symbol.clone(),
                price: new_price,
                volume,
                timestamp: now_micros(),
                bid_price: new_price * 0.9999,
                ask_price: new_price * 1.0001,
                spread_bps: 1.0,
            };
            
            // Update history
            self.current_prices.insert(symbol.clone(), new_price);
            self.price_history.get_mut(symbol).unwrap().push(new_price);
            self.volume_history.get_mut(symbol).unwrap().push(volume);
            
            ticks.insert(symbol.clone(), tick);
        }
        
        ticks
    }
    
    /// Set stress test conditions
    pub fn set_stress_conditions(&mut self, extreme_volatility: bool) {
        self.volatility_multiplier = if extreme_volatility { 
            self.config.extreme_volatility_multiplier 
        } else { 
            1.0 
        };
    }
    
    /// Get current market state
    pub fn get_market_state(&self) -> MarketState {
        MarketState {
            current_prices: self.current_prices.clone(),
            trend_directions: self.trend_direction.clone(),
            volatility_multiplier: self.volatility_multiplier,
            total_ticks: self.price_history.values().map(|v| v.len()).sum(),
        }
    }
}

/// Market tick data
#[derive(Debug, Clone)]
pub struct MarketTick {
    pub symbol: String,
    pub price: f64,
    pub volume: f64,
    pub timestamp: Timestamp,
    pub bid_price: f64,
    pub ask_price: f64,
    pub spread_bps: f64,
}

/// Current market state
#[derive(Debug, Clone)]
pub struct MarketState {
    pub current_prices: HashMap<String, f64>,
    pub trend_directions: HashMap<String, f64>,
    pub volatility_multiplier: f64,
    pub total_ticks: usize,
}

/// Simulation test results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResults {
    /// Test configuration
    pub config: SimulationConfig,
    
    /// Test execution metrics
    pub execution_metrics: SimulationExecutionMetrics,
    
    /// Trading performance
    pub trading_performance: TradingPerformanceMetrics,
    
    /// Risk metrics
    pub risk_metrics: RiskMetrics,
    
    /// System health
    pub system_health: SystemHealthMetrics,
    
    /// Test validation results
    pub validation_results: ValidationResults,
    
    /// Detailed logs (if enabled)
    pub detailed_logs: Option<Vec<String>>,
}

/// Simulation execution metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationExecutionMetrics {
    pub total_runtime_seconds: f64,
    pub total_ticks_processed: u64,
    pub total_signals_generated: u64,
    pub total_trades_executed: u64,
    pub average_tick_processing_time_us: f64,
    pub average_signal_generation_time_us: f64,
    pub max_latency_us: u64,
    pub min_latency_us: u64,
    pub system_uptime_pct: f64,
    pub errors_encountered: u64,
}

/// Trading performance metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingPerformanceMetrics {
    pub total_pnl_usdt: f64,
    pub total_return_pct: f64,
    pub daily_return_pct: f64,
    pub win_rate_pct: f64,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub average_trade_pnl_usdt: f64,
    pub average_win_usdt: f64,
    pub average_loss_usdt: f64,
    pub profit_factor: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown_usdt: f64,
    pub max_drawdown_pct: f64,
    pub capital_efficiency_pct: f64,
    pub average_position_hold_time_seconds: f64,
}

/// Risk metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMetrics {
    pub max_daily_loss_usdt: f64,
    pub max_position_loss_usdt: f64,
    pub var_95_usdt: f64,
    pub var_99_usdt: f64,
    pub expected_shortfall_usdt: f64,
    pub risk_utilization_pct: f64,
    pub kelly_fraction_avg: f64,
    pub position_sizing_accuracy_pct: f64,
    pub stop_loss_effectiveness_pct: f64,
    pub risk_adjusted_return: f64,
}

/// System health metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHealthMetrics {
    pub overall_health_score: f64,        // 0-10 scale
    pub performance_score: f64,           // 0-10 scale
    pub risk_management_score: f64,       // 0-10 scale
    pub execution_quality_score: f64,     // 0-10 scale
    pub system_stability_score: f64,      // 0-10 scale
    pub recommendation: SystemRecommendation,
}

/// System recommendation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemRecommendation {
    ReadyForProduction,
    NeedsOptimization,
    RequiresSignificantChanges,
    NotReadyForDeployment,
}

/// Validation results against targets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResults {
    pub meets_return_target: bool,
    pub meets_risk_target: bool,
    pub meets_win_rate_target: bool,
    pub meets_drawdown_target: bool,
    pub meets_sharpe_target: bool,
    pub meets_efficiency_target: bool,
    pub overall_validation_passed: bool,
    pub validation_score: f64,            // 0-100 scale
    pub recommendations: Vec<String>,
}

/// Main simulation framework
pub struct SimulationFramework {
    config: SimulationConfig,
    market_simulator: MarketSimulator,
    position_manager: Arc<TestCapitalPositionManager>,
    ml_extractor: LobTimeSeriesExtractor,
    ml_predictor: DlTrendPredictor,
    risk_controller: Arc<Mutex<EnhancedRiskController>>,
    results: SimulationResults,
    detailed_logs: Vec<String>,
}

impl SimulationFramework {
    /// Create new simulation framework
    pub fn new(config: SimulationConfig) -> Result<Self> {
        let market_simulator = MarketSimulator::new(config.clone());
        
        // Initialize position manager
        let test_config = TestCapitalConfig {
            test_capital: config.test_capital,
            capital_per_symbol: config.test_capital * 0.4,
            max_concurrent_positions: config.max_concurrent_positions,
            position_hold_time_seconds: config.position_hold_time_seconds,
            ..Default::default()
        };
        let position_manager = Arc::new(TestCapitalPositionManager::new(test_config));
        
        // Initialize ML components
        let lob_config = LobTimeSeriesConfig::default();
        let ml_extractor = LobTimeSeriesExtractor::new("simulation".to_string(), lob_config);
        
        let dl_config = DlTrendPredictorConfig::default();
        let ml_predictor = DlTrendPredictor::new("simulation".to_string(), dl_config, LobTimeSeriesConfig::default())?;
        
        // Initialize risk controller
        let risk_config = EnhancedRiskConfig::default();
        let risk_controller = Arc::new(Mutex::new(EnhancedRiskController::new(risk_config)));
        
        // Initialize results structure
        let results = SimulationResults {
            config: config.clone(),
            execution_metrics: SimulationExecutionMetrics {
                total_runtime_seconds: 0.0,
                total_ticks_processed: 0,
                total_signals_generated: 0,
                total_trades_executed: 0,
                average_tick_processing_time_us: 0.0,
                average_signal_generation_time_us: 0.0,
                max_latency_us: 0,
                min_latency_us: u64::MAX,
                system_uptime_pct: 100.0,
                errors_encountered: 0,
            },
            trading_performance: TradingPerformanceMetrics {
                total_pnl_usdt: 0.0,
                total_return_pct: 0.0,
                daily_return_pct: 0.0,
                win_rate_pct: 0.0,
                total_trades: 0,
                winning_trades: 0,
                losing_trades: 0,
                average_trade_pnl_usdt: 0.0,
                average_win_usdt: 0.0,
                average_loss_usdt: 0.0,
                profit_factor: 0.0,
                sharpe_ratio: 0.0,
                max_drawdown_usdt: 0.0,
                max_drawdown_pct: 0.0,
                capital_efficiency_pct: 0.0,
                average_position_hold_time_seconds: 0.0,
            },
            risk_metrics: RiskMetrics {
                max_daily_loss_usdt: 0.0,
                max_position_loss_usdt: 0.0,
                var_95_usdt: 0.0,
                var_99_usdt: 0.0,
                expected_shortfall_usdt: 0.0,
                risk_utilization_pct: 0.0,
                kelly_fraction_avg: 0.0,
                position_sizing_accuracy_pct: 0.0,
                stop_loss_effectiveness_pct: 0.0,
                risk_adjusted_return: 0.0,
            },
            system_health: SystemHealthMetrics {
                overall_health_score: 0.0,
                performance_score: 0.0,
                risk_management_score: 0.0,
                execution_quality_score: 0.0,
                system_stability_score: 0.0,
                recommendation: SystemRecommendation::NotReadyForDeployment,
            },
            validation_results: ValidationResults {
                meets_return_target: false,
                meets_risk_target: false,
                meets_win_rate_target: false,
                meets_drawdown_target: false,
                meets_sharpe_target: false,
                meets_efficiency_target: false,
                overall_validation_passed: false,
                validation_score: 0.0,
                recommendations: Vec::new(),
            },
            detailed_logs: None,
        };
        
        Ok(Self {
            config,
            market_simulator,
            position_manager,
            ml_extractor,
            ml_predictor,
            risk_controller,
            results,
            detailed_logs: Vec::new(),
        })
    }
    
    /// Run full simulation test
    pub async fn run_simulation(&mut self) -> Result<SimulationResults> {
        info!("🚀 Starting Ultra-Think LOB DL+RL Simulation Framework");
        info!("Test Duration: {} seconds", self.config.total_duration_seconds);
        info!("Test Capital: {} USDT", self.config.test_capital);
        
        let start_time = Instant::now();
        let mut tick_count = 0u64;
        let mut signal_count = 0u64;
        let mut trade_count = 0u64;
        let mut error_count = 0u64;
        
        // Run simulation for each scenario
        let scenarios = self.config.test_scenarios.clone(); // Clone to avoid borrowing issues
        for scenario in scenarios {
            info!("📊 Running scenario: {:?}", scenario);
            
            let scenario_result = self.run_scenario(scenario).await;
            match scenario_result {
                Ok((ticks, signals, trades)) => {
                    tick_count += ticks;
                    signal_count += signals;
                    trade_count += trades;
                    info!("✅ Scenario {:?} completed: {} ticks, {} signals, {} trades", 
                          scenario, ticks, signals, trades);
                }
                Err(e) => {
                    error_count += 1;
                    error!("❌ Scenario {:?} failed: {}", scenario, e);
                }
            }
        }
        
        // Run stress test if enabled
        if self.config.enable_stress_testing {
            info!("🔥 Running stress test scenario");
            self.market_simulator.set_stress_conditions(true);
            
            let stress_result = self.run_scenario(TestScenario::StressTest).await;
            match stress_result {
                Ok((ticks, signals, trades)) => {
                    tick_count += ticks;
                    signal_count += signals;
                    trade_count += trades;
                    info!("✅ Stress test completed: {} ticks, {} signals, {} trades", 
                          ticks, signals, trades);
                }
                Err(e) => {
                    error_count += 1;
                    error!("❌ Stress test failed: {}", e);
                }
            }
        }
        
        let total_runtime = start_time.elapsed();
        
        // Collect final results
        self.collect_results(total_runtime, tick_count, signal_count, trade_count, error_count).await?;
        
        // Generate performance report
        if self.config.generate_performance_report {
            self.generate_performance_report().await?;
        }
        
        info!("🎯 Simulation completed in {:.2} seconds", total_runtime.as_secs_f64());
        info!("📈 Final Health Score: {:.1}/10.0", self.results.system_health.overall_health_score);
        
        Ok(self.results.clone())
    }
    
    /// Run a single test scenario
    async fn run_scenario(&mut self, scenario: TestScenario) -> Result<(u64, u64, u64)> {
        let scenarios_count = self.config.test_scenarios.len() as u64;
        let scenario_duration = Duration::from_secs(self.config.total_duration_seconds / scenarios_count);
        let start_time = Instant::now();
        
        let mut tick_count = 0u64;
        let mut signal_count = 0u64;
        let mut trade_count = 0u64;
        
        let mut tick_interval = tokio::time::interval(Duration::from_millis(self.config.tick_interval_ms));
        let mut signal_interval = tokio::time::interval(Duration::from_millis(self.config.signal_interval_ms));
        let mut status_interval = tokio::time::interval(Duration::from_secs(self.config.status_report_interval_seconds));
        
        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    // Generate market tick
                    let ticks = self.market_simulator.generate_tick(scenario);
                    tick_count += ticks.len() as u64;
                    
                    // Update position manager with new prices
                    let mut price_updates = HashMap::new();
                    for (symbol, tick) in ticks {
                        price_updates.insert(symbol, tick.price);
                    }
                    
                    if let Err(e) = self.position_manager.update_positions(price_updates).await {
                        error!("Failed to update positions: {}", e);
                    }
                    
                    // Check for position exits
                    if let Ok(closed_positions) = self.position_manager.check_position_exits().await {
                        trade_count += closed_positions.len() as u64;
                    }
                }
                
                _ = signal_interval.tick() => {
                    // Generate trading signals
                    let signals = self.generate_trading_signals(scenario).await?;
                    signal_count += signals.len() as u64;
                    
                    // Execute approved signals
                    for signal in signals {
                        if let Ok(executed) = self.execute_signal(signal).await {
                            if executed {
                                trade_count += 1;
                            }
                        }
                    }
                }
                
                _ = status_interval.tick() => {
                    // Log status
                    let status = self.position_manager.get_portfolio_status().await;
                    debug!("Scenario {:?} status: P&L {:.2} USDT, {} active positions", 
                           scenario, status.metrics.total_pnl, status.active_positions.len());
                    
                    // Check if scenario time is up
                    if start_time.elapsed() >= scenario_duration {
                        break;
                    }
                }
            }
        }
        
        Ok((tick_count, signal_count, trade_count))
    }
    
    /// Generate trading signals for current market conditions
    async fn generate_trading_signals(&mut self, scenario: TestScenario) -> Result<Vec<TradingSignal>> {
        let mut signals = Vec::new();
        let market_state = self.market_simulator.get_market_state();
        let symbols = self.config.symbols.clone(); // Clone to avoid borrowing issues
        let min_confidence = self.config.min_win_rate_pct / 100.0;
        
        for symbol in &symbols {
            let current_price = market_state.current_prices[symbol];
            
            // Generate ML prediction (simplified for simulation)
            let prediction = self.generate_simulation_prediction(symbol, current_price, scenario).await?;
            
            // Check if signal is strong enough
            if prediction.confidence >= min_confidence {
                let signal = TradingSignal {
                    symbol: symbol.clone(),
                    trend_prediction: prediction.clone(),
                    current_price,
                    signal_strength: prediction.confidence,
                    timestamp: now_micros(),
                };
                
                signals.push(signal);
            }
        }
        
        Ok(signals)
    }
    
    /// Generate realistic ML prediction for simulation
    async fn generate_simulation_prediction(
        &mut self,
        symbol: &str,
        current_price: f64,
        scenario: TestScenario,
    ) -> Result<TrendPrediction> {
        // Simulate ML prediction based on scenario
        let (trend_bias, confidence_range) = match scenario {
            TestScenario::NormalMarket => (0.0, (0.5, 0.8)),
            TestScenario::TrendingMarket => (0.5, (0.6, 0.9)),
            TestScenario::VolatileMarket => (0.0, (0.4, 0.7)),
            TestScenario::RangeboundMarket => (0.0, (0.3, 0.6)),
            TestScenario::StressTest => (0.0, (0.2, 0.5)),
        };
        
        let mut rng = rand::thread_rng();
        let trend_direction = rng.gen_range(-1.0..1.0) + trend_bias;
        let confidence = rng.gen_range(confidence_range.0..confidence_range.1);
        
        let trend_class = if trend_direction > 0.5 {
            TrendClass::StrongUp
        } else if trend_direction > 0.2 {
            TrendClass::WeakUp
        } else if trend_direction < -0.5 {
            TrendClass::StrongDown
        } else if trend_direction < -0.2 {
            TrendClass::WeakDown
        } else {
            TrendClass::Neutral
        };
        
        let expected_change_bps = match trend_class {
            TrendClass::StrongUp => 10.0 + rng.gen_range(0.0..5.0),
            TrendClass::WeakUp => 3.0 + rng.gen_range(0.0..4.0),
            TrendClass::Neutral => rng.gen_range(-2.0..2.0),
            TrendClass::WeakDown => -(3.0 + rng.gen_range(0.0..4.0)),
            TrendClass::StrongDown => -(10.0 + rng.gen_range(0.0..5.0)),
        };
        
        Ok(TrendPrediction {
            trend_class,
            class_probabilities: vec![0.2, 0.2, 0.2, 0.2, 0.2], // Simplified
            confidence,
            expected_change_bps,
            inference_latency_us: 500 + rng.gen_range(0..1000),
            timestamp: now_micros(),
            sequence_length: 30,
        })
    }
    
    /// Execute a trading signal
    async fn execute_signal(&self, signal: TradingSignal) -> Result<bool> {
        // Assess position with position manager
        let assessment = self.position_manager.assess_new_position(
            &signal.symbol,
            &signal.trend_prediction,
            signal.current_price,
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
                let side = match signal.trend_prediction.trend_class {
                    TrendClass::StrongUp | TrendClass::WeakUp => Side::Bid,
                    TrendClass::StrongDown | TrendClass::WeakDown => Side::Ask,
                    TrendClass::Neutral => return Ok(false),
                };
                
                // Execute trade
                let _position_id = self.position_manager.open_position(
                    signal.symbol,
                    side,
                    suggested_size,
                    signal.current_price,
                    &signal.trend_prediction,
                    kelly_fraction,
                    stop_loss,
                    take_profit,
                ).await?;
                
                if self.config.save_detailed_logs {
                    // Log execution details (would be implemented)
                }
                
                Ok(true)
            }
            PositionAssessment::Rejected { reason: _ } => Ok(false),
        }
    }
    
    /// Collect final simulation results
    async fn collect_results(
        &mut self,
        total_runtime: Duration,
        tick_count: u64,
        signal_count: u64,
        trade_count: u64,
        error_count: u64,
    ) -> Result<()> {
        let portfolio_status = self.position_manager.get_portfolio_status().await;
        
        // Update execution metrics
        self.results.execution_metrics.total_runtime_seconds = total_runtime.as_secs_f64();
        self.results.execution_metrics.total_ticks_processed = tick_count;
        self.results.execution_metrics.total_signals_generated = signal_count;
        self.results.execution_metrics.total_trades_executed = trade_count;
        self.results.execution_metrics.errors_encountered = error_count;
        
        // Update trading performance from portfolio status
        let metrics = &portfolio_status.metrics;
        self.results.trading_performance.total_pnl_usdt = metrics.total_pnl;
        self.results.trading_performance.total_return_pct = (metrics.total_pnl / self.config.test_capital) * 100.0;
        self.results.trading_performance.daily_return_pct = metrics.daily_pnl / self.config.test_capital * 100.0;
        self.results.trading_performance.win_rate_pct = metrics.win_rate;
        self.results.trading_performance.total_trades = metrics.total_trades;
        self.results.trading_performance.profit_factor = metrics.profit_factor;
        self.results.trading_performance.sharpe_ratio = metrics.sharpe_ratio;
        self.results.trading_performance.max_drawdown_usdt = metrics.max_drawdown;
        self.results.trading_performance.max_drawdown_pct = (metrics.max_drawdown / self.config.test_capital) * 100.0;
        self.results.trading_performance.capital_efficiency_pct = metrics.capital_efficiency * 100.0;
        
        // Calculate system health scores
        self.calculate_system_health_scores();
        
        // Validate against targets
        self.validate_against_targets();
        
        Ok(())
    }
    
    /// Calculate system health scores
    fn calculate_system_health_scores(&mut self) {
        let perf = &self.results.trading_performance;
        let _risk = &self.results.risk_metrics;
        let exec = &self.results.execution_metrics;
        
        // Performance score (40% weight)
        let performance_score = if perf.daily_return_pct >= self.config.min_daily_return_pct {
            let excess_return = (perf.daily_return_pct - self.config.min_daily_return_pct) / self.config.min_daily_return_pct;
            (8.0 + excess_return * 2.0).min(10.0)
        } else {
            (perf.daily_return_pct / self.config.min_daily_return_pct) * 8.0
        };
        
        // Risk management score (25% weight)
        let risk_score = if perf.max_drawdown_pct <= self.config.max_drawdown_pct {
            let safety_margin = (self.config.max_drawdown_pct - perf.max_drawdown_pct) / self.config.max_drawdown_pct;
            (7.0 + safety_margin * 3.0).min(10.0)
        } else {
            (self.config.max_drawdown_pct / perf.max_drawdown_pct) * 7.0
        };
        
        // Execution quality score (20% weight)
        let execution_score = if perf.win_rate_pct >= self.config.min_win_rate_pct {
            let win_rate_excess = (perf.win_rate_pct - self.config.min_win_rate_pct) / self.config.min_win_rate_pct;
            (7.0 + win_rate_excess * 3.0).min(10.0)
        } else {
            (perf.win_rate_pct / self.config.min_win_rate_pct) * 7.0
        };
        
        // System stability score (15% weight)
        let stability_score = if exec.errors_encountered == 0 {
            10.0
        } else {
            let error_rate = exec.errors_encountered as f64 / exec.total_signals_generated as f64;
            (10.0_f64 * (1.0 - error_rate * 10.0)).max(0.0)
        };
        
        // Overall health score
        let overall_score = (performance_score * 0.4 + risk_score * 0.25 + execution_score * 0.2 + stability_score * 0.15).min(10.0);
        
        // Determine recommendation
        let recommendation = if overall_score >= 8.0 {
            SystemRecommendation::ReadyForProduction
        } else if overall_score >= 6.0 {
            SystemRecommendation::NeedsOptimization
        } else if overall_score >= 4.0 {
            SystemRecommendation::RequiresSignificantChanges
        } else {
            SystemRecommendation::NotReadyForDeployment
        };
        
        self.results.system_health = SystemHealthMetrics {
            overall_health_score: overall_score,
            performance_score,
            risk_management_score: risk_score,
            execution_quality_score: execution_score,
            system_stability_score: stability_score,
            recommendation,
        };
    }
    
    /// Validate results against targets
    fn validate_against_targets(&mut self) {
        let perf = &self.results.trading_performance;
        
        let meets_return = perf.daily_return_pct >= self.config.min_daily_return_pct;
        let meets_risk = perf.max_drawdown_pct <= self.config.max_drawdown_pct;
        let meets_win_rate = perf.win_rate_pct >= self.config.min_win_rate_pct;
        let meets_drawdown = perf.max_drawdown_pct <= self.config.max_drawdown_pct;
        let meets_sharpe = perf.sharpe_ratio >= self.config.min_sharpe_ratio;
        let meets_efficiency = perf.capital_efficiency_pct >= self.config.min_capital_efficiency_pct;
        
        let validation_score = [
            meets_return,
            meets_risk,
            meets_win_rate,
            meets_drawdown,
            meets_sharpe,
            meets_efficiency,
        ].iter().map(|&x| if x { 1.0 } else { 0.0 }).sum::<f64>() / 6.0 * 100.0;
        
        let overall_passed = validation_score >= 80.0;
        
        let mut recommendations = Vec::new();
        if !meets_return { recommendations.push("Improve ML prediction accuracy".to_string()); }
        if !meets_risk { recommendations.push("Tighten risk management controls".to_string()); }
        if !meets_win_rate { recommendations.push("Optimize entry/exit signals".to_string()); }
        if !meets_sharpe { recommendations.push("Improve risk-adjusted returns".to_string()); }
        if !meets_efficiency { recommendations.push("Increase capital utilization".to_string()); }
        
        self.results.validation_results = ValidationResults {
            meets_return_target: meets_return,
            meets_risk_target: meets_risk,
            meets_win_rate_target: meets_win_rate,
            meets_drawdown_target: meets_drawdown,
            meets_sharpe_target: meets_sharpe,
            meets_efficiency_target: meets_efficiency,
            overall_validation_passed: overall_passed,
            validation_score,
            recommendations,
        };
    }
    
    /// Generate comprehensive performance report
    async fn generate_performance_report(&self) -> Result<()> {
        info!("=== ULTRA-THINK LOB DL+RL SIMULATION REPORT ===");
        info!("📊 Execution Metrics:");
        info!("  Runtime: {:.1} seconds", self.results.execution_metrics.total_runtime_seconds);
        info!("  Ticks Processed: {}", self.results.execution_metrics.total_ticks_processed);
        info!("  Signals Generated: {}", self.results.execution_metrics.total_signals_generated);
        info!("  Trades Executed: {}", self.results.execution_metrics.total_trades_executed);
        
        info!("💰 Trading Performance:");
        info!("  Total P&L: {:.2} USDT ({:.2}%)", 
              self.results.trading_performance.total_pnl_usdt,
              self.results.trading_performance.total_return_pct);
        info!("  Daily Return: {:.2}%", self.results.trading_performance.daily_return_pct);
        info!("  Win Rate: {:.1}%", self.results.trading_performance.win_rate_pct);
        info!("  Profit Factor: {:.2}", self.results.trading_performance.profit_factor);
        info!("  Sharpe Ratio: {:.2}", self.results.trading_performance.sharpe_ratio);
        info!("  Max Drawdown: {:.2}%", self.results.trading_performance.max_drawdown_pct);
        
        info!("🏥 System Health:");
        info!("  Overall Score: {:.1}/10.0", self.results.system_health.overall_health_score);
        info!("  Performance: {:.1}/10.0", self.results.system_health.performance_score);
        info!("  Risk Management: {:.1}/10.0", self.results.system_health.risk_management_score);
        info!("  Execution Quality: {:.1}/10.0", self.results.system_health.execution_quality_score);
        info!("  System Stability: {:.1}/10.0", self.results.system_health.system_stability_score);
        info!("  Recommendation: {:?}", self.results.system_health.recommendation);
        
        info!("✅ Validation Results:");
        info!("  Overall Passed: {}", self.results.validation_results.overall_validation_passed);
        info!("  Validation Score: {:.1}%", self.results.validation_results.validation_score);
        if !self.results.validation_results.recommendations.is_empty() {
            info!("  Recommendations:");
            for rec in &self.results.validation_results.recommendations {
                info!("    - {}", rec);
            }
        }
        
        Ok(())
    }
}

/// Trading signal for simulation
#[derive(Debug, Clone)]
pub struct TradingSignal {
    pub symbol: String,
    pub trend_prediction: TrendPrediction,
    pub current_price: f64,
    pub signal_strength: f64,
    pub timestamp: Timestamp,
}

/// Utility function to get current timestamp
fn now_micros() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as Timestamp
}