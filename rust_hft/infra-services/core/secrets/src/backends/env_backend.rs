use async_trait::async_trait;
use std::collections::HashMap;
use tracing::debug;

use crate::{Result, SecretValue, SecretsBackend, SecretsError};

/// 環境變數秘密後端 - 用於開發環境
///
/// 前綴範例: `HFT_SECRET_`
/// 環境變數名稱: `HFT_SECRET_BITGET_API_KEY` → 秘密鍵: `bitget::api_key`
pub struct EnvBackend {
    prefix: String,
}

impl EnvBackend {
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    /// 將秘密鍵轉換為環境變數名稱
    /// 例: `bitget::api_key` → `HFT_SECRET_BITGET_API_KEY`
    fn key_to_env_name(&self, key: &str) -> String {
        format!(
            "{}{}",
            self.prefix,
            key.to_uppercase().replace("::", "_").replace("-", "_")
        )
    }
}

#[async_trait]
impl SecretsBackend for EnvBackend {
    async fn get_secret(&self, key: &str) -> Result<SecretValue> {
        let env_name = self.key_to_env_name(key);

        match std::env::var(&env_name) {
            Ok(value) => {
                debug!("從環境變數 {} 讀取秘密: {}", env_name, key);
                Ok(SecretValue::new(key, value.into_bytes()))
            }
            Err(std::env::VarError::NotPresent) => Err(SecretsError::SecretNotFound(format!(
                "環境變數 {} 未設置 (鍵: {})",
                env_name, key
            ))),
            Err(std::env::VarError::NotUnicode(_)) => Err(SecretsError::InvalidFormat(format!(
                "環境變數 {} 包含無效 UTF-8",
                env_name
            ))),
        }
    }

    async fn get_bulk(&self, keys: &[&str]) -> Result<HashMap<String, SecretValue>> {
        let mut result = HashMap::with_capacity(keys.len());
        let mut errors = Vec::new();

        for key in keys {
            match self.get_secret(key).await {
                Ok(secret) => {
                    result.insert(key.to_string(), secret);
                }
                Err(e) => {
                    errors.push(format!("{}: {}", key, e));
                }
            }
        }

        if !errors.is_empty() {
            // 部分成功的情況下只記錄警告，不拋出錯誤
            tracing::warn!("批量獲取秘密時部分失敗: {}", errors.join("; "));
        }

        Ok(result)
    }

    async fn health_check(&self) -> Result<()> {
        // 環境變數後端總是可用
        Ok(())
    }

    async fn clear_cache(&self) -> Result<()> {
        // 環境變數後端無快取
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_key_to_env_name() {
        let backend = EnvBackend::new("HFT_SECRET_");
        assert_eq!(
            backend.key_to_env_name("bitget::api_key"),
            "HFT_SECRET_BITGET_API_KEY"
        );
        assert_eq!(
            backend.key_to_env_name("binance-secret"),
            "HFT_SECRET_BINANCE_SECRET"
        );
    }

    #[tokio::test]
    async fn test_get_secret_not_found() {
        let backend = EnvBackend::new("HFT_SECRET_");
        let result = backend.get_secret("non_existent_key").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_secret_success() {
        // 設置測試環境變數
        std::env::set_var("HFT_SECRET_TEST_KEY", "test_value");

        let backend = EnvBackend::new("HFT_SECRET_");
        let result = backend.get_secret("test_key").await;

        assert!(result.is_ok());
        let secret = result.unwrap();
        assert_eq!(secret.as_str().unwrap(), "test_value");

        // 清理
        std::env::remove_var("HFT_SECRET_TEST_KEY");
    }
}
