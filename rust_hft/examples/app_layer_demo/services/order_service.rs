/*!
 * 訂單服務 (Order Service)
 *
 * 專注於訂單生命週期管理，包括訂單狀態機、訂單路由和執行跟蹤。
 * 從 CompleteOMS 中分離出純粹的訂單管理職責。
 */

use anyhow::Result;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

use crate::app::routing::ExchangeRouter;
use crate::domains::trading::{
    AccountId, ExecutionReport, OrderSide, OrderStatus, OrderType, Price, Quantity, TimeInForce,
    Timestamp,
};

/// 訂單狀態機
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderStateMachine {
    /// 訂單ID
    pub order_id: String,

    /// 客戶端訂單ID
    pub client_order_id: String,

    /// 賬戶ID
    pub account_id: AccountId,

    /// 交易所
    pub exchange: String,

    /// 交易對
    pub symbol: String,

    /// 訂單方向
    pub side: OrderSide,

    /// 訂單類型
    pub order_type: OrderType,

    /// 原始數量
    pub original_quantity: Quantity,

    /// 已成交數量
    pub executed_quantity: Quantity,

    /// 剩餘數量
    pub remaining_quantity: Quantity,

    /// 訂單價格
    pub price: Option<Price>,

    /// 平均成交價格
    pub avg_fill_price: Option<Price>,

    /// 訂單狀態
    pub status: OrderStatus,

    /// 成交記錄
    pub fills: Vec<Fill>,

    /// 創建時間
    pub create_time: Timestamp,

    /// 更新時間
    pub update_time: Timestamp,

    /// 最後成交時間
    pub last_fill_time: Option<Timestamp>,

    /// 拒絕原因
    pub reject_reason: Option<String>,

    /// 滑點 (基點)
    pub slippage_bps: f64,

    /// 手續費
    pub commission: f64,

    /// 手續費資產
    pub commission_asset: String,

    /// 時效類型
    pub time_in_force: TimeInForce,
}

/// 成交記錄
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    /// 成交ID
    pub fill_id: String,

    /// 成交價格
    pub price: Price,

    /// 成交數量
    pub quantity: Quantity,

    /// 手續費
    pub commission: f64,

    /// 手續費資產
    pub commission_asset: String,

    /// 成交時間
    pub timestamp: Timestamp,

    /// 是否為 Maker
    pub is_maker: bool,
}

/// 訂單請求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRequest {
    /// 客戶端訂單ID
    pub client_order_id: String,

    /// 賬戶ID
    pub account_id: AccountId,

    /// 交易對
    pub symbol: String,

    /// 訂單方向
    pub side: OrderSide,

    /// 訂單類型
    pub order_type: OrderType,

    /// 數量
    pub quantity: Quantity,

    /// 價格 (限價單必填)
    pub price: Option<Price>,

    /// 止損價格
    pub stop_price: Option<Price>,

    /// 時效類型
    pub time_in_force: TimeInForce,

    /// 是否只做 Maker
    pub post_only: bool,

    /// 是否減倉
    pub reduce_only: bool,

    /// 附加元數據
    pub metadata: HashMap<String, String>,
}

/// 訂單取消請求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCancelRequest {
    /// 訂單ID
    pub order_id: Option<String>,

    /// 客戶端訂單ID
    pub client_order_id: Option<String>,

    /// 賬戶ID
    pub account_id: AccountId,

    /// 交易對
    pub symbol: String,
}

/// 訂單更新事件
#[derive(Debug, Clone)]
pub enum OrderUpdate {
    /// 新訂單
    New(OrderStateMachine),

    /// 部分成交
    PartialFill(OrderStateMachine, Fill),

    /// 完全成交
    Filled(OrderStateMachine),

    /// 已取消
    Cancelled(OrderStateMachine),

    /// 已拒絕
    Rejected(OrderStateMachine, String),

    /// 已過期
    Expired(OrderStateMachine),

    /// 錯誤
    Error(String, String), // order_id, error
}

/// 訂單服務配置
#[derive(Debug, Clone)]
pub struct OrderServiceConfig {
    /// 默認超時時間 (毫秒)
    pub default_timeout_ms: u64,

    /// 最大滑點 (基點)
    pub max_slippage_bps: f64,

    /// 啟用部分成交
    pub enable_partial_fills: bool,

    /// 斷線時自動取消
    pub auto_cancel_on_disconnect: bool,

    /// 訂單更新間隔 (毫秒)
    pub order_update_interval_ms: u64,

    /// 最大待處理訂單數
    pub max_pending_orders: usize,
}

impl Default for OrderServiceConfig {
    fn default() -> Self {
        Self {
            default_timeout_ms: 30000, // 30秒
            max_slippage_bps: 50.0,    // 50個基點
            enable_partial_fills: true,
            auto_cancel_on_disconnect: true,
            order_update_interval_ms: 1000, // 1秒
            max_pending_orders: 1000,
        }
    }
}

/// 訂單統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrderStats {
    /// 已提交訂單數
    pub orders_submitted: u64,

    /// 已成交訂單數
    pub orders_filled: u64,

    /// 已取消訂單數
    pub orders_cancelled: u64,

    /// 已拒絕訂單數
    pub orders_rejected: u64,

    /// 總交易量
    pub total_volume: f64,

    /// 平均成交時間 (毫秒)
    pub avg_fill_time_ms: f64,

    /// 平均滑點 (基點)
    pub avg_slippage_bps: f64,

    /// 錯誤率
    pub error_rate: f64,
}

/// 訂單服務 - 專注於訂單生命週期管理
pub struct OrderService {
    /// 交易所路由器
    router: Arc<ExchangeRouter>,

    /// 活躍訂單映射 - 使用 DashMap 提升並發性能
    orders: Arc<DashMap<String, OrderStateMachine>>,

    /// 待處理訂單映射 - 使用 DashMap 提升並發性能
    pending_orders: Arc<DashMap<String, OrderRequest>>,

    /// 訂單更新發送器
    order_updates_sender: mpsc::UnboundedSender<OrderUpdate>,

    /// 配置
    config: OrderServiceConfig,

    /// 統計信息
    stats: Arc<RwLock<OrderStats>>,
}

impl OrderService {
    /// 創建新的訂單服務
    pub fn new(router: Arc<ExchangeRouter>, config: OrderServiceConfig) -> Self {
        let (order_updates_sender, _) = mpsc::unbounded_channel();

        Self {
            router,
            orders: Arc::new(DashMap::new()),
            pending_orders: Arc::new(DashMap::new()),
            order_updates_sender,
            config,
            stats: Arc::new(RwLock::new(OrderStats::default())),
        }
    }

    /// 創建訂單服務並返回事件接收器
    pub fn new_with_receiver(
        router: Arc<ExchangeRouter>,
        config: OrderServiceConfig,
    ) -> (Self, mpsc::UnboundedReceiver<OrderUpdate>) {
        let (order_updates_sender, order_updates_receiver) = mpsc::unbounded_channel();

        let service = Self {
            router,
            orders: Arc::new(DashMap::new()),
            pending_orders: Arc::new(DashMap::new()),
            order_updates_sender,
            config,
            stats: Arc::new(RwLock::new(OrderStats::default())),
        };

        (service, order_updates_receiver)
    }

    /// 提交訂單
    pub async fn submit_order(&self, request: OrderRequest) -> Result<String> {
        // 1. 基本驗證
        self.validate_order_request(&request).await?;

        // 2. 檢查待處理訂單限制 - DashMap 無鎖操作
        let pending_count = self.pending_orders.len();
        if pending_count >= self.config.max_pending_orders {
            return Err(anyhow::anyhow!(
                "Maximum pending orders limit reached: {}",
                self.config.max_pending_orders
            ));
        }

        // 3. 使用路由器選擇最佳交易所實例
        let routing_result = self
            .router
            .route_for_account(&request.account_id, &request.symbol)
            .await
            .map_err(|e| anyhow::anyhow!("Routing failed: {}", e))?;

        // 4. 生成訂單ID
        let order_id = self.generate_order_id().await;

        // 5. 創建訂單狀態機
        let order_state = self
            .create_order_state_machine(order_id.clone(), request.clone())
            .await;

        // 6. 添加到待處理訂單 - DashMap 無鎖插入
        self.pending_orders.insert(order_id.clone(), request.clone());

        // 7. 添加到活躍訂單 - DashMap 無鎖插入  
        self.orders.insert(order_id.clone(), order_state.clone());

        // 8. 發送訂單更新事件
        let _ = self
            .order_updates_sender
            .send(OrderUpdate::New(order_state));

        // 9. 更新統計
        self.stats.write().await.orders_submitted += 1;

        info!(
            "Order submitted: {} for account {} on {} ({})",
            order_id, request.account_id, routing_result.instance.config.name, request.symbol
        );

        Ok(order_id)
    }

    /// 取消訂單
    pub async fn cancel_order(&self, request: OrderCancelRequest) -> Result<bool> {
        // 1. 確定訂單ID
        let order_id = if let Some(id) = request.order_id {
            id
        } else if let Some(client_id) = request.client_order_id {
            self.find_order_by_client_id(&client_id)
                .await
                .ok_or_else(|| {
                    anyhow::anyhow!("Order not found by client_order_id: {}", client_id)
                })?
        } else {
            return Err(anyhow::anyhow!(
                "Either order_id or client_order_id must be provided"
            ));
        };

        // 2. 獲取訂單狀態 - DashMap 無鎖可變訪問
        let mut order_entry = self.orders.get_mut(&order_id)
            .ok_or_else(|| anyhow::anyhow!("Order not found: {}", order_id))?;
        let order = order_entry.value_mut();

        // 3. 檢查是否可以取消
        if !order.status.is_active() {
            return Err(anyhow::anyhow!(
                "Order {} is not active, current status: {:?}",
                order_id,
                order.status
            ));
        }

        // 4. 檢查賬戶匹配
        if order.account_id != request.account_id {
            return Err(anyhow::anyhow!("Account mismatch for order {}", order_id));
        }

        // 5. 更新訂單狀態
        order.status = OrderStatus::Cancelled;
        order.update_time = crate::core::types::now_micros();

        // 6. 從待處理訂單中移除 - DashMap 無鎖移除
        self.pending_orders.remove(&order_id);

        // 7. 發送取消事件
        let _ = self
            .order_updates_sender
            .send(OrderUpdate::Cancelled(order.clone()));

        // 8. 更新統計
        self.stats.write().await.orders_cancelled += 1;

        info!(
            "Order cancelled: {} for account {}",
            order_id, request.account_id
        );

        Ok(true)
    }

    /// 處理執行回報
    pub async fn handle_execution_report(&self, report: ExecutionReport) -> Result<()> {
        // DashMap 無鎖可變訪問，執行報告處理性能大幅提升  
        let mut order_entry = self.orders.get_mut(&report.order_id).ok_or_else(|| {
            anyhow::anyhow!("Order not found for execution report: {}", report.order_id)
        })?;
        let order = order_entry.value_mut();

        // 更新訂單狀態
        order.status = report.status;
        order.executed_quantity = report.executed_quantity;
        order.remaining_quantity = report.remaining_quantity;
        order.update_time = report.update_time;

        if let Some(avg_price) = report.average_price {
            order.avg_fill_price = Some(avg_price);
        }

        // 處理新成交
        if report.last_executed_quantity > crate::domains::trading::ToQuantity::to_quantity(0.0) {
            let fill = Fill {
                fill_id: format!("{}_{}", report.order_id, order.fills.len()),
                price: report.last_executed_price.unwrap_or_default(),
                quantity: report.last_executed_quantity,
                commission: report.commission,
                commission_asset: report.commission_asset.clone(),
                timestamp: report.transaction_time.unwrap_or(report.update_time),
                is_maker: false, // 需要從執行回報中獲取
            };

            order.fills.push(fill.clone());
            order.last_fill_time = Some(report.update_time);

            // 發送部分成交或完全成交事件
            let update_event = if order.status == OrderStatus::Filled {
                self.stats.write().await.orders_filled += 1;
                OrderUpdate::Filled(order.clone())
            } else {
                OrderUpdate::PartialFill(order.clone(), fill)
            };

            let _ = self.order_updates_sender.send(update_event);
        }

        // 如果訂單完成，從待處理訂單中移除 - DashMap 無鎖操作
        if order.status.is_finished() {
            self.pending_orders.remove(&report.order_id);
        }

        Ok(())
    }

    /// 獲取訂單狀態
    pub async fn get_order(&self, order_id: &str) -> Option<OrderStateMachine> {
        // DashMap 無鎖讀取，性能大幅提升
        self.orders.get(order_id).map(|entry| entry.value().clone())
    }

    /// 獲取賬戶的所有活躍訂單
    pub async fn get_active_orders_for_account(
        &self,
        account_id: &AccountId,
    ) -> Vec<OrderStateMachine> {
        // DashMap 並行迭代，無鎖查詢特定賬戶活躍訂單
        self.orders
            .iter()
            .filter(|entry| &entry.value().account_id == account_id && entry.value().status.is_active())
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// 獲取統計信息
    pub async fn get_stats(&self) -> OrderStats {
        self.stats.read().await.clone()
    }

    // ====================== 內部輔助方法 ======================

    /// 驗證訂單請求
    async fn validate_order_request(&self, request: &OrderRequest) -> Result<()> {
        // 基本字段驗證
        if request.symbol.is_empty() {
            return Err(anyhow::anyhow!("Symbol cannot be empty"));
        }

        if request.quantity <= crate::domains::trading::ToQuantity::to_quantity(0.0) {
            return Err(anyhow::anyhow!("Quantity must be positive"));
        }

        // 限價單必須有價格
        if matches!(request.order_type, OrderType::Limit | OrderType::PostOnly)
            && request.price.is_none()
        {
            return Err(anyhow::anyhow!("Price is required for limit orders"));
        }

        // 檢查交易對是否可用
        if !self
            .router
            .is_symbol_available(&request.account_id.exchange, &request.symbol)
            .await
        {
            return Err(anyhow::anyhow!(
                "Symbol {} is not available on {}",
                request.symbol,
                request.account_id.exchange
            ));
        }

        Ok(())
    }

    /// 生成訂單ID
    async fn generate_order_id(&self) -> String {
        let timestamp = crate::core::types::now_micros();
        let order_count = self.stats.read().await.orders_submitted;
        format!("ORD_{}__{}", timestamp, order_count)
    }

    /// 根據客戶端訂單ID查找訂單
    async fn find_order_by_client_id(&self, client_order_id: &str) -> Option<String> {
        // DashMap 並行迭代，性能優於單一鎖
        for entry in self.orders.iter() {
            if entry.value().client_order_id == client_order_id {
                return Some(entry.key().clone());
            }
        }
        None
    }

    /// 創建訂單狀態機
    async fn create_order_state_machine(
        &self,
        order_id: String,
        request: OrderRequest,
    ) -> OrderStateMachine {
        let now = crate::core::types::now_micros();

        OrderStateMachine {
            order_id,
            client_order_id: request.client_order_id,
            account_id: request.account_id.clone(),
            exchange: request.account_id.exchange.clone(),
            symbol: request.symbol,
            side: request.side,
            order_type: request.order_type,
            original_quantity: request.quantity,
            executed_quantity: crate::domains::trading::ToQuantity::to_quantity(0.0),
            remaining_quantity: request.quantity,
            price: request.price,
            avg_fill_price: None,
            status: OrderStatus::New,
            fills: Vec::new(),
            create_time: now,
            update_time: now,
            last_fill_time: None,
            reject_reason: None,
            slippage_bps: 0.0,
            commission: 0.0,
            commission_asset: "USDT".to_string(),
            time_in_force: request.time_in_force,
        }
    }
}

// ====================== 訂單狀態擴展 ======================

impl OrderStatus {
    /// 檢查訂單是否為活躍狀態
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            OrderStatus::New | OrderStatus::PartiallyFilled | OrderStatus::Pending
        )
    }

    /// 檢查訂單是否已完成
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::routing::{ExchangeRouter, RouterConfig};
    use crate::exchanges::ExchangeManager;

    #[tokio::test]
    async fn test_order_service_creation() {
        let exchange_manager = Arc::new(ExchangeManager::new());
        let router = Arc::new(ExchangeRouter::with_default_config(exchange_manager));
        let config = OrderServiceConfig::default();

        let order_service = OrderService::new(router, config);
        let stats = order_service.get_stats().await;

        assert_eq!(stats.orders_submitted, 0);
        assert_eq!(stats.orders_filled, 0);
    }

    #[tokio::test]
    async fn test_order_validation() {
        let exchange_manager = Arc::new(ExchangeManager::new());
        let router = Arc::new(ExchangeRouter::with_default_config(exchange_manager));
        let config = OrderServiceConfig::default();
        let order_service = OrderService::new(router, config);

        // 測試空交易對
        let invalid_request = OrderRequest {
            client_order_id: "test_001".to_string(),
            account_id: AccountId::new("bitget", "main"),
            symbol: "".to_string(),
            side: OrderSide::Buy,
            order_type: OrderType::Market,
            quantity: crate::domains::trading::ToQuantity::to_quantity(100.0),
            price: None,
            stop_price: None,
            time_in_force: TimeInForce::GTC,
            post_only: false,
            reduce_only: false,
            metadata: HashMap::new(),
        };

        let result = order_service.validate_order_request(&invalid_request).await;
        assert!(result.is_err());
    }
}
