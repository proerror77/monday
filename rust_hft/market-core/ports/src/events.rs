//! 統一事件模型 - 穩定契約

use hft_core::latency::LatencyTracker;
use hft_core::*;
use serde::{Deserialize, Serialize};

/// 訂單簿檔位
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: Price,
    pub quantity: Quantity,
}

impl BookLevel {
    pub fn new(price: f64, quantity: f64) -> Result<Self, rust_decimal::Error> {
        Ok(Self {
            price: Price::from_f64(price)?,
            quantity: Quantity::from_f64(quantity)?,
        })
    }

    /// 便利方法 - 實際交易中應該避免 unwrap
    pub fn new_unchecked(price: f64, quantity: f64) -> Self {
        Self {
            price: Price::from_f64(price).expect("Invalid price"),
            quantity: Quantity::from_f64(quantity).expect("Invalid quantity"),
        }
    }
}

/// 市場快照事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub bids: Vec<BookLevel>, // 按價格降序
    pub asks: Vec<BookLevel>, // 按價格升序
    pub sequence: u64,        // 序號，檢測缺口用
    /// 來源交易所（Phase 1 重構：顯式 venue 語義）
    #[serde(default)]
    pub source_venue: Option<VenueId>,
}

/// 訂單簿增量更新
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookUpdate {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub bids: Vec<BookLevel>, // 變更的檔位
    pub asks: Vec<BookLevel>,
    pub sequence: u64,
    pub is_snapshot: bool, // true=快照，false=增量
    /// 來源交易所（Phase 1 重構：顯式 venue 語義）
    #[serde(default)]
    pub source_venue: Option<VenueId>,
}

/// 交易事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub price: Price,
    pub quantity: Quantity,
    pub side: Side, // 市場角度：買/賣
    pub trade_id: String,
    /// 來源交易所（Phase 1 重構：顯式 venue 語義）
    #[serde(default)]
    pub source_venue: Option<VenueId>,
}

/// 聚合K線
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedBar {
    pub symbol: Symbol,
    pub interval_ms: u64,
    pub open_time: Timestamp,
    pub close_time: Timestamp,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: Quantity,
    pub trade_count: u32,
    /// 來源交易所（Phase 1 重構：顯式 venue 語義）
    #[serde(default)]
    pub source_venue: Option<VenueId>,
}

/// 統一市場事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketEvent {
    Snapshot(MarketSnapshot),
    Update(BookUpdate),
    Trade(Trade),
    Bar(AggregatedBar),
    Arbitrage(ArbitrageOpportunity),
    Disconnect { reason: String },
}

/// 帶延遲追蹤的市場事件 - 用於端到端延遲測量
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedMarketEvent {
    /// 市場事件本身
    pub event: MarketEvent,
    /// 延遲追蹤器
    pub tracker: LatencyTracker,
}

impl TrackedMarketEvent {
    /// 創建帶追蹤的市場事件
    pub fn new(event: MarketEvent) -> Self {
        Self {
            event,
            tracker: LatencyTracker::new(),
        }
    }

    /// 從指定時間創建帶追蹤的市場事件
    pub fn from_time(event: MarketEvent, origin_time: u64) -> Self {
        Self {
            event,
            tracker: LatencyTracker::from_time(origin_time),
        }
    }

    /// 記錄延遲階段
    pub fn record_stage(&mut self, stage: hft_core::latency::LatencyStage) {
        self.tracker.record_stage(stage);
    }

    /// 獲取事件的 symbol（輔助方法）
    pub fn symbol(&self) -> Option<&Symbol> {
        match &self.event {
            MarketEvent::Snapshot(s) => Some(&s.symbol),
            MarketEvent::Update(u) => Some(&u.symbol),
            MarketEvent::Trade(t) => Some(&t.symbol),
            MarketEvent::Bar(b) => Some(&b.symbol),
            MarketEvent::Arbitrage(arb) => Some(&arb.symbol),
            MarketEvent::Disconnect { .. } => None,
        }
    }
}

/// 未結訂單狀態 (用於對賬)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenOrder {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub original_quantity: Quantity,
    pub remaining_quantity: Quantity,
    pub filled_quantity: Quantity,
    pub price: Option<Price>,
    pub status: OrderStatus,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 訂單狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OrderStatus {
    /// 新訂單，等待確認
    New,
    /// 已確認，等待成交（OMS 層使用）
    Acknowledged,
    /// 已確認，等待成交（交易所回報）
    Accepted,
    /// 部分成交
    PartiallyFilled,
    /// 完全成交
    Filled,
    /// 已撤銷
    Canceled,
    /// 已拒絕
    Rejected,
    /// 過期
    Expired,
    /// 已替換
    Replaced,
}

/// 訂單意圖 (下單前)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: Quantity,
    pub order_type: OrderType,
    pub price: Option<Price>, // Limit 訂單必填
    pub time_in_force: TimeInForce,
    pub strategy_id: String,
    /// 目標交易所（Phase 1 重構：可插拔路由）
    /// None = 由 Router 決策，Some = 指定交易所
    #[serde(default)]
    pub target_venue: Option<VenueId>,
}

/// OrderIntent 的生命週期元資料。
///
/// 策略仍可輸出穩定的 `OrderIntent`；live/paper/shadow 路徑在進入風控或
/// 執行隊列前用這層 envelope 判斷過期、來源簿是否已被更新、以及本地等待
/// 是否超過策略允許的最大延遲。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderIntentLifecycle {
    pub created_ts: Timestamp,
    pub valid_until: Timestamp,
    pub source_book_seq: Option<u64>,
    pub source_feature_ts: Option<Timestamp>,
    pub max_latency_us: Option<u64>,
    pub max_slippage_bps: Option<i32>,
    #[serde(default)]
    pub reduce_only: bool,
}

impl Default for OrderIntentLifecycle {
    fn default() -> Self {
        Self {
            created_ts: 0,
            valid_until: Timestamp::MAX,
            source_book_seq: None,
            source_feature_ts: None,
            max_latency_us: None,
            max_slippage_bps: None,
            reduce_only: false,
        }
    }
}

impl OrderIntentLifecycle {
    pub fn new(created_ts: Timestamp, valid_until: Timestamp) -> Self {
        Self {
            created_ts,
            valid_until,
            ..Self::default()
        }
    }

    pub fn is_expired_at(&self, now: Timestamp) -> bool {
        now >= self.valid_until
    }

    pub fn is_stale_for_book_seq(&self, latest_book_seq: u64) -> bool {
        self.source_book_seq
            .is_some_and(|source_seq| latest_book_seq > source_seq)
    }

    pub fn latency_us_at(&self, now: Timestamp) -> u64 {
        now.saturating_sub(self.created_ts)
    }

    pub fn validate_pre_risk(
        &self,
        now: Timestamp,
        latest_book_seq: Option<u64>,
    ) -> Result<(), OrderIntentRejectReason> {
        self.validate(now, latest_book_seq)
    }

    pub fn validate_pre_execution(
        &self,
        now: Timestamp,
        latest_book_seq: Option<u64>,
    ) -> Result<(), OrderIntentRejectReason> {
        self.validate(now, latest_book_seq)
    }

    fn validate(
        &self,
        now: Timestamp,
        latest_book_seq: Option<u64>,
    ) -> Result<(), OrderIntentRejectReason> {
        if self.is_expired_at(now) {
            return Err(OrderIntentRejectReason::Expired {
                now,
                valid_until: self.valid_until,
            });
        }

        if let (Some(source_book_seq), Some(latest_book_seq)) =
            (self.source_book_seq, latest_book_seq)
        {
            if latest_book_seq > source_book_seq {
                return Err(OrderIntentRejectReason::SourceBookStale {
                    source_book_seq,
                    latest_book_seq,
                });
            }
        }

        if let Some(max_latency_us) = self.max_latency_us {
            let elapsed_us = self.latency_us_at(now);
            if elapsed_us > max_latency_us {
                return Err(OrderIntentRejectReason::MaxLatencyExceeded {
                    now,
                    created_ts: self.created_ts,
                    elapsed_us,
                    max_latency_us,
                });
            }
        }

        Ok(())
    }
}

/// 帶生命週期的下單意圖。這是策略輸出和風控/執行邊界之間的兼容 envelope，
/// 不改動既有 Strategy trait。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntentEnvelope {
    pub intent: OrderIntent,
    pub lifecycle: OrderIntentLifecycle,
}

impl OrderIntentEnvelope {
    pub fn new(intent: OrderIntent, lifecycle: OrderIntentLifecycle) -> Self {
        Self { intent, lifecycle }
    }

    pub fn into_inner(self) -> OrderIntent {
        self.intent
    }

    pub fn validate_pre_risk(
        &self,
        now: Timestamp,
        latest_book_seq: Option<u64>,
    ) -> Result<(), OrderIntentRejectReason> {
        self.lifecycle.validate_pre_risk(now, latest_book_seq)
    }

    pub fn validate_pre_execution(
        &self,
        now: Timestamp,
        latest_book_seq: Option<u64>,
    ) -> Result<(), OrderIntentRejectReason> {
        self.lifecycle.validate_pre_execution(now, latest_book_seq)
    }
}

/// OrderIntent envelope 被風控或執行前置 gate 拒絕的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderIntentRejectReason {
    Expired {
        now: Timestamp,
        valid_until: Timestamp,
    },
    SourceBookStale {
        source_book_seq: u64,
        latest_book_seq: u64,
    },
    MaxLatencyExceeded {
        now: Timestamp,
        created_ts: Timestamp,
        elapsed_us: u64,
        max_latency_us: u64,
    },
}

/// 執行事件 (下單後回報)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionEvent {
    /// 新下單事件（用於在引擎內註冊訂單元資料）
    /// 來源：ExecutionWorker 在下單成功後立即派發（Paper/Live 通用）
    OrderNew {
        order_id: OrderId,
        symbol: Symbol,
        side: Side,
        quantity: Quantity,
        /// 原始意圖中的價格（Market 單通常為 None，會在引擎層補全）
        requested_price: Option<Price>,
        timestamp: Timestamp,
        /// 目標交易場（若可推斷，由執行路由器或 worker 填充）
        #[serde(default)]
        venue: Option<VenueId>,
        /// 策略實例 ID（便於對帳/追蹤）
        #[serde(default)]
        strategy_id: String,
    },
    /// 訂單確認
    OrderAck {
        order_id: OrderId,
        timestamp: Timestamp,
    },
    /// 成交回報
    Fill {
        order_id: OrderId,
        price: Price,
        quantity: Quantity,
        timestamp: Timestamp,
        fill_id: String,
    },
    /// 訂單拒絕
    OrderReject {
        order_id: OrderId,
        reason: String,
        timestamp: Timestamp,
    },
    /// 訂單完成 (完全成交)
    OrderCompleted {
        order_id: OrderId,
        final_price: Price,
        total_filled: Quantity,
        timestamp: Timestamp,
    },
    /// 訂單撤銷確認
    OrderCanceled {
        order_id: OrderId,
        timestamp: Timestamp,
    },
    /// 訂單修改確認
    OrderModified {
        order_id: OrderId,
        new_quantity: Option<Quantity>,
        new_price: Option<Price>,
        timestamp: Timestamp,
    },
    /// 餘額更新 (私有流)
    BalanceUpdate {
        asset: String,
        balance: Quantity,
        timestamp: Timestamp,
    },
    /// 連線狀態
    ConnectionStatus {
        connected: bool,
        timestamp: Timestamp,
    },
}

/// 套利機會事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageOpportunity {
    /// 基礎交易品種（不包含交易所前綴）
    pub symbol: Symbol,
    /// 提供最佳買價的交易所
    pub bid_venue: VenueId,
    /// 提供最佳賣價的交易所
    pub ask_venue: VenueId,
    pub spread_bps: Bps,
    pub max_quantity: Quantity,
    pub timestamp: Timestamp,
}

/// 風控告警事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAlert {
    pub alert_type: RiskAlertType,
    pub description: String,
    pub severity: AlertSeverity,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RiskAlertType {
    PositionLimit,
    Drawdown,
    Latency,
    OrderRate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lifecycle(created_ts: Timestamp, valid_until: Timestamp) -> OrderIntentLifecycle {
        OrderIntentLifecycle::new(created_ts, valid_until)
    }

    #[test]
    fn lifecycle_rejects_expired_intent() {
        let lifecycle = lifecycle(1_000, 1_100);

        assert_eq!(
            lifecycle.validate_pre_risk(1_100, None),
            Err(OrderIntentRejectReason::Expired {
                now: 1_100,
                valid_until: 1_100,
            })
        );
    }

    #[test]
    fn lifecycle_rejects_stale_book_source() {
        let mut lifecycle = lifecycle(1_000, 2_000);
        lifecycle.source_book_seq = Some(42);

        assert_eq!(
            lifecycle.validate_pre_execution(1_100, Some(43)),
            Err(OrderIntentRejectReason::SourceBookStale {
                source_book_seq: 42,
                latest_book_seq: 43,
            })
        );
    }

    #[test]
    fn lifecycle_rejects_max_latency_exceeded() {
        let mut lifecycle = lifecycle(1_000, 2_000);
        lifecycle.max_latency_us = Some(25);

        assert_eq!(
            lifecycle.validate_pre_execution(1_026, None),
            Err(OrderIntentRejectReason::MaxLatencyExceeded {
                now: 1_026,
                created_ts: 1_000,
                elapsed_us: 26,
                max_latency_us: 25,
            })
        );
    }

    #[test]
    fn lifecycle_accepts_current_intent() {
        let mut lifecycle = lifecycle(1_000, 1_100);
        lifecycle.source_book_seq = Some(42);
        lifecycle.max_latency_us = Some(25);

        assert_eq!(lifecycle.validate_pre_risk(1_025, Some(42)), Ok(()));
    }
}
