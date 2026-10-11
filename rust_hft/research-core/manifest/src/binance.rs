//! Canonical Binance market identity shared by raw acquisition and immutable inputs.
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Market {
    Spot,
    Usdm,
}

impl FromStr for Market {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "spot" => Ok(Self::Spot),
            "usdm" => Ok(Self::Usdm),
            other => Err(format!("unsupported MARKET={other}")),
        }
    }
}

impl Market {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spot => "spot",
            Self::Usdm => "usdm",
        }
    }
}
