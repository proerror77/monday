use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentPurpose {
    Entry,
    Exit,
    Reduce,
    Hedge,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeSide {
    Buy,
    Sell,
}

impl TradeSide {
    pub fn sign(self) -> Decimal {
        match self {
            Self::Buy => Decimal::ONE,
            Self::Sell => -Decimal::ONE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingIntent {
    pub intent_id: String,
    pub deployment_id: String,
    pub market_id: String,
    pub token_id: String,
    pub side: TradeSide,
    pub quantity: Decimal,
    pub limit_price: Option<Decimal>,
    pub purpose: IntentPurpose,
    pub created_at: DateTime<Utc>,
}

impl TradingIntent {
    pub fn signed_quantity(&self) -> Decimal {
        self.quantity * self.side.sign()
    }
}
