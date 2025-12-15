//! Error types for model manager

use thiserror::Error;

/// Model manager error types
#[derive(Error, Debug)]
pub enum ModelError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("File watch error: {0}")]
    Watch(#[from] notify::Error),

    #[error("Model not found: {0}")]
    NotFound(String),

    #[error("Invalid model format: {0}")]
    InvalidFormat(String),

    #[error("Model validation failed: {0}")]
    ValidationFailed(String),

    #[error("Rollback failed: {0}")]
    RollbackFailed(String),
}

/// Result type for model operations
pub type ModelResult<T> = Result<T, ModelError>;
