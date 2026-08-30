use thiserror::Error;

pub type Result<T> = std::result::Result<T, SecretsError>;

#[derive(Error, Debug)]
pub enum SecretsError {
    #[error("秘密不存在: {0}")]
    SecretNotFound(String),

    #[error("秘密格式錯誤: {0}")]
    InvalidFormat(String),
}
