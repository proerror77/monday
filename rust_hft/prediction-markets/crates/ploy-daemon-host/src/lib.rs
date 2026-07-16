pub mod config;
pub mod events;
pub mod http;
pub mod reports;
pub mod runtime;

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

        pub(crate) fn rejected(reason: impl Into<String>) -> Self {
            let reason = reason.into();
            Self {
                probe_result: Ok(()),
                submit_result: Ok(ExecutionOutcome::Rejected {
                    reason: reason.clone(),
                }),
                cancel_result: Ok(CancellationOutcome::Canceled),
                replace_result: Ok(ReplaceOutcome::Rejected { reason }),
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

        pub(crate) fn with_probe_result(mut self, result: Result<(), ExecutionError>) -> Self {
            self.probe_result = result;
            self
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
