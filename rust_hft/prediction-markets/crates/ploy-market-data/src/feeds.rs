//! Live market data feed producers.
//!
//! Async tasks that bridge venue WebSocket/REST streams into the unified
//! `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Timelike, Utc};
use futures::{SinkExt, StreamExt};
use ploy_market_contracts::{
    l2_updates_from_depth_totals, normalize_token_id, BookLevel, MarketUpdate,
};
use polymarket_client_sdk::clob::types::Side;
use polymarket_client_sdk::clob::ws::interest::MessageInterest;
use polymarket_client_sdk::clob::ws::types::request::SubscriptionRequest;
use polymarket_client_sdk::clob::ws::types::response::{
    parse_if_interested, BookUpdate, PriceChange, PriceChangeBatchEntry, WsMessage,
};
use polymarket_client_sdk::rtds::{Client as RtdsClient, Subscription};
use polymarket_client_sdk::types::U256;
use polymarket_client_sdk::ws::config::{Config as PolymarketWsConfig, ReconnectConfig};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::collector::POLYMARKET_CLOB_WS_ENDPOINT;
use crate::reference_prices::{
    infer_pyth_asset_class, market_symbol_to_binance_symbol, normalize_reference_symbol,
    parse_chainlink_twap_price, pyth_symbol, upsert_reference_price, ReferenceAssetClass,
    ReferencePriceKey, ReferencePriceRegistry, ReferencePriceSnapshot, ReferencePriceSource,
};

const POLYMARKET_RTDS_WS_ENDPOINT: &str = "wss://ws-live-data.polymarket.com";
const POLYMARKET_CLOB_HTTP_ENDPOINT: &str = "https://clob.polymarket.com";
const POLYMARKET_CLOB_FAILURE_CAPTURE_ENV: &str = "MONDAY_POLYMARKET_CLOB_FAILURE_CAPTURE_PATH";
const POLYMARKET_CLOB_CROSSED_BOOK_ERROR: &str =
    "Polymarket price-change batch produced a crossed book";
const MAX_POLYMARKET_CLOB_FAILURE_CAPTURE_BYTES: usize = 1_048_576;
// Deltas buffered per token between (re)subscription and the first book
// snapshot; exceeding the bound fails the token closed instead of growing
// without limit.
const MAX_POLYMARKET_CLOB_PENDING_CHANGES: usize = 256;
const NEAR_DEPTH_PCT_RANGE: f64 = 0.001;
const DB_POLYMARKET_SETTLEMENT_RETRY_LOOKBACK_SECS: i64 = 30 * 60;

fn rtds_market_data_ws_config() -> PolymarketWsConfig {
    let mut config = PolymarketWsConfig::default();
    // These feeds only need resilient market-data delivery. A wider heartbeat
    // window avoids unnecessary reconnect churn on transient stalls.
    config.heartbeat_interval = StdDuration::from_secs(15);
    config.heartbeat_timeout = StdDuration::from_secs(45);
    config.reconnect = ReconnectConfig::default();
    config
}

#[derive(Debug, Deserialize)]
struct RestBookLevel {
    price: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct RestBook {
    #[serde(default)]
    bids: Vec<RestBookLevel>,
    #[serde(default)]
    asks: Vec<RestBookLevel>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct BookQuote {
    bid: Option<Decimal>,
    ask: Option<Decimal>,
    bid_size: Option<Decimal>,
    ask_size: Option<Decimal>,
}

fn pm_tradeable_price(price: Decimal) -> bool {
    price > rust_decimal_macros::dec!(0.02) && price < rust_decimal_macros::dec!(0.98)
}

fn parse_rest_book_level(level: &RestBookLevel) -> Option<(Decimal, Decimal)> {
    let price = level.price.parse::<Decimal>().ok()?;
    let size = level.size.parse::<Decimal>().ok()?;
    if size <= Decimal::ZERO || !pm_tradeable_price(price) {
        return None;
    }
    Some((price, size))
}

fn best_tradeable_bid_level(levels: &[RestBookLevel]) -> Option<(Decimal, Decimal)> {
    levels
        .iter()
        .filter_map(parse_rest_book_level)
        .max_by(|left, right| left.0.cmp(&right.0))
}

fn best_tradeable_ask_level(levels: &[RestBookLevel]) -> Option<(Decimal, Decimal)> {
    levels
        .iter()
        .filter_map(parse_rest_book_level)
        .min_by(|left, right| left.0.cmp(&right.0))
}

fn book_quote_from_rest(book: &RestBook) -> BookQuote {
    let bid = best_tradeable_bid_level(&book.bids);
    let ask = best_tradeable_ask_level(&book.asks);

    BookQuote {
        bid: bid.map(|(price, _)| price),
        bid_size: bid.map(|(_, size)| size),
        ask: ask.map(|(price, _)| price),
        ask_size: ask.map(|(_, size)| size),
    }
}

fn book_levels_from_rest(levels: &[RestBookLevel], ascending: bool) -> Vec<BookLevel> {
    let mut levels = levels
        .iter()
        .filter_map(parse_rest_book_level)
        .map(|(price, size)| BookLevel { price, size })
        .collect::<Vec<_>>();
    if ascending {
        levels.sort_by(|left, right| left.price.cmp(&right.price));
    } else {
        levels.sort_by(|left, right| right.price.cmp(&left.price));
    }
    levels
}

/// Spawn a task that subscribes to Binance spot prices via RTDS WebSocket
/// and publishes `MarketUpdate::SpotPrice` events in real-time.
///
/// When `pool` is provided, each tick is also persisted to `binance_price_ticks`
/// (at full tick resolution) so that historical backtests can replay
/// the same spot-price stream.
pub fn spawn_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_spot_symbols = HashSet::new();
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        // Track last-persisted second per symbol to deduplicate high-frequency ticks.
        let mut last_persisted: HashMap<String, DateTime<Utc>> = HashMap::new();

        info!(
            symbols = ?symbols_upper,
            "Starting RTDS WebSocket spot price feed"
        );

        let client = RtdsClient::new(POLYMARKET_RTDS_WS_ENDPOINT, rtds_market_data_ws_config())
            .expect("RTDS market-data config should be valid");

        // Subscribe to crypto prices (Binance feed)
        let stream = match client.subscribe_crypto_prices(Some(symbols_upper.clone())) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "Failed to subscribe to crypto_prices");
                return;
            }
        };

        let mut stream = Box::pin(stream);
        let mut price_count = 0_u64;

        while let Some(result) = stream.next().await {
            match result {
                Ok(crypto_price) => {
                    // Convert Unix millis to DateTime<Utc>
                    let ts = DateTime::from_timestamp_millis(crypto_price.timestamp)
                        .unwrap_or_else(Utc::now);

                    let symbol_upper = crypto_price.symbol.to_uppercase();

                    upsert_reference_price(
                        &reference_prices,
                        ReferencePriceSnapshot {
                            key: ReferencePriceKey {
                                source: ReferencePriceSource::Binance,
                                symbol: market_symbol_to_binance_symbol(&crypto_price.symbol),
                            },
                            asset_class: ReferenceAssetClass::Crypto,
                            value: crypto_price.value,
                            full_accuracy_value: None,
                            source_timestamp: ts,
                            received_at: Utc::now(),
                            is_carried_forward: false,
                        },
                    )
                    .await;

                    let update = MarketUpdate::SpotPrice {
                        symbol: Arc::from(symbol_upper.as_str()),
                        price: crypto_price.value,
                        ts,
                    };

                    let receivers = tx.receiver_count();

                    match tx.send(update) {
                        Ok(_) => {
                            price_count += 1;
                            if logged_spot_symbols.insert(symbol_upper.clone()) {
                                info!(
                                    symbol = %symbol_upper,
                                    price = %crypto_price.value,
                                    receivers,
                                    "First RTDS spot price received"
                                );
                            }
                            if price_count % 100 == 0 {
                                debug!(
                                    prices = price_count,
                                    tracked_symbols = logged_spot_symbols.len(),
                                    receivers,
                                    "RTDS spot prices forwarded"
                                );
                            }

                            // Persist to DB at most once per second per symbol.
                            if let Some(ref db) = pool {
                                // Truncate to second by zeroing sub-second component.
                                let ts_sec = ts.with_nanosecond(0).unwrap_or(ts);
                                let last = last_persisted.get(&symbol_upper).copied();
                                if last.map_or(true, |l| ts_sec > l) {
                                    last_persisted.insert(symbol_upper.clone(), ts_sec);
                                    persist_spot_price(db, &symbol_upper, crypto_price.value, ts)
                                        .await;
                                }
                            }
                        }
                        Err(_) => {
                            warn!(
                                symbols = ?symbols_upper,
                                "Broadcast channel closed, stopping RTDS spot feed"
                            );
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "RTDS crypto_prices stream error");
                    // Don't exit on transient errors, let SDK handle reconnection
                }
            }
        }

        info!("RTDS spot price feed ended");
    })
}

/// Spawn a task that polls `binance_price_ticks` every 5 seconds and publishes
/// `MarketUpdate::SpotPrice` events as a fallback when the RTDS WebSocket is unavailable.
///
/// This ensures the strategy always has fresh spot prices even if the RTDS
/// subscription fails (e.g. protocol mismatch, network issues).
pub fn spawn_db_spot_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut last_ts: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
        let mut price_count = 0u64;

        info!(symbols = ?symbols_upper, "Starting DB spot price fallback feed");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;

            // Fetch latest price per symbol from binance_price_ticks
            let rows: Vec<(String, rust_decimal::Decimal, chrono::DateTime<chrono::Utc>)> =
                match sqlx::query_as(
                    r#"
                    SELECT DISTINCT ON (symbol) symbol, price, trade_time
                    FROM binance_price_ticks
                    WHERE symbol = ANY($1)
                      AND trade_time > NOW() - INTERVAL '30 seconds'
                    ORDER BY symbol, trade_time DESC
                    "#,
                )
                .bind(&symbols_upper)
                .fetch_all(&pool)
                .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(error = %e, "DB spot feed query failed");
                        continue;
                    }
                };

            for (symbol, price, ts) in rows {
                // Only emit if newer than last seen
                let last = last_ts.get(&symbol).copied();
                if last.map_or(true, |l| ts > l) {
                    last_ts.insert(symbol.clone(), ts);
                    let update = MarketUpdate::SpotPrice {
                        symbol: Arc::from(symbol.as_str()),
                        price,
                        ts,
                    };
                    if tx.send(update).is_err() {
                        return; // channel closed
                    }
                    price_count += 1;
                    if price_count % 50 == 0 {
                        debug!(prices = price_count, "DB spot feed forwarded prices");
                    }
                }
            }
        }
    })
}

/// Spawn a task that polls `binance_agg_trade_ticks` and publishes
/// `MarketUpdate::AggTrade` events for live/dry-run strategies.
///
/// This keeps aggTrade collection decoupled from strategy runtimes while still
/// allowing the runtime to consume a near-real-time trade-flow signal stream.
pub fn spawn_db_aggtrade_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut last_seen: HashMap<String, (chrono::DateTime<chrono::Utc>, i64)> = HashMap::new();
        let mut trade_count = 0u64;

        info!(symbols = ?symbols_upper, "Starting DB aggTrade fallback feed");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let rows: Vec<(
                String,
                i64,
                rust_decimal::Decimal,
                rust_decimal::Decimal,
                bool,
                chrono::DateTime<chrono::Utc>,
            )> = match sqlx::query_as(
                r#"
                SELECT symbol, agg_trade_id, price, quantity, is_buyer_maker, trade_time
                FROM binance_agg_trade_ticks
                WHERE symbol = ANY($1)
                  AND trade_time > NOW() - INTERVAL '30 seconds'
                ORDER BY trade_time ASC, agg_trade_id ASC
                "#,
            )
            .bind(&symbols_upper)
            .fetch_all(&pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "DB aggTrade feed query failed");
                    continue;
                }
            };

            for (symbol, agg_trade_id, price, quantity, is_buyer_maker, ts) in rows {
                let should_emit = match last_seen.get(&symbol).copied() {
                    Some((last_ts, last_id)) => {
                        ts > last_ts || (ts == last_ts && agg_trade_id > last_id)
                    }
                    None => true,
                };
                if !should_emit {
                    continue;
                }

                let Ok(agg_trade_id_u64) = u64::try_from(agg_trade_id) else {
                    warn!(
                        symbol = %symbol,
                        agg_trade_id,
                        "Skipping DB aggTrade row with negative aggregate trade id"
                    );
                    continue;
                };
                last_seen.insert(symbol.clone(), (ts, agg_trade_id));
                let update = MarketUpdate::AggTrade {
                    symbol: Arc::from(symbol.as_str()),
                    agg_trade_id: agg_trade_id_u64,
                    price,
                    quantity,
                    is_buyer_maker,
                    ts,
                };
                if tx.send(update).is_err() {
                    return;
                }
                trade_count += 1;
                if trade_count % 100 == 0 {
                    debug!(trades = trade_count, "DB aggTrade feed forwarded trades");
                }
            }
        }
    })
}

/// Spawn a task that polls `binance_lob_ticks` and publishes
/// `MarketUpdate::L2` and `MarketUpdate::L2Depth` events for live/dry-run strategies.
pub fn spawn_db_l2_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut last_seen: HashMap<String, (chrono::DateTime<chrono::Utc>, i64)> = HashMap::new();
        let mut l2_count = 0u64;

        info!(symbols = ?symbols_upper, "Starting DB L2 feed");

        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let rows: Vec<(
                String,
                i64,
                rust_decimal::Decimal,
                i32,
                rust_decimal::Decimal,
                rust_decimal::Decimal,
                chrono::DateTime<chrono::Utc>,
            )> = match sqlx::query_as(
                r#"
                    SELECT symbol,
                           COALESCE(update_id, 0) AS update_id,
                    COALESCE(obi_5, 0) AS obi_5,
                           COALESCE(spread_bps, 0)::int AS spread_bps,
                           COALESCE(bid_volume_5, 0) AS bid_volume_5,
                           COALESCE(ask_volume_5, 0) AS ask_volume_5,
                           event_time
                    FROM binance_lob_ticks
                    WHERE symbol = ANY($1)
                      AND event_time > NOW() - INTERVAL '30 seconds'
                    ORDER BY event_time ASC, update_id ASC
                    "#,
            )
            .bind(&symbols_upper)
            .fetch_all(&pool)
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "DB L2 feed query failed");
                    continue;
                }
            };

            for (symbol, update_id, obi, spread_bps, bid_volume_5, ask_volume_5, ts) in rows {
                let should_emit = match last_seen.get(&symbol).copied() {
                    Some((last_ts, last_id)) => {
                        ts > last_ts || (ts == last_ts && update_id > last_id)
                    }
                    None => true,
                };
                if !should_emit {
                    continue;
                }

                last_seen.insert(symbol.clone(), (ts, update_id));
                for update in l2_updates_from_depth_totals(
                    &symbol,
                    obi.to_f64().unwrap_or_default(),
                    spread_bps as u32,
                    bid_volume_5,
                    ask_volume_5,
                    ts,
                ) {
                    if tx.send(update).is_err() {
                        return;
                    }
                }
                l2_count += 1;
                if l2_count % 100 == 0 {
                    debug!(updates = l2_count, "DB L2 feed forwarded updates");
                }
            }
        }
    })
}

/// Spawn a task that consumes collector-persisted Polymarket events and quotes.
///
/// This is the strategy-runtime boundary for live/dry-run mode: collector
/// services own public Polymarket/Gamma/CLOB connectivity, while strategy
/// runners consume the local database projection.
pub fn spawn_db_polymarket_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols_upper: Vec<String> = symbols.iter().map(|s| s.to_uppercase()).collect();
        let mut discovered_events = HashSet::new();
        let mut expired_events = HashSet::new();
        let mut last_quote_ts: HashMap<String, DateTime<Utc>> = HashMap::new();
        let mut last_book_ts: HashMap<String, DateTime<Utc>> = HashMap::new();
        let mut active_tokens = Vec::new();
        let (catalog_poll_interval, quote_poll_interval) = db_polymarket_poll_intervals();
        let mut catalog_poll = tokio::time::interval(catalog_poll_interval);
        let mut quote_poll = tokio::time::interval(quote_poll_interval);
        catalog_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        quote_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        info!(
            symbols = ?symbols_upper,
            "Starting DB Polymarket event/quote feed"
        );

        loop {
            tokio::select! {
                biased;
                _ = quote_poll.tick(), if !active_tokens.is_empty() => {
                    if !publish_db_polymarket_quotes(
                        &tx,
                        &pool,
                        &active_tokens,
                        &mut last_quote_ts,
                        &mut last_book_ts,
                    ).await {
                        return;
                    }
                }
                _ = catalog_poll.tick() => {
                    match refresh_db_polymarket_catalog(
                        &tx,
                        &symbols_upper,
                        &pool,
                        &mut discovered_events,
                        &mut expired_events,
                    ).await {
                        Ok(tokens) => active_tokens = tokens,
                        Err(error) => warn!(error = %error, "DB Polymarket event query failed"),
                    }
                }
            }
        }
    })
}

fn db_polymarket_poll_intervals() -> (StdDuration, StdDuration) {
    (StdDuration::from_secs(2), StdDuration::from_millis(100))
}

async fn refresh_db_polymarket_catalog(
    tx: &broadcast::Sender<MarketUpdate>,
    symbols: &[String],
    pool: &PgPool,
    discovered_events: &mut HashSet<String>,
    expired_events: &mut HashSet<String>,
) -> Result<Vec<String>, sqlx::Error> {
    let now = Utc::now();
    let rows: Vec<(
        String,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        Option<String>,
        Option<String>,
        Option<Decimal>,
    )> = sqlx::query_as(
        r#"
        SELECT
            market_slug,
            symbol,
            start_time,
            end_time,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0) AS up_token_id,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>1) AS down_token_id,
            price_to_beat
        FROM pm_market_metadata
        WHERE symbol = ANY($1)
          AND end_time > NOW() - ($2::BIGINT * INTERVAL '1 second')
          AND COALESCE(start_time, end_time - INTERVAL '300 seconds')
                < NOW() + INTERVAL '6 minutes'
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY start_time, end_time, market_slug
        "#,
    )
    .bind(symbols)
    .bind(DB_POLYMARKET_SETTLEMENT_RETRY_LOOKBACK_SECS)
    .fetch_all(pool)
    .await?;
    let mut active_tokens = Vec::new();

    for (event_id, symbol, start_time, end_time, up_token, down_token, price_to_beat) in rows {
        let Some(symbol) = symbol.filter(|value| !value.is_empty()) else {
            continue;
        };
        let Some(end_time) = end_time else {
            continue;
        };
        let Some(up_token) = up_token.map(|value| normalize_token_id(&value)) else {
            continue;
        };
        let Some(down_token) = down_token.map(|value| normalize_token_id(&value)) else {
            continue;
        };
        if up_token.is_empty() || down_token.is_empty() {
            continue;
        }

        let start_time = start_time.unwrap_or(end_time - Duration::seconds(300));
        let window_secs = (end_time - start_time).num_seconds().max(0) as u64;

        if discovered_events.insert(event_id.clone()) {
            let _ = tx.send(MarketUpdate::EventDiscovered {
                event_id: Arc::from(event_id.as_str()),
                symbol: Arc::from(symbol.as_str()),
                up_token: Arc::from(up_token.as_str()),
                down_token: Arc::from(down_token.as_str()),
                end_time,
                window_secs,
                price_to_beat,
                resolved_up_won: None,
            });
        }

        if end_time <= now {
            if !expired_events.contains(&event_id) {
                let resolved_up_won =
                    resolve_db_event_outcome(pool, &event_id, &up_token, &down_token).await;
                if !mark_db_event_expired_if_resolved(expired_events, &event_id, resolved_up_won) {
                    debug!(
                        event_id = %event_id,
                        "DB Polymarket event settlement pending; retrying until official outcome is available",
                    );
                    continue;
                }
                let _ = tx.send(MarketUpdate::EventExpired {
                    event_id: Arc::from(event_id.as_str()),
                    end_time,
                    resolved_up_won,
                });
            }
        } else {
            active_tokens.push(up_token);
            active_tokens.push(down_token);
        }
    }

    Ok(active_tokens)
}

async fn publish_db_polymarket_quotes(
    tx: &broadcast::Sender<MarketUpdate>,
    pool: &PgPool,
    active_tokens: &[String],
    last_quote_ts: &mut HashMap<String, DateTime<Utc>>,
    last_book_ts: &mut HashMap<String, DateTime<Utc>>,
) -> bool {
    let quote_rows: Vec<(
        String,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
        Option<Decimal>,
        DateTime<Utc>,
    )> = match sqlx::query_as(
        r#"
        SELECT DISTINCT ON (token_id)
            token_id, best_bid, best_ask, bid_size, ask_size, received_at
        FROM clob_quote_ticks
        WHERE token_id = ANY($1)
          AND received_at > NOW() - INTERVAL '30 seconds'
          AND (best_bid IS NOT NULL OR best_ask IS NOT NULL)
        ORDER BY token_id, received_at DESC
        "#,
    )
    .bind(active_tokens)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "DB Polymarket quote query failed");
            Vec::new()
        }
    };

    for (token_id, bid, ask, bid_size, ask_size, ts) in quote_rows {
        if last_quote_ts
            .get(&token_id)
            .is_some_and(|last_ts| *last_ts >= ts)
        {
            continue;
        }
        last_quote_ts.insert(token_id.clone(), ts);
        if tx
            .send(MarketUpdate::Quote {
                token_id: Arc::from(token_id.as_str()),
                bid,
                ask,
                bid_size,
                ask_size,
                bid_levels: Vec::new(),
                ask_levels: Vec::new(),
                ts,
            })
            .is_err()
        {
            return false;
        }
    }

    let book_rows: Vec<(String, Value, Value, DateTime<Utc>)> = match sqlx::query_as(
        r#"
        SELECT DISTINCT ON (token_id)
            token_id, bids, asks, received_at
        FROM clob_orderbook_snapshots
        WHERE token_id = ANY($1)
          AND received_at > NOW() - INTERVAL '30 seconds'
          AND (
              jsonb_array_length(bids) > 0
              OR jsonb_array_length(asks) > 0
          )
        ORDER BY token_id, received_at DESC
        "#,
    )
    .bind(active_tokens)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "DB Polymarket orderbook query failed");
            Vec::new()
        }
    };

    for (token_id, bids, asks, ts) in book_rows {
        if last_book_ts
            .get(&token_id)
            .is_some_and(|last_ts| *last_ts >= ts)
        {
            continue;
        }
        let bid_levels = book_levels_from_json(&bids, false);
        let ask_levels = book_levels_from_json(&asks, true);
        if bid_levels.is_empty() && ask_levels.is_empty() {
            continue;
        }
        let best_bid = bid_levels.first();
        let best_ask = ask_levels.first();
        last_book_ts.insert(token_id.clone(), ts);
        if tx
            .send(MarketUpdate::Quote {
                token_id: Arc::from(token_id.as_str()),
                bid: best_bid.map(|level| level.price),
                ask: best_ask.map(|level| level.price),
                bid_size: best_bid.map(|level| level.size),
                ask_size: best_ask.map(|level| level.size),
                bid_levels,
                ask_levels,
                ts,
            })
            .is_err()
        {
            return false;
        }
    }

    true
}

fn mark_db_event_expired_if_resolved(
    expired_events: &mut HashSet<String>,
    event_id: &str,
    resolved_up_won: Option<bool>,
) -> bool {
    if resolved_up_won.is_none() {
        return false;
    }
    expired_events.insert(event_id.to_string())
}

async fn resolve_db_event_outcome(
    pool: &PgPool,
    event_id: &str,
    up_token: &str,
    down_token: &str,
) -> Option<bool> {
    let token_ids = vec![up_token.to_string(), down_token.to_string()];
    let rows: Vec<(String, Option<Decimal>)> = sqlx::query_as(
        r#"
        SELECT token_id, settled_price
        FROM pm_token_settlements
        WHERE market_slug = $1
          AND token_id = ANY($2)
          AND resolved = TRUE
        "#,
    )
    .bind(event_id)
    .bind(&token_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut up = None;
    let mut down = None;
    for (token_id, settled_price) in rows {
        let token_id = normalize_token_id(&token_id);
        if token_id == up_token {
            up = settled_price;
        } else if token_id == down_token {
            down = settled_price;
        }
    }

    match (up, down) {
        (Some(up), Some(down)) if up != down => Some(up > down),
        (Some(up), _) => Some(up > Decimal::new(5, 1)),
        (_, Some(down)) => Some(down < Decimal::new(5, 1)),
        _ => None,
    }
}

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
        symbol: sym.clone(),
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

fn parse_depth_level(level: &Value) -> Option<(f64, f64)> {
    match level {
        Value::Array(items) if items.len() >= 2 => {
            Some((json_f64(&items[0])?, json_f64(&items[1])?))
        }
        Value::Object(map) => Some((json_f64(map.get("price")?)?, json_f64(map.get("size")?)?)),
        _ => None,
    }
}

fn json_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

fn book_levels_from_json(value: &Value, ascending: bool) -> Vec<BookLevel> {
    let mut levels = value
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
        .filter_map(parse_depth_level)
        .filter_map(|(price, size)| {
            let price = Decimal::try_from(price).ok()?;
            let size = Decimal::try_from(size).ok()?;
            if size <= Decimal::ZERO || !pm_tradeable_price(price) {
                return None;
            }
            Some(BookLevel { price, size })
        })
        .collect::<Vec<_>>();
    if ascending {
        levels.sort_by(|left, right| left.price.cmp(&right.price));
    } else {
        levels.sort_by(|left, right| right.price.cmp(&left.price));
    }
    levels
}

#[derive(Default)]
struct ClobBookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
    bid_timestamps: BTreeMap<Decimal, i64>,
    ask_timestamps: BTreeMap<Decimal, i64>,
    snapshot_timestamp: Option<i64>,
    initialized: bool,
    // Set when the cached depth proved stale against a provider-reported BBA.
    // A dirty token stays failed closed until a full book snapshot clears it.
    dirty: bool,
    // Deltas buffered while the token awaits its first book snapshot after
    // (re)subscription; replayed in arrival order once the snapshot lands.
    pending: Vec<(i64, PriceChangeBatchEntry)>,
}

impl ClobBookState {
    fn replace(&mut self, book: &BookUpdate) -> Result<(), String> {
        if book.bids.iter().chain(&book.asks).any(|level| {
            level.size <= Decimal::ZERO || !(Decimal::ZERO..=Decimal::ONE).contains(&level.price)
        }) {
            return Err("Polymarket book contains an invalid price or size".to_string());
        }
        self.bids = book
            .bids
            .iter()
            .map(|level| (level.price, level.size))
            .collect();
        self.asks = book
            .asks
            .iter()
            .map(|level| (level.price, level.size))
            .collect();
        self.bid_timestamps.clear();
        self.ask_timestamps.clear();
        self.snapshot_timestamp = Some(book.timestamp);
        self.initialized = true;
        self.dirty = false;
        Ok(())
    }

    fn validate_change(&self, entry: &PriceChangeBatchEntry) -> Result<(), String> {
        let Some(size) = entry.size else {
            return Err("Polymarket price change is missing size".to_string());
        };
        let invalid_bba = entry
            .best_bid
            .is_some_and(|price| !(Decimal::ZERO..=Decimal::ONE).contains(&price))
            || entry
                .best_ask
                .is_some_and(|price| !(Decimal::ZERO..=Decimal::ONE).contains(&price));
        if size < Decimal::ZERO
            || !(Decimal::ZERO..=Decimal::ONE).contains(&entry.price)
            || !matches!(entry.side, Side::Buy | Side::Sell)
            || invalid_bba
        {
            return Err("Polymarket price change contains an invalid field".to_string());
        }
        Ok(())
    }

    fn level_timestamp(&self, entry: &PriceChangeBatchEntry) -> Option<i64> {
        match entry.side {
            Side::Buy => self.bid_timestamps.get(&entry.price).copied(),
            Side::Sell => self.ask_timestamps.get(&entry.price).copied(),
            _ => None,
        }
    }

    fn apply(&mut self, entry: &PriceChangeBatchEntry, timestamp: i64) {
        let size = entry.size.expect("price change was validated");
        if !self.initialized {
            return;
        }
        let (levels, timestamps) = match entry.side {
            Side::Buy => (&mut self.bids, &mut self.bid_timestamps),
            Side::Sell => (&mut self.asks, &mut self.ask_timestamps),
            _ => unreachable!("price change side was validated"),
        };
        if size > Decimal::ZERO {
            levels.insert(entry.price, size);
        } else {
            levels.remove(&entry.price);
        }
        timestamps.insert(entry.price, timestamp);
        let Some((best_bid, best_ask)) = entry.best_bid.zip(entry.best_ask) else {
            return;
        };
        if best_bid > best_ask {
            return;
        }
        let bid_timestamps = &mut self.bid_timestamps;
        self.bids.retain(|price, _| {
            let keep = *price <= best_bid;
            if !keep {
                bid_timestamps.insert(*price, timestamp);
            }
            keep
        });
        let ask_timestamps = &mut self.ask_timestamps;
        self.asks.retain(|price, _| {
            let keep = *price >= best_ask;
            if !keep {
                ask_timestamps.insert(*price, timestamp);
            }
            keep
        });
    }

    fn quote(
        &self,
        token_id: String,
        ts: DateTime<Utc>,
        entry: Option<&PriceChangeBatchEntry>,
    ) -> MarketUpdate {
        let bid_levels = if self.initialized {
            self.bids
                .iter()
                .rev()
                .map(|(price, size)| BookLevel {
                    price: *price,
                    size: *size,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let ask_levels = if self.initialized {
            self.asks
                .iter()
                .map(|(price, size)| BookLevel {
                    price: *price,
                    size: *size,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let bid = bid_levels
            .iter()
            .find(|level| pm_tradeable_price(level.price))
            .map(|level| level.price)
            .or_else(|| {
                entry
                    .and_then(|change| change.best_bid)
                    .filter(|price| pm_tradeable_price(*price))
            });
        let ask = ask_levels
            .iter()
            .find(|level| pm_tradeable_price(level.price))
            .map(|level| level.price)
            .or_else(|| {
                entry
                    .and_then(|change| change.best_ask)
                    .filter(|price| pm_tradeable_price(*price))
            });
        let bid_size = bid_levels
            .iter()
            .find(|level| Some(level.price) == bid)
            .map(|level| level.size)
            .or_else(|| {
                entry.and_then(|change| {
                    (change.side == Side::Buy && Some(change.price) == bid)
                        .then_some(change.size)
                        .flatten()
                        .filter(|size| *size > Decimal::ZERO)
                })
            });
        let ask_size = ask_levels
            .iter()
            .find(|level| Some(level.price) == ask)
            .map(|level| level.size)
            .or_else(|| {
                entry.and_then(|change| {
                    (change.side == Side::Sell && Some(change.price) == ask)
                        .then_some(change.size)
                        .flatten()
                        .filter(|size| *size > Decimal::ZERO)
                })
            });
        MarketUpdate::Quote {
            token_id: Arc::from(token_id),
            bid,
            ask,
            bid_size,
            ask_size,
            bid_levels,
            ask_levels,
            ts,
        }
    }
}

fn clob_failed_closed_updates(token_id: &str, ts: DateTime<Utc>) -> Vec<MarketUpdate> {
    let token_id: Arc<str> = Arc::from(token_id);
    vec![
        MarketUpdate::QuoteCollectionFailure {
            token_id: Arc::clone(&token_id),
            request_started_at: ts,
            http_status: None,
            error_kind: Arc::from("websocket_payload"),
            ts,
        },
        MarketUpdate::Quote {
            token_id,
            bid: None,
            ask: None,
            bid_size: None,
            ask_size: None,
            bid_levels: Vec::new(),
            ask_levels: Vec::new(),
            ts,
        },
    ]
}

fn market_update_from_clob_book(
    book: &BookUpdate,
    state: &mut ClobBookState,
) -> Result<(Vec<MarketUpdate>, i64), String> {
    let pending = std::mem::take(&mut state.pending);
    state.replace(book)?;
    // Replay buffered messages in arrival order. Messages strictly older than
    // an already-applied timestamp are skipped (the live path likewise rejects
    // regressing source time), each replayed message's final entry faces the
    // same BBA reconciliation as a live batch, and the timestamp watermark is
    // rebuilt from what was actually applied.
    let mut watermark = book.timestamp;
    let mut replay_dirty = false;
    for (index, (timestamp, entry)) in pending.iter().enumerate() {
        if *timestamp < watermark {
            continue;
        }
        state.apply(entry, *timestamp);
        watermark = *timestamp;
        let message_ends = pending
            .get(index + 1)
            .is_none_or(|(next_timestamp, _)| next_timestamp != timestamp);
        if message_ends
            && (entry
                .best_bid
                .is_some_and(|price| pm_tradeable_price(price) && !state.bids.contains_key(&price))
                || entry.best_ask.is_some_and(|price| {
                    pm_tradeable_price(price) && !state.asks.contains_key(&price)
                }))
        {
            replay_dirty = true;
            break;
        }
    }
    if replay_dirty {
        *state = ClobBookState::default();
        state.dirty = true;
        return Ok((
            clob_failed_closed_updates(&book.asset_id.to_string(), Utc::now()),
            watermark,
        ));
    }
    let ts = DateTime::from_timestamp_millis(watermark)
        .ok_or_else(|| "Polymarket book timestamp is out of range".to_string())?;
    let quote = state.quote(book.asset_id.to_string(), ts, None);
    // A replayed book can cross when buffered entries lack BBA fields, so the
    // final quote faces the same crossed-book check as a live batch, with the
    // same in-band isolation as the reconciliation above.
    if let MarketUpdate::Quote {
        bid: Some(bid),
        ask: Some(ask),
        ..
    } = quote
    {
        if bid > ask {
            *state = ClobBookState::default();
            state.dirty = true;
            return Ok((
                clob_failed_closed_updates(&book.asset_id.to_string(), Utc::now()),
                watermark,
            ));
        }
    }
    Ok((vec![quote], watermark))
}

fn market_updates_from_price_change(
    change: &PriceChange,
    books: &mut HashMap<String, ClobBookState>,
    last_timestamp: &mut HashMap<String, i64>,
) -> Result<Vec<MarketUpdate>, String> {
    let Some(ts) = DateTime::from_timestamp_millis(change.timestamp) else {
        return Err("Polymarket price-change timestamp is out of range".to_string());
    };
    let default_state = ClobBookState::default();
    let mut applicable_entries = Vec::new();
    let mut updated_tokens = Vec::new();
    let mut last_entries = HashMap::new();

    for entry in &change.price_changes {
        default_state.validate_change(entry)?;
    }

    for entry in &change.price_changes {
        let token_id = entry.asset_id.to_string();
        let state = books.get(&token_id).unwrap_or(&default_state);
        if state
            .snapshot_timestamp
            .is_some_and(|last| change.timestamp < last)
        {
            continue;
        }
        if last_timestamp
            .get(&token_id)
            .is_some_and(|last| change.timestamp < *last)
        {
            if state
                .level_timestamp(entry)
                .is_some_and(|last| change.timestamp < last)
            {
                continue;
            }
            return Err("Polymarket price-change source time moved backwards".to_string());
        }
        if entry
            .best_bid
            .zip(entry.best_ask)
            .is_some_and(|(bid, ask)| bid > ask)
        {
            return Err(POLYMARKET_CLOB_CROSSED_BOOK_ERROR.to_string());
        }
        let size = entry.size.expect("price change was validated");
        if size > Decimal::ZERO
            && match entry.side {
                Side::Buy => entry.best_bid.is_some_and(|best| entry.price > best),
                Side::Sell => entry.best_ask.is_some_and(|best| entry.price < best),
                _ => unreachable!("price change side was validated"),
            }
        {
            return Err("Polymarket price change contains an invalid field".to_string());
        }
        applicable_entries.push((token_id, entry));
    }

    for (token_id, entry) in applicable_entries {
        let state = books.entry(token_id.clone()).or_default();
        if !state.initialized && !state.dirty {
            // Syncing: no snapshot has arrived since (re)subscription. Buffer
            // the delta for ordered replay after the first book instead of
            // dropping it while still poisoning last_timestamp, and bound the
            // buffer by failing the token closed on overflow.
            if state.pending.len() < MAX_POLYMARKET_CLOB_PENDING_CHANGES {
                state.pending.push((change.timestamp, entry.clone()));
                continue;
            }
            *state = ClobBookState::default();
            state.dirty = true;
        } else {
            state.apply(entry, change.timestamp);
        }
        last_timestamp.insert(token_id.clone(), change.timestamp);
        if last_entries.insert(token_id.clone(), entry).is_none() {
            updated_tokens.push(token_id);
        }
    }

    // A reported BBA level missing from cached depth proves only that token's
    // cache is stale. Mark just that token dirty so it stays failed closed
    // until the next book snapshot reinitializes it, and fail it closed
    // in-band instead of rejecting the whole batch and reconnecting every
    // subscribed token.
    for token_id in &updated_tokens {
        let state = &books[token_id];
        let entry = last_entries[token_id];
        let missing_bba = state.initialized
            && (entry.best_bid.is_some_and(|price| {
                pm_tradeable_price(price) && !state.bids.contains_key(&price)
            }) || entry.best_ask.is_some_and(|price| {
                pm_tradeable_price(price) && !state.asks.contains_key(&price)
            }));
        if missing_bba {
            let state = books.get_mut(token_id).expect("token state was applied");
            *state = ClobBookState::default();
            state.dirty = true;
        }
    }

    let failed_at = Utc::now();
    let updates = updated_tokens
        .into_iter()
        .flat_map(|token_id| {
            if books[&token_id].dirty {
                clob_failed_closed_updates(&token_id, failed_at)
            } else {
                vec![books[&token_id].quote(
                    token_id.clone(),
                    ts,
                    last_entries.get(&token_id).copied(),
                )]
            }
        })
        .collect::<Vec<_>>();
    if updates.iter().any(|update| {
        matches!(
            update,
            MarketUpdate::Quote {
                bid: Some(bid),
                ask: Some(ask),
                ..
            } if bid > ask
        )
    }) {
        return Err(POLYMARKET_CLOB_CROSSED_BOOK_ERROR.to_string());
    }
    Ok(updates)
}

fn persist_clob_failure_payload(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_POLYMARKET_CLOB_FAILURE_CAPTURE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("capture payload exceeds {MAX_POLYMARKET_CLOB_FAILURE_CAPTURE_BYTES} bytes"),
        ));
    }
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture path must be absolute",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture path has no parent directory",
        )
    })?;
    let metadata = fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || fs::canonicalize(parent)? != parent
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "capture parent must be a direct canonical directory",
        ));
    }

    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(payload)?;
    file.sync_all()?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn send_quote_collection_failure_and_empty(
    tx: &broadcast::Sender<MarketUpdate>,
    token_ids: &[U256],
    request_started_at: DateTime<Utc>,
    error_kind: &str,
) -> bool {
    let now = Utc::now();
    token_ids.iter().all(|token_id| {
        let token_id: Arc<str> = Arc::from(token_id.to_string());
        tx.send(MarketUpdate::QuoteCollectionFailure {
            token_id: Arc::clone(&token_id),
            request_started_at,
            http_status: None,
            error_kind: Arc::from(error_kind),
            ts: now,
        })
        .is_ok()
            && tx
                .send(MarketUpdate::Quote {
                    token_id,
                    bid: None,
                    ask: None,
                    bid_size: None,
                    ask_size: None,
                    bid_levels: Vec::new(),
                    ask_levels: Vec::new(),
                    ts: now,
                })
                .is_ok()
    })
}

fn forward_clob_ws_payload(
    payload: &[u8],
    tx: &broadcast::Sender<MarketUpdate>,
    books_by_token: &mut HashMap<String, ClobBookState>,
    last_timestamp: &mut HashMap<String, i64>,
) -> Result<bool, String> {
    let messages = parse_if_interested(payload, &MessageInterest::MARKET)
        .map_err(|error| error.to_string())?;
    for message in messages {
        match message {
            WsMessage::Book(book) => {
                let token_id = book.asset_id.to_string();
                // A dirty or never-initialized token has no valid state to
                // protect: accept even an older snapshot so it can self-heal.
                let healable = books_by_token
                    .get(&token_id)
                    .is_none_or(|state| state.dirty || !state.initialized);
                if !healable
                    && last_timestamp
                        .get(&token_id)
                        .is_some_and(|last| book.timestamp < *last)
                {
                    return Err("Polymarket book source time moved backwards".to_string());
                }
                let state = books_by_token.entry(token_id.clone()).or_default();
                let (updates, watermark) = market_update_from_clob_book(&book, state)?;
                last_timestamp.insert(token_id, watermark);
                for update in updates {
                    if tx.send(update).is_err() {
                        return Ok(false);
                    }
                }
            }
            WsMessage::PriceChange(change) => {
                for update in
                    market_updates_from_price_change(&change, books_by_token, last_timestamp)?
                {
                    if tx.send(update).is_err() {
                        return Ok(false);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(true)
}

async fn capture_crossed_clob_failure_and_empty(
    tx: &broadcast::Sender<MarketUpdate>,
    token_ids: &[U256],
    request_started_at: DateTime<Utc>,
    failure_capture_path: Option<&Path>,
    payload: &[u8],
    failure: &str,
) -> bool {
    let Some(path) = failure_capture_path.filter(|_| failure == POLYMARKET_CLOB_CROSSED_BOOK_ERROR)
    else {
        return false;
    };

    let _ = send_quote_collection_failure_and_empty(
        tx,
        token_ids,
        request_started_at,
        "websocket_payload",
    );
    let owned_path = path.to_owned();
    let owned_payload = payload.to_vec();
    match tokio::task::spawn_blocking(move || {
        persist_clob_failure_payload(&owned_path, &owned_payload)
    })
    .await
    {
        Ok(Ok(())) => warn!(
            path = %path.display(),
            bytes = payload.len(),
            "Captured first crossed-book CLOB payload; stopping diagnostic feed"
        ),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => warn!(
            path = %path.display(),
            "Crossed-book CLOB payload capture already exists; stopping diagnostic feed"
        ),
        Ok(Err(error)) => error!(
            %error,
            path = %path.display(),
            "Crossed-book CLOB payload capture failed; stopping diagnostic feed"
        ),
        Err(error) => error!(
            %error,
            path = %path.display(),
            "Crossed-book CLOB payload capture task failed; stopping diagnostic feed"
        ),
    }
    true
}

/// Classify a hot-path receive error into a failure kind: bare transport
/// drops (including TCP resets, which tungstenite surfaces as
/// `ProtocolError::ResetWithoutClosingHandshake`) are reconnection lifecycle
/// evidence, while genuine protocol violations are payload-integrity errors.
fn classify_clob_ws_receive_error(error: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::error::ProtocolError;
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
            ) =>
        {
            "transport_reconnect"
        }
        Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => "transport_reconnect",
        _ => "websocket_payload",
    }
}

/// Publish Polymarket CLOB book and BBA ticks directly to the strategy runtime.
///
/// Disconnects publish empty quotes so the strategy fails closed until a fresh
/// WebSocket snapshot arrives. REST polling is intentionally kept out of this
/// hot path because it can reopen trading with delayed state.
pub fn spawn_clob_ws_quote_feed_until(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<U256>,
    stop_at: Option<DateTime<Utc>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_timestamp: HashMap<String, i64> = HashMap::new();
        let mut books_by_token: HashMap<String, ClobBookState> = HashMap::new();
        let failure_capture_path =
            std::env::var_os(POLYMARKET_CLOB_FAILURE_CAPTURE_ENV).map(PathBuf::from);
        let endpoint = format!(
            "{}/ws/market",
            POLYMARKET_CLOB_WS_ENDPOINT.trim_end_matches('/')
        );

        loop {
            if stop_at.is_some_and(|deadline| Utc::now() >= deadline) {
                return;
            }
            let request_started_at = Utc::now();

            let (socket, _) = match connect_async(&endpoint).await {
                Ok(connection) => connection,
                Err(error) => {
                    warn!(%error, %endpoint, "Polymarket hot-path WebSocket connect failed");
                    last_timestamp.clear();
                    books_by_token.clear();
                    if !send_quote_collection_failure_and_empty(
                        &tx,
                        &token_ids,
                        request_started_at,
                        "websocket_connect",
                    ) {
                        return;
                    }
                    tokio::time::sleep(StdDuration::from_millis(250)).await;
                    continue;
                }
            };
            let (mut write, mut read) = socket.split();
            let subscription =
                match serde_json::to_string(&SubscriptionRequest::market(token_ids.clone())) {
                    Ok(subscription) => subscription,
                    Err(error) => {
                        error!(%error, "Polymarket subscription serialization failed");
                        let _ = send_quote_collection_failure_and_empty(
                            &tx,
                            &token_ids,
                            request_started_at,
                            "websocket_subscription_encode",
                        );
                        return;
                    }
                };
            if let Err(error) = write.send(Message::Text(subscription.into())).await {
                warn!(%error, "Polymarket hot-path subscription send failed");
                last_timestamp.clear();
                books_by_token.clear();
                if !send_quote_collection_failure_and_empty(
                    &tx,
                    &token_ids,
                    request_started_at,
                    "websocket_subscribe",
                ) {
                    return;
                }
                tokio::time::sleep(StdDuration::from_millis(250)).await;
                continue;
            }

            let mut heartbeat = tokio::time::interval(StdDuration::from_secs(3));
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            heartbeat.tick().await;
            let mut last_pong = Instant::now();
            let stop = async {
                match stop_at {
                    Some(deadline) => {
                        tokio::time::sleep((deadline - Utc::now()).to_std().unwrap_or_default())
                            .await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::pin!(stop);

            let failure_kind = loop {
                tokio::select! {
                    _ = &mut stop => return,
                    message = read.next() => match message {
                        Some(Ok(Message::Text(text))) if text == "PONG" => {
                            last_pong = Instant::now();
                        }
                        Some(Ok(Message::Text(text))) => {
                            match forward_clob_ws_payload(
                                text.as_bytes(),
                                &tx,
                                &mut books_by_token,
                                &mut last_timestamp,
                            ) {
                                Ok(true) => {}
                                Ok(false) => return,
                                Err(error) => {
                                    warn!(%error, "Polymarket hot-path payload parse failed; reconnecting for a fresh snapshot");
                                    if capture_crossed_clob_failure_and_empty(
                                        &tx,
                                        &token_ids,
                                        request_started_at,
                                        failure_capture_path.as_deref(),
                                        text.as_bytes(),
                                        &error,
                                    ).await {
                                        return;
                                    }
                                    break "websocket_payload";
                                }
                            }
                        }
                        Some(Ok(Message::Binary(bytes))) => {
                            match forward_clob_ws_payload(
                                bytes.as_ref(),
                                &tx,
                                &mut books_by_token,
                                &mut last_timestamp,
                            ) {
                                Ok(true) => {}
                                Ok(false) => return,
                                Err(error) => {
                                    warn!(%error, "Polymarket hot-path binary payload parse failed; reconnecting for a fresh snapshot");
                                    if capture_crossed_clob_failure_and_empty(
                                        &tx,
                                        &token_ids,
                                        request_started_at,
                                        failure_capture_path.as_deref(),
                                        bytes.as_ref(),
                                        &error,
                                    ).await {
                                        return;
                                    }
                                    break "websocket_payload";
                                }
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            if let Err(error) = write.send(Message::Pong(payload)).await {
                                warn!(%error, "Polymarket hot-path pong failed");
                                break "websocket_pong";
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            last_pong = Instant::now();
                        }
                        Some(Ok(Message::Close(frame))) => {
                            warn!(?frame, "Polymarket hot-path WebSocket closed");
                            break "websocket_close";
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => {
                            warn!(%error, "Polymarket hot-path WebSocket receive failed");
                            break classify_clob_ws_receive_error(&error);
                        }
                        None => break "websocket_eof",
                    },
                    _ = heartbeat.tick() => {
                        if last_pong.elapsed() > StdDuration::from_secs(6) {
                            warn!("Polymarket hot-path heartbeat timed out");
                            break "websocket_heartbeat_timeout";
                        }
                        if let Err(error) = write.send(Message::Text("PING".into())).await {
                            warn!(%error, "Polymarket hot-path heartbeat send failed");
                            break "websocket_heartbeat_send";
                        }
                    }
                }
            };

            last_timestamp.clear();
            books_by_token.clear();
            if !send_quote_collection_failure_and_empty(
                &tx,
                &token_ids,
                request_started_at,
                failure_kind,
            ) {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(250)).await;
        }
    })
}

/// Spawn a task that polls the Polymarket CLOB REST API for orderbook data
/// and publishes `MarketUpdate::Quote` events with top-of-book sizes.
///
/// REST polling is more reliable than WS for the 5-min window lifecycle.
/// Polls every 5 seconds per token batch.
///
/// When `pool` is provided, each non-empty quote is also persisted to
/// `clob_quote_ticks` so that historical backtests can replay the same data.
pub fn spawn_quote_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<U256>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    spawn_quote_feed_until(tx, token_ids, pool, None)
}

/// Spawn a quote poller that optionally exits after `stop_at`.
pub fn spawn_quote_feed_until(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<U256>,
    pool: Option<PgPool>,
    stop_at: Option<DateTime<Utc>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let poll_interval = std::time::Duration::from_secs(5);
        let mut quoted_tokens = 0_u64;
        let mut logged_quote_tokens = HashSet::new();

        info!(tokens = token_ids.len(), "Starting REST quote poller");

        loop {
            if stop_at.is_some_and(|deadline| Utc::now() >= deadline) {
                info!(
                    tokens = token_ids.len(),
                    stop_at = ?stop_at,
                    "Stopping REST quote poller after market window"
                );
                return;
            }

            for token in &token_ids {
                let token_str = token.to_string();

                let url = format!("{POLYMARKET_CLOB_HTTP_ENDPOINT}/book?token_id={token_str}");

                match http.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(book) = resp.json::<RestBook>().await {
                            let quote = book_quote_from_rest(&book);
                            let now = Utc::now();
                            let update = MarketUpdate::Quote {
                                token_id: Arc::from(token_str.as_str()),
                                bid: quote.bid,
                                ask: quote.ask,
                                bid_size: quote.bid_size,
                                ask_size: quote.ask_size,
                                bid_levels: book_levels_from_rest(&book.bids, false),
                                ask_levels: book_levels_from_rest(&book.asks, true),
                                ts: now,
                            };
                            if tx.send(update).is_err() {
                                warn!(
                                    tokens = token_ids.len(),
                                    "All receivers dropped, stopping quote poller"
                                );
                                return;
                            }

                            // Persist non-empty top-of-book quotes to DB for replay.
                            if let Some(ref db) = pool {
                                if quote.bid.is_some() || quote.ask.is_some() {
                                    persist_quote(db, &token_str, quote, now).await;
                                }
                            }

                            quoted_tokens += 1;
                            if logged_quote_tokens.insert(token_str.clone()) {
                                info!(
                                    token = %token_str,
                                    bid = ?quote.bid,
                                    ask = ?quote.ask,
                                    bid_size = ?quote.bid_size,
                                    ask_size = ?quote.ask_size,
                                    "First orderbook quote observed"
                                );
                            } else if quoted_tokens % 100 == 0 {
                                info!(
                                    quotes = quoted_tokens,
                                    tracked_tokens = logged_quote_tokens.len(),
                                    "REST quote poller forwarded orderbook quotes"
                                );
                            }
                        }
                    }
                    Ok(resp) => {
                        debug!(
                            status = %resp.status(),
                            token = %token_str,
                            "REST orderbook fetch returned non-success status"
                        );
                    }
                    Err(e) => {
                        debug!(error = %e, token = %token_str, "REST orderbook fetch failed");
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    })
}

/// Spawn a task that subscribes to Chainlink 60-second TWAP prices via RTDS WebSocket.
///
/// Used to capture S0 (open price) at eventStartTime for 5M markets.
/// Current Polymarket 5M/15M crypto markets use this feed for their resolution baseline.
///
/// Prices are stored in the shared cache for scanner to use when creating EventDiscovered.
pub fn spawn_chainlink_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut logged_chainlink_symbols = HashSet::new();
        let symbols_chainlink: Vec<String> = symbols
            .iter()
            .map(|s| {
                let base = s.trim_end_matches("USDT").to_lowercase();
                format!("{}/usd", base)
            })
            .collect();

        info!(
            symbols = ?symbols_chainlink,
            "Starting RTDS Chainlink price feed"
        );

        let client = RtdsClient::new(POLYMARKET_RTDS_WS_ENDPOINT, rtds_market_data_ws_config())
            .expect("RTDS market-data config should be valid");

        let subscription = Subscription::builder()
            .topic("crypto_prices_twap_sixty".to_string())
            .msg_type("update".to_string())
            .build();
        let stream = match client.subscribe_raw(subscription) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, "Failed to subscribe to Chainlink 60-second TWAP prices");
                return;
            }
        };

        let mut stream = Box::pin(stream);
        let mut price_count = 0_u64;

        while let Some(result) = stream.next().await {
            match result {
                Ok(message) => {
                    let Some(chainlink_price) = parse_chainlink_twap_price(&message.payload) else {
                        warn!(topic = %message.topic, "Invalid Chainlink 60-second TWAP payload");
                        continue;
                    };
                    // Filter to only our symbols
                    if !symbols_chainlink.contains(&chainlink_price.symbol) {
                        continue;
                    }

                    // Convert Unix millis to DateTime<Utc>
                    let ts = DateTime::from_timestamp_millis(chainlink_price.timestamp)
                        .unwrap_or_else(Utc::now);
                    let received_at = Utc::now();

                    upsert_reference_price(
                        &reference_prices,
                        ReferencePriceSnapshot {
                            key: ReferencePriceKey {
                                source: ReferencePriceSource::Chainlink,
                                symbol: normalize_reference_symbol(&chainlink_price.symbol),
                            },
                            asset_class: ReferenceAssetClass::Crypto,
                            value: chainlink_price.value,
                            full_accuracy_value: Some(chainlink_price.full_accuracy_value.clone()),
                            source_timestamp: ts,
                            received_at,
                            is_carried_forward: false,
                        },
                    )
                    .await;

                    let update = MarketUpdate::ReferencePrice {
                        symbol: Arc::from(
                            normalize_reference_symbol(&chainlink_price.symbol).as_str(),
                        ),
                        source: Arc::from(ReferencePriceSource::Chainlink.as_str()),
                        asset_class: Arc::from(ReferenceAssetClass::Crypto.as_str()),
                        price: chainlink_price.value,
                        full_accuracy_value: Some(Arc::from(
                            chainlink_price.full_accuracy_value.as_str(),
                        )),
                        is_carried_forward: false,
                        received_at: Some(received_at),
                        ts,
                    };

                    if tx.send(update).is_err() {
                        warn!(
                            symbols = ?symbols_chainlink,
                            "Broadcast channel closed, stopping RTDS Chainlink feed"
                        );
                        return;
                    }

                    if let Some(ref db) = pool {
                        persist_chainlink_price(
                            db,
                            &chainlink_price.symbol,
                            chainlink_price.value,
                            ts,
                            received_at,
                        )
                        .await;
                    }

                    let receivers = tx.receiver_count();
                    price_count += 1;

                    if logged_chainlink_symbols.insert(chainlink_price.symbol.clone()) {
                        info!(
                            symbol = %chainlink_price.symbol,
                            price = %chainlink_price.value,
                            receivers,
                            "First Chainlink price received and cached"
                        );
                    }
                    if price_count % 100 == 0 {
                        debug!(
                            prices = price_count,
                            tracked_symbols = logged_chainlink_symbols.len(),
                            receivers,
                            "Chainlink prices cached"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "RTDS Chainlink 60-second TWAP stream error");
                    // Don't exit on transient errors, let SDK handle reconnection
                }
            }
        }

        info!("RTDS Chainlink price feed ended");
    })
}

/// Spawn one RTDS Pyth feed task per symbol and publish all ticks into the
/// shared reference-price registry.
pub fn spawn_pyth_reference_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    pool: Option<PgPool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if symbols.is_empty() {
            info!("No Pyth symbols configured, skipping equity_prices feed");
            return;
        }

        let mut join_set = JoinSet::new();

        for raw_symbol in symbols {
            let tx = tx.clone();
            let registry = reference_prices.clone();
            let pool = pool.clone();
            let subscribe_symbol = raw_symbol.clone();

            join_set.spawn(async move {
                run_pyth_reference_worker(tx, registry, subscribe_symbol, pool).await;
            });
        }

        while let Some(result) = join_set.join_next().await {
            if let Err(error) = result {
                warn!(error = %error, "A Pyth reference-price worker exited");
            }
        }
    })
}

#[derive(Debug, Clone, Deserialize)]
struct EquityPriceTick {
    #[serde(default)]
    symbol: String,
    value: Decimal,
    full_accuracy_value: Option<String>,
    timestamp: i64,
    received_at: Option<i64>,
    #[serde(default)]
    is_carried_forward: bool,
}

#[derive(Debug, Deserialize)]
struct EquityPriceSnapshotPayload {
    symbol: String,
    data: Vec<EquityPriceTick>,
}

fn parse_equity_price_payload(value: &Value) -> Option<Vec<EquityPriceTick>> {
    if value.get("topic")?.as_str()? != "equity_prices" {
        return None;
    }
    let message_type = value.get("type")?.as_str()?;
    let payload = value.get("payload")?.clone();
    if matches!(message_type, "subscribe" | "snapshot") {
        let snapshot: EquityPriceSnapshotPayload = serde_json::from_value(payload).ok()?;
        return Some(
            snapshot
                .data
                .into_iter()
                .map(|mut point| {
                    point.symbol.clone_from(&snapshot.symbol);
                    point
                })
                .collect(),
        );
    }
    if message_type == "update" {
        return serde_json::from_value(payload).ok().map(|tick| vec![tick]);
    }
    None
}

fn equity_price_subscription(symbol: &str) -> Subscription {
    let inner_filter = serde_json::json!({"symbol": symbol}).to_string();
    let encoded_filter = serde_json::to_string(&inner_filter)
        .expect("serializing an equity symbol filter cannot fail");
    Subscription::builder()
        .topic("equity_prices".to_owned())
        .msg_type("*".to_owned())
        .filters(encoded_filter)
        .build()
}

async fn run_pyth_reference_worker(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    registry: ReferencePriceRegistry,
    subscribe_symbol: String,
    pool: Option<PgPool>,
) {
    let normalized_symbol = pyth_symbol(&subscribe_symbol);
    let asset_class = infer_pyth_asset_class(&subscribe_symbol);
    let mut message_count = 0_u64;
    let client = RtdsClient::new(POLYMARKET_RTDS_WS_ENDPOINT, rtds_market_data_ws_config())
        .expect("RTDS market-data config should be valid");
    let subscription = equity_price_subscription(&subscribe_symbol);
    let stream = match client.subscribe_raw(subscription) {
        Ok(stream) => stream,
        Err(error) => {
            warn!(symbol = %subscribe_symbol, error = %error, "RTDS equity_prices subscribe failed");
            return;
        }
    };
    let mut stream = Box::pin(stream);

    while let Some(message) = stream.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                warn!(symbol = %subscribe_symbol, error = %error, "RTDS equity_prices stream error");
                continue;
            }
        };
        let envelope = serde_json::json!({
            "topic": message.topic,
            "type": message.msg_type,
            "timestamp": message.timestamp,
            "payload": message.payload,
        });
        let Some(ticks) = parse_equity_price_payload(&envelope) else {
            continue;
        };
        for tick in ticks {
            let source_timestamp =
                DateTime::from_timestamp_millis(tick.timestamp).unwrap_or_else(Utc::now);
            let received_at = tick
                .received_at
                .and_then(DateTime::from_timestamp_millis)
                .unwrap_or_else(Utc::now);
            let snapshot = ReferencePriceSnapshot {
                key: ReferencePriceKey {
                    source: ReferencePriceSource::Pyth,
                    symbol: normalize_reference_symbol(&tick.symbol),
                },
                asset_class,
                value: tick.value,
                full_accuracy_value: tick.full_accuracy_value,
                source_timestamp,
                received_at,
                is_carried_forward: tick.is_carried_forward,
            };
            upsert_reference_price(&registry, snapshot.clone()).await;
            if tx.send(reference_price_update(&snapshot)).is_err() {
                return;
            }
            if let Some(ref db) = pool {
                persist_reference_price(db, &snapshot).await;
            }
            message_count += 1;
            if message_count == 1 || message_count % 100 == 0 {
                info!(
                    symbol = %normalized_symbol,
                    source = %ReferencePriceSource::Pyth.as_str(),
                    asset_class = %asset_class.as_str(),
                    carried_forward = snapshot.is_carried_forward,
                    count = message_count,
                    "Pyth reference prices captured"
                );
            }
        }
    }
    warn!(symbol = %subscribe_symbol, "RTDS equity_prices stream ended");
}

/// Persist a spot price tick to `binance_price_ticks` for backtest replay.
/// Called at most once per second per symbol (throttled in spawn_spot_feed).
async fn persist_spot_price(
    pool: &PgPool,
    symbol: &str,
    price: Decimal,
    trade_time: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO binance_price_ticks (symbol, price, trade_time, received_at)
        VALUES ($1, $2, $3, NOW())
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(symbol)
    .bind(price)
    .bind(trade_time)
    .execute(pool)
    .await;

    if let Err(e) = result {
        debug!(symbol, error = %e, "Failed to persist spot price tick");
    }
}

/// Persist a quote tick to `clob_quote_ticks` for backtest replay.
/// Every tick is stored at full resolution (no per-second dedup).
async fn persist_quote(
    pool: &PgPool,
    token_id: &str,
    quote: BookQuote,
    received_at: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO clob_quote_ticks (
            token_id, best_bid, best_ask, bid_size, ask_size, received_at, source
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'ploy_runner_live')
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(token_id)
    .bind(quote.bid)
    .bind(quote.ask)
    .bind(quote.bid_size)
    .bind(quote.ask_size)
    .bind(received_at)
    .execute(pool)
    .await;

    if let Err(e) = result {
        debug!(token_id, error = %e, "Failed to persist quote tick");
    }
}

async fn persist_chainlink_price(
    pool: &PgPool,
    symbol: &str,
    price: Decimal,
    source_timestamp: DateTime<Utc>,
    received_at: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO chainlink_price_ticks (symbol, price, source_timestamp, received_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(symbol)
    .bind(price)
    .bind(source_timestamp)
    .bind(received_at)
    .execute(pool)
    .await;

    if let Err(error) = result {
        debug!(symbol, error = %error, "Failed to persist Chainlink price tick");
    }
}

async fn persist_reference_price(pool: &PgPool, snapshot: &ReferencePriceSnapshot) {
    let result = sqlx::query(
        r#"
        INSERT INTO reference_price_ticks (
            symbol, source, asset_class, price, full_accuracy_value,
            price_time, received_at, is_carried_forward
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(&snapshot.key.symbol)
    .bind(snapshot.key.source.as_str())
    .bind(snapshot.asset_class.as_str())
    .bind(snapshot.value)
    .bind(snapshot.full_accuracy_value.as_deref())
    .bind(snapshot.source_timestamp)
    .bind(snapshot.received_at)
    .bind(snapshot.is_carried_forward)
    .execute(pool)
    .await;

    if let Err(error) = result {
        debug!(
            symbol = %snapshot.key.symbol,
            source = %snapshot.key.source.as_str(),
            error = %error,
            "Failed to persist reference price tick"
        );
    }
}

fn reference_price_update(snapshot: &ReferencePriceSnapshot) -> MarketUpdate {
    MarketUpdate::ReferencePrice {
        symbol: Arc::from(snapshot.key.symbol.as_str()),
        source: Arc::from(snapshot.key.source.as_str()),
        asset_class: Arc::from(snapshot.asset_class.as_str()),
        price: snapshot.value,
        full_accuracy_value: snapshot.full_accuracy_value.as_deref().map(Arc::from),
        is_carried_forward: snapshot.is_carried_forward,
        received_at: Some(snapshot.received_at),
        ts: snapshot.source_timestamp,
    }
}

#[derive(Debug)]
struct AggTradeMsg {
    symbol: String,
    agg_trade_id: i64,
    first_trade_id: i64,
    last_trade_id: i64,
    price: rust_decimal::Decimal,
    quantity: rust_decimal::Decimal,
    trade_time: chrono::DateTime<chrono::Utc>,
    event_time: chrono::DateTime<chrono::Utc>,
    is_buyer_maker: bool,
}

fn parse_agg_trade_msg(v: &serde_json::Value) -> Option<AggTradeMsg> {
    use chrono::TimeZone;
    let symbol = v["s"].as_str()?.to_string();
    let agg_trade_id = v["a"].as_i64()?;
    let first_trade_id = v["f"].as_i64().unwrap_or(0);
    let last_trade_id = v["l"].as_i64().unwrap_or(0);
    let price_str = v["p"].as_str()?;
    let qty_str = v["q"].as_str()?;
    let trade_time_ms = v["T"].as_i64()?;
    let event_time_ms = v["E"].as_i64().unwrap_or(trade_time_ms);
    let is_buyer_maker = v["m"].as_bool().unwrap_or(false);
    let price = price_str.parse::<rust_decimal::Decimal>().ok()?;
    let quantity = qty_str.parse::<rust_decimal::Decimal>().ok()?;
    let trade_time = chrono::Utc.timestamp_millis_opt(trade_time_ms).single()?;
    let event_time = chrono::Utc.timestamp_millis_opt(event_time_ms).single()?;
    Some(AggTradeMsg {
        symbol,
        agg_trade_id,
        first_trade_id,
        last_trade_id,
        price,
        quantity,
        trade_time,
        event_time,
        is_buyer_maker,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        book_quote_from_rest, capture_crossed_clob_failure_and_empty,
        classify_clob_ws_receive_error, db_polymarket_poll_intervals, equity_price_subscription,
        forward_clob_ws_payload, l2_updates_from_book, mark_db_event_expired_if_resolved,
        market_update_from_clob_book, market_updates_from_price_change, parse_agg_trade_msg,
        parse_equity_price_payload, rtds_market_data_ws_config,
        send_quote_collection_failure_and_empty, ClobBookState, RestBook,
        MAX_POLYMARKET_CLOB_PENDING_CHANGES, POLYMARKET_CLOB_CROSSED_BOOK_ERROR, U256,
    };
    use chrono::Utc;
    use ploy_market_contracts::MarketUpdate;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal_macros::dec;
    use serde_json::json;
    use std::collections::HashSet;
    use std::time::Duration;

    fn seeded_clob_books() -> std::collections::HashMap<String, ClobBookState> {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.40", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        std::collections::HashMap::from([("7".to_string(), state)])
    }

    #[test]
    fn dry_run_rtds_market_data_uses_relaxed_ws_heartbeat_settings() {
        let config = rtds_market_data_ws_config();
        assert_eq!(config.heartbeat_interval, Duration::from_secs(15));
        assert_eq!(config.heartbeat_timeout, Duration::from_secs(45));
        assert!(config.reconnect.max_attempts.is_none());
    }

    #[test]
    fn db_polymarket_quotes_refresh_without_accelerating_catalog_queries() {
        let (catalog, quotes) = db_polymarket_poll_intervals();
        assert_eq!(catalog, Duration::from_secs(2));
        assert_eq!(quotes, Duration::from_millis(100));
    }

    #[test]
    fn clob_book_tick_becomes_immediate_depth_quote() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600123",
            "bids": [
                {"price": "0.52", "size": "7.25"},
                {"price": "0.47", "size": "12.5"}
            ],
            "asks": [
                {"price": "0.53", "size": "9.5"},
                {"price": "0.54", "size": "20"}
            ],
            "hash": null
        }))
        .expect("valid CLOB book update");

        let (updates, _) = market_update_from_clob_book(&book, &mut ClobBookState::default())
            .expect("tradeable quote");
        let [update] = updates.try_into().ok().expect("single quote update");
        let MarketUpdate::Quote {
            token_id,
            bid,
            ask,
            bid_size,
            ask_size,
            bid_levels,
            ask_levels,
            ts,
        } = update
        else {
            panic!("expected quote update");
        };

        assert_eq!(token_id.as_ref(), "7");
        assert_eq!(bid, Some(dec!(0.52)));
        assert_eq!(ask, Some(dec!(0.53)));
        assert_eq!(bid_size, Some(dec!(7.25)));
        assert_eq!(ask_size, Some(dec!(9.5)));
        assert_eq!(bid_levels.len(), 2);
        assert_eq!(ask_levels.len(), 2);
        assert_eq!(ts.timestamp_millis(), 1_712_205_600_123);
    }

    #[test]
    fn clob_price_change_before_first_snapshot_is_buffered_without_publishing() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600456",
            "price_changes": [{
                "asset_id": "7",
                "price": "0.53",
                "size": "4",
                "side": "SELL",
                "hash": null,
                "best_bid": "0.51",
                "best_ask": "0.53"
            }]
        }))
        .expect("valid price change");

        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let updates = market_updates_from_price_change(&change, &mut books, &mut timestamps)
            .expect("valid price change");
        assert!(updates.is_empty());
        assert_eq!(books["7"].pending.len(), 1);
        assert!(!books["7"].initialized);
        assert!(!books["7"].dirty);
        assert!(!timestamps.contains_key("7"));
    }

    #[test]
    fn invalid_clob_price_change_timestamp_is_not_silently_dropped() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "9223372036854775807",
            "price_changes": [{
                "asset_id": "7", "price": "0.53", "size": "4", "side": "SELL",
                "hash": null, "best_bid": "0.51", "best_ask": "0.53"
            }]
        }))
        .expect("syntactically valid price change");

        assert!(market_updates_from_price_change(
            &change,
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .is_err());
    }

    #[test]
    fn clob_empty_book_tick_clears_stale_quote() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600789",
            "bids": [],
            "asks": [],
            "hash": null
        }))
        .expect("valid empty CLOB book update");

        let (updates, _) = market_update_from_clob_book(&book, &mut ClobBookState::default())
            .expect("empty book is still a state transition");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                bid: None,
                ask: None,
                bid_size: None,
                ask_size: None,
                bid_levels,
                ask_levels,
                ..
            }] if bid_levels.is_empty() && ask_levels.is_empty()
        ));
    }

    #[test]
    fn malformed_clob_snapshot_is_not_reclassified_as_empty() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600789",
            "bids": [{"price": "0.49", "size": "-1"}],
            "asks": [],
            "hash": null
        }))
        .expect("syntactically valid CLOB book update");

        assert!(market_update_from_clob_book(&book, &mut ClobBookState::default()).is_err());
    }

    #[test]
    fn terminal_only_clob_levels_are_preserved_but_not_executable() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600789",
            "bids": [{"price": "0.01", "size": "5"}],
            "asks": [{"price": "0.99", "size": "6"}],
            "hash": null
        }))
        .expect("valid terminal-only CLOB book update");

        let (updates, _) = market_update_from_clob_book(&book, &mut ClobBookState::default())
            .expect("valid terminal-only book");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                bid: None,
                ask: None,
                bid_size: None,
                ask_size: None,
                bid_levels,
                ask_levels,
                ..
            }] if bid_levels.len() == 1 && ask_levels.len() == 1
        ));
    }

    #[test]
    fn collection_failure_precedes_the_fail_closed_empty_quote() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        let started_at = Utc::now() - chrono::Duration::milliseconds(25);

        assert!(send_quote_collection_failure_and_empty(
            &tx,
            &[U256::from(7)],
            started_at,
            "websocket_receive",
        ));

        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::QuoteCollectionFailure {
                token_id,
                request_started_at,
                http_status: None,
                error_kind,
                ..
            } if token_id.as_ref() == "7"
                && request_started_at == started_at
                && error_kind.as_ref() == "websocket_receive"
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: None,
                ask: None,
                bid_size: None,
                ask_size: None,
                bid_levels,
                ask_levels,
                ..
            } if token_id.as_ref() == "7" && bid_levels.is_empty() && ask_levels.is_empty()
        ));
    }

    #[test]
    fn clob_cancel_tick_updates_cached_depth_before_broadcast() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.52", "size": "7.25"}],
            "asks": [
                {"price": "0.53", "size": "9.5"},
                {"price": "0.54", "size": "20"}
            ],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7",
                "price": "0.53",
                "size": "0",
                "side": "SELL",
                "hash": null,
                "best_bid": "0.52",
                "best_ask": "0.54"
            }]
        }))
        .expect("valid cancellation price change");

        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        let mut books = std::collections::HashMap::from([("7".to_string(), state)]);
        let updates = market_updates_from_price_change(
            &change,
            &mut books,
            &mut std::collections::HashMap::new(),
        )
        .expect("valid cancellation");

        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                ask: Some(ask),
                ask_size: Some(size),
                ask_levels,
                ..
            }] if *ask == dec!(0.54)
                && *size == dec!(20)
                && ask_levels == &vec![ploy_market_contracts::BookLevel {
                    price: dec!(0.54),
                    size: dec!(20),
                }]
        ));
    }

    #[test]
    fn clob_price_change_batch_publishes_only_the_final_book_state() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [
                {
                    "asset_id": "7", "price": "0.61", "size": "10", "side": "BUY",
                    "hash": null, "best_bid": "0.61", "best_ask": "0.62"
                },
                {
                    "asset_id": "7", "price": "0.60", "size": "0", "side": "SELL",
                    "hash": null, "best_bid": "0.61", "best_ask": "0.62"
                },
                {
                    "asset_id": "7", "price": "0.62", "size": "5", "side": "SELL",
                    "hash": null, "best_bid": "0.61", "best_ask": "0.62"
                }
            ]
        }))
        .expect("valid batched price change");

        let mut books = seeded_clob_books();
        let updates = market_updates_from_price_change(
            &change,
            &mut books,
            &mut std::collections::HashMap::new(),
        )
        .expect("valid batched price change");

        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                bid: Some(bid),
                ask: Some(ask),
                bid_levels,
                ask_levels,
                ..
            }] if *bid == dec!(0.61)
                && *ask == dec!(0.62)
                && bid_levels.first().is_some_and(|level| level.price == dec!(0.61))
                && ask_levels.first().is_some_and(|level| level.price == dec!(0.62))
        ));
    }

    #[test]
    fn clob_crossed_final_batch_requires_a_fresh_snapshot() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.61", "size": "10", "side": "BUY",
                "hash": null, "best_bid": "0.61", "best_ask": "0.60"
            }]
        }))
        .expect("valid crossed price change");

        let mut books = seeded_clob_books();

        assert!(market_updates_from_price_change(
            &change,
            &mut books,
            &mut std::collections::HashMap::new(),
        )
        .is_err());
    }

    #[test]
    fn clob_positive_level_outside_reported_bba_fails_closed() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.61", "size": "10", "side": "BUY",
                "hash": null, "best_bid": "0.60", "best_ask": "0.62"
            }]
        }))
        .expect("syntactically valid but inconsistent price change");

        assert!(market_updates_from_price_change(
            &change,
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .is_err());
    }

    #[test]
    fn clob_missing_reported_bba_dirties_token_until_fresh_snapshot() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}, {"price": "0.38", "size": "5"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.41", "size": "10", "side": "SELL",
                "hash": null, "best_bid": "0.40", "best_ask": "0.41"
            }]
        }))
        .expect("valid price change whose reported bid is missing locally");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        let mut books = std::collections::HashMap::from([("7".to_string(), state)]);
        let mut timestamps = std::collections::HashMap::new();

        let updates = market_updates_from_price_change(&change, &mut books, &mut timestamps)
            .expect("missing authoritative BBA must not reject the whole batch");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::QuoteCollectionFailure {
                token_id,
                error_kind,
                http_status: None,
                ..
            }, MarketUpdate::Quote {
                token_id: quote_token_id,
                bid: None,
                ask: None,
                bid_size: None,
                ask_size: None,
                bid_levels,
                ask_levels,
                ..
            }] if token_id.as_ref() == "7"
                && error_kind.as_ref() == "websocket_payload"
                && quote_token_id.as_ref() == "7"
                && bid_levels.is_empty()
                && ask_levels.is_empty()
        ));
        assert!(!books["7"].initialized);
        assert!(books["7"].dirty);

        let fresh_book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "bids": [{"price": "0.42", "size": "6"}],
            "asks": [{"price": "0.43", "size": "8"}],
            "hash": null
        }))
        .expect("valid healing CLOB book update");
        market_update_from_clob_book(&fresh_book, books.get_mut("7").expect("dirty token state"))
            .expect("fresh snapshot reinitializes the dirty token");
        assert!(books["7"].initialized);
        assert!(!books["7"].dirty);
        let healed_change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600400",
            "price_changes": [{
                "asset_id": "7", "price": "0.41", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.42", "best_ask": "0.43"
            }]
        }))
        .expect("valid post-heal price change");
        let healed = market_updates_from_price_change(&healed_change, &mut books, &mut timestamps)
            .expect("post-heal price change publishes normally");
        assert!(matches!(
            healed.as_slice(),
            [MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ..
            }] if token_id.as_ref() == "7" && *bid == dec!(0.42) && *ask == dec!(0.43)
        ));
    }

    #[test]
    fn clob_dirty_token_stays_failed_closed_until_fresh_snapshot() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}, {"price": "0.38", "size": "5"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let dirtying_change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.41", "size": "10", "side": "SELL",
                "hash": null, "best_bid": "0.40", "best_ask": "0.41"
            }]
        }))
        .expect("valid price change whose reported bid is missing locally");
        let follow_up = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "price_changes": [{
                "asset_id": "7", "price": "0.45", "size": "5", "side": "BUY",
                "hash": null, "best_bid": "0.45", "best_ask": "0.46"
            }]
        }))
        .expect("valid follow-up price change before any healing snapshot");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        let mut books = std::collections::HashMap::from([("7".to_string(), state)]);
        let mut timestamps = std::collections::HashMap::new();

        market_updates_from_price_change(&dirtying_change, &mut books, &mut timestamps)
            .expect("missing authoritative BBA must not reject the whole batch");
        assert!(!books["7"].initialized);
        assert!(books["7"].dirty);

        let updates = market_updates_from_price_change(&follow_up, &mut books, &mut timestamps)
            .expect("a dirty token must stay failed closed, not reject the batch");
        assert!(
            matches!(
                updates.as_slice(),
                [MarketUpdate::QuoteCollectionFailure {
                    token_id,
                    error_kind,
                    ..
                }, MarketUpdate::Quote {
                    token_id: quote_token_id,
                    bid: None,
                    ask: None,
                    bid_size: None,
                    ask_size: None,
                    bid_levels,
                    ask_levels,
                    ..
                }] if token_id.as_ref() == "7"
                    && error_kind.as_ref() == "websocket_payload"
                    && quote_token_id.as_ref() == "7"
                    && bid_levels.is_empty()
                    && ask_levels.is_empty()
            ),
            "a dirty token must not silently reopen on unverified entry-reported BBA"
        );

        let fresh_book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600400",
            "bids": [{"price": "0.42", "size": "6"}],
            "asks": [{"price": "0.43", "size": "8"}],
            "hash": null
        }))
        .expect("valid healing CLOB book update");
        market_update_from_clob_book(&fresh_book, books.get_mut("7").expect("dirty token state"))
            .expect("fresh snapshot reinitializes the dirty token");
        assert!(books["7"].initialized);
        assert!(!books["7"].dirty);
        let healed_change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600500",
            "price_changes": [{
                "asset_id": "7", "price": "0.41", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.42", "best_ask": "0.43"
            }]
        }))
        .expect("valid post-heal price change");
        let healed = market_updates_from_price_change(&healed_change, &mut books, &mut timestamps)
            .expect("post-heal price change publishes normally");
        assert!(matches!(
            healed.as_slice(),
            [MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ..
            }] if token_id.as_ref() == "7" && *bid == dec!(0.42) && *ask == dec!(0.43)
        ));
    }

    #[test]
    fn clob_missing_reported_bba_does_not_fail_healthy_batch_tokens() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}, {"price": "0.38", "size": "5"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let healthy_book = serde_json::from_value(json!({
            "asset_id": "8",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.45", "size": "5"}],
            "asks": [{"price": "0.50", "size": "6"}],
            "hash": null
        }))
        .expect("valid healthy CLOB book update");
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [
                {
                    "asset_id": "7", "price": "0.41", "size": "10", "side": "SELL",
                    "hash": null, "best_bid": "0.40", "best_ask": "0.41"
                },
                {
                    "asset_id": "8", "price": "0.45", "size": "8", "side": "BUY",
                    "hash": null, "best_bid": "0.45", "best_ask": "0.50"
                }
            ]
        }))
        .expect("valid batch mixing a dirty and a healthy token");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        let mut healthy_state = ClobBookState::default();
        market_update_from_clob_book(&healthy_book, &mut healthy_state)
            .expect("healthy initial snapshot");
        let mut books = std::collections::HashMap::from([
            ("7".to_string(), state),
            ("8".to_string(), healthy_state),
        ]);

        let updates = market_updates_from_price_change(
            &change,
            &mut books,
            &mut std::collections::HashMap::new(),
        )
        .expect("a dirty token must not reject healthy batch entries");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::QuoteCollectionFailure { token_id, .. }, MarketUpdate::Quote {
                token_id: empty_token_id,
                bid: None,
                ask: None,
                ..
            }, MarketUpdate::Quote {
                token_id: healthy_token_id,
                bid: Some(bid),
                ask: Some(ask),
                bid_size: Some(bid_size),
                ..
            }] if token_id.as_ref() == "7"
                && empty_token_id.as_ref() == "7"
                && healthy_token_id.as_ref() == "8"
                && *bid == dec!(0.45)
                && *ask == dec!(0.50)
                && *bid_size == dec!(8)
        ));
        assert!(!books["7"].initialized);
        assert!(books["7"].dirty);
        assert!(books["8"].initialized);
        assert!(!books["8"].dirty);
    }

    #[test]
    fn clob_stale_book_resyncs_buffering_token_but_fails_closed_for_healthy_token() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let early_change = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600200","price_changes":[{"asset_id":"7","price":"0.52","size":"7","side":"BUY","hash":null,"best_bid":"0.52","best_ask":"0.53"}]}"#;
        let stale_book = br#"{"event_type":"book","asset_id":"7","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600100","bids":[{"price":"0.52","size":"7"}],"asks":[{"price":"0.53","size":"9"}]}"#;
        let healthy_book = br#"{"event_type":"book","asset_id":"8","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600300","bids":[{"price":"0.45","size":"5"}],"asks":[{"price":"0.50","size":"6"}]}"#;
        let backwards_book = br#"{"event_type":"book","asset_id":"8","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600250","bids":[{"price":"0.44","size":"5"}],"asks":[{"price":"0.51","size":"6"}]}"#;

        assert!(
            forward_clob_ws_payload(early_change, &tx, &mut books, &mut timestamps)
                .expect("delta arriving before any snapshot is buffered for replay")
        );
        assert!(!books["7"].initialized);
        assert!(
            !books["7"].dirty,
            "a new token without any snapshot is syncing, not dirty"
        );
        assert_eq!(books["7"].pending.len(), 1);
        assert!(
            !timestamps.contains_key("7"),
            "a buffered delta must not poison last_timestamp"
        );
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "a syncing token publishes nothing until its first snapshot"
        );

        assert!(
            forward_clob_ws_payload(stale_book, &tx, &mut books, &mut timestamps)
                .expect("a syncing token has no valid state to protect from an older snapshot")
        );
        assert!(books["7"].initialized);
        assert!(!books["7"].dirty);
        assert!(books["7"].pending.is_empty());
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ..
            } if token_id.as_ref() == "7" && bid == dec!(0.52) && ask == dec!(0.53)
        ));

        assert!(
            forward_clob_ws_payload(healthy_book, &tx, &mut books, &mut timestamps)
                .expect("healthy snapshot")
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote { token_id, .. } if token_id.as_ref() == "8"
        ));
        let error = forward_clob_ws_payload(backwards_book, &tx, &mut books, &mut timestamps)
            .expect_err("initialized tokens keep the backwards-timestamp protection");
        assert!(error.contains("moved backwards"));
    }

    #[test]
    fn clob_missing_reported_bba_keeps_socket_alive_at_feed_layer() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let snapshot = br#"{"event_type":"book","asset_id":"7","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600100","bids":[{"price":"0.50","size":"7"},{"price":"0.38","size":"5"}],"asks":[{"price":"0.60","size":"9"}]}"#;
        let missing_bba = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600200","price_changes":[{"asset_id":"7","price":"0.41","size":"10","side":"SELL","hash":null,"best_bid":"0.40","best_ask":"0.41"}]}"#;

        assert!(forward_clob_ws_payload(snapshot, &tx, &mut books, &mut timestamps).unwrap());
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote { token_id, .. } if token_id.as_ref() == "7"
        ));

        assert!(
            forward_clob_ws_payload(missing_bba, &tx, &mut books, &mut timestamps)
                .expect("missing BBA must isolate the token instead of breaking the socket")
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::QuoteCollectionFailure {
                token_id,
                error_kind,
                ..
            } if token_id.as_ref() == "7" && error_kind.as_ref() == "websocket_payload"
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: None,
                ask: None,
                ..
            } if token_id.as_ref() == "7"
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(!books["7"].initialized);
        assert!(books["7"].dirty);
    }

    #[test]
    fn clob_price_change_before_first_snapshot_is_buffered_then_replayed() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let early_change = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600200","price_changes":[{"asset_id":"7","price":"0.55","size":"3","side":"BUY","hash":null,"best_bid":"0.55","best_ask":"0.60"}]}"#;
        let snapshot = br#"{"event_type":"book","asset_id":"7","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600100","bids":[{"price":"0.50","size":"7"}],"asks":[{"price":"0.60","size":"9"}]}"#;
        let follow_up = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600300","price_changes":[{"asset_id":"7","price":"0.54","size":"2","side":"BUY","hash":null,"best_bid":"0.55","best_ask":"0.60"}]}"#;

        assert!(
            forward_clob_ws_payload(early_change, &tx, &mut books, &mut timestamps)
                .expect("buffering a pre-snapshot delta must not error")
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(!timestamps.contains_key("7"));

        assert!(
            forward_clob_ws_payload(snapshot, &tx, &mut books, &mut timestamps)
                .expect("the first snapshot resyncs the buffered delta")
        );
        assert!(books["7"].initialized);
        assert!(books["7"].pending.is_empty());
        assert_eq!(timestamps["7"], 1_712_205_600_200);
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ..
            } if token_id.as_ref() == "7" && bid == dec!(0.55) && ask == dec!(0.60)
        ));

        assert!(
            forward_clob_ws_payload(follow_up, &tx, &mut books, &mut timestamps)
                .expect("post-resync delta publishes normally")
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ..
            } if token_id.as_ref() == "7" && bid == dec!(0.55) && ask == dec!(0.60)
        ));
        assert!(!books["7"].dirty);
    }

    #[test]
    fn clob_replayed_delta_fills_snapshot_gap_so_bba_reconciles() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600150",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid pre-snapshot price change");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();

        let updates = market_updates_from_price_change(&change, &mut books, &mut timestamps)
            .expect("pre-snapshot delta is buffered");
        assert!(updates.is_empty());
        assert_eq!(books["7"].pending.len(), 1);

        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let (updates, watermark) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("snapshot replays the buffered delta");
        assert_eq!(watermark, 1_712_205_600_150);
        timestamps.insert("7".to_string(), watermark);
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                bid: Some(bid),
                ask: Some(ask),
                ..
            }] if *bid == dec!(0.55) && *ask == dec!(0.60)
        ));

        let report = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "5", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid post-resync price change");
        let updates = market_updates_from_price_change(&report, &mut books, &mut timestamps)
            .expect("reported BBA reconciles against the replayed level");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote { bid: Some(bid), .. }] if *bid == dec!(0.55)
        ));
        assert!(!books["7"].dirty);
    }

    #[test]
    fn clob_replay_drops_changes_older_than_the_snapshot() {
        let stale_change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600050",
            "price_changes": [{
                "asset_id": "7", "price": "0.70", "size": "1", "side": "BUY",
                "hash": null, "best_bid": "0.70", "best_ask": "0.75"
            }]
        }))
        .expect("valid pre-snapshot price change");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();

        let updates = market_updates_from_price_change(&stale_change, &mut books, &mut timestamps)
            .expect("pre-snapshot delta is buffered");
        assert!(updates.is_empty());

        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let (updates, watermark) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("snapshot applies");
        assert_eq!(watermark, 1_712_205_600_100);
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                bid: Some(bid),
                ask: Some(ask),
                ..
            }] if *bid == dec!(0.50) && *ask == dec!(0.60)
        ));
        assert!(!books["7"].bids.contains_key(&dec!(0.70)));
        assert!(books["7"].pending.is_empty());
    }

    #[test]
    fn clob_replay_applies_same_millisecond_deltas_like_the_live_path() {
        let stale_change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600050",
            "price_changes": [{
                "asset_id": "7", "price": "0.70", "size": "1", "side": "BUY",
                "hash": null, "best_bid": "0.70", "best_ask": "0.75"
            }]
        }))
        .expect("valid older pre-snapshot price change");
        let same_millisecond = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid same-millisecond pre-snapshot price change");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();

        market_updates_from_price_change(&stale_change, &mut books, &mut timestamps)
            .expect("older delta is buffered");
        market_updates_from_price_change(&same_millisecond, &mut books, &mut timestamps)
            .expect("same-millisecond delta is buffered");

        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let (updates, watermark) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("snapshot applies");
        assert_eq!(watermark, 1_712_205_600_100);
        assert!(
            matches!(
                updates.as_slice(),
                [MarketUpdate::Quote {
                    bid: Some(bid),
                    ask: Some(ask),
                    ..
                }] if *bid == dec!(0.55) && *ask == dec!(0.60)
            ),
            "a delta sharing the snapshot millisecond must replay like the live path"
        );
        assert!(books["7"].bids.contains_key(&dec!(0.55)));
        assert!(!books["7"].bids.contains_key(&dec!(0.70)));
        assert!(books["7"].pending.is_empty());
    }

    #[test]
    fn clob_replay_crossed_book_isolates_token_instead_of_publishing() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        // Without BBA fields apply() cannot prune the opposite side, so the
        // replayed level crosses the snapshot ask.
        let crossing = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600150",
            "price_changes": [{
                "asset_id": "7", "price": "0.65", "size": "3", "side": "BUY", "hash": null
            }]
        }))
        .expect("valid crossing pre-snapshot price change");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        market_updates_from_price_change(&crossing, &mut books, &mut timestamps)
            .expect("crossing delta is buffered");

        let (updates, _) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("a crossed replay must isolate the token, not error the batch");
        assert!(
            matches!(
                updates.as_slice(),
                [MarketUpdate::QuoteCollectionFailure {
                    token_id,
                    error_kind,
                    ..
                }, MarketUpdate::Quote {
                    token_id: quote_token_id,
                    bid: None,
                    ask: None,
                    ..
                }] if token_id.as_ref() == "7"
                    && error_kind.as_ref() == "websocket_payload"
                    && quote_token_id.as_ref() == "7"
            ),
            "a crossed replayed book must fail closed instead of publishing"
        );
        assert!(books["7"].dirty);
    }

    #[test]
    fn clob_replayed_quote_carries_the_replay_watermark_timestamp() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600150",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid pre-snapshot price change");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        market_updates_from_price_change(&change, &mut books, &mut timestamps)
            .expect("pre-snapshot delta is buffered");

        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let (updates, _) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("snapshot replays the buffered delta");
        assert!(
            matches!(
                updates.as_slice(),
                [MarketUpdate::Quote { ts, .. }] if ts.timestamp_millis() == 1_712_205_600_150
            ),
            "a quote built from replayed deltas must carry the replay watermark, \
             not the older snapshot timestamp"
        );
    }

    #[test]
    fn clob_replay_reconciles_every_buffered_message_not_just_the_last() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let missing_bba = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600150",
            "price_changes": [{
                "asset_id": "7", "price": "0.41", "size": "10", "side": "SELL",
                "hash": null, "best_bid": "0.40", "best_ask": "0.41"
            }]
        }))
        .expect("valid earlier message whose reported bid is missing locally");
        let no_bba = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "3", "side": "SELL", "hash": null
            }]
        }))
        .expect("valid later message without BBA fields");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        market_updates_from_price_change(&missing_bba, &mut books, &mut timestamps)
            .expect("earlier message is buffered");
        market_updates_from_price_change(&no_bba, &mut books, &mut timestamps)
            .expect("later message is buffered");

        let (updates, _) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("reconciliation isolates the token in-band");
        assert!(
            matches!(
                updates.as_slice(),
                [MarketUpdate::QuoteCollectionFailure { token_id, .. }, MarketUpdate::Quote {
                    token_id: quote_token_id,
                    bid: None,
                    ask: None,
                    ..
                }] if token_id.as_ref() == "7" && quote_token_id.as_ref() == "7"
            ),
            "an earlier message's missing-BBA evidence must not be drowned by a later message"
        );
        assert!(books["7"].dirty);
    }

    #[test]
    fn clob_replay_skips_regressing_messages_and_keeps_watermark() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.50", "size": "7"}],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let newer = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid newer buffered message");
        let regressing = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "99", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid regressing buffered message");
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        market_updates_from_price_change(&newer, &mut books, &mut timestamps)
            .expect("newer message is buffered");
        market_updates_from_price_change(&regressing, &mut books, &mut timestamps)
            .expect("regressing message is buffered");

        let (updates, watermark) =
            market_update_from_clob_book(&book, books.get_mut("7").expect("syncing token state"))
                .expect("snapshot replays non-regressing messages");
        assert_eq!(watermark, 1_712_205_600_300);
        assert!(
            matches!(
                updates.as_slice(),
                [MarketUpdate::Quote {
                    bid: Some(bid),
                    bid_size: Some(size),
                    ..
                }] if *bid == dec!(0.55) && *size == dec!(3)
            ),
            "a regressing buffered message must not overwrite newer replayed state"
        );
        assert_eq!(books["7"].bids[&dec!(0.55)], dec!(3));
    }

    #[test]
    fn clob_resubscription_rebuilds_sync_state_without_backwards_errors() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let first_book = br#"{"event_type":"book","asset_id":"7","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600300","bids":[{"price":"0.40","size":"7"}],"asks":[{"price":"0.60","size":"9"}]}"#;
        let early_change = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600150","price_changes":[{"asset_id":"7","price":"0.55","size":"3","side":"BUY","hash":null,"best_bid":"0.55","best_ask":"0.60"}]}"#;
        let resync_book = br#"{"event_type":"book","asset_id":"7","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600100","bids":[{"price":"0.50","size":"7"}],"asks":[{"price":"0.60","size":"9"}]}"#;
        let stale_delta = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600120","price_changes":[{"asset_id":"7","price":"0.55","size":"5","side":"BUY","hash":null,"best_bid":"0.55","best_ask":"0.60"}]}"#;
        let fresh_delta = br#"{"event_type":"price_change","market":"0x0000000000000000000000000000000000000000000000000000000000000000","timestamp":"1712205600160","price_changes":[{"asset_id":"7","price":"0.54","size":"2","side":"BUY","hash":null,"best_bid":"0.55","best_ask":"0.60"}]}"#;

        assert!(forward_clob_ws_payload(first_book, &tx, &mut books, &mut timestamps).unwrap());
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote { token_id, .. } if token_id.as_ref() == "7"
        ));

        // The spawn loop clears both maps on reconnect; resubscription then
        // restarts the token in syncing state.
        books.clear();
        timestamps.clear();

        assert!(
            forward_clob_ws_payload(early_change, &tx, &mut books, &mut timestamps)
                .expect("post-reconnect delta is buffered")
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        assert!(
            forward_clob_ws_payload(resync_book, &tx, &mut books, &mut timestamps)
                .expect("resubscription resyncs without a backwards error")
        );
        assert_eq!(timestamps["7"], 1_712_205_600_150);
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                ..
            } if token_id.as_ref() == "7" && bid == dec!(0.55) && ask == dec!(0.60)
        ));

        assert!(
            forward_clob_ws_payload(stale_delta, &tx, &mut books, &mut timestamps)
                .expect("replayed watermark makes an older delta skippable, not fatal")
        );
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        assert!(
            forward_clob_ws_payload(fresh_delta, &tx, &mut books, &mut timestamps)
                .expect("fresh delta publishes after resync")
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ..
            } if token_id.as_ref() == "7" && bid == dec!(0.55)
        ));
    }

    #[test]
    fn clob_syncing_buffer_overflow_fails_closed() {
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        for i in 0..MAX_POLYMARKET_CLOB_PENDING_CHANGES {
            let change = serde_json::from_value(json!({
                "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
                "timestamp": (1_712_205_600_000i64 + i as i64).to_string(),
                "price_changes": [{
                    "asset_id": "7", "price": "0.55", "size": "3", "side": "BUY",
                    "hash": null, "best_bid": "0.55", "best_ask": "0.60"
                }]
            }))
            .expect("valid buffered price change");
            let updates = market_updates_from_price_change(&change, &mut books, &mut timestamps)
                .expect("buffered delta");
            assert!(updates.is_empty());
        }
        assert_eq!(
            books["7"].pending.len(),
            MAX_POLYMARKET_CLOB_PENDING_CHANGES
        );

        let overflow = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": (1_712_205_600_000i64
                + MAX_POLYMARKET_CLOB_PENDING_CHANGES as i64)
                .to_string(),
            "price_changes": [{
                "asset_id": "7", "price": "0.55", "size": "3", "side": "BUY",
                "hash": null, "best_bid": "0.55", "best_ask": "0.60"
            }]
        }))
        .expect("valid overflowing price change");
        let updates = market_updates_from_price_change(&overflow, &mut books, &mut timestamps)
            .expect("overflow fails the token closed instead of growing the buffer");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::QuoteCollectionFailure {
                token_id,
                error_kind,
                ..
            }, MarketUpdate::Quote {
                token_id: quote_token_id,
                bid: None,
                ask: None,
                ..
            }] if token_id.as_ref() == "7"
                && error_kind.as_ref() == "websocket_payload"
                && quote_token_id.as_ref() == "7"
        ));
        assert!(books["7"].dirty);
        assert!(books["7"].pending.is_empty());

        let updates = market_updates_from_price_change(&overflow, &mut books, &mut timestamps)
            .expect("a dirty token stays failed closed");
        assert_eq!(updates.len(), 2);
        assert!(books["7"].pending.is_empty());
    }

    #[test]
    fn clob_ws_receive_error_classification_splits_transport_from_protocol() {
        use tokio_tungstenite::tungstenite::error::ProtocolError;
        use tokio_tungstenite::tungstenite::Error;

        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::NotConnected,
        ] {
            assert_eq!(
                classify_clob_ws_receive_error(&Error::Io(std::io::Error::new(kind, "io"))),
                "transport_reconnect"
            );
        }
        assert_eq!(
            classify_clob_ws_receive_error(&Error::Protocol(
                ProtocolError::ResetWithoutClosingHandshake
            )),
            "transport_reconnect",
            "a TCP reset surfaces through the protocol layer but stays transport evidence"
        );
        assert_eq!(
            classify_clob_ws_receive_error(&Error::Protocol(ProtocolError::InvalidOpcode(9))),
            "websocket_payload"
        );
        assert_eq!(
            classify_clob_ws_receive_error(&Error::Utf8("bad utf8".to_string())),
            "websocket_payload"
        );
        assert_eq!(
            classify_clob_ws_receive_error(&Error::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timeout"
            ))),
            "websocket_payload"
        );
    }

    #[test]
    fn clob_cancellation_bba_prunes_only_more_competitive_stale_depth() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [
                {"price": "0.40", "size": "7"},
                {"price": "0.35", "size": "6"},
                {"price": "0.30", "size": "5"}
            ],
            "asks": [{"price": "0.60", "size": "9"}],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.35", "size": "0", "side": "BUY",
                "hash": null, "best_bid": "0.30", "best_ask": "0.60"
            }]
        }))
        .expect("valid cancellation with post-event BBA");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        let mut books = std::collections::HashMap::from([("7".to_string(), state)]);

        let updates = market_updates_from_price_change(
            &change,
            &mut books,
            &mut std::collections::HashMap::new(),
        )
        .expect("cancellation BBA should reconcile stale top levels");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote {
                bid: Some(bid),
                ask: Some(ask),
                bid_levels,
                ask_levels,
                ..
            }] if *bid == dec!(0.30)
                && *ask == dec!(0.60)
                && bid_levels.iter().map(|level| (level.price, level.size)).collect::<Vec<_>>()
                    == vec![(dec!(0.30), dec!(5))]
                && ask_levels.iter().map(|level| (level.price, level.size)).collect::<Vec<_>>()
                    == vec![(dec!(0.60), dec!(9))]
        ));
        assert!(!books["7"].bids.contains_key(&dec!(0.40)));
        assert!(!books["7"].bids.contains_key(&dec!(0.35)));
        assert!(books["7"].bids.contains_key(&dec!(0.30)));
    }

    #[test]
    fn clob_invalid_later_bba_does_not_mutate_prior_token() {
        let first_book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.38", "size": "2"}],
            "asks": [{"price": "0.39", "size": "1"}, {"price": "0.41", "size": "3"}],
            "hash": null
        }))
        .expect("valid first CLOB book update");
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [
                {
                    "asset_id": "7", "price": "0.40", "size": "10", "side": "BUY",
                    "hash": null, "best_bid": "0.40", "best_ask": "0.41"
                },
                {
                    "asset_id": "8", "price": "0.61", "size": "10", "side": "BUY",
                    "hash": null, "best_bid": "0.60", "best_ask": "0.62"
                }
            ]
        }))
        .expect("syntactically valid batch with an inconsistent later entry");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&first_book, &mut state).expect("initial snapshot");
        let mut books = std::collections::HashMap::from([("7".to_string(), state)]);

        assert!(market_updates_from_price_change(
            &change,
            &mut books,
            &mut std::collections::HashMap::new(),
        )
        .is_err());
        assert!(!books["7"].bids.contains_key(&dec!(0.40)));
        assert!(books["7"].asks.contains_key(&dec!(0.39)));
        assert!(!books.contains_key("8"));
    }

    #[test]
    fn clob_price_change_bba_prunes_stale_crossed_levels_per_token() {
        use sha2::Digest;

        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let up_token =
            "41542505488057268747646102765538559787092195571513884627130758935628213319126";
        let down_token =
            "7335525875962923702441063748642161498788112653863579087206891242613686090845";
        let up_snapshot = br#"{"event_type":"book","asset_id":"41542505488057268747646102765538559787092195571513884627130758935628213319126","market":"0xa37566a917002ef3c9267135f9de9a3ba64c0c422ba764cb20a44f5f36451f47","timestamp":"1785328736000","bids":[{"price":"0.38","size":"2"}],"asks":[{"price":"0.39","size":"1"},{"price":"0.41","size":"3"}]}"#;
        let down_snapshot = br#"{"event_type":"book","asset_id":"7335525875962923702441063748642161498788112653863579087206891242613686090845","market":"0xa37566a917002ef3c9267135f9de9a3ba64c0c422ba764cb20a44f5f36451f47","timestamp":"1785328736000","bids":[{"price":"0.61","size":"1"},{"price":"0.59","size":"4"}],"asks":[{"price":"0.62","size":"5"}]}"#;
        let payload = br#"{"market":"0xa37566a917002ef3c9267135f9de9a3ba64c0c422ba764cb20a44f5f36451f47", "price_changes":[{"asset_id":"41542505488057268747646102765538559787092195571513884627130758935628213319126", "price":"0.4", "size":"566.56", "side":"BUY", "hash":"4d147f0d1fc447309a9fbfe2ead47ad805f70133", "best_bid":"0.4", "best_ask":"0.41"}, {"asset_id":"7335525875962923702441063748642161498788112653863579087206891242613686090845", "price":"0.6", "size":"566.56", "side":"SELL", "hash":"d4344ba2f0f8e944ddb9fdd722cd7ff0e791abd2", "best_bid":"0.59", "best_ask":"0.6"}], "timestamp":"1785328736855", "event_type":"price_change"}"#;

        assert_eq!(payload.len(), 611);
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(payload)),
            "a69df924185368799f86d441c9250083ede2ca7fc943084e6524c03600f4f2df"
        );
        assert!(forward_clob_ws_payload(up_snapshot, &tx, &mut books, &mut timestamps).unwrap());
        assert!(forward_clob_ws_payload(down_snapshot, &tx, &mut books, &mut timestamps).unwrap());
        rx.try_recv().expect("up snapshot quote");
        rx.try_recv().expect("down snapshot quote");

        assert!(
            forward_clob_ws_payload(payload, &tx, &mut books, &mut timestamps)
                .expect("provider BBA should reconcile stale cached top levels")
        );

        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                bid_levels,
                ask_levels,
                ..
            } if token_id.as_ref() == up_token
                && bid == dec!(0.4)
                && ask == dec!(0.41)
                && bid_levels.iter().map(|level| (level.price, level.size)).collect::<Vec<_>>()
                    == vec![(dec!(0.4), dec!(566.56)), (dec!(0.38), dec!(2))]
                && ask_levels.iter().map(|level| (level.price, level.size)).collect::<Vec<_>>()
                    == vec![(dec!(0.41), dec!(3))]
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: Some(bid),
                ask: Some(ask),
                bid_levels,
                ask_levels,
                ..
            } if token_id.as_ref() == down_token
                && bid == dec!(0.59)
                && ask == dec!(0.6)
                && bid_levels.iter().map(|level| (level.price, level.size)).collect::<Vec<_>>()
                    == vec![(dec!(0.59), dec!(4))]
                && ask_levels.iter().map(|level| (level.price, level.size)).collect::<Vec<_>>()
                    == vec![(dec!(0.6), dec!(566.56)), (dec!(0.62), dec!(5))]
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn clob_failure_capture_is_byte_exact_create_once_and_only_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        let capture = temp.path().canonicalize().unwrap().join("clob-failure.raw");
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut books = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        let snapshot = br#"{
            "event_type":"book",
            "asset_id":"7",
            "market":"0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp":"1712205600100",
            "bids":[{"price":"0.40","size":"7"}],
            "asks":[{"price":"0.60","size":"9"}]
        }"#;
        let crossed = br#"{
            "event_type":"price_change",
            "market":"0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp":"1712205600200",
            "price_changes":[{
                "asset_id":"7","price":"0.61","size":"10","side":"BUY",
                "best_bid":"0.61","best_ask":"0.60"
            }]
        }"#;

        assert!(forward_clob_ws_payload(snapshot, &tx, &mut books, &mut timestamps,).unwrap());
        assert!(!capture.exists());
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote { token_id, .. } if token_id.as_ref() == "7"
        ));
        assert!(
            !capture_crossed_clob_failure_and_empty(
                &tx,
                &[U256::from(7)],
                Utc::now(),
                Some(&capture),
                b"not captured",
                "websocket_receive",
            )
            .await
        );
        assert!(!capture.exists());
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        let error = forward_clob_ws_payload(crossed, &tx, &mut books, &mut timestamps).unwrap_err();
        assert!(error.contains("crossed book"));
        assert!(
            capture_crossed_clob_failure_and_empty(
                &tx,
                &[U256::from(7)],
                Utc::now(),
                Some(&capture),
                crossed,
                &error,
            )
            .await
        );
        assert_eq!(std::fs::read(&capture).unwrap(), crossed);
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::QuoteCollectionFailure { token_id, .. }
                if token_id.as_ref() == "7"
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                token_id,
                bid: None,
                ask: None,
                ..
            } if token_id.as_ref() == "7"
        ));

        let crossed_again = crossed
            .strip_suffix(b"}")
            .unwrap()
            .iter()
            .copied()
            .chain(b" }".iter().copied())
            .collect::<Vec<_>>();
        assert!(
            capture_crossed_clob_failure_and_empty(
                &tx,
                &[U256::from(7)],
                Utc::now(),
                Some(&capture),
                &crossed_again,
                &error,
            )
            .await
        );
        assert_eq!(std::fs::read(capture).unwrap(), crossed);
    }

    #[tokio::test]
    async fn clob_failure_capture_rejects_an_unbounded_payload() {
        let temp = tempfile::tempdir().unwrap();
        let capture = temp.path().canonicalize().unwrap().join("clob-failure.raw");
        let (tx, _rx) = tokio::sync::broadcast::channel(2);
        assert!(
            capture_crossed_clob_failure_and_empty(
                &tx,
                &[U256::from(7)],
                Utc::now(),
                Some(&capture),
                &vec![b' '; 1_048_577],
                POLYMARKET_CLOB_CROSSED_BOOK_ERROR,
            )
            .await
        );
        assert!(!capture.exists());
    }

    #[test]
    fn stale_clob_price_change_is_skipped_without_dropping_fresh_batch_entries() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.52", "size": "7.25"}],
            "asks": [
                {"price": "0.53", "size": "9.5"},
                {"price": "0.54", "size": "20"}
            ],
            "hash": null
        }))
        .expect("valid CLOB book update");
        let newer = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "price_changes": [{
                "asset_id": "7", "price": "0.53", "size": "0", "side": "SELL",
                "hash": null, "best_bid": "0.52", "best_ask": "0.54"
            }]
        }))
        .expect("newer price change");
        let mixed = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [
                {
                    "asset_id": "7", "price": "0.53", "size": "100", "side": "SELL",
                    "hash": null, "best_bid": "0.52", "best_ask": "0.53"
                },
                {
                    "asset_id": "8", "price": "0.45", "size": "12", "side": "BUY",
                    "hash": null, "best_bid": "0.45", "best_ask": "0.50"
                }
            ]
        }))
        .expect("mixed stale and fresh price change");
        let same_millisecond = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "price_changes": [{
                "asset_id": "7", "price": "0.54", "size": "30", "side": "SELL",
                "hash": null, "best_bid": "0.52", "best_ask": "0.54"
            }]
        }))
        .expect("same-millisecond price change");

        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("initial snapshot");
        let second_book = serde_json::from_value(json!({
            "asset_id": "8",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600100",
            "bids": [{"price": "0.40", "size": "5"}],
            "asks": [{"price": "0.50", "size": "6"}],
            "hash": null
        }))
        .expect("valid second CLOB book update");
        let mut second_state = ClobBookState::default();
        market_update_from_clob_book(&second_book, &mut second_state).expect("second snapshot");
        let mut books = std::collections::HashMap::from([
            ("7".to_string(), state),
            ("8".to_string(), second_state),
        ]);
        let mut timestamps = std::collections::HashMap::from([
            ("7".to_string(), book.timestamp),
            ("8".to_string(), second_book.timestamp),
        ]);
        assert_eq!(
            market_updates_from_price_change(&newer, &mut books, &mut timestamps)
                .expect("valid newer price change")
                .len(),
            1
        );
        assert_eq!(
            market_updates_from_price_change(&same_millisecond, &mut books, &mut timestamps,)
                .expect("valid same-millisecond price change")
                .len(),
            1,
            "distinct deltas sharing a wire millisecond must not be dropped"
        );
        let updates = market_updates_from_price_change(&mixed, &mut books, &mut timestamps)
            .expect("stale entry is superseded without rejecting the fresh entry");
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::Quote { token_id, bid: Some(bid), .. }]
                if token_id.as_ref() == "8" && *bid == dec!(0.45)
        ));

        let quote = books["7"].quote(
            "7".to_string(),
            chrono::DateTime::from_timestamp_millis(newer.timestamp).unwrap(),
            None,
        );
        assert!(matches!(
            quote,
            MarketUpdate::Quote { ask: Some(ask), ask_size: Some(size), .. }
                if ask == dec!(0.54) && size == dec!(30)
        ));
    }

    #[test]
    fn malformed_stale_clob_price_change_still_fails_closed() {
        let change = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.53", "side": "SELL",
                "hash": null, "best_bid": "0.52", "best_ask": "0.53"
            }]
        }))
        .expect("syntactically valid malformed price change");
        let mut timestamps =
            std::collections::HashMap::from([("7".to_string(), 1_712_205_600_300)]);

        assert!(market_updates_from_price_change(
            &change,
            &mut seeded_clob_books(),
            &mut timestamps,
        )
        .is_err());
    }

    #[test]
    fn stale_clob_price_change_for_an_unrelated_level_still_fails_closed() {
        let newer = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "price_changes": [{
                "asset_id": "7", "price": "0.52", "size": "8", "side": "BUY",
                "hash": null, "best_bid": "0.52", "best_ask": "0.60"
            }]
        }))
        .expect("newer price change");
        let stale = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.53", "size": "0", "side": "SELL",
                "hash": null, "best_bid": "0.52", "best_ask": "0.54"
            }]
        }))
        .expect("stale unrelated-level price change");
        let mut books = seeded_clob_books();
        let mut timestamps =
            std::collections::HashMap::from([("7".to_string(), 1_712_205_600_100)]);

        market_updates_from_price_change(&newer, &mut books, &mut timestamps)
            .expect("newer price change applies");
        assert!(market_updates_from_price_change(&stale, &mut books, &mut timestamps).is_err());
    }

    #[test]
    fn first_price_change_older_than_a_full_snapshot_is_skipped() {
        let book = serde_json::from_value(json!({
            "asset_id": "7",
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600300",
            "bids": [{"price": "0.52", "size": "7.25"}],
            "asks": [{"price": "0.53", "size": "9.5"}],
            "hash": null
        }))
        .expect("newer full snapshot");
        let stale = serde_json::from_value(json!({
            "market": "0x0000000000000000000000000000000000000000000000000000000000000000",
            "timestamp": "1712205600200",
            "price_changes": [{
                "asset_id": "7", "price": "0.54", "size": "20", "side": "BUY",
                "hash": null, "best_bid": "0.52", "best_ask": "0.53"
            }]
        }))
        .expect("stale price change");
        let mut state = ClobBookState::default();
        market_update_from_clob_book(&book, &mut state).expect("snapshot applies");
        let mut books = std::collections::HashMap::from([("7".to_string(), state)]);
        let mut timestamps = std::collections::HashMap::new();

        assert!(
            market_updates_from_price_change(&stale, &mut books, &mut timestamps)
                .expect("full snapshot proves stale delta is superseded")
                .is_empty()
        );
        assert!(!books["7"].bids.contains_key(&dec!(0.54)));
    }

    #[test]
    fn parses_current_rtds_equity_update_and_snapshot_payloads() {
        let update = parse_equity_price_payload(&json!({
            "topic": "equity_prices",
            "type": "update",
            "timestamp": 1711382400000_i64,
            "payload": {
                "symbol": "aapl",
                "value": 198.45,
                "full_accuracy_value": "198.4523",
                "timestamp": 1711382400000_i64,
                "received_at": 1711382400005_i64
            }
        }))
        .expect("current update envelope");
        assert_eq!(update.len(), 1);
        assert_eq!(update[0].symbol, "aapl");
        assert_eq!(update[0].full_accuracy_value.as_deref(), Some("198.4523"));

        let snapshot = parse_equity_price_payload(&json!({
            "topic": "equity_prices",
            "type": "subscribe",
            "timestamp": 1711382400000_i64,
            "payload": {
                "symbol": "aapl",
                "data": [{
                    "value": 198.30,
                    "full_accuracy_value": "198.3000",
                    "timestamp": 1711382280000_i64,
                    "received_at": 1711382280005_i64,
                    "is_carried_forward": false
                }]
            }
        }))
        .expect("current snapshot envelope");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].symbol, "aapl");
        assert_eq!(snapshot[0].value, dec!(198.30));
    }

    #[test]
    fn equity_subscription_preserves_the_server_string_filter_contract() {
        let serialized = serde_json::to_value(equity_price_subscription("AAPL"))
            .expect("serialize subscription");
        assert_eq!(serialized["topic"], "equity_prices");
        assert_eq!(serialized["type"], "*");
        assert_eq!(serialized["filters"], r#"{"symbol":"AAPL"}"#);
    }

    #[test]
    fn db_l2_feed_builds_depth_variant_from_pair_levels() {
        let ts = Utc::now();
        let updates = l2_updates_from_book(
            "BTCUSDT",
            0.2,
            11,
            dec!(100.0),
            Some(&json!([
                ["100.0", "2.0"],
                ["99.92", "3.5"],
                ["99.6", "9.0"]
            ])),
            Some(&json!([
                ["100.02", "1.5"],
                ["100.08", "4.0"],
                ["100.4", "8.0"]
            ])),
            ts,
        );

        assert!(
            matches!(updates.first(), Some(MarketUpdate::L2 { symbol, .. }) if symbol.as_ref() == "BTCUSDT")
        );
        assert!(matches!(
            updates.get(1),
            Some(MarketUpdate::L2Depth {
                bid_depth_near,
                ask_depth_near,
                spread_bps,
                ..
            }) if (bid_depth_near - 5.5).abs() < 1e-9
                && (ask_depth_near - 5.5).abs() < 1e-9
                && *spread_bps == 11
        ));
    }

    #[test]
    fn rest_book_quote_uses_tradeable_top_of_book_size() {
        let book: RestBook = serde_json::from_value(json!({
            "bids": [
                {"price": "0.01", "size": "999"},
                {"price": "0.47", "size": "12.5"},
                {"price": "0.52", "size": "7.25"}
            ],
            "asks": [
                {"price": "0.99", "size": "999"},
                {"price": "0.54", "size": "20"},
                {"price": "0.53", "size": "9.5"}
            ]
        }))
        .unwrap();

        let quote = book_quote_from_rest(&book);

        assert_eq!(quote.bid, Some(dec!(0.52)));
        assert_eq!(quote.bid_size, Some(dec!(7.25)));
        assert_eq!(quote.ask, Some(dec!(0.53)));
        assert_eq!(quote.ask_size, Some(dec!(9.5)));
    }

    #[test]
    fn rest_book_quote_filters_placeholder_only_books() {
        let book: RestBook = serde_json::from_value(json!({
            "bids": [{"price": "0.01", "size": "999"}],
            "asks": [{"price": "0.99", "size": "999"}]
        }))
        .unwrap();

        let quote = book_quote_from_rest(&book);

        assert_eq!(quote.bid, None);
        assert_eq!(quote.bid_size, None);
        assert_eq!(quote.ask, None);
        assert_eq!(quote.ask_size, None);
    }

    #[test]
    fn db_polymarket_expiry_waits_for_official_settlement_before_marking_done() {
        let mut expired_events = HashSet::new();

        assert!(!mark_db_event_expired_if_resolved(
            &mut expired_events,
            "event-1",
            None
        ));
        assert!(
            !expired_events.contains("event-1"),
            "missing settlement must stay retryable"
        );

        assert!(mark_db_event_expired_if_resolved(
            &mut expired_events,
            "event-1",
            Some(true)
        ));
        assert!(expired_events.contains("event-1"));
        assert!(!mark_db_event_expired_if_resolved(
            &mut expired_events,
            "event-1",
            Some(true)
        ));
    }

    #[test]
    fn parse_agg_trade_message_extracts_fields() {
        let msg = serde_json::json!({
            "e": "aggTrade",
            "s": "BTCUSDT",
            "a": 12345_i64,
            "p": "50000.00",
            "q": "0.01",
            "f": 100_i64,
            "l": 105_i64,
            "T": 1672515782136_i64,
            "m": true
        });
        let parsed = parse_agg_trade_msg(&msg).unwrap();
        assert_eq!(parsed.symbol, "BTCUSDT");
        assert_eq!(parsed.agg_trade_id, 12345);
        assert!((parsed.price.to_f64().unwrap() - 50000.0).abs() < 0.01);
        assert!(parsed.is_buyer_maker);
    }
}
