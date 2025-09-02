//! HFT Recovery System
//! 
//! Handles startup recovery and state reconciliation:
//! - Persist and restore OMS/Portfolio state
//! - Reconcile with exchange open orders
//! - Detect and handle state inconsistencies

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use tracing::{info, warn, error, debug};

pub mod persistence;
pub mod reconciliation;
pub mod recovery_manager;

pub use persistence::*;
pub use reconciliation::*;
pub use recovery_manager::*;

/// Recovery system errors
#[derive(thiserror::Error, Debug)]
pub enum RecoveryError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    
    #[error("State inconsistency: {0}")]
    StateInconsistency(String),
    
    #[error("Exchange reconciliation failed: {0}")]
    ReconciliationFailed(String),
    
    #[error("Recovery timeout: operation took too long")]
    Timeout,
    
    #[error("Recovery failed: {0}")]
    RecoveryFailed(String),
}

pub type RecoveryResult<T> = Result<T, RecoveryError>;

/// System state snapshot that can be persisted
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemSnapshot {
    /// Unique snapshot ID
    pub snapshot_id: Uuid,
    /// Timestamp when snapshot was created
    pub timestamp: DateTime<Utc>,
    /// System version/build info
    pub system_version: String,
    /// OMS state
    pub oms_state: HashMap<hft_core::OrderId, oms_core::OrderRecord>,
    /// Portfolio state 
    pub portfolio_state: portfolio_core::PortfolioState,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

impl SystemSnapshot {
    pub fn new(
        oms_state: HashMap<hft_core::OrderId, oms_core::OrderRecord>,
        portfolio_state: portfolio_core::PortfolioState,
        system_version: String,
    ) -> Self {
        Self {
            snapshot_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            system_version,
            oms_state,
            portfolio_state,
            metadata: HashMap::new(),
        }
    }
    
    /// Add metadata to the snapshot
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
    
    /// Get the age of this snapshot
    pub fn age(&self) -> chrono::Duration {
        Utc::now() - self.timestamp
    }
    
    /// Check if snapshot is recent (less than specified duration)
    pub fn is_recent(&self, max_age: chrono::Duration) -> bool {
        self.age() < max_age
    }
}

/// Recovery configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryConfig {
    /// Directory to store state snapshots
    pub state_dir: PathBuf,
    /// Maximum age of snapshot to consider for recovery
    pub max_snapshot_age: chrono::Duration,
    /// Whether to perform exchange reconciliation on startup
    pub enable_reconciliation: bool,
    /// Timeout for reconciliation operations
    pub reconciliation_timeout_secs: u64,
    /// Number of snapshots to keep
    pub max_snapshots: usize,
    /// Auto-cancel orders found on exchange but not locally
    #[serde(default)]
    pub auto_cancel_exchange_only: bool,
    /// Auto-mark local-only orders as canceled
    #[serde(default)]
    pub auto_mark_local_only_canceled: bool,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from("data/state"),
            max_snapshot_age: chrono::Duration::hours(24),
            enable_reconciliation: true,
            reconciliation_timeout_secs: 30,
            max_snapshots: 10,
            auto_cancel_exchange_only: false,
            auto_mark_local_only_canceled: true,
        }
    }
}

/// Recovery statistics
#[derive(Debug, Clone, Default)]
pub struct RecoveryStats {
    /// Number of orders restored
    pub orders_restored: usize,
    /// Number of positions restored
    pub positions_restored: usize,
    /// Number of orders reconciled with exchange
    pub orders_reconciled: usize,
    /// Number of discrepancies found
    pub discrepancies_found: usize,
    /// Recovery duration in milliseconds
    pub recovery_duration_ms: u64,
}

impl RecoveryStats {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_core::*;
    use std::str::FromStr;
    
    #[test]
    fn test_system_snapshot_creation() {
        let oms_state = HashMap::new();
        let portfolio_state = portfolio_core::PortfolioState {
            account_view: ports::AccountView::default(),
            order_meta: HashMap::new(),
            market_prices: HashMap::new(),
        };
        
        let snapshot = SystemSnapshot::new(
            oms_state,
            portfolio_state,
            "test-v1.0.0".to_string(),
        ).with_metadata("test_key".to_string(), "test_value".to_string());
        
        assert!(snapshot.snapshot_id != Uuid::nil());
        assert_eq!(snapshot.system_version, "test-v1.0.0");
        assert_eq!(snapshot.metadata.get("test_key"), Some(&"test_value".to_string()));
        assert!(snapshot.is_recent(chrono::Duration::minutes(1)));
    }
    
    #[test]
    fn test_recovery_config_default() {
        let config = RecoveryConfig::default();
        assert_eq!(config.state_dir, PathBuf::from("data/state"));
        assert_eq!(config.max_snapshot_age, chrono::Duration::hours(24));
        assert!(config.enable_reconciliation);
        assert_eq!(config.reconciliation_timeout_secs, 30);
        assert_eq!(config.max_snapshots, 10);
    }
}
