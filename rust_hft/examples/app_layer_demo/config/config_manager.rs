/*!
 * 配置管理器 (Configuration Manager)
 *
 * 提供統一的配置管理接口，支援配置加載、驗證、熱重載和變更通知。
 */

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use super::{AppConfig, ConfigError, ConfigLoader, ConfigResult, EnvironmentResolver};

/// 配置變更通知
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigChangeNotification {
    /// 變更的配置路徑
    pub path: String,

    /// 變更類型 (updated, deleted, added)
    pub change_type: ConfigChangeType,

    /// 舊值 (如果適用)
    pub old_value: Option<serde_json::Value>,

    /// 新值 (如果適用)
    pub new_value: Option<serde_json::Value>,

    /// 變更時間戳
    pub timestamp: u64,

    /// 變更來源
    pub source: String,
}

/// 配置變更類型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfigChangeType {
    /// 配置已更新
    Updated,
    /// 配置已刪除
    Deleted,
    /// 配置已添加
    Added,
    /// 配置重新加載
    Reloaded,
}

/// 配置管理器
pub struct ConfigManager {
    /// 當前配置
    config: Arc<RwLock<AppConfig>>,

    /// 配置加載器
    loader: ConfigLoader,

    /// 環境變數解析器
    env_resolver: EnvironmentResolver,

    /// 變更通知發送器
    change_notifier: broadcast::Sender<ConfigChangeNotification>,

    /// 配置變更接收器 (用於內部監控)
    _change_receiver: broadcast::Receiver<ConfigChangeNotification>,

    /// 配置緩存
    config_cache: Arc<RwLock<HashMap<String, serde_json::Value>>>,

    /// 熱重載監控
    hot_reload_enabled: bool,

    /// 配置版本號
    version: Arc<RwLock<u64>>,
}

impl ConfigManager {
    /// 創建新的配置管理器
    pub fn new() -> Self {
        let (change_sender, change_receiver) = broadcast::channel(1000);

        Self {
            config: Arc::new(RwLock::new(AppConfig::default())),
            loader: ConfigLoader::new(),
            env_resolver: EnvironmentResolver::new(),
            change_notifier: change_sender,
            _change_receiver: change_receiver,
            config_cache: Arc::new(RwLock::new(HashMap::new())),
            hot_reload_enabled: false,
            version: Arc::new(RwLock::new(1)),
        }
    }

    /// 從文件加載配置
    pub async fn load_from_file<P: AsRef<Path>>(&self, path: P) -> ConfigResult<()> {
        debug!("正在從文件加載配置: {:?}", path.as_ref());

        let config = self
            .loader
            .load_from_file(path.as_ref())
            .map_err(|e| ConfigError::ParseError { source: e })?;

        let mut resolved_config = self
            .env_resolver
            .resolve_config(config)
            .map_err(|e| ConfigError::ParseError { source: e })?;

        self.validate_config(&resolved_config)?;
        self.apply_config_defaults(&mut resolved_config);

        // 更新配置
        {
            let mut current_config = self.config.write().await;
            *current_config = resolved_config;
        }

        // 增加版本號
        {
            let mut version = self.version.write().await;
            *version += 1;
        }

        // 發送變更通知
        let notification = ConfigChangeNotification {
            path: "*".to_string(),
            change_type: ConfigChangeType::Reloaded,
            old_value: None,
            new_value: None,
            timestamp: crate::core::types::now_micros(),
            source: "file_reload".to_string(),
        };

        let _ = self.change_notifier.send(notification);

        info!("配置已從文件成功加載: {:?}", path.as_ref());
        Ok(())
    }

    /// 從環境變數加載配置
    pub async fn load_from_env(&self) -> ConfigResult<()> {
        debug!("正在從環境變數加載配置");

        let config = self.env_resolver.load_from_environment().map_err(|e| {
            ConfigError::EnvironmentError {
                var: "MULTIPLE".to_string(),
                reason: e.to_string(),
            }
        })?;

        self.validate_config(&config)?;

        // 更新配置
        {
            let mut current_config = self.config.write().await;
            *current_config = config;
        }

        // 增加版本號
        {
            let mut version = self.version.write().await;
            *version += 1;
        }

        info!("配置已從環境變數成功加載");
        Ok(())
    }

    /// 獲取完整配置的只讀副本
    pub async fn get_config(&self) -> AppConfig {
        self.config.read().await.clone()
    }

    /// 獲取配置的特定部分
    pub async fn get_config_section<T>(&self, path: &str) -> ConfigResult<Option<T>>
    where
        T: for<'de> Deserialize<'de>,
    {
        let config = self.config.read().await;
        let config_value = serde_json::to_value(&*config)
            .map_err(|e| ConfigError::ParseError { source: e.into() })?;

        // 使用 JSON 指針語法導航配置
        let value = if path == "/" || path.is_empty() {
            &config_value
        } else {
            config_value
                .pointer(path)
                .ok_or_else(|| ConfigError::ValidationError {
                    field: path.to_string(),
                    reason: "配置路徑不存在".to_string(),
                })?
        };

        let result: T = serde_json::from_value(value.clone())
            .map_err(|e| ConfigError::ParseError { source: e.into() })?;

        Ok(Some(result))
    }

    /// 更新配置的特定部分
    pub async fn update_config_section<T>(&self, path: &str, value: T) -> ConfigResult<()>
    where
        T: Serialize,
    {
        debug!("正在更新配置段: {}", path);

        let new_value = serde_json::to_value(&value)
            .map_err(|e| ConfigError::ParseError { source: e.into() })?;

        let old_value;
        {
            let mut config = self.config.write().await;
            let mut config_value = serde_json::to_value(&*config)
                .map_err(|e| ConfigError::ParseError { source: e.into() })?;

            // 保存舊值
            old_value = config_value.pointer(path).cloned();

            // 更新值 (使用簡化的路徑更新邏輯)
            self.update_json_path(&mut config_value, path, new_value.clone())?;

            // 將更新後的 JSON 轉回配置結構
            *config = serde_json::from_value(config_value)
                .map_err(|e| ConfigError::ParseError { source: e.into() })?;

            // 驗證更新後的配置
            self.validate_config(&*config)?;
        }

        // 增加版本號
        {
            let mut version = self.version.write().await;
            *version += 1;
        }

        // 發送變更通知
        let notification = ConfigChangeNotification {
            path: path.to_string(),
            change_type: if old_value.is_some() {
                ConfigChangeType::Updated
            } else {
                ConfigChangeType::Added
            },
            old_value,
            new_value: Some(new_value),
            timestamp: crate::core::types::now_micros(),
            source: "runtime_update".to_string(),
        };

        let _ = self.change_notifier.send(notification);

        info!("配置段已更新: {}", path);
        Ok(())
    }

    /// 訂閱配置變更通知
    pub fn subscribe_to_changes(&self) -> broadcast::Receiver<ConfigChangeNotification> {
        self.change_notifier.subscribe()
    }

    /// 啟用配置熱重載
    pub async fn enable_hot_reload<P: AsRef<Path>>(&mut self, config_file: P) -> ConfigResult<()> {
        if self.hot_reload_enabled {
            warn!("配置熱重載已經啟用");
            return Ok(());
        }

        info!("啟用配置熱重載: {:?}", config_file.as_ref());

        // TODO: 實現文件監控邏輯
        // 可以使用 notify crate 來監控文件變更
        // 這裡先標記為啟用狀態
        self.hot_reload_enabled = true;

        Ok(())
    }

    /// 禁用配置熱重載
    pub fn disable_hot_reload(&mut self) {
        self.hot_reload_enabled = false;
        info!("配置熱重載已禁用");
    }

    /// 獲取配置版本號
    pub async fn get_version(&self) -> u64 {
        *self.version.read().await
    }

    /// 驗證配置
    pub fn validate_config(&self, config: &AppConfig) -> ConfigResult<()> {
        // 應用程序基本驗證
        if config.app.name.is_empty() {
            return Err(ConfigError::ValidationError {
                field: "app.name".to_string(),
                reason: "應用程序名稱不能為空".to_string(),
            });
        }

        // 交易所配置驗證
        for (exchange_name, exchange_config) in &config.exchanges {
            if exchange_config.api.base_url.is_empty() {
                return Err(ConfigError::ValidationError {
                    field: format!("exchanges.{}.api.base_url", exchange_name),
                    reason: "API 基礎 URL 不能為空".to_string(),
                });
            }

            if exchange_config.api.timeout_ms == 0 {
                return Err(ConfigError::ValidationError {
                    field: format!("exchanges.{}.api.timeout_ms", exchange_name),
                    reason: "API 超時時間必須大於零".to_string(),
                });
            }
        }

        // 風險配置驗證
        if config.risk_service.max_daily_loss <= 0.0 {
            return Err(ConfigError::ValidationError {
                field: "risk_service.max_daily_loss".to_string(),
                reason: "每日最大虧損必須大於零".to_string(),
            });
        }

        if config.risk_service.max_leverage <= 0.0 {
            return Err(ConfigError::ValidationError {
                field: "risk_service.max_leverage".to_string(),
                reason: "最大槓桿必須大於零".to_string(),
            });
        }

        // 性能配置驗證
        if config.execution_service.target_latency_us == 0 {
            return Err(ConfigError::ValidationError {
                field: "execution_service.target_latency_us".to_string(),
                reason: "目標延遲必須大於零".to_string(),
            });
        }

        debug!("配置驗證通過");
        Ok(())
    }

    /// 應用配置默認值
    fn apply_config_defaults(&self, config: &mut AppConfig) {
        // 為空的配置項應用合理的默認值
        if config.performance.thread_pool_size.is_none() {
            config.performance.thread_pool_size = Some(num_cpus::get());
        }

        // 確保至少有一個 Prometheus 配置
        if !config.monitoring.prometheus.enabled {
            config.monitoring.prometheus.enabled = true;
        }

        debug!("已應用配置默認值");
    }

    /// 更新 JSON 路徑中的值
    fn update_json_path(
        &self,
        json: &mut serde_json::Value,
        path: &str,
        new_value: serde_json::Value,
    ) -> ConfigResult<()> {
        let path_segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

        if path_segments.is_empty() || (path_segments.len() == 1 && path_segments[0].is_empty()) {
            return Err(ConfigError::ValidationError {
                field: path.to_string(),
                reason: "無效的配置路徑".to_string(),
            });
        }

        let mut current = json;

        // 導航到父節點
        for segment in &path_segments[..path_segments.len() - 1] {
            current = current
                .get_mut(segment)
                .ok_or_else(|| ConfigError::ValidationError {
                    field: path.to_string(),
                    reason: format!("配置路徑不存在: {}", segment),
                })?;
        }

        // 更新最終值
        let final_key = path_segments[path_segments.len() - 1];
        if let Some(obj) = current.as_object_mut() {
            obj.insert(final_key.to_string(), new_value);
        } else {
            return Err(ConfigError::ValidationError {
                field: path.to_string(),
                reason: "無法更新非對象節點".to_string(),
            });
        }

        Ok(())
    }

    /// 導出配置到文件
    pub async fn export_config<P: AsRef<Path>>(
        &self,
        path: P,
        format: ConfigFormat,
    ) -> ConfigResult<()> {
        let config = self.config.read().await;

        match format {
            ConfigFormat::Json => {
                let json = serde_json::to_string_pretty(&*config)
                    .map_err(|e| ConfigError::ParseError { source: e.into() })?;

                tokio::fs::write(path.as_ref(), json)
                    .await
                    .map_err(|e| ConfigError::ParseError { source: e.into() })?;
            }
            ConfigFormat::Yaml => {
                let yaml = serde_yaml::to_string(&*config)
                    .map_err(|e| ConfigError::ParseError { source: e.into() })?;

                tokio::fs::write(path.as_ref(), yaml)
                    .await
                    .map_err(|e| ConfigError::ParseError { source: e.into() })?;
            }
            ConfigFormat::Toml => {
                let toml = toml::to_string_pretty(&*config)
                    .map_err(|e| ConfigError::ParseError { source: e.into() })?;

                tokio::fs::write(path.as_ref(), toml)
                    .await
                    .map_err(|e| ConfigError::ParseError { source: e.into() })?;
            }
        }

        info!("配置已導出到: {:?}", path.as_ref());
        Ok(())
    }

    /// 重新加載配置
    pub async fn reload(&self) -> ConfigResult<()> {
        info!("正在重新加載配置...");

        // 這裡可以實現從最後使用的配置源重新加載
        // 目前簡單地觸發一個重載事件
        let notification = ConfigChangeNotification {
            path: "*".to_string(),
            change_type: ConfigChangeType::Reloaded,
            old_value: None,
            new_value: None,
            timestamp: crate::core::types::now_micros(),
            source: "manual_reload".to_string(),
        };

        let _ = self.change_notifier.send(notification);

        info!("配置重新加載完成");
        Ok(())
    }
}

/// 配置格式
#[derive(Debug, Clone, Copy)]
pub enum ConfigFormat {
    Json,
    Yaml,
    Toml,
}

impl Default for ConfigManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_config_manager_creation() {
        let manager = ConfigManager::new();
        let config = manager.get_config().await;

        // 驗證默認配置
        assert_eq!(config.app.name, "rust_hft");
        assert_eq!(config.app.environment, "development");
    }

    #[tokio::test]
    async fn test_config_validation() {
        let manager = ConfigManager::new();
        let mut config = AppConfig::default();

        // 測試無效配置
        config.app.name = "".to_string();
        let result = manager.validate_config(&config);
        assert!(result.is_err());

        // 修復配置
        config.app.name = "test_app".to_string();
        let result = manager.validate_config(&config);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_config_section_update() {
        let manager = ConfigManager::new();

        // 更新應用名稱
        let result = manager
            .update_config_section("/app/name", "new_app_name".to_string())
            .await;
        assert!(result.is_ok());

        // 驗證更新
        let config = manager.get_config().await;
        assert_eq!(config.app.name, "new_app_name");
    }

    #[tokio::test]
    async fn test_config_change_notifications() {
        let manager = ConfigManager::new();
        let mut receiver = manager.subscribe_to_changes();

        // 更新配置
        let _ = manager
            .update_config_section("/app/version", "2.0.0".to_string())
            .await;

        // 接收通知
        let notification = receiver.recv().await.unwrap();
        assert_eq!(notification.path, "/app/version");
        assert_eq!(notification.change_type, ConfigChangeType::Updated);
    }
}
