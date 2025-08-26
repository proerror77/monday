/*!
 * 路由模組
 *
 * 提供智能路由功能，包括交易所選擇、故障轉移和負載均衡。
 */

pub mod exchange_router;

// 重新導出核心類型
pub use exchange_router::{
    ExchangeRouter, ExchangeStats, RouterConfig, RouterError, RouterStats, RoutingResult,
};

// 預留其他路由策略模組
// pub mod order_router;     // 訂單路由策略
// pub mod symbol_router;    // 交易對路由策略
// pub mod latency_router;   // 延遲優化路由
