use chrono::prelude::*;
use clickhouse::Row;
use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub struct OrderBookState {
    pub bids: BTreeMap<OrderedFloat<f64>, f64>,
    pub asks: BTreeMap<OrderedFloat<f64>, f64>,
    pub last_update_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct DepthUpdate {
    #[serde(rename = "e")]
    pub event_type: String,
    #[serde(rename = "E")]
    pub event_time: i64,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "u")]
    pub final_update_id: i64,
    #[serde(rename = "b")]
    pub bids: Vec<(String, String)>,
    #[serde(rename = "a")]
    pub asks: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
pub struct Trade {
    #[serde(rename = "e")]
    pub event_type: String,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "t")]
    pub trade_id: i64,
    #[serde(rename = "p")]
    pub price: String,
    #[serde(rename = "q")]
    pub quantity: String,
    #[serde(rename = "T")]
    pub trade_time: i64,
    #[serde(rename = "m")]
    pub is_buyer_maker: bool,
}

#[derive(Debug, Deserialize)]
pub struct PartialDepthSnapshot {
    #[serde(rename = "lastUpdateId")]
    pub last_update_id: i64,
    #[serde(rename = "bids")]
    pub bids: Vec<(String, String)>,
    #[serde(rename = "asks")]
    pub asks: Vec<(String, String)>,
}

#[derive(Debug, Deserialize)]
pub struct BookTicker {
    #[serde(rename = "e")]
    pub _event_type: Option<String>,
    #[serde(rename = "E")]
    pub event_time: Option<i64>,
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

#[derive(Debug, Deserialize)]
pub struct MiniTicker {
    #[serde(rename = "e")]
    pub event_type: Option<String>,
    #[serde(rename = "E")]
    pub event_time: Option<i64>,
    #[serde(rename = "s")]
    pub symbol: String,
    #[serde(rename = "c")]
    pub last_price: String,
    #[serde(rename = "o")]
    pub open_price: String,
    #[serde(rename = "h")]
    pub high_price: String,
    #[serde(rename = "l")]
    pub low_price: String,
    #[serde(rename = "v")]
    pub volume: String,
    #[serde(rename = "q")]
    pub quote_volume: String,
}

#[derive(Debug, Serialize, Deserialize, Row)]
pub struct OrderbookRow {
    pub ts: DateTime<Utc>,
    pub symbol: String,
    pub side: String,
    pub price: f64,
    pub qty: f64,
    pub update_id: i64,
}

#[derive(Debug, Serialize, Deserialize, Row)]
pub struct TradesRow {
    pub ts: DateTime<Utc>,
    pub symbol: String,
    pub trade_id: i64,
    pub price: f64,
    pub qty: f64,
    pub is_buyer_maker: bool,
}

#[derive(Debug, Serialize, Deserialize, Row)]
pub struct L1Row {
    pub ts: DateTime<Utc>,
    pub symbol: String,
    pub bid_px: f64,
    pub bid_qty: f64,
    pub ask_px: f64,
    pub ask_qty: f64,
}

#[derive(Debug, Serialize, Deserialize, Row)]
pub struct TickerRow {
    pub ts: DateTime<Utc>,
    pub symbol: String,
    pub last: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: f64,
    pub quote_volume: f64,
}
