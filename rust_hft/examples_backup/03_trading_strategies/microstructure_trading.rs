/*!
 * True HFT Microstructure Trading Demo
 * 
 * 真正的高頻交易系統：
 * 1. 微秒級市場微結構套利
 * 2. Tick級別剝頭皮策略  
 * 3. Spread捕獲和流動性套利
 * 4. 超快進出場（1-10秒持倉）
 * 5. 基於PnL的智能節流
 */

use rust_hft::{
    integrations::bitget_connector::*,
    ml::{
        features::FeatureExtractor,
        dl_trend_predictor::{DlTrendPredictor, DlTrendPredictorConfig, TrendPrediction, TrendClass},
        lob_time_series_extractor::{LobTimeSeriesExtractor, LobTimeSeriesConfig},
    },
    core::{types::*, orderbook::OrderBook},
    utils::performance::*,
};
use anyhow::Result;
use tracing::{info, warn};
use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use tokio::time::Duration;
use serde_json::Value;

/// 多時間範圍DL預測器集合
#[derive(Debug)]
pub struct MultiTimeframeDlPredictors {
    /// 1秒預測器
    predictor_1s: DlTrendPredictor,
    /// 3秒預測器  
    predictor_3s: DlTrendPredictor,
    /// 5秒預測器
    predictor_5s: DlTrendPredictor,
    /// 最後預測時間
    last_prediction_time: u64,
    /// 預測冷卻時間（微秒）
    prediction_cooldown_us: u64,
}

impl MultiTimeframeDlPredictors {
    pub fn new() -> Result<Self> {
        // 配置不同時間範圍的預測器
        let config_1s = DlTrendPredictorConfig {
            prediction_horizon_seconds: 1,
            confidence_threshold: 0.55,
            max_inference_latency_us: 500,
            ..Default::default()
        };
        
        let config_3s = DlTrendPredictorConfig {
            prediction_horizon_seconds: 3,
            confidence_threshold: 0.5,
            max_inference_latency_us: 800,
            ..Default::default()
        };
        
        let config_5s = DlTrendPredictorConfig {
            prediction_horizon_seconds: 5,
            confidence_threshold: 0.45,
            max_inference_latency_us: 1000,
            ..Default::default()
        };
        
        let lob_config = LobTimeSeriesConfig {
            dl_lookback_seconds: 30,
            dl_prediction_seconds: 5,
            rl_state_window_ms: 5000,
            extraction_interval_us: 200_000, // 200ms更頻繁提取
            max_sequence_length: 150, // 30s / 200ms = 150
            lob_depth: 10,
            enable_advanced_features: true,
            memory_cleanup_interval: 60,
        };
        
        Ok(Self {
            predictor_1s: DlTrendPredictor::new("BTCUSDT".to_string(), config_1s, lob_config.clone())?,
            predictor_3s: DlTrendPredictor::new("BTCUSDT".to_string(), config_3s, lob_config.clone())?,
            predictor_5s: DlTrendPredictor::new("BTCUSDT".to_string(), config_5s, lob_config)?,
            last_prediction_time: 0,
            prediction_cooldown_us: 100_000, // 100ms預測間隔
        })
    }
    
    /// 基於DL模型生成多時間範圍交易信號
    pub fn generate_dl_trading_signals(
        &mut self, 
        extractor: &LobTimeSeriesExtractor,
        current_time: u64
    ) -> Result<Vec<DlTradingSignal>> {
        let mut signals = Vec::new();
        
        // 檢查預測冷卻時間
        if current_time - self.last_prediction_time < self.prediction_cooldown_us {
            return Ok(signals);
        }
        
        // 1秒預測
        if let Ok(Some(pred_1s)) = self.predictor_1s.predict_trend(extractor) {
            if let Some(signal) = self.convert_prediction_to_signal(&pred_1s, TimeFrame::UltraShort) {
                signals.push(signal);
            }
        }
        
        // 3秒預測
        if let Ok(Some(pred_3s)) = self.predictor_3s.predict_trend(extractor) {
            if let Some(signal) = self.convert_prediction_to_signal(&pred_3s, TimeFrame::Short) {
                signals.push(signal);
            }
        }
        
        // 5秒預測
        if let Ok(Some(pred_5s)) = self.predictor_5s.predict_trend(extractor) {
            if let Some(signal) = self.convert_prediction_to_signal(&pred_5s, TimeFrame::Medium) {
                signals.push(signal);
            }
        }
        
        self.last_prediction_time = current_time;
        Ok(signals)
    }
    
    /// 將DL預測轉換為交易信號
    fn convert_prediction_to_signal(&self, prediction: &TrendPrediction, timeframe: TimeFrame) -> Option<DlTradingSignal> {
        // 根據預測的趨勢類別決定交易方向
        let (direction, strength) = match prediction.trend_class {
            TrendClass::StrongUp => (TradeDirection::Buy, 1.0),
            TrendClass::WeakUp => (TradeDirection::Buy, 0.6),
            TrendClass::StrongDown => (TradeDirection::Sell, 1.0),
            TrendClass::WeakDown => (TradeDirection::Sell, 0.6),
            TrendClass::Neutral => return None, // 中性信號不交易
        };
        
        // 計算倉位大小（基於預測強度和置信度）
        let base_size = match timeframe {
            TimeFrame::UltraShort => 3.0, // 1秒：小倉位高頻
            TimeFrame::Short => 5.0,      // 3秒：中等倉位
            TimeFrame::Medium => 7.0,     // 5秒：較大倉位
            TimeFrame::Tick => 2.0,       // Tick級別
        };
        
        let position_size = base_size * strength * prediction.confidence;
        
        // 設置持倉時間
        let hold_time_ms = match timeframe {
            TimeFrame::UltraShort => 800,
            TimeFrame::Short => 2000,
            TimeFrame::Medium => 3000,
            TimeFrame::Tick => 200,
        };
        
        Some(DlTradingSignal {
            direction,
            timeframe,
            position_size,
            confidence: prediction.confidence,
            expected_return_bps: prediction.expected_change_bps,
            hold_time_ms,
            reasoning: format!("DL-{:?}: {} (conf: {:.1}%, ret: {:.1}bps)", 
                             timeframe, prediction.trend_class.name(), 
                             prediction.confidence * 100.0, prediction.expected_change_bps),
        })
    }
}

/// DL交易信號
#[derive(Debug, Clone)]
pub struct DlTradingSignal {
    pub direction: TradeDirection,
    pub timeframe: TimeFrame,
    pub position_size: f64,
    pub confidence: f64,
    pub expected_return_bps: f64,
    pub hold_time_ms: u64,
    pub reasoning: String,
}

/// 真正的HFT微結構交易系統 - 基於DL LOB預測
pub struct TrueHftMicrostructureSystem {
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// DL LOB時間序列提取器
    lob_extractor: Arc<Mutex<LobTimeSeriesExtractor>>,
    
    /// 多時間範圍DL預測器
    dl_predictors: Arc<Mutex<MultiTimeframeDlPredictors>>,
    
    /// 性能管理器
    performance_manager: Arc<Mutex<PerformanceManager>>,
    
    /// 微結構策略引擎
    microstructure_engine: Arc<Mutex<MicrostructureEngine>>,
    
    /// 智能節流控制器
    smart_throttle: Arc<Mutex<SmartThrottleController>>,
    
    /// HFT統計
    stats: Arc<Mutex<HftStats>>,
}

#[derive(Debug, Default, Clone)]
pub struct HftStats {
    pub total_updates: u64,
    pub microstructure_signals: u64,
    pub scalping_trades: u64,
    pub spread_captures: u64,
    pub arbitrage_opportunities: u64,
    
    // PnL統計
    pub current_position: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub avg_trade_duration_ms: f64,
    
    // 性能統計
    pub avg_signal_latency_us: f64,
    pub avg_execution_latency_us: f64,
    pub signals_per_second: f64,
    
    // 風險指標
    pub max_position: f64,
    pub daily_pnl: f64,
    pub max_drawdown: f64,
    pub sharpe_ratio: f64,
}

/// 微結構交易引擎
#[derive(Debug)]
pub struct MicrostructureEngine {
    /// 當前持倉
    current_position: f64,
    
    /// 活躍交易
    active_trades: VecDeque<ActiveTrade>,
    
    /// 最後價格
    last_price: f64,
    
    /// 近期價格歷史（用於微結構分析）
    price_ticks: VecDeque<PriceTick>,
    
    /// OBI歷史
    obi_history: VecDeque<f64>,
    
    /// 配置參數
    config: MicrostructureConfig,
}

#[derive(Debug, Clone)]
pub struct MicrostructureConfig {
    /// 基礎資金
    pub base_capital: f64,  // 400 USDT
    
    /// 單次最大風險
    pub max_risk_per_trade: f64,  // 0.5% = 2 USDT
    
    /// 最大持倉
    pub max_position: f64,  // 20 USDT
    
    /// 目標spread capture
    pub target_spread_capture_bps: f64,  // 1-3 bps
    
    /// 最小利潤目標
    pub min_profit_target_bps: f64,  // 0.5 bps
    
    /// 最大持倉時間
    pub max_hold_time_ms: u64,  // 10秒
    
    /// OBI閾值
    pub obi_threshold: f64,  // 0.3
    
    /// 深度壓力閾值
    pub depth_pressure_threshold: f64,  // 0.2
    
    /// 快速止損
    pub fast_stop_loss_bps: f64,  // 2 bps
}

impl Default for MicrostructureConfig {
    fn default() -> Self {
        Self {
            base_capital: 400.0,
            max_risk_per_trade: 0.005,  // 0.5%
            max_position: 20.0,
            target_spread_capture_bps: 2.0,
            min_profit_target_bps: 0.5,
            max_hold_time_ms: 10000,  // 10秒
            obi_threshold: 0.3,
            depth_pressure_threshold: 0.2,
            fast_stop_loss_bps: 2.0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TradeDirection {
    Buy,
    Sell,
    Close,
}

#[derive(Debug, Clone)]
pub struct ActiveTrade {
    pub trade_id: u64,
    pub direction: TradeDirection,
    pub entry_price: f64,
    pub position_size: f64,
    pub entry_time: u64,
    pub stop_loss: f64,
    pub take_profit: f64,
    pub trade_type: HftTradeType,
}

#[derive(Debug, Clone)]
pub enum HftTradeType {
    /// 剝頭皮（快進快出）
    Scalping,
    /// Spread捕獲
    SpreadCapture,
    /// 微結構套利
    MicrostructureArbitrage,
    /// 流動性提供
    LiquidityProvision,
}

#[derive(Debug, Clone)]
pub struct PriceTick {
    pub timestamp: u64,
    pub price: f64,
    pub volume: f64,
    pub spread_bps: f64,
    pub obi: f64,
}

/// 智能節流控制器
#[derive(Debug)]
pub struct SmartThrottleController {
    /// 最後交易時間
    last_trade_time: u64,
    
    /// 近期PnL
    recent_pnl: VecDeque<f64>,
    
    /// 交易頻率統計
    trade_frequency: f64,
    
    /// 動態冷卻期
    dynamic_cooldown_us: u64,
    
    /// 最小冷卻期
    min_cooldown_us: u64,  // 100ms
    
    /// 最大冷卻期
    max_cooldown_us: u64,  // 5秒
}

impl SmartThrottleController {
    pub fn new() -> Self {
        Self {
            last_trade_time: 0,
            recent_pnl: VecDeque::with_capacity(100),
            trade_frequency: 0.0,
            dynamic_cooldown_us: 200_000,  // 200ms初始值（降低以增加頻率）
            min_cooldown_us: 50_000,   // 50ms（降低最小冷卻時間）
            max_cooldown_us: 2_000_000,  // 2秒（降低最大冷卻時間）
        }
    }
    
    /// 檢查是否可以交易
    pub fn can_trade(&self, current_time: u64) -> bool {
        current_time - self.last_trade_time >= self.dynamic_cooldown_us
    }
    
    /// 更新節流參數 - 優化交易頻率
    pub fn update(&mut self, trade_time: u64, pnl: f64) {
        self.last_trade_time = trade_time;
        self.recent_pnl.push_back(pnl);
        
        if self.recent_pnl.len() > 20 {
            self.recent_pnl.pop_front();
        }
        
        // 計算近期表現指標
        let avg_pnl = self.recent_pnl.iter().sum::<f64>() / self.recent_pnl.len().max(1) as f64;
        let positive_trades = self.recent_pnl.iter().filter(|&&x| x > 0.0).count();
        let win_rate = positive_trades as f64 / self.recent_pnl.len().max(1) as f64;
        
        // 更積極的冷卻時間調整策略
        if avg_pnl > 0.0 && win_rate > 0.6 {
            // 表現良好：大幅縮短冷卻期
            self.dynamic_cooldown_us = (self.dynamic_cooldown_us as f64 * 0.8)
                .max(self.min_cooldown_us as f64) as u64;
        } else if avg_pnl > 0.0 {
            // 盈利但勝率一般：適度縮短
            self.dynamic_cooldown_us = (self.dynamic_cooldown_us as f64 * 0.9)
                .max(self.min_cooldown_us as f64) as u64;
        } else if win_rate < 0.4 {
            // 勝率低：適度增加冷卻期
            self.dynamic_cooldown_us = (self.dynamic_cooldown_us as f64 * 1.15)
                .min(self.max_cooldown_us as f64) as u64;
        } else {
            // 略微虧損但勝率還行：小幅調整
            self.dynamic_cooldown_us = (self.dynamic_cooldown_us as f64 * 1.05)
                .min(self.max_cooldown_us as f64) as u64;
        }
        
        // 計算交易頻率（每分鐘交易次數）
        if self.recent_pnl.len() >= 10 {
            let time_span_minutes = 60.0 / 1000000.0; // 假設1分鐘內的交易
            self.trade_frequency = self.recent_pnl.len() as f64 / time_span_minutes;
        }
    }
}

impl MicrostructureEngine {
    pub fn new(config: MicrostructureConfig) -> Self {
        Self {
            current_position: 0.0,
            active_trades: VecDeque::new(),
            last_price: 0.0,
            price_ticks: VecDeque::with_capacity(100),
            obi_history: VecDeque::with_capacity(50),
            config,
        }
    }
    
    /// 分析微結構並生成信號
    pub fn analyze_microstructure(&mut self, features: &FeatureSet, orderbook: &OrderBook) -> Vec<HftSignal> {
        let mut signals = Vec::new();
        let current_time = now_micros();
        
        // 更新價格tick
        let best_bid = orderbook.bids.keys().next_back().map(|p| p.0).unwrap_or(0.0);
        let best_ask = orderbook.asks.keys().next().map(|p| p.0).unwrap_or(0.0);
        let mid_price = (best_bid + best_ask) / 2.0;
        let spread_bps = if mid_price > 0.0 { (best_ask - best_bid) / mid_price * 10000.0 } else { 0.0 };
        
        self.update_tick_data(current_time, mid_price, features.obi_l5, spread_bps);
        
        // 1. 檢查剝頭皮機會
        if let Some(scalping_signal) = self.detect_scalping_opportunity(features, best_bid, best_ask, mid_price) {
            signals.push(scalping_signal);
        }
        
        // 2. 檢查spread捕獲機會
        if let Some(spread_signal) = self.detect_spread_capture_opportunity(features, best_bid, best_ask, spread_bps) {
            signals.push(spread_signal);
        }
        
        // 3. 檢查微結構套利
        if let Some(arbitrage_signal) = self.detect_microstructure_arbitrage(features, orderbook) {
            signals.push(arbitrage_signal);
        }
        
        // 4. 檢查持倉管理
        self.check_position_management(current_time, mid_price, &mut signals);
        
        signals
    }
    
    /// 檢測剝頭皮機會 - 基於多時間範圍DL LOB預測（優化交易頻率）
    fn detect_scalping_opportunity(&self, features: &FeatureSet, best_bid: f64, best_ask: f64, mid_price: f64) -> Option<HftSignal> {
        // 多時間範圍DL預測
        let multi_prediction = self.generate_multi_timeframe_prediction(features);
        
        // 優化交易頻率：降低信號閾值，增加交易機會
        let (selected_prediction, hold_time_ms, strategy_type) = match multi_prediction.recommended_timeframe {
            TimeFrame::Tick => {
                // Tick級超高頻策略 - 降低閾值以增加交易頻率
                if multi_prediction.tick_prediction.confidence > 0.6  // 從0.7降低到0.6
                   && multi_prediction.tick_prediction.next_tick_change_bps.abs() > 0.3 { // 從0.5降低到0.3
                    let prediction = DlLobPrediction {
                        predicted_change: multi_prediction.tick_prediction.next_tick_change_bps / 10000.0,
                        prediction_strength: multi_prediction.tick_prediction.next_tick_change_bps.abs() / 10000.0,
                        confidence: multi_prediction.tick_prediction.confidence,
                        time_horizon_ms: multi_prediction.tick_prediction.expected_tick_time_ms,
                    };
                    (Some(prediction), multi_prediction.tick_prediction.expected_tick_time_ms + 100, HftTradeType::Scalping) // 縮短持倉時間
                } else {
                    (None, 0, HftTradeType::Scalping)
                }
            }
            TimeFrame::UltraShort => {
                // 1秒快速剝頭皮 - 降低閾值
                if multi_prediction.prediction_1s.confidence > 0.55  // 從0.65降低到0.55
                   && multi_prediction.prediction_1s.prediction_strength > 0.0008 { // 從0.001降低到0.0008
                    (Some(multi_prediction.prediction_1s), 800, HftTradeType::Scalping) // 縮短持倉時間
                } else {
                    (None, 0, HftTradeType::Scalping)
                }
            }
            TimeFrame::Short => {
                // 3秒短期動量 - 降低閾值
                if multi_prediction.prediction_3s.confidence > 0.5   // 從0.6降低到0.5
                   && multi_prediction.prediction_3s.prediction_strength > 0.0012 { // 從0.0015降低到0.0012
                    (Some(multi_prediction.prediction_3s), 2000, HftTradeType::MicrostructureArbitrage) // 縮短持倉時間
                } else {
                    (None, 0, HftTradeType::MicrostructureArbitrage)
                }
            }
            TimeFrame::Medium => {
                // 5秒中期趨勢 - 降低閾值
                if multi_prediction.prediction_5s.confidence > 0.45  // 從0.55降低到0.45  
                   && multi_prediction.prediction_5s.prediction_strength > 0.0015 { // 從0.002降低到0.0015
                    (Some(multi_prediction.prediction_5s), 3000, HftTradeType::SpreadCapture) // 縮短持倉時間
                } else {
                    (None, 0, HftTradeType::SpreadCapture)
                }
            }
        };
        
        // 檢查是否有有效預測
        if let Some(prediction) = selected_prediction {
            // 檢查市場條件
            let reasonable_spread = features.spread_bps < 15.0;
            if !reasonable_spread {
                return None;
            }
            
            let direction = if prediction.predicted_change > 0.0 {
                TradeDirection::Buy
            } else {
                TradeDirection::Sell
            };
            
            // 基於時間範圍和預測強度動態調整倉位
            let base_size = match multi_prediction.recommended_timeframe {
                TimeFrame::Tick => 2.0,      // Tick級：小倉位高頻
                TimeFrame::UltraShort => 4.0, // 1秒：中等倉位
                TimeFrame::Short => 6.0,     // 3秒：較大倉位
                TimeFrame::Medium => 8.0,    // 5秒：大倉位
            };
            
            let volatility_multiplier = (multi_prediction.volatility * 2.0).max(0.5).min(2.0);
            let confidence_multiplier = (prediction.confidence * 1.5).max(0.5).min(2.0);
            let position_size = (base_size * volatility_multiplier * confidence_multiplier).min(15.0);
            
            let entry_price = match direction {
                TradeDirection::Buy => best_ask,
                TradeDirection::Sell => best_bid,
                _ => mid_price,
            };
            
            return Some(HftSignal {
                direction,
                signal_type: strategy_type,
                entry_price,
                position_size,
                confidence: prediction.confidence,
                expected_hold_time_ms: hold_time_ms,
                reasoning: format!("DL-{:?}: {:.3}% in {}ms, conf:{:.1}%, vol:{:.3}", 
                                 multi_prediction.recommended_timeframe,
                                 prediction.predicted_change * 100.0, 
                                 prediction.time_horizon_ms,
                                 prediction.confidence * 100.0,
                                 multi_prediction.volatility),
            });
        }
        
        None
    }
    
    /// 生成多時間範圍DL預測
    fn generate_multi_timeframe_prediction(&self, features: &FeatureSet) -> MultiTimeframePrediction {
        // 計算市場波動性指標
        let volatility = self.calculate_market_volatility(features);
        
        // 生成各時間範圍的DL預測
        let prediction_1s = self.generate_dl_prediction_for_timeframe(features, 1000, 0.7); // 1秒
        let prediction_3s = self.generate_dl_prediction_for_timeframe(features, 3000, 0.6); // 3秒  
        let prediction_5s = self.generate_dl_prediction_for_timeframe(features, 5000, 0.55); // 5秒
        
        // 生成tick級預測
        let tick_prediction = self.generate_tick_prediction(features);
        
        // 基於市場條件和預測置信度選擇最佳時間範圍
        let recommended_timeframe = self.select_optimal_timeframe(
            &prediction_1s, &prediction_3s, &prediction_5s, &tick_prediction, volatility
        );
        
        MultiTimeframePrediction {
            prediction_1s,
            prediction_3s,
            prediction_5s,
            tick_prediction,
            volatility,
            recommended_timeframe,
        }
    }
    
    /// 計算市場波動性
    fn calculate_market_volatility(&self, features: &FeatureSet) -> f64 {
        // 基於多個指標計算綜合波動性
        let spread_volatility = features.spread_bps / 20.0; // 標準化spread
        let obi_volatility = features.obi_l5.abs(); // OBI不平衡
        let depth_volatility = (features.depth_pressure_bid - features.depth_pressure_ask).abs();
        let price_volatility = features.price_momentum.abs() * 10.0;
        
        // 加權平均
        (spread_volatility * 0.3 + obi_volatility * 0.3 + depth_volatility * 0.2 + price_volatility * 0.2)
            .min(1.0)
    }
    
    /// 為特定時間範圍生成DL預測
    fn generate_dl_prediction_for_timeframe(&self, features: &FeatureSet, time_horizon_ms: u64, base_confidence: f64) -> DlLobPrediction {
        // 時間範圍調整因子
        let time_factor = match time_horizon_ms {
            0..=1000 => 1.2,    // 1秒：增強短期信號
            1001..=3000 => 1.0, // 3秒：標準權重
            _ => 0.8,           // 5秒：降低長期信號權重
        };
        
        // 基於LOB特徵的DL模擬預測
        let obi_signal = features.obi_l5 * 0.4 * time_factor;
        let depth_signal = (features.depth_pressure_bid - features.depth_pressure_ask) * 0.3;
        let spread_signal = if features.spread_bps < 8.0 { 0.15 * time_factor } else { -0.1 };
        let momentum_signal = features.price_momentum * 0.25;
        let microstructure_signal = (features.microprice - *features.mid_price) * 0.1;
        
        // 綜合信號（模擬DL模型的非線性組合）
        let raw_prediction = obi_signal + depth_signal + spread_signal + momentum_signal + microstructure_signal;
        
        // 應用激活函數和時間衰減
        let time_decay = 1.0 - (time_horizon_ms as f64 / 10000.0) * 0.1; // 時間越長衰減越多
        let predicted_change = raw_prediction.tanh() * 0.004 * time_decay; // 最大0.4%變化
        let prediction_strength = predicted_change.abs();
        
        // 動態置信度計算
        let feature_consistency = self.calculate_feature_consistency(features);
        let market_quality = self.calculate_market_quality(features);
        let time_adjusted_confidence = base_confidence * time_factor;
        
        let confidence = (feature_consistency * market_quality * time_adjusted_confidence * (1.0 + prediction_strength))
            .min(0.95)
            .max(0.1);
        
        DlLobPrediction {
            predicted_change,
            prediction_strength,
            confidence,
            time_horizon_ms,
        }
    }
    
    /// 生成tick級預測
    fn generate_tick_prediction(&self, features: &FeatureSet) -> TickPrediction {
        // tick級預測專注於極短期微結構變化
        let obi_impact = features.obi_l5 * 2.0;  // OBI對下一個tick影響很大
        let depth_impact = (features.depth_pressure_bid - features.depth_pressure_ask) * 1.5;
        let microprice_impact = (features.microprice - *features.mid_price) * 3.0;
        let order_flow_impact = features.order_flow_imbalance * 1.0;
        
        // tick預測（basis points）
        let raw_tick_prediction = obi_impact + depth_impact + microprice_impact + order_flow_impact;
        let next_tick_change_bps = raw_tick_prediction.tanh() * 3.0; // 最大3bps變化
        
        // tick預測置信度（基於微結構一致性）
        let microstructure_alignment = if (obi_impact > 0.0) == (depth_impact > 0.0) && 
                                         (depth_impact > 0.0) == (microprice_impact > 0.0) {
            0.9
        } else if (obi_impact > 0.0) == (depth_impact > 0.0) {
            0.7
        } else {
            0.4
        };
        
        let spread_quality = (1.0 - features.spread_bps / 15.0).max(0.3);
        let confidence = microstructure_alignment * spread_quality * (1.0 + next_tick_change_bps.abs() / 3.0);
        
        // 預期tick時間（基於市場活躍度）
        let base_tick_time = 200; // 基礎200ms
        let activity_factor = (features.obi_l5.abs() + features.order_flow_imbalance.abs()).min(2.0);
        let expected_tick_time_ms = (base_tick_time as f64 / (1.0 + activity_factor)) as u64;
        
        TickPrediction {
            next_tick_change_bps,
            confidence: confidence.min(0.95),
            expected_tick_time_ms: expected_tick_time_ms.max(100).min(500), // 100-500ms
        }
    }
    
    /// 選擇最佳時間範圍
    fn select_optimal_timeframe(
        &self,
        pred_1s: &DlLobPrediction,
        pred_3s: &DlLobPrediction, 
        pred_5s: &DlLobPrediction,
        tick_pred: &TickPrediction,
        volatility: f64
    ) -> TimeFrame {
        // 計算每個時間範圍的分數
        let tick_score = tick_pred.confidence * tick_pred.next_tick_change_bps.abs() / 3.0;
        let ultra_short_score = pred_1s.confidence * pred_1s.prediction_strength * 100.0;
        let short_score = pred_3s.confidence * pred_3s.prediction_strength * 100.0;
        let medium_score = pred_5s.confidence * pred_5s.prediction_strength * 100.0;
        
        // 高波動性市場偏向更短時間範圍
        let volatility_adjustment = if volatility > 0.5 {
            (2.0, 1.5, 1.0, 0.8) // (tick, 1s, 3s, 5s)
        } else if volatility > 0.3 {
            (1.5, 1.2, 1.0, 0.9)
        } else {
            (1.0, 1.0, 1.1, 1.2) // 低波動性偏向較長時間範圍
        };
        
        let adjusted_tick_score = tick_score * volatility_adjustment.0;
        let adjusted_ultra_short_score = ultra_short_score * volatility_adjustment.1;
        let adjusted_short_score = short_score * volatility_adjustment.2;
        let adjusted_medium_score = medium_score * volatility_adjustment.3;
        
        // 選擇分數最高的時間範圍
        let max_score = adjusted_tick_score.max(adjusted_ultra_short_score)
                                         .max(adjusted_short_score)
                                         .max(adjusted_medium_score);
        
        if max_score == adjusted_tick_score && tick_pred.confidence > 0.7 {
            TimeFrame::Tick
        } else if max_score == adjusted_ultra_short_score && pred_1s.confidence > 0.65 {
            TimeFrame::UltraShort
        } else if max_score == adjusted_short_score && pred_3s.confidence > 0.6 {
            TimeFrame::Short
        } else if pred_5s.confidence > 0.55 {
            TimeFrame::Medium
        } else {
            // 默認回退到1秒策略
            TimeFrame::UltraShort
        }
    }
    
    /// 計算特徵一致性
    fn calculate_feature_consistency(&self, features: &FeatureSet) -> f64 {
        let obi_direction = features.obi_l5 > 0.0;
        let depth_direction = features.depth_pressure_bid > features.depth_pressure_ask;
        let flow_direction = features.order_flow_imbalance > 0.0;
        let microprice_direction = features.microprice > *features.mid_price;
        
        let agreements = [
            obi_direction == depth_direction,
            depth_direction == flow_direction,
            flow_direction == microprice_direction,
            obi_direction == microprice_direction,
        ].iter().filter(|&&x| x).count();
        
        agreements as f64 / 4.0
    }
    
    /// 計算市場質量
    fn calculate_market_quality(&self, features: &FeatureSet) -> f64 {
        let spread_quality = (1.0 - features.spread_bps / 25.0).max(0.2);
        let depth_quality = if features.depth_imbalance_l5.abs() < 0.3 { 0.9 } else { 0.6 };
        let stability = 1.0 - features.obi_l5.abs().min(0.5);
        
        (spread_quality + depth_quality + stability) / 3.0
    }
    
    /// 模擬DL LOB預測（您的原始需求）
    fn simulate_dl_lob_prediction(&self, features: &FeatureSet) -> DlLobPrediction {
        // 這個函數保持向後兼容，但現在使用多時間範圍預測的5秒版本
        self.generate_dl_prediction_for_timeframe(features, 5000, 0.55)
    }
    
    /// 檢測spread捕獲機會
    fn detect_spread_capture_opportunity(&self, features: &FeatureSet, best_bid: f64, best_ask: f64, spread_bps: f64) -> Option<HftSignal> {
        // 尋找異常大的spread
        let wide_spread = spread_bps > self.config.target_spread_capture_bps * 2.0;
        let stable_obi = features.obi_l5.abs() < 0.1;  // 相對穩定的市場
        
        if wide_spread && stable_obi {
            // 在中間價格提供流動性
            let _mid_price = (best_bid + best_ask) / 2.0;
            let position_size = self.calculate_spread_capture_size(spread_bps);
            
            // 同時下買單和賣單（做市）
            return Some(HftSignal {
                direction: TradeDirection::Buy,  // 先做一邊
                signal_type: HftTradeType::SpreadCapture,
                entry_price: best_bid + (best_ask - best_bid) * 0.3,  // 靠近bid的位置
                position_size,
                confidence: spread_bps / 20.0,  // 基於spread大小
                expected_hold_time_ms: 5000,  // 5秒
                reasoning: format!("Spread Capture: Spread={:.1}bps, Target={:.1}bps", 
                                 spread_bps, self.config.target_spread_capture_bps),
            });
        }
        
        None
    }
    
    /// 檢測微結構套利
    fn detect_microstructure_arbitrage(&self, features: &FeatureSet, _orderbook: &OrderBook) -> Option<HftSignal> {
        // 檢查order flow和微價格不一致
        let microprice_signal = features.microprice - *features.mid_price;
        let order_flow_signal = features.order_flow_imbalance;
        
        // 信號不一致表示套利機會
        let signal_divergence = (microprice_signal > 0.0) != (order_flow_signal > 0.0);
        let strong_divergence = microprice_signal.abs() > 1.0 && order_flow_signal.abs() > 0.2;
        
        if signal_divergence && strong_divergence {
            let direction = if microprice_signal > order_flow_signal {
                TradeDirection::Buy
            } else {
                TradeDirection::Sell
            };
            
            let position_size = self.calculate_arbitrage_size(microprice_signal.abs());
            
            return Some(HftSignal {
                direction,
                signal_type: HftTradeType::MicrostructureArbitrage,
                entry_price: features.microprice,
                position_size,
                confidence: microprice_signal.abs() * order_flow_signal.abs(),
                expected_hold_time_ms: 3000,  // 3秒
                reasoning: format!("Microstructure Arbitrage: MicroPrice={:.3}, OrderFlow={:.3}", 
                                 microprice_signal, order_flow_signal),
            });
        }
        
        None
    }
    
    /// 檢查持倉管理 - 快速止盈止損機制
    fn check_position_management(&mut self, current_time: u64, current_price: f64, signals: &mut Vec<HftSignal>) {
        let mut trades_to_close: Vec<(usize, String)> = Vec::new();
        
        for (idx, trade) in self.active_trades.iter().enumerate() {
            let hold_time = current_time - trade.entry_time;
            let price_change = current_price - trade.entry_price;
            let pnl_bps = match trade.direction {
                TradeDirection::Buy => price_change / trade.entry_price * 10000.0,
                TradeDirection::Sell => -price_change / trade.entry_price * 10000.0,
                _ => 0.0,
            };
            
            // 快速止盈止損機制
            let mut should_close = false;
            let mut close_reason = String::new();
            
            // 1. 時間止損 - 根據交易類型調整
            let max_hold_time = match trade.trade_type {
                HftTradeType::Scalping => self.config.max_hold_time_ms / 2, // Scalping更快退出
                HftTradeType::SpreadCapture => self.config.max_hold_time_ms,
                HftTradeType::MicrostructureArbitrage => self.config.max_hold_time_ms * 3 / 4,
                HftTradeType::LiquidityProvision => self.config.max_hold_time_ms * 2,
            };
            
            if hold_time > max_hold_time * 1000 {
                should_close = true;
                close_reason = format!("Time stop: {}ms", hold_time / 1000);
            }
            
            // 2. 快速止損 - 動態調整
            let dynamic_stop_loss = match trade.trade_type {
                HftTradeType::Scalping => self.config.fast_stop_loss_bps * 0.5, // Scalping更緊的止損
                _ => self.config.fast_stop_loss_bps,
            };
            
            if pnl_bps < -dynamic_stop_loss {
                should_close = true;
                close_reason = format!("Fast stop loss: {:.1}bps", pnl_bps);
            }
            
            // 3. 快速止盈 - 分階段止盈
            let quick_profit_target = self.config.min_profit_target_bps;
            let medium_profit_target = self.config.min_profit_target_bps * 2.0;
            let large_profit_target = self.config.min_profit_target_bps * 3.0;
            
            if pnl_bps > large_profit_target {
                // 大利潤立即止盈
                should_close = true;
                close_reason = format!("Large profit target: {:.1}bps", pnl_bps);
            } else if pnl_bps > medium_profit_target && hold_time > 1000_000 { // 1秒後中等利潤止盈
                should_close = true;
                close_reason = format!("Medium profit target: {:.1}bps", pnl_bps);
            } else if pnl_bps > quick_profit_target && hold_time > 2000_000 { // 2秒後小利潤止盈
                should_close = true;
                close_reason = format!("Quick profit target: {:.1}bps", pnl_bps);
            }
            
            // 4. 反向信號止盈 - 如果信號反轉且有利潤則立即退出
            if pnl_bps > 0.5 { // 有小利潤時檢查反向信號
                // 這裡可以添加反向信號檢測邏輯
                // 簡化版本：如果OBI反轉且有利潤則退出
                if let Some(recent_obi) = self.obi_history.back() {
                    let trade_direction_bullish = matches!(trade.direction, TradeDirection::Buy);
                    let current_obi_bullish = *recent_obi > 0.0;
                    
                    if trade_direction_bullish != current_obi_bullish {
                        should_close = true;
                        close_reason = format!("Signal reversal with profit: {:.1}bps", pnl_bps);
                    }
                }
            }
            
            if should_close {
                trades_to_close.push((idx, close_reason));
                continue;
            }
        }
        
        // 生成平倉信號
        for (idx, reason) in trades_to_close.iter().rev() {
            if let Some(trade) = self.active_trades.get(*idx) {
                signals.push(HftSignal {
                    direction: TradeDirection::Close,
                    signal_type: trade.trade_type.clone(),
                    entry_price: current_price,
                    position_size: trade.position_size,
                    confidence: 1.0,
                    expected_hold_time_ms: 0,
                    reasoning: format!("Fast exit: {}", reason),
                });
            }
            self.active_trades.remove(*idx);
        }
    }
    
    fn update_tick_data(&mut self, timestamp: u64, price: f64, obi: f64, spread_bps: f64) {
        self.last_price = price;
        
        self.price_ticks.push_back(PriceTick {
            timestamp,
            price,
            volume: 0.0,  // 暫時設為0
            spread_bps,
            obi,
        });
        
        if self.price_ticks.len() > 100 {
            self.price_ticks.pop_front();
        }
        
        self.obi_history.push_back(obi);
        if self.obi_history.len() > 50 {
            self.obi_history.pop_front();
        }
    }
    
    fn calculate_scalping_size(&self, signal_strength: f64) -> f64 {
        let base_size = self.config.base_capital * self.config.max_risk_per_trade;
        let strength_multiplier = (signal_strength * 2.0).min(3.0).max(0.5);
        (base_size * strength_multiplier).min(self.config.max_position - self.current_position.abs())
    }
    
    fn calculate_spread_capture_size(&self, spread_bps: f64) -> f64 {
        let base_size = self.config.base_capital * self.config.max_risk_per_trade * 0.5;  // 更保守
        let spread_multiplier = (spread_bps / 5.0).min(2.0).max(0.3);
        base_size * spread_multiplier
    }
    
    fn calculate_arbitrage_size(&self, signal_strength: f64) -> f64 {
        let base_size = self.config.base_capital * self.config.max_risk_per_trade * 0.8;
        let strength_multiplier = (signal_strength * 1.5).min(2.0).max(0.4);
        base_size * strength_multiplier
    }
}

#[derive(Debug, Clone)]
pub struct HftSignal {
    pub direction: TradeDirection,
    pub signal_type: HftTradeType,
    pub entry_price: f64,
    pub position_size: f64,
    pub confidence: f64,
    pub expected_hold_time_ms: u64,
    pub reasoning: String,
}

/// 多時間範圍DL LOB預測結果
#[derive(Debug, Clone)]
pub struct MultiTimeframePrediction {
    /// 1秒預測
    pub prediction_1s: DlLobPrediction,
    /// 3秒預測
    pub prediction_3s: DlLobPrediction,
    /// 5秒預測
    pub prediction_5s: DlLobPrediction,
    /// Tick級預測（下一個tick，~200ms）
    pub tick_prediction: TickPrediction,
    /// 市場波動性
    pub volatility: f64,
    /// 推薦的交易時間範圍
    pub recommended_timeframe: TimeFrame,
}

/// DL LOB預測結果
#[derive(Debug, Clone)]
pub struct DlLobPrediction {
    /// 預測的價格變化（百分比）
    pub predicted_change: f64,
    /// 預測強度（絕對值）
    pub prediction_strength: f64,
    /// 預測置信度 (0-1)
    pub confidence: f64,
    /// 預測時間範圍（毫秒）
    pub time_horizon_ms: u64,
}

/// Tick級預測結果
#[derive(Debug, Clone)]
pub struct TickPrediction {
    /// 下一個tick的價格變化（basis points）
    pub next_tick_change_bps: f64,
    /// tick預測置信度
    pub confidence: f64,
    /// 預期tick發生時間（毫秒）
    pub expected_tick_time_ms: u64,
}

/// 交易時間範圍
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeFrame {
    Tick,      // 100-500ms
    UltraShort, // 1s
    Short,     // 3s
    Medium,    // 5s
}

impl TrueHftMicrostructureSystem {
    pub fn new() -> Result<Self> {
        let feature_extractor = FeatureExtractor::new(50);
        
        // 初始化LOB時間序列提取器
        let lob_config = LobTimeSeriesConfig {
            dl_lookback_seconds: 30,
            dl_prediction_seconds: 5,
            rl_state_window_ms: 5000,
            extraction_interval_us: 200_000, // 200ms更頻繁提取
            max_sequence_length: 150, // 30s / 200ms = 150
            lob_depth: 10,
            enable_advanced_features: true,
            memory_cleanup_interval: 60,
        };
        let lob_extractor = Arc::new(Mutex::new(LobTimeSeriesExtractor::new("BTCUSDT".to_string(), lob_config)));
        
        // 初始化多時間範圍DL預測器
        let dl_predictors = Arc::new(Mutex::new(MultiTimeframeDlPredictors::new()?));
        
        let performance_manager = Arc::new(Mutex::new(PerformanceManager::new(PerformanceConfig::default()).unwrap()));
        let config = MicrostructureConfig::default();
        let microstructure_engine = Arc::new(Mutex::new(MicrostructureEngine::new(config)));
        let smart_throttle = Arc::new(Mutex::new(SmartThrottleController::new()));
        let stats = Arc::new(Mutex::new(HftStats::default()));
        
        info!("🧠 真正的DL LOB預測HFT系統已初始化");
        
        Ok(Self {
            feature_extractor,
            lob_extractor,
            dl_predictors,
            performance_manager,
            microstructure_engine,
            smart_throttle,
            stats,
        })
    }
    
    /// 處理實時LOB數據並生成基於DL的HFT信號
    pub fn process_lob_for_hft(&mut self, symbol: &str, data: &Value, timestamp: u64) -> Result<()> {
        if symbol != "BTCUSDT" {
            return Ok(());
        }
        
        let processing_start = now_micros();
        
        // 解析LOB數據
        let orderbook = self.parse_lob_data(data, timestamp)?;
        
        // 提取特徵（用於基礎統計）
        let features = self.feature_extractor.extract_features(&orderbook, 50, timestamp)?;
        
        // 更新LOB時間序列（用於DL預測）
        {
            let mut lob_extractor = self.lob_extractor.lock().unwrap();
            let _ = lob_extractor.extract_snapshot(&orderbook, 50)?; // 50μs網絡延遲
        }
        
        // 生成基於DL的交易信號
        let dl_signals = {
            let mut dl_predictors = self.dl_predictors.lock().unwrap();
            let lob_extractor = self.lob_extractor.lock().unwrap();
            dl_predictors.generate_dl_trading_signals(&*lob_extractor, timestamp)?
        };
        
        // 處理DL信號
        for dl_signal in dl_signals {
            self.process_dl_signal(dl_signal, timestamp)?;
        }
        
        // 檢查現有持倉的管理（基於DL預測的退出條件）
        self.check_dl_position_management(timestamp)?;
        
        // 更新統計
        let processing_latency = now_micros() - processing_start;
        self.update_stats(processing_latency);
        
        Ok(())
    }
    
    /// 處理基於DL的交易信號
    fn process_dl_signal(&self, dl_signal: DlTradingSignal, timestamp: u64) -> Result<()> {
        // 檢查智能節流
        {
            let throttle = self.smart_throttle.lock().unwrap();
            if !throttle.can_trade(timestamp) {
                return Ok(());  // 被節流控制拒絕
            }
        }
        
        // 基於DL預測執行交易邏輯
        let pnl = self.simulate_dl_execution(&dl_signal);
        
        // 更新節流控制器
        {
            let mut throttle = self.smart_throttle.lock().unwrap();
            throttle.update(timestamp, pnl);
        }
        
        // 記錄DL交易
        self.record_dl_trade(&dl_signal, pnl);
        
        Ok(())
    }
    
    /// 檢查基於DL預測的持倉管理
    fn check_dl_position_management(&self, _timestamp: u64) -> Result<()> {
        // 這裡可以添加基於新DL預測的持倉退出邏輯
        // 例如：如果新預測與當前持倉方向相反且置信度高，則立即平倉
        Ok(())
    }
    
    /// 模擬DL交易執行
    fn simulate_dl_execution(&self, dl_signal: &DlTradingSignal) -> f64 {
        let execution_start = now_micros();
        
        // 模擬執行延遲（基於時間範圍調整）
        let base_latency = match dl_signal.timeframe {
            TimeFrame::Tick => 50,        // Tick級：50μs
            TimeFrame::UltraShort => 100, // 1秒：100μs
            TimeFrame::Short => 150,      // 3秒：150μs
            TimeFrame::Medium => 200,     // 5秒：200μs
        };
        std::thread::sleep(Duration::from_micros(base_latency));
        
        let fee = dl_signal.position_size * 0.0004;  // 0.04% 手續費
        let slippage = dl_signal.position_size * 0.0001;  // 0.01% 滑點
        
        let execution_latency = now_micros() - execution_start;
        
        // 基於DL預測的期望收益計算
        let expected_return_ratio = dl_signal.expected_return_bps / 10000.0;
        let confidence_adjusted_return = expected_return_ratio * dl_signal.confidence;
        let expected_pnl = dl_signal.position_size * confidence_adjusted_return - fee - slippage;
        
        info!("🧠 DL-HFT: {:?}-{:?} | Size: {:.1} | ExpRet: {:.1}bps | Conf: {:.1}% | PnL: {:.4} | {}μs", 
             dl_signal.direction, dl_signal.timeframe, dl_signal.position_size, 
             dl_signal.expected_return_bps, dl_signal.confidence * 100.0, expected_pnl, execution_latency);
        info!("   📊 {}", dl_signal.reasoning);
        
        expected_pnl
    }
    
    /// 記錄DL交易
    fn record_dl_trade(&self, dl_signal: &DlTradingSignal, pnl: f64) {
        let mut stats = self.stats.lock().unwrap();
        stats.total_trades += 1;
        stats.realized_pnl += pnl;
        
        if pnl > 0.0 {
            stats.winning_trades += 1;
        }
        
        // 根據時間範圍分類統計
        match dl_signal.timeframe {
            TimeFrame::UltraShort => stats.scalping_trades += 1,
            TimeFrame::Short => stats.arbitrage_opportunities += 1,
            TimeFrame::Medium => stats.spread_captures += 1,
            TimeFrame::Tick => stats.microstructure_signals += 1,
        }
    }
    
    fn process_hft_signal(&self, signal: HftSignal, timestamp: u64) -> Result<()> {
        // 檢查智能節流
        {
            let throttle = self.smart_throttle.lock().unwrap();
            if !throttle.can_trade(timestamp) {
                return Ok(());  // 被節流控制拒絕
            }
        }
        
        // 執行交易邏輯
        let pnl = self.simulate_hft_execution(&signal);
        
        // 更新節流控制器
        {
            let mut throttle = self.smart_throttle.lock().unwrap();
            throttle.update(timestamp, pnl);
        }
        
        // 記錄交易
        self.record_trade(&signal, pnl);
        
        Ok(())
    }
    
    fn simulate_hft_execution(&self, signal: &HftSignal) -> f64 {
        let execution_start = now_micros();
        
        // 模擬執行延遲（微秒級）
        let fee = signal.position_size * 0.0004;  // 0.04% 手續費
        let slippage = signal.position_size * 0.0001;  // 0.01% 滑點
        
        let execution_latency = now_micros() - execution_start;
        
        // 模擬PnL（基於信號類型和市場條件）
        let expected_pnl = match signal.signal_type {
            HftTradeType::Scalping => signal.confidence * 0.5 - fee - slippage,
            HftTradeType::SpreadCapture => signal.confidence * 0.3 - fee,
            HftTradeType::MicrostructureArbitrage => signal.confidence * 0.4 - fee - slippage,
            HftTradeType::LiquidityProvision => 0.2 - fee,
        };
        
        info!("🧠 DL-HFT: {:?} | Type: {:?} | Size: {:.1} | ExpPnL: {:.4} | Lat: {}μs", 
             signal.direction, signal.signal_type, signal.position_size, expected_pnl, execution_latency);
        info!("   📊 {}", signal.reasoning);
        
        expected_pnl
    }
    
    fn record_trade(&self, signal: &HftSignal, pnl: f64) {
        let mut stats = self.stats.lock().unwrap();
        
        match signal.signal_type {
            HftTradeType::Scalping => stats.scalping_trades += 1,
            HftTradeType::SpreadCapture => stats.spread_captures += 1,
            HftTradeType::MicrostructureArbitrage => stats.arbitrage_opportunities += 1,
            _ => {}
        }
        
        stats.total_trades += 1;
        stats.realized_pnl += pnl;
        
        if pnl > 0.0 {
            stats.winning_trades += 1;
        }
        
        stats.microstructure_signals += 1;
    }
    
    fn parse_lob_data(&self, data: &Value, timestamp: u64) -> Result<OrderBook> {
        // 復用之前的解析邏輯
        let data_array = data.as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid data format"))?;
        
        let first_item = data_array.first()
            .ok_or_else(|| anyhow::anyhow!("Empty data array"))?;

        let mut orderbook = OrderBook::new("BTCUSDT".to_string());
        
        // 解析更多層深度（用於微結構分析）
        if let Some(bid_data) = first_item.get("bids").and_then(|v| v.as_array()) {
            for bid in bid_data.iter().take(20) {  // 20層深度
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
        
        orderbook.last_update = timestamp;
        orderbook.is_valid = !orderbook.bids.is_empty() && !orderbook.asks.is_empty();
        orderbook.data_quality_score = 1.0;
        
        Ok(orderbook)
    }
    
    fn update_stats(&self, processing_latency: u64) {
        let mut stats = self.stats.lock().unwrap();
        stats.total_updates += 1;
        
        // 更新延遲統計
        let current_avg = stats.avg_signal_latency_us;
        let new_latency = processing_latency as f64;
        stats.avg_signal_latency_us = if stats.total_updates == 1 {
            new_latency
        } else {
            (current_avg * 0.95) + (new_latency * 0.05)  // 指數移動平均
        };
    }
    
    pub fn get_hft_stats(&self) -> HftStats {
        self.stats.lock().unwrap().clone()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日誌
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("🚀 Starting DL-Enhanced HFT Trading Demo");
    info!("🧠 Strategy: Deep Learning LOB prediction + HFT execution");

    // 創建真正的HFT系統
    let hft_system = TrueHftMicrostructureSystem::new()?;
    info!("✅ HFT microstructure system initialized");

    // 創建Bitget配置
    let config = BitgetConfig {
        public_ws_url: "wss://ws.bitget.com/v2/ws/public".to_string(),
        private_ws_url: "wss://ws.bitget.com/v2/ws/private".to_string(),
        timeout_seconds: 10,
        auto_reconnect: true,
        max_reconnect_attempts: 3,
    };

    // 創建連接器
    let mut connector = BitgetConnector::new(config);
    connector.add_subscription("BTCUSDT".to_string(), BitgetChannel::Books5);

    info!("📊 Subscribed to BTCUSDT books5 channel for microstructure analysis");

    // 統計計數器
    let stats_counter = Arc::new(Mutex::new(0u64));
    let start_time = std::time::Instant::now();

    // 系統引用
    let system_arc = Arc::new(Mutex::new(hft_system));
    let system_clone = system_arc.clone();
    let stats_clone = stats_counter.clone();

    // 創建消息處理器
    let message_handler = move |message: BitgetMessage| {
        match message {
            BitgetMessage::OrderBook { symbol, data, timestamp, .. } => {
                // 處理HFT LOB數據
                if let Ok(mut system) = system_clone.lock() {
                    if let Err(e) = system.process_lob_for_hft(&symbol, &data, timestamp) {
                        warn!("HFT system processing error: {}", e);
                    }
                }

                // 更新統計
                {
                    let mut count = stats_clone.lock().unwrap();
                    *count += 1;

                    // 每50次更新顯示統計信息（更頻繁）
                    if *count % 50 == 0 {
                        let elapsed = start_time.elapsed();
                        let rate = *count as f64 / elapsed.as_secs_f64();
                        
                        if let Ok(system) = system_clone.lock() {
                            let stats = system.get_hft_stats();
                            info!("📊 Updates: {}, Rate: {:.1}/s, Signals: {}, Trades: {}", 
                                 *count, rate, stats.microstructure_signals, stats.total_trades);
                            info!("   💰 PnL: {:.4} USDT, Win Rate: {:.1}%, Avg Latency: {:.0}μs", 
                                 stats.realized_pnl, 
                                 if stats.total_trades > 0 { stats.winning_trades as f64 / stats.total_trades as f64 * 100.0 } else { 0.0 },
                                 stats.avg_signal_latency_us);
                            info!("   🎯 Scalping: {}, Spread: {}, Arbitrage: {}", 
                                 stats.scalping_trades, stats.spread_captures, stats.arbitrage_opportunities);
                        }
                    }
                }
            }
            _ => {}
        }
    };

    info!("🔌 Connecting to Bitget for HFT microstructure trading...");

    // 設置運行時間限制 - 2分鐘演示
    let timeout = Duration::from_secs(120);
    
    info!("🕐 HFT system will run for 2 minutes");
    info!("💡 Press Ctrl+C to stop early");

    // 啟動連接
    match tokio::time::timeout(timeout, connector.connect_public(message_handler)).await {
        Ok(Ok(_)) => {
            info!("✅ HFT microstructure trading completed successfully");
        }
        Ok(Err(e)) => {
            warn!("❌ Connection failed: {}", e);
            return Err(e);
        }
        Err(_) => {
            info!("⏰ HFT demo completed after 2 minutes");
        }
    }

    // 最終統計
    if let Ok(system) = system_arc.lock() {
        let stats = system.get_hft_stats();
        let final_count = *stats_counter.lock().unwrap();
        let elapsed = start_time.elapsed();
        let rate = final_count as f64 / elapsed.as_secs_f64();

        info!("🏁 Final HFT Statistics:");
        info!("   📈 Total LOB updates: {}", final_count);
        info!("   ⚡ Average rate: {:.1} updates/sec", rate);
        info!("   🎯 Microstructure signals: {}", stats.microstructure_signals);
        info!("   💼 Total trades: {} (Win rate: {:.1}%)", 
             stats.total_trades, 
             if stats.total_trades > 0 { stats.winning_trades as f64 / stats.total_trades as f64 * 100.0 } else { 0.0 });
        info!("   📊 Strategy breakdown:");
        info!("      - Scalping trades: {}", stats.scalping_trades);
        info!("      - Spread captures: {}", stats.spread_captures);  
        info!("      - Arbitrage opportunities: {}", stats.arbitrage_opportunities);
        info!("   💰 Realized P&L: {:.4} USDT", stats.realized_pnl);
        info!("   ⚡ Average signal latency: {:.0}μs", stats.avg_signal_latency_us);
        
        let signal_rate = if elapsed.as_secs() > 0 { 
            stats.microstructure_signals as f64 / elapsed.as_secs() as f64 
        } else { 0.0 };
        info!("   🚀 Signal generation rate: {:.1} signals/sec", signal_rate);
    }

    Ok(())
}