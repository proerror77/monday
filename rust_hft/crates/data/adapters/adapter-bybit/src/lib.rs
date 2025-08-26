//! Bybit 市场数据适配器 - 占位符实现
//! 
//! 提供对 Bybit v5 公共 WebSocket API 的基础接口

use tracing::warn;

/// Bybit 市场数据流占位符
pub struct BybitMarketStream;

impl BybitMarketStream {
    pub fn new() -> Self {
        warn!("Bybit 市场数据适配器为占位符实现");
        Self
    }
}

// 不实现 MarketStream trait，仅提供基础结构供 runtime feature 编译通过