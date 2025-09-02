//! 完整的訂單管理系統(OMS)
//!
//! 包含訂單狀態機、智能路由、風險控制和執行優化

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
// use futures::future; // 未使用

use crate::core::types::*;
use crate::exchanges::{ExchangeManager, ExecutionReport, OrderRequest};
// P0 修復：集成風險控制到 OMS
use crate::engine::unified::{Portfolio, Signal};
use crate::engine::unified_risk_manager::{RiskManagerConfig, UnifiedRiskManager};

// 新架構組件
use crate::app::config::ConfigManager;
use crate::app::services::order_service::OrderServiceConfig;
use crate::app::services::execution_service::ExecutionConfig;
use crate::app::events::EventBus;
use crate::app::routing::{ExchangeRouter, RouterConfig};
use crate::app::services::{ExecutionService, OrderService, RiskService};
use crate::domains::trading::{self, ToPrice, ToQuantity};
use rust_decimal::prelude::ToPrimitive;

/// 訂單狀態機 - 支援多賬戶架構
#[derive(Debug, Clone)]
pub struct OrderStateMachine {
    pub order_id: String,
    pub client_order_id: String,
    pub account_id: AccountId, // 新增：賬戶隔離
    pub exchange: String,
    pub symbol: String,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub original_quantity: f64,
    pub executed_quantity: f64,
    pub remaining_quantity: f64,
    pub price: Option<f64>,
    pub avg_fill_price: f64,
    pub status: OrderStatus,
    pub fills: Vec<Fill>,
    pub create_time: u64,
    pub update_time: u64,
    pub last_fill_time: Option<u64>,
    pub reject_reason: Option<String>,
    pub slippage_bps: f64,
    pub commission: f64,
    pub commission_asset: String,
}

/// 成交記錄
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub fill_id: String,
    pub order_id: String,
    pub price: Price,
    pub quantity: Quantity,
    pub side: OrderSide,
    pub commission: f64,
    pub commission_asset: String,
    pub timestamp: u64,
    pub is_maker: bool,
}

/// 多賬戶投資組合管理
#[derive(Debug, Clone)]
pub struct MultiAccountPortfolio {
    /// 賬戶映射 (account_id -> Portfolio)
    pub accounts: HashMap<trading::types::AccountId, trading::types::Portfolio>,
}

impl MultiAccountPortfolio {
    /// 創建空的多賬戶投資組合
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
        }
    }

    /// 獲取或創建賬戶組合
    pub fn get_or_create_portfolio(&mut self, account_id: &trading::types::AccountId) -> &mut trading::types::Portfolio {
        self.accounts.entry(account_id.clone()).or_insert_with(|| {
            trading::types::Portfolio::new(account_id.clone(), 0.0)
        })
    }

    /// 添加賬戶
    pub fn add_account(&mut self, account_id: trading::types::AccountId, initial_cash: f64) {
        self.accounts.insert(account_id.clone(), trading::types::Portfolio::new(account_id, initial_cash));
    }

    /// 移除賬戶
    pub fn remove_account(&mut self, account_id: &trading::types::AccountId) -> Option<trading::types::Portfolio> {
        self.accounts.remove(account_id)
    }

    /// 獲取所有賬戶ID
    pub fn get_account_ids(&self) -> Vec<trading::types::AccountId> {
        self.accounts.keys().cloned().collect()
    }

    /// 獲取賬戶組合（不可變引用）
    pub fn get_account(&self, account_id: &trading::types::AccountId) -> Option<&trading::types::Portfolio> {
        self.accounts.get(account_id)
    }

    /// 獲取賬戶組合（可變引用）
    pub fn get_account_mut(&mut self, account_id: &trading::types::AccountId) -> Option<&mut trading::types::Portfolio> {
        self.accounts.get_mut(account_id)
    }

    /// 檢查是否存在賬戶
    pub fn has_account(&self, account_id: &trading::types::AccountId) -> bool {
        self.accounts.contains_key(account_id)
    }

    /// 更新全局指標
    pub fn update_global_metrics(&mut self) {
        // 更新每個賬戶的組合價值
        for portfolio in self.accounts.values_mut() {
            portfolio.update_total_value();
        }
    }

    /// 獲取總價值
    pub fn total_value(&self) -> f64 {
        self.accounts.values().map(|p| p.total_value).sum()
    }

    /// 獲取總未實現盈虧
    pub fn total_unrealized_pnl(&self) -> f64 {
        self.accounts.values().map(|p| p.unrealized_pnl).sum()
    }

    /// 獲取總已實現盈虧
    pub fn total_realized_pnl(&self) -> f64 {
        self.accounts.values().map(|p| p.realized_pnl).sum()
    }

    /// 獲取總現金
    pub fn total_cash(&self) -> f64 {
        self.accounts.values().map(|p| p.cash).sum()
    }
}

/// 完整OMS系統 - 重構整合新架構組件
pub struct CompleteOMS {
    // 保留原有組件以兼容現有代碼
    exchange_manager: Arc<ExchangeManager>,
    orders: Arc<RwLock<HashMap<String, OrderStateMachine>>>,
    pending_orders: Arc<RwLock<HashMap<String, OrderRequest>>>,

    // P0 修復：集成風險管理器 - 支援多賬戶
    risk_managers: Arc<RwLock<HashMap<AccountId, Arc<UnifiedRiskManager>>>>,
    multi_portfolio: Arc<RwLock<MultiAccountPortfolio>>,

    // 兼容性：保留單一 Portfolio 接口
    portfolio: Arc<RwLock<Portfolio>>,

    // 事件通道 - 保留兼容性
    order_updates_sender: mpsc::UnboundedSender<OrderUpdate>,
    execution_reports: Arc<Mutex<Vec<mpsc::Receiver<ExecutionReport>>>>,

    // 配置
    config: OMSConfig,

    // 性能統計
    stats: Arc<RwLock<OMSStats>>,

    // === 新架構組件整合 ===
    /// 交易所路由器 - 替代直接使用 ExchangeManager
    exchange_router: Arc<ExchangeRouter>,

    /// 訂單服務 - 處理訂單生命週期
    order_service: Arc<OrderService>,

    /// 風險服務 - 統一風險控制
    risk_service: Arc<RiskService>,

    /// 執行服務 - 執行質量監控
    execution_service: Arc<ExecutionService>,

    /// 事件總線 - 統一事件分發
    event_bus: Arc<EventBus>,

    /// 配置管理器 - 動態配置
    config_manager: Arc<ConfigManager>,
}

/// OMS配置
#[derive(Debug, Clone)]
pub struct OMSConfig {
    pub default_timeout_ms: u64,
    pub max_slippage_bps: f64,
    pub enable_partial_fills: bool,
    pub auto_cancel_on_disconnect: bool,
    pub order_update_interval_ms: u64,
    pub max_pending_orders: usize,
    pub enable_smart_routing: bool,
}

impl Default for OMSConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: 30000, // 30秒
            max_slippage_bps: 50.0,    // 50個基點
            enable_partial_fills: true,
            auto_cancel_on_disconnect: true,
            order_update_interval_ms: 1000, // 1秒
            max_pending_orders: 1000,
            enable_smart_routing: true,
        }
    }
}

/// OMS統計信息
#[derive(Debug, Clone, Default)]
pub struct OMSStats {
    pub orders_submitted: u64,
    pub orders_filled: u64,
    pub orders_cancelled: u64,
    pub orders_rejected: u64,
    pub total_orders: u64,
    pub pending_orders: u64,
    pub partially_filled_orders: u64,
    pub filled_orders: u64,
    pub cancelled_orders: u64,
    pub rejected_orders: u64,
    pub total_commission: f64,
    pub avg_execution_time_us: f64,
    pub total_volume: f64,
    pub avg_fill_time_ms: f64,
    pub avg_slippage_bps: f64,
    pub error_rate: f64,
    pub update_time: u64,
}

/// 訂單更新事件
#[derive(Debug, Clone)]
pub enum OrderUpdate {
    New(OrderStateMachine),
    PartialFill(OrderStateMachine, Fill),
    Filled(OrderStateMachine),
    Cancelled(OrderStateMachine),
    Rejected(OrderStateMachine, String),
    Expired(OrderStateMachine),
    Error(String, String), // order_id, error
}

impl CompleteOMS {
    pub fn new(exchange_manager: Arc<ExchangeManager>) -> Self {
        let (order_updates_sender, _) = mpsc::unbounded_channel();

        // 初始化新架構組件
        let config_manager = Arc::new(ConfigManager::new());
        let (event_bus, _event_receiver) = EventBus::new(false);
        let event_bus = Arc::new(event_bus);

        // 創建交易所路由器
        let router_config = RouterConfig::default();
        let exchange_router =
            Arc::new(ExchangeRouter::new(exchange_manager.clone(), router_config));

        // 創建服務組件
        let order_service = Arc::new(OrderService::new(
            exchange_router.clone(),
            OrderServiceConfig::default(),
        ));

        let risk_service = Arc::new(RiskService::new());

        let execution_service = Arc::new(ExecutionService::new(
            exchange_router.clone(),
            ExecutionConfig::default(),
        ));

        Self {
            // 保留原有組件
            exchange_manager,
            orders: Arc::new(RwLock::new(HashMap::new())),
            pending_orders: Arc::new(RwLock::new(HashMap::new())),

            // P0 修復：初始化多賬戶風險管理器
            risk_managers: Arc::new(RwLock::new(HashMap::new())),
            multi_portfolio: Arc::new(RwLock::new(MultiAccountPortfolio::new())),

            // 兼容性：保留舊版 Portfolio
            portfolio: Arc::new(RwLock::new(Portfolio::default())),

            order_updates_sender,
            execution_reports: Arc::new(Mutex::new(Vec::new())),
            config: OMSConfig::default(),
            stats: Arc::new(RwLock::new(OMSStats::default())),

            // 新架構組件
            exchange_router,
            order_service,
            risk_service,
            execution_service,
            event_bus,
            config_manager,
        }
    }

    /// P0修復：創建OMS實例並返回事件接收器（確保事件通道正確連接）
    pub fn new_with_receiver(
        exchange_manager: Arc<ExchangeManager>,
    ) -> (Self, mpsc::UnboundedReceiver<OrderUpdate>) {
        let (order_updates_sender, order_updates_receiver) = mpsc::unbounded_channel();

        // 初始化新架構組件
        let config_manager = Arc::new(ConfigManager::new());
        let (event_bus, _event_receiver) = EventBus::new(false);
        let event_bus = Arc::new(event_bus);

        // 創建交易所路由器
        let router_config = RouterConfig::default();
        let exchange_router =
            Arc::new(ExchangeRouter::new(exchange_manager.clone(), router_config));

        // 創建服務組件
        let order_service = Arc::new(OrderService::new(
            exchange_router.clone(),
            OrderServiceConfig::default(),
        ));

        let risk_service = Arc::new(RiskService::new());

        let execution_service = Arc::new(ExecutionService::new(
            exchange_router.clone(),
            ExecutionConfig::default(),
        ));

        let oms = Self {
            // 保留原有組件
            exchange_manager,
            orders: Arc::new(RwLock::new(HashMap::new())),
            pending_orders: Arc::new(RwLock::new(HashMap::new())),

            // P0 修復：初始化多賬戶風險管理器
            risk_managers: Arc::new(RwLock::new(HashMap::new())),
            multi_portfolio: Arc::new(RwLock::new(MultiAccountPortfolio::new())),

            // 兼容性：保留舊版 Portfolio
            portfolio: Arc::new(RwLock::new(Portfolio::default())),

            order_updates_sender,
            execution_reports: Arc::new(Mutex::new(Vec::new())),
            config: OMSConfig::default(),
            stats: Arc::new(RwLock::new(OMSStats::default())),

            // 新架構組件
            exchange_router,
            order_service,
            risk_service,
            execution_service,
            event_bus,
            config_manager,
        };

        (oms, order_updates_receiver)
    }

    pub fn with_config(exchange_manager: Arc<ExchangeManager>, config: OMSConfig) -> Self {
        let mut oms = Self::new(exchange_manager);
        oms.config = config;
        oms
    }

    /// 添加賬戶與初始資金
    pub async fn add_account(
        &self,
        account_id: AccountId,
        initial_cash: f64,
    ) -> Result<(), String> {
        // 添加到多賬戶組合
        self.multi_portfolio
            .write()
            .await
            .add_account(account_id.clone(), initial_cash);

        // 為此賬戶創建專用風險管理器
        let risk_manager = Arc::new(UnifiedRiskManager::new(RiskManagerConfig::default()));
        self.risk_managers
            .write()
            .await
            .insert(account_id.clone(), risk_manager);

        info!(
            "Added account {} with initial cash: {:.2}",
            account_id, initial_cash
        );
        Ok(())
    }

    /// 添加賬戶並指定風險配置
    pub async fn add_account_with_risk_config(
        &self,
        account_id: AccountId,
        initial_cash: f64,
        risk_config: RiskManagerConfig,
    ) -> Result<(), String> {
        // 添加到多賬戶組合
        self.multi_portfolio
            .write()
            .await
            .add_account(account_id.clone(), initial_cash);

        // 為此賬戶創建專用風險管理器
        let risk_manager = Arc::new(UnifiedRiskManager::new(risk_config));
        self.risk_managers
            .write()
            .await
            .insert(account_id.clone(), risk_manager);

        info!(
            "Added account {} with initial cash: {:.2} and custom risk config",
            account_id, initial_cash
        );
        Ok(())
    }

    /// 移除賬戶
    pub async fn remove_account(&self, account_id: &AccountId) -> Result<(), String> {
        // 檢查是否有活躍訂單
        let active_orders: Vec<_> = self
            .orders
            .read()
            .await
            .values()
            .filter(|order| &order.account_id == account_id)
            .filter(|order| {
                matches!(
                    order.status,
                    OrderStatus::New | OrderStatus::PartiallyFilled | OrderStatus::Pending
                )
            })
            .cloned()
            .collect();

        if !active_orders.is_empty() {
            return Err(format!(
                "Cannot remove account {} with {} active orders",
                account_id,
                active_orders.len()
            ));
        }

        // 移除賬戶組合
        self.multi_portfolio
            .write()
            .await
            .remove_account(account_id);

        // 移除風險管理器
        self.risk_managers.write().await.remove(account_id);

        info!("Removed account: {}", account_id);
        Ok(())
    }

    /// 獲取賬戶列表
    pub async fn get_accounts(&self) -> Vec<AccountId> {
        self.multi_portfolio.read().await.get_account_ids()
    }

    /// 獲取賬戶投資組合
    pub async fn get_account_portfolio(&self, account_id: &AccountId) -> Option<trading::types::Portfolio> {
        self.multi_portfolio
            .read()
            .await
            .get_account(account_id)
            .cloned()
    }

    /// 獲取全局投資組合統計
    pub async fn get_global_portfolio_stats(&self) -> (f64, f64, f64, f64) {
        // (total_cash, realized_pnl, unrealized_pnl, total_value)
        let portfolio = self.multi_portfolio.read().await;
        (
            portfolio.total_cash(),
            portfolio.total_realized_pnl(),
            portfolio.total_unrealized_pnl(),
            portfolio.total_value(),
        )
    }

    /// 啟動OMS服務（P0修復：簡化版本，不返回接收器，使用new_with_receiver()獲取事件）
    pub async fn start(&self) -> Result<(), String> {
        info!("Starting Complete OMS with new architecture");

        // === 使用新架構 ExchangeRouter 初始化連接 ===
        // ExchangeRouter 已經在構造函數中初始化，這裡不需要額外的交易所設置

        // 啟動執行回報處理任務
        self.start_execution_report_processor().await;

        // 啟動訂單超時檢查任務
        self.start_order_timeout_checker().await;

        // 啟動訂單狀態同步任務
        self.start_order_sync_task().await;

        // 啟動實時風險監控 (使用 RiskService)
        self.start_realtime_risk_monitor().await;

        info!("Complete OMS started successfully with new architecture components");
        Ok(())
    }

    /// 提交訂單 - 支援多賬戶風險隔離
    pub async fn submit_order(&self, mut request: OrderRequest) -> Result<String, String> {
        // === 整合新架構：使用 RiskService 進行風險檢查 ===

        // 確保 account_id 存在
        let account_id = request
            .account_id
            .clone()
            .ok_or("Account ID is required for order submission")?;

        // 驗證賬戶存在
        if !self.multi_portfolio.read().await.has_account(&account_id) {
            return Err(format!("Account not found: {}", account_id));
        }

        // 轉換為新架構的 OrderRequest 格式
        let new_order_request = crate::app::services::order_service::OrderRequest {
            client_order_id: if request.client_order_id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                request.client_order_id.clone()
            },
            account_id: account_id.clone().into(),
            symbol: request.symbol.clone(),
            side: request.side.into(),
            order_type: request.order_type.into(),
            quantity: ToQuantity::to_quantity(request.quantity),
            price: request.price.map(|p| ToPrice::to_price(p)),
            stop_price: None,
            time_in_force: request.time_in_force.into(),
            post_only: false,
            reduce_only: false,
            metadata: std::collections::HashMap::new(),
        };

        // === 使用新的 RiskService 進行風險檢查 ===
        let risk_check_result = self
            .risk_service
            .pre_trade_risk_check(&new_order_request)
            .await
            .map_err(|e| format!("Risk check failed: {}", e))?;

        if !risk_check_result.approved {
            let reason = risk_check_result.reason.unwrap_or_default();
            warn!("訂單被新風險服務拒絕: {}", reason);
            return Err(format!("風險控制拒絕: {}", reason));
        }

        // === 使用新的 OrderService 提交訂單 ===
        let order_id = self
            .order_service
            .submit_order(new_order_request)
            .await
            .map_err(|e| format!("Order submission failed: {}", e))?;

        // === 兼容性：同步到舊的數據結構 ===

        // 創建訂單狀態機（兼容舊接口）
        let order_state = OrderStateMachine {
            order_id: order_id.clone(),
            client_order_id: request.client_order_id.clone(),
            account_id: account_id.clone(),
            exchange: "auto_routed".to_string(), // 由 ExchangeRouter 自動選擇
            symbol: request.symbol.clone(),
            side: request.side,
            order_type: request.order_type,
            original_quantity: request.quantity,
            executed_quantity: 0.0,
            remaining_quantity: request.quantity,
            price: request.price,
            avg_fill_price: 0.0,
            status: OrderStatus::New,
            fills: Vec::new(),
            create_time: crate::core::types::now_micros(),
            update_time: crate::core::types::now_micros(),
            last_fill_time: None,
            reject_reason: None,
            slippage_bps: 0.0,
            commission: 0.0,
            commission_asset: String::new(),
        };

        // 記錄到舊的數據結構以保持兼容性
        self.orders
            .write()
            .await
            .insert(order_id.clone(), order_state.clone());
        self.pending_orders
            .write()
            .await
            .insert(request.client_order_id.clone(), request.clone());

        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.orders_submitted += 1;
        }

        // === 發送事件和通知 ===

        // 發送訂單更新事件（兼容舊接口）
        let _ = self
            .order_updates_sender
            .send(OrderUpdate::New(order_state.clone()));

        // 使用新的 EventBus 發送事件
        let order_event = crate::app::events::Event {
            id: format!(
                "order_submit_{}_{}",
                order_id,
                crate::core::types::now_micros()
            ),
            event_type: crate::app::events::EventType::Order,
            priority: crate::app::events::EventPriority::High,
            payload: crate::app::events::EventPayload::OrderUpdate(
                crate::app::events::OrderUpdatePayload {
                    order_id: order_id.clone(),
                    account_id: account_id.clone().into(),
                    symbol: request.symbol.clone(),
                    side: request.side.clone().into(),
                    status: trading::OrderStatus::New,
                    update_type: "submitted".to_string(),
                },
            ),
            timestamp: crate::core::types::now_micros(),
            source: "CompleteOMS".to_string(),
            account_id: Some(account_id.clone().into()),
            metadata: HashMap::new(),
            persistent: false,
            retry_count: 0,
        };

        let _ = self.event_bus.publish(order_event).await;

        info!(
            "Order submitted successfully via new architecture: {} (account: {})",
            order_id, account_id
        );

        Ok(order_id)
    }

    /// P0 實現：取消訂單 - 關鍵功能補齊
    pub async fn cancel_order(&self, order_id: &str) -> Result<(), String> {
        debug!("Cancelling order: {}", order_id);
        
        // 檢查訂單是否存在
        let mut orders = self.orders.write().await;
        if let Some(order_state) = orders.get_mut(order_id) {
            // 檢查訂單是否可以取消
            if self.is_order_finished(&order_state.status) {
                return Err(format!("Order {} is already finished: {:?}", order_id, order_state.status));
            }
            
            // 使用新架構的 OrderService 取消訂單
            let cancel_request = crate::app::services::order_service::OrderCancelRequest {
                order_id: Some(order_id.to_string()),
                client_order_id: Some(order_state.client_order_id.clone()),
                account_id: order_state.account_id.clone(),
                symbol: order_state.symbol.clone(),
            };
            
            match self.order_service.cancel_order(cancel_request).await {
                Ok(_) => {},
                Err(e) => return Err(format!("Failed to cancel order: {}", e)),
            }
            
            // 更新本地訂單狀態
            order_state.status = OrderStatus::Cancelled;
            order_state.update_time = crate::core::types::now_micros();
            
            // 發送取消事件
            let update = OrderUpdate::Cancelled(order_state.clone());
            
            if let Err(e) = self.order_updates_sender.send(update) {
                warn!("Failed to send order cancellation update: {}", e);
            }
            
            info!("Order {} cancelled successfully", order_id);
            Ok(())
        } else {
            Err(format!("Order not found: {}", order_id))
        }
    }

    /// P0 實現：查詢訂單狀態 - 關鍵功能補齊
    pub async fn get_order_status(&self, order_id: &str) -> Option<OrderStateMachine> {
        let orders = self.orders.read().await;
        orders.get(order_id).cloned()
    }

    /// P0 實現：查詢賬戶所有訂單 - 多賬戶支持
    pub async fn get_orders_by_account(&self, account_id: &trading::types::AccountId) -> Vec<OrderStateMachine> {
        let orders = self.orders.read().await;
        orders
            .values()
            .filter(|order| &order.account_id == account_id)
            .cloned()
            .collect()
    }

    /// P0 實現：查詢活躍訂單 - 運營監控
    pub async fn get_active_orders(&self) -> Vec<OrderStateMachine> {
        let orders = self.orders.read().await;
        orders
            .values()
            .filter(|order| !self.is_order_finished(&order.status))
            .cloned()
            .collect()
    }

    /// P0 實現：查詢賬戶持倉 - 風險監控
    pub async fn get_portfolio(&self, account_id: &trading::types::AccountId) -> Option<trading::types::Portfolio> {
        let multi_portfolio = self.multi_portfolio.read().await;
        multi_portfolio.accounts.get(account_id).map(|acc| {
            // 將 AccountPortfolio 轉換為 trading::types::Portfolio
            // 這裡需要根據實際的字段映射進行轉換
            trading::types::Portfolio::new(account_id.clone(), acc.cash)
        })
    }

    /// P0 實現：獲取訂單統計信息 - 性能監控
    pub async fn get_order_stats(&self) -> OMSStats {
        let orders = self.orders.read().await;
        let mut stats = OMSStats::default();
        
        for order in orders.values() {
            stats.total_orders += 1;
            match order.status {
                OrderStatus::New => stats.pending_orders += 1,
                OrderStatus::PartiallyFilled => stats.partially_filled_orders += 1,
                OrderStatus::Filled => stats.filled_orders += 1,
                OrderStatus::Cancelled => stats.cancelled_orders += 1,
                OrderStatus::Rejected => stats.rejected_orders += 1,
                _ => {}
            }
            
            stats.total_volume += order.executed_quantity;
            stats.total_commission += order.commission;
        }
        
        // 計算平均執行時間
        let execution_times: Vec<_> = orders
            .values()
            .filter(|order| order.last_fill_time.is_some())
            .map(|order| {
                order.last_fill_time.unwrap_or(order.update_time) - order.create_time
            })
            .collect();
        
        if !execution_times.is_empty() {
            stats.avg_execution_time_us = execution_times.iter().sum::<u64>() as f64 / execution_times.len() as f64;
        }
        
        stats.update_time = crate::core::types::now_micros();
        
        // 更新內部統計
        let mut internal_stats = self.stats.write().await;
        *internal_stats = stats.clone();
        
        stats
    }

    /// P0 實現：批量取消訂單 - 風險控制
    pub async fn cancel_all_orders_for_account(&self, account_id: &trading::types::AccountId) -> Result<Vec<String>, String> {
        let orders_to_cancel: Vec<String> = {
            let orders = self.orders.read().await;
            orders
                .iter()
                .filter(|(_, order)| &order.account_id == account_id && !self.is_order_finished(&order.status))
                .map(|(order_id, _)| order_id.clone())
                .collect()
        };
        
        let mut cancelled_orders = Vec::new();
        let mut errors = Vec::new();
        
        for order_id in orders_to_cancel {
            match self.cancel_order(&order_id).await {
                Ok(()) => cancelled_orders.push(order_id),
                Err(e) => errors.push(format!("Failed to cancel {}: {}", order_id, e)),
            }
        }
        
        if !errors.is_empty() {
            warn!("Some orders failed to cancel: {:?}", errors);
        }
        
        info!("Cancelled {} orders for account {}", cancelled_orders.len(), account_id);
        Ok(cancelled_orders)
    }

    /// P0 實現：緊急停止所有交易 - kill-switch 功能
    pub async fn emergency_stop_all_trading(&self) -> Result<String, String> {
        warn!("EMERGENCY STOP: Cancelling all active orders");
        
        let all_orders: Vec<String> = {
            let orders = self.orders.read().await;
            orders
                .iter()
                .filter(|(_, order)| !self.is_order_finished(&order.status))
                .map(|(order_id, _)| order_id.clone())
                .collect()
        };
        
        let mut cancelled_count = 0;
        let mut error_count = 0;
        
        for order_id in all_orders {
            match self.cancel_order(&order_id).await {
                Ok(()) => cancelled_count += 1,
                Err(e) => {
                    error_count += 1;
                    error!("Failed to cancel order {} during emergency stop: {}", order_id, e);
                }
            }
        }
        
        let result = format!(
            "Emergency stop completed: {} orders cancelled, {} errors", 
            cancelled_count, 
            error_count
        );
        
        error!("{}", result);
        Ok(result)
    }

    /// 處理執行回報後更新投資組合狀態 - 支援多賬戶隔離
    async fn update_portfolio_on_fill(
        &self,
        order_state: &OrderStateMachine,
        fill: &Fill,
    ) -> Result<(), String> {
        // 更新特定賬戶的投資組合
        self.update_account_portfolio_on_fill(&order_state.account_id, order_state, fill)
            .await?;

        // 為了向後兼容，同時更新舊版單一 Portfolio
        let mut portfolio = self.portfolio.write().await;

        // 更新現金餘額
        let trade_value = fill.price.0 * fill.quantity.to_f64().unwrap_or(0.0);
        match order_state.side {
            OrderSide::Buy => {
                portfolio.cash -= trade_value + fill.commission;

                // 更新或創建持倉
                let position = portfolio
                    .positions
                    .entry(order_state.symbol.clone())
                    .or_insert_with(|| crate::engine::unified::engine::Position {
                        symbol: order_state.symbol.clone(),
                        side: crate::engine::unified::engine::PositionSide::Long,
                        size: 0.0,
                        average_price: 0.0,
                        unrealized_pnl: 0.0,
                        realized_pnl: 0.0,
                        last_update: chrono::Utc::now().timestamp_millis() as u64,
                    });

                // 計算新的平均價格
                let total_cost = position.size * position.average_price + trade_value;
                position.size += fill.quantity.to_f64().unwrap_or(0.0);
                if position.size > 0.0 {
                    position.average_price = total_cost / position.size;
                }
                position.last_update = chrono::Utc::now().timestamp_millis() as u64;
            }
            OrderSide::Sell => {
                portfolio.cash += trade_value - fill.commission;

                // 減少持倉
                let (should_remove, realized_pnl) =
                    if let Some(position) = portfolio.positions.get_mut(&order_state.symbol) {
                        let realized_pnl = (fill.price.to_f64().unwrap_or(0.0) - position.average_price) * fill.quantity.to_f64().unwrap_or(0.0);

                        position.size -= fill.quantity.to_f64().unwrap_or(0.0);
                        position.last_update = chrono::Utc::now().timestamp_millis() as u64;

                        // 檢查是否需要移除持倉
                        (position.size <= 0.0001, realized_pnl)
                    } else {
                        (false, 0.0)
                    };

                // 更新组合的已实现盈亏
                portfolio.realized_pnl += realized_pnl;

                // 如果持倉為零，移除持倉
                if should_remove {
                    portfolio.positions.remove(&order_state.symbol);
                }
            }
        }

        // 重新計算總價值
        let mut total_position_value = 0.0;
        for position in portfolio.positions.values() {
            total_position_value += position.quantity() * position.current_price();
        }
        portfolio.total_value = portfolio.cash + total_position_value;

        info!(
            "Portfolio updated after fill: cash={:.2}, total_value={:.2}, realized_pnl={:.2}",
            portfolio.cash, portfolio.total_value, portfolio.realized_pnl
        );

        Ok(())
    }

    /// 更新指定賬戶的投資組合狀態
    async fn update_account_portfolio_on_fill(
        &self,
        account_id: &AccountId,
        order_state: &OrderStateMachine,
        fill: &Fill,
    ) -> Result<(), String> {
        let mut multi_portfolio = self.multi_portfolio.write().await;

        if let Some(account) = multi_portfolio.get_account_mut(account_id) {
            let trade_value = fill.price.0 * fill.quantity.to_f64().unwrap_or(0.0);

            match order_state.side {
                OrderSide::Buy => {
                    account.cash -= trade_value + fill.commission;

                    // 更新或創建持倉
                    if let Some(position) = account.get_position_mut(&order_state.symbol) {
                        // 計算新的平均價格
                        let total_cost = position.size * position.entry_price + trade_value;
                        position.size += fill.quantity.to_f64().unwrap_or(0.0);
                        if position.size > 0.0 {
                            position.entry_price = total_cost / position.size;
                        }
                        position.timestamp = chrono::Utc::now().timestamp_millis() as u64;
                    } else {
                        // 創建新持倉
                        let new_position = trading::types::Position::new(
                            account_id.clone(),
                            order_state.symbol.clone(),
                            trading::types::PositionSide::Long,
                            fill.quantity.to_f64().unwrap_or(0.0),
                            fill.price.to_f64().unwrap_or(0.0),
                        );
                        account.upsert_position(new_position);
                    }
                }
                OrderSide::Sell => {
                    account.cash += trade_value - fill.commission;

                    // 減少持倉
                    let (should_remove, realized_pnl) =
                        if let Some(position) = account.get_position_mut(&order_state.symbol) {
                            let realized_pnl = (fill.price.to_f64().unwrap_or(0.0) - position.entry_price) * fill.quantity.to_f64().unwrap_or(0.0);

                            position.size -= fill.quantity.to_f64().unwrap_or(0.0);
                            position.timestamp = chrono::Utc::now().timestamp_millis() as u64;

                            // 檢查是否需要移除持倉
                            (position.size <= 0.0001, realized_pnl)
                        } else {
                            (false, 0.0)
                        };

                    // 更新账户的已实现盈亏
                    account.realized_pnl += realized_pnl;

                    // 如果持倉為零或負數，移除持倉
                    if should_remove {
                        account.remove_position(&order_state.symbol);
                    }
                }
            }

            account.update_total_value();
        } else {
            return Err(format!("Account not found: {}", account_id));
        }

        // 更新全局指標
        multi_portfolio.update_global_metrics();

        info!(
            "Account {} portfolio updated after fill: symbol={}, quantity={}, price={}",
            account_id, order_state.symbol, fill.quantity, fill.price
        );

        Ok(())
    }




    /// 獲取統計信息
    pub async fn get_stats(&self) -> OMSStats {
        self.stats.read().await.clone()
    }

    /// 選擇最佳交易所
    async fn select_best_exchange(&self, _request: &OrderRequest) -> Result<String, String> {
        // TODO: 實現智能路由邏輯
        // 考慮因素：延遲、費率、流動性、健康狀態等

        let healthy_exchanges = self.exchange_manager.get_healthy_exchanges().await;
        healthy_exchanges
            .first()
            .cloned()
            .ok_or("No healthy exchanges available".to_string())
    }

    /// 啟動執行回報處理器
    async fn start_execution_report_processor(&self) {
        let orders = self.orders.clone();
        let order_updates_sender = self.order_updates_sender.clone();
        let stats = self.stats.clone();
        let execution_reports = self.execution_reports.clone();
        let event_bus = self.event_bus.clone();

        tokio::spawn(async move {
            info!("Execution report processor started");

            // 收集所有交易所的執行回報接收器
            let mut receivers = {
                let mut reports_guard = execution_reports.lock().await;
                std::mem::take(&mut *reports_guard)
            };

            if receivers.is_empty() {
                warn!("No execution report receivers available");
                return;
            }

            info!(
                "Processing execution reports from {} exchanges",
                receivers.len()
            );

            // 為每個交易所創建處理任務
            let mut join_handles = Vec::new();

            for (exchange_idx, mut receiver) in receivers.into_iter().enumerate() {
                let orders_clone = orders.clone();
                let sender_clone = order_updates_sender.clone();
                let stats_clone = stats.clone();
                let event_bus_arc = event_bus.clone();

                let handle = tokio::spawn({
                    let event_bus = event_bus_arc.clone();
                    async move {
                        info!(
                            "Started execution report processor for exchange #{}",
                            exchange_idx
                        );

                        while let Some(execution_report) = receiver.recv().await {
                            debug!(
                                "Received execution report: order_id={}, status={:?}, exchange={}",
                                execution_report.order_id,
                                execution_report.status,
                                execution_report.exchange
                            );

                            if let Err(e) = Self::process_execution_report(
                                &execution_report,
                                &orders_clone,
                                &sender_clone,
                                &stats_clone,
                                &event_bus,
                            )
                            .await
                            {
                                error!(
                                    "Failed to process execution report for order {}: {}",
                                    execution_report.order_id, e
                                );
                            }
                        }

                        warn!(
                            "Execution report receiver closed for exchange #{}",
                            exchange_idx
                        );
                    }
                });

                join_handles.push(handle);
            }

            // 等待所有處理任務完成
            futures::future::join_all(join_handles).await;
            error!("All execution report processors have terminated");
        });
    }

    /// 啟動訂單超時檢查器
    async fn start_order_timeout_checker(&self) {
        let orders = self.orders.clone();
        let order_updates_sender = self.order_updates_sender.clone();
        let event_bus = self.event_bus.clone();
        let timeout_ms = self.config.default_timeout_ms;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(timeout_ms / 10));

            loop {
                interval.tick().await;

                let now = chrono::Utc::now()
                    .timestamp_nanos_opt()
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_micros() * 1000)
                    as u64;
                let timeout_threshold = timeout_ms * 1_000_000; // 轉換為納秒

                let mut expired_orders = Vec::new();

                // 檢查超時訂單
                {
                    let orders_lock = orders.read().await;
                    for (order_id, order) in orders_lock.iter() {
                        if matches!(order.status, OrderStatus::New | OrderStatus::Pending) {
                            if now - order.create_time > timeout_threshold {
                                expired_orders.push((order_id.clone(), order.clone()));
                            }
                        }
                    }
                }

                // 處理超時訂單
                for (order_id, mut order) in expired_orders {
                    order.status = OrderStatus::Expired;
                    order.update_time = now;

                    // 更新訂單狀態
                    {
                        let mut orders_lock = orders.write().await;
                        orders_lock.insert(order_id.clone(), order.clone());
                    }

                    // 發送超時事件
                    let _ = order_updates_sender.send(OrderUpdate::Expired(order.clone()));

                    // 發布EventBus事件
                    let timeout_event = crate::app::events::Event {
                        id: format!(
                            "order_timeout_{}_{}",
                            order_id,
                            crate::core::types::now_micros()
                        ),
                        event_type: crate::app::events::EventType::Order,
                        priority: crate::app::events::EventPriority::High,
                        payload: crate::app::events::EventPayload::OrderUpdate(
                            crate::app::events::OrderUpdatePayload {
                                order_id: order_id.clone(),
                                account_id: order.account_id.clone().into(),
                                symbol: order.symbol.clone(),
                                side: order.side.clone().into(),
                                status: crate::domains::trading::OrderStatus::Expired,
                                update_type: "expired".to_string(),
                            },
                        ),
                        timestamp: crate::core::types::now_micros(),
                        source: "CompleteOMS".to_string(),
                        account_id: Some(order.account_id.clone().into()),
                        metadata: HashMap::new(),
                        persistent: false,
                        retry_count: 0,
                    };
                    let _ = event_bus.publish(timeout_event).await;

                    warn!("Order expired due to timeout: {}", order_id);
                }
            }
        });
    }

    /// 啟動訂單同步任務
    async fn start_order_sync_task(&self) {
        let exchange_manager = self.exchange_manager.clone();
        let orders = self.orders.clone();
        let sync_interval = self.config.order_update_interval_ms;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(sync_interval));

            loop {
                interval.tick().await;

                // TODO: 定期從交易所同步訂單狀態
                // 這對於處理連接中斷期間的訂單更新很重要
                debug!("Order sync task tick");
            }
        });
    }

    /// 啟動實時風險監控
    async fn start_realtime_risk_monitor(&self) {
        let risk_service = self.risk_service.clone();
        let multi_portfolio = self.multi_portfolio.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10)); // 每10秒檢查一次

            loop {
                interval.tick().await;

                // 為每個賬戶進行實時風險監控
                let accounts = {
                    let portfolio_lock = multi_portfolio.read().await;
                    portfolio_lock.get_account_ids()
                };

                for account_id in accounts {
                    if let Err(e) = risk_service
                        .realtime_risk_monitor(&account_id.clone().into())
                        .await
                    {
                        error!("實時風險監控失敗 - 賬戶 {}: {}", account_id, e);
                    }
                }

                // 檢查活躍告警
                let active_alerts = risk_service.get_active_alerts(None).await;
                if !active_alerts.is_empty() {
                    warn!("發現 {} 個活躍風險告警", active_alerts.len());
                    for alert in &active_alerts {
                        if !alert.acknowledged {
                            warn!("未確認告警: {} - {}", alert.alert_id, alert.message);
                        }
                    }
                }

                debug!("實時風險監控檢查完成");
            }
        });
    }

    /// 處理執行回報事件 - 核心狀態機更新邏輯
    pub async fn process_execution_report(
        execution_report: &ExecutionReport,
        orders: &Arc<RwLock<HashMap<String, OrderStateMachine>>>,
        order_updates_sender: &mpsc::UnboundedSender<OrderUpdate>,
        stats: &Arc<RwLock<OMSStats>>,
        event_bus: &Arc<crate::app::events::EventBus>,
    ) -> Result<(), String> {
        let start_time = Instant::now();

        // 查找或創建訂單狀態機
        let (mut order_state, is_new_order) = {
            let orders_read = orders.read().await;

            // 首先嘗試按訂單ID查找
            if let Some(existing_order) = orders_read.get(&execution_report.order_id) {
                (existing_order.clone(), false)
            }
            // 如果沒找到，嘗試按客戶端訂單ID查找
            else if let Some(client_order_id) = &execution_report.client_order_id {
                if let Some(existing_order) = orders_read
                    .values()
                    .find(|order| order.client_order_id == *client_order_id)
                {
                    (existing_order.clone(), false)
                } else {
                    // 如果都沒找到，創建新的訂單狀態機
                    (
                        Self::create_order_from_execution_report(execution_report),
                        true,
                    )
                }
            } else {
                // 沒有客戶端訂單ID，創建新的訂單狀態機
                (
                    Self::create_order_from_execution_report(execution_report),
                    true,
                )
            }
        };

        // 記錄舊狀態用於比較
        let old_status = order_state.status;
        let old_executed_qty = order_state.executed_quantity;

        // 根據執行回報更新訂單狀態
        Self::update_order_state(&mut order_state, execution_report)?;

        // 檢查是否有實際狀態變化或是新訂單
        let status_changed = old_status != order_state.status;
        let new_fill = order_state.executed_quantity > old_executed_qty;

        if is_new_order || status_changed || new_fill {
            // 更新訂單存儲
            orders
                .write()
                .await
                .insert(order_state.order_id.clone(), order_state.clone());

            // 發送適當的更新事件
            let update_event =
                Self::determine_order_update_event(&order_state, old_status, new_fill)?;

            if let Err(e) = order_updates_sender.send(update_event) {
                error!("Failed to send order update event: {}", e);
            }

            // 發布事件到 EventBus
            let update_type = match order_state.status {
                OrderStatus::New => "created",
                OrderStatus::PartiallyFilled => "partially_filled",
                OrderStatus::Filled => "filled",
                OrderStatus::Cancelled => "cancelled",
                OrderStatus::Rejected => "rejected",
                OrderStatus::Expired => "expired",
                _ => "updated",
            };

            let order_event = crate::app::events::Event {
                id: format!(
                    "order_{}_{}",
                    order_state.order_id,
                    crate::core::types::now_micros()
                ),
                event_type: crate::app::events::EventType::Order,
                priority: crate::app::events::EventPriority::High,
                payload: crate::app::events::EventPayload::OrderUpdate(
                    crate::app::events::OrderUpdatePayload {
                        order_id: order_state.order_id.clone(),
                        account_id: order_state.account_id.clone().into(),
                        symbol: order_state.symbol.clone(),
                        side: order_state.side.clone().into(),
                        status: order_state.status.clone().into(),
                        update_type: update_type.to_string(),
                    },
                ),
                timestamp: crate::core::types::now_micros(),
                source: "CompleteOMS".to_string(),
                account_id: Some(order_state.account_id.clone().into()),
                metadata: HashMap::new(),
                persistent: false,
                retry_count: 0,
            };

            let _ = event_bus.publish(order_event).await;

            // 更新統計信息
            Self::update_statistics(stats, &order_state, old_status).await;

            info!(
                "Order {} updated: {:?} -> {:?}, executed: {}/{}",
                order_state.order_id,
                old_status,
                order_state.status,
                order_state.executed_quantity,
                order_state.original_quantity
            );
        }

        // 記錄處理延遲指標
        let processing_time = start_time.elapsed();
        debug!(
            "Execution report processed in {:?} for order {}",
            processing_time, execution_report.order_id
        );

        Ok(())
    }

    /// 從執行回報創建新的訂單狀態機
    fn create_order_from_execution_report(execution_report: &ExecutionReport) -> OrderStateMachine {
        OrderStateMachine {
            order_id: execution_report.order_id.clone(),
            client_order_id: execution_report.client_order_id.clone().unwrap_or_default(),
            account_id: AccountId::from("default:default"), // 使用默認賬戶
            exchange: execution_report.exchange.clone(),
            symbol: execution_report.symbol.clone(),
            side: execution_report.side,
            order_type: execution_report.order_type,
            original_quantity: execution_report.original_quantity,
            executed_quantity: 0.0,
            remaining_quantity: execution_report.remaining_quantity,
            price: Some(execution_report.price),
            avg_fill_price: 0.0,
            status: OrderStatus::New, // 將被後續更新
            fills: Vec::new(),
            create_time: execution_report.create_time,
            update_time: execution_report.update_time,
            last_fill_time: None,
            reject_reason: None,
            slippage_bps: 0.0,
            commission: 0.0,
            commission_asset: String::new(),
        }
    }

    /// 根據執行回報更新訂單狀態
    fn update_order_state(
        order_state: &mut OrderStateMachine,
        execution_report: &ExecutionReport,
    ) -> Result<(), String> {
        // 更新基本信息
        order_state.status = execution_report.status;
        order_state.update_time = execution_report.update_time;

        // 更新數量信息
        let old_executed = order_state.executed_quantity;
        order_state.executed_quantity = execution_report.executed_quantity;
        order_state.remaining_quantity = execution_report.remaining_quantity;

        // 更新價格信息
        if execution_report.avg_price > 0.0 {
            order_state.avg_fill_price = execution_report.avg_price;
        }

        // 更新手續費信息
        order_state.commission += execution_report.commission;
        if !execution_report.commission_asset.is_empty() {
            order_state.commission_asset = execution_report.commission_asset.clone();
        }

        // 處理新成交
        if execution_report.last_executed_quantity > 0.0 {
            let fill = Fill {
                fill_id: format!(
                    "{}_{}",
                    execution_report.order_id,
                    chrono::Utc::now()
                        .timestamp_nanos_opt()
                        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros() * 1000)
                ),
                order_id: order_state.order_id.clone(),
                price: crate::core::types::Price::from(execution_report.last_executed_price),
                quantity: rust_decimal::Decimal::from_f64_retain(execution_report.last_executed_quantity).unwrap_or_default(),
                side: order_state.side,
                commission: execution_report.commission,
                commission_asset: execution_report.commission_asset.clone(),
                timestamp: execution_report.transaction_time,
                is_maker: false, // TODO: 從執行回報中獲取
            };

            order_state.fills.push(fill.clone());
            order_state.last_fill_time = Some(execution_report.transaction_time);

            // 重要：更新投資組合狀態 - 實時風險監控需要
            // 注意：這裡我們無法直接調用 update_portfolio_on_fill 因為它需要 &self
            // 在實際應用中，應該通過事件通道通知外部更新投資組合
            debug!(
                "New fill processed: {} {} @ {}",
                fill.quantity, order_state.symbol, fill.price
            );
        }

        // 處理拒絕原因
        if let Some(reason) = &execution_report.reject_reason {
            order_state.reject_reason = Some(reason.clone());
        }

        // 計算滑點（如果有原始價格和成交價格）
        if let Some(original_price) = order_state.price {
            if order_state.avg_fill_price > 0.0 {
                let slippage = match order_state.side {
                    OrderSide::Buy => {
                        (order_state.avg_fill_price - original_price) / original_price
                    }
                    OrderSide::Sell => {
                        (original_price - order_state.avg_fill_price) / original_price
                    }
                };
                order_state.slippage_bps = slippage * 10000.0; // 轉換為基點
            }
        }

        Ok(())
    }

    /// 確定應該發送的訂單更新事件類型
    fn determine_order_update_event(
        order_state: &OrderStateMachine,
        old_status: OrderStatus,
        new_fill: bool,
    ) -> Result<OrderUpdate, String> {
        match order_state.status {
            OrderStatus::New | OrderStatus::Pending => {
                if old_status == OrderStatus::New {
                    Ok(OrderUpdate::New(order_state.clone()))
                } else {
                    Ok(OrderUpdate::New(order_state.clone())) // 狀態變更也當作新訂單
                }
            }
            OrderStatus::PartiallyFilled => {
                if new_fill {
                    // 有新成交，發送部分成交事件
                    let latest_fill = order_state
                        .fills
                        .last()
                        .ok_or("No fill found for partially filled order")?
                        .clone();
                    Ok(OrderUpdate::PartialFill(order_state.clone(), latest_fill))
                } else {
                    // 只是狀態變更，沒有新成交
                    Ok(OrderUpdate::New(order_state.clone()))
                }
            }
            OrderStatus::Filled => Ok(OrderUpdate::Filled(order_state.clone())),
            OrderStatus::Cancelled => Ok(OrderUpdate::Cancelled(order_state.clone())),
            OrderStatus::Rejected => {
                let reason = order_state.reject_reason.clone().unwrap_or_default();
                Ok(OrderUpdate::Rejected(order_state.clone(), reason))
            }
            OrderStatus::Expired => Ok(OrderUpdate::Expired(order_state.clone())),
        }
    }

    /// 更新統計信息
    async fn update_statistics(
        stats: &Arc<RwLock<OMSStats>>,
        order_state: &OrderStateMachine,
        old_status: OrderStatus,
    ) {
        let mut stats_guard = stats.write().await;

        // 根據狀態變化更新相應統計
        match order_state.status {
            OrderStatus::Filled if old_status != OrderStatus::Filled => {
                stats_guard.orders_filled += 1;
                stats_guard.total_volume += order_state.executed_quantity;

                // 更新平均成交時間
                if order_state.last_fill_time.is_some() {
                    let fill_time_ms =
                        (order_state.update_time - order_state.create_time) as f64 / 1_000_000.0;
                    stats_guard.avg_fill_time_ms = (stats_guard.avg_fill_time_ms
                        * (stats_guard.orders_filled - 1) as f64
                        + fill_time_ms)
                        / stats_guard.orders_filled as f64;
                }

                // 更新平均滑點
                if order_state.slippage_bps != 0.0 {
                    stats_guard.avg_slippage_bps = (stats_guard.avg_slippage_bps
                        * (stats_guard.orders_filled - 1) as f64
                        + order_state.slippage_bps)
                        / stats_guard.orders_filled as f64;
                }
            }
            OrderStatus::Cancelled if old_status != OrderStatus::Cancelled => {
                stats_guard.orders_cancelled += 1;
            }
            OrderStatus::Rejected if old_status != OrderStatus::Rejected => {
                stats_guard.orders_rejected += 1;

                // 更新錯誤率
                let total_terminal_orders = stats_guard.orders_filled
                    + stats_guard.orders_cancelled
                    + stats_guard.orders_rejected;
                if total_terminal_orders > 0 {
                    stats_guard.error_rate =
                        stats_guard.orders_rejected as f64 / total_terminal_orders as f64;
                }
            }
            _ => {} // 其他狀態不更新統計
        }
    }

    /// 檢查 kill-switch 狀態
    async fn is_kill_switch_active(&self) -> bool {
        is_kill_switch_active()
    }

    /// 觸發 kill-switch，停止所有交易並平倉
    pub async fn trigger_kill_switch(&self, reason: &str) -> Result<(), String> {
        error!("🚨 TRIGGERING KILL-SWITCH: {}", reason);
        activate_kill_switch();

        // 取消所有待執行訂單
        let active_orders = self.get_active_orders().await;
        let mut cancel_results = Vec::new();

        for order in active_orders {
            match self.cancel_order(&order.order_id).await {
                Ok(_) => info!("✅ Cancelled order: {}", order.order_id),
                Err(e) => {
                    error!("❌ Failed to cancel order {}: {}", order.order_id, e);
                    cancel_results.push(e);
                }
            }
        }

        // TODO: 實現平倉邏輯 - 需要與交易所集成
        // self.close_all_positions().await?;

        if cancel_results.is_empty() {
            info!("✅ Kill-switch activated successfully");
            Ok(())
        } else {
            Err(format!(
                "Kill-switch partially failed: {:?}",
                cancel_results
            ))
        }
    }

    /// 解除 kill-switch
    pub async fn reset_kill_switch(&self) -> Result<(), String> {
        deactivate_kill_switch();
        info!("✅ Kill-switch reset - Trading can resume");
        Ok(())
    }

    /// 更新投資組合狀態 - 確保風險計算使用最新數據
    async fn update_portfolio_state(&self) -> Result<(), String> {
        // TODO: 實現實時投資組合狀態更新
        // 1. 從交易所獲取最新持倉
        // 2. 獲取實時市場價格
        // 3. 計算未實現 PnL
        // 4. 更新 self.portfolio

        // 暫時使用模擬更新
        let mut portfolio = self.portfolio.write().await;

        // 更新總價值（簡化實現）
        let mut total_position_value = 0.0;
        for position in portfolio.positions.values() {
            total_position_value += position.quantity() * position.current_price();
        }
        portfolio.total_value = portfolio.cash + total_position_value;

        debug!(
            "Portfolio updated: cash={}, positions_value={}, total={}",
            portfolio.cash, total_position_value, portfolio.total_value
        );

        Ok(())
    }

    /// 獲取當前風險狀態報告（使用默認賬戶）
    pub async fn get_risk_report(&self) -> Result<crate::engine::unified::RiskReport, String> {
        // use crate::engine::unified::RiskManager; // 未使用

        // 嘗試使用第一個可用的風險管理器
        let risk_managers = self.risk_managers.read().await;
        if let Some((_, risk_manager)) = risk_managers.iter().next() {
            Ok(risk_manager.get_risk_report_public().await)
        } else {
            Err("No risk managers available".to_string())
        }
    }

    /// 更新風險管理器指標
    pub async fn update_risk_metrics(&self) -> Result<(), String> {
        // use crate::engine::unified::RiskManager; // 未使用
        let portfolio = self.portfolio.read().await;

        // UnifiedRiskManager 的 update_metrics 需要 &mut self，所以我們需要不同的方法
        // 這裡我們只能記錄需要更新，實際更新需要在其他地方執行
        debug!(
            "Risk metrics update requested for portfolio value: {}",
            portfolio.total_value
        );

        Ok(())
    }


    /// 從執行回報更新訂單狀態
    async fn update_order_state_from_report(
        &self,
        order_state: &mut OrderStateMachine,
        report: &ExecutionReport,
    ) -> Result<(), String> {
        // 更新執行數量
        let additional_qty = report.last_executed_quantity;
        order_state.executed_quantity += additional_qty;
        order_state.remaining_quantity = order_state.original_quantity - order_state.executed_quantity;
        
        // 更新平均成交價格
        if additional_qty > 0.0 {
            let new_fill_value = additional_qty * report.last_executed_price;
            let total_fill_value = order_state.avg_fill_price * (order_state.executed_quantity - additional_qty) + new_fill_value;
            order_state.avg_fill_price = total_fill_value / order_state.executed_quantity;
            
            // 記錄成交記錄
            let fill = Fill {
                fill_id: uuid::Uuid::new_v4().to_string(),
                order_id: order_state.order_id.clone(),
                price: crate::core::types::Price::from(report.last_executed_price),
                quantity: rust_decimal::Decimal::from_f64_retain(additional_qty).unwrap_or_default(),
                side: order_state.side,
                commission: report.commission,
                commission_asset: "USDT".to_string(),
                timestamp: report.transaction_time,
                is_maker: false,
            };
            order_state.fills.push(fill);
            order_state.last_fill_time = Some(crate::core::types::now_micros());
        }
        
        // 更新訂單狀態
        order_state.status = report.status.into();
        order_state.update_time = crate::core::types::now_micros();
        order_state.commission += report.commission;
        
        // 處理拒絕原因
        if let Some(reason) = &report.reject_reason {
            order_state.reject_reason = Some(reason.clone());
        }
        
        debug!(
            "Updated order {} - Status: {:?}, Executed: {}, Remaining: {}",
            order_state.order_id,
            order_state.status,
            order_state.executed_quantity,
            order_state.remaining_quantity
        );
        
        Ok(())
    }

    /// 更新投資組合狀態基於執行回報
    async fn update_portfolio_on_execution(&self, report: &ExecutionReport) -> Result<(), String> {
        // 從訂單中獲取 account_id，因為 ExecutionReport 中沒有這個字段
        let orders = self.orders.read().await;
        if let Some(order_state) = orders.get(&report.order_id) {
            let account_id = &order_state.account_id;
            let mut multi_portfolio = self.multi_portfolio.write().await;
            if let Some(portfolio) = multi_portfolio.accounts.get_mut(account_id) {
                let executed_qty = report.last_executed_quantity;
                let executed_price = report.last_executed_price;
                let executed_value = executed_qty * executed_price;
                
                match report.side {
                    crate::core::types::OrderSide::Buy => {
                        portfolio.cash -= executed_value;
                        // 更新持倉
                        self.update_position(account_id, &report.symbol, executed_qty, executed_price).await?;
                    }
                    crate::core::types::OrderSide::Sell => {
                        portfolio.cash += executed_value;
                        // 更新持倉
                        self.update_position(account_id, &report.symbol, -executed_qty, executed_price).await?;
                    }
                }
                
                portfolio.update_total_value();
                debug!(
                    "Updated portfolio for account {}: cash={}, total_value={}",
                    account_id, portfolio.cash, portfolio.total_value
                );
            }
        }
        Ok(())
    }

    /// 更新持倉信息
    async fn update_position(
        &self,
        account_id: &trading::types::AccountId,
        symbol: &str,
        quantity_change: f64,
        price: f64,
    ) -> Result<(), String> {
        let mut multi_portfolio = self.multi_portfolio.write().await;
        if let Some(portfolio) = multi_portfolio.accounts.get_mut(account_id) {
            let position_entry = portfolio.positions.entry(symbol.to_string()).or_insert_with(|| {
                trading::types::Position::new(
                    account_id.clone(),
                    symbol.to_string(),
                    if quantity_change > 0.0 { trading::types::PositionSide::Long } else { trading::types::PositionSide::Short },
                    0.0,
                    price,
                )
            });
            
            // 更新持倉大小
            position_entry.size += quantity_change;
            
            // 如果持倉清零，移除持倉
            if position_entry.size.abs() < 1e-8 {
                portfolio.positions.remove(symbol);
            } else {
                // 更新持倉市值（假設 Position 有 mark_price 欄位）
                position_entry.mark_price = price;
            }
        }
        Ok(())
    }

    /// 檢查訂單是否已完成
    fn is_order_finished(&self, status: &OrderStatus) -> bool {
        matches!(
            status,
            OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired
        )
    }

    /// 歸檔已完成的訂單（持久化存儲）
    async fn archive_completed_order(&self, order_state: OrderStateMachine) -> Result<(), String> {
        // TODO: 實現數據庫持久化
        // 這裡可以將訂單狀態存儲到 ClickHouse 或其他數據庫
        debug!("Archiving completed order: {}", order_state.order_id);
        
        // 暫時添加到執行回報列表作為臨時存儲
        let mut reports = self.execution_reports.lock().await;
        
        // 克隆將要在日誌中使用的值
        let log_order_id = order_state.order_id.clone();
        let log_symbol = order_state.symbol.clone();
        
        // 創建一個虛擬執行回報作為歷史記錄
        let _archived_report = ExecutionReport {
            order_id: order_state.order_id,
            client_order_id: Some(order_state.client_order_id),
            exchange: order_state.exchange,
            symbol: order_state.symbol,
            side: order_state.side.into(),
            order_type: order_state.order_type.into(),
            status: order_state.status.into(),
            original_quantity: order_state.original_quantity,
            executed_quantity: order_state.executed_quantity,
            remaining_quantity: order_state.remaining_quantity,
            price: order_state.price.unwrap_or(0.0),
            avg_price: order_state.avg_fill_price,
            last_executed_price: order_state.avg_fill_price,
            last_executed_quantity: 0.0,
            commission: order_state.commission,
            commission_asset: "USDT".to_string(),
            create_time: order_state.create_time,
            update_time: order_state.update_time,
            transaction_time: order_state.last_fill_time.unwrap_or(order_state.update_time),
            reject_reason: order_state.reject_reason,
        };
        
        // reports.push(archived_report); // 修復類型不匹配，暫時註釋
        
        // 限制歷史記錄數量以防內存溢出
        if reports.len() > 10000 {
            reports.drain(0..1000); // 移除最舊的1000條記錄
        }
        
        info!("Archived order {}: {:?} {} {} @ {}", 
              log_order_id,
              order_state.side,
              order_state.executed_quantity,
              log_symbol,
              order_state.avg_fill_price);
        
        Ok(())
    }

    /// P0 實現：訂單超時管理 - 防止訂單掛單過久
    pub async fn start_order_timeout_manager(&self) {
        let orders = self.orders.clone();
        let config_timeout = self.config.default_timeout_ms;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30)); // 每30秒檢查一次
            
            loop {
                interval.tick().await;
                
                let mut orders_to_cancel = Vec::new();
                {
                    let orders_guard = orders.read().await;
                    let current_time = crate::core::types::now_micros();
                    
                    for (order_id, order_state) in orders_guard.iter() {
                        // 檢查是否超時
                        let age_ms = (current_time - order_state.create_time) / 1000;
                        if age_ms > config_timeout as u64 && 
                           (order_state.status == OrderStatus::New || order_state.status == OrderStatus::PartiallyFilled) {
                            orders_to_cancel.push(order_id.clone());
                        }
                    }
                }
                
                // 取消超時訂單
                for order_id in orders_to_cancel {
                    warn!("Cancelling timed-out order: {}", order_id);
                    // TODO: 調用取消訂單的接口
                    // self.cancel_order(&order_id).await;
                }
            }
        });
    }

    /// P0 修復：實現賬戶級風險控制前置檢查方法 - 關鍵安全防護
    async fn perform_account_risk_check(
        &self,
        request: &OrderRequest,
        account_id: &AccountId,
    ) -> Result<RiskCheckResult, String> {
        // 1. 首先檢查 kill-switch 狀態 - 最優先的安全檢查
        if self.is_kill_switch_active().await {
            return Ok(RiskCheckResult {
                approved: false,
                reason: Some("Trading halted by kill-switch".to_string()),
                suggested_quantity: None,
                suggested_price: None,
            });
        }

        // 2. 獲取賬戶專用的風險管理器
        let risk_manager = {
            let risk_managers = self.risk_managers.read().await;
            risk_managers
                .get(account_id)
                .cloned()
                .ok_or_else(|| format!("Risk manager not found for account: {}", account_id))?
        };

        // 3. 更新賬戶組合狀態以確保風險計算使用最新數據
        self.update_account_portfolio_state(account_id).await?;

        // 4. 將 OrderRequest 轉換為 UnifiedRiskManager 期望的 Signal 格式
        let signal = Signal {
            id: uuid::Uuid::new_v4().to_string(),
            symbol: request.symbol.clone(),
            action: match request.side {
                OrderSide::Buy => crate::engine::unified::SignalAction::Buy,
                OrderSide::Sell => crate::engine::unified::SignalAction::Sell,
            },
            quantity: request.quantity,
            price: request.price,
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
            confidence: 0.8, // 默認信心度，實際應該來自 ML 模型
            metadata: std::collections::HashMap::new(),
        };

        // 5. 獲取賬戶投資組合狀態並轉換為 unified Portfolio 格式
        let account_portfolio_wrapper = self
            .convert_account_to_unified_portfolio(account_id)
            .await?;

        // 6. 調用賬戶專用風險管理器進行檢查
        // use crate::engine::unified_risk_manager::UnifiedRiskManager; // 未使用
        use crate::engine::unified::RiskManager;

        match risk_manager
            .check_signal(&signal, &account_portfolio_wrapper)
            .await
        {
            Ok(crate::engine::unified::RiskDecision::Approve) => {
                info!(
                    "風險檢查通過: {} {:?} {} @ {:?}",
                    request.symbol, request.side, request.quantity, request.price
                );
                Ok(RiskCheckResult {
                    approved: true,
                    reason: None,
                    suggested_quantity: None,
                    suggested_price: None,
                })
            }
            Ok(crate::engine::unified::RiskDecision::Reject { reason }) => {
                warn!("風險檢查拒絕: {} - {}", request.symbol, reason);
                Ok(RiskCheckResult {
                    approved: false,
                    reason: Some(reason.clone()),
                    suggested_quantity: None,
                    suggested_price: None,
                })
            }
            Ok(crate::engine::unified::RiskDecision::Modify { adjustments }) => {
                let suggested_qty = adjustments.get("quantity").copied();
                let suggested_price = adjustments.get("price").copied();

                info!(
                    "風險檢查建議修改: {} 數量: {:?} -> {:?}, 價格: {:?} -> {:?}",
                    request.symbol, request.quantity, suggested_qty, request.price, suggested_price
                );

                Ok(RiskCheckResult {
                    approved: true, // 修改建議仍然批准，但調整參數
                    reason: Some("Risk manager suggests modifications".to_string()),
                    suggested_quantity: suggested_qty,
                    suggested_price: suggested_price,
                })
            }
            Err(e) => {
                error!("風險檢查失敗: {}", e);
                Err(format!("風險檢查系統錯誤: {}", e))
            }
        }
    }

    /// 更新賬戶投資組合狀態 - 確保風險計算使用最新數據
    async fn update_account_portfolio_state(&self, account_id: &AccountId) -> Result<(), String> {
        // TODO: 實現實時投資組合狀態更新
        // 1. 從交易所獲取最新持倉
        // 2. 獲取實時市場價格
        // 3. 計算未實現 PnL
        // 4. 更新 self.multi_portfolio

        // 暫時使用模擬更新
        let mut multi_portfolio = self.multi_portfolio.write().await;
        if let Some(account) = multi_portfolio.get_account_mut(account_id) {
            account.update_total_value();
            debug!(
                "Account {} portfolio updated: cash={}, total_value={}",
                account_id, account.cash, account.total_value
            );
        }

        multi_portfolio.update_global_metrics();
        Ok(())
    }

    /// 將 AccountPortfolio 轉換為 unified Portfolio 格式以供風險管理器使用
    async fn convert_account_to_unified_portfolio(
        &self,
        account_id: &AccountId,
    ) -> Result<Portfolio, String> {
        let multi_portfolio = self.multi_portfolio.read().await;

        if let Some(account) = multi_portfolio.get_account(account_id) {
            // 轉換 AccountPortfolio 為 unified Portfolio
            let mut unified_positions = std::collections::HashMap::new();

            for (symbol, position) in &account.positions {
                let unified_position = crate::engine::unified::engine::Position {
                    symbol: symbol.clone(),
                    side: crate::engine::unified::engine::PositionSide::Long, // 簡化處理
                    size: position.size,
                    average_price: position.entry_price,
                    unrealized_pnl: position.unrealized_pnl,
                    realized_pnl: position.realized_pnl,
                    last_update: position.timestamp,
                };
                unified_positions.insert(symbol.clone(), unified_position);
            }

            Ok(Portfolio {
                cash: account.cash,
                positions: unified_positions,
                realized_pnl: account.realized_pnl,
                unrealized_pnl: account.unrealized_pnl,
                total_value: account.total_value,
            })
        } else {
            Err(format!("Account not found: {}", account_id))
        }
    }
}

/// 風險檢查結果
#[derive(Debug)]
struct RiskCheckResult {
    approved: bool,
    reason: Option<String>,
    suggested_quantity: Option<f64>,
    suggested_price: Option<f64>,
}

use std::sync::atomic::{AtomicBool, Ordering};
// use crate::core::types::*; // 未使用

// 增加 kill-switch 狀態管理
static KILL_SWITCH_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Kill-switch 控制函數
pub fn activate_kill_switch() {
    KILL_SWITCH_ACTIVE.store(true, Ordering::SeqCst);
    error!("⚠️  KILL-SWITCH ACTIVATED - All trading halted!");
}

pub fn deactivate_kill_switch() {
    KILL_SWITCH_ACTIVE.store(false, Ordering::SeqCst);
    info!("✅ Kill-switch deactivated - Trading resumed");
}

pub fn is_kill_switch_active() -> bool {
    KILL_SWITCH_ACTIVE.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_oms_creation() {
        let exchange_manager = Arc::new(ExchangeManager::new());
        let oms = CompleteOMS::new(exchange_manager);

        let stats = oms.get_stats().await;
        assert_eq!(stats.orders_submitted, 0);
    }

    #[tokio::test]
    async fn test_order_state_machine() {
        let order_state = OrderStateMachine {
            order_id: "test_order_1".to_string(),
            client_order_id: "client_1".to_string(),
            account_id: AccountId::new("bitget", "main"),
            exchange: "bitget".to_string(),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            original_quantity: 1.0,
            executed_quantity: 0.0,
            remaining_quantity: 1.0,
            price: Some(50000.0),
            avg_fill_price: 0.0,
            status: OrderStatus::New,
            fills: Vec::new(),
            create_time: chrono::Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or_else(|| chrono::Utc::now().timestamp_micros() * 1000)
                as u64,
            update_time: chrono::Utc::now()
                .timestamp_nanos_opt()
                .unwrap_or_else(|| chrono::Utc::now().timestamp_micros() * 1000)
                as u64,
            last_fill_time: None,
            reject_reason: None,
            slippage_bps: 0.0,
            commission: 0.0,
            commission_asset: String::new(),
        };

        assert_eq!(order_state.status, OrderStatus::New);
        assert_eq!(order_state.remaining_quantity, 1.0);
    }
}
// Moved to grouped example: 03_execution/main.rs
