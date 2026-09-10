use super::intents::TradingIntent;
use super::orders::OrderLedger;
use super::positions::PositionLedger;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RiskSnapshot {
    pub pending_intents: usize,
    pub active_orders: usize,
    pub open_positions: usize,
    pub gross_exposure: Decimal,
    pub reserved_order_exposure: Decimal,
    pub total_gross_exposure: Decimal,
}

pub fn snapshot_from_state(
    intents: &[TradingIntent],
    orders: &OrderLedger,
    positions: &PositionLedger,
) -> RiskSnapshot {
    let gross_exposure = positions
        .positions()
        .map(|position| position.net_qty.abs() * position.avg_entry_price)
        .sum();

    let reserved_order_exposure = intents
        .iter()
        .filter(|intent| {
            matches!(
                intent.purpose,
                super::intents::IntentPurpose::Entry | super::intents::IntentPurpose::Hedge
            )
        })
        .filter_map(|intent| {
            orders.orders().find(|order| {
                order.intent_id == intent.intent_id
                    && matches!(
                        order.state,
                        super::orders::OrderState::Pending
                            | super::orders::OrderState::Unknown
                            | super::orders::OrderState::Acknowledged
                            | super::orders::OrderState::PartiallyFilled
                    )
            })
        })
        .map(|order| {
            let remaining_qty = (order.requested_qty - order.filled_qty).max(Decimal::ZERO);
            remaining_qty * order.limit_price.unwrap_or(Decimal::ONE)
        })
        .sum();

    RiskSnapshot {
        pending_intents: intents.len(),
        active_orders: orders.active_orders(),
        open_positions: positions.positions().count(),
        gross_exposure,
        reserved_order_exposure,
        total_gross_exposure: gross_exposure + reserved_order_exposure,
    }
}
