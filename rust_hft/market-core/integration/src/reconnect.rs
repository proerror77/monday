//! 重連/心跳（骨架）
#[derive(Clone, Debug)]
pub struct ReconnectPolicy {
    pub max_retries: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self { max_retries: 5 }
    }
}
