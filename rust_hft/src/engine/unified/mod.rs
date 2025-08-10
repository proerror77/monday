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
pub use config::{UnifiedEngineConfig, RiskConfig, ExecutionConfig, PerformanceConfig};
pub use engine::UnifiedTradingEngine;
pub use builder::UnifiedEngineBuilder;
pub use modes::{EngineMode, LiveMode, BacktestMode};