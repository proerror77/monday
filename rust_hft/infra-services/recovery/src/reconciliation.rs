//! Exchange reconciliation module
//!
//! Compares local state with exchange and resolves discrepancies

use std::collections::{HashMap, HashSet};
use tokio::time::{timeout, Duration};
use tracing::{debug, error, info, warn};

use hft_core::{OrderId, Price, Quantity, Side, Symbol};
use oms_core::{OrderRecord, OrderStatus};
use ports::{ExecutionClient, OrderStatus as PortsOrderStatus};

use crate::{RecoveryError, RecoveryResult, RecoveryStats};

/// Represents an order from the exchange
#[derive(Debug, Clone)]
pub struct ExchangeOrder {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: Quantity,
    pub filled_quantity: Quantity,
    pub price: Option<Price>,
    pub status: ExchangeOrderStatus,
    pub timestamp: u64,
}

/// Exchange order status (mapped from exchange-specific statuses)
#[derive(Debug, Clone, PartialEq)]
pub enum ExchangeOrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
}

impl From<ExchangeOrderStatus> for OrderStatus {
    fn from(status: ExchangeOrderStatus) -> Self {
        match status {
            ExchangeOrderStatus::New => OrderStatus::Acknowledged,
            ExchangeOrderStatus::PartiallyFilled => OrderStatus::PartiallyFilled,
            ExchangeOrderStatus::Filled => OrderStatus::Filled,
            ExchangeOrderStatus::Canceled => OrderStatus::Canceled,
            ExchangeOrderStatus::Rejected => OrderStatus::Rejected,
            ExchangeOrderStatus::Expired => OrderStatus::Expired,
        }
    }
}

/// Map ports::OrderStatus to ExchangeOrderStatus for reconciliation
fn map_order_status_to_exchange(status: &PortsOrderStatus) -> ExchangeOrderStatus {
    match status {
        PortsOrderStatus::New => ExchangeOrderStatus::New,
        PortsOrderStatus::Accepted => ExchangeOrderStatus::New,
        PortsOrderStatus::PartiallyFilled => ExchangeOrderStatus::PartiallyFilled,
        PortsOrderStatus::Filled => ExchangeOrderStatus::Filled,
        PortsOrderStatus::Canceled => ExchangeOrderStatus::Canceled,
        PortsOrderStatus::Rejected => ExchangeOrderStatus::Rejected,
        PortsOrderStatus::Expired => ExchangeOrderStatus::Expired,
    }
}

/// Discrepancy between local and exchange state
#[derive(Debug, Clone)]
pub enum Discrepancy {
    /// Order exists locally but not on exchange
    LocalOnly { local_order: OrderRecord },
    /// Order exists on exchange but not locally
    ExchangeOnly { exchange_order: ExchangeOrder },
    /// Order exists in both but has different status
    StatusMismatch {
        local_order: OrderRecord,
        exchange_order: ExchangeOrder,
    },
    /// Order exists in both but has different filled quantity
    QuantityMismatch {
        local_order: OrderRecord,
        exchange_order: ExchangeOrder,
    },
}

impl Discrepancy {
    pub fn order_id(&self) -> &OrderId {
        match self {
            Discrepancy::LocalOnly { local_order } => &local_order.order_id,
            Discrepancy::ExchangeOnly { exchange_order } => &exchange_order.order_id,
            Discrepancy::StatusMismatch { local_order, .. } => &local_order.order_id,
            Discrepancy::QuantityMismatch { local_order, .. } => &local_order.order_id,
        }
    }

    pub fn description(&self) -> String {
        match self {
            Discrepancy::LocalOnly { local_order } => {
                format!(
                    "Order {} exists locally ({:?}) but not on exchange",
                    local_order.order_id.0, local_order.status
                )
            }
            Discrepancy::ExchangeOnly { exchange_order } => {
                format!(
                    "Order {} exists on exchange ({:?}) but not locally",
                    exchange_order.order_id.0, exchange_order.status
                )
            }
            Discrepancy::StatusMismatch {
                local_order,
                exchange_order,
            } => {
                format!(
                    "Order {} status mismatch: local={:?}, exchange={:?}",
                    local_order.order_id.0, local_order.status, exchange_order.status
                )
            }
            Discrepancy::QuantityMismatch {
                local_order,
                exchange_order,
            } => {
                format!(
                    "Order {} filled quantity mismatch: local={}, exchange={}",
                    local_order.order_id.0, local_order.cum_qty.0, exchange_order.filled_quantity.0
                )
            }
        }
    }
}

/// Reconciliation result
#[derive(Debug)]
pub struct ReconciliationResult {
    pub discrepancies: Vec<Discrepancy>,
    pub stats: ReconciliationStats,
}

/// Reconciliation statistics
#[derive(Debug, Clone, Default)]
pub struct ReconciliationStats {
    pub local_orders: usize,
    pub exchange_orders: usize,
    pub matched_orders: usize,
    pub local_only_orders: usize,
    pub exchange_only_orders: usize,
    pub status_mismatches: usize,
    pub quantity_mismatches: usize,
    pub reconciliation_duration_ms: u64,
}

/// Exchange reconciliation engine
pub struct Reconciler {
    timeout_duration: Duration,
}

impl Reconciler {
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            timeout_duration: Duration::from_secs(timeout_secs),
        }
    }

    /// Perform reconciliation between local OMS state and exchange
    pub async fn reconcile<T: ExecutionClient>(
        &self,
        local_orders: HashMap<OrderId, OrderRecord>,
        execution_client: &T,
    ) -> RecoveryResult<ReconciliationResult> {
        let start_time = std::time::Instant::now();
        info!(
            "Starting order reconciliation with {} local orders",
            local_orders.len()
        );

        // Get open orders from exchange with timeout
        let exchange_orders = timeout(
            self.timeout_duration,
            self.fetch_exchange_orders(execution_client),
        )
        .await
        .map_err(|_| RecoveryError::Timeout)?
        .map_err(|e| RecoveryError::ReconciliationFailed(e.to_string()))?;

        info!("Fetched {} orders from exchange", exchange_orders.len());

        // Compare local and exchange states
        let discrepancies = self.compare_orders(&local_orders, &exchange_orders);

        let stats = ReconciliationStats {
            local_orders: local_orders.len(),
            exchange_orders: exchange_orders.len(),
            matched_orders: self.count_matched_orders(&local_orders, &exchange_orders),
            local_only_orders: self.count_local_only(&discrepancies),
            exchange_only_orders: self.count_exchange_only(&discrepancies),
            status_mismatches: self.count_status_mismatches(&discrepancies),
            quantity_mismatches: self.count_quantity_mismatches(&discrepancies),
            reconciliation_duration_ms: start_time.elapsed().as_millis() as u64,
        };

        info!(
            "Reconciliation completed: {} discrepancies found in {}ms",
            discrepancies.len(),
            stats.reconciliation_duration_ms
        );

        Ok(ReconciliationResult {
            discrepancies,
            stats,
        })
    }

    /// Resolve discrepancies by taking appropriate actions
    pub async fn resolve_discrepancies<T: ExecutionClient>(
        &self,
        discrepancies: Vec<Discrepancy>,
        execution_client: &T,
        recovery_stats: &mut RecoveryStats,
    ) -> RecoveryResult<()> {
        info!("Resolving {} discrepancies", discrepancies.len());

        for discrepancy in discrepancies {
            match self
                .resolve_single_discrepancy(discrepancy, execution_client)
                .await
            {
                Ok(_) => {
                    recovery_stats.orders_reconciled += 1;
                }
                Err(e) => {
                    error!("Failed to resolve discrepancy: {}", e);
                    recovery_stats.discrepancies_found += 1;
                }
            }
        }

        Ok(())
    }

    /// Fetch orders from exchange using ExecutionClient
    async fn fetch_exchange_orders<T: ExecutionClient>(
        &self,
        execution_client: &T,
    ) -> Result<HashMap<OrderId, ExchangeOrder>, Box<dyn std::error::Error>> {
        info!("正在從交易所獲取未結訂單列表...");

        let open_orders = execution_client
            .list_open_orders()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;

        let mut exchange_orders = HashMap::new();

        for open_order in open_orders {
            let exchange_order = ExchangeOrder {
                order_id: open_order.order_id.clone(),
                symbol: open_order.symbol,
                side: open_order.side,
                quantity: open_order.remaining_quantity, // 使用剩餘數量
                filled_quantity: open_order.filled_quantity,
                price: open_order.price,
                status: map_order_status_to_exchange(&open_order.status),
                timestamp: open_order.updated_at,
            };

            exchange_orders.insert(open_order.order_id, exchange_order);
        }

        info!("成功獲取 {} 個交易所訂單", exchange_orders.len());
        Ok(exchange_orders)
    }

    /// Compare local and exchange orders to find discrepancies
    fn compare_orders(
        &self,
        local_orders: &HashMap<OrderId, OrderRecord>,
        exchange_orders: &HashMap<OrderId, ExchangeOrder>,
    ) -> Vec<Discrepancy> {
        let mut discrepancies = Vec::new();

        // Find all order IDs
        let mut all_order_ids = HashSet::new();
        all_order_ids.extend(local_orders.keys());
        all_order_ids.extend(exchange_orders.keys());

        for order_id in all_order_ids {
            match (local_orders.get(order_id), exchange_orders.get(order_id)) {
                (Some(local), None) => {
                    // Order exists locally but not on exchange
                    if matches!(
                        local.status,
                        OrderStatus::New | OrderStatus::Acknowledged | OrderStatus::PartiallyFilled
                    ) {
                        discrepancies.push(Discrepancy::LocalOnly {
                            local_order: local.clone(),
                        });
                    }
                }
                (None, Some(exchange)) => {
                    // Order exists on exchange but not locally
                    discrepancies.push(Discrepancy::ExchangeOnly {
                        exchange_order: exchange.clone(),
                    });
                }
                (Some(local), Some(exchange)) => {
                    // Order exists in both - check for mismatches
                    let expected_local_status = OrderStatus::from(exchange.status.clone());

                    if local.status != expected_local_status {
                        discrepancies.push(Discrepancy::StatusMismatch {
                            local_order: local.clone(),
                            exchange_order: exchange.clone(),
                        });
                    } else if (local.cum_qty.0 - exchange.filled_quantity.0).abs()
                        > rust_decimal::Decimal::new(1, 8)
                    {
                        // Check quantity mismatch (with small tolerance for precision)
                        discrepancies.push(Discrepancy::QuantityMismatch {
                            local_order: local.clone(),
                            exchange_order: exchange.clone(),
                        });
                    }
                }
                (None, None) => {
                    // This shouldn't happen as we iterate over union of keys
                    unreachable!("Order ID found in neither local nor exchange orders");
                }
            }
        }

        discrepancies
    }

    /// Resolve a single discrepancy
    async fn resolve_single_discrepancy<T: ExecutionClient>(
        &self,
        discrepancy: Discrepancy,
        _execution_client: &T,
    ) -> RecoveryResult<()> {
        match discrepancy {
            Discrepancy::LocalOnly { local_order } => {
                // Order exists locally but not on exchange - might have been canceled outside our system
                warn!(
                    "Order {} exists locally but not on exchange - assuming canceled externally",
                    local_order.order_id.0
                );
                // TODO: Update local OMS to mark as canceled
            }
            Discrepancy::ExchangeOnly { exchange_order } => {
                // Order exists on exchange but not locally - add to local OMS
                info!(
                    "Order {} exists on exchange but not locally - adding to local state",
                    exchange_order.order_id.0
                );
                // TODO: Add order to local OMS
            }
            Discrepancy::StatusMismatch {
                local_order,
                exchange_order,
            } => {
                // Status mismatch - update local status to match exchange
                info!(
                    "Updating order {} status from {:?} to {:?}",
                    local_order.order_id.0, local_order.status, exchange_order.status
                );
                // TODO: Update local OMS status
            }
            Discrepancy::QuantityMismatch {
                local_order,
                exchange_order,
            } => {
                // Quantity mismatch - update local filled quantity to match exchange
                info!(
                    "Updating order {} filled quantity from {} to {}",
                    local_order.order_id.0, local_order.cum_qty.0, exchange_order.filled_quantity.0
                );
                // TODO: Update local OMS filled quantity
            }
        }

        Ok(())
    }

    // Helper methods for counting different types of discrepancies
    fn count_matched_orders(
        &self,
        local_orders: &HashMap<OrderId, OrderRecord>,
        exchange_orders: &HashMap<OrderId, ExchangeOrder>,
    ) -> usize {
        local_orders
            .keys()
            .filter(|id| exchange_orders.contains_key(id))
            .count()
    }

    fn count_local_only(&self, discrepancies: &[Discrepancy]) -> usize {
        discrepancies
            .iter()
            .filter(|d| matches!(d, Discrepancy::LocalOnly { .. }))
            .count()
    }

    fn count_exchange_only(&self, discrepancies: &[Discrepancy]) -> usize {
        discrepancies
            .iter()
            .filter(|d| matches!(d, Discrepancy::ExchangeOnly { .. }))
            .count()
    }

    fn count_status_mismatches(&self, discrepancies: &[Discrepancy]) -> usize {
        discrepancies
            .iter()
            .filter(|d| matches!(d, Discrepancy::StatusMismatch { .. }))
            .count()
    }

    fn count_quantity_mismatches(&self, discrepancies: &[Discrepancy]) -> usize {
        discrepancies
            .iter()
            .filter(|d| matches!(d, Discrepancy::QuantityMismatch { .. }))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures;
    use hft_core::*;
    use ports::*;
    use std::collections::HashMap;

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
            _new_price: Option<Price>,
        ) -> HftResult<()> {
            Ok(())
        }

        async fn execution_stream(&self) -> HftResult<BoxStream<ExecutionEvent>> {
            Ok(Box::pin(futures::stream::empty()))
        }

        async fn list_open_orders(&self) -> HftResult<Vec<OpenOrder>> {
            Ok(Vec::new())
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
    async fn test_reconciler_creation() {
        let reconciler = Reconciler::new(30);
        assert_eq!(reconciler.timeout_duration, Duration::from_secs(30));
    }

    #[test]
    fn test_discrepancy_description() {
        use hft_core::*;

        let order_id = OrderId("test-123".to_string());
        let local_order = OrderRecord {
            order_id: order_id.clone(),
            client_order_id: None,
            symbol: Symbol("BTCUSDT".to_string()),
            side: Side::Buy,
            qty: Quantity::from_str("1.0").unwrap(),
            cum_qty: Quantity::from_str("0.5").unwrap(),
            avg_price: Some(Price::from_str("50000").unwrap()),
            status: oms_core::OrderStatus::PartiallyFilled,
            venue: None,
            strategy_id: Some("test_strategy".to_string()),
            processed_fill_ids: std::collections::HashSet::new(),
        };

        let discrepancy = Discrepancy::LocalOnly {
            local_order: local_order.clone(),
        };

        let description = discrepancy.description();
        assert!(description.contains("test-123"));
        assert!(description.contains("locally"));
        assert!(description.contains("not on exchange"));
    }
}
