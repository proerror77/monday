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
        // Canonical Portfolio applies fees to realized PnL and exposes the
        // fee total separately for disclosure. Subtracting it again here
        // would charge the same venue fee twice.
        self.realized_pnl + self.unrealized_pnl
    }
}
