//! Transitional execution interface for the imported prediction-market module.
//!
//! Real venue connectivity belongs to Monday's data-pipeline and execution
//! Adapters. This compatibility crate deliberately exposes only a fail-closed
//! gateway plus deterministic test fakes for the imported paper/control-plane
//! runtime. It contains no credentials, wallet code, network client, or venue SDK.

use ploy_trading::{FillRecord, TradeSide};
use rust_decimal::Decimal;
use thiserror::Error;

pub const CRATE_MARKER: &str = "ploy-connectivity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderExecutionType {
    GTC,
    #[default]
    FAK,
    FOK,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionRequest {
    pub order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    pub order_type: OrderExecutionType,
    pub aggressive_ticks: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedOrder {
    pub order_id: String,
    pub venue_order_id: String,
    pub token_id: String,
    pub side: TradeSide,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OrderObservation {
    Acknowledged {
        order_id: String,
        venue_order_id: String,
    },
    Canceled {
        order_id: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileBatch {
    pub fills: Vec<FillRecord>,
    pub order_observations: Vec<OrderObservation>,
}

impl ReconcileBatch {
    #[must_use]
    pub fn fills_only(fills: Vec<FillRecord>) -> Self {
        Self {
            fills,
            order_observations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancellationRequest {
    pub order_id: String,
    pub venue_order_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReplaceRequest {
    pub order_id: String,
    pub venue_order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionOutcome {
    Acknowledged { venue_order_id: String },
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CancellationOutcome {
    Canceled,
    Rejected { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplaceOutcome {
    Replaced { venue_order_id: String },
    Rejected { reason: String },
    PartialFailure { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecutionError {
    #[error("live execution misconfigured: {0}")]
    Configuration(String),
    #[error("live execution validation failed: {0}")]
    Validation(String),
    #[error("live execution transport failed: {0}")]
    Transport(String),
}

/// Compatibility interface used by the imported paper/control-plane runtime.
///
/// Production construction always installs [`DisabledLiveExecutionGateway`].
/// Test code may inject [`StaticExecutionGateway`] to verify state transitions.
/// Concrete venue Adapters must never be implemented in this crate.
pub trait LiveExecutionGateway: Send + Sync + std::fmt::Debug {
    fn probe(&self) -> Result<(), ExecutionError>;

    fn submit(&self, request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError>;

    fn cancel(&self, request: &CancellationRequest) -> Result<CancellationOutcome, ExecutionError>;

    fn replace(&self, request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError>;

    fn reconcile_fills(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError>;

    fn reconcile_updates(
        &self,
        tracked_orders: &[TrackedOrder],
    ) -> Result<ReconcileBatch, ExecutionError> {
        self.reconcile_fills(tracked_orders)
            .map(ReconcileBatch::fills_only)
    }
}

pub const MONDAY_LIVE_EXECUTION_DISABLED: &str =
    "prediction-market compatibility execution is disabled; Monday runtime is the only production execution authority";

fn monday_live_execution_disabled_error() -> ExecutionError {
    ExecutionError::Configuration(MONDAY_LIVE_EXECUTION_DISABLED.to_string())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DisabledLiveExecutionGateway;

impl LiveExecutionGateway for DisabledLiveExecutionGateway {
    fn probe(&self) -> Result<(), ExecutionError> {
        Err(monday_live_execution_disabled_error())
    }

    fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        Err(monday_live_execution_disabled_error())
    }

    fn cancel(
        &self,
        _request: &CancellationRequest,
    ) -> Result<CancellationOutcome, ExecutionError> {
        Err(monday_live_execution_disabled_error())
    }

    fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
        Err(monday_live_execution_disabled_error())
    }

    fn reconcile_fills(
        &self,
        _tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError> {
        Err(monday_live_execution_disabled_error())
    }
}

#[derive(Debug, Clone)]
pub struct StaticExecutionGateway {
    probe_result: Result<(), ExecutionError>,
    result: Result<ExecutionOutcome, ExecutionError>,
    cancel_result: Result<CancellationOutcome, ExecutionError>,
    replace_result: Result<ReplaceOutcome, ExecutionError>,
    reconcile_result: Result<ReconcileBatch, ExecutionError>,
}

impl StaticExecutionGateway {
    #[must_use]
    pub fn acknowledged(venue_order_id: impl Into<String>) -> Self {
        let venue_order_id = venue_order_id.into();
        Self {
            probe_result: Ok(()),
            result: Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: venue_order_id.clone(),
            }),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Ok(ReplaceOutcome::Replaced {
                venue_order_id: format!("{venue_order_id}-replaced"),
            }),
            reconcile_result: Ok(ReconcileBatch::default()),
        }
    }

    #[must_use]
    pub fn rejected(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            probe_result: Ok(()),
            result: Ok(ExecutionOutcome::Rejected {
                reason: reason.clone(),
            }),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Ok(ReplaceOutcome::Rejected { reason }),
            reconcile_result: Ok(ReconcileBatch::default()),
        }
    }

    #[must_use]
    pub fn failed(error: ExecutionError) -> Self {
        Self {
            probe_result: Ok(()),
            result: Err(error.clone()),
            cancel_result: Ok(CancellationOutcome::Canceled),
            replace_result: Err(error),
            reconcile_result: Ok(ReconcileBatch::default()),
        }
    }

    #[must_use]
    pub fn with_cancel_result(
        mut self,
        result: Result<CancellationOutcome, ExecutionError>,
    ) -> Self {
        self.cancel_result = result;
        self
    }

    #[must_use]
    pub fn with_probe_result(mut self, result: Result<(), ExecutionError>) -> Self {
        self.probe_result = result;
        self
    }

    #[must_use]
    pub fn with_replace_result(mut self, result: Result<ReplaceOutcome, ExecutionError>) -> Self {
        self.replace_result = result;
        self
    }

    #[must_use]
    pub fn with_reconciled_fills(mut self, fills: Vec<FillRecord>) -> Self {
        self.reconcile_result = Ok(ReconcileBatch::fills_only(fills));
        self
    }

    #[must_use]
    pub fn with_reconciled_updates(mut self, updates: ReconcileBatch) -> Self {
        self.reconcile_result = Ok(updates);
        self
    }

    #[must_use]
    pub fn with_reconcile_result(
        mut self,
        result: Result<Vec<FillRecord>, ExecutionError>,
    ) -> Self {
        self.reconcile_result = result.map(ReconcileBatch::fills_only);
        self
    }
}

impl LiveExecutionGateway for StaticExecutionGateway {
    fn probe(&self) -> Result<(), ExecutionError> {
        self.probe_result.clone()
    }

    fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
        self.result.clone()
    }

    fn cancel(
        &self,
        _request: &CancellationRequest,
    ) -> Result<CancellationOutcome, ExecutionError> {
        self.cancel_result.clone()
    }

    fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
        self.replace_result.clone()
    }

    fn reconcile_fills(
        &self,
        _tracked_orders: &[TrackedOrder],
    ) -> Result<Vec<FillRecord>, ExecutionError> {
        self.reconcile_result.clone().map(|batch| batch.fills)
    }

    fn reconcile_updates(
        &self,
        _tracked_orders: &[TrackedOrder],
    ) -> Result<ReconcileBatch, ExecutionError> {
        self.reconcile_result.clone()
    }
}

#[must_use]
pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}

#[cfg(test)]
mod tests {
    use super::{
        CancellationRequest, DisabledLiveExecutionGateway, ExecutionError, ExecutionRequest,
        LiveExecutionGateway, OrderExecutionType, ReplaceRequest, StaticExecutionGateway,
        TrackedOrder, MONDAY_LIVE_EXECUTION_DISABLED,
    };
    use ploy_trading::TradeSide;
    use rust_decimal_macros::dec;

    fn execution_request() -> ExecutionRequest {
        ExecutionRequest {
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.55)),
            order_type: OrderExecutionType::GTC,
            aggressive_ticks: 0,
        }
    }

    #[test]
    fn static_gateway_returns_acknowledged_outcome() {
        let gateway = StaticExecutionGateway::acknowledged("venue-order-1");
        let outcome = gateway.submit(&execution_request()).expect("ack outcome");
        assert_eq!(
            outcome,
            super::ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-order-1".to_string()
            }
        );
    }

    #[test]
    fn disabled_gateway_rejects_every_live_operation() {
        let gateway = DisabledLiveExecutionGateway;
        let cancel = CancellationRequest {
            order_id: "order-1".to_string(),
            venue_order_id: "venue-order-1".to_string(),
        };
        let replace = ReplaceRequest {
            order_id: "order-1".to_string(),
            venue_order_id: "venue-order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.56)),
        };
        let tracked = TrackedOrder {
            order_id: "order-1".to_string(),
            venue_order_id: "venue-order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
        };
        let expected = ExecutionError::Configuration(MONDAY_LIVE_EXECUTION_DISABLED.to_string());

        assert_eq!(gateway.probe(), Err(expected.clone()));
        assert_eq!(gateway.submit(&execution_request()), Err(expected.clone()));
        assert_eq!(gateway.cancel(&cancel), Err(expected.clone()));
        assert_eq!(gateway.replace(&replace), Err(expected.clone()));
        assert_eq!(gateway.reconcile_fills(&[tracked]), Err(expected));
    }
}
