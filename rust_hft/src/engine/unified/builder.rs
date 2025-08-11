/*!
 * Unified Engine Builder
 * 
 * 统一引擎的构建器模式实现
 */

use super::{UnifiedEngineConfig, UnifiedTradingEngine};
use crate::engine::strategies::TradingStrategy;
use anyhow::Result;
use async_trait::async_trait;
use uuid;
use chrono;

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

// Import types from parent module, avoid redefinition
pub use super::{Signal, Portfolio, OrderStatus, OrderModification, RiskReport, OrderSide};

// Define RiskDecision locally to avoid conflict
#[derive(Debug, Clone)]
pub enum RiskDecision {
    Approve,
    Reject { reason: String },
    Modify { adjustments: std::collections::HashMap<String, f64> },
}

// Updated traits to match actual usage
#[async_trait]
pub trait RiskManager: Send + Sync {
    async fn check_signal(&self, signal: &Signal, portfolio: &Portfolio) -> Result<RiskDecision>;
    async fn check_order(&self, order: &Order, portfolio: &Portfolio) -> Result<RiskDecision>;
    async fn update_metrics(&mut self, portfolio: &Portfolio) -> Result<()>;
    async fn get_risk_report(&self) -> RiskReport;
}

#[async_trait]
pub trait ExecutionManager: Send + Sync {
    async fn execute_signal(&mut self, signal: Signal) -> Result<Order>;
    async fn cancel_order(&mut self, order_id: &str) -> Result<()>;
    async fn modify_order(&mut self, order_id: &str, new_params: OrderModification) -> Result<()>;
    async fn get_order_status(&self, order_id: &str) -> Result<OrderStatus>;
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
    pub order_type: super::OrderType,
    pub status: OrderStatus,
    pub filled_quantity: f64,
}

// Removed duplicate RiskDecision definition

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub order_id: String,
    pub status: String,
    pub filled_quantity: f64,
    pub average_price: f64,
}

// Dummy implementations for compilation
pub struct DummyRiskManager;

#[async_trait]
impl RiskManager for DummyRiskManager {
    async fn check_signal(&self, _signal: &Signal, _portfolio: &Portfolio) -> Result<RiskDecision> {
        Ok(RiskDecision::Approve)
    }
    
    async fn check_order(&self, _order: &Order, _portfolio: &Portfolio) -> Result<RiskDecision> {
        Ok(RiskDecision::Approve)
    }
    
    async fn update_metrics(&mut self, _portfolio: &Portfolio) -> Result<()> {
        Ok(())
    }
    
    async fn get_risk_report(&self) -> RiskReport {
        RiskReport {
            timestamp: chrono::Utc::now().timestamp_millis() as u64,
            var_95: 0.0,
            var_99: 0.0,
            sharpe_ratio: 0.0,
            max_drawdown: 0.0,
            current_drawdown: 0.0,
            exposure: std::collections::HashMap::new(),
            risk_level: super::RiskLevel::Low,
        }
    }
}

pub struct DummyExecutionManager;

#[async_trait]
impl ExecutionManager for DummyExecutionManager {
    async fn execute_signal(&mut self, signal: Signal) -> Result<Order> {
        Ok(Order {
            id: uuid::Uuid::new_v4().to_string(),
            symbol: signal.symbol,
            side: match signal.action {
                super::SignalAction::Buy => "Buy".to_string(),
                super::SignalAction::Sell => "Sell".to_string(),
                _ => "Buy".to_string(),
            },
            quantity: signal.quantity,
            price: signal.price,
            order_type: super::OrderType::Market,
            status: OrderStatus::New,
            filled_quantity: 0.0,
        })
    }
    
    async fn cancel_order(&mut self, _order_id: &str) -> Result<()> {
        Ok(())
    }
    
    async fn modify_order(&mut self, _order_id: &str, _new_params: OrderModification) -> Result<()> {
        Ok(())
    }
    
    async fn get_order_status(&self, _order_id: &str) -> Result<OrderStatus> {
        Ok(OrderStatus::New)
    }
}