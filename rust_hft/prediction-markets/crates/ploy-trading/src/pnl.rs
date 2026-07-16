use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PnlSnapshot {
    pub realized_pnl: Decimal,
    pub unrealized_pnl: Decimal,
    pub total_fees: Decimal,
}

impl PnlSnapshot {
    pub fn net_pnl(&self) -> Decimal {
        self.realized_pnl + self.unrealized_pnl - self.total_fees
    }
}
