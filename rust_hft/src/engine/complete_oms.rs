//! 完整的訂單管理系統(OMS)
//! 
//! 包含訂單狀態機、智能路由、風險控制和執行優化

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error, debug};
use serde::{Serialize, Deserialize};

use crate::core::types::*;
use crate::exchanges::{Exchange, ExchangeManager, MarketEvent, ExecutionReport, OrderRequest, OrderResponse, CancelRequest};
// P0 修復：集成風險控制到 OMS
use crate::engine::unified_risk_manager::{UnifiedRiskManager, RiskManagerConfig};
use crate::engine::unified::{RiskDecision, Signal, Order as RiskOrder, Portfolio};

/// 訂單狀態機
#[derive(Debug, Clone)]
pub struct OrderStateMachine {
    pub order_id: String,
    pub client_order_id: String,
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
    pub price: f64,
    pub quantity: f64,
    pub commission: f64,
    pub commission_asset: String,
    pub timestamp: u64,
    pub is_maker: bool,
}

/// 完整OMS系統
pub struct CompleteOMS {
    exchange_manager: Arc<ExchangeManager>,
    orders: Arc<RwLock<HashMap<String, OrderStateMachine>>>,
    pending_orders: Arc<RwLock<HashMap<String, OrderRequest>>>,
    
    // P0 修復：集成風險管理器
    risk_manager: Arc<UnifiedRiskManager>,
    portfolio: Arc<RwLock<Portfolio>>,
    
    // 事件通道
    order_updates_sender: mpsc::UnboundedSender<OrderUpdate>,
    execution_reports: Arc<Mutex<Vec<mpsc::UnboundedReceiver<ExecutionReport>>>>,
    
    // 配置
    config: OMSConfig,
    
    // 性能統計
    stats: Arc<RwLock<OMSStats>>,
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
            max_slippage_bps: 50.0,     // 50個基點
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
    pub total_volume: f64,
    pub avg_fill_time_ms: f64,
    pub avg_slippage_bps: f64,
    pub error_rate: f64,
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
        
        Self {
            exchange_manager,
            orders: Arc::new(RwLock::new(HashMap::new())),
            pending_orders: Arc::new(RwLock::new(HashMap::new())),
            
            // P0 修復：初始化風險管理器，使用默認配置
            risk_manager: Arc::new(UnifiedRiskManager::new(RiskManagerConfig::default())),
            portfolio: Arc::new(RwLock::new(Portfolio::default())),
            
            order_updates_sender,
            execution_reports: Arc::new(Mutex::new(Vec::new())),
            config: OMSConfig::default(),
            stats: Arc::new(RwLock::new(OMSStats::default())),
        }
    }

    pub fn with_config(exchange_manager: Arc<ExchangeManager>, config: OMSConfig) -> Self {
        let mut oms = Self::new(exchange_manager);
        oms.config = config;
        oms
    }

    /// 啟動OMS服務
    pub async fn start(&self) -> Result<mpsc::UnboundedReceiver<OrderUpdate>, String> {
        info!("Starting Complete OMS");
        
        // 獲取所有交易所的執行回報
        let exchanges = self.exchange_manager.list_exchanges().await;
        for exchange_name in exchanges {
            if let Some(exchange) = self.exchange_manager.get_exchange(&exchange_name).await {
                let exchange_lock = exchange.read().await;
                if let Ok(reports_rx) = exchange_lock.get_execution_reports().await {
                    self.execution_reports.lock().await.push(reports_rx);
                }
            }
        }
        
        // 啟動執行回報處理任務
        self.start_execution_report_processor().await;
        
        // 啟動訂單超時檢查任務
        self.start_order_timeout_checker().await;
        
        // 啟動訂單狀態同步任務
        self.start_order_sync_task().await;
        
        let (sender, receiver) = mpsc::unbounded_channel();
        
        info!("Complete OMS started successfully");
        Ok(receiver)
    }

    /// 提交訂單
    pub async fn submit_order(&self, mut request: OrderRequest) -> Result<String, String> {
        // P0 修復：風險控制前置檢查 - 關鍵安全防護
        let risk_check_result = self.perform_risk_check(&request).await?;
        if !risk_check_result.approved {
            let reason = risk_check_result.reason.unwrap_or_default();
            warn!("訂單被風險管理器拒絕: {}", reason);
            return Err(format!("風險控制拒絕: {}", reason));
        }
        
        // P0 修復：強制執行風險管理器的修改建議
        if let Some(suggested_qty) = risk_check_result.suggested_quantity {
            info!("風險管理器建議調整數量: {} -> {}", request.quantity, suggested_qty);
            request.quantity = suggested_qty;
        }
        if let Some(suggested_price) = risk_check_result.suggested_price {
            info!("風險管理器建議調整價格: {:?} -> {}", request.price, suggested_price);
            request.price = Some(suggested_price);
        }
        
        // 檢查待處理訂單數量限制
        if self.pending_orders.read().await.len() >= self.config.max_pending_orders {
            return Err("Maximum pending orders limit reached".to_string());
        }

        // 生成訂單ID
        if request.client_order_id.is_empty() {
            request.client_order_id = uuid::Uuid::new_v4().to_string();
        }

        // 選擇最佳交易所（如果啟用智能路由）
        let exchange_name = if self.config.enable_smart_routing {
            self.select_best_exchange(&request).await?
        } else {
            // 默認使用第一個健康的交易所
            let healthy_exchanges = self.exchange_manager.get_healthy_exchanges().await;
            healthy_exchanges.first()
                .ok_or("No healthy exchanges available")?
                .clone()
        };

        // 獲取交易所實例
        let exchange = self.exchange_manager.get_exchange(&exchange_name).await
            .ok_or("Exchange not found")?;

        // 創建訂單狀態機
        let order_state = OrderStateMachine {
            order_id: String::new(), // 將在收到響應後填入
            client_order_id: request.client_order_id.clone(),
            exchange: exchange_name.clone(),
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
            create_time: chrono::Utc::now().timestamp_nanos() as u64,
            update_time: chrono::Utc::now().timestamp_nanos() as u64,
            last_fill_time: None,
            reject_reason: None,
            slippage_bps: 0.0,
            commission: 0.0,
            commission_asset: String::new(),
        };

        // 記錄待處理訂單
        self.pending_orders.write().await.insert(request.client_order_id.clone(), request.clone());

        // 提交訂單到交易所
        let mut exchange_lock = exchange.write().await;
        let response = exchange_lock.place_order(request).await?;

        // 更新統計
        {
            let mut stats = self.stats.write().await;
            stats.orders_submitted += 1;
        }

        if response.success {
            // 更新訂單狀態
            let mut updated_order = order_state;
            updated_order.order_id = response.order_id.clone();
            updated_order.status = response.status;
            
            // 存儲訂單
            self.orders.write().await.insert(response.order_id.clone(), updated_order.clone());
            
            // 發送更新事件
            let _ = self.order_updates_sender.send(OrderUpdate::New(updated_order));
            
            // 從待處理列表中移除
            self.pending_orders.write().await.remove(&response.client_order_id);
            
            info!("Order submitted successfully: {} -> {}", response.client_order_id, response.order_id);
            Ok(response.order_id)
        } else {
            // 訂單被拒絕
            let mut rejected_order = order_state;
            rejected_order.status = OrderStatus::Rejected;
            rejected_order.reject_reason = response.error.clone();
            
            // 更新統計
            {
                let mut stats = self.stats.write().await;
                stats.orders_rejected += 1;
            }
            
            // 發送拒絕事件
            let reject_reason = response.error.unwrap_or_else(|| "Unknown error".to_string());
            let _ = self.order_updates_sender.send(OrderUpdate::Rejected(rejected_order, reject_reason.clone()));
            
            // 從待處理列表中移除
            self.pending_orders.write().await.remove(&response.client_order_id);
            
            Err(reject_reason)
        }
    }

    /// 取消訂單
    pub async fn cancel_order(&self, order_id: &str) -> Result<(), String> {
        let order = {
            self.orders.read().await.get(order_id).cloned()
        };

        if let Some(mut order) = order {
            let exchange = self.exchange_manager.get_exchange(&order.exchange).await
                .ok_or("Exchange not found")?;

            let cancel_request = CancelRequest {
                order_id: Some(order.order_id.clone()),
                client_order_id: Some(order.client_order_id.clone()),
                symbol: order.symbol.clone(),
            };

            let mut exchange_lock = exchange.write().await;
            let response = exchange_lock.cancel_order(cancel_request).await?;

            if response.success {
                // 更新訂單狀態
                order.status = OrderStatus::Cancelled;
                order.update_time = chrono::Utc::now().timestamp_nanos() as u64;
                
                // 存儲更新
                self.orders.write().await.insert(order_id.to_string(), order.clone());
                
                // 更新統計
                {
                    let mut stats = self.stats.write().await;
                    stats.orders_cancelled += 1;
                }
                
                // 發送取消事件
                let _ = self.order_updates_sender.send(OrderUpdate::Cancelled(order));
                
                info!("Order cancelled successfully: {}", order_id);
                Ok(())
            } else {
                let error_msg = response.error.unwrap_or_else(|| "Cancel failed".to_string());
                error!("Failed to cancel order {}: {}", order_id, error_msg);
                Err(error_msg)
            }
        } else {
            Err(format!("Order not found: {}", order_id))
        }
    }

    /// 查詢訂單狀態
    pub async fn get_order_status(&self, order_id: &str) -> Option<OrderStateMachine> {
        self.orders.read().await.get(order_id).cloned()
    }

    /// 獲取活躍訂單
    pub async fn get_active_orders(&self) -> Vec<OrderStateMachine> {
        self.orders.read().await
            .values()
            .filter(|order| matches!(order.status, OrderStatus::New | OrderStatus::PartiallyFilled | OrderStatus::Pending))
            .cloned()
            .collect()
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
        healthy_exchanges.first()
            .cloned()
            .ok_or("No healthy exchanges available".to_string())
    }

    /// 啟動執行回報處理器
    async fn start_execution_report_processor(&self) {
        let orders = self.orders.clone();
        let order_updates_sender = self.order_updates_sender.clone();
        let stats = self.stats.clone();
        let execution_reports = self.execution_reports.clone();

        tokio::spawn(async move {
            // TODO: 處理來自所有交易所的執行回報
            // 這裡需要對每個交易所的執行回報接收器進行監聽
            info!("Execution report processor started");
        });
    }

    /// 啟動訂單超時檢查器
    async fn start_order_timeout_checker(&self) {
        let orders = self.orders.clone();
        let order_updates_sender = self.order_updates_sender.clone();
        let timeout_ms = self.config.default_timeout_ms;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(timeout_ms / 10));
            
            loop {
                interval.tick().await;
                
                let now = chrono::Utc::now().timestamp_nanos() as u64;
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
                    let _ = order_updates_sender.send(OrderUpdate::Expired(order));
                    
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
    
    /// P0 修復：實現風險控制前置檢查方法 - 關鍵安全防護
    async fn perform_risk_check(&self, request: &OrderRequest) -> Result<RiskCheckResult, String> {
        // 將 OrderRequest 轉換為 UnifiedRiskManager 期望的 Signal 格式
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
        
        // 獲取當前投資組合狀態
        let portfolio = self.portfolio.read().await;
        
        // 調用統一風險管理器進行檢查
        use crate::engine::unified::RiskManager;
        match self.risk_manager.check_signal(&signal, &portfolio).await {
            Ok(crate::engine::unified::RiskDecision::Approve) => {
                Ok(RiskCheckResult {
                    approved: true,
                    reason: None,
                    suggested_quantity: None,
                    suggested_price: None,
                })
            },
            Ok(crate::engine::unified::RiskDecision::Reject { reason }) => {
                Ok(RiskCheckResult {
                    approved: false,
                    reason: Some(reason.clone()),
                    suggested_quantity: None,
                    suggested_price: None,
                })
            },
            Ok(crate::engine::unified::RiskDecision::Modify { adjustments }) => {
                let suggested_qty = adjustments.get("quantity").copied();
                let suggested_price = adjustments.get("price").copied();
                
                Ok(RiskCheckResult {
                    approved: true, // 修改建議仍然批准，但調整參數
                    reason: Some("Risk manager suggests modifications".to_string()),
                    suggested_quantity: suggested_qty,
                    suggested_price: suggested_price,
                })
            },
            Err(e) => {
                error!("風險檢查失敗: {}", e);
                Err(format!("風險檢查系統錯誤: {}", e))
            }
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
            create_time: chrono::Utc::now().timestamp_nanos() as u64,
            update_time: chrono::Utc::now().timestamp_nanos() as u64,
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