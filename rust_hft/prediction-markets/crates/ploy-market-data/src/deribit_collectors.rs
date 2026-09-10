//! Deribit public option reference collectors.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adapter_deribit_data::DeribitReferenceSource;
use chrono::{DateTime, TimeZone, Utc};
use data::deribit_reference::{DeribitGreeksObservation, DeribitIvObservation};
use rust_decimal::Decimal;
use sqlx::PgPool;
use tracing::{error, info, warn};

type CollectorResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, PartialEq)]
struct StoredIvFields {
    raw_mark_iv: Option<Decimal>,
    stored_mark_iv: Option<Decimal>,
    source_iv_unit: String,
    storage_iv_unit: String,
}

fn stored_iv_fields(observation: &DeribitIvObservation) -> StoredIvFields {
    StoredIvFields {
        raw_mark_iv: observation.mark_iv_raw,
        stored_mark_iv: observation.mark_iv,
        source_iv_unit: observation.source_iv_unit.clone(),
        storage_iv_unit: observation.iv_unit.clone(),
    }
}

fn same_deribit_content(
    existing_raw: &serde_json::Value,
    existing_canonical: Option<&serde_json::Value>,
    raw: &serde_json::Value,
    canonical: &serde_json::Value,
) -> bool {
    existing_raw == raw
        && existing_canonical.is_some_and(|existing| {
            canonical_without_receipt(existing) == canonical_without_receipt(canonical)
        })
}

fn canonical_without_receipt(value: &serde_json::Value) -> serde_json::Value {
    let mut canonical = value.clone();
    if let Some(object) = canonical.as_object_mut() {
        object.remove("received_at_us");
    }
    canonical
}

fn running_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(true));
    let clone = Arc::clone(&flag);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        clone.store(false, Ordering::SeqCst);
    });
    flag
}

fn parse_currencies(raw: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.split(',')
        .map(|value| value.trim().to_ascii_uppercase())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

pub async fn collect_deribit_iv(pool: PgPool, currencies_raw: &str, poll_secs: u64) {
    let currencies = parse_currencies(currencies_raw);
    let running = running_flag();
    let source = match DeribitReferenceSource::production(Duration::from_secs(20)) {
        Ok(source) => source,
        Err(error) => {
            error!("[deribit-iv] reference source configuration failed: {error}");
            return;
        }
    };
    let poll_secs = poll_secs.max(1);
    while running.load(Ordering::SeqCst) {
        let started = Instant::now();
        for currency in &currencies {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            if let Err(error) = fetch_and_store_iv(&source, &pool, currency).await {
                error!(%error, currency, "[deribit-iv] collection failed");
            }
        }
        let elapsed = started.elapsed();
        if elapsed < Duration::from_secs(poll_secs) {
            tokio::time::sleep(Duration::from_secs(poll_secs) - elapsed).await;
        }
    }
}

async fn fetch_and_store_iv(
    source: &DeribitReferenceSource,
    pool: &PgPool,
    currency: &str,
) -> CollectorResult<()> {
    let batch = source.option_iv(currency).await?;
    for observation in &batch.observations {
        persist_iv(pool, observation).await?;
    }
    info!(
        currency,
        rows = batch.observations.len(),
        received_at_us = batch.received_at_us,
        "[deribit-iv] canonical batch persisted"
    );
    Ok(())
}

pub async fn collect_deribit_greeks(pool: PgPool, currencies_raw: &str, poll_secs: u64) {
    let currencies = parse_currencies(currencies_raw);
    let running = running_flag();
    let source = match DeribitReferenceSource::production(Duration::from_secs(20)) {
        Ok(source) => source,
        Err(error) => {
            error!("[deribit-greeks] reference source configuration failed: {error}");
            return;
        }
    };
    let poll_secs = poll_secs.max(1);
    while running.load(Ordering::SeqCst) {
        let started = Instant::now();
        for currency in &currencies {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            if let Err(error) = pick_and_fetch_greeks(&source, &pool, currency).await {
                error!(%error, currency, "[deribit-greeks] collection failed");
            }
        }
        let elapsed = started.elapsed();
        if elapsed < Duration::from_secs(poll_secs) {
            tokio::time::sleep(Duration::from_secs(poll_secs) - elapsed).await;
        }
    }
}

async fn pick_and_fetch_greeks(
    source: &DeribitReferenceSource,
    pool: &PgPool,
    currency: &str,
) -> CollectorResult<()> {
    let instrument: Option<(String,)> = sqlx::query_as(
        r#"
        WITH candidates AS (
            SELECT instrument_name,
                   strike,
                   underlying_price,
                   abs(strike - underlying_price) AS atm_distance,
                   fetched_at,
                   source_timestamp_ms
            FROM deribit_iv_ticks
            WHERE upper(currency) = $1
              AND fetched_at >= NOW() - INTERVAL '10 minutes'
              AND fetched_at <= NOW()
              AND iv_unit = 'decimal_fraction'
              AND strike IS NOT NULL
              AND source_timestamp_ms IS NOT NULL
              AND underlying_price IS NOT NULL
              AND instrument_name ~ '^[^-]+-[0-9]{1,2}[A-Z]{3}[0-9]{2}-[0-9]+(\.[0-9]+)?-[CP]$'
            ORDER BY fetched_at DESC, source_timestamp_ms DESC
            LIMIT 500
        )
        SELECT instrument_name
        FROM candidates
        ORDER BY atm_distance ASC, fetched_at DESC, source_timestamp_ms DESC
        LIMIT 1
        "#,
    )
    .bind(currency)
    .fetch_optional(pool)
    .await?;
    let Some((instrument_name,)) = instrument else {
        warn!(currency, "[deribit-greeks] no typed recent IV instrument");
        return Ok(());
    };
    let observation = source
        .option_greeks(currency, &instrument_name)
        .await?
        .observation;
    persist_greeks(pool, &observation).await
}

async fn persist_iv(pool: &PgPool, observation: &DeribitIvObservation) -> CollectorResult<()> {
    // `creation_ts` is the legacy source-clock column. Instrument creation
    // metadata is a separate optional identity field and is not available in
    // these market-data responses.
    let creation_ts = observation
        .source_timestamp_ms
        .map(millis_to_utc)
        .transpose()?;
    let expiry_ts = millis_to_utc(observation.identity.expiry_timestamp_ms)?;
    let canonical = serde_json::to_value(observation)?;
    let iv = stored_iv_fields(observation);
    debug_assert_eq!(iv.raw_mark_iv, observation.mark_iv_raw);
    let inserted: Option<(i64,)> = sqlx::query_as(
        r#"
        INSERT INTO deribit_iv_ticks (
            currency, instrument_name, creation_ts, expiry_ts,
            mark_iv, bid_iv, ask_iv,
            underlying_price, index_price, mark_price,
            best_bid_price, best_ask_price, open_interest, volume,
            payload, fetched_at, strike, option_type, source_iv_unit,
            iv_unit, source_timestamp_ms, canonical_observation
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15::jsonb,$16,
            $17,$18,$19,$20,$21,$22::jsonb
        )
        ON CONFLICT (
            currency, instrument_name, source_timestamp_ms, fetched_at
        ) WHERE source_timestamp_ms IS NOT NULL
            AND source_iv_unit = 'percent_points'
            AND iv_unit = 'decimal_fraction'
        DO NOTHING
        RETURNING 1::bigint
        "#,
    )
    .bind(&observation.identity.currency)
    .bind(&observation.identity.instrument_name)
    .bind(creation_ts)
    .bind(expiry_ts)
    .bind(observation.mark_iv)
    .bind(observation.bid_iv)
    .bind(observation.ask_iv)
    .bind(observation.underlying_price)
    .bind(observation.index_price)
    .bind(observation.mark_price)
    .bind(observation.best_bid_price)
    .bind(observation.best_ask_price)
    .bind(observation.open_interest)
    .bind(observation.volume)
    .bind(&observation.raw)
    .bind(micros_to_utc(observation.received_at_us)?)
    .bind(observation.identity.strike)
    .bind(&observation.identity.option_type)
    .bind(&iv.source_iv_unit)
    .bind(&iv.storage_iv_unit)
    .bind(observation.source_timestamp_ms)
    .bind(&canonical)
    .fetch_optional(pool)
    .await?;
    if inserted.is_some() {
        return Ok(());
    }

    let existing: Option<(serde_json::Value, Option<serde_json::Value>)> = sqlx::query_as(
        r#"
        SELECT payload, canonical_observation
        FROM deribit_iv_ticks
        WHERE currency = $1
          AND instrument_name = $2
          AND source_timestamp_ms = $3
          AND fetched_at = $4
        "#,
    )
    .bind(&observation.identity.currency)
    .bind(&observation.identity.instrument_name)
    .bind(observation.source_timestamp_ms)
    .bind(micros_to_utc(observation.received_at_us)?)
    .fetch_optional(pool)
    .await?;
    match existing {
        Some((existing_raw, existing_canonical))
            if same_deribit_content(
                &existing_raw,
                existing_canonical.as_ref(),
                &observation.raw,
                &canonical,
            ) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "Deribit IV content conflict for {}/{} at source {:?} and receive {}",
            observation.identity.currency,
            observation.identity.instrument_name,
            observation.source_timestamp_ms,
            observation.received_at_us
        )
        .into()),
    }
}

async fn persist_greeks(
    pool: &PgPool,
    observation: &DeribitGreeksObservation,
) -> CollectorResult<()> {
    let source_ts = millis_to_utc(observation.source_timestamp_ms)?;
    let canonical = serde_json::to_value(observation)?;
    let inserted: Option<(i64,)> = sqlx::query_as(
        r#"
        INSERT INTO deribit_atm_greeks_ticks (
            currency, instrument_name, source_ts, fetched_at,
            mark_iv, bid_iv, ask_iv,
            delta, gamma, vega, theta, rho,
            mark_price, underlying_price, index_price,
            best_bid_price, best_ask_price, open_interest, raw,
            strike, option_type, source_iv_unit, iv_unit, canonical_observation
        ) VALUES (
            $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,
            $19::jsonb,$20,$21,$22,$23,$24::jsonb
        )
        ON CONFLICT (currency, instrument_name, source_ts) DO NOTHING
        RETURNING 1::bigint
        "#,
    )
    .bind(&observation.identity.currency)
    .bind(&observation.identity.instrument_name)
    .bind(source_ts)
    .bind(micros_to_utc(observation.received_at_us)?)
    .bind(observation.mark_iv)
    .bind(observation.bid_iv)
    .bind(observation.ask_iv)
    .bind(observation.delta)
    .bind(observation.gamma)
    .bind(observation.vega)
    .bind(observation.theta)
    .bind(observation.rho)
    .bind(observation.mark_price)
    .bind(observation.underlying_price)
    .bind(observation.index_price)
    .bind(observation.best_bid_price)
    .bind(observation.best_ask_price)
    .bind(observation.open_interest)
    .bind(&observation.raw)
    .bind(observation.identity.strike)
    .bind(&observation.identity.option_type)
    .bind(&observation.source_iv_unit)
    .bind(&observation.iv_unit)
    .bind(&canonical)
    .fetch_optional(pool)
    .await?;
    if inserted.is_some() {
        return Ok(());
    }

    let existing: Option<(serde_json::Value, Option<serde_json::Value>)> = sqlx::query_as(
        r#"
        SELECT raw, canonical_observation
        FROM deribit_atm_greeks_ticks
        WHERE currency = $1
          AND instrument_name = $2
          AND source_ts = $3
        "#,
    )
    .bind(&observation.identity.currency)
    .bind(&observation.identity.instrument_name)
    .bind(source_ts)
    .fetch_optional(pool)
    .await?;
    match existing {
        Some((existing_raw, existing_canonical))
            if same_deribit_content(
                &existing_raw,
                existing_canonical.as_ref(),
                &observation.raw,
                &canonical,
            ) =>
        {
            Ok(())
        }
        _ => Err(format!(
            "Deribit Greeks content conflict for {}/{} at source {}",
            observation.identity.currency,
            observation.identity.instrument_name,
            observation.source_timestamp_ms
        )
        .into()),
    }
}

fn millis_to_utc(milliseconds: i64) -> CollectorResult<DateTime<Utc>> {
    if milliseconds <= 0 {
        return Err("Deribit timestamp must be positive".into());
    }
    Utc.timestamp_millis_opt(milliseconds)
        .single()
        .ok_or_else(|| "Deribit timestamp is out of range".into())
}

fn micros_to_utc(micros: u64) -> CollectorResult<DateTime<Utc>> {
    if micros == 0 {
        return Err("Deribit receive timestamp must be positive".into());
    }
    let seconds = i64::try_from(micros / 1_000_000)?;
    DateTime::from_timestamp(seconds, (micros % 1_000_000) as u32 * 1_000)
        .ok_or_else(|| "Deribit receive timestamp is out of range".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::deribit_reference::parse_greeks_result;
    use data::deribit_reference::parse_iv_summary_row;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    #[test]
    fn currencies_are_uppercase_and_deduplicated() {
        assert_eq!(
            parse_currencies("btc, ETH,btc,,SOL"),
            vec!["BTC", "ETH", "SOL"]
        );
    }

    #[test]
    fn sink_keeps_raw_iv_unit_separate_from_decimal_storage() {
        let observation = parse_iv_summary_row(
            &json!({
                "instrument_name":"BTC-6SEP24-50000-C",
                "creation_timestamp":1_700_000_000_000_i64,
                "mark_iv":77.72
            }),
            "BTC",
            1_700_000_001_000_000,
        )
        .unwrap();
        let stored = stored_iv_fields(&observation);
        assert_eq!(
            stored.raw_mark_iv,
            Some(Decimal::from_str_exact("77.72").unwrap())
        );
        assert_eq!(
            stored.stored_mark_iv,
            Some(Decimal::from_str_exact("0.7772").unwrap())
        );
        assert_eq!(stored.source_iv_unit, "percent_points");
        assert_eq!(stored.storage_iv_unit, "decimal_fraction");
    }

    #[test]
    fn repeated_source_content_is_idempotent_but_changed_content_is_rejected() {
        let raw = json!({"mark_iv":77.72});
        let canonical = json!({
            "mark_iv":0.7772,
            "iv_unit":"decimal_fraction",
            "received_at_us":1_700_000_001_000_000_u64
        });
        let replay = json!({
            "mark_iv":0.7772,
            "iv_unit":"decimal_fraction",
            "received_at_us":1_700_000_002_000_000_u64
        });
        assert!(same_deribit_content(&raw, Some(&canonical), &raw, &replay));
        assert!(!same_deribit_content(
            &json!({"mark_iv":77.73}),
            Some(&replay),
            &raw,
            &replay
        ));
        assert!(!same_deribit_content(&raw, None, &raw, &replay));
    }

    #[cfg(feature = "live")]
    #[tokio::test]
    #[ignore = "requires PLOY_TEST_DATABASE_URL and a temporary PostgreSQL fixture"]
    async fn postgres_deribit_persistence_and_pit_contract() {
        let database_url = std::env::var("PLOY_TEST_DATABASE_URL").expect(
            "PLOY_TEST_DATABASE_URL is required for the ignored Deribit PostgreSQL integration test",
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .expect("connect to PostgreSQL fixture");
        sqlx::raw_sql(include_str!(
            "../../../migrations/055_deribit_reference_evidence.sql"
        ))
        .execute(&pool)
        .await
        .expect("apply Deribit evidence migration");

        let iv = parse_iv_summary_row(
            &json!({
                "instrument_name":"BTC-6SEP24-50000-C",
                "creation_timestamp":1_700_000_000_000_i64,
                "mark_iv":77.72,
                "bid_price":0,
                "open_interest":1
            }),
            "BTC",
            1_700_000_001_000_000,
        )
        .unwrap();
        persist_iv(&pool, &iv).await.unwrap();
        persist_iv(&pool, &iv).await.unwrap();

        let first_iv: (DateTime<Utc>, DateTime<Utc>, Decimal, i64, String, String) =
            sqlx::query_as(
                "SELECT fetched_at, creation_ts, mark_iv, source_timestamp_ms, iv_unit, source_iv_unit FROM deribit_iv_ticks WHERE currency = $1 AND instrument_name = $2",
            )
            .bind("BTC")
            .bind("BTC-6SEP24-50000-C")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(first_iv.0.timestamp(), 1_700_000_001);
        assert_eq!(first_iv.1.timestamp(), 1_700_000_000);
        assert_eq!(first_iv.2, Decimal::from_str_exact("0.7772").unwrap());
        assert_eq!(first_iv.3, 1_700_000_000_000);
        assert_eq!(first_iv.4, "decimal_fraction");
        assert_eq!(first_iv.5, "percent_points");
        let iv_count: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM deribit_iv_ticks WHERE currency = $1 AND instrument_name = $2",
        )
        .bind("BTC")
        .bind("BTC-6SEP24-50000-C")
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(iv_count.0, 1);

        let pit_bucket = Utc.timestamp_opt(1_700_000_001, 500_000_000).unwrap();
        let pit_before = Utc.timestamp_opt(1_700_000_000, 500_000_000).unwrap();
        let pit_sql = r#"
            SELECT mark_iv
            FROM deribit_iv_ticks
            WHERE currency = $1
              AND iv_unit = 'decimal_fraction'
              AND fetched_at <= $2
              AND fetched_at > $2 - interval '5 minutes'
              AND source_timestamp_ms <= (extract(epoch FROM $2) * 1000)::bigint
              AND source_timestamp_ms >
                  (extract(epoch FROM ($2 - interval '5 minutes')) * 1000)::bigint
        "#;
        let pit_value: Option<(Decimal,)> = sqlx::query_as(pit_sql)
            .bind("BTC")
            .bind(pit_bucket)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert_eq!(
            pit_value.unwrap().0,
            Decimal::from_str_exact("0.7772").unwrap()
        );
        let before_value: Option<(Decimal,)> = sqlx::query_as(pit_sql)
            .bind("BTC")
            .bind(pit_before)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(before_value.is_none());

        let conflicting_iv = parse_iv_summary_row(
            &json!({
                "instrument_name":"BTC-6SEP24-50000-C",
                "creation_timestamp":1_700_000_000_000_i64,
                "mark_iv":77.73
            }),
            "BTC",
            1_700_000_001_000_000,
        )
        .unwrap();
        assert!(persist_iv(&pool, &conflicting_iv).await.is_err());

        let greeks_result = json!({
            "instrument_name":"BTC-6SEP24-50000-C",
            "timestamp":1_700_000_002_000_i64,
            "mark_iv":77.72,
            "greeks":{"delta":0.5,"theta":-0.1}
        });
        let greeks = parse_greeks_result(
            &greeks_result,
            "BTC",
            "BTC-6SEP24-50000-C",
            1_700_000_003_000_000,
        )
        .unwrap();
        persist_greeks(&pool, &greeks).await.unwrap();
        let first_greeks: (DateTime<Utc>, Decimal, String) = sqlx::query_as(
            "SELECT fetched_at, mark_iv, iv_unit FROM deribit_atm_greeks_ticks WHERE currency = $1 AND instrument_name = $2 AND source_ts = to_timestamp($3)::timestamptz",
        )
        .bind("BTC")
        .bind("BTC-6SEP24-50000-C")
        .bind(1_700_000_002_f64)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(first_greeks.0.timestamp(), 1_700_000_003);
        assert_eq!(first_greeks.1, Decimal::from_str_exact("0.7772").unwrap());
        assert_eq!(first_greeks.2, "decimal_fraction");

        let mut replay = greeks.clone();
        replay.received_at_us += 1_000_000;
        persist_greeks(&pool, &replay).await.unwrap();
        let replay_fetched: (DateTime<Utc>,) = sqlx::query_as(
            "SELECT fetched_at FROM deribit_atm_greeks_ticks WHERE currency = $1 AND instrument_name = $2 AND source_ts = to_timestamp($3)::timestamptz",
        )
        .bind("BTC")
        .bind("BTC-6SEP24-50000-C")
        .bind(1_700_000_002_f64)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(replay_fetched.0, first_greeks.0);
        let greeks_count: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM deribit_atm_greeks_ticks WHERE currency = $1 AND instrument_name = $2 AND source_ts = to_timestamp($3)::timestamptz",
        )
        .bind("BTC")
        .bind("BTC-6SEP24-50000-C")
        .bind(1_700_000_002_f64)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(greeks_count.0, 1);

        let mut conflicting_result = greeks_result;
        conflicting_result["mark_iv"] = json!(77.73);
        let conflicting_greeks = parse_greeks_result(
            &conflicting_result,
            "BTC",
            "BTC-6SEP24-50000-C",
            1_700_000_004_000_000,
        )
        .unwrap();
        assert!(persist_greeks(&pool, &conflicting_greeks).await.is_err());
    }
}
