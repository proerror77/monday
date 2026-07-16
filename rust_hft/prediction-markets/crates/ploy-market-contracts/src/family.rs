use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionFamily {
    CryptoExpiry,
    SportsPregame,
    SportsLive,
    Politics,
    Custom(u16),
}

impl Default for PredictionFamily {
    fn default() -> Self {
        Self::CryptoExpiry
    }
}
