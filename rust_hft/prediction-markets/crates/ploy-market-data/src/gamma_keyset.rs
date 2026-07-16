use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::gamma::types::response::Market;
use serde::{de::Error as _, Deserialize, Deserializer};
use serde_json::Value;

const MARKETS_KEYSET_ENDPOINT: &str = "https://gamma-api.polymarket.com/markets/keyset";
const MAX_PAGE_SIZE: i32 = 100;

#[derive(Debug)]
struct MarketPage {
    markets: Vec<Market>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct RawMarketPage {
    markets: Vec<Value>,
    next_cursor: Option<String>,
}

impl<'de> Deserialize<'de> for MarketPage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // The workspace enables serde_json's `arbitrary_precision` support. A
        // direct nested decode sends Gamma's numeric Decimal fields through the
        // private arbitrary-precision map representation, which rust_decimal
        // rejects. Materializing each market as Value first restores the normal
        // serde_json::from_value path already used successfully elsewhere.
        let raw = RawMarketPage::deserialize(deserializer)?;
        let markets = raw
            .markets
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<Market>, _>>()
            .map_err(D::Error::custom)?;

        Ok(Self {
            markets,
            next_cursor: raw.next_cursor,
        })
    }
}

pub(crate) fn markets_keyset_url() -> &'static str {
    MARKETS_KEYSET_ENDPOINT
}

pub(crate) async fn fetch_markets(
    request: &MarketsRequest,
    max_items: usize,
) -> Result<Vec<Market>, reqwest::Error> {
    let http = reqwest::Client::new();
    let mut request = request.clone();
    request.offset = None;
    request.limit = Some(MAX_PAGE_SIZE.min(max_items.max(1) as i32));
    let mut cursor: Option<String> = None;
    let mut markets = Vec::with_capacity(max_items);

    while markets.len() < max_items {
        let mut call = http.get(markets_keyset_url()).query(&request);
        if let Some(cursor) = cursor.as_deref() {
            call = call.query(&[("after_cursor", cursor)]);
        }
        let page: MarketPage = call.send().await?.error_for_status()?.json().await?;
        let page_len = page.markets.len();
        markets.extend(page.markets.into_iter().take(max_items - markets.len()));
        cursor = page.next_cursor.filter(|next| !next.is_empty());
        if page_len < request.limit.unwrap_or(MAX_PAGE_SIZE) as usize || cursor.is_none() {
            break;
        }
    }

    Ok(markets)
}

#[cfg(test)]
mod tests {
    use super::{markets_keyset_url, MarketPage, MAX_PAGE_SIZE};

    #[test]
    fn gamma_market_keyset_contract_uses_current_endpoint_and_limit() {
        assert_eq!(
            markets_keyset_url(),
            "https://gamma-api.polymarket.com/markets/keyset"
        );
        assert_eq!(MAX_PAGE_SIZE, 100);
    }

    #[test]
    fn gamma_keyset_page_decodes_live_numeric_and_json_string_fields() {
        let page: MarketPage = serde_json::from_str(
            r#"{
                "markets": [{
                    "id": "540817",
                    "outcomes": "[\"Yes\", \"No\"]",
                    "outcomePrices": "[\"0.505\", \"0.495\"]",
                    "clobTokenIds": "[\"11\", \"22\"]",
                    "orderPriceMinTickSize": 0.01,
                    "orderMinSize": 5,
                    "volumeNum": 855297.0246820152,
                    "liquidityNum": 19954.5302
                }],
                "next_cursor": "cursor-1"
            }"#,
        )
        .expect("decode current Gamma keyset response shape");

        assert_eq!(page.markets[0].id, "540817");
        assert_eq!(
            page.markets[0]
                .outcomes
                .as_ref()
                .expect("outcomes")
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["Yes", "No"]
        );
        assert_eq!(page.next_cursor.as_deref(), Some("cursor-1"));
    }
}
