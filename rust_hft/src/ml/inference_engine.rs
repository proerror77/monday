//! ML模型推理引擎
//!
//! 負責加載ML模型並進行實時推理，為交易決策提供信號

use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tracing::{info, warn, error, debug};
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

use crate::core::types::*;
use crate::exchanges::MarketEvent;

/// ML推理引擎
pub struct InferenceEngine {
    /// 當前加載的模型
    current_model: Arc<RwLock<Option<ModelContainer>>>,
    
    /// 特徵提取器
    feature_extractor: FeatureExtractor,
    
    /// 推理配置
    config: InferenceConfig,
    
    /// 推理結果發送器
    signal_sender: mpsc::UnboundedSender<TradingSignal>,
    
    /// 性能統計
    stats: Arc<RwLock<InferenceStats>>,
}

/// 模型容器
pub struct ModelContainer {
    /// 模型名稱
    pub name: String,
    
    /// 模型版本
    pub version: String,
    
    /// 模型類型（mock implementation for now）
    pub model_type: ModelType,
    
    /// 模型參數
    pub parameters: HashMap<String, f32>,
    
    /// 加載時間
    pub loaded_at: u64,
    
    /// 模型元數據
    pub metadata: ModelMetadata,
}

/// 模型類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelType {
    /// 監督學習分類模型
    Supervised,
    
    /// 強化學習模型
    Reinforcement,
    
    /// 時序預測模型
    TimeSeries,
    
    /// 深度神經網絡
    DeepLearning,
    
    /// 集成模型
    Ensemble,
}

/// 模型元數據
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// 輸入特徵數量
    pub input_features: u32,
    
    /// 輸出類別數量
    pub output_classes: u32,
    
    /// 訓練準確率
    pub training_accuracy: f64,
    
    /// 驗證準確率
    pub validation_accuracy: f64,
    
    /// IC (Information Coefficient)
    pub ic: f64,
    
    /// IR (Information Ratio)
    pub ir: f64,
    
    /// 最大回撤
    pub max_drawdown: f64,
    
    /// 夏普比率
    pub sharpe_ratio: f64,
}

/// 特徵提取器
pub struct FeatureExtractor {
    /// 特徵緩存
    feature_cache: Arc<RwLock<HashMap<String, FeatureSet>>>,
    
    /// 特徵配置
    config: FeatureConfig,
}

/// 特徵集
#[derive(Debug, Clone)]
pub struct FeatureSet {
    /// 交易對
    pub symbol: String,
    
    /// 基本特徵
    pub basic_features: Vec<f32>,
    
    /// 技術指標
    pub technical_indicators: Vec<f32>,
    
    /// 市場微觀結構特徵
    pub microstructure_features: Vec<f32>,
    
    /// 時間戳
    pub timestamp: u64,
}

/// 交易信號
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingSignal {
    /// 交易對
    pub symbol: String,
    
    /// 信號類型
    pub signal_type: SignalType,
    
    /// 信號強度 (-1.0 到 1.0)
    pub strength: f64,
    
    /// 置信度 (0.0 到 1.0)
    pub confidence: f64,
    
    /// 建議操作
    pub action: SignalAction,
    
    /// 目標價格
    pub target_price: Option<f64>,
    
    /// 止損價格
    pub stop_loss: Option<f64>,
    
    /// 建議頭寸規模
    pub position_size: Option<f64>,
    
    /// 時效性（毫秒）
    pub validity_ms: u64,
    
    /// 時間戳
    pub timestamp: u64,
    
    /// 模型信息
    pub model_info: ModelInfo,
}

/// 信號類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalType {
    /// 方向性預測
    Directional,
    
    /// 均值回歸
    MeanReversion,
    
    /// 動量跟隨
    Momentum,
    
    /// 波動率預測
    Volatility,
    
    /// 流動性預測
    Liquidity,
}

/// 信號動作
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SignalAction {
    /// 強烈買入
    StrongBuy,
    
    /// 買入
    Buy,
    
    /// 持有
    Hold,
    
    /// 賣出
    Sell,
    
    /// 強烈賣出
    StrongSell,
    
    /// 平倉
    Close,
}

/// 模型信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// 模型名稱
    pub name: String,
    
    /// 模型版本
    pub version: String,
    
    /// 推理延遲（微秒）
    pub inference_latency_us: u64,
}

/// 推理配置
#[derive(Debug, Clone)]
pub struct InferenceConfig {
    /// 最大推理延遲（微秒）
    pub max_inference_latency_us: u64,
    
    /// 最小信號強度閾值
    pub min_signal_strength: f64,
    
    /// 最小置信度閾值
    pub min_confidence: f64,
    
    /// 特徵更新間隔（毫秒）
    pub feature_update_interval_ms: u64,
    
    /// 模型預熱時間（毫秒）
    pub model_warmup_ms: u64,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            max_inference_latency_us: 1000, // 1毫秒
            min_signal_strength: 0.3,
            min_confidence: 0.6,
            feature_update_interval_ms: 100,
            model_warmup_ms: 5000,
        }
    }
}

/// 特徵配置
#[derive(Debug, Clone)]
pub struct FeatureConfig {
    /// 價格回看窗口
    pub price_lookback_window: u32,
    
    /// 成交量回看窗口
    pub volume_lookback_window: u32,
    
    /// 技術指標參數
    pub technical_params: HashMap<String, f64>,
    
    /// 是否啟用微觀結構特徵
    pub enable_microstructure: bool,
}

impl Default for FeatureConfig {
    fn default() -> Self {
        let mut technical_params = HashMap::new();
        technical_params.insert("sma_period".to_string(), 20.0);
        technical_params.insert("ema_period".to_string(), 12.0);
        technical_params.insert("rsi_period".to_string(), 14.0);
        technical_params.insert("bb_period".to_string(), 20.0);
        technical_params.insert("bb_std".to_string(), 2.0);
        
        Self {
            price_lookback_window: 100,
            volume_lookback_window: 50,
            technical_params,
            enable_microstructure: true,
        }
    }
}

/// 推理統計
#[derive(Debug, Clone, Default)]
pub struct InferenceStats {
    /// 總推理次數
    pub total_inferences: u64,
    
    /// 成功推理次數
    pub successful_inferences: u64,
    
    /// 失敗推理次數
    pub failed_inferences: u64,
    
    /// 平均推理延遲（微秒）
    pub avg_inference_latency_us: f64,
    
    /// 信號準確率
    pub signal_accuracy: f64,
    
    /// 模型加載次數
    pub models_loaded: u64,
    
    /// 最後更新時間
    pub last_update: u64,
}

impl InferenceEngine {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<TradingSignal>) {
        let (signal_sender, signal_receiver) = mpsc::unbounded_channel();
        
        let engine = Self {
            current_model: Arc::new(RwLock::new(None)),
            feature_extractor: FeatureExtractor::new(),
            config: InferenceConfig::default(),
            signal_sender,
            stats: Arc::new(RwLock::new(InferenceStats::default())),
        };
        
        (engine, signal_receiver)
    }
    
    pub fn with_config(config: InferenceConfig) -> (Self, mpsc::UnboundedReceiver<TradingSignal>) {
        let (mut engine, receiver) = Self::new();
        engine.config = config;
        (engine, receiver)
    }
    
    /// 啟動推理引擎
    pub async fn start(&self) -> Result<(), String> {
        info!("Starting ML Inference Engine");
        
        // 啟動特徵更新任務
        self.start_feature_update_task().await;
        
        info!("ML Inference Engine started successfully");
        Ok(())
    }
    
    /// 加載模型
    pub async fn load_model(&self, model_path: &str, metadata: ModelMetadata) -> Result<(), String> {
        info!("Loading ML model from: {}", model_path);
        
        // Mock模型加載邏輯
        let model = ModelContainer {
            name: "hft_model".to_string(),
            version: "1.0.0".to_string(),
            model_type: ModelType::DeepLearning,
            parameters: HashMap::new(),
            loaded_at: chrono::Utc::now().timestamp_nanos() as u64,
            metadata,
        };
        
        *self.current_model.write().await = Some(model);
        
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.models_loaded += 1;
        }
        
        info!("Model loaded successfully");
        Ok(())
    }
    
    /// 處理市場事件並執行推理
    pub async fn handle_market_event(&self, event: MarketEvent) -> Result<(), String> {
        // 提取特徵
        let features = self.feature_extractor.extract_features(&event).await?;
        
        // 執行推理
        if let Some(signal) = self.perform_inference(&features).await? {
            // 發送交易信號
            let _ = self.signal_sender.send(signal);
        }
        
        Ok(())
    }
    
    /// 執行推理
    async fn perform_inference(&self, features: &FeatureSet) -> Result<Option<TradingSignal>, String> {
        let start_time = std::time::Instant::now();
        
        let model_guard = self.current_model.read().await;
        let model = match model_guard.as_ref() {
            Some(m) => m,
            None => {
                debug!("No model loaded, skipping inference");
                return Ok(None);
            }
        };
        
        // Mock推理邏輯
        let signal_strength = self.mock_inference(&features, model).await?;
        let confidence = 0.8; // Mock置信度
        
        let inference_latency_us = start_time.elapsed().as_micros() as u64;
        
        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.total_inferences += 1;
            stats.avg_inference_latency_us = (stats.avg_inference_latency_us * (stats.total_inferences - 1) as f64 + inference_latency_us as f64) / stats.total_inferences as f64;
        }
        
        // 檢查信號強度和置信度閾值
        if signal_strength.abs() < self.config.min_signal_strength || confidence < self.config.min_confidence {
            return Ok(None);
        }
        
        // 檢查推理延遲
        if inference_latency_us > self.config.max_inference_latency_us {
            warn!("Inference latency too high: {}μs", inference_latency_us);
        }
        
        // 生成交易信號
        let action = if signal_strength > 0.7 {
            SignalAction::StrongBuy
        } else if signal_strength > 0.3 {
            SignalAction::Buy
        } else if signal_strength < -0.7 {
            SignalAction::StrongSell
        } else if signal_strength < -0.3 {
            SignalAction::Sell
        } else {
            SignalAction::Hold
        };
        
        let signal = TradingSignal {
            symbol: features.symbol.clone(),
            signal_type: SignalType::Directional,
            strength: signal_strength,
            confidence,
            action,
            target_price: None, // TODO: 實現目標價格計算
            stop_loss: None,    // TODO: 實現止損計算
            position_size: Some(0.1), // Mock頭寸規模
            validity_ms: 5000,  // 5秒有效期
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            model_info: ModelInfo {
                name: model.name.clone(),
                version: model.version.clone(),
                inference_latency_us,
            },
        };
        
        Ok(Some(signal))
    }
    
    /// Mock推理實現
    async fn mock_inference(&self, _features: &FeatureSet, _model: &ModelContainer) -> Result<f64, String> {
        // 這裡是Mock實現，實際應該調用真實的ML模型
        // 使用簡單的隨機信號模擬
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let signal = rng.gen_range(-1.0..1.0);
        
        Ok(signal)
    }
    
    /// 獲取推理統計
    pub async fn get_stats(&self) -> InferenceStats {
        self.stats.read().await.clone()
    }
    
    /// 啟動特徵更新任務
    async fn start_feature_update_task(&self) {
        let feature_extractor = self.feature_extractor.clone();
        let update_interval = self.config.feature_update_interval_ms;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(update_interval));
            
            loop {
                interval.tick().await;
                // TODO: 實現定期特徵更新邏輯
                debug!("Feature update task tick");
            }
        });
    }
}

impl FeatureExtractor {
    pub fn new() -> Self {
        Self {
            feature_cache: Arc::new(RwLock::new(HashMap::new())),
            config: FeatureConfig::default(),
        }
    }
    
    /// 從市場事件提取特徵
    pub async fn extract_features(&self, event: &MarketEvent) -> Result<FeatureSet, String> {
        match event {
            MarketEvent::OrderBookUpdate { symbol, bids, asks, timestamp, .. } => {
                // 提取訂單簿特徵
                let basic_features = self.extract_orderbook_features(bids, asks);
                let microstructure_features = if self.config.enable_microstructure {
                    self.extract_microstructure_features(bids, asks)
                } else {
                    Vec::new()
                };
                
                let features = FeatureSet {
                    symbol: symbol.clone(),
                    basic_features,
                    technical_indicators: Vec::new(), // TODO: 實現技術指標
                    microstructure_features,
                    timestamp: *timestamp,
                };
                
                // 緩存特徵
                {
                    let mut cache = self.feature_cache.write().await;
                    cache.insert(symbol.clone(), features.clone());
                }
                
                Ok(features)
            },
            MarketEvent::Trade { symbol, price, quantity, timestamp, .. } => {
                // 提取成交特徵
                let basic_features = vec![*price as f32, *quantity as f32];
                
                let features = FeatureSet {
                    symbol: symbol.clone(),
                    basic_features,
                    technical_indicators: Vec::new(),
                    microstructure_features: Vec::new(),
                    timestamp: *timestamp,
                };
                
                Ok(features)
            },
            _ => Err("Unsupported market event type for feature extraction".to_string()),
        }
    }
    
    /// 提取訂單簿基本特徵
    fn extract_orderbook_features(&self, bids: &[crate::exchanges::message_types::OrderBookLevel], asks: &[crate::exchanges::message_types::OrderBookLevel]) -> Vec<f32> {
        let mut features = Vec::new();
        
        if let (Some(best_bid), Some(best_ask)) = (bids.first(), asks.first()) {
            // 最佳買賣價
            features.push(best_bid.price as f32);
            features.push(best_ask.price as f32);
            
            // 買賣價差
            let spread = best_ask.price - best_bid.price;
            features.push(spread as f32);
            
            // 買賣價差基點
            let spread_bps = (spread / best_bid.price) * 10000.0;
            features.push(spread_bps as f32);
            
            // 最佳檔位數量
            features.push(best_bid.quantity as f32);
            features.push(best_ask.quantity as f32);
            
            // 數量不平衡
            let quantity_imbalance = (best_bid.quantity - best_ask.quantity) / (best_bid.quantity + best_ask.quantity);
            features.push(quantity_imbalance as f32);
        }
        
        // 深度特徵（前5檔）
        let bid_depth: f64 = bids.iter().take(5).map(|level| level.quantity).sum();
        let ask_depth: f64 = asks.iter().take(5).map(|level| level.quantity).sum();
        features.push(bid_depth as f32);
        features.push(ask_depth as f32);
        
        // 深度不平衡
        if bid_depth + ask_depth > 0.0 {
            let depth_imbalance = (bid_depth - ask_depth) / (bid_depth + ask_depth);
            features.push(depth_imbalance as f32);
        } else {
            features.push(0.0);
        }
        
        features
    }
    
    /// 提取市場微觀結構特徵
    fn extract_microstructure_features(&self, bids: &[crate::exchanges::message_types::OrderBookLevel], asks: &[crate::exchanges::message_types::OrderBookLevel]) -> Vec<f32> {
        let mut features = Vec::new();
        
        // 訂單簿斜率（價格梯度）
        if bids.len() >= 2 {
            let bid_slope = (bids[1].price - bids[0].price) / 1.0; // 歸一化
            features.push(bid_slope as f32);
        } else {
            features.push(0.0);
        }
        
        if asks.len() >= 2 {
            let ask_slope = (asks[1].price - asks[0].price) / 1.0;
            features.push(ask_slope as f32);
        } else {
            features.push(0.0);
        }
        
        // 加權平均價格
        let bid_wap = self.calculate_weighted_average_price(bids, 3);
        let ask_wap = self.calculate_weighted_average_price(asks, 3);
        features.push(bid_wap as f32);
        features.push(ask_wap as f32);
        
        features
    }
    
    /// 計算加權平均價格
    fn calculate_weighted_average_price(&self, levels: &[crate::exchanges::message_types::OrderBookLevel], depth: usize) -> f64 {
        let relevant_levels = levels.iter().take(depth);
        let total_quantity: f64 = relevant_levels.clone().map(|l| l.quantity).sum();
        
        if total_quantity == 0.0 {
            return 0.0;
        }
        
        let weighted_sum: f64 = relevant_levels.map(|l| l.price * l.quantity).sum();
        weighted_sum / total_quantity
    }
}

impl Clone for FeatureExtractor {
    fn clone(&self) -> Self {
        Self {
            feature_cache: self.feature_cache.clone(),
            config: self.config.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchanges::message_types::OrderBookLevel;
    
    #[tokio::test]
    async fn test_inference_engine_creation() {
        let (engine, _receiver) = InferenceEngine::new();
        let stats = engine.get_stats().await;
        assert_eq!(stats.total_inferences, 0);
    }
    
    #[tokio::test]
    async fn test_feature_extraction() {
        let extractor = FeatureExtractor::new();
        
        let bids = vec![
            OrderBookLevel { price: 50000.0, quantity: 1.0, order_count: 1 },
            OrderBookLevel { price: 49999.0, quantity: 2.0, order_count: 2 },
        ];
        
        let asks = vec![
            OrderBookLevel { price: 50001.0, quantity: 1.5, order_count: 1 },
            OrderBookLevel { price: 50002.0, quantity: 2.5, order_count: 2 },
        ];
        
        let event = MarketEvent::OrderBookUpdate {
            symbol: "BTCUSDT".to_string(),
            exchange: "test".to_string(),
            bids,
            asks,
            timestamp: chrono::Utc::now().timestamp_nanos() as u64,
            sequence: 1,
            is_snapshot: true,
        };
        
        let features = extractor.extract_features(&event).await.unwrap();
        assert_eq!(features.symbol, "BTCUSDT");
        assert!(!features.basic_features.is_empty());
    }
}