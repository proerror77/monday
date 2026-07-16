//! Binance WebSocket data collectors — price ticks, aggregated trades, and L2 orderbook.
//!
//! Each collector is a long-running `async fn` that connects to Binance's combined
//! WebSocket stream, subscribes to the relevant `@trade` / `@aggTrade` / `@depth<N>@100ms`
//! channels, parses the JSON payloads, and persists normalized rows to PostgreSQL.
//!
//! All three share the same reconnection and batching strategy.
//!
//! Run via `ploy-runner`:
//!   ploy-runner collect-binance-lob --symbols BTCUSDT,ETHUSDT,SOLUSDT
//!   ploy-runner collect-binance-price --symbols BTCUSDT,ETHUSDT,SOLUSDT
//!   ploy-runner collect-binance-aggtrade --symbols BTCUSDT,ETHUSDT,SOLUSDT

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use futures::{SinkExt, StreamExt};
use ploy_market_contracts::{l2_updates_from_depth_totals, MarketUpdate};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

use crate::reference_prices::{
    market_symbol_to_binance_symbol, upsert_reference_price, ReferenceAssetClass,
    ReferencePriceKey, ReferencePriceRegistry, ReferencePriceSnapshot, ReferencePriceSource,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/stream";

fn parse_symbols(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a millisecond epoch from a Binance JSON field into a `DateTime<Utc>`.
fn ms_to_utc(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ms / 1000, ((ms % 1000) * 1_000_000) as u32)
        .unwrap()
}

fn market_updates_from_combined_stream(
    payload: &Value,
    received_at: DateTime<Utc>,
) -> Vec<MarketUpdate> {
    let Some(data) = payload.get("data") else {
        return Vec::new();
    };
    let event = data
        .get("e")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("stream")
                .and_then(Value::as_str)
                .and_then(|stream| stream.split('@').nth(1))
        })
        .unwrap_or_default();

    if event.eq_ignore_ascii_case("aggTrade") {
        let Some(symbol) = data.get("s").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(agg_trade_id) = data.get("a").and_then(Value::as_u64) else {
            return Vec::new();
        };
        let Some(price) = data
            .get("p")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<Decimal>().ok())
            .filter(|price| *price > Decimal::ZERO)
        else {
            return Vec::new();
        };
        let Some(quantity) = data
            .get("q")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<Decimal>().ok())
            .filter(|quantity| *quantity > Decimal::ZERO)
        else {
            return Vec::new();
        };
        let Some(ts) = data
            .get("T")
            .and_then(Value::as_i64)
            .and_then(|millis| Utc.timestamp_millis_opt(millis).single())
        else {
            return Vec::new();
        };
        return vec![MarketUpdate::AggTrade {
            symbol: Arc::from(symbol.to_uppercase()),
            agg_trade_id,
            price,
            quantity,
            is_buyer_maker: data.get("m").and_then(Value::as_bool).unwrap_or(false),
            ts,
        }];
    }

    if event.eq_ignore_ascii_case("trade") {
        let Some(symbol) = data.get("s").and_then(Value::as_str) else {
            return Vec::new();
        };
        let Some(price) = data
            .get("p")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<Decimal>().ok())
            .filter(|price| *price > Decimal::ZERO)
        else {
            return Vec::new();
        };
        let Some(ts) = data
            .get("T")
            .and_then(Value::as_i64)
            .and_then(|millis| Utc.timestamp_millis_opt(millis).single())
        else {
            return Vec::new();
        };
        return vec![MarketUpdate::SpotPrice {
            symbol: Arc::from(symbol.to_uppercase()),
            price,
            ts,
        }];
    }

    if event.to_ascii_lowercase().starts_with("depth") {
        let Some(symbol) = data.get("s").and_then(Value::as_str).or_else(|| {
            payload
                .get("stream")
                .and_then(Value::as_str)
                .and_then(|stream| stream.split('@').next())
        }) else {
            return Vec::new();
        };
        let bids = data
            .get("b")
            .or_else(|| data.get("bids"))
            .and_then(Value::as_array)
            .map(|levels| parse_level(levels))
            .unwrap_or_default();
        let asks = data
            .get("a")
            .or_else(|| data.get("asks"))
            .and_then(Value::as_array)
            .map(|levels| parse_level(levels))
            .unwrap_or_default();
        let (Some((best_bid, _)), Some((best_ask, _))) = (bids.first(), asks.first()) else {
            return Vec::new();
        };
        if best_ask < best_bid {
            return Vec::new();
        }
        let mid_price = (*best_bid + *best_ask) / Decimal::from(2);
        if mid_price <= Decimal::ZERO {
            return Vec::new();
        }
        let spread_bps = ((*best_ask - *best_bid) / mid_price * Decimal::from(10_000))
            .round_dp(0)
            .to_u32()
            .unwrap_or(u32::MAX);
        let bid_depth = sum_volume(&bids, 5);
        let ask_depth = sum_volume(&asks, 5);
        let Some(obi) = obi(bid_depth, ask_depth).to_f64() else {
            return Vec::new();
        };
        let ts = data
            .get("E")
            .and_then(Value::as_i64)
            .and_then(|millis| Utc.timestamp_millis_opt(millis).single())
            .unwrap_or(received_at);
        return l2_updates_from_depth_totals(
            &symbol.to_uppercase(),
            obi,
            spread_bps,
            bid_depth,
            ask_depth,
            ts,
        );
    }

    Vec::new()
}

fn send_unavailable_spot_ticks(tx: &broadcast::Sender<MarketUpdate>, symbols: &[String]) -> bool {
    let ts = Utc::now();
    symbols.iter().all(|symbol| {
        tx.send(MarketUpdate::SpotPrice {
            symbol: Arc::from(symbol.as_str()),
            price: Decimal::ZERO,
            ts,
        })
        .is_ok()
    })
}

/// Stream Binance spot, aggTrade, and partial-depth ticks directly into a
/// strategy runtime without putting PostgreSQL on the quote-to-decision path.
///
/// The standalone collectors remain responsible for durable tick persistence.
pub fn spawn_binance_tick_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: ReferencePriceRegistry,
    symbols: Vec<String>,
    depth_levels: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols = symbols
            .into_iter()
            .map(|symbol| symbol.trim().to_uppercase())
            .filter(|symbol| !symbol.is_empty())
            .collect::<Vec<_>>();
        let depth_levels = match depth_levels {
            5 | 10 | 20 => depth_levels,
            _ => 20,
        };
        let streams = symbols
            .iter()
            .flat_map(|symbol| {
                let symbol = symbol.to_lowercase();
                [
                    format!("{symbol}@trade"),
                    format!("{symbol}@aggTrade"),
                    format!("{symbol}@depth{depth_levels}@100ms"),
                ]
            })
            .collect::<Vec<_>>();
        if streams.is_empty() {
            warn!("Direct Binance tick feed has no configured symbols");
            return;
        }

        let mut reconnect_delay = Duration::from_millis(250);
        loop {
            let (mut write, mut read) = match binance_connect(&streams).await {
                Ok(connection) => connection,
                Err(error) => {
                    warn!(%error, ?reconnect_delay, "Direct Binance tick feed connect failed");
                    if !send_unavailable_spot_ticks(&tx, &symbols) {
                        return;
                    }
                    tokio::time::sleep(reconnect_delay).await;
                    reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(5));
                    continue;
                }
            };
            reconnect_delay = Duration::from_millis(250);
            info!(symbols = ?symbols, depth_levels, "Direct Binance tick feed connected");

            while let Some(message) = read.next().await {
                match message {
                    Ok(Message::Text(text)) => {
                        let payload: Value = match serde_json::from_str(&text) {
                            Ok(payload) => payload,
                            Err(error) => {
                                warn!(%error, "Direct Binance tick payload was invalid JSON");
                                continue;
                            }
                        };
                        let received_at = Utc::now();
                        for update in market_updates_from_combined_stream(&payload, received_at) {
                            let reference_snapshot = match &update {
                                MarketUpdate::SpotPrice { symbol, price, ts } => {
                                    Some(ReferencePriceSnapshot {
                                        key: ReferencePriceKey {
                                            source: ReferencePriceSource::Binance,
                                            symbol: market_symbol_to_binance_symbol(symbol),
                                        },
                                        asset_class: ReferenceAssetClass::Crypto,
                                        value: *price,
                                        full_accuracy_value: None,
                                        source_timestamp: *ts,
                                        received_at: Utc::now(),
                                        is_carried_forward: false,
                                    })
                                }
                                _ => None,
                            };
                            if tx.send(update).is_err() {
                                return;
                            }
                            if let Some(snapshot) = reference_snapshot {
                                upsert_reference_price(&reference_prices, snapshot).await;
                            }
                        }
                    }
                    Ok(Message::Ping(payload)) => {
                        if let Err(error) = write.send(Message::Pong(payload)).await {
                            warn!(%error, "Direct Binance tick feed pong failed");
                            break;
                        }
                    }
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Close(frame)) => {
                        warn!(?frame, "Direct Binance tick feed closed");
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(%error, "Direct Binance tick feed receive failed");
                        break;
                    }
                }
            }

            if !send_unavailable_spot_ticks(&tx, &symbols) {
                return;
            }
            tokio::time::sleep(reconnect_delay).await;
            reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(5));
        }
    })
}

/// Shared signal — set to false on shutdown request.
type SharedRunning = Arc<AtomicBool>;
type CollectorResult<T> = Result<T, String>;

fn running_flag() -> SharedRunning {
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Shutdown signal received, stopping collector...");
        f.store(false, Ordering::SeqCst);
    });
    flag
}

/// Connect to Binance combined stream, subscribe, return the writer + reader.
async fn binance_connect(
    streams: &[String],
) -> Result<
    (
        futures::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        futures::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ),
    String,
> {
    let (ws, _) = connect_async(BINANCE_WS_URL)
        .await
        .map_err(|error| error.to_string())?;
    let (mut write, read) = ws.split();

    let subscribe = serde_json::json!({
        "method": "SUBSCRIBE",
        "params": streams,
        "id": 1,
    });
    write
        .send(Message::Text(subscribe.to_string().into()))
        .await
        .map_err(|error| error.to_string())?;

    Ok((write, read))
}

// ---------------------------------------------------------------------------
// Price collector (binance_price_ticks)
// ---------------------------------------------------------------------------

/// Collect spot trade prices from Binance `@trade` streams.
///
/// Schema: `binance_price_ticks (symbol, price, quantity, trade_time, received_at)`
pub async fn collect_binance_price(pool: PgPool, symbols_raw: &str, batch_size: usize) {
    let symbols = parse_symbols(symbols_raw);
    let running = running_flag();
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@trade", s.to_lowercase()))
        .collect();

    info!(
        "[binance-price] Starting collector for symbols: {:?}",
        symbols
    );

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match run_price_ws(&pool, &symbols, &streams, batch_size, &running).await {
            Ok(()) => {
                info!("[binance-price] Collector finished (shutdown)");
                break;
            }
            Err(e) => {
                error!("[binance-price] Connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    info!("[binance-price] Collector stopped");
}

async fn run_price_ws(
    pool: &PgPool,
    _symbols: &[String],
    streams: &[String],
    batch_size: usize,
    running: &SharedRunning,
) -> CollectorResult<()> {
    let (_write, mut read) = binance_connect(streams).await?;
    info!(
        "[binance-price] WebSocket connected, subscribed to {} streams",
        streams.len()
    );

    let mut pending = 0u32;
    let mut inserted: u64 = 0;
    let mut last_report = Instant::now();
    let mut batch_timer = Instant::now();

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    None => {
                        warn!("[binance-price] WebSocket stream ended");
                        break;
                    }
                    Some(Err(e)) => {
                        warn!("[binance-price] WebSocket error: {e}");
                        break;
                    }
                    Some(Ok(Message::Text(text))) => {
                        let val: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        // Skip subscription confirmation
                        if val.get("result").is_some() { continue; }
                        let data = match val.get("data") {
                            Some(d) => d,
                            None => continue,
                        };

                        let symbol = data["s"].as_str().unwrap_or("").to_string();
                        let price_str = data["p"].as_str().unwrap_or("0");
                        let qty_str = data["q"].as_str().unwrap_or("0");
                        let trade_time_ms = data["T"].as_i64().unwrap_or(0);

                        let price: Decimal = price_str.parse().unwrap_or(Decimal::ZERO);
                        let qty: Decimal = qty_str.parse().unwrap_or(Decimal::ZERO);
                        let trade_time = ms_to_utc(trade_time_ms);

                        sqlx::query(
                            "INSERT INTO binance_price_ticks (symbol, price, quantity, trade_time) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING"
                        )
                        .bind(&symbol)
                        .bind(price)
                        .bind(qty)
                        .bind(trade_time)
                        .execute(pool)
                        .await
                        .map_err(|error| error.to_string())?;

                        pending += 1;
                        inserted += 1;

                        if pending >= batch_size as u32 || batch_timer.elapsed() >= Duration::from_secs(1) {
                            pending = 0;
                            batch_timer = Instant::now();
                        }

                        if last_report.elapsed() >= Duration::from_secs(60) {
                            info!("[binance-price] Inserted {} ticks in last minute", inserted);
                            inserted = 0;
                            last_report = Instant::now();
                        }
                    }
                    Some(Ok(Message::Ping(_))) => continue,
                    Some(Ok(Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => {
                        warn!("[binance-price] WebSocket closed by server");
                        break;
                    }
                    _ => continue,
                }
            }
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AggTrade collector (binance_agg_trade_ticks)
// ---------------------------------------------------------------------------

/// Collect aggregated trade data from Binance `@aggTrade` streams.
///
/// Schema: `binance_agg_trade_ticks (symbol, agg_trade_id, first_trade_id, last_trade_id,
///          price, quantity, trade_time, event_time, is_buyer_maker, received_at)`
pub async fn collect_binance_aggtrade(pool: PgPool, symbols_raw: &str, batch_size: usize) {
    let symbols = parse_symbols(symbols_raw);
    let running = running_flag();
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@aggTrade", s.to_lowercase()))
        .collect();

    info!(
        "[binance-aggtrade] Starting collector for symbols: {:?}",
        symbols
    );

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match run_aggtrade_ws(&pool, &symbols, &streams, batch_size, &running).await {
            Ok(()) => {
                info!("[binance-aggtrade] Collector finished (shutdown)");
                break;
            }
            Err(e) => {
                error!("[binance-aggtrade] Connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    info!("[binance-aggtrade] Collector stopped");
}

async fn run_aggtrade_ws(
    pool: &PgPool,
    _symbols: &[String],
    streams: &[String],
    batch_size: usize,
    running: &SharedRunning,
) -> CollectorResult<()> {
    let (_write, mut read) = binance_connect(streams).await?;
    info!(
        "[binance-aggtrade] WebSocket connected, subscribed to {} streams",
        streams.len()
    );

    let mut pending = 0u32;
    let mut inserted: u64 = 0;
    let mut duplicates: u64 = 0;
    let mut last_report = Instant::now();
    let mut batch_timer = Instant::now();

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    None => { warn!("[binance-aggtrade] WebSocket stream ended"); break; }
                    Some(Err(e)) => { warn!("[binance-aggtrade] WebSocket error: {e}"); break; }
                    Some(Ok(Message::Text(text))) => {
                        let val: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        if val.get("result").is_some() { continue; }
                        let data = match val.get("data") { Some(d) => d, None => continue, };

                        let symbol = data["s"].as_str().unwrap_or("").to_string();
                        let agg_trade_id = data["a"].as_i64().unwrap_or(0);
                        let first_trade_id = data["f"].as_i64().unwrap_or(0);
                        let last_trade_id = data["l"].as_i64().unwrap_or(0);
                        let price: Decimal = data["p"].as_str().unwrap_or("0").parse().unwrap_or(Decimal::ZERO);
                        let qty: Decimal = data["q"].as_str().unwrap_or("0").parse().unwrap_or(Decimal::ZERO);
                        let trade_time = ms_to_utc(data["T"].as_i64().unwrap_or(0));
                        let event_time = ms_to_utc(data["E"].as_i64().unwrap_or(0));
                        let is_buyer_maker = data["m"].as_bool().unwrap_or(false);

                        let result = sqlx::query(
                            "INSERT INTO binance_agg_trade_ticks \
                             (symbol, agg_trade_id, first_trade_id, last_trade_id, price, quantity, trade_time, event_time, is_buyer_maker) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING"
                        )
                        .bind(&symbol)
                        .bind(agg_trade_id)
                        .bind(first_trade_id)
                        .bind(last_trade_id)
                        .bind(price)
                        .bind(qty)
                        .bind(trade_time)
                        .bind(event_time)
                        .bind(is_buyer_maker)
                        .execute(pool)
                        .await
                        .map_err(|error| error.to_string())?;

                        if result.rows_affected() > 0 {
                            inserted += 1;
                        } else {
                            duplicates += 1;
                        }
                        pending += 1;

                        if pending >= batch_size as u32 || batch_timer.elapsed() >= Duration::from_secs(1) {
                            pending = 0;
                            batch_timer = Instant::now();
                        }

                        if last_report.elapsed() >= Duration::from_secs(60) {
                            info!("[binance-aggtrade] Inserted {} agg trades in last minute (duplicates={})", inserted, duplicates);
                            inserted = 0;
                            duplicates = 0;
                            last_report = Instant::now();
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => { warn!("[binance-aggtrade] WS closed"); break; }
                    _ => continue,
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// LOB collector (binance_lob_ticks)
// ---------------------------------------------------------------------------

/// Collect orderbook depth snapshots from Binance `@depth<N>@100ms` streams.
///
/// Computes mid-price, spread (bps), OBI (order-book imbalance at depths 5/10),
/// and persists the full level-2 snapshot as JSONB in `bids` / `asks`.
///
/// Schema: `binance_lob_ticks (symbol, update_id, best_bid, best_ask, mid_price,
///          spread_bps, obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks,
///          event_time, source)`
pub async fn collect_binance_lob(
    pool: PgPool,
    symbols_raw: &str,
    depth_levels: usize,
    batch_size: usize,
) {
    let symbols = parse_symbols(symbols_raw);
    let running = running_flag();
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@depth{}@100ms", s.to_lowercase(), depth_levels))
        .collect();

    info!(
        "[binance-lob] Starting collector symbols={:?} depth={}",
        symbols, depth_levels
    );

    loop {
        if !running.load(Ordering::SeqCst) {
            break;
        }

        match run_lob_ws(
            &pool,
            &symbols,
            &streams,
            depth_levels,
            batch_size,
            &running,
        )
        .await
        {
            Ok(()) => {
                info!("[binance-lob] Collector finished (shutdown)");
                break;
            }
            Err(e) => {
                error!("[binance-lob] Connection error: {e}, reconnecting in 5s...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    info!("[binance-lob] Collector stopped");
}

fn parse_level(raw_levels: &[Value]) -> Vec<(Decimal, Decimal)> {
    raw_levels
        .iter()
        .filter_map(|level| {
            let arr = level.as_array()?;
            if arr.len() < 2 {
                return None;
            }
            let price: Decimal = arr[0].as_str()?.parse().ok()?;
            let size: Decimal = arr[1].as_str()?.parse().ok()?;
            if price <= Decimal::ZERO || size <= Decimal::ZERO {
                return None;
            }
            Some((price, size))
        })
        .collect()
}

fn sum_volume(levels: &[(Decimal, Decimal)], depth: usize) -> Decimal {
    levels.iter().take(depth).map(|(_, s)| s).sum()
}

fn obi(bid_vol: Decimal, ask_vol: Decimal) -> Decimal {
    let denom = bid_vol + ask_vol;
    if denom == Decimal::ZERO {
        return Decimal::ZERO;
    }
    (bid_vol - ask_vol) / denom
}

fn levels_to_json(levels: &[(Decimal, Decimal)]) -> Value {
    let arr: Vec<Value> = levels
        .iter()
        .map(|(p, s)| serde_json::json!({"price": format!("{}", p), "size": format!("{}", s)}))
        .collect();
    Value::Array(arr)
}

async fn run_lob_ws(
    pool: &PgPool,
    _symbols: &[String],
    streams: &[String],
    depth_levels: usize,
    batch_size: usize,
    running: &SharedRunning,
) -> CollectorResult<()> {
    let (_write, mut read) = binance_connect(streams).await?;
    info!(
        "[binance-lob] WebSocket connected, subscribed to {} streams",
        streams.len()
    );

    let mut pending = 0u32;
    let mut inserted: u64 = 0;
    let mut last_report = Instant::now();
    let mut batch_timer = Instant::now();

    while running.load(Ordering::SeqCst) {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    None => { warn!("[binance-lob] WebSocket stream ended"); break; }
                    Some(Err(e)) => { warn!("[binance-lob] WebSocket error: {e}"); break; }
                    Some(Ok(Message::Text(text))) => {
                        let val: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
                        if val.get("result").is_some() { continue; }
                        let data = match val.get("data") { Some(d) => d, None => continue, };

                        let stream_name = val.get("stream").and_then(|s| s.as_str());
                        let symbol = data["s"].as_str()
                            .or_else(|| stream_name.and_then(|s| s.split('@').next()))
                            .unwrap_or("")
                            .to_uppercase();

                        let received_at = Utc::now();
                        let event_time_ms = data["E"].as_i64();
                        let update_id = data["u"].as_i64().or_else(|| data["lastUpdateId"].as_i64());
                        let raw_bids = data["b"].as_array().or_else(|| data["bids"].as_array());
                        let raw_asks = data["a"].as_array().or_else(|| data["asks"].as_array());

                        let update_id = match update_id {
                            Some(u) => u,
                            _ => continue,
                        };

                        let bids = raw_bids.map(|levels| parse_level(levels)).unwrap_or_default();
                        let asks = raw_asks.map(|levels| parse_level(levels)).unwrap_or_default();

                        if bids.is_empty() || asks.is_empty() { continue; }

                        let best_bid = bids[0].0;
                        let best_ask = asks[0].0;
                        if best_bid <= Decimal::ZERO || best_ask <= Decimal::ZERO || best_ask < best_bid {
                            continue;
                        }

                        let mid_price = (best_bid + best_ask) / Decimal::from(2);
                        if mid_price <= Decimal::ZERO { continue; }

                        let spread = (best_ask - best_bid) / mid_price * Decimal::from(10_000);
                        let bid_vol_5 = sum_volume(&bids, 5);
                        let ask_vol_5 = sum_volume(&asks, 5);
                        let bid_vol_10 = sum_volume(&bids, 10);
                        let ask_vol_10 = sum_volume(&asks, 10);
                        let event_time = event_time_ms.map(ms_to_utc).unwrap_or(received_at);

                        let bids_json = levels_to_json(&bids[..bids.len().min(depth_levels)]);
                        let asks_json = levels_to_json(&asks[..asks.len().min(depth_levels)]);

                        sqlx::query(
                            "INSERT INTO binance_lob_ticks \
                             (symbol, update_id, best_bid, best_ask, mid_price, spread_bps, \
                              obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks, event_time, source) \
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'binance_depth_ws')"
                        )
                        .bind(&symbol)
                        .bind(update_id)
                        .bind(best_bid)
                        .bind(best_ask)
                        .bind(mid_price)
                        .bind(spread)
                        .bind(obi(bid_vol_5, ask_vol_5))
                        .bind(obi(bid_vol_10, ask_vol_10))
                        .bind(bid_vol_5)
                        .bind(ask_vol_5)
                        .bind(bids_json)
                        .bind(asks_json)
                        .bind(event_time)
                        .execute(pool)
                        .await
                        .map_err(|error| error.to_string())?;

                        pending += 1;
                        inserted += 1;

                        if pending >= batch_size as u32 || batch_timer.elapsed() >= Duration::from_secs(1) {
                            pending = 0;
                            batch_timer = Instant::now();
                        }

                        if last_report.elapsed() >= Duration::from_secs(60) {
                            info!("[binance-lob] Inserted {} snapshots in last minute", inserted);
                            inserted = 0;
                            last_report = Instant::now();
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(_))) => { warn!("[binance-lob] WS closed"); break; }
                    _ => continue,
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::market_updates_from_combined_stream;
    use chrono::Utc;
    use ploy_market_contracts::MarketUpdate;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn direct_binance_trade_tick_becomes_spot_update() {
        let payload = json!({
            "stream": "btcusdt@trade",
            "data": {
                "e": "trade",
                "E": 1712205600120_i64,
                "s": "BTCUSDT",
                "p": "65000.25",
                "q": "0.015",
                "T": 1712205600123_i64
            }
        });

        let updates = market_updates_from_combined_stream(&payload, Utc::now());
        assert_eq!(updates.len(), 1);
        assert!(matches!(
            &updates[0],
            MarketUpdate::SpotPrice { symbol, price, ts }
                if symbol.as_ref() == "BTCUSDT"
                    && *price == dec!(65000.25)
                    && ts.timestamp_millis() == 1_712_205_600_123
        ));
    }

    #[test]
    fn direct_binance_aggtrade_tick_preserves_aggressor_metadata() {
        let payload = json!({
            "stream": "ethusdt@aggTrade",
            "data": {
                "e": "aggTrade",
                "E": 1712205600450_i64,
                "s": "ETHUSDT",
                "a": 987_u64,
                "p": "3500.75",
                "q": "1.25",
                "T": 1712205600456_i64,
                "m": true
            }
        });

        let updates = market_updates_from_combined_stream(&payload, Utc::now());
        assert_eq!(updates.len(), 1);
        assert!(matches!(
            &updates[0],
            MarketUpdate::AggTrade {
                symbol,
                agg_trade_id,
                price,
                quantity,
                is_buyer_maker,
                ts,
            } if symbol.as_ref() == "ETHUSDT"
                && *agg_trade_id == 987
                && *price == dec!(3500.75)
                && *quantity == dec!(1.25)
                && *is_buyer_maker
                && ts.timestamp_millis() == 1_712_205_600_456
        ));
    }

    #[test]
    fn direct_binance_depth_tick_becomes_l2_updates() {
        let payload = json!({
            "stream": "btcusdt@depth20@100ms",
            "data": {
                "E": 1712205600789_i64,
                "lastUpdateId": 42_i64,
                "bids": [["99.95", "5"], ["99.90", "3"]],
                "asks": [["100.05", "2"], ["100.10", "4"]]
            }
        });

        let updates = market_updates_from_combined_stream(&payload, Utc::now());
        assert_eq!(updates.len(), 2);
        assert!(matches!(
            &updates[0],
            MarketUpdate::L2 {
                symbol,
                obi,
                spread_bps,
                ts,
            } if symbol.as_ref() == "BTCUSDT"
                && (*obi - (2.0 / 14.0)).abs() < 1e-9
                && *spread_bps == 10
                && ts.timestamp_millis() == 1_712_205_600_789
        ));
        assert!(matches!(
            &updates[1],
            MarketUpdate::L2Depth {
                symbol,
                bid_depth_near,
                ask_depth_near,
                ..
            } if symbol.as_ref() == "BTCUSDT"
                && (*bid_depth_near - 8.0).abs() < 1e-9
                && (*ask_depth_near - 6.0).abs() < 1e-9
        ));
    }
}
