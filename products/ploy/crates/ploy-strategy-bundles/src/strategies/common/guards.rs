use ploy_trading::{OrderLedger, OrderState};

#[must_use]
pub fn active_order_exists(token_id: &str, orders: &OrderLedger) -> bool {
    orders.orders().any(|order| {
        order.token_id == token_id
            && matches!(
                order.state,
                OrderState::Pending | OrderState::Acknowledged | OrderState::PartiallyFilled
            )
    })
}

#[cfg(test)]
mod tests {
    use super::active_order_exists;
    use chrono::Utc;
    use ploy_trading::{IntentPurpose, OrderLedger, TradeSide, TradingIntent};
    use rust_decimal_macros::dec;

    fn intent(token_id: &str) -> TradingIntent {
        TradingIntent {
            intent_id: format!("intent-{token_id}"),
            deployment_id: "test.deployment".to_string(),
            market_id: "event-1".to_string(),
            token_id: token_id.to_string(),
            side: TradeSide::Buy,
            quantity: dec!(1),
            limit_price: Some(dec!(0.5)),
            purpose: IntentPurpose::Entry,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn finds_active_orders() {
        let mut orders = OrderLedger::default();
        orders.insert_from_intent("order-1", &intent("token-a"));
        assert!(active_order_exists("token-a", &orders));

        orders.cancel("order-1");
        assert!(!active_order_exists("token-a", &orders));
    }
}
