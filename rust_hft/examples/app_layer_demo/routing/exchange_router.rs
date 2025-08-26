/*!
 * ExchangeRouter 門面封裝
 *
 * 簡化 ExchangeManager 的複雜內部 API，提供清晰的外部接口。
 * 隱藏 ArcSwap 兩層快照的複雜性，暴露簡潔的業務操作。
 */

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::domains::trading::AccountId;
use crate::exchanges::{ExchangeInstance, ExchangeManager, HealthMetrics, RoutingStrategy};

/// 交易所路由策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    /// 健康度閾值 (0.0 - 1.0)
    pub health_threshold: f64,
    /// 最大延遲容忍度 (毫秒)
    pub max_latency_ms: f64,
    /// 故障轉移啟用
    pub failover_enabled: bool,
    /// 預設路由策略
    pub default_strategy: RoutingStrategy,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            health_threshold: 0.8,
            max_latency_ms: 100.0,
            failover_enabled: true,
            default_strategy: RoutingStrategy::HealthBased,
        }
    }
}

/// 路由結果
#[derive(Debug, Clone)]
pub struct RoutingResult {
    /// 選中的交易所實例
    pub instance: Arc<ExchangeInstance>,
    /// 選擇原因
    pub reason: String,
    /// 健康度評分
    pub health_score: f64,
    /// 預期延遲 (毫秒)
    pub estimated_latency_ms: f64,
}

/// 交易所路由錯誤
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("No healthy exchanges available for {exchange}:{symbol}")]
    NoHealthyExchanges { exchange: String, symbol: String },

    #[error("Exchange {exchange} not found")]
    ExchangeNotFound { exchange: String },

    #[error("Symbol {symbol} not supported on {exchange}")]
    SymbolNotSupported { exchange: String, symbol: String },

    #[error("Account {account_id} not configured for {exchange}")]
    AccountNotConfigured {
        exchange: String,
        account_id: String,
    },

    #[error("All instances for {exchange}:{symbol} are unhealthy")]
    AllInstancesUnhealthy { exchange: String, symbol: String },

    #[error("Configuration error: {message}")]
    ConfigurationError { message: String },
}

/// ExchangeRouter 門面
///
/// 提供簡化的交易所路由接口，隱藏內部 ExchangeManager 的複雜性
pub struct ExchangeRouter {
    /// 內部 ExchangeManager 實例
    manager: Arc<ExchangeManager>,
    /// 路由配置
    config: RouterConfig,
}

impl ExchangeRouter {
    /// 創建新的路由器
    pub fn new(manager: Arc<ExchangeManager>, config: RouterConfig) -> Self {
        Self { manager, config }
    }

    /// 創建默認配置的路由器
    pub fn with_default_config(manager: Arc<ExchangeManager>) -> Self {
        Self::new(manager, RouterConfig::default())
    }

    // ====================== 核心路由方法 ======================

    /// 智能路由：選擇最佳交易所實例
    ///
    /// 根據健康度、延遲、可用性等因素自動選擇最優實例
    pub async fn route_best_instance(
        &self,
        exchange: &str,
        symbol: &str,
    ) -> Result<RoutingResult, RouterError> {
        // 1. 基礎可用性檢查
        let instance = self
            .manager
            .select_instance(exchange, symbol)
            .ok_or_else(|| RouterError::NoHealthyExchanges {
                exchange: exchange.to_string(),
                symbol: symbol.to_string(),
            })?;

        // 2. 健康度評估
        let health = instance.health.load();
        if health.availability_score < self.config.health_threshold {
            return Err(RouterError::AllInstancesUnhealthy {
                exchange: exchange.to_string(),
                symbol: symbol.to_string(),
            });
        }

        // 3. 延遲檢查
        if health.latency_ewma > self.config.max_latency_ms {
            if self.config.failover_enabled {
                // 嘗試故障轉移到其他實例
                return self.try_failover(exchange, symbol).await;
            }
        }

        Ok(RoutingResult {
            instance: instance.clone(),
            reason: format!(
                "Selected healthy instance with {:.2}% availability",
                health.availability_score * 100.0
            ),
            health_score: health.availability_score,
            estimated_latency_ms: health.latency_ewma,
        })
    }

    /// 為特定賬戶路由
    pub async fn route_for_account(
        &self,
        account_id: &AccountId,
        symbol: &str,
    ) -> Result<RoutingResult, RouterError> {
        self.route_best_instance(&account_id.exchange, symbol).await
    }

    /// 批量路由：為多個交易對同時路由
    pub async fn route_multiple(
        &self,
        requests: Vec<(String, String)>, // (exchange, symbol) pairs
    ) -> HashMap<String, Result<RoutingResult, RouterError>> {
        let mut results = HashMap::new();

        for (exchange, symbol) in requests {
            let key = format!("{}:{}", exchange, symbol);
            let result = self.route_best_instance(&exchange, &symbol).await;
            results.insert(key, result);
        }

        results
    }

    // ====================== 便利方法 ======================

    /// 檢查交易所是否健康
    pub async fn is_exchange_healthy(&self, exchange: &str) -> bool {
        self.manager
            .get_healthy_exchanges()
            .await
            .contains(&exchange.to_string())
    }

    /// 獲取所有健康的交易所
    pub async fn get_healthy_exchanges(&self) -> Vec<String> {
        self.manager.get_healthy_exchanges().await
    }

    /// 檢查特定交易對是否可用
    pub async fn is_symbol_available(&self, exchange: &str, symbol: &str) -> bool {
        self.manager.select_instance(exchange, symbol).is_some()
    }

    /// 獲取交易所健康指標
    pub async fn get_exchange_health(&self, exchange: &str, symbol: &str) -> Option<HealthMetrics> {
        let instance = self.manager.select_instance(exchange, symbol)?;
        let health = instance.health.load();
        Some((**health).clone())
    }

    // ====================== 管理操作 ======================

    /// 啟用/禁用特定實例
    pub fn set_instance_enabled(
        &self,
        exchange: &str,
        account_id: &str,
        enabled: bool,
    ) -> Result<(), RouterError> {
        self.manager
            .set_instance_enabled(exchange, account_id, enabled)
            .map_err(|msg| RouterError::ConfigurationError { message: msg })
    }

    /// 記錄延遲指標
    pub async fn record_latency(&self, exchange: &str, account_id: &str, latency_ms: f64) {
        self.manager
            .record_latency(exchange, account_id, latency_ms)
            .await;
    }

    /// 記錄錯誤
    pub async fn record_error(&self, exchange: &str, account_id: &str) {
        self.manager.record_error(exchange, account_id).await;
    }

    /// 更新路由配置
    pub fn update_config(&mut self, new_config: RouterConfig) {
        self.config = new_config;
    }

    /// 獲取當前配置
    pub fn get_config(&self) -> &RouterConfig {
        &self.config
    }

    // ====================== 內部實現 ======================

    /// 故障轉移邏輯
    async fn try_failover(
        &self,
        exchange: &str,
        symbol: &str,
    ) -> Result<RoutingResult, RouterError> {
        // 簡化的故障轉移：嘗試獲取另一個實例
        // 在實際實現中，這裡可以嘗試其他交易所或其他賬戶

        let instance = self
            .manager
            .select_instance(exchange, symbol)
            .ok_or_else(|| RouterError::AllInstancesUnhealthy {
                exchange: exchange.to_string(),
                symbol: symbol.to_string(),
            })?;

        let health = instance.health.load();

        Ok(RoutingResult {
            instance: instance.clone(),
            reason: "Failover to backup instance".to_string(),
            health_score: health.availability_score,
            estimated_latency_ms: health.latency_ewma,
        })
    }

    /// 評估實例健康度
    async fn evaluate_instance_health(&self, instance: &ExchangeInstance) -> f64 {
        let health = instance.health.load();

        // 綜合評分：可用性 * 延遲因子
        let latency_factor = if health.latency_ewma > 0.0 {
            (self.config.max_latency_ms / health.latency_ewma).min(1.0)
        } else {
            1.0
        };

        health.availability_score * latency_factor
    }
}

// ====================== 便利構造函數 ======================

impl ExchangeRouter {
    /// 從現有 ExchangeManager 創建路由器
    pub fn from_manager(manager: Arc<ExchangeManager>) -> Self {
        Self::with_default_config(manager)
    }

    /// 創建用於測試的路由器
    #[cfg(test)]
    pub fn for_testing() -> Self {
        let manager = Arc::new(ExchangeManager::new());
        Self::with_default_config(manager)
    }
}

// ====================== 統計和監控 ======================

/// 路由統計信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterStats {
    /// 總路由請求數
    pub total_requests: u64,
    /// 成功路由數
    pub successful_routes: u64,
    /// 故障轉移次數
    pub failover_count: u64,
    /// 平均路由延遲 (微秒)
    pub avg_routing_latency_us: f64,
    /// 按交易所分組的統計
    pub exchange_stats: HashMap<String, ExchangeStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeStats {
    /// 請求次數
    pub request_count: u64,
    /// 成功次數
    pub success_count: u64,
    /// 平均健康度
    pub avg_health_score: f64,
    /// 平均延遲
    pub avg_latency_ms: f64,
}

impl ExchangeRouter {
    /// 獲取路由統計信息
    pub async fn get_stats(&self) -> RouterStats {
        // 這裡應該從內部指標收集器收集數據
        // 簡化實現，返回默認值
        RouterStats {
            total_requests: 0,
            successful_routes: 0,
            failover_count: 0,
            avg_routing_latency_us: 0.0,
            exchange_stats: HashMap::new(),
        }
    }

    /// 重置統計信息
    pub fn reset_stats(&self) {
        // 實現統計重置邏輯
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_router_creation() {
        let router = ExchangeRouter::for_testing();
        assert_eq!(router.config.health_threshold, 0.8);
        assert!(router.config.failover_enabled);
    }

    #[tokio::test]
    async fn test_health_check() {
        let router = ExchangeRouter::for_testing();
        let healthy_exchanges = router.get_healthy_exchanges().await;
        // 新創建的管理器應該沒有交易所
        assert!(healthy_exchanges.is_empty());
    }

    #[tokio::test]
    async fn test_symbol_availability() {
        let router = ExchangeRouter::for_testing();
        let available = router.is_symbol_available("bitget", "BTCUSDT").await;
        // 沒有配置的情況下應該不可用
        assert!(!available);
    }
}
