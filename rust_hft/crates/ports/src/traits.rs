//! 核心 traits - 適配器實現的穩定契約

use crate::events::*;
use hft_core::*;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// 裝箱的事件流
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = HftResult<T>> + Send>>;

/// 連線健康狀態
#[derive(Debug, Clone)]
pub struct ConnectionHealth {
    pub connected: bool,
    pub latency_ms: Option<f64>,
    pub last_heartbeat: Timestamp,
}

/// 市場數據流接口 (公有行情)
#[async_trait]
pub trait MarketStream: Send + Sync {
    /// 訂閱指定品種，返回統一事件流
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>>;
    
    /// 健康檢查
    async fn health(&self) -> ConnectionHealth;
    
    /// 開始連線
    async fn connect(&mut self) -> HftResult<()>;
    
    /// 斷開連線
    async fn disconnect(&mut self) -> HftResult<()>;
}

/// 執行客戶端接口 (私有流 + 下單)
#[async_trait]
pub trait ExecutionClient: Send + Sync {
    /// 下單 (live/mock 實現不同)
    async fn place_order(&mut self, intent: OrderIntent) -> HftResult<OrderId>;
    
    /// 帶 VenueSpec 校驗的下單
    async fn place_order_with_spec(
        &mut self, 
        intent: OrderIntent, 
        _venue_spec: Option<&VenueSpec>
    ) -> HftResult<OrderId> {
        // 默認實現：調用普通下單方法 (向後兼容)
        self.place_order(intent).await
    }
    
    /// 撤單
    async fn cancel_order(&mut self, order_id: &OrderId) -> HftResult<()>;
    
    /// 修改訂單
    async fn modify_order(
        &mut self, 
        order_id: &OrderId, 
        new_quantity: Option<Quantity>,
        new_price: Option<Price>
    ) -> HftResult<()>;
    
    /// 執行回報流 (填充、ACK、拒絕等)
    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>>;
    
    /// 連線管理
    async fn connect(&mut self) -> HftResult<()>;
    async fn disconnect(&mut self) -> HftResult<()>;
    async fn health(&self) -> ConnectionHealth;
}

/// 帳戶視圖 (策略決策用)
#[derive(Debug, Clone)]
pub struct AccountView {
    pub cash_balance: f64,
    pub positions: std::collections::HashMap<Symbol, Position>,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
}

impl Default for AccountView {
    fn default() -> Self {
        Self {
            cash_balance: 0.0,
            positions: std::collections::HashMap::new(),
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: Symbol,
    pub quantity: Quantity,
    pub avg_price: Price,
    pub unrealized_pnl: f64,
}

/// 策略接口
pub trait Strategy: Send + Sync {
    /// 處理市場事件，返回交易意圖
    fn on_market_event(
        &mut self, 
        event: &MarketEvent, 
        account: &AccountView
    ) -> Vec<OrderIntent>;
    
    /// 處理執行事件 (成交回報等)
    fn on_execution_event(
        &mut self, 
        event: &ExecutionEvent, 
        account: &AccountView
    ) -> Vec<OrderIntent>;
    
    /// 策略名稱
    fn name(&self) -> &str;
    
    /// 策略初始化
    fn initialize(&mut self) -> HftResult<()> { Ok(()) }
    
    /// 策略清理
    fn shutdown(&mut self) -> HftResult<()> { Ok(()) }
}

/// 風控決策
#[derive(Debug, Clone)]
pub enum RiskDecision {
    Allow,
    Reject { reason: String },
    Modify { 
        new_quantity: Quantity, 
        reason: String 
    },
}

/// 交易所規格（作為穩定契約的一部分）
#[derive(Debug, Clone)]
pub struct VenueSpec {
    pub name: String,
    // 精度/步進
    pub tick_size: Price,
    pub lot_size: Quantity,
    // 數量/名義約束
    pub min_qty: Quantity,
    pub max_quantity: Option<Quantity>,
    pub min_notional: rust_decimal::Decimal,
    // 風險/費率/限流（可選）
    pub maker_fee_bps: Option<rust_decimal::Decimal>,
    pub taker_fee_bps: Option<rust_decimal::Decimal>,
    pub rate_limit: Option<u32>, // 每秒請求限制
}

impl Default for VenueSpec {
    fn default() -> Self {
        Self {
            name: "DEFAULT".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.001).unwrap(),
            min_qty: Quantity::from_f64(0.001).unwrap(),
            max_quantity: None,
            min_notional: rust_decimal::Decimal::from(10),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }
}

/// 風控管理器接口
pub trait RiskManager: Send + Sync {
    /// 審核訂單意圖，返回風控決策
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue_specs: &std::collections::HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent>;
    
    /// 審核訂單意圖（使用新的 instrument VenueSpec）
    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue: &VenueSpec,
    ) -> Vec<OrderIntent>;
    
    /// 處理執行事件
    fn on_execution_event(&mut self, event: &ExecutionEvent);
    
    /// 緊急停止
    fn emergency_stop(&mut self) -> Result<(), HftError>;
    
    /// 獲取風控指標
    fn get_risk_metrics(&self) -> std::collections::HashMap<String, f64>;
    
    /// 是否應該暫停交易 (熔斷)
    fn should_halt_trading(&self, account: &AccountView) -> bool;
    
    /// 風控指標
    fn risk_metrics(&self) -> RiskMetrics;
}

/// 風控指標
#[derive(Debug, Clone)]
pub struct RiskMetrics {
    pub max_drawdown: f64,
    pub current_drawdown: f64,
    pub var_1d: f64,               // 1日風險價值
    pub leverage: f64,
    pub concentration_risk: f64,
    pub order_rate: f64,           // 訂單頻率
    pub last_update: Timestamp,
}
