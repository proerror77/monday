//! Streaming DuckDB-backed Parquet feed for O(1) memory backtest.
//!
//! Unlike [`super::parquet`] which loads all data into a `Vec<MarketUpdate>`,
//! this feed runs DuckDB in a background thread and streams rows through a
//! bounded `mpsc` channel. Memory usage is O(channel buffer) regardless of
//! date range.
//!
//! The main data tables (spot, agg trades, LOB, quotes) are merged via a single
//! DuckDB `UNION ALL … ORDER BY` query — DuckDB handles the merge sort
//! internally with disk spill, so Rust never holds more than the channel buffer
//! in memory. Events (~11K rows) are loaded separately and merged via a
//! two-pointer technique in the send loop.
//!
//! # Usage
//!
//! ```no_run
//! use ploy_strategy_bundles::feed::{parquet_stream::StreamingParquetFeed, HistoricalLoadOptions};
//! use chrono::Utc;
//!
//! let feed = StreamingParquetFeed::new(
//!     "/data/parquet",
//!     &["BTCUSDT".to_string()],
//!     Utc::now() - chrono::Duration::days(7),
//!     Utc::now(),
//!     &HistoricalLoadOptions::default(),
//! );
//! ```

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

use super::options::HistoricalLoadOptions;
use crate::traits::{Feed, MarketUpdate};

/// Bounded channel capacity — limits how far ahead the background thread runs.
const CHANNEL_CAPACITY: usize = 1000;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum StreamingParquetFeedError {
    #[error("StreamingParquetFeed background worker failed: {0}")]
    Background(String),
}

enum FeedMessage {
    Update(MarketUpdate),
    Error(StreamingParquetFeedError),
}

/// Streaming Parquet feed backed by a DuckDB background thread.
///
/// The background thread executes a single DuckDB `UNION ALL` query across all
/// data tables (spot, LOB, agg trades, quotes) with `ORDER BY ts_us`. DuckDB
/// handles the merge sort internally with disk spill, so Rust never holds more
/// than the channel buffer in memory. Events are loaded separately (~11K rows)
/// and merged via two-pointer in the send loop.
pub struct StreamingParquetFeed {
    receiver: Receiver<FeedMessage>,
    error: Option<StreamingParquetFeedError>,
    /// Keep the thread handle so it is joined on drop (prevents leaks).
    _worker: thread::JoinHandle<()>,
}

impl StreamingParquetFeed {
    /// Spawn the background DuckDB thread and return a ready-to-use feed.
    ///
    /// Returns immediately; data starts flowing as soon as DuckDB opens the
    /// Parquet files. If `data_dir` does not exist the feed will be empty.
    pub fn new(
        data_dir: &str,
        symbols: &[String],
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        options: &HistoricalLoadOptions,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<FeedMessage>(CHANNEL_CAPACITY);

        let data_dir = data_dir.to_string();
        let symbols = symbols.to_vec();
        let lob_sample_secs = options.lob_sample_secs;
        let require_official_settlement = options.require_official_settlement;

        let worker = thread::spawn(move || {
            let error_tx = tx.clone();
            if let Err(e) = run_background(
                &data_dir,
                &symbols,
                from,
                to,
                lob_sample_secs,
                require_official_settlement,
                tx,
            ) {
                let error = StreamingParquetFeedError::Background(e.to_string());
                tracing::error!(error = %error, "StreamingParquetFeed background thread error");
                let _ = error_tx.send(FeedMessage::Error(error));
            }
        });

        Self {
            receiver: rx,
            error: None,
            _worker: worker,
        }
    }

    /// Read the next update, returning any background DuckDB/row-conversion
    /// failure instead of making it look like ordinary feed exhaustion.
    pub fn next_result(&mut self) -> Result<Option<MarketUpdate>, StreamingParquetFeedError> {
        if let Some(error) = self.error.clone() {
            return Err(error);
        }

        match self.receiver.recv() {
            Ok(FeedMessage::Update(update)) => Ok(Some(update)),
            Ok(FeedMessage::Error(error)) => {
                self.error = Some(error.clone());
                Err(error)
            }
            Err(_) => Ok(None),
        }
    }

    pub fn background_error(&self) -> Option<&StreamingParquetFeedError> {
        self.error.as_ref()
    }
}

#[async_trait]
impl Feed for StreamingParquetFeed {
    async fn next(&mut self) -> Option<MarketUpdate> {
        match self.next_result() {
            Ok(update) => update,
            Err(error) => panic!("{error}"),
        }
    }
}

// ── Background thread ────────────────────────────────────────────────────────

/// Entry point for the background worker thread.
///
/// Builds a single DuckDB `UNION ALL` query across spot, agg trades, LOB, and
/// quotes, ordered by timestamp. DuckDB handles the merge sort with disk spill.
/// Events are loaded separately (small dataset) and merged via two-pointer.
/// Memory usage is O(channel buffer) regardless of date range.
#[cfg(feature = "parquet-feed")]
fn run_background(
    data_dir: &str,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    lob_sample_secs: u32,
    require_official_settlement: bool,
    tx: SyncSender<FeedMessage>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use chrono::Duration;
    use duckdb::Connection;
    use std::path::Path;
    use tracing::info;

    const WARMUP_MINUTES: i64 = 30;

    if !Path::new(data_dir).exists() {
        return Ok(());
    }

    let memory_limit =
        std::env::var("PLOY_DUCKDB_MEMORY_LIMIT").unwrap_or_else(|_| "6GB".to_string());
    let temp_dir =
        std::env::var("PLOY_DUCKDB_TEMP_DIR").unwrap_or_else(|_| "/tmp/duckdb_spill".to_string());
    std::fs::create_dir_all(&temp_dir).ok();
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(&format!(
        "SET memory_limit='{}'; SET temp_directory='{}';",
        memory_limit.replace('\'', "''"),
        temp_dir.replace('\'', "''")
    ))?;

    let sym_filter = symbol_filter_sql(symbols);
    let spot_from = from - Duration::minutes(WARMUP_MINUTES);
    let from_str = from.to_rfc3339();
    let to_str = to.to_rfc3339();
    let spot_from_str = spot_from.to_rfc3339();
    let _bucket_us = (lob_sample_secs.max(1) as i64) * 1_000_000;

    // File globs
    let spot_glob = format!("{data_dir}/binance_price_ticks/*.parquet");
    let agg_glob = format!("{data_dir}/binance_agg_trade_ticks/*.parquet");
    let lob_glob = format!("{data_dir}/binance_lob_ticks/*.parquet");
    let quote_glob = format!("{data_dir}/clob_quote_ticks/*.parquet");
    let orderbook_dir = format!("{data_dir}/orderbook_snapshots");
    let orderbook_glob = format!("{orderbook_dir}/**/*.parquet");
    if Path::new(&orderbook_dir).exists() {
        validate_orderbook_archive_window(Path::new(&orderbook_dir), from, to)?;
    }

    // ── 1. Load events separately (small, ~11K rows) ────────────────────────
    let events = load_events_vec(
        &conn,
        data_dir,
        symbols,
        &from_str,
        &to_str,
        require_official_settlement,
    )?;
    info!(count = events.len(), "StreamingParquetFeed: loaded events");
    let token_filter = event_token_filter_sql(&events);
    let mut evt_idx = 0;

    // ── 2. Build UNION ALL query for the big tables ─────────────────────────
    let mut parts: Vec<String> = Vec::new();

    // Spot prices (with 30min warmup)
    if Path::new(&format!("{data_dir}/binance_price_ticks")).exists() {
        parts.push(format!(
            "SELECT epoch_us(trade_time)::BIGINT AS ts_us, \
                    {SPOT_SOURCE_RANK} AS source_rank, \
                    'spot' AS typ, \
                    symbol AS s1, NULL AS s2, NULL AS s3, \
                    CAST(price AS DOUBLE) AS f1, 0.0 AS f2, 0.0 AS f3, 0.0 AS f4, \
                    CAST(0 AS BIGINT) AS i1, false AS b1 \
             FROM read_parquet('{spot_glob}') \
             WHERE trade_time >= TIMESTAMPTZ '{spot_from_str}' \
               AND trade_time <= TIMESTAMPTZ '{to_str}' \
               {sym_filter}"
        ));
    }

    // Agg trades — full tick-by-tick, no downsampling.
    // Real collection rate: 0.6-4.4 r/s per symbol. Each trade carries direction
    // (is_buyer_maker) used for signed_trade_imbalance in the Confirmation layer.
    if Path::new(&format!("{data_dir}/binance_agg_trade_ticks")).exists() {
        parts.push(format!(
            "SELECT epoch_us(trade_time)::BIGINT AS ts_us, \
                    {AGG_SOURCE_RANK} AS source_rank, \
                    'agg' AS typ, \
                    symbol AS s1, NULL AS s2, NULL AS s3, \
                    CAST(price AS DOUBLE) AS f1, CAST(quantity AS DOUBLE) AS f2, \
                    0.0 AS f3, 0.0 AS f4, \
                    CAST(agg_trade_id AS BIGINT) AS i1, is_buyer_maker AS b1 \
             FROM read_parquet('{agg_glob}') \
             WHERE trade_time >= TIMESTAMPTZ '{from_str}' \
               AND trade_time <= TIMESTAMPTZ '{to_str}' \
               {sym_filter}"
        ));
    }

    // LOB — full tick-by-tick, no downsampling. Memory is O(channel buffer) regardless.
    // Downsampling causes temporal misalignment: a 30s bucket at T=0 is used when
    // evaluating quotes at T=15s, which is incorrect. Each LOB tick is processed
    // in timestamp order, matching live trading behavior exactly.
    if Path::new(&format!("{data_dir}/binance_lob_ticks")).exists() {
        parts.push(format!(
            "SELECT epoch_us(event_time)::BIGINT AS ts_us, \
                    {LOB_SOURCE_RANK} AS source_rank, \
                    'lob' AS typ, \
                    symbol AS s1, NULL AS s2, NULL AS s3, \
                    CAST(COALESCE(obi_5, 0.0) AS DOUBLE) AS f1, \
                    CAST(COALESCE(spread_bps, 0.0) AS DOUBLE) AS f2, \
                    CAST(COALESCE(bid_volume_5, 0.0) AS DOUBLE) AS f3, \
                    CAST(COALESCE(ask_volume_5, 0.0) AS DOUBLE) AS f4, \
                    CAST(0 AS BIGINT) AS i1, false AS b1 \
             FROM read_parquet('{lob_glob}') \
             WHERE event_time >= TIMESTAMPTZ '{from_str}' \
               AND event_time <= TIMESTAMPTZ '{to_str}' \
               {sym_filter}"
        ));
    }

    // Prefer the full-fidelity CLOB archive. If it is present, missing rows for
    // the requested window fail closed instead of silently mixing top-book data.
    if Path::new(&orderbook_dir).exists() {
        parts.push(format!(
            "SELECT EPOCH_US(received_at)::BIGINT AS ts_us, \
                    {QUOTE_SOURCE_RANK} AS source_rank, \
                    'quote_depth' AS typ, \
                    token_id AS s1, CAST(bids AS VARCHAR) AS s2, \
                    CAST(asks AS VARCHAR) AS s3, \
                    NULL AS f1, NULL AS f2, NULL AS f3, NULL AS f4, \
                    CAST(0 AS BIGINT) AS i1, false AS b1 \
             FROM read_parquet('{orderbook_glob}') \
             WHERE received_at >= TIMESTAMPTZ '{from_str}' \
               AND received_at <= TIMESTAMPTZ '{to_str}' \
               {token_filter}"
        ));
    // Legacy PM quote ticks remain useful for diagnostics, but never carry
    // full-depth evidence.
    } else if Path::new(&format!("{data_dir}/clob_quote_ticks")).exists() {
        parts.push(format!(
            "SELECT ts_us, source_rank, typ, s1, s2, s3, f1, f2, f3, f4, i1, b1 \
             FROM ( \
                 SELECT DISTINCT ON (date_trunc('second', received_at), token_id) \
                        EPOCH_US(received_at)::BIGINT AS ts_us, \
                        {QUOTE_SOURCE_RANK} AS source_rank, \
                        'quote' AS typ, \
                        token_id AS s1, NULL AS s2, NULL AS s3, \
                        CAST(best_bid AS DOUBLE) AS f1, CAST(best_ask AS DOUBLE) AS f2, \
                        CAST(bid_size AS DOUBLE) AS f3, CAST(ask_size AS DOUBLE) AS f4, \
                        CAST(0 AS BIGINT) AS i1, false AS b1 \
                 FROM read_parquet('{quote_glob}') \
                 WHERE received_at >= TIMESTAMPTZ '{from_str}' \
                   AND received_at <= TIMESTAMPTZ '{to_str}' \
                   {token_filter} \
                   AND source IN ('polymarket_ws', 'polymarket_ws_collector', 'ploy_runner_live') \
                   AND best_bid IS NOT NULL AND best_ask IS NOT NULL \
                   AND (best_bid > 0.01 AND best_bid < 0.99 OR best_ask > 0.01 AND best_ask < 0.99) \
                 ORDER BY date_trunc('second', received_at), token_id, \
                          CASE WHEN ask_size IS NOT NULL AND ask_size > 0 THEN 1 ELSE 0 END DESC, \
                          CASE WHEN bid_size IS NOT NULL AND bid_size > 0 THEN 1 ELSE 0 END DESC, \
                          received_at DESC \
             )"
        ));
    }

    if parts.is_empty() {
        // No data tables found — just send events
        for (_, update) in &events {
            if send_update(&tx, update.clone()).is_err() {
                return Ok(());
            }
        }
        return Ok(());
    }

    let union_sql = build_union_sql(&parts);

    // ── 3. Stream UNION ALL results, merging events via two-pointer ─────────
    let mut stmt = match conn.prepare(&union_sql) {
        Ok(s) => s,
        Err(e) => {
            return Err(e.into());
        }
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,            // ts_us
            row.get::<_, String>(1)?,         // typ
            row.get::<_, Option<String>>(2)?, // s1
            row.get::<_, Option<String>>(3)?, // s2
            row.get::<_, Option<String>>(4)?, // s3
            row.get::<_, Option<f64>>(5)?,    // f1 (nullable for quotes with NULL bid)
            row.get::<_, Option<f64>>(6)?,    // f2
            row.get::<_, Option<f64>>(7)?,    // f3
            row.get::<_, Option<f64>>(8)?,    // f4
            row.get::<_, Option<i64>>(9)?,    // i1
            row.get::<_, Option<bool>>(10)?,  // b1
        ))
    })?;

    let mut total = 0usize;
    for row in rows {
        let (ts_us, typ, s1, s2, s3, f1_opt, f2_opt, f3_opt, f4_opt, i1_opt, b1_opt) = row?;

        // Insert any events that should come before this row
        while evt_idx < events.len() && should_send_event_before_row(events[evt_idx].0, ts_us) {
            if send_update(&tx, events[evt_idx].1.clone()).is_err() {
                return Ok(());
            }
            evt_idx += 1;
        }

        let row = StreamRow {
            ts_us,
            typ,
            s1,
            s2,
            s3,
            f1: f1_opt,
            f2: f2_opt,
            f3: f3_opt,
            f4: f4_opt,
            i1: i1_opt,
            b1: b1_opt,
        };
        for update in market_updates_from_row(row) {
            if send_update(&tx, update).is_err() {
                return Ok(());
            }
        }
        total += 1;
    }

    // Send remaining events after the UNION ALL stream is exhausted
    while evt_idx < events.len() {
        if send_update(&tx, events[evt_idx].1.clone()).is_err() {
            break;
        }
        evt_idx += 1;
    }

    info!(
        total,
        events = events.len(),
        "StreamingParquetFeed: streaming complete"
    );
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn send_update(
    tx: &SyncSender<FeedMessage>,
    update: MarketUpdate,
) -> Result<(), mpsc::SendError<FeedMessage>> {
    tx.send(FeedMessage::Update(update))
}

#[cfg(feature = "parquet-feed")]
#[derive(Debug)]
struct StreamRow {
    ts_us: i64,
    typ: String,
    s1: Option<String>,
    s2: Option<String>,
    s3: Option<String>,
    f1: Option<f64>,
    f2: Option<f64>,
    f3: Option<f64>,
    f4: Option<f64>,
    i1: Option<i64>,
    b1: Option<bool>,
}

#[cfg(feature = "parquet-feed")]
fn market_updates_from_row(row: StreamRow) -> Vec<MarketUpdate> {
    use rust_decimal::Decimal;
    use std::sync::Arc;

    let ts = DateTime::from_timestamp_micros(row.ts_us).unwrap_or_default();
    match row.typ.as_str() {
        "spot" => {
            let symbol: Arc<str> = Arc::from(row.s1.unwrap_or_default());
            let price = Decimal::try_from(row.f1.unwrap_or(0.0)).unwrap_or_default();
            vec![MarketUpdate::SpotPrice { symbol, price, ts }]
        }
        "agg" => {
            let symbol: Arc<str> = Arc::from(row.s1.unwrap_or_default());
            let price = Decimal::try_from(row.f1.unwrap_or(0.0)).unwrap_or_default();
            let quantity = Decimal::try_from(row.f2.unwrap_or(0.0)).unwrap_or_default();
            vec![MarketUpdate::AggTrade {
                symbol,
                agg_trade_id: row.i1.unwrap_or(0) as u64,
                price,
                quantity,
                is_buyer_maker: row.b1.unwrap_or(false),
                ts,
            }]
        }
        "lob" => {
            let symbol: Arc<str> = Arc::from(row.s1.unwrap_or_default());
            let obi = row.f1.unwrap_or(0.0);
            let spread_bps = row.f2.unwrap_or(0.0) as u32;
            let bid_depth_near = row.f3.unwrap_or(0.0);
            let ask_depth_near = row.f4.unwrap_or(0.0);
            vec![
                MarketUpdate::L2 {
                    symbol: Arc::clone(&symbol),
                    obi,
                    spread_bps,
                    ts,
                },
                MarketUpdate::L2Depth {
                    symbol,
                    obi,
                    spread_bps,
                    bid_depth_near,
                    ask_depth_near,
                    ts,
                },
            ]
        }
        "quote" => {
            let token_id: Arc<str> = Arc::from(row.s1.unwrap_or_default());
            let bid = row.f1.and_then(|v| Decimal::try_from(v).ok());
            let ask = row.f2.and_then(|v| Decimal::try_from(v).ok());
            let bid_size = row.f3.and_then(|v| Decimal::try_from(v).ok());
            let ask_size = row.f4.and_then(|v| Decimal::try_from(v).ok());
            if bid.is_none() && ask.is_none() {
                Vec::new()
            } else {
                vec![MarketUpdate::Quote {
                    token_id,
                    bid,
                    ask,
                    bid_size,
                    ask_size,
                    bid_levels: Vec::new(),
                    ask_levels: Vec::new(),
                    ts,
                }]
            }
        }
        "quote_depth" => {
            let token_id: std::sync::Arc<str> = std::sync::Arc::from(row.s1.unwrap_or_default());
            let bid_levels = book_levels_from_json(row.s2.as_deref(), false);
            let ask_levels = book_levels_from_json(row.s3.as_deref(), true);
            if bid_levels.is_empty() && ask_levels.is_empty() {
                Vec::new()
            } else {
                let best_bid = bid_levels.first();
                let best_ask = ask_levels.first();
                vec![MarketUpdate::Quote {
                    token_id,
                    bid: best_bid.map(|level| level.price),
                    ask: best_ask.map(|level| level.price),
                    bid_size: best_bid.map(|level| level.size),
                    ask_size: best_ask.map(|level| level.size),
                    bid_levels,
                    ask_levels,
                    ts,
                }]
            }
        }
        _ => Vec::new(),
    }
}

#[cfg(feature = "parquet-feed")]
fn book_levels_from_json(
    value: Option<&str>,
    ascending: bool,
) -> Vec<ploy_market_contracts::BookLevel> {
    use ploy_market_contracts::BookLevel;
    use rust_decimal_macros::dec;

    let Ok(serde_json::Value::Array(items)) =
        serde_json::from_str::<serde_json::Value>(value.unwrap_or("[]"))
    else {
        return Vec::new();
    };
    let mut levels = items
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::Array(parts) if parts.len() >= 2 => {
                Some((decimal_from_json(&parts[0])?, decimal_from_json(&parts[1])?))
            }
            serde_json::Value::Object(parts) => Some((
                decimal_from_json(parts.get("price")?)?,
                decimal_from_json(parts.get("size")?)?,
            )),
            _ => None,
        })
        .filter(|(price, size)| {
            *price > dec!(0.01) && *price < dec!(0.99) && *size > rust_decimal::Decimal::ZERO
        })
        .map(|(price, size)| BookLevel { price, size })
        .collect::<Vec<_>>();
    if ascending {
        levels.sort_by_key(|level| level.price);
    } else {
        levels.sort_by_key(|level| std::cmp::Reverse(level.price));
    }
    levels
}

#[cfg(feature = "parquet-feed")]
fn decimal_from_json(value: &serde_json::Value) -> Option<rust_decimal::Decimal> {
    match value {
        serde_json::Value::Number(number) => number.to_string().parse().ok(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// Stub for when the feature flag is disabled.
#[cfg(not(feature = "parquet-feed"))]
fn run_background(
    _data_dir: &str,
    _symbols: &[String],
    _from: DateTime<Utc>,
    _to: DateTime<Utc>,
    _lob_sample_secs: u32,
    _require_official_settlement: bool,
    _tx: SyncSender<FeedMessage>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────────

#[cfg(feature = "parquet-feed")]
const SPOT_SOURCE_RANK: u8 = 10;
#[cfg(feature = "parquet-feed")]
const AGG_SOURCE_RANK: u8 = 20;
#[cfg(feature = "parquet-feed")]
const LOB_SOURCE_RANK: u8 = 30;
#[cfg(feature = "parquet-feed")]
const QUOTE_SOURCE_RANK: u8 = 40;

#[cfg(feature = "parquet-feed")]
fn build_union_sql(parts: &[String]) -> String {
    format!(
        "SELECT ts_us, typ, s1, s2, s3, f1, f2, f3, f4, i1, b1 \
         FROM ({}) \
         ORDER BY ts_us, source_rank, s1, i1, f1, f2, f3, f4",
        parts.join(" UNION ALL ")
    )
}

#[cfg(feature = "parquet-feed")]
fn should_send_event_before_row(event_ts_us: i64, row_ts_us: i64) -> bool {
    event_ts_us <= row_ts_us
}

#[cfg(feature = "parquet-feed")]
fn event_sort_key(update: &MarketUpdate) -> (u8, String) {
    match update {
        MarketUpdate::EventDiscovered { event_id, .. } => (0, event_id.to_string()),
        MarketUpdate::EventExpired { event_id, .. } => (1, event_id.to_string()),
        _ => (2, String::new()),
    }
}

/// Load event rows (EventDiscovered + EventExpired pairs) into a sorted Vec.
///
/// Events are small (~11K rows) so loading them into memory is fine. They are
/// merged into the main UNION ALL stream via two-pointer in the send loop.
#[cfg(feature = "parquet-feed")]
fn load_events_vec(
    conn: &duckdb::Connection,
    data_dir: &str,
    symbols: &[String],
    from_str: &str,
    to_str: &str,
    require_official_settlement: bool,
) -> Result<Vec<(i64, MarketUpdate)>, Box<dyn std::error::Error + Send + Sync>> {
    use super::options::normalize_token_id;
    use rust_decimal::Decimal;
    use std::path::Path;
    use std::sync::Arc;

    #[derive(Debug)]
    struct EventRow {
        market_slug: String,
        symbol: String,
        start_us: i64,
        end_us: i64,
        price_to_beat: Option<Decimal>,
        up_token: String,
        down_token: String,
    }

    let dir = format!("{data_dir}/pm_market_metadata");
    if !Path::new(&dir).exists() {
        return Ok(Vec::new());
    }

    let glob = format!("{dir}/*.parquet");
    let sym_filter = symbol_filter_sql(symbols);
    let sql = format!(
        "SELECT market_slug, symbol, \
                EPOCH_US(start_time)::BIGINT, EPOCH_US(end_time)::BIGINT, \
                CAST(price_to_beat AS DOUBLE), \
                (json_extract_string(raw_market, '$.markets[0].clobTokenIds')::JSON->>0) AS up_token_id, \
                (json_extract_string(raw_market, '$.markets[0].clobTokenIds')::JSON->>1) AS down_token_id \
         FROM read_parquet('{glob}') \
         WHERE end_time >= TIMESTAMPTZ '{from_str}' \
           AND start_time <= TIMESTAMPTZ '{to_str}' \
           {sym_filter} \
           AND raw_market IS NOT NULL \
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

    let mut event_rows = Vec::new();
    for row in rows {
        let (market_slug, symbol_opt, start_us, end_us, price_to_beat_f, up_opt, dn_opt) = row?;
        let symbol = match symbol_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let start_us = match start_us {
            Some(v) => v,
            None => continue,
        };
        let end_us = match end_us {
            Some(v) => v,
            None => continue,
        };
        let up_raw = match up_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };
        let dn_raw = match dn_opt {
            Some(s) if !s.is_empty() => s,
            _ => continue,
        };

        event_rows.push(EventRow {
            market_slug,
            symbol,
            start_us,
            end_us,
            price_to_beat: price_to_beat_f.and_then(|f| Decimal::try_from(f).ok()),
            up_token: normalize_token_id(&up_raw),
            down_token: normalize_token_id(&dn_raw),
        });
    }

    let settlement_prices = load_event_settlement_prices(conn, data_dir)?;
    tracing::info!(
        count = settlement_prices.len(),
        require_official_settlement,
        "StreamingParquetFeed: loaded event settlement prices"
    );
    if require_official_settlement && settlement_prices.is_empty() {
        tracing::warn!(
            "StreamingParquetFeed: official settlement required but no pm_token_settlements parquet rows were loaded"
        );
    }

    let discovered_event_rows = event_rows.len();
    let resolved_event_rows = event_rows
        .iter()
        .filter(|row| {
            resolve_up_won_from_settlements(
                &settlement_prices,
                &row.market_slug,
                &row.up_token,
                &row.down_token,
            )
            .is_some()
        })
        .count();
    if require_official_settlement && resolved_event_rows != discovered_event_rows {
        return Err(format!(
            "official settlement coverage is incomplete \
             (resolved={resolved_event_rows}, event_rows={discovered_event_rows}, settlement_prices={})",
            settlement_prices.len()
        )
        .into());
    }
    let mut events = Vec::new();
    for row in event_rows {
        let resolved_up_won = resolve_up_won_from_settlements(
            &settlement_prices,
            &row.market_slug,
            &row.up_token,
            &row.down_token,
        );
        if require_official_settlement && resolved_up_won.is_none() {
            continue;
        }
        let up_token: Arc<str> = Arc::from(row.up_token);
        let down_token: Arc<str> = Arc::from(row.down_token);
        if row.symbol.is_empty() || up_token.is_empty() || down_token.is_empty() {
            continue;
        }

        let start_us = row.start_us;
        let end_us = row.end_us;
        let start_time = DateTime::from_timestamp_micros(start_us).unwrap_or_default();
        let end_time = DateTime::from_timestamp_micros(end_us).unwrap_or_default();
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        let event_id: Arc<str> = Arc::from(row.market_slug);
        let symbol: Arc<str> = Arc::from(row.symbol);

        // EventDiscovered fires at start_time
        events.push((
            start_us,
            MarketUpdate::EventDiscovered {
                event_id: Arc::clone(&event_id),
                symbol,
                up_token,
                down_token,
                end_time,
                window_secs,
                price_to_beat: row.price_to_beat,
                resolved_up_won: None,
            },
        ));

        // EventExpired fires at end_time
        events.push((
            end_us,
            MarketUpdate::EventExpired {
                event_id,
                end_time,
                resolved_up_won,
            },
        ));
    }

    events.sort_by(|(left_ts, left), (right_ts, right)| {
        (*left_ts, event_sort_key(left)).cmp(&(*right_ts, event_sort_key(right)))
    });
    Ok(events)
}

#[cfg(feature = "parquet-feed")]
fn load_event_settlement_prices(
    conn: &duckdb::Connection,
    data_dir: &str,
) -> Result<
    std::collections::HashMap<(String, String), rust_decimal::Decimal>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    use super::options::normalize_token_id;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::path::Path;

    let dir = format!("{data_dir}/pm_token_settlements");
    if !Path::new(&dir).exists() {
        return Ok(HashMap::new());
    }

    let glob = format!("{dir}/*.parquet");
    let sql = format!(
        "SELECT market_slug, token_id, CAST(settled_price AS DOUBLE) \
         FROM read_parquet('{glob}') \
         WHERE resolved = TRUE \
           AND market_slug IS NOT NULL \
           AND token_id IS NOT NULL \
           AND settled_price IS NOT NULL"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<f64>>(2)?,
        ))
    })?;

    let mut prices = HashMap::new();
    for row in rows {
        let (market_slug, token_id, settled_price) = row?;
        let (Some(market_slug), Some(token_id), Some(settled_price)) =
            (market_slug, token_id, settled_price)
        else {
            continue;
        };
        let Ok(settled_price) = Decimal::try_from(settled_price) else {
            continue;
        };
        prices.insert((market_slug, normalize_token_id(&token_id)), settled_price);
    }

    Ok(prices)
}

#[cfg(feature = "parquet-feed")]
fn resolve_up_won_from_settlements(
    settlement_prices: &std::collections::HashMap<(String, String), rust_decimal::Decimal>,
    event_id: &str,
    up_token: &str,
    down_token: &str,
) -> Option<bool> {
    use rust_decimal::Decimal;

    match (
        settlement_prices
            .get(&(event_id.to_string(), up_token.to_string()))
            .copied(),
        settlement_prices
            .get(&(event_id.to_string(), down_token.to_string()))
            .copied(),
    ) {
        (Some(up), Some(down)) if up != down => Some(up > down),
        (Some(up), _) => Some(up > Decimal::new(5, 1)),
        (_, Some(down)) => Some(down < Decimal::new(5, 1)),
        _ => None,
    }
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
fn event_token_filter_sql(events: &[(i64, MarketUpdate)]) -> String {
    let mut tokens = events
        .iter()
        .filter_map(|(_, update)| match update {
            MarketUpdate::EventDiscovered {
                up_token,
                down_token,
                ..
            } => Some([up_token.as_ref(), down_token.as_ref()]),
            _ => None,
        })
        .flatten()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tokens.sort();
    tokens.dedup();
    if tokens.is_empty() {
        return "AND 1 = 0".to_string();
    }
    let list = tokens
        .iter()
        .map(|token| format!("'{}'", token.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    format!("AND token_id IN ({list})")
}

#[cfg(feature = "parquet-feed")]
fn validate_orderbook_archive_window(
    root: &std::path::Path,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use chrono::{FixedOffset, TimeZone};

    if to < from {
        return Err("full-depth archive window end precedes start".into());
    }
    let shanghai = FixedOffset::east_opt(8 * 60 * 60).expect("valid UTC+8 offset");
    let start_hour = from.timestamp().div_euclid(3600) * 3600;
    let end_hour = to.timestamp().div_euclid(3600) * 3600;
    let mut epoch = start_hour;
    while epoch <= end_hour {
        let utc = Utc
            .timestamp_opt(epoch, 0)
            .single()
            .ok_or("invalid archive hour")?;
        let local = utc.with_timezone(&shanghai);
        let day = local.format("%Y-%m-%d").to_string();
        let hour = local.format("%H").to_string();
        let hour_dir = root.join(format!("date={day}/hour={hour}"));
        let marker = hour_dir.join("_SUCCESS");
        let manifest_path = hour_dir.join("manifest.json");
        let parquet_path = hour_dir.join("snapshots.parquet");
        if !marker.is_file() || !manifest_path.is_file() || !parquet_path.is_file() {
            return Err(
                format!("incomplete full-depth archive hour: {}", hour_dir.display()).into(),
            );
        }
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        let row_count = manifest
            .get("row_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let expected_sha256 = manifest
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if manifest
            .get("full_fidelity")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            || manifest.get("date").and_then(serde_json::Value::as_str) != Some(day.as_str())
            || manifest.get("hour").and_then(serde_json::Value::as_str) != Some(hour.as_str())
            || row_count == 0
            || std::fs::metadata(&parquet_path)?.len() == 0
            || expected_sha256.len() != 64
            || sha256_file(&parquet_path)? != expected_sha256
        {
            return Err(format!(
                "invalid full-depth archive manifest: {}",
                manifest_path.display()
            )
            .into());
        }
        epoch += 3600;
    }
    Ok(())
}

#[cfg(feature = "parquet-feed")]
fn sha256_file(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(all(test, feature = "parquet-feed"))]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn next_result_returns_background_error() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(FeedMessage::Error(StreamingParquetFeedError::Background(
            "duckdb exploded".to_string(),
        )))
        .unwrap();
        drop(tx);

        let worker = thread::spawn(|| {});
        let mut feed = StreamingParquetFeed {
            receiver: rx,
            error: None,
            _worker: worker,
        };

        let err = feed
            .next_result()
            .expect_err("stream error must be observable");
        assert_eq!(
            err,
            StreamingParquetFeedError::Background("duckdb exploded".to_string())
        );
        assert_eq!(feed.background_error(), Some(&err));
    }

    #[test]
    fn union_query_orders_same_timestamp_sources_deterministically() {
        let sql = build_union_sql(&["SELECT 1 AS ts_us, 10 AS source_rank, 'spot' AS typ, 'BTCUSDT' AS s1, NULL AS s2, NULL AS s3, 1.0 AS f1, 0.0 AS f2, 0.0 AS f3, 0.0 AS f4, 0 AS i1, false AS b1".to_string()]);

        assert!(sql.contains("SELECT ts_us, typ, s1, s2, s3, f1, f2, f3, f4, i1, b1"));
        assert!(sql.contains("ORDER BY ts_us, source_rank, s1, i1, f1, f2, f3, f4"));
        assert!(SPOT_SOURCE_RANK < AGG_SOURCE_RANK);
        assert!(AGG_SOURCE_RANK < LOB_SOURCE_RANK);
        assert!(LOB_SOURCE_RANK < QUOTE_SOURCE_RANK);
    }

    #[test]
    fn full_depth_archive_requires_every_intersecting_shanghai_hour() {
        let root =
            std::env::temp_dir().join(format!("ploy-orderbook-window-{}", uuid::Uuid::new_v4()));
        let from = Utc.with_ymd_and_hms(2026, 7, 1, 15, 30, 0).unwrap();
        let to = Utc.with_ymd_and_hms(2026, 7, 1, 16, 30, 0).unwrap();
        for (day, hour) in [("2026-07-01", "23"), ("2026-07-02", "00")] {
            let hour_dir = root.join(format!("date={day}/hour={hour}"));
            std::fs::create_dir_all(&hour_dir).unwrap();
            std::fs::write(hour_dir.join("_SUCCESS"), "").unwrap();
            let parquet_path = hour_dir.join("snapshots.parquet");
            std::fs::write(&parquet_path, "fixture").unwrap();
            std::fs::write(
                hour_dir.join("manifest.json"),
                serde_json::json!({
                    "date": day,
                    "hour": hour,
                    "row_count": 1,
                    "full_fidelity": true,
                    "sha256": sha256_file(&parquet_path).unwrap()
                })
                .to_string(),
            )
            .unwrap();
        }
        assert!(validate_orderbook_archive_window(&root, from, to).is_ok());
        std::fs::remove_file(root.join("date=2026-07-02/hour=00/_SUCCESS")).unwrap();
        assert!(validate_orderbook_archive_window(&root, from, to)
            .unwrap_err()
            .to_string()
            .contains("incomplete full-depth archive hour"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn events_are_emitted_before_same_timestamp_rows() {
        assert!(should_send_event_before_row(1_000, 1_000));
        assert!(should_send_event_before_row(999, 1_000));
        assert!(!should_send_event_before_row(1_001, 1_000));
    }

    #[test]
    fn archive_quotes_are_scoped_to_discovered_event_tokens() {
        let ts = Utc.timestamp_micros(1_000_000).unwrap();
        let events = vec![(
            1_000_000,
            MarketUpdate::EventDiscovered {
                event_id: Arc::from("event-a"),
                symbol: Arc::from("BTCUSDT"),
                up_token: Arc::from("up-token"),
                down_token: Arc::from("down-token"),
                end_time: ts,
                window_secs: 300,
                price_to_beat: Some(dec!(50000)),
                resolved_up_won: None,
            },
        )];

        assert_eq!(
            event_token_filter_sql(&events),
            "AND token_id IN ('down-token', 'up-token')"
        );
        assert_eq!(event_token_filter_sql(&[]), "AND 1 = 0");
    }

    #[test]
    fn lifecycle_events_sort_discovered_before_expired_at_same_timestamp() {
        let ts = Utc.timestamp_micros(1_000_000).unwrap();
        let mut events = vec![
            (
                1_000_000,
                MarketUpdate::EventExpired {
                    event_id: Arc::from("event-a"),
                    end_time: ts,
                    resolved_up_won: None,
                },
            ),
            (
                1_000_000,
                MarketUpdate::EventDiscovered {
                    event_id: Arc::from("event-a"),
                    symbol: Arc::from("BTCUSDT"),
                    up_token: Arc::from("up"),
                    down_token: Arc::from("down"),
                    end_time: ts,
                    window_secs: 300,
                    price_to_beat: Some(dec!(50000)),
                    resolved_up_won: None,
                },
            ),
        ];

        events.sort_by(|(left_ts, left), (right_ts, right)| {
            (*left_ts, event_sort_key(left)).cmp(&(*right_ts, event_sort_key(right)))
        });

        assert!(matches!(events[0].1, MarketUpdate::EventDiscovered { .. }));
        assert!(matches!(events[1].1, MarketUpdate::EventExpired { .. }));
    }

    #[test]
    fn lob_row_emits_l2_then_l2_depth() {
        let updates = market_updates_from_row(StreamRow {
            ts_us: 1_000_000,
            typ: "lob".to_string(),
            s1: Some("BTCUSDT".to_string()),
            s2: None,
            s3: None,
            f1: Some(0.25),
            f2: Some(3.0),
            f3: Some(12.5),
            f4: Some(13.5),
            i1: None,
            b1: None,
        });

        assert_eq!(updates.len(), 2);
        assert!(matches!(updates[0], MarketUpdate::L2 { .. }));
        assert!(matches!(updates[1], MarketUpdate::L2Depth { .. }));
    }

    #[test]
    fn agg_row_emits_one_tick_level_trade() {
        let updates = market_updates_from_row(StreamRow {
            ts_us: 1_000_000,
            typ: "agg".to_string(),
            s1: Some("BTCUSDT".to_string()),
            s2: None,
            s3: None,
            f1: Some(51000.0),
            f2: Some(0.42),
            f3: None,
            f4: None,
            i1: Some(123),
            b1: Some(true),
        });

        assert_eq!(updates.len(), 1);
        match &updates[0] {
            MarketUpdate::AggTrade {
                agg_trade_id,
                is_buyer_maker,
                ..
            } => {
                assert_eq!(*agg_trade_id, 123);
                assert!(*is_buyer_maker);
            }
            other => panic!("expected AggTrade, got {other:?}"),
        }
    }

    #[test]
    fn quote_row_without_bid_or_ask_is_filtered() {
        let updates = market_updates_from_row(StreamRow {
            ts_us: 1_000_000,
            typ: "quote".to_string(),
            s1: Some("token".to_string()),
            s2: None,
            s3: None,
            f1: None,
            f2: None,
            f3: None,
            f4: None,
            i1: None,
            b1: None,
        });

        assert!(updates.is_empty());
    }

    #[test]
    fn quote_row_preserves_executable_sizes() {
        let updates = market_updates_from_row(StreamRow {
            ts_us: 1_000_000,
            typ: "quote".to_string(),
            s1: Some("token".to_string()),
            s2: None,
            s3: None,
            f1: Some(0.5),
            f2: Some(0.75),
            f3: Some(12.0),
            f4: Some(13.0),
            i1: None,
            b1: None,
        });

        assert_eq!(updates.len(), 1);
        match &updates[0] {
            MarketUpdate::Quote {
                bid,
                ask,
                bid_size,
                ask_size,
                bid_levels,
                ask_levels,
                ..
            } => {
                assert_eq!(*bid, Some(dec!(0.5)));
                assert_eq!(*ask, Some(dec!(0.75)));
                assert_eq!(*bid_size, Some(dec!(12)));
                assert_eq!(*ask_size, Some(dec!(13)));
                assert!(bid_levels.is_empty());
                assert!(ask_levels.is_empty());
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn orderbook_row_preserves_sorted_executable_depth() {
        let updates = market_updates_from_row(StreamRow {
            ts_us: 1_000_000,
            typ: "quote_depth".to_string(),
            s1: Some("token".to_string()),
            s2: Some(r#"[["0.48","10"],["0.49","5"]]"#.to_string()),
            s3: Some(r#"[{"price":"0.52","size":"8"},{"price":"0.51","size":"7"}]"#.to_string()),
            f1: None,
            f2: None,
            f3: None,
            f4: None,
            i1: None,
            b1: None,
        });

        assert_eq!(updates.len(), 1);
        match &updates[0] {
            MarketUpdate::Quote {
                bid,
                ask,
                bid_size,
                ask_size,
                bid_levels,
                ask_levels,
                ..
            } => {
                assert_eq!(*bid, Some(dec!(0.49)));
                assert_eq!(*ask, Some(dec!(0.51)));
                assert_eq!(*bid_size, Some(dec!(5)));
                assert_eq!(*ask_size, Some(dec!(7)));
                assert_eq!(bid_levels.len(), 2);
                assert_eq!(ask_levels.len(), 2);
            }
            other => panic!("expected Quote, got {other:?}"),
        }
    }

    #[test]
    fn official_settlement_resolves_event_expiry_outcome() {
        let mut prices = HashMap::new();
        prices.insert(("event-a".to_string(), "up-token".to_string()), dec!(1));
        prices.insert(("event-a".to_string(), "down-token".to_string()), dec!(0));
        prices.insert(("event-b".to_string(), "up-token".to_string()), dec!(0));
        prices.insert(("event-b".to_string(), "down-token".to_string()), dec!(1));

        assert_eq!(
            resolve_up_won_from_settlements(&prices, "event-a", "up-token", "down-token"),
            Some(true)
        );
        assert_eq!(
            resolve_up_won_from_settlements(&prices, "event-b", "up-token", "down-token"),
            Some(false)
        );
    }

    #[test]
    fn official_settlement_resolution_requires_token_evidence() {
        let mut prices = HashMap::new();
        prices.insert(("event-a".to_string(), "up-token".to_string()), dec!(1));
        prices.insert(("event-b".to_string(), "up-token".to_string()), dec!(0.5));
        prices.insert(("event-b".to_string(), "down-token".to_string()), dec!(0.5));

        assert_eq!(
            resolve_up_won_from_settlements(&prices, "event-a", "up-token", "down-token"),
            Some(true)
        );
        assert_eq!(
            resolve_up_won_from_settlements(&prices, "event-b", "up-token", "down-token"),
            None
        );
        assert_eq!(
            resolve_up_won_from_settlements(&prices, "event-c", "up-token", "down-token"),
            None
        );
    }

    #[test]
    fn sql_keeps_lob_and_aggtrade_full_cadence_but_filters_pm_quotes() {
        let source = include_str!("parquet_stream.rs");
        let agg_section = section_between(source, "// Agg trades", "// LOB");
        let lob_section = section_between(source, "// LOB", "// PM quotes");
        let quote_section = section_between(source, "// PM quotes", "if parts.is_empty()");

        for section in [agg_section, lob_section] {
            let lower = section.to_ascii_lowercase();
            assert!(!lower.contains("date_trunc"));
            assert!(!lower.contains("distinct on"));
            assert!(!lower.contains("sample"));
            assert!(!lower.contains("bucket"));
        }
        assert!(agg_section.contains("agg_trade_id"));
        assert!(lob_section.contains("event_time"));
        assert!(quote_section.contains(
            "source IN ('polymarket_ws', 'polymarket_ws_collector', 'ploy_runner_live')"
        ));
        assert!(quote_section.contains("best_bid IS NOT NULL AND best_ask IS NOT NULL"));
        assert!(quote_section.contains("CAST(bid_size AS DOUBLE) AS f3"));
        assert!(quote_section.contains("CAST(ask_size AS DOUBLE) AS f4"));
        assert!(quote_section.contains("CASE WHEN ask_size IS NOT NULL AND ask_size > 0"));
        assert!(quote_section.contains("CASE WHEN bid_size IS NOT NULL AND bid_size > 0"));
        assert!(source.contains("pm_token_settlements"));
        assert!(source.contains("if require_official_settlement && resolved_up_won.is_none()"));
        assert!(source.contains("official settlement coverage is incomplete"));
        assert!(source.contains("resolved_event_rows != discovered_event_rows"));
    }

    fn section_between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
        let start_idx = source.find(start).expect("start marker");
        let tail = &source[start_idx..];
        let end_idx = tail.find(end).expect("end marker");
        &tail[..end_idx]
    }
}
