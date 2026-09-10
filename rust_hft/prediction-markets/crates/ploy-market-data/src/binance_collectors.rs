//! Canonical Binance continuous market-data collectors.
//!
//! Transport, JSON parsing, venue selection, snapshot/delta sequencing, and
//! reconnect invalidation live in `data-pipelines/adapters/adapter-binance`.
//! This module is only the durable sink and the legacy `MarketUpdate` bridge.

pub use adapter_binance_data::BinanceMarketKind;
use adapter_binance_data::{
    BinanceBook, BinanceTradeStreams, MarketEvent, MarketStream, Symbol, TrackedMarketEvent,
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use ploy_market_contracts::{l2_updates_from_depth_totals, MarketUpdate};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Copy)]
enum CollectionSurface {
    SpotTrade,
    AggregateTrade,
    Lob,
}

type CollectorResult<T> = Result<T, String>;

fn parse_symbols(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|symbol| symbol.trim().to_ascii_uppercase())
        .filter(|symbol| !symbol.is_empty())
        .collect()
}

fn utc_from_micros(micros: u64) -> CollectorResult<DateTime<Utc>> {
    i64::try_from(micros)
        .ok()
        .and_then(DateTime::from_timestamp_micros)
        .ok_or_else(|| format!("Binance timestamp is out of range: {micros}"))
}

fn market_type(kind: BinanceMarketKind) -> &'static str {
    kind.market_type()
}

fn venue(kind: BinanceMarketKind) -> &'static str {
    match kind {
        BinanceMarketKind::Spot => "binance",
        BinanceMarketKind::UsdM => "binance_futures",
    }
}

fn depth_mode() -> &'static str {
    let mode = std::env::var("COLLECTOR_DEPTH_MODE")
        .or_else(|_| std::env::var("BINANCE_DEPTH_MODE"))
        .unwrap_or_else(|_| "partial20".to_string())
        .to_ascii_lowercase();
    let force_partial = matches!(
        std::env::var("BINANCE_USE_LIMITED")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    );
    if !force_partial
        && matches!(
            mode.as_str(),
            "diff" | "diff-depth" | "full" | "incremental"
        )
    {
        "diff"
    } else {
        "partial"
    }
}

fn validate_depth_levels(depth_levels: usize) -> CollectorResult<()> {
    if matches!(depth_levels, 5 | 10 | 20) {
        Ok(())
    } else {
        Err(format!(
            "unsupported Binance depth {depth_levels}; use 5, 10, or 20"
        ))
    }
}

fn event_timestamps(event: &MarketEvent) -> (Option<u64>, Option<u64>, Option<u64>) {
    let timestamps = event.timestamps();
    (
        timestamps
            .and_then(|value| value.exchange_event)
            .map(|value| value.as_micros()),
        timestamps
            .and_then(|value| value.exchange_trade)
            .map(|value| value.as_micros()),
        timestamps
            .and_then(|value| value.local_receive)
            .map(|value| value.as_micros()),
    )
}

fn event_receive_time(event: &MarketEvent) -> CollectorResult<DateTime<Utc>> {
    let local_receive = event_timestamps(event)
        .2
        .ok_or_else(|| "Binance canonical event omitted local receive timestamp".to_string())?;
    utc_from_micros(local_receive)
}

fn trade_times(
    trade: &adapter_binance_data::Trade,
) -> CollectorResult<(DateTime<Utc>, DateTime<Utc>, DateTime<Utc>)> {
    let event = MarketEvent::Trade(trade.clone());
    let (exchange_event, exchange_trade, local_receive) = event_timestamps(&event);
    let exchange_event = exchange_event
        .ok_or_else(|| "Binance trade omitted exchange event timestamp".to_string())?;
    let exchange_trade = exchange_trade
        .ok_or_else(|| "Binance trade omitted exchange trade timestamp".to_string())?;
    let local_receive =
        local_receive.ok_or_else(|| "Binance trade omitted local receive timestamp".to_string())?;
    let trade_time = utc_from_micros(exchange_trade)?;
    let event_time = utc_from_micros(exchange_event)?;
    let received_at = utc_from_micros(local_receive)?;
    Ok((trade_time, event_time, received_at))
}

fn trade_id_i64(trade_id: &str) -> CollectorResult<i64> {
    trade_id
        .parse::<i64>()
        .map_err(|error| format!("Binance trade ID is not an i64: {trade_id}: {error}"))
}

fn sequence_i64(sequence: Option<u64>, field: &str) -> CollectorResult<Option<i64>> {
    sequence
        .map(|value| {
            i64::try_from(value)
                .map_err(|_| format!("Binance {field} sequence is not an i64: {value}"))
        })
        .transpose()
}

async fn persist_raw_trade(
    pool: &PgPool,
    kind: BinanceMarketKind,
    trade: &adapter_binance_data::Trade,
) -> CollectorResult<()> {
    if trade.aggregate.is_some() {
        return Ok(());
    }
    let (trade_time, event_time, received_at) = trade_times(trade)?;
    sqlx::query(
        "INSERT INTO binance_price_ticks \
         (market_type, venue, symbol, trade_id, price, quantity, trade_time, event_time, received_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT DO NOTHING",
    )
    .bind(market_type(kind))
    .bind(venue(kind))
    .bind(trade.symbol.as_str())
    .bind(trade_id_i64(&trade.trade_id)?)
    .bind(trade.price.0)
    .bind(trade.quantity.0)
    .bind(trade_time)
    .bind(event_time)
    .bind(received_at)
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn persist_aggregate_trade(
    pool: &PgPool,
    kind: BinanceMarketKind,
    trade: &adapter_binance_data::Trade,
) -> CollectorResult<()> {
    let Some(metadata) = trade.aggregate.as_ref() else {
        return Ok(());
    };
    let (trade_time, event_time, received_at) = trade_times(trade)?;
    sqlx::query(
        "INSERT INTO binance_agg_trade_ticks \
         (market_type, venue, symbol, agg_trade_id, first_trade_id, last_trade_id, \
          price, quantity, trade_time, event_time, is_buyer_maker, source, received_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
         ON CONFLICT DO NOTHING",
    )
    .bind(market_type(kind))
    .bind(venue(kind))
    .bind(trade.symbol.as_str())
    .bind(
        i64::try_from(metadata.aggregate_trade_id)
            .map_err(|_| "aggregate trade ID overflow".to_string())?,
    )
    .bind(
        i64::try_from(metadata.first_trade_id)
            .map_err(|_| "first trade ID overflow".to_string())?,
    )
    .bind(i64::try_from(metadata.last_trade_id).map_err(|_| "last trade ID overflow".to_string())?)
    .bind(trade.price.0)
    .bind(trade.quantity.0)
    .bind(trade_time)
    .bind(event_time)
    .bind(metadata.is_buyer_maker)
    .bind("binance_canonical_agg_trade")
    .bind(received_at)
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn level_json(levels: &[adapter_binance_data::BookLevel]) -> Value {
    Value::Array(
        levels
            .iter()
            .map(|level| {
                serde_json::json!({
                    "price": level.price.to_string(),
                    "size": level.quantity.to_string()
                })
            })
            .collect(),
    )
}

struct LobMetrics {
    best_bid: Decimal,
    best_ask: Decimal,
    mid_price: Decimal,
    spread_bps: Decimal,
    obi_5: Decimal,
    obi_10: Decimal,
    bid_volume_5: Decimal,
    ask_volume_5: Decimal,
}

fn lob_metrics(snapshot: &adapter_binance_data::MarketSnapshot) -> Option<LobMetrics> {
    let best_bid = snapshot.bids.first()?.price.0;
    let best_ask = snapshot.asks.first()?.price.0;
    if best_bid <= Decimal::ZERO || best_ask <= best_bid {
        return None;
    }
    let mid_price = (best_bid + best_ask) / Decimal::from(2);
    let volume = |levels: &[adapter_binance_data::BookLevel], depth: usize| {
        levels
            .iter()
            .take(depth)
            .map(|level| level.quantity.0)
            .sum::<Decimal>()
    };
    let bid_volume_5 = volume(&snapshot.bids, 5);
    let ask_volume_5 = volume(&snapshot.asks, 5);
    let bid_volume_10 = volume(&snapshot.bids, 10);
    let ask_volume_10 = volume(&snapshot.asks, 10);
    let imbalance = |bid: Decimal, ask: Decimal| {
        let total = bid + ask;
        if total.is_zero() {
            Decimal::ZERO
        } else {
            (bid - ask) / total
        }
    };
    Some(LobMetrics {
        best_bid,
        best_ask,
        mid_price,
        spread_bps: (best_ask - best_bid) / mid_price * Decimal::from(10_000),
        obi_5: imbalance(bid_volume_5, ask_volume_5),
        obi_10: imbalance(bid_volume_10, ask_volume_10),
        bid_volume_5,
        ask_volume_5,
    })
}

async fn persist_lob(
    pool: &PgPool,
    kind: BinanceMarketKind,
    tracked: &TrackedMarketEvent,
    snapshot: &adapter_binance_data::MarketSnapshot,
    depth_levels: usize,
) -> CollectorResult<()> {
    let Some(metrics) = lob_metrics(snapshot) else {
        return Ok(());
    };
    let (first_sequence, update_sequence) = match &tracked.event {
        MarketEvent::Update(update) => (update.first_sequence, Some(update.sequence)),
        MarketEvent::Snapshot(snapshot) => (None, Some(snapshot.sequence)),
        _ => (None, None),
    };
    let received_at = event_receive_time(&tracked.event)?;
    let event_time = snapshot
        .timestamps
        .exchange_event
        .map(|value| utc_from_micros(value.as_micros()))
        .transpose()?;
    let bids = snapshot.bids[..snapshot.bids.len().min(depth_levels)].to_vec();
    let asks = snapshot.asks[..snapshot.asks.len().min(depth_levels)].to_vec();
    sqlx::query(
        "INSERT INTO binance_lob_ticks \
         (market_type, venue, symbol, depth_mode, update_id, first_sequence, previous_sequence, \
          best_bid, best_ask, mid_price, spread_bps, obi_5, obi_10, bid_volume_5, ask_volume_5, \
          bids, asks, event_time, source, received_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)",
    )
    .bind(market_type(kind))
    .bind(venue(kind))
    .bind(snapshot.symbol.as_str())
    .bind(depth_mode())
    .bind(sequence_i64(update_sequence, "update")?)
    .bind(sequence_i64(first_sequence, "first")?)
    .bind(sequence_i64(tracked.previous_sequence, "previous")?)
    .bind(metrics.best_bid)
    .bind(metrics.best_ask)
    .bind(metrics.mid_price)
    .bind(metrics.spread_bps)
    .bind(metrics.obi_5)
    .bind(metrics.obi_10)
    .bind(metrics.bid_volume_5)
    .bind(metrics.ask_volume_5)
    .bind(level_json(&bids))
    .bind(level_json(&asks))
    .bind(event_time)
    .bind("binance_canonical_depth")
    .bind(received_at)
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn run_database_collector(
    pool: PgPool,
    symbols_raw: &str,
    kind: BinanceMarketKind,
    surface: CollectionSurface,
    depth_levels: usize,
    _batch_size: usize,
) -> CollectorResult<()> {
    let symbols = parse_symbols(symbols_raw);
    if symbols.is_empty() {
        return Err("Binance collector requires at least one symbol".to_string());
    }
    let trade_streams = match surface {
        CollectionSurface::SpotTrade => BinanceTradeStreams::Raw,
        CollectionSurface::AggregateTrade => BinanceTradeStreams::Aggregate,
        CollectionSurface::Lob => BinanceTradeStreams::None,
    };
    let adapter = kind
        .stream()
        .with_trade_streams(trade_streams)
        .with_depth_stream(matches!(surface, CollectionSurface::Lob))
        .with_book_ticker(false);
    let adapter = if matches!(surface, CollectionSurface::Lob) {
        adapter
            .with_depth_levels(depth_levels)
            .map_err(|error| error.to_string())?
    } else {
        adapter
    };
    let mut stream = adapter
        .subscribe_tracked(symbols.iter().map(Symbol::new).collect())
        .await
        .map_err(|error| error.to_string())?;
    let mut books = HashMap::<String, BinanceBook>::new();
    while let Some(result) = stream.next().await {
        match result {
            Ok(tracked) => match &tracked.event {
                MarketEvent::Trade(trade) => match surface {
                    CollectionSurface::SpotTrade => persist_raw_trade(&pool, kind, trade).await?,
                    CollectionSurface::AggregateTrade => {
                        persist_aggregate_trade(&pool, kind, trade).await?
                    }
                    CollectionSurface::Lob => {}
                },
                MarketEvent::Snapshot(_) | MarketEvent::Update(_)
                    if matches!(surface, CollectionSurface::Lob) =>
                {
                    let symbol = match &tracked.event {
                        MarketEvent::Snapshot(snapshot) => snapshot.symbol.as_str().to_string(),
                        MarketEvent::Update(update) => update.symbol.as_str().to_string(),
                        _ => return Err("Binance depth event omitted symbol".to_string()),
                    };
                    let book = books.entry(symbol).or_default();
                    if let Some(snapshot) = book
                        .apply(&tracked.event)
                        .map_err(|error| error.to_string())?
                    {
                        persist_lob(&pool, kind, &tracked, &snapshot, depth_levels).await?;
                    }
                }
                MarketEvent::Disconnect { .. } => books.clear(),
                _ => {}
            },
            Err(error) => {
                books.clear();
                warn!(error = %error, "Canonical Binance stream rejected a frame; awaiting adapter recovery");
            }
        }
    }
    Ok(())
}

async fn run_until_reconnect(
    pool: PgPool,
    symbols: &str,
    kind: BinanceMarketKind,
    surface: CollectionSurface,
    depth_levels: usize,
    batch_size: usize,
) {
    loop {
        let collection = run_database_collector(
            pool.clone(),
            symbols,
            kind,
            surface,
            depth_levels,
            batch_size,
        );
        tokio::pin!(collection);
        tokio::select! {
            result = &mut collection => {
                if let Err(error) = result {
                    error!(%error, market_type = kind.market_type(), "Canonical Binance collector stopped; reconnecting");
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {}
                        _ = tokio::signal::ctrl_c() => {
                            info!(market_type = kind.market_type(), "Canonical Binance collector stopped by Ctrl-C");
                            break;
                        }
                    }
                } else {
                    break;
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!(market_type = kind.market_type(), "Canonical Binance collector stopped by Ctrl-C");
                break;
            }
        }
    }
}

pub async fn collect_binance_price(
    pool: PgPool,
    symbols_raw: &str,
    batch_size: usize,
    kind: BinanceMarketKind,
) {
    run_until_reconnect(
        pool,
        symbols_raw,
        kind,
        CollectionSurface::SpotTrade,
        20,
        batch_size,
    )
    .await;
}

pub async fn collect_binance_aggtrade(
    pool: PgPool,
    symbols_raw: &str,
    batch_size: usize,
    kind: BinanceMarketKind,
) {
    run_until_reconnect(
        pool,
        symbols_raw,
        kind,
        CollectionSurface::AggregateTrade,
        20,
        batch_size,
    )
    .await;
}

pub async fn collect_binance_lob(
    pool: PgPool,
    symbols_raw: &str,
    depth_levels: usize,
    batch_size: usize,
    kind: BinanceMarketKind,
) {
    if let Err(error) = validate_depth_levels(depth_levels) {
        error!(%error, "Canonical Binance LOB collector depth configuration rejected");
        return;
    }
    run_until_reconnect(
        pool,
        symbols_raw,
        kind,
        CollectionSurface::Lob,
        depth_levels,
        batch_size,
    )
    .await;
}

fn unavailable_spot_ticks(tx: &broadcast::Sender<MarketUpdate>, symbols: &[String]) -> bool {
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

fn market_updates_from_canonical_trade(
    trade: &adapter_binance_data::Trade,
) -> Option<(MarketUpdate, DateTime<Utc>)> {
    let (trade_time, _event_time, received_at) = trade_times(trade).ok()?;
    let update = if let Some(metadata) = trade.aggregate.as_ref() {
        MarketUpdate::AggTrade {
            symbol: Arc::from(trade.symbol.as_str()),
            agg_trade_id: metadata.aggregate_trade_id,
            price: trade.price.0,
            quantity: trade.quantity.0,
            is_buyer_maker: metadata.is_buyer_maker,
            ts: trade_time,
        }
    } else {
        MarketUpdate::SpotPrice {
            symbol: Arc::from(trade.symbol.as_str()),
            price: trade.price.0,
            ts: trade_time,
        }
    };
    Some((update, received_at))
}

fn market_updates_from_canonical_book(
    snapshot: &adapter_binance_data::MarketSnapshot,
) -> Option<Vec<MarketUpdate>> {
    // `MarketUpdate` carries only one timestamp, so the bridge must at least
    // prove a real local receive time before exposing a snapshot. The
    // exchange event clock remains optional for partial-depth/REST frames and
    // is kept separately in the canonical snapshot for persistence.
    snapshot.timestamps.local_receive?;
    let timestamp = utc_from_micros(snapshot.timestamp).ok()?;
    let Some(metrics) = lob_metrics(snapshot) else {
        return Some(Vec::new());
    };
    Some(l2_updates_from_depth_totals(
        snapshot.symbol.as_str(),
        metrics.obi_5.to_f64().unwrap_or_default(),
        metrics.spread_bps.round_dp(0).to_u32().unwrap_or(u32::MAX),
        metrics.bid_volume_5,
        metrics.ask_volume_5,
        timestamp,
    ))
}

/// Direct strategy feed backed by the canonical Binance adapter. The adapter
/// owns transport and sequencing; this bridge owns only legacy MarketUpdate
/// projection. It defaults to Spot for existing strategy configs.
pub fn spawn_binance_tick_feed(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: crate::reference_prices::ReferencePriceRegistry,
    symbols: Vec<String>,
    depth_levels: usize,
) -> JoinHandle<()> {
    spawn_binance_tick_feed_for_market(
        tx,
        reference_prices,
        symbols,
        depth_levels,
        BinanceMarketKind::Spot,
    )
}

pub fn spawn_binance_tick_feed_for_market(
    tx: Arc<broadcast::Sender<MarketUpdate>>,
    reference_prices: crate::reference_prices::ReferencePriceRegistry,
    symbols: Vec<String>,
    depth_levels: usize,
    kind: BinanceMarketKind,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let symbols = symbols
            .into_iter()
            .map(|symbol| symbol.trim().to_ascii_uppercase())
            .filter(|symbol| !symbol.is_empty())
            .collect::<Vec<_>>();
        if symbols.is_empty() {
            warn!("Canonical Binance direct feed has no configured symbols");
            return;
        }
        let adapter = match kind.stream().with_depth_levels(depth_levels) {
            Ok(adapter) => adapter.with_trade_streams(BinanceTradeStreams::Both),
            Err(error) => {
                error!(%error, depth_levels, "Canonical Binance direct feed depth configuration rejected");
                let _ = unavailable_spot_ticks(&tx, &symbols);
                return;
            }
        };
        let mut stream = match adapter
            .subscribe_tracked(symbols.iter().map(Symbol::new).collect())
            .await
        {
            Ok(stream) => stream,
            Err(error) => {
                error!(%error, "Canonical Binance direct feed subscription failed");
                let _ = unavailable_spot_ticks(&tx, &symbols);
                return;
            }
        };
        let mut books = HashMap::<String, BinanceBook>::new();
        while let Some(result) = stream.next().await {
            match result {
                Ok(tracked) => match &tracked.event {
                    MarketEvent::Trade(trade) => {
                        let Some((update, received_at)) =
                            market_updates_from_canonical_trade(trade)
                        else {
                            warn!(
                                symbol = %trade.symbol,
                                "Canonical Binance trade omitted a proven exchange/local clock"
                            );
                            continue;
                        };
                        if let MarketUpdate::SpotPrice { symbol, price, ts } = &update {
                            crate::reference_prices::upsert_reference_price(
                                &reference_prices,
                                crate::reference_prices::ReferencePriceSnapshot {
                                    key: crate::reference_prices::ReferencePriceKey {
                                        source: crate::reference_prices::ReferencePriceSource::Binance,
                                        symbol: crate::reference_prices::market_symbol_to_binance_symbol(symbol),
                                    },
                                    asset_class: crate::reference_prices::ReferenceAssetClass::Crypto,
                                    value: *price,
                                    full_accuracy_value: None,
                                    source_timestamp: *ts,
                                    received_at,
                                    is_carried_forward: false,
                                },
                            )
                            .await;
                        }
                        if tx.send(update).is_err() {
                            return;
                        }
                    }
                    MarketEvent::Snapshot(_) | MarketEvent::Update(_) => {
                        let symbol = match &tracked.event {
                            MarketEvent::Snapshot(snapshot) => &snapshot.symbol,
                            MarketEvent::Update(update) => &update.symbol,
                            _ => continue,
                        };
                        let book = books.entry(symbol.as_str().to_string()).or_default();
                        match book.apply(&tracked.event) {
                            Ok(Some(snapshot)) => {
                                let Some(updates) = market_updates_from_canonical_book(&snapshot)
                                else {
                                    books.clear();
                                    warn!(
                                        symbol = %symbol,
                                        "Canonical Binance book omitted a proven local/source clock"
                                    );
                                    if !unavailable_spot_ticks(&tx, &symbols) {
                                        return;
                                    }
                                    continue;
                                };
                                for update in updates {
                                    if tx.send(update).is_err() {
                                        return;
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                books.clear();
                                warn!(%error, "Canonical Binance book projection rejected event");
                            }
                        }
                    }
                    MarketEvent::Disconnect { .. } => {
                        books.clear();
                        if !unavailable_spot_ticks(&tx, &symbols) {
                            return;
                        }
                    }
                    _ => {}
                },
                Err(error) => {
                    books.clear();
                    warn!(%error, "Canonical Binance direct feed rejected frame");
                    if !unavailable_spot_ticks(&tx, &symbols) {
                        return;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        market_updates_from_canonical_book, market_updates_from_canonical_trade,
        validate_depth_levels,
    };
    use adapter_binance_data::{
        AggregateTradeMetadata, BookLevel, MarketSnapshot, Price, Quantity, Side, Symbol, VenueId,
    };
    use rust_decimal_macros::dec;

    fn canonical_timestamps() -> adapter_binance_data::MarketDataTimestamps {
        adapter_binance_data::MarketDataTimestamps {
            exchange_event: Some(adapter_binance_data::ExchangeEventTimestamp::new(1_000_100)),
            exchange_trade: Some(adapter_binance_data::ExchangeTradeTimestamp::new(1_000_000)),
            local_receive: Some(adapter_binance_data::LocalReceiveTimestamp::new(1_000_200)),
        }
    }

    #[test]
    fn canonical_raw_trade_maps_to_spot_price() {
        let trade = adapter_binance_data::Trade {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1_000_000,
            price: Price(dec!(100)),
            quantity: Quantity(dec!(1)),
            side: Side::Buy,
            trade_id: "7".to_string(),
            source_venue: Some(VenueId::BINANCE),
            timestamps: canonical_timestamps(),
            aggregate: None,
        };
        let (update, received_at) = market_updates_from_canonical_trade(&trade).unwrap();
        assert!(matches!(
            update,
            ploy_market_contracts::MarketUpdate::SpotPrice { .. }
        ));
        assert_eq!(received_at.timestamp_micros(), 1_000_200);
    }

    #[test]
    fn canonical_aggregate_trade_maps_without_losing_aggregate_id() {
        let trade = adapter_binance_data::Trade {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1_000_000,
            price: Price(dec!(100)),
            quantity: Quantity(dec!(1)),
            side: Side::Sell,
            trade_id: "9".to_string(),
            source_venue: Some(VenueId::BINANCE_FUTURES),
            timestamps: canonical_timestamps(),
            aggregate: Some(AggregateTradeMetadata {
                aggregate_trade_id: 9,
                first_trade_id: 8,
                last_trade_id: 9,
                is_buyer_maker: true,
            }),
        };
        let (update, _) = market_updates_from_canonical_trade(&trade).unwrap();
        assert!(matches!(
            update,
            ploy_market_contracts::MarketUpdate::AggTrade {
                agg_trade_id: 9,
                is_buyer_maker: true,
                ..
            }
        ));
    }

    #[test]
    fn canonical_depth_uses_full_snapshot_before_l2_projection() {
        let snapshot = MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1_000_000,
            bids: vec![
                BookLevel {
                    price: Price(dec!(99)),
                    quantity: Quantity(dec!(2)),
                },
                BookLevel {
                    price: Price(dec!(98)),
                    quantity: Quantity(dec!(1)),
                },
            ],
            asks: vec![
                BookLevel {
                    price: Price(dec!(101)),
                    quantity: Quantity(dec!(3)),
                },
                BookLevel {
                    price: Price(dec!(102)),
                    quantity: Quantity(dec!(1)),
                },
            ],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: adapter_binance_data::MarketDataTimestamps {
                exchange_event: None,
                exchange_trade: None,
                local_receive: Some(adapter_binance_data::LocalReceiveTimestamp::new(1_000_000)),
            },
            provider_identity: None,
        };
        let updates = market_updates_from_canonical_book(&snapshot).unwrap();
        assert_eq!(updates.len(), 2);
    }

    #[test]
    fn canonical_projection_rejects_missing_clock_evidence() {
        let trade = adapter_binance_data::Trade {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1_000_000,
            price: Price(dec!(100)),
            quantity: Quantity(dec!(1)),
            side: Side::Buy,
            trade_id: "7".to_string(),
            source_venue: Some(VenueId::BINANCE),
            timestamps: adapter_binance_data::MarketDataTimestamps::default(),
            aggregate: None,
        };
        assert!(market_updates_from_canonical_trade(&trade).is_none());
        let snapshot = MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1_000_000,
            bids: vec![BookLevel::new(99.0, 2.0).unwrap()],
            asks: vec![BookLevel::new(101.0, 3.0).unwrap()],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: Default::default(),
            provider_identity: None,
        };
        assert!(market_updates_from_canonical_book(&snapshot).is_none());

        let exchange_only = MarketSnapshot {
            symbol: Symbol::new("BTCUSDT"),
            timestamp: 1_000_000,
            bids: vec![BookLevel::new(99.0, 2.0).unwrap()],
            asks: vec![BookLevel::new(101.0, 3.0).unwrap()],
            sequence: 1,
            source_venue: Some(VenueId::BINANCE),
            timestamps: adapter_binance_data::MarketDataTimestamps {
                exchange_event: Some(adapter_binance_data::ExchangeEventTimestamp::new(1_000_000)),
                exchange_trade: None,
                local_receive: None,
            },
            provider_identity: None,
        };
        assert!(market_updates_from_canonical_book(&exchange_only).is_none());
    }

    #[test]
    fn collector_depth_configuration_is_bounded_to_canonical_levels() {
        assert!(validate_depth_levels(5).is_ok());
        assert!(validate_depth_levels(10).is_ok());
        assert!(validate_depth_levels(20).is_ok());
        assert!(validate_depth_levels(50).is_err());
    }
}
