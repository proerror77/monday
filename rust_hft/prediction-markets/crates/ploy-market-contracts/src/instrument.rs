use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentKind {
    UpDown,
    YesNo,
    Moneyline,
    Spread,
    Total,
}

impl Default for InstrumentKind {
    fn default() -> Self {
        Self::UpDown
    }
}
