//! WebSocket 幀處理延遲量測工具
//!
//! 使用單調時鐘（CLOCK_MONOTONIC）量測 `received` → `parsed` 的耗時，
//! 提供後續延遲追蹤與監控。

use hft_core::monotonic_micros;

/// 儲存單一 WS 幀的接收與解析時間戳（微秒）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsFrameMetrics {
    /// epoll 喚醒 / 接收到幀的時間（microseconds，自系統啟動以來遞增）
    pub received_at_us: u64,
    /// JSON 解析完成時間（microseconds，自系統啟動以來遞增）
    pub parsed_at_us: u64,
}

impl WsFrameMetrics {
    /// 建立新的量測紀錄
    ///
    /// `received_at_us` 與 `parsed_at_us` 應使用 `monotonic_micros()` 取得，
    /// 以避免系統時間調整造成回跳。
    pub const fn new(received_at_us: u64, parsed_at_us: u64) -> Self {
        Self {
            received_at_us,
            parsed_at_us,
        }
    }

    /// 以目前單調時間建立 `received_at_us`
    pub fn record_receive() -> Self {
        Self {
            received_at_us: monotonic_micros(),
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
        let mut metrics = WsFrameMetrics::record_receive();
        thread::sleep(Duration::from_micros(10));
        metrics.mark_parsed();
        assert!(metrics.validate());
        assert!(metrics.parsed_at_us >= metrics.received_at_us);
    }

    #[test]
    fn manual_metrics_validate() {
        let metrics = WsFrameMetrics::new(100, 120);
        assert!(metrics.validate());

        let invalid = WsFrameMetrics::new(120, 100);
        assert!(!invalid.validate());
    }
}
