//! Common types and utilities shared across all execution adapters
//!
//! This module consolidates repeated patterns found in execution adapters
//! to reduce code duplication and improve maintainability.

use crate::{ExecutionAlert, ResilientExecutor};
use std::sync::Arc;

/// Execution mode for adapters
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionMode {
    /// Paper trading (simulated execution)
    #[default]
    Paper,
    /// Live trading (real execution)
    Live,
    /// Testnet trading (real API, fake money)
    Testnet,
}

/// Alert callback type for execution alerts
///
/// Used by adapters to notify external systems about circuit breaker
/// state changes and other execution-related events.
pub type AlertCallback = Arc<dyn Fn(ExecutionAlert) + Send + Sync>;

/// Common resilience configuration for adapters
#[derive(Debug, Clone)]
pub struct AdapterResilienceConfig {
    /// Maximum number of retry attempts
    pub max_retries: u32,
    /// Initial delay between retries in milliseconds
    pub initial_delay_ms: u64,
    /// Maximum delay between retries in milliseconds
    pub max_delay_ms: u64,
    /// Backoff multiplier for exponential backoff
    pub backoff_multiplier: f64,
    /// Number of failures before circuit breaker opens
    pub failure_threshold: u32,
    /// Duration circuit breaker stays open in seconds
    pub open_duration_secs: u64,
    /// Maximum requests allowed in half-open state
    pub half_open_max_requests: u32,
    /// Successes needed to close circuit breaker
    pub half_open_success_threshold: u32,
}

impl Default for AdapterResilienceConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_multiplier: 2.0,
            failure_threshold: 5,
            open_duration_secs: 30,
            half_open_max_requests: 3,
            half_open_success_threshold: 2,
        }
    }
}

/// Trait for adapters with resilience capabilities
pub trait ResilientAdapter {
    /// Get the resilient executor
    fn resilient_executor(&self) -> Option<&Arc<ResilientExecutor>>;

    /// Get the alert callback
    fn alert_callback(&self) -> Option<&AlertCallback>;

    /// Check if resilience is enabled
    fn resilience_enabled(&self) -> bool {
        self.resilient_executor().is_some()
    }
}
