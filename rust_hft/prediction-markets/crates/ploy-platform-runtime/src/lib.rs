pub mod bootstrap;
pub mod deployment_control;
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
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReconcileBatch, ReplaceOutcome, ReplaceRequest,
        TrackedOrder,
    };
    use ploy_trading::FillRecord;

    #[derive(Debug, Clone)]
    pub(crate) struct StaticExecutionGateway {
        probe_result: Result<(), ExecutionError>,
        submit_result: Result<ExecutionOutcome, ExecutionError>,
        cancel_result: Result<CancellationOutcome, ExecutionError>,
        replace_result: Result<ReplaceOutcome, ExecutionError>,
        reconcile_result: Result<ReconcileBatch, ExecutionError>,
    }

    impl StaticExecutionGateway {
        pub(crate) fn acknowledged(venue_order_id: impl Into<String>) -> Self {
            let venue_order_id = venue_order_id.into();
            Self {
                probe_result: Ok(()),
                submit_result: Ok(ExecutionOutcome::Acknowledged {
                    venue_order_id: venue_order_id.clone(),
                }),
                cancel_result: Ok(CancellationOutcome::Canceled),
                replace_result: Ok(ReplaceOutcome::Replaced {
                    venue_order_id: format!("{venue_order_id}-replaced"),
                }),
                reconcile_result: Ok(ReconcileBatch::default()),
            }
        }

        pub(crate) fn failed(error: ExecutionError) -> Self {
            Self {
                probe_result: Ok(()),
                submit_result: Err(error.clone()),
                cancel_result: Ok(CancellationOutcome::Canceled),
                replace_result: Err(error),
                reconcile_result: Ok(ReconcileBatch::default()),
            }
        }

        pub(crate) fn with_cancel_result(
            mut self,
            result: Result<CancellationOutcome, ExecutionError>,
        ) -> Self {
            self.cancel_result = result;
            self
        }

        pub(crate) fn with_replace_result(
            mut self,
            result: Result<ReplaceOutcome, ExecutionError>,
        ) -> Self {
            self.replace_result = result;
            self
        }

        pub(crate) fn with_reconciled_fills(mut self, fills: Vec<FillRecord>) -> Self {
            self.reconcile_result = Ok(ReconcileBatch::fills_only(fills));
            self
        }

        pub(crate) fn with_reconciled_updates(mut self, updates: ReconcileBatch) -> Self {
            self.reconcile_result = Ok(updates);
            self
        }
    }

    impl LiveExecutionGateway for StaticExecutionGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            self.probe_result.clone()
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            self.submit_result.clone()
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
}

use ploy_deployments::WorkerSupervisor;
use ploy_platform::ControlPlane;
use std::marker::PhantomData;

pub use bootstrap::apply_loaded_registry_state;
pub use deployment_control::{
    apply_deployment, build_deployment_record, control_deployment, enforce_exposure_limit,
    enforce_order_replacement_exposure, ensure_intent_allowed, set_deployment_max_gross_exposure,
};
pub use health_runtime::{
    mark_live_runtime_degraded, mark_runtime_healthy, mark_venue_healthy, next_live_reconcile_at,
    LiveHealthConfig,
};
pub use proposals::{ProposalExecutionPlan, ProposalStore};
pub use reconcile::reconcile_live_fills;
pub use runtime_support::{
    build_order_control_response, build_trading_state_snapshot, deployment_state_wire,
    intent_allowed_while_draining, intent_counts_toward_exposure, intent_purpose_from_contract,
    intent_purpose_wire, io_error_from_execution_error, live_reconcile_backoff_ms,
    next_paper_intent_id, next_proposal_id, observed_state_for_desired, order_state_from_wire,
    order_state_wire, restore_trading_runtime, trade_side_from_wire, trade_side_wire, write_json,
    ReconcileStatus,
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
