use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

pub const CHAINLINK_TWAP_WINDOW_SECS: u64 = 60;
const REFERENCE_PRICE_HISTORY_CAPACITY: usize = 2_048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferencePriceSource {
    Binance,
    // ponytail: Chainlink is the canonical 60s TWAP while only one window is consumed;
    // split the source identity when a market rule requires another window.
    Chainlink,
    Pyth,
}

impl ReferencePriceSource {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binance => "binance",
            Self::Chainlink => "chainlink",
            Self::Pyth => "pyth",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceAssetClass {
    Crypto,
    Equity,
    Etf,
    Forex,
    PreciousMetal,
    Commodity,
}

impl ReferenceAssetClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crypto => "crypto",
            Self::Equity => "equity",
            Self::Etf => "etf",
            Self::Forex => "forex",
            Self::PreciousMetal => "precious_metal",
            Self::Commodity => "commodity",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReferencePriceKey {
    pub source: ReferencePriceSource,
    pub symbol: String,
}

#[derive(Debug, Clone)]
pub struct ReferencePriceSnapshot {
    pub key: ReferencePriceKey,
    pub asset_class: ReferenceAssetClass,
    pub value: Decimal,
    pub full_accuracy_value: Option<String>,
    pub source_timestamp: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub is_carried_forward: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChainlinkTwapPrice {
    pub symbol: String,
    pub timestamp: i64,
    pub value: Decimal,
    pub full_accuracy_value: String,
    pub window_s: u64,
}

pub type ReferencePriceRegistry =
    Arc<RwLock<HashMap<ReferencePriceKey, VecDeque<ReferencePriceSnapshot>>>>;

#[must_use]
pub fn new_reference_price_registry() -> ReferencePriceRegistry {
    Arc::new(RwLock::new(HashMap::new()))
}

#[must_use]
pub fn parse_chainlink_twap_price(payload: &Value) -> Option<ChainlinkTwapPrice> {
    serde_json::from_value::<ChainlinkTwapPrice>(payload.clone())
        .ok()
        .filter(|price| price.window_s == CHAINLINK_TWAP_WINDOW_SECS)
}

#[must_use]
pub fn normalize_reference_symbol(symbol: &str) -> String {
    symbol.trim().to_lowercase()
}

#[must_use]
pub fn market_symbol_to_binance_symbol(symbol: &str) -> String {
    normalize_reference_symbol(symbol)
}

#[must_use]
pub fn market_symbol_to_chainlink_symbol(symbol: &str) -> String {
    let normalized = normalize_reference_symbol(symbol);
    let base = normalized.strip_suffix("usdt").unwrap_or(&normalized);
    format!("{base}/usd")
}

#[must_use]
pub fn pyth_symbol(symbol: &str) -> String {
    normalize_reference_symbol(symbol)
}

#[must_use]
pub fn infer_pyth_asset_class(symbol: &str) -> ReferenceAssetClass {
    match pyth_symbol(symbol).as_str() {
        "qqq" | "spy" | "ewy" | "vxx" => ReferenceAssetClass::Etf,
        "eurusd" | "gbpusd" | "usdcad" | "usdjpy" | "usdkrw" => ReferenceAssetClass::Forex,
        "xauusd" | "xagusd" => ReferenceAssetClass::PreciousMetal,
        "wti" | "cc" | "ngd" => ReferenceAssetClass::Commodity,
        _ => ReferenceAssetClass::Equity,
    }
}

pub async fn upsert_reference_price(
    registry: &ReferencePriceRegistry,
    snapshot: ReferencePriceSnapshot,
) {
    let mut guard = registry.write().await;
    let history = guard.entry(snapshot.key.clone()).or_default();
    if history
        .back()
        .is_some_and(|current| current.source_timestamp > snapshot.source_timestamp)
    {
        return;
    }
    history.push_back(snapshot);
    if history.len() > REFERENCE_PRICE_HISTORY_CAPACITY {
        history.pop_front();
    }
}

pub async fn latest_reference_price(
    registry: &ReferencePriceRegistry,
    source: ReferencePriceSource,
    symbol: &str,
) -> Option<ReferencePriceSnapshot> {
    let key = ReferencePriceKey {
        source,
        symbol: normalize_reference_symbol(symbol),
    };
    let guard = registry.read().await;
    guard.get(&key).and_then(|history| history.back()).cloned()
}

pub async fn reference_price_at(
    registry: &ReferencePriceRegistry,
    source: ReferencePriceSource,
    symbol: &str,
    timestamp: DateTime<Utc>,
) -> Option<ReferencePriceSnapshot> {
    let key = ReferencePriceKey {
        source,
        symbol: normalize_reference_symbol(symbol),
    };
    let guard = registry.read().await;
    guard.get(&key).and_then(|history| {
        history
            .iter()
            .rev()
            .find(|snapshot| snapshot.source_timestamp == timestamp)
            .cloned()
    })
}

#[cfg(test)]
mod tests {
    use super::{
        infer_pyth_asset_class, latest_reference_price, market_symbol_to_binance_symbol,
        market_symbol_to_chainlink_symbol, new_reference_price_registry,
        parse_chainlink_twap_price, pyth_symbol, reference_price_at, upsert_reference_price,
        ReferenceAssetClass, ReferencePriceKey, ReferencePriceSnapshot, ReferencePriceSource,
    };
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn normalizes_supported_symbol_families() {
        assert_eq!(market_symbol_to_binance_symbol("BTCUSDT"), "btcusdt");
        assert_eq!(market_symbol_to_chainlink_symbol("BTCUSDT"), "btc/usd");
        assert_eq!(pyth_symbol("AAPL"), "aapl");
    }

    #[test]
    fn infers_pyth_asset_class_from_symbol() {
        assert_eq!(infer_pyth_asset_class("AAPL"), ReferenceAssetClass::Equity);
        assert_eq!(infer_pyth_asset_class("SPY"), ReferenceAssetClass::Etf);
        assert_eq!(infer_pyth_asset_class("EURUSD"), ReferenceAssetClass::Forex);
        assert_eq!(
            infer_pyth_asset_class("XAUUSD"),
            ReferenceAssetClass::PreciousMetal
        );
        assert_eq!(
            infer_pyth_asset_class("WTI"),
            ReferenceAssetClass::Commodity
        );
    }

    #[test]
    fn parses_only_chainlink_sixty_second_twap_payloads() {
        let payload = json!({
            "symbol": "btc/usd",
            "timestamp": 1_786_868_400_000_i64,
            "value": 65000.5,
            "full_accuracy_value": "65000500000000000000000",
            "window_s": 60
        });
        let price = parse_chainlink_twap_price(&payload).expect("60-second TWAP should parse");
        assert_eq!(price.value, dec!(65000.5));
        assert_eq!(price.full_accuracy_value, "65000500000000000000000");

        let mut thirty_seconds = payload;
        thirty_seconds["window_s"] = json!(30);
        assert!(parse_chainlink_twap_price(&thirty_seconds).is_none());
    }

    #[tokio::test]
    async fn registry_returns_latest_snapshot_for_source_and_symbol() {
        let registry = new_reference_price_registry();
        let snapshot = ReferencePriceSnapshot {
            key: ReferencePriceKey {
                source: ReferencePriceSource::Chainlink,
                symbol: "btc/usd".to_string(),
            },
            asset_class: ReferenceAssetClass::Crypto,
            value: dec!(67234.50),
            full_accuracy_value: None,
            source_timestamp: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 0).unwrap(),
            received_at: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 1).unwrap(),
            is_carried_forward: false,
        };

        upsert_reference_price(&registry, snapshot.clone()).await;

        let found = latest_reference_price(&registry, ReferencePriceSource::Chainlink, "BTC/USD")
            .await
            .expect("snapshot should exist");
        assert_eq!(found.value, dec!(67234.50));
        assert_eq!(found.key.symbol, "btc/usd");
        assert_eq!(found.received_at, snapshot.received_at);

        let at_source_time = reference_price_at(
            &registry,
            ReferencePriceSource::Chainlink,
            "BTC/USD",
            snapshot.source_timestamp,
        )
        .await
        .expect("timestamped snapshot should exist");
        assert_eq!(at_source_time.value, dec!(67234.50));
    }
}
