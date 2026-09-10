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

/// Read-only strategy projection rebuilt from the canonical OMS checkpoint.
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

#[derive(Debug, Clone, Default)]
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
