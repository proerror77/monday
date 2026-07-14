use std::sync::Arc;

use chrono::{DateTime, Utc};

#[derive(Clone)]
pub struct BasicHoldingState {
    #[allow(dead_code)]
    pub token_id: Arc<str>,
    #[allow(dead_code)]
    pub direction: String,
    #[allow(dead_code)]
    pub entry_time: DateTime<Utc>,
}
