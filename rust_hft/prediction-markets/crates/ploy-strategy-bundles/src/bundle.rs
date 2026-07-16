use crate::signals::SignalConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrategyBundle {
    pub bundle_id: String,
    pub signals: Vec<SignalConfig>,
}
