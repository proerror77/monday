//! 統一交易所抽象層
//! 
//! 提供多交易所支持的核心抽象和實現

pub mod exchange_trait;
pub mod bitget;
pub mod binance;
pub mod message_types;
pub mod utils;

// 重新導出核心類型
pub use exchange_trait::{
    Exchange, MarketDataClient, TradingClient, ExchangeInfo,
    ConnectionStatus, MarketDataStatus, TradingStatus
};
pub use message_types::{
    MarketEvent, ExecutionReport, OrderRequest, CancelRequest, 
    AmendRequest, OrderResponse, OrderBookLevel
};
pub use bitget::BitgetExchange;
pub use binance::BinanceExchange;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 交易所管理器 - 管理多個交易所連接
pub struct ExchangeManager {
    exchanges: Arc<RwLock<HashMap<String, Arc<RwLock<Box<dyn Exchange + Send + Sync>>>>>>,
}

impl ExchangeManager {
    pub fn new() -> Self {
        Self {
            exchanges: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 添加交易所
    pub async fn add_exchange(&self, name: String, exchange: Box<dyn Exchange + Send + Sync>) {
        let mut exchanges = self.exchanges.write().await;
        exchanges.insert(name, Arc::new(RwLock::new(exchange)));
    }

    /// 獲取交易所 - 修復邏輯錯誤，正確返回已註冊的交易所實例
    pub async fn get_exchange(&self, name: &str) -> Option<Arc<RwLock<Box<dyn Exchange + Send + Sync>>>> {
        let exchanges = self.exchanges.read().await;
        exchanges.get(name).cloned()
    }

    /// 獲取所有交易所名稱
    pub async fn list_exchanges(&self) -> Vec<String> {
        let exchanges = self.exchanges.read().await;
        exchanges.keys().cloned().collect()
    }

    /// 獲取健康的交易所
    pub async fn get_healthy_exchanges(&self) -> Vec<String> {
        let exchanges = self.exchanges.read().await;
        let mut healthy = Vec::new();
        
        for (name, exchange_arc) in exchanges.iter() {
            let exchange = exchange_arc.read().await;
            let info = exchange.get_info().await;
            if info.connection_status == ConnectionStatus::Connected {
                healthy.push(name.clone());
            }
        }
        
        healthy
    }
}

impl Default for ExchangeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_exchange_manager() {
        let manager = ExchangeManager::new();
        
        // 添加Bitget交易所
        let bitget = Box::new(bitget::BitgetExchange::new());
        manager.add_exchange("bitget".to_string(), bitget).await;
        
        let exchanges = manager.list_exchanges().await;
        assert!(exchanges.contains(&"bitget".to_string()));
    }
}