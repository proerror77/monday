//! Database-backed historical feed loader.
//!
//! Loads market data from PostgreSQL into a `Vec<MarketUpdate>` that
//! can be wrapped in [`HistoricalFeed`] for backtesting.
//!
//! Kept outside `ploy-strategy-bundles` so strategy logic can compile without
//! SQLx when only synthetic, recorded, or live feeds are needed.
//!
//! # Tables Queried
//!
//! | Table | Data |
//! |-------|------|
//! | `binance_price_ticks` | CEX spot prices |
//! | `binance_agg_trade_ticks` | CEX aggregated trades |
//! | `binance_lob_ticks` | L2 orderbook (OBI, spread) |
//! | `clob_quote_ticks` | Polymarket quotes |
//! | `pm_market_metadata` | Event windows (UP/DOWN tokens) |
//! | `pm_token_settlements` | Settlement outcomes |

use chrono::{DateTime, Duration, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::info;

use ploy_market_contracts::{
    l2_updates_from_depth_totals, market_update_sort_ts, normalize_token_id, BinanceSourceClock,
    BinanceSourceKind, BookLevel, HistoricalLoadOptions, MarketUpdate,
};

use serde_json::Value;

/// How far before `from` to load spot prices for EWMA volatility warm-up.
const WARMUP_MINUTES: i64 = 30;

const BINANCE_PRICE_SAMPLED_QUERY: &str = r#"
        WITH buckets AS (
            SELECT s.symbol, bucket_start
            FROM unnest($1::text[]) AS s(symbol)
            CROSS JOIN generate_series(
                $2::timestamptz,
                $3::timestamptz,
                make_interval(secs => $4::int)
            ) AS bucket_start
        )
        SELECT spot.received_at, spot.trade_time, spot.symbol, spot.price
        FROM buckets
        JOIN LATERAL (
            SELECT received_at, trade_time, symbol, price
            FROM binance_price_ticks
            WHERE symbol = buckets.symbol
              AND trade_time >= buckets.bucket_start
              AND trade_time < buckets.bucket_start + make_interval(secs => $4::int)
              AND received_at <= $3
              AND received_at >= trade_time
              AND received_at - trade_time <= make_interval(secs => $5::int)
            ORDER BY trade_time DESC
            LIMIT 1
        ) AS spot ON true
        ORDER BY spot.received_at
        "#;

const BINANCE_AGG_TRADE_SAMPLED_QUERY: &str = r#"
        WITH buckets AS (
            SELECT s.symbol, bucket_start
            FROM unnest($1::text[]) AS s(symbol)
            CROSS JOIN generate_series(
                $2::timestamptz,
                $3::timestamptz,
                interval '5 seconds'
            ) AS bucket_start
        )
        SELECT agg.received_at, buckets.bucket_start, agg.source_time,
               agg.symbol, agg.agg_trade_id,
               agg.price, agg.quantity, agg.is_buyer_maker
        FROM buckets
        JOIN LATERAL (
            SELECT
                MAX(received_at) AS received_at,
                MAX(trade_time) AS source_time,
                symbol,
                MAX(agg_trade_id) AS agg_trade_id,
                SUM(price * quantity) / NULLIF(SUM(quantity), 0) AS price,
                SUM(quantity) AS quantity,
                is_buyer_maker
            FROM binance_agg_trade_ticks
            WHERE symbol = buckets.symbol
              AND trade_time >= buckets.bucket_start
              AND trade_time < buckets.bucket_start + interval '5 seconds'
              AND received_at <= $3
              AND received_at >= trade_time
              AND received_at - trade_time <= make_interval(secs => $4::int)
              AND quantity > 0
              AND price > 0
            GROUP BY symbol, is_buyer_maker
        ) AS agg ON true
        ORDER BY agg.received_at, agg.symbol, agg.is_buyer_maker
        "#;

const BINANCE_LOB_SAMPLED_QUERY: &str = r#"
        WITH buckets AS (
            SELECT s.symbol, bucket_start
            FROM unnest($1::text[]) AS s(symbol)
            CROSS JOIN generate_series(
                $2::timestamptz,
                $3::timestamptz,
                make_interval(secs => $4::int)
            ) AS bucket_start
        )
        SELECT lob.received_at, lob.event_time, lob.symbol,
               COALESCE(lob.obi_5, 0.0) as obi,
               COALESCE(lob.spread_bps, 0)::int as spread_bps,
               COALESCE(lob.bid_volume_5, 0) as bid_volume_5,
               COALESCE(lob.ask_volume_5, 0) as ask_volume_5
        FROM buckets
        JOIN LATERAL (
            SELECT received_at, event_time, symbol, obi_5, spread_bps, bid_volume_5, ask_volume_5
            FROM binance_lob_ticks
            WHERE symbol = buckets.symbol
              AND event_time >= buckets.bucket_start
              AND event_time < buckets.bucket_start + make_interval(secs => $4::int)
              AND received_at <= $3
              AND received_at >= event_time
              AND received_at - event_time <= make_interval(secs => $5::int)
            ORDER BY event_time DESC
            LIMIT 1
        ) AS lob ON true
        ORDER BY lob.received_at
        "#;

const REFERENCE_PRICE_TICKS_QUERY: &str = r#"
        WITH reference_ticks AS (
            SELECT
                lower(symbol) AS symbol,
                lower(source) AS source,
                asset_class,
                price,
                full_accuracy_value,
                price_time,
                received_at,
                is_carried_forward,
                0 AS source_rank,
                id AS row_id
            FROM reference_price_ticks
            WHERE lower(symbol) = ANY($1)
              AND received_at >= $2
              AND received_at <= $3

            UNION ALL

            SELECT
                lower(symbol) AS symbol,
                'chainlink'::text AS source,
                'crypto'::text AS asset_class,
                price,
                NULL::text AS full_accuracy_value,
                source_timestamp AS price_time,
                received_at,
                false AS is_carried_forward,
                1 AS source_rank,
                id AS row_id
            FROM chainlink_price_ticks
            WHERE lower(symbol) = ANY($1)
              AND received_at >= $2
              AND received_at <= $3
        ), ranked AS (
            SELECT
                symbol,
                source,
                asset_class,
                price,
                full_accuracy_value,
                price_time,
                received_at,
                is_carried_forward,
                row_number() OVER (
                    PARTITION BY symbol, source, price_time, price
                    ORDER BY received_at ASC, source_rank ASC, row_id ASC
                ) AS duplicate_rank
            FROM reference_ticks
        )
        SELECT
            symbol,
            source,
            asset_class,
            price,
            full_accuracy_value,
            price_time,
            received_at,
            is_carried_forward
        FROM ranked
        WHERE duplicate_rank = 1
        ORDER BY received_at, price_time, symbol, source, price
        "#;

/// Historical research backtests only trust canonical historical PM quote captures.
///
/// Older `ploy_runner_live` rows were synthetic midpoint quotes; newer rows carry
/// filtered top-of-book sizes from REST `/book`. The existing
/// `polymarket_ws_collector` history before the next validated cutover is too
/// polluted with placeholder `0.01/0.99` books to mix into research results.
/// Dry-run parity should use recorded replay mode instead of the historical DB path.
const TRUSTED_PM_RESEARCH_QUOTE_SOURCES: &[&str] = &[
    "polymarket_ws",
    "polymarket_ws_collector",
    "ploy_runner_live",
];

fn accept_monotonic_source_time(
    latest_by_symbol: &mut HashMap<String, DateTime<Utc>>,
    symbol: &str,
    source_time: DateTime<Utc>,
) -> bool {
    if latest_by_symbol
        .get(symbol)
        .is_some_and(|latest| source_time < *latest)
    {
        return false;
    }
    latest_by_symbol.insert(symbol.to_string(), source_time);
    true
}

/// One persisted reference-price tick from the canonical or legacy capture.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReferencePriceTick {
    pub symbol: String,
    pub source: String,
    pub asset_class: String,
    pub price: Decimal,
    pub full_accuracy_value: Option<String>,
    pub price_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub is_carried_forward: bool,
}

/// One persisted sports-state event from `sports_state_events`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SportsStateEventRow {
    pub game_id: String,
    pub league: String,
    pub slug: String,
    pub home_team: String,
    pub away_team: String,
    pub status: String,
    pub period: Option<String>,
    pub score: Option<String>,
    pub elapsed: Option<String>,
    pub live: bool,
    pub ended: bool,
    pub finished_at: Option<DateTime<Utc>>,
    pub event_time: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    pub source: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EventMetadataRow {
    market_slug: String,
    symbol: Option<String>,
    end_time: Option<DateTime<Utc>>,
    start_time: Option<DateTime<Utc>>,
    up_token_id: Option<String>,
    down_token_id: Option<String>,
    price_to_beat: Option<Decimal>,
}

#[derive(Debug, Clone)]
pub struct HistoricalLoadBatch {
    pub updates: Vec<MarketUpdate>,
    pub binance_source_clocks: Vec<BinanceSourceClock>,
}

/// Load all historical market updates for given symbols and time range.
///
/// Returns updates sorted by timestamp, ready for `HistoricalFeed::new()`.
///
/// Spot prices are loaded from `from - WARMUP_MINUTES` to give the EWMA
/// volatility estimator enough history before the first event arrives.
/// All other data (quotes, events, L2) uses the exact `from..to` range.
pub async fn load_from_database(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    load_from_database_with_options(pool, symbols, from, to, &HistoricalLoadOptions::default())
        .await
}

/// Load all historical market updates for given symbols and time range plus
/// any explicitly requested additive sources.
pub async fn load_from_database_with_options(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    Ok(
        load_from_database_with_options_and_source_clocks(pool, symbols, from, to, options)
            .await?
            .updates,
    )
}

/// Load replay updates plus the exchange/source clocks needed by governed
/// research freshness checks. Runtime consumers can use the compatibility
/// wrapper above when they do not need clock provenance.
pub async fn load_from_database_with_options_and_source_clocks(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    options: &HistoricalLoadOptions,
) -> Result<HistoricalLoadBatch, sqlx::Error> {
    let mut updates: Vec<MarketUpdate> = Vec::new();
    let mut binance_source_clocks = Vec::new();

    // 1. Spot prices — load earlier for EWMA warm-up
    let spot_from = from - Duration::minutes(WARMUP_MINUTES);
    load_spot_prices(
        pool,
        symbols,
        spot_from,
        to,
        options.spot_sample_secs,
        options.max_source_delay_secs,
        &mut updates,
        &mut binance_source_clocks,
    )
    .await?;

    // 1b. Aggregated trade flow — additive signal stream for roadmap work.
    load_agg_trades(
        pool,
        symbols,
        from,
        to,
        options.max_source_delay_secs,
        &mut updates,
        &mut binance_source_clocks,
    )
    .await?;

    // 2. Event windows FIRST — must be registered before quotes are processed.
    //    Quotes for a token can arrive before the event's official start_time
    //    (Polymarket tokens are continuous; ~90% of quotes precede start_time).
    //    Loading events before quotes ensures token_symbol mappings exist.
    load_events(
        pool,
        symbols,
        from,
        to,
        options.require_official_settlement,
        &mut updates,
    )
    .await?;

    // 3. Polymarket quotes — prefer sampled full-depth snapshots. Top-of-book
    // quote ticks are a diagnostic fallback only.
    let token_map = load_token_mappings(pool, symbols, from, to).await?;
    let snapshot_stats = load_pm_quotes_from_snapshots(
        pool,
        &token_map,
        from,
        to,
        options.lob_sample_secs,
        &mut updates,
    )
    .await?;
    if snapshot_stats.rows == 0 {
        info!("No usable orderbook snapshots found; using diagnostic top-of-book quotes");
        load_pm_quotes(pool, &token_map, from, to, &mut updates).await?;
    }

    // 4. L2 orderbook from binance_lob_ticks.
    //
    // Some research consumers load richer LOB snapshots separately and do not
    // need these generic updates. Keep the default enabled for strategy replay,
    // but allow research jobs to avoid scanning the large LOB table twice.
    if options.include_l2 {
        load_l2_data(
            pool,
            symbols,
            from,
            to,
            options.lob_sample_secs,
            options.max_source_delay_secs,
            &mut updates,
            &mut binance_source_clocks,
        )
        .await?;
    }

    if options.include_reference_prices {
        let reference_symbols = options.normalized_reference_symbols();
        if !reference_symbols.is_empty() {
            updates.extend(load_reference_price_updates(pool, &reference_symbols, from, to).await?);
        }
    }

    if options.include_sports_state {
        updates.extend(load_sports_state_events(pool, from, to).await?);
    }

    // Sort by timestamp
    updates.sort_by_key(market_update_sort_ts);

    info!(
        count = updates.len(),
        symbols = ?symbols,
        reference_symbols = ?options.normalized_reference_symbols(),
        include_reference_prices = options.include_reference_prices,
        include_sports_state = options.include_sports_state,
        from = %from,
        to = %to,
        "Loaded historical data from database",
    );

    binance_source_clocks.sort_by_key(|clock| {
        (
            clock.received_at,
            clock.source_ts,
            clock.kind,
            clock.symbol.clone(),
            clock.sequence_id,
        )
    });
    Ok(HistoricalLoadBatch {
        updates,
        binance_source_clocks,
    })
}

// ── Spot Prices ──────────────────────────────────────────

async fn load_spot_prices(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    max_source_delay_secs: u32,
    updates: &mut Vec<MarketUpdate>,
    source_clocks: &mut Vec<BinanceSourceClock>,
) -> Result<(), sqlx::Error> {
    let sample_secs = sample_secs.max(1) as i64;
    let rows: Vec<(DateTime<Utc>, DateTime<Utc>, String, Decimal)> =
        sqlx::query_as(BINANCE_PRICE_SAMPLED_QUERY)
            .bind(symbols)
            .bind(from)
            .bind(to)
            .bind(sample_secs)
            .bind(max_source_delay_secs as i32)
            .fetch_all(pool)
            .await?;

    info!(
        count = rows.len(),
        sample_secs, "Loaded spot prices from binance_price_ticks"
    );
    let mut latest_source_time = HashMap::new();
    for (received_at, trade_time, symbol, price) in rows {
        if !accept_monotonic_source_time(&mut latest_source_time, &symbol, trade_time) {
            continue;
        }
        source_clocks.push(BinanceSourceClock {
            kind: BinanceSourceKind::Spot,
            symbol: symbol.clone(),
            source_ts: trade_time,
            received_at,
            sequence_id: None,
        });
        updates.push(MarketUpdate::SpotPrice {
            symbol: Arc::from(symbol),
            price,
            ts: received_at,
        });
    }

    Ok(())
}

async fn load_agg_trades(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    max_source_delay_secs: u32,
    updates: &mut Vec<MarketUpdate>,
    source_clocks: &mut Vec<BinanceSourceClock>,
) -> Result<(), sqlx::Error> {
    // Aggregate to at most one row per aggressor side in each 5-second bucket.
    // AggTrade volume can reach millions of rows per hour; the strategy only
    // needs signed and gross flow, not tick-level granularity. Each aggregate
    // becomes replay-visible at the last constituent trade's received_at.
    let rows: Vec<(
        DateTime<Utc>,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
        i64,
        Decimal,
        Decimal,
        bool,
    )> = sqlx::query_as(BINANCE_AGG_TRADE_SAMPLED_QUERY)
        .bind(symbols)
        .bind(from)
        .bind(to)
        .bind(max_source_delay_secs as i32)
        .fetch_all(pool)
        .await?;

    info!(
        count = rows.len(),
        "Loaded agg trades from binance_agg_trade_ticks (5s side aggregates)"
    );
    let mut latest_source_time = HashMap::new();
    for (
        received_at,
        bucket_start,
        source_time,
        symbol,
        agg_trade_id,
        price,
        quantity,
        is_buyer_maker,
    ) in rows
    {
        if !accept_monotonic_source_time(&mut latest_source_time, &symbol, bucket_start) {
            continue;
        }
        source_clocks.push(BinanceSourceClock {
            kind: BinanceSourceKind::AggTrade,
            symbol: symbol.clone(),
            source_ts: source_time,
            received_at,
            sequence_id: Some(agg_trade_id as u64),
        });
        updates.push(MarketUpdate::AggTrade {
            symbol: Arc::from(symbol),
            agg_trade_id: agg_trade_id as u64,
            price,
            quantity,
            is_buyer_maker,
            ts: received_at,
        });
    }

    Ok(())
}

/// Load canonical reference ticks plus the legacy Chainlink capture.
pub async fn load_reference_price_ticks(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<ReferencePriceTick>, sqlx::Error> {
    sqlx::query_as(REFERENCE_PRICE_TICKS_QUERY)
        .bind(
            &symbols
                .iter()
                .map(|symbol| symbol.trim().to_lowercase())
                .collect::<Vec<_>>(),
        )
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
}

/// Load reference-price ticks as canonical `MarketUpdate` values.
pub async fn load_reference_price_updates(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    let rows = load_reference_price_ticks(pool, symbols, from, to).await?;

    info!(
        count = rows.len(),
        symbols = ?symbols,
        "Loaded reference prices from canonical and legacy captures"
    );

    Ok(rows
        .into_iter()
        .map(|row| MarketUpdate::ReferencePrice {
            symbol: Arc::from(row.symbol),
            source: Arc::from(row.source),
            asset_class: Arc::from(row.asset_class),
            price: row.price,
            full_accuracy_value: row.full_accuracy_value.map(Arc::from),
            is_carried_forward: row.is_carried_forward,
            received_at: Some(row.received_at),
            ts: row.price_time,
        })
        .collect())
}

/// Load normalized sports-state events from `sports_state_events`.
pub async fn load_sports_state_events(
    pool: &PgPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<MarketUpdate>, sqlx::Error> {
    let rows: Vec<SportsStateEventRow> = sqlx::query_as(
        r#"
        SELECT
            game_id,
            league,
            slug,
            home_team,
            away_team,
            status,
            period,
            score,
            elapsed,
            live,
            ended,
            finished_at,
            event_time,
            received_at,
            source
        FROM sports_state_events
        WHERE event_time >= $1
          AND event_time <= $2
        ORDER BY event_time, received_at, id
        "#,
    )
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    info!(
        count = rows.len(),
        "Loaded sports state from sports_state_events"
    );

    Ok(rows
        .into_iter()
        .map(|row| MarketUpdate::SportsState {
            game_id: Arc::from(row.game_id),
            league: Arc::from(row.league),
            slug: Arc::from(row.slug),
            home_team: Arc::from(row.home_team),
            away_team: Arc::from(row.away_team),
            status: Arc::from(row.status),
            period: row.period.map(Arc::from),
            score: row.score.map(Arc::from),
            elapsed: row.elapsed.map(Arc::from),
            live: row.live,
            ended: row.ended,
            finished_at: row.finished_at,
            ts: row.event_time,
        })
        .collect())
}

// ── Token Mappings ───────────────────────────────────────

/// Map of token_id → (symbol, is_up_token)
type TokenMap = std::collections::HashMap<String, (String, bool)>;

#[derive(Debug, Default, Clone, Copy)]
struct PmQuoteLoadStats {
    rows: usize,
}

async fn load_token_mappings(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<TokenMap, sqlx::Error> {
    let mut map = TokenMap::new();

    // Extract token IDs from JSONB structure: raw_market->'markets'->0->>'clobTokenIds'
    // clobTokenIds is stored as a JSON string, so we use ->> then cast to jsonb
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        r#"
        SELECT
            market_slug,
            token_id,
            symbol
        FROM pm_market_metadata
        CROSS JOIN LATERAL (
            SELECT jsonb_array_elements_text(
                (raw_market->'markets'->0->>'clobTokenIds')::jsonb
            ) as token_id
        ) t
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND end_time >= $2
          AND start_time <= $3
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for (_slug, token_id, symbol) in rows {
        let token_id = normalize_token_id(&token_id);
        let sym = symbol.unwrap_or_default();
        // Heuristic: UP tokens tend to come first in the array
        let is_up = !map.contains_key(&token_id);
        map.insert(token_id, (sym, is_up));
    }

    info!(tokens = map.len(), "Loaded token mappings");
    Ok(map)
}

// ── PM Quotes ────────────────────────────────────────────

async fn load_pm_quotes(
    pool: &PgPool,
    token_map: &TokenMap,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    updates: &mut Vec<MarketUpdate>,
) -> Result<PmQuoteLoadStats, sqlx::Error> {
    let token_ids: Vec<String> = token_map.keys().cloned().collect();
    if token_ids.is_empty() {
        return Ok(PmQuoteLoadStats::default());
    }
    let trusted_sources: Vec<&str> = TRUSTED_PM_RESEARCH_QUOTE_SOURCES.to_vec();

    // Polymarket UP/DOWN token structure:
    // - UP token:   best_bid is the real price, best_ask may be NULL
    // - DOWN token: best_ask is the real price, best_bid may be NULL
    // Accept rows where EITHER bid OR ask is in the real price range (0.01, 0.99).
    // Within each second, prefer book rows with executable size over later price-only
    // best_bid_ask/price_change rows; otherwise LOB-aware replay sees valid prices
    // but no executable top-of-book liquidity.
    let rows: Vec<(
        DateTime<Utc>,
        String,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
    )> = sqlx::query_as(
        r#"
        SELECT DISTINCT ON (date_trunc('second', received_at), token_id)
               received_at, token_id, best_bid, best_ask, bid_size, ask_size
        FROM clob_quote_ticks
        WHERE received_at >= $1
          AND received_at <= $2
          AND token_id = ANY($3)
          AND source = ANY($4)
          AND (
            (best_bid  IS NOT NULL AND best_bid  > 0.01 AND best_bid  < 0.99)
            OR
            (best_ask  IS NOT NULL AND best_ask  > 0.01 AND best_ask  < 0.99)
          )
        ORDER BY date_trunc('second', received_at), token_id,
                 (ask_size IS NOT NULL AND ask_size > 0)::int DESC,
                 (bid_size IS NOT NULL AND bid_size > 0)::int DESC,
                 received_at DESC
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&token_ids)
    .bind(&trusted_sources)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let sized_rows = rows
        .iter()
        .filter(|(_, _, _, _, bid_size, ask_size)| {
            bid_size.is_some_and(|size| size > Decimal::ZERO)
                || ask_size.is_some_and(|size| size > Decimal::ZERO)
        })
        .count();

    info!(
        count = rows.len(),
        sized_rows,
        sources = ?TRUSTED_PM_RESEARCH_QUOTE_SOURCES,
        "Loaded PM quotes from clob_quote_ticks (trusted sources, filtered: bid/ask in 0.02-0.98)"
    );
    let row_count = rows.len();
    for (ts, token_id, bid, ask, bid_size, ask_size) in rows {
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
    }

    Ok(PmQuoteLoadStats { rows: row_count })
}

/// Fallback quote loader from `clob_orderbook_snapshots`.
///
/// Polymarket CLOB structure for UP/DOWN binary markets:
/// - UP token:   bids array has full depth (0.01→0.99), asks is empty []
/// - DOWN token: asks array has full depth (0.99→0.01), bids is empty []
///
/// So best_bid = MAX(bids prices), best_ask = MIN(asks prices).
/// For UP tokens: best_bid is the real market price.
/// For DOWN tokens: best_ask is the real market price (= 1 - UP price).
async fn load_pm_quotes_from_snapshots(
    pool: &PgPool,
    token_map: &TokenMap,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    updates: &mut Vec<MarketUpdate>,
) -> Result<PmQuoteLoadStats, sqlx::Error> {
    let token_ids: Vec<String> = token_map.keys().cloned().collect();
    if token_ids.is_empty() {
        return Ok(PmQuoteLoadStats::default());
    }
    let sample_secs = sample_secs.max(1) as i64;

    // Downsample before transferring JSONB so full-day reviews do not load every
    // snapshot. Preserve every level in each selected book.
    let rows: Vec<(DateTime<Utc>, String, Value, Value)> = sqlx::query_as(
        r#"
        WITH sampled AS (
            SELECT DISTINCT ON (snapshot.token_id, snapshot.bucket)
                   snapshot.received_at,
                   snapshot.token_id,
                   snapshot.bids,
                   snapshot.asks
            FROM (
                SELECT
                    received_at,
                    token_id,
                    bids,
                    asks,
                    to_timestamp(
                        floor(EXTRACT(EPOCH FROM received_at) / $4::double precision) * $4
                    ) AS bucket
                FROM clob_orderbook_snapshots
                WHERE received_at >= $1
                  AND received_at <= $2
                  AND token_id = ANY($3)
            ) snapshot
            ORDER BY snapshot.token_id, snapshot.bucket, snapshot.received_at DESC
        )
        SELECT received_at, token_id, bids, asks
        FROM sampled
        WHERE jsonb_array_length(bids) > 0 OR jsonb_array_length(asks) > 0
        ORDER BY received_at
        "#,
    )
    .bind(from)
    .bind(to)
    .bind(&token_ids)
    .bind(sample_secs)
    .fetch_all(pool)
    .await?;

    let mut row_count = 0usize;
    for (ts, token_id, bids, asks) in rows {
        let bid_levels = book_levels_from_json(&bids, false);
        let ask_levels = book_levels_from_json(&asks, true);
        if bid_levels.is_empty() && ask_levels.is_empty() {
            continue;
        }
        let best_bid = bid_levels.first();
        let best_ask = ask_levels.first();
        updates.push(MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid: best_bid.map(|level| level.price),
            ask: best_ask.map(|level| level.price),
            bid_size: best_bid.map(|level| level.size),
            ask_size: best_ask.map(|level| level.size),
            bid_levels,
            ask_levels,
            ts,
        });
        row_count += 1;
    }

    info!(
        count = row_count,
        sample_secs, "Loaded full-depth PM quotes from clob_orderbook_snapshots"
    );

    Ok(PmQuoteLoadStats { rows: row_count })
}

fn book_levels_from_json(value: &Value, ascending: bool) -> Vec<BookLevel> {
    let mut levels = value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| match item {
            Value::Array(parts) if parts.len() >= 2 => {
                Some((decimal_from_json(&parts[0])?, decimal_from_json(&parts[1])?))
            }
            Value::Object(parts) => Some((
                decimal_from_json(parts.get("price")?)?,
                decimal_from_json(parts.get("size")?)?,
            )),
            _ => None,
        })
        .filter(|(price, size)| {
            *price > Decimal::new(1, 2) && *price < Decimal::new(99, 2) && *size > Decimal::ZERO
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

fn decimal_from_json(value: &Value) -> Option<Decimal> {
    match value {
        Value::Number(number) => number.to_string().parse().ok(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

// ── L2 Data ──────────────────────────────────────────────

async fn load_l2_data(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    sample_secs: u32,
    max_source_delay_secs: u32,
    updates: &mut Vec<MarketUpdate>,
    source_clocks: &mut Vec<BinanceSourceClock>,
) -> Result<(), sqlx::Error> {
    let sample_secs = sample_secs.max(1) as i64;
    let rows: Vec<(
        DateTime<Utc>,
        DateTime<Utc>,
        String,
        Decimal,
        i32,
        Decimal,
        Decimal,
    )> = match sqlx::query_as(BINANCE_LOB_SAMPLED_QUERY)
        .bind(symbols)
        .bind(from)
        .bind(to)
        .bind(sample_secs)
        .bind(max_source_delay_secs as i32)
        .fetch_all(pool)
        .await
    {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "Failed to load L2 data from binance_lob_ticks");
            Vec::new()
        }
    };

    info!(count = rows.len(), "Loaded L2 data from binance_lob_ticks");
    let mut latest_source_time = HashMap::new();
    for (received_at, event_time, symbol, obi, spread_bps, bid_volume_5, ask_volume_5) in rows {
        if !accept_monotonic_source_time(&mut latest_source_time, &symbol, event_time) {
            continue;
        }
        source_clocks.push(BinanceSourceClock {
            kind: BinanceSourceKind::L2,
            symbol: symbol.clone(),
            source_ts: event_time,
            received_at,
            sequence_id: None,
        });
        updates.extend(l2_updates_from_depth_totals(
            &symbol,
            obi.to_f64().unwrap_or_default(),
            spread_bps as u32,
            bid_volume_5,
            ask_volume_5,
            received_at,
        ));
    }

    Ok(())
}

// near_depth / sum_depth_in_range / parse_depth_level / json_f64 are only
// used in tests via l2_updates_from_book.
#[cfg(test)]
const NEAR_DEPTH_PCT_RANGE: f64 = 0.001;

#[cfg(test)]
fn l2_updates_from_book(
    symbol: &str,
    obi: f64,
    spread_bps: u32,
    mid_price: Decimal,
    bids: Option<&Value>,
    asks: Option<&Value>,
    ts: DateTime<Utc>,
) -> Vec<MarketUpdate> {
    let sym: Arc<str> = Arc::from(symbol);
    let mut updates = vec![MarketUpdate::L2 {
        symbol: Arc::clone(&sym),
        obi,
        spread_bps,
        ts,
    }];

    if bids.is_none() && asks.is_none() {
        return updates;
    }

    let Some(mid_price) = mid_price.to_f64() else {
        return updates;
    };
    if !mid_price.is_finite() || mid_price <= 0.0 {
        return updates;
    }

    let empty = Value::Null;
    let (bid_depth_near, ask_depth_near) = near_depth(
        bids.unwrap_or(&empty),
        asks.unwrap_or(&empty),
        mid_price,
        NEAR_DEPTH_PCT_RANGE,
    );

    updates.push(MarketUpdate::L2Depth {
        symbol: sym,
        obi,
        spread_bps,
        bid_depth_near,
        ask_depth_near,
        ts,
    });

    updates
}

#[cfg(test)]
fn near_depth(bids: &Value, asks: &Value, mid_price: f64, pct_range: f64) -> (f64, f64) {
    if !mid_price.is_finite() || mid_price <= 0.0 || !pct_range.is_finite() || pct_range < 0.0 {
        return (0.0, 0.0);
    }

    let bid_min = mid_price * (1.0 - pct_range);
    let ask_max = mid_price * (1.0 + pct_range);

    (
        sum_depth_in_range(bids, bid_min, mid_price),
        sum_depth_in_range(asks, mid_price, ask_max),
    )
}

#[cfg(test)]
fn sum_depth_in_range(levels: &Value, min_price: f64, max_price: f64) -> f64 {
    levels
        .as_array()
        .map(|levels| {
            levels
                .iter()
                .filter_map(parse_depth_level)
                .filter(|(price, _)| *price >= min_price && *price <= max_price)
                .map(|(_, size)| size)
                .sum()
        })
        .unwrap_or(0.0)
}

#[cfg(test)]
fn parse_depth_level(level: &Value) -> Option<(f64, f64)> {
    match level {
        Value::Array(items) if items.len() >= 2 => {
            Some((json_f64(&items[0])?, json_f64(&items[1])?))
        }
        Value::Object(map) => Some((json_f64(map.get("price")?)?, json_f64(map.get("size")?)?)),
        _ => None,
    }
}

#[cfg(test)]
fn json_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

// ── Events ───────────────────────────────────────────────

async fn load_events(
    pool: &PgPool,
    symbols: &[String],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    require_official_settlement: bool,
    updates: &mut Vec<MarketUpdate>,
) -> Result<(), sqlx::Error> {
    // Extract token IDs from JSONB: raw_market->'markets'->0->>'clobTokenIds'
    // clobTokenIds is stored as a JSON string, so we use ->> then cast to jsonb
    // First token is UP, second is DOWN
    let rows: Vec<EventMetadataRow> = sqlx::query_as(
        r#"
        SELECT
            market_slug,
            symbol,
            end_time,
            start_time,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text AS up_token_id,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text AS down_token_id,
            price_to_beat
        FROM pm_market_metadata
        WHERE ($1::text[] IS NULL OR symbol = ANY($1))
          AND end_time >= $2
          AND start_time <= $3
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY start_time
        "#,
    )
    .bind(symbols)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let event_ids: Vec<String> = rows.iter().map(|row| row.market_slug.clone()).collect();
    let settlement_prices = load_event_settlement_prices(pool, &event_ids).await?;
    if require_official_settlement {
        let (discovered, resolved) = official_settlement_coverage(&rows, &settlement_prices);
        if resolved != discovered {
            return Err(sqlx::Error::Protocol(format!(
                "official settlement coverage is incomplete: resolved={resolved}, discovered={discovered}"
            )));
        }
    }

    info!(count = rows.len(), "Loaded events from pm_market_metadata");
    updates.extend(build_event_updates(
        rows,
        &settlement_prices,
        require_official_settlement,
    ));

    Ok(())
}

fn official_settlement_coverage(
    rows: &[EventMetadataRow],
    settlement_prices: &HashMap<(String, String), Decimal>,
) -> (usize, usize) {
    let mut discovered = 0usize;
    let mut resolved = 0usize;
    for row in rows {
        let symbol = row.symbol.as_deref().unwrap_or_default();
        let up_token = normalize_token_id(row.up_token_id.as_deref().unwrap_or_default());
        let down_token = normalize_token_id(row.down_token_id.as_deref().unwrap_or_default());
        if symbol.is_empty()
            || row.start_time.is_none()
            || row.end_time.is_none()
            || up_token.is_empty()
            || down_token.is_empty()
        {
            continue;
        }
        discovered += 1;
        if resolve_up_won_from_settlements(
            settlement_prices,
            &row.market_slug,
            &up_token,
            &down_token,
        )
        .is_some()
        {
            resolved += 1;
        }
    }
    (discovered, resolved)
}

fn build_event_updates(
    rows: Vec<EventMetadataRow>,
    settlement_prices: &HashMap<(String, String), Decimal>,
    require_official_settlement: bool,
) -> Vec<MarketUpdate> {
    let mut updates = Vec::new();

    for row in rows {
        let symbol = row.symbol.unwrap_or_default();
        let end_time = match row.end_time {
            Some(t) => t,
            None => continue,
        };
        let start_time = match row.start_time {
            Some(t) => t,
            None => continue,
        };

        let up_token: Arc<str> =
            Arc::from(normalize_token_id(&row.up_token_id.unwrap_or_default()));
        let down_token: Arc<str> =
            Arc::from(normalize_token_id(&row.down_token_id.unwrap_or_default()));
        let resolved_up_won = resolve_up_won_from_settlements(
            settlement_prices,
            &row.market_slug,
            &up_token,
            &down_token,
        );
        if require_official_settlement && resolved_up_won.is_none() {
            continue;
        }
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        if symbol.is_empty() || up_token.is_empty() || down_token.is_empty() {
            continue;
        }

        let event_id: Arc<str> = Arc::from(row.market_slug);
        let symbol: Arc<str> = Arc::from(symbol);

        updates.push(MarketUpdate::EventDiscovered {
            event_id: Arc::clone(&event_id),
            symbol,
            up_token,
            down_token,
            end_time,
            window_secs,
            price_to_beat: row.price_to_beat,
            resolved_up_won: None,
        });
        updates.push(MarketUpdate::EventExpired {
            event_id,
            end_time,
            resolved_up_won,
        });
    }

    updates
}

fn resolve_up_won_from_settlements(
    settlement_prices: &HashMap<(String, String), Decimal>,
    event_id: &str,
    up_token: &str,
    down_token: &str,
) -> Option<bool> {
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

async fn load_event_settlement_prices(
    pool: &PgPool,
    event_ids: &[String],
) -> Result<HashMap<(String, String), Decimal>, sqlx::Error> {
    if event_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows: Vec<(Option<String>, String, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT market_slug, token_id, settled_price
        FROM pm_token_settlements
        WHERE market_slug = ANY($1)
          AND resolved = TRUE
        "#,
    )
    .bind(event_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut prices = HashMap::new();
    for (market_slug, token_id, settled_price) in rows {
        let Some(market_slug) = market_slug else {
            continue;
        };
        let Some(settled_price) = settled_price else {
            continue;
        };
        prices.insert((market_slug, normalize_token_id(&token_id)), settled_price);
    }

    Ok(prices)
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use rust_decimal_macros::dec;
    use serde_json::json;
    use std::collections::HashMap;

    use super::{
        accept_monotonic_source_time, book_levels_from_json, build_event_updates,
        l2_updates_from_book, near_depth, official_settlement_coverage, EventMetadataRow,
        MarketUpdate, BINANCE_AGG_TRADE_SAMPLED_QUERY, BINANCE_LOB_SAMPLED_QUERY,
        BINANCE_PRICE_SAMPLED_QUERY, REFERENCE_PRICE_TICKS_QUERY,
    };

    #[test]
    fn polymarket_depth_levels_are_decimal_sorted_and_tradeable() {
        let bids = json!([["0.48", "10"], ["0.49", "5"], ["0.01", "999"]]);
        let asks = json!([
            {"price": "0.52", "size": "8"},
            {"price": "0.51", "size": "7"}
        ]);

        let bids = book_levels_from_json(&bids, false);
        let asks = book_levels_from_json(&asks, true);

        assert_eq!(bids[0].price, dec!(0.49));
        assert_eq!(bids[0].size, dec!(5));
        assert_eq!(bids.len(), 2);
        assert_eq!(asks[0].price, dec!(0.51));
        assert_eq!(asks[0].size, dec!(7));
    }

    #[test]
    fn full_depth_snapshot_query_does_not_separate_cte_with_a_dangling_comma() {
        let source = include_str!("database.rs");
        let query = &source[source
            .find("async fn load_pm_quotes_from_snapshots")
            .unwrap()..];
        let query = &query[..query.find("let mut row_count").unwrap()];
        assert!(!query.contains(
            "ORDER BY snapshot.token_id, snapshot.bucket, snapshot.received_at DESC\n        ),\n        SELECT received_at, token_id, bids, asks"
        ));
        assert!(query.contains(
            "ORDER BY snapshot.token_id, snapshot.bucket, snapshot.received_at DESC\n        )\n        SELECT received_at, token_id, bids, asks"
        ));
        assert!(!query.contains(".fetch_all(pool)\n    .await\n    .unwrap_or_default()"));
    }

    #[test]
    fn binance_samplers_use_bucketed_index_lookups() {
        for query in [
            BINANCE_PRICE_SAMPLED_QUERY,
            BINANCE_AGG_TRADE_SAMPLED_QUERY,
            BINANCE_LOB_SAMPLED_QUERY,
        ] {
            assert!(
                query.contains("generate_series"),
                "sampled Binance loaders should generate bounded buckets"
            );
            assert!(
                query.contains("JOIN LATERAL"),
                "sampled Binance loaders should perform one indexable lookup per bucket"
            );
            assert!(
                !query.contains("DISTINCT ON"),
                "sampled Binance loaders must not sort full time ranges by bucket"
            );
            assert!(
                !query.contains("EXTRACT(EPOCH FROM"),
                "sampled Binance loaders should avoid computed bucket expressions over full tables"
            );
        }

        assert!(BINANCE_PRICE_SAMPLED_QUERY.contains("trade_time >= buckets.bucket_start"));
        assert!(BINANCE_PRICE_SAMPLED_QUERY.contains("ORDER BY trade_time DESC"));
        assert!(BINANCE_AGG_TRADE_SAMPLED_QUERY.contains("trade_time >= buckets.bucket_start"));
        assert!(BINANCE_LOB_SAMPLED_QUERY.contains("event_time >= buckets.bucket_start"));
        assert!(BINANCE_LOB_SAMPLED_QUERY
            .contains("event_time < buckets.bucket_start + make_interval(secs => $4::int)"));
        assert!(BINANCE_LOB_SAMPLED_QUERY.contains("ORDER BY event_time DESC"));
    }

    #[test]
    fn binance_agg_trade_sampler_preserves_two_sided_bucket_flow() {
        let query = BINANCE_AGG_TRADE_SAMPLED_QUERY
            .split_whitespace()
            .collect::<String>();

        assert!(query.contains("GROUPBYsymbol,is_buyer_maker"));
        assert!(query.contains("SUM(quantity)ASquantity"));
        assert!(query.contains("SUM(price*quantity)/NULLIF(SUM(quantity),0)ASprice"));
        assert!(query.contains("MAX(received_at)ASreceived_at"));
        assert!(query.contains("MAX(trade_time)ASsource_time"));
        assert!(query.contains("MAX(agg_trade_id)ASagg_trade_id"));
        assert!(query.contains("ANDquantity>0"));
        assert!(query.contains("ANDprice>0"));
        assert!(!query.contains("LIMIT1"));
    }

    #[test]
    fn binance_replay_uses_canonical_tables_and_arrival_time() {
        let source = include_str!("database.rs");
        let legacy_source = ["sync", "_records"].concat();
        assert!(!source.contains(&legacy_source));

        for query in [
            BINANCE_PRICE_SAMPLED_QUERY,
            BINANCE_AGG_TRADE_SAMPLED_QUERY,
            BINANCE_LOB_SAMPLED_QUERY,
        ] {
            let query = query.split_whitespace().collect::<String>();
            assert!(query.contains("SELECT") && query.contains("received_at"));
            assert!(query.contains("received_at<=$3"));
            assert!(query
                .rsplit_once("ORDERBY")
                .is_some_and(|(_, order)| order.contains("received_at")));
        }
    }

    #[test]
    fn binance_replay_rejects_delayed_or_clock_reversed_source_rows() {
        for (query, source_ts, delay_parameter) in [
            (BINANCE_PRICE_SAMPLED_QUERY, "trade_time", "$5"),
            (BINANCE_AGG_TRADE_SAMPLED_QUERY, "trade_time", "$4"),
            (BINANCE_LOB_SAMPLED_QUERY, "event_time", "$5"),
        ] {
            let query = query.split_whitespace().collect::<String>();
            assert!(query.contains(&format!("received_at>={source_ts}")));
            assert!(query.contains(&format!(
                "received_at-{source_ts}<=make_interval(secs=>{delay_parameter}::int)"
            )));
        }
    }

    #[test]
    fn binance_replay_rejects_bounded_out_of_order_source_time() {
        let base = Utc::now();
        let mut latest = HashMap::new();

        assert!(accept_monotonic_source_time(
            &mut latest,
            "BTCUSDT",
            base + Duration::seconds(10)
        ));
        assert!(!accept_monotonic_source_time(&mut latest, "BTCUSDT", base));
        assert!(accept_monotonic_source_time(
            &mut latest,
            "BTCUSDT",
            base + Duration::seconds(10)
        ));
        assert!(accept_monotonic_source_time(
            &mut latest,
            "BTCUSDT",
            base + Duration::seconds(20)
        ));
        assert!(accept_monotonic_source_time(&mut latest, "SOLUSDT", base));
    }

    #[test]
    fn reference_price_query_preserves_source_and_arrival_time() {
        let query = REFERENCE_PRICE_TICKS_QUERY
            .split_whitespace()
            .collect::<String>();

        assert!(query.contains("source_timestampASprice_time"));
        assert_eq!(query.matches("received_at>=$2").count(), 2);
        assert!(query.contains("PARTITIONBYsymbol,source,price_time,price"));
        assert!(query.contains("ORDERBYreceived_atASC,source_rankASC,row_idASC"));
        assert!(query.ends_with("ORDERBYreceived_at,price_time,symbol,source,price"));
    }

    #[test]
    fn official_settlement_backfill_repairs_event_expiry_outcome() {
        let now = Utc::now();
        let row = EventMetadataRow {
            market_slug: "evt-1".into(),
            symbol: Some("BTCUSDT".into()),
            end_time: Some(now + Duration::minutes(5)),
            start_time: Some(now),
            up_token_id: Some("up-1".into()),
            down_token_id: Some("down-1".into()),
            price_to_beat: Some(dec!(100000)),
        };

        let initial = build_event_updates(vec![row.clone()], &HashMap::new(), false);
        assert!(matches!(
            initial.last(),
            Some(MarketUpdate::EventExpired {
                resolved_up_won: None,
                ..
            })
        ));

        let mut settlement_prices = HashMap::new();
        settlement_prices.insert(("evt-1".to_string(), "up-1".to_string()), dec!(1));
        settlement_prices.insert(("evt-1".to_string(), "down-1".to_string()), dec!(0));

        let repaired = build_event_updates(vec![row], &settlement_prices, false);
        assert!(matches!(
            repaired.last(),
            Some(MarketUpdate::EventExpired {
                resolved_up_won: Some(true),
                ..
            })
        ));
    }

    #[test]
    fn official_only_backtest_skips_unresolved_events() {
        let now = Utc::now();
        let row = EventMetadataRow {
            market_slug: "evt-1".into(),
            symbol: Some("BTCUSDT".into()),
            end_time: Some(now + Duration::minutes(5)),
            start_time: Some(now),
            up_token_id: Some("up-1".into()),
            down_token_id: Some("down-1".into()),
            price_to_beat: Some(dec!(100000)),
        };

        let updates = build_event_updates(vec![row], &HashMap::new(), true);
        assert!(
            updates.is_empty(),
            "official-only mode should skip unresolved events"
        );
    }

    #[test]
    fn official_settlement_coverage_detects_partial_event_windows() {
        let now = Utc::now();
        let rows = vec![
            EventMetadataRow {
                market_slug: "evt-1".into(),
                symbol: Some("BTCUSDT".into()),
                end_time: Some(now + Duration::minutes(5)),
                start_time: Some(now),
                up_token_id: Some("up-1".into()),
                down_token_id: Some("down-1".into()),
                price_to_beat: Some(dec!(100000)),
            },
            EventMetadataRow {
                market_slug: "evt-2".into(),
                symbol: Some("ETHUSDT".into()),
                end_time: Some(now + Duration::minutes(5)),
                start_time: Some(now),
                up_token_id: Some("up-2".into()),
                down_token_id: Some("down-2".into()),
                price_to_beat: Some(dec!(3000)),
            },
        ];
        let mut prices = HashMap::new();
        prices.insert(("evt-1".to_string(), "up-1".to_string()), dec!(1));
        prices.insert(("evt-1".to_string(), "down-1".to_string()), dec!(0));

        assert_eq!(official_settlement_coverage(&rows, &prices), (2, 1));
    }

    #[test]
    fn near_depth_sums_volume_within_mid_range_for_array_pairs() {
        let bids = json!([["100.0", "5.0"], ["99.95", "3.0"], ["99.85", "10.0"]]);
        let asks = json!([["100.05", "4.0"], ["100.10", "2.0"], ["100.25", "7.0"]]);

        let (bid_near, ask_near) = near_depth(&bids, &asks, 100.0, 0.001);

        assert!((bid_near - 8.0).abs() < 1e-9, "bid_near={bid_near}");
        assert!((ask_near - 6.0).abs() < 1e-9, "ask_near={ask_near}");
    }

    #[test]
    fn near_depth_supports_object_levels_and_l2_depth_variant() {
        let ts = Utc::now();
        let bids = json!([
            {"price": "100.0", "size": "1.5"},
            {"price": "99.91", "size": "2.5"},
            {"price": "99.70", "size": "9.0"}
        ]);
        let asks = json!([
            {"price": "100.03", "size": "4.5"},
            {"price": "100.09", "size": "1.0"},
            {"price": "100.30", "size": "8.0"}
        ]);

        let updates = l2_updates_from_book(
            "BTCUSDT",
            0.125,
            9,
            dec!(100.0),
            Some(&bids),
            Some(&asks),
            ts,
        );

        assert!(matches!(
            updates.first(),
            Some(MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts: update_ts
            }) if symbol.as_ref() == "BTCUSDT" && (obi - 0.125).abs() < f64::EPSILON && *spread_bps == 9 && *update_ts == ts
        ));

        assert!(matches!(
            updates.get(1),
            Some(MarketUpdate::L2Depth {
                symbol,
                obi,
                spread_bps,
                bid_depth_near,
                ask_depth_near,
                ts: update_ts
            }) if symbol.as_ref() == "BTCUSDT"
                && (obi - 0.125).abs() < f64::EPSILON
                && *spread_bps == 9
                && (bid_depth_near - 4.0).abs() < 1e-9
                && (ask_depth_near - 5.5).abs() < 1e-9
                && *update_ts == ts
        ));
    }
}
