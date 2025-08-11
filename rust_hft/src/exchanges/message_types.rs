//! 交易所消息類型定義

use serde::{Serialize, Deserialize};
use crate::core::types::*;

/// 訂單簿層級
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookLevel {
    /// 價格
    pub price: f64,
    
    /// 數量
    pub quantity: f64,
    
    /// 訂單數量
    pub order_count: u32,
}

/// 市場事件類型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketEvent {
    /// 訂單簿更新
    OrderBookUpdate {
        symbol: String,
        exchange: String,
        bids: Vec<OrderBookLevel>,
        asks: Vec<OrderBookLevel>,
        timestamp: u64,
        sequence: u64,
        is_snapshot: bool,
    },
    
    /// 成交數據
    Trade {
        symbol: String,
        exchange: String,
        trade_id: String,
        price: f64,
        quantity: f64,
        side: OrderSide,
        timestamp: u64,
        buyer_maker: bool,
    },
    
    /// Ticker更新
    Ticker {
        symbol: String,
        exchange: String,
        last_price: f64,
        bid_price: f64,
        ask_price: f64,
        bid_size: f64,
        ask_size: f64,
        volume_24h: f64,
        change_24h: f64,
        timestamp: u64,
    },
    
    /// 心跳
    Heartbeat {
        exchange: String,
        timestamp: u64,
    },
    
    /// 連接狀態變化
    ConnectionStatus {
        exchange: String,
        status: String,
        timestamp: u64,
    },
    
    /// 錯誤事件
    Error {
        exchange: String,
        error: String,
        timestamp: u64,
    },
}

/// 執行回報
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReport {
    /// 訂單ID
    pub order_id: String,
    
    /// 客戶端訂單ID
    pub client_order_id: Option<String>,
    
    /// 交易所名稱
    pub exchange: String,
    
    /// 交易對
    pub symbol: String,
    
    /// 訂單側別
    pub side: OrderSide,
    
    /// 訂單類型
    pub order_type: OrderType,
    
    /// 訂單狀態
    pub status: OrderStatus,
    
    /// 原始數量
    pub original_quantity: f64,
    
    /// 已成交數量
    pub executed_quantity: f64,
    
    /// 剩餘數量
    pub remaining_quantity: f64,
    
    /// 訂單價格
    pub price: f64,
    
    /// 平均成交價格
    pub avg_price: f64,
    
    /// 最後成交價格
    pub last_executed_price: f64,
    
    /// 最後成交數量
    pub last_executed_quantity: f64,
    
    /// 累計手續費
    pub commission: f64,
    
    /// 手續費資產
    pub commission_asset: String,
    
    /// 創建時間
    pub create_time: u64,
    
    /// 更新時間
    pub update_time: u64,
    
    /// 成交時間
    pub transaction_time: u64,
    
    /// 拒絕原因
    pub reject_reason: Option<String>,
}

/// 下單請求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// 客戶端訂單ID
    pub client_order_id: String,
    
    /// 交易對
    pub symbol: String,
    
    /// 訂單側別
    pub side: OrderSide,
    
    /// 訂單類型
    pub order_type: OrderType,
    
    /// 數量
    pub quantity: f64,
    
    /// 價格（限價單必填）
    pub price: Option<f64>,
    
    /// 止損價格
    pub stop_price: Option<f64>,
    
    /// 時效類型
    pub time_in_force: TimeInForce,
    
    /// 是否只做Maker
    pub post_only: bool,
    
    /// 是否減倉
    pub reduce_only: bool,
    
    /// 附加參數
    pub metadata: std::collections::HashMap<String, String>,
}

/// 取消訂單請求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelRequest {
    /// 訂單ID
    pub order_id: Option<String>,
    
    /// 客戶端訂單ID
    pub client_order_id: Option<String>,
    
    /// 交易對
    pub symbol: String,
}

/// 修改訂單請求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmendRequest {
    /// 訂單ID
    pub order_id: String,
    
    /// 新數量
    pub quantity: Option<f64>,
    
    /// 新價格
    pub price: Option<f64>,
    
    /// 新止損價格
    pub stop_price: Option<f64>,
}

/// 訂單響應
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderResponse {
    /// 是否成功
    pub success: bool,
    
    /// 訂單ID
    pub order_id: String,
    
    /// 客戶端訂單ID
    pub client_order_id: String,
    
    /// 訂單狀態
    pub status: OrderStatus,
    
    /// 錯誤消息
    pub error: Option<String>,
    
    /// 響應時間戳
    pub timestamp: u64,
}

impl MarketEvent {
    /// 獲取事件的交易對
    pub fn symbol(&self) -> &str {
        match self {
            MarketEvent::OrderBookUpdate { symbol, .. } => symbol,
            MarketEvent::Trade { symbol, .. } => symbol,
            MarketEvent::Ticker { symbol, .. } => symbol,
            _ => "",
        }
    }
    
    /// 獲取事件的交易所
    pub fn exchange(&self) -> &str {
        match self {
            MarketEvent::OrderBookUpdate { exchange, .. } => exchange,
            MarketEvent::Trade { exchange, .. } => exchange,
            MarketEvent::Ticker { exchange, .. } => exchange,
            MarketEvent::Heartbeat { exchange, .. } => exchange,
            MarketEvent::ConnectionStatus { exchange, .. } => exchange,
            MarketEvent::Error { exchange, .. } => exchange,
        }
    }
    
    /// 獲取事件時間戳
    pub fn timestamp(&self) -> u64 {
        match self {
            MarketEvent::OrderBookUpdate { timestamp, .. } => *timestamp,
            MarketEvent::Trade { timestamp, .. } => *timestamp,
            MarketEvent::Ticker { timestamp, .. } => *timestamp,
            MarketEvent::Heartbeat { timestamp, .. } => *timestamp,
            MarketEvent::ConnectionStatus { timestamp, .. } => *timestamp,
            MarketEvent::Error { timestamp, .. } => *timestamp,
        }
    }
}

impl ExecutionReport {
    /// 計算訂單完成度
    pub fn fill_percentage(&self) -> f64 {
        if self.original_quantity <= 0.0 {
            return 0.0;
        }
        (self.executed_quantity / self.original_quantity) * 100.0
    }
    
    /// 檢查訂單是否完全成交
    pub fn is_fully_filled(&self) -> bool {
        self.status == OrderStatus::Filled
    }
    
    /// 檢查訂單是否部分成交
    pub fn is_partially_filled(&self) -> bool {
        self.status == OrderStatus::PartiallyFilled && self.executed_quantity > 0.0
    }
    
    /// 檢查訂單是否活躍
    pub fn is_active(&self) -> bool {
        matches!(self.status, OrderStatus::New | OrderStatus::PartiallyFilled | OrderStatus::Pending)
    }
}

impl OrderRequest {
    /// 創建市價買單
    pub fn market_buy(symbol: String, quantity: f64) -> Self {
        Self {
            client_order_id: uuid::Uuid::new_v4().to_string(),
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity,
            price: None,
            stop_price: None,
            time_in_force: TimeInForce::IOC,
            post_only: false,
            reduce_only: false,
            metadata: std::collections::HashMap::new(),
        }
    }
    
    /// 創建市價賣單
    pub fn market_sell(symbol: String, quantity: f64) -> Self {
        Self {
            client_order_id: uuid::Uuid::new_v4().to_string(),
            symbol,
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity,
            price: None,
            stop_price: None,
            time_in_force: TimeInForce::IOC,
            post_only: false,
            reduce_only: false,
            metadata: std::collections::HashMap::new(),
        }
    }
    
    /// 創建限價買單
    pub fn limit_buy(symbol: String, quantity: f64, price: f64) -> Self {
        Self {
            client_order_id: uuid::Uuid::new_v4().to_string(),
            symbol,
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity,
            price: Some(price),
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: std::collections::HashMap::new(),
        }
    }
    
    /// 創建限價賣單
    pub fn limit_sell(symbol: String, quantity: f64, price: f64) -> Self {
        Self {
            client_order_id: uuid::Uuid::new_v4().to_string(),
            symbol,
            side: OrderSide::Sell,
            order_type: OrderType::Limit,
            quantity,
            price: Some(price),
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: std::collections::HashMap::new(),
        }
    }
    
    /// 計算名義價值
    pub fn notional_value(&self) -> f64 {
        match self.price {
            Some(price) => self.quantity * price,
            None => 0.0, // 市價單無法計算
        }
    }
}