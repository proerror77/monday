/*!
 * 統一事件總線架構 (Unified Event Bus)
 *
 * 提供系統內統一的事件發布/訂閱機制，解耦各個服務之間的直接依賴。
 * 支援異步事件處理、事件過濾、優先級排序和事件持久化。
 */

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

const EVENT_BUS_CAPACITY: usize = 1024;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domains::trading::{AccountId, OrderSide, OrderStatus};

/// 事件類型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    /// 訂單事件
    Order,
    /// 執行事件
    Execution,
    /// 風險事件
    Risk,
    /// 市場數據事件
    MarketData,
    /// 系統事件
    System,
    /// 交易所連接事件
    Exchange,
    /// 用戶自定義事件
    Custom(String),
}

/// 事件優先級
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EventPriority {
    /// 低優先級 (批次處理)
    Low = 1,
    /// 普通優先級 (一般業務)
    Normal = 2,
    /// 高優先級 (實時處理)
    High = 3,
    /// 緊急優先級 (風險控制)
    Critical = 4,
}

impl Default for EventPriority {
    fn default() -> Self {
        EventPriority::Normal
    }
}

/// 統一事件封裝
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// 事件ID
    pub id: String,

    /// 事件類型
    pub event_type: EventType,

    /// 事件優先級
    pub priority: EventPriority,

    /// 事件載荷
    pub payload: EventPayload,

    /// 事件時間戳 (微秒)
    pub timestamp: u64,

    /// 來源服務
    pub source: String,

    /// 關聯的賬戶ID (可選)
    pub account_id: Option<AccountId>,

    /// 事件元數據
    pub metadata: HashMap<String, String>,

    /// 是否需要持久化
    pub persistent: bool,

    /// 重試次數
    pub retry_count: u32,
}

/// 事件載荷 (支援各種事件類型)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventPayload {
    /// 訂單更新事件
    OrderUpdate(OrderUpdatePayload),

    /// 執行事件
    Execution(ExecutionEventPayload),

    /// 風險告警事件
    RiskAlert(RiskAlertPayload),

    /// 市場數據事件
    MarketData(MarketDataPayload),

    /// 系統狀態事件
    SystemStatus(SystemStatusPayload),

    /// 交易所連接事件
    ExchangeConnection(ExchangeConnectionPayload),

    /// 原始 JSON 事件
    Raw(serde_json::Value),
}

/// 訂單更新載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdatePayload {
    pub order_id: String,
    pub account_id: AccountId,
    pub symbol: String,
    pub side: OrderSide,
    pub status: OrderStatus,
    pub update_type: String, // "new", "filled", "cancelled", etc.
}

/// 執行事件載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEventPayload {
    pub execution_id: String,
    pub order_id: String,
    pub account_id: AccountId,
    pub symbol: String,
    pub execution_type: String, // "started", "completed", "failed", etc.
    pub latency_us: Option<u64>,
    pub quality_score: Option<f64>,
}

/// 風險告警載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAlertPayload {
    pub alert_id: String,
    pub account_id: AccountId,
    pub alert_type: String,
    pub severity: String,
    pub message: String,
    pub risk_score: f64,
}

/// 市場數據載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketDataPayload {
    pub symbol: String,
    pub exchange: String,
    pub data_type: String, // "orderbook", "trade", "ticker", etc.
    pub quality_score: f64,
}

/// 系統狀態載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemStatusPayload {
    pub component: String,
    pub status: String, // "healthy", "warning", "critical", "offline"
    pub message: String,
    pub metrics: HashMap<String, f64>,
}

/// 交易所連接載荷
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeConnectionPayload {
    pub exchange: String,
    pub account_id: AccountId,
    pub connection_type: String, // "websocket", "rest", etc.
    pub status: String,          // "connected", "disconnected", "reconnecting", etc.
}

/// 事件過濾器
#[derive(Clone)]
pub struct EventFilter {
    /// 事件類型過濾
    pub event_types: Option<Vec<EventType>>,

    /// 優先級過濾
    pub min_priority: Option<EventPriority>,

    /// 賬戶過濾
    pub account_ids: Option<Vec<AccountId>>,

    /// 來源服務過濾
    pub sources: Option<Vec<String>>,

    /// 自定義過濾函數
    pub custom_filter: Option<Arc<dyn Fn(&Event) -> bool + Send + Sync>>,
}

impl std::fmt::Debug for EventFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventFilter")
            .field("event_types", &self.event_types)
            .field("min_priority", &self.min_priority)
            .field("account_ids", &self.account_ids)
            .field("sources", &self.sources)
            .field("custom_filter", &self.custom_filter.as_ref().map(|_| "<function>"))
            .finish()
    }
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            event_types: None,
            min_priority: None,
            account_ids: None,
            sources: None,
            custom_filter: None,
        }
    }
}

impl EventFilter {
    /// 檢查事件是否匹配過濾條件
    pub fn matches(&self, event: &Event) -> bool {
        // 事件類型過濾
        if let Some(ref types) = self.event_types {
            if !types.contains(&event.event_type) {
                return false;
            }
        }

        // 優先級過濾
        if let Some(min_priority) = self.min_priority {
            if event.priority < min_priority {
                return false;
            }
        }

        // 賬戶過濾
        if let Some(ref account_ids) = self.account_ids {
            match &event.account_id {
                Some(account_id) => {
                    if !account_ids.contains(account_id) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        // 來源服務過濾
        if let Some(ref sources) = self.sources {
            if !sources.contains(&event.source) {
                return false;
            }
        }

        // 自定義過濾
        if let Some(ref filter_fn) = self.custom_filter {
            if !filter_fn(event) {
                return false;
            }
        }

        true
    }
}

/// 事件處理器類型
pub type EventHandler = Arc<dyn Fn(Event) -> Result<()> + Send + Sync>;

/// 事件訂閱者信息
struct Subscriber {
    id: String,
    filter: EventFilter,
    handler: EventHandler,
    channel: mpsc::Sender<Event>,
}

impl std::fmt::Debug for Subscriber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscriber")
            .field("id", &self.id)
            .field("filter", &self.filter)
            .field("handler", &"<function>")
            .field("channel", &"<channel>")
            .finish()
    }
}

/// 事件總線統計信息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventBusStats {
    /// 總事件數
    pub total_events: u64,

    /// 按類型分組的事件數
    pub events_by_type: HashMap<String, u64>,

    /// 按優先級分組的事件數
    pub events_by_priority: HashMap<String, u64>,

    /// 總訂閱者數
    pub total_subscribers: usize,

    /// 處理延遲統計 (微秒)
    pub avg_processing_latency_us: f64,

    /// 最大處理延遲 (微秒)
    pub max_processing_latency_us: u64,

    /// 事件丟失數
    pub dropped_events: u64,

    /// 重試次數
    pub retry_count: u64,
}

/// 統一事件總線
pub struct EventBus {
    /// 訂閱者映射
    subscribers: Arc<RwLock<HashMap<String, Subscriber>>>,

    /// 事件發布通道
    event_sender: mpsc::Sender<Event>,

    /// 廣播通道 (用於高優先級事件)
    broadcast_sender: broadcast::Sender<Event>,

    /// 統計信息
    stats: Arc<RwLock<EventBusStats>>,

    /// 是否啟用事件持久化
    enable_persistence: bool,
}

impl EventBus {
    /// 創建新的事件總線
    pub fn new(enable_persistence: bool) -> (Self, mpsc::Receiver<Event>) {
        let (event_sender, event_receiver) = mpsc::channel(EVENT_BUS_CAPACITY);
        let (broadcast_sender, _) = broadcast::channel(1000);

        let bus = Self {
            subscribers: Arc::new(RwLock::new(HashMap::new())),
            event_sender,
            broadcast_sender,
            stats: Arc::new(RwLock::new(EventBusStats::default())),
            enable_persistence,
        };

        (bus, event_receiver)
    }

    /// 發布事件
    pub async fn publish(&self, mut event: Event) -> Result<()> {
        let start_time = crate::core::types::now_micros();

        // 設置事件ID (如果未設置)
        if event.id.is_empty() {
            event.id = Uuid::new_v4().to_string();
        }

        // 設置時間戳 (如果未設置)
        if event.timestamp == 0 {
            event.timestamp = start_time;
        }

        // 更新統計
        self.update_stats(&event).await;

        // 高優先級事件使用廣播通道
        if event.priority >= EventPriority::High {
            let _ = self.broadcast_sender.send(event.clone());
        }

        // 發送到主事件通道
        match self.event_sender.try_send(event) {
            Ok(_) => {}
            Err(mpsc::error::TrySendError::Full(_e)) => {
                // drop when full
                let mut stats = self.stats.write().await;
                stats.dropped_events += 1;
            }
            Err(mpsc::error::TrySendError::Closed(e)) => {
                return Err(anyhow::anyhow!("Event channel closed: {:?}", e));
            }
        }

        Ok(())
    }

    /// 訂閱事件
    pub async fn subscribe(
        &self,
        subscriber_id: String,
        filter: EventFilter,
        handler: EventHandler,
    ) -> Result<mpsc::Receiver<Event>> {
        let (sender, receiver) = mpsc::channel(EVENT_BUS_CAPACITY);

        let subscriber = Subscriber {
            id: subscriber_id.clone(),
            filter,
            handler,
            channel: sender,
        };

        self.subscribers
            .write()
            .await
            .insert(subscriber_id, subscriber);

        Ok(receiver)
    }

    /// 取消訂閱
    pub async fn unsubscribe(&self, subscriber_id: &str) -> Result<()> {
        self.subscribers.write().await.remove(subscriber_id);
        Ok(())
    }

    /// 啟動事件總線處理循環
    pub async fn start_processing(&self, mut event_receiver: mpsc::Receiver<Event>) {
        let subscribers = self.subscribers.clone();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            while let Some(event) = event_receiver.recv().await {
                let start_time = crate::core::types::now_micros();

                // 分發事件給匹配的訂閱者
                let subscribers_read = subscribers.read().await;
                for subscriber in subscribers_read.values() {
                    if subscriber.filter.matches(&event) {
                        match subscriber.channel.try_send(event.clone()) {
                            Ok(()) => {
                                // 調用處理器 (非阻塞)
                                let handler = subscriber.handler.clone();
                                let event_clone = event.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handler(event_clone) {
                                        tracing::warn!("Event handler failed: {}", e);
                                    }
                                });
                            }
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                // 訂閱者通道滿，丟棄事件並統計
                                stats.write().await.dropped_events += 1;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                // 通道關閉，記錄丟失
                                stats.write().await.dropped_events += 1;
                            }
                        }
                    }
                }

                // 更新處理延遲統計
                let processing_latency = crate::core::types::now_micros() - start_time;
                let mut stats_lock = stats.write().await;

                if stats_lock.total_events == 0 {
                    stats_lock.avg_processing_latency_us = processing_latency as f64;
                } else {
                    let prev_total =
                        stats_lock.avg_processing_latency_us * stats_lock.total_events as f64;
                    stats_lock.avg_processing_latency_us = (prev_total + processing_latency as f64)
                        / (stats_lock.total_events + 1) as f64;
                }

                stats_lock.max_processing_latency_us =
                    stats_lock.max_processing_latency_us.max(processing_latency);
            }
        });
    }

    /// 獲取統計信息
    pub async fn get_stats(&self) -> EventBusStats {
        self.stats.read().await.clone()
    }

    /// 獲取訂閱者列表
    pub async fn get_subscribers(&self) -> Vec<String> {
        self.subscribers.read().await.keys().cloned().collect()
    }

    /// 清理無效訂閱者
    pub async fn cleanup_subscribers(&self) {
        let mut subscribers = self.subscribers.write().await;
        subscribers.retain(|_, subscriber| !subscriber.channel.is_closed());
    }

    // ====================== 內部輔助方法 ======================

    /// 更新統計信息
    async fn update_stats(&self, event: &Event) {
        let mut stats = self.stats.write().await;

        stats.total_events += 1;

        // 按類型統計
        let type_key = format!("{:?}", event.event_type);
        *stats.events_by_type.entry(type_key).or_insert(0) += 1;

        // 按優先級統計
        let priority_key = format!("{:?}", event.priority);
        *stats.events_by_priority.entry(priority_key).or_insert(0) += 1;

        // 更新訂閱者數量
        stats.total_subscribers = self.subscribers.read().await.len();
    }
}

// ====================== 便利方法 ======================

impl Event {
    /// 創建訂單事件
    pub fn order_event(
        payload: OrderUpdatePayload,
        priority: EventPriority,
        source: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            event_type: EventType::Order,
            priority,
            payload: EventPayload::OrderUpdate(payload.clone()),
            timestamp: crate::core::types::now_micros(),
            source,
            account_id: Some(payload.account_id.clone()),
            metadata: HashMap::new(),
            persistent: false,
            retry_count: 0,
        }
    }

    /// 創建執行事件
    pub fn execution_event(
        payload: ExecutionEventPayload,
        priority: EventPriority,
        source: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            event_type: EventType::Execution,
            priority,
            payload: EventPayload::Execution(payload.clone()),
            timestamp: crate::core::types::now_micros(),
            source,
            account_id: Some(payload.account_id.clone()),
            metadata: HashMap::new(),
            persistent: false,
            retry_count: 0,
        }
    }

    /// 創建風險告警事件
    pub fn risk_alert_event(payload: RiskAlertPayload, source: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            event_type: EventType::Risk,
            priority: EventPriority::Critical, // 風險事件始終高優先級
            payload: EventPayload::RiskAlert(payload.clone()),
            timestamp: crate::core::types::now_micros(),
            source,
            account_id: Some(payload.account_id.clone()),
            metadata: HashMap::new(),
            persistent: true, // 風險事件需要持久化
            retry_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::trading::{AccountId, OrderSide, OrderStatus};

    #[tokio::test]
    async fn test_event_bus_creation() {
        let (event_bus, _receiver) = EventBus::new(false);
        let stats = event_bus.get_stats().await;

        assert_eq!(stats.total_events, 0);
        assert_eq!(stats.total_subscribers, 0);
    }

    #[tokio::test]
    async fn test_event_publishing() {
        let (event_bus, _receiver) = EventBus::new(false);

        let payload = OrderUpdatePayload {
            order_id: "order_123".to_string(),
            account_id: AccountId::new("bitget", "main"),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            status: OrderStatus::New,
            update_type: "new".to_string(),
        };

        let event = Event::order_event(payload, EventPriority::Normal, "order_service".to_string());

        let result = event_bus.publish(event).await;
        assert!(result.is_ok());

        let stats = event_bus.get_stats().await;
        assert_eq!(stats.total_events, 1);
    }

    #[tokio::test]
    async fn test_event_filtering() {
        let filter = EventFilter {
            event_types: Some(vec![EventType::Order]),
            min_priority: Some(EventPriority::High),
            ..Default::default()
        };

        let payload = OrderUpdatePayload {
            order_id: "order_123".to_string(),
            account_id: AccountId::new("bitget", "main"),
            symbol: "BTCUSDT".to_string(),
            side: OrderSide::Buy,
            status: OrderStatus::New,
            update_type: "new".to_string(),
        };

        // 低優先級事件應該被過濾
        let low_priority_event =
            Event::order_event(payload.clone(), EventPriority::Normal, "test".to_string());
        assert!(!filter.matches(&low_priority_event));

        // 高優先級事件應該匹配
        let high_priority_event =
            Event::order_event(payload, EventPriority::High, "test".to_string());
        assert!(filter.matches(&high_priority_event));
    }
}
