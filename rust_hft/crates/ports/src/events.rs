//! 統一事件模型 - 穩定契約

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
    pub bids: Vec<BookLevel>,     // 按價格降序
    pub asks: Vec<BookLevel>,     // 按價格升序
    pub sequence: u64,            // 序號，檢測缺口用
}

/// 訂單簿增量更新
#[derive(Debug, Clone, Serialize, Deserialize)]  
pub struct BookUpdate {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub bids: Vec<BookLevel>,     // 變更的檔位
    pub asks: Vec<BookLevel>,
    pub sequence: u64,
    pub is_snapshot: bool,        // true=快照，false=增量
}

/// 交易事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub symbol: Symbol,
    pub timestamp: Timestamp,
    pub price: Price,
    pub quantity: Quantity,
    pub side: Side,               // 市場角度：買/賣
    pub trade_id: String,
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

/// 訂單意圖 (下單前)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: Quantity,
    pub order_type: OrderType,
    pub price: Option<Price>,     // Limit 訂單必填
    pub time_in_force: TimeInForce,
    pub strategy_id: String,
}

/// 執行事件 (下單後回報)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionEvent {
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
    /// 訂單拒絕 (兼容性別名)
    OrderRejected {
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
    pub leg1: Symbol,
    pub leg2: Symbol,
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
