//! 統一時間戳處理
//!
//! Legacy compatibility projection plus provenance-bearing timestamp types.

use crate::Timestamp;
use serde::{Deserialize, Serialize};

/// Binance `E`: exchange event time in Unix microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExchangeEventTimestamp(Timestamp);

impl ExchangeEventTimestamp {
    pub const fn new(micros: Timestamp) -> Self {
        Self(micros)
    }

    pub const fn as_micros(self) -> Timestamp {
        self.0
    }
}

/// Binance `T`: exchange match/trade time in Unix microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExchangeTradeTimestamp(Timestamp);

impl ExchangeTradeTimestamp {
    pub const fn new(micros: Timestamp) -> Self {
        Self(micros)
    }

    pub const fn as_micros(self) -> Timestamp {
        self.0
    }
}

/// Local userspace frame-delivery time projected onto the host Unix clock, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalReceiveTimestamp(Timestamp);

impl LocalReceiveTimestamp {
    pub const fn new(micros: Timestamp) -> Self {
        Self(micros)
    }

    pub const fn as_micros(self) -> Timestamp {
        self.0
    }

    /// Same-message-family `local-E`; clock-discipline evidence is still required externally.
    ///
    /// `ExchangeTradeTimestamp` cannot be passed accidentally:
    ///
    /// ```compile_fail
    /// use hft_core::{ExchangeTradeTimestamp, LocalReceiveTimestamp};
    /// let local = LocalReceiveTimestamp::new(2);
    /// let trade = ExchangeTradeTimestamp::new(1);
    /// let _ = local.latency_since_event(trade);
    /// ```
    pub const fn latency_since_event(self, event: ExchangeEventTimestamp) -> i128 {
        self.0 as i128 - event.0 as i128
    }
}

/// Timestamp provenance attached to a canonical market event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketDataTimestamps {
    pub exchange_event: Option<ExchangeEventTimestamp>,
    pub exchange_trade: Option<ExchangeTradeTimestamp>,
    pub local_receive: Option<LocalReceiveTimestamp>,
}

impl MarketDataTimestamps {
    pub const fn local_only(local_receive: LocalReceiveTimestamp) -> Self {
        Self {
            exchange_event: None,
            exchange_trade: None,
            local_receive: Some(local_receive),
        }
    }
}

/// 統一時間戳結構
///
/// 包含交易所時間戳和本地時間戳，提供統一的時間處理接口
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedTimestamp {
    /// 交易所時間戳 (微秒)
    pub exchange_ts: Timestamp,

    /// 本地接收時間戳 (微秒)
    pub local_ts: Timestamp,
}

impl UnifiedTimestamp {
    /// 創建新的統一時間戳
    pub fn new(exchange_ts: Timestamp, local_ts: Timestamp) -> Self {
        Self {
            exchange_ts,
            local_ts,
        }
    }

    /// 僅使用本地時間戳創建 (交易所時間戳為 0)
    pub fn local_only(local_ts: Timestamp) -> Self {
        Self {
            exchange_ts: 0,
            local_ts,
        }
    }

    /// 僅使用交易所時間戳創建 (本地時間戳為當前時間)
    pub fn exchange_only(exchange_ts: Timestamp) -> Self {
        Self {
            exchange_ts,
            local_ts: Self::current_timestamp(),
        }
    }

    /// 向後兼容別名：從交易所時間戳創建（等同於 exchange_only）
    pub fn from_exchange_ts(exchange_ts: Timestamp) -> Self {
        Self::exchange_only(exchange_ts)
    }

    /// 自動創建 (交易所時間戳 + 當前本地時間)
    pub fn auto(exchange_ts: Timestamp) -> Self {
        Self::new(exchange_ts, Self::current_timestamp())
    }

    /// 獲取主要時間戳 (優先交易所時間，否則使用本地時間)
    pub fn primary_ts(&self) -> Timestamp {
        if self.exchange_ts != 0 {
            self.exchange_ts
        } else {
            self.local_ts
        }
    }

    /// 檢查時間戳是否陳舊 (基於配置的閾值)
    pub fn is_stale(&self, stale_threshold_us: u64) -> bool {
        let age = Self::current_timestamp() - self.primary_ts();
        age > stale_threshold_us
    }

    /// 獲取當前時間戳 (微秒)
    pub fn current_timestamp() -> Timestamp {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros() as u64
    }

    /// 從毫秒時間戳創建統一時間戳
    pub fn from_millis(exchange_ts_ms: u64) -> Self {
        Self::auto(exchange_ts_ms * 1000) // 轉換為微秒
    }

    /// 從秒時間戳創建統一時間戳
    pub fn from_secs(exchange_ts_secs: u64) -> Self {
        Self::auto(exchange_ts_secs * 1_000_000) // 轉換為微秒
    }

    /// 驗證時間戳合理性
    pub fn validate(&self) -> bool {
        let now = Self::current_timestamp();
        let future_threshold = 60 * 1_000_000; // 60秒未來時間容錯
        let past_threshold = 24 * 3600 * 1_000_000; // 24小時過去時間容錯

        // 檢查本地時間戳
        if self.local_ts > now + future_threshold {
            return false; // 本地時間戳不能太遠的未來
        }

        // 檢查交易所時間戳 (如果提供)
        if self.exchange_ts != 0 {
            if self.exchange_ts > now + future_threshold {
                return false; // 交易所時間戳不能太遠的未來
            }

            if self.exchange_ts + past_threshold < now {
                return false; // 交易所時間戳不能太舊
            }
        }

        true
    }
}

impl Default for UnifiedTimestamp {
    fn default() -> Self {
        Self::local_only(Self::current_timestamp())
    }
}

impl From<Timestamp> for UnifiedTimestamp {
    fn from(timestamp: Timestamp) -> Self {
        Self::local_only(timestamp)
    }
}

impl From<UnifiedTimestamp> for Timestamp {
    fn from(unified: UnifiedTimestamp) -> Self {
        unified.primary_ts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unified_timestamp_creation() {
        let now = UnifiedTimestamp::current_timestamp();
        let exchange_ts = now - 1000; // 1ms 前

        let unified = UnifiedTimestamp::new(exchange_ts, now);
        assert_eq!(unified.exchange_ts, exchange_ts);
        assert_eq!(unified.local_ts, now);
        assert_eq!(unified.primary_ts(), exchange_ts); // 優先交易所時間
    }

    #[test]
    fn test_primary_timestamp_selection() {
        let now = UnifiedTimestamp::current_timestamp();

        // 有交易所時間戳時優先使用
        let unified1 = UnifiedTimestamp::new(now - 1000, now);
        assert_eq!(unified1.primary_ts(), now - 1000);

        // 無交易所時間戳時使用本地時間
        let unified2 = UnifiedTimestamp::new(0, now);
        assert_eq!(unified2.primary_ts(), now);
    }

    #[test]
    fn test_latency_calculation() {
        let exchange_ts = 1000000; // 1秒
        let local_ts = 1001000; // 1.001秒 (1ms延遲)

        assert_eq!(
            LocalReceiveTimestamp::new(local_ts)
                .latency_since_event(ExchangeEventTimestamp::new(exchange_ts)),
            1000
        );
        assert_eq!(
            MarketDataTimestamps::local_only(LocalReceiveTimestamp::new(local_ts)).exchange_event,
            None
        );
    }

    #[test]
    fn test_stale_detection() {
        let now = UnifiedTimestamp::current_timestamp();
        let old_ts = now - 10000; // 10ms 前

        let unified = UnifiedTimestamp::new(old_ts, now);

        // 5ms 閾值 - 應該是陳舊的
        assert!(unified.is_stale(5000));

        // 20ms 閾值 - 不應該是陳舊的
        assert!(!unified.is_stale(20000));
    }

    #[test]
    fn test_millis_conversion() {
        let millis_ts = 1609459200000; // 2021-01-01 00:00:00 UTC in ms
        let unified = UnifiedTimestamp::from_millis(millis_ts);

        assert_eq!(unified.exchange_ts, millis_ts * 1000); // 轉換為微秒
    }

    #[test]
    fn test_validation() {
        let now = UnifiedTimestamp::current_timestamp();

        // 正常時間戳應該驗證通過
        let normal = UnifiedTimestamp::new(now - 1000, now);
        assert!(normal.validate());

        // 未來時間過遠應該驗證失敗
        let far_future = UnifiedTimestamp::new(now + 120 * 1_000_000, now); // 2分鐘後
        assert!(!far_future.validate());

        // 過去時間太久應該驗證失敗
        let far_past = UnifiedTimestamp::new(now - 25 * 3600 * 1_000_000, now); // 25小時前
        assert!(!far_past.validate());
    }
}
