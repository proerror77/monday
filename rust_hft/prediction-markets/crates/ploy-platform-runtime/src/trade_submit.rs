use crate::order_state_wire;
use hft_core::{OrderId, OrderType, Price, Quantity, Side, Symbol, TimeInForce, VenueId};
use ploy_operator_contracts::{DeploymentRuntimeMode, PaperIntentResponse};
use ploy_platform::DeploymentRecord;
use portfolio_core::prediction::{TradingIntent, TradingRuntime};
use ports::{ExecutionClient, OrderIntent};
use std::io;

fn response_for_order(
    deployment_id: String,
    order: &portfolio_core::prediction::OrderRecord,
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

pub async fn submit_live_intent(
    runtime: &mut TradingRuntime,
    client: &mut dyn ExecutionClient,
    intent: TradingIntent,
    idempotency_key: Option<&str>,
) -> io::Result<PaperIntentResponse> {
    let prepared = prepare_live_intent(runtime, intent, idempotency_key)?;
    finish_live_intent(runtime, client, prepared).await
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

pub async fn finish_live_intent(
    runtime: &mut TradingRuntime,
    client: &mut dyn ExecutionClient,
    prepared: PreparedLiveIntent,
) -> io::Result<PaperIntentResponse> {
    let outcome = execute_live_intent(client, &prepared).await;
    apply_live_intent_outcome(runtime, prepared, outcome)
}

pub async fn execute_live_intent(
    client: &mut dyn ExecutionClient,
    prepared: &PreparedLiveIntent,
) -> Result<OrderId, hft_core::HftError> {
    let PreparedLiveIntent::Pending { intent, order_id } = prepared else {
        return Err(hft_core::HftError::InvalidOrder(
            "existing live intent must not be submitted again".to_string(),
        ));
    };
    let canonical_intent = OrderIntent::prediction_market(
        Symbol::new(intent.token_id.clone()),
        match intent.side {
            portfolio_core::prediction::TradeSide::Buy => Side::Buy,
            portfolio_core::prediction::TradeSide::Sell => Side::Sell,
        },
        Quantity(intent.quantity),
        if intent.limit_price.is_some() {
            OrderType::Limit
        } else {
            OrderType::Market
        },
        intent.limit_price.map(Price),
        TimeInForce::GTC,
        format!("{}:{order_id}", intent.deployment_id),
        VenueId::POLYMARKET,
    );
    client.place_order(canonical_intent).await
}

pub fn apply_live_intent_outcome(
    runtime: &mut TradingRuntime,
    prepared: PreparedLiveIntent,
    outcome: Result<OrderId, hft_core::HftError>,
) -> io::Result<PaperIntentResponse> {
    let (intent, order_id) = match prepared {
        PreparedLiveIntent::Existing(response) => return Ok(response),
        PreparedLiveIntent::Pending { intent, order_id } => (intent, order_id),
    };
    match outcome {
        Ok(venue_order_id) => {
            runtime.acknowledge_order(&order_id, venue_order_id.0);
        }
        Err(err) => {
            let message = match &err {
                hft_core::HftError::SubmissionNotAttempted(reason) => reason.clone(),
                _ => err.to_string(),
            };
            match err {
                hft_core::HftError::Network(_)
                | hft_core::HftError::Timeout(_)
                | hft_core::HftError::Io { .. } => {
                    runtime.mark_order_unknown(&order_id, message);
                }
                hft_core::HftError::Config(_)
                | hft_core::HftError::InvalidOrder(_)
                | hft_core::HftError::SubmissionNotAttempted(_)
                | hft_core::HftError::Risk(_)
                | hft_core::HftError::Authentication(_)
                | hft_core::HftError::InsufficientBalance(_)
                | hft_core::HftError::OrderNotFound(_)
                | hft_core::HftError::Parse(_)
                | hft_core::HftError::Serialization(_)
                | hft_core::HftError::Exchange(_)
                | hft_core::HftError::Execution(_)
                | hft_core::HftError::RateLimit(_)
                | hft_core::HftError::Generic { .. } => {
                    runtime.reject_order(&order_id, message);
                }
            }
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
    use async_trait::async_trait;
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::DeploymentRecord;
    use portfolio_core::prediction::{IntentPurpose, TradeSide, TradingIntent, TradingRuntime};
    use rust_decimal_macros::dec;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct CountingGateway {
        submits: AtomicUsize,
    }

    #[async_trait]
    impl ports::ExecutionClient for CountingGateway {
        async fn place_order(
            &mut self,
            _intent: ports::OrderIntent,
        ) -> Result<hft_core::OrderId, hft_core::HftError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Ok(hft_core::OrderId("venue-1".to_string()))
        }
        async fn cancel_order(
            &mut self,
            _order_id: &hft_core::OrderId,
        ) -> Result<(), hft_core::HftError> {
            Ok(())
        }
        async fn modify_order(
            &mut self,
            order_id: &hft_core::OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> Result<hft_core::OrderId, hft_core::HftError> {
            Ok(order_id.clone())
        }
        async fn execution_stream(
            &self,
        ) -> Result<ports::BoxStream<ports::ExecutionEvent>, hft_core::HftError> {
            Err(hft_core::HftError::Config(
                "test execution stream unavailable".into(),
            ))
        }
        async fn list_open_orders(&self) -> Result<Vec<ports::OpenOrder>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn list_recent_fills(&self) -> Result<Vec<ports::AccountFill>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn get_balance(&self) -> Result<Vec<ports::AccountBalance>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn get_positions(&self) -> Result<Vec<ports::Position>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn connect(&mut self) -> Result<(), hft_core::HftError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), hft_core::HftError> {
            Ok(())
        }
        async fn health(&self) -> ports::ConnectionHealth {
            ports::ConnectionHealth {
                connected: true,
                latency_ms: Some(0.0),
                last_heartbeat: 0,
            }
        }
    }

    #[derive(Debug, Default)]
    struct TransportGateway {
        submits: AtomicUsize,
    }

    #[async_trait]
    impl ports::ExecutionClient for TransportGateway {
        async fn place_order(
            &mut self,
            _intent: ports::OrderIntent,
        ) -> Result<hft_core::OrderId, hft_core::HftError> {
            self.submits.fetch_add(1, Ordering::SeqCst);
            Err(hft_core::HftError::Network("offline".to_string()))
        }
        async fn cancel_order(
            &mut self,
            _order_id: &hft_core::OrderId,
        ) -> Result<(), hft_core::HftError> {
            Ok(())
        }
        async fn modify_order(
            &mut self,
            order_id: &hft_core::OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> Result<hft_core::OrderId, hft_core::HftError> {
            Ok(order_id.clone())
        }
        async fn execution_stream(
            &self,
        ) -> Result<ports::BoxStream<ports::ExecutionEvent>, hft_core::HftError> {
            Err(hft_core::HftError::Network("offline".to_string()))
        }
        async fn list_open_orders(&self) -> Result<Vec<ports::OpenOrder>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn list_recent_fills(&self) -> Result<Vec<ports::AccountFill>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn get_balance(&self) -> Result<Vec<ports::AccountBalance>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn get_positions(&self) -> Result<Vec<ports::Position>, hft_core::HftError> {
            Ok(Vec::new())
        }
        async fn connect(&mut self) -> Result<(), hft_core::HftError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), hft_core::HftError> {
            Ok(())
        }
        async fn health(&self) -> ports::ConnectionHealth {
            ports::ConnectionHealth {
                connected: false,
                latency_ms: None,
                last_heartbeat: 0,
            }
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

    #[tokio::test]
    async fn paper_submit_acknowledges() {
        let mut runtime = TradingRuntime::default();
        let response =
            submit_paper_intent(&mut runtime, &paper_deployment(), intent(), None).expect("submit");
        assert_eq!(response.state, "acknowledged");
        assert!(runtime.order(&response.order_id).is_some());
    }

    #[tokio::test]
    async fn paper_submit_returns_existing_result_for_idempotency_key() {
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

    #[tokio::test]
    async fn live_submit_does_not_resubmit_identical_idempotent_replay() {
        let mut runtime = TradingRuntime::default();
        let mut gateway = CountingGateway::default();
        let first = submit_live_intent(&mut runtime, &mut gateway, intent(), Some("request-1"))
            .await
            .expect("first submit");
        let second = submit_live_intent(&mut runtime, &mut gateway, intent(), Some("request-1"))
            .await
            .expect("idempotent replay");

        assert_eq!(second, first);
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn idempotency_key_rejects_mismatched_payload() {
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

    #[tokio::test]
    async fn transport_error_stays_unknown_and_is_not_retried() {
        let mut runtime = TradingRuntime::default();
        let mut gateway = TransportGateway::default();
        let first = submit_live_intent(&mut runtime, &mut gateway, intent(), Some("request-1"))
            .await
            .expect("unknown response");
        let snapshot = runtime.snapshot(&Default::default());
        let mut restored = TradingRuntime::restore(snapshot).expect("canonical restore");
        let mut replay_gateway = CountingGateway::default();
        let replay = submit_live_intent(
            &mut restored,
            &mut replay_gateway,
            intent(),
            Some("request-1"),
        )
        .await
        .expect("durable replay");

        assert_eq!(first.state, "unknown");
        assert_eq!(replay.order_id, first.order_id);
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
        assert_eq!(replay_gateway.submits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn live_submit_execution_is_separate_from_pending_and_terminal_state_changes() {
        let mut runtime = TradingRuntime::default();
        let mut gateway = CountingGateway::default();
        let prepared =
            prepare_live_intent(&mut runtime, intent(), Some("request-1")).expect("prepare");
        assert_eq!(
            runtime
                .order("order-intent-1")
                .expect("pending order")
                .state,
            portfolio_core::prediction::OrderState::Pending
        );

        let outcome = execute_live_intent(&mut gateway, &prepared).await;
        assert_eq!(
            runtime
                .order("order-intent-1")
                .expect("pending order")
                .state,
            portfolio_core::prediction::OrderState::Pending
        );

        let response = apply_live_intent_outcome(&mut runtime, prepared, outcome)
            .expect("apply submission outcome");
        assert_eq!(response.state, "acknowledged");
        assert_eq!(gateway.submits.load(Ordering::SeqCst), 1);
    }
}
