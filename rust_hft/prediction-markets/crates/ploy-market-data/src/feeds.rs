//! Live market data feed producers.
//!
//! Async tasks that bridge venue WebSocket/REST streams into the unified
//! `MarketUpdate` broadcast channel consumed by `LiveFeed`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use adapter_polymarket_data::{
    MarketEvent, MarketSnapshot, MarketStream, PolymarketBook, PolymarketMarketStream, Symbol,
};
use chrono::{DateTime, Duration, Timelike, Utc};
use futures::StreamExt;
use ploy_market_contracts::{
    l2_updates_from_depth_totals, normalize_token_id, BookLevel, MarketUpdate,
};
use polymarket_client_sdk::rtds::{Client as RtdsClient, Subscription};
use polymarket_client_sdk::ws::config::{Config as PolymarketWsConfig, ReconnectConfig};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, error, info, warn};

use crate::reference_prices::{
    infer_pyth_asset_class, market_symbol_to_binance_symbol, normalize_reference_symbol,
    parse_chainlink_twap_price, pyth_symbol, upsert_reference_price, ReferenceAssetClass,
    ReferencePriceKey, ReferencePriceRegistry, ReferencePriceSnapshot, ReferencePriceSource,
};

const POLYMARKET_RTDS_WS_ENDPOINT: &str = "wss://ws-live-data.polymarket.com";
#[cfg(test)]
const NEAR_DEPTH_PCT_RANGE: f64 = 0.001;
const DB_POLYMARKET_SETTLEMENT_RETRY_LOOKBACK_SECS: i64 = 30 * 60;

fn binance_identity_for_market_type(market_type: &str) -> Option<(&'static str, &'static str)> {
    match market_type.trim().to_ascii_lowercase().as_str() {
        "spot" => Some(("spot", "binance")),
        "usd_m" => Some(("usd_m", "binance_futures")),
        _ => None,
    }
}

fn rtds_market_data_ws_config() -> PolymarketWsConfig {
    let mut config = PolymarketWsConfig::default();
    // These feeds only need resilient market-data delivery. A wider heartbeat
    // window avoids unnecessary reconnect churn on transient stalls.
    config.heartbeat_interval = StdDuration::from_secs(15);
    config.heartbeat_timeout = StdDuration::from_secs(45);
    config.reconnect = ReconnectConfig::default();
    config
}

fn pm_tradeable_price(price: Decimal) -> bool {
    price > rust_decimal_macros::dec!(0.02) && price < rust_decimal_macros::dec!(0.98)
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
                    let Some(ts) = DateTime::from_timestamp_millis(crypto_price.timestamp) else {
                        warn!(
                            symbol = %crypto_price.symbol,
                            timestamp = crypto_price.timestamp,
                            "Skipping Binance RTDS tick with an invalid source timestamp"
                        );
                        continue;
                    };
                    let received_at = Utc::now();

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
                            received_at,
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
                                    persist_spot_price(
                                        db,
                                        &symbol_upper,
                                        crypto_price.value,
                                        ts,
                                        received_at,
                                    )
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
    spawn_db_spot_feed_for_market(tx, symbols, pool, "spot")
}

/// DB spot-price feed for an explicit Binance market family. Legacy rows with
/// unknown identity remain audit-only and are never admitted to this feed.
pub fn spawn_db_spot_feed_for_market(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
    market_type: impl Into<String>,
) -> JoinHandle<()> {
    let market_type = market_type.into();
    let Some((canonical_market_type, venue)) = binance_identity_for_market_type(&market_type)
    else {
        warn!(
            market_type,
            "Unsupported Binance market type for DB spot feed"
        );
        return tokio::spawn(async {});
    };
    let market_type = canonical_market_type.to_string();
    let venue = venue.to_string();
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
                      AND trade_id IS NOT NULL
                      AND event_time IS NOT NULL
                      AND market_type = $2
                      AND venue = $3
                    ORDER BY symbol, trade_time DESC
                    "#,
                )
                .bind(&symbols_upper)
                .bind(&market_type)
                .bind(&venue)
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
    spawn_db_aggtrade_feed_for_market(tx, symbols, pool, "spot")
}

pub fn spawn_db_aggtrade_feed_for_market(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
    market_type: impl Into<String>,
) -> JoinHandle<()> {
    let market_type = market_type.into();
    let Some((canonical_market_type, venue)) = binance_identity_for_market_type(&market_type)
    else {
        warn!(
            market_type,
            "Unsupported Binance market type for DB aggTrade feed"
        );
        return tokio::spawn(async {});
    };
    let market_type = canonical_market_type.to_string();
    let venue = venue.to_string();
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
                  AND event_time IS NOT NULL
                  AND first_trade_id IS NOT NULL
                  AND last_trade_id IS NOT NULL
                  AND market_type = $2
                  AND venue = $3
                ORDER BY trade_time ASC, agg_trade_id ASC
                "#,
            )
            .bind(&symbols_upper)
            .bind(&market_type)
            .bind(&venue)
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
    spawn_db_l2_feed_for_market(tx, symbols, pool, "spot")
}

pub fn spawn_db_l2_feed_for_market(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    symbols: Vec<String>,
    pool: PgPool,
    market_type: impl Into<String>,
) -> JoinHandle<()> {
    let market_type = market_type.into();
    let Some((canonical_market_type, venue)) = binance_identity_for_market_type(&market_type)
    else {
        warn!(
            market_type,
            "Unsupported Binance market type for DB L2 feed"
        );
        return tokio::spawn(async {});
    };
    let market_type = canonical_market_type.to_string();
    let venue = venue.to_string();
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
                      AND event_time IS NOT NULL
                      AND depth_mode IS NOT NULL
                      AND event_time > NOW() - INTERVAL '30 seconds'
                      AND market_type = $2
                      AND venue = $3
                    ORDER BY event_time ASC, update_id ASC
                    "#,
            )
            .bind(&symbols_upper)
            .bind(&market_type)
            .bind(&venue)
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

/// Convert one complete canonical post-event projection into the legacy prediction-market quote
/// contract. The broader discovery/reference/sports domains continue to use MarketUpdate directly;
/// only the Polymarket CLOB book path crosses this adapter seam.
fn market_update_from_canonical_snapshot(
    snapshot: &MarketSnapshot,
) -> Result<MarketUpdate, String> {
    let bid_levels = snapshot
        .bids
        .iter()
        .map(|level| BookLevel {
            price: level.price.0,
            size: level.quantity.0,
        })
        .collect::<Vec<_>>();
    let ask_levels = snapshot
        .asks
        .iter()
        .map(|level| BookLevel {
            price: level.price.0,
            size: level.quantity.0,
        })
        .collect::<Vec<_>>();
    let bid = bid_levels
        .iter()
        .find(|level| pm_tradeable_price(level.price))
        .map(|level| level.price);
    let ask = ask_levels
        .iter()
        .find(|level| pm_tradeable_price(level.price))
        .map(|level| level.price);
    let bid_size = bid_levels
        .iter()
        .find(|level| Some(level.price) == bid)
        .map(|level| level.size);
    let ask_size = ask_levels
        .iter()
        .find(|level| Some(level.price) == ask)
        .map(|level| level.size);
    let timestamp = i64::try_from(snapshot.timestamp)
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| "Polymarket canonical timestamp is out of range".to_string())?;

    Ok(MarketUpdate::Quote {
        token_id: Arc::from(snapshot.symbol.as_str()),
        bid,
        ask,
        bid_size,
        ask_size,
        bid_levels,
        ask_levels,
        ts: timestamp,
    })
}

fn canonical_quote_event(
    event: &MarketEvent,
    books_by_token: &mut HashMap<String, PolymarketBook>,
) -> Result<Option<MarketUpdate>, String> {
    let token_id = match event {
        MarketEvent::Snapshot(snapshot) => snapshot.symbol.as_str().to_string(),
        MarketEvent::Update(update) => update.symbol.as_str().to_string(),
        MarketEvent::Disconnect { symbol, .. } => {
            if let Some(symbol) = symbol {
                books_by_token.remove(symbol.as_str());
            } else {
                books_by_token.clear();
            }
            return Ok(None);
        }
        _ => return Ok(None),
    };
    let book = books_by_token.entry(token_id).or_default();
    let snapshot = book.apply(event).map_err(|error| error.to_string())?;
    snapshot
        .as_ref()
        .map(market_update_from_canonical_snapshot)
        .transpose()
}

fn send_quote_collection_failure_and_empty(
    tx: &broadcast::Sender<MarketUpdate>,
    token_ids: &[String],
    request_started_at: DateTime<Utc>,
    error_kind: &str,
) -> bool {
    for token in token_ids {
        let token_id: Arc<str> = Arc::from(token.as_str());
        let now = Utc::now();
        if tx
            .send(MarketUpdate::QuoteCollectionFailure {
                token_id: Arc::clone(&token_id),
                request_started_at,
                http_status: None,
                error_kind: Arc::from(error_kind),
                ts: now,
            })
            .is_err()
            || tx
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
                .is_err()
        {
            return false;
        }
    }
    true
}

fn publish_failure_once(
    tx: &broadcast::Sender<MarketUpdate>,
    token_ids: &[String],
    request_started_at: DateTime<Utc>,
    error_kind: &str,
    failed_closed: &mut bool,
) -> bool {
    if *failed_closed {
        return true;
    }
    if !send_quote_collection_failure_and_empty(tx, token_ids, request_started_at, error_kind) {
        return false;
    }
    *failed_closed = true;
    true
}

fn connection_started_at(event: &MarketEvent) -> Option<DateTime<Utc>> {
    let MarketEvent::Disconnect {
        connection_started_at: Some(timestamp),
        ..
    } = event
    else {
        return None;
    };
    i64::try_from(*timestamp)
        .ok()
        .and_then(DateTime::from_timestamp_micros)
}

/// Publish canonical Polymarket CLOB snapshots and post-snapshot deltas into the
/// legacy MarketUpdate::Quote broadcast contract.
///
/// The canonical adapter owns transport parsing and per-token synchronization. This
/// bridge only projects its complete post-event book into the existing strategy,
/// research, and database-facing quote shape. A venue disconnect clears every
/// subscribed token; a token-scoped invalidation clears only that token. No quote
/// is published until a fresh snapshot initializes the affected token.
pub fn spawn_polymarket_market_stream_until(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    token_ids: Vec<String>,
    stop_at: Option<DateTime<Utc>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols = token_ids.iter().map(Symbol::new).collect::<Vec<_>>();

        loop {
            if stop_at.is_some_and(|deadline| Utc::now() >= deadline) {
                return;
            }
            let subscription_started_at = Utc::now();
            let adapter = PolymarketMarketStream::new();
            let mut stream = match adapter.subscribe(symbols.clone()).await {
                Ok(stream) => stream,
                Err(error) => {
                    warn!(error = %error, "Canonical Polymarket market stream subscribe failed");
                    if !send_quote_collection_failure_and_empty(
                        &tx,
                        &token_ids,
                        subscription_started_at,
                        "canonical_subscribe",
                    ) {
                        return;
                    }
                    tokio::time::sleep(StdDuration::from_millis(250)).await;
                    continue;
                }
            };
            let mut books_by_token: HashMap<String, PolymarketBook> = HashMap::new();
            let mut failed_closed = false;
            let mut active_connection_started_at = subscription_started_at;
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

            loop {
                tokio::select! {
                    _ = &mut stop => return,
                    event = stream.next() => match event {
                        Some(Ok(event)) => {
                            if let Some(started_at) = connection_started_at(&event) {
                                active_connection_started_at = started_at;
                            }
                            match event {
                            MarketEvent::Snapshot(_) | MarketEvent::Update(_) => {
                                match canonical_quote_event(&event, &mut books_by_token) {
                                    Ok(Some(update)) => {
                                        if tx.send(update).is_err() {
                                            return;
                                        }
                                        if matches!(&event, MarketEvent::Snapshot(_)) {
                                            failed_closed = false;
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(error) => {
                                        books_by_token.clear();
                                        warn!(error = %error, "Canonical Polymarket book projection failed");
                                        if !publish_failure_once(
                                            &tx,
                                            &token_ids,
                                            active_connection_started_at,
                                            "canonical_book",
                                            &mut failed_closed,
                                        ) {
                                            return;
                                        }
                                    }
                                }
                            }
                            MarketEvent::Disconnect { symbol, .. } => match symbol {
                                Some(symbol) => {
                                    let token_ids = vec![symbol.as_str().to_owned()];
                                    books_by_token.remove(symbol.as_str());
                                    if !send_quote_collection_failure_and_empty(
                                        &tx,
                                        &token_ids,
                                        active_connection_started_at,
                                        "canonical_book",
                                    ) {
                                        return;
                                    }
                                }
                                None => {
                                    books_by_token.clear();
                                    if !publish_failure_once(
                                        &tx,
                                        &token_ids,
                                        active_connection_started_at,
                                        "canonical_disconnect",
                                        &mut failed_closed,
                                    ) {
                                        return;
                                    }
                                }
                            },
                            MarketEvent::Trade(_) => {}
                            MarketEvent::Quote(_)
                            | MarketEvent::Bar(_)
                            | MarketEvent::Arbitrage(_) => {}
                        }
                    },
                        Some(Err(error)) => {
                            books_by_token.clear();
                            warn!(error = %error, "Canonical Polymarket market stream error");
                            if !publish_failure_once(
                                &tx,
                                &token_ids,
                                active_connection_started_at,
                                "canonical_stream_error",
                                &mut failed_closed,
                            ) {
                                return;
                            }
                        }
                        None => {
                            books_by_token.clear();
                            if !publish_failure_once(
                                &tx,
                                &token_ids,
                                active_connection_started_at,
                                "canonical_stream_ended",
                                &mut failed_closed,
                            ) {
                                return;
                            }
                            break;
                        }
                    },
                }
            }
            tokio::time::sleep(StdDuration::from_millis(250)).await;
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
    received_at: DateTime<Utc>,
) {
    let result = sqlx::query(
        r#"
        INSERT INTO binance_price_ticks
            (symbol, price, trade_time, received_at)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(symbol)
    .bind(price)
    .bind(trade_time)
    .bind(received_at)
    .execute(pool)
    .await;

    if let Err(e) = result {
        debug!(symbol, error = %e, "Failed to persist spot price tick");
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

#[cfg(test)]
#[derive(Debug)]
#[allow(dead_code)]
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

#[cfg(test)]
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
        canonical_quote_event, connection_started_at, db_polymarket_poll_intervals,
        equity_price_subscription, l2_updates_from_book, mark_db_event_expired_if_resolved,
        parse_agg_trade_msg, parse_equity_price_payload, rtds_market_data_ws_config,
        send_quote_collection_failure_and_empty,
    };
    use adapter_polymarket_data::{
        BookLevel as CanonicalBookLevel, BookUpdate as CanonicalBookUpdate,
        MarketEvent as CanonicalMarketEvent, MarketSnapshot as CanonicalMarketSnapshot, Price,
        Quantity, Symbol, VenueId,
    };
    use chrono::Utc;
    use ploy_market_contracts::MarketUpdate;
    use rust_decimal::prelude::ToPrimitive;
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;

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
    fn canonical_quote_projection_keeps_long_token_depth_through_delete() {
        let token =
            "106585164761922456203746651621390029417453862034640469075081961934906147433548";
        let snapshot = CanonicalMarketEvent::Snapshot(CanonicalMarketSnapshot {
            symbol: Symbol::new(token),
            timestamp: 1_712_205_600_123_000,
            bids: vec![CanonicalBookLevel {
                price: Price(dec!(0.52)),
                quantity: Quantity(dec!(7.25)),
            }],
            asks: vec![CanonicalBookLevel {
                price: Price(dec!(0.53)),
                quantity: Quantity(dec!(9.5)),
            }],
            sequence: 1,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),

            provider_identity: None,
        });
        let mut books = HashMap::new();

        let quote = canonical_quote_event(&snapshot, &mut books)
            .unwrap()
            .expect("snapshot quote");
        let MarketUpdate::Quote {
            token_id,
            bid_levels,
            ask_levels,
            bid,
            ask,
            ts,
            ..
        } = quote
        else {
            panic!("expected quote");
        };
        assert_eq!(token_id.as_ref(), token);
        assert_eq!(bid, Some(dec!(0.52)));
        assert_eq!(ask, Some(dec!(0.53)));
        assert_eq!(bid_levels.len(), 1);
        assert_eq!(ask_levels.len(), 1);
        assert_eq!(ts.timestamp_micros(), 1_712_205_600_123_000);

        let deleted = CanonicalMarketEvent::Update(CanonicalBookUpdate {
            symbol: Symbol::new(token),
            timestamp: 1_712_205_600_124_000,
            bids: vec![CanonicalBookLevel {
                price: Price(dec!(0.52)),
                quantity: Quantity(Decimal::ZERO),
            }],
            asks: Vec::new(),
            first_sequence: None,
            sequence: 2,
            is_snapshot: false,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        });
        let quote = canonical_quote_event(&deleted, &mut books)
            .unwrap()
            .expect("delete quote");
        let MarketUpdate::Quote {
            bid, bid_levels, ..
        } = quote
        else {
            panic!("expected quote");
        };
        assert_eq!(bid, None);
        assert!(bid_levels.is_empty());

        canonical_quote_event(
            &CanonicalMarketEvent::Disconnect {
                reason: "test".to_string(),
                source_venue: Some(VenueId::POLYMARKET),
                symbol: None,
                connection_started_at: None,
            },
            &mut books,
        )
        .unwrap();
        assert!(books.is_empty());
    }

    #[test]
    fn canonical_quote_projection_rejects_delta_before_snapshot() {
        let token = Symbol::new("123");
        let delta = CanonicalMarketEvent::Update(CanonicalBookUpdate {
            symbol: token,
            timestamp: 1_000,
            bids: vec![CanonicalBookLevel::new(0.4, 1.0).unwrap()],
            asks: Vec::new(),
            first_sequence: None,
            sequence: 1,
            is_snapshot: false,
            source_venue: Some(VenueId::POLYMARKET),
            timestamps: Default::default(),
        });
        assert!(canonical_quote_event(&delta, &mut HashMap::new()).is_err());
    }

    #[test]
    fn canonical_token_disconnect_does_not_clear_a_healthy_token_projection() {
        let mut books = HashMap::new();
        for token in ["123", "456"] {
            let event = CanonicalMarketEvent::Snapshot(CanonicalMarketSnapshot {
                symbol: Symbol::new(token),
                timestamp: 1_000,
                bids: vec![CanonicalBookLevel {
                    price: Price(dec!(0.4)),
                    quantity: Quantity(dec!(2)),
                }],
                asks: vec![CanonicalBookLevel {
                    price: Price(dec!(0.6)),
                    quantity: Quantity(dec!(3)),
                }],
                sequence: 1,
                source_venue: Some(VenueId::POLYMARKET),
                timestamps: Default::default(),

                provider_identity: None,
            });
            canonical_quote_event(&event, &mut books)
                .unwrap()
                .expect("snapshot quote");
        }

        canonical_quote_event(
            &CanonicalMarketEvent::Disconnect {
                reason: "token BBA mismatch".to_string(),
                source_venue: Some(VenueId::POLYMARKET),
                symbol: Some(Symbol::new("123")),
                connection_started_at: None,
            },
            &mut books,
        )
        .unwrap();
        assert!(!books.contains_key("123"));
        assert!(books.contains_key("456"));
    }

    #[test]
    fn canonical_stream_failure_publishes_failure_before_empty_quote() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4);
        assert!(send_quote_collection_failure_and_empty(
            &tx,
            &["7".to_string()],
            Utc::now(),
            "canonical_disconnect",
        ));

        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::QuoteCollectionFailure { error_kind, .. }
                if error_kind.as_ref() == "canonical_disconnect"
        ));
        assert!(matches!(
            rx.try_recv().unwrap(),
            MarketUpdate::Quote {
                bid: None,
                ask: None,
                bid_levels,
                ask_levels,
                ..
            } if bid_levels.is_empty() && ask_levels.is_empty()
        ));
    }

    #[test]
    fn canonical_disconnect_keeps_the_adapter_connection_start_for_failure_evidence() {
        let started_at = 1_712_205_600_123_000;
        let event = CanonicalMarketEvent::Disconnect {
            reason: "reconnect".to_string(),
            source_venue: Some(VenueId::POLYMARKET),
            symbol: None,
            connection_started_at: Some(started_at),
        };
        assert_eq!(
            connection_started_at(&event),
            chrono::DateTime::from_timestamp_micros(started_at as i64)
        );
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
