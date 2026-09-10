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
                            | portfolio_core::prediction::OrderState::Filled
                            | portfolio_core::prediction::OrderState::Rejected
                    ) && order
                        .state_changed_at
                        .is_some_and(|changed_at| changed_at >= terminal_cutoff)))
            })
        {
            let Some(_venue_order_id) = order.venue_order_id.clone() else {
                continue;
            };
            if let Some(existing_deployment_id) = order_deployments
                .insert(order.order_id.clone(), record.deployment_id.clone())
                .filter(|existing| existing != &record.deployment_id)
            {
                return Err(reconciliation_identity_error(
                    "local order",
                    &order.order_id,
                    &existing_deployment_id,
                    &record.deployment_id,
                ));
            }
            let local = order.order_id.clone();
            let deployment_id = record.deployment_id.clone();
            if let Some(venue_order_id) = order.venue_order_id {
                bind_venue_identity(&mut venue_to_local, venue_order_id, local.clone(), deployment_id.clone())?;
            }
            for venue_order_id in order.venue_order_history {
                bind_venue_identity(&mut venue_to_local, venue_order_id, local.clone(), deployment_id.clone())?;
            }
        }
    }

    validate_local_venue_identity_collisions(&order_deployments, &venue_to_local)?;

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
                        venue_to_local.get(&venue_order_id).cloned()
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
        let Some((local_id, deployment_id)) = resolve_account_fill_identity(
            &account_fill.order_id.0,
            &order_deployments,
            &venue_to_local,
        )? else {
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
        let Some(runtime) = trading.get_mut(&deployment_id) else {
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

fn bind_venue_identity(
    venue_to_local: &mut HashMap<String, (String, String)>,
    venue_order_id: String,
    local_order_id: String,
    deployment_id: String,
) -> io::Result<()> {
    if venue_order_id.is_empty() {
        return Ok(());
    }
    let candidate = (local_order_id, deployment_id);
    if let Some(existing) = venue_to_local.get(&venue_order_id) {
        if existing != &candidate {
            return Err(reconciliation_identity_error(
                "venue order",
                &venue_order_id,
                &existing.1,
                &candidate.1,
            ));
        }
        return Ok(());
    }
    venue_to_local.insert(venue_order_id, candidate);
    Ok(())
}

fn validate_local_venue_identity_collisions(
    order_deployments: &HashMap<String, String>,
    venue_to_local: &HashMap<String, (String, String)>,
) -> io::Result<()> {
    for (local_order_id, local_deployment_id) in order_deployments {
        let Some((mapped_local_order_id, venue_deployment_id)) = venue_to_local.get(local_order_id)
        else {
            continue;
        };
        if mapped_local_order_id != local_order_id || venue_deployment_id != local_deployment_id {
            return Err(reconciliation_identity_error(
                "local/venue order",
                local_order_id,
                local_deployment_id,
                venue_deployment_id,
            ));
        }
    }
    Ok(())
}

fn resolve_account_fill_identity(
    order_id: &str,
    order_deployments: &HashMap<String, String>,
    venue_to_local: &HashMap<String, (String, String)>,
) -> io::Result<Option<(String, String)>> {
    match (order_deployments.get(order_id), venue_to_local.get(order_id)) {
        (None, None) => Ok(None),
        (Some(deployment_id), None) => Ok(Some((order_id.to_string(), deployment_id.clone()))),
        (None, Some((local_order_id, deployment_id))) => {
            Ok(Some((local_order_id.clone(), deployment_id.clone())))
        }
        (Some(local_deployment_id), Some((mapped_local_order_id, venue_deployment_id))) => {
            if mapped_local_order_id != order_id || local_deployment_id != venue_deployment_id {
                return Err(reconciliation_identity_error(
                    "local/venue order",
                    order_id,
                    local_deployment_id,
                    venue_deployment_id,
                ));
            }
            Ok(Some((mapped_local_order_id.clone(), venue_deployment_id.clone())))
        }
    }
}

fn reconciliation_identity_error(
    identity_kind: &str,
    identity: &str,
    existing_deployment_id: &str,
    conflicting_deployment_id: &str,
) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "reconciliation {identity_kind} identity collision for `{identity}` between deployments `{existing_deployment_id}` and `{conflicting_deployment_id}`"
        ),
    )
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

    #[derive(Debug, Clone)]
    struct LateFeeGateway {
        old_venue_order_id: String,
        stream_started: Arc<AtomicBool>,
        rest_fail: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ports::ExecutionClient for LateFeeGateway {
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
            if self.stream_started.swap(true, Ordering::SeqCst) {
                return Err(HftError::Config("stream already attached".to_string()));
            }
            let order_id = self.old_venue_order_id.clone();
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let _ = tx.send(Ok(ExecutionEvent::Fill {
                    order_id: OrderId(order_id.clone()),
                    price: hft_core::Price(dec!(0.5)),
                    quantity: hft_core::Quantity(dec!(1)),
                    timestamp: chrono::Utc::now().timestamp_micros().max(0) as u64,
                    fill_id: "late-old-fill".to_string(),
                }));
                tokio::time::sleep(std::time::Duration::from_millis(8)).await;
                let _ = tx.send(Ok(ExecutionEvent::FeeCharged {
                    order_id: OrderId(order_id),
                    amount: dec!(0.01),
                    timestamp: chrono::Utc::now().timestamp_micros().max(0) as u64,
                    fill_id: "late-old-fill".to_string(),
                }));
            });
            Ok(Box::pin(futures::stream::unfold(rx, |mut rx| async {
                rx.recv().await.map(|event| (event, rx))
            })))
        }

        async fn list_open_orders(&self) -> Result<Vec<ports::OpenOrder>, HftError> {
            Ok(Vec::new())
        }

        async fn list_recent_fills(&self) -> Result<Vec<ports::AccountFill>, HftError> {
            if self.rest_fail.swap(false, Ordering::SeqCst) {
                Err(HftError::Network("REST snapshot unavailable".to_string()))
            } else {
                Ok(Vec::new())
            }
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
    async fn filled_order_keeps_bounded_route_for_late_fee_and_old_venue_identity() {
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
                    intent_id: "intent-late-fee".to_string(),
                    deployment_id: deployment.deployment_id.clone(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.5)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-late-fee",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-late-fee", "venue-old");
        runtime
            .replace_order("order-late-fee", dec!(1), Some(dec!(0.5)), "venue-new")
            .expect("replace order");
        let mut gateway = LateFeeGateway {
            old_venue_order_id: "venue-old".to_string(),
            stream_started: Arc::new(AtomicBool::new(false)),
            rest_fail: Arc::new(AtomicBool::new(true)),
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
        assert!(first.is_err(), "first REST failure must remain visible");
        assert_eq!(
            trading
                .get("example.live")
                .unwrap()
                .snapshot(&BTreeMap::new())
                .fills
                .len(),
            1
        );
        assert_eq!(
            trading
                .get("example.live")
                .unwrap()
                .order("order-late-fee")
                .unwrap()
                .state,
            OrderState::Filled
        );

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let second = reconcile_live_fills_with_stream(
            &mut gateway,
            &mut stream,
            &[deployment],
            &mut trading,
        )
        .await
        .expect("late fee reconciliation");
        assert_eq!(second, ReconcileStatus::Applied(1));
        let snapshot = trading
            .get("example.live")
            .unwrap()
            .snapshot(&BTreeMap::new());
        assert_eq!(snapshot.pnl.total_fees, dec!(0.01));
        assert_eq!(
            snapshot
                .canonical_portfolio
                .unwrap()
                .processed_fee_ids
                .len(),
            1
        );
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

    #[tokio::test]
    async fn cross_deployment_local_venue_identity_collision_fails_before_reconciliation_mutation() {
        let deployment_a = DeploymentRecord {
            deployment_id: "example.live-a".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-a".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let deployment_b = DeploymentRecord {
            deployment_id: "example.live-b".to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: "acct-b".to_string(),
            max_gross_exposure: Some(dec!(5)),
            deployment_state: DeploymentState::Enabled,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let mut runtime_a = TradingRuntime::default();
        runtime_a
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-a".to_string(),
                    deployment_id: deployment_a.deployment_id.clone(),
                    market_id: "market-a".to_string(),
                    token_id: "token-a".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.4)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "shared-venue-order",
                None,
            )
            .expect("valid deployment A intent");
        runtime_a.acknowledge_order("shared-venue-order", "venue-a");
        let mut runtime_b = TradingRuntime::default();
        runtime_b
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-b".to_string(),
                    deployment_id: deployment_b.deployment_id.clone(),
                    market_id: "market-b".to_string(),
                    token_id: "token-b".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.4)),
                    purpose: IntentPurpose::Entry,
                    created_at: chrono::Utc::now(),
                },
                "order-b",
                None,
            )
            .expect("valid deployment B intent");
        runtime_b.acknowledge_order("order-b", "shared-venue-order");
        let mut trading = BTreeMap::from([
            (deployment_a.deployment_id.clone(), runtime_a),
            (deployment_b.deployment_id.clone(), runtime_b),
        ]);
        let before_a = trading
            .get("example.live-a")
            .expect("deployment A runtime")
            .snapshot(&BTreeMap::new());
        let before_b = trading
            .get("example.live-b")
            .expect("deployment B runtime")
            .snapshot(&BTreeMap::new());

        let mut gateway = StaticExecutionGateway::acknowledged("unused");
        let error = reconcile_live_fills(
            &mut gateway,
            &[deployment_a, deployment_b],
            &mut trading,
        )
        .await
        .expect_err("ambiguous venue identity must fail closed");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("shared-venue-order"));
        assert_eq!(
            trading
                .get("example.live-a")
                .expect("deployment A runtime")
                .snapshot(&BTreeMap::new())
                .fills,
            before_a.fills
        );
        assert_eq!(
            trading
                .get("example.live-b")
                .expect("deployment B runtime")
                .snapshot(&BTreeMap::new())
                .fills,
            before_b.fills
        );
    }
}
