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

impl EventWindow {
    #[must_use]
    pub fn contains_token(&self, token_id: &Arc<str>) -> bool {
        self.up_token == *token_id || self.down_token == *token_id
    }

    #[must_use]
    pub fn token_wins(&self, token_id: &Arc<str>, up_won: bool) -> Option<bool> {
        if *token_id == self.up_token {
            Some(up_won)
        } else if *token_id == self.down_token {
            Some(!up_won)
        } else {
            None
        }
    }
}
