//! Recovery Manager - orchestrates the complete recovery process

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn, error};
use ports::ExecutionEvent;

use oms_core::OmsCore;
use portfolio_core::Portfolio;
use ports::ExecutionClient;

use crate::{
    SystemSnapshot, RecoveryConfig, RecoveryError, RecoveryResult, RecoveryStats,
    StatePersistence, Reconciler,
};

/// Main recovery manager that orchestrates the complete recovery process
pub struct RecoveryManager {
    persistence: StatePersistence,
    reconciler: Reconciler,
    config: RecoveryConfig,
}

impl RecoveryManager {
    /// Create new recovery manager
    pub fn new(config: RecoveryConfig) -> Self {
        let persistence = StatePersistence::new(config.clone());
        let reconciler = Reconciler::new(config.reconciliation_timeout_secs);
        
        Self {
            persistence,
            reconciler,
            config,
        }
    }
    
    /// Initialize recovery system (create directories, etc.)
    pub async fn init(&self) -> RecoveryResult<()> {
        info!("Initializing recovery system");
        self.persistence.init().await?;
        Ok(())
    }
    
    /// Perform complete startup recovery
    pub async fn perform_startup_recovery<T: ExecutionClient>(
        &self,
        oms: Arc<Mutex<OmsCore>>,
        portfolio: Arc<Mutex<Portfolio>>,
        execution_clients: &[T],
    ) -> RecoveryResult<RecoveryStats> {
        let start_time = std::time::Instant::now();
        let mut stats = RecoveryStats::new();
        
        info!("Starting system recovery process");
        
        // Step 1: Try to load latest snapshot
        if let Some(snapshot) = self.persistence.load_latest_snapshot().await? {
            info!("Found valid snapshot from {}, restoring state", snapshot.timestamp);
            
            // Restore OMS state
            {
                let mut oms_guard = oms.lock().await;
                oms_guard.import_state(snapshot.oms_state);
                stats.orders_restored = oms_guard.order_count();
            }
            
            // Restore Portfolio state  
            {
                let mut portfolio_guard = portfolio.lock().await;
                portfolio_guard.import_state(snapshot.portfolio_state);
                stats.positions_restored = portfolio_guard.export_state().account_view.positions.len();
            }
            
            info!("State restored: {} orders, {} positions", 
                  stats.orders_restored, stats.positions_restored);
        } else {
            info!("No valid snapshot found, starting with clean state");
        }
        
        // Step 2: Perform exchange reconciliation if enabled
        if self.config.enable_reconciliation && !execution_clients.is_empty() {
            info!("Performing exchange reconciliation");
            
            for execution_client in execution_clients {
                match self.reconcile_with_client(oms.clone(), execution_client, &mut stats).await {
                    Ok(_) => {
                        info!("Successfully reconciled with exchange");
                    }
                    Err(e) => {
                        error!("Exchange reconciliation failed: {}", e);
                        // Continue with other exchanges or proceed without reconciliation
                    }
                }
            }
        }
        
        // Step 3: Validate recovered state
        self.validate_recovered_state(oms.clone(), portfolio.clone()).await?;
        
        stats.recovery_duration_ms = start_time.elapsed().as_millis() as u64;
        
        info!("Recovery completed in {}ms - {} orders, {} positions, {} reconciled",
              stats.recovery_duration_ms,
              stats.orders_restored,
              stats.positions_restored,
              stats.orders_reconciled);
        
        Ok(stats)
    }
    
    /// Save current system state snapshot
    pub async fn save_snapshot(
        &self,
        oms: Arc<Mutex<OmsCore>>,
        portfolio: Arc<Mutex<Portfolio>>,
        system_version: String,
    ) -> RecoveryResult<()> {
        info!("Creating system snapshot");
        
        // Export states
        let oms_state = {
            let oms_guard = oms.lock().await;
            oms_guard.export_state()
        };
        
        let portfolio_state = {
            let portfolio_guard = portfolio.lock().await;
            portfolio_guard.export_state()
        };
        
        // Create snapshot
        let snapshot = SystemSnapshot::new(oms_state, portfolio_state, system_version)
            .with_metadata("recovery_manager".to_string(), "auto_snapshot".to_string());
        
        // Save to disk
        self.persistence.save_snapshot(&snapshot).await?;
        
        info!("Snapshot saved successfully (ID: {})", snapshot.snapshot_id);
        Ok(())
    }
    
    /// Perform reconciliation with a specific exchange
    /// Reconcile local OMS state with a specific execution client (single venue)
    pub async fn reconcile_with_client<T: ExecutionClient>(
        &self,
        oms: Arc<Mutex<OmsCore>>,
        execution_client: &T,
        stats: &mut RecoveryStats,
    ) -> RecoveryResult<()> {
        let local_orders = {
            let oms_guard = oms.lock().await;
            oms_guard.export_state()
        };
        
        // Only reconcile open orders
        let open_orders = local_orders
            .into_iter()
            .filter(|(_, order)| {
                matches!(order.status, 
                    oms_core::OrderStatus::New | 
                    oms_core::OrderStatus::Acknowledged | 
                    oms_core::OrderStatus::PartiallyFilled)
            })
            .collect();
        
        let reconciliation_result = self.reconciler
            .reconcile(open_orders, execution_client)
            .await?;
        
        info!("Reconciliation found {} discrepancies", reconciliation_result.discrepancies.len());
        
        // Log discrepancies
        for discrepancy in &reconciliation_result.discrepancies {
            warn!("Discrepancy: {}", discrepancy.description());
        }
        
        // Auto-actions based on config
        for d in &reconciliation_result.discrepancies {
            match d {
                crate::reconciliation::Discrepancy::ExchangeOnly { exchange_order } => {
                    if self.config.auto_cancel_exchange_only {
                        info!("Auto-cancel exchange-only order: {}", exchange_order.order_id.0);
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_secs(self.config.reconciliation_timeout_secs),
                            execution_client.cancel_order(&exchange_order.order_id)
                        ).await;
                    } else {
                        // 可選：導入交易所訂單到本地（如果配置後續增加 import 選項）
                        // 目前僅記錄
                    }
                }
                crate::reconciliation::Discrepancy::LocalOnly { local_order } => {
                    if self.config.auto_mark_local_only_canceled {
                        info!("Mark local-only order as canceled: {}", local_order.order_id.0);
                        let ev = ExecutionEvent::OrderCanceled {
                            order_id: local_order.order_id.clone(),
                            timestamp: hft_core::now_micros(),
                        };
                        let _ = oms.lock().await.on_execution_event(&ev);
                    }
                }
                _ => {}
            }
        }

        // Resolve remaining discrepancies via reconciler (no-op placeholders for now)
        self.reconciler
            .resolve_discrepancies(reconciliation_result.discrepancies, execution_client, stats)
            .await?;

        // 額外：對狀態/數量不一致進行本地矯正（向上補齊，向下修正）
        let local_state = { let g = oms.lock().await; g.export_state() };
        let exch_state = self.reconciler
            .reconcile(local_state.clone(), execution_client)
            .await?
            .discrepancies; // reuse compare via reconcile (not ideal, but quick)

        for d in exch_state {
            match d {
                crate::reconciliation::Discrepancy::StatusMismatch { local_order, exchange_order } => {
                    info!("Recon-fix status: {} {:?}->{:?}", local_order.order_id.0, local_order.status, exchange_order.status);
                    let mut guard = oms.lock().await;
                    match exchange_order.status {
                        crate::reconciliation::ExchangeOrderStatus::Canceled => {
                            let ev = ExecutionEvent::OrderCanceled { order_id: local_order.order_id.clone(), timestamp: hft_core::now_micros() };
                            let _ = guard.on_execution_event(&ev);
                        }
                        crate::reconciliation::ExchangeOrderStatus::Filled | crate::reconciliation::ExchangeOrderStatus::PartiallyFilled => {
                            if exchange_order.filled_quantity.0 > local_order.cum_qty.0 {
                                let delta = hft_core::Quantity(exchange_order.filled_quantity.0 - local_order.cum_qty.0);
                                let fill_ev = ExecutionEvent::Fill {
                                    order_id: local_order.order_id.clone(),
                                    price: exchange_order.price.unwrap_or(local_order.avg_price.unwrap_or(hft_core::Price::from_f64(0.0).unwrap())),
                                    quantity: delta,
                                    timestamp: hft_core::now_micros(),
                                    fill_id: format!("RECOVER_FILL_{}", hft_core::now_micros()),
                                };
                                let _ = guard.on_execution_event(&fill_ev);
                            } else if exchange_order.filled_quantity.0 < local_order.cum_qty.0 {
                                // 向下修正：直接更新 OMS 累計數量
                                let _ = guard.update_filled_quantity(&local_order.order_id, exchange_order.filled_quantity, exchange_order.price);
                            }
                        }
                        _ => {
                            let _ = guard.update_status(&local_order.order_id, local_order.status);
                        }
                    }
                }
                crate::reconciliation::Discrepancy::QuantityMismatch { local_order, exchange_order } => {
                    info!("Recon-fix qty: {} local={} exch={}", local_order.order_id.0, local_order.cum_qty.0, exchange_order.filled_quantity.0);
                    let mut guard = oms.lock().await;
                    if exchange_order.filled_quantity.0 > local_order.cum_qty.0 {
                        let delta = hft_core::Quantity(exchange_order.filled_quantity.0 - local_order.cum_qty.0);
                        let fill_ev = ExecutionEvent::Fill {
                            order_id: local_order.order_id.clone(),
                            price: exchange_order.price.unwrap_or(local_order.avg_price.unwrap_or(hft_core::Price::from_f64(0.0).unwrap())),
                            quantity: delta,
                            timestamp: hft_core::now_micros(),
                            fill_id: format!("RECOVER_FILL_{}", hft_core::now_micros()),
                        };
                        let _ = guard.on_execution_event(&fill_ev);
                    } else if exchange_order.filled_quantity.0 < local_order.cum_qty.0 {
                        let _ = guard.update_filled_quantity(&local_order.order_id, exchange_order.filled_quantity, exchange_order.price);
                    }
                }
                _ => {}
            }
        }

        // Update stats
        stats.orders_reconciled += reconciliation_result.stats.matched_orders;
        stats.discrepancies_found += reconciliation_result.stats.local_only_orders
            + reconciliation_result.stats.exchange_only_orders
            + reconciliation_result.stats.status_mismatches
            + reconciliation_result.stats.quantity_mismatches;
        
        Ok(())
    }
    
    /// Validate recovered state for consistency
    async fn validate_recovered_state(
        &self,
        oms: Arc<Mutex<OmsCore>>,
        portfolio: Arc<Mutex<Portfolio>>,
    ) -> RecoveryResult<()> {
        info!("Validating recovered state");
        
        let validation_errors: Vec<String> = Vec::new();
        
        // Basic validation checks
        {
            let oms_guard = oms.lock().await;
            let portfolio_guard = portfolio.lock().await;
            
            let open_orders = oms_guard.get_open_orders();
            let account_view = portfolio_guard.reader().load();
            
            // Check for negative cash balance (warning, not error)
            if account_view.cash_balance < 0.0 {
                warn!("Negative cash balance detected: {:.2}", account_view.cash_balance);
            }
            
            // Check for positions without market prices
            for (symbol, position) in &account_view.positions {
                if position.quantity.0 != rust_decimal::Decimal::ZERO && position.unrealized_pnl == 0.0 {
                    warn!("Position in {} has no market value", symbol);
                }
            }
            
            info!("State validation completed: {} open orders, {} positions, cash: {:.2}",
                  open_orders.len(),
                  account_view.positions.len(),
                  account_view.cash_balance);
        }
        
        if !validation_errors.is_empty() {
            return Err(RecoveryError::StateInconsistency(
                format!("Validation failed: {:?}", validation_errors)
            ));
        }
        
        Ok(())
    }
    
    /// Get recovery system statistics
    pub async fn get_stats(&self) -> RecoveryResult<RecoverySystemStats> {
        let snapshot_stats = self.persistence.get_stats().await?;
        
        Ok(RecoverySystemStats {
            total_snapshots: snapshot_stats.total_snapshots,
            total_snapshot_size_bytes: snapshot_stats.total_size_bytes,
            oldest_snapshot: snapshot_stats.oldest_snapshot,
            newest_snapshot: snapshot_stats.newest_snapshot,
            config: self.config.clone(),
        })
    }
}

/// Recovery system statistics
#[derive(Debug, Clone)]
pub struct RecoverySystemStats {
    pub total_snapshots: usize,
    pub total_snapshot_size_bytes: u64,
    pub oldest_snapshot: Option<chrono::DateTime<chrono::Utc>>,
    pub newest_snapshot: Option<chrono::DateTime<chrono::Utc>>,
    pub config: RecoveryConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::collections::HashMap;
    use ports::*;
    use hft_core::*;
    use futures;
    
    // Mock execution client for testing
    struct MockExecutionClient;
    
    #[async_trait::async_trait]
    impl ExecutionClient for MockExecutionClient {
        async fn place_order(&mut self, _intent: OrderIntent) -> HftResult<OrderId> {
            Ok(OrderId("mock-order-1".to_string()))
        }
        
        async fn cancel_order(&mut self, _order_id: &OrderId) -> HftResult<()> {
            Ok(())
        }
        
        async fn modify_order(
            &mut self,
            _order_id: &OrderId,
            _new_quantity: Option<Quantity>,
            _new_price: Option<Price>
        ) -> HftResult<()> {
            Ok(())
        }
        
        async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
            Ok(Box::pin(futures::stream::empty()))
        }
        
        async fn connect(&mut self) -> HftResult<()> {
            Ok(())
        }
        
        async fn disconnect(&mut self) -> HftResult<()> {
            Ok(())
        }
        
        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: true,
                latency_ms: Some(1.0),
                last_heartbeat: 12345,
            }
        }
    }
    
    #[tokio::test]
    async fn test_recovery_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = RecoveryConfig {
            state_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        
        let manager = RecoveryManager::new(config);
        manager.init().await.unwrap();
    }
    
    #[tokio::test]
    async fn test_save_and_recovery() {
        let temp_dir = TempDir::new().unwrap();
        let config = RecoveryConfig {
            state_dir: temp_dir.path().to_path_buf(),
            enable_reconciliation: false, // Skip reconciliation for test
            ..Default::default()
        };
        
        let manager = RecoveryManager::new(config);
        manager.init().await.unwrap();
        
        let oms = Arc::new(Mutex::new(OmsCore::new()));
        let portfolio = Arc::new(Mutex::new(Portfolio::new()));
        
        // Save snapshot
        manager.save_snapshot(
            oms.clone(),
            portfolio.clone(),
            "test-v1.0.0".to_string(),
        ).await.unwrap();
        
        // Perform recovery
        let execution_clients: Vec<MockExecutionClient> = vec![];
        let stats = manager.perform_startup_recovery(
            oms,
            portfolio,
            &execution_clients,
        ).await.unwrap();
        
        // Should complete without error
        assert!(stats.recovery_duration_ms > 0);
    }
}
