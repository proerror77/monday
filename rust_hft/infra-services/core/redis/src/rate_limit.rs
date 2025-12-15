//! Redis 速率限制
//!
//! 使用 INCR/EXPIRE 實現滑動窗口速率限制

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::pubsub::PubSubError;

/// 速率限制結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitResult {
    /// 允許通過
    Allowed {
        /// 當前窗口內的請求數
        current: u64,
        /// 窗口大小限制
        limit: u64,
    },
    /// 超過限制
    Exceeded {
        /// 當前窗口內的請求數
        current: u64,
        /// 窗口大小限制
        limit: u64,
        /// 重試建議時間（毫秒）
        retry_after_ms: u64,
    },
}

impl RateLimitResult {
    /// 是否允許
    pub fn is_allowed(&self) -> bool {
        matches!(self, RateLimitResult::Allowed { .. })
    }

    /// 當前計數
    pub fn current(&self) -> u64 {
        match self {
            RateLimitResult::Allowed { current, .. } => *current,
            RateLimitResult::Exceeded { current, .. } => *current,
        }
    }
}

/// 速率限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    /// 鍵前綴
    pub key_prefix: String,
    /// 窗口大小（秒）
    pub window_secs: u64,
    /// 窗口內最大請求數
    pub max_requests: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            key_prefix: "ratelimit".to_string(),
            window_secs: 1,
            max_requests: 100,
        }
    }
}

/// Redis 速率限制器
pub struct RateLimiter {
    client: redis::Client,
    config: RateLimitConfig,
}

impl RateLimiter {
    /// 建立新的速率限制器
    pub fn new(url: &str, config: RateLimitConfig) -> Result<Self, PubSubError> {
        let client = redis::Client::open(url)?;
        Ok(Self { client, config })
    }

    /// 檢查並增加計數
    pub async fn check_and_increment(&self, key: &str) -> Result<RateLimitResult, PubSubError> {
        let full_key = format!("{}:{}", self.config.key_prefix, key);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        // 使用 INCR 增加計數
        let current: u64 = conn
            .incr(&full_key, 1)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        // 如果是第一次，設置過期時間
        if current == 1 {
            let _: () = conn
                .expire(&full_key, self.config.window_secs as i64)
                .await
                .map_err(|e| PubSubError::Connection(e.to_string()))?;
        }

        debug!(
            "速率限制檢查 {}: {}/{}",
            key, current, self.config.max_requests
        );

        if current <= self.config.max_requests {
            Ok(RateLimitResult::Allowed {
                current,
                limit: self.config.max_requests,
            })
        } else {
            // 獲取 TTL 來計算重試時間
            let ttl: i64 = conn
                .ttl(&full_key)
                .await
                .map_err(|e| PubSubError::Connection(e.to_string()))?;

            let retry_after_ms = if ttl > 0 { ttl as u64 * 1000 } else { 1000 };

            Ok(RateLimitResult::Exceeded {
                current,
                limit: self.config.max_requests,
                retry_after_ms,
            })
        }
    }

    /// 僅檢查（不增加計數）
    pub async fn check(&self, key: &str) -> Result<RateLimitResult, PubSubError> {
        let full_key = format!("{}:{}", self.config.key_prefix, key);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let current: u64 = conn
            .get::<_, Option<u64>>(&full_key)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?
            .unwrap_or(0);

        if current < self.config.max_requests {
            Ok(RateLimitResult::Allowed {
                current,
                limit: self.config.max_requests,
            })
        } else {
            let ttl: i64 = conn
                .ttl(&full_key)
                .await
                .map_err(|e| PubSubError::Connection(e.to_string()))?;

            let retry_after_ms = if ttl > 0 { ttl as u64 * 1000 } else { 1000 };

            Ok(RateLimitResult::Exceeded {
                current,
                limit: self.config.max_requests,
                retry_after_ms,
            })
        }
    }

    /// 重置計數
    pub async fn reset(&self, key: &str) -> Result<(), PubSubError> {
        let full_key = format!("{}:{}", self.config.key_prefix, key);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let _: () = conn
            .del(&full_key)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        Ok(())
    }

    /// 獲取當前配置
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }
}

/// 冷卻管理器 (用於策略冷卻時間)
pub struct CooldownManager {
    client: redis::Client,
    key_prefix: String,
}

impl CooldownManager {
    /// 建立新的冷卻管理器
    pub fn new(url: &str, key_prefix: &str) -> Result<Self, PubSubError> {
        let client = redis::Client::open(url)?;
        Ok(Self {
            client,
            key_prefix: key_prefix.to_string(),
        })
    }

    /// 設置冷卻
    pub async fn set_cooldown(&self, key: &str, duration_ms: u64) -> Result<(), PubSubError> {
        let full_key = format!("{}:{}", self.key_prefix, key);
        let duration_secs = (duration_ms / 1000).max(1);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let _: () = conn
            .set_ex(&full_key, "1", duration_secs)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        debug!("設置冷卻 {}: {}ms", key, duration_ms);
        Ok(())
    }

    /// 檢查是否在冷卻中
    pub async fn is_cooling(&self, key: &str) -> Result<bool, PubSubError> {
        let full_key = format!("{}:{}", self.key_prefix, key);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let exists: bool = conn
            .exists(&full_key)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        Ok(exists)
    }

    /// 獲取剩餘冷卻時間（毫秒）
    pub async fn remaining_cooldown_ms(&self, key: &str) -> Result<u64, PubSubError> {
        let full_key = format!("{}:{}", self.key_prefix, key);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let ttl: i64 = conn
            .ttl(&full_key)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        if ttl > 0 {
            Ok(ttl as u64 * 1000)
        } else {
            Ok(0)
        }
    }

    /// 清除冷卻
    pub async fn clear_cooldown(&self, key: &str) -> Result<(), PubSubError> {
        let full_key = format!("{}:{}", self.key_prefix, key);

        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await?;

        let _: () = conn
            .del(&full_key)
            .await
            .map_err(|e| PubSubError::Connection(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limit_result_allowed() {
        let result = RateLimitResult::Allowed {
            current: 5,
            limit: 100,
        };
        assert!(result.is_allowed());
        assert_eq!(result.current(), 5);
    }

    #[test]
    fn test_rate_limit_result_exceeded() {
        let result = RateLimitResult::Exceeded {
            current: 101,
            limit: 100,
            retry_after_ms: 500,
        };
        assert!(!result.is_allowed());
        assert_eq!(result.current(), 101);
    }

    #[test]
    fn test_config_default() {
        let config = RateLimitConfig::default();
        assert_eq!(config.window_secs, 1);
        assert_eq!(config.max_requests, 100);
        assert_eq!(config.key_prefix, "ratelimit");
    }
}
