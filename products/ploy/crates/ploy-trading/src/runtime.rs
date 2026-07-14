use crate::fills::{FillLedger, FillRecord};
use crate::intents::{IntentPurpose, TradeSide, TradingIntent};
use crate::orders::{OrderLedger, OrderState};
use crate::pnl::PnlSnapshot;
use crate::positions::{PositionLedger, PositionSnapshot};
use crate::risk::{snapshot_from_state, RiskSnapshot};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TradingRuntimeError {
    #[error("{0} must not be empty")]
    EmptyIdentifier(&'static str),
    #[error("{0} already exists")]
    DuplicateIdentifier(&'static str),
    #[error("{0}")]
    InvalidIntent(&'static str),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TradingRuntimeSnapshot {
    pub intents: Vec<TradingIntent>,
    pub orders: Vec<crate::orders::OrderRecord>,
    pub fills: Vec<FillRecord>,
    pub positions: Vec<PositionSnapshot>,
    pub pnl: PnlSnapshot,
    pub risk: RiskSnapshot,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TradeCashflowSummary {
    pub buy_shares: Decimal,
    pub sell_shares: Decimal,
    pub gross_buy_cost: Decimal,
    pub gross_sell_proceeds: Decimal,
    pub total_fees: Decimal,
}

impl TradeCashflowSummary {
    pub fn deployed_capital(&self) -> Decimal {
        self.gross_buy_cost
    }

    pub fn net_pnl(&self) -> Decimal {
        self.gross_sell_proceeds - self.gross_buy_cost - self.total_fees
    }

    pub fn roi_on_deployed_capital(&self) -> Option<Decimal> {
        let deployed = self.deployed_capital();
        if deployed.is_zero() {
            None
        } else {
            Some(self.net_pnl() / deployed)
        }
    }
}

impl TradingRuntimeSnapshot {
    pub fn fill_cashflow_summary(&self) -> TradeCashflowSummary {
        let mut summary = TradeCashflowSummary::default();

        for fill in &self.fills {
            let notional = fill.quantity * fill.price;
            summary.total_fees += fill.fee;

            match fill.side {
                TradeSide::Buy => {
                    summary.buy_shares += fill.quantity;
                    summary.gross_buy_cost += notional;
                }
                TradeSide::Sell => {
                    summary.sell_shares += fill.quantity;
                    summary.gross_sell_proceeds += notional;
                }
            }
        }

        summary
    }
}

#[derive(Debug, Default)]
pub struct TradingRuntime {
    intents: Vec<TradingIntent>,
    intent_by_id: BTreeMap<String, usize>,
    order_by_idempotency_key: BTreeMap<String, (String, TradingIntent)>,
    orders: OrderLedger,
    fills: FillLedger,
    positions: PositionLedger,
}

impl TradingRuntime {
    pub fn restore(snapshot: TradingRuntimeSnapshot) -> Self {
        let positions = if snapshot.positions.is_empty() {
            let mut positions = PositionLedger::default();
            for fill in &snapshot.fills {
                positions.apply_fill(fill);
            }
            positions
        } else {
            PositionLedger::restore(snapshot.positions, snapshot.pnl.total_fees)
        };

        let intent_by_id = snapshot
            .intents
            .iter()
            .enumerate()
            .map(|(index, intent)| (intent.intent_id.clone(), index))
            .collect();

        let order_by_idempotency_key = snapshot
            .orders
            .iter()
            .filter_map(|order| {
                let key = order.idempotency_key.clone()?;
                let intent = snapshot
                    .intents
                    .iter()
                    .find(|intent| intent.intent_id == order.intent_id)?
                    .clone();
                Some((key, (order.order_id.clone(), intent)))
            })
            .collect();

        Self {
            intents: snapshot.intents,
            intent_by_id,
            order_by_idempotency_key,
            orders: OrderLedger::restore(snapshot.orders),
            fills: FillLedger::restore(snapshot.fills),
            positions,
        }
    }

    pub fn submit_intent(
        &mut self,
        intent: TradingIntent,
        order_id: impl Into<String>,
        idempotency_key: Option<&str>,
    ) -> Result<&crate::orders::OrderRecord, TradingRuntimeError> {
        let idempotency_key = idempotency_key.map(str::trim).filter(|key| !key.is_empty());
        if let Some(existing_order_id) = self.idempotent_order_id(&intent, idempotency_key)? {
            return Ok(self
                .orders
                .order(&existing_order_id)
                .expect("idempotency key references an existing order"));
        }
        let order_id = order_id.into();
        if intent.intent_id.trim().is_empty() {
            return Err(TradingRuntimeError::EmptyIdentifier("intent_id"));
        }
        if order_id.trim().is_empty() {
            return Err(TradingRuntimeError::EmptyIdentifier("order_id"));
        }
        if self
            .orders
            .orders()
            .any(|order| order.intent_id == intent.intent_id)
        {
            return Err(TradingRuntimeError::DuplicateIdentifier("intent_id"));
        }
        if self.orders.contains(&order_id) {
            return Err(TradingRuntimeError::DuplicateIdentifier("order_id"));
        }
        if intent.quantity <= Decimal::ZERO {
            return Err(TradingRuntimeError::InvalidIntent(
                "quantity must be greater than zero",
            ));
        }
        if intent
            .limit_price
            .is_some_and(|price| price <= Decimal::ZERO || price >= Decimal::ONE)
        {
            return Err(TradingRuntimeError::InvalidIntent(
                "limit_price must be between zero and one",
            ));
        }
        if intent.purpose == IntentPurpose::Cancel {
            return Err(TradingRuntimeError::InvalidIntent(
                "cancel purpose cannot submit an order",
            ));
        }
        if matches!(intent.purpose, IntentPurpose::Reduce | IntentPurpose::Exit)
            && !self.positions.can_reduce(
                &intent.token_id,
                intent.side,
                intent.quantity + self.reserved_reduction_qty(&intent, None),
            )
        {
            return Err(TradingRuntimeError::InvalidIntent(
                "reduce or exit must decrease an existing position without flipping it",
            ));
        }

        self.prune_inactive_intents();
        let index = self.intents.len();
        self.intent_by_id.insert(intent.intent_id.clone(), index);
        self.intents.push(intent.clone());
        self.orders.insert_from_intent(order_id.clone(), &intent);
        if let Some(key) = idempotency_key {
            self.orders.set_idempotency_key(&order_id, key);
            self.order_by_idempotency_key
                .insert(key.to_string(), (order_id.clone(), intent));
        }
        Ok(self.orders.order(&order_id).expect("order inserted"))
    }

    pub fn idempotent_order(
        &self,
        intent: &TradingIntent,
        idempotency_key: Option<&str>,
    ) -> Result<Option<&crate::orders::OrderRecord>, TradingRuntimeError> {
        let Some(existing_order_id) = self.idempotent_order_id(intent, idempotency_key)? else {
            return Ok(None);
        };
        Ok(Some(
            self.orders
                .order(&existing_order_id)
                .expect("idempotency key references an existing order"),
        ))
    }

    fn idempotent_order_id(
        &self,
        intent: &TradingIntent,
        idempotency_key: Option<&str>,
    ) -> Result<Option<String>, TradingRuntimeError> {
        let Some((existing_order_id, existing_intent)) = idempotency_key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .and_then(|key| self.order_by_idempotency_key.get(key))
        else {
            return Ok(None);
        };
        if !same_idempotent_payload(existing_intent, intent) {
            return Err(TradingRuntimeError::InvalidIntent(
                "idempotency key payload mismatch",
            ));
        }
        Ok(Some(existing_order_id.clone()))
    }

    fn reserved_reduction_qty(
        &self,
        intent: &TradingIntent,
        excluded_order_id: Option<&str>,
    ) -> Decimal {
        self.orders
            .orders()
            .filter(|order| {
                matches!(
                    order.state,
                    OrderState::Pending
                        | OrderState::Unknown
                        | OrderState::Acknowledged
                        | OrderState::PartiallyFilled
                ) && order.token_id == intent.token_id
                    && excluded_order_id != Some(order.order_id.as_str())
            })
            .filter(|order| {
                self.intent(&order.intent_id).is_some_and(|existing| {
                    existing.side == intent.side
                        && matches!(
                            existing.purpose,
                            IntentPurpose::Reduce | IntentPurpose::Exit
                        )
                })
            })
            .map(|order| (order.requested_qty - order.filled_qty).max(Decimal::ZERO))
            .sum()
    }

    pub fn validate_order_replacement(
        &self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
    ) -> Result<(), TradingRuntimeError> {
        let order = self
            .orders
            .order(order_id)
            .ok_or(TradingRuntimeError::InvalidIntent("order not found"))?;
        if requested_qty <= Decimal::ZERO {
            return Err(TradingRuntimeError::InvalidIntent(
                "quantity must be greater than zero",
            ));
        }
        if requested_qty < order.filled_qty {
            return Err(TradingRuntimeError::InvalidIntent(
                "replacement quantity cannot be below filled quantity",
            ));
        }
        if limit_price.is_some_and(|price| price <= Decimal::ZERO || price >= Decimal::ONE) {
            return Err(TradingRuntimeError::InvalidIntent(
                "limit_price must be between zero and one",
            ));
        }
        let intent = self
            .intent(&order.intent_id)
            .ok_or(TradingRuntimeError::InvalidIntent("intent not found"))?;
        if matches!(intent.purpose, IntentPurpose::Reduce | IntentPurpose::Exit) {
            let replacement_remaining = (requested_qty - order.filled_qty).max(Decimal::ZERO);
            let reserved = self.reserved_reduction_qty(intent, Some(order_id));
            if !self.positions.can_reduce(
                &intent.token_id,
                intent.side,
                replacement_remaining + reserved,
            ) {
                return Err(TradingRuntimeError::InvalidIntent(
                    "reduce or exit replacement must not increase or flip the position",
                ));
            }
        }
        Ok(())
    }

    pub fn acknowledge_order(
        &mut self,
        order_id: &str,
        venue_order_id: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.acknowledge(order_id, venue_order_id)
    }

    pub fn replace_order(
        &mut self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
        venue_order_id: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders
            .replace(order_id, requested_qty, limit_price, venue_order_id)
    }

    pub fn reject_order(
        &mut self,
        order_id: &str,
        reason: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.reject(order_id, reason)
    }

    pub fn record_order_error(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.record_error(order_id, error)
    }

    pub fn mark_order_unknown(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&crate::orders::OrderRecord> {
        self.orders.mark_unknown(order_id, error)
    }

    pub fn cancel_order(&mut self, order_id: &str) -> Option<&crate::orders::OrderRecord> {
        self.orders.cancel(order_id)
    }

    pub fn cancel_active_entry_orders_for_market(&mut self, market_id: &str) -> usize {
        let order_ids = self
            .orders
            .orders()
            .filter(|order| {
                matches!(
                    order.state,
                    OrderState::Pending
                        | OrderState::Unknown
                        | OrderState::Acknowledged
                        | OrderState::PartiallyFilled
                )
            })
            .filter(|order| {
                self.intent(&order.intent_id).is_some_and(|intent| {
                    intent.market_id == market_id
                        && matches!(intent.purpose, IntentPurpose::Entry | IntentPurpose::Hedge)
                })
            })
            .map(|order| order.order_id.clone())
            .collect::<Vec<_>>();

        for order_id in &order_ids {
            self.orders.cancel(order_id);
        }
        if !order_ids.is_empty() {
            self.prune_inactive_intents();
        }
        order_ids.len()
    }

    pub fn order(&self, order_id: &str) -> Option<&crate::orders::OrderRecord> {
        self.orders.order(order_id)
    }

    pub fn intent(&self, intent_id: &str) -> Option<&TradingIntent> {
        self.intent_by_id
            .get(intent_id)
            .and_then(|index| self.intents.get(*index))
    }

    pub fn record_fill(&mut self, fill: FillRecord) -> bool {
        if fill.fill_id.trim().is_empty() || self.fills.contains(&fill.fill_id) {
            return false;
        }
        if fill.quantity <= Decimal::ZERO || fill.price <= Decimal::ZERO || fill.fee < Decimal::ZERO
        {
            return false;
        }
        let Some(order) = self.orders.order(&fill.order_id) else {
            return false;
        };
        let Some(intent) = self.intent(&order.intent_id) else {
            return false;
        };
        if fill.token_id != order.token_id || fill.side != intent.side {
            return false;
        }
        let remaining_qty = (order.requested_qty - order.filled_qty).max(Decimal::ZERO);
        let price_improved_overfill =
            fill.quantity > remaining_qty && self.buy_fill_is_price_improved_overfill(&fill);
        if fill.quantity > remaining_qty && !price_improved_overfill {
            return false;
        }
        let updated = if price_improved_overfill {
            self.orders.apply_price_improved_buy_fill(&fill)
        } else {
            self.orders.apply_fill(&fill)
        };
        if updated.is_none() {
            return false;
        }
        self.positions.apply_fill(&fill);
        self.fills.record(fill);
        self.prune_inactive_intents();
        true
    }

    fn buy_fill_is_price_improved_overfill(&self, fill: &FillRecord) -> bool {
        if fill.side != TradeSide::Buy {
            return false;
        }

        let Some(order) = self.orders.order(&fill.order_id) else {
            return false;
        };
        let Some(limit_price) = order.limit_price else {
            return false;
        };
        let Some(intent) = self.intent(&order.intent_id) else {
            return false;
        };
        if intent.side != TradeSide::Buy
            || matches!(intent.purpose, IntentPurpose::Reduce | IntentPurpose::Exit)
        {
            return false;
        }

        let requested_notional = order.requested_qty.max(Decimal::ZERO) * limit_price;
        let recorded_notional: Decimal = self
            .fills
            .all()
            .iter()
            .filter(|existing| existing.order_id == fill.order_id)
            .map(|existing| {
                existing.quantity.max(Decimal::ZERO) * existing.price.max(Decimal::ZERO)
            })
            .sum();
        let fill_notional = fill.quantity.max(Decimal::ZERO) * fill.price.max(Decimal::ZERO);
        let remaining_notional = (requested_notional - recorded_notional).max(Decimal::ZERO);

        fill_notional <= remaining_notional + Decimal::new(2, 2)
    }

    pub fn last_fill_time(&self) -> Option<DateTime<Utc>> {
        self.fills.all().iter().map(|fill| fill.timestamp).max()
    }

    /// Read-only access to the position ledger.
    pub fn positions(&self) -> &PositionLedger {
        &self.positions
    }

    /// Read-only access to the order ledger.
    pub fn orders(&self) -> &OrderLedger {
        &self.orders
    }

    fn prune_inactive_intents(&mut self) {
        let retained_intent_ids = self
            .orders
            .orders()
            .filter(|order| {
                matches!(
                    order.state,
                    crate::orders::OrderState::Pending
                        | crate::orders::OrderState::Unknown
                        | crate::orders::OrderState::Acknowledged
                        | crate::orders::OrderState::PartiallyFilled
                ) || order.idempotency_key.is_some()
                    || self.positions.net_qty(&order.token_id) != Decimal::ZERO
            })
            .map(|order| order.intent_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();

        if retained_intent_ids.len() == self.intents.len() {
            return;
        }

        self.intents
            .retain(|intent| retained_intent_ids.contains(intent.intent_id.as_str()));
        self.intent_by_id = self
            .intents
            .iter()
            .enumerate()
            .map(|(index, intent)| (intent.intent_id.clone(), index))
            .collect();
    }

    pub fn snapshot(&self, mark_prices: &BTreeMap<String, Decimal>) -> TradingRuntimeSnapshot {
        let orders = self.orders.orders().cloned().collect::<Vec<_>>();
        let active_intents = self
            .intents
            .iter()
            .filter(|intent| {
                orders.iter().any(|order| {
                    order.intent_id == intent.intent_id
                        && matches!(
                            order.state,
                            crate::orders::OrderState::Pending
                                | crate::orders::OrderState::Unknown
                                | crate::orders::OrderState::Acknowledged
                                | crate::orders::OrderState::PartiallyFilled
                        )
                })
            })
            .cloned()
            .collect::<Vec<_>>();

        TradingRuntimeSnapshot {
            intents: self.intents.clone(),
            orders,
            fills: self.fills.all().to_vec(),
            positions: self.positions.positions().cloned().collect(),
            pnl: self.positions.pnl_snapshot(mark_prices),
            risk: snapshot_from_state(&active_intents, &self.orders, &self.positions),
        }
    }
}

fn same_idempotent_payload(left: &TradingIntent, right: &TradingIntent) -> bool {
    left.deployment_id == right.deployment_id
        && left.market_id == right.market_id
        && left.token_id == right.token_id
        && left.side == right.side
        && left.quantity == right.quantity
        && left.limit_price == right.limit_price
        && left.purpose == right.purpose
}

#[cfg(test)]
mod tests {
    use super::{TradingRuntime, TradingRuntimeError};
    use crate::{
        FillRecord, IntentPurpose, OrderRecord, OrderState, PnlSnapshot, PositionSnapshot,
        TradeSide, TradingIntent, TradingRuntimeSnapshot,
    };
    use chrono::Utc;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    #[test]
    fn restore_rebuilds_positions_and_active_risk_from_snapshot() {
        let snapshot = super::TradingRuntimeSnapshot {
            intents: vec![TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.45)),
                purpose: IntentPurpose::Entry,
                created_at: Utc::now(),
            }],
            orders: vec![OrderRecord {
                order_id: "order-1".to_string(),
                intent_id: "intent-1".to_string(),
                deployment_id: "example.live".to_string(),
                token_id: "token-1".to_string(),
                requested_qty: dec!(2),
                limit_price: Some(dec!(0.45)),
                venue_order_id: Some("venue-1".to_string()),
                venue_order_history: vec!["venue-0".to_string()],
                revision: 1,
                state: OrderState::PartiallyFilled,
                state_changed_at: Some(Utc::now()),
                filled_qty: dec!(1),
                rejection_reason: None,
                last_error: None,
                idempotency_key: None,
            }],
            fills: vec![FillRecord {
                fill_id: "fill-1".to_string(),
                order_id: "order-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(1),
                price: dec!(0.45),
                fee: dec!(0.02),
                timestamp: Utc::now(),
            }],
            positions: Vec::new(),
            pnl: Default::default(),
            risk: Default::default(),
        };

        let runtime = TradingRuntime::restore(snapshot);
        let restored = runtime.snapshot(&BTreeMap::new());
        assert_eq!(restored.orders.len(), 1);
        assert_eq!(restored.fills.len(), 1);
        assert_eq!(restored.positions.len(), 1);
        assert_eq!(restored.positions[0].net_qty, dec!(1));
        assert_eq!(restored.risk.active_orders, 1);
    }

    #[test]
    fn cancel_active_entry_orders_for_market_releases_expired_event_reserve() {
        let mut runtime = TradingRuntime::default();
        let opened_at = Utc::now();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-expired".to_string(),
                    deployment_id: "example.replay".to_string(),
                    market_id: "expired-event".to_string(),
                    token_id: "token-expired".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(10),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Entry,
                    created_at: opened_at,
                },
                "order-expired",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-expired", "venue-expired");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-expired-partial".to_string(),
            order_id: "order-expired".to_string(),
            token_id: "token-expired".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(4),
            price: dec!(0.60),
            fee: Decimal::ZERO,
            timestamp: opened_at,
        }));

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-live".to_string(),
                    deployment_id: "example.replay".to_string(),
                    market_id: "live-event".to_string(),
                    token_id: "token-live".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.50)),
                    purpose: IntentPurpose::Entry,
                    created_at: opened_at,
                },
                "order-live",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-live", "venue-live");

        let before = runtime.snapshot(&BTreeMap::new()).risk;
        assert_eq!(before.active_orders, 2);
        assert_eq!(before.reserved_order_exposure, dec!(4.60));

        assert_eq!(
            runtime.cancel_active_entry_orders_for_market("expired-event"),
            1
        );
        assert_eq!(
            runtime.order("order-expired").expect("expired order").state,
            OrderState::Canceled
        );
        assert_eq!(
            runtime.order("order-live").expect("live order").state,
            OrderState::Acknowledged
        );

        let after = runtime.snapshot(&BTreeMap::new()).risk;
        assert_eq!(after.active_orders, 1);
        assert_eq!(after.reserved_order_exposure, dec!(1.00));
        assert_eq!(after.gross_exposure, dec!(2.40));
    }

    #[test]
    fn restore_preserves_persisted_positions_when_fills_are_absent() {
        let snapshot = TradingRuntimeSnapshot {
            positions: vec![PositionSnapshot {
                token_id: "token-1".to_string(),
                net_qty: dec!(3),
                avg_entry_price: dec!(0.42),
                realized_pnl: dec!(0.7),
            }],
            pnl: PnlSnapshot {
                realized_pnl: dec!(0.7),
                unrealized_pnl: Decimal::ZERO,
                total_fees: dec!(0.03),
            },
            ..TradingRuntimeSnapshot::default()
        };

        let runtime = TradingRuntime::restore(snapshot);
        let restored = runtime.snapshot(&BTreeMap::new());

        assert_eq!(restored.positions.len(), 1);
        assert_eq!(restored.positions[0].net_qty, dec!(3));
        assert_eq!(restored.pnl.realized_pnl, dec!(0.7));
        assert_eq!(restored.pnl.total_fees, dec!(0.03));
        assert_eq!(restored.risk.open_positions, 1);
        assert_eq!(restored.risk.gross_exposure, dec!(1.26));
    }

    #[test]
    fn closed_position_intents_are_pruned_from_lookup() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-1",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-1", "venue-1");
        assert!(runtime.intent("intent-1").is_some());

        runtime.record_fill(FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });
        assert!(runtime.intent("intent-1").is_some());

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-exit".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "order-exit",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-exit", "venue-exit");
        runtime.record_fill(FillRecord {
            fill_id: "fill-2".to_string(),
            order_id: "order-exit".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Sell,
            quantity: dec!(1),
            price: dec!(0.60),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });
        assert!(runtime.intent("intent-1").is_none());
        assert!(runtime.snapshot(&BTreeMap::new()).intents.is_empty());
    }

    #[test]
    fn price_improved_buy_fill_can_exceed_requested_shares_within_notional_cap() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-buy".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(28.30),
                    limit_price: Some(dec!(0.53)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-buy",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-buy", "venue-buy");

        let recorded = runtime.record_fill(FillRecord {
            fill_id: "fill-buy".to_string(),
            order_id: "order-buy".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(31.466665),
            price: dec!(0.4767),
            fee: dec!(0.12),
            timestamp: Utc::now(),
        });

        assert!(recorded);
        let order = runtime.order("order-buy").expect("order");
        assert_eq!(order.state, OrderState::Filled);
        assert_eq!(order.filled_qty, dec!(31.466665));
        let snapshot = runtime.snapshot(&BTreeMap::new());
        assert_eq!(snapshot.fills.len(), 1);
        assert_eq!(snapshot.positions[0].net_qty, dec!(31.466665));
    }

    #[test]
    fn price_improved_buy_overfill_still_rejects_above_notional_cap() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-buy".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(28.30),
                    limit_price: Some(dec!(0.53)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-buy",
                None,
            )
            .expect("valid intent");
        runtime.acknowledge_order("order-buy", "venue-buy");

        let recorded = runtime.record_fill(FillRecord {
            fill_id: "fill-buy".to_string(),
            order_id: "order-buy".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(40),
            price: dec!(0.53),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        });

        assert!(!recorded);
        let order = runtime.order("order-buy").expect("order");
        assert_eq!(order.state, OrderState::Acknowledged);
        assert_eq!(order.filled_qty, Decimal::ZERO);
    }

    #[test]
    fn price_improved_exit_buy_overfill_cannot_flip_short_position() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-short".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
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
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-short".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "order-exit",
                None,
            )
            .expect("valid exit");
        let before = runtime.snapshot(&BTreeMap::new());

        assert!(!runtime.record_fill(FillRecord {
            fill_id: "fill-exit".to_string(),
            order_id: "order-exit".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(3),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn cashflow_summary_treats_quantity_as_shares_not_dollars() {
        let now = Utc::now();
        let snapshot = TradingRuntimeSnapshot {
            fills: vec![
                FillRecord {
                    fill_id: "fill-buy".to_string(),
                    order_id: "order-buy".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(25),
                    price: dec!(0.40),
                    fee: dec!(0.05),
                    timestamp: now,
                },
                FillRecord {
                    fill_id: "fill-sell".to_string(),
                    order_id: "order-sell".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(25),
                    price: dec!(1.00),
                    fee: Decimal::ZERO,
                    timestamp: now,
                },
            ],
            ..Default::default()
        };

        let summary = snapshot.fill_cashflow_summary();
        assert_eq!(summary.buy_shares, dec!(25));
        assert_eq!(summary.sell_shares, dec!(25));
        assert_eq!(summary.gross_buy_cost, dec!(10.00));
        assert_eq!(summary.gross_sell_proceeds, dec!(25.00));
        assert_eq!(summary.deployed_capital(), dec!(10.00));
        assert_eq!(summary.net_pnl(), dec!(14.95));
        assert_eq!(
            summary.roi_on_deployed_capital().expect("roi").round_dp(4),
            dec!(1.4950)
        );
    }

    #[test]
    fn duplicate_intent_and_order_ids_do_not_overwrite() {
        let mut runtime = TradingRuntime::default();
        let intent = TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "dep-1".to_string(),
            market_id: "market-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        };
        runtime
            .submit_intent(intent.clone(), "order-1", None)
            .expect("valid intent");
        let before = runtime.snapshot(&BTreeMap::new());

        let mut duplicate_intent = intent.clone();
        duplicate_intent.quantity = dec!(2);
        assert_eq!(
            runtime.submit_intent(duplicate_intent, "order-2", None),
            Err(TradingRuntimeError::DuplicateIdentifier("intent_id"))
        );
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);

        let mut duplicate_order = intent;
        duplicate_order.intent_id = "intent-2".to_string();
        assert_eq!(
            runtime.submit_intent(duplicate_order, "order-1", None),
            Err(TradingRuntimeError::DuplicateIdentifier("order_id"))
        );
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn cancel_purpose_cannot_submit_order() {
        let mut runtime = TradingRuntime::default();
        let before = runtime.snapshot(&BTreeMap::new());

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "cancel-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(1),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Cancel,
                    created_at: Utc::now(),
                },
                "order-cancel",
                None,
            )
            .expect_err("cancel cannot submit");

        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn exit_cannot_increase_or_flip_position() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-entry",
                None,
            )
            .expect("valid intent");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-entry".to_string(),
            order_id: "order-entry".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        let before = runtime.snapshot(&BTreeMap::new());

        for (intent_id, order_id, side, quantity) in [
            ("exit-increase", "order-increase", TradeSide::Buy, dec!(1)),
            ("exit-flip", "order-flip", TradeSide::Sell, dec!(3)),
        ] {
            runtime
                .submit_intent(
                    TradingIntent {
                        intent_id: intent_id.to_string(),
                        deployment_id: "dep-1".to_string(),
                        market_id: "market-1".to_string(),
                        token_id: "token-1".to_string(),
                        side,
                        quantity,
                        limit_price: Some(dec!(0.40)),
                        purpose: IntentPurpose::Exit,
                        created_at: Utc::now(),
                    },
                    order_id,
                    None,
                )
                .expect_err("invalid exit");
            assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
        }
    }

    #[test]
    fn active_exit_quantity_is_reserved_before_accepting_another_exit() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "entry-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-entry",
                None,
            )
            .expect("valid entry");
        assert!(runtime.record_fill(FillRecord {
            fill_id: "fill-entry".to_string(),
            order_id: "order-entry".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        }));
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Exit,
                    created_at: Utc::now(),
                },
                "order-exit-1",
                None,
            )
            .expect("first exit");
        let before = runtime.snapshot(&BTreeMap::new());

        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "exit-2".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Sell,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.60)),
                    purpose: IntentPurpose::Reduce,
                    created_at: Utc::now(),
                },
                "order-exit-2",
                None,
            )
            .expect_err("second exit exceeds unreserved position");
        assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
    }

    #[test]
    fn fill_token_side_and_numeric_invariants_are_enforced() {
        let mut runtime = TradingRuntime::default();
        runtime
            .submit_intent(
                TradingIntent {
                    intent_id: "intent-1".to_string(),
                    deployment_id: "dep-1".to_string(),
                    market_id: "market-1".to_string(),
                    token_id: "token-1".to_string(),
                    side: TradeSide::Buy,
                    quantity: dec!(2),
                    limit_price: Some(dec!(0.40)),
                    purpose: IntentPurpose::Entry,
                    created_at: Utc::now(),
                },
                "order-1",
                None,
            )
            .expect("valid intent");
        let before = runtime.snapshot(&BTreeMap::new());
        let valid = FillRecord {
            fill_id: String::new(),
            order_id: "order-1".to_string(),
            token_id: "token-1".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.40),
            fee: Decimal::ZERO,
            timestamp: Utc::now(),
        };
        let invalid = [
            FillRecord {
                fill_id: "   ".to_string(),
                ..valid.clone()
            },
            FillRecord {
                fill_id: "zero-qty".to_string(),
                quantity: Decimal::ZERO,
                ..valid.clone()
            },
            FillRecord {
                fill_id: "zero-price".to_string(),
                price: Decimal::ZERO,
                ..valid.clone()
            },
            FillRecord {
                fill_id: "negative-fee".to_string(),
                fee: dec!(-0.01),
                ..valid.clone()
            },
            FillRecord {
                fill_id: "wrong-token".to_string(),
                token_id: "token-2".to_string(),
                ..valid.clone()
            },
            FillRecord {
                fill_id: "wrong-side".to_string(),
                side: TradeSide::Sell,
                ..valid.clone()
            },
            FillRecord {
                fill_id: "overfill".to_string(),
                quantity: dec!(3),
                ..valid
            },
        ];

        for fill in invalid {
            assert!(!runtime.record_fill(fill));
            assert_eq!(runtime.snapshot(&BTreeMap::new()), before);
        }
    }
}
