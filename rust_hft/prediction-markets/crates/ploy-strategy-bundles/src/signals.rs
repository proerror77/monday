use ploy_trading::{IntentPurpose, TradeSide, TradingIntent};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalConfig {
    ThresholdEntry {
        market_id: String,
        token_id: String,
        threshold_bps: i64,
        quantity: Decimal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketSignal {
    pub market_id: String,
    pub token_id: String,
    pub strength_bps: i64,
}

impl SignalConfig {
    pub fn evaluate(
        &self,
        deployment_id: &str,
        signal: &MarketSignal,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<TradingIntent> {
        match self {
            Self::ThresholdEntry {
                market_id,
                token_id,
                threshold_bps,
                quantity,
            } if signal.market_id == *market_id
                && signal.token_id == *token_id
                && signal.strength_bps >= *threshold_bps =>
            {
                Some(TradingIntent {
                    intent_id: format!(
                        "{deployment_id}:{market_id}:{token_id}:{}",
                        signal.strength_bps
                    ),
                    deployment_id: deployment_id.to_string(),
                    market_id: market_id.clone(),
                    token_id: token_id.clone(),
                    side: TradeSide::Buy,
                    quantity: *quantity,
                    limit_price: None,
                    purpose: IntentPurpose::Entry,
                    created_at: now,
                })
            }
            _ => None,
        }
    }
}
