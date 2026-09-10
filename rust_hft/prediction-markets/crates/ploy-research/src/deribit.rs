use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::{DeribitFeatureSnapshot, ResearchSnapshotPhaseTiming};

pub struct DeribitFeatureLoadResult {
    pub snapshots: Vec<DeribitFeatureSnapshot>,
    pub phase_timings: Vec<ResearchSnapshotPhaseTiming>,
}

pub async fn load_deribit_feature_snapshots(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i64,
) -> Vec<DeribitFeatureSnapshot> {
    load_deribit_feature_snapshots_with_timings(pool, symbols, start, end, sample_secs)
        .await
        .snapshots
}

pub async fn load_deribit_feature_snapshots_with_timings(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i64,
) -> DeribitFeatureLoadResult {
    let mut phase_timings = Vec::new();
    let mut currency_set = BTreeSet::new();
    let mut unsupported_symbols = Vec::new();
    for symbol in symbols {
        match symbol_to_deribit_currency(symbol) {
            Some(currency) => {
                currency_set.insert(currency);
            }
            None => unsupported_symbols.push(symbol.clone()),
        }
    }
    if !unsupported_symbols.is_empty() {
        tracing::warn!(
            symbols = ?unsupported_symbols,
            "deribit feature loader skipped unsupported symbols"
        );
    }

    let currencies: Vec<String> = currency_set.into_iter().collect();
    if currencies.is_empty() {
        tracing::warn!("deribit feature loader has no supported currencies");
        return DeribitFeatureLoadResult {
            snapshots: Vec::new(),
            phase_timings,
        };
    }

    let sample_secs = sample_secs.clamp(1, 300) as i32;
    let mut snapshots: BTreeMap<(String, DateTime<Utc>), DeribitFeatureSnapshot> = BTreeMap::new();

    let started = Instant::now();
    if canonical_cache_exists(pool).await {
        let started = Instant::now();
        let cache_rows = load_deribit_cache_rows(pool, &currencies, start, end, sample_secs).await;
        let elapsed_ms = started.elapsed().as_millis();
        tracing::info!(
            rows = cache_rows.len(),
            elapsed_ms,
            "deribit cache snapshot phase complete"
        );
        phase_timings.push(deribit_phase_timing(
            "deribit_cache_snapshots",
            elapsed_ms,
            Some(cache_rows.len()),
        ));
        merge_deribit_rows(&mut snapshots, cache_rows);
    } else {
        phase_timings.push(deribit_phase_timing(
            "deribit_cache_snapshots",
            started.elapsed().as_millis(),
            Some(0),
        ));
        tracing::info!("deribit cache relation not present; using live ATM Greeks table");
    }

    let started = Instant::now();
    let greeks_rows =
        load_deribit_atm_greeks_rows(pool, &currencies, start, end, sample_secs).await;
    let elapsed_ms = started.elapsed().as_millis();
    tracing::info!(
        rows = greeks_rows.len(),
        elapsed_ms,
        "deribit ATM Greeks phase complete"
    );
    phase_timings.push(deribit_phase_timing(
        "deribit_atm_greeks",
        elapsed_ms,
        Some(greeks_rows.len()),
    ));
    merge_deribit_rows(&mut snapshots, greeks_rows);

    if snapshots.is_empty() {
        tracing::warn!(
            "deribit ATM/cache loaders returned no rows; raw IV fallback is disabled by default"
        );
    }

    if snapshots.is_empty() && raw_iv_fallback_enabled() {
        let started = Instant::now();
        let current_iv_rows =
            load_deribit_raw_iv_rows(pool, &currencies, start, end, sample_secs).await;
        let elapsed_ms = started.elapsed().as_millis();
        tracing::warn!(
            rows = current_iv_rows.len(),
            elapsed_ms,
            "deribit raw IV fallback phase complete"
        );
        phase_timings.push(deribit_phase_timing(
            "deribit_raw_iv_fallback",
            elapsed_ms,
            Some(current_iv_rows.len()),
        ));
        merge_deribit_rows(&mut snapshots, current_iv_rows);
    }

    let mut snapshots: Vec<_> = snapshots.into_values().collect();
    snapshots.sort_by_key(|snapshot| (snapshot.symbol.clone(), snapshot.ts));
    DeribitFeatureLoadResult {
        snapshots,
        phase_timings,
    }
}

fn deribit_phase_timing(
    phase: &str,
    elapsed_ms: u128,
    rows: Option<usize>,
) -> ResearchSnapshotPhaseTiming {
    ResearchSnapshotPhaseTiming {
        phase: phase.to_string(),
        elapsed_ms,
        rows,
    }
}

type DeribitRow = (
    String,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    DateTime<Utc>,
);

async fn canonical_cache_exists(pool: &sqlx::PgPool) -> bool {
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT COUNT(*) = 2
        FROM information_schema.columns
        WHERE table_schema = 'strategy_data'
          AND table_name = 'deribit_atm_greeks_snapshots_cache'
          AND column_name IN ('fetched_at', 'iv_unit')
        "#,
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

fn raw_iv_fallback_enabled() -> bool {
    std::env::var("MONDAY_PREDICTION_DERIBIT_RAW_IV_FALLBACK")
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

const DERIBIT_CACHE_ROWS_QUERY: &str = r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.bid_iv::double precision,
            d.ask_iv::double precision,
            d.underlying_price::double precision,
            d.delta::double precision,
            d.gamma::double precision,
            d.vega::double precision,
            d.theta::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, bid_iv, ask_iv, underlying_price, delta, gamma, vega, theta, source_ts
            FROM strategy_data.deribit_atm_greeks_snapshots_cache d
            WHERE d.currency = c.currency
              AND d.source_ts <= b.bucket_ts
              AND d.fetched_at <= b.bucket_ts
              AND d.source_ts > b.bucket_ts - interval '5 minutes'
              AND d.fetched_at > b.bucket_ts - interval '5 minutes'
              AND d.iv_unit = 'decimal_fraction'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.fetched_at DESC, d.source_ts DESC
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#;

const DERIBIT_ATM_GREEKS_ROWS_QUERY: &str = r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.bid_iv::double precision,
            d.ask_iv::double precision,
            d.underlying_price::double precision,
            d.delta::double precision,
            d.gamma::double precision,
            d.vega::double precision,
            d.theta::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, bid_iv, ask_iv, underlying_price, delta, gamma, vega, theta
            FROM deribit_atm_greeks_ticks d
            WHERE d.currency = c.currency
              AND d.source_ts <= b.bucket_ts
              AND d.fetched_at <= b.bucket_ts
              AND d.source_ts > b.bucket_ts - interval '5 minutes'
              AND d.fetched_at > b.bucket_ts - interval '5 minutes'
              AND d.iv_unit = 'decimal_fraction'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.fetched_at DESC, d.source_ts DESC, d.open_interest DESC NULLS LAST
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#;

const DERIBIT_RAW_IV_ROWS_QUERY: &str = r#"
        WITH currencies AS (
            SELECT unnest($1::text[]) AS currency
        ),
        buckets AS (
            SELECT generate_series($2::timestamptz, $3::timestamptz, make_interval(secs => $4::int)) AS bucket_ts
        )
        SELECT
            c.currency,
            d.mark_iv::double precision,
            d.bid_iv::double precision,
            d.ask_iv::double precision,
            d.underlying_price::double precision,
            b.bucket_ts
        FROM currencies c
        CROSS JOIN buckets b
        JOIN LATERAL (
            SELECT mark_iv, bid_iv, ask_iv, underlying_price
            FROM deribit_iv_ticks d
            WHERE d.currency = c.currency
              AND d.fetched_at <= b.bucket_ts
              AND d.fetched_at > b.bucket_ts - interval '5 minutes'
              AND d.source_timestamp_ms <= (extract(epoch FROM b.bucket_ts) * 1000)::bigint
              AND d.source_timestamp_ms >
                  (extract(epoch FROM (b.bucket_ts - interval '5 minutes')) * 1000)::bigint
              AND d.iv_unit = 'decimal_fraction'
              AND d.mark_iv IS NOT NULL
            ORDER BY d.fetched_at DESC, d.creation_ts DESC, d.open_interest DESC NULLS LAST
            LIMIT 1
        ) d ON true
        ORDER BY b.bucket_ts
        "#;

async fn load_deribit_cache_rows(
    pool: &sqlx::PgPool,
    currencies: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i32,
) -> Vec<DeribitRow> {
    match sqlx::query_as(DERIBIT_CACHE_ROWS_QUERY)
        .bind(currencies)
        .bind(start)
        .bind(end)
        .bind(sample_secs)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "deribit cache load failed");
            Vec::new()
        }
    }
}

async fn load_deribit_atm_greeks_rows(
    pool: &sqlx::PgPool,
    currencies: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i32,
) -> Vec<DeribitRow> {
    match sqlx::query_as(DERIBIT_ATM_GREEKS_ROWS_QUERY)
        .bind(currencies)
        .bind(start)
        .bind(end)
        .bind(sample_secs)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "deribit ATM Greeks load failed");
            Vec::new()
        }
    }
}

async fn load_deribit_raw_iv_rows(
    pool: &sqlx::PgPool,
    currencies: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i32,
) -> Vec<DeribitRow> {
    let current_iv_rows: Vec<(
        String,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        DateTime<Utc>,
    )> = match sqlx::query_as(DERIBIT_RAW_IV_ROWS_QUERY)
        .bind(currencies)
        .bind(start)
        .bind(end)
        .bind(sample_secs)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = %err, "deribit iv load failed");
            Vec::new()
        }
    };

    current_iv_rows
        .into_iter()
        .map(
            |(currency, mark_iv, bid_iv, ask_iv, underlying_price, ts)| {
                (
                    currency,
                    mark_iv,
                    bid_iv,
                    ask_iv,
                    underlying_price,
                    None,
                    None,
                    None,
                    None,
                    ts,
                )
            },
        )
        .collect()
}

fn merge_deribit_rows(
    snapshots: &mut BTreeMap<(String, DateTime<Utc>), DeribitFeatureSnapshot>,
    rows: Vec<DeribitRow>,
) {
    for (currency, mark_iv, bid_iv, ask_iv, underlying_price, delta, gamma, vega, theta, ts) in rows
    {
        let symbol = deribit_currency_to_symbol(&currency);
        merge_deribit_snapshot(
            snapshots,
            symbol,
            ts,
            DeribitFeatureSnapshot {
                symbol: String::new(),
                ts,
                mark_iv: mark_iv.unwrap_or(f64::NAN),
                bid_iv: bid_iv.unwrap_or(f64::NAN),
                ask_iv: ask_iv.unwrap_or(f64::NAN),
                underlying_price: underlying_price.unwrap_or(f64::NAN),
                delta: delta.unwrap_or(f64::NAN),
                gamma: gamma.unwrap_or(f64::NAN),
                vega: vega.unwrap_or(f64::NAN),
                theta: theta.unwrap_or(f64::NAN),
            },
        );
    }
}

fn merge_deribit_snapshot(
    snapshots: &mut BTreeMap<(String, DateTime<Utc>), DeribitFeatureSnapshot>,
    symbol: String,
    ts: DateTime<Utc>,
    mut incoming: DeribitFeatureSnapshot,
) {
    incoming.symbol = symbol.clone();
    incoming.ts = ts;
    snapshots
        .entry((symbol, ts))
        .and_modify(|existing| merge_finite_fields(existing, &incoming))
        .or_insert(incoming);
}

fn merge_finite_fields(existing: &mut DeribitFeatureSnapshot, incoming: &DeribitFeatureSnapshot) {
    update_if_finite(&mut existing.mark_iv, incoming.mark_iv);
    update_if_finite(&mut existing.bid_iv, incoming.bid_iv);
    update_if_finite(&mut existing.ask_iv, incoming.ask_iv);
    update_if_finite(&mut existing.underlying_price, incoming.underlying_price);
    update_if_finite(&mut existing.delta, incoming.delta);
    update_if_finite(&mut existing.gamma, incoming.gamma);
    update_if_finite(&mut existing.vega, incoming.vega);
    update_if_finite(&mut existing.theta, incoming.theta);
}

fn update_if_finite(target: &mut f64, value: f64) {
    if value.is_finite() {
        *target = value;
    }
}

fn symbol_to_deribit_currency(symbol: &str) -> Option<String> {
    let upper = symbol.trim().to_ascii_uppercase();
    if upper.starts_with("BTC") {
        Some("BTC".to_string())
    } else if upper.starts_with("ETH") {
        Some("ETH".to_string())
    } else if upper.starts_with("SOL") {
        Some("SOL".to_string())
    } else {
        None
    }
}

fn deribit_currency_to_symbol(currency: &str) -> String {
    match currency.trim().to_ascii_uppercase().as_str() {
        "BTC" => "BTCUSDT".to_string(),
        "ETH" => "ETHUSDT".to_string(),
        "SOL" => "SOLUSDT".to_string(),
        other => format!("{other}USDT"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pit_queries_require_receive_availability_and_typed_iv_units() {
        for query in [
            DERIBIT_CACHE_ROWS_QUERY,
            DERIBIT_ATM_GREEKS_ROWS_QUERY,
            DERIBIT_RAW_IV_ROWS_QUERY,
        ] {
            assert!(query.contains("d.fetched_at <= b.bucket_ts"));
            assert!(query.contains("d.fetched_at > b.bucket_ts - interval '5 minutes'"));
            assert!(query.contains("d.iv_unit = 'decimal_fraction'"));
        }
        for query in [DERIBIT_CACHE_ROWS_QUERY, DERIBIT_ATM_GREEKS_ROWS_QUERY] {
            assert!(query.contains("d.source_ts <= b.bucket_ts"));
            assert!(query.contains("d.source_ts > b.bucket_ts - interval '5 minutes'"));
        }
        assert!(!DERIBIT_RAW_IV_ROWS_QUERY.contains("d.creation_ts <= b.bucket_ts"));
        assert!(DERIBIT_RAW_IV_ROWS_QUERY.contains("d.source_timestamp_ms <="));
    }

    #[test]
    fn deribit_currency_mapping_stays_explicit() {
        assert_eq!(
            symbol_to_deribit_currency("btc-usdt"),
            Some("BTC".to_string())
        );
        assert_eq!(
            symbol_to_deribit_currency("ETHUSDT"),
            Some("ETH".to_string())
        );
        assert_eq!(
            symbol_to_deribit_currency("SOL-USD"),
            Some("SOL".to_string())
        );
        assert_eq!(symbol_to_deribit_currency("DOGEUSDT"), None);
    }
}
