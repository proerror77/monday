/*!
 * Engine Module - 交易策略和執行引擎
 * 
 * 包含策略實現和訂單執行邏輯
 */

pub mod strategy;
pub mod execution;
pub mod barter_execution;
pub mod risk_manager;
pub mod enhanced_risk_controller;
pub mod barter_engine;
pub mod barter_backtest;
pub mod dual_symbol_trading_framework;
pub mod test_capital_position_manager;
pub mod single_symbol_dl_trader;

// Re-export main functions and types
pub use strategy::run as strategy_run;
pub use execution::run as execution_run;
pub use barter_execution::run_barter_execution;
pub use risk_manager::{BarterRiskManager, RiskCheckResult, RiskStatus, RiskLevel};
pub use enhanced_risk_controller::{
    EnhancedRiskController, 
    EnhancedRiskConfig, 
    RiskAssessment, 
    PositionRisk, 
    PortfolioRiskMetrics,
    KellyParameters,
    TradeRecord,
    EnhancedRiskStats,
};
pub use barter_engine::{BarterUnifiedEngine, BarterEvent, BarterEngineBuilder, SystemControlEvent};
pub use barter_backtest::{BarterBacktestEngine, BacktestConfig, BacktestResults, FileDataSource};
pub use dual_symbol_trading_framework::{
    DualSymbolTradingFramework,
    DualSymbolTradingConfig,
    DualSymbolTradingSignal,
    TradingAction as DualTradingAction,
    SymbolTradingState,
    Position,
    SymbolPerformanceMetrics,
    FrameworkPerformanceMetrics,
    FrameworkStatus,
    SymbolStatus,
};
pub use test_capital_position_manager::{
    TestCapitalPositionManager,
    TestCapitalConfig,
    TestPosition,
    TestPortfolioMetrics,
    PositionAssessment,
    TestPortfolioStatus,
    PerformanceSummary,
    ClosedPosition,
};
pub use single_symbol_dl_trader::{
    BtcusdtDlTrader,
    BtcusdtDlConfig,
    MultiHorizonPrediction,
    BtcusdtTradingSignal,
    TradingAction,
    BtcusdtPerformanceMetrics,
};