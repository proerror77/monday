/*!
 * Engine Module - 交易策略和執行引擎
 * 
 * 包含統一的引擎架構和專門化實現
 */

// 統一引擎架構（推薦使用）
pub mod unified;
pub mod unified_engine;
pub mod unified_risk_manager;
pub mod strategies;

// 核心功能模塊
pub mod strategy;
pub mod execution;
pub mod inference_strategy;
pub mod complete_oms;
pub mod risk_manager;

// 舊版實現已移除，請使用 unified_engine 和 unified_risk_manager

// 特定策略實現已移除，請使用 unified_engine 自定義策略

// 導出統一接口（推薦使用）
pub use unified_engine::{
    UnifiedTradingEngine,
    UnifiedEngineConfig,
    UnifiedEngineBuilder,
    EngineMode,
    RiskConfig,
    ExecutionConfig,
    PerformanceConfig,
    TradingStrategy,
    RiskManager,
    ExecutionManager,
    MarketData,
    Signal,
    SignalAction,
    Order,
    OrderType,
    OrderSide,
    OrderStatus,
    Portfolio,
    Position,
    RiskDecision,
    RiskReport,
    RiskLevel,
    StrategyState,
    OrderModification,
    Trade,
    EngineEvent,
};

pub use unified_risk_manager::{
    UnifiedRiskManager,
    RiskManagerConfig,
    create_default_risk_manager,
    create_conservative_risk_manager,
    create_aggressive_risk_manager,
};

// 導出新的完整OMS和風險管理器
pub use complete_oms::{
    CompleteOMS, OMSConfig, OMSStats, OrderStateMachine, Fill, OrderUpdate
};
pub use risk_manager::{
    RiskManager as CoreRiskManager, RiskLimits, RiskState, 
    RiskLevel as CoreRiskLevel, RiskEvent, OrderRiskDecision, 
    RiskConfig as CoreRiskConfig, RiskStats
};

// 導出核心功能 - 暫時註釋掉不存在的函數
// pub use strategy::run as strategy_run;
// pub use execution::run as execution_run;

// 舊版接口已移除

// 便捷函數：創建實時交易引擎
pub fn create_live_engine(
    symbols: Vec<String>,
    strategy: Box<dyn TradingStrategy>,
    dry_run: bool,
) -> UnifiedTradingEngine {
    let config = UnifiedEngineConfig {
        mode: EngineMode::Live {
            dry_run,
            enable_paper_trading: dry_run,
        },
        symbols,
        risk_config: RiskConfig {
            max_position_ratio: 0.1,
            max_loss_per_trade: 0.02,
            max_daily_loss: 0.05,
            max_leverage: 1.0,
            enable_dynamic_adjustment: true,
            kelly_fraction: Some(0.25),
        },
        execution_config: ExecutionConfig {
            preferred_order_type: OrderType::Limit,
            slippage_tolerance_bps: 10.0,
            order_timeout_ms: 5000,
            enable_smart_routing: true,
            enable_iceberg: false,
        },
        performance_config: PerformanceConfig {
            enable_metrics: true,
            metrics_update_interval_secs: 10,
            enable_latency_tracking: true,
            enable_memory_optimization: true,
        },
    };
    
    UnifiedEngineBuilder::new()
        .with_config(config)
        .with_strategy(strategy)
        .with_risk_manager(Box::new(create_default_risk_manager()))
        .with_execution_manager(Box::new(DummyExecutionManager))
        .build()
        .expect("Failed to build engine")
}

// 便捷函數：創建回測引擎
pub fn create_backtest_engine(
    symbols: Vec<String>,
    strategy: Box<dyn TradingStrategy>,
    start_time: u64,
    end_time: u64,
    initial_capital: f64,
) -> UnifiedTradingEngine {
    let config = UnifiedEngineConfig {
        mode: EngineMode::Backtest {
            start_time,
            end_time,
            initial_capital,
        },
        symbols,
        risk_config: RiskConfig {
            max_position_ratio: 0.1,
            max_loss_per_trade: 0.02,
            max_daily_loss: 0.05,
            max_leverage: 1.0,
            enable_dynamic_adjustment: true,
            kelly_fraction: Some(0.25),
        },
        execution_config: ExecutionConfig {
            preferred_order_type: OrderType::Market,
            slippage_tolerance_bps: 5.0,
            order_timeout_ms: 0, // 回測中立即執行
            enable_smart_routing: false,
            enable_iceberg: false,
        },
        performance_config: PerformanceConfig {
            enable_metrics: true,
            metrics_update_interval_secs: 60,
            enable_latency_tracking: false,
            enable_memory_optimization: false,
        },
    };
    
    UnifiedEngineBuilder::new()
        .with_config(config)
        .with_strategy(strategy)
        .with_risk_manager(Box::new(create_default_risk_manager()))
        .with_execution_manager(Box::new(DummyExecutionManager))
        .build()
        .expect("Failed to build engine")
}

// 臨時的執行管理器實現（用於示例）
struct DummyExecutionManager;

#[async_trait::async_trait]
impl ExecutionManager for DummyExecutionManager {
    async fn execute_signal(&mut self, signal: Signal) -> Result<Order, anyhow::Error> {
        Ok(Order {
            id: uuid::Uuid::new_v4().to_string(),
            symbol: signal.symbol,
            side: match signal.action {
                SignalAction::Buy => "Buy".to_string(),
                SignalAction::Sell => "Sell".to_string(),
                _ => "Buy".to_string(),
            },
            order_type: OrderType::Market,
            quantity: signal.quantity,
            price: signal.price,
            status: OrderStatus::New,
            // timestamp: chrono::Utc::now().timestamp_millis() as u64,
            filled_quantity: 0.0,
            // average_price: 0.0,
        })
    }
    
    async fn cancel_order(&mut self, _order_id: &str) -> Result<(), anyhow::Error> {
        Ok(())
    }
    
    async fn modify_order(&mut self, _order_id: &str, _new_params: OrderModification) -> Result<(), anyhow::Error> {
        Ok(())
    }
    
    async fn get_order_status(&self, _order_id: &str) -> Result<OrderStatus, anyhow::Error> {
        Ok(OrderStatus::New)
    }
}