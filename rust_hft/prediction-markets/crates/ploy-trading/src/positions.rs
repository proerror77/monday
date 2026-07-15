use crate::fills::FillRecord;
use crate::intents::TradeSide;
use crate::pnl::PnlSnapshot;
use rust_decimal::prelude::Signed;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PositionSnapshot {
    pub token_id: String,
    pub net_qty: Decimal,
    pub avg_entry_price: Decimal,
    pub realized_pnl: Decimal,
}

#[derive(Debug, Default)]
pub struct PositionLedger {
    positions: BTreeMap<String, PositionSnapshot>,
    total_fees: Decimal,
}

impl PositionLedger {
    pub fn restore(positions: Vec<PositionSnapshot>, total_fees: Decimal) -> Self {
        Self {
            positions: positions
                .into_iter()
                .map(|position| (position.token_id.clone(), position))
                .collect(),
            total_fees,
        }
    }

    pub fn apply_fill(&mut self, fill: &FillRecord) {
        let signed_qty = fill.quantity * fill.side.sign();
        let position = self
            .positions
            .entry(fill.token_id.clone())
            .or_insert_with(|| PositionSnapshot {
                token_id: fill.token_id.clone(),
                ..PositionSnapshot::default()
            });

        let current_qty = position.net_qty;
        let next_qty = current_qty + signed_qty;

        if current_qty.is_zero() || current_qty.signum() == signed_qty.signum() {
            let total_cost =
                (position.avg_entry_price * current_qty.abs()) + (fill.price * fill.quantity);
            let total_qty = current_qty.abs() + fill.quantity;
            position.net_qty = next_qty;
            position.avg_entry_price = if total_qty.is_zero() {
                Decimal::ZERO
            } else {
                total_cost / total_qty
            };
        } else {
            let closing_qty = current_qty.abs().min(fill.quantity);
            let pnl_per_unit = if current_qty.is_sign_positive() {
                fill.price - position.avg_entry_price
            } else {
                position.avg_entry_price - fill.price
            };
            position.realized_pnl += pnl_per_unit * closing_qty;
            position.net_qty = next_qty;

            if position.net_qty.is_zero() {
                position.avg_entry_price = Decimal::ZERO;
            } else if current_qty.signum() != position.net_qty.signum() {
                position.avg_entry_price = fill.price;
            }
        }

        self.total_fees += fill.fee;
    }

    pub fn net_qty(&self, token_id: &str) -> Decimal {
        self.positions
            .get(token_id)
            .map(|position| position.net_qty)
            .unwrap_or(Decimal::ZERO)
    }

    pub fn can_reduce(&self, token_id: &str, side: TradeSide, quantity: Decimal) -> bool {
        let current = self.net_qty(token_id);
        !current.is_zero() && current.signum() != side.sign() && quantity <= current.abs()
    }

    pub fn positions(&self) -> impl Iterator<Item = &PositionSnapshot> {
        self.positions
            .values()
            .filter(|position| !position.net_qty.is_zero())
    }

    pub fn pnl_snapshot(&self, mark_prices: &BTreeMap<String, Decimal>) -> PnlSnapshot {
        let unrealized_pnl = self
            .positions
            .values()
            .filter_map(|position| {
                let mark = mark_prices.get(&position.token_id)?;
                let pnl_per_unit = if position.net_qty.is_sign_positive() {
                    *mark - position.avg_entry_price
                } else {
                    position.avg_entry_price - *mark
                };
                Some(pnl_per_unit * position.net_qty.abs())
            })
            .sum();

        PnlSnapshot {
            realized_pnl: self
                .positions
                .values()
                .map(|position| position.realized_pnl)
                .sum(),
            unrealized_pnl,
            total_fees: self.total_fees,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PositionLedger;
    use crate::fills::FillRecord;
    use crate::intents::TradeSide;
    use chrono::Utc;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use std::collections::BTreeMap;

    fn sample_fill(
        fill_id: &str,
        side: TradeSide,
        quantity: Decimal,
        price: Decimal,
    ) -> FillRecord {
        FillRecord {
            fill_id: fill_id.to_string(),
            order_id: format!("order-{fill_id}"),
            token_id: "yes-token".to_string(),
            side,
            quantity,
            price,
            fee: dec!(0.10),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn fill_updates_position_quantity() {
        let mut ledger = PositionLedger::default();
        ledger.apply_fill(&sample_fill("1", TradeSide::Buy, dec!(5), dec!(0.42)));
        assert_eq!(ledger.net_qty("yes-token"), dec!(5));
    }

    #[test]
    fn closing_fill_realizes_pnl() {
        let mut ledger = PositionLedger::default();
        ledger.apply_fill(&sample_fill("1", TradeSide::Buy, dec!(5), dec!(0.42)));
        ledger.apply_fill(&sample_fill("2", TradeSide::Sell, dec!(2), dec!(0.60)));

        let position = ledger.positions().next().expect("position");
        assert_eq!(position.net_qty, dec!(3));
        assert_eq!(position.realized_pnl.round_dp(2), dec!(0.36));
    }

    #[test]
    fn pnl_snapshot_includes_unrealized_and_fees() {
        let mut ledger = PositionLedger::default();
        ledger.apply_fill(&sample_fill("1", TradeSide::Buy, dec!(5), dec!(0.42)));
        let mut marks = BTreeMap::new();
        marks.insert("yes-token".to_string(), dec!(0.50));

        let pnl = ledger.pnl_snapshot(&marks);
        assert_eq!(pnl.realized_pnl, Decimal::ZERO);
        assert_eq!(pnl.unrealized_pnl.round_dp(2), dec!(0.40));
        assert_eq!(pnl.total_fees.round_dp(2), dec!(0.10));
    }
}
