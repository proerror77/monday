use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

#[derive(Clone)]
pub struct EventWindow {
    pub event_id: Arc<str>,
    pub symbol: Arc<str>,
    pub up_token: Arc<str>,
    pub down_token: Arc<str>,
    pub end_time: DateTime<Utc>,
    #[allow(dead_code)]
    pub window_secs: u64,
    #[allow(dead_code)]
    pub price_to_beat: Option<Decimal>,
}
