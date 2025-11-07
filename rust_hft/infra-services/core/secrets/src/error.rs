use thiserror::Error;

pub type Result<T> = std::result::Result<T, SecretsError>;

#[derive(Error, Debug)]
pub enum SecretsError {
    #[error("秘密不存在: {0}")]
    SecretNotFound(String),

    #[error("秘密格式錯誤: {0}")]
    InvalidFormat(String),

    #[error("後端連接失敗: {0}")]
    BackendError(String),

    #[error("秘密解析失敗: {0}")]
    ParsingError(String),

    #[error("配置錯誤: {0}")]
    ConfigError(String),

    #[error("序列化/反序列化錯誤: {0}")]
    SerializationError(#[from] serde_json::Error),

    #[error("IO 錯誤: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Vault 後端錯誤: {0}")]
    VaultError(String),

    #[error("AWS 後端錯誤: {0}")]
    AwsError(String),

    #[error("未初始化: 後端未就緒")]
    NotInitialized,

    #[error("緊急停止: {0}")]
    EmergencyStop(String),
}
