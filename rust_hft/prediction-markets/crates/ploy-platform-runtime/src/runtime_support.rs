use ploy_connectivity::ExecutionError;
use ploy_operator_contracts::{
    DeploymentState, DesiredState, FillSnapshot, IntentPurpose, ObservedState,
    OrderControlResponse, OrderSnapshot, PnlSnapshotResponse, PositionSnapshotResponse,
    RiskSnapshotResponse, TradingIntentSnapshot, TradingStateSnapshot,
};
use ploy_platform::DeploymentRecord;
use ploy_trading::{OrderState, TradeSide, TradingIntent, TradingRuntime, TradingRuntimeSnapshot};
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileStatus {
    Applied(usize),
    Noop,
    BackoffActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentRiskEffect {
    Increase,
    Reduce,
    Control,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentAdmissionSource {
    Worker,
    AuthenticatedOperator,
    Emergency,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenExposureEnvelope {
    pub settled_net_qty: Decimal,
    pub worst_case_min_qty: Decimal,
    pub worst_case_max_qty: Decimal,
}

pub fn next_proposal_id(target_deployment_id: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_millis();
    let target = target_deployment_id.replace('.', "-");
    format!("proposal-{target}-{millis}")
}

pub fn build_trading_state_snapshot(
    record: DeploymentRecord,
    snapshot: TradingRuntimeSnapshot,
) -> TradingStateSnapshot {
    TradingStateSnapshot {
        deployment_id: record.deployment_id,
        runtime_mode: record.runtime_mode,
        intents: snapshot
            .intents
            .into_iter()
            .map(|intent| TradingIntentSnapshot {
                intent_id: intent.intent_id,
                market_id: intent.market_id,
                token_id: intent.token_id,
                side: trade_side_wire(intent.side),
                quantity: intent.quantity,
                limit_price: intent.limit_price,
                purpose: intent_purpose_wire(intent.purpose),
                created_at: intent.created_at,
            })
            .collect(),
        orders: snapshot
            .orders
            .into_iter()
            .map(|order| OrderSnapshot {
                order_id: order.order_id,
                intent_id: order.intent_id,
                token_id: order.token_id,
                requested_qty: order.requested_qty,
                limit_price: order.limit_price,
                venue_order_id: order.venue_order_id,
                venue_order_history: order.venue_order_history,
                revision: order.revision,
                state: order_state_wire(order.state),
                state_changed_at: order.state_changed_at,
                filled_qty: order.filled_qty,
                rejection_reason: order.rejection_reason,
                last_error: order.last_error,
                idempotency_key: order.idempotency_key,
            })
            .collect(),
        fills: snapshot
            .fills
            .into_iter()
            .map(|fill| FillSnapshot {
                fill_id: fill.fill_id,
                order_id: fill.order_id,
                token_id: fill.token_id,
                side: trade_side_wire(fill.side),
                quantity: fill.quantity,
                price: fill.price,
                fee: fill.fee,
                timestamp: fill.timestamp,
            })
            .collect(),
        positions: snapshot
            .positions
            .into_iter()
            .map(|position| PositionSnapshotResponse {
                token_id: position.token_id,
                net_qty: position.net_qty,
                avg_entry_price: position.avg_entry_price,
                realized_pnl: position.realized_pnl,
            })
            .collect(),
        pnl: PnlSnapshotResponse {
            realized_pnl: snapshot.pnl.realized_pnl,
            unrealized_pnl: snapshot.pnl.unrealized_pnl,
            total_fees: snapshot.pnl.total_fees,
            net_pnl: snapshot.pnl.net_pnl(),
        },
        risk: RiskSnapshotResponse {
            pending_intents: snapshot.risk.pending_intents,
            active_orders: snapshot.risk.active_orders,
            open_positions: snapshot.risk.open_positions,
            gross_exposure: snapshot.risk.gross_exposure,
            reserved_order_exposure: snapshot.risk.reserved_order_exposure,
            total_gross_exposure: snapshot.risk.total_gross_exposure,
        },
    }
}

pub fn restore_trading_runtime(snapshot: TradingStateSnapshot) -> io::Result<TradingRuntime> {
    let deployment_id = snapshot.deployment_id.clone();
    let persisted_positions = snapshot.positions.clone();
    let intents = snapshot
        .intents
        .into_iter()
        .map(|intent| {
            Ok(TradingIntent {
                intent_id: intent.intent_id,
                deployment_id: deployment_id.clone(),
                market_id: intent.market_id,
                token_id: intent.token_id,
                side: trade_side_from_wire(&intent.side)?,
                quantity: intent.quantity,
                limit_price: intent.limit_price,
                purpose: intent_purpose_from_contract(intent.purpose),
                created_at: intent.created_at,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let orders = snapshot
        .orders
        .into_iter()
        .map(|order| {
            Ok(ploy_trading::OrderRecord {
                order_id: order.order_id,
                intent_id: order.intent_id,
                deployment_id: deployment_id.clone(),
                token_id: order.token_id,
                requested_qty: order.requested_qty,
                limit_price: order.limit_price,
                venue_order_id: order.venue_order_id,
                venue_order_history: order.venue_order_history,
                revision: order.revision,
                state: order_state_from_wire(&order.state)?,
                state_changed_at: order.state_changed_at,
                filled_qty: order.filled_qty,
                rejection_reason: order.rejection_reason,
                last_error: order.last_error,
                idempotency_key: order.idempotency_key,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let fills = snapshot
        .fills
        .into_iter()
        .map(|fill| {
            Ok(ploy_trading::FillRecord {
                fill_id: fill.fill_id,
                order_id: fill.order_id,
                token_id: fill.token_id,
                side: trade_side_from_wire(&fill.side)?,
                quantity: fill.quantity,
                price: fill.price,
                fee: fill.fee,
                timestamp: fill.timestamp,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    let runtime = TradingRuntime::restore(TradingRuntimeSnapshot {
        intents,
        orders,
        fills,
        positions: Vec::new(),
        pnl: Default::default(),
        risk: Default::default(),
    });
    if !persisted_positions.is_empty() {
        let rebuilt = runtime.snapshot(&Default::default()).positions;
        let matches = persisted_positions.len() == rebuilt.len()
            && persisted_positions.iter().all(|persisted| {
                rebuilt.iter().any(|position| {
                    position.token_id == persisted.token_id
                        && position.net_qty == persisted.net_qty
                        && position.avg_entry_price == persisted.avg_entry_price
                        && position.realized_pnl == persisted.realized_pnl
                })
            });
        if !matches {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "persisted positions do not match fill-rebuilt positions",
            ));
        }
    }
    Ok(runtime)
}

pub fn build_order_control_response(
    deployment_id: String,
    order: &ploy_trading::OrderRecord,
) -> OrderControlResponse {
    OrderControlResponse {
        deployment_id,
        order_id: order.order_id.clone(),
        state: order_state_wire(order.state),
        venue_order_id: order.venue_order_id.clone(),
        venue_order_history: order.venue_order_history.clone(),
        revision: order.revision,
        requested_qty: order.requested_qty,
        limit_price: order.limit_price,
        rejection_reason: order.rejection_reason.clone(),
        last_error: order.last_error.clone(),
        filled_qty: order.filled_qty,
    }
}

pub fn trade_side_wire(side: TradeSide) -> String {
    match side {
        TradeSide::Buy => "buy".to_string(),
        TradeSide::Sell => "sell".to_string(),
    }
}

pub fn trade_side_from_wire(side: &str) -> io::Result<TradeSide> {
    match side {
        "buy" => Ok(TradeSide::Buy),
        "sell" => Ok(TradeSide::Sell),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported trade side `{other}`"),
        )),
    }
}

pub fn order_state_wire(state: OrderState) -> String {
    match state {
        OrderState::Pending => "pending".to_string(),
        OrderState::Unknown => "unknown".to_string(),
        OrderState::Acknowledged => "acknowledged".to_string(),
        OrderState::PartiallyFilled => "partially_filled".to_string(),
        OrderState::Filled => "filled".to_string(),
        OrderState::Canceled => "canceled".to_string(),
        OrderState::Rejected => "rejected".to_string(),
    }
}

pub fn order_state_from_wire(state: &str) -> io::Result<OrderState> {
    match state {
        "pending" => Ok(OrderState::Pending),
        "unknown" => Ok(OrderState::Unknown),
        "acknowledged" => Ok(OrderState::Acknowledged),
        "partially_filled" => Ok(OrderState::PartiallyFilled),
        "filled" => Ok(OrderState::Filled),
        "canceled" => Ok(OrderState::Canceled),
        "rejected" => Ok(OrderState::Rejected),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported order state `{other}`"),
        )),
    }
}

pub fn intent_purpose_wire(purpose: ploy_trading::IntentPurpose) -> IntentPurpose {
    match purpose {
        ploy_trading::IntentPurpose::Entry => IntentPurpose::Entry,
        ploy_trading::IntentPurpose::Exit => IntentPurpose::Exit,
        ploy_trading::IntentPurpose::Reduce => IntentPurpose::Reduce,
        ploy_trading::IntentPurpose::Hedge => IntentPurpose::Hedge,
        ploy_trading::IntentPurpose::Cancel => IntentPurpose::Cancel,
    }
}

pub fn intent_purpose_from_contract(purpose: IntentPurpose) -> ploy_trading::IntentPurpose {
    match purpose {
        IntentPurpose::Entry => ploy_trading::IntentPurpose::Entry,
        IntentPurpose::Exit => ploy_trading::IntentPurpose::Exit,
        IntentPurpose::Reduce => ploy_trading::IntentPurpose::Reduce,
        IntentPurpose::Hedge => ploy_trading::IntentPurpose::Hedge,
        IntentPurpose::Cancel => ploy_trading::IntentPurpose::Cancel,
    }
}

pub fn deployment_state_wire(state: DeploymentState) -> &'static str {
    match state {
        DeploymentState::Enabled => "enabled",
        DeploymentState::Draining => "draining",
        DeploymentState::Disabled => "disabled",
        DeploymentState::Archived => "archived",
    }
}

pub fn intent_counts_toward_exposure(purpose: ploy_trading::IntentPurpose) -> bool {
    matches!(
        purpose,
        ploy_trading::IntentPurpose::Entry | ploy_trading::IntentPurpose::Hedge
    )
}

pub fn intent_allowed_while_draining(purpose: ploy_trading::IntentPurpose) -> bool {
    matches!(
        purpose,
        ploy_trading::IntentPurpose::Exit
            | ploy_trading::IntentPurpose::Reduce
            | ploy_trading::IntentPurpose::Cancel
    )
}

pub fn account_token_exposure_envelope(
    deployments: &[DeploymentRecord],
    trading: &BTreeMap<String, TradingRuntime>,
    account_id: &str,
    token_id: &str,
) -> TokenExposureEnvelope {
    let mut settled_net_qty = Decimal::ZERO;
    let mut open_sell_qty = Decimal::ZERO;
    let mut open_buy_qty = Decimal::ZERO;

    for deployment in deployments.iter().filter(|deployment| {
        deployment.deployment_state != DeploymentState::Archived
            && deployment
                .account_id
                .trim()
                .eq_ignore_ascii_case(account_id.trim())
    }) {
        let Some(runtime) = trading.get(&deployment.deployment_id) else {
            continue;
        };
        settled_net_qty += runtime.positions().net_qty(token_id);
        for order in runtime.orders().orders().filter(|order| {
            order.token_id == token_id
                && matches!(
                    order.state,
                    OrderState::Pending
                        | OrderState::Unknown
                        | OrderState::Acknowledged
                        | OrderState::PartiallyFilled
                )
        }) {
            let remaining = (order.requested_qty - order.filled_qty).max(Decimal::ZERO);
            match runtime.intent(&order.intent_id).map(|intent| intent.side) {
                Some(TradeSide::Sell) => open_sell_qty += remaining,
                Some(TradeSide::Buy) => open_buy_qty += remaining,
                None => {
                    open_sell_qty += remaining;
                    open_buy_qty += remaining;
                }
            }
        }
    }

    TokenExposureEnvelope {
        settled_net_qty,
        worst_case_min_qty: settled_net_qty - open_sell_qty,
        worst_case_max_qty: settled_net_qty + open_buy_qty,
    }
}

pub fn intent_risk_effect(
    intent: &TradingIntent,
    exposure: TokenExposureEnvelope,
) -> IntentRiskEffect {
    match intent.purpose {
        ploy_trading::IntentPurpose::Entry => IntentRiskEffect::Increase,
        ploy_trading::IntentPurpose::Reduce | ploy_trading::IntentPurpose::Exit => {
            IntentRiskEffect::Reduce
        }
        ploy_trading::IntentPurpose::Cancel => IntentRiskEffect::Control,
        ploy_trading::IntentPurpose::Hedge => {
            let current_worst = exposure
                .worst_case_min_qty
                .abs()
                .max(exposure.worst_case_max_qty.abs());
            let signed_quantity = intent.signed_quantity();
            let next_worst = (exposure.worst_case_min_qty + signed_quantity)
                .abs()
                .max((exposure.worst_case_max_qty + signed_quantity).abs());
            if next_worst < current_worst {
                IntentRiskEffect::Reduce
            } else {
                IntentRiskEffect::Increase
            }
        }
    }
}

pub fn observed_state_for_desired(desired_state: DesiredState) -> ObservedState {
    match desired_state {
        DesiredState::Running => ObservedState::Starting,
        DesiredState::Paused => ObservedState::Paused,
        DesiredState::Stopped => ObservedState::Stopped,
    }
}

pub fn live_reconcile_backoff_ms(failures: u32, base_ms: u64, max_ms: u64) -> u64 {
    if failures == 0 {
        return 0;
    }
    let exponent = failures.saturating_sub(1).min(10);
    let scaled = base_ms.saturating_mul(2_u64.saturating_pow(exponent));
    scaled.min(max_ms.max(base_ms))
}

pub fn io_error_from_execution_error(err: ExecutionError) -> io::Error {
    match err {
        ExecutionError::Validation(message) => io::Error::new(io::ErrorKind::InvalidInput, message),
        ExecutionError::Configuration(message) => {
            io::Error::new(io::ErrorKind::InvalidData, message)
        }
        ExecutionError::Transport(message) => {
            io::Error::new(io::ErrorKind::ConnectionAborted, message)
        }
    }
}

pub fn next_paper_intent_id(deployment_id: &str) -> String {
    format!("{deployment_id}-{}", Uuid::new_v4())
}

pub fn write_json<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(parent)?;
    let body =
        serde_json::to_vec(value).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, &body)?;
    fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod tests {
    use super::{
        account_token_exposure_envelope, build_order_control_response, intent_risk_effect,
        live_reconcile_backoff_ms, observed_state_for_desired, order_state_from_wire,
        order_state_wire, restore_trading_runtime, trade_side_from_wire, trade_side_wire,
        IntentRiskEffect,
    };
    use ploy_operator_contracts::{
        DeploymentState, DesiredState, FillSnapshot, IntentPurpose, ObservedState, OrderSnapshot,
        PnlSnapshotResponse, PositionSnapshotResponse, RiskSnapshotResponse, TradingIntentSnapshot,
        TradingStateSnapshot,
    };
    use ploy_platform::DeploymentRecord;
    use ploy_trading::{
        IntentPurpose as TradingIntentPurpose, OrderRecord, OrderState, PositionSnapshot,
        TradeSide, TradingIntent, TradingRuntime, TradingRuntimeSnapshot,
    };
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::{collections::BTreeMap, io};

    #[test]
    fn state_and_side_wire_formats_round_trip() {
        assert_eq!(trade_side_wire(TradeSide::Buy), "buy");
        assert_eq!(trade_side_from_wire("sell").unwrap(), TradeSide::Sell);
        assert_eq!(
            order_state_wire(OrderState::PartiallyFilled),
            "partially_filled"
        );
        assert_eq!(
            order_state_from_wire("acknowledged").unwrap(),
            OrderState::Acknowledged
        );
        assert_eq!(
            observed_state_for_desired(DesiredState::Running),
            ObservedState::Starting
        );
        assert_eq!(DeploymentState::Enabled, DeploymentState::Enabled);
    }

    #[test]
    fn backoff_scales_and_caps() {
        assert_eq!(live_reconcile_backoff_ms(0, 500, 8_000), 0);
        assert_eq!(live_reconcile_backoff_ms(1, 500, 8_000), 500);
        assert_eq!(live_reconcile_backoff_ms(2, 500, 8_000), 1_000);
        assert_eq!(live_reconcile_backoff_ms(10, 500, 8_000), 8_000);
    }

    #[test]
    fn stacked_hedges_use_worst_case_active_and_unknown_order_exposure() {
        let deployment = |deployment_id: &str, account_id: &str, state| DeploymentRecord {
            deployment_id: deployment_id.to_string(),
            bundle_id: "example".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            account_id: account_id.to_string(),
            max_gross_exposure: Some(dec!(20)),
            deployment_state: state,
            desired_state: DesiredState::Running,
            observed_state: ObservedState::Running,
        };
        let intent = |deployment_id: &str, intent_id: &str, side, quantity| TradingIntent {
            intent_id: intent_id.to_string(),
            deployment_id: deployment_id.to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side,
            quantity,
            limit_price: Some(dec!(0.5)),
            purpose: TradingIntentPurpose::Hedge,
            created_at: chrono::Utc::now(),
        };
        let order = |deployment_id: &str,
                     order_id: &str,
                     intent_id: &str,
                     requested_qty,
                     filled_qty,
                     state| OrderRecord {
            order_id: order_id.to_string(),
            intent_id: intent_id.to_string(),
            deployment_id: deployment_id.to_string(),
            token_id: "token-1".to_string(),
            requested_qty,
            limit_price: Some(dec!(0.5)),
            venue_order_id: Some(format!("venue-{order_id}")),
            venue_order_history: Vec::new(),
            revision: 0,
            state,
            state_changed_at: Some(chrono::Utc::now()),
            filled_qty,
            rejection_reason: None,
            last_error: None,
            idempotency_key: None,
        };
        let deployments = vec![
            deployment("a.live", " 0xAbC ", DeploymentState::Enabled),
            deployment("b.live", "0xabc", DeploymentState::Enabled),
            deployment("archived.live", "0xabc", DeploymentState::Archived),
        ];
        let mut trading = BTreeMap::new();
        trading.insert(
            "a.live".to_string(),
            TradingRuntime::restore(TradingRuntimeSnapshot {
                intents: vec![intent("a.live", "sell-ack", TradeSide::Sell, dec!(4))],
                orders: vec![order(
                    "a.live",
                    "sell-ack-order",
                    "sell-ack",
                    dec!(4),
                    dec!(1),
                    OrderState::Acknowledged,
                )],
                positions: vec![PositionSnapshot {
                    token_id: "token-1".to_string(),
                    net_qty: dec!(5),
                    avg_entry_price: dec!(0.5),
                    realized_pnl: Decimal::ZERO,
                }],
                ..TradingRuntimeSnapshot::default()
            }),
        );
        trading.insert(
            "b.live".to_string(),
            TradingRuntime::restore(TradingRuntimeSnapshot {
                intents: vec![
                    intent("b.live", "sell-unknown", TradeSide::Sell, dec!(8)),
                    intent("b.live", "buy-ack", TradeSide::Buy, dec!(2)),
                ],
                orders: vec![
                    order(
                        "b.live",
                        "sell-unknown-order",
                        "sell-unknown",
                        dec!(8),
                        Decimal::ZERO,
                        OrderState::Unknown,
                    ),
                    order(
                        "b.live",
                        "buy-ack-order",
                        "buy-ack",
                        dec!(2),
                        Decimal::ZERO,
                        OrderState::Acknowledged,
                    ),
                ],
                ..TradingRuntimeSnapshot::default()
            }),
        );
        trading.insert(
            "archived.live".to_string(),
            TradingRuntime::restore(TradingRuntimeSnapshot {
                positions: vec![PositionSnapshot {
                    token_id: "token-1".to_string(),
                    net_qty: dec!(100),
                    avg_entry_price: dec!(0.5),
                    realized_pnl: Decimal::ZERO,
                }],
                ..TradingRuntimeSnapshot::default()
            }),
        );

        let exposure = account_token_exposure_envelope(&deployments, &trading, "0xABC", "token-1");
        assert_eq!(exposure.settled_net_qty, dec!(5));
        assert_eq!(exposure.worst_case_min_qty, dec!(-6));
        assert_eq!(exposure.worst_case_max_qty, dec!(7));

        let next_hedge = intent("a.live", "next-sell", TradeSide::Sell, dec!(1));
        assert_eq!(
            intent_risk_effect(&next_hedge, exposure),
            IntentRiskEffect::Increase
        );
    }

    #[test]
    fn order_control_response_preserves_fields() {
        let order = OrderRecord {
            order_id: "order-1".to_string(),
            intent_id: "intent-1".to_string(),
            deployment_id: "dep-1".to_string(),
            token_id: "123".to_string(),
            requested_qty: Decimal::new(100, 0),
            limit_price: Some(Decimal::new(55, 2)),
            venue_order_id: Some("venue-1".to_string()),
            venue_order_history: vec!["venue-0".to_string()],
            revision: 2,
            state: OrderState::Acknowledged,
            state_changed_at: Some(chrono::Utc::now()),
            filled_qty: Decimal::new(25, 0),
            rejection_reason: None,
            last_error: None,
            idempotency_key: None,
        };

        let response = build_order_control_response("dep-1".to_string(), &order);
        assert_eq!(response.deployment_id, "dep-1");
        assert_eq!(response.state, "acknowledged");
        assert_eq!(response.venue_order_id.as_deref(), Some("venue-1"));
    }

    #[test]
    fn restore_rejects_persisted_position_mismatch() {
        let error = restore_trading_runtime(TradingStateSnapshot {
            deployment_id: "dep-1".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            positions: vec![PositionSnapshotResponse {
                token_id: "token-1".to_string(),
                net_qty: dec!(4),
                avg_entry_price: dec!(0.25),
                realized_pnl: dec!(0.5),
            }],
            pnl: PnlSnapshotResponse {
                realized_pnl: dec!(0.5),
                unrealized_pnl: Decimal::ZERO,
                total_fees: dec!(0.02),
                net_pnl: dec!(0.48),
            },
            risk: RiskSnapshotResponse::default(),
            ..TradingStateSnapshot::default()
        })
        .expect_err("mismatched persisted position must block restore");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn restore_reconstructs_positions_from_filled_orders() {
        let now = chrono::Utc::now();
        let runtime = restore_trading_runtime(TradingStateSnapshot {
            deployment_id: "dep-1".to_string(),
            runtime_mode: ploy_operator_contracts::DeploymentRuntimeMode::Live,
            intents: vec![TradingIntentSnapshot {
                intent_id: "intent-1".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: "buy".to_string(),
                quantity: dec!(4),
                limit_price: Some(dec!(0.25)),
                purpose: IntentPurpose::Entry,
                created_at: now,
            }],
            orders: vec![OrderSnapshot {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                token_id: "token-1".to_string(),
                requested_qty: dec!(4),
                limit_price: Some(dec!(0.25)),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: Vec::new(),
                revision: 0,
                state: "filled".to_string(),
                state_changed_at: Some(now),
                filled_qty: dec!(4),
                rejection_reason: None,
                last_error: None,
                idempotency_key: None,
            }],
            fills: vec![FillSnapshot {
                fill_id: "fill-1".to_string(),
                order_id: "order-1".to_string(),
                token_id: "token-1".to_string(),
                side: "buy".to_string(),
                quantity: dec!(4),
                price: dec!(0.25),
                fee: dec!(0.01),
                timestamp: now,
            }],
            positions: Vec::new(),
            ..TradingStateSnapshot::default()
        })
        .expect("restore filled runtime");

        let restored = runtime.snapshot(&Default::default());
        assert_eq!(restored.positions[0].net_qty, dec!(4));
        assert_eq!(restored.fills.len(), 1);
        assert_eq!(restored.orders[0].state, OrderState::Filled);
        assert_eq!(restored.orders[0].state_changed_at, Some(now));
    }
}
