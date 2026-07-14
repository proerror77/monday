use polymarket_client_sdk::gamma::types::request::MarketsRequest;
use polymarket_client_sdk::gamma::types::response::Market;
use serde::Deserialize;

const MARKETS_KEYSET_ENDPOINT: &str = "https://gamma-api.polymarket.com/markets/keyset";
const MAX_PAGE_SIZE: i32 = 100;

#[derive(Debug, Deserialize)]
struct MarketPage {
    markets: Vec<Market>,
    next_cursor: Option<String>,
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
    use super::{markets_keyset_url, MAX_PAGE_SIZE};

    #[test]
    fn gamma_market_keyset_contract_uses_current_endpoint_and_limit() {
        assert_eq!(
            markets_keyset_url(),
            "https://gamma-api.polymarket.com/markets/keyset"
        );
        assert_eq!(MAX_PAGE_SIZE, 100);
    }
}
