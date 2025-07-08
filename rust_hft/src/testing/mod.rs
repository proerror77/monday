/*!
 * Testing Module - 模擬測試框架
 * 
 * 提供完整的交易系統測試能力
 */

pub mod simulation_framework;

// Re-export main testing components
pub use simulation_framework::{
    SimulationFramework,
    SimulationConfig,
    SimulationResults,
    TestScenario,
    MarketSimulator,
    SimulationExecutionMetrics,
    TradingPerformanceMetrics,
    RiskMetrics,
    SystemHealthMetrics,
    SystemRecommendation,
    ValidationResults,
    MarketTick,
    MarketState,
    TradingSignal,
};