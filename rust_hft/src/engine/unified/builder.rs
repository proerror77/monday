/*!
 * Unified Engine Builder
 * 
 * 统一引擎的构建器模式实现
 */

use super::{UnifiedEngineConfig, UnifiedTradingEngine};
use crate::engine::strategy::TradingStrategy;
use crate::engine::unified_risk_manager::UnifiedRiskManager;
use anyhow::Result;

/// 統一引擎建構器
pub struct UnifiedEngineBuilder {
    config: Option<UnifiedEngineConfig>,
    strategy: Option<Box<dyn TradingStrategy>>,
    risk_manager: Option<Box<dyn RiskManager>>,
    execution_manager: Option<Box<dyn ExecutionManager>>,
}

impl UnifiedEngineBuilder {
    /// 創建新的建構器
    pub fn new() -> Self {
        Self {
            config: None,
            strategy: None,
            risk_manager: None,
            execution_manager: None,
        }
    }

    /// 設置配置
    pub fn with_config(mut self, config: UnifiedEngineConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// 設置策略
    pub fn with_strategy(mut self, strategy: Box<dyn TradingStrategy>) -> Self {
        self.strategy = Some(strategy);
        self
    }

    /// 設置風險管理器
    pub fn with_risk_manager(mut self, risk_manager: Box<dyn RiskManager>) -> Self {
        self.risk_manager = Some(risk_manager);
        self
    }

    /// 設置執行管理器
    pub fn with_execution_manager(mut self, execution_manager: Box<dyn ExecutionManager>) -> Self {
        self.execution_manager = Some(execution_manager);
        self
    }

    /// 建構引擎
    pub fn build(self) -> Result<UnifiedTradingEngine> {
        let config = self.config.unwrap_or_default();
        
        let strategy = self.strategy
            .ok_or_else(|| anyhow::anyhow!("Trading strategy is required"))?;
            
        let risk_manager = self.risk_manager
            .ok_or_else(|| anyhow::anyhow!("Risk manager is required"))?;
            
        let execution_manager = self.execution_manager
            .ok_or_else(|| anyhow::anyhow!("Execution manager is required"))?;

        UnifiedTradingEngine::new(config, strategy, risk_manager, execution_manager)
    }
}

impl Default for UnifiedEngineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// Placeholder traits - these would be defined elsewhere in a real implementation
pub trait RiskManager: Send + Sync {
    fn evaluate_risk(&self, order: &PendingOrder) -> RiskDecision;
}

pub trait ExecutionManager: Send + Sync {
    fn execute_order(&mut self, order: Order) -> Result<ExecutionResult>;
}

// Placeholder types
#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub symbol: String,
    pub side: String,
    pub quantity: f64,
    pub price: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub quantity: f64,
    pub price: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum RiskDecision {
    Allow,
    Reject(String),
    Modify(f64), // New quantity
}

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub order_id: String,
    pub status: String,
    pub filled_quantity: f64,
    pub average_price: f64,
}

// Dummy implementations for compilation
pub struct DummyRiskManager;

impl RiskManager for DummyRiskManager {
    fn evaluate_risk(&self, _order: &PendingOrder) -> RiskDecision {
        RiskDecision::Allow
    }
}

pub struct DummyExecutionManager;

impl ExecutionManager for DummyExecutionManager {
    fn execute_order(&mut self, order: Order) -> Result<ExecutionResult> {
        Ok(ExecutionResult {
            order_id: order.id,
            status: "filled".to_string(),
            filled_quantity: order.quantity,
            average_price: order.price.unwrap_or(100.0),
        })
    }
}