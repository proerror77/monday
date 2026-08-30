use tracing::debug;

use crate::{Result, SecretValue, SecretsError};

const SECRET_PREFIX: &str = "HFT_SECRET_";

/// 從環境變數讀取秘密。
pub struct SecretsManager;

impl SecretsManager {
    pub fn from_env() -> Self {
        Self
    }

    /// 獲取單個秘密
    pub fn get_secret(&self, key: &str) -> Result<SecretValue> {
        debug!("獲取秘密: {}", key);
        let env_name = format!(
            "{}{}",
            SECRET_PREFIX,
            key.to_uppercase().replace("::", "_").replace('-', "_")
        );
        match std::env::var(&env_name) {
            Ok(value) => Ok(SecretValue::new(key, value.into_bytes())),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_mapped_to_environment_name() {
        std::env::set_var("HFT_SECRET_BINANCE_SECRET", "value");
        let secret = SecretsManager::from_env()
            .get_secret("binance-secret")
            .unwrap();
        assert_eq!(secret.to_string_safe().unwrap(), "value");
        std::env::remove_var("HFT_SECRET_BINANCE_SECRET");
    }

    #[test]
    fn missing_secret_fails_closed() {
        std::env::remove_var("HFT_SECRET_MISSING_TEST_KEY");
        let result = SecretsManager::from_env().get_secret("missing_test_key");
        assert!(matches!(result, Err(SecretsError::SecretNotFound(_))));
    }
}
