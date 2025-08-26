/*!
 * 集中化配置管理模組 (Centralized Configuration Management)
 *
 * 統一管理系統所有配置項，消除散佈在各處的配置代碼。
 * 支援環境變數覆蓋、配置驗證、熱重載和配置變更通知。
 */

pub mod config_loader;
pub mod config_manager;
pub mod config_types;
pub mod environment;

// 重新導出核心類型
pub use config_loader::ConfigLoader;
pub use config_manager::{ConfigChangeNotification, ConfigManager};
pub use config_types::{
    AppConfig, EventBusConfig, ExchangeConfig, ExecutionServiceConfig, MonitoringConfig,
    OrderBookConfig, OrderServiceConfig, PerformanceConfig, RiskServiceConfig, SecurityConfig,
};
pub use environment::EnvironmentResolver;

/// 配置管理錯誤類型
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("配置文件不存在: {path}")]
    FileNotFound { path: String },

    #[error("配置解析失敗: {source}")]
    ParseError { source: anyhow::Error },

    #[error("配置驗證失敗: {field} - {reason}")]
    ValidationError { field: String, reason: String },

    #[error("環境變數錯誤: {var} - {reason}")]
    EnvironmentError { var: String, reason: String },

    #[error("配置熱重載失敗: {reason}")]
    HotReloadError { reason: String },
}

pub type ConfigResult<T> = Result<T, ConfigError>;
