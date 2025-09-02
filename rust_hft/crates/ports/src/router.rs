//! 可插拔執行路由 - Phase 1 重構
//! 
//! 將 ExecutionWorker 的硬編碼路由邏輯抽象為可配置的 trait，
//! 支持不同的路由策略：SameVenue、StrategyMap、RoundRobin、LatencyAware

use hft_core::*;
use crate::events::{OrderIntent, MarketEvent};
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// 執行路由決策結果
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    /// 目標 Venue
    pub target_venue: VenueId,
    /// 對應的 ExecutionClient 索引
    pub client_index: usize,
    /// 決策理由（用於日誌和調試）
    pub reason: String,
}

/// 可插拔執行路由 Trait
pub trait ExecutionRouter: Send + Sync {
    /// 為給定的訂單意圖決定路由
    /// 
    /// # 參數
    /// - `intent`: 訂單意圖，可能包含 target_venue 偏好
    /// - `available_venues`: 可用的交易所及其客戶端索引
    /// - `market_context`: 當前市場狀態（可選，用於延遲感知路由）
    /// 
    /// # 返回
    /// - `Some(decision)`: 成功決策
    /// - `None`: 無可用路由（例如指定的 venue 不可用）
    fn route_order(
        &self,
        intent: &OrderIntent,
        available_venues: &HashMap<VenueId, usize>,
        market_context: Option<&MarketEvent>,
    ) -> Option<RoutingDecision>;
    
    /// 路由器名稱（用於日誌）
    fn name(&self) -> &'static str;
}

// === 基本路由實現 ===

/// 同場所路由器 - 優先選擇相同場所
#[derive(Debug, Clone)]
pub struct SameVenueRouter {
    /// 默認場所（當訂單未指定 target_venue 時使用）
    pub default_venue: VenueId,
}

impl SameVenueRouter {
    pub fn new(default_venue: VenueId) -> Self {
        Self { default_venue }
    }
}

impl ExecutionRouter for SameVenueRouter {
    fn route_order(
        &self,
        intent: &OrderIntent,
        available_venues: &HashMap<VenueId, usize>,
        _market_context: Option<&MarketEvent>,
    ) -> Option<RoutingDecision> {
        // 1. 如果 intent 指定了 target_venue，優先使用
        if let Some(target_venue) = intent.target_venue {
            if let Some(&client_index) = available_venues.get(&target_venue) {
                return Some(RoutingDecision {
                    target_venue,
                    client_index,
                    reason: format!("Intent specified venue: {}", target_venue),
                });
            }
        }
        
        // 2. 使用默認場所
        if let Some(&client_index) = available_venues.get(&self.default_venue) {
            Some(RoutingDecision {
                target_venue: self.default_venue,
                client_index,
                reason: format!("Default venue: {}", self.default_venue),
            })
        } else {
            // 3. 默認場所不可用，選擇第一個可用場所
            available_venues.iter().next().map(|(&venue, &index)| {
                RoutingDecision {
                    target_venue: venue,
                    client_index: index,
                    reason: format!("Fallback to first available: {}", venue),
                }
            })
        }
    }
    
    fn name(&self) -> &'static str {
        "SameVenue"
    }
}

/// 策略映射路由器 - 不同策略使用不同場所
#[derive(Debug, Clone)]
pub struct StrategyMapRouter {
    /// 策略 ID -> VenueId 映射
    pub strategy_venues: HashMap<String, VenueId>,
    /// 默認場所
    pub default_venue: VenueId,
}

impl StrategyMapRouter {
    pub fn new(strategy_venues: HashMap<String, VenueId>, default_venue: VenueId) -> Self {
        Self { strategy_venues, default_venue }
    }
}

impl ExecutionRouter for StrategyMapRouter {
    fn route_order(
        &self,
        intent: &OrderIntent,
        available_venues: &HashMap<VenueId, usize>,
        _market_context: Option<&MarketEvent>,
    ) -> Option<RoutingDecision> {
        // 1. Intent 指定場所優先
        if let Some(target_venue) = intent.target_venue {
            if let Some(&client_index) = available_venues.get(&target_venue) {
                return Some(RoutingDecision {
                    target_venue,
                    client_index,
                    reason: format!("Intent specified venue: {}", target_venue),
                });
            }
        }
        
        // 2. 根據策略映射決定場所
        let target_venue = self.strategy_venues
            .get(&intent.strategy_id)
            .copied()
            .unwrap_or(self.default_venue);
            
        if let Some(&client_index) = available_venues.get(&target_venue) {
            Some(RoutingDecision {
                target_venue,
                client_index,
                reason: format!("Strategy {} -> venue: {}", intent.strategy_id, target_venue),
            })
        } else {
            // 3. 映射的場所不可用，使用默認場所
            available_venues.get(&self.default_venue).map(|&index| {
                RoutingDecision {
                    target_venue: self.default_venue,
                    client_index: index,
                    reason: format!("Strategy venue unavailable, fallback to default: {}", self.default_venue),
                }
            })
        }
    }
    
    fn name(&self) -> &'static str {
        "StrategyMap"
    }
}

/// 輪詢路由器 - 輪流使用不同場所
#[derive(Debug)]
pub struct RoundRobinRouter {
    /// 場所列表（按優先級排序）
    pub venues: Vec<VenueId>,
    /// 當前索引（使用 Atomic 以支持並發）
    current_index: std::sync::atomic::AtomicUsize,
}

impl RoundRobinRouter {
    pub fn new(venues: Vec<VenueId>) -> Self {
        Self {
            venues,
            current_index: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl ExecutionRouter for RoundRobinRouter {
    fn route_order(
        &self,
        intent: &OrderIntent,
        available_venues: &HashMap<VenueId, usize>,
        _market_context: Option<&MarketEvent>,
    ) -> Option<RoutingDecision> {
        use std::sync::atomic::Ordering;
        
        // 1. Intent 指定場所優先
        if let Some(target_venue) = intent.target_venue {
            if let Some(&client_index) = available_venues.get(&target_venue) {
                return Some(RoutingDecision {
                    target_venue,
                    client_index,
                    reason: format!("Intent specified venue: {}", target_venue),
                });
            }
        }
        
        // 2. 輪詢選擇可用場所
        let start_index = self.current_index.fetch_add(1, Ordering::Relaxed) % self.venues.len();
        
        for i in 0..self.venues.len() {
            let venue_index = (start_index + i) % self.venues.len();
            let venue = self.venues[venue_index];
            
            if let Some(&client_index) = available_venues.get(&venue) {
                return Some(RoutingDecision {
                    target_venue: venue,
                    client_index,
                    reason: format!("Round-robin selected: {}", venue),
                });
            }
        }
        
        None // 無可用場所
    }
    
    fn name(&self) -> &'static str {
        "RoundRobin"
    }
}

/// 路由器配置（用於從配置文件構建路由器）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RouterConfig {
    SameVenue {
        default_venue: String, // 字串形式，運行時轉換為 VenueId
    },
    StrategyMap {
        strategy_venues: HashMap<String, String>, // strategy_id -> venue_name
        default_venue: String,
    },
    RoundRobin {
        venues: Vec<String>, // venue_name 列表
    },
}

impl RouterConfig {
    /// 構建實際的路由器實例
    pub fn build(self) -> Box<dyn ExecutionRouter> {
        match self {
            RouterConfig::SameVenue { default_venue } => {
                let venue_id = VenueId::from_str(&default_venue)
                    .unwrap_or_else(|| {
                        eprintln!("Warning: Unknown venue '{}', using BINANCE as fallback", default_venue);
                        VenueId::BINANCE
                    });
                Box::new(SameVenueRouter::new(venue_id))
            }
            RouterConfig::StrategyMap { strategy_venues, default_venue } => {
                let strategy_map: HashMap<String, VenueId> = strategy_venues
                    .into_iter()
                    .filter_map(|(strategy, venue_name)| {
                        VenueId::from_str(&venue_name).map(|venue_id| (strategy, venue_id))
                    })
                    .collect();
                    
                let default_venue_id = VenueId::from_str(&default_venue).unwrap_or(VenueId::BINANCE);
                Box::new(StrategyMapRouter::new(strategy_map, default_venue_id))
            }
            RouterConfig::RoundRobin { venues } => {
                let venue_ids: Vec<VenueId> = venues
                    .into_iter()
                    .filter_map(|name| VenueId::from_str(&name))
                    .collect();
                    
                if venue_ids.is_empty() {
                    eprintln!("Warning: No valid venues in RoundRobin config, using BINANCE as fallback");
                    Box::new(SameVenueRouter::new(VenueId::BINANCE))
                } else {
                    Box::new(RoundRobinRouter::new(venue_ids))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_same_venue_router() {
        let router = SameVenueRouter::new(VenueId::BINANCE);
        let mut available = HashMap::new();
        available.insert(VenueId::BINANCE, 0);
        available.insert(VenueId::BITGET, 1);
        
        let intent = OrderIntent {
            symbol: Symbol("BTCUSDT".to_string()),
            side: Side::Buy,
            quantity: Quantity::from_f64(1.0).unwrap(),
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: "test".to_string(),
            target_venue: None,
        };
        
        let decision = router.route_order(&intent, &available, None).unwrap();
        assert_eq!(decision.target_venue, VenueId::BINANCE);
        assert_eq!(decision.client_index, 0);
    }
    
    #[test]
    fn test_intent_specified_venue() {
        let router = SameVenueRouter::new(VenueId::BINANCE);
        let mut available = HashMap::new();
        available.insert(VenueId::BINANCE, 0);
        available.insert(VenueId::BITGET, 1);
        
        let mut intent = OrderIntent {
            symbol: Symbol("BTCUSDT".to_string()),
            side: Side::Buy,
            quantity: Quantity::from_f64(1.0).unwrap(),
            order_type: OrderType::Market,
            price: None,
            time_in_force: TimeInForce::IOC,
            strategy_id: "test".to_string(),
            target_venue: Some(VenueId::BITGET), // 指定 BITGET
        };
        
        let decision = router.route_order(&intent, &available, None).unwrap();
        assert_eq!(decision.target_venue, VenueId::BITGET);
        assert_eq!(decision.client_index, 1);
    }
}