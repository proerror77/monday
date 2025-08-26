//! 限速輔助（骨架）
#[derive(Clone, Debug)]
pub struct RateLimiterConfig { pub rps: u32 }

#[derive(Debug)]
pub struct RateLimiter { pub cfg: RateLimiterConfig }

impl RateLimiter { pub fn new(cfg: RateLimiterConfig) -> Self { Self { cfg } } }

