use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketFamily {
    Crypto,
    Sports,
}

impl MarketFamily {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crypto => "crypto",
            Self::Sports => "sports",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketSemantics {
    UpDown,
    YesNo,
    Moneyline,
    Spread,
    Total,
    Unknown,
}

impl MarketSemantics {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpDown => "updown",
            Self::YesNo => "yesno",
            Self::Moneyline => "moneyline",
            Self::Spread => "spread",
            Self::Total => "total",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn from_sports_market_type(value: Option<&str>) -> Self {
        match value.map(str::to_ascii_lowercase).as_deref() {
            Some("moneyline") => Self::Moneyline,
            Some(value) if value.contains("spread") => Self::Spread,
            Some(value) if value.contains("total") => Self::Total,
            Some("yesno") | Some("yes_no") => Self::YesNo,
            Some(_) => Self::Unknown,
            None => Self::YesNo,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementSource {
    Chainlink,
    OfficialPolymarket,
    Unknown,
}

impl SettlementSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chainlink => "chainlink",
            Self::OfficialPolymarket => "official_polymarket",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MarketDescriptor {
    pub market_family: MarketFamily,
    pub event_id: Option<String>,
    pub event_slug: Option<String>,
    pub market_id: String,
    pub market_slug: Option<String>,
    pub title: Option<String>,
    pub strategy_symbol: Option<String>,
    pub reference_symbol: Option<String>,
    pub settlement_source: SettlementSource,
    pub league: Option<String>,
    pub sport: Option<String>,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub token_ids: Vec<String>,
    pub market_semantics: MarketSemantics,
    pub home_team: Option<String>,
    pub away_team: Option<String>,
    pub active: Option<bool>,
    pub accepting_orders: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::{MarketSemantics, SettlementSource};

    #[test]
    fn sports_market_type_maps_to_semantics() {
        assert_eq!(
            MarketSemantics::from_sports_market_type(Some("moneyline")),
            MarketSemantics::Moneyline
        );
        assert_eq!(
            MarketSemantics::from_sports_market_type(Some("spreads")),
            MarketSemantics::Spread
        );
        assert_eq!(
            MarketSemantics::from_sports_market_type(Some("totals")),
            MarketSemantics::Total
        );
        assert_eq!(
            MarketSemantics::from_sports_market_type(None),
            MarketSemantics::YesNo
        );
        assert_eq!(SettlementSource::Chainlink.as_str(), "chainlink");
    }
}
