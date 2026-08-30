//! 安全密鑰管理抽象層
//!
//! 提供以環境變數為唯一後端的秘密管理接口。
//!
//! # 例子
//!
//! ```ignore
//! let manager = SecretsManager::from_env();
//!
//! let api_key = manager.get_secret("bitget::api_key")?;
//! ```

mod backends;
mod error;
mod types;

pub use backends::SecretsManager;
pub use error::{Result, SecretsError};
pub use types::SecretValue;
