//! Predict.fun public REST market-data adapter.
//
// This crate owns the public REST transport, strict response validation, full
// depth YES/NO projection, and the canonical MarketStream seam. It does not
// contain order submission, wallet, signing, or private-account operations.

use async_trait::async_trait;
pub use hft_core::Symbol;
use hft_core::{
    now_micros, ExchangeEventTimestamp, HftError, HftResult, LocalReceiveTimestamp, Price,
    Quantity, VenueId,
};
use ports::{BookLevel, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream};
use reqwest::{Client, StatusCode};
use rust_decimal::Decimal;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};
use url::Url;

pub const MAINNET_API: &str = "https://api.predict.fun";
pub const TESTNET_API: &str = "https://api-testnet.predict.fun";
pub const MARKETS_PATH: &str = "/v1/markets";
pub const ORDERBOOK_PATH_PREFIX: &str = "/v1/markets/";
pub const MAX_MARKET_PAGES: usize = 1_024;
pub const MAX_MARKETS: usize = 100_000;
pub const MAX_DECIMAL_PRECISION: u32 = 18;

#[derive(Debug, Error)]
pub enum PredictFunError {
    #[error("unsupported Predict.fun API origin: {0}")]
    UnsupportedOrigin(String),
    #[error("Predict.fun mainnet requires an API key")]
    MissingApiKey,
    #[error("build Predict.fun HTTP client: {0}")]
    Client(String),
    #[error("Predict.fun request {path} failed: {message}")]
    Request { path: String, message: String },
    #[error("Predict.fun request {path} returned HTTP {status}: {body}")]
    Http {
        path: String,
        status: StatusCode,
        body: String,
    },
    #[error("Predict.fun response for {path} is invalid JSON: {message}")]
    Json { path: String, message: String },
    #[error("Predict.fun endpoint {path} returned success=false")]
    Api { path: String },
    #[error("Predict.fun market pagination repeated cursor {cursor}")]
    CursorRepeated { cursor: String },
    #[error("Predict.fun market pagination exceeded the page bound")]
    PageLimit,
    #[error("Predict.fun market catalog exceeded the market bound")]
    MarketLimit,
    #[error("Predict.fun market id must be positive")]
    InvalidMarketId,
    #[error(
        "Predict.fun order book response market id {actual} does not match requested {requested}"
    )]
    MarketIdMismatch { requested: i64, actual: i64 },
    #[error("Predict.fun {field} must be positive")]
    NonPositiveClock { field: &'static str },
    #[error("Predict.fun {side} level is invalid: {message}")]
    InvalidLevel { side: &'static str, message: String },
    #[error("Predict.fun order book is crossed")]
    CrossedBook,
    #[error("Predict.fun exchange clock regressed for market {market_id}: {actual} < {previous}")]
    ExchangeClockRegressed {
        market_id: i64,
        previous: u64,
        actual: u64,
    },
    #[error("Predict.fun order book has duplicate {side} prices after canonicalization")]
    DuplicatePrice { side: &'static str },
    #[error("Predict.fun decimal precision {0} exceeds the supported bound")]
    PrecisionOutOfRange(u32),
    #[error("Predict.fun binary outcome binding is invalid: {0}")]
    InvalidBinding(String),
    #[error("Predict.fun order book is not ready: {0}")]
    NotReady(String),
    #[error("Predict.fun stream generation was superseded")]
    SupersededGeneration,
}

pub type PredictFunResult<T> = Result<T, PredictFunError>;

pub fn validate_api_access(base_url: &str, api_key: Option<&str>) -> PredictFunResult<()> {
    let parsed = Url::parse(base_url)
        .map_err(|_| PredictFunError::UnsupportedOrigin(base_url.to_owned()))?;
    let origin_only = parsed.scheme() == "https"
        && parsed.port_or_known_default() == Some(443)
        && matches!(parsed.path(), "" | "/")
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.username().is_empty()
        && parsed.password().is_none();
    if !origin_only {
        return Err(PredictFunError::UnsupportedOrigin(base_url.to_owned()));
    }
    match parsed.host_str() {
        Some("api.predict.fun") if api_key.is_none_or(|key| key.trim().is_empty()) => {
            Err(PredictFunError::MissingApiKey)
        }
        Some("api.predict.fun" | "api-testnet.predict.fun") => Ok(()),
        _ => Err(PredictFunError::UnsupportedOrigin(base_url.to_owned())),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictFunOutcome {
    pub name: String,
    pub index_set: i64,
    pub on_chain_id: String,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictFunMarket {
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
    pub is_yield_bearing: bool,
    pub fee_rate_bps: i32,
    pub outcomes: Vec<PredictFunOutcome>,
    #[serde(default)]
    pub resolution: Option<Value>,
}

impl PredictFunMarket {
    #[must_use]
    pub fn is_collectible(&self) -> bool {
        self.is_visible && self.trading_status.eq_ignore_ascii_case("OPEN")
    }

    pub fn validate(&self) -> PredictFunResult<()> {
        if self.id <= 0 {
            return Err(PredictFunError::InvalidMarketId);
        }
        if self.decimal_precision > MAX_DECIMAL_PRECISION {
            return Err(PredictFunError::PrecisionOutOfRange(self.decimal_precision));
        }
        if self.outcomes.is_empty()
            || self
                .outcomes
                .iter()
                .any(|outcome| outcome.on_chain_id.trim().is_empty())
        {
            return Err(PredictFunError::Json {
                path: MARKETS_PATH.to_owned(),
                message: format!("market {} has no canonical outcome token ids", self.id),
            });
        }
        let mut outcome_tokens = HashSet::new();
        if self
            .outcomes
            .iter()
            .any(|outcome| !outcome_tokens.insert(outcome.on_chain_id.clone()))
        {
            return Err(PredictFunError::Json {
                path: MARKETS_PATH.to_owned(),
                message: format!("market {} has duplicate outcome token ids", self.id),
            });
        }
        Ok(())
    }

    pub fn binary_binding(&self) -> PredictFunResult<PredictFunMarketBinding> {
        if self.outcomes.len() != 2 {
            return Err(PredictFunError::InvalidBinding(
                "binary market must contain exactly YES and NO outcomes".to_owned(),
            ));
        }
        let mut yes = None;
        let mut no = None;
        for outcome in &self.outcomes {
            match outcome.name.trim().to_ascii_lowercase().as_str() {
                "yes" if yes.is_none() => yes = Some(outcome),
                "no" if no.is_none() => no = Some(outcome),
                "yes" | "no" => {
                    return Err(PredictFunError::InvalidBinding(
                        "binary market contains a duplicate YES or NO label".to_owned(),
                    ))
                }
                other => {
                    return Err(PredictFunError::InvalidBinding(format!(
                        "unsupported binary outcome label {other}"
                    )))
                }
            }
        }
        let yes = yes.ok_or_else(|| {
            PredictFunError::InvalidBinding("binary market is missing YES".to_owned())
        })?;
        let no = no.ok_or_else(|| {
            PredictFunError::InvalidBinding("binary market is missing NO".to_owned())
        })?;
        if !((yes.index_set == 1 && no.index_set == 2) || (yes.index_set == 2 && no.index_set == 1))
        {
            return Err(PredictFunError::InvalidBinding(
                "binary outcomes must use index sets 1 and 2".to_owned(),
            ));
        }
        PredictFunMarketBinding::new(
            self.id,
            Symbol::new(yes.on_chain_id.clone()),
            Symbol::new(no.on_chain_id.clone()),
            self.decimal_precision,
        )
    }
}

#[derive(Debug, Clone)]
pub struct ReceivedMarket {
    pub market: PredictFunMarket,
    pub received_at_us: u64,
    /// Original market object, retained so callers can persist unknown fields.
    pub raw: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketsResponse {
    success: bool,
    cursor: Option<String>,
    data: Vec<PredictFunMarket>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderBookResponse {
    success: bool,
    data: PredictFunOrderBook,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PredictFunOrderBook {
    pub market_id: i64,
    pub update_timestamp_ms: i64,
    pub asks: Vec<[Value; 2]>,
    pub bids: Vec<[Value; 2]>,
    #[serde(default)]
    pub last_order_settled: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct ReceivedOrderBook {
    pub requested_market_id: i64,
    pub book: PredictFunOrderBook,
    pub received_at_us: u64,
    /// Original orderbook response object, retained for the persistence sink.
    pub raw: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredictFunLevel {
    pub price: Decimal,
    pub size: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredictFunBookProjection {
    pub market_id: i64,
    pub exchange_timestamp_ms: Option<u64>,
    pub received_at_us: u64,
    pub ready: bool,
    pub readiness_reason: Option<String>,
    pub yes_bids: Vec<PredictFunLevel>,
    pub yes_asks: Vec<PredictFunLevel>,
    pub no_bids: Vec<PredictFunLevel>,
    pub no_asks: Vec<PredictFunLevel>,
    pub raw: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PredictFunOutcomeSide {
    Yes,
    No,
}

impl PredictFunBookProjection {
    pub fn snapshot(
        &self,
        symbol: Symbol,
        outcome: PredictFunOutcomeSide,
        sequence: u64,
    ) -> PredictFunResult<MarketSnapshot> {
        if !self.ready {
            return Err(PredictFunError::NotReady(
                self.readiness_reason
                    .clone()
                    .unwrap_or_else(|| "empty order book".to_owned()),
            ));
        }
        let (bids, asks) = match outcome {
            PredictFunOutcomeSide::Yes => (&self.yes_bids, &self.yes_asks),
            PredictFunOutcomeSide::No => (&self.no_bids, &self.no_asks),
        };
        let to_level = |level: &PredictFunLevel| BookLevel {
            price: Price(level.price),
            quantity: Quantity(level.size),
        };
        let exchange_timestamp_ms =
            self.exchange_timestamp_ms
                .ok_or(PredictFunError::NonPositiveClock {
                    field: "exchange_timestamp_ms",
                })?;
        let timestamp =
            exchange_timestamp_ms
                .checked_mul(1_000)
                .ok_or(PredictFunError::NonPositiveClock {
                    field: "exchange_timestamp_ms",
                })?;
        Ok(MarketSnapshot {
            symbol,
            timestamp,
            bids: bids.iter().map(to_level).collect(),
            asks: asks.iter().map(to_level).collect(),
            sequence,
            source_venue: Some(VenueId::PREDICT_FUN),
            timestamps: hft_core::MarketDataTimestamps {
                exchange_event: Some(ExchangeEventTimestamp::new(timestamp)),
                exchange_trade: None,
                local_receive: Some(LocalReceiveTimestamp::new(self.received_at_us)),
            },
            provider_identity: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PredictFunMarketBinding {
    market_id: i64,
    yes_token: Symbol,
    no_token: Symbol,
    decimal_precision: u32,
}

impl PredictFunMarketBinding {
    pub fn new(
        market_id: i64,
        yes_token: Symbol,
        no_token: Symbol,
        decimal_precision: u32,
    ) -> PredictFunResult<Self> {
        let binding = Self {
            market_id,
            yes_token,
            no_token,
            decimal_precision,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> PredictFunResult<()> {
        if self.market_id <= 0
            || self.yes_token.as_str().trim().is_empty()
            || self.no_token.as_str().trim().is_empty()
        {
            return Err(PredictFunError::InvalidMarketId);
        }
        if self.yes_token == self.no_token {
            return Err(PredictFunError::Json {
                path: MARKETS_PATH.to_owned(),
                message: "YES and NO token ids must be distinct".to_owned(),
            });
        }
        if self.decimal_precision > MAX_DECIMAL_PRECISION {
            return Err(PredictFunError::PrecisionOutOfRange(self.decimal_precision));
        }
        Ok(())
    }

    #[must_use]
    pub fn market_id(&self) -> i64 {
        self.market_id
    }

    #[must_use]
    pub fn decimal_precision(&self) -> u32 {
        self.decimal_precision
    }

    #[must_use]
    pub fn yes_token(&self) -> &Symbol {
        &self.yes_token
    }

    #[must_use]
    pub fn no_token(&self) -> &Symbol {
        &self.no_token
    }
}

#[derive(Debug, Clone)]
pub struct PredictFunClient {
    client: Client,
    origin: Url,
    api_key: Option<SecretString>,
    acceptance: Arc<Mutex<AcceptanceTracker>>,
}

#[derive(Debug, Default)]
struct MarketAcceptance {
    high_water_exchange_timestamp_ms: Option<u64>,
    active: bool,
    invalidated: bool,
}

#[derive(Debug, Default)]
struct AcceptanceTracker {
    markets: HashMap<i64, MarketAcceptance>,
    pending_invalidations: Vec<PredictFunBookProjection>,
    generation: u64,
}

impl AcceptanceTracker {
    fn accept(
        &mut self,
        projection: PredictFunBookProjection,
    ) -> PredictFunResult<PredictFunBookProjection> {
        let state = self.markets.entry(projection.market_id).or_default();
        state.active = true;
        if let (Some(previous), Some(actual)) = (
            state.high_water_exchange_timestamp_ms,
            projection.exchange_timestamp_ms,
        ) {
            if actual < previous {
                state.invalidated = true;
                return Err(PredictFunError::ExchangeClockRegressed {
                    market_id: projection.market_id,
                    previous,
                    actual,
                });
            }
        }
        if let Some(actual) = projection.exchange_timestamp_ms {
            state.high_water_exchange_timestamp_ms = Some(
                state
                    .high_water_exchange_timestamp_ms
                    .map_or(actual, |previous| previous.max(actual)),
            );
        }
        state.invalidated = !projection.ready;
        Ok(projection)
    }

    fn invalidate(
        &mut self,
        market_id: i64,
        reason: String,
    ) -> PredictFunResult<PredictFunBookProjection> {
        let state = self.markets.entry(market_id).or_default();
        state.active = true;
        state.invalidated = true;
        failure_projection(market_id, reason)
    }

    fn reconcile_catalog(&mut self, active_market_ids: &HashSet<i64>) {
        let previous_active = self
            .markets
            .iter()
            .filter_map(|(market_id, state)| state.active.then_some(*market_id))
            .collect::<Vec<_>>();
        for market_id in previous_active {
            if !active_market_ids.contains(&market_id) {
                if let Ok(projection) = failure_projection(
                    market_id,
                    "market is absent from the active Predict.fun catalog".to_owned(),
                ) {
                    self.pending_invalidations.push(projection);
                }
                if let Some(state) = self.markets.get_mut(&market_id) {
                    state.active = false;
                    state.invalidated = true;
                }
            }
        }
        for market_id in active_market_ids {
            self.markets.entry(*market_id).or_default().active = true;
        }
    }

    fn invalidate_all(&mut self, reason: String) {
        let active_market_ids = self
            .markets
            .iter()
            .filter_map(|(market_id, state)| state.active.then_some(*market_id))
            .collect::<Vec<_>>();
        for market_id in active_market_ids {
            if let Ok(projection) = self.invalidate(market_id, reason.clone()) {
                self.pending_invalidations.push(projection);
            }
        }
    }

    fn drain_invalidations(&mut self) -> Vec<PredictFunBookProjection> {
        std::mem::take(&mut self.pending_invalidations)
    }

    fn all_ready(&self, market_ids: &HashSet<i64>) -> bool {
        !market_ids.is_empty()
            && market_ids.iter().all(|market_id| {
                self.markets
                    .get(market_id)
                    .is_some_and(|state| state.active && !state.invalidated)
            })
    }
}

fn failure_projection(
    market_id: i64,
    reason: String,
) -> PredictFunResult<PredictFunBookProjection> {
    let received_at_us = now_micros();
    if received_at_us == 0 {
        return Err(PredictFunError::NonPositiveClock {
            field: "local_receive_us",
        });
    }
    Ok(PredictFunBookProjection {
        market_id,
        exchange_timestamp_ms: None,
        received_at_us,
        ready: false,
        readiness_reason: Some(reason.clone()),
        yes_bids: Vec::new(),
        yes_asks: Vec::new(),
        no_bids: Vec::new(),
        no_asks: Vec::new(),
        raw: serde_json::json!({"error": reason}),
    })
}

impl PredictFunClient {
    pub fn new(base_url: String, api_key: Option<SecretString>) -> PredictFunResult<Self> {
        validate_api_access(&base_url, api_key.as_ref().map(ExposeSecret::expose_secret))?;
        let origin = Url::parse(base_url.trim_end_matches('/'))
            .map_err(|error| PredictFunError::Client(error.to_string()))?;
        Self::build(origin, api_key)
    }

    fn build(origin: Url, api_key: Option<SecretString>) -> PredictFunResult<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| PredictFunError::Client(error.to_string()))?;
        Ok(Self {
            client,
            origin,
            api_key,
            acceptance: Arc::new(Mutex::new(AcceptanceTracker::default())),
        })
    }

    #[cfg(test)]
    fn for_test(base_url: &str) -> Self {
        Self::build(Url::parse(base_url).unwrap(), None).unwrap()
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> PredictFunResult<(Value, u64)> {
        let mut url = self.origin.clone();
        url.set_path(path);
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        let mut request = self.client.get(url);
        if let Some(api_key) = &self.api_key {
            request = request.header("x-api-key", api_key.expose_secret());
        }
        let response = request
            .send()
            .await
            .map_err(|error| PredictFunError::Request {
                path: path.to_owned(),
                message: error.to_string(),
            })?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| PredictFunError::Request {
                path: path.to_owned(),
                message: error.to_string(),
            })?;
        let received_at_us = now_micros();
        if !status.is_success() {
            return Err(PredictFunError::Http {
                path: path.to_owned(),
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
        }
        let value = serde_json::from_slice(&body).map_err(|error| PredictFunError::Json {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
        if received_at_us == 0 {
            return Err(PredictFunError::NonPositiveClock {
                field: "local_receive_us",
            });
        }
        Ok((value, received_at_us))
    }

    pub async fn markets(&self) -> PredictFunResult<Vec<ReceivedMarket>> {
        let result = self.fetch_markets().await;
        match result {
            Ok(markets) => {
                let active_market_ids = markets
                    .iter()
                    .filter(|received| received.market.is_collectible())
                    .map(|received| received.market.id)
                    .collect::<HashSet<_>>();
                self.acceptance
                    .lock()
                    .map_err(|_| {
                        PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
                    })?
                    .reconcile_catalog(&active_market_ids);
                Ok(markets)
            }
            Err(error) => {
                if let Ok(mut acceptance) = self.acceptance.lock() {
                    acceptance.invalidate_all(error.to_string());
                }
                Err(error)
            }
        }
    }

    async fn fetch_markets(&self) -> PredictFunResult<Vec<ReceivedMarket>> {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let mut seen_market_ids = HashSet::new();
        let mut markets = Vec::new();
        for _ in 0..MAX_MARKET_PAGES {
            let query = cursor
                .as_deref()
                .map(|cursor| vec![("first", "100"), ("after", cursor)])
                .unwrap_or_else(|| vec![("first", "100")]);
            let (raw, received_at_us) = self.get_json(MARKETS_PATH, &query).await?;
            let response: MarketsResponse =
                serde_json::from_value(raw.clone()).map_err(|error| PredictFunError::Json {
                    path: MARKETS_PATH.to_owned(),
                    message: error.to_string(),
                })?;
            if !response.success {
                return Err(PredictFunError::Api {
                    path: MARKETS_PATH.to_owned(),
                });
            }
            let raw_markets =
                raw.get("data")
                    .and_then(Value::as_array)
                    .ok_or_else(|| PredictFunError::Json {
                        path: MARKETS_PATH.to_owned(),
                        message: "data is not an array".to_owned(),
                    })?;
            if raw_markets.len() != response.data.len() {
                return Err(PredictFunError::Json {
                    path: MARKETS_PATH.to_owned(),
                    message: "typed and raw market counts differ".to_owned(),
                });
            }
            for (market, raw_market) in response.data.into_iter().zip(raw_markets.iter()) {
                market.validate()?;
                if !seen_market_ids.insert(market.id) {
                    return Err(PredictFunError::Json {
                        path: MARKETS_PATH.to_owned(),
                        message: format!("market {} appeared more than once", market.id),
                    });
                }
                markets.push(ReceivedMarket {
                    market,
                    received_at_us,
                    raw: raw_market.clone(),
                });
                if markets.len() > MAX_MARKETS {
                    return Err(PredictFunError::MarketLimit);
                }
            }
            let Some(next) = response.cursor.filter(|cursor| !cursor.trim().is_empty()) else {
                return Ok(markets);
            };
            if !seen_cursors.insert(next.clone()) {
                return Err(PredictFunError::CursorRepeated { cursor: next });
            }
            cursor = Some(next);
        }
        Err(PredictFunError::PageLimit)
    }

    pub async fn orderbook(&self, market_id: i64) -> PredictFunResult<ReceivedOrderBook> {
        if market_id <= 0 {
            return Err(PredictFunError::InvalidMarketId);
        }
        let path_string = format!("{ORDERBOOK_PATH_PREFIX}{market_id}/orderbook");
        let (raw, received_at_us) = self.get_json(&path_string, &[]).await?;
        let response: OrderBookResponse =
            serde_json::from_value(raw.clone()).map_err(|error| PredictFunError::Json {
                path: path_string.clone(),
                message: error.to_string(),
            })?;
        if !response.success {
            return Err(PredictFunError::Api { path: path_string });
        }
        if response.data.market_id != market_id {
            return Err(PredictFunError::MarketIdMismatch {
                requested: market_id,
                actual: response.data.market_id,
            });
        }
        if response.data.update_timestamp_ms <= 0 {
            return Err(PredictFunError::NonPositiveClock {
                field: "exchange_timestamp_ms",
            });
        }
        Ok(ReceivedOrderBook {
            requested_market_id: market_id,
            book: response.data,
            received_at_us,
            raw,
        })
    }

    pub fn accept_orderbook(
        &self,
        received: &ReceivedOrderBook,
        decimal_precision: u32,
    ) -> PredictFunResult<PredictFunBookProjection> {
        let projection = project_orderbook(received, decimal_precision)?;
        self.acceptance
            .lock()
            .map_err(|_| {
                PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
            })?
            .accept(projection)
    }

    fn begin_stream_generation(&self) -> PredictFunResult<u64> {
        let mut acceptance = self.acceptance.lock().map_err(|_| {
            PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
        })?;
        acceptance.generation = acceptance.generation.saturating_add(1);
        Ok(acceptance.generation)
    }

    fn accept_orderbook_for_generation(
        &self,
        generation: u64,
        received: &ReceivedOrderBook,
        decimal_precision: u32,
    ) -> PredictFunResult<Option<PredictFunBookProjection>> {
        let projection = project_orderbook(received, decimal_precision)?;
        let mut acceptance = self.acceptance.lock().map_err(|_| {
            PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
        })?;
        if acceptance.generation != generation {
            return Ok(None);
        }
        acceptance.accept(projection).map(Some)
    }

    pub fn invalidate_market(
        &self,
        market_id: i64,
        reason: impl Into<String>,
    ) -> PredictFunResult<PredictFunBookProjection> {
        self.acceptance
            .lock()
            .map_err(|_| {
                PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
            })?
            .invalidate(market_id, reason.into())
    }

    fn invalidate_market_for_generation(
        &self,
        generation: u64,
        market_id: i64,
        reason: String,
    ) -> PredictFunResult<Option<PredictFunBookProjection>> {
        let mut acceptance = self.acceptance.lock().map_err(|_| {
            PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
        })?;
        if acceptance.generation != generation {
            return Ok(None);
        }
        acceptance.invalidate(market_id, reason).map(Some)
    }

    pub fn seed_acceptance_clock(
        &self,
        market_id: i64,
        exchange_timestamp_ms: u64,
    ) -> PredictFunResult<()> {
        if market_id <= 0 || exchange_timestamp_ms == 0 {
            return Err(PredictFunError::InvalidMarketId);
        }
        let mut acceptance = self.acceptance.lock().map_err(|_| {
            PredictFunError::Client("Predict.fun acceptance state poisoned".to_owned())
        })?;
        let state = acceptance.markets.entry(market_id).or_default();
        state.high_water_exchange_timestamp_ms = Some(
            state
                .high_water_exchange_timestamp_ms
                .map_or(exchange_timestamp_ms, |previous| {
                    previous.max(exchange_timestamp_ms)
                }),
        );
        state.active = true;
        state.invalidated = true;
        Ok(())
    }

    pub fn drain_invalidations(&self) -> Vec<PredictFunBookProjection> {
        self.acceptance
            .lock()
            .map(|mut acceptance| acceptance.drain_invalidations())
            .unwrap_or_default()
    }

    fn all_markets_ready(&self, market_ids: &HashSet<i64>) -> bool {
        self.acceptance
            .lock()
            .map(|acceptance| acceptance.all_ready(market_ids))
            .unwrap_or(false)
    }
}

fn parse_decimal(
    value: &Value,
    side: &'static str,
    field: &'static str,
) -> PredictFunResult<Decimal> {
    let text = match value {
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        _ => {
            return Err(PredictFunError::InvalidLevel {
                side,
                message: format!("{field} is not a decimal"),
            })
        }
    };
    Decimal::from_str(&text).map_err(|error| PredictFunError::InvalidLevel {
        side,
        message: format!("{field} is invalid: {error}"),
    })
}

fn parse_levels(rows: &[[Value; 2]], side: &'static str) -> PredictFunResult<Vec<PredictFunLevel>> {
    let mut levels = rows
        .iter()
        .map(|row| {
            let price = parse_decimal(&row[0], side, "price")?;
            let size = parse_decimal(&row[1], side, "size")?;
            if !(Decimal::ZERO..=Decimal::ONE).contains(&price) || size <= Decimal::ZERO {
                return Err(PredictFunError::InvalidLevel {
                    side,
                    message: format!("price={price} size={size} is out of range"),
                });
            }
            Ok(PredictFunLevel { price, size })
        })
        .collect::<PredictFunResult<Vec<_>>>()?;
    let descending = side == "bids";
    levels.sort_by(|left, right| {
        if descending {
            right.price.cmp(&left.price)
        } else {
            left.price.cmp(&right.price)
        }
    });
    if levels
        .windows(2)
        .any(|window| window[0].price == window[1].price)
    {
        return Err(PredictFunError::DuplicatePrice { side });
    }
    Ok(levels)
}

fn complement_levels(
    levels: &[PredictFunLevel],
    precision: u32,
    descending: bool,
    side: &'static str,
) -> PredictFunResult<Vec<PredictFunLevel>> {
    let mut result = levels
        .iter()
        .map(|level| PredictFunLevel {
            price: (Decimal::ONE - level.price).round_dp(precision),
            size: level.size,
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        if descending {
            right.price.cmp(&left.price)
        } else {
            left.price.cmp(&right.price)
        }
    });
    if result
        .windows(2)
        .any(|window| window[0].price == window[1].price)
    {
        return Err(PredictFunError::DuplicatePrice { side });
    }
    Ok(result)
}

pub fn project_orderbook(
    received: &ReceivedOrderBook,
    decimal_precision: u32,
) -> PredictFunResult<PredictFunBookProjection> {
    if received.requested_market_id <= 0 {
        return Err(PredictFunError::InvalidMarketId);
    }
    if received.book.market_id != received.requested_market_id {
        return Err(PredictFunError::MarketIdMismatch {
            requested: received.requested_market_id,
            actual: received.book.market_id,
        });
    }
    if decimal_precision > MAX_DECIMAL_PRECISION {
        return Err(PredictFunError::PrecisionOutOfRange(decimal_precision));
    }
    if received.book.update_timestamp_ms <= 0 {
        return Err(PredictFunError::NonPositiveClock {
            field: "exchange_timestamp_ms",
        });
    }
    if received.received_at_us == 0 {
        return Err(PredictFunError::NonPositiveClock {
            field: "local_receive_us",
        });
    }
    let exchange_timestamp_ms = u64::try_from(received.book.update_timestamp_ms).map_err(|_| {
        PredictFunError::NonPositiveClock {
            field: "exchange_timestamp_ms",
        }
    })?;
    exchange_timestamp_ms
        .checked_mul(1_000)
        .ok_or(PredictFunError::NonPositiveClock {
            field: "exchange_timestamp_ms",
        })?;
    let yes_bids = parse_levels(&received.book.bids, "bids")?;
    let yes_asks = parse_levels(&received.book.asks, "asks")?;
    if let (Some(best_bid), Some(best_ask)) = (yes_bids.first(), yes_asks.first()) {
        if best_bid.price > best_ask.price {
            return Err(PredictFunError::CrossedBook);
        }
    }
    let no_bids = complement_levels(&yes_asks, decimal_precision, true, "no_bids")?;
    let no_asks = complement_levels(&yes_bids, decimal_precision, false, "no_asks")?;
    if let (Some(best_bid), Some(best_ask)) = (no_bids.first(), no_asks.first()) {
        if best_bid.price > best_ask.price {
            return Err(PredictFunError::CrossedBook);
        }
    }
    let ready = !yes_bids.is_empty() && !yes_asks.is_empty();
    Ok(PredictFunBookProjection {
        market_id: received.requested_market_id,
        exchange_timestamp_ms: Some(exchange_timestamp_ms),
        received_at_us: received.received_at_us,
        ready,
        readiness_reason: (!ready).then(|| "empty bid or ask side".to_owned()),
        yes_bids,
        yes_asks,
        no_bids,
        no_asks,
        raw: received.raw.clone(),
    })
}

#[derive(Debug, Default)]
struct StreamState {
    connected: AtomicBool,
    enabled: AtomicBool,
    generation: AtomicU64,
    last_receive_us: AtomicU64,
    selected_markets: Mutex<HashSet<i64>>,
}

pub struct PredictFunMarketStream {
    client: PredictFunClient,
    bindings: Vec<PredictFunMarketBinding>,
    poll_interval: Duration,
    state: Arc<StreamState>,
}

impl PredictFunMarketStream {
    pub fn new(
        client: PredictFunClient,
        bindings: Vec<PredictFunMarketBinding>,
        poll_interval: Duration,
    ) -> PredictFunResult<Self> {
        if bindings.is_empty() || poll_interval.is_zero() {
            return Err(PredictFunError::Json {
                path: MARKETS_PATH.to_owned(),
                message: "Predict.fun stream requires markets and a positive poll interval"
                    .to_owned(),
            });
        }
        let mut market_ids = HashSet::new();
        let mut tokens = HashSet::new();
        for binding in &bindings {
            binding.validate()?;
            if !market_ids.insert(binding.market_id)
                || !tokens.insert(binding.yes_token.as_str().to_owned())
                || !tokens.insert(binding.no_token.as_str().to_owned())
            {
                return Err(PredictFunError::Json {
                    path: MARKETS_PATH.to_owned(),
                    message: "Predict.fun market bindings must be unique".to_owned(),
                });
            }
        }
        Ok(Self {
            client,
            bindings,
            poll_interval,
            state: Arc::new(StreamState::default()),
        })
    }

    fn binding_for(
        &self,
        symbol: &Symbol,
    ) -> Option<(PredictFunMarketBinding, PredictFunOutcomeSide)> {
        self.bindings.iter().find_map(|binding| {
            if binding.yes_token == *symbol {
                Some((binding.clone(), PredictFunOutcomeSide::Yes))
            } else if binding.no_token == *symbol {
                Some((binding.clone(), PredictFunOutcomeSide::No))
            } else {
                None
            }
        })
    }

    async fn validate_against_catalog(&self) -> HftResult<()> {
        let markets = self.client.markets().await.map_err(hft_error)?;
        for binding in &self.bindings {
            let market = markets
                .iter()
                .find(|received| received.market.id == binding.market_id)
                .ok_or_else(|| {
                    HftError::Config(format!(
                        "Predict.fun market {} is absent from the fresh catalog",
                        binding.market_id
                    ))
                })?;
            let canonical = market.market.binary_binding().map_err(hft_error)?;
            if canonical.yes_token != binding.yes_token
                || canonical.no_token != binding.no_token
                || canonical.decimal_precision != binding.decimal_precision
            {
                return Err(HftError::Config(format!(
                    "Predict.fun market {} binding disagrees with the fresh catalog",
                    binding.market_id
                )));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl MarketStream for PredictFunMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::Config(
                "Predict.fun requires at least one configured outcome token".to_owned(),
            ));
        }
        let state = Arc::clone(&self.state);
        let generation = self.client.begin_stream_generation().map_err(hft_error)?;
        state.generation.store(generation, Ordering::Release);
        state.enabled.store(false, Ordering::Release);
        state.connected.store(false, Ordering::Release);
        if let Ok(mut selected_markets) = state.selected_markets.lock() {
            selected_markets.clear();
        }
        self.validate_against_catalog().await?;
        let selected = symbols
            .into_iter()
            .map(|symbol| {
                self.binding_for(&symbol)
                    .map(|(binding, outcome)| (symbol.clone(), binding, outcome))
                    .ok_or_else(|| {
                        HftError::Config(format!(
                            "Predict.fun token {} is missing from market bindings",
                            symbol.as_str()
                        ))
                    })
            })
            .collect::<HftResult<Vec<_>>>()?;
        let mut grouped: HashMap<
            i64,
            (
                PredictFunMarketBinding,
                Vec<(Symbol, PredictFunOutcomeSide)>,
            ),
        > = HashMap::new();
        for (symbol, binding, outcome) in selected {
            grouped
                .entry(binding.market_id)
                .or_insert_with(|| (binding.clone(), Vec::new()))
                .1
                .push((symbol, outcome));
        }
        if let Ok(mut selected_markets) = state.selected_markets.lock() {
            selected_markets.clear();
            selected_markets.extend(grouped.keys().copied());
        }
        state.enabled.store(true, Ordering::Release);
        state.connected.store(false, Ordering::Release);
        let client = self.client.clone();
        let poll_interval = self.poll_interval;
        let (tx, mut rx) = mpsc::channel(1_024);
        tokio::spawn(async move {
            let mut ticker = interval(poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut sequences: HashMap<(i64, PredictFunOutcomeSide), u64> = HashMap::new();
            let mut invalidated = HashSet::new();
            let mut ready_markets = HashSet::new();
            while state.enabled.load(Ordering::Acquire)
                && state.generation.load(Ordering::Acquire) == generation
                && !tx.is_closed()
            {
                ticker.tick().await;
                for (market_id, (binding, outcomes)) in &grouped {
                    let result = client.orderbook(*market_id).await.and_then(|received| {
                        client
                            .accept_orderbook_for_generation(
                                generation,
                                &received,
                                binding.decimal_precision,
                            )
                            .and_then(|projection| {
                                projection.ok_or(PredictFunError::SupersededGeneration)
                            })
                    });
                    if state.generation.load(Ordering::Acquire) != generation {
                        return;
                    }
                    match result {
                        Ok(projection) if projection.ready => {
                            state
                                .last_receive_us
                                .store(projection.received_at_us, Ordering::Release);
                            ready_markets.insert(*market_id);
                            invalidated.remove(market_id);
                            for (symbol, outcome) in outcomes {
                                let key = (*market_id, *outcome);
                                let sequence = sequences.entry(key).or_default();
                                *sequence = sequence.saturating_add(1);
                                let event = projection
                                    .snapshot(symbol.clone(), *outcome, *sequence)
                                    .map(MarketEvent::Snapshot)
                                    .map_err(hft_error);
                                if tx.send(event).await.is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(projection) => {
                            ready_markets.remove(market_id);
                            if invalidated.insert(*market_id) {
                                for (symbol, _) in outcomes {
                                    if tx
                                        .send(Ok(MarketEvent::Disconnect {
                                            reason: projection
                                                .readiness_reason
                                                .clone()
                                                .unwrap_or_else(|| {
                                                    "Predict.fun order book not ready".to_owned()
                                                }),
                                            source_venue: Some(VenueId::PREDICT_FUN),
                                            symbol: Some(symbol.clone()),
                                            connection_started_at: None,
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            ready_markets.remove(market_id);
                            let reason = error.to_string();
                            match client.invalidate_market_for_generation(
                                generation,
                                *market_id,
                                reason.clone(),
                            ) {
                                Ok(Some(_)) => {}
                                Ok(None) | Err(_) => return,
                            }
                            if invalidated.insert(*market_id) {
                                for (symbol, _) in outcomes {
                                    if tx
                                        .send(Ok(MarketEvent::Disconnect {
                                            reason: reason.clone(),
                                            source_venue: Some(VenueId::PREDICT_FUN),
                                            symbol: Some(symbol.clone()),
                                            connection_started_at: None,
                                        }))
                                        .await
                                        .is_err()
                                    {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
                if state.generation.load(Ordering::Acquire) != generation {
                    return;
                }
                state
                    .connected
                    .store(ready_markets.len() == grouped.len(), Ordering::Release);
            }
        });
        Ok(Box::pin(futures::stream::poll_fn(move |cx| {
            rx.poll_recv(cx)
        })))
    }

    async fn health(&self) -> ConnectionHealth {
        let selected_markets = self
            .state
            .selected_markets
            .lock()
            .map(|market_ids| market_ids.clone())
            .unwrap_or_default();
        ConnectionHealth {
            connected: self.state.connected.load(Ordering::Acquire)
                && self.client.all_markets_ready(&selected_markets),
            latency_ms: None,
            last_heartbeat: self.state.last_receive_us.load(Ordering::Acquire),
        }
    }

    async fn connect(&mut self) -> HftResult<()> {
        self.state.enabled.store(true, Ordering::Release);
        Ok(())
    }

    async fn disconnect(&mut self) -> HftResult<()> {
        let generation = self.client.begin_stream_generation().map_err(hft_error)?;
        self.state.generation.store(generation, Ordering::Release);
        self.state.enabled.store(false, Ordering::Release);
        self.state.connected.store(false, Ordering::Release);
        if let Ok(mut selected_markets) = self.state.selected_markets.lock() {
            selected_markets.clear();
        }
        Ok(())
    }
}

fn hft_error(error: PredictFunError) -> HftError {
    HftError::Parse(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use std::thread;
    use tokio::time::{timeout, Duration as TokioDuration};

    fn valid_book(market_id: i64, asks: Value, bids: Value, timestamp: i64) -> Value {
        json!({
            "success": true,
            "data": {
                "marketId": market_id,
                "updateTimestampMs": timestamp,
                "asks": asks,
                "bids": bids,
                "lastOrderSettled": null,
                "unknownField": "preserved"
            },
            "unknownResponseField": "preserved"
        })
    }

    fn response_bytes(value: &Value, status: &str) -> Vec<u8> {
        let body = value.to_string();
        format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }
    fn spawn_responses(responses: Vec<(Value, &'static str)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for (body, status) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1_024];
                let _ = stream.read(&mut request);
                stream.write_all(&response_bytes(&body, status)).unwrap();
            }
        });
        (format!("http://{address}"), handle)
    }

    fn market_value(id: i64, cursor: Option<&str>) -> Value {
        let mut response = json!({
            "success": true,
            "data": [{
                "id": id,
                "title": format!("Market {id}"),
                "question": format!("Question {id}"),
                "conditionId": format!("condition-{id}"),
                "decimalPrecision": 3,
                "tradingStatus": "OPEN",
                "status": "REGISTERED",
                "isVisible": true,
                "isNegRisk": false,
                "isYieldBearing": false,
                "feeRateBps": 0,
                "outcomes": [
                    {"name": "Yes", "indexSet": 1, "onChainId": format!("yes-token-{id}")},
                    {"name": "No", "indexSet": 2, "onChainId": format!("no-token-{id}")}
                ],
                "unknownMarketField": "preserved"
            }]
        });
        if let Some(cursor) = cursor {
            response["cursor"] = json!(cursor);
        }
        response
    }

    fn market_catalog(ids: &[i64]) -> Value {
        let mut response = market_value(ids[0], None);
        let markets = response["data"].as_array_mut().unwrap();
        for id in ids.iter().skip(1) {
            markets.push(market_value(*id, None)["data"][0].clone());
        }
        response
    }

    fn received_book(value: Value, market_id: i64, received_at_us: u64) -> ReceivedOrderBook {
        ReceivedOrderBook {
            requested_market_id: market_id,
            book: serde_json::from_value(value["data"].clone()).unwrap(),
            received_at_us,
            raw: value,
        }
    }

    #[test]
    fn origin_and_key_policy_is_fail_closed() {
        assert!(validate_api_access(MAINNET_API, None).is_err());
        assert!(validate_api_access(MAINNET_API, Some("key")).is_ok());
        assert!(validate_api_access(TESTNET_API, None).is_ok());
        for origin in [
            "http://api.predict.fun",
            "https://evil.example",
            "https://api.predict.fun/path",
            "https://api.predict.fun?evil=1",
            "https://user:pass@api.predict.fun",
        ] {
            assert!(
                validate_api_access(origin, Some("key")).is_err(),
                "{origin}"
            );
        }
    }

    #[test]
    fn projects_full_depth_yes_no_and_precision_without_crossing() {
        let raw = valid_book(
            7,
            json!([["0.42", "3"], ["0.45", "4"]]),
            json!([["0.40", "2"], ["0.35", "5"]]),
            1_700_000_000_000,
        );
        let projection =
            project_orderbook(&received_book(raw, 7, 1_700_000_000_500_000), 2).unwrap();
        assert!(projection.ready);
        assert_eq!(projection.yes_bids.len(), 2);
        assert_eq!(projection.yes_asks.len(), 2);
        assert_eq!(projection.no_bids[0].price, Decimal::new(58, 2));
        assert_eq!(projection.no_asks[0].price, Decimal::new(60, 2));
        assert_eq!(projection.raw["data"]["unknownField"], "preserved");
    }

    #[test]
    fn empty_book_is_not_ready_and_precision_collision_fails_closed() {
        let empty = valid_book(7, json!([]), json!([]), 1_700_000_000_000);
        let projection =
            project_orderbook(&received_book(empty, 7, 1_700_000_000_500_000), 2).unwrap();
        assert!(!projection.ready);
        assert!(projection.readiness_reason.is_some());

        let collision = valid_book(
            7,
            json!([["0.401", "1"], ["0.402", "1"]]),
            json!([["0.2", "1"]]),
            1_700_000_000_000,
        );
        assert!(matches!(
            project_orderbook(&received_book(collision, 7, 1_700_000_000_500_000), 2),
            Err(PredictFunError::DuplicatePrice { .. })
        ));
    }

    #[test]
    fn standalone_adapter_preserves_unquoted_high_precision_numbers() {
        let raw: Value = serde_json::from_str(
            r#"{
                "success": true,
                "data": {
                    "marketId": 7,
                    "updateTimestampMs": 1700000000000,
                    "asks": [[0.123456789012345678, 2]],
                    "bids": [[0.123456789012345677, 3]]
                }
            }"#,
        )
        .unwrap();
        let projection =
            project_orderbook(&received_book(raw, 7, 1_700_000_000_500_000), 18).unwrap();
        assert_eq!(
            projection.yes_asks[0].price,
            Decimal::from_str("0.123456789012345678").unwrap()
        );
        assert_eq!(projection.yes_asks[0].size, Decimal::from(2));
    }

    #[test]
    fn acceptance_clock_survives_invalidation_and_rejects_older_recovery() {
        let client = PredictFunClient::for_test("http://127.0.0.1:1");
        let first = valid_book(
            7,
            json!([["0.6", "1"]]),
            json!([["0.4", "1"]]),
            1_700_000_000_010,
        );
        let first = client
            .accept_orderbook(&received_book(first, 7, 1_700_000_000_500_000), 3)
            .unwrap();
        assert!(first.ready);
        let invalidated = client.invalidate_market(7, "transport failure").unwrap();
        assert!(!invalidated.ready);
        assert_eq!(invalidated.exchange_timestamp_ms, None);

        let older = valid_book(
            7,
            json!([["0.61", "1"]]),
            json!([["0.39", "1"]]),
            1_700_000_000_009,
        );
        assert!(matches!(
            client.accept_orderbook(&received_book(older, 7, 1_700_000_001_500_000), 3),
            Err(PredictFunError::ExchangeClockRegressed { .. })
        ));
    }

    #[test]
    fn non_ready_exchange_observation_advances_high_water_for_recovery() {
        let client = PredictFunClient::for_test("http://127.0.0.1:1");
        let ready = valid_book(7, json!([["0.6", "1"]]), json!([["0.4", "1"]]), 100);
        assert!(
            client
                .accept_orderbook(&received_book(ready, 7, 1_700_000_000_500_000), 3)
                .unwrap()
                .ready
        );
        let empty = valid_book(7, json!([]), json!([]), 200);
        assert!(
            !client
                .accept_orderbook(&received_book(empty, 7, 1_700_000_000_500_001), 3)
                .unwrap()
                .ready
        );

        let restarted = PredictFunClient::for_test("http://127.0.0.1:1");
        restarted.seed_acceptance_clock(7, 200).unwrap();
        let older = valid_book(7, json!([["0.61", "1"]]), json!([["0.39", "1"]]), 150);
        assert!(matches!(
            restarted.accept_orderbook(&received_book(older, 7, 1_700_000_000_500_002), 3),
            Err(PredictFunError::ExchangeClockRegressed { .. })
        ));
    }

    #[test]
    fn binary_binding_requires_exact_binary_labels_and_accepts_reversed_order() {
        let mut reversed = market_value(1, None)["data"][0].clone();
        let outcomes = reversed["outcomes"].as_array_mut().unwrap();
        outcomes.swap(0, 1);
        let market: PredictFunMarket = serde_json::from_value(reversed).unwrap();
        let binding = market.binary_binding().unwrap();
        assert_eq!(binding.yes_token.as_str(), "yes-token-1");
        assert_eq!(binding.no_token.as_str(), "no-token-1");

        let mut duplicate = market_value(1, None)["data"][0].clone();
        duplicate["outcomes"][1]["name"] = json!("Yes");
        let duplicate: PredictFunMarket = serde_json::from_value(duplicate).unwrap();
        assert!(matches!(
            duplicate.binary_binding(),
            Err(PredictFunError::InvalidBinding(_))
        ));

        let mut extra = market_value(1, None)["data"][0].clone();
        extra["outcomes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"Maybe","indexSet":4,"onChainId":"maybe-token"}));
        let extra: PredictFunMarket = serde_json::from_value(extra).unwrap();
        assert!(matches!(
            extra.binary_binding(),
            Err(PredictFunError::InvalidBinding(_))
        ));

        let mut wrong_index = market_value(1, None)["data"][0].clone();
        wrong_index["outcomes"][1]["indexSet"] = json!(3);
        let wrong_index: PredictFunMarket = serde_json::from_value(wrong_index).unwrap();
        assert!(matches!(
            wrong_index.binary_binding(),
            Err(PredictFunError::InvalidBinding(_))
        ));
    }

    #[test]
    fn public_structs_cannot_bypass_market_id_or_binding_validation() {
        let raw = valid_book(
            7,
            json!([["0.6", "1"]]),
            json!([["0.4", "1"]]),
            1_700_000_000_000,
        );
        let mut invalid_market = received_book(raw.clone(), 0, 1_700_000_000_500_000);
        assert!(matches!(
            project_orderbook(&invalid_market, 2),
            Err(PredictFunError::InvalidMarketId)
        ));
        invalid_market.requested_market_id = 8;
        assert!(matches!(
            project_orderbook(&invalid_market, 2),
            Err(PredictFunError::MarketIdMismatch {
                requested: 8,
                actual: 7
            })
        ));

        let invalid_binding = PredictFunMarketBinding {
            market_id: 0,
            yes_token: Symbol::new("yes"),
            no_token: Symbol::new("no"),
            decimal_precision: 2,
        };
        let client = PredictFunClient::for_test("http://127.0.0.1:1");
        assert!(matches!(
            PredictFunMarketStream::new(client, vec![invalid_binding], Duration::from_millis(1)),
            Err(PredictFunError::InvalidMarketId)
        ));
    }

    #[tokio::test]
    async fn catalog_pagination_is_bounded_and_preserves_raw_market_fields() {
        let page_one = market_value(1, Some("page-1"));
        let page_two = market_value(2, None);
        let (origin, server) = spawn_responses(vec![(page_one, "200 OK"), (page_two, "200 OK")]);
        let client = PredictFunClient::for_test(&origin);
        let markets = client.markets().await.unwrap();
        assert_eq!(markets.len(), 2);
        assert_eq!(markets[0].raw["unknownMarketField"], "preserved");
        assert!(markets.iter().all(|market| market.received_at_us > 0));
        server.join().unwrap();

        let (origin, server) = spawn_responses(vec![
            (market_value(1, Some("loop")), "200 OK"),
            (market_value(2, Some("loop")), "200 OK"),
        ]);
        let error = PredictFunClient::for_test(&origin)
            .markets()
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            PredictFunError::CursorRepeated { cursor } if cursor == "loop"
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn catalog_failure_invalidates_previously_active_market() {
        let (origin, server) = spawn_responses(vec![
            (market_value(7, None), "200 OK"),
            (json!({}), "500 Error"),
        ]);
        let client = PredictFunClient::for_test(&origin);
        client.markets().await.expect("initial catalog");
        client
            .seed_acceptance_clock(7, 1_700_000_000_000)
            .expect("seed catalog market");
        assert!(client.markets().await.is_err());
        let invalidations = client.drain_invalidations();
        assert_eq!(invalidations.len(), 1);
        assert_eq!(invalidations[0].market_id, 7);
        assert!(!invalidations[0].ready);
        assert_eq!(invalidations[0].exchange_timestamp_ms, None);
        assert!(invalidations[0]
            .readiness_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("HTTP")));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn orderbook_binds_requested_market_and_requires_exchange_clock() {
        let mismatch = valid_book(
            8,
            json!([["0.6", "1"]]),
            json!([["0.4", "1"]]),
            1_700_000_000_000,
        );
        let zero_clock = valid_book(7, json!([["0.6", "1"]]), json!([["0.4", "1"]]), 0);
        let (origin, server) = spawn_responses(vec![(mismatch, "200 OK"), (zero_clock, "200 OK")]);
        let client = PredictFunClient::for_test(&origin);
        assert!(matches!(
            client.orderbook(7).await,
            Err(PredictFunError::MarketIdMismatch {
                requested: 7,
                actual: 8
            })
        ));
        assert!(matches!(
            client.orderbook(7).await,
            Err(PredictFunError::NonPositiveClock {
                field: "exchange_timestamp_ms"
            })
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn stream_invalidates_ready_on_empty_and_error_then_recovers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let ready = valid_book(
            7,
            json!([["0.6", "1"]]),
            json!([["0.4", "1"]]),
            1_700_000_000_000,
        );
        let empty = valid_book(7, json!([]), json!([]), 1_700_000_000_001);
        let fresh = valid_book(
            7,
            json!([["0.61", "1"]]),
            json!([["0.39", "1"]]),
            1_700_000_000_002,
        );
        let server = thread::spawn(move || {
            for (body, status) in [
                (market_value(7, None), "200 OK"),
                (ready, "200 OK"),
                (empty, "200 OK"),
                (json!({}), "500 Error"),
                (fresh, "200 OK"),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1_024];
                let _ = stream.read(&mut request);
                stream.write_all(&response_bytes(&body, status)).unwrap();
            }
        });
        let client = PredictFunClient::for_test(&format!("http://{address}"));
        let binding = PredictFunMarketBinding::new(
            7,
            Symbol::new("yes-token-7"),
            Symbol::new("no-token-7"),
            3,
        )
        .unwrap();
        let stream_adapter =
            PredictFunMarketStream::new(client, vec![binding], Duration::from_millis(1)).unwrap();
        let mut events = stream_adapter
            .subscribe(vec![Symbol::new("yes-token-7"), Symbol::new("no-token-7")])
            .await
            .unwrap();
        let first = timeout(TokioDuration::from_secs(1), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let MarketEvent::Snapshot(snapshot) = first else {
            panic!("expected first ready snapshot");
        };
        assert!(snapshot.timestamps.exchange_event.is_some());
        assert!(snapshot.timestamps.local_receive.is_some());
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Snapshot(_)
        ));
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Disconnect { .. }
        ));
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Disconnect { .. }
        ));
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Snapshot(_)
        ));
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Snapshot(_)
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn stream_rejects_forged_binding_against_fresh_catalog_before_books() {
        let (origin, server) = spawn_responses(vec![
            (market_value(7, None), "200 OK"),
            (market_value(7, None), "200 OK"),
        ]);
        for (yes_token, precision) in [("forged-yes", 3), ("yes-token-7", 2)] {
            let client = PredictFunClient::for_test(&origin);
            let binding = PredictFunMarketBinding::new(
                7,
                Symbol::new(yes_token),
                Symbol::new("no-token-7"),
                precision,
            )
            .unwrap();
            let stream =
                PredictFunMarketStream::new(client, vec![binding], Duration::from_millis(1))
                    .unwrap();
            let error = match stream.subscribe(vec![Symbol::new(yes_token)]).await {
                Ok(_) => panic!("forged binding must be rejected before orderbook polling"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("binding disagrees"));
        }
        server.join().unwrap();
    }

    #[tokio::test]
    async fn multi_market_health_stays_degraded_when_any_selected_market_is_invalid() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let catalog = market_catalog(&[7, 8]);
        let ready = valid_book(
            7,
            json!([["0.6", "1"]]),
            json!([["0.4", "1"]]),
            1_700_000_000_000,
        );
        let server = thread::spawn(move || {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1_024];
                let count = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                let (body, status) = if request.contains("/v1/markets/") {
                    if request.contains("/v1/markets/7/") {
                        (ready.clone(), "200 OK")
                    } else {
                        (json!({}), "500 Error")
                    }
                } else {
                    (catalog.clone(), "200 OK")
                };
                stream.write_all(&response_bytes(&body, status)).unwrap();
            }
        });
        let client = PredictFunClient::for_test(&format!("http://{address}"));
        let bindings = vec![
            PredictFunMarketBinding::new(
                7,
                Symbol::new("yes-token-7"),
                Symbol::new("no-token-7"),
                3,
            )
            .unwrap(),
            PredictFunMarketBinding::new(
                8,
                Symbol::new("yes-token-8"),
                Symbol::new("no-token-8"),
                3,
            )
            .unwrap(),
        ];
        let stream_adapter =
            PredictFunMarketStream::new(client, bindings, Duration::from_millis(1)).unwrap();
        let mut events = stream_adapter
            .subscribe(vec![Symbol::new("yes-token-7"), Symbol::new("yes-token-8")])
            .await
            .unwrap();
        let mut saw_snapshot = false;
        for _ in 0..2 {
            let event = timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            if matches!(event, MarketEvent::Snapshot(_)) {
                saw_snapshot = true;
            }
        }
        assert!(saw_snapshot);
        assert!(!stream_adapter.health().await.connected);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn superseded_delayed_response_cannot_mutate_current_generation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let catalog = market_value(7, None);
        let old_book = valid_book(7, json!([["0.60", "1"]]), json!([["0.40", "1"]]), 100);
        let current_book = valid_book(7, json!([["0.61", "1"]]), json!([["0.39", "1"]]), 50);
        let orderbook_count = StdArc::new(AtomicUsize::new(0));
        let (old_started_tx, mut old_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let (release_old_tx, release_old_rx) = std::sync::mpsc::channel();
        let release_old_rx = StdArc::new(StdMutex::new(release_old_rx));
        let server = thread::spawn(move || {
            let mut workers = Vec::new();
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1_024];
                let count = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]).into_owned();
                let orderbook_count = StdArc::clone(&orderbook_count);
                let old_started_tx = old_started_tx.clone();
                let release_old_rx = StdArc::clone(&release_old_rx);
                let catalog = catalog.clone();
                let old_book = old_book.clone();
                let current_book = current_book.clone();
                workers.push(thread::spawn(move || {
                    let body = if request.contains("/v1/markets/") {
                        if orderbook_count.fetch_add(1, AtomicOrdering::SeqCst) == 0 {
                            old_started_tx.send(()).unwrap();
                            release_old_rx.lock().unwrap().recv().unwrap();
                            old_book
                        } else {
                            current_book
                        }
                    } else {
                        catalog
                    };
                    stream.write_all(&response_bytes(&body, "200 OK")).unwrap();
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });

        let client = PredictFunClient::for_test(&format!("http://{address}"));
        let binding = PredictFunMarketBinding::new(
            7,
            Symbol::new("yes-token-7"),
            Symbol::new("no-token-7"),
            3,
        )
        .unwrap();
        let stream_adapter =
            PredictFunMarketStream::new(client, vec![binding], Duration::from_millis(1)).unwrap();
        let _old_events = stream_adapter
            .subscribe(vec![Symbol::new("yes-token-7")])
            .await
            .unwrap();
        timeout(TokioDuration::from_secs(1), old_started_rx.recv())
            .await
            .unwrap()
            .unwrap();

        let mut current_events = stream_adapter
            .subscribe(vec![Symbol::new("yes-token-7")])
            .await
            .unwrap();
        release_old_tx.send(()).unwrap();
        let current_event = timeout(TokioDuration::from_secs(1), current_events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(current_event, MarketEvent::Snapshot(_)));
        assert!(stream_adapter.health().await.connected);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn stream_rejects_exchange_clock_regression_and_recovers_at_newer_clock() {
        let (origin, server) = spawn_responses(vec![
            (market_value(7, None), "200 OK"),
            (
                valid_book(
                    7,
                    json!([["0.6", "1"]]),
                    json!([["0.4", "1"]]),
                    1_700_000_000_002,
                ),
                "200 OK",
            ),
            (
                valid_book(
                    7,
                    json!([["0.61", "1"]]),
                    json!([["0.39", "1"]]),
                    1_700_000_000_001,
                ),
                "200 OK",
            ),
            (
                valid_book(
                    7,
                    json!([["0.62", "1"]]),
                    json!([["0.38", "1"]]),
                    1_700_000_000_003,
                ),
                "200 OK",
            ),
        ]);
        let client = PredictFunClient::for_test(&origin);
        let binding = PredictFunMarketBinding::new(
            7,
            Symbol::new("yes-token-7"),
            Symbol::new("no-token-7"),
            3,
        )
        .unwrap();
        let stream_adapter =
            PredictFunMarketStream::new(client, vec![binding], Duration::from_millis(1)).unwrap();
        let mut events = stream_adapter
            .subscribe(vec![Symbol::new("yes-token-7")])
            .await
            .unwrap();
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Snapshot(_)
        ));
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Disconnect { .. }
        ));
        assert!(matches!(
            timeout(TokioDuration::from_secs(1), events.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            MarketEvent::Snapshot(_)
        ));
        server.join().unwrap();
    }
}
