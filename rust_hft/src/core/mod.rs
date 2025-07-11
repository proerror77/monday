/*!
 * Core Module - 核心類型和配置
 * 
 * 包含系統的基礎類型定義、配置管理、訂單簿實現和統一應用框架
 */

pub mod types;
pub mod config;
pub mod config_loader;
pub mod orderbook;
pub mod zero_alloc_orderbook;
pub mod app_runner;
pub mod cli;
pub mod workflow;
pub mod simple_workflow;
pub mod shutdown;
pub mod ultra_performance;
pub mod pipeline_manager;
pub mod unified_interface;
pub mod error;

// Re-export commonly used types
pub use types::*;
pub use config::Config;
pub use config_loader::{ConfigLoader, ResolvedConfig, HistoricalDataConfig};
pub use orderbook::OrderBook;
pub use zero_alloc_orderbook::{ZeroAllocOrderBook, PriceLevel, L2Snapshot, MemoryUsageStats};
pub use app_runner::{HftAppRunner, UnifiedReporter, run_simple_app, run_timed_app};
pub use cli::*;
pub use workflow::{WorkflowExecutor, WorkflowStep, StepResult, WorkflowConfig, WorkflowReport};
pub use simple_workflow::{WorkflowExecutor as SimpleWorkflowExecutor, WorkflowStep as SimpleWorkflowStep, WorkflowConfig as SimpleWorkflowConfig, WorkflowResult};
pub use shutdown::{
    ShutdownManager, ShutdownConfig, ShutdownSignal, ShutdownComponent, 
    ComponentState, SystemState, ShutdownStats, create_default_shutdown_manager, 
    create_fast_shutdown_manager
};
pub use ultra_performance::{
    ZeroAllocDecisionEngine, PerformanceStats, UltraFastOrderBookUpdater,
    PrefetchOptimizedCache, DecisionMetrics
};
pub use pipeline_manager::{
    AdvancedPipelineManager, PipelineDefinition, PipelineState, TaskType,
    DeploymentStrategy, ResourceManager, PipelineStats
};
pub use unified_interface::{
    UnifiedInterfaceManager, UnifiedRequest, UnifiedResponse, RequestType,
    ResponseStatus, ResponseData, SystemHealth
};
pub use error::{
    PipelineError, ConfigurationError, DataProcessingError, ModelError, 
    ExecutionError, ResourceError, ConcurrencyError, ExternalError, SystemError,
    ErrorContext, RecoveryStrategy, ErrorSeverity, PipelineResult, PipelineErrorExt
};
// Note: Pipeline types are re-exported from their respective modules