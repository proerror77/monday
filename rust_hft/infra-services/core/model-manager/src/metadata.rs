//! Model metadata types

use serde::{Deserialize, Serialize};
use std::fmt;

/// 模型版本標識
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ModelVersion(pub String);

impl Default for ModelVersion {
    fn default() -> Self {
        Self("v0.0.0".to_string())
    }
}

impl fmt::Display for ModelVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ModelVersion {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ModelVersion {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// 訓練指標
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrainingMetrics {
    /// Information Coefficient
    pub ic: f64,
    /// Information Ratio
    pub ir: f64,
    /// Maximum Drawdown
    pub max_drawdown: f64,
    /// Sharpe Ratio
    pub sharpe: f64,
    /// 訓練損失
    pub train_loss: f64,
    /// 驗證損失
    pub val_loss: f64,
}

/// 模型元數據
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// 當前版本
    pub current_version: ModelVersion,

    /// 模型類型 (e.g., "lstm_attention", "tcn", "deep_lob")
    pub model_type: String,

    /// 輸入特徵維度
    pub input_dim: usize,

    /// 輸出維度
    pub output_dim: usize,

    /// 序列長度
    pub sequence_length: usize,

    /// 訓練時間
    pub trained_at: Option<String>,

    /// 訓練數據範圍
    pub data_range: Option<DataRange>,

    /// 訓練指標
    pub metrics: TrainingMetrics,

    /// 部署條件
    pub deployment_criteria: DeploymentCriteria,

    /// 歷史版本記錄
    pub version_history: Vec<VersionRecord>,
}

impl Default for ModelMetadata {
    fn default() -> Self {
        Self {
            current_version: ModelVersion::default(),
            model_type: "unknown".to_string(),
            input_dim: 39,
            output_dim: 1,
            sequence_length: 100,
            trained_at: None,
            data_range: None,
            metrics: TrainingMetrics::default(),
            deployment_criteria: DeploymentCriteria::default(),
            version_history: Vec::new(),
        }
    }
}

/// 數據範圍
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataRange {
    pub start: String,
    pub end: String,
    pub records: usize,
}

/// 部署條件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentCriteria {
    /// 最小 IC 閾值
    pub min_ic: f64,
    /// 最小 IR 閾值
    pub min_ir: f64,
    /// 最大回撤閾值
    pub max_drawdown: f64,
}

impl Default for DeploymentCriteria {
    fn default() -> Self {
        Self {
            min_ic: 0.03,
            min_ir: 1.2,
            max_drawdown: 0.05,
        }
    }
}

impl DeploymentCriteria {
    /// 檢查指標是否達標
    pub fn is_met(&self, metrics: &TrainingMetrics) -> bool {
        metrics.ic >= self.min_ic
            && metrics.ir >= self.min_ir
            && metrics.max_drawdown <= self.max_drawdown
    }
}

/// 版本記錄
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionRecord {
    pub version: ModelVersion,
    pub deployed_at: String,
    pub archived_at: Option<String>,
    pub metrics: TrainingMetrics,
    pub status: VersionStatus,
}

/// 版本狀態
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VersionStatus {
    /// 當前使用中
    Active,
    /// 已歸檔
    Archived,
    /// 已棄用
    Deprecated,
    /// 回滾來源
    RolledBack,
}
