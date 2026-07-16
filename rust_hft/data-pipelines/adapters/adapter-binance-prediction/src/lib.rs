//! Binance Prediction REST order-book market data adapter.
//!
//! Binance's Prediction Trading API exposes REST order books rather than a public websocket.
//! This adapter polls only explicitly configured outcome tokens and never fabricates quotes.

use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;

use async_trait::async_trait;
use binance_sdk::{
    config::ConfigurationRestApi,
    w3w_prediction::{
        rest_api::{QueryOrderBookParams, RestApi},
        W3WPredictionRestApi,
    },
};
use hft_core::{HftError, HftResult, Price, Quantity, Symbol, VenueId};
use ports::{BookLevel, BoxStream, ConnectionHealth, MarketEvent, MarketSnapshot, MarketStream};
use rust_decimal::Decimal;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::time::{interval, MissedTickBehavior};

const EVENT_QUEUE_CAPACITY: usize = 1_024;
const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

#[derive(Debug, Clone, Deserialize)]
pub struct PredictionOutcomeConfig {
    pub token_id: String,
    pub market_id: i64,
    #[serde(default = "default_vendor")]
    pub vendor: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BinancePredictionMarketDataConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub api_secret: String,
    #[serde(default = "default_rest_base_url")]
    pub rest_base_url: String,
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    pub outcomes: Vec<PredictionOutcomeConfig>,
}

#[derive(Default)]
struct ConnectionState {
    connected: AtomicBool,
    last_heartbeat: AtomicU64,
}

pub struct BinancePredictionMarketStream {
    config: BinancePredictionMarketDataConfig,
    state: Arc<ConnectionState>,
}

impl BinancePredictionMarketStream {
    pub fn new(config: BinancePredictionMarketDataConfig) -> HftResult<Self> {
        if config.api_key.trim().is_empty() || config.api_secret.trim().is_empty() {
            return Err(HftError::Authentication(
                "Binance Prediction market data requires API credentials".to_string(),
            ));
        }
        if config.outcomes.is_empty() {
            return Err(HftError::Config(
                "Binance Prediction market data requires at least one configured outcome"
                    .to_string(),
            ));
        }
        if config.poll_interval_ms == 0 {
            return Err(HftError::Config(
                "Binance Prediction poll_interval_ms must be positive".to_string(),
            ));
        }
        if config.outcomes.iter().any(|outcome| {
            outcome.token_id.trim().is_empty()
                || outcome.market_id <= 0
                || outcome.vendor.trim().is_empty()
        }) {
            return Err(HftError::Config(
                "Binance Prediction outcomes require token_id, positive market_id, and vendor"
                    .to_string(),
            ));
        }
        let unique_token_ids: HashSet<_> = config
            .outcomes
            .iter()
            .map(|outcome| outcome.token_id.as_str())
            .collect();
        if unique_token_ids.len() != config.outcomes.len() {
            return Err(HftError::Config(
                "Binance Prediction outcome token_id values must be unique".to_string(),
            ));
        }
        Ok(Self {
            config,
            state: Arc::new(ConnectionState::default()),
        })
    }

    fn api(&self) -> HftResult<RestApi> {
        let config = ConfigurationRestApi::builder()
            .api_key(self.config.api_key.clone())
            .api_secret(self.config.api_secret.clone())
            .base_path(self.config.rest_base_url.clone())
            .timeout(self.config.poll_interval_ms.max(1_000))
            .keep_alive(true)
            .retries(1)
            .build()
            .map_err(|error| HftError::Config(error.to_string()))?;
        Ok(W3WPredictionRestApi::from_config(config))
    }
}

fn default_vendor() -> String {
    "predict_fun".to_string()
}
fn default_rest_base_url() -> String {
    "https://api.binance.com".to_string()
}
const fn default_poll_interval_ms() -> u64 {
    DEFAULT_POLL_INTERVAL_MS
}

fn timestamp_micros(timestamp_ms: Option<i64>) -> HftResult<u64> {
    let timestamp = timestamp_ms.ok_or_else(|| {
        HftError::Parse("Binance Prediction order-book timestamp is missing".to_string())
    })?;
    u64::try_from(timestamp)
        .ok()
        .and_then(|value| value.checked_mul(1_000))
        .ok_or_else(|| HftError::Parse("Binance Prediction timestamp is invalid".to_string()))
}

fn parse_levels(
    rows: impl IntoIterator<Item = (Option<String>, Option<String>)>,
    side: &str,
) -> HftResult<Vec<BookLevel>> {
    rows.into_iter()
        .map(|(price, size)| {
            level(
                price.as_deref().ok_or_else(|| {
                    HftError::Parse(format!("Binance Prediction {side} is missing price"))
                })?,
                size.as_deref().ok_or_else(|| {
                    HftError::Parse(format!("Binance Prediction {side} is missing size"))
                })?,
            )
        })
        .collect()
}

fn level(price: &str, size: &str) -> HftResult<BookLevel> {
    let price = price.parse::<Decimal>().map_err(|error| {
        HftError::Parse(format!(
            "Binance Prediction order-book price is invalid: {error}"
        ))
    })?;
    let size = size.parse::<Decimal>().map_err(|error| {
        HftError::Parse(format!(
            "Binance Prediction order-book size is invalid: {error}"
        ))
    })?;
    if !(Decimal::ZERO..=Decimal::ONE).contains(&price) || size < Decimal::ZERO {
        return Err(HftError::Parse(
            "Binance Prediction order-book level is out of range".to_string(),
        ));
    }
    Ok(BookLevel {
        price: Price(price),
        quantity: Quantity(size),
    })
}

fn snapshot_from_levels(
    symbol: Symbol,
    sequence: u64,
    timestamp_ms: Option<i64>,
    mut bids: Vec<BookLevel>,
    mut asks: Vec<BookLevel>,
) -> HftResult<MarketSnapshot> {
    if bids.is_empty() || asks.is_empty() {
        return Err(HftError::Parse(
            "Binance Prediction order book is missing a bid or ask side".to_string(),
        ));
    }
    bids.sort_by_key(|level| std::cmp::Reverse(level.price));
    asks.sort_by_key(|level| level.price);
    Ok(MarketSnapshot {
        symbol,
        timestamp: timestamp_micros(timestamp_ms)?,
        bids,
        asks,
        sequence,
        source_venue: Some(VenueId::BINANCE_PREDICTION),
    })
}

#[async_trait]
impl MarketStream for BinancePredictionMarketStream {
    async fn subscribe(&self, symbols: Vec<Symbol>) -> HftResult<BoxStream<MarketEvent>> {
        if symbols.is_empty() {
            return Err(HftError::Config(
                "Binance Prediction requires at least one configured outcome token".to_string(),
            ));
        }
        let outcomes: HashMap<_, _> = self
            .config
            .outcomes
            .iter()
            .map(|outcome| (outcome.token_id.as_str(), outcome.clone()))
            .collect();
        let selected: Vec<_> = symbols
            .into_iter()
            .map(|symbol| {
                let token_id = symbol.as_str().to_string();
                let outcome = outcomes.get(token_id.as_str()).cloned().ok_or_else(|| {
                    HftError::Config(format!(
                        "Binance Prediction token {token_id} is missing from data_config.outcomes"
                    ))
                })?;
                Ok((symbol, outcome))
            })
            .collect::<HftResult<_>>()?;
        let api = self.api()?;
        let state = Arc::clone(&self.state);
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
        tokio::spawn(async move {
            let mut sequence = 0_u64;
            let mut ticker = interval(poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                for (symbol, outcome) in &selected {
                    let params = match QueryOrderBookParams::builder(
                        outcome.vendor.clone(),
                        outcome.market_id,
                        outcome.token_id.clone(),
                    )
                    .build()
                    {
                        Ok(params) => params,
                        Err(error) => {
                            let _ = tx.send(Err(HftError::Config(error.to_string()))).await;
                            continue;
                        }
                    };
                    let response = match api.query_order_book(params).await {
                        Ok(response) => response
                            .data()
                            .await
                            .map_err(|error| HftError::Exchange(error.to_string())),
                        Err(error) => Err(HftError::Exchange(error.to_string())),
                    };
                    match response {
                        Ok(book) => {
                            let bids = parse_levels(
                                book.bids
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|row| (row.price, row.size)),
                                "bid",
                            );
                            let asks = parse_levels(
                                book.asks
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|row| (row.price, row.size)),
                                "ask",
                            );
                            match bids.and_then(|bids| {
                                asks.and_then(|asks| {
                                    snapshot_from_levels(
                                        symbol.clone(),
                                        sequence.saturating_add(1),
                                        book.timestamp,
                                        bids,
                                        asks,
                                    )
                                })
                            }) {
                                Ok(snapshot) => {
                                    sequence = sequence.saturating_add(1);
                                    state.connected.store(true, Ordering::SeqCst);
                                    state
                                        .last_heartbeat
                                        .store(hft_core::now_micros(), Ordering::SeqCst);
                                    if tx.send(Ok(MarketEvent::Snapshot(snapshot))).await.is_err() {
                                        return;
                                    }
                                }
                                Err(error) => {
                                    let _ = tx.send(Err(error)).await;
                                }
                            }
                        }
                        Err(error) => {
                            state.connected.store(false, Ordering::SeqCst);
                            if tx.send(Err(error)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        });
        Ok(Box::pin(futures::stream::poll_fn(move |cx| {
            rx.poll_recv(cx)
        })))
    }

    async fn health(&self) -> ConnectionHealth {
        ConnectionHealth {
            connected: self.state.connected.load(Ordering::SeqCst),
            latency_ms: None,
            last_heartbeat: self.state.last_heartbeat.load(Ordering::SeqCst),
        }
    }
    async fn connect(&mut self) -> HftResult<()> {
        Ok(())
    }
    async fn disconnect(&mut self) -> HftResult<()> {
        self.state.connected.store(false, Ordering::SeqCst);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_prediction_order_book_into_a_snapshot() {
        let snapshot = snapshot_from_levels(
            Symbol::new("token-yes"),
            1,
            Some(1_750_000_000_000),
            vec![level("0.42", "10").unwrap()],
            vec![level("0.55", "8").unwrap()],
        )
        .unwrap();
        assert_eq!(snapshot.symbol.as_str(), "token-yes");
        assert_eq!(snapshot.source_venue, Some(VenueId::BINANCE_PREDICTION));
        assert_eq!(snapshot.bids[0].price.to_string(), "0.42");
        assert_eq!(snapshot.asks[0].price.to_string(), "0.55");
    }

    #[test]
    fn rejects_one_sided_order_books() {
        assert!(snapshot_from_levels(
            Symbol::new("token-yes"),
            1,
            Some(1_750_000_000_000),
            vec![level("0.42", "10").unwrap()],
            vec![],
        )
        .is_err());
    }

    #[test]
    fn rejects_empty_or_uncredentialed_config() {
        let config = BinancePredictionMarketDataConfig {
            api_key: String::new(),
            api_secret: String::new(),
            rest_base_url: default_rest_base_url(),
            poll_interval_ms: 1_000,
            outcomes: vec![],
        };
        assert!(BinancePredictionMarketStream::new(config).is_err());
    }

    #[test]
    fn rejects_duplicate_outcome_token_ids() {
        let config = BinancePredictionMarketDataConfig {
            api_key: "test-key".to_string(),
            api_secret: "test-secret".to_string(),
            rest_base_url: default_rest_base_url(),
            poll_interval_ms: 1_000,
            outcomes: vec![
                PredictionOutcomeConfig {
                    token_id: "token-yes".to_string(),
                    market_id: 1,
                    vendor: default_vendor(),
                },
                PredictionOutcomeConfig {
                    token_id: "token-yes".to_string(),
                    market_id: 2,
                    vendor: default_vendor(),
                },
            ],
        };
        assert!(BinancePredictionMarketStream::new(config).is_err());
    }

    #[test]
    fn rejects_missing_exchange_timestamp() {
        assert!(snapshot_from_levels(
            Symbol::new("token-yes"),
            1,
            None,
            vec![level("0.42", "10").unwrap()],
            vec![level("0.55", "8").unwrap()],
        )
        .is_err());
    }
}
