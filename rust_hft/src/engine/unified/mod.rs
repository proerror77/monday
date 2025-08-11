/*!
 * Unified Trading Engine - 统一交易引擎模块
 * 
 * 将768行的unified_engine.rs拆分为可管理的模块
 */

pub mod config;
pub mod engine;
pub mod builder;
pub mod modes;

// Re-export main types
pub use config::{UnifiedEngineConfig, RiskConfig, ExecutionConfig, PerformanceConfig, OrderType};
pub use engine::{
    UnifiedTradingEngine, EngineState, Position, PositionSide,
    EngineEvent, EngineStats
};
// 直接導出策略層 TradingSignal 作為統一信號型別
pub use crate::engine::strategies::traits::TradingSignal;
pub use builder::{
    UnifiedEngineBuilder, RiskManager, ExecutionManager, PendingOrder, 
    Order, RiskDecision, ExecutionResult, DummyRiskManager, DummyExecutionManager
};
pub use modes::{EngineMode, LiveMode, BacktestMode, SlippageModel};

// Additional types for compatibility
use std::collections::HashMap;
use serde_json::Value;

/// Signal for trading actions
#[derive(Debug, Clone)]
pub struct Signal {
    pub id: String,
    pub symbol: String,
    pub action: SignalAction,
    pub quantity: f64,
    pub price: Option<f64>,
    pub timestamp: u64,
    pub confidence: f64, // 信心度，用於風險管理
    pub metadata: HashMap<String, Value>,
}

/// Signal action types
#[derive(Debug, Clone, Copy)]
pub enum SignalAction {
    Buy,
    Sell,
    Hold,
}

/// Order side enum
#[derive(Debug, Clone, Copy)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order status enum  
#[derive(Debug, Clone)]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
}

/// Portfolio information
#[derive(Debug, Clone)]
pub struct Portfolio {
    pub cash: f64,
    pub positions: HashMap<String, Position>,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub total_value: f64,
}

impl Default for Portfolio {
    fn default() -> Self {
        Self {
            cash: 0.0,
            positions: HashMap::new(),
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            total_value: 0.0,
        }
    }
}

/// Risk level enum
#[derive(Debug, Clone)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

/// Risk report
#[derive(Debug, Clone)]
pub struct RiskReport {
    pub timestamp: u64,
    pub var_95: f64,
    pub var_99: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub current_drawdown: f64,
    pub exposure: HashMap<String, f64>,
    pub risk_level: RiskLevel,
}

/// Strategy state
#[derive(Debug, Clone)]
pub enum StrategyState {
    Active,
    Inactive,
    Paused,
    Error(String),
}

/// Order modification
#[derive(Debug, Clone)]
pub struct OrderModification {
    pub order_id: String,
    pub new_quantity: Option<f64>,
    pub new_price: Option<f64>,
}

/// Trade execution result
#[derive(Debug, Clone)]
pub struct Trade {
    pub id: String,
    pub symbol: String,
    pub side: OrderSide,
    pub quantity: f64,
    pub price: f64,
    pub timestamp: u64,
    pub commission: f64,
}

/// Market data snapshot
#[derive(Debug, Clone)]
pub struct MarketData {
    pub symbol: String,
    pub timestamp: u64,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    pub volume: f64,
}

// Import the actual TradingStrategy from strategies module
pub use crate::engine::strategies::TradingStrategy;

/// Position information with additional fields for compatibility
impl Position {
    pub fn current_price(&self) -> f64 {
        self.average_price
    }
    
    pub fn quantity(&self) -> f64 {
        self.size
    }
}
