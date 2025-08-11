//! 統一交易所接口定義

use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::core::types::*;
use super::message_types::*;

/// 連接狀態
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Error(String),
}

/// 市場數據狀態
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MarketDataStatus {
    Inactive,
    Subscribing,
    Active,
    Stale,
    Error(String),
}

/// 交易狀態
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TradingStatus {
    Disabled,
    Enabled,
    Suspended,
    Error(String),
}

/// 交易所信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeInfo {
    pub name: String,
    pub connection_status: ConnectionStatus,
    pub market_data_status: MarketDataStatus,
    pub trading_status: TradingStatus,
    pub supported_symbols: Vec<String>,
    pub latency_ms: Option<f64>,
    pub last_heartbeat: u64,
    pub error_count: u64,
}

/// 市場數據客戶端接口
#[async_trait]
pub trait MarketDataClient {
    /// 連接到公共數據流
    async fn connect_public(&mut self) -> Result<(), String>;
    
    /// 斷開公共數據連接
    async fn disconnect_public(&mut self) -> Result<(), String>;
    
    /// 訂閱訂單簿數據
    async fn subscribe_orderbook(&mut self, symbol: &str, depth: u32) -> Result<(), String>;
    
    /// 取消訂閱訂單簿
    async fn unsubscribe_orderbook(&mut self, symbol: &str) -> Result<(), String>;
    
    /// 訂閱成交數據
    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), String>;
    
    /// 取消訂閱成交數據
    async fn unsubscribe_trades(&mut self, symbol: &str) -> Result<(), String>;
    
    /// 訂閱ticker數據
    async fn subscribe_ticker(&mut self, symbol: &str) -> Result<(), String>;
    
    /// 獲取市場事件接收器（有界通道，防止內存泄漏）
    async fn get_market_events(&self) -> Result<mpsc::Receiver<MarketEvent>, String>;
    
    /// 獲取支持的交易對
    async fn get_symbols(&self) -> Result<Vec<String>, String>;
    
    /// 檢查連接健康狀態
    async fn is_healthy(&self) -> bool;
}

/// 交易客戶端接口
#[async_trait]
pub trait TradingClient {
    /// 連接到私有數據流
    async fn connect_private(&mut self) -> Result<(), String>;
    
    /// 斷開私有數據連接
    async fn disconnect_private(&mut self) -> Result<(), String>;
    
    /// 下單
    async fn place_order(&mut self, request: OrderRequest) -> Result<OrderResponse, String>;
    
    /// 取消訂單
    async fn cancel_order(&mut self, request: CancelRequest) -> Result<OrderResponse, String>;
    
    /// 修改訂單
    async fn amend_order(&mut self, request: AmendRequest) -> Result<OrderResponse, String>;
    
    /// 查詢訂單
    async fn get_order(&mut self, order_id: &str) -> Result<ExecutionReport, String>;
    
    /// 查詢活躍訂單
    async fn get_open_orders(&mut self, symbol: Option<&str>) -> Result<Vec<ExecutionReport>, String>;
    
    /// 獲取執行回報接收器
    async fn get_execution_reports(&self) -> Result<mpsc::UnboundedReceiver<ExecutionReport>, String>;
    
    /// 獲取賬戶餘額
    async fn get_balance(&mut self) -> Result<HashMap<String, f64>, String>;
    
    /// 獲取持倉信息
    async fn get_positions(&mut self) -> Result<Vec<Position>, String>;
}

/// 統一交易所接口
#[async_trait]
pub trait Exchange: MarketDataClient + TradingClient {
    /// 獲取交易所名稱
    fn name(&self) -> &str;
    
    /// 獲取交易所信息
    async fn get_info(&self) -> ExchangeInfo;
    
    /// 初始化交易所連接
    async fn initialize(&mut self) -> Result<(), String>;
    
    /// 關閉所有連接
    async fn shutdown(&mut self) -> Result<(), String>;
    
    /// 檢查是否準備好交易
    async fn is_ready_for_trading(&self) -> bool;
    
    /// 獲取手續費信息
    async fn get_fees(&self, symbol: &str) -> Result<(f64, f64), String>; // (maker, taker)
    
    /// 獲取交易規則
    async fn get_trading_rules(&self, symbol: &str) -> Result<TradingRules, String>;
    
    /// 標準化交易對名稱
    fn normalize_symbol(&self, symbol: &str) -> String;
    
    /// 反標準化交易對名稱（轉換為交易所格式）
    fn denormalize_symbol(&self, symbol: &str) -> String;
}

/// 交易規則
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingRules {
    pub symbol: String,
    pub min_quantity: f64,
    pub max_quantity: f64,
    pub quantity_precision: u32,
    pub price_precision: u32,
    pub min_notional: f64,
    pub tick_size: f64,
    pub step_size: f64,
}

impl Default for TradingRules {
    fn default() -> Self {
        Self {
            symbol: String::new(),
            min_quantity: 0.0001,
            max_quantity: 1000000.0,
            quantity_precision: 8,
            price_precision: 8,
            min_notional: 10.0,
            tick_size: 0.01,
            step_size: 0.0001,
        }
    }
}