//! DL 策略配置系統

use hft_core::{HftError, HftResult, Symbol};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// DL 策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlStrategyConfig {
    pub name: String,
    pub symbols: Vec<Symbol>,
    pub model: ModelConfig,
    pub features: FeatureConfig,
    pub inference: InferenceConfig,
    pub risk: DlRiskConfig,
}

/// 模型配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// 模型文件路徑 (.pt TorchScript)
    pub model_path: PathBuf,
    /// 模型版本/哈希 (用於驗證)
    pub model_version: Option<String>,
    /// 設備選擇: "cpu", "cuda:0", "auto"
    pub device: String,
    /// 批次大小
    pub batch_size: usize,
    /// 模型輸入維度
    pub input_dim: usize,
    /// 模型輸出維度
    pub output_dim: usize,
}

/// 特徵配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureConfig {
    /// TopN 檔位數量
    pub top_n: usize,
    /// 時間窗口大小 (用於滑動特徵)
    pub window_size: Option<usize>,
    /// 歸一化方法: "zscore", "minmax", "robust"
    pub normalization: String,
    /// 是否包含價格特徵
    pub include_price: bool,
    /// 是否包含量比特徵
    pub include_volume: bool,
    /// 是否包含失衡特徵
    pub include_imbalance: bool,
    /// 是否包含斜率特徵
    pub include_slope: bool,
    /// 額外特徵配置
    pub custom_features: Vec<String>,
}

/// 推理配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceConfig {
    /// 推理隊列大小 (有界隊列)
    pub queue_capacity: usize,
    /// 推理超時 (毫秒)
    pub timeout_ms: u64,
    /// 最大並發推理數
    pub max_concurrent: usize,
    /// Last-wins 策略 (丟棄舊請求)
    pub last_wins: bool,
    /// 推理觸發閾值
    pub trigger_threshold: f64,
    /// 輸出閾值 (低於此值不產生訂單)
    pub output_threshold: f64,
}

/// DL 專用風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlRiskConfig {
    /// 推理錯誤率閾值 (觸發降級)
    pub max_error_rate: f64,
    /// 推理超時率閾值
    pub max_timeout_rate: f64,
    /// 降級模式: "fallback", "pause", "simple_strategy"
    pub degradation_mode: String,
    /// 恢復條件檢查間隔 (秒)
    pub recovery_check_interval_sec: u64,
    /// 最大推理延遲 (毫秒，超過則降級)
    pub max_inference_latency_ms: u64,
}

impl Default for DlStrategyConfig {
    fn default() -> Self {
        Self {
            name: "dl_strategy_default".to_string(),
            symbols: vec![Symbol::new("BTCUSDT")],
            model: ModelConfig::default(),
            features: FeatureConfig::default(),
            inference: InferenceConfig::default(),
            risk: DlRiskConfig::default(),
        }
    }
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/default_model.pt"),
            model_version: None,
            device: "cpu".to_string(),
            batch_size: 1,
            input_dim: 120, // 根據 PRD: 120 維特徵
            output_dim: 1,  // 單一輸出 (方向/強度)
        }
    }
}

impl Default for FeatureConfig {
    fn default() -> Self {
        Self {
            top_n: 10,
            window_size: Some(30), // 30 個 tick 窗口
            normalization: "zscore".to_string(),
            include_price: true,
            include_volume: true,
            include_imbalance: true,
            include_slope: true,
            custom_features: vec![],
        }
    }
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 100,
            timeout_ms: 5,     // 5ms 推理超時
            max_concurrent: 1, // 單線程推理避免競爭
            last_wins: true,
            trigger_threshold: 0.1, // 市場變化觸發推理
            output_threshold: 0.5,  // 模型輸出閾值
        }
    }
}

impl Default for DlRiskConfig {
    fn default() -> Self {
        Self {
            max_error_rate: 0.05,   // 5% 錯誤率
            max_timeout_rate: 0.10, // 10% 超時率
            degradation_mode: "fallback".to_string(),
            recovery_check_interval_sec: 60,
            max_inference_latency_ms: 10,
        }
    }
}

/// 從配置文件加載 DL 策略配置
pub fn load_config(path: &str) -> HftResult<DlStrategyConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| HftError::Config(format!("讀取配置失敗: {}", e)))?;

    serde_yaml::from_str(&content).map_err(|e| HftError::Config(format!("解析配置失敗: {}", e)))
}

/// 驗證配置合理性
pub fn validate_config(config: &DlStrategyConfig) -> HftResult<()> {
    // 檢查模型文件是否存在
    if !config.model.model_path.exists() {
        return Err(HftError::Config(format!(
            "模型文件不存在: {:?}",
            config.model.model_path
        )));
    }

    // 檢查參數合理性
    if config.features.top_n == 0 {
        return Err(HftError::Config("top_n 必須大於 0".to_string()));
    }

    if config.inference.queue_capacity == 0 {
        return Err(HftError::Config("queue_capacity 必須大於 0".to_string()));
    }

    if config.inference.timeout_ms == 0 {
        return Err(HftError::Config("timeout_ms 必須大於 0".to_string()));
    }

    // 檢查閾值範圍
    if config.risk.max_error_rate < 0.0 || config.risk.max_error_rate > 1.0 {
        return Err(HftError::Config(
            "max_error_rate 必須在 [0.0, 1.0] 範圍內".to_string(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = DlStrategyConfig::default();
        assert_eq!(config.name, "dl_strategy_default");
        assert_eq!(config.features.top_n, 10);
        assert_eq!(config.model.input_dim, 120);
    }

    #[test]
    fn test_config_validation() {
        let mut config = DlStrategyConfig::default();

        // 測試無效的 top_n
        config.features.top_n = 0;
        assert!(validate_config(&config).is_err());

        // 恢復正常值
        config.features.top_n = 10;

        // 測試無效的錯誤率
        config.risk.max_error_rate = 1.5;
        assert!(validate_config(&config).is_err());
    }
}
