use serde::{Deserialize, Serialize};
use thiserror::Error;

/// HFT 系統統一錯誤類型
#[derive(Debug, Error, Clone, Serialize, Deserialize)]
pub enum HftError {
    #[error("網路錯誤: {0}")]
    Network(String),

    #[error("解析錯誤: {0}")]
    Parse(String),

    #[error("序列化錯誤: {0}")]
    Serialization(String),

    #[error("交易所錯誤: {0}")]
    Exchange(String),

    #[error("執行錯誤: {0}")]
    Execution(String),

    #[error("配置錯誤: {0}")]
    Config(String),

    #[error("風控錯誤: {0}")]
    Risk(String),

    #[error("認證錯誤: {0}")]
    Authentication(String),

    #[error("限流錯誤: {0}")]
    RateLimit(String),

    #[error("余額不足: {0}")]
    InsufficientBalance(String),

    #[error("訂單未找到: {0}")]
    OrderNotFound(String),

    #[error("無效訂單: {0}")]
    InvalidOrder(String),

    #[error("超時錯誤: {0}")]
    Timeout(String),

    #[error("I/O 錯誤: {message}")]
    Io { message: String },

    // 向後兼容的通用錯誤
    #[error("{message}")]
    Generic { message: String },
}

impl HftError {
    pub fn new(message: &str) -> Self {
        HftError::Generic {
            message: message.to_string(),
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

// 向後兼容：從 std::io::Error 轉換
impl From<std::io::Error> for HftError {
    fn from(err: std::io::Error) -> Self {
        HftError::Io {
            message: err.to_string(),
        }
    }
}

pub type HftResult<T> = Result<T, HftError>;
