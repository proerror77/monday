use hft_core::HftError;
use thiserror::Error;

pub type AdapterResult<T> = Result<T, AdapterError>;

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("other: {0}")]
    Other(String),
}

impl From<serde_json::Error> for AdapterError {
    fn from(e: serde_json::Error) -> Self {
        AdapterError::Serde(e.to_string())
    }
}

#[cfg(feature = "json-simd")]
impl From<simd_json::Error> for AdapterError {
    fn from(e: simd_json::Error) -> Self {
        AdapterError::Serde(e.to_string())
    }
}

impl From<AdapterError> for HftError {
    fn from(e: AdapterError) -> Self {
        match e {
            AdapterError::Parse(msg) => HftError::Parse(msg),
            AdapterError::Serde(msg) => HftError::Serialization(msg),
            AdapterError::Io(msg) => HftError::Io { message: msg },
            AdapterError::Other(msg) => HftError::Generic { message: msg },
        }
    }
}
