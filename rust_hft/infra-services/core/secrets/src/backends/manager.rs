use std::sync::Arc;
use tracing::{debug, info};

use crate::{Result, SecretValue, SecretsBackend, SecretsConfig, SecretsError, VenueCredentials};

use super::EnvBackend;

/// 秘密管理器 - 統一的秘密訪問接口
pub struct SecretsManager {
    backend: Arc<dyn SecretsBackend>,
    config: SecretsConfig,
}

impl SecretsManager {
    /// 從配置創建秘密管理器
    pub async fn new(config: SecretsConfig) -> Result<Self> {
        // 驗證配置
        config.validate()?;

        let backend: Arc<dyn SecretsBackend> = match &config {
            SecretsConfig::EnvBackend { prefix } => {
                debug!("初始化環境變數秘密後端，前綴: {}", prefix);
                Arc::new(EnvBackend::new(prefix.clone()))
            }

            SecretsConfig::VaultBackend { address, .. } => {
                info!("初始化 Vault 秘密後端: {}", address);
                // TODO: Phase 2 實現 Vault 後端
                return Err(SecretsError::ConfigError("Vault 後端暫未實現".to_string()));
            }

            SecretsConfig::AwsBackend { region, .. } => {
                info!("初始化 AWS Secrets Manager 後端: {}", region);
                // TODO: Phase 2 實現 AWS 後端
                return Err(SecretsError::ConfigError("AWS 後端暫未實現".to_string()));
            }

            SecretsConfig::LocalBackend { file_path, .. } => {
                info!("初始化本地加密秘密後端: {}", file_path);
                // TODO: Phase 2 實現本地加密後端
                return Err(SecretsError::ConfigError("本地後端暫未實現".to_string()));
            }
        };

        // 健康檢查
        backend.health_check().await?;

        info!("秘密管理器初始化成功");

        Ok(Self { backend, config })
    }

    /// 使用預設環境變數配置創建管理器
    pub async fn from_env() -> Result<Self> {
        Self::new(SecretsConfig::from_env()).await
    }

    /// 獲取單個秘密
    pub async fn get_secret(&self, key: &str) -> Result<SecretValue> {
        debug!("獲取秘密: {}", key);
        self.backend.get_secret(key).await
    }

    /// 批量獲取秘密
    pub async fn get_secrets(&self, keys: &[&str]) -> Result<Vec<SecretValue>> {
        debug!("批量獲取 {} 個秘密", keys.len());
        let map = self.backend.get_bulk(keys).await?;

        // 保持原始順序
        let mut result = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(secret) = map.get(*key) {
                result.push(secret.clone());
            } else {
                return Err(SecretsError::SecretNotFound(key.to_string()));
            }
        }

        Ok(result)
    }

    /// 解析交易所認證信息
    ///
    /// 期望 JSON 格式: `{"api_key": "...", "secret": "...", "passphrase": "..."}`
    pub fn parse_venue_credentials(
        &self,
        _venue: &str,
        secret: &SecretValue,
    ) -> Result<VenueCredentials> {
        let json_str = secret
            .as_str()
            .map_err(|e| SecretsError::ParsingError(format!("秘密值包含無效 UTF-8: {}", e)))?;

        serde_json::from_str::<VenueCredentials>(json_str)
            .map_err(|e| SecretsError::ParsingError(format!("無法解析認證信息: {}", e)))
    }

    /// 健康檢查
    pub async fn health_check(&self) -> Result<()> {
        self.backend.health_check().await
    }

    /// 清理快取（如果支持）
    pub async fn clear_cache(&self) -> Result<()> {
        self.backend.clear_cache().await
    }

    /// 獲取當前配置
    pub fn config(&self) -> &SecretsConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_creation_env() {
        let config = SecretsConfig::env_backend();
        let manager = SecretsManager::new(config).await;
        assert!(manager.is_ok());
    }

    #[tokio::test]
    async fn test_manager_health_check() {
        let manager = SecretsManager::from_env().await.unwrap();
        let result = manager.health_check().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_parse_venue_credentials() {
        std::env::set_var(
            "HFT_SECRET_TEST",
            r#"{"api_key": "key123", "secret": "sec456"}"#,
        );

        let manager = SecretsManager::from_env().await.unwrap();
        let secret = manager.get_secret("test").await.unwrap();
        let creds = manager.parse_venue_credentials("bitget", &secret).unwrap();

        assert_eq!(creds.api_key, "key123");
        assert_eq!(creds.secret, "sec456");
        assert!(creds.passphrase.is_none());

        std::env::remove_var("HFT_SECRET_TEST");
    }
}
