//! 數據流管線和零拷貝消息傳遞
//! 
//! 提供高性能的數據流處理能力，支持零拷貝消息傳遞和批次處理

pub mod ring_buffer;

use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use rust_decimal::Decimal;

/// 市場數據載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketDataPayload {
    OrderBookUpdate {
        symbol: String,
        bids: Vec<(Decimal, Decimal)>,
        asks: Vec<(Decimal, Decimal)>,
        timestamp: SystemTime,
    },
    Trade {
        symbol: String,
        price: Decimal,
        quantity: Decimal,
        side: String,
        timestamp: SystemTime,
    },
    Ticker {
        symbol: String,
        best_bid: Decimal,
        best_ask: Decimal,
        last_price: Decimal,
        timestamp: SystemTime,
    },
}

/// 執行載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionPayload {
    OrderRequest {
        order_id: String,
        symbol: String,
        side: String,
        quantity: Decimal,
        price: Option<Decimal>,
        order_type: String,
    },
    OrderResponse {
        order_id: String,
        status: String,
        filled_quantity: Decimal,
        remaining_quantity: Decimal,
    },
    ExecutionReport {
        order_id: String,
        trade_id: String,
        executed_price: Decimal,
        executed_quantity: Decimal,
        timestamp: SystemTime,
    },
}

/// 控制消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    Pause,
    Resume,
    Shutdown,
    ConfigUpdate { config: String },
    HealthCheck,
}

/// 統一數據流消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataflowMessage {
    MarketData(MarketDataPayload),
    Execution(ExecutionPayload),
    Control(ControlMessage),
}

impl DataflowMessage {
    pub fn timestamp(&self) -> SystemTime {
        match self {
            DataflowMessage::MarketData(MarketDataPayload::OrderBookUpdate { timestamp, .. }) => *timestamp,
            DataflowMessage::MarketData(MarketDataPayload::Trade { timestamp, .. }) => *timestamp,
            DataflowMessage::MarketData(MarketDataPayload::Ticker { timestamp, .. }) => *timestamp,
            DataflowMessage::Execution(ExecutionPayload::ExecutionReport { timestamp, .. }) => *timestamp,
            _ => SystemTime::now(),
        }
    }
}

/// 數據流管線配置
#[derive(Debug, Clone)]
pub struct DataflowConfig {
    pub buffer_size: usize,
    pub batch_size: usize,
    pub flush_interval_micros: u64,
}

impl Default for DataflowConfig {
    fn default() -> Self {
        Self {
            buffer_size: 8192,
            batch_size: 64,
            flush_interval_micros: 100,
        }
    }
}