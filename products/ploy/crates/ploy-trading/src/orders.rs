use crate::fills::FillRecord;
use crate::intents::TradingIntent;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    Pending,
    Unknown,
    Acknowledged,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderRecord {
    pub order_id: String,
    pub intent_id: String,
    pub deployment_id: String,
    pub token_id: String,
    pub requested_qty: Decimal,
    pub limit_price: Option<Decimal>,
    pub venue_order_id: Option<String>,
    #[serde(default)]
    pub venue_order_history: Vec<String>,
    #[serde(default)]
    pub revision: u32,
    pub state: OrderState,
    #[serde(default)]
    pub state_changed_at: Option<DateTime<Utc>>,
    pub filled_qty: Decimal,
    pub rejection_reason: Option<String>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Default)]
pub struct OrderLedger {
    orders: BTreeMap<String, OrderRecord>,
}

impl OrderLedger {
    pub fn restore(records: Vec<OrderRecord>) -> Self {
        let orders = records
            .into_iter()
            .map(|mut record| {
                if record.state_changed_at.is_none() {
                    record.state_changed_at = Some(Utc::now());
                }
                (record.order_id.clone(), record)
            })
            .collect();
        Self { orders }
    }

    pub fn insert_from_intent(
        &mut self,
        order_id: impl Into<String>,
        intent: &TradingIntent,
    ) -> &OrderRecord {
        let order_id = order_id.into();
        let record = OrderRecord {
            order_id: order_id.clone(),
            intent_id: intent.intent_id.clone(),
            deployment_id: intent.deployment_id.clone(),
            token_id: intent.token_id.clone(),
            requested_qty: intent.quantity,
            limit_price: intent.limit_price,
            venue_order_id: None,
            venue_order_history: Vec::new(),
            revision: 0,
            state: OrderState::Pending,
            state_changed_at: Some(Utc::now()),
            filled_qty: Decimal::ZERO,
            rejection_reason: None,
            last_error: None,
            idempotency_key: None,
        };
        self.orders.insert(order_id.clone(), record);
        self.orders.get(&order_id).expect("order inserted")
    }

    pub fn set_idempotency_key(&mut self, order_id: &str, key: impl Into<String>) {
        if let Some(order) = self.orders.get_mut(order_id) {
            order.idempotency_key = Some(key.into());
        }
    }

    pub fn acknowledge(
        &mut self,
        order_id: &str,
        venue_order_id: impl Into<String>,
    ) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        record.venue_order_id = Some(venue_order_id.into());
        if matches!(
            record.state,
            OrderState::Pending | OrderState::Unknown | OrderState::Acknowledged
        ) {
            set_state(record, OrderState::Acknowledged);
            record.last_error = None;
        }
        Some(record)
    }

    pub fn replace(
        &mut self,
        order_id: &str,
        requested_qty: Decimal,
        limit_price: Option<Decimal>,
        venue_order_id: impl Into<String>,
    ) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        let next_venue_order_id = venue_order_id.into();
        if let Some(current_venue_order_id) =
            record.venue_order_id.replace(next_venue_order_id.clone())
        {
            if current_venue_order_id != next_venue_order_id {
                record.venue_order_history.push(current_venue_order_id);
            }
        }
        record.requested_qty = requested_qty;
        record.limit_price = limit_price;
        record.revision += 1;
        record.last_error = None;
        let state = if record.filled_qty >= record.requested_qty {
            OrderState::Filled
        } else if record.filled_qty > Decimal::ZERO {
            OrderState::PartiallyFilled
        } else {
            OrderState::Acknowledged
        };
        set_state(record, state);
        Some(record)
    }

    pub fn reject(&mut self, order_id: &str, reason: impl Into<String>) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        let reason = reason.into();
        if matches!(
            record.state,
            OrderState::PartiallyFilled | OrderState::Filled | OrderState::Canceled
        ) {
            record.last_error = Some(reason);
            return Some(record);
        }
        set_state(record, OrderState::Rejected);
        record.rejection_reason = Some(reason.clone());
        record.last_error = Some(reason);
        Some(record)
    }

    pub fn record_error(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        record.last_error = Some(error.into());
        Some(record)
    }

    pub fn mark_unknown(
        &mut self,
        order_id: &str,
        error: impl Into<String>,
    ) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        if !matches!(
            record.state,
            OrderState::PartiallyFilled
                | OrderState::Filled
                | OrderState::Canceled
                | OrderState::Rejected
        ) {
            set_state(record, OrderState::Unknown);
            record.last_error = Some(error.into());
        }
        Some(record)
    }

    pub fn cancel(&mut self, order_id: &str) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(order_id)?;
        if matches!(record.state, OrderState::Filled | OrderState::Rejected) {
            return Some(record);
        }
        set_state(record, OrderState::Canceled);
        record.last_error = None;
        Some(record)
    }

    pub fn apply_fill(&mut self, fill: &FillRecord) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(&fill.order_id)?;
        let remaining_qty = (record.requested_qty - record.filled_qty).max(Decimal::ZERO);
        if fill.quantity > remaining_qty {
            return None;
        }
        apply_fill_to_record(record, fill.quantity);
        Some(record)
    }

    pub fn apply_price_improved_buy_fill(&mut self, fill: &FillRecord) -> Option<&OrderRecord> {
        let record = self.orders.get_mut(&fill.order_id)?;
        apply_fill_to_record(record, fill.quantity);
        Some(record)
    }

    pub fn active_orders(&self) -> usize {
        self.orders
            .values()
            .filter(|record| {
                matches!(
                    record.state,
                    OrderState::Pending
                        | OrderState::Unknown
                        | OrderState::Acknowledged
                        | OrderState::PartiallyFilled
                )
            })
            .count()
    }

    pub fn order(&self, order_id: &str) -> Option<&OrderRecord> {
        self.orders.get(order_id)
    }

    pub fn contains(&self, order_id: &str) -> bool {
        self.orders.contains_key(order_id)
    }

    pub fn orders(&self) -> impl Iterator<Item = &OrderRecord> {
        self.orders.values()
    }
}

fn apply_fill_to_record(record: &mut OrderRecord, quantity: Decimal) {
    record.filled_qty += quantity;
    let state = if record.filled_qty >= record.requested_qty {
        OrderState::Filled
    } else {
        OrderState::PartiallyFilled
    };
    set_state(record, state);
}

fn set_state(record: &mut OrderRecord, state: OrderState) {
    if record.state != state {
        record.state = state;
        record.state_changed_at = Some(Utc::now());
    }
}

#[cfg(test)]
mod tests {
    use super::{OrderLedger, OrderState};
    use crate::{IntentPurpose, TradeSide, TradingIntent};
    use chrono::Utc;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn intent(token_id: &str) -> TradingIntent {
        TradingIntent {
            intent_id: format!("intent-{token_id}"),
            deployment_id: "example.live".to_string(),
            market_id: "market-1".to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.45)),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn replace_preserves_logical_order_and_tracks_revision_history() {
        let mut ledger = OrderLedger::default();
        ledger.insert_from_intent(
            "order-1",
            &TradingIntent {
                intent_id: "intent-1".to_string(),
                deployment_id: "example.live".to_string(),
                market_id: "market-1".to_string(),
                token_id: "token-1".to_string(),
                side: TradeSide::Buy,
                quantity: dec!(2),
                limit_price: Some(dec!(0.45)),
                purpose: IntentPurpose::Entry,
                created_at: Utc::now(),
            },
        );
        ledger.acknowledge("order-1", "venue-1");

        let order = ledger
            .replace("order-1", dec!(3), Some(dec!(0.47)), "venue-2")
            .expect("replace");

        assert_eq!(order.order_id, "order-1");
        assert_eq!(order.venue_order_id.as_deref(), Some("venue-2"));
        assert_eq!(order.venue_order_history, vec!["venue-1".to_string()]);
        assert_eq!(order.revision, 1);
        assert_eq!(order.requested_qty, dec!(3));
        assert_eq!(order.limit_price, Some(dec!(0.47)));
        assert_eq!(order.state, OrderState::Acknowledged);
    }

    #[test]
    fn terminal_filled_order_is_not_overwritten_by_cancel_or_reject() {
        let mut ledger = OrderLedger::default();
        ledger.insert_from_intent("order-1", &intent("token-a"));
        ledger.acknowledge("order-1", "venue-1");
        ledger.apply_fill(&crate::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(0),
            timestamp: Utc::now(),
        });

        ledger.cancel("order-1");
        assert_eq!(ledger.order("order-1").unwrap().state, OrderState::Filled);

        ledger.reject("order-1", "late rejection");
        let order = ledger.order("order-1").unwrap();
        assert_eq!(order.state, OrderState::Filled);
        assert_eq!(order.last_error.as_deref(), Some("late rejection"));
    }

    #[test]
    fn late_acknowledgement_does_not_regress_filled_or_partially_filled_orders() {
        let mut ledger = OrderLedger::default();
        let mut intent = intent("token-a");
        intent.quantity = dec!(2);
        ledger.insert_from_intent("order-1", &intent);
        ledger.acknowledge("order-1", "venue-1");
        ledger.apply_fill(&crate::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(0),
            timestamp: Utc::now(),
        });

        ledger.acknowledge("order-1", "venue-1");
        assert_eq!(
            ledger.order("order-1").unwrap().state,
            OrderState::PartiallyFilled
        );

        ledger.apply_fill(&crate::FillRecord {
            fill_id: "fill-2".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(0),
            timestamp: Utc::now(),
        });
        ledger.acknowledge("order-1", "venue-1");
        assert_eq!(ledger.order("order-1").unwrap().state, OrderState::Filled);
    }

    #[test]
    fn late_ambiguous_or_rejected_outcome_does_not_erase_fill_or_cancel_state() {
        let mut ledger = OrderLedger::default();
        let mut intent = intent("token-a");
        intent.quantity = dec!(2);
        ledger.insert_from_intent("order-1", &intent);
        ledger.acknowledge("order-1", "venue-1");
        ledger.apply_fill(&crate::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            price: dec!(0.5),
            fee: dec!(0),
            timestamp: Utc::now(),
        });

        ledger.mark_unknown("order-1", "response lost");
        ledger.reject("order-1", "late rejection");
        assert_eq!(
            ledger.order("order-1").unwrap().state,
            OrderState::PartiallyFilled
        );

        ledger.cancel("order-1");
        ledger.reject("order-1", "later rejection");
        assert_eq!(ledger.order("order-1").unwrap().state, OrderState::Canceled);
    }

    #[test]
    fn overfill_is_rejected() {
        let mut ledger = OrderLedger::default();
        ledger.insert_from_intent("order-1", &intent("token-a"));
        ledger.acknowledge("order-1", "venue-1");

        let result = ledger.apply_fill(&crate::FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "token-a".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(2),
            price: dec!(0.5),
            fee: dec!(0),
            timestamp: Utc::now(),
        });

        assert!(result.is_none());
        let order = ledger.order("order-1").unwrap();
        assert_eq!(order.filled_qty, Decimal::ZERO);
        assert_eq!(order.state, OrderState::Acknowledged);
    }
}
