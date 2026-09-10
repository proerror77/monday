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

/// Read-only strategy projection rebuilt from canonical Portfolio AccountView.
#[derive(Debug, Clone, Default)]
pub struct PositionLedger {
    positions: BTreeMap<String, PositionSnapshot>,
}

impl PositionLedger {
    pub fn restore(positions: Vec<PositionSnapshot>) -> Self {
        Self {
            positions: positions
                .into_iter()
                .map(|position| (position.token_id.clone(), position))
                .collect(),
        }
    }

    pub fn net_qty(&self, token_id: &str) -> Decimal {
        self.positions
            .get(token_id)
            .map(|position| position.net_qty)
            .unwrap_or(Decimal::ZERO)
    }

    pub fn can_reduce(
        &self,
        token_id: &str,
        side: super::intents::TradeSide,
        quantity: Decimal,
    ) -> bool {
        let current = self.net_qty(token_id);
        !current.is_zero() && current.signum() != side.sign() && quantity <= current.abs()
    }

    pub fn positions(&self) -> impl Iterator<Item = &PositionSnapshot> {
        self.positions
            .values()
            .filter(|position| !position.net_qty.is_zero())
    }
}
