//! WebSocket message processing latency measurement.
//!
//! Uses a monotonic clock from WS-library-delivered complete-message delivery to parse completion.

use hft_core::{monotonic_micros, now_micros};

/// Receive and parse timestamps for one complete WebSocket message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsMessageMetrics {
    /// WS-library complete-message delivery time in userspace; not kernel or NIC RX.
    pub received_at_us: u64,
    /// Paired Unix sample at the same message boundary, for venue-to-local estimates only.
    pub received_at_unix_us: Option<u64>,
    /// JSON 解析完成時間（microseconds，自系統啟動以來遞增）
    pub parsed_at_us: u64,
}

impl WsMessageMetrics {
    /// 建立新的量測紀錄
    ///
    /// `received_at_us` 與 `parsed_at_us` 應使用 `monotonic_micros()` 取得，
    /// 以避免系統時間調整造成回跳。
    pub const fn new(received_at_us: u64, parsed_at_us: u64) -> Self {
        Self {
            received_at_us,
            received_at_unix_us: None,
            parsed_at_us,
        }
    }

    pub const fn new_with_unix(
        received_at_us: u64,
        received_at_unix_us: u64,
        parsed_at_us: u64,
    ) -> Self {
        Self {
            received_at_us,
            received_at_unix_us: Some(received_at_unix_us),
            parsed_at_us,
        }
    }

    /// Record delivery immediately after the WS library yields a complete message.
    pub fn record_receive() -> Self {
        Self {
            received_at_us: monotonic_micros(),
            received_at_unix_us: Some(now_micros()),
            parsed_at_us: 0,
        }
    }

    /// 標記解析完成時間（若已設定會覆寫）
    pub fn mark_parsed(&mut self) {
        self.parsed_at_us = monotonic_micros();
    }

    /// 檢查時間戳是否有效（解析時間須晚於接收時間）
    pub fn validate(&self) -> bool {
        self.parsed_at_us >= self.received_at_us
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn monotonic_time_monotonic() {
        let t1 = monotonic_micros();
        thread::sleep(Duration::from_micros(50));
        let t2 = monotonic_micros();
        assert!(t2 >= t1);
    }

    #[test]
    fn validate_metrics() {
        let before_unix_us = now_micros();
        let mut metrics = WsMessageMetrics::record_receive();
        let after_unix_us = now_micros();
        thread::sleep(Duration::from_micros(10));
        metrics.mark_parsed();
        assert!(metrics.validate());
        assert!(metrics.parsed_at_us >= metrics.received_at_us);
        assert!(matches!(
            metrics.received_at_unix_us,
            Some(timestamp) if timestamp >= before_unix_us && timestamp <= after_unix_us
        ));
    }

    #[test]
    fn manual_metrics_validate() {
        let metrics = WsMessageMetrics::new(100, 120);
        assert!(metrics.validate());

        let invalid = WsMessageMetrics::new(120, 100);
        assert!(!invalid.validate());
    }
}
