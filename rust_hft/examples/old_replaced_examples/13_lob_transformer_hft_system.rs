/*!
 * LOB Transformer 集成 HFT 交易系統
 * 
 * 完整集成LOB Transformer到HFT系統，提供：
 * - 實時LOB數據處理和特徵提取
 * - LOB Transformer多時間範圍預測
 * - 智能交易信號生成和風險管理
 * - 性能優化和延遲監控
 * - 自動模型評估和部署決策
 * 
 * 執行方式：
 * cargo run --example lob_transformer_hft_system -- --mode dry-run --model-path models/lob_transformer.safetensors
 * cargo run --example lob_transformer_hft_system -- --mode live --model-path models/lob_transformer.safetensors
 */

use rust_hft::{
    core::{types::*, orderbook::OrderBook},
    ml::{features::FeatureExtractor},
    integrations::bitget_connector::*,
    utils::performance::*,
};
use anyhow::Result;
use candle_core::{Device, Tensor, DType};
use candle_nn::{VarBuilder, VarMap};
use std::collections::{HashMap, VecDeque};
use tracing::{info, warn, error, debug};
use clap::{Parser, ValueEnum};
use serde::{Serialize, Deserialize};
use tokio::time::{Duration, Instant};
use std::sync::{Arc, Mutex, RwLock};
use std::fs;

#[derive(Parser, Debug)]
#[command(name = "lob_transformer_hft_system")]
#[command(about = "LOB Transformer Integrated HFT Trading System")]
struct Args {
    #[arg(short, long, value_enum, default_value_t = TradingMode::DryRun)]
    mode: TradingMode,
    
    #[arg(short, long, default_value_t = String::from("BTCUSDT"))]
    symbol: String,
    
    #[arg(long, default_value_t = String::from("models/lob_transformer.safetensors"))]
    model_path: String,
    
    #[arg(long, default_value_t = 1000.0)]
    initial_capital: f64,
    
    #[arg(long, default_value_t = 0.05)]
    max_position_pct: f64,
    
    #[arg(long, default_value_t = 0.65)]
    confidence_threshold: f64,
    
    #[arg(long, default_value_t = false)]
    enable_auto_retraining: bool,
    
    #[arg(long, default_value_t = false)]
    use_gpu: bool,
}

#[derive(Clone, ValueEnum, Debug)]
enum TradingMode {
    DryRun,    // 模擬交易，不實際下單
    Live,      // 實時交易
    Backtest,  // 歷史回測
}

/// LOB Transformer 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTransformerConfig {
    pub lob_features: usize,
    pub market_features: usize,
    pub d_model: usize,
    pub n_heads: usize,
    pub n_layers: usize,
    pub sequence_length: usize,
    pub prediction_horizons: Vec<u64>,
    pub num_classes: usize,
}

impl Default for LobTransformerConfig {
    fn default() -> Self {
        Self {
            lob_features: 40,
            market_features: 10,
            d_model: 256,
            n_heads: 8,
            n_layers: 6,
            sequence_length: 50,
            prediction_horizons: vec![1, 3, 5, 10],
            num_classes: 3,
        }
    }
}

/// 多時間範圍預測結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiHorizonPrediction {
    pub timestamp: Timestamp,
    pub symbol: String,
    pub predictions: HashMap<u64, PredictionResult>, // horizon -> prediction
    pub confidence: f64,
    pub model_latency_us: u64,
    pub data_quality_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionResult {
    pub direction: i32,        // -1: down, 0: neutral, 1: up
    pub probability: f64,      // 預測概率
    pub expected_return: f64,  // 預期收益
    pub risk_score: f64,       // 風險評分
}

/// LOB Transformer 交易信號
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobTransformerSignal {
    pub timestamp: Timestamp,
    pub symbol: String,
    pub signal_type: SignalType,
    pub confidence: f64,
    pub position_size: f64,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub signal_source: String,
    pub model_predictions: MultiHorizonPrediction,
    pub risk_metrics: RiskMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalType {
    Buy,
    Sell,
    Hold,
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskMetrics {
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub var_95: f64,
    pub expected_shortfall: f64,
    pub kelly_fraction: f64,
}

/// 模擬LOB Transformer模型（簡化版本）
pub struct MockLobTransformerModel {
    config: LobTransformerConfig,
    device: Device,
    _var_map: VarMap,
    inference_count: Arc<Mutex<u64>>,
    total_latency_us: Arc<Mutex<u64>>,
}

impl MockLobTransformerModel {
    pub fn new(config: LobTransformerConfig, device: Device) -> Result<Self> {
        let var_map = VarMap::new();
        
        Ok(Self {
            config,
            device,
            _var_map: var_map,
            inference_count: Arc::new(Mutex::new(0)),
            total_latency_us: Arc::new(Mutex::new(0)),
        })
    }
    
    pub fn predict(&self, _lob_features: &[Vec<f64>], _market_features: &[Vec<f64>]) -> Result<MultiHorizonPrediction> {
        let start = now_micros();
        
        // 模擬推理過程（實際實現中會使用真正的模型）
        let mut predictions = HashMap::new();
        
        for &horizon in &self.config.prediction_horizons {
            // 簡化的預測邏輯
            let base_prob = 0.5 + (fastrand::f64() - 0.5) * 0.4; // 0.3-0.7
            let direction = if base_prob > 0.6 {
                1  // Up
            } else if base_prob < 0.4 {
                -1 // Down
            } else {
                0  // Neutral
            };
            
            let prediction = PredictionResult {
                direction,
                probability: base_prob,
                expected_return: (fastrand::f64() - 0.5) * 0.002, // ±0.1%
                risk_score: fastrand::f64() * 0.5, // 0-0.5
            };
            
            predictions.insert(horizon, prediction);
        }
        
        let latency = now_micros() - start;
        
        // 更新統計信息
        {
            let mut count = self.inference_count.lock().unwrap();
            let mut total_lat = self.total_latency_us.lock().unwrap();
            *count += 1;
            *total_lat += latency;
        }
        
        // 計算整體置信度
        let overall_confidence = predictions.values()
            .map(|p| p.probability)
            .fold(0.0, |acc, p| acc + (p - 0.5).abs()) / predictions.len() as f64;
        
        Ok(MultiHorizonPrediction {
            timestamp: now_micros(),
            symbol: "BTCUSDT".to_string(),
            predictions,
            confidence: overall_confidence,
            model_latency_us: latency,
            data_quality_score: 0.95, // 簡化
        })
    }
    
    pub fn get_performance_stats(&self) -> (u64, f64) {
        let count = *self.inference_count.lock().unwrap();
        let total_lat = *self.total_latency_us.lock().unwrap();
        let avg_lat = if count > 0 { total_lat as f64 / count as f64 } else { 0.0 };
        (count, avg_lat)
    }
}

/// LOB Transformer 交易引擎
pub struct LobTransformerTradingEngine {
    config: LobTransformerConfig,
    model: MockLobTransformerModel,
    feature_extractor: FeatureExtractor,
    
    // 數據管理
    lob_history: VecDeque<(OrderBook, Timestamp)>,
    feature_history: VecDeque<(Vec<f64>, Vec<f64>, Timestamp)>, // (lob_features, market_features, timestamp)
    
    // 交易狀態
    current_position: f64,
    current_capital: f64,
    initial_capital: f64,
    max_position_pct: f64,
    
    // 信號管理
    signal_history: VecDeque<LobTransformerSignal>,
    active_signals: HashMap<String, LobTransformerSignal>,
    
    // 風險管理
    daily_pnl: f64,
    max_drawdown: f64,
    peak_capital: f64,
    
    // 性能監控
    prediction_latencies: VecDeque<u64>,
    signal_generation_count: u64,
    successful_predictions: u64,
    
    // 配置
    confidence_threshold: f64,
    trading_mode: TradingMode,
}

impl LobTransformerTradingEngine {
    pub fn new(
        args: &Args,
        model_config: LobTransformerConfig,
        device: Device
    ) -> Result<Self> {
        let model = MockLobTransformerModel::new(model_config.clone(), device)?;
        let feature_extractor = FeatureExtractor::new(model_config.sequence_length);
        
        Ok(Self {
            config: model_config.clone(),
            model,
            feature_extractor,
            lob_history: VecDeque::with_capacity(model_config.sequence_length * 2),
            feature_history: VecDeque::with_capacity(model_config.sequence_length),
            current_position: 0.0,
            current_capital: args.initial_capital,
            initial_capital: args.initial_capital,
            max_position_pct: args.max_position_pct,
            signal_history: VecDeque::with_capacity(1000),
            active_signals: HashMap::new(),
            daily_pnl: 0.0,
            max_drawdown: 0.0,
            peak_capital: args.initial_capital,
            prediction_latencies: VecDeque::with_capacity(1000),
            signal_generation_count: 0,
            successful_predictions: 0,
            confidence_threshold: args.confidence_threshold,
            trading_mode: args.mode.clone(),
        })
    }
    
    /// 處理新的訂單簿數據
    pub async fn process_orderbook_update(&mut self, orderbook: OrderBook, timestamp: Timestamp) -> Result<Option<LobTransformerSignal>> {
        // 1. 添加到歷史記錄
        self.lob_history.push_back((orderbook.clone(), timestamp));
        if self.lob_history.len() > self.config.sequence_length * 2 {
            self.lob_history.pop_front();
        }
        
        // 2. 提取特徵
        let features = self.feature_extractor.extract_features(&orderbook, 0, timestamp)?;
        
        // 3. 構建LOB和市場特徵向量
        let lob_features = vec![
            features.mid_price.0,
            features.spread_bps,
            features.bid_depth_l5,
            features.ask_depth_l5,
            features.bid_depth_l10,
            features.ask_depth_l10,
            features.obi_l1,
            features.obi_l5,
            features.obi_l10,
            features.obi_l20,
            features.depth_imbalance_l5,
            features.depth_imbalance_l10,
            features.depth_imbalance_l20,
            features.bid_slope,
            features.ask_slope,
            features.price_momentum,
            features.volume_imbalance,
            features.microprice,
            features.vwap,
            features.realized_volatility,
            // 擴展到40個特徵
            features.effective_spread,
            features.market_impact,
            features.liquidity_score,
            features.trade_intensity,
            features.momentum_5_tick,
            features.momentum_10_tick,
            features.volatility_5_tick,
            features.volatility_10_tick,
            features.data_quality_score,
            if features.is_valid { 1.0 } else { 0.0 },
            // 填充剩餘特徵
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        
        let market_features = vec![
            features.effective_spread,
            features.market_impact,
            features.liquidity_score,
            features.trade_intensity,
            features.momentum_5_tick,
            features.momentum_10_tick,
            features.volatility_5_tick,
            features.volatility_10_tick,
            features.data_quality_score,
            if features.is_valid { 1.0 } else { 0.0 },
        ];
        
        self.feature_history.push_back((lob_features.clone(), market_features.clone(), timestamp));
        if self.feature_history.len() > self.config.sequence_length {
            self.feature_history.pop_front();
        }
        
        // 4. 如果有足夠的歷史數據，進行預測
        if self.feature_history.len() >= self.config.sequence_length {
            return self.generate_trading_signal(timestamp).await;
        }
        
        Ok(None)
    }
    
    /// 生成交易信號
    async fn generate_trading_signal(&mut self, timestamp: Timestamp) -> Result<Option<LobTransformerSignal>> {
        // 準備模型輸入
        let sequence_lob: Vec<Vec<f64>> = self.feature_history.iter()
            .take(self.config.sequence_length)
            .map(|(lob, _, _)| lob.clone())
            .collect();
        
        let sequence_market: Vec<Vec<f64>> = self.feature_history.iter()
            .take(self.config.sequence_length)
            .map(|(_, market, _)| market.clone())
            .collect();
        
        // 模型預測
        let prediction = self.model.predict(&sequence_lob, &sequence_market)?;
        self.prediction_latencies.push_back(prediction.model_latency_us);
        if self.prediction_latencies.len() > 1000 {
            self.prediction_latencies.pop_front();
        }
        
        // 分析預測結果
        let signal = self.analyze_prediction_and_generate_signal(prediction, timestamp)?;
        
        if let Some(ref signal) = signal {
            self.signal_generation_count += 1;
            self.signal_history.push_back(signal.clone());
            if self.signal_history.len() > 1000 {
                self.signal_history.pop_front();
            }
            
            info!("🎯 LOB Transformer信號: {:?}, 置信度: {:.3}, 倉位: {:.2}%", 
                  signal.signal_type, signal.confidence, signal.position_size * 100.0);
        }
        
        Ok(signal)
    }
    
    /// 分析預測結果並生成交易信號
    fn analyze_prediction_and_generate_signal(
        &mut self,
        prediction: MultiHorizonPrediction,
        timestamp: Timestamp
    ) -> Result<Option<LobTransformerSignal>> {
        // 多時間範圍預測融合
        let mut weighted_direction = 0.0;
        let mut total_weight = 0.0;
        let mut avg_confidence = 0.0;
        
        for (&horizon, pred) in &prediction.predictions {
            let weight = match horizon {
                1 => 0.4,   // 短期權重高
                3 => 0.3,
                5 => 0.2,
                10 => 0.1,  // 長期權重低
                _ => 0.1,
            };
            
            weighted_direction += pred.direction as f64 * weight * pred.probability;
            total_weight += weight;
            avg_confidence += pred.probability * weight;
        }
        
        if total_weight > 0.0 {
            weighted_direction /= total_weight;
            avg_confidence /= total_weight;
        }
        
        // 信號過濾
        if avg_confidence < self.confidence_threshold {
            return Ok(None);
        }
        
        // 確定信號類型
        let signal_type = if weighted_direction > 0.3 {
            SignalType::Buy
        } else if weighted_direction < -0.3 {
            SignalType::Sell
        } else {
            SignalType::Hold
        };
        
        // 計算倉位大小
        let position_size = match signal_type {
            SignalType::Buy | SignalType::Sell => {
                let max_size = self.max_position_pct;
                let confidence_adj = (avg_confidence - self.confidence_threshold) / (1.0 - self.confidence_threshold);
                (max_size * confidence_adj).min(max_size)
            },
            _ => 0.0,
        };
        
        // 計算風險指標
        let risk_metrics = self.calculate_risk_metrics(&prediction);
        
        // 風險檢查
        if risk_metrics.kelly_fraction < 0.0 || risk_metrics.sharpe_ratio < 0.5 {
            return Ok(None);
        }
        
        Ok(Some(LobTransformerSignal {
            timestamp,
            symbol: prediction.symbol.clone(),
            signal_type,
            confidence: avg_confidence,
            position_size,
            stop_loss: Some(0.005), // 0.5% 止損
            take_profit: Some(0.015), // 1.5% 止盈
            signal_source: "LOB_Transformer".to_string(),
            model_predictions: prediction,
            risk_metrics,
        }))
    }
    
    /// 計算風險指標
    fn calculate_risk_metrics(&self, prediction: &MultiHorizonPrediction) -> RiskMetrics {
        // 簡化的風險計算
        let expected_returns: Vec<f64> = prediction.predictions.values()
            .map(|p| p.expected_return)
            .collect();
        
        let avg_return = expected_returns.iter().sum::<f64>() / expected_returns.len() as f64;
        let variance = expected_returns.iter()
            .map(|&r| (r - avg_return).powi(2))
            .sum::<f64>() / expected_returns.len() as f64;
        let volatility = variance.sqrt();
        
        let sharpe_ratio = if volatility > 0.0 { avg_return / volatility } else { 0.0 };
        let max_drawdown = 0.02; // 簡化為固定值
        let var_95 = avg_return - 1.65 * volatility; // 95% VaR
        let expected_shortfall = avg_return - 2.33 * volatility; // 99% ES
        
        // Kelly criterion 計算
        let win_prob = prediction.predictions.values()
            .filter(|p| p.expected_return > 0.0)
            .count() as f64 / prediction.predictions.len() as f64;
        let avg_win = expected_returns.iter().filter(|&&r| r > 0.0).sum::<f64>() / 
                      expected_returns.iter().filter(|&&r| r > 0.0).count().max(1) as f64;
        let avg_loss = expected_returns.iter().filter(|&&r| r < 0.0).map(|&r| -r).sum::<f64>() / 
                       expected_returns.iter().filter(|&&r| r < 0.0).count().max(1) as f64;
        
        let kelly_fraction = if avg_loss > 0.0 {
            (win_prob * avg_win - (1.0 - win_prob) * avg_loss) / avg_win
        } else {
            0.0
        }.max(0.0).min(0.25); // 限制最大25%
        
        RiskMetrics {
            sharpe_ratio,
            max_drawdown,
            var_95,
            expected_shortfall,
            kelly_fraction,
        }
    }
    
    /// 執行交易信號
    pub async fn execute_signal(&mut self, signal: &LobTransformerSignal) -> Result<()> {
        match self.trading_mode {
            TradingMode::DryRun => {
                info!("🔄 [DRY RUN] 模擬執行交易: {:?}", signal.signal_type);
                // 模擬交易執行
                self.simulate_trade_execution(signal).await?;
            },
            TradingMode::Live => {
                warn!("⚠️ [LIVE] 實時交易執行 - 需要實現實際交易接口");
                // 實際交易執行邏輯
                // self.execute_real_trade(signal).await?;
            },
            TradingMode::Backtest => {
                info!("📊 [BACKTEST] 回測模式執行");
                self.simulate_trade_execution(signal).await?;
            },
        }
        
        Ok(())
    }
    
    /// 模擬交易執行
    async fn simulate_trade_execution(&mut self, signal: &LobTransformerSignal) -> Result<()> {
        let trade_size = signal.position_size * self.current_capital;
        
        match signal.signal_type {
            SignalType::Buy => {
                self.current_position += trade_size / 50000.0; // 假設BTC價格50k
                info!("📈 模擬買入: ${:.2}, 當前倉位: {:.4} BTC", trade_size, self.current_position);
            },
            SignalType::Sell => {
                let sell_amount = (signal.position_size * self.current_position).min(self.current_position);
                self.current_position -= sell_amount;
                info!("📉 模擬賣出: {:.4} BTC, 剩餘倉位: {:.4} BTC", sell_amount, self.current_position);
            },
            SignalType::Close => {
                info!("🔄 平倉所有倉位: {:.4} BTC", self.current_position);
                self.current_position = 0.0;
            },
            SignalType::Hold => {
                debug!("⏸️ 持倉不變");
            },
        }
        
        // 更新資本和風險指標
        self.update_portfolio_metrics();
        
        Ok(())
    }
    
    /// 更新投資組合指標
    fn update_portfolio_metrics(&mut self) {
        let current_value = self.current_capital + self.current_position * 50000.0; // 簡化計算
        
        if current_value > self.peak_capital {
            self.peak_capital = current_value;
        }
        
        let drawdown = (self.peak_capital - current_value) / self.peak_capital;
        if drawdown > self.max_drawdown {
            self.max_drawdown = drawdown;
        }
        
        self.daily_pnl = current_value - self.initial_capital;
    }
    
    /// 獲取系統狀態報告
    pub fn get_status_report(&self) -> String {
        let (inference_count, avg_latency) = self.model.get_performance_stats();
        let total_return = (self.current_capital + self.current_position * 50000.0 - self.initial_capital) / self.initial_capital;
        
        format!(
            "📊 LOB Transformer HFT系統狀態報告\n\
             ├─ 模型性能: {} 次推理, 平均延遲 {:.1}μs\n\
             ├─ 信號生成: {} 個信號\n\
             ├─ 當前倉位: {:.4} BTC\n\
             ├─ 總收益率: {:.2}%\n\
             ├─ 最大回撤: {:.2}%\n\
             └─ 當日盈虧: ${:.2}",
            inference_count, avg_latency,
            self.signal_generation_count,
            self.current_position,
            total_return * 100.0,
            self.max_drawdown * 100.0,
            self.daily_pnl
        )
    }
}

/// 主要的HFT系統運行器
pub struct LobTransformerHftSystem {
    trading_engine: LobTransformerTradingEngine,
    bitget_connector: BitgetConnector,
    performance_monitor: PerformanceManager,
    last_status_report: Instant,
}

impl LobTransformerHftSystem {
    pub async fn new(args: Args) -> Result<Self> {
        info!("🚀 初始化LOB Transformer HFT系統");
        
        // 設備選擇
        let device = if args.use_gpu && Device::cuda_if_available(0).is_ok() {
            info!("🎮 使用GPU設備");
            Device::cuda_if_available(0)?
        } else {
            info!("💻 使用CPU設備");
            Device::Cpu
        };
        
        // 模型配置
        let model_config = LobTransformerConfig::default();
        
        // 創建交易引擎
        let trading_engine = LobTransformerTradingEngine::new(&args, model_config, device)?;
        
        // Bitget連接配置
        let bitget_config = BitgetConfig {
            public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
            private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
            timeout_seconds: 10,
            auto_reconnect: true,
            max_reconnect_attempts: 5,
        };
        
        let mut bitget_connector = BitgetConnector::new(bitget_config);
        bitget_connector.add_subscription(args.symbol.clone(), BitgetChannel::Books5);
        
        // 性能監控
        let perf_config = rust_hft::utils::PerformanceConfig::default();
        let performance_monitor = PerformanceManager::new(perf_config)?;
        
        info!("✅ LOB Transformer HFT系統初始化完成");
        info!("   模式: {:?}", args.mode);
        info!("   交易對: {}", args.symbol);
        info!("   模型路徑: {}", args.model_path);
        info!("   初始資金: ${:.2}", args.initial_capital);
        
        Ok(Self {
            trading_engine,
            bitget_connector,
            performance_monitor,
            last_status_report: Instant::now(),
        })
    }
    
    pub async fn run(self) -> Result<()> {
        info!("🔄 啟動LOB Transformer HFT系統");
        
        let trading_engine = Arc::new(Mutex::new(self.trading_engine));
        let trading_engine_clone = trading_engine.clone();
        
        // 消息處理器 - 避免runtime嵌套問題
        let message_handler = move |message: BitgetMessage| {
            let engine = trading_engine_clone.clone();
            
            // 簡化為同步處理避免runtime嵌套
            match message {
                BitgetMessage::OrderBook { data, timestamp, .. } => {
                    if let Ok(orderbook) = Self::parse_orderbook_from_bitget(&data, timestamp) {
                        // 使用阻塞調用處理訂單簿
                        std::thread::spawn(move || {
                            let rt = tokio::runtime::Runtime::new().unwrap();
                            rt.block_on(async move {
                                if let Err(e) = Self::handle_orderbook_sync(engine, orderbook, timestamp).await {
                                    error!("訂單簿處理錯誤: {}", e);
                                }
                            });
                        });
                    }
                },
                _ => {
                    debug!("收到其他消息類型");
                }
            }
        };
        
        // 啟動WebSocket連接
        let mut connector = self.bitget_connector;
        tokio::select! {
            result = connector.connect_public(message_handler) => {
                if let Err(e) = result {
                    error!("WebSocket連接失敗: {}", e);
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("📊 收到停止信號，正在關閉系統...");
                Self::print_final_report(trading_engine).await;
            }
        }
        
        Ok(())
    }
    
    async fn handle_orderbook_sync(
        trading_engine: Arc<Mutex<LobTransformerTradingEngine>>, 
        orderbook: OrderBook, 
        timestamp: Timestamp
    ) -> Result<()> {
        let signal = {
            let mut engine = trading_engine.lock().unwrap();
            engine.process_orderbook_update(orderbook, timestamp).await?
        };
        
        if let Some(signal) = signal {
            let mut engine = trading_engine.lock().unwrap();
            // 執行交易信號
            if let Err(e) = engine.execute_signal(&signal).await {
                error!("交易執行失敗: {}", e);
            }
            
            // 定期狀態報告
            static mut LAST_REPORT: Option<Instant> = None;
            unsafe {
                let now = Instant::now();
                if LAST_REPORT.map_or(true, |last| now.duration_since(last) > Duration::from_secs(30)) {
                    info!("{}", engine.get_status_report());
                    LAST_REPORT = Some(now);
                }
            }
        }
        
        Ok(())
    }
    
    fn parse_orderbook_from_bitget(data: &serde_json::Value, timestamp: Timestamp) -> Result<OrderBook> {
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;
        
        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        orderbook.last_update = timestamp;
        orderbook.is_valid = true;
        
        // 解析 bids
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(20) {
                if let Some(bid_array) = bid.as_array() {
                    if bid_array.len() >= 2 {
                        let price: f64 = bid_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = bid_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.bids.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        // 解析 asks
        if let Some(ask_data) = first_item.get("asks").and_then(|v| v.as_array()) {
            for ask in ask_data.iter().take(20) {
                if let Some(ask_array) = ask.as_array() {
                    if ask_array.len() >= 2 {
                        let price: f64 = ask_array[0].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        let qty: f64 = ask_array[1].as_str().unwrap_or("0").parse().unwrap_or(0.0);
                        if price > 0.0 && qty > 0.0 {
                            use ordered_float::OrderedFloat;
                            use rust_decimal::Decimal;
                            orderbook.asks.insert(OrderedFloat(price), Decimal::from_f64_retain(qty).unwrap_or_default());
                        }
                    }
                }
            }
        }
        
        Ok(orderbook)
    }
    
    async fn print_final_report(trading_engine: Arc<Mutex<LobTransformerTradingEngine>>) {
        let engine = trading_engine.lock().unwrap();
        info!("📋 ======= 最終系統報告 =======");
        info!("{}", engine.get_status_report());
        info!("🏁 系統已安全關閉");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let args = Args::parse();
    
    info!("🚀 LOB Transformer HFT系統啟動");
    info!("配置: {:?}", args);
    
    let system = LobTransformerHftSystem::new(args).await?;
    system.run().await?;
    
    Ok(())
}