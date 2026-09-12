pub mod bootstrap;
pub mod deployment_control;
pub mod execution_client;
pub mod health_runtime;
pub mod proposals;
pub mod reconcile;
pub mod runtime_support;
pub mod state_io;
pub mod trade_control;
pub mod trade_submit;
pub mod worker_tick;

#[cfg(test)]
pub(crate) mod test_support {
    use async_trait::async_trait;
    use hft_core::{HftError, OrderId, Price, Quantity};
    use portfolio_core::prediction::{FillRecord, TradeSide};
    use ports::{
        AccountBalance, AccountFill, BoxStream, ConnectionHealth, ExecutionClient, ExecutionEvent,
        OpenOrder, OrderIntent, Position,
    };

    #[derive(Debug, Clone)]
    pub(crate) struct StaticExecutionGateway {
        submit_result: Result<String, HftError>,
        cancel_result: Result<(), HftError>,
        replace_result: Result<OrderId, HftError>,
        fills: Vec<AccountFill>,
        events: Vec<ExecutionEvent>,
    }

    impl StaticExecutionGateway {
        pub(crate) fn acknowledged(venue_order_id: impl Into<String>) -> Self {
            let venue_order_id = venue_order_id.into();
            Self {
                submit_result: Ok(venue_order_id.clone()),
                cancel_result: Ok(()),
                replace_result: Ok(OrderId(venue_order_id.clone())),
                fills: Vec::new(),
                events: Vec::new(),
            }
        }

        pub(crate) fn failed(error: HftError) -> Self {
            Self {
                submit_result: Err(error.clone()),
                cancel_result: Err(error.clone()),
                replace_result: Err(error),
                fills: Vec::new(),
                events: Vec::new(),
            }
        }

        #[allow(dead_code)]
        pub(crate) fn with_replace_result(mut self, result: Result<OrderId, HftError>) -> Self {
            self.replace_result = result;
            self
        }

        #[allow(dead_code)]
        pub(crate) fn with_cancel_result(mut self, result: Result<(), HftError>) -> Self {
            self.cancel_result = result;
            self
        }

        pub(crate) fn with_reconciled_fills(mut self, fills: Vec<FillRecord>) -> Self {
            self.fills = fills
                .into_iter()
                .map(|fill| AccountFill {
                    fill_id: fill.fill_id,
                    order_id: OrderId(fill.order_id),
                    symbol: hft_core::Symbol::new(fill.token_id),
                    side: match fill.side {
                        TradeSide::Buy => hft_core::Side::Buy,
                        TradeSide::Sell => hft_core::Side::Sell,
                    },
                    price: hft_core::Price(fill.price),
                    quantity: hft_core::Quantity(fill.quantity),
                    fee: Some(fill.fee),
                    timestamp: fill.timestamp.timestamp_micros().max(0) as u64,
                })
                .collect();
            self
        }

        pub(crate) fn with_execution_events(mut self, events: Vec<ExecutionEvent>) -> Self {
            self.events = events;
            self
        }
    }

    #[async_trait]
    impl ExecutionClient for StaticExecutionGateway {
        async fn place_order(&mut self, _intent: OrderIntent) -> Result<OrderId, HftError> {
            self.submit_result.clone().map(OrderId)
        }

        async fn cancel_order(&mut self, _order_id: &OrderId) -> Result<(), HftError> {
            self.cancel_result.clone()
        }

        async fn modify_order(
            &mut self,
            _order_id: &OrderId,
            _new_quantity: Option<Quantity>,
            _new_price: Option<Price>,
        ) -> Result<OrderId, HftError> {
            self.replace_result.clone()
        }

        async fn execution_stream(&self) -> Result<BoxStream<ExecutionEvent>, HftError> {
            if self.events.is_empty() {
                return Err(HftError::Config(
                    "test execution stream unavailable".to_string(),
                ));
            }
            Ok(Box::pin(futures::stream::iter(
                self.events.clone().into_iter().map(Ok),
            )))
        }

        fn execution_stream_may_complete(&self) -> bool {
            true
        }

        async fn list_open_orders(&self) -> Result<Vec<OpenOrder>, HftError> {
            Ok(Vec::new())
        }

        async fn list_recent_fills(&self) -> Result<Vec<AccountFill>, HftError> {
            Ok(self.fills.clone())
        }

        async fn get_balance(&self) -> Result<Vec<AccountBalance>, HftError> {
            Ok(Vec::new())
        }

        async fn get_positions(&self) -> Result<Vec<Position>, HftError> {
            Ok(Vec::new())
        }

        async fn connect(&mut self) -> Result<(), HftError> {
            Ok(())
        }

        async fn disconnect(&mut self) -> Result<(), HftError> {
            Ok(())
        }

        async fn health(&self) -> ConnectionHealth {
            ConnectionHealth {
                connected: true,
                latency_ms: Some(0.0),
                last_heartbeat: 0,
            }
        }
    }
}

use ploy_deployments::WorkerSupervisor;
use ploy_platform::ControlPlane;
use std::marker::PhantomData;

pub use bootstrap::apply_loaded_registry_state;
pub use deployment_control::{
    apply_deployment, build_deployment_record, control_deployment, enforce_exposure_limit,
    enforce_order_replacement_exposure, ensure_intent_allowed, set_deployment_max_gross_exposure,
};
pub use execution_client::{
    disabled_execution_client, execution_io_error, lock_execution_client, DisabledExecutionClient,
    SharedExecutionClient, MONDAY_EXECUTION_DISABLED,
};
pub use health_runtime::{
    mark_live_runtime_degraded, mark_runtime_healthy, mark_venue_healthy, next_live_reconcile_at,
    LiveHealthConfig,
};
pub use proposals::{ProposalExecutionPlan, ProposalStore};
pub use reconcile::{reconcile_live_fills, reconcile_live_fills_with_stream};
pub use runtime_support::{
    build_order_control_response, build_persisted_trading_state_snapshot,
    build_trading_state_snapshot, deployment_state_wire, intent_allowed_while_draining,
    intent_counts_toward_exposure, intent_purpose_from_contract, intent_purpose_wire,
    io_error_from_execution_error, live_reconcile_backoff_ms, next_paper_intent_id,
    next_proposal_id, observed_state_for_desired, order_state_from_wire, order_state_wire,
    restore_persisted_trading_runtime, restore_trading_runtime, trade_side_from_wire,
    trade_side_wire, write_json, PersistedTradingStateSnapshot, ReconcileStatus,
};
pub use state_io::{load_proposal_store, load_registry_records, load_trading_runtimes};
pub use trade_control::{cancel_order, replace_order};
pub use trade_submit::{
    apply_live_intent_outcome, execute_live_intent, finish_live_intent, prepare_live_intent,
    submit_live_intent, submit_paper_intent, PreparedLiveIntent,
};
pub use worker_tick::{
    build_worker_launch_spec, refresh_source_health, tick_workers, WorkerTickConfig,
};

#[derive(Debug)]
pub struct PlatformRuntime {
    control_plane: ControlPlane,
    _supervisor_marker: PhantomData<WorkerSupervisor>,
}

impl PlatformRuntime {
    #[must_use]
    pub fn new(control_plane: ControlPlane) -> Self {
        Self {
            control_plane,
            _supervisor_marker: PhantomData,
        }
    }

    #[must_use]
    pub fn control_plane(&self) -> &ControlPlane {
        &self.control_plane
    }

    #[must_use]
    pub fn deployment_state_type_marker(&self) -> &'static str {
        let _ = core::mem::size_of::<WorkerSupervisor>();
        "platform-runtime"
    }
}
