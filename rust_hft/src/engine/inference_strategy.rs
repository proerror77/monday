/*!
 * 基於推理的交易策略
 * 
 * 使用 TorchScript 模型進行預測和交易信號生成
 * 專注於超低延遲推理
 */

use crate::core::types::*;
use crate::engine::unified_engine::{Signal, SignalAction};
use crate::core::config::Config;
use crate::ml::features::{features_to_vector, validate_features};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{info, warn, error, debug};

/// 策略統計
#[derive(Debug, Clone, Default)]
pub struct StrategyStats {
    pub features_processed: u64,
    pub predictions_made: u64,
    pub buy_signals: u64,
    pub sell_signals: u64,
    pub hold_signals: u64,
    pub avg_inference_latency_us: f64,
    pub avg_confidence: f64,
    pub last_prediction: f64,
}

/// 推理策略配置
#[derive(Debug, Clone)]
pub struct InferenceStrategyConfig {
    pub model_path: String,
    pub confidence_threshold: f64,
    pub position_size_multiplier: f64,
    pub max_position_size: f64,
    pub enable_gpu: bool,
}

impl Default for InferenceStrategyConfig {
    fn default() -> Self {
        Self {
            model_path: "models/policy.pt".to_string(),
            confidence_threshold: 0.6,
            position_size_multiplier: 1.0,
            max_position_size: 1.0,
            enable_gpu: false,
        }
    }
}

/// 基於推理的交易策略
pub struct InferenceStrategy {
    config: InferenceStrategyConfig,
    stats: StrategyStats,
    feature_history: VecDeque<FeatureSet>,
    signals_sent: AtomicU64,
    
    // 模型推理接口（當啟用 torchscript 特性時）
    #[cfg(feature = "torchscript")]
    inference_engine: Option<Arc<tokio::sync::Mutex<crate::ml::TorchScriptInference>>>,
}

impl InferenceStrategy {
    pub fn new(config: InferenceStrategyConfig) -> Result<Self> {
        info!("初始化推理策略，模型路徑: {}", config.model_path);
        
        #[cfg(feature = "torchscript")]
        let inference_engine = {
            match crate::ml::TorchScriptInference::load_model(&config.model_path) {
                Ok(engine) => {
                    info!("成功加載 TorchScript 模型");
                    Some(Arc::new(tokio::sync::Mutex::new(engine)))
                }
                Err(e) => {
                    warn!("無法加載 TorchScript 模型: {}，將使用模擬推理", e);
                    None
                }
            }
        };
        
        Ok(Self {
            config,
            stats: StrategyStats::default(),
            feature_history: VecDeque::with_capacity(100),
            signals_sent: AtomicU64::new(0),
            #[cfg(feature = "torchscript")]
            inference_engine,
        })
    }
    
    /// 處理新的特徵集並生成交易信號
    pub async fn process_features(
        &mut self,
        symbol: String,
        features: FeatureSet,
        signal_sender: &Sender<Signal>,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        
        // 驗證特徵
        if !validate_features(&features) {
            warn!("特徵驗證失敗");
            return Ok(());
        }
        
        // 添加到歷史
        self.feature_history.push_back(features.clone());
        if self.feature_history.len() > 100 {
            self.feature_history.pop_front();
        }
        
        // 執行推理
        let prediction = self.run_inference(&features).await?;
        
        // 生成交易信號
        if let Some(signal) = self.generate_signal(symbol, features, prediction) {
            if let Err(e) = signal_sender.send(signal) {
                error!("發送信號失敗: {}", e);
            } else {
                self.signals_sent.fetch_add(1, Ordering::Relaxed);
            }
        }
        
        // 更新統計
        let latency = start.elapsed().as_micros() as f64;
        self.update_stats(latency);
        
        Ok(())
    }
    
    /// 執行模型推理
    async fn run_inference(&self, features: &FeatureSet) -> Result<InferencePrediction> {
        #[cfg(feature = "torchscript")]
        {
            if let Some(ref engine) = self.inference_engine {
                // 準備特徵張量
                let feature_vec = features_to_vector(features);
                
                // 使用 TorchScript 推理
                let mut engine_guard = engine.lock().await;
                
                // 這裡需要將特徵向量轉換為 Tensor
                // 實際實現取決於 tch crate 的 API
                
                // 暫時返回模擬結果
                return Ok(InferencePrediction {
                    action: PredictedAction::Hold,
                    confidence: 0.7,
                    position_size: 0.0,
                });
            }
        }
        
        // 沒有實際模型時的模擬推理
        self.mock_inference(features)
    }
    
    /// 模擬推理（用於測試和開發）
    fn mock_inference(&self, features: &FeatureSet) -> Result<InferencePrediction> {
        // 簡單的規則基礎策略
        let spread_bps = features.spread * 10000.0;
        let imbalance = features.order_flow_imbalance;
        
        let (action, confidence) = if spread_bps > 5.0 && imbalance > 0.3 {
            (PredictedAction::Buy, 0.75)
        } else if spread_bps > 5.0 && imbalance < -0.3 {
            (PredictedAction::Sell, 0.75)
        } else {
            (PredictedAction::Hold, 0.8)
        };
        
        let position_size = match action {
            PredictedAction::Buy | PredictedAction::Sell => {
                (confidence * self.config.position_size_multiplier)
                    .min(self.config.max_position_size)
            }
            PredictedAction::Hold => 0.0,
        };
        
        Ok(InferencePrediction {
            action,
            confidence,
            position_size,
        })
    }
    
    /// 生成交易信號
    fn generate_signal(
        &mut self,
        symbol: String,
        features: FeatureSet,
        prediction: InferencePrediction,
    ) -> Option<Signal> {
        // 檢查置信度閾值
        if prediction.confidence < self.config.confidence_threshold {
            debug!("預測置信度不足: {:.3}", prediction.confidence);
            return None;
        }
        
        // 更新統計
        match prediction.action {
            PredictedAction::Buy => self.stats.buy_signals += 1,
            PredictedAction::Sell => self.stats.sell_signals += 1,
            PredictedAction::Hold => self.stats.hold_signals += 1,
        }
        
        // 生成信號
        let action = match prediction.action {
            PredictedAction::Buy => SignalAction::Buy,
            PredictedAction::Sell => SignalAction::Sell,
            PredictedAction::Hold => return None,
        };
        
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("strategy".to_string(), serde_json::Value::String("InferenceStrategy".to_string()));
        metadata.insert("confidence".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(prediction.confidence).unwrap_or_else(|| serde_json::Number::from(0))));
        metadata.insert("expected_return".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(0.0).unwrap_or_else(|| serde_json::Number::from(0))));
        metadata.insert("risk_score".to_string(), serde_json::Value::Number(serde_json::Number::from_f64(0.0).unwrap_or_else(|| serde_json::Number::from(0))));
        metadata.insert("time_horizon_ms".to_string(), serde_json::Value::Number(serde_json::Number::from(1000)));
        
        Some(Signal {
            id: format!("sig_{}", self.signals_sent.load(Ordering::Relaxed)),
            symbol,
            action,
            quantity: prediction.position_size,
            price: Some(features.mid_price.0),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64,
            confidence: prediction.confidence,
            metadata,
        })
    }
    
    /// 更新統計信息
    fn update_stats(&mut self, latency_us: f64) {
        self.stats.features_processed += 1;
        self.stats.predictions_made += 1;
        
        // 更新平均延遲
        let alpha = 0.01;
        self.stats.avg_inference_latency_us = 
            alpha * latency_us + (1.0 - alpha) * self.stats.avg_inference_latency_us;
    }
    
    /// 獲取策略統計
    pub fn get_stats(&self) -> &StrategyStats {
        &self.stats
    }
    
    /// 重新加載模型
    #[cfg(feature = "torchscript")]
    pub async fn reload_model(&mut self) -> Result<()> {
        if let Some(ref engine) = self.inference_engine {
            let mut engine_guard = engine.lock().await;
            engine_guard.reload_model()?;
            info!("模型重新加載成功");
        }
        Ok(())
    }
}

/// 推理預測結果
#[derive(Debug, Clone)]
struct InferencePrediction {
    action: PredictedAction,
    confidence: f64,
    position_size: f64,
}

/// 預測的動作
#[derive(Debug, Clone, Copy)]
enum PredictedAction {
    Buy,
    Sell,
    Hold,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_inference_strategy_creation() {
        let config = InferenceStrategyConfig::default();
        let strategy = InferenceStrategy::new(config);
        assert!(strategy.is_ok());
    }
    
    #[tokio::test]
    async fn test_mock_inference() {
        let config = InferenceStrategyConfig::default();
        let strategy = InferenceStrategy::new(config).unwrap();
        
        let features = FeatureSet::default_enhanced();
        
        let prediction = strategy.mock_inference(&features).unwrap();
        assert!(prediction.confidence > 0.0);
    }
}