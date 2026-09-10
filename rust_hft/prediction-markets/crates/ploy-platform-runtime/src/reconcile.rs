use crate::execution_client::execution_io_error;
use crate::ReconcileStatus;
use futures::StreamExt;
use ploy_operator_contracts::DeploymentRuntimeMode;
use ploy_platform::DeploymentRecord;
use portfolio_core::prediction::{FillRecord, TradeSide, TradingRuntime};
use ports::{ExecutionClient, ExecutionEvent};
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::time::Instant;
use tokio::time::{timeout, Duration};

const TERMINAL_RECONCILE_RETENTION_HOURS: i64 = 24;
const MAX_STREAM_EVENTS_PER_RECONCILE: usize = 128;
const MAX_STREAM_DRAIN_MS: u64 = 5;

pub async fn reconcile_live_fills(
    client: &mut dyn ExecutionClient,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
) -> io::Result<ReconcileStatus> {
    let mut stream = None;
    reconcile_live_fills_with_stream(client, &mut stream, deployments, trading).await
}

pub async fn reconcile_live_fills_with_stream(
    client: &mut dyn ExecutionClient,
    stream_slot: &mut Option<ports::BoxStream<ExecutionEvent>>,
    deployments: &[DeploymentRecord],
    trading: &mut BTreeMap<String, TradingRuntime>,
) -> io::Result<ReconcileStatus> {
    let mut order_deployments = HashMap::new();
    let mut venue_to_local = HashMap::new();
    let terminal_cutoff =
        chrono::Utc::now() - chrono::Duration::hours(TERMINAL_RECONCILE_RETENTION_HOURS);

    for record in deployments {
        if record.runtime_mode != DeploymentRuntimeMode::Live {
            continue;
        }

        let Some(runtime) = trading.get(&record.deployment_id) else {
            continue;
        };

        for order in runtime
            .snapshot(&BTreeMap::new())
            .orders
            .into_iter()
            .filter(|order| {
                order.venue_order_id.is_some()
                    && (matches!(
                        order.state,
                        portfolio_core::prediction::OrderState::Unknown
                            | portfolio_core::prediction::OrderState::Acknowledged
                            | portfolio_core::prediction::OrderState::PartiallyFilled
                    ) || (matches!(
                        order.state,
                        portfolio_core::prediction::OrderState::Canceled
                    ) && order
                        .state_changed_at
                        .is_none_or(|changed_at| changed_at >= terminal_cutoff)))
            })
        {
            let Some(_venue_order_id) = order.venue_order_id.clone() else {
                continue;
            };
            order_deployments.insert(order.order_id.clone(), record.deployment_id.clone());
            venue_to_local.insert(
                order.venue_order_id.clone().unwrap_or_default(),
                (order.order_id, record.deployment_id.clone()),
            );
        }
    }

    if order_deployments.is_empty() {
        return Ok(ReconcileStatus::Noop);
    }

    let mut recorded = 0;
    if let Some(stream) = stream_slot.as_mut() {
        let deadline = Instant::now() + std::time::Duration::from_millis(MAX_STREAM_DRAIN_MS);
        while recorded < MAX_STREAM_EVENTS_PER_RECONCILE && Instant::now() < deadline {
            match timeout(Duration::from_millis(1), stream.next()).await {
                Ok(Some(Ok(event))) => {
                    let Some(venue_order_id) = execution_event_order_id(&event) else {
                        continue;
                    };
                    let Some((local_id, deployment_id)) =
                        venue_to_local.get(&venue_order_id).cloned().or_else(|| {
                            order_deployments
                                .get(&venue_order_id)
                                .cloned()
                                .map(|deployment_id| (venue_order_id.clone(), deployment_id))
                        })
                    else {
                        continue;
                    };
                    if let Some(runtime) = trading.get_mut(&deployment_id) {
                        if runtime.apply_reconciliation_event(
                            &local_id,
                            Some(&venue_order_id),
                            &event,
                        ) {
                            recorded += 1;
                        }
                    }
                }
                Ok(Some(Err(error))) => return Err(execution_io_error(error)),
                Ok(None) | Err(_) => break,
            }
        }
    }

    // REST snapshots are supplementary evidence. Events have already been
    // applied above, so a REST failure cannot discard private-stream state.
    let open_orders = client
        .list_open_orders()
        .await
        .map_err(execution_io_error)?;
    let account_fills = client
        .list_recent_fills()
        .await
        .map_err(execution_io_error)?;

    for account_fill in account_fills {
        let local_id = if order_deployments.contains_key(&account_fill.order_id.0) {
            account_fill.order_id.0.clone()
        } else if let Some((local_id, _)) = venue_to_local.get(&account_fill.order_id.0) {
            local_id.clone()
        } else {
            continue;
        };
        let Some(deployment_id) = order_deployments.get(&local_id) else {
            continue;
        };
        let event = ExecutionEvent::Fill {
            order_id: account_fill.order_id.clone(),
            price: account_fill.price,
            quantity: account_fill.quantity,
            timestamp: account_fill.timestamp,
            fill_id: account_fill.fill_id.clone(),
        };
        let ExecutionEvent::Fill {
            price,
            quantity,
            timestamp,
            fill_id,
            ..
        } = event
        else {
            unreachable!("canonical fill conversion must remain a fill event");
        };
        let Some(runtime) = trading.get_mut(deployment_id) else {
            continue;
        };
        let fill = FillRecord {
            fill_id,
            order_id: local_id.clone(),
            token_id: account_fill.symbol.as_str().to_string(),
            side: match account_fill.side {
                hft_core::Side::Buy => TradeSide::Buy,
                hft_core::Side::Sell => TradeSide::Sell,
            },
            quantity: quantity.0,
            price: price.0,
            fee: account_fill.fee.unwrap_or_default(),
            timestamp: chrono::DateTime::from_timestamp_micros(timestamp as i64).ok_or_else(
                || io::Error::new(io::ErrorKind::InvalidData, "invalid fill timestamp"),
            )?,
        };
        if runtime.record_fill(fill) {
            recorded += 1;
        }
    }

    // Reconcile authoritative order observations after fills so a confirmed
    // fill is never erased by a later cancellation observation.
    for open_order in open_orders {
        let Some((local_id, deployment_id)) = venue_to_local
            .get(open_order.order_id.0.as_str())
            .or_else(|| {
                open_order
                    .client_order_id
                    .as_ref()
                    .and_then(|client_id| venue_to_local.get(client_id))
            })
            .cloned()
        else {
            continue;
        };
        if let Some(runtime) = trading.get_mut(&deployment_id) {
            runtime.acknowledge_order(&local_id, open_order.order_id.0);
        }
    }

    Ok(ReconcileStatus::Applied(recorded))
}

fn execution_event_order_id(event: &ExecutionEvent) -> Option<String> {
    match event {
        ExecutionEvent::OrderAck { order_id, .. }
        | ExecutionEvent::Fill { order_id, .. }
        | ExecutionEvent::FeeCharged { order_id, .. }
        | ExecutionEvent::OrderReject { order_id, .. }
        | ExecutionEvent::OrderCompleted { order_id, .. }
        | ExecutionEvent::OrderCanceled { order_id, .. }
        | ExecutionEvent::OrderModified { order_id, .. }
        | ExecutionEvent::PrivateOrderTiming { order_id, .. }
        | ExecutionEvent::OrderLifecycleTiming { order_id, .. } => Some(order_id.0.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::reconcile_live_fills;
    use super::reconcile_live_fills_with_stream;
    use crate::test_support::StaticExecutionGateway;
    use crate::ReconcileStatus;
    use async_trait::async_trait;
    use hft_core::{HftError, OrderId};
    use ploy_operator_contracts::{DeploymentState, DesiredState, ObservedState};
    use ploy_platform::DeploymentRecord;
    use portfolio_core::prediction::{
        FillRecord, IntentPurpose, OrderState, TradeSide, TradingIntent, TradingRuntime,
    };
    use ports::{ExecutionClient, ExecutionEvent};
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    struct DelayedTerminalGateway {
        stream_calls: Arc<AtomicUsize>,
        rest_fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ports::ExecutionClient for DelayedTerminalGateway {
        async fn place_order(&mut self, _intent: ports::OrderIntent) -> Result<OrderId, HftError> {
            Err(HftError::Config("unused".to_string()))
        }

        async fn cancel_order(&mut self, _order_id: &OrderId) -> Result<(), HftError> {
            Ok(())
        }

        async fn modify_order(
            &mut self,
            _order_id: &OrderId,
            _new_quantity: Option<hft_core::Quantity>,
            _new_price: Option<hft_core::Price>,
        ) -> Result<(), HftError> {
            Ok(())
        }

        async fn execution_stream(&self) -> Result<ports::BoxStream<ExecutionEvent>, HftError> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Box::pin(futures::stream::once(async {
                tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                Ok(ExecutionEvent::OrderCanceled {
                    order_id: OrderId("venue-delayed".to_string()),
                    timestamp: 1,
                })
            })))
        }

        async fn list_open_orders(&self) -> Result<Vec<ports::OpenOrder>, HftError> {
            if self.rest_fail.load(Ordering::SeqCst) {
                Err(HftError::Network("REST snapshot unavailable".to_string()))
            } else {
                Ok(Vec::new())
            }
        }

        async fn list_recent_fills(&self) -> Result<Vec<ports::AccountFill>, HftError> {
            Ok(Vec::new())
        }

        async fn get_balance(&self) -> Result<Vec<ports::AccountBalance>, HftError> {
            Ok(Vec::new())
        }

        async fn get_positions(&self) -> Result<Vec<ports::Position>, HftError> {
            Ok(Vec::new())
        }

        async fn connect(&mut self) -> Result<(), HftError> {
            Ok(())
        }

        async fn disconnect(&mut self) -> Result<(), HftError> {
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

    #[tokio::test]
    async fn reconcile_records_fills_into_trading_runtime() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
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
        runtime.mark_order_unknown("order-1", "final persistence lost");

        let fill = FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.45),
            fee: dec!(0.01),
            timestamp: chrono::Utc::now(),
        };
        let mut gateway =
            StaticExecutionGateway::acknowledged("venue-1").with_reconciled_fills(vec![fill]);
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result = reconcile_live_fills(&mut gateway, &[deployment], &mut trading)
            .await
            .expect("reconcile");
        assert_eq!(result, crate::ReconcileStatus::Applied(1));
        assert_eq!(
            trading
                .get("example.live")
                .expect("runtime")
                .snapshot(&BTreeMap::new())
                .fills
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn unknown_without_venue_order_id_is_not_reconciled() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: None,
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Paused,
            observed_state: ObservedState::Degraded,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-unknown".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-unknown",
                None,
            )
            .unwrap();
        runtime.mark_order_unknown("order-unknown", "transport lost");
        let mut runtimes = BTreeMap::from([("example.live".to_string(), runtime)]);

        let mut gateway = StaticExecutionGateway::acknowledged("unused");
        let result = reconcile_live_fills(&mut gateway, &[deployment], &mut runtimes)
            .await
            .unwrap();
        assert_eq!(result, crate::ReconcileStatus::Noop);
    }

    #[tokio::test]
    async fn archived_orders_remain_reconcilable_until_flat() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Archived,
            desired_state: DesiredState::Stopped,
            observed_state: ObservedState::Stopped,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-archived".to_string(),
                    deployment_id: "example.live".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-archived",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-archived", "venue-archived");
        let mut gateway = StaticExecutionGateway::acknowledged("venue-archived")
            .with_reconciled_fills(vec![FillRecord {
                fill_id: "fill-archived".to_string(),
                order_id: "order-archived".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                price: dec!(0.45),
                fee: dec!(0.01),
                timestamp: chrono::Utc::now(),
            }]);
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result = reconcile_live_fills(&mut gateway, &[deployment], &mut trading)
            .await
            .expect("archived order reconciliation");

        assert_eq!(result, crate::ReconcileStatus::Applied(1));
    }

    #[tokio::test]
    async fn confirmed_fill_is_applied_before_cancellation_observation() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-ordered".to_string(),
                    deployment_id: deployment.deployment_id.clone(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-ordered",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-ordered", "venue-ordered");

        let mut gateway = StaticExecutionGateway::acknowledged("venue-ordered")
            .with_reconciled_fills(vec![FillRecord {
                fill_id: "fill-ordered".to_string(),
                order_id: "order-ordered".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                price: dec!(0.45),
                fee: dec!(0.01),
                timestamp: chrono::Utc::now(),
            }]);
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result = reconcile_live_fills(&mut gateway, &[deployment], &mut trading)
            .await
            .expect("reconcile updates");

        assert_eq!(result, crate::ReconcileStatus::Applied(1));
        let snapshot = trading
            .get("example.live")
            .expect("runtime")
            .snapshot(&BTreeMap::new());
        assert_eq!(snapshot.fills.len(), 1);
        assert_eq!(snapshot.orders[0].filled_qty, dec!(1));
        assert_eq!(snapshot.orders[0].state, OrderState::PartiallyFilled);
    }

    #[tokio::test]
    async fn recently_canceled_order_remains_reconcilable_for_late_confirmed_fill() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-canceled".to_string(),
                    deployment_id: deployment.deployment_id.clone(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-canceled",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-canceled", "venue-canceled");
        runtime.cancel_order("order-canceled");

        let mut gateway = StaticExecutionGateway::acknowledged("venue-canceled")
            .with_reconciled_fills(vec![FillRecord {
                fill_id: "fill-after-cancel".to_string(),
                order_id: "order-canceled".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                price: dec!(0.45),
                fee: dec!(0.01),
                timestamp: chrono::Utc::now(),
            }]);
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result = reconcile_live_fills(&mut gateway, &[deployment], &mut trading)
            .await
            .expect("late fill reconciliation");

        assert_eq!(result, crate::ReconcileStatus::Applied(1));
        let snapshot = trading
            .get("example.live")
            .expect("runtime")
            .snapshot(&BTreeMap::new());
        assert_eq!(snapshot.fills.len(), 1);
        assert_eq!(snapshot.orders[0].filled_qty, dec!(1));
    }

    #[tokio::test]
    async fn explicit_terminal_stream_event_closes_order_without_open_order_absence() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-terminal".to_string(),
                    deployment_id: deployment.deployment_id.clone(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-terminal",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-terminal", "venue-terminal");
        let mut gateway = StaticExecutionGateway::acknowledged("venue-terminal")
            .with_execution_events(vec![ExecutionEvent::OrderCanceled {
                order_id: hft_core::OrderId("venue-terminal".to_string()),
                timestamp: 1,
            }]);
        let mut stream = Some(gateway.execution_stream().await.expect("attach stream"));
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let result = reconcile_live_fills_with_stream(
            &mut gateway,
            &mut stream,
            &[deployment],
            &mut trading,
        )
        .await
        .expect("terminal event reconciliation");

        assert_eq!(result, ReconcileStatus::Applied(1));
        assert_eq!(
            trading
                .get("example.live")
                .expect("runtime")
                .snapshot(&BTreeMap::new())
                .orders[0]
                .state,
            OrderState::Canceled
        );
    }

    #[tokio::test]
    async fn owned_stream_survives_idle_tick_and_rest_failure_without_resubscribe() {
        let deployment = DeploymentRecord {
            deployment_id: "example.live".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-live".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-delayed".to_string(),
                    deployment_id: deployment.deployment_id.clone(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.45)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-delayed",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-delayed", "venue-delayed");
        let stream_calls = Arc::new(AtomicUsize::new(0));
        let rest_fail = Arc::new(AtomicBool::new(true));
        let mut gateway = DelayedTerminalGateway {
            stream_calls: Arc::clone(&stream_calls),
            rest_fail: Arc::clone(&rest_fail),
        };
        let mut stream = Some(gateway.execution_stream().await.expect("attach stream"));
        let mut trading = BTreeMap::from([(deployment.deployment_id.clone(), runtime)]);

        let first = reconcile_live_fills_with_stream(
            &mut gateway,
            &mut stream,
            &[deployment.clone()],
            &mut trading,
        )
        .await;
        assert!(first.is_err(), "REST failure must remain visible");
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            trading
                .get("example.live")
                .expect("runtime")
                .snapshot(&BTreeMap::new())
                .orders[0]
                .state,
            OrderState::Acknowledged
        );

        rest_fail.store(false, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let second = reconcile_live_fills_with_stream(
            &mut gateway,
            &mut stream,
            &[deployment],
            &mut trading,
        )
        .await
        .expect("delayed event reconciliation");
        assert_eq!(second, ReconcileStatus::Applied(1));
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            trading
                .get("example.live")
                .expect("runtime")
                .snapshot(&BTreeMap::new())
                .orders[0]
                .state,
            OrderState::Canceled
        );
    }
}
