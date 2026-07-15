//! DuckDB-backed Parquet feed loader.
//!
//! Reads date-partitioned Parquet files from a local data directory and
//! returns a sorted `Vec<MarketUpdate>` for use with [`HistoricalFeed`].
//!
//! # Directory layout expected
//!
//! ```text
//! <data_dir>/
//!   binance_price_ticks/YYYY-MM-DD.parquet
//!   binance_lob_ticks/YYYY-MM-DD.parquet
//!   binance_agg_trade_ticks/YYYY-MM-DD.parquet
//!   clob_quote_ticks/YYYY-MM-DD.parquet
//!   pm_market_metadata/YYYY-MM-DD.parquet
//! ```

use chrono::{DateTime, Duration, Utc};
use ploy_market_contracts::market_update_sort_ts;
use rust_decimal::Decimal;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

use super::options::HistoricalLoadOptions;
use crate::traits::MarketUpdate;

/// How far before `from` to load spot prices for EWMA warm-up.
const WARMUP_MINUTES: i64 = 30;

/// Load historical market updates from date-partitioned Parquet files.
///
/// Returns an empty `Vec` if `data_dir` does not exist.
/// Requires the `parquet-feed` feature to be enabled for actual data loading;
/// without it the function always returns an empty `Vec`.
pub fn load_from_parquet(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, Box<dyn std::error::Error>> {
    if !Path::new(data_dir).exists() {
        return Ok(Vec::new());
    }

    #[cfg(not(feature = "parquet-feed"))]
    {
        let _ = (symbols, from, to, options);
        return Ok(Vec::new());
    }

    #[cfg(feature = "parquet-feed")]
    {
        load_with_duckdb(data_dir, symbols, from, to, options)
    }
}

#[cfg(feature = "parquet-feed")]
fn load_with_duckdb(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, Box<dyn std::error::Error>> {
    use duckdb::Connection;

    // Create spill directory and process one day at a time to keep memory bounded.
    // LOB Parquet files are large; loading all days at once causes OOM.
    std::fs::create_dir_all("/tmp/duckdb_spill").ok();

    let mut updates: Vec<MarketUpdate> = Vec::new();
    let spot_from = from - Duration::minutes(WARMUP_MINUTES);

    // Non-LOB tables: load all at once (small files)
    {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("SET memory_limit='4GB'; SET temp_directory='/tmp/duckdb_spill';")?;
        load_spot_prices(&conn, data_dir, symbols, spot_from, to, &mut updates)?;
        load_agg_trades(&conn, data_dir, symbols, from, to, &mut updates)?;
        load_events(&conn, data_dir, symbols, from, to, &mut updates)?;
        load_pm_quotes(&conn, data_dir, from, to, &mut updates)?;
    }

    // LOB: process one day at a time to avoid OOM
    let mut day = from.date_naive();
    let to_date = to.date_naive();
    while day <= to_date {
        let day_start = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
        let day_end = day
            .succ_opt()
            .unwrap_or(day)
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc();
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("SET memory_limit='4GB'; SET temp_directory='/tmp/duckdb_spill';")?;
        load_l2_data(
            &conn,
            data_dir,
            symbols,
            day_start,
            day_end,
            options.lob_sample_secs,
            &mut updates,
        )?;
        day = day.succ_opt().unwrap_or(day);
    }

    updates.sort_by_key(market_update_sort_ts);

    info!(
        count = updates.len(),
        symbols = ?symbols,
        from = %from,
        to = %to,
        "Loaded historical data from Parquet files",
    );

    Ok(updates)
}

#[cfg(feature = "parquet-feed")]
fn glob_parquet_files(dir: &str, _from: DateTime<Utc>, _to: DateTime<Utc>) -> String {
    // Build a glob pattern covering all dates in [from, to].
    // DuckDB accepts a glob like: 'dir/YYYY-MM-DD.parquet'
    // For simplicity, use a wildcard and let DuckDB filter by timestamp column.
    format!("{dir}/*.parquet")
}

#[cfg(feature = "parquet-feed")]
fn symbol_filter_sql(symbols: &[String]) -> String {
    if symbols.is_empty() {
        return String::new();
    }
    let list = symbols
        .iter()
        .map(|s| format!("'{}'", s.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("AND symbol IN ({list})")
}

#[cfg(feature = "parquet-feed")]
fn load_spot_prices(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/binance_price_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    let sql = format!(
        "SELECT EPOCH_US(trade_time)::BIGINT, symbol, CAST(price AS DOUBLE) \
         FROM read_parquet('{glob}') \
         WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
           AND trade_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY trade_time"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_us, symbol, price_f) = row?;
        let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
        let price = Decimal::try_from(price_f).unwrap_or_default();
        updates.push(MarketUpdate::SpotPrice {
            symbol: Arc::from(symbol),
            price,
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded spot prices from Parquet");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_agg_trades(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/binance_agg_trade_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    // 5-second downsampling: keep first row per (symbol, 5s bucket)
    let sql = format!(
        "SELECT DISTINCT ON (symbol, epoch_ms(trade_time)::BIGINT / 5000) \
             EPOCH_US(trade_time)::BIGINT, symbol, agg_trade_id, \
             CAST(price AS DOUBLE), CAST(quantity AS DOUBLE), is_buyer_maker \
         FROM read_parquet('{glob}') \
         WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
           AND trade_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY symbol, epoch_ms(trade_time)::BIGINT / 5000, trade_time"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, f64>(3)?,
            row.get::<_, f64>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_us, symbol, agg_trade_id, price_f, qty_f, is_buyer_maker) = row?;
        let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
        let price = Decimal::try_from(price_f).unwrap_or_default();
        let quantity = Decimal::try_from(qty_f).unwrap_or_default();
        updates.push(MarketUpdate::AggTrade {
            symbol: Arc::from(symbol),
            agg_trade_id: agg_trade_id as u64,
            price,
            quantity,
            is_buyer_maker,
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded agg trades from Parquet (5s downsampled)");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_l2_data(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/binance_lob_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();
    let bucket_ms = (sample_secs.max(1) as i64) * 1000;

    let sql = format!(
        "SELECT DISTINCT ON (symbol, epoch_ms(event_time)::BIGINT / {bucket_ms}) \
             EPOCH_US(event_time)::BIGINT, symbol, \
             COALESCE(obi_5, 0.0) AS obi, \
             COALESCE(spread_bps, 0) AS spread_bps, \
             COALESCE(bid_volume_5, 0.0) AS bid_depth_near, \
             COALESCE(ask_volume_5, 0.0) AS ask_depth_near \
         FROM read_parquet('{glob}') \
         WHERE event_time >= TIMESTAMPTZ '{from_str}' \
           AND event_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY symbol, epoch_ms(event_time)::BIGINT / {bucket_ms}, event_time DESC"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, f64>(4)?,
            row.get::<_, f64>(5)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_us, symbol, obi, spread_bps, bid_depth_near, ask_depth_near) = row?;
        let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
        let spread_bps = spread_bps as u32;
        let sym: Arc<str> = Arc::from(symbol);
        updates.push(MarketUpdate::L2 {
            symbol: Arc::clone(&sym),
            obi,
            spread_bps,
            ts,
        });
        updates.push(MarketUpdate::L2Depth {
            symbol: sym,
            obi,
            spread_bps,
            bid_depth_near,
            ask_depth_near,
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded L2 rows from Parquet");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_pm_quotes(
    conn: &duckdb::Connection,
    data_dir: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/clob_quote_ticks");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    let sql = format!(
        "SELECT EPOCH_US(received_at)::BIGINT, token_id, \
                CAST(best_bid AS DOUBLE), CAST(best_ask AS DOUBLE), \
                CAST(bid_size AS DOUBLE), CAST(ask_size AS DOUBLE) \
         FROM read_parquet('{glob}') \
         WHERE received_at >= TIMESTAMPTZ '{from_str}' \
           AND received_at <= TIMESTAMPTZ '{to_str}' \
           AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99) \
         ORDER BY received_at"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, Option<f64>>(5)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (ts_us, token_id, bid_f, ask_f, bid_size_f, ask_size_f) = row?;
        let ts = DateTime::from_timestamp_micros(ts_us).unwrap_or_default();
        let bid = bid_f.and_then(|f| Decimal::try_from(f).ok());
        let ask = ask_f.and_then(|f| Decimal::try_from(f).ok());
        let bid_size = bid_size_f.and_then(|f| Decimal::try_from(f).ok());
        let ask_size = ask_size_f.and_then(|f| Decimal::try_from(f).ok());
        updates.push(MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid,
            ask,
            bid_size,
            ask_size,
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts,
        });
        count += 1;
    }
    info!(count, "Loaded PM quotes from Parquet");
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn load_events(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = format!("{data_dir}/pm_market_metadata");
    if !Path::new(&dir).exists() {
        return Ok(());
    }
    let glob = glob_parquet_files(&dir, from, to);
    let sym_filter = symbol_filter_sql(symbols);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();

    let sql = format!(
        "SELECT market_slug, symbol, \
                EPOCH_US(start_time)::BIGINT, EPOCH_US(end_time)::BIGINT, \
                CAST(price_to_beat AS DOUBLE), \
                json_extract_string(raw_market, '$.markets[0].clobTokenIds[0]') AS up_token_id, \
                json_extract_string(raw_market, '$.markets[0].clobTokenIds[1]') AS down_token_id \
         FROM read_parquet('{glob}') \
         WHERE end_time >= TIMESTAMPTZ '{from_str}' \
           AND start_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
         ORDER BY start_time"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<f64>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, Option<String>>(6)?,
        ))
    })?;

    let mut count = 0usize;
    for row in rows {
        let (
            market_slug,
            symbol_opt,
            start_us,
            end_us,
            price_to_beat_f,
            up_token_opt,
            down_token_opt,
        ) = row?;

        let symbol = match symbol_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let end_time = match end_us {
            Some(us) => DateTime::from_timestamp_micros(us).unwrap_or_default(),
            None => continue,
        };
        let start_time = match start_us {
            Some(us) => DateTime::from_timestamp_micros(us).unwrap_or_default(),
            None => continue,
        };
        let up_token = match up_token_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let down_token = match down_token_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let price_to_beat = price_to_beat_f.and_then(|f| Decimal::try_from(f).ok());
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        let event_id: Arc<str> = Arc::from(market_slug);
        updates.push(MarketUpdate::EventDiscovered {
            event_id: Arc::clone(&event_id),
            symbol: Arc::from(symbol),
            up_token: Arc::from(up_token),
            down_token: Arc::from(down_token),
            end_time,
            window_secs,
            price_to_beat,
            resolved_up_won: None,
        });
        updates.push(MarketUpdate::EventExpired {
            event_id,
            end_time,
            resolved_up_won: None,
        });
        count += 1;
    }
    info!(count, "Loaded events from Parquet pm_market_metadata");
    Ok(())
}
