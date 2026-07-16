use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

#[derive(Clone, Copy)]
pub struct QuoteState {
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
    #[allow(dead_code)]
    pub ts: DateTime<Utc>,
}
