//! Binance WebSocket 消息類型定義

use integration::json::Value;
use serde::Deserialize;

/// Binance 深度快照
#[derive(Debug, Clone, Deserialize)]
pub struct DepthSnapshot {
    #[serde(rename = "lastUpdateId")]
    pub last_update_id: u64,
    pub bids: Vec<[String; 2]>, // [price, quantity]
    pub asks: Vec<[String; 2]>,
}

/// Binance 深度增量更新
#[derive(Debug, Clone, Deserialize)]
pub struct DepthUpdate {
    #[serde(rename = "e")]
    pub _event_type: String, // "depthUpdate"
    #[serde(rename = "E")]
    pub event_time: u64, // Event time
    #[serde(rename = "s")]
    pub symbol: String, // Symbol
    #[serde(rename = "U")]
    pub _first_update_id: u64, // First update ID in event
    #[serde(rename = "u")]
    pub final_update_id: u64, // Final update ID in event
    #[serde(rename = "b")]
    pub bids: Vec<[String; 2]>, // Bids to be updated
    #[serde(rename = "a")]
    pub asks: Vec<[String; 2]>, // Asks to be updated
}

/// Binance 交易事件
#[derive(Debug, Clone, Deserialize)]
pub struct TradeEvent {
    #[serde(rename = "e")]
    pub _event_type: String, // "trade"
    #[serde(rename = "E")]
    pub _event_time: u64, // Event time
    #[serde(rename = "s")]
    pub symbol: String, // Symbol
    #[serde(rename = "t")]
    pub trade_id: u64, // Trade ID
    #[serde(rename = "p")]
    pub price: String, // Price
    #[serde(rename = "q")]
    pub quantity: String, // Quantity
    #[serde(rename = "b")]
    pub _buyer_order_id: u64, // Buyer order ID
    #[serde(rename = "a")]
    pub _seller_order_id: u64, // Seller order ID
    #[serde(rename = "T")]
    pub trade_time: u64, // Trade time
    #[serde(rename = "m")]
    pub is_buyer_maker: bool, // Is the buyer the market maker?
    #[serde(rename = "M")]
    pub _ignore: bool, // Ignore
}

/// Binance K線事件
#[derive(Debug, Clone, Deserialize)]
pub struct KlineEvent {
    #[serde(rename = "e")]
    pub _event_type: String, // "kline"
    #[serde(rename = "E")]
    pub _event_time: u64, // Event time
    #[serde(rename = "s")]
    pub symbol: String, // Symbol
    #[serde(rename = "k")]
    pub kline: KlineData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct KlineData {
    #[serde(rename = "t")]
    pub start_time: u64, // Kline start time
    #[serde(rename = "T")]
    pub close_time: u64, // Kline close time
    #[serde(rename = "s")]
    pub _symbol: String, // Symbol
    #[serde(rename = "i")]
    pub interval: String, // Interval
    #[serde(rename = "f")]
    pub _first_trade_id: u64, // First trade ID
    #[serde(rename = "L")]
    pub _last_trade_id: u64, // Last trade ID
    #[serde(rename = "o")]
    pub open_price: String, // Open price
    #[serde(rename = "c")]
    pub close_price: String, // Close price
    #[serde(rename = "h")]
    pub high_price: String, // High price
    #[serde(rename = "l")]
    pub low_price: String, // Low price
    #[serde(rename = "v")]
    pub volume: String, // Base asset volume
    #[serde(rename = "n")]
    pub trade_count: u32, // Number of trades
    #[serde(rename = "x")]
    pub _is_closed: bool, // Is this kline closed?
    #[serde(rename = "q")]
    pub _quote_volume: String, // Quote asset volume
    #[serde(rename = "V")]
    pub _taker_buy_volume: String, // Taker buy base asset volume
    #[serde(rename = "Q")]
    pub _taker_buy_quote_volume: String, // Taker buy quote asset volume
    #[serde(rename = "B")]
    pub _ignore: String, // Ignore
}

/// WebSocket 流消息封裝
///
/// 使用統一的 Value 類型（根據 json-simd feature 自動切換）
#[derive(Debug, Clone, Deserialize)]
pub struct StreamMessage {
    pub stream: String,
    pub data: Value,
}

/// Binance L1 最優買賣（bookTicker）
#[derive(Debug, Clone, Deserialize)]
pub struct BookTickerEvent {
    #[serde(rename = "e")]
    pub _event_type: String, // "bookTicker"
    #[serde(rename = "E")]
    pub event_time: u64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "b")]
    pub best_bid_price: String,
    #[serde(rename = "B")]
    pub best_bid_qty: String,
    #[serde(rename = "a")]
    pub best_ask_price: String,
    #[serde(rename = "A")]
    pub best_ask_qty: String,
}

/// REST API 響應錯誤
#[derive(Debug, Clone, Deserialize)]
pub struct BinanceError {
    pub code: i32,
    pub msg: String,
}
