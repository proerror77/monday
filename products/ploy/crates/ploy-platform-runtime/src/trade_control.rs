use crate::{build_order_control_response, io_error_from_execution_error, order_state_wire};
use ploy_connectivity::{
    CancellationOutcome, CancellationRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest,
};
use ploy_operator_contracts::{DeploymentRuntimeMode, OrderControlResponse, OrderReplaceRequest};
use ploy_platform::DeploymentRecord;
use ploy_trading::{OrderState, TradingRuntime};
use std::io;

fn reject_submission_in_progress(
    deployment: &DeploymentRecord,
    order: &ploy_trading::OrderRecord,
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

pub fn cancel_order(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
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
                let cancel_result = gateway.cancel(&CancellationRequest {
                    order_id: order_id.to_string(),
                    venue_order_id,
                });
                match cancel_result {
                    Ok(CancellationOutcome::Canceled) => {}
                    Ok(CancellationOutcome::Rejected { reason }) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("live cancel rejected: {reason}"),
                        ));
                    }
                    Err(err) => return Err(io_error_from_execution_error(err)),
                }
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

pub fn replace_order(
    runtime: &mut TradingRuntime,
    gateway: &dyn LiveExecutionGateway,
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
            let side = runtime
                .intent(&order.intent_id)
                .map(|intent| intent.side)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "intent `{}` for order `{order_id}` was not found",
                            order.intent_id
                        ),
                    )
                })?;

            match gateway.replace(&ReplaceRequest {
                order_id: order_id.to_string(),
                venue_order_id,
                token_id: order.token_id.clone(),
                side,
                quantity: request.quantity,
                limit_price: request.limit_price,
            }) {
                Ok(ReplaceOutcome::Replaced { venue_order_id }) => {
                    let updated = runtime
                        .replace_order(
                            order_id,
                            request.quantity,
                            request.limit_price,
                            venue_order_id,
                        )
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::NotFound, "order not found")
                        })?;
                    Ok(build_order_control_response(
                        deployment_id.to_string(),
                        updated,
                    ))
                }
                Ok(ReplaceOutcome::Rejected { reason }) => Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("live replace rejected: {reason}"),
                )),
                Ok(ReplaceOutcome::PartialFailure { reason }) => {
                    let message = format!("live replace partially failed after cancel: {reason}");
                    let _ = runtime.cancel_order(order_id);
                    let _ = runtime.record_order_error(order_id, message.clone());
                    Err(io::Error::other(message))
                }
                Err(err) => {
                    let _ = runtime.record_order_error(order_id, err.to_string());
                    Err(io_error_from_execution_error(err))
                }
            }
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
    use ploy_connectivity::{
        CancellationOutcome, CancellationRequest, ExecutionError, ExecutionOutcome,
        ExecutionRequest, LiveExecutionGateway, ReplaceOutcome, ReplaceRequest,
        StaticExecutionGateway, TrackedOrder,
    };
    use ploy_operator_contracts::{
        DeploymentState, DesiredState, ObservedState, OrderReplaceRequest,
    };
    use ploy_platform::DeploymentRecord;
    use ploy_trading::{
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

    impl LiveExecutionGateway for CountingControlGateway {
        fn probe(&self) -> Result<(), ExecutionError> {
            Ok(())
        }

        fn submit(&self, _request: &ExecutionRequest) -> Result<ExecutionOutcome, ExecutionError> {
            unreachable!("submit is not used by replacement tests")
        }

        fn cancel(
            &self,
            _request: &CancellationRequest,
        ) -> Result<CancellationOutcome, ExecutionError> {
            self.cancellations.fetch_add(1, Ordering::SeqCst);
            Ok(CancellationOutcome::Canceled)
        }

        fn replace(&self, _request: &ReplaceRequest) -> Result<ReplaceOutcome, ExecutionError> {
            self.replacements.fetch_add(1, Ordering::SeqCst);
            Ok(ReplaceOutcome::Replaced {
                venue_order_id: "venue-exit-r1".to_string(),
            })
        }

        fn reconcile_fills(
            &self,
            _tracked_orders: &[TrackedOrder],
        ) -> Result<Vec<FillRecord>, ExecutionError> {
            unreachable!("reconcile is not used by replacement tests")
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

    #[test]
    fn cancel_live_order_updates_runtime() {
        let mut runtime = seeded_runtime();
        let gateway = StaticExecutionGateway::acknowledged("venue-1")
            .with_cancel_result(Ok(CancellationOutcome::Canceled));
        let response = cancel_order(
            &mut runtime,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-1",
        )
        .expect("cancel");
        assert_eq!(response.state, "canceled");
    }

    #[test]
    fn replace_rejects_invalid_quantity() {
        let mut runtime = seeded_runtime();
        runtime.record_fill(ploy_trading::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1.5),
            price: dec!(0.45),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        });
        let gateway =
            StaticExecutionGateway::failed(ExecutionError::Transport("offline".to_string()));
        let error = replace_order(
            &mut runtime,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(1),
                limit_price: Some(dec!(0.47)),
            },
            dec!(2),
        )
        .expect_err("invalid quantity");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn replace_partial_failure_marks_order_canceled_with_error() {
        let mut runtime = seeded_runtime();
        let gateway = StaticExecutionGateway::acknowledged("venue-1").with_replace_result(Ok(
            ReplaceOutcome::PartialFailure {
                reason: "submit rejected".to_string(),
            },
        ));

        let error = replace_order(
            &mut runtime,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(2),
                limit_price: Some(dec!(0.47)),
            },
            dec!(2),
        )
        .expect_err("partial failure should be surfaced");

        assert_eq!(error.kind(), ErrorKind::Other);
        let order = runtime.order("order-1").expect("order");
        assert_eq!(order.state, OrderState::Canceled);
        assert_eq!(
            order.last_error.as_deref(),
            Some("live replace partially failed after cancel: submit rejected")
        );
    }

    #[test]
    fn replace_exit_cannot_exceed_remaining_reducible_position() {
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
        let gateway = CountingControlGateway::default();

        let error = replace_order(
            &mut runtime,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-exit",
            OrderReplaceRequest {
                quantity: dec!(2),
                limit_price: Some(dec!(0.60)),
            },
            dec!(0),
        )
        .expect_err("replacement would flip short position");

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(gateway.replacements.load(Ordering::SeqCst), 0);
        assert_eq!(runtime.snapshot(&std::collections::BTreeMap::new()), before);
    }

    #[test]
    fn live_submission_in_progress_cannot_be_canceled_or_replaced() {
        let runtime = seeded_runtime();
        let snapshot = runtime.snapshot(&std::collections::BTreeMap::new());
        let mut pending = TradingRuntime::default();
        let intent = snapshot.intents[0].clone();
        pending
            .submit_intent(intent, "order-1", None)
            .expect("pending order");
        let gateway = CountingControlGateway::default();

        let cancel_error = cancel_order(
            &mut pending,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-1",
        )
        .expect_err("pending live submission cannot be canceled");
        let replace_error = replace_order(
            &mut pending,
            &gateway,
            &live_deployment(),
            "example.live",
            "order-1",
            OrderReplaceRequest {
                quantity: dec!(2),
                limit_price: Some(dec!(0.47)),
            },
            Decimal::ZERO,
        )
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
