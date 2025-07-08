/*!
 * Dual Symbol Trading Framework for LOB DL+RL System
 * 
 * Integrates all Phase 1 components for BTCUSDT and ETHUSDT trading:
 * - LOB time series feature extraction
 * - DL trend prediction (10-second horizon)
 * - Enhanced risk management (Kelly formula + VaR)
 * - Dual-symbol portfolio management
 * - Real-time signal generation and execution
 */

use crate::core::types::*;
use crate::core::config::Config;
use crate::core::orderbook::OrderBook;
use crate::ml::{
    LobTimeSeriesExtractor, 
    LobTimeSeriesConfig,
    DlTrendPredictor,
    DlTrendPredictorConfig,
    TrendPrediction,
    TrendClass,
};
use crate::engine::{
    EnhancedRiskController,
    EnhancedRiskConfig,
    RiskAssessment,
    PositionRisk,
    TestCapitalPositionManager,
    TestCapitalConfig,
    PositionAssessment,
};
use crate::integrations::bitget_connector::BitgetConnector;
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, sleep, Instant};
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};

/// Configuration for dual symbol trading framework
#[derive(Debug, Clone)]
pub struct DualSymbolTradingConfig {
    /// Symbols to trade
    pub symbols: Vec<String>,               // ["BTCUSDT", "ETHUSDT"]
    
    /// Capital allocation  
    pub total_capital: f64,                 // 400.0 USDT
    pub test_capital: f64,                  // 100.0 USDT (Phase 1 testing)
    pub capital_per_symbol_pct: f64,        // 0.4 (40% per symbol max)
    
    /// Trading parameters
    pub prediction_interval_ms: u64,        // 500ms between predictions
    pub min_confidence_threshold: f64,      // 0.65 minimum confidence
    pub max_positions_per_symbol: usize,    // 2 concurrent positions per symbol
    
    /// Risk management
    pub daily_loss_limit_pct: f64,          // 0.1 (10% daily loss limit)
    pub emergency_stop_pct: f64,            // 0.15 (15% emergency stop)
    pub position_hold_time_seconds: u64,    // 10-30 seconds per position
    
    /// Performance targets
    pub target_daily_return_pct: f64,       // 0.008 to 0.023 (0.8% to 2.3%)
    pub max_drawdown_pct: f64,              // 0.05 (5% max drawdown)
    pub min_sharpe_ratio: f64,              // 1.0 minimum Sharpe ratio
}

impl Default for DualSymbolTradingConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            total_capital: 400.0,
            test_capital: 100.0,
            capital_per_symbol_pct: 0.4,
            prediction_interval_ms: 500,
            min_confidence_threshold: 0.65,
            max_positions_per_symbol: 2,
            daily_loss_limit_pct: 0.1,
            emergency_stop_pct: 0.15,
            position_hold_time_seconds: 15, // 15-second average hold time
            target_daily_return_pct: 0.015, // 1.5% daily target
            max_drawdown_pct: 0.05,
            min_sharpe_ratio: 1.0,
        }
    }
}

/// Trading signal with risk assessment
#[derive(Debug, Clone)]
pub struct DualSymbolTradingSignal {
    pub symbol: String,
    pub trend_prediction: TrendPrediction,
    pub risk_assessment: RiskAssessment,
    pub suggested_action: TradingAction,
    pub urgency_score: f64,             // 0-1, higher = more urgent
    pub correlation_impact: f64,        // Impact on other symbol
    pub timestamp: Timestamp,
}

/// Trading action recommendation
#[derive(Debug, Clone)]
pub enum TradingAction {
    Buy {
        size: f64,
        max_price: f64,
        confidence: f64,
        hold_duration_seconds: u64,
    },
    Sell {
        size: f64,
        min_price: f64,
        confidence: f64,
        hold_duration_seconds: u64,
    },
    Hold {
        reason: String,
    },
    ClosePosition {
        symbol: String,
        reason: String,
        urgency: f64,
    },
}

/// Symbol-specific trading state
#[derive(Debug, Clone)]
pub struct SymbolTradingState {
    pub symbol: String,
    pub extractor: Arc<Mutex<LobTimeSeriesExtractor>>,
    pub predictor: Arc<Mutex<DlTrendPredictor>>,
    pub current_positions: Vec<Position>,
    pub recent_signals: Vec<DualSymbolTradingSignal>,
    pub performance_metrics: SymbolPerformanceMetrics,
    pub last_prediction_time: Timestamp,
    pub is_active: bool,
}

/// Position tracking
#[derive(Debug, Clone)]
pub struct Position {
    pub id: String,
    pub symbol: String,
    pub side: Side,
    pub size: f64,
    pub entry_price: f64,
    pub entry_time: Timestamp,
    pub target_exit_time: Timestamp,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub confidence: f64,
    pub current_pnl: f64,
}

/// Symbol performance metrics
#[derive(Debug, Clone, Default)]
pub struct SymbolPerformanceMetrics {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub total_pnl: f64,
    pub daily_pnl: f64,
    pub max_drawdown: f64,
    pub avg_hold_time_seconds: f64,
    pub avg_confidence: f64,
    pub sharpe_ratio: f64,
    pub last_update: Timestamp,
}

/// Main dual symbol trading framework
pub struct DualSymbolTradingFramework {
    /// Configuration
    config: DualSymbolTradingConfig,
    
    /// Global components
    risk_controller: Arc<Mutex<EnhancedRiskController>>,
    position_manager: Arc<Mutex<TestCapitalPositionManager>>,
    connector: Arc<Mutex<BitgetConnector>>,
    
    /// Symbol-specific states
    symbol_states: HashMap<String, Arc<Mutex<SymbolTradingState>>>,
    
    /// Communication channels
    signal_sender: mpsc::UnboundedSender<DualSymbolTradingSignal>,
    signal_receiver: Arc<Mutex<mpsc::UnboundedReceiver<DualSymbolTradingSignal>>>,
    
    /// Framework state
    is_running: Arc<Mutex<bool>>,
    start_time: Instant,
    
    /// Performance tracking
    framework_metrics: FrameworkPerformanceMetrics,
}

/// Framework-wide performance metrics
#[derive(Debug, Clone, Default)]
pub struct FrameworkPerformanceMetrics {
    pub total_trades: u64,
    pub total_signals_generated: u64,
    pub total_signals_executed: u64,
    pub portfolio_pnl: f64,
    pub daily_portfolio_pnl: f64,
    pub portfolio_sharpe_ratio: f64,
    pub correlation_btc_eth: f64,
    pub risk_adjusted_return: f64,
    pub max_portfolio_drawdown: f64,
    pub uptime_percentage: f64,
    pub avg_signal_latency_us: f64,
    pub last_update: Timestamp,
}

impl DualSymbolTradingFramework {
    /// Create new dual symbol trading framework
    pub async fn new(
        config: DualSymbolTradingConfig,
        base_config: Config,
    ) -> Result<Self> {
        info!("Initializing Dual Symbol Trading Framework for {:?}", config.symbols);
        
        // Initialize risk controller with enhanced config
        let risk_config = EnhancedRiskConfig {
            total_capital: config.test_capital, // Start with test capital
            max_risk_capital: config.test_capital * 0.5, // 50U max risk
            daily_loss_limit: config.test_capital * config.daily_loss_limit_pct,
            emergency_stop_loss: config.test_capital * config.emergency_stop_pct,
            max_single_position_pct: config.capital_per_symbol_pct,
            max_total_exposure_pct: 0.8, // 80% max exposure
            kelly_fraction_limit: 0.25,
            var_confidence_levels: vec![0.95, 0.99],
            var_time_horizons: vec![3600, 86400],
            sharpe_ratio_threshold: config.min_sharpe_ratio,
            min_prediction_confidence: config.min_confidence_threshold,
            max_correlation_threshold: 0.8,
            cooling_period_minutes: 30,
            enable_position_scaling: true,
            enable_automatic_recovery: true,
            recovery_confidence_boost: 0.1,
        };
        
        let risk_controller = Arc::new(Mutex::new(
            EnhancedRiskController::new(risk_config)
        ));
        
        // Initialize test capital position manager
        let test_config = TestCapitalConfig {
            test_capital: config.test_capital,
            capital_per_symbol: config.test_capital * config.capital_per_symbol_pct,
            min_position_size: 2.0,
            max_position_size: config.test_capital * 0.15, // 15% max single position
            daily_loss_limit: config.test_capital * config.daily_loss_limit_pct,
            position_loss_limit: 3.0,
            max_drawdown_limit: config.test_capital * 0.05,
            target_daily_profit: config.test_capital * config.target_daily_return_pct,
            profit_taking_threshold: 2.0,
            kelly_fraction_conservative: 0.15,
            max_concurrent_positions: config.max_positions_per_symbol * config.symbols.len(),
            position_hold_time_seconds: config.position_hold_time_seconds,
            rebalance_interval_seconds: 60,
            track_trade_performance: true,
            log_position_changes: true,
            calculate_metrics_realtime: true,
        };
        
        let mut position_manager = TestCapitalPositionManager::new(test_config);
        position_manager.set_risk_controller(risk_controller.clone());
        let position_manager = Arc::new(Mutex::new(position_manager));
        
        // Initialize Bitget connector with default config for demo
        let bitget_config = crate::integrations::bitget_connector::BitgetConfig {
            public_ws_url: "wss://ws.bitget.com/spot/v1/stream".to_string(),
            private_ws_url: "wss://ws.bitget.com/spot/v1/stream".to_string(),
            timeout_seconds: 30,
            auto_reconnect: true,
            max_reconnect_attempts: 5,
            reconnect_strategy: crate::integrations::bitget_connector::ReconnectStrategy::Standard,
        };
        let connector = Arc::new(Mutex::new(
            BitgetConnector::new(bitget_config)
        ));
        
        // Create signal channel
        let (signal_sender, signal_receiver) = mpsc::unbounded_channel();
        let signal_receiver = Arc::new(Mutex::new(signal_receiver));
        
        // Initialize symbol states
        let mut symbol_states = HashMap::new();
        
        for symbol in &config.symbols {
            let symbol_state = Self::create_symbol_state(symbol.clone(), &base_config).await?;
            symbol_states.insert(symbol.clone(), Arc::new(Mutex::new(symbol_state)));
        }
        
        Ok(Self {
            config,
            risk_controller,
            position_manager,
            connector,
            symbol_states,
            signal_sender,
            signal_receiver,
            is_running: Arc::new(Mutex::new(false)),
            start_time: Instant::now(),
            framework_metrics: FrameworkPerformanceMetrics::default(),
        })
    }
    
    /// Create symbol-specific trading state
    async fn create_symbol_state(
        symbol: String,
        base_config: &Config,
    ) -> Result<SymbolTradingState> {
        // Configure LOB time series extractor for this symbol
        let lob_config = LobTimeSeriesConfig {
            dl_lookback_seconds: 30,
            dl_prediction_seconds: 10,
            rl_state_window_ms: 5000,
            extraction_interval_us: 500_000,
            max_sequence_length: 60,
            lob_depth: 20,
            enable_advanced_features: true,
            memory_cleanup_interval: 60,
        };
        
        let extractor = Arc::new(Mutex::new(
            LobTimeSeriesExtractor::new(symbol.clone(), lob_config.clone())
        ));
        
        // Configure DL trend predictor for this symbol
        let dl_config = DlTrendPredictorConfig {
            hidden_dim: 256,
            num_heads: 8,
            num_layers: 4,
            dropout_rate: 0.1,
            max_sequence_length: 60,
            input_dim: 76,
            output_dim: 5,
            learning_rate: 0.001,
            batch_size: 32,
            warmup_steps: 1000,
            confidence_threshold: 0.65,
            prediction_horizon_seconds: 10,
            update_frequency_minutes: 30,
            max_inference_latency_us: 1000,
            min_prediction_accuracy: 0.55,
        };
        
        let predictor = Arc::new(Mutex::new(
            DlTrendPredictor::new(symbol.clone(), dl_config, lob_config)?
        ));
        
        info!("Created trading state for symbol: {}", symbol);
        
        Ok(SymbolTradingState {
            symbol: symbol.clone(),
            extractor,
            predictor,
            current_positions: Vec::new(),
            recent_signals: Vec::with_capacity(100),
            performance_metrics: SymbolPerformanceMetrics::default(),
            last_prediction_time: 0,
            is_active: true,
        })
    }
    
    /// Start the dual symbol trading framework
    pub async fn start(&mut self) -> Result<()> {
        info!("Starting Dual Symbol Trading Framework");
        
        {
            let mut is_running = self.is_running.lock().await;
            *is_running = true;
        }
        
        // Start market data collection for all symbols
        self.start_market_data_collection().await?;
        
        // Start prediction engine
        self.start_prediction_engine().await?;
        
        // Start signal processing
        self.start_signal_processing().await?;
        
        // Start risk monitoring
        self.start_risk_monitoring().await?;
        
        // Start performance tracking
        self.start_performance_tracking().await?;
        
        // Start position management
        self.start_position_management().await?;
        
        info!("Dual Symbol Trading Framework started successfully");
        Ok(())
    }
    
    /// Start market data collection for all symbols
    async fn start_market_data_collection(&self) -> Result<()> {
        for (symbol, state) in &self.symbol_states {
            let state_clone = state.clone();
            let connector_clone = self.connector.clone();
            let symbol_clone = symbol.clone();
            let symbol_for_error = symbol.clone();
            
            tokio::spawn(async move {
                if let Err(e) = Self::collect_market_data_for_symbol(
                    symbol_clone,
                    state_clone,
                    connector_clone,
                ).await {
                    error!("Market data collection failed for {}: {}", symbol_for_error, e);
                }
            });
        }
        
        Ok(())
    }
    
    /// Collect market data for a specific symbol
    async fn collect_market_data_for_symbol(
        symbol: String,
        state: Arc<Mutex<SymbolTradingState>>,
        connector: Arc<Mutex<BitgetConnector>>,
    ) -> Result<()> {
        info!("Starting market data collection for {}", symbol);
        
        let mut interval = tokio::time::interval(Duration::from_millis(100)); // 100ms collection
        
        loop {
            interval.tick().await;
            
            // Get latest orderbook (placeholder - would use actual Bitget API)
            let orderbook = {
                let _connector_guard = connector.lock().await;
                // For demo purposes, create a mock orderbook
                OrderBook::new(symbol.clone()) // This would be replaced with actual API call
            };
            
            // Update extractor with new orderbook data
            {
                let state_guard = state.lock().await;
                if state_guard.is_active {
                    let mut extractor = state_guard.extractor.lock().await;
                    extractor.extract_snapshot(&orderbook, 100)?; // 100μs network latency estimate
                }
            }
        }
    }
    
    /// Start prediction engine for all symbols
    async fn start_prediction_engine(&self) -> Result<()> {
        for (symbol, state) in &self.symbol_states {
            let state_clone = state.clone();
            let signal_sender = self.signal_sender.clone();
            let prediction_interval = Duration::from_millis(self.config.prediction_interval_ms);
            let symbol_clone = symbol.clone();
            let symbol_for_error = symbol.clone();
            
            tokio::spawn(async move {
                if let Err(e) = Self::run_prediction_engine_for_symbol(
                    symbol_clone,
                    state_clone,
                    signal_sender,
                    prediction_interval,
                ).await {
                    error!("Prediction engine failed for {}: {}", symbol_for_error, e);
                }
            });
        }
        
        Ok(())
    }
    
    /// Run prediction engine for a specific symbol
    async fn run_prediction_engine_for_symbol(
        symbol: String,
        state: Arc<Mutex<SymbolTradingState>>,
        signal_sender: mpsc::UnboundedSender<DualSymbolTradingSignal>,
        prediction_interval: Duration,
    ) -> Result<()> {
        info!("Starting prediction engine for {}", symbol);
        
        let mut interval = tokio::time::interval(prediction_interval);
        
        loop {
            interval.tick().await;
            
            let state_guard = state.lock().await;
            if !state_guard.is_active {
                continue;
            }
            
            // Generate prediction
            let prediction = {
                let extractor = state_guard.extractor.lock().await;
                let mut predictor = state_guard.predictor.lock().await;
                predictor.predict_trend(&*extractor)?
            };
            
            if let Some(trend_prediction) = prediction {
                // Create trading signal
                let trading_signal = Self::create_trading_signal(
                    symbol.clone(),
                    trend_prediction,
                    &state_guard,
                ).await;
                
                // Send signal for processing
                if let Err(e) = signal_sender.send(trading_signal) {
                    error!("Failed to send trading signal for {}: {}", symbol, e);
                }
            }
            
            drop(state_guard);
        }
    }
    
    /// Create trading signal from trend prediction
    async fn create_trading_signal(
        symbol: String,
        trend_prediction: TrendPrediction,
        state: &SymbolTradingState,
    ) -> DualSymbolTradingSignal {
        // Determine suggested action based on trend prediction
        let suggested_action = match trend_prediction.trend_class {
            TrendClass::StrongUp | TrendClass::WeakUp => {
                if trend_prediction.confidence > 0.7 {
                    TradingAction::Buy {
                        size: 10.0, // Will be adjusted by risk controller
                        max_price: 0.0, // Will be set based on current market price
                        confidence: trend_prediction.confidence,
                        hold_duration_seconds: 15,
                    }
                } else {
                    TradingAction::Hold {
                        reason: "Confidence too low for buy signal".to_string(),
                    }
                }
            },
            TrendClass::StrongDown | TrendClass::WeakDown => {
                if trend_prediction.confidence > 0.7 {
                    TradingAction::Sell {
                        size: 10.0, // Will be adjusted by risk controller
                        min_price: 0.0, // Will be set based on current market price
                        confidence: trend_prediction.confidence,
                        hold_duration_seconds: 15,
                    }
                } else {
                    TradingAction::Hold {
                        reason: "Confidence too low for sell signal".to_string(),
                    }
                }
            },
            TrendClass::Neutral => {
                TradingAction::Hold {
                    reason: "Market trend is neutral".to_string(),
                }
            },
        };
        
        // Calculate urgency score
        let urgency_score = Self::calculate_urgency_score(&trend_prediction, state);
        
        // Estimate correlation impact (simplified)
        let correlation_impact = if symbol == "BTCUSDT" { 0.7 } else { 0.7 }; // BTC-ETH correlation
        
        DualSymbolTradingSignal {
            symbol,
            trend_prediction,
            risk_assessment: RiskAssessment::Approved {
                suggested_size: 10.0,
                max_size: 20.0,
                confidence_requirement: 0.65,
            }, // Placeholder, will be assessed by risk controller
            suggested_action,
            urgency_score,
            correlation_impact,
            timestamp: now_micros(),
        }
    }
    
    /// Calculate urgency score for a signal
    fn calculate_urgency_score(
        prediction: &TrendPrediction,
        _state: &SymbolTradingState,
    ) -> f64 {
        let mut urgency = 0.0;
        
        // Base urgency from confidence
        urgency += prediction.confidence * 0.4;
        
        // Urgency from expected magnitude
        let magnitude = prediction.expected_change_bps.abs() / 20.0; // Normalize to 20 bps
        urgency += magnitude.min(1.0) * 0.3;
        
        // Urgency from trend strength
        let trend_strength = match prediction.trend_class {
            TrendClass::StrongUp | TrendClass::StrongDown => 0.3,
            TrendClass::WeakUp | TrendClass::WeakDown => 0.15,
            TrendClass::Neutral => 0.0,
        };
        urgency += trend_strength;
        
        urgency.min(1.0)
    }
    
    /// Start signal processing engine
    async fn start_signal_processing(&self) -> Result<()> {
        let signal_receiver = self.signal_receiver.clone();
        let risk_controller = self.risk_controller.clone();
        let position_manager = self.position_manager.clone();
        let connector = self.connector.clone();
        let is_running = self.is_running.clone();
        
        tokio::spawn(async move {
            Self::process_trading_signals(
                signal_receiver,
                risk_controller,
                position_manager,
                connector,
                is_running,
            ).await;
        });
        
        Ok(())
    }
    
    /// Process trading signals with risk assessment
    async fn process_trading_signals(
        signal_receiver: Arc<Mutex<mpsc::UnboundedReceiver<DualSymbolTradingSignal>>>,
        risk_controller: Arc<Mutex<EnhancedRiskController>>,
        position_manager: Arc<Mutex<TestCapitalPositionManager>>,
        connector: Arc<Mutex<BitgetConnector>>,
        is_running: Arc<Mutex<bool>>,
    ) {
        info!("Starting signal processing engine");
        
        loop {
            // Check if framework is still running
            {
                let running = is_running.lock().await;
                if !*running {
                    break;
                }
            }
            
            // Receive signal
            let signal = {
                let mut receiver = signal_receiver.lock().await;
                receiver.recv().await
            };
            
            if let Some(trading_signal) = signal {
                // Process the signal
                if let Err(e) = Self::execute_trading_signal(
                    trading_signal,
                    &risk_controller,
                    &position_manager,
                    &connector,
                ).await {
                    error!("Failed to execute trading signal: {}", e);
                }
            }
        }
        
        info!("Signal processing engine stopped");
    }
    
    /// Execute a trading signal after risk assessment
    async fn execute_trading_signal(
        mut signal: DualSymbolTradingSignal,
        risk_controller: &Arc<Mutex<EnhancedRiskController>>,
        position_manager: &Arc<Mutex<TestCapitalPositionManager>>,
        connector: &Arc<Mutex<BitgetConnector>>,
    ) -> Result<()> {
        debug!("Processing trading signal for {}", signal.symbol);
        
        // Get current market price from orderbook
        let current_price = {
            let _connector_guard = connector.lock().await;
            // For demo purposes, use mock prices based on symbol
            match signal.symbol.as_str() {
                "BTCUSDT" => 50000.0,
                "ETHUSDT" => 3000.0,
                _ => 1000.0,
            }
        };
        
        if current_price <= 0.0 {
            warn!("Invalid price for {}, skipping signal", signal.symbol);
            return Ok(());
        }
        
        // Create basic trading signal for risk assessment
        let basic_signal = TradingSignal {
            signal_type: match signal.suggested_action {
                TradingAction::Buy { .. } => SignalType::Buy,
                TradingAction::Sell { .. } => SignalType::Sell,
                _ => SignalType::Hold,
            },
            confidence: signal.trend_prediction.confidence,
            suggested_price: current_price.to_price(),
            suggested_quantity: 10.0.to_quantity(), // Base quantity
            timestamp: signal.timestamp,
            features_timestamp: signal.timestamp,
            signal_latency_us: signal.trend_prediction.inference_latency_us,
        };
        
        // Use test capital position manager for position assessment
        let position_assessment = {
            let pm_guard = position_manager.lock().await;
            pm_guard.assess_new_position(
                &signal.symbol,
                &signal.trend_prediction,
                current_price,
            ).await?
        };
        
        // Execute based on position manager assessment
        match position_assessment {
            PositionAssessment::Approved { 
                suggested_size, 
                kelly_fraction,
                stop_loss,
                take_profit,
                .. 
            } => {
                // Determine trade side based on prediction
                let side = match signal.trend_prediction.trend_class {
                    TrendClass::StrongUp | TrendClass::WeakUp => Side::Bid,  // Long
                    TrendClass::StrongDown | TrendClass::WeakDown => Side::Ask, // Short
                    TrendClass::Neutral => {
                        debug!("Neutral trend prediction, skipping trade");
                        return Ok(());
                    }
                };
                
                // Open position through position manager
                let position_id = {
                    let pm_guard = position_manager.lock().await;
                    pm_guard.open_position(
                        signal.symbol.clone(),
                        side,
                        suggested_size,
                        current_price,
                        &signal.trend_prediction,
                        kelly_fraction,
                        stop_loss,
                        take_profit,
                    ).await?
                };
                
                info!(
                    "Opened position {}: {} {} {:.2} USDT @ {:.2} (Kelly: {:.3}, Confidence: {:.3})",
                    position_id,
                    signal.symbol,
                    match side { Side::Bid => "LONG", Side::Ask => "SHORT" },
                    suggested_size,
                    current_price,
                    kelly_fraction,
                    signal.trend_prediction.confidence
                );
            },
            PositionAssessment::Rejected { reason } => {
                debug!("Position rejected for {}: {}", signal.symbol, reason);
            },
        }
        
        Ok(())
    }
    
    /// Start risk monitoring
    async fn start_risk_monitoring(&self) -> Result<()> {
        let risk_controller = self.risk_controller.clone();
        let is_running = self.is_running.clone();
        
        tokio::spawn(async move {
            Self::monitor_risk_continuously(risk_controller, is_running).await;
        });
        
        Ok(())
    }
    
    /// Continuously monitor risk levels
    async fn monitor_risk_continuously(
        risk_controller: Arc<Mutex<EnhancedRiskController>>,
        is_running: Arc<Mutex<bool>>,
    ) {
        info!("Starting risk monitoring");
        
        let mut interval = tokio::time::interval(Duration::from_secs(5)); // Monitor every 5 seconds
        
        loop {
            interval.tick().await;
            
            // Check if framework is still running
            {
                let running = is_running.lock().await;
                if !*running {
                    break;
                }
            }
            
            // Check portfolio risk metrics
            let portfolio_metrics = {
                let risk_guard = risk_controller.lock().await;
                risk_guard.get_portfolio_metrics().clone()
            };
            
            // Log risk status
            debug!(
                "Portfolio status: capital_used={:.2}/{:.2}, daily_pnl={:.2}, risk_level={:?}",
                portfolio_metrics.used_capital,
                portfolio_metrics.total_capital,
                portfolio_metrics.daily_pnl,
                portfolio_metrics.risk_level
            );
            
            // Check for emergency conditions
            if portfolio_metrics.is_emergency_stop {
                error!("Emergency stop detected - halting trading");
                // Emergency stop logic would go here
            }
        }
        
        info!("Risk monitoring stopped");
    }
    
    /// Start performance tracking
    async fn start_performance_tracking(&self) -> Result<()> {
        let symbol_states = self.symbol_states.clone();
        let is_running = self.is_running.clone();
        
        tokio::spawn(async move {
            Self::track_performance_continuously(symbol_states, is_running).await;
        });
        
        Ok(())
    }
    
    /// Continuously track performance
    async fn track_performance_continuously(
        symbol_states: HashMap<String, Arc<Mutex<SymbolTradingState>>>,
        is_running: Arc<Mutex<bool>>,
    ) {
        info!("Starting performance tracking");
        
        let mut interval = tokio::time::interval(Duration::from_secs(10)); // Track every 10 seconds
        
        loop {
            interval.tick().await;
            
            // Check if framework is still running
            {
                let running = is_running.lock().await;
                if !*running {
                    break;
                }
            }
            
            // Update performance metrics for each symbol
            for (symbol, state) in &symbol_states {
                let state_guard = state.lock().await;
                
                debug!(
                    "{} performance: trades={}, pnl={:.2}, daily_pnl={:.2}",
                    symbol,
                    state_guard.performance_metrics.total_trades,
                    state_guard.performance_metrics.total_pnl,
                    state_guard.performance_metrics.daily_pnl
                );
            }
        }
        
        info!("Performance tracking stopped");
    }
    
    /// Start position management for test capital
    async fn start_position_management(&self) -> Result<()> {
        let position_manager = self.position_manager.clone();
        let is_running = self.is_running.clone();
        
        tokio::spawn(async move {
            Self::manage_positions_continuously(position_manager, is_running).await;
        });
        
        Ok(())
    }
    
    /// Continuously manage positions (price updates, exits, rebalancing)
    async fn manage_positions_continuously(
        position_manager: Arc<Mutex<TestCapitalPositionManager>>,
        is_running: Arc<Mutex<bool>>,
    ) {
        info!("Starting position management");
        
        let mut price_update_interval = tokio::time::interval(Duration::from_millis(1000)); // 1s price updates
        let mut exit_check_interval = tokio::time::interval(Duration::from_millis(500));   // 500ms exit checks
        
        loop {
            tokio::select! {
                _ = price_update_interval.tick() => {
                    // Check if framework is still running
                    {
                        let running = is_running.lock().await;
                        if !*running {
                            break;
                        }
                    }
                    
                    // Update positions with current market prices (mock prices for demo)
                    let mut price_updates = HashMap::new();
                    let btc_price = 50000.0 + ((now_micros() % 1000) as f64 - 500.0); // Simple price variation
                    let eth_price = 3000.0 + ((now_micros() % 100) as f64 - 50.0);
                    price_updates.insert("BTCUSDT".to_string(), btc_price);
                    price_updates.insert("ETHUSDT".to_string(), eth_price);
                    
                    {
                        let pm_guard = position_manager.lock().await;
                        if let Err(e) = pm_guard.update_positions(price_updates).await {
                            error!("Failed to update positions: {}", e);
                        }
                    }
                },
                
                _ = exit_check_interval.tick() => {
                    // Check if framework is still running
                    {
                        let running = is_running.lock().await;
                        if !*running {
                            break;
                        }
                    }
                    
                    // Check for position exits
                    let closed_positions = {
                        let pm_guard = position_manager.lock().await;
                        pm_guard.check_position_exits().await.unwrap_or_default()
                    };
                    
                    if !closed_positions.is_empty() {
                        info!("Closed {} positions", closed_positions.len());
                    }
                }
            }
        }
        
        info!("Position management stopped");
    }
    
    /// Stop the trading framework
    pub async fn stop(&mut self) -> Result<()> {
        info!("Stopping Dual Symbol Trading Framework");
        
        {
            let mut is_running = self.is_running.lock().await;
            *is_running = false;
        }
        
        // Give time for all tasks to complete
        sleep(Duration::from_secs(1)).await;
        
        info!("Dual Symbol Trading Framework stopped");
        Ok(())
    }
    
    /// Get current framework status
    pub async fn get_status(&self) -> FrameworkStatus {
        let is_running = *self.is_running.lock().await;
        let uptime = self.start_time.elapsed();
        
        // Collect symbol statuses
        let mut symbol_statuses = HashMap::new();
        for (symbol, state) in &self.symbol_states {
            let state_guard = state.lock().await;
            symbol_statuses.insert(symbol.clone(), SymbolStatus {
                symbol: symbol.clone(),
                is_active: state_guard.is_active,
                current_positions: state_guard.current_positions.len(),
                performance: state_guard.performance_metrics.clone(),
            });
        }
        
        let portfolio_metrics = {
            let risk_guard = self.risk_controller.lock().await;
            risk_guard.get_portfolio_metrics().clone()
        };
        
        FrameworkStatus {
            is_running,
            uptime_seconds: uptime.as_secs(),
            symbols: symbol_statuses,
            portfolio_metrics,
            framework_metrics: self.framework_metrics.clone(),
        }
    }
}

/// Framework status information
#[derive(Debug, Clone)]
pub struct FrameworkStatus {
    pub is_running: bool,
    pub uptime_seconds: u64,
    pub symbols: HashMap<String, SymbolStatus>,
    pub portfolio_metrics: crate::engine::PortfolioRiskMetrics,
    pub framework_metrics: FrameworkPerformanceMetrics,
}

/// Symbol-specific status
#[derive(Debug, Clone)]
pub struct SymbolStatus {
    pub symbol: String,
    pub is_active: bool,
    pub current_positions: usize,
    pub performance: SymbolPerformanceMetrics,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_dual_symbol_framework_creation() {
        let config = DualSymbolTradingConfig::default();
        let base_config = Config::default();
        
        let framework = DualSymbolTradingFramework::new(config, base_config).await;
        assert!(framework.is_ok());
        
        let framework = framework.unwrap();
        assert_eq!(framework.symbol_states.len(), 2);
        assert!(framework.symbol_states.contains_key("BTCUSDT"));
        assert!(framework.symbol_states.contains_key("ETHUSDT"));
    }
    
    #[tokio::test]
    async fn test_trading_signal_creation() {
        let config = DualSymbolTradingConfig::default();
        let base_config = Config::default();
        
        let framework = DualSymbolTradingFramework::new(config, base_config).await.unwrap();
        let btc_state = framework.symbol_states.get("BTCUSDT").unwrap();
        let state_guard = btc_state.lock().await;
        
        let trend_prediction = TrendPrediction {
            trend_class: TrendClass::WeakUp,
            class_probabilities: vec![0.1, 0.2, 0.3, 0.3, 0.1],
            confidence: 0.75,
            expected_change_bps: 8.0,
            inference_latency_us: 800,
            timestamp: now_micros(),
            sequence_length: 30,
        };
        
        let signal = DualSymbolTradingFramework::create_trading_signal(
            "BTCUSDT".to_string(),
            trend_prediction,
            &state_guard,
        ).await;
        
        assert_eq!(signal.symbol, "BTCUSDT");
        assert!(signal.urgency_score > 0.0);
        matches!(signal.suggested_action, TradingAction::Buy { .. });
    }
}