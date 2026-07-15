use crate::intents::TradingIntent;
use crate::orders::OrderLedger;
use crate::positions::PositionLedger;
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
                crate::intents::IntentPurpose::Entry | crate::intents::IntentPurpose::Hedge
            )
        })
        .filter_map(|intent| {
            orders.orders().find(|order| {
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

#[cfg(test)]
mod tests {
    use super::snapshot_from_state;
    use crate::fills::FillRecord;
    use crate::intents::{IntentPurpose, TradeSide, TradingIntent};
    use crate::orders::OrderLedger;
    use crate::positions::PositionLedger;
    use chrono::Utc;
    use rust_decimal_macros::dec;

    #[test]
    fn risk_snapshot_reflects_active_orders_and_exposure() {
        let intent = TradingIntent {
            intent_id: "intent-1".to_string(),
            deployment_id: "openclaw.default".to_string(),
            market_id: "market-1".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(5),
            limit_price: Some(dec!(0.40)),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        };

        let mut orders = OrderLedger::default();
        orders.insert_from_intent("order-1", &intent);
        orders.acknowledge("order-1", "venue-1");

        let fill = FillRecord {
            fill_id: "fill-1".to_string(),
            order_id: "order-1".to_string(),
            token_id: "yes-token".to_string(),
            side: TradeSide::Buy,
            quantity: dec!(3),
            price: dec!(0.40),
            fee: dec!(0.02),
            timestamp: Utc::now(),
        };
        orders.apply_fill(&fill);

        let mut positions = PositionLedger::default();
        positions.apply_fill(&fill);

        let snapshot = snapshot_from_state(&[intent], &orders, &positions);
        assert_eq!(snapshot.pending_intents, 1);
        assert_eq!(snapshot.active_orders, 1);
        assert_eq!(snapshot.open_positions, 1);
        assert_eq!(snapshot.gross_exposure.round_dp(2), dec!(1.20));
        assert_eq!(snapshot.reserved_order_exposure.round_dp(2), dec!(0.80));
        assert_eq!(snapshot.total_gross_exposure.round_dp(2), dec!(2.00));
    }
}
