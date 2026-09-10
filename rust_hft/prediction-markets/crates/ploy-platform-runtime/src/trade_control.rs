use crate::execution_client::execution_io_error;
use crate::{build_order_control_response, order_state_wire};
use ploy_operator_contracts::{DeploymentRuntimeMode, OrderControlResponse, OrderReplaceRequest};
use ploy_platform::DeploymentRecord;
use portfolio_core::prediction::{OrderState, TradingRuntime};
use ports::ExecutionClient;
use std::io;

fn reject_submission_in_progress(
    deployment: &DeploymentRecord,
    order: &portfolio_core::prediction::OrderRecord,
) -> io::Result<()> {
    if deployment.runtime_mode == DeploymentRuntimeMode::Live
        && order.state == OrderState::Pending
        && order.venue_order_id.is_none()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("order `{}` submission is in progress", order.order_id),
        ));
    }
    Ok(())
}

pub async fn cancel_order(
    runtime: &mut TradingRuntime,
    client: &mut dyn ExecutionClient,
    deployment: &DeploymentRecord,
    deployment_id: &str,
    order_id: &str,
) -> io::Result<OrderControlResponse> {
    let order = runtime
        .order(order_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;

    reject_submission_in_progress(deployment, &order)?;

    if !matches!(
        order.state,
        OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "order `{order_id}` is not cancelable from state `{}`",
                order_state_wire(order.state)
            ),
        ));
    }

    match deployment.runtime_mode {
        DeploymentRuntimeMode::Paper => {}
        DeploymentRuntimeMode::Live => {
            if let Some(venue_order_id) = order.venue_order_id.clone() {
                client
                    .cancel_order(&hft_core::OrderId(venue_order_id))
                    .await
                    .map_err(execution_io_error)?;
            }
        }
    }

    let updated = runtime
        .cancel_order(order_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
    Ok(build_order_control_response(
        deployment_id.to_string(),
        updated,
    ))
}

pub async fn replace_order(
    runtime: &mut TradingRuntime,
    client: &mut dyn ExecutionClient,
    deployment: &DeploymentRecord,
    deployment_id: &str,
    order_id: &str,
    request: OrderReplaceRequest,
    current_total_exposure: rust_decimal::Decimal,
) -> io::Result<OrderControlResponse> {
    let order = runtime
        .order(order_id)
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;

    reject_submission_in_progress(deployment, &order)?;

    if !matches!(
        order.state,
        OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
    ) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "order `{order_id}` is not replaceable from state `{}`",
                order_state_wire(order.state)
            ),
        ));
    }

    runtime
        .validate_order_replacement(order_id, request.quantity, request.limit_price)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

    let Some(intent) = runtime.intent(&order.intent_id) else {
        return Ok(build_order_control_response(
            deployment_id.to_string(),
            &order,
        ));
    };

    crate::enforce_order_replacement_exposure(
        deployment,
        &order,
        &request,
        intent.purpose,
        current_total_exposure,
    )?;

    match deployment.runtime_mode {
        DeploymentRuntimeMode::Live => {
            let venue_order_id = order.venue_order_id.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("order `{order_id}` has no live venue order to replace"),
                )
            })?;
            client
                .modify_order(
                    &hft_core::OrderId(venue_order_id.clone()),
                    Some(hft_core::Quantity(request.quantity)),
                    request.limit_price.map(hft_core::Price),
                )
                .await
                .map_err(|error| {
                    let _ = runtime.record_order_error(order_id, error.to_string());
                    execution_io_error(error)
                })?;
            let updated = runtime
                .replace_order(
                    order_id,
                    request.quantity,
                    request.limit_price,
                    venue_order_id,
                )
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
            Ok(build_order_control_response(
                deployment_id.to_string(),
                updated,
            ))
        }
        DeploymentRuntimeMode::Paper => {
            let next_revision = order.revision + 1;
            let venue_order_id = format!("paper-{order_id}-r{next_revision}");
            let updated = runtime
                .replace_order(
                    order_id,
                    request.quantity,
                    request.limit_price,
                    venue_order_id,
                )
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "order not found"))?;
            Ok(build_order_control_response(
                deployment_id.to_string(),
                updated,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{cancel_order, replace_order};
    use crate::test_support::StaticExecutionGateway;
    use async_trait::async_trait;
    use ploy_operator_contracts::{
        DeploymentState, DesiredState, ObservedState, OrderReplaceRequest,
    };
    use ploy_platform::DeploymentRecord;
    use portfolio_core::prediction::{
        FillRecord, IntentPurpose, OrderState, TradeSide, TradingIntent, TradingRuntime,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug, Default)]
    struct CountingControlGateway {
        cancellations: AtomicUsize,
        replacements: AtomicUsize,
    }

    #[async_trait]
    impl ports::ExecutionClient for CountingControlGateway {
        async fn place_order(
            &mut self,
            _intent: ports::OrderIntent,
        ) -> Result<hft_core::OrderId, hft_core::HftError> {
            unreachable!("submit is not used by replacement tests")
        }

        async fn cancel_order(
            &mut self,
            _order_id: &hft_core::OrderId,
        ) -> Result<(), hft_core::HftError> {
            self.cancellations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn modify_order(
            &mut self,
            _order_id: &hft_core::OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> Result<(), hft_core::HftError> {
            self.replacements.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn execution_stream(
            &self,
        ) -> Result<ports::BoxStream<ports::ExecutionEvent>, hft_core::HftError> {
            unreachable!("reconcile is not used")
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

    fn live_deployment() -> DeploymentRecord {
        DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        }
    }

    fn seeded_runtime() -> TradingRuntime {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-1",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-1", "venue-1");
        runtime
    }

    #[tokio::test]
    async fn cancel_live_order_updates_runtime() {
        let mut runtime = seeded_runtime();
        let mut gateway =
            StaticExecutionGateway::acknowledged("venue-1").with_cancel_result(Ok(()));
        let response = cancel_order(
            &mut runtime,
            &mut gateway,
            &live_deployment(),
            "example.live",
            "order-1",
        )
        .await
        .expect("cancel");
        assert_eq!(response.state, "canceled");
    }

    #[tokio::test]
    async fn replace_rejects_invalid_quantity() {
        let mut runtime = seeded_runtime();
        runtime.record_fill(portfolio_core::prediction::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1.5),
            price: dec!(0.45),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        });
        let mut gateway =
            StaticExecutionGateway::failed(hft_core::HftError::Network("offline".to_string()));
        let error = replace_order(
            &mut runtime,
            &mut gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(1),
                limit_price: Some(dec!(0.47)),
            },
            dec!(2),
        )
        .await
        .expect_err("invalid quantity");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn replace_partial_failure_marks_order_canceled_with_error() {
        let mut runtime = seeded_runtime();
        let mut gateway = StaticExecutionGateway::failed(hft_core::HftError::Exchange(
            "submit rejected".to_string(),
        ));

        let error = replace_order(
            &mut runtime,
            &mut gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(2),
                limit_price: Some(dec!(0.47)),
            },
            dec!(2),
        )
        .await
        .expect_err("partial failure should be surfaced");

        assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
        let order = runtime.order("order-1").expect("order");
        assert_eq!(order.state, OrderState::Acknowledged);
        assert_eq!(
            order.last_error.as_deref(),
            Some("交易所錯誤: submit rejected")
        );
    }

    #[tokio::test]
    async fn replace_exit_cannot_exceed_remaining_reducible_position() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-short".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-entry",
                None,
            )
            .expect("short entry");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-entry".to_string(),
            order_id: "order-entry".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Sell,
            quantity: dec!(2),
            price: dec!(0.40),
            fee: dec!(0),
            timestamp: chrono::Utc::now(),
        }));
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-short".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: chrono::Utc::now(),
                },
                "order-exit",
                None,
            )
            .expect("exit order");
        runtime.acknowledge_order("order-exit", "venue-exit");
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "reduce-short".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Reduce,
                    created_at: chrono::Utc::now(),
                },
                "order-reduce",
                None,
            )
            .expect("second reduction reserves remaining position");
        runtime.acknowledge_order("order-reduce", "venue-reduce");
        let before = runtime.snapshot(&std::collections::BTreeMap::new());
        let mut gateway = CountingControlGateway::default();

        let error = replace_order(
            &mut runtime,
            &mut gateway,
            &live_deployment(),
            "example.live",
            "order-exit",
            OrderReplaceRequest {
                quantity: dec!(2),
                limit_price: Some(dec!(0.60)),
            },
            dec!(0),
        )
        .await
        .expect_err("replacement would flip short position");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(gateway.replacements.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.snapshot(&std::collections::BTreeMap::new()), before);
    }

    #[tokio::test]
    async fn live_submission_in_progress_cannot_be_canceled_or_replaced() {
        let runtime = seeded_runtime();
        let snapshot = runtime.snapshot(&std::collections::BTreeMap::new());
        let mut pending = TradingRuntime::default();
        let intent = snapshot.intents[0].clone();
        pending
            .submit_intent(intent, "order-1", None)
            .expect("pending order");
        let mut gateway = CountingControlGateway::default();

        let cancel_error = cancel_order(
            &mut pending,
            &mut gateway,
            &live_deployment(),
            "example.live",
            "order-1",
        )
        .await
        .expect_err("pending live submission cannot be canceled");
        let replace_error = replace_order(
            &mut pending,
            &mut gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(2),
                limit_price: Some(dec!(0.47)),
            },
            Decimal::ZERO,
        )
        .await
        .expect_err("pending live submission cannot be replaced");

        assert_eq!(cancel_error.kind(), ErrorKind::InvalidInput);
        assert!(cancel_error
            .to_string()
            .contains("submission is in progress"));
        assert_eq!(replace_error.kind(), ErrorKind::InvalidInput);
        assert!(replace_error
            .to_string()
            .contains("submission is in progress"));
        assert_eq!(gateway.cancellations.load(Ordering::SeqCst), 0);
        assert_eq!(gateway.replacements.load(Ordering::SeqCst), 0);
        assert_eq!(
            pending.order("order-1").expect("pending order").state,
            OrderState::Pending
        );
    }
}
