use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;

use hft_core::{HftError, HftResult, Price, Quantity, Symbol, VenueId};
use polymarket_client_sdk::clob::ws::{BookUpdate as SdkBookUpdate, WsMessage};
use polymarket_client_sdk::types::{B256, U256};
use ports::{BookLevel, MarketEvent, MarketSnapshot, TopOfBook};
use rust_decimal::Decimal;
use serde_json::Value;

pub const POLYMARKET_VENUE_ID: VenueId = VenueId(11);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenMetadata {
    pub token_id: U256,
    pub condition_id: B256,
    pub symbol: Symbol,
    pub outcome: String,
}

impl TokenMetadata {
    pub fn new(
        token_id: U256,
        condition_id: B256,
        symbol: impl Into<String>,
        outcome: impl Into<String>,
    ) -> HftResult<Self> {
        let symbol = symbol.into();
        let outcome = outcome.into();
        if token_id == U256::ZERO || condition_id == B256::ZERO {
            return Err(HftError::Config(
                "Polymarket token and condition ids must be non-zero".to_string(),
            ));
        }
        if symbol.trim().is_empty() || outcome.trim().is_empty() {
            return Err(HftError::Config(
                "Polymarket symbol and outcome must be non-empty".to_string(),
            ));
        }
        Ok(Self {
            token_id,
            condition_id,
            symbol: Symbol::new(symbol),
            outcome,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct TokenCatalog {
    by_token: HashMap<U256, TokenMetadata>,
    token_by_symbol: HashMap<String, U256>,
}

impl TokenCatalog {
    pub fn try_new(tokens: impl IntoIterator<Item = TokenMetadata>) -> HftResult<Self> {
        let mut catalog = Self::default();
        for token in tokens {
            if catalog.by_token.contains_key(&token.token_id) {
                return Err(HftError::Config(format!(
                    "duplicate Polymarket token id {}",
                    token.token_id
                )));
            }
            if catalog
                .token_by_symbol
                .contains_key(token.symbol.as_str())
            {
                return Err(HftError::Config(format!(
                    "duplicate Polymarket symbol {}",
                    token.symbol.as_str()
                )));
            }
            catalog
                .token_by_symbol
                .insert(token.symbol.as_str().to_string(), token.token_id);
            catalog.by_token.insert(token.token_id, token);
        }
        if catalog.by_token.is_empty() {
            return Err(HftError::Config(
                "Polymarket token catalog cannot be empty".to_string(),
            ));
        }
        Ok(catalog)
    }

    pub fn by_symbol(&self, symbol: &str) -> Option<&TokenMetadata> {
        self.token_by_symbol
            .get(symbol)
            .and_then(|token_id| self.by_token.get(token_id))
    }

    pub fn by_token_id(&self, token_id: U256) -> Option<&TokenMetadata> {
        self.by_token.get(&token_id)
    }

    pub fn resolve_symbols(&self, symbols: &[Symbol]) -> HftResult<Vec<U256>> {
        symbols
            .iter()
            .map(|symbol| {
                self.by_symbol(symbol.as_str())
                    .map(|token| token.token_id)
                    .ok_or_else(|| {
                        HftError::Config(format!(
                            "Polymarket symbol {} is absent from the token catalog",
                            symbol.as_str()
                        ))
                    })
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
struct LocalBook {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    sequence: u64,
    last_timestamp_ms: Option<i64>,
    snapshot_ready: bool,
}

#[derive(Debug, Clone)]
pub struct PolymarketEventConverter {
    catalog: TokenCatalog,
    books: HashMap<U256, LocalBook>,
}

impl PolymarketEventConverter {
    pub fn new(catalog: TokenCatalog) -> Self {
        Self {
            catalog,
            books: HashMap::new(),
        }
    }

    pub fn convert_text(&mut self, text: &str) -> HftResult<Vec<MarketEvent>> {
        let text = text.trim();
        if text.is_empty() || matches!(text, "PING" | "PONG") {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(text)
            .map_err(|error| HftError::Parse(format!("Polymarket websocket JSON: {error}")))?;
        let values = match value {
            Value::Array(values) => values,
            value => vec![value],
        };
        let mut events = Vec::new();
        for value in values {
            if value.get("event_type").is_none() {
                continue;
            }
            let message: WsMessage = serde_json::from_value(value).map_err(|error| {
                HftError::Parse(format!("Polymarket websocket message: {error}"))
            })?;
            if let WsMessage::Book(book) = message {
                events.extend(self.apply_snapshot(book)?);
            }
        }
        Ok(events)
    }

    fn apply_snapshot(&mut self, update: SdkBookUpdate) -> HftResult<Vec<MarketEvent>> {
        let token = self.token(update.asset_id)?.clone();
        if token.condition_id != update.market {
            return Err(HftError::Parse(format!(
                "Polymarket token {} arrived for unexpected condition {}",
                update.asset_id, update.market
            )));
        }
        let timestamp = timestamp_micros(update.timestamp)?;
        let mut book = LocalBook::default();
        for level in update.bids {
            insert_snapshot_level(&mut book.bids, level.price, level.size, "bid")?;
        }
        for level in update.asks {
            insert_snapshot_level(&mut book.asks, level.price, level.size, "ask")?;
        }
        book.sequence = self
            .books
            .get(&update.asset_id)
            .map_or(1, |previous| previous.sequence.saturating_add(1));
        book.last_timestamp_ms = Some(update.timestamp);
        book.snapshot_ready = true;

        let bids = levels_descending(&book.bids)?;
        let asks = levels_ascending(&book.asks)?;
        let snapshot = MarketSnapshot {
            symbol: token.symbol.clone(),
            timestamp,
            bids,
            asks,
            sequence: book.sequence,
            source_venue: Some(POLYMARKET_VENUE_ID),
        };
        let quote = quote_from_snapshot(&snapshot);
        self.books.insert(update.asset_id, book);

        let mut events = vec![MarketEvent::Snapshot(snapshot)];
        if let Some(quote) = quote {
            events.push(MarketEvent::Quote(quote));
        }
        Ok(events)
    }

    fn token(&self, token_id: U256) -> HftResult<&TokenMetadata> {
        self.catalog.by_token_id(token_id).ok_or_else(|| {
            HftError::Config(format!(
                "Polymarket token {token_id} is absent from the token catalog"
            ))
        })
    }
}

fn timestamp_micros(timestamp_ms: i64) -> HftResult<u64> {
    let timestamp_ms = u64::try_from(timestamp_ms)
        .map_err(|_| HftError::Parse("Polymarket timestamp predates Unix epoch".to_string()))?;
    Ok(timestamp_ms.saturating_mul(1_000))
}

fn insert_snapshot_level(
    side: &mut BTreeMap<Decimal, Decimal>,
    price: Decimal,
    quantity: Decimal,
    name: &str,
) -> HftResult<()> {
    if price <= Decimal::ZERO || price >= Decimal::ONE || quantity <= Decimal::ZERO {
        return Err(HftError::Parse(format!(
            "Polymarket {name} level has invalid price {price} or quantity {quantity}"
        )));
    }
    side.insert(price, quantity);
    Ok(())
}

fn level(price: Decimal, quantity: Decimal) -> HftResult<BookLevel> {
    Ok(BookLevel {
        price: Price::from_str(&price.to_string())
            .map_err(|error| HftError::Parse(format!("Polymarket price {price}: {error}")))?,
        quantity: Quantity::from_str(&quantity.to_string()).map_err(|error| {
            HftError::Parse(format!("Polymarket quantity {quantity}: {error}"))
        })?,
    })
}

fn levels_descending(levels: &BTreeMap<Decimal, Decimal>) -> HftResult<Vec<BookLevel>> {
    levels
        .iter()
        .rev()
        .map(|(price, quantity)| level(*price, *quantity))
        .collect()
}

fn levels_ascending(levels: &BTreeMap<Decimal, Decimal>) -> HftResult<Vec<BookLevel>> {
    levels
        .iter()
        .map(|(price, quantity)| level(*price, *quantity))
        .collect()
}

fn quote_from_snapshot(snapshot: &MarketSnapshot) -> Option<TopOfBook> {
    Some(TopOfBook {
        symbol: snapshot.symbol.clone(),
        timestamp: snapshot.timestamp,
        sequence: snapshot.sequence,
        bid: snapshot.bids.first()?.clone(),
        ask: snapshot.asks.first()?.clone(),
        source_venue: Some(POLYMARKET_VENUE_ID),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use polymarket_client_sdk::types::{b256, U256};
    use std::str::FromStr;

    #[test]
    fn token_catalog_resolves_symbols_and_rejects_duplicates() {
        let token = TokenMetadata::new(
            U256::from_str("65818619657568813474341868652308942079804919287380422192892211131408793125422").unwrap(),
            b256!("bd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af"),
            "btc-up",
            "Up",
        )
        .unwrap();
        let catalog = TokenCatalog::try_new([token.clone()]).unwrap();

        assert_eq!(catalog.by_symbol("btc-up").unwrap(), &token);
        assert_eq!(catalog.by_token_id(token.token_id).unwrap(), &token);
        assert!(TokenCatalog::try_new([token.clone(), token]).is_err());
    }

    #[test]
    fn book_snapshot_publishes_sorted_l2_and_bbo() {
        let token = TokenMetadata::new(
            U256::from_str("65818619657568813474341868652308942079804919287380422192892211131408793125422").unwrap(),
            b256!("bd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af"),
            "btc-up",
            "Up",
        )
        .unwrap();
        let mut converter =
            PolymarketEventConverter::new(TokenCatalog::try_new([token]).unwrap());

        let events = converter
            .convert_text(
                r#"{
                    "event_type":"book",
                    "asset_id":"65818619657568813474341868652308942079804919287380422192892211131408793125422",
                    "market":"0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af",
                    "bids":[{"price":".48","size":"30"},{"price":".50","size":"15"},{"price":".49","size":"20"}],
                    "asks":[{"price":".54","size":"10"},{"price":".52","size":"25"},{"price":".53","size":"60"}],
                    "timestamp":"123456789000",
                    "hash":"0x1234"
                }"#,
            )
            .unwrap();

        let MarketEvent::Snapshot(snapshot) = &events[0] else {
            panic!("expected snapshot");
        };
        assert_eq!(snapshot.symbol.as_str(), "btc-up");
        assert_eq!(snapshot.timestamp, 123_456_789_000_000);
        assert_eq!(snapshot.sequence, 1);
        assert_eq!(snapshot.bids[0].price.to_string(), "0.50");
        assert_eq!(snapshot.bids[0].quantity.to_string(), "15");
        assert_eq!(snapshot.asks[0].price.to_string(), "0.52");
        assert_eq!(snapshot.asks[0].quantity.to_string(), "25");
        assert_eq!(snapshot.source_venue, Some(POLYMARKET_VENUE_ID));

        let MarketEvent::Quote(quote) = &events[1] else {
            panic!("expected BBO");
        };
        assert_eq!(quote.bid, snapshot.bids[0]);
        assert_eq!(quote.ask, snapshot.asks[0]);
        assert_eq!(quote.sequence, snapshot.sequence);
    }

    #[test]
    fn price_change_applies_delta_and_preserves_zero_delete() {
        let token = TokenMetadata::new(
            U256::from_str("65818619657568813474341868652308942079804919287380422192892211131408793125422").unwrap(),
            b256!("bd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af"),
            "btc-up",
            "Up",
        )
        .unwrap();
        let mut converter =
            PolymarketEventConverter::new(TokenCatalog::try_new([token]).unwrap());
        converter
            .convert_text(
                r#"{"event_type":"book","asset_id":"65818619657568813474341868652308942079804919287380422192892211131408793125422","market":"0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af","bids":[{"price":".49","size":"20"},{"price":".50","size":"15"}],"asks":[{"price":".52","size":"25"}],"timestamp":"1000"}"#,
            )
            .unwrap();

        let events = converter
            .convert_text(
                r#"{"event_type":"price_change","market":"0xbd31dc8a20211944f6b70f31557f1001557b59905b7738480ca09bd4532f84af","timestamp":"1001","price_changes":[{"asset_id":"65818619657568813474341868652308942079804919287380422192892211131408793125422","price":".50","size":"0","side":"BUY","best_bid":".49","best_ask":".51"},{"asset_id":"65818619657568813474341868652308942079804919287380422192892211131408793125422","price":".49","size":"25","side":"BUY","best_bid":".49","best_ask":".51"},{"asset_id":"65818619657568813474341868652308942079804919287380422192892211131408793125422","price":".51","size":"8","side":"SELL","best_bid":".49","best_ask":".51"}]}"#,
            )
            .unwrap();

        let MarketEvent::Update(update) = &events[0] else {
            panic!("expected delta");
        };
        assert_eq!(update.sequence, 2);
        assert_eq!(update.first_sequence, None);
        assert_eq!(update.bids[0].price.to_string(), "0.50");
        assert_eq!(update.bids[0].quantity.to_string(), "0");
        assert_eq!(update.bids[1].price.to_string(), "0.49");
        assert_eq!(update.bids[1].quantity.to_string(), "25");
        assert_eq!(update.asks[0].price.to_string(), "0.51");
        assert_eq!(update.asks[0].quantity.to_string(), "8");

        let MarketEvent::Quote(quote) = &events[1] else {
            panic!("expected updated BBO");
        };
        assert_eq!(quote.bid.price.to_string(), "0.49");
        assert_eq!(quote.bid.quantity.to_string(), "25");
        assert_eq!(quote.ask.price.to_string(), "0.51");
        assert_eq!(quote.ask.quantity.to_string(), "8");
    }
}
