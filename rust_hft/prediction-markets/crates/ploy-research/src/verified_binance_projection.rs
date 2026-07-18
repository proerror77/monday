use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use data::binance_lob_replay::{Market, ReplaySequenceEvent};
use data::binance_market_tape::AggregateTrade;
use data::binance_market_tape_artifact::{
    ReplayedBinanceBookEvent, VerifiedBinanceLobObservation, VerifiedBinanceMarketTape,
};
use ploy_market_contracts::{BinanceSourceClock, BinanceSourceKind, MarketUpdate};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::ResearchLobSnapshot;

type ProjectedMarketUpdate = (MarketUpdate, BinanceSourceClock);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedBinanceResearchSurfaceCounts {
    pub spot_prices: usize,
    pub aggregate_trades: usize,
    pub lob_snapshots: usize,
}

#[derive(Debug, Clone)]
pub struct VerifiedBinanceResearchProjection {
    pub updates: Vec<MarketUpdate>,
    pub source_clocks: Vec<BinanceSourceClock>,
    pub lob_snapshots: Vec<ResearchLobSnapshot>,
    pub counts: VerifiedBinanceResearchSurfaceCounts,
}

pub fn project_verified_binance_market_tape(
    verified: &VerifiedBinanceMarketTape,
    symbol: &str,
    history_start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_secs: i32,
) -> Result<VerifiedBinanceResearchProjection> {
    ensure!(!symbol.is_empty(), "Binance projection symbol is empty");
    ensure!(history_start < end, "invalid Binance projection window");
    let bucket_ns = u64::try_from(sample_secs)
        .context("Binance sample cadence must be positive")?
        .checked_mul(1_000_000_000)
        .context("Binance sample cadence overflows nanoseconds")?;
    ensure!(bucket_ns > 0, "Binance sample cadence must be positive");

    let segments = verified.segments();
    let first = segments.first().context("verified Binance tape is empty")?;
    let last = segments.last().context("verified Binance tape is empty")?;
    ensure!(
        segments
            .iter()
            .all(|segment| segment.market == Market::Spot),
        "verified Binance tape is not spot"
    );
    ensure_verified_window(
        first.start_received_at_ns,
        last.end_received_at_ns,
        history_start,
        end,
    )?;

    let (paired_updates, spot_prices, aggregate_trades) = project_trades(
        verified.aggregate_trades(),
        symbol,
        history_start,
        end,
        bucket_ns,
    )?;
    let (updates, source_clocks) = paired_updates.into_iter().unzip();

    let book = verified
        .replayed_books()
        .iter()
        .find(|book| book.symbol == symbol)
        .context("requested Binance LOB is absent")?;
    let lob_snapshots = project_lob_snapshots(
        symbol,
        book.events(),
        verified.lob_observations(),
        history_start,
        end,
        bucket_ns,
    )?;
    let counts = VerifiedBinanceResearchSurfaceCounts {
        spot_prices,
        aggregate_trades,
        lob_snapshots: lob_snapshots.len(),
    };

    Ok(VerifiedBinanceResearchProjection {
        updates,
        source_clocks,
        lob_snapshots,
        counts,
    })
}

fn datetime_from_ns(value: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(value / 1_000_000_000).context("nanosecond time overflow")?;
    DateTime::from_timestamp(seconds, (value % 1_000_000_000) as u32)
        .context("nanosecond time is outside DateTime range")
}

fn datetime_from_ms(value: u64) -> Result<DateTime<Utc>> {
    datetime_from_ns(
        value
            .checked_mul(1_000_000)
            .context("millisecond time overflow")?,
    )
}

fn ensure_verified_window(
    first_ns: u64,
    last_ns: u64,
    history_start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<()> {
    ensure!(
        datetime_from_ns(first_ns)? <= history_start && datetime_from_ns(last_ns)? >= end,
        "verified Binance tape does not cover the requested history and window"
    );
    Ok(())
}

#[derive(Default)]
struct AggregateTradeBucket {
    quantity: Decimal,
    notional: Decimal,
    aggregate_trade_id: u64,
    source_time_ms: u64,
    received_at_ns: u64,
}

fn project_trades(
    trades: &[AggregateTrade],
    symbol: &str,
    history_start: DateTime<Utc>,
    end: DateTime<Utc>,
    spot_bucket_ns: u64,
) -> Result<(Vec<ProjectedMarketUpdate>, usize, usize)> {
    let mut spot = BTreeMap::<u64, &AggregateTrade>::new();
    let mut aggregate = BTreeMap::<(u64, bool), AggregateTradeBucket>::new();
    for trade in trades.iter().filter(|trade| trade.symbol == symbol) {
        let received_at = datetime_from_ns(trade.received_at_ns)?;
        if received_at < history_start || received_at >= end {
            continue;
        }
        let spot_key = trade.received_at_ns / spot_bucket_ns;
        if spot.get(&spot_key).is_none_or(|current| {
            (trade.received_at_ns, trade.aggregate_trade_id)
                > (current.received_at_ns, current.aggregate_trade_id)
        }) {
            spot.insert(spot_key, trade);
        }
        let bucket = aggregate
            .entry((trade.trade_time_ms / 5_000, trade.is_buyer_maker))
            .or_default();
        bucket.quantity += trade.quantity;
        bucket.notional += trade.price * trade.quantity;
        bucket.aggregate_trade_id = bucket.aggregate_trade_id.max(trade.aggregate_trade_id);
        bucket.source_time_ms = bucket.source_time_ms.max(trade.trade_time_ms);
        bucket.received_at_ns = bucket.received_at_ns.max(trade.received_at_ns);
    }

    let mut rows = Vec::with_capacity(spot.len() + aggregate.len());
    for trade in spot.into_values() {
        let received_at = datetime_from_ns(trade.received_at_ns)?;
        rows.push((
            MarketUpdate::SpotPrice {
                symbol: Arc::from(symbol),
                price: trade.price,
                ts: received_at,
            },
            BinanceSourceClock {
                kind: BinanceSourceKind::Spot,
                symbol: symbol.to_owned(),
                source_ts: datetime_from_ms(trade.trade_time_ms)?,
                received_at,
                sequence_id: None,
            },
        ));
    }
    let spot_prices = rows.len();
    for ((_, is_buyer_maker), bucket) in aggregate {
        ensure!(
            bucket.quantity > Decimal::ZERO,
            "empty aggregate-trade bucket"
        );
        let received_at = datetime_from_ns(bucket.received_at_ns)?;
        rows.push((
            MarketUpdate::AggTrade {
                symbol: Arc::from(symbol),
                agg_trade_id: bucket.aggregate_trade_id,
                price: bucket.notional / bucket.quantity,
                quantity: bucket.quantity,
                is_buyer_maker,
                ts: received_at,
            },
            BinanceSourceClock {
                kind: BinanceSourceKind::AggTrade,
                symbol: symbol.to_owned(),
                source_ts: datetime_from_ms(bucket.source_time_ms)?,
                received_at,
                sequence_id: Some(bucket.aggregate_trade_id),
            },
        ));
    }
    let aggregate_trades = rows.len() - spot_prices;
    rows.sort_by_key(|(update, _)| {
        (
            update.sort_ts(),
            !matches!(update, MarketUpdate::AggTrade { .. }),
        )
    });
    Ok((rows, spot_prices, aggregate_trades))
}

#[derive(Default)]
struct BookState {
    bids: BTreeMap<Decimal, Decimal>,
    asks: BTreeMap<Decimal, Decimal>,
}

fn apply_levels(book: &mut BTreeMap<Decimal, Decimal>, levels: &[[String; 2]]) -> Result<()> {
    for [price, quantity] in levels {
        let price = price
            .parse::<Decimal>()
            .context("parse verified book price")?;
        let quantity = quantity
            .parse::<Decimal>()
            .context("parse verified book quantity")?;
        ensure!(
            price > Decimal::ZERO && quantity >= Decimal::ZERO,
            "verified book contains an invalid level"
        );
        if quantity.is_zero() {
            book.remove(&price);
        } else {
            book.insert(price, quantity);
        }
    }
    Ok(())
}

fn apply_book_event(state: &mut Option<BookState>, event: &ReplayedBinanceBookEvent) -> Result<()> {
    match event {
        ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot { bids, asks, .. }) => {
            let mut next = BookState::default();
            apply_levels(&mut next.bids, bids)?;
            apply_levels(&mut next.asks, asks)?;
            *state = Some(next);
            Ok(())
        }
        ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff { bids, asks, .. }) => {
            let current = state
                .as_mut()
                .context("verified diff precedes its snapshot")?;
            apply_levels(&mut current.bids, bids)?;
            apply_levels(&mut current.asks, asks)
        }
        ReplayedBinanceBookEvent::Checkpoint { .. } => {
            ensure!(state.is_some(), "verified checkpoint precedes its snapshot");
            Ok(())
        }
    }
}

fn decimal_f64(value: Decimal) -> Result<f64> {
    value.to_f64().context("convert verified book decimal")
}

fn book_depth(state: &BookState, levels: usize) -> (Decimal, Decimal) {
    let bids = state.bids.values().rev().take(levels).copied().sum();
    let asks = state.asks.values().take(levels).copied().sum();
    (bids, asks)
}

fn depth_band(state: &BookState, mid: Decimal, range: Decimal) -> Result<(f64, f64)> {
    let delta = mid * range;
    let sum = |book: &BTreeMap<Decimal, Decimal>, low: Decimal, high: Decimal| -> Decimal {
        book.range(low..=high).map(|(_, quantity)| *quantity).sum()
    };
    Ok((
        decimal_f64(sum(&state.bids, mid - delta, mid))?,
        decimal_f64(sum(&state.asks, mid, mid + delta))?,
    ))
}

fn sample_book(
    symbol: &str,
    state: &BookState,
    source_time_ms: Option<u64>,
    received_at_ns: u64,
) -> Result<ResearchLobSnapshot> {
    let (best_bid, _) = state
        .bids
        .last_key_value()
        .context("verified book has no bids")?;
    let (best_ask, _) = state
        .asks
        .first_key_value()
        .context("verified book has no asks")?;
    ensure!(best_bid < best_ask, "verified replayed book is crossed");
    let mid = (*best_bid + *best_ask) / Decimal::TWO;
    let (bid_5, ask_5) = book_depth(state, 5);
    let (bid_10, ask_10) = book_depth(state, 10);
    ensure!(bid_5 + ask_5 > Decimal::ZERO, "verified book has no depth");
    let imbalance = |bid: Decimal, ask: Decimal| decimal_f64((bid - ask) / (bid + ask));
    let (bid_depth_near, ask_depth_near) = depth_band(state, mid, Decimal::new(1, 3))?;
    let (bid_depth_far, ask_depth_far) = depth_band(state, mid, Decimal::new(5, 3))?;
    let (bid_depth_inner, ask_depth_inner) = depth_band(state, mid, Decimal::new(3, 5))?;
    Ok(ResearchLobSnapshot {
        symbol: symbol.to_owned(),
        source_ts: source_time_ms.map(datetime_from_ms).transpose()?,
        ts: datetime_from_ns(received_at_ns)?,
        obi: imbalance(bid_5, ask_5)?,
        obi_10: imbalance(bid_10, ask_10)?,
        spread_bps: decimal_f64((*best_ask - *best_bid) / mid * Decimal::from(10_000))?,
        best_bid: decimal_f64(*best_bid)?,
        best_ask: decimal_f64(*best_ask)?,
        mid_price: decimal_f64(mid)?,
        bid_depth_near,
        ask_depth_near,
        bid_depth_far,
        ask_depth_far,
        bid_depth_inner,
        ask_depth_inner,
    })
}

fn project_lob_snapshots(
    symbol: &str,
    events: &[ReplayedBinanceBookEvent],
    observations: &[VerifiedBinanceLobObservation],
    history_start: DateTime<Utc>,
    end: DateTime<Utc>,
    bucket_ns: u64,
) -> Result<Vec<ResearchLobSnapshot>> {
    let mut observations = observations
        .iter()
        .filter(|observation| observation.symbol == symbol)
        .peekable();
    let mut latest_source_time_ms = None;
    let mut sampled = BTreeMap::new();
    let mut state = None;
    let mut index = 0;
    while index < events.len() {
        let received_at_ns = events[index].received_at_ns();
        let mut next_index = index;
        while next_index < events.len() && events[next_index].received_at_ns() == received_at_ns {
            apply_book_event(&mut state, &events[next_index])?;
            next_index += 1;
        }
        while observations
            .peek()
            .is_some_and(|observation| observation.received_at_ns <= received_at_ns)
        {
            latest_source_time_ms = observations.next().map(|row| row.source_time_ms);
        }
        let received_at = datetime_from_ns(received_at_ns)?;
        if received_at >= history_start && received_at < end && latest_source_time_ms.is_some() {
            sampled.insert(
                received_at_ns / bucket_ns,
                sample_book(
                    symbol,
                    state
                        .as_ref()
                        .context("verified replay has no book state")?,
                    latest_source_time_ms,
                    received_at_ns,
                )?,
            );
        }
        index = next_index;
    }
    Ok(sampled.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECEIVED_NS: u64 = 1_700_000_030_000_000_000;
    const SOURCE_MS: u64 = 1_700_000_000_000;
    const SECOND_NS: u64 = 1_000_000_000;

    fn window() -> (DateTime<Utc>, DateTime<Utc>) {
        let start = datetime_from_ns(RECEIVED_NS).unwrap();
        (start, start + chrono::Duration::seconds(1))
    }

    fn book_events(snapshot_received_at_ns: u64) -> Vec<ReplayedBinanceBookEvent> {
        vec![
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Snapshot {
                received_at_ns: snapshot_received_at_ns,
                bids: vec![["100".into(), "1".into()]],
                asks: vec![["101".into(), "1".into()]],
            }),
            ReplayedBinanceBookEvent::Replay(ReplaySequenceEvent::Diff {
                received_at_ns: RECEIVED_NS,
                bids: vec![["100.5".into(), "2".into()]],
                asks: Vec::new(),
            }),
        ]
    }

    fn lob_observation() -> VerifiedBinanceLobObservation {
        VerifiedBinanceLobObservation {
            symbol: "BTCUSDT".to_owned(),
            source_time_ms: SOURCE_MS,
            received_at_ns: RECEIVED_NS,
        }
    }

    #[test]
    fn stale_lob_source_clock_is_preserved() {
        let start = datetime_from_ns(RECEIVED_NS - SECOND_NS).unwrap();
        let end = datetime_from_ns(RECEIVED_NS + SECOND_NS).unwrap();

        let rows = project_lob_snapshots(
            "BTCUSDT",
            &book_events(RECEIVED_NS - SECOND_NS),
            &[lob_observation()],
            start,
            end,
            SECOND_NS,
        )
        .unwrap();

        assert_eq!(
            (rows.len(), rows[0].source_ts),
            (1, Some(datetime_from_ms(SOURCE_MS).unwrap()))
        );
    }

    #[test]
    fn equal_receive_time_aggregate_trade_sorts_before_spot_price() {
        let (start, end) = window();
        let trade = AggregateTrade {
            symbol: "BTCUSDT".to_owned(),
            aggregate_trade_id: 7,
            first_trade_id: 7,
            last_trade_id: 7,
            price: Decimal::from(100),
            quantity: Decimal::ONE,
            event_time_ms: RECEIVED_NS / 1_000_000,
            trade_time_ms: RECEIVED_NS / 1_000_000,
            is_buyer_maker: false,
            received_at_ns: RECEIVED_NS,
        };
        let (rows, _, _) = project_trades(&[trade], "BTCUSDT", start, end, SECOND_NS).unwrap();

        assert!(matches!(
            (&rows[0].0, &rows[1].0),
            (
                MarketUpdate::AggTrade { .. },
                MarketUpdate::SpotPrice { .. }
            )
        ));
    }

    #[test]
    fn equal_receive_time_book_events_merge_before_one_sample() {
        let (start, end) = window();

        let rows = project_lob_snapshots(
            "BTCUSDT",
            &book_events(RECEIVED_NS),
            &[lob_observation()],
            start,
            end,
            SECOND_NS,
        )
        .unwrap();

        assert_eq!((rows.len(), rows[0].best_bid), (1, 100.5));
    }

    #[test]
    fn insufficient_verified_window_fails_closed() {
        let (start, end) = window();
        let end_ns = RECEIVED_NS + SECOND_NS;

        assert_eq!(
            (
                ensure_verified_window(RECEIVED_NS + 1, end_ns, start, end).is_err(),
                ensure_verified_window(RECEIVED_NS, end_ns - 1, start, end).is_err(),
            ),
            (true, true)
        );
    }
}
