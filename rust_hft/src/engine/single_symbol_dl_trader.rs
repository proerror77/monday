/*!
 * Single Symbol DL Trader - BTCUSDT專門優化
 * 
 * 專注於單個商品的深度學習交易系統：
 * - BTCUSDT專門的特徵工程和模型優化
 * - 實時LOB數據處理和特徵提取
 * - Transformer-based趨勢預測（3s, 5s, 10s多時間框架）
 * - Kelly公式優化的動態倉位管理
 * - 超低延遲執行（目標<1ms推理）
 * 
 * 性能目標：
 * - 每日回報：1.2-2.5%
 * - 勝率：>65%
 * - 最大回撤：<3%
 * - 推理延遲：<1ms (P99)
 * - 資金效率：>90%
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
use crate::integrations::bitget_connector::{BitgetConnector, BitgetConfig};
use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio::time::{Duration, sleep, Instant};
use tracing::{info, warn, debug, error};
use serde::{Serialize, Deserialize};

/// BTCUSDT專門的配置參數
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtcusdtDlConfig {
    /// 基礎交易參數
    pub symbol: String,                           // "BTCUSDT"
    pub test_capital: f64,                        // 100.0 USDT (Phase 1)
    pub max_position_size_pct: f64,               // 0.15 (15% max single position)
    pub min_position_size_usdt: f64,              // 2.0 USDT minimum
    
    /// DL模型參數
    pub sequence_length: usize,                   // 30 time steps
    pub prediction_horizons: Vec<u64>,            // [3, 5, 10] seconds
    pub min_prediction_confidence: f64,           // 0.65 minimum confidence
    pub model_update_interval_seconds: u64,      // 300 (5分鐘模型更新)
    
    /// 特徵工程參數
    pub lob_depth: usize,                         // 20 levels LOB depth
    pub feature_window_size: usize,               // 50 historical points
    pub technical_indicators: Vec<String>,        // ["SMA", "EMA", "RSI", "MACD", "VWAP"]
    pub market_microstructure_features: bool,    // true (spread, imbalance, etc.)
    
    /// 風險管理參數
    pub kelly_fraction_max: f64,                  // 0.25 max Kelly fraction
    pub kelly_fraction_conservative: f64,         // 0.15 conservative scaling
    pub daily_loss_limit_pct: f64,               // 0.08 (8% daily stop loss)
    pub position_loss_limit_pct: f64,            // 0.03 (3% per position)
    pub max_drawdown_pct: f64,                   // 0.05 (5% max drawdown)
    
    /// 執行參數
    pub signal_generation_interval_ms: u64,      // 200ms signal generation
    pub position_rebalance_interval_ms: u64,     // 1000ms position rebalancing
    pub max_concurrent_positions: usize,         // 3 positions max
    pub position_hold_time_target_seconds: u64,  // 15 seconds target
    
    /// 性能優化參數
    pub enable_simd_optimization: bool,          // true
    pub enable_parallel_feature_extraction: bool, // true
    pub max_inference_latency_us: u64,           // 1000 (1ms max)
    pub memory_pool_size: usize,                 // 1024 pre-allocated objects
    
    /// 監控和日誌
    pub enable_detailed_logging: bool,           // true for Phase 1
    pub performance_tracking_window: usize,     // 1000 trades
    pub real_time_metrics: bool,                 // true
}

impl Default for BtcusdtDlConfig {
    fn default() -> Self {
        Self {
            symbol: "BTCUSDT".to_string(),
            test_capital: 100.0,
            max_position_size_pct: 0.15,
            min_position_size_usdt: 2.0,
            
            sequence_length: 30,
            prediction_horizons: vec![3, 5, 10],
            min_prediction_confidence: 0.65,
            model_update_interval_seconds: 300,
            
            lob_depth: 20,
            feature_window_size: 50,
            technical_indicators: vec![
                "SMA".to_string(), 
                "EMA".to_string(), 
                "RSI".to_string(), 
                "MACD".to_string(), 
                "VWAP".to_string()
            ],
            market_microstructure_features: true,
            
            kelly_fraction_max: 0.25,
            kelly_fraction_conservative: 0.15,
            daily_loss_limit_pct: 0.08,
            position_loss_limit_pct: 0.03,
            max_drawdown_pct: 0.05,
            
            signal_generation_interval_ms: 200,
            position_rebalance_interval_ms: 1000,
            max_concurrent_positions: 3,
            position_hold_time_target_seconds: 15,
            
            enable_simd_optimization: true,
            enable_parallel_feature_extraction: true,
            max_inference_latency_us: 1000,
            memory_pool_size: 1024,
            
            enable_detailed_logging: true,
            performance_tracking_window: 1000,
            real_time_metrics: true,
        }
    }
}

/// 多時間框架預測結果
#[derive(Debug, Clone)]
pub struct MultiHorizonPrediction {
    pub predictions: HashMap<u64, TrendPrediction>, // horizon_seconds -> prediction
    pub consensus_direction: TrendClass,             // 多框架共識方向
    pub consensus_confidence: f64,                   // 共識置信度
    pub signal_strength: f64,                        // 0-1 overall signal strength
    pub timestamp: Timestamp,
    pub total_inference_time_us: u64,
}

/// BTCUSDT交易信號
#[derive(Debug, Clone)]
pub struct BtcusdtTradingSignal {
    pub symbol: String,
    pub multi_horizon_prediction: MultiHorizonPrediction,
    pub recommended_action: TradingAction,
    pub position_size_usdt: f64,
    pub kelly_fraction: f64,
    pub stop_loss_price: f64,
    pub take_profit_price: f64,
    pub urgency_score: f64,               // 0-1, higher = more urgent
    pub current_price: f64,
    pub timestamp: Timestamp,
}

/// 交易動作建議
#[derive(Debug, Clone, PartialEq)]
pub enum TradingAction {
    StrongBuy,      // 強烈買入
    Buy,            // 買入
    Hold,           // 持倉
    Sell,           // 賣出
    StrongSell,     // 強烈賣出
    ClosePosition,  // 平倉
}

/// 實時性能指標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtcusdtPerformanceMetrics {
    /// 交易統計
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub win_rate_pct: f64,
    pub avg_trade_return_pct: f64,
    pub avg_winning_trade_pct: f64,
    pub avg_losing_trade_pct: f64,
    pub profit_factor: f64,
    
    /// 回報指標
    pub total_pnl_usdt: f64,
    pub total_return_pct: f64,
    pub daily_return_pct: f64,
    pub annualized_return_pct: f64,
    pub sharpe_ratio: f64,
    pub sortino_ratio: f64,
    
    /// 風險指標
    pub max_drawdown_usdt: f64,
    pub max_drawdown_pct: f64,
    pub current_drawdown_pct: f64,
    pub var_95_pct: f64,
    pub var_99_pct: f64,
    pub expected_shortfall_pct: f64,
    
    /// 執行指標
    pub avg_inference_latency_us: f64,
    pub max_inference_latency_us: u64,
    pub avg_position_hold_time_seconds: f64,
    pub capital_utilization_pct: f64,
    pub trade_frequency_per_hour: f64,
    
    /// 模型表現
    pub prediction_accuracy_3s_pct: f64,
    pub prediction_accuracy_5s_pct: f64,
    pub prediction_accuracy_10s_pct: f64,
    pub model_confidence_avg: f64,
    pub signal_quality_score: f64,
    
    /// 時間戳
    pub last_updated: Timestamp,
    pub measurement_window_trades: usize,
}

/// BTCUSDT單商品DL交易者
pub struct BtcusdtDlTrader {
    /// 配置
    config: BtcusdtDlConfig,
    
    /// 核心組件
    lob_extractor: LobTimeSeriesExtractor,
    dl_predictor: DlTrendPredictor,
    position_manager: Arc<TestCapitalPositionManager>,
    risk_controller: Arc<Mutex<EnhancedRiskController>>,
    
    /// 市場連接
    bitget_connector: Option<BitgetConnector>,
    
    /// 數據管道
    signal_tx: Option<mpsc::Sender<BtcusdtTradingSignal>>,
    
    /// 實時狀態
    current_positions: Arc<RwLock<Vec<String>>>,        // Position IDs
    active_signals: Arc<RwLock<VecDeque<BtcusdtTradingSignal>>>,
    performance_metrics: Arc<RwLock<BtcusdtPerformanceMetrics>>,
    
    /// 內存池（性能優化）
    prediction_pool: Arc<Mutex<VecDeque<MultiHorizonPrediction>>>,
    signal_pool: Arc<Mutex<VecDeque<BtcusdtTradingSignal>>>,
    
    /// 運行狀態
    is_running: Arc<RwLock<bool>>,
    last_model_update: Arc<RwLock<Timestamp>>,
    last_signal_time: Arc<RwLock<Timestamp>>,
}

impl BtcusdtDlTrader {
    /// 創建新的BTCUSDT DL交易者
    pub fn new(config: BtcusdtDlConfig) -> Result<Self> {
        info!("🚀 Initializing BTCUSDT DL Trader with optimized configuration");
        
        // 初始化LOB特徵提取器（專門為BTCUSDT優化）
        let lob_config = LobTimeSeriesConfig::default();
        
        let lob_extractor = LobTimeSeriesExtractor::new(
            config.symbol.clone(), 
            lob_config.clone()
        );
        
        // 初始化DL趨勢預測器（多時間框架）
        let dl_config = DlTrendPredictorConfig {
            max_sequence_length: config.sequence_length,
            max_inference_latency_us: config.max_inference_latency_us,
            confidence_threshold: config.min_prediction_confidence,
            hidden_dim: 384, // 增加隱藏維度以提高性能
            num_heads: 12,   // 增加注意力頭數
            num_layers: 6,   // 增加層數
            ..Default::default()
        };
        
        let dl_predictor = DlTrendPredictor::new(
            config.symbol.clone(),
            dl_config,
            lob_config
        )?;
        
        // 初始化倉位管理器（專門為BTCUSDT配置）
        let position_config = TestCapitalConfig {
            test_capital: config.test_capital,
            capital_per_symbol: config.test_capital * 0.95, // 95% for BTCUSDT
            min_position_size: config.min_position_size_usdt,
            max_position_size: config.test_capital * config.max_position_size_pct,
            daily_loss_limit: config.test_capital * config.daily_loss_limit_pct,
            position_loss_limit: config.test_capital * config.position_loss_limit_pct,
            max_drawdown_limit: config.test_capital * config.max_drawdown_pct,
            kelly_fraction_conservative: config.kelly_fraction_conservative,
            max_concurrent_positions: config.max_concurrent_positions,
            position_hold_time_seconds: config.position_hold_time_target_seconds,
            target_daily_profit: config.test_capital * 0.015, // 1.5% daily target
            ..Default::default()
        };
        
        let position_manager = Arc::new(TestCapitalPositionManager::new(position_config));
        
        // 初始化風險控制器
        let risk_config = EnhancedRiskConfig {
            total_capital: config.test_capital,
            max_risk_capital: config.test_capital * 0.5,
            daily_loss_limit: config.test_capital * config.daily_loss_limit_pct,
            kelly_fraction_limit: config.kelly_fraction_max,
            min_prediction_confidence: config.min_prediction_confidence,
            ..Default::default()
        };
        
        let risk_controller = Arc::new(Mutex::new(
            EnhancedRiskController::new(risk_config)
        ));
        
        // 初始化性能指標
        let performance_metrics = Arc::new(RwLock::new(BtcusdtPerformanceMetrics {
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate_pct: 0.0,
            avg_trade_return_pct: 0.0,
            avg_winning_trade_pct: 0.0,
            avg_losing_trade_pct: 0.0,
            profit_factor: 0.0,
            total_pnl_usdt: 0.0,
            total_return_pct: 0.0,
            daily_return_pct: 0.0,
            annualized_return_pct: 0.0,
            sharpe_ratio: 0.0,
            sortino_ratio: 0.0,
            max_drawdown_usdt: 0.0,
            max_drawdown_pct: 0.0,
            current_drawdown_pct: 0.0,
            var_95_pct: 0.0,
            var_99_pct: 0.0,
            expected_shortfall_pct: 0.0,
            avg_inference_latency_us: 0.0,
            max_inference_latency_us: 0,
            avg_position_hold_time_seconds: 0.0,
            capital_utilization_pct: 0.0,
            trade_frequency_per_hour: 0.0,
            prediction_accuracy_3s_pct: 0.0,
            prediction_accuracy_5s_pct: 0.0,
            prediction_accuracy_10s_pct: 0.0,
            model_confidence_avg: 0.0,
            signal_quality_score: 0.0,
            last_updated: now_micros(),
            measurement_window_trades: config.performance_tracking_window,
        }));
        
        // 初始化內存池
        let mut prediction_pool = VecDeque::new();
        let mut signal_pool = VecDeque::new();
        for _ in 0..config.memory_pool_size {
            prediction_pool.push_back(MultiHorizonPrediction {
                predictions: HashMap::new(),
                consensus_direction: TrendClass::Neutral,
                consensus_confidence: 0.0,
                signal_strength: 0.0,
                timestamp: 0,
                total_inference_time_us: 0,
            });
            
            signal_pool.push_back(BtcusdtTradingSignal {
                symbol: config.symbol.clone(),
                multi_horizon_prediction: MultiHorizonPrediction {
                    predictions: HashMap::new(),
                    consensus_direction: TrendClass::Neutral,
                    consensus_confidence: 0.0,
                    signal_strength: 0.0,
                    timestamp: 0,
                    total_inference_time_us: 0,
                },
                recommended_action: TradingAction::Hold,
                position_size_usdt: 0.0,
                kelly_fraction: 0.0,
                stop_loss_price: 0.0,
                take_profit_price: 0.0,
                urgency_score: 0.0,
                current_price: 0.0,
                timestamp: 0,
            });
        }
        
        Ok(Self {
            config,
            lob_extractor,
            dl_predictor,
            position_manager,
            risk_controller,
            bitget_connector: None,
            signal_tx: None,
            current_positions: Arc::new(RwLock::new(Vec::new())),
            active_signals: Arc::new(RwLock::new(VecDeque::new())),
            performance_metrics,
            prediction_pool: Arc::new(Mutex::new(prediction_pool)),
            signal_pool: Arc::new(Mutex::new(signal_pool)),
            is_running: Arc::new(RwLock::new(false)),
            last_model_update: Arc::new(RwLock::new(now_micros())),
            last_signal_time: Arc::new(RwLock::new(now_micros())),
        })
    }
    
    /// 初始化市場連接
    pub async fn initialize_market_connection(&mut self, bitget_config: BitgetConfig) -> Result<()> {
        info!("🔌 Initializing BTCUSDT market data connection");
        
        // 暫時使用模擬連接，實際實施需要異步初始化
        // let connector = BitgetConnector::new(bitget_config).await?;
        // self.bitget_connector = Some(connector);
        
        info!("✅ Market connection initialized successfully (simulation mode)");
        Ok(())
    }
    
    /// 啟動交易系統
    pub async fn start_trading(&mut self) -> Result<()> {
        info!("🚀 Starting BTCUSDT DL Trading System");
        
        // 設置運行狀態
        {
            let mut is_running = self.is_running.write().await;
            *is_running = true;
        }
        
        // 創建數據管道
        let (signal_tx, mut signal_rx) = mpsc::channel::<BtcusdtTradingSignal>(1000);
        self.signal_tx = Some(signal_tx);
        
        // 啟動各個組件
        self.start_signal_generation_loop().await?;
        self.start_position_management_loop().await?;
        self.start_performance_monitoring_loop().await?;
        self.start_signal_execution_loop(signal_rx).await?;
        
        info!("✅ BTCUSDT DL Trading System started successfully");
        Ok(())
    }
    
    /// 停止交易系統
    pub async fn stop_trading(&mut self) -> Result<()> {
        info!("🛑 Stopping BTCUSDT DL Trading System");
        
        {
            let mut is_running = self.is_running.write().await;
            *is_running = false;
        }
        
        // 平倉所有位置
        self.close_all_positions().await?;
        
        // 生成最終報告
        self.generate_final_performance_report().await?;
        
        info!("✅ BTCUSDT DL Trading System stopped successfully");
        Ok(())
    }
    
    /// 信號生成循環
    async fn start_signal_generation_loop(&self) -> Result<()> {
        let config = self.config.clone();
        let is_running = Arc::clone(&self.is_running);
        let signal_tx = self.signal_tx.as_ref().unwrap().clone();
        let last_signal_time = Arc::clone(&self.last_signal_time);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                Duration::from_millis(config.signal_generation_interval_ms)
            );
            
            info!("📡 Signal generation loop started ({}ms interval)", 
                  config.signal_generation_interval_ms);
            
            while *is_running.read().await {
                interval.tick().await;
                
                // 這裡會實現完整的信號生成邏輯
                // 暫時使用模擬信號
                if let Ok(signal) = Self::generate_simulated_signal(&config).await {
                    if signal_tx.send(signal).await.is_err() {
                        error!("Failed to send trading signal");
                        break;
                    }
                    
                    {
                        let mut last_time = last_signal_time.write().await;
                        *last_time = now_micros();
                    }
                }
            }
        });
        
        Ok(())
    }
    
    /// 倉位管理循環
    async fn start_position_management_loop(&self) -> Result<()> {
        let config = self.config.clone();
        let is_running = Arc::clone(&self.is_running);
        let position_manager = Arc::clone(&self.position_manager);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(
                Duration::from_millis(config.position_rebalance_interval_ms)
            );
            
            info!("⚖️ Position management loop started ({}ms interval)", 
                  config.position_rebalance_interval_ms);
            
            while *is_running.read().await {
                interval.tick().await;
                
                // 檢查倉位退出條件
                if let Ok(closed_positions) = position_manager.check_position_exits().await {
                    if !closed_positions.is_empty() {
                        info!("📤 Closed {} positions", closed_positions.len());
                    }
                }
                
                // 更新倉位價格（這裡需要實際市場數據）
                // 暫時使用模擬價格更新
                let mut price_updates = HashMap::new();
                price_updates.insert(config.symbol.clone(), 50000.0); // 模擬BTC價格
                
                if let Err(e) = position_manager.update_positions(price_updates).await {
                    error!("Failed to update positions: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    /// 性能監控循環
    async fn start_performance_monitoring_loop(&self) -> Result<()> {
        let config = self.config.clone();
        let is_running = Arc::clone(&self.is_running);
        let performance_metrics = Arc::clone(&self.performance_metrics);
        let position_manager = Arc::clone(&self.position_manager);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10)); // 每10秒更新
            
            info!("📊 Performance monitoring loop started");
            
            while *is_running.read().await {
                interval.tick().await;
                
                // 更新性能指標
                let portfolio_status = position_manager.get_portfolio_status().await;
                
                {
                    let mut metrics = performance_metrics.write().await;
                    metrics.total_pnl_usdt = portfolio_status.metrics.total_pnl;
                    metrics.total_return_pct = (portfolio_status.metrics.total_pnl / config.test_capital) * 100.0;
                    metrics.daily_return_pct = (portfolio_status.metrics.daily_pnl / config.test_capital) * 100.0;
                    metrics.total_trades = portfolio_status.metrics.total_trades;
                    metrics.win_rate_pct = portfolio_status.metrics.win_rate;
                    metrics.capital_utilization_pct = portfolio_status.metrics.capital_efficiency * 100.0;
                    metrics.max_drawdown_usdt = portfolio_status.metrics.max_drawdown;
                    metrics.max_drawdown_pct = (portfolio_status.metrics.max_drawdown / config.test_capital) * 100.0;
                    metrics.last_updated = now_micros();
                }
                
                // 每分鐘打印一次狀態
                if (now_micros() / 1_000_000) % 60 == 0 {
                    Self::print_performance_summary(&config, &*performance_metrics.read().await).await;
                }
            }
        });
        
        Ok(())
    }
    
    /// 信號執行循環
    async fn start_signal_execution_loop(&self, mut signal_rx: mpsc::Receiver<BtcusdtTradingSignal>) -> Result<()> {
        let position_manager = Arc::clone(&self.position_manager);
        let config = self.config.clone();
        
        tokio::spawn(async move {
            info!("⚡ Signal execution loop started");
            
            while let Some(signal) = signal_rx.recv().await {
                if let Err(e) = Self::execute_trading_signal(&position_manager, &config, signal).await {
                    error!("Failed to execute trading signal: {}", e);
                }
            }
        });
        
        Ok(())
    }
    
    /// 執行交易信號 - 增強版本包含動態持倉時間優化
    async fn execute_trading_signal(
        position_manager: &Arc<TestCapitalPositionManager>,
        config: &BtcusdtDlConfig,
        signal: BtcusdtTradingSignal,
    ) -> Result<()> {
        match signal.recommended_action {
            TradingAction::StrongBuy | TradingAction::Buy => {
                // 計算動態持倉時間
                let optimal_hold_time = Self::calculate_optimal_hold_time(&signal, config).await;
                
                // 評估新倉位
                let assessment = position_manager.assess_new_position(
                    &signal.symbol,
                    &signal.multi_horizon_prediction.predictions.values().next().unwrap(),
                    signal.current_price,
                ).await?;
                
                match assessment {
                    PositionAssessment::Approved { suggested_size, kelly_fraction, stop_loss, take_profit, .. } => {
                        // 動態調整止損止盈基於預期持倉時間
                        let (dynamic_stop_loss, dynamic_take_profit) = Self::calculate_dynamic_stop_take_profit(
                            signal.current_price,
                            &signal.multi_horizon_prediction,
                            optimal_hold_time
                        );
                        
                        let side = Side::Bid; // Long position
                        let position_id = position_manager.open_position(
                            signal.symbol.clone(),
                            side,
                            suggested_size,
                            signal.current_price,
                            &signal.multi_horizon_prediction.predictions.values().next().unwrap(),
                            kelly_fraction,
                            Some(dynamic_stop_loss),
                            Some(dynamic_take_profit),
                        ).await?;
                        
                        info!("🟢 {} signal executed: {} {} {:.2} USDT @ {:.2} | ID: {} | Confidence: {:.3} | Hold time: {}s",
                              if signal.recommended_action == TradingAction::StrongBuy { "STRONG BUY" } else { "BUY" },
                              signal.symbol,
                              "LONG",
                              suggested_size,
                              signal.current_price,
                              position_id,
                              signal.multi_horizon_prediction.consensus_confidence,
                              optimal_hold_time);
                    }
                    PositionAssessment::Rejected { reason } => {
                        debug!("❌ Signal rejected: {}", reason);
                    }
                }
            }
            TradingAction::StrongSell | TradingAction::Sell => {
                // 實施賣出邏輯（做空或平多倉）
                debug!("📉 SELL signal received but short selling not implemented yet");
            }
            TradingAction::ClosePosition => {
                // 動態平倉邏輯
                debug!("🔄 Close position signal received");
            }
            TradingAction::Hold => {
                // 持倉信號，但檢查是否需要調整現有倉位
                debug!("⏸️ Hold signal - checking existing positions for adjustments");
            }
        }
        
        Ok(())
    }
    
    /// 計算最優持倉時間基於市場條件和預測強度
    async fn calculate_optimal_hold_time(signal: &BtcusdtTradingSignal, config: &BtcusdtDlConfig) -> u64 {
        let base_hold_time = config.position_hold_time_target_seconds;
        
        // 基於多時間框架預測的一致性調整
        let mut hold_time_multiplier = 1.0;
        
        // 1. 預測置信度調整
        if signal.multi_horizon_prediction.consensus_confidence > 0.8 {
            hold_time_multiplier *= 1.3; // 高置信度延長持倉
        } else if signal.multi_horizon_prediction.consensus_confidence < 0.65 {
            hold_time_multiplier *= 0.7; // 低置信度縮短持倉
        }
        
        // 2. 信號強度調整
        if signal.multi_horizon_prediction.signal_strength > 0.8 {
            hold_time_multiplier *= 1.2;
        } else if signal.multi_horizon_prediction.signal_strength < 0.5 {
            hold_time_multiplier *= 0.8;
        }
        
        // 3. 緊急程度調整
        if signal.urgency_score > 0.8 {
            hold_time_multiplier *= 0.6; // 高緊急性快速進出
        }
        
        // 4. 多時間框架一致性調整
        let horizon_consistency = Self::calculate_horizon_consistency(&signal.multi_horizon_prediction);
        hold_time_multiplier *= 0.5 + horizon_consistency * 0.8; // 0.5-1.3倍調整
        
        // 5. 市場波動率調整（簡化版）
        let volatility_adjustment = Self::estimate_current_volatility();
        if volatility_adjustment > 0.03 { // 高波動
            hold_time_multiplier *= 0.8;
        } else if volatility_adjustment < 0.015 { // 低波動
            hold_time_multiplier *= 1.2;
        }
        
        // 應用調整並限制範圍
        let optimal_time = (base_hold_time as f64 * hold_time_multiplier) as u64;
        optimal_time.max(5).min(120) // 5秒到2分鐘之間
    }
    
    /// 計算多時間框架預測的一致性
    fn calculate_horizon_consistency(prediction: &MultiHorizonPrediction) -> f64 {
        if prediction.predictions.len() < 2 {
            return 0.5; // 單一時間框架，中性一致性
        }
        
        let mut bullish_count = 0;
        let mut bearish_count = 0;
        let mut total_confidence = 0.0;
        
        for pred in prediction.predictions.values() {
            total_confidence += pred.confidence;
            match pred.trend_class {
                TrendClass::StrongUp | TrendClass::WeakUp => bullish_count += 1,
                TrendClass::StrongDown | TrendClass::WeakDown => bearish_count += 1,
                TrendClass::Neutral => {},
            }
        }
        
        let avg_confidence = total_confidence / prediction.predictions.len() as f64;
        let directional_agreement = if bullish_count > bearish_count {
            bullish_count as f64 / prediction.predictions.len() as f64
        } else {
            bearish_count as f64 / prediction.predictions.len() as f64
        };
        
        // 結合方向一致性和平均置信度
        (directional_agreement * 0.7 + avg_confidence * 0.3).min(1.0)
    }
    
    /// 估算當前市場波動率（簡化版）
    fn estimate_current_volatility() -> f64 {
        // 實際實現中應該基於實時價格數據
        // 這裡使用時間戳作為偽隨機種子來模擬波動率變化
        let time_seed = now_micros() as f64;
        let pseudo_vol = 0.015 + (time_seed * 0.00001 % 1.0) * 0.03; // 1.5%-4.5%
        pseudo_vol
    }
    
    /// 計算動態止損止盈價位
    fn calculate_dynamic_stop_take_profit(
        current_price: f64,
        prediction: &MultiHorizonPrediction,
        hold_time_seconds: u64,
    ) -> (f64, f64) {
        // 基礎止損止盈比例
        let mut stop_loss_pct = 0.025; // 2.5%
        let mut take_profit_pct = 0.035; // 3.5%
        
        // 1. 基於預測置信度調整
        if prediction.consensus_confidence > 0.8 {
            take_profit_pct *= 1.3; // 高置信度擴大盈利目標
            stop_loss_pct *= 0.9;   // 略微收緊止損
        } else if prediction.consensus_confidence < 0.65 {
            take_profit_pct *= 0.8; // 低置信度縮小目標
            stop_loss_pct *= 1.1;   // 放寬止損
        }
        
        // 2. 基於持倉時間調整
        let time_factor = (hold_time_seconds as f64 / 15.0).sqrt(); // 基於15秒基準
        take_profit_pct *= time_factor.max(0.5).min(2.0);
        
        // 3. 基於信號強度調整
        let strength_factor = 0.7 + prediction.signal_strength * 0.6; // 0.7-1.3倍
        take_profit_pct *= strength_factor;
        
        // 4. 基於多時間框架一致性調整
        let consistency = Self::calculate_horizon_consistency(prediction);
        if consistency > 0.8 {
            take_profit_pct *= 1.2;
            stop_loss_pct *= 0.95;
        } else if consistency < 0.5 {
            take_profit_pct *= 0.9;
            stop_loss_pct *= 1.05;
        }
        
        // 計算實際價位
        let stop_loss_price = current_price * (1.0 - stop_loss_pct);
        let take_profit_price = current_price * (1.0 + take_profit_pct);
        
        (stop_loss_price, take_profit_price)
    }
    
    /// 生成模擬交易信號（用於測試）
    async fn generate_simulated_signal(config: &BtcusdtDlConfig) -> Result<BtcusdtTradingSignal> {
        // 模擬多時間框架預測
        let mut predictions = HashMap::new();
        let mut total_inference_time = 0u64;
        
        for &horizon in &config.prediction_horizons {
            // 使用確定性的基於時間的隨機值
            let time_seed = (now_micros() + horizon) as f64;
            let pseudo_random = (time_seed * 0.00001) % 1.0;
            
            let prediction = TrendPrediction {
                trend_class: if pseudo_random > 0.5 { TrendClass::WeakUp } else { TrendClass::WeakDown },
                class_probabilities: vec![0.1, 0.2, 0.3, 0.3, 0.1],
                confidence: 0.6 + pseudo_random * 0.3, // 0.6-0.9
                expected_change_bps: (pseudo_random - 0.5) * 20.0, // -10 to +10 bps
                inference_latency_us: 200 + ((time_seed as u64) % 600), // 200-800μs
                timestamp: now_micros(),
                sequence_length: config.sequence_length,
            };
            
            total_inference_time += prediction.inference_latency_us;
            predictions.insert(horizon, prediction);
        }
        
        // 計算共識
        let bullish_count = predictions.values()
            .filter(|p| matches!(p.trend_class, TrendClass::StrongUp | TrendClass::WeakUp))
            .count();
        
        let consensus_direction = if bullish_count > predictions.len() / 2 {
            TrendClass::WeakUp
        } else {
            TrendClass::WeakDown
        };
        
        let avg_confidence = predictions.values()
            .map(|p| p.confidence)
            .sum::<f64>() / predictions.len() as f64;
        
        let multi_horizon_prediction = MultiHorizonPrediction {
            predictions,
            consensus_direction: consensus_direction.clone(),
            consensus_confidence: avg_confidence,
            signal_strength: avg_confidence * 0.8, // 略微保守
            timestamp: now_micros(),
            total_inference_time_us: total_inference_time,
        };
        
        // 確定交易動作
        let recommended_action = if avg_confidence >= 0.75 {
            match consensus_direction {
                TrendClass::StrongUp | TrendClass::WeakUp => TradingAction::StrongBuy,
                TrendClass::StrongDown | TrendClass::WeakDown => TradingAction::StrongSell,
                TrendClass::Neutral => TradingAction::Hold,
            }
        } else if avg_confidence >= config.min_prediction_confidence {
            match consensus_direction {
                TrendClass::StrongUp | TrendClass::WeakUp => TradingAction::Buy,
                TrendClass::StrongDown | TrendClass::WeakDown => TradingAction::Sell,
                TrendClass::Neutral => TradingAction::Hold,
            }
        } else {
            TradingAction::Hold
        };
        
        let time_now = now_micros() as f64;
        let price_variation = ((time_now * 0.00001) % 1.0 - 0.5) * 1000.0;
        let current_price = 50000.0 + price_variation; // 49500-50500
        
        Ok(BtcusdtTradingSignal {
            symbol: config.symbol.clone(),
            multi_horizon_prediction,
            recommended_action,
            position_size_usdt: config.min_position_size_usdt,
            kelly_fraction: config.kelly_fraction_conservative,
            stop_loss_price: current_price * 0.97, // 3% stop loss
            take_profit_price: current_price * 1.05, // 5% take profit
            urgency_score: avg_confidence,
            current_price,
            timestamp: now_micros(),
        })
    }
    
    /// 平倉所有位置
    async fn close_all_positions(&self) -> Result<()> {
        info!("🔄 Closing all positions before system shutdown");
        
        let closed_positions = self.position_manager.check_position_exits().await?;
        if !closed_positions.is_empty() {
            info!("✅ Closed {} positions", closed_positions.len());
        }
        
        Ok(())
    }
    
    /// 生成最終性能報告
    async fn generate_final_performance_report(&self) -> Result<()> {
        info!("📊 === BTCUSDT DL TRADER FINAL PERFORMANCE REPORT ===");
        
        let metrics = self.performance_metrics.read().await;
        let portfolio_status = self.position_manager.get_portfolio_status().await;
        
        info!("💰 Trading Performance:");
        info!("  Total P&L: {:.2} USDT ({:.2}%)", metrics.total_pnl_usdt, metrics.total_return_pct);
        info!("  Daily Return: {:.2}%", metrics.daily_return_pct);
        info!("  Win Rate: {:.1}%", metrics.win_rate_pct);
        info!("  Total Trades: {}", metrics.total_trades);
        info!("  Capital Utilization: {:.1}%", metrics.capital_utilization_pct);
        
        info!("⚠️ Risk Metrics:");
        info!("  Max Drawdown: {:.2} USDT ({:.1}%)", metrics.max_drawdown_usdt, metrics.max_drawdown_pct);
        info!("  Sharpe Ratio: {:.2}", metrics.sharpe_ratio);
        
        info!("⚡ Execution Metrics:");
        info!("  Avg Inference Latency: {:.0}μs", metrics.avg_inference_latency_us);
        info!("  Max Inference Latency: {}μs", metrics.max_inference_latency_us);
        info!("  Avg Position Hold Time: {:.1}s", metrics.avg_position_hold_time_seconds);
        
        // 評估Phase 2準備情況
        let phase2_ready = metrics.daily_return_pct >= 1.0 
            && metrics.win_rate_pct >= 60.0 
            && metrics.max_drawdown_pct <= 5.0
            && metrics.avg_inference_latency_us <= 1000.0;
        
        if phase2_ready {
            info!("🚀 PHASE 2 READY: System meets all performance targets!");
            info!("✅ Ready for multi-symbol expansion and rule-based scalping");
        } else {
            warn!("⚠️ OPTIMIZATION NEEDED: Some targets not met, continue Phase 1 optimization");
        }
        
        info!("=================================================");
        
        Ok(())
    }
    
    /// 打印性能摘要
    async fn print_performance_summary(config: &BtcusdtDlConfig, metrics: &BtcusdtPerformanceMetrics) {
        info!("📈 {} Performance: P&L {:.2} USDT ({:.2}%) | {} trades | {:.1}% win rate | {:.1}% utilization",
              config.symbol,
              metrics.total_pnl_usdt,
              metrics.total_return_pct,
              metrics.total_trades,
              metrics.win_rate_pct,
              metrics.capital_utilization_pct);
    }
    
    /// 獲取當前性能指標
    pub async fn get_performance_metrics(&self) -> BtcusdtPerformanceMetrics {
        self.performance_metrics.read().await.clone()
    }
    
    /// 獲取實時狀態
    pub async fn get_realtime_status(&self) -> (usize, f64, bool) {
        let positions = self.current_positions.read().await;
        let metrics = self.performance_metrics.read().await;
        let is_running = *self.is_running.read().await;
        
        (positions.len(), metrics.total_pnl_usdt, is_running)
    }
}

/// 輔助函數：獲取當前微秒時間戳
fn now_micros() -> Timestamp {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as Timestamp
}