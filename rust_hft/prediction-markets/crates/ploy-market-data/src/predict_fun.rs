//! Predict.fun collector sink.
//
// HTTP, pagination, response validation, full-depth projection, and stream
// invalidation live in adapter-predict-fun. This module owns only scheduling
// and append-only database persistence for the existing collector command.

use std::time::Duration;

use adapter_predict_fun_data::{
    validate_api_access, PredictFunBookProjection, PredictFunClient,
    PredictFunError as AdapterError, PredictFunLevel, ReceivedMarket,
};
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use sqlx::PgPool;
use thiserror::Error;
use tokio::time::sleep;
use tracing::{info, warn};

pub const MAINNET_API: &str = adapter_predict_fun_data::MAINNET_API;
pub const TESTNET_API: &str = adapter_predict_fun_data::TESTNET_API;

#[derive(Debug, Error)]
pub enum PredictFunError {
    #[error("Predict.fun mainnet requires PREDICT_FUN_API_KEY")]
    MissingMainnetApiKey,
    #[error("unsupported Predict.fun API origin: {0}")]
    UnsupportedApiOrigin(String),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error("Predict.fun persistence failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Predict.fun time conversion failed: {0}")]
    Time(String),
}

#[derive(Clone)]
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
            std::env::var("PREDICT_FUN_API_URL").unwrap_or_else(|_| MAINNET_API.to_owned());
        let api_key = std::env::var("PREDICT_FUN_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let api_key = api_key.map(SecretString::from);
        validate_api_access(
            &base_url,
            api_key
                .as_ref()
                .map(ExposeSecret::expose_secret)
                .map(String::as_str),
        )
        .map_err(map_config_error)?;
        Ok(Self {
            base_url,
            api_key,
            refresh_interval_secs: env_positive_u64("PLOY_PREDICT_FUN_REFRESH_SECS", 30),
            per_market_delay_ms: env_positive_u64("PLOY_PREDICT_FUN_MARKET_DELAY_MS", 300),
            once,
        })
    }
}

fn map_config_error(error: AdapterError) -> PredictFunError {
    match error {
        AdapterError::MissingApiKey => PredictFunError::MissingMainnetApiKey,
        AdapterError::UnsupportedOrigin(origin) => PredictFunError::UnsupportedApiOrigin(origin),
        other => PredictFunError::Adapter(other),
    }
}

pub async fn run_collector(config: PredictFunConfig, pool: PgPool) -> Result<(), PredictFunError> {
    let client = PredictFunClient::new(config.base_url.clone(), config.api_key.clone())?;
    restore_acceptance_state(&client, &pool).await?;
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
    let markets = match client.markets().await {
        Ok(markets) => markets,
        Err(error) => {
            persist_invalidations(pool, client.drain_invalidations()).await?;
            return Err(error.into());
        }
    };
    persist_invalidations(pool, client.drain_invalidations()).await?;
    let mut books = 0usize;
    let mut attempted_books = 0usize;
    for received_market in &markets {
        persist_market(pool, received_market).await?;
        if !received_market.market.is_collectible() {
            continue;
        }
        let binding = match received_market.market.binary_binding() {
            Ok(binding) => binding,
            Err(error) => {
                warn!(market_id = received_market.market.id, %error, "Predict.fun market has no valid YES/NO binding");
                let projection =
                    client.invalidate_market(received_market.market.id, error.to_string())?;
                persist_book(pool, &projection).await?;
                continue;
            }
        };
        attempted_books += 1;
        match client
            .orderbook(binding.market_id())
            .await
            .and_then(|received| client.accept_orderbook(&received, binding.decimal_precision()))
        {
            Ok(projection) => {
                persist_book(pool, &projection).await?;
                books += 1;
            }
            Err(error) => {
                warn!(market_id = binding.market_id(), %error, "Predict.fun orderbook fetch failed");
                let projection =
                    client.invalidate_market(binding.market_id(), error.to_string())?;
                persist_book(pool, &projection).await?;
            }
        }
        sleep(Duration::from_millis(config.per_market_delay_ms)).await;
    }
    if attempted_books > 0 && books == 0 {
        return Err(PredictFunError::Adapter(AdapterError::Api {
            path: "/v1/markets/{id}/orderbook".to_owned(),
        }));
    }
    info!(
        markets = markets.len(),
        books, "Predict.fun collection pass complete"
    );
    Ok(())
}

async fn persist_invalidations(
    pool: &PgPool,
    projections: Vec<PredictFunBookProjection>,
) -> Result<(), PredictFunError> {
    for projection in projections {
        persist_book(pool, &projection).await?;
    }
    Ok(())
}

async fn restore_acceptance_state(
    client: &PredictFunClient,
    pool: &PgPool,
) -> Result<(), PredictFunError> {
    let rows = sqlx::query_as::<_, (i64, Option<i64>)>(
        "SELECT market_id, MAX(exchange_timestamp_ms)\
         FROM predict_fun_orderbook_ticks\
         WHERE exchange_timestamp_ms IS NOT NULL\
         GROUP BY market_id",
    )
    .fetch_all(pool)
    .await?;
    for (market_id, timestamp_ms) in rows {
        let Some(timestamp_ms) = timestamp_ms else {
            continue;
        };
        let timestamp_ms = u64::try_from(timestamp_ms).map_err(|_| {
            PredictFunError::Time("stored exchange timestamp is negative".to_owned())
        })?;
        client.seed_acceptance_clock(market_id, timestamp_ms)?;
    }
    Ok(())
}

async fn persist_market(pool: &PgPool, received: &ReceivedMarket) -> Result<(), PredictFunError> {
    let observed_at = timestamp_to_utc(received.received_at_us)?;
    let outcomes = serde_json::to_value(&received.market.outcomes)
        .map_err(|error| PredictFunError::Time(error.to_string()))?;
    sqlx::query(
        r#"
        INSERT INTO predict_fun_markets (
            market_id, condition_id, title, question, description,
            decimal_precision, trading_status, status, is_visible, is_neg_risk,
            is_yield_bearing, fee_rate_bps, outcomes, resolution, raw, observed_at
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16
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
            raw = EXCLUDED.raw,
            observed_at = EXCLUDED.observed_at
        "#,
    )
    .bind(received.market.id)
    .bind(&received.market.condition_id)
    .bind(&received.market.title)
    .bind(&received.market.question)
    .bind(&received.market.description)
    .bind(i32::try_from(received.market.decimal_precision).unwrap_or(i32::MAX))
    .bind(&received.market.trading_status)
    .bind(&received.market.status)
    .bind(received.market.is_visible)
    .bind(received.market.is_neg_risk)
    .bind(received.market.is_yield_bearing)
    .bind(received.market.fee_rate_bps)
    .bind(outcomes)
    .bind(&received.market.resolution)
    .bind(&received.raw)
    .bind(observed_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn persist_book(
    pool: &PgPool,
    projection: &PredictFunBookProjection,
) -> Result<(), PredictFunError> {
    let plan = book_persistence_plan(projection)?;
    let (yes_bid, yes_bid_size) = top_level(&projection.yes_bids);
    let (yes_ask, yes_ask_size) = top_level_ask(&projection.yes_asks);
    let (no_bid, no_bid_size) = top_level(&projection.no_bids);
    let (no_ask, no_ask_size) = top_level_ask(&projection.no_asks);
    sqlx::query(
        r#"
        INSERT INTO predict_fun_orderbook_ticks (
            market_id, exchange_timestamp_ms,
            best_yes_bid, best_yes_bid_size, best_yes_ask, best_yes_ask_size,
            best_no_bid, best_no_bid_size, best_no_ask, best_no_ask_size,
            received_at, ready, readiness_reason, yes_bids, yes_asks, no_bids, no_asks, raw
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18
        )
        "#,
    )
    .bind(plan.market_id)
    .bind(plan.exchange_timestamp_ms)
    .bind(yes_bid)
    .bind(yes_bid_size)
    .bind(yes_ask)
    .bind(yes_ask_size)
    .bind(no_bid)
    .bind(no_bid_size)
    .bind(no_ask)
    .bind(no_ask_size)
    .bind(plan.received_at)
    .bind(plan.ready)
    .bind(&plan.readiness_reason)
    .bind(serde_json::to_value(&projection.yes_bids).unwrap_or(Value::Null))
    .bind(serde_json::to_value(&projection.yes_asks).unwrap_or(Value::Null))
    .bind(serde_json::to_value(&projection.no_bids).unwrap_or(Value::Null))
    .bind(serde_json::to_value(&projection.no_asks).unwrap_or(Value::Null))
    .bind(&projection.raw)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct BookPersistencePlan {
    market_id: i64,
    exchange_timestamp_ms: Option<i64>,
    received_at: DateTime<Utc>,
    ready: bool,
    readiness_reason: Option<String>,
}

fn book_persistence_plan(
    projection: &PredictFunBookProjection,
) -> Result<BookPersistencePlan, PredictFunError> {
    let exchange_timestamp_ms = projection
        .exchange_timestamp_ms
        .map(|timestamp| {
            i64::try_from(timestamp).map_err(|_| {
                PredictFunError::Time("exchange timestamp exceeds PostgreSQL BIGINT".to_owned())
            })
        })
        .transpose()?;
    Ok(BookPersistencePlan {
        market_id: projection.market_id,
        exchange_timestamp_ms,
        received_at: timestamp_to_utc(projection.received_at_us)?,
        ready: projection.ready,
        readiness_reason: projection.readiness_reason.clone(),
    })
}

fn top_level(
    levels: &[PredictFunLevel],
) -> (Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>) {
    levels
        .first()
        .map(|level| (Some(level.price), Some(level.size)))
        .unwrap_or((None, None))
}

fn top_level_ask(
    levels: &[PredictFunLevel],
) -> (Option<rust_decimal::Decimal>, Option<rust_decimal::Decimal>) {
    top_level(levels)
}

fn timestamp_to_utc(micros: u64) -> Result<DateTime<Utc>, PredictFunError> {
    if micros == 0 {
        return Err(PredictFunError::Time(
            "receive timestamp is zero".to_owned(),
        ));
    }
    let seconds = i64::try_from(micros / 1_000_000)
        .map_err(|_| PredictFunError::Time("receive timestamp exceeds i64".to_owned()))?;
    DateTime::from_timestamp(seconds, (micros % 1_000_000) as u32 * 1_000)
        .ok_or_else(|| PredictFunError::Time("receive timestamp is invalid".to_owned()))
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
    use super::*;
    use adapter_predict_fun_data::PredictFunBookProjection;
    use serde_json::json;

    #[test]
    fn mainnet_requires_api_key_but_testnet_does_not() {
        assert!(adapter_predict_fun_data::validate_api_access(TESTNET_API, None).is_ok());
        assert!(adapter_predict_fun_data::validate_api_access(MAINNET_API, None).is_err());
    }

    #[test]
    fn market_binding_preserves_exact_outcome_tokens() {
        let market = adapter_predict_fun_data::PredictFunMarket {
            id: 1,
            title: "t".to_owned(),
            question: "q".to_owned(),
            description: None,
            condition_id: "c".to_owned(),
            decimal_precision: 3,
            trading_status: "OPEN".to_owned(),
            status: "REGISTERED".to_owned(),
            is_visible: true,
            is_neg_risk: false,
            is_yield_bearing: false,
            fee_rate_bps: 0,
            outcomes: vec![
                adapter_predict_fun_data::PredictFunOutcome {
                    name: "Yes".to_owned(),
                    index_set: 1,
                    on_chain_id: "yes-token-without-truncation".to_owned(),
                    status: None,
                },
                adapter_predict_fun_data::PredictFunOutcome {
                    name: "No".to_owned(),
                    index_set: 2,
                    on_chain_id: "no-token-without-truncation".to_owned(),
                    status: None,
                },
            ],
            resolution: None,
        };
        let binding = market.binary_binding().unwrap();
        assert_eq!(binding.yes_token().as_str(), "yes-token-without-truncation");
        assert_eq!(binding.no_token().as_str(), "no-token-without-truncation");
        assert_eq!(market.decimal_precision, 3);
    }

    #[test]
    fn failed_book_persistence_plan_keeps_unknown_exchange_time_and_reason() {
        let projection = PredictFunBookProjection {
            market_id: 77,
            exchange_timestamp_ms: None,
            received_at_us: 1_700_000_000_500_000,
            ready: false,
            readiness_reason: Some("HTTP 503".to_owned()),
            yes_bids: Vec::<PredictFunLevel>::new(),
            yes_asks: Vec::new(),
            no_bids: Vec::new(),
            no_asks: Vec::new(),
            raw: json!({"error": "HTTP 503"}),
        };
        let plan = book_persistence_plan(&projection).expect("failure persistence plan");
        assert_eq!(plan.market_id, 77);
        assert_eq!(plan.exchange_timestamp_ms, None);
        assert!(!plan.ready);
        assert_eq!(plan.readiness_reason.as_deref(), Some("HTTP 503"));
    }

    #[tokio::test]
    #[ignore = "requires the migrated PostgreSQL service in rust-research-heavy"]
    async fn postgres_predict_fun_restart_restores_clock_and_persists_failure_readiness() {
        let database_url = std::env::var("PLOY_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .expect("research-heavy PostgreSQL URL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await
            .expect("connect research-heavy PostgreSQL");
        let market_id = 9_876_543_i64;
        sqlx::query("DELETE FROM predict_fun_orderbook_ticks WHERE market_id = $1")
            .bind(market_id)
            .execute(&pool)
            .await
            .expect("clear test ticks");
        sqlx::query("DELETE FROM predict_fun_markets WHERE market_id = $1")
            .bind(market_id)
            .execute(&pool)
            .await
            .expect("clear test market");
        sqlx::query(
            "INSERT INTO predict_fun_markets (market_id, condition_id, title, question, decimal_precision, trading_status, status, is_visible, is_neg_risk, is_yield_bearing, fee_rate_bps, outcomes) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(market_id)
        .bind("condition-restart")
        .bind("restart")
        .bind("restart")
        .bind(3_i32)
        .bind("OPEN")
        .bind("REGISTERED")
        .bind(true)
        .bind(false)
        .bind(false)
        .bind(0_i32)
        .bind(json!([]))
        .execute(&pool)
        .await
        .expect("insert test market");
        sqlx::query(
            "INSERT INTO predict_fun_orderbook_ticks (market_id, exchange_timestamp_ms, received_at, ready) VALUES ($1,$2,NOW(),TRUE)",
        )
        .bind(market_id)
        .bind(100_i64)
        .execute(&pool)
        .await
        .expect("insert historical ready tick");
        sqlx::query(
            "INSERT INTO predict_fun_orderbook_ticks (market_id, exchange_timestamp_ms, received_at, ready, readiness_reason) VALUES ($1,$2,NOW(),FALSE,$3)",
        )
        .bind(market_id)
        .bind(200_i64)
        .bind("empty bid or ask side")
        .execute(&pool)
        .await
        .expect("insert historical non-ready observation");

        let client = adapter_predict_fun_data::PredictFunClient::new(TESTNET_API.to_owned(), None)
            .expect("testnet client");
        restore_acceptance_state(&client, &pool)
            .await
            .expect("restore acceptance high-water");
        let raw = json!({
            "success": true,
            "data": {
                "marketId": market_id,
                "updateTimestampMs": 150,
                "asks": [["0.60", "1"]],
                "bids": [["0.40", "1"]]
            }
        });
        let received = adapter_predict_fun_data::ReceivedOrderBook {
            requested_market_id: market_id,
            book: serde_json::from_value(raw["data"].clone()).expect("test book"),
            received_at_us: 1_700_000_000_500_000,
            raw,
        };
        assert!(matches!(
            client.accept_orderbook(&received, 3),
            Err(AdapterError::ExchangeClockRegressed { .. })
        ));
        let failure = client
            .invalidate_market(market_id, "clock regression")
            .expect("failure projection");
        persist_book(&pool, &failure)
            .await
            .expect("persist failure evidence");
        let row: (Option<i64>, bool, Option<String>) = sqlx::query_as(
            "SELECT exchange_timestamp_ms, ready, readiness_reason FROM predict_fun_orderbook_ticks WHERE market_id = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(market_id)
        .fetch_one(&pool)
        .await
        .expect("read failure evidence");
        assert_eq!(row.0, None);
        assert!(!row.1);
        assert_eq!(row.2.as_deref(), Some("clock regression"));
        sqlx::query("DELETE FROM predict_fun_orderbook_ticks WHERE market_id = $1")
            .bind(market_id)
            .execute(&pool)
            .await
            .expect("remove test ticks");
        sqlx::query("DELETE FROM predict_fun_markets WHERE market_id = $1")
            .bind(market_id)
            .execute(&pool)
            .await
            .expect("remove test market");
    }
}
