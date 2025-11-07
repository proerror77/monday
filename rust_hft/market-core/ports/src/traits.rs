//! 核心 traits - 適配器實現的穩定契約

use crate::events::*;
use async_trait::async_trait;
use futures::Stream;
use hft_core::*;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;

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
        _venue_spec: Option<&VenueSpec>,
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
        new_price: Option<Price>,
    ) -> HftResult<()>;

    /// 執行回報流 (填充、ACK、拒絕等)
    async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>>;

    /// 獲取未結訂單列表 (用於對賬)
    async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>>;

    /// 連線管理
    async fn connect(&mut self) -> HftResult<()>;
    async fn disconnect(&mut self) -> HftResult<()>;
    async fn health(&self) -> ConnectionHealth;
}

/// 帳戶視圖 (策略決策用)
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub quantity: Quantity,
    pub avg_price: Price,
    pub unrealized_pnl: f64,
}

/// 策略接口
/// 策略處理的市場範疇
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueScope {
    Single,
    Cross,
}

pub trait Strategy: Send + Sync {
    /// 處理市場事件，返回交易意圖
    fn on_market_event(&mut self, event: &MarketEvent, account: &AccountView) -> Vec<OrderIntent>;

    /// 處理執行事件 (成交回報等)
    fn on_execution_event(
        &mut self,
        event: &ExecutionEvent,
        account: &AccountView,
    ) -> Vec<OrderIntent>;

    /// 策略名稱
    fn name(&self) -> &str;
    /// 策略實例ID（預設等同於 name；可被覆寫以回傳穩定實例ID）
    fn id(&self) -> &str {
        self.name()
    }
    /// 策略場域範疇（單場/跨場）；預設單場，可由策略覆寫
    fn venue_scope(&self) -> VenueScope {
        VenueScope::Single
    }

    /// 策略初始化
    fn initialize(&mut self) -> HftResult<()> {
        Ok(())
    }

    /// 策略清理
    fn shutdown(&mut self) -> HftResult<()> {
        Ok(())
    }

    /// 向下转型支持（用于运行时类型检查）
    fn as_any(&self) -> &dyn std::any::Any {
        panic!("as_any not implemented for this strategy")
    }

    /// 向下转型支持（可变引用）
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        panic!("as_any_mut not implemented for this strategy")
    }
}

/// 風控決策
#[derive(Debug, Clone)]
pub enum RiskDecision {
    Allow,
    Reject {
        reason: String,
    },
    Modify {
        new_quantity: Quantity,
        reason: String,
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

impl VenueSpec {
    /// Phase 1 重構：為常見交易所創建預設 VenueSpec
    pub fn binance_spot() -> Self {
        Self {
            name: "BINANCE".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(), // 通用價格精度
            lot_size: Quantity::from_f64(0.00001).unwrap(), // 5位小數
            min_qty: Quantity::from_f64(0.00001).unwrap(),
            max_quantity: Some(Quantity::from_f64(900000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(10), // 10 USDT
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            rate_limit: Some(1200),                        // 1200 requests/minute
        }
    }

    pub fn bitget_spot() -> Self {
        Self {
            name: "BITGET".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(), // 4位小數
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1000000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5), // 5 USDT
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            rate_limit: Some(600),                        // 600 requests/minute
        }
    }

    pub fn bybit_spot() -> Self {
        Self {
            name: "BYBIT".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.000001).unwrap(), // 6位小數
            min_qty: Quantity::from_f64(0.000001).unwrap(),
            max_quantity: Some(Quantity::from_f64(500000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(1), // 1 USDT
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.1%
            rate_limit: Some(120),                        // 120 requests/minute
        }
    }

    pub fn okx_spot() -> Self {
        Self {
            name: "OKX".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.000001).unwrap(),
            min_qty: Quantity::from_f64(0.000001).unwrap(),
            max_quantity: Some(Quantity::from_f64(500000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: Some(rust_decimal::Decimal::new(8, 4)), // 0.08%
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)), // 0.10%
            rate_limit: Some(180),
        }
    }

    pub fn hyperliquid_spot() -> Self {
        // 占位默认规格，后续可依据官方文档细化
        Self {
            name: "HYPERLIQUID".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    pub fn backpack_spot() -> Self {
        // Backpack 官方最小步进依 symbol 不同，可透过市場 API 覆寫
        Self {
            name: "BACKPACK".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)),
            taker_fee_bps: Some(rust_decimal::Decimal::new(10, 4)),
            rate_limit: Some(600),
        }
    }

    pub fn lighter_spot() -> Self {
        // 參照公開資訊設置保守預設，後續可依據 lighter 規則更新
        Self {
            name: "LIGHTER".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    pub fn grvt_perp() -> Self {
        // GRVT 預設合約規格（暫以保守數值，實際可依 instrument API 覆寫）
        Self {
            name: "GRVT".to_string(),
            tick_size: Price::from_f64(0.01).unwrap(),
            lot_size: Quantity::from_f64(0.0001).unwrap(),
            min_qty: Quantity::from_f64(0.0001).unwrap(),
            max_quantity: Some(Quantity::from_f64(1_000_000.0).unwrap()),
            min_notional: rust_decimal::Decimal::from(5),
            maker_fee_bps: None,
            taker_fee_bps: None,
            rate_limit: None,
        }
    }

    /// 構建預設的 VenueSpec 映射
    pub fn build_default_venue_specs() -> std::collections::HashMap<VenueId, VenueSpec> {
        let mut specs = std::collections::HashMap::new();
        specs.insert(VenueId::BINANCE, Self::binance_spot());
        specs.insert(VenueId::BITGET, Self::bitget_spot());
        specs.insert(VenueId::BYBIT, Self::bybit_spot());
        specs.insert(VenueId::OKX, Self::okx_spot());
        specs.insert(hft_core::VenueId::HYPERLIQUID, Self::hyperliquid_spot());
        specs.insert(hft_core::VenueId::BACKPACK, Self::backpack_spot());
        specs.insert(hft_core::VenueId::LIGHTER, Self::lighter_spot());
        specs.insert(hft_core::VenueId::GRVT, Self::grvt_perp());
        specs
    }
}

/// 風控管理器接口
pub trait RiskManager: Send + Sync {
    /// 審核訂單意圖，返回風控決策（舊版 - 字符串映射）
    fn review_orders(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue_specs: &std::collections::HashMap<String, VenueSpec>,
    ) -> Vec<OrderIntent>;

    /// 審核訂單意圖（使用單個 VenueSpec）
    fn review(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue: &VenueSpec,
    ) -> Vec<OrderIntent>;

    /// Phase 1 重構：審核訂單意圖（使用 VenueId 映射）
    fn review_with_venue_specs(
        &mut self,
        intents: Vec<OrderIntent>,
        account: &AccountView,
        venue_specs: &std::collections::HashMap<VenueId, VenueSpec>,
    ) -> Vec<OrderIntent> {
        // 默認實現：批量調用 review()，根據 target_venue 或 symbol 查找對應 VenueSpec
        let mut approved_intents = Vec::new();

        for intent in intents {
            // 1. 優先使用 intent.target_venue
            let venue_spec = if let Some(target_venue) = intent.target_venue {
                venue_specs.get(&target_venue)
            } else {
                // 2. 回退到從 symbol 推斷 venue（簡單實現）
                // 這裡假設 symbol 格式為 "VENUE:SYMBOL" 或純 symbol
                let _base_symbol = BaseSymbol::from_venue_symbol(intent.symbol.as_str());
                // 簡化處理：使用第一個可用的 VenueSpec
                venue_specs.values().next()
            };

            if let Some(spec) = venue_spec {
                let reviewed = self.review(vec![intent], account, spec);
                approved_intents.extend(reviewed);
            } else {
                // 沒有找到對應的 VenueSpec，拒絕此訂單
                eprintln!("Warning: No VenueSpec found for intent: {:?}", intent);
            }
        }

        approved_intents
    }

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
    pub var_1d: f64, // 1日風險價值
    pub leverage: f64,
    pub concentration_risk: f64,
    pub order_rate: f64, // 訂單頻率
    pub last_update: Timestamp,
}

// OrderStatus 已在 events 模組定義，此處直接使用 pub use crate::events::OrderStatus;

/// 訂單更新資訊
#[derive(Debug, Clone)]
pub struct OrderUpdate {
    pub order_id: OrderId,
    pub status: OrderStatus,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub previous_status: OrderStatus,
}

/// 註冊訂單參數
#[derive(Debug, Clone)]
pub struct RegisterOrderParams {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    pub venue: Option<hft_core::VenueId>,
    pub strategy_id: Option<String>,
}

/// 訂單記錄
#[derive(Debug, Clone)]
pub struct OrderRecord {
    pub order_id: OrderId,
    pub client_order_id: Option<String>,
    pub symbol: Symbol,
    pub side: Side,
    pub qty: Quantity,
    pub cum_qty: Quantity,
    pub avg_price: Option<Price>,
    pub status: OrderStatus,
    pub venue: Option<hft_core::VenueId>,
    pub strategy_id: Option<String>,
}

/// 訂單管理器 trait - 提供訂單生命週期管理能力
pub trait OrderManager: Send + Sync {
    /// 註冊新訂單
    fn register_order(&mut self, params: RegisterOrderParams);

    /// 處理執行事件，返回訂單狀態更新
    fn on_execution_event(&mut self, event: &ExecutionEvent) -> Option<OrderUpdate>;

    /// 導出 OMS 狀態（供恢復/持久化使用）
    fn export_state(&self) -> std::collections::HashMap<OrderId, OrderRecord>;

    /// 導入 OMS 狀態（供恢復/持久化使用）
    fn import_state(&mut self, state: std::collections::HashMap<OrderId, OrderRecord>);

    /// 取得指定策略的未結訂單
    fn open_order_pairs_by_strategy(&self, strategy_id: &str) -> Vec<(OrderId, Symbol)>;
}

/// Portfolio 狀態（供持久化使用）
#[derive(Debug, Clone)]
pub struct PortfolioState {
    pub account_view: AccountView,
    pub order_meta: std::collections::HashMap<OrderId, (Symbol, Side)>,
    pub market_prices: std::collections::HashMap<Symbol, Price>,
    /// 已處理的成交ID（去重），恢復後避免重覆累計
    pub processed_fill_ids: std::collections::HashMap<OrderId, std::collections::HashSet<String>>,
}

/// Portfolio 管理器 trait - 提供帳戶會計能力
pub trait PortfolioManager: Send + Sync {
    /// 註冊訂單元資訊（供 fill 時查找 symbol/side）
    fn register_order(&mut self, order_id: OrderId, symbol: Symbol, side: Side);

    /// 處理執行事件
    fn on_execution_event(&mut self, event: &ExecutionEvent);

    /// 獲取帳戶視圖讀取器
    fn reader(&self) -> Arc<dyn hft_snapshot::SnapshotReader<AccountView>>;

    /// 更新市場價格並重新計算未實現盈虧
    fn update_market_prices(&mut self, prices: &std::collections::HashMap<Symbol, Price>);

    /// 導出 Portfolio 狀態（供恢復/持久化使用）
    fn export_state(&self) -> PortfolioState;

    /// 導入 Portfolio 狀態（供恢復/持久化使用）
    fn import_state(&mut self, state: PortfolioState);
}
