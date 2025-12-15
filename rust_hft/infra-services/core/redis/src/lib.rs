//! Redis 整合（可選，旁路，不進熱路徑）
//!
//! 功能:
//! - Pub/Sub 用於事件通道 (ml.deploy, ops.alert, kill-switch)
//! - Rate Limiting (INCR/EXPIRE 實現速率控制)
//! - KV 熱配置存取
//! - Streams 跨程序可靠匯流

#[cfg(feature = "redis")]
mod pubsub;

#[cfg(feature = "redis")]
mod rate_limit;

#[cfg(feature = "redis")]
pub use pubsub::*;

#[cfg(feature = "redis")]
pub use rate_limit::*;

// Re-export for convenience
#[cfg(feature = "redis")]
pub mod prelude {
    pub use super::pubsub::{
        EventChannel, PubSubError, RedisPubSub, RedisConfig, AlertEvent, DeployEvent, KillSwitchEvent,
    };
    pub use super::rate_limit::{RateLimiter, RateLimitResult};
}

/// 空實作，當 redis feature 未啟用時使用
#[cfg(not(feature = "redis"))]
pub mod prelude {
    /// 佔位類型
    pub struct RedisPubSub;

    impl RedisPubSub {
        pub fn new(_config: ()) -> Self {
            Self
        }
    }
}
