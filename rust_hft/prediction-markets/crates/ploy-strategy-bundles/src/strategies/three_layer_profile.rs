//! Runtime profile selector for the PM5D three-layer strategy.

use serde::{de, Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreeLayerProfile {
    /// Historical runtime behavior: blended CEX continuation and book confirmation.
    Mixed,
    /// Snapshot Strategy A: contrarian alpha plus executable liquidity/risk gates.
    Champion,
    /// Snapshot Strategy B: Champion plus CEX/PM order-book imbalance soft score.
    ObiSoft,
    /// Snapshot Strategy B-hard: OBI confirmation must pass before scoring.
    ObiHard,
    /// Snapshot Strategy C: Champion plus CEX continuation soft score.
    ContinuationSoft,
    /// Candidate Strategy D: external CEX repricing pressure adjusted by PM spread.
    RepricingMomentum,
    /// PRD settlement-probability lane: probability edge first, no repricing alpha gate.
    SettlementProbability,
}

impl Default for ThreeLayerProfile {
    fn default() -> Self {
        Self::Mixed
    }
}

impl ThreeLayerProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Champion => "champion",
            Self::ObiSoft => "obi_soft",
            Self::ObiHard => "obi_hard",
            Self::ContinuationSoft => "continuation_soft",
            Self::RepricingMomentum => "repricing_momentum",
            Self::SettlementProbability => "settlement_probability",
        }
    }

    pub fn uses_snapshot_scoring(self) -> bool {
        !matches!(self, Self::Mixed)
    }
}

impl FromStr for ThreeLayerProfile {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "mixed" | "legacy" => Ok(Self::Mixed),
            "champion" | "a" | "alpha" | "alpha_only" | "contrarian_alpha" => Ok(Self::Champion),
            "obi" | "obi_soft" | "b" | "book_imbalance" | "orderbook" => Ok(Self::ObiSoft),
            "obi_hard" | "obi_confirmed" | "book_imbalance_hard" | "orderbook_hard" => {
                Ok(Self::ObiHard)
            }
            "continuation" | "continuation_soft" | "c" | "cex_continuation" => {
                Ok(Self::ContinuationSoft)
            }
            "repricing"
            | "repricing_momentum"
            | "reprice_momentum"
            | "spread_adjusted_external_move" => Ok(Self::RepricingMomentum),
            "settlement" | "settlement_probability" | "settlement_prob" | "probability_edge" => {
                Ok(Self::SettlementProbability)
            }
            other => Err(format!(
                "unknown three_layer_strategy_profile {other:?}; expected mixed, champion, obi_soft, obi_hard, continuation_soft, repricing_momentum, or settlement_probability"
            )),
        }
    }
}

impl fmt::Display for ThreeLayerProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ThreeLayerProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::from_str(&raw).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::ThreeLayerProfile;
    use std::str::FromStr;

    #[test]
    fn parses_obi_hard_operator_aliases() {
        assert_eq!(
            ThreeLayerProfile::from_str("obi_hard").unwrap(),
            ThreeLayerProfile::ObiHard
        );
        assert_eq!(
            ThreeLayerProfile::from_str("book_imbalance_hard").unwrap(),
            ThreeLayerProfile::ObiHard
        );
        assert_eq!(ThreeLayerProfile::ObiHard.as_str(), "obi_hard");
    }

    #[test]
    fn parses_repricing_momentum_aliases() {
        assert_eq!(
            ThreeLayerProfile::from_str("repricing_momentum").unwrap(),
            ThreeLayerProfile::RepricingMomentum
        );
        assert_eq!(
            ThreeLayerProfile::from_str("spread_adjusted_external_move").unwrap(),
            ThreeLayerProfile::RepricingMomentum
        );
        assert_eq!(
            ThreeLayerProfile::RepricingMomentum.as_str(),
            "repricing_momentum"
        );
    }

    #[test]
    fn parses_settlement_probability_aliases() {
        assert_eq!(
            ThreeLayerProfile::from_str("settlement_probability").unwrap(),
            ThreeLayerProfile::SettlementProbability
        );
        assert_eq!(
            ThreeLayerProfile::from_str("probability_edge").unwrap(),
            ThreeLayerProfile::SettlementProbability
        );
        assert_eq!(
            ThreeLayerProfile::SettlementProbability.as_str(),
            "settlement_probability"
        );
    }
}
