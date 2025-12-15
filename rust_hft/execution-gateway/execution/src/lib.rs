//! Execution layer
//! - Unifies live (REST + private WS) and simulation under `ports::ExecutionClient`
//! - No strategy or risk logic here

pub mod adapter_common;
pub mod resilience;

pub use adapter_common::{
    AdapterResilienceConfig, AlertCallback, ExecutionMode, ResilientAdapter,
};
pub use resilience::{
    CircuitBreaker, CircuitBreakerConfig, CircuitState,
    ExecutionAlert, ExecutionAlertType, ExecutorStats,
    ResilientExecutor, RetryConfig,
    is_retryable, retry_with_backoff,
};
