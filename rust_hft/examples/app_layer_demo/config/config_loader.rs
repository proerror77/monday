/*!
 * 配置加載器 (Configuration Loader)
 *
 * 支援從多種格式 (JSON, YAML, TOML) 和來源 (文件, URL, 數據庫) 加載配置。
 */

use anyhow::{Context, Result};
use std::path::Path;
use tracing::{debug, info, warn};

use super::{AppConfig, ConfigError};

/// 配置加載器
pub struct ConfigLoader {
    /// 支援的文件擴展名
    supported_extensions: Vec<String>,
}

impl ConfigLoader {
    /// 創建新的配置加載器
    pub fn new() -> Self {
        Self {
            supported_extensions: vec![
                "json".to_string(),
                "yaml".to_string(),
                "yml".to_string(),
                "toml".to_string(),
            ],
        }
    }

    /// 從文件加載配置
    pub fn load_from_file<P: AsRef<Path>>(&self, path: P) -> Result<AppConfig> {
        let path_ref = path.as_ref();
        info!("正在加載配置文件: {:?}", path_ref);

        // 檢查文件是否存在
        if !path_ref.exists() {
            return Err(ConfigError::FileNotFound {
                path: path_ref.to_string_lossy().to_string(),
            }
            .into());
        }

        // 讀取文件內容
        let content = std::fs::read_to_string(path_ref)
            .with_context(|| format!("無法讀取配置文件: {:?}", path_ref))?;

        // 根據文件擴展名確定格式
        let extension = path_ref
            .extension()
            .and_then(|ext| ext.to_str())
            .ok_or_else(|| anyhow::anyhow!("無法確定文件格式: {:?}", path_ref))?
            .to_lowercase();

        if !self.supported_extensions.contains(&extension) {
            return Err(anyhow::anyhow!(
                "不支援的配置文件格式: {}. 支援的格式: {:?}",
                extension,
                self.supported_extensions
            ));
        }

        // 解析配置
        let config = match extension.as_str() {
            "json" => self.parse_json(&content)?,
            "yaml" | "yml" => self.parse_yaml(&content)?,
            "toml" => self.parse_toml(&content)?,
            _ => unreachable!("已檢查擴展名支援性"),
        };

        info!("配置文件加載成功: {:?}", path_ref);
        Ok(config)
    }

    /// 從 JSON 字符串加載配置
    pub fn load_from_json(&self, json_str: &str) -> Result<AppConfig> {
        debug!("正在從 JSON 字符串加載配置");
        self.parse_json(json_str)
    }

    /// 從 YAML 字符串加載配置
    pub fn load_from_yaml(&self, yaml_str: &str) -> Result<AppConfig> {
        debug!("正在從 YAML 字符串加載配置");
        self.parse_yaml(yaml_str)
    }

    /// 從 TOML 字符串加載配置
    pub fn load_from_toml(&self, toml_str: &str) -> Result<AppConfig> {
        debug!("正在從 TOML 字符串加載配置");
        self.parse_toml(toml_str)
    }

    /// 從 URL 加載配置
    pub async fn load_from_url(&self, url: &str) -> Result<AppConfig> {
        info!("正在從 URL 加載配置: {}", url);

        let response = reqwest::get(url)
            .await
            .with_context(|| format!("無法獲取配置 URL: {}", url))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "HTTP 請求失敗: {} - {}",
                response.status(),
                url
            ));
        }

        // 嘗試根據 Content-Type 確定格式
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|ct| ct.to_str().ok())
            .map(|s| s.to_string());

        let content = response
            .text()
            .await
            .with_context(|| "無法讀取 HTTP 響應內容")?;

        let config = if content_type
            .as_deref()
            .unwrap_or("")
            .contains("application/json")
        {
            self.parse_json(&content)?
        } else if content_type
            .as_deref()
            .unwrap_or("")
            .contains("application/yaml")
            || content_type.as_deref().unwrap_or("").contains("text/yaml")
        {
            self.parse_yaml(&content)?
        } else if content_type
            .as_deref()
            .unwrap_or("")
            .contains("application/toml")
        {
            self.parse_toml(&content)?
        } else {
            // 嘗試根據 URL 路徑擴展名確定格式
            let format = self.detect_format_from_url(url);
            match format.as_str() {
                "json" => self.parse_json(&content)?,
                "yaml" | "yml" => self.parse_yaml(&content)?,
                "toml" => self.parse_toml(&content)?,
                _ => {
                    warn!("無法確定配置格式，嘗試 JSON 解析");
                    self.parse_json(&content)?
                }
            }
        };

        info!("配置從 URL 加載成功: {}", url);
        Ok(config)
    }

    /// 從多個文件合併配置
    pub fn load_from_multiple_files<P: AsRef<Path>>(&self, paths: &[P]) -> Result<AppConfig> {
        if paths.is_empty() {
            return Err(anyhow::anyhow!("至少需要提供一個配置文件路徑"));
        }

        info!("正在從多個文件合併配置: {} 個文件", paths.len());

        // 加載第一個文件作為基礎配置
        let mut base_config = self.load_from_file(&paths[0])?;

        // 合併其他配置文件
        for path in &paths[1..] {
            let overlay_config = self.load_from_file(path)?;
            base_config = self.merge_configs(base_config, overlay_config)?;
        }

        info!("多文件配置合併完成");
        Ok(base_config)
    }

    /// 驗證配置格式 (不完全解析)
    pub fn validate_format<P: AsRef<Path>>(&self, path: P) -> Result<ConfigFormat> {
        let path_ref = path.as_ref();

        if !path_ref.exists() {
            return Err(ConfigError::FileNotFound {
                path: path_ref.to_string_lossy().to_string(),
            }
            .into());
        }

        let extension = path_ref
            .extension()
            .and_then(|ext| ext.to_str())
            .ok_or_else(|| anyhow::anyhow!("無法確定文件格式: {:?}", path_ref))?
            .to_lowercase();

        let format = match extension.as_str() {
            "json" => ConfigFormat::Json,
            "yaml" | "yml" => ConfigFormat::Yaml,
            "toml" => ConfigFormat::Toml,
            _ => return Err(anyhow::anyhow!("不支援的配置文件格式: {}", extension)),
        };

        // 嘗試快速解析驗證
        let content = std::fs::read_to_string(path_ref)
            .with_context(|| format!("無法讀取文件: {:?}", path_ref))?;

        match format {
            ConfigFormat::Json => {
                serde_json::from_str::<serde_json::Value>(&content)
                    .with_context(|| "JSON 格式驗證失敗")?;
            }
            ConfigFormat::Yaml => {
                serde_yaml::from_str::<serde_yaml::Value>(&content)
                    .with_context(|| "YAML 格式驗證失敗")?;
            }
            ConfigFormat::Toml => {
                toml::from_str::<toml::Value>(&content).with_context(|| "TOML 格式驗證失敗")?;
            }
        }

        debug!("配置格式驗證通過: {:?} - {:?}", path_ref, format);
        Ok(format)
    }

    // ====================== 私有方法 ======================

    /// 解析 JSON 配置
    fn parse_json(&self, content: &str) -> Result<AppConfig> {
        serde_json::from_str(content).with_context(|| "JSON 配置解析失敗")
    }

    /// 解析 YAML 配置
    fn parse_yaml(&self, content: &str) -> Result<AppConfig> {
        serde_yaml::from_str(content).with_context(|| "YAML 配置解析失敗")
    }

    /// 解析 TOML 配置
    fn parse_toml(&self, content: &str) -> Result<AppConfig> {
        toml::from_str(content).with_context(|| "TOML 配置解析失敗")
    }

    /// 從 URL 檢測格式
    fn detect_format_from_url(&self, url: &str) -> String {
        if let Some(path) = url.split('?').next() {
            if let Some(extension) = path.split('.').last() {
                return extension.to_lowercase();
            }
        }
        "json".to_string() // 默認 JSON
    }

    /// 合併兩個配置 (overlay 覆蓋 base)
    fn merge_configs(&self, base: AppConfig, overlay: AppConfig) -> Result<AppConfig> {
        // 將配置轉換為 JSON Value 進行合併
        let mut base_value = serde_json::to_value(base).with_context(|| "基礎配置序列化失敗")?;

        let overlay_value = serde_json::to_value(overlay).with_context(|| "覆蓋配置序列化失敗")?;

        // 執行深度合併
        self.merge_json_values(&mut base_value, overlay_value);

        // 轉換回配置結構
        let merged_config: AppConfig =
            serde_json::from_value(base_value).with_context(|| "合併配置反序列化失敗")?;

        Ok(merged_config)
    }

    /// 深度合併 JSON 值
    fn merge_json_values(&self, base: &mut serde_json::Value, overlay: serde_json::Value) {
        match (base, overlay) {
            (serde_json::Value::Object(base_map), serde_json::Value::Object(overlay_map)) => {
                for (key, value) in overlay_map {
                    if let Some(base_value) = base_map.get_mut(&key) {
                        self.merge_json_values(base_value, value);
                    } else {
                        base_map.insert(key, value);
                    }
                }
            }
            (base_value, overlay_value) => {
                *base_value = overlay_value;
            }
        }
    }
}

/// 配置格式枚舉
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Yaml,
    Toml,
}

impl Default for ConfigLoader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_config_loader_creation() {
        let loader = ConfigLoader::new();
        assert_eq!(loader.supported_extensions.len(), 4);
        assert!(loader.supported_extensions.contains(&"json".to_string()));
        assert!(loader.supported_extensions.contains(&"yaml".to_string()));
    }

    #[test]
    fn test_json_config_parsing() {
        let loader = ConfigLoader::new();
        let json_config = r#"
        {
            "app": {
                "name": "test_app",
                "version": "1.0.0",
                "environment": "test",
                "log_level": "debug",
                "debug_mode": true
            },
            "exchanges": {},
            "accounts": {},
            "order_service": {
                "max_concurrent_orders": 50,
                "order_timeout_ms": 3000,
                "retry": {
                    "max_attempts": 2,
                    "initial_delay_ms": 50,
                    "max_delay_ms": 2000,
                    "backoff_multiplier": 1.5,
                    "jitter": false
                },
                "validation": {
                    "enable_pre_trade_checks": true,
                    "enable_post_trade_checks": true,
                    "max_order_size_validation": true,
                    "symbol_validation": true,
                    "balance_validation": true
                },
                "stats": {
                    "enabled": true,
                    "collection_interval_ms": 500,
                    "retention_period_sec": 1800,
                    "enable_histograms": true
                }
            },
            "risk_service": {
                "max_daily_loss": 500.0,
                "max_order_value": 5000.0,
                "max_position_value": 25000.0,
                "max_leverage": 2.0,
                "max_order_rate": 5,
                "check_interval_ms": 500,
                "circuit_breaker": {
                    "enabled": true,
                    "failure_threshold": 3,
                    "timeout_ms": 30000,
                    "half_open_max_calls": 2
                },
                "risk_models": {}
            },
            "execution_service": {
                "execution_mode": "dry_run",
                "target_latency_us": 50,
                "quality_threshold": 0.98,
                "monitoring": {
                    "enable_latency_tracking": true,
                    "enable_quality_scoring": true,
                    "sample_rate": 1.0,
                    "alert_threshold_us": 500
                },
                "retry": {
                    "max_attempts": 2,
                    "initial_delay_ms": 25,
                    "max_delay_ms": 1000,
                    "backoff_multiplier": 1.5,
                    "jitter": false
                }
            },
            "event_bus": {
                "buffer_size": 5000,
                "broadcast_buffer_size": 500,
                "max_subscribers": 50,
                "persistence": {
                    "enabled": false,
                    "storage_type": "memory",
                    "retention_hours": 12,
                    "batch_size": 50
                },
                "priority_config": {
                    "default_priority": "Normal",
                    "priority_queue_sizes": {},
                    "priority_weights": {}
                },
                "enable_stats": true
            },
            "orderbook": {
                "default_implementation": "Standard",
                "max_levels": 15,
                "quality_threshold": 0.98,
                "validation_interval_us": 500000,
                "cleanup_threshold": 75,
                "stats": {
                    "enabled": true,
                    "collection_interval_ms": 500,
                    "retention_period_sec": 1800,
                    "enable_histograms": true
                }
            },
            "performance": {
                "cpu_affinity": null,
                "thread_pool_size": null,
                "memory": {
                    "initial_heap_size_mb": null,
                    "max_heap_size_mb": null,
                    "gc_strategy": "default"
                },
                "network": {
                    "tcp_nodelay": true,
                    "tcp_keepalive": true,
                    "buffer_sizes": {
                        "read_buffer_size": 8192,
                        "write_buffer_size": 8192,
                        "receive_buffer_size": 65536,
                        "send_buffer_size": 65536
                    }
                },
                "jit": {
                    "enabled": true,
                    "optimization_level": 2,
                    "inline_threshold": 100
                }
            },
            "monitoring": {
                "prometheus": {
                    "enabled": true,
                    "listen_address": "0.0.0.0:9090",
                    "metrics_path": "/metrics",
                    "scrape_interval_sec": 15
                },
                "metrics": {
                    "enabled": true,
                    "buffer_size": 1000,
                    "flush_interval_ms": 1000,
                    "tags": {}
                },
                "alerting": {
                    "enabled": false,
                    "channels": [],
                    "rules": []
                },
                "tracing": {
                    "enabled": false,
                    "sampler_type": "const",
                    "sampler_param": 1.0,
                    "jaeger_endpoint": null
                }
            },
            "security": {
                "tls": {
                    "enabled": false,
                    "cert_file": null,
                    "key_file": null,
                    "ca_file": null,
                    "verify_peer": true
                },
                "encryption": {
                    "enabled": false,
                    "algorithm": "AES256-GCM",
                    "key_rotation_interval_hours": 24
                },
                "access_control": {
                    "enabled": false,
                    "default_policy": "deny",
                    "rules": []
                },
                "audit": {
                    "enabled": false,
                    "log_level": "info",
                    "events": [],
                    "retention_days": 30
                }
            }
        }
        "#;

        let config = loader.load_from_json(json_config).unwrap();
        assert_eq!(config.app.name, "test_app");
        assert_eq!(config.app.version, "1.0.0");
        assert_eq!(config.order_service.max_concurrent_orders, 50);
        assert_eq!(config.risk_service.max_daily_loss, 500.0);
    }

    #[test]
    fn test_file_loading() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let json_config = r#"
        {
            "app": {
                "name": "file_test",
                "version": "1.0.0",
                "environment": "test",
                "log_level": "info",
                "debug_mode": false
            }
        }
        "#;

        write!(temp_file, "{}", json_config).unwrap();
        temp_file.flush().unwrap();

        let loader = ConfigLoader::new();

        // 測試不支援的格式
        let result = loader.load_from_file("/tmp/test.unsupported");
        assert!(result.is_err());

        // 重命名文件以測試 JSON 加載
        let json_path = temp_file.path().with_extension("json");
        std::fs::copy(temp_file.path(), &json_path).unwrap();

        let config = loader.load_from_file(&json_path).unwrap();
        assert_eq!(config.app.name, "file_test");

        // 清理
        std::fs::remove_file(json_path).ok();
    }

    #[test]
    fn test_format_validation() {
        let loader = ConfigLoader::new();

        // 測試有效的 JSON
        let mut json_file = NamedTempFile::new().unwrap();
        write!(json_file, r#"{{"test": "value"}}"#).unwrap();
        json_file.flush().unwrap();

        let json_path = json_file.path().with_extension("json");
        std::fs::copy(json_file.path(), &json_path).unwrap();

        let format = loader.validate_format(&json_path).unwrap();
        assert_eq!(format, ConfigFormat::Json);

        // 測試無效的 JSON
        let mut invalid_json_file = NamedTempFile::new().unwrap();
        write!(invalid_json_file, r#"{{"invalid": "json"}}"#).unwrap();
        invalid_json_file.flush().unwrap();

        let invalid_json_path = invalid_json_file.path().with_extension("json");
        std::fs::copy(invalid_json_file.path(), &invalid_json_path).unwrap();

        let result = loader.validate_format(&invalid_json_path);
        assert!(result.is_err());

        // 清理
        std::fs::remove_file(json_path).ok();
        std::fs::remove_file(invalid_json_path).ok();
    }
}
