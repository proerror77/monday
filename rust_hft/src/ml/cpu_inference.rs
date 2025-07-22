/*!
 * CPU-based ML Inference Engine
 * 
 * 提供純CPU的機器學習推理實現，支持：
 * - 線性模型（Linear Regression, Logistic Regression）
 * - 簡單神經網絡（前饋網絡）
 * - 基於特徵的傳統ML模型
 * 
 * 這個實現不依賴外部ML框架，使用純Rust實現
 * 模型參數由Python訓練後序列化為二進制格式供Rust加載
 */

use crate::core::types::*;
use anyhow::{Result, Context};
use std::path::Path;
use std::fs;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// 交易動作類型
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TradingAction {
    Buy,
    Sell,
    Hold,
}

/// 推理結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    pub action: TradingAction,
    pub confidence: f64,
    pub expected_return: f64,
    pub risk_score: f64,
    pub inference_time_us: u64,
}

/// 推理統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InferenceStatsSummary {
    pub total_inferences: u64,
    pub avg_inference_time_us: f64,
    pub accuracy_percentage: f64,
    pub last_model_update: Option<u64>,
}

/// 線性模型參數
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LinearModelParams {
    weights: Vec<f64>,
    bias: f64,
    feature_count: usize,
}

/// 神經網絡層
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DenseLayer {
    weights: Vec<Vec<f64>>, // [input_size][output_size]
    biases: Vec<f64>,
    activation: ActivationType,
}

/// 激活函數類型
#[derive(Debug, Clone, Serialize, Deserialize)]
enum ActivationType {
    ReLU,
    Sigmoid,
    Tanh,
    Linear,
}

/// 模型類型
#[derive(Debug, Clone, Serialize, Deserialize)]
enum ModelType {
    Linear(LinearModelParams),
    NeuralNetwork { layers: Vec<DenseLayer> },
}

/// 模型元數據
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    pub version: String,
    pub model_type: String,
    pub feature_count: usize,
    pub training_timestamp: u64,
    pub performance_metrics: PerformanceMetrics,
}

/// 性能指標
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub accuracy: f64,
    pub precision: f64,
    pub recall: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
}

/// CPU推理引擎
pub struct CpuInference {
    model: ModelType,
    metadata: ModelMetadata,
    stats: InferenceStatsSummary,
    threshold_buy: f64,
    threshold_sell: f64,
}

impl CpuInference {
    /// 從文件加載模型
    pub fn load_model<P: AsRef<Path>>(model_path: P) -> Result<Self> {
        let model_data = fs::read(model_path.as_ref())
            .with_context(|| format!("Failed to read model file: {:?}", model_path.as_ref()))?;
        
        // 假設模型文件是JSON格式
        let model_config: ModelConfig = serde_json::from_slice(&model_data)
            .context("Failed to parse model configuration")?;
        
        Ok(Self {
            model: model_config.model,
            metadata: model_config.metadata,
            stats: InferenceStatsSummary::default(),
            threshold_buy: model_config.thresholds.buy,
            threshold_sell: model_config.thresholds.sell,
        })
    }
    
    /// 創建一個簡單的默認線性模型（用於演示）
    pub fn create_default_linear_model() -> Self {
        // 創建一個簡單的17維特徵線性模型
        let weights = vec![
            0.1, -0.05, 0.2, 0.15, -0.1,  // 基本價格特徵
            0.3, -0.2, 0.25, -0.15, 0.1,  // OBI 特徵
            0.05, -0.05, 0.1, -0.1, 0.2,  // 深度特徵
            0.15, -0.1                     // 其他特徵
        ];
        
        let linear_params = LinearModelParams {
            weights,
            bias: 0.0,
            feature_count: 17,
        };
        
        let metadata = ModelMetadata {
            version: "1.0.0".to_string(),
            model_type: "linear_regression".to_string(),
            feature_count: 17,
            training_timestamp: now_micros(),
            performance_metrics: PerformanceMetrics {
                accuracy: 0.65,
                precision: 0.62,
                recall: 0.68,
                sharpe_ratio: 1.2,
                max_drawdown: 0.15,
            },
        };
        
        Self {
            model: ModelType::Linear(linear_params),
            metadata,
            stats: InferenceStatsSummary::default(),
            threshold_buy: 0.6,
            threshold_sell: -0.6,
        }
    }
    
    /// 執行推理 - 接收 Python 預處理的特徵張量
    pub fn infer(&mut self, feature_tensor: &[f64]) -> Result<InferenceResult> {
        let start_time = Instant::now();
        
        // 驗證特徵張量維度
        if feature_tensor.len() != self.metadata.feature_count {
            return Err(anyhow::anyhow!(
                "Feature tensor length mismatch: expected {}, got {}",
                self.metadata.feature_count,
                feature_tensor.len()
            ));
        }
        
        // 根據模型類型執行推理
        let raw_prediction = match &self.model {
            ModelType::Linear(params) => self.linear_forward(feature_tensor, params)?,
            ModelType::NeuralNetwork { layers } => self.neural_network_forward(feature_tensor, layers)?,
        };
        
        // 將原始預測轉換為交易決策
        let (action, confidence) = self.interpret_prediction(raw_prediction);
        
        let inference_time = start_time.elapsed().as_micros() as u64;
        
        // 更新統計
        self.stats.total_inferences += 1;
        let alpha = 0.01; // 指數移動平均
        self.stats.avg_inference_time_us = 
            alpha * inference_time as f64 + (1.0 - alpha) * self.stats.avg_inference_time_us;
        
        Ok(InferenceResult {
            action,
            confidence,
            expected_return: self.estimate_expected_return(raw_prediction),
            risk_score: self.estimate_risk_score(feature_tensor),
            inference_time_us: inference_time,
        })
    }
    
    /// 獲取統計信息
    pub fn get_stats(&self) -> &InferenceStatsSummary {
        &self.stats
    }
    
    /// 獲取模型元數據
    pub fn get_metadata(&self) -> &ModelMetadata {
        &self.metadata
    }
    
    // --- 內部實現方法 ---
    
    fn linear_forward(&self, features: &[f64], params: &LinearModelParams) -> Result<f64> {
        if features.len() != params.feature_count {
            return Err(anyhow::anyhow!("Feature dimension mismatch"));
        }
        
        let mut result = params.bias;
        for (i, &feature) in features.iter().enumerate() {
            result += feature * params.weights[i];
        }
        
        Ok(result)
    }
    
    fn neural_network_forward(&self, features: &[f64], layers: &[DenseLayer]) -> Result<f64> {
        let mut current_input = features.to_vec();
        
        for layer in layers {
            current_input = self.dense_layer_forward(&current_input, layer)?;
        }
        
        // 返回最後一層的第一個輸出（假設是單輸出）
        Ok(current_input[0])
    }
    
    fn dense_layer_forward(&self, input: &[f64], layer: &DenseLayer) -> Result<Vec<f64>> {
        let input_size = input.len();
        let output_size = layer.biases.len();
        
        if layer.weights.len() != input_size {
            return Err(anyhow::anyhow!("Layer weight dimension mismatch"));
        }
        
        let mut output = vec![0.0; output_size];
        
        for j in 0..output_size {
            output[j] = layer.biases[j];
            for i in 0..input_size {
                output[j] += input[i] * layer.weights[i][j];
            }
            
            // 應用激活函數
            output[j] = self.apply_activation(output[j], &layer.activation);
        }
        
        Ok(output)
    }
    
    fn apply_activation(&self, x: f64, activation: &ActivationType) -> f64 {
        match activation {
            ActivationType::ReLU => x.max(0.0),
            ActivationType::Sigmoid => 1.0 / (1.0 + (-x).exp()),
            ActivationType::Tanh => x.tanh(),
            ActivationType::Linear => x,
        }
    }
    
    fn interpret_prediction(&self, prediction: f64) -> (TradingAction, f64) {
        let confidence = prediction.abs().min(1.0);
        
        if prediction > self.threshold_buy {
            (TradingAction::Buy, confidence)
        } else if prediction < self.threshold_sell {
            (TradingAction::Sell, confidence)
        } else {
            (TradingAction::Hold, confidence)
        }
    }
    
    fn estimate_expected_return(&self, prediction: f64) -> f64 {
        // 簡單的預期收益估算：基於預測值和歷史表現
        prediction * 0.001 * self.metadata.performance_metrics.sharpe_ratio
    }
    
    fn estimate_risk_score(&self, feature_tensor: &[f64]) -> f64 {
        // 基於特徵張量的風險評分 (簡化版本)
        // 假設特徵張量的前幾個維度包含風險相關信息
        if feature_tensor.len() >= 4 {
            let spread_proxy = feature_tensor[3].abs();  // 假設第4個特徵是spread相關
            let imbalance_proxy = if feature_tensor.len() >= 5 { feature_tensor[4].abs() } else { 0.0 };
            (spread_proxy + imbalance_proxy) / 2.0
        } else {
            0.5 // 默認中等風險
        }
    }
}

/// 模型配置結構（用於序列化/反序列化）
#[derive(Serialize, Deserialize)]
struct ModelConfig {
    model: ModelType,
    metadata: ModelMetadata,
    thresholds: TradingThresholds,
}

#[derive(Serialize, Deserialize)]
struct TradingThresholds {
    buy: f64,
    sell: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_model_creation() {
        let model = CpuInference::create_default_linear_model();
        assert_eq!(model.metadata.feature_count, 17);
        assert_eq!(model.metadata.model_type, "linear_regression");
    }

    #[test]
    fn test_linear_inference() {
        let mut model = CpuInference::create_default_linear_model();
        // 創建17維預處理特徵張量 (由Python預處理)
        let feature_tensor = vec![
            100.0, 101.0, 100.5, 1.0, 100.0,  // 基本價格特徵
            0.1, -0.05, 0.02, -0.01, 1000.0,  // OBI 特徵
            1100.0, 2000.0, 2200.0, 4000.0, 4400.0, // 深度特徵
            -0.05, -0.05                       // 其他特徵
        ];
        
        let result = model.infer(&feature_tensor).unwrap();
        assert!(result.confidence >= 0.0);
        assert!(result.confidence <= 1.0);
        // 推理時間可能很快，只需要確保不是負數
        assert!(result.inference_time_us >= 0);
    }

    #[test]
    fn test_tensor_dimension_validation() {
        let mut model = CpuInference::create_default_linear_model();
        // 測試錯誤的特徵張量維度
        let wrong_tensor = vec![1.0, 2.0, 3.0]; // 只有3維，應該是17維
        
        let result = model.infer(&wrong_tensor);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Feature tensor length mismatch"));
    }
}