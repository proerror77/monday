use chrono::{DateTime, Utc};
use polymarket_client_sdk::gamma::types::response::{Event, Market};
use rust_decimal::Decimal;
use serde_json::Value;

use crate::discovery::types::{MarketDescriptor, MarketFamily, MarketSemantics, SettlementSource};
use crate::reference_prices::{
    market_symbol_to_chainlink_symbol, reference_price_at, ReferencePriceRegistry,
    ReferencePriceSource,
};

#[derive(Debug, Clone)]
pub struct DiscoveredCryptoMarket {
    pub descriptor: MarketDescriptor,
    pub compatibility_event_id: String,
    pub symbol: String,
    pub up_token: String,
    pub down_token: String,
    pub end_time: DateTime<Utc>,
    /// Remaining seconds until expiry at discovery time.
    pub window_secs: u64,
    /// Total market duration in seconds (300 = 5-minute, 900 = 15-minute).
    pub market_window_secs: u64,
    pub price_to_beat: Option<Decimal>,
    pub raw_event: Option<Value>,
    pub raw_market: Value,
}

pub async fn discover_crypto_markets(
    markets: &[Market],
    configured_symbols: &[String],
    reference_prices: &ReferencePriceRegistry,
    now: DateTime<Utc>,
) -> Vec<DiscoveredCryptoMarket> {
    let mut discovered = Vec::new();

    for market in markets {
        if let Some(item) =
            normalize_crypto_market(market, configured_symbols, reference_prices, now).await
        {
            discovered.push(item);
        }
    }

    discovered
}

async fn normalize_crypto_market(
    market: &Market,
    configured_symbols: &[String],
    reference_prices: &ReferencePriceRegistry,
    now: DateTime<Utc>,
) -> Option<DiscoveredCryptoMarket> {
    let question = market.question.as_deref().unwrap_or("");
    let event = market.events.as_ref().and_then(|events| events.first());
    let market_start_time = crypto_market_start_time(market, event);
    if market_start_time.is_some_and(|start_time| start_time > now) {
        return None;
    }

    let strategy_symbol = infer_crypto_strategy_symbol(question)?;

    if !configured_symbols
        .iter()
        .any(|configured| configured.eq_ignore_ascii_case(strategy_symbol))
    {
        return None;
    }

    let token_ids = market.clob_token_ids.as_ref()?;
    if token_ids.len() != 2 {
        return None;
    }

    let end_time = market.end_date?;
    if end_time <= now {
        return None;
    }
    let window_secs = (end_time - now).num_seconds().max(0) as u64;
    let market_window_secs = infer_market_window_secs(market_start_time, end_time, question);
    if !matches!(market_window_secs, Some(300 | 900)) {
        return None;
    }
    let (up_token, down_token) = semantic_up_down_tokens(market, token_ids)?;
    let reference_symbol = market_symbol_to_chainlink_symbol(strategy_symbol);

    let price_to_beat = match market_start_time {
        Some(start_time) => reference_price_at(
            reference_prices,
            ReferencePriceSource::Chainlink,
            &reference_symbol,
            start_time,
        )
        .await
        .map(|snapshot| snapshot.value),
        None => None,
    }
    .or_else(|| usable_metadata_threshold(market.group_item_threshold.as_deref()))?;

    let descriptor = MarketDescriptor {
        market_family: MarketFamily::Crypto,
        event_id: event.map(|value| value.id.clone()),
        event_slug: event.and_then(|value| value.slug.clone()),
        market_id: market.id.clone(),
        market_slug: market.slug.clone(),
        title: market
            .question
            .clone()
            .or_else(|| event.and_then(|value| value.title.clone())),
        strategy_symbol: Some(strategy_symbol.to_string()),
        reference_symbol: Some(reference_symbol),
        settlement_source: SettlementSource::Chainlink,
        league: market
            .subcategory
            .clone()
            .or_else(|| market.category.clone()),
        sport: Some("crypto".to_string()),
        start_time: market_start_time,
        end_time: Some(end_time),
        token_ids: vec![up_token.clone(), down_token.clone()],
        market_semantics: MarketSemantics::UpDown,
        home_team: None,
        away_team: None,
        active: market.active,
        accepting_orders: market.accepting_orders,
    };

    Some(DiscoveredCryptoMarket {
        descriptor,
        compatibility_event_id: market.id.clone(),
        symbol: strategy_symbol.to_string(),
        up_token,
        down_token,
        end_time,
        window_secs,
        market_window_secs: market_window_secs.expect("validated allowed market window"),
        price_to_beat: Some(price_to_beat),
        raw_event: event.and_then(event_to_value),
        raw_market: serde_json::to_value(market).ok()?,
    })
}

fn semantic_up_down_tokens<T: ToString>(
    market: &Market,
    token_ids: &[T],
) -> Option<(String, String)> {
    if token_ids.len() != 2 {
        return None;
    }

    if let Some(outcomes) = market.outcomes.as_ref() {
        if outcomes.len() == token_ids.len() {
            let mut up_token = None;
            let mut down_token = None;
            for (outcome, token_id) in outcomes.iter().zip(token_ids.iter()) {
                let outcome = outcome.to_ascii_lowercase();
                if outcome.contains("up") || outcome.contains("yes") {
                    up_token = Some(token_id.to_string());
                } else if outcome.contains("down") || outcome.contains("no") {
                    down_token = Some(token_id.to_string());
                }
            }
            if let (Some(up), Some(down)) = (up_token, down_token) {
                return Some((up, down));
            }
        }
    }

    Some((token_ids[0].to_string(), token_ids[1].to_string()))
}

fn crypto_market_start_time(market: &Market, event: Option<&Event>) -> Option<DateTime<Utc>> {
    market
        .event_start_time
        .or_else(|| event.and_then(|value| value.start_time))
        .or_else(|| market.start_date)
        .or_else(|| event.and_then(|value| value.start_date))
}

fn infer_market_window_secs(
    start_time: Option<DateTime<Utc>>,
    end_time: DateTime<Utc>,
    question: &str,
) -> Option<u64> {
    if let Some(start_time) = start_time {
        let window_secs = (end_time - start_time).num_seconds();
        if matches!(window_secs, 300 | 900) {
            return Some(window_secs as u64);
        }
        if window_secs > 0 {
            return None;
        }
    }

    let normalized = question.to_ascii_lowercase();
    if normalized.contains("15 minute")
        || normalized.contains("15 minutes")
        || normalized.contains("fifteen minute")
        || normalized.contains("fifteen minutes")
    {
        Some(900)
    } else if normalized.contains("5 minute")
        || normalized.contains("5 minutes")
        || normalized.contains("five minute")
        || normalized.contains("five minutes")
    {
        Some(300)
    } else {
        None
    }
}

fn event_to_value(event: &Event) -> Option<Value> {
    serde_json::to_value(event).ok()
}

fn usable_metadata_threshold(raw: Option<&str>) -> Option<Decimal> {
    raw.and_then(|value| value.parse::<Decimal>().ok())
        .filter(|threshold| *threshold > Decimal::ONE)
}

fn infer_crypto_strategy_symbol(question: &str) -> Option<&'static str> {
    let upper = question.to_ascii_uppercase();
    if upper.contains("BITCOIN") || upper.contains("BTC") {
        Some("BTCUSDT")
    } else if upper.contains("ETHEREUM") || upper.contains("ETH") {
        Some("ETHUSDT")
    } else if upper.contains("SOLANA") || upper.contains("SOL ") {
        Some("SOLUSDT")
    } else if upper.contains("XRP") {
        Some("XRPUSDT")
    } else if upper.contains("DOGECOIN") || upper.contains("DOGE") {
        Some("DOGEUSDT")
    } else if upper.contains("HYPE") {
        Some("HYPEUSDT")
    } else if upper.contains("BNB") || upper.contains("BINANCE COIN") {
        Some("BNBUSDT")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use rust_decimal_macros::dec;
    use serde_json::json;

    use super::{
        crypto_market_start_time, discover_crypto_markets, infer_crypto_strategy_symbol,
        infer_market_window_secs,
    };
    use crate::reference_prices::{
        new_reference_price_registry, upsert_reference_price, ReferenceAssetClass,
        ReferencePriceKey, ReferencePriceSnapshot, ReferencePriceSource,
    };

    #[test]
    fn recognizes_all_seven_configured_crypto_assets() {
        for (question, symbol) in [
            ("Bitcoin Up or Down 5m", "BTCUSDT"),
            ("Ethereum Up or Down 5m", "ETHUSDT"),
            ("Solana Up or Down 5m", "SOLUSDT"),
            ("XRP Up or Down 5m", "XRPUSDT"),
            ("Dogecoin Up or Down 5m", "DOGEUSDT"),
            ("Hyperliquid Up or Down 5m", "HYPEUSDT"),
            ("BNB Up or Down 5m", "BNBUSDT"),
        ] {
            assert_eq!(infer_crypto_strategy_symbol(question), Some(symbol));
        }
    }

    #[tokio::test]
    async fn normalizes_crypto_updown_markets_into_catalog_descriptors() {
        let registry = new_reference_price_registry();
        upsert_reference_price(
            &registry,
            ReferencePriceSnapshot {
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
            },
        )
        .await;

        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-123",
                "question": "Will Bitcoin be up or down in 5 minutes?",
                "slug": "bitcoin-up-or-down-apr-6-0000",
                "endDate": "2026-04-06T00:05:00Z",
                "startDate": "2026-04-06T00:00:00Z",
                "groupItemThreshold": "0",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true,
                "events": [{
                    "id": "event-456",
                    "slug": "bitcoin-up-or-down-apr-6-0000",
                    "title": "Bitcoin Up Or Down",
                    "startDate": "2026-04-06T00:00:00Z"
                }]
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 30).unwrap(),
        )
        .await;

        assert_eq!(discovered.len(), 1);
        let item = &discovered[0];
        assert_eq!(item.descriptor.market_id, "market-123");
        assert_eq!(item.descriptor.event_id.as_deref(), Some("event-456"));
        assert_eq!(item.descriptor.strategy_symbol.as_deref(), Some("BTCUSDT"));
        assert_eq!(item.descriptor.reference_symbol.as_deref(), Some("btc/usd"));
        assert_eq!(item.price_to_beat, Some(dec!(67234.50)));
        assert_eq!(item.compatibility_event_id, "market-123");
        assert_eq!(item.up_token, "111");
        assert_eq!(item.down_token, "222");
    }

    #[tokio::test]
    async fn keeps_fifteen_minute_crypto_markets() {
        let registry = new_reference_price_registry();
        upsert_reference_price(
            &registry,
            ReferencePriceSnapshot {
                key: ReferencePriceKey {
                    source: ReferencePriceSource::Chainlink,
                    symbol: "btc/usd".to_string(),
                },
                asset_class: ReferenceAssetClass::Crypto,
                value: dec!(67234.50),
                full_accuracy_value: Some("67234500000000000000000".to_string()),
                source_timestamp: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 0).unwrap(),
                received_at: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 1).unwrap(),
                is_carried_forward: false,
            },
        )
        .await;

        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-999",
                "question": "Will Bitcoin be up or down in 15 minutes?",
                "slug": "bitcoin-up-or-down-apr-6-0000",
                "endDate": "2026-04-06T00:15:00Z",
                "startDate": "2026-04-06T00:00:00Z",
                "groupItemThreshold": "0",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true,
                "events": [{
                    "id": "event-999",
                    "slug": "bitcoin-up-or-down-apr-6-0000",
                    "title": "Bitcoin Up Or Down",
                    "startDate": "2026-04-06T00:00:00Z"
                }]
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 30).unwrap(),
        )
        .await;

        assert_eq!(discovered.len(), 1, "15-minute markets should be kept");
    }

    #[tokio::test]
    async fn skips_crypto_market_without_exact_opening_reference() {
        let registry = new_reference_price_registry();
        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-without-opening-reference",
                "question": "Will Bitcoin be up or down in 5 minutes?",
                "endDate": "2026-04-06T00:05:00Z",
                "startDate": "2026-04-06T00:00:00Z",
                "groupItemThreshold": "0",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 30).unwrap(),
        )
        .await;

        assert!(discovered.is_empty());
    }

    #[tokio::test]
    async fn ignores_crypto_markets_that_have_not_started() {
        let registry = new_reference_price_registry();
        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-future",
                "question": "Will Bitcoin be up or down in 15 minutes?",
                "endDate": "2026-04-06T00:30:00Z",
                "eventStartTime": "2026-04-06T00:15:00Z",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 10, 0).unwrap(),
        )
        .await;

        assert!(discovered.is_empty());
    }

    #[tokio::test]
    async fn maps_crypto_tokens_by_outcome_semantics_not_array_order() {
        let registry = new_reference_price_registry();
        upsert_reference_price(
            &registry,
            ReferencePriceSnapshot {
                key: ReferencePriceKey {
                    source: ReferencePriceSource::Chainlink,
                    symbol: "btc/usd".to_string(),
                },
                asset_class: ReferenceAssetClass::Crypto,
                value: dec!(67234.50),
                full_accuracy_value: Some("67234500000000000000000".to_string()),
                source_timestamp: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 0).unwrap(),
                received_at: Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 1).unwrap(),
                is_carried_forward: false,
            },
        )
        .await;

        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-reversed",
                "question": "Will Bitcoin be up or down in 5 minutes?",
                "slug": "bitcoin-up-or-down-apr-6-0000",
                "endDate": "2026-04-06T00:05:00Z",
                "startDate": "2026-04-06T00:00:00Z",
                "groupItemThreshold": "0",
                "outcomes": "[\"Down\",\"Up\"]",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true,
                "events": [{
                    "id": "event-reversed",
                    "slug": "bitcoin-up-or-down-apr-6-0000",
                    "title": "Bitcoin Up Or Down",
                    "startDate": "2026-04-06T00:00:00Z"
                }]
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 30).unwrap(),
        )
        .await;

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].up_token, "222");
        assert_eq!(discovered[0].down_token, "111");
    }

    #[tokio::test]
    async fn ignores_one_hour_crypto_markets() {
        let registry = new_reference_price_registry();

        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-1000",
                "question": "Will Bitcoin be up or down in 1 hour?",
                "slug": "bitcoin-up-or-down-apr-6-0000",
                "endDate": "2026-04-06T01:00:00Z",
                "startDate": "2026-04-06T00:00:00Z",
                "groupItemThreshold": "0",
                "clobTokenIds": "[\"111\",\"222\"]",
                "active": true,
                "acceptingOrders": true,
                "events": [{
                    "id": "event-1000",
                    "slug": "bitcoin-up-or-down-apr-6-0000",
                    "title": "Bitcoin Up Or Down",
                    "startDate": "2026-04-06T00:00:00Z"
                }]
            }))
            .unwrap();

        let discovered = discover_crypto_markets(
            &[market],
            &["BTCUSDT".to_string()],
            &registry,
            Utc.with_ymd_and_hms(2026, 4, 6, 0, 0, 30).unwrap(),
        )
        .await;

        assert!(discovered.is_empty(), "1-hour markets should be ignored");
    }

    #[test]
    fn prefers_event_start_time_over_series_start_date() {
        let market: polymarket_client_sdk::gamma::types::response::Market =
            serde_json::from_value(json!({
                "id": "market-actual",
                "question": "Bitcoin Up or Down - April 10, 3:45AM-3:50AM ET",
                "startDate": "2026-04-09T07:53:03.027282Z",
                "endDate": "2026-04-10T07:50:00Z",
                "eventStartTime": "2026-04-10T07:45:00Z",
                "clobTokenIds": "[\"111\",\"222\"]",
                "events": [{
                    "id": "event-actual",
                    "startDate": "2026-04-09T07:56:35.383348Z",
                    "startTime": "2026-04-10T07:45:00Z"
                }]
            }))
            .unwrap();

        let event = market.events.as_ref().and_then(|events| events.first());
        let start_time = crypto_market_start_time(&market, event);
        assert_eq!(
            start_time,
            Some(Utc.with_ymd_and_hms(2026, 4, 10, 7, 45, 0).unwrap())
        );
        assert_eq!(
            infer_market_window_secs(
                start_time,
                market.end_date.unwrap(),
                market.question.as_deref().unwrap()
            ),
            Some(300)
        );
    }
}
