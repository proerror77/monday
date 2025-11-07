use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::ZeroizeOnDrop;

/// 秘密值 - 自動清理內存，禁止 Debug/Display 輸出
#[derive(Clone, ZeroizeOnDrop)]
pub struct SecretValue {
    #[zeroize(skip)]
    name: String,

    #[zeroize(drop)]
    data: Vec<u8>,
}

impl SecretValue {
    pub fn new(name: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            data,
        }
    }

    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.data)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    pub fn to_string_safe(&self) -> Result<String, std::str::Utf8Error> {
        self.as_str().map(|s| s.to_string())
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretValue")
            .field("name", &self.name)
            .field("data", &"***REDACTED***")
            .finish()
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretValue({})", &self.name)
    }
}

/// 交易所認證信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueCredentials {
    pub api_key: String,
    pub secret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<String>,
}

impl VenueCredentials {
    pub fn new(api_key: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            secret: secret.into(),
            passphrase: None,
        }
    }

    pub fn with_passphrase(
        api_key: impl Into<String>,
        secret: impl Into<String>,
        passphrase: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            secret: secret.into(),
            passphrase: Some(passphrase.into()),
        }
    }
}

/// 秘密管理器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecretsConfig {
    /// 環境變數後端（開發環境）
    EnvBackend {
        #[serde(default)]
        prefix: String, // 例: "HFT_SECRET_"
    },

    /// HashiCorp Vault 後端（生產環境）
    VaultBackend {
        address: String,
        token: String,
        namespace: String,
    },

    /// AWS Secrets Manager 後端
    AwsBackend {
        region: String,
        role_arn: Option<String>,
    },

    /// 本地加密檔案後端
    LocalBackend {
        file_path: String,
        key_derivation_salt: Option<Vec<u8>>,
    },
}

impl SecretsConfig {
    /// 使用環境變數後端（預設）
    pub fn env_backend() -> Self {
        Self::EnvBackend {
            prefix: "HFT_SECRET_".to_string(),
        }
    }

    /// 從環境變數讀取配置
    pub fn from_env() -> Self {
        if let Ok(vault_addr) = std::env::var("HFT_VAULT_ADDR") {
            Self::VaultBackend {
                address: vault_addr,
                token: std::env::var("HFT_VAULT_TOKEN").unwrap_or_default(),
                namespace: std::env::var("HFT_VAULT_NAMESPACE")
                    .unwrap_or_else(|_| "hft".to_string()),
            }
        } else if std::env::var("AWS_REGION").is_ok() {
            Self::AwsBackend {
                region: std::env::var("AWS_REGION").unwrap_or_else(|_| "us-east-1".to_string()),
                role_arn: std::env::var("HFT_AWS_ROLE_ARN").ok(),
            }
        } else {
            Self::env_backend()
        }
    }

    /// 驗證配置有效性
    pub fn validate(&self) -> crate::Result<()> {
        match self {
            SecretsConfig::EnvBackend { .. } => Ok(()),
            SecretsConfig::VaultBackend { address, token, .. } => {
                if address.is_empty() {
                    return Err(crate::SecretsError::ConfigError(
                        "Vault address cannot be empty".to_string(),
                    ));
                }
                if token.is_empty() {
                    return Err(crate::SecretsError::ConfigError(
                        "Vault token cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
            SecretsConfig::AwsBackend { region, .. } => {
                if region.is_empty() {
                    return Err(crate::SecretsError::ConfigError(
                        "AWS region cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
            SecretsConfig::LocalBackend { file_path, .. } => {
                if file_path.is_empty() {
                    return Err(crate::SecretsError::ConfigError(
                        "Local file path cannot be empty".to_string(),
                    ));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secret_value_redaction() {
        let secret = SecretValue::new("api_key", b"super_secret".to_vec());
        let debug_str = format!("{:?}", secret);
        assert!(!debug_str.contains("super_secret"));
        assert!(debug_str.contains("REDACTED"));
    }

    #[test]
    fn test_vault_config_validation() {
        let valid = SecretsConfig::VaultBackend {
            address: "http://vault:8200".to_string(),
            token: "s.xxxxx".to_string(),
            namespace: "hft".to_string(),
        };
        assert!(valid.validate().is_ok());

        let invalid = SecretsConfig::VaultBackend {
            address: String::new(),
            token: "s.xxxxx".to_string(),
            namespace: "hft".to_string(),
        };
        assert!(invalid.validate().is_err());
    }
}
