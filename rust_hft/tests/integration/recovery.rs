//! Recovery system integration tests

use std::collections::HashMap;
use tempfile::TempDir;
use hft_recovery::*;
use oms_core::OmsCore;
use portfolio_core::Portfolio;
use hft_core::*;
use std::sync::Arc;
use tokio::sync::Mutex;

// Mock execution client for testing
struct MockExecutionClient;

impl ports::ExecutionClient for MockExecutionClient {
    // Mock implementation - in real system would be provided by exchange adapters
}

#[tokio::test]
async fn test_recovery_system_end_to_end() {
    let temp_dir = TempDir::new().unwrap();

    // Create recovery config
    let config = RecoveryConfig {
        state_dir: temp_dir.path().to_path_buf(),
        enable_reconciliation: false, // Skip reconciliation for this test
        ..RecoveryConfig::default()
    };

    let recovery_manager = RecoveryManager::new(config);
    recovery_manager.init().await.unwrap();

    // Create OMS and Portfolio with some test data
    let oms = Arc::new(Mutex::new(OmsCore::new()));
    let portfolio = Arc::new(Mutex::new(Portfolio::new()));

    // Add test order to OMS
    {
        let mut oms_guard = oms.lock().await;
        oms_guard.register_order(
            OrderId("test-order-1".to_string()),
            Some("client-1".to_string()),
            Symbol::new("BTCUSDT"),
            Side::Buy,
            Quantity::from_str("1.0").unwrap(),
            None,
            Some("test_strategy".to_string()),
        );
    }

    // Add test order metadata to portfolio
    {
        let mut portfolio_guard = portfolio.lock().await;
        portfolio_guard.register_order(
            OrderId("test-order-1".to_string()),
            Symbol::new("BTCUSDT"),
            Side::Buy,
        );
    }

    // Save snapshot
    recovery_manager.save_snapshot(
        oms.clone(),
        portfolio.clone(),
        "test-v1.0.0".to_string(),
    ).await.unwrap();

    // Create new instances (simulate restart)
    let new_oms = Arc::new(Mutex::new(OmsCore::new()));
    let new_portfolio = Arc::new(Mutex::new(Portfolio::new()));

    // Perform recovery
    let mut execution_clients: Vec<MockExecutionClient> = vec![];
    let stats = recovery_manager.perform_startup_recovery(
        new_oms.clone(),
        new_portfolio.clone(),
        execution_clients.as_mut_slice(),
    ).await.unwrap();

    // Verify recovery
    assert_eq!(stats.orders_restored, 1);
    assert!(stats.recovery_duration_ms > 0);

    // Verify OMS state is restored
    {
        let oms_guard = new_oms.lock().await;
        assert_eq!(oms_guard.order_count(), 1);

        let restored_order = oms_guard.get(&OrderId("test-order-1".to_string()));
        assert!(restored_order.is_some());
        assert_eq!(restored_order.unwrap().symbol.as_str(), "BTCUSDT");
    }
}

#[tokio::test]
async fn test_snapshot_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let config = RecoveryConfig {
        state_dir: temp_dir.path().to_path_buf(),
        max_snapshots: 2,
        ..RecoveryConfig::default()
    };

    let persistence = StatePersistence::new(config);
    persistence.init().await.unwrap();

    // Create test snapshots
    for i in 0..3 {
        let oms_state = HashMap::new();
        let portfolio_state = portfolio_core::PortfolioState {
            account_view: ports::AccountView::default(),
            order_meta: HashMap::new(),
            market_prices: HashMap::new(),
            processed_fill_ids: HashMap::new(),
        };

        let snapshot = SystemSnapshot::new(
            oms_state,
            portfolio_state,
            format!("test-v1.{}.0", i),
        );

        persistence.save_snapshot(&snapshot).await.unwrap();

        // Small delay to ensure different timestamps
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Should only have 2 snapshots (the limit)
    let snapshots = persistence.list_snapshots().await.unwrap();
    assert_eq!(snapshots.len(), 2);

    // Load latest snapshot
    let latest = persistence.load_latest_snapshot().await.unwrap();
    assert!(latest.is_some());
}

#[tokio::test]
async fn test_recovery_config_validation() {
    let temp_dir = TempDir::new().unwrap();

    let config = RecoveryConfig {
        state_dir: temp_dir.path().to_path_buf(),
        max_snapshot_age: chrono::Duration::seconds(1), // Very short for testing
        enable_reconciliation: true,
        reconciliation_timeout_secs: 5,
        max_snapshots: 5,
    };

    // Test that config values are correctly set
    assert_eq!(config.reconciliation_timeout_secs, 5);
    assert_eq!(config.max_snapshots, 5);
    assert!(config.enable_reconciliation);

    let recovery_manager = RecoveryManager::new(config);
    recovery_manager.init().await.unwrap();

    // Manager should be created successfully
    let stats = recovery_manager.get_stats().await.unwrap();
    assert_eq!(stats.total_snapshots, 0); // No snapshots yet
}

#[test]
fn test_recovery_stats() {
    let mut stats = RecoveryStats::new();

    stats.orders_restored = 10;
    stats.positions_restored = 5;
    stats.orders_reconciled = 8;
    stats.discrepancies_found = 2;
    stats.recovery_duration_ms = 1500;

    assert_eq!(stats.orders_restored, 10);
    assert_eq!(stats.positions_restored, 5);
    assert_eq!(stats.orders_reconciled, 8);
    assert_eq!(stats.discrepancies_found, 2);
    assert_eq!(stats.recovery_duration_ms, 1500);
}
