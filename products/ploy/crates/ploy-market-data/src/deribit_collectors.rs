//! Deribit HTTP data collectors — implied volatility ticks and ATM greeks.
//!
//! Both collectors poll the Deribit public REST API periodically and persist
//! normalized rows to PostgreSQL using batched INSERT ... ON CONFLICT upserts.
//!
//! Run via `ploy-runner`:
//!   ploy-runner collect-deribit-iv --currencies BTC,ETH,SOL --poll-secs 30
//!   ploy-runner collect-deribit-greeks --currencies BTC,ETH,SOL --poll-secs 30

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

const DERIBIT_API_BASE: &str = "https://www.deribit.com/api/v2/public";

type SharedRunning = Arc<AtomicBool>;

fn running_flag() -> SharedRunning {
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received, stopping Deribit collector...");
        f.store(false, Ordering::SeqCst);
    });
    flag
}

fn parse_currencies(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

// ---------------------------------------------------------------------------
// IV collector (deribit_iv_ticks)
// ---------------------------------------------------------------------------

/// Collect option book summaries from Deribit `get_book_summary_by_currency`.
///
/// Polls every `poll_secs` seconds for each configured currency and upserts
/// normalized IV ticks into `deribit_iv_ticks`.
pub async fn collect_deribit_iv(pool: PgPool, currencies_raw: &str, poll_secs: u64) {
    let currencies = parse_currencies(currencies_raw);
    let running = running_flag();
    info!(
        "[deribit-iv] Starting collector currencies={:?} poll_secs={}",
        currencies, poll_secs
    );

    while running.load(Ordering::SeqCst) {
        let start = Instant::now();

        for currency in &currencies {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = fetch_and_store_iv(&pool, currency).await {
                error!("[deribit-iv] currency={currency} error: {e}");
            }
        }

        let elapsed = start.elapsed();
        let sleep = if elapsed.as_secs() < poll_secs {
            poll_secs - elapsed.as_secs()
        } else {
            0
        };
        if sleep > 0 {
            tokio::time::sleep(Duration::from_secs(sleep)).await;
        }
    }
    info!("[deribit-iv] Collector stopped");
}

async fn fetch_and_store_iv(
    pool: &PgPool,
    currency: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{DERIBIT_API_BASE}/get_book_summary_by_currency");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .query(&[("currency", currency), ("kind", "option")])
        .timeout(Duration::from_secs(20))
        .send()
        .await?;
    let payload: Value = resp.json().await?;
    let rows = payload["result"].as_array().ok_or("missing result array")?;

    let fetched_at = Utc::now();

    for item in rows {
        let instrument_name = match item["instrument_name"].as_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Parse expiry / strike / option_type from instrument name like "BTC-29MAR24-50000-C"
        let parts: Vec<&str> = instrument_name.split('-').collect();
        let (expiry_ts, _strike, _option_type) = if parts.len() >= 4 {
            (
                parse_deribit_expiry(parts.get(1).unwrap_or(&"")),
                parts.get(2).and_then(|s| s.parse::<Decimal>().ok()),
                parts.get(3).map(|s| s.to_string()),
            )
        } else {
            (None, None, None)
        };

        let creation_ms = item["creation_timestamp"].as_i64();
        let creation_ts = creation_ms.map(|ms| {
            Utc.timestamp_opt(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
                .unwrap()
        });

        let mark_iv_raw = item["mark_iv"].as_f64();
        let bid_iv_raw = item["bid_iv"].as_f64();
        let ask_iv_raw = item["ask_iv"].as_f64();

        let normalize = |raw: Option<f64>| -> Option<Decimal> {
            let v = raw?;
            if v <= 0.0 {
                return None;
            }
            let normalized = if v > 2.0 { v / 100.0 } else { v };
            Decimal::try_from(normalized).ok()
        };

        let underlying_price = item["underlying_price"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());
        let index_price = item["index_price"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());
        let mark_price = item["mark_price"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());
        let best_bid = item["bid_price"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());
        let best_ask = item["ask_price"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());
        let open_interest = item["open_interest"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());
        let volume = item["volume"]
            .as_f64()
            .and_then(|v| Decimal::try_from(v).ok());

        sqlx::query(
            r#"
            INSERT INTO deribit_iv_ticks (
                currency, instrument_name, creation_ts, expiry_ts,
                mark_iv, bid_iv, ask_iv,
                underlying_price, index_price, mark_price,
                best_bid_price, best_ask_price,
                open_interest, volume,
                payload, fetched_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::jsonb, $16)
            ON CONFLICT (currency, instrument_name, creation_ts, fetched_at) DO UPDATE SET
                mark_iv = EXCLUDED.mark_iv,
                bid_iv = EXCLUDED.bid_iv,
                ask_iv = EXCLUDED.ask_iv,
                underlying_price = EXCLUDED.underlying_price,
                index_price = EXCLUDED.index_price,
                mark_price = EXCLUDED.mark_price,
                best_bid_price = EXCLUDED.best_bid_price,
                best_ask_price = EXCLUDED.best_ask_price,
                open_interest = EXCLUDED.open_interest,
                volume = EXCLUDED.volume,
                payload = EXCLUDED.payload
            "#,
        )
        .bind(currency)
        .bind(&instrument_name)
        .bind(creation_ts)
        .bind(expiry_ts)
        .bind(normalize(mark_iv_raw))
        .bind(normalize(bid_iv_raw))
        .bind(normalize(ask_iv_raw))
        .bind(underlying_price)
        .bind(index_price)
        .bind(mark_price)
        .bind(best_bid)
        .bind(best_ask)
        .bind(open_interest)
        .bind(volume)
        .bind(item.to_string())
        .bind(fetched_at)
        .execute(pool)
        .await?;
    }

    info!(
        "[deribit-iv] Fetched {} instruments for currency={}",
        rows.len(),
        currency
    );
    Ok(())
}

/// Parse Deribit expiry codes like "29MAR24" into UTC 08:00 timestamp.
fn parse_deribit_expiry(code: &str) -> Option<DateTime<Utc>> {
    if code.len() < 7 {
        return None;
    }
    let day: u32 = code[0..2].parse().ok()?;
    let mon_str = code[2..5].to_uppercase();
    let year: i32 = code[5..].parse().ok()?;
    let year = if year < 100 { year + 2000 } else { year };

    let month = match mon_str.as_str() {
        "JAN" => 1,
        "FEB" => 2,
        "MAR" => 3,
        "APR" => 4,
        "MAY" => 5,
        "JUN" => 6,
        "JUL" => 7,
        "AUG" => 8,
        "SEP" => 9,
        "OCT" => 10,
        "NOV" => 11,
        "DEC" => 12,
        _ => return None,
    };

    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .map(|d| d.and_hms_opt(8, 0, 0).unwrap())
        .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc))
}

// ---------------------------------------------------------------------------
// ATM Greeks collector (deribit_atm_greeks_ticks)
// ---------------------------------------------------------------------------

/// Collect ATM option greeks by:
///   1. Picking the nearest ATM instrument per currency from `deribit_iv_ticks`
///   2. Calling Deribit `get_order_book` for Greeks
///   3. Upserting into `deribit_atm_greeks_ticks`
pub async fn collect_deribit_greeks(pool: PgPool, currencies_raw: &str, poll_secs: u64) {
    let currencies = parse_currencies(currencies_raw);
    let running = running_flag();
    info!(
        "[deribit-greeks] Starting collector currencies={:?} poll_secs={}",
        currencies, poll_secs
    );

    while running.load(Ordering::SeqCst) {
        let start = Instant::now();

        for currency in &currencies {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = pick_and_fetch_greeks(&pool, currency).await {
                error!("[deribit-greeks] currency={currency} error: {e}");
            }
        }

        let elapsed = start.elapsed();
        let sleep = if elapsed.as_secs() < poll_secs {
            poll_secs - elapsed.as_secs()
        } else {
            0
        };
        if sleep > 0 {
            tokio::time::sleep(Duration::from_secs(sleep)).await;
        }
    }
    info!("[deribit-greeks] Collector stopped");
}

/// Find the ATM instrument for a currency, then fetch its order book for Greeks.
async fn pick_and_fetch_greeks(
    pool: &PgPool,
    currency: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Pick ATM instrument from recent deribit_iv_ticks
    let instrument: Option<(String,)> = sqlx::query_as(
        r#"
        WITH candidates AS (
            SELECT instrument_name,
                   underlying_price,
                   abs(NULLIF(split_part(instrument_name, '-', 3), '')::numeric - underlying_price) AS atm_distance
            FROM deribit_iv_ticks
            WHERE upper(currency) = $1
              AND fetched_at >= NOW() - INTERVAL '10 minutes'
              AND creation_ts IS NOT NULL
              AND underlying_price IS NOT NULL
              AND instrument_name ~ '^[^-]+-[0-9]{1,2}[A-Z]{3}[0-9]{2}-[0-9]+(\.[0-9]+)?-[CP]$'
            ORDER BY fetched_at DESC, creation_ts DESC
            LIMIT 500
        )
        SELECT instrument_name
        FROM candidates
        ORDER BY atm_distance ASC
        LIMIT 1
        "#,
    )
    .bind(currency)
    .fetch_optional(pool)
    .await?;

    let instrument_name = match instrument {
        Some((n,)) => n,
        None => {
            warn!("[deribit-greeks] No recent instruments for {currency}, skipping");
            return Ok(());
        }
    };

    // Fetch order book via Deribit REST API
    let url = format!("{DERIBIT_API_BASE}/get_order_book");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .query(&[("instrument_name", &instrument_name)])
        .timeout(Duration::from_secs(20))
        .send()
        .await?;
    let payload: Value = resp.json().await?;
    let result = payload["result"]
        .as_object()
        .ok_or("missing result object")?;

    let ts_ms = result
        .get("timestamp")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| Utc::now().timestamp_millis());
    let source_ts = Utc
        .timestamp_opt(ts_ms / 1000, ((ts_ms % 1000) * 1_000_000) as u32)
        .unwrap();

    let greeks = result.get("greeks").and_then(|g| g.as_object());

    let to_dec = |key: &str| -> Option<Decimal> {
        result
            .get(key)
            .and_then(|v| v.as_f64())
            .and_then(|f| Decimal::try_from(f).ok())
    };
    let greek_dec = |key: &str| -> Option<Decimal> {
        greeks
            .and_then(|g| g.get(key))
            .and_then(|v| v.as_f64())
            .and_then(|f| Decimal::try_from(f).ok())
    };

    sqlx::query(
        r#"
        INSERT INTO deribit_atm_greeks_ticks (
            currency, instrument_name, source_ts, fetched_at,
            mark_iv, bid_iv, ask_iv,
            delta, gamma, vega, theta, rho,
            mark_price, underlying_price, index_price,
            best_bid_price, best_ask_price, open_interest,
            raw
        ) VALUES ($1, $2, $3, NOW(), $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18::jsonb)
        ON CONFLICT (currency, instrument_name, source_ts) DO UPDATE SET
            fetched_at = NOW(),
            mark_iv = EXCLUDED.mark_iv,
            bid_iv = EXCLUDED.bid_iv,
            ask_iv = EXCLUDED.ask_iv,
            delta = EXCLUDED.delta,
            gamma = EXCLUDED.gamma,
            vega = EXCLUDED.vega,
            theta = EXCLUDED.theta,
            rho = EXCLUDED.rho,
            mark_price = EXCLUDED.mark_price,
            underlying_price = EXCLUDED.underlying_price,
            index_price = EXCLUDED.index_price,
            best_bid_price = EXCLUDED.best_bid_price,
            best_ask_price = EXCLUDED.best_ask_price,
            open_interest = EXCLUDED.open_interest,
            raw = EXCLUDED.raw
        "#,
    )
    .bind(currency)
    .bind(&instrument_name)
    .bind(source_ts)
    .bind(to_dec("mark_iv"))
    .bind(to_dec("bid_iv"))
    .bind(to_dec("ask_iv"))
    .bind(greek_dec("delta"))
    .bind(greek_dec("gamma"))
    .bind(greek_dec("vega"))
    .bind(greek_dec("theta"))
    .bind(greek_dec("rho"))
    .bind(to_dec("mark_price"))
    .bind(to_dec("underlying_price"))
    .bind(to_dec("index_price"))
    .bind(to_dec("best_bid_price"))
    .bind(to_dec("best_ask_price"))
    .bind(to_dec("open_interest"))
    .bind(serde_json::to_string(result).unwrap_or_default())
    .execute(pool)
    .await?;

    info!("[deribit-greeks] Stored Greeks for {instrument_name}");
    Ok(())
}
