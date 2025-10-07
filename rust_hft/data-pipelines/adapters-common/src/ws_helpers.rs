//! Lightweight placeholders for WebSocket helpers.
//! Concrete adapters can ignore these or wrap integration::ws utilities.

#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    pub url: String,
    pub heartbeat_interval_ms: u64,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self { url: String::new(), heartbeat_interval_ms: 25_000 }
    }
}

