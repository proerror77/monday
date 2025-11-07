//! 推理引擎（同步簡化版）
//!
//! 由於 Phase 1 重構後策略接口為同步調用，我們在此實作一個
//! 單線程、同步的推理引擎，保留原有統計及降級邏輯的骨架，
//! 但移除背景併發佇列，確保編譯與整合順暢。

use crate::config::{DlRiskConfig, InferenceConfig};
use crate::model_loader::ModelHandle;
use hft_core::{HftError, HftResult, Timestamp};
use ndarray::Array1;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

/// 單次推理結果
#[derive(Debug, Clone)]
pub struct InferenceResult {
    pub output: Vec<f32>,
    pub timestamp: Timestamp,
    pub inference_latency_us: u64,
    pub model_version: String,
}

/// 推理引擎狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceEngineState {
    Normal,
    Degraded,
}

/// 推理統計資訊
#[derive(Debug, Clone)]
pub struct InferenceStats {
    pub total_requests: u64,
    pub successful_inferences: u64,
    pub failed_inferences: u64,
    pub timeout_count: u64,
    pub avg_latency_us: u64,
    pub error_rate: f64,
    pub timeout_rate: f64,
    pub current_state: InferenceEngineState,
}

#[derive(Debug, Default)]
struct AggregatedStats {
    total_requests: u64,
    successful_inferences: u64,
    failed_inferences: u64,
    timeout_count: u64,
    total_latency_us: u64,
}

impl AggregatedStats {
    fn record_success(&mut self, latency_us: u64) {
        self.total_requests += 1;
        self.successful_inferences += 1;
        self.total_latency_us += latency_us;
    }

    fn record_failure(&mut self) {
        self.total_requests += 1;
        self.failed_inferences += 1;
    }

    #[allow(dead_code)]
    fn record_timeout(&mut self) {
        self.total_requests += 1;
        self.timeout_count += 1;
    }

    fn error_rate(&self) -> f64 {
        if self.total_requests == 0 {
            0.0
        } else {
            self.failed_inferences as f64 / self.total_requests as f64
        }
    }

    fn timeout_rate(&self) -> f64 {
        if self.total_requests == 0 {
            0.0
        } else {
            self.timeout_count as f64 / self.total_requests as f64
        }
    }

    fn avg_latency(&self) -> u64 {
        if self.successful_inferences == 0 {
            0
        } else {
            self.total_latency_us / self.successful_inferences
        }
    }

    fn build_stats(&self, state: InferenceEngineState) -> InferenceStats {
        InferenceStats {
            total_requests: self.total_requests,
            successful_inferences: self.successful_inferences,
            failed_inferences: self.failed_inferences,
            timeout_count: self.timeout_count,
            avg_latency_us: self.avg_latency(),
            error_rate: self.error_rate(),
            timeout_rate: self.timeout_rate(),
            current_state: state,
        }
    }
}

/// 同步推理引擎
pub struct InferenceEngine {
    #[allow(dead_code)]
    config: InferenceConfig,
    risk_config: DlRiskConfig,
    model: Arc<ModelHandle>,
    model_version: String,
    stats: AggregatedStats,
    state: InferenceEngineState,
    consecutive_failures: u32,
}

impl InferenceEngine {
    /// 建立推理引擎實例
    pub fn new(
        model: Arc<ModelHandle>,
        config: InferenceConfig,
        risk_config: DlRiskConfig,
        model_version: Option<String>,
    ) -> HftResult<Self> {
        info!("推理引擎初始化 (同步模式)");
        Ok(Self {
            config,
            risk_config,
            model,
            model_version: model_version.unwrap_or_else(|| "unknown".to_string()),
            stats: AggregatedStats::default(),
            state: InferenceEngineState::Normal,
            consecutive_failures: 0,
        })
    }

    /// 執行同步推理
    pub fn infer(&mut self, features: &Array1<f32>, symbol: &str) -> HftResult<InferenceResult> {
        if self.state == InferenceEngineState::Degraded {
            warn!("推理引擎處於降級模式，返回緊急推理結果");
            return Ok(self.emergency_inference(features));
        }

        let slice = features
            .as_slice()
            .ok_or_else(|| HftError::Execution("特徵資料需為連續切片".to_string()))?;

        let start = Instant::now();
        match self.model.predict(slice) {
            Ok(output) => {
                let latency_us = start.elapsed().as_micros() as u64;
                self.stats.record_success(latency_us);
                self.consecutive_failures = 0;

                Ok(InferenceResult {
                    output,
                    timestamp: Self::current_timestamp(),
                    inference_latency_us: latency_us,
                    model_version: self.model_version.clone(),
                })
            }
            Err(err) => {
                self.stats.record_failure();
                self.consecutive_failures += 1;
                self.evaluate_degradation(symbol);
                Err(err)
            }
        }
    }

    /// 緊急降級推理（線性近似）
    pub fn emergency_inference(&self, features: &Array1<f32>) -> InferenceResult {
        let slice = features.as_slice().unwrap_or(&[]);
        let signal = if slice.is_empty() {
            0.0
        } else {
            let sum: f32 = slice.iter().take(10).copied().sum();
            (sum / 10.0).tanh()
        };

        InferenceResult {
            output: vec![signal],
            timestamp: Self::current_timestamp(),
            inference_latency_us: 0,
            model_version: "emergency_fallback".to_string(),
        }
    }

    /// 取得統計資訊
    pub fn get_stats(&self) -> InferenceStats {
        self.stats.build_stats(self.state)
    }

    /// 取得當前狀態
    pub fn get_state(&self) -> InferenceEngineState {
        self.state
    }

    /// 手動設定狀態（調試用途）
    pub fn set_state(&mut self, new_state: InferenceEngineState) {
        self.state = new_state;
    }

    /// 關閉推理引擎（同步版本僅回傳 Ok）
    pub fn shutdown(&mut self) -> HftResult<()> {
        info!("推理引擎已關閉");
        Ok(())
    }

    fn evaluate_degradation(&mut self, symbol: &str) {
        let error_rate = self.stats.error_rate();
        if self.consecutive_failures >= 3 || error_rate > self.risk_config.max_error_rate {
            warn!("推理錯誤率過高，對 {} 進入降級模式", symbol);
            self.state = InferenceEngineState::Degraded;
        }
    }

    fn current_timestamp() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DlRiskConfig, InferenceConfig, ModelConfig};
    use crate::model_loader::ModelLoader;
    use ndarray::Array1;
    use std::path::PathBuf;

    fn mock_model() -> Arc<ModelHandle> {
        let config = ModelConfig {
            model_path: PathBuf::from("mock.pt"),
            model_version: Some("test".to_string()),
            device: "cpu".to_string(),
            batch_size: 1,
            input_dim: 10,
            output_dim: 1,
        };
        ModelLoader::load_model(config).unwrap()
    }

    #[test]
    fn test_infer_success() {
        let model = mock_model();
        let config = InferenceConfig::default();
        let risk = DlRiskConfig::default();

        let mut engine =
            InferenceEngine::new(model, config, risk, Some("mock".to_string())).unwrap();
        let features = Array1::from(vec![0.1; 10]);
        let result = engine.infer(&features, "BTCUSDT").unwrap();
        assert_eq!(result.output.len(), 1);
    }

    #[test]
    fn test_emergency_inference() {
        let model = mock_model();
        let config = InferenceConfig::default();
        let risk = DlRiskConfig::default();

        let mut engine =
            InferenceEngine::new(model, config, risk, Some("mock".to_string())).unwrap();
        engine.set_state(InferenceEngineState::Degraded);
        let features = Array1::from(vec![0.1; 10]);
        let result = engine.infer(&features, "BTCUSDT").unwrap();
        assert_eq!(result.model_version, "emergency_fallback");
    }
}
