//! 安全密鑰管理抽象層
//!
//! 提供統一的秘密管理接口，支持多個後端：
//! - 環境變數（開發環境）
//! - HashiCorp Vault（生產環境）
//! - AWS Secrets Manager（雲環境）
//! - 本地加密儲存
//!
//! # 例子
//!
//! ```ignore
//! let manager = SecretsManager::new(
//!     SecretsConfig::from_env()
//! ).await?;
//!
//! let api_key = manager.get_secret("bitget::api_key").await?;
//! let credentials = manager.parse_venue_credentials("bitget", &api_key)?;
//! ```

mod backends;
mod error;
mod types;

pub use backends::SecretsManager;
pub use error::{Result, SecretsError};
pub use types::{SecretValue, SecretsConfig, VenueCredentials};

use async_trait::async_trait;
use std::collections::HashMap;

/// 秘密後端 trait - 所有秘密儲存實現的統一接口
#[async_trait]
pub trait SecretsBackend: Send + Sync {
    /// 獲取單個秘密
    async fn get_secret(&self, key: &str) -> Result<SecretValue>;

    /// 批量獲取秘密（性能最佳化）
    async fn get_bulk(&self, keys: &[&str]) -> Result<HashMap<String, SecretValue>> {
        let mut result = HashMap::with_capacity(keys.len());
        for key in keys {
            result.insert(key.to_string(), self.get_secret(key).await?);
        }
        Ok(result)
    }

    /// 驗證秘密後端連接性
    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    /// 清理快取（可選）
    async fn clear_cache(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_secrets_manager_creation() {
        let config = SecretsConfig::env_backend();
        let manager = SecretsManager::new(config).await;
        assert!(manager.is_ok());
    }
}
