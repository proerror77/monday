use crate::order_state_wire;
use ploy_connectivity::{
    ExecutionOutcome, ExecutionRequest, LiveExecutionGateway, OrderExecutionType,
};
use ploy_operator_contracts::{DeploymentRuntimeMode, PaperIntentResponse};
use ploy_platform::DeploymentRecord;
use ploy_trading::{TradingIntent, TradingRuntime};
use std::io;

fn response_for_order(
    deployment_id: String,
    order: &ploy_trading::OrderRecord,
) -> PaperIntentResponse {
    PaperIntentResponse {
        deployment_id,
        intent_id: order.intent_id.clone(),
        order_id: order.order_id.clone(),
        state: order_state_wire(order.state),
        venue_order_id: order.venue_order_id.clone(),
        rejection_reason: order.rejection_reason.clone(),
        last_error: order.last_error.clone(),
    }
}

pub fn submit_paper_intent(
    runtime: &mut TradingRuntime,
    deployment: &DeploymentRecord,
    intent: TradingIntent,
    idempotency_key: Option<&str>,
) -> io::Result<PaperIntentResponse> {
    if deployment.runtime_mode != DeploymentRuntimeMode::Paper {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only paper deployments are supported by the local trading runtime",
        ));
    }

    let deployment_id = intent.deployment_id.clone();
    if let Some(order) = runtime
        .idempotent_order(&intent, idempotency_key)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?
    {
        return Ok(response_for_order(deployment_id, order));
    }
    let order_id = format!("order-{}", intent.intent_id);
    let venue_order_id = format!("paper-{}", intent.intent_id);
    let order = runtime
        .submit_intent(intent, order_id, idempotency_key)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let order_id = order.order_id.clone();
    runtime.acknowledge_order(&order_id, venue_order_id.clone());
    let order = runtime.order(&order_id).expect("submitted order");
    Ok(response_for_order(deployment_id, order))
}

pub fn submit_live_intent(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
    intent: TradingIntent,
    idempotency_key: Option<&str>,
) -> io::Result<PaperIntentResponse> {
    let prepared = prepare_live_intent(runtime, intent, idempotency_key)?;
    finish_live_intent(runtime, gateway, prepared)
}

#[derive(Debug, Clone)]
pub enum PreparedLiveIntent {
    Existing(PaperIntentResponse),
    Pending {
        intent: TradingIntent,
        order_id: String,
    },
}

pub fn prepare_live_intent(
    runtime: &mut TradingRuntime,
    intent: TradingIntent,
    idempotency_key: Option<&str>,
) -> io::Result<PreparedLiveIntent> {
    if let Some(order) = runtime
        .idempotent_order(&intent, idempotency_key)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?
    {
        return Ok(PreparedLiveIntent::Existing(response_for_order(
            intent.deployment_id.clone(),
            order,
        )));
    }
    if let Some(existing) = runtime.intent(&intent.intent_id) {
        let order = runtime
            .orders()
            .orders()
            .find(|order| order.intent_id == intent.intent_id)
            .expect("restored intent has order");
        if existing.deployment_id != intent.deployment_id
            || existing.market_id != intent.market_id
            || existing.token_id != intent.token_id
            || existing.side != intent.side
            || existing.quantity != intent.quantity
            || existing.limit_price != intent.limit_price
            || existing.purpose != intent.purpose
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "idempotency key payload mismatch",
            ));
        }
        return Ok(PreparedLiveIntent::Existing(response_for_order(
            intent.deployment_id.clone(),
            order,
        )));
    }
    let order_id = format!("order-{}", intent.intent_id);
    runtime
        .submit_intent(intent.clone(), order_id.clone(), idempotency_key)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    Ok(PreparedLiveIntent::Pending { intent, order_id })
}

pub fn finish_live_intent(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
    prepared: PreparedLiveIntent,
) -> io::Result<PaperIntentResponse> {
    let outcome = execute_live_intent(gateway, &prepared);
    apply_live_intent_outcome(runtime, prepared, outcome)
}

pub fn execute_live_intent(
    gateway: &dyn LiveExecutionGateway,
    prepared: &PreparedLiveIntent,
) -> Result<ExecutionOutcome, ploy_connectivity::ExecutionError> {
    let PreparedLiveIntent::Pending { intent, order_id } = prepared else {
        return Err(ploy_connectivity::ExecutionError::Validation(
            "existing live intent must not be submitted again".to_string(),
        ));
    };
    gateway.submit(&ExecutionRequest {
        order_id: order_id.clone(),
        token_id: intent.token_id.clone(),
        side: intent.side,
        quantity: intent.quantity,
        limit_price: intent.limit_price,
        order_type: OrderExecutionType::GTC,
        aggressive_ticks: 0,
    })
}

pub fn apply_live_intent_outcome(
    runtime: &mut TradingRuntime,
    prepared: PreparedLiveIntent,
    outcome: Result<ExecutionOutcome, ploy_connectivity::ExecutionError>,
) -> io::Result<PaperIntentResponse> {
    let (intent, order_id) = match prepared {
        PreparedLiveIntent::Existing(response) => return Ok(response),
        PreparedLiveIntent::Pending { intent, order_id } => (intent, order_id),
    };
    match outcome {
        Ok(ExecutionOutcome::Acknowledged { venue_order_id }) => {
            runtime.acknowledge_order(&order_id, venue_order_id);
        }
        Ok(ExecutionOutcome::Rejected { reason }) => {
            runtime.reject_order(&order_id, reason);
        }
        Err(err) => {
            runtime.mark_order_unknown(&order_id, err.to_string());
        }
    }
    let order = runtime
        .order(&order_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "prepared order not found"))?;
    Ok(response_for_order(intent.deployment_id, order))
}

#[cfg(test)]
mod tests {
    use super::{
        apply_live_intent_outcome, execute_live_intent, prepare_live_intent, submit_live_intent,
        submit_paper_intent,
    };
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest, TrackedOrder,
    };
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::DeploymentRecord;
    use ploy_trading::{FillRecord, IntentPurpose, TradeSide, TradingIntent, TradingRuntime};
    use rust_decimal_macros::dec;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct CountingGateway {
        submits: AtomicUsize,
    }

    impl LiveExecutionGateway for CountingGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Ok(ExecutionOutcome::Acknowledged {
                venue_order_id: "venue-1".to_string(),
            })
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-2".to_string(),
            })
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            Ok(Vec::new())
        }
    }

    #[derive(Debug, Default)]
    struct TransportGateway {
        submits: AtomicUsize,
    }

    impl LiveExecutionGateway for TransportGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Err(ExecutionError::Transport("offline".to_string()))
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            unreachable!()
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            Ok(Vec::new())
        }
    }

    fn paper_deployment() -> DeploymentRecord {
        DeploymentRecord {
            deployment_id: "example.paper".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Paper,
            account_id: "acct-paper".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        }
    }

    fn intent() -> TradingIntent {
        TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "example.paper".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            limit_price: Some(dec!(0.45)),
            purpose: IntentPurpose::Entry,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn paper_submit_acknowledges() {
        let mut runtime = TradingRuntime::default();
        let response =
            submit_paper_intent(&mut runtime, &paper_deployment(), intent(), None).expect("submit");
        assert_eq!(response.state, "acknowledged");
        assert!(runtime.order(&response.order_id).is_some());
    }

    #[test]
    fn paper_submit_returns_existing_result_for_idempotency_key() {
        let mut runtime = TradingRuntime::default();
        let first = submit_paper_intent(
            &mut runtime,
            &paper_deployment(),
            intent(),
            Some("request-1"),
        )
        .expect("first submit");
        let mut retry = intent();
        retry.intent_id = "intent-2".to_string();
        let second =
            submit_paper_intent(&mut runtime, &paper_deployment(), retry, Some("request-1"))
                .expect("idempotent retry");

        assert_eq!(second, first);
        assert_eq!(runtime.orders().orders().count(), 1);
    }

    #[test]
    fn live_submit_does_not_resubmit_identical_idempotent_replay() {
        let mut runtime = TradingRuntime::default();
        let gateway = CountingGateway::default();
        let first = submit_live_intent(&mut runtime, &gateway, intent(), Some("request-1"))
            .expect("first submit");
        let second = submit_live_intent(&mut runtime, &gateway, intent(), Some("request-1"))
            .expect("idempotent replay");

        assert_eq!(second, first);
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn idempotency_key_rejects_mismatched_payload() {
        let mut runtime = TradingRuntime::default();
        submit_paper_intent(
            &mut runtime,
            &paper_deployment(),
            intent(),
            Some("request-1"),
        )
        .expect("first submit");
        let mut mismatched = intent();
        mismatched.quantity = dec!(1);

        let error = submit_paper_intent(
            &mut runtime,
            &paper_deployment(),
            mismatched,
            Some("request-1"),
        )
        .expect_err("payload mismatch");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn transport_error_stays_unknown_and_is_not_retried() {
        let mut runtime = TradingRuntime::default();
        let gateway = TransportGateway::default();
        let first = submit_live_intent(&mut runtime, &gateway, intent(), Some("request-1"))
            .expect("unknown response");
        let snapshot = runtime.snapshot(&Default::default());
        let mut restored = TradingRuntime::restore(snapshot);
        let replay_gateway = CountingGateway::default();
        let replay =
            submit_live_intent(&mut restored, &replay_gateway, intent(), Some("request-1"))
                .expect("durable replay");

        assert_eq!(first.state, "unknown");
        assert_eq!(replay.order_id, first.order_id);
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
        assert_eq!(replay_gateway.submits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn live_submit_execution_is_separate_from_pending_and_terminal_state_changes() {
        let mut runtime = TradingRuntime::default();
        let gateway = CountingGateway::default();
        let prepared =
            prepare_live_intent(&mut runtime, intent(), Some("request-1")).expect("prepare");
        assert_eq!(
            runtime
                .order("order-intent-1")
                .expect("pending order")
                .state,
            ploy_trading::OrderState::Pending
        );

        let outcome = execute_live_intent(&gateway, &prepared);
        assert_eq!(
            runtime
                .order("order-intent-1")
                .expect("pending order")
                .state,
            ploy_trading::OrderState::Pending
        );

        let response = apply_live_intent_outcome(&mut runtime, prepared, outcome)
            .expect("apply submission outcome");
        assert_eq!(response.state, "acknowledged");
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
    }
}
