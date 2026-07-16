use serde::{Deserialize, Serialize};

/// Time-remaining regime for a binary option market.
///
/// This is market-domain data shared by research and runtime consumers. It is
/// deliberately independent of operator, OMS, risk, and execution contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Regime {
    /// 181..=300 seconds remaining.
    Early,
    /// 61..=180 seconds remaining.
    Middle,
    /// 6..=60 seconds remaining.
    Late,
    /// 0..=5 seconds remaining.
    Expiry,
}

impl Regime {
    pub fn from_secs(t: i64) -> Self {
        match t {
            181..=300 => Self::Early,
            61..=180 => Self::Middle,
            6..=60 => Self::Late,
            _ => Self::Expiry,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Early => "early",
            Self::Middle => "middle",
            Self::Late => "late",
            Self::Expiry => "expiry",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Regime;

    #[test]
    fn maps_time_remaining_to_binary_market_regime() {
        assert_eq!(Regime::from_secs(300), Regime::Early);
        assert_eq!(Regime::from_secs(180), Regime::Middle);
        assert_eq!(Regime::from_secs(60), Regime::Late);
        assert_eq!(Regime::from_secs(5), Regime::Expiry);
    }
}
