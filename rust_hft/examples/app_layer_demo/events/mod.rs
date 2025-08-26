/*!
 * 事件處理模組 (Event Handling Module)
 *
 * 統一的事件驅動架構，提供事件總線、事件處理器和事件路由。
 */

pub mod event_bus;

// 重新導出核心類型
pub use event_bus::{
    Event, EventBus, EventBusStats, EventFilter, EventHandler, EventPayload, EventPriority,
    EventType, ExchangeConnectionPayload, ExecutionEventPayload, MarketDataPayload,
    OrderUpdatePayload, RiskAlertPayload, SystemStatusPayload,
};
