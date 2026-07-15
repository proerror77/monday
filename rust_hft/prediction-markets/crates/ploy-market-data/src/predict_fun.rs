//! Predict.fun REST market-discovery and order-book collector.
//!
//! Predict's API is currently beta. Mainnet requires an API key; the official
//! testnet permits keyless access. This module intentionally contains no order
//! submission or wallet operations.

use std::str::FromStr;
use std::time::Duration;

use reqwest::Client;
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{info, warn};

pub const MAINNET_API: &str = "https://api.predict.fun";
pub const TESTNET_API: &str = "https://api-testnet.predict.fun";

#[derive(Debug, Error)]
pub enum PredictFunError {
    #[error("Predict.fun mainnet requires PREDICT_FUN_API_KEY")]
    MissingMainnetApiKey,
    #[error("unsupported Predict.fun API origin: {0}")]
    UnsupportedApiOrigin(String),
    #[error("Predict.fun API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Predict.fun API returned success=false for {0}")]
    Api(String),
    #[error("Predict.fun persistence failed: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone)]
pub struct PredictFunConfig {
    pub base_url: String,
    pub api_key: Option<SecretString>,
    pub refresh_interval_secs: u64,
    pub per_market_delay_ms: u64,
    pub once: bool,
}

impl PredictFunConfig {
    pub fn from_env(once: bool) -> Result<Self, PredictFunError> {
        let base_url =
            std::env::var("PREDICT_FUN_API_URL").unwrap_or_else(|_| MAINNET_API.to_string());
        let api_key = std::env::var("PREDICT_FUN_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(SecretString::from);
        validate_api_access(
            &base_url,
            api_key
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
        )?;
        Ok(Self {
            base_url,
            api_key,
            refresh_interval_secs: env_positive_u64("PLOY_PREDICT_FUN_REFRESH_SECS", 30),
            // Predict documents a 240 requests/minute default limit. Keep book
            // polling below that ceiling and leave room for catalog pages.
            per_market_delay_ms: env_positive_u64("PLOY_PREDICT_FUN_MARKET_DELAY_MS", 300),
            once,
        })
    }
}

pub fn validate_api_access(base_url: &str, api_key: Option<&str>) -> Result<(), PredictFunError> {
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|_| PredictFunError::UnsupportedApiOrigin(base_url.to_string()))?;
    let is_origin_only = parsed.scheme() == "https"
        && parsed.port_or_known_default() == Some(443)
        && matches!(parsed.path(), "" | "/")
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !is_origin_only {
        return Err(PredictFunError::UnsupportedApiOrigin(base_url.to_string()));
    }
    match parsed.host_str() {
        Some("api.predict.fun") if api_key.is_none_or(|key| key.trim().is_empty()) => {
            Err(PredictFunError::MissingMainnetApiKey)
        }
        Some("api.predict.fun" | "api-testnet.predict.fun") => Ok(()),
        _ => Err(PredictFunError::UnsupportedApiOrigin(base_url.to_string())),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictMarket {
    pub id: i64,
    pub title: String,
    pub question: String,
    #[serde(default)]
    pub description: Option<String>,
    pub condition_id: String,
    pub decimal_precision: u32,
    pub trading_status: String,
    pub status: String,
    pub is_visible: bool,
    pub is_neg_risk: bool,
    #[serde(default)]
    pub is_yield_bearing: bool,
    pub fee_rate_bps: i32,
    #[serde(default)]
    pub outcomes: Vec<PredictOutcome>,
    #[serde(default)]
    pub resolution: Option<serde_json::Value>,
}

impl PredictMarket {
    fn is_collectible(&self) -> bool {
        self.is_visible && self.trading_status.eq_ignore_ascii_case("OPEN")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictOutcome {
    pub name: String,
    pub index_set: i64,
    pub on_chain_id: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketsResponse {
    pub success: bool,
    #[serde(default)]
    pub cursor: Option<String>,
    pub data: Vec<PredictMarket>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderBookResponse {
    pub success: bool,
    pub data: OrderBook,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderBook {
    pub market_id: i64,
    pub update_timestamp_ms: i64,
    #[serde(default)]
    pub asks: Vec<[serde_json::Value; 2]>,
    #[serde(default)]
    pub bids: Vec<[serde_json::Value; 2]>,
    #[serde(default)]
    pub last_order_settled: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComplementedBook {
    pub yes_bid: Option<Decimal>,
    pub yes_bid_size: Option<Decimal>,
    pub yes_ask: Option<Decimal>,
    pub yes_ask_size: Option<Decimal>,
    pub no_bid: Option<Decimal>,
    pub no_bid_size: Option<Decimal>,
    pub no_ask: Option<Decimal>,
    pub no_ask_size: Option<Decimal>,
}

pub fn complement_book(book: &OrderBook, precision: u32) -> ComplementedBook {
    let yes_bid = book.bids.first().and_then(|level| decimal(&level[0]));
    let yes_bid_size = book.bids.first().and_then(|level| decimal(&level[1]));
    let yes_ask = book.asks.first().and_then(|level| decimal(&level[0]));
    let yes_ask_size = book.asks.first().and_then(|level| decimal(&level[1]));

    ComplementedBook {
        yes_bid,
        yes_bid_size,
        yes_ask,
        yes_ask_size,
        no_bid: yes_ask.map(|price| (Decimal::ONE - price).round_dp(precision)),
        no_bid_size: yes_ask_size,
        no_ask: yes_bid.map(|price| (Decimal::ONE - price).round_dp(precision)),
        no_ask_size: yes_bid_size,
    }
}

fn decimal(value: &serde_json::Value) -> Option<Decimal> {
    match value {
        serde_json::Value::Number(number) => Decimal::from_str(number.as_str()).ok(),
        serde_json::Value::String(number) => Decimal::from_str(number).ok(),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct PredictFunClient {
    client: Client,
    base_url: String,
    api_key: Option<SecretString>,
}

impl PredictFunClient {
    pub fn new(base_url: String, api_key: Option<SecretString>) -> Result<Self, PredictFunError> {
        validate_api_access(
            &base_url,
            api_key
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
        )?;
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.get(format!("{}{path}", self.base_url));
        match &self.api_key {
            Some(key) => request.header("x-api-key", key.expose_secret()),
            None => request,
        }
    }

    pub async fn markets(&self) -> Result<Vec<PredictMarket>, PredictFunError> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut request = self.get("/v1/markets").query(&[("first", "100")]);
            if let Some(after) = cursor.as_deref() {
                request = request.query(&[("after", after)]);
            }
            let response: MarketsResponse =
                request.send().await?.error_for_status()?.json().await?;
            if !response.success {
                return Err(PredictFunError::Api("GET /v1/markets".to_string()));
            }
            all.extend(response.data);
            match response.cursor.filter(|next| Some(next) != cursor.as_ref()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(all)
    }

    pub async fn orderbook(&self, market_id: i64) -> Result<OrderBook, PredictFunError> {
        let response: OrderBookResponse = self
            .get(&format!("/v1/markets/{market_id}/orderbook"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if !response.success {
            return Err(PredictFunError::Api(format!(
                "GET /v1/markets/{market_id}/orderbook"
            )));
        }
        Ok(response.data)
    }
}

pub async fn run_collector(config: PredictFunConfig, pool: PgPool) -> Result<(), PredictFunError> {
    let client = PredictFunClient::new(config.base_url.clone(), config.api_key.clone())?;
    loop {
        if let Err(error) = collect_once(&client, &config, &pool).await {
            if config.once {
                return Err(error);
            }
            warn!(%error, "Predict.fun collection pass failed");
        }
        if config.once {
            return Ok(());
        }
        sleep(Duration::from_secs(config.refresh_interval_secs)).await;
    }
}

async fn collect_once(
    client: &PredictFunClient,
    config: &PredictFunConfig,
    pool: &PgPool,
) -> Result<(), PredictFunError> {
    let markets = client.markets().await?;
    let mut books = 0usize;
    let mut attempted_books = 0usize;
    for market in &markets {
        persist_market(pool, market).await?;
        if !market.is_collectible() {
            continue;
        }
        attempted_books += 1;
        match client.orderbook(market.id).await {
            Ok(book) => {
                persist_book(pool, market, &book).await?;
                books += 1;
            }
            Err(error) => {
                warn!(market_id = market.id, %error, "Predict.fun orderbook fetch failed")
            }
        }
        sleep(Duration::from_millis(config.per_market_delay_ms)).await;
    }
    if attempted_books > 0 && books == 0 {
        return Err(PredictFunError::Api(
            "all collectible market orderbook requests failed".to_string(),
        ));
    }
    info!(
        markets = markets.len(),
        books, "Predict.fun collection pass complete"
    );
    Ok(())
}

async fn persist_market(pool: &PgPool, market: &PredictMarket) -> Result<(), sqlx::Error> {
    let outcomes = serde_json::to_value(&market.outcomes).unwrap_or(serde_json::Value::Null);
    sqlx::query(
        r#"
        INSERT INTO predict_fun_markets (
            market_id, condition_id, title, question, description,
            decimal_precision, trading_status, status, is_visible, is_neg_risk,
            is_yield_bearing, fee_rate_bps, outcomes, resolution, observed_at
        ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW()
        )
        ON CONFLICT (market_id) DO UPDATE SET
            condition_id = EXCLUDED.condition_id,
            title = EXCLUDED.title,
            question = EXCLUDED.question,
            description = EXCLUDED.description,
            decimal_precision = EXCLUDED.decimal_precision,
            trading_status = EXCLUDED.trading_status,
            status = EXCLUDED.status,
            is_visible = EXCLUDED.is_visible,
            is_neg_risk = EXCLUDED.is_neg_risk,
            is_yield_bearing = EXCLUDED.is_yield_bearing,
            fee_rate_bps = EXCLUDED.fee_rate_bps,
            outcomes = EXCLUDED.outcomes,
            resolution = EXCLUDED.resolution,
            observed_at = NOW()
        "#,
    )
    .bind(market.id)
    .bind(&market.condition_id)
    .bind(&market.title)
    .bind(&market.question)
    .bind(&market.description)
    .bind(i32::try_from(market.decimal_precision).unwrap_or(i32::MAX))
    .bind(&market.trading_status)
    .bind(&market.status)
    .bind(market.is_visible)
    .bind(market.is_neg_risk)
    .bind(market.is_yield_bearing)
    .bind(market.fee_rate_bps)
    .bind(outcomes)
    .bind(&market.resolution)
    .execute(pool)
    .await?;
    Ok(())
}

async fn persist_book(
    pool: &PgPool,
    market: &PredictMarket,
    book: &OrderBook,
) -> Result<(), sqlx::Error> {
    let normalized = complement_book(book, market.decimal_precision);
    sqlx::query(
        r#"
        INSERT INTO predict_fun_orderbook_ticks (
            market_id, exchange_timestamp_ms,
            best_yes_bid, best_yes_bid_size, best_yes_ask, best_yes_ask_size,
            best_no_bid, best_no_bid_size, best_no_ask, best_no_ask_size,
            received_at
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,NOW())
        "#,
    )
    .bind(book.market_id)
    .bind(book.update_timestamp_ms)
    .bind(normalized.yes_bid)
    .bind(normalized.yes_bid_size)
    .bind(normalized.yes_ask)
    .bind(normalized.yes_ask_size)
    .bind(normalized.no_bid)
    .bind(normalized.no_bid_size)
    .bind(normalized.no_ask)
    .bind(normalized.no_ask_size)
    .execute(pool)
    .await?;
    Ok(())
}

fn env_positive_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{complement_book, validate_api_access, MarketsResponse, OrderBookResponse};
    use rust_decimal_macros::dec;

    #[test]
    fn parses_official_market_payload() {
        let response: MarketsResponse = serde_json::from_str(
            r#"{
              "success": true,
              "cursor": "NDc2",
              "data": [{
                "id": 476,
                "title": "<$2,500",
                "question": "Will Gold close under $2,500?",
                "description": "Resolution rules",
                "conditionId": "0xf5cb",
                "decimalPrecision": 2,
                "tradingStatus": "OPEN",
                "status": "REGISTERED",
                "isVisible": true,
                "isNegRisk": true,
                "isYieldBearing": true,
                "feeRateBps": 200,
                "outcomes": [
                  {"name":"Yes","indexSet":1,"onChainId":"11","status":null},
                  {"name":"No","indexSet":2,"onChainId":"22","status":null}
                ],
                "resolution": null
              }]
            }"#,
        )
        .unwrap();
        assert!(response.success);
        assert_eq!(response.cursor.as_deref(), Some("NDc2"));
        assert_eq!(response.data[0].id, 476);
        assert_eq!(response.data[0].outcomes[1].on_chain_id, "22");
    }

    #[test]
    fn parses_book_and_derives_no_side_at_market_precision() {
        let response: OrderBookResponse = serde_json::from_str(
            r#"{
              "success": true,
              "data": {
                "marketId": 476,
                "updateTimestampMs": 1727910141000,
                "asks": [[0.492, 30192.26]],
                "bids": [[0.491, 303518.1]],
                "lastOrderSettled": null
              }
            }"#,
        )
        .unwrap();
        assert!(response.success);
        let book = complement_book(&response.data, 3);
        assert_eq!(book.yes_bid, Some(dec!(0.491)));
        assert_eq!(book.yes_ask, Some(dec!(0.492)));
        assert_eq!(book.no_bid, Some(dec!(0.508)));
        assert_eq!(book.no_ask, Some(dec!(0.509)));
        assert_eq!(book.no_bid_size, Some(dec!(30192.26)));
        assert_eq!(book.no_ask_size, Some(dec!(303518.1)));
    }

    #[test]
    fn mainnet_requires_api_key_but_official_testnet_does_not() {
        assert!(validate_api_access(MAINNET_API, None).is_err());
        assert!(validate_api_access("https://api.predict.fun/", Some("key")).is_ok());
        assert!(validate_api_access("https://api.predict.fun:443", None).is_err());
        assert!(validate_api_access(TESTNET_API, None).is_ok());
        assert!(validate_api_access("https://example.com", Some("key")).is_err());
    }

    use super::{MAINNET_API, TESTNET_API};
}
