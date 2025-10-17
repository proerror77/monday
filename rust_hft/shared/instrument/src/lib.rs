use hft_core::{Symbol, VenueId};
use serde::de::{self, Deserializer};
use serde::ser::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

/// 唯一的交易品種識別子，例如 "BTCUSDT@binance"。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstrumentId(pub String);

impl InstrumentId {
    pub fn new<S: Into<String>>(s: S) -> Self {
        Self(s.into())
    }

    /// 解析 InstrumentId，回傳 `(Symbol, VenueId)`，格式需為 `SYMBOL@VENUE`。
    pub fn split(&self) -> Option<(Symbol, VenueId)> {
        let mut parts = self.0.split('@');
        let symbol = parts.next()?;
        let venue = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        VenueId::from_str(venue).map(|venue_id| (Symbol::new(symbol), venue_id))
    }

    /// 取得純 Symbol（忽略 venue）。
    pub fn symbol(&self) -> Option<Symbol> {
        self.split().map(|(symbol, _)| symbol)
    }

    /// 取得 VenueId。
    pub fn venue_id(&self) -> Option<VenueId> {
        self.split().map(|(_, venue)| venue)
    }
}

/// 交易品種的細節資訊：價格精度、單位、合約屬性等。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentMeta {
    pub symbol: Symbol,
    #[serde(
        deserialize_with = "venue_id_deserialize",
        serialize_with = "venue_id_serialize"
    )]
    pub venue: VenueId,
    pub base: String,
    pub quote: String,
    pub tick_size: f64,
    pub lot_size: f64,
    #[serde(default)]
    pub contract_multiplier: Option<f64>,
    #[serde(default)]
    pub leverage: Option<f64>,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// 交易所能力描述：支援的功能、限制等。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VenueCapabilities {
    #[serde(default)]
    pub supports_incremental_book: bool,
    #[serde(default)]
    pub supports_private_ws: bool,
    #[serde(default)]
    pub allows_post_only: bool,
    #[serde(default)]
    pub max_subscriptions: Option<u32>,
}

/// 交易所的連線資訊與能力。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VenueMeta {
    #[serde(
        deserialize_with = "venue_id_deserialize",
        serialize_with = "venue_id_serialize"
    )]
    pub venue_id: VenueId,
    pub name: String,
    pub rest_endpoint: Option<String>,
    pub ws_public_endpoint: Option<String>,
    pub ws_private_endpoint: Option<String>,
    #[serde(default)]
    pub capabilities: VenueCapabilities,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

/// YAML catalog 結構。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct InstrumentCatalogConfig {
    #[serde(default)]
    pub venues: Vec<VenueMeta>,
    #[serde(default)]
    pub instruments: Vec<InstrumentMeta>,
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("unknown instrument: {0}")]
    UnknownInstrument(String),
    #[error("unknown venue: {0}")]
    UnknownVenue(String),
}

/// 記憶體中的 instrument / venue 查詢介面。
pub struct InstrumentCatalog {
    venues: HashMap<VenueId, VenueMeta>,
    instruments: HashMap<InstrumentId, InstrumentMeta>,
}

impl InstrumentCatalog {
    pub fn new(config: InstrumentCatalogConfig) -> Self {
        let venues = config
            .venues
            .into_iter()
            .map(|meta| (meta.venue_id, meta))
            .collect();
        let instruments = config
            .instruments
            .into_iter()
            .map(|meta| {
                let id =
                    InstrumentId::new(format!("{}@{}", meta.symbol.as_str(), meta.venue.as_str()));
                (id, meta)
            })
            .collect();

        Self {
            venues,
            instruments,
        }
    }

    pub fn venue(&self, venue: &VenueId) -> Result<&VenueMeta, CatalogError> {
        self.venues
            .get(venue)
            .ok_or_else(|| CatalogError::UnknownVenue(venue.as_str().to_string()))
    }

    pub fn instrument(&self, id: &InstrumentId) -> Result<&InstrumentMeta, CatalogError> {
        self.instruments
            .get(id)
            .ok_or_else(|| CatalogError::UnknownInstrument(id.0.clone()))
    }

    pub fn instrument_for(
        &self,
        symbol: &Symbol,
        venue: &VenueId,
    ) -> Result<&InstrumentMeta, CatalogError> {
        let id = InstrumentId::new(format!("{}@{}", symbol.as_str(), venue.as_str()));
        self.instrument(&id)
    }

    /// 回傳 catalog 是否包含指定商品（忽略 venue）。
    pub fn has_symbol(&self, symbol: &Symbol) -> bool {
        self.instruments.values().any(|meta| &meta.symbol == symbol)
    }

    /// 檢查特定 venue 下是否存在該商品。
    pub fn has_instrument(&self, symbol: &Symbol, venue: &VenueId) -> bool {
        self.instruments
            .values()
            .any(|meta| &meta.symbol == symbol && &meta.venue == venue)
    }

    /// 取得指定 venue 的所有 InstrumentId。
    pub fn instrument_ids_for_venue(&self, venue: &VenueId) -> Vec<InstrumentId> {
        self.instruments
            .iter()
            .filter_map(|(id, meta)| {
                if &meta.venue == venue {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

fn venue_id_deserialize<'de, D>(deserializer: D) -> Result<VenueId, D::Error>
where
    D: Deserializer<'de>,
{
    struct VenueIdVisitor;

    impl<'de> serde::de::Visitor<'de> for VenueIdVisitor {
        type Value = VenueId;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("string (e.g. BINANCE) 或數值 venue id")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(VenueId(value as u16))
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            VenueId::from_str(value).ok_or_else(|| E::custom(format!("未知的 venue_id: {}", value)))
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }
    }

    deserializer.deserialize_any(VenueIdVisitor)
}

fn venue_id_serialize<S>(venue: &VenueId, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(venue.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_lookup() {
        let config = InstrumentCatalogConfig {
            venues: vec![VenueMeta {
                venue_id: VenueId::BINANCE,
                name: "Binance".into(),
                rest_endpoint: Some("https://api.binance.com".into()),
                ws_public_endpoint: None,
                ws_private_endpoint: None,
                capabilities: VenueCapabilities {
                    supports_incremental_book: true,
                    ..Default::default()
                },
                metadata: HashMap::new(),
            }],
            instruments: vec![InstrumentMeta {
                symbol: Symbol::new("BTCUSDT"),
                venue: VenueId::BINANCE,
                base: "BTC".into(),
                quote: "USDT".into(),
                tick_size: 0.1,
                lot_size: 0.001,
                contract_multiplier: None,
                leverage: Some(50.0),
                metadata: HashMap::new(),
            }],
        };

        let catalog = InstrumentCatalog::new(config);
        let meta = catalog
            .instrument(&InstrumentId::new(format!(
                "BTCUSDT@{}",
                VenueId::BINANCE.as_str()
            )))
            .expect("instrument should exist");
        assert_eq!(meta.base, "BTC");
        assert_eq!(meta.tick_size, 0.1);
        assert!(catalog.has_symbol(&Symbol::new("BTCUSDT")));

        let instrument_id = InstrumentId::new(format!("BTCUSDT@{}", VenueId::BINANCE.as_str()));
        let (symbol, venue) = instrument_id.split().expect("split instrument id");
        assert_eq!(symbol.as_str(), "BTCUSDT");
        assert_eq!(venue, VenueId::BINANCE);
        assert_eq!(instrument_id.symbol().unwrap().0, "BTCUSDT");
        assert_eq!(instrument_id.venue_id().unwrap(), VenueId::BINANCE);

        assert!(catalog.has_instrument(&Symbol::new("BTCUSDT"), &VenueId::BINANCE));
        let ids = catalog.instrument_ids_for_venue(&VenueId::BINANCE);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].0, instrument_id.0);
    }
}
