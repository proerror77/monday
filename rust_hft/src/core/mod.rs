/*!
 * Core Module - 核心類型和配置
 * 
 * 包含系統的基礎類型定義、配置管理、訂單簿實現和統一應用框架
 */

pub mod types;
pub mod config;
pub mod config_loader;
pub mod orderbook;
pub mod app_runner;
pub mod cli;
pub mod workflow;

// Re-export commonly used types
pub use types::*;
pub use config::Config;
pub use config_loader::{ConfigLoader, ResolvedConfig, HistoricalDataConfig};
pub use orderbook::OrderBook;
pub use app_runner::{HftAppRunner, UnifiedReporter, run_simple_app, run_timed_app};
pub use cli::*;
pub use workflow::{WorkflowExecutor, WorkflowStep, StepResult, WorkflowConfig, WorkflowReport};