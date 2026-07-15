use crate::intents::TradeSide;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillRecord {
    pub fill_id: String,
    pub order_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub price: Decimal,
    pub fee: Decimal,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct FillLedger {
    fills: Vec<FillRecord>,
    fill_ids: HashSet<String>,
}

impl FillLedger {
    pub fn restore(fills: Vec<FillRecord>) -> Self {
        let fill_ids = fills.iter().map(|fill| fill.fill_id.clone()).collect();
        Self { fills, fill_ids }
    }

    pub fn contains(&self, fill_id: &str) -> bool {
        self.fill_ids.contains(fill_id)
    }

    pub fn record(&mut self, fill: FillRecord) -> Option<&FillRecord> {
        if self.contains(&fill.fill_id) {
            return None;
        }
        self.fill_ids.insert(fill.fill_id.clone());
        self.fills.push(fill);
        self.fills.last()
    }

    pub fn all(&self) -> &[FillRecord] {
        &self.fills
    }

    pub fn total_fees(&self) -> Decimal {
        self.fills.iter().map(|fill| fill.fee).sum()
    }
}
