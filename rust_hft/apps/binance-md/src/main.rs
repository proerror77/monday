//! Binance market-data fast-lane runner.
//!
//! This binary is intentionally narrow: it owns live WebSocket/REST I/O,
//! replay files, latency summaries, and paper evaluation. The hot lane remains
//! in `engine::binance_md`.

use clap::{Parser, Subcommand};
use data_adapter_binance::BinanceRestClient;
use engine::binance_md::{
    normalize_depth_update, parse_fixed_6, read_replay_records, write_replay_batch,
    BinanceDepthUpdate, BookSyncState, FeatureSnapshot, LatencyTrace, MarketDataLane,
    ParsedDepthUpdate, ReplayBatch, ReplayKind, ReplayPayload, ReplayRecord, SequenceDecision,
    Signal, SignalRules, SignalSide,
};
use futures_util::StreamExt;
use hdrhistogram::Histogram;
use hft_core::{now_micros, Symbol};
use serde::de::Error as _;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

const BINANCE_SPOT_WS: &str = "wss://stream.binance.com:9443/stream";
const BTCUSDT_ID: u32 = 1;

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run live Binance depth + bookTicker sanity collection.
    Live(LiveArgs),
    /// Replay a Binance MD NDJSON file through the same lane.
    Replay(ReplayArgs),
    /// Evaluate expiring signals from a Binance MD replay file with paper fills.
    Paper(PaperArgs),
}

#[derive(Parser)]
struct LiveArgs {
    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,
    #[arg(long, default_value_t = BTCUSDT_ID)]
    symbol_id: u32,
    #[arg(long, default_value_t = 50)]
    depth_levels: u16,
    #[arg(long)]
    replay_out: Option<PathBuf>,
    #[arg(long, default_value_t = 60)]
    report_every_secs: u64,
    #[arg(long)]
    max_messages: Option<u64>,
    #[arg(long)]
    max_runtime_secs: Option<u64>,
    #[arg(long, default_value = BINANCE_SPOT_WS)]
    ws_base_url: String,
}

#[derive(Parser)]
struct ReplayArgs {
    #[arg(long)]
    replay_in: PathBuf,
    #[arg(long, default_value_t = BTCUSDT_ID)]
    symbol_id: u32,
}

#[derive(Parser)]
struct PaperArgs {
    #[arg(long)]
    replay_in: PathBuf,
    #[arg(long, default_value_t = BTCUSDT_ID)]
    symbol_id: u32,
    #[arg(long, default_value_t = 2.0)]
    fee_bps: f64,
    #[arg(long, default_value_t = 1.0)]
    slippage_bps: f64,
    #[arg(long, default_value_t = 10_000.0)]
    notional: f64,
}

#[derive(Debug, Deserialize)]
struct Envelope<'a> {
    #[serde(borrow)]
    stream: &'a str,
}

#[derive(Debug, Deserialize)]
struct DepthEnvelope<'a> {
    #[serde(borrow)]
    data: BinanceDepthUpdate<'a>,
}

#[derive(Debug, Deserialize)]
struct BookTickerEnvelope<'a> {
    #[serde(borrow)]
    data: BookTicker<'a>,
}

#[derive(Debug, Deserialize)]
struct BookTicker<'a> {
    #[serde(rename = "b", borrow)]
    best_bid_price: &'a str,
    #[serde(rename = "a", borrow)]
    best_ask_price: &'a str,
}

#[derive(Debug, Clone, Copy)]
struct BookTickerSnapshot {
    best_bid_price: i64,
    best_ask_price: i64,
}

struct LatencyHistograms {
    parse: Histogram<u64>,
    book: Histogram<u64>,
    feature: Histogram<u64>,
    signal: Histogram<u64>,
    total: Histogram<u64>,
}

impl LatencyHistograms {
    fn new() -> Self {
        Self {
            parse: Histogram::new(3).expect("valid parse histogram"),
            book: Histogram::new(3).expect("valid book histogram"),
            feature: Histogram::new(3).expect("valid feature histogram"),
            signal: Histogram::new(3).expect("valid signal histogram"),
            total: Histogram::new(3).expect("valid total histogram"),
        }
    }

    fn record(&mut self, latency: LatencyTrace) {
        record_non_negative(&mut self.parse, latency.parse_latency_ns());
        record_non_negative(&mut self.book, latency.book_latency_ns());
        record_non_negative(&mut self.feature, latency.feature_latency_ns());
        record_non_negative(&mut self.signal, latency.signal_latency_ns());
        record_non_negative(&mut self.total, latency.total_latency_ns());
    }

    fn print(&self, label: &str) {
        print_histogram(label, "parse", &self.parse);
        print_histogram(label, "book", &self.book);
        print_histogram(label, "feature", &self.feature);
        print_histogram(label, "signal", &self.signal);
        print_histogram(label, "total", &self.total);
    }
}

struct LiveStats {
    depth_messages: u64,
    book_ticker_messages: u64,
    signals: u64,
    rebuilds: u64,
    book_ticker_checks: u64,
    book_ticker_mismatches: u64,
    replay_records: u64,
}

impl LiveStats {
    fn new() -> Self {
        Self {
            depth_messages: 0,
            book_ticker_messages: 0,
            signals: 0,
            rebuilds: 0,
            book_ticker_checks: 0,
            book_ticker_mismatches: 0,
            replay_records: 0,
        }
    }
}

#[derive(Debug, Default)]
struct ReplaySummary {
    snapshots: u64,
    depth_updates: u64,
    applied: u64,
    ignored: u64,
    gaps: u64,
    features: u64,
    signals: u64,
    parity_mismatches: u64,
    final_update_id: u64,
}

#[derive(Debug, Default)]
struct PaperSummary {
    signals: u64,
    longs: u64,
    shorts: u64,
    realized_trades: u64,
    gross_pnl: f64,
    fees: f64,
    slippage: f64,
    net_pnl: f64,
    max_drawdown: f64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt().with_env_filter("info").init();
    let args = Args::parse();

    match args.command {
        Command::Live(args) => run_live(args).await?,
        Command::Replay(args) => {
            let summary = replay_file(&args.replay_in, args.symbol_id)?;
            info!("replay summary: {:?}", summary);
        }
        Command::Paper(args) => {
            let summary = paper_file(&args.replay_in, args.symbol_id, &args)?;
            info!("paper summary: {:?}", summary);
        }
    }

    Ok(())
}

async fn run_live(args: LiveArgs) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let symbol = args.symbol.to_uppercase();
    let mut lane = MarketDataLane::<50>::new(args.symbol_id, SignalRules::default());
    let mut histograms = LatencyHistograms::new();
    let mut stats = LiveStats::new();
    let mut replay_writer = match &args.replay_out {
        Some(path) => Some(BufWriter::new(File::create(path)?)),
        None => None,
    };

    lane.start_buffering();
    let ws_url = build_stream_url(&args.ws_base_url, &symbol);
    info!("connecting Binance stream: {}", ws_url);
    let (mut ws, _) = connect_async(&ws_url).await?;
    info!("connected; buffering first diff before REST snapshot");

    let start = Instant::now();
    let mut last_report = Instant::now();
    let mut snapshot_bridged = false;
    let mut latest_book_ticker: Option<BookTickerSnapshot> = None;

    loop {
        if args
            .max_runtime_secs
            .is_some_and(|secs| start.elapsed() >= Duration::from_secs(secs))
        {
            break;
        }
        if args
            .max_messages
            .is_some_and(|max| stats.depth_messages >= max)
        {
            break;
        }

        let Some(message) = ws.next().await else {
            warn!("websocket stream ended");
            break;
        };
        let message = message?;
        let bytes = match message {
            Message::Text(text) => text.to_string().into_bytes(),
            Message::Binary(bytes) => bytes.to_vec(),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
            Message::Close(frame) => {
                warn!("websocket closed: {:?}", frame);
                break;
            }
        };

        let envelope: Envelope<'_> = match serde_json::from_slice(&bytes) {
            Ok(envelope) => envelope,
            Err(err) => {
                warn!("skipping malformed Binance frame: {}", err);
                continue;
            }
        };

        if envelope.stream.ends_with("@bookTicker") {
            stats.book_ticker_messages += 1;
            let ticker: BookTickerEnvelope<'_> = serde_json::from_slice(&bytes)?;
            latest_book_ticker = Some(parse_book_ticker(&ticker.data)?);
            continue;
        }
        if !is_depth_stream(envelope.stream) {
            debug!("skipping non-depth stream: {}", envelope.stream);
            continue;
        }

        let recv_ts_ns = now_ns();
        let depth: DepthEnvelope<'_> = serde_json::from_slice(&bytes)?;
        let update = normalize_depth_update(depth.data, args.symbol_id, recv_ts_ns)?;

        if !snapshot_bridged {
            lane.buffer_depth_update(update);
            let snapshot = fetch_snapshot(&symbol, args.depth_levels).await?;
            let bridge = lane.apply_snapshot_bridge(
                snapshot.last_update_id,
                &snapshot.bids,
                &snapshot.asks,
                now_ns(),
            );
            write_batch(&mut replay_writer, &bridge.replay, &mut stats)?;
            if bridge.result.gap.is_some() {
                stats.rebuilds += 1;
                lane.start_buffering();
                continue;
            }
            snapshot_bridged = true;
            info!(
                "snapshot bridge live: applied={} ignored_stale={} last_update_id={}",
                bridge.result.applied,
                bridge.result.ignored_stale,
                lane.sync().last_update_id()
            );
            continue;
        }

        let raw_record_update = update.clone();
        let outcome = lane.process_depth_update_with_clock(
            &update,
            LatencyTrace {
                recv_ns: recv_ts_ns,
                parse_done_ns: now_ns(),
                ..LatencyTrace::default()
            },
            &mut now_ns,
        )?;

        stats.depth_messages += 1;
        histograms.record(outcome.latency);
        if outcome.signal.is_some() {
            stats.signals += 1;
        }
        if let Some(ticker) = latest_book_ticker {
            stats.book_ticker_checks += 1;
            if book_ticker_mismatch(&lane, ticker) {
                stats.book_ticker_mismatches += 1;
            }
        }

        let mut replay = outcome.replay;
        replay.raw = Some(raw_replay_record(
            args.symbol_id,
            &raw_record_update,
            outcome.latency,
            lane.sync().state(),
            lane.sync().last_update_id(),
            bytes,
        ));
        write_batch(&mut replay_writer, &replay, &mut stats)?;

        if lane.sync().state() == BookSyncState::RebuildRequired {
            stats.rebuilds += 1;
            let snapshot = fetch_snapshot(&symbol, args.depth_levels).await?;
            let replay = lane.apply_replay_snapshot(
                snapshot.last_update_id,
                &snapshot.bids,
                &snapshot.asks,
                now_ns(),
            );
            write_batch(&mut replay_writer, &replay, &mut stats)?;
        }

        if last_report.elapsed() >= Duration::from_secs(args.report_every_secs) {
            report_live(&stats, &histograms);
            last_report = Instant::now();
        }
    }

    if let Some(writer) = replay_writer.as_mut() {
        writer.flush()?;
    }
    report_live(&stats, &histograms);
    Ok(())
}

fn replay_file(path: &PathBuf, symbol_id: u32) -> Result<ReplaySummary, serde_json::Error> {
    let records = load_records(path)?;
    replay_records(&records, symbol_id)
}

fn replay_records(
    records: &[ReplayRecord],
    symbol_id: u32,
) -> Result<ReplaySummary, serde_json::Error> {
    let mut lane = MarketDataLane::<50>::new(symbol_id, SignalRules::default());
    let mut summary = ReplaySummary::default();
    let mut pending_feature: Option<FeatureSnapshot> = None;
    let mut pending_signal: Option<Signal> = None;

    for record in records {
        match &record.payload {
            ReplayPayload::Snapshot { bids, asks } => {
                lane.load_snapshot_for_replay(
                    record.final_update_id,
                    bids,
                    asks,
                    record.exchange_ts_ns,
                );
                summary.snapshots += 1;
            }
            ReplayPayload::Bridge {
                result,
                applied_updates,
            } => {
                if result.applied != applied_updates.len() {
                    summary.parity_mismatches += 1;
                }
                for bridge_update in applied_updates {
                    let update = ParsedDepthUpdate {
                        symbol_id: bridge_update.symbol_id,
                        exchange_ts_ns: bridge_update.exchange_ts_ns,
                        receive_ts_ns: bridge_update.receive_ts_ns,
                        first_update_id: bridge_update.first_update_id,
                        final_update_id: bridge_update.final_update_id,
                        bids: bridge_update.bids.clone(),
                        asks: bridge_update.asks.clone(),
                    };
                    if lane.apply_bridge_update_for_replay(&update) != SequenceDecision::Apply {
                        summary.parity_mismatches += 1;
                    }
                }
                if lane.sync().last_update_id() != record.last_book_update_id {
                    summary.parity_mismatches += 1;
                }
            }
            ReplayPayload::DepthUpdate {
                decision,
                bids,
                asks,
            } => {
                summary.depth_updates += 1;
                let update = ParsedDepthUpdate {
                    symbol_id: record.symbol_id,
                    exchange_ts_ns: record.exchange_ts_ns,
                    receive_ts_ns: record.receive_ts_ns,
                    first_update_id: record.first_update_id,
                    final_update_id: record.final_update_id,
                    bids: bids.clone(),
                    asks: asks.clone(),
                };
                let outcome = lane
                    .process_depth_update(&update, record.latency)
                    .map_err(serde_json::Error::custom)?;
                if outcome.decision != *decision {
                    summary.parity_mismatches += 1;
                }
                match outcome.decision {
                    SequenceDecision::Apply => summary.applied += 1,
                    SequenceDecision::IgnoreStale => summary.ignored += 1,
                    SequenceDecision::Gap { .. } | SequenceDecision::InvalidRange => {
                        summary.gaps += 1
                    }
                }
                if pending_feature.is_some() || pending_signal.is_some() {
                    summary.parity_mismatches += 1;
                }
                pending_feature = outcome.feature.map(|feature| feature.snapshot);
                if let Some(signal) = outcome.signal {
                    summary.signals += 1;
                    pending_signal = Some(signal);
                } else {
                    pending_signal = None;
                }
            }
            ReplayPayload::Feature(snapshot) => {
                if pending_feature.take() == Some(*snapshot) {
                    summary.features += 1;
                } else {
                    summary.parity_mismatches += 1;
                }
            }
            ReplayPayload::Signal(signal) => {
                if pending_signal.take() != Some(*signal) {
                    summary.parity_mismatches += 1;
                }
            }
            _ => {}
        }
    }

    if pending_feature.is_some() || pending_signal.is_some() {
        summary.parity_mismatches += 1;
    }
    summary.final_update_id = lane.sync().last_update_id();
    Ok(summary)
}

fn paper_file(
    path: &PathBuf,
    symbol_id: u32,
    args: &PaperArgs,
) -> Result<PaperSummary, serde_json::Error> {
    let records = load_records(path)?;
    paper_records(&records, symbol_id, args)
}

fn load_records(path: &PathBuf) -> Result<Vec<ReplayRecord>, serde_json::Error> {
    read_replay_records(BufReader::new(
        File::open(path).map_err(serde_json::Error::io)?,
    ))
}

fn paper_records(
    records: &[ReplayRecord],
    symbol_id: u32,
    args: &PaperArgs,
) -> Result<PaperSummary, serde_json::Error> {
    let mut lane = MarketDataLane::<50>::new(symbol_id, SignalRules::default());
    let mut summary = PaperSummary::default();
    let mut open: Option<(SignalSide, f64)> = None;
    let mut peak = 0.0_f64;

    for record in records {
        match &record.payload {
            ReplayPayload::Snapshot { bids, asks } => {
                lane.load_snapshot_for_replay(
                    record.final_update_id,
                    bids,
                    asks,
                    record.exchange_ts_ns,
                );
            }
            ReplayPayload::Bridge {
                applied_updates, ..
            } => {
                for bridge_update in applied_updates {
                    let update = ParsedDepthUpdate {
                        symbol_id: bridge_update.symbol_id,
                        exchange_ts_ns: bridge_update.exchange_ts_ns,
                        receive_ts_ns: bridge_update.receive_ts_ns,
                        first_update_id: bridge_update.first_update_id,
                        final_update_id: bridge_update.final_update_id,
                        bids: bridge_update.bids.clone(),
                        asks: bridge_update.asks.clone(),
                    };
                    lane.apply_bridge_update_for_replay(&update);
                }
            }
            ReplayPayload::DepthUpdate { bids, asks, .. } => {
                let update = ParsedDepthUpdate {
                    symbol_id: record.symbol_id,
                    exchange_ts_ns: record.exchange_ts_ns,
                    receive_ts_ns: record.receive_ts_ns,
                    first_update_id: record.first_update_id,
                    final_update_id: record.final_update_id,
                    bids: bids.clone(),
                    asks: asks.clone(),
                };
                let outcome = lane
                    .process_depth_update(&update, record.latency)
                    .map_err(serde_json::Error::custom)?;
                let Some(signal) = outcome.signal else {
                    continue;
                };
                let Some(mid) = lane
                    .book()
                    .best_bid()
                    .zip(lane.book().best_ask())
                    .map(|(b, a)| fixed_to_float((b.price + a.price) / 2))
                else {
                    continue;
                };

                if let Some((side, entry)) = open.take() {
                    let direction = match side {
                        SignalSide::Long => 1.0,
                        SignalSide::Short => -1.0,
                    };
                    let gross = ((mid - entry) / entry) * args.notional * direction;
                    let fees = args.notional * (args.fee_bps / 10_000.0) * 2.0;
                    let slippage = args.notional * (args.slippage_bps / 10_000.0) * 2.0;
                    summary.gross_pnl += gross;
                    summary.fees += fees;
                    summary.slippage += slippage;
                    summary.net_pnl += gross - fees - slippage;
                    summary.realized_trades += 1;
                    let equity = summary.net_pnl;
                    peak = peak.max(equity);
                    summary.max_drawdown = summary.max_drawdown.max(peak - equity);
                }

                summary.signals += 1;
                match signal.side {
                    SignalSide::Long => summary.longs += 1,
                    SignalSide::Short => summary.shorts += 1,
                }
                open = Some((signal.side, mid));
            }
            _ => {}
        }
    }

    Ok(summary)
}

async fn fetch_snapshot(
    symbol: &str,
    depth_levels: u16,
) -> Result<FixedSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    let client = BinanceRestClient::new();
    let snapshot = client
        .get_depth(&Symbol::new(symbol), Some(depth_levels))
        .await
        .map_err(|err| format!("Binance snapshot fetch failed: {err}"))?;
    Ok(FixedSnapshot {
        last_update_id: snapshot.last_update_id,
        bids: parse_levels(&snapshot.bids)?,
        asks: parse_levels(&snapshot.asks)?,
    })
}

struct FixedSnapshot {
    last_update_id: u64,
    bids: Vec<(i64, i64)>,
    asks: Vec<(i64, i64)>,
}

fn parse_levels(
    levels: &[[String; 2]],
) -> Result<Vec<(i64, i64)>, Box<dyn std::error::Error + Send + Sync>> {
    levels
        .iter()
        .map(|[price, qty]| Ok((parse_fixed_6(price)?, parse_fixed_6(qty)?)))
        .collect()
}

fn build_stream_url(base_url: &str, symbol: &str) -> String {
    let symbol = symbol.to_lowercase();
    format!(
        "{}?streams={}@depth@100ms/{}@bookTicker&timeUnit=MICROSECOND",
        base_url, symbol, symbol
    )
}

fn is_depth_stream(stream: &str) -> bool {
    stream.ends_with("@depth") || stream.ends_with("@depth@100ms")
}

fn now_ns() -> i64 {
    (now_micros() as i64) * 1_000
}

fn record_non_negative(histogram: &mut Histogram<u64>, value: i64) {
    if value >= 0 {
        let _ = histogram.record(value as u64);
    }
}

fn print_histogram(label: &str, stage: &str, histogram: &Histogram<u64>) {
    if histogram.len() == 0 {
        info!("{label} {stage}: no samples");
        return;
    }
    info!(
        "{label} {stage}: p50={}ns p95={}ns p99={}ns p999={}ns max={}ns count={}",
        histogram.value_at_quantile(0.50),
        histogram.value_at_quantile(0.95),
        histogram.value_at_quantile(0.99),
        histogram.value_at_quantile(0.999),
        histogram.max(),
        histogram.len()
    );
}

fn report_live(stats: &LiveStats, histograms: &LatencyHistograms) {
    info!(
        "live stats: depth={} bookTicker={} signals={} rebuilds={} bookTicker_mismatch={} replay_records={}",
        stats.depth_messages,
        stats.book_ticker_messages,
        stats.signals,
        stats.rebuilds,
        format_args!(
            "{}/{}",
            stats.book_ticker_mismatches, stats.book_ticker_checks
        ),
        stats.replay_records
    );
    histograms.print("live");
}

fn write_batch(
    writer: &mut Option<BufWriter<File>>,
    batch: &ReplayBatch,
    stats: &mut LiveStats,
) -> Result<(), serde_json::Error> {
    if let Some(writer) = writer {
        stats.replay_records += write_replay_batch(writer, batch)? as u64;
    }
    Ok(())
}

fn raw_replay_record(
    symbol_id: u32,
    update: &ParsedDepthUpdate,
    latency: LatencyTrace,
    sync_state: BookSyncState,
    last_book_update_id: u64,
    bytes: Vec<u8>,
) -> ReplayRecord {
    ReplayRecord {
        kind: ReplayKind::RawFrame,
        symbol_id,
        exchange_ts_ns: update.exchange_ts_ns,
        receive_ts_ns: update.receive_ts_ns,
        first_update_id: update.first_update_id,
        final_update_id: update.final_update_id,
        sync_state,
        last_book_update_id,
        latency,
        payload: ReplayPayload::RawFrame { bytes },
    }
}

fn book_ticker_mismatch<const N: usize>(
    lane: &MarketDataLane<N>,
    ticker: BookTickerSnapshot,
) -> bool {
    let Some(best_bid) = lane.book().best_bid() else {
        return false;
    };
    let Some(best_ask) = lane.book().best_ask() else {
        return false;
    };
    best_bid.price != ticker.best_bid_price || best_ask.price != ticker.best_ask_price
}

fn parse_book_ticker(
    ticker: &BookTicker<'_>,
) -> Result<BookTickerSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    Ok(BookTickerSnapshot {
        best_bid_price: parse_fixed_6(ticker.best_bid_price)?,
        best_ask_price: parse_fixed_6(ticker.best_ask_price)?,
    })
}

fn fixed_to_float(value: i64) -> f64 {
    value as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::binance_md::{ReplayPayload, Signal};

    #[test]
    fn replay_ndjson_round_trips_records() {
        let batch = ReplayBatch {
            signal: Some(ReplayRecord {
                kind: ReplayKind::Signal,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 1,
                receive_ts_ns: 2,
                first_update_id: 10,
                final_update_id: 10,
                sync_state: BookSyncState::Live,
                last_book_update_id: 10,
                latency: LatencyTrace {
                    recv_ns: 1,
                    parse_done_ns: 2,
                    book_done_ns: 3,
                    feature_done_ns: 4,
                    signal_done_ns: 5,
                },
                payload: ReplayPayload::Signal(Signal {
                    symbol_id: BTCUSDT_ID,
                    side: SignalSide::Long,
                    confidence_fp: 1,
                    edge_bps: 0,
                    ts_ns: 2,
                    expire_ts_ns: 3,
                }),
            }),
            ..ReplayBatch::default()
        };
        let mut bytes = Vec::new();

        let written = write_replay_batch(&mut bytes, &batch).unwrap();
        let records = read_replay_records(BufReader::new(bytes.as_slice())).unwrap();

        assert_eq!(written, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0], batch.signal.unwrap());
    }

    #[test]
    fn replay_summary_reconstructs_snapshot_and_depth() {
        let records = vec![
            ReplayRecord {
                kind: ReplayKind::Snapshot,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 1,
                receive_ts_ns: 1,
                first_update_id: 99,
                final_update_id: 99,
                sync_state: BookSyncState::Live,
                last_book_update_id: 99,
                latency: LatencyTrace::default(),
                payload: ReplayPayload::Snapshot {
                    bids: vec![(100_000_000, 100_000)],
                    asks: vec![(102_000_000, 100_000)],
                },
            },
            ReplayRecord {
                kind: ReplayKind::DepthUpdate,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 2,
                receive_ts_ns: 2,
                first_update_id: 100,
                final_update_id: 100,
                sync_state: BookSyncState::Live,
                last_book_update_id: 100,
                latency: LatencyTrace {
                    recv_ns: 2,
                    parse_done_ns: 3,
                    book_done_ns: 4,
                    feature_done_ns: 5,
                    signal_done_ns: 6,
                },
                payload: ReplayPayload::DepthUpdate {
                    decision: SequenceDecision::Apply,
                    bids: vec![(100_000_000, 200)],
                    asks: vec![(102_000_000, 50)],
                },
            },
            ReplayRecord {
                kind: ReplayKind::Feature,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 2,
                receive_ts_ns: 2,
                first_update_id: 100,
                final_update_id: 100,
                sync_state: BookSyncState::Live,
                last_book_update_id: 100,
                latency: LatencyTrace {
                    recv_ns: 2,
                    parse_done_ns: 3,
                    book_done_ns: 4,
                    feature_done_ns: 5,
                    signal_done_ns: 6,
                },
                payload: ReplayPayload::Feature(FeatureSnapshot {
                    symbol_id: BTCUSDT_ID,
                    mid: 101_000_000,
                    spread: 2_000_000,
                    obi1_fp: 600_000,
                    obi3_fp: 600_000,
                    microgap: 600_000,
                    flow_1s: 0,
                    book_staleness_ns: 0,
                    ts_ns: 2,
                }),
            },
            ReplayRecord {
                kind: ReplayKind::Signal,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 2,
                receive_ts_ns: 2,
                first_update_id: 100,
                final_update_id: 100,
                sync_state: BookSyncState::Live,
                last_book_update_id: 100,
                latency: LatencyTrace {
                    recv_ns: 2,
                    parse_done_ns: 3,
                    book_done_ns: 4,
                    feature_done_ns: 5,
                    signal_done_ns: 6,
                },
                payload: ReplayPayload::Signal(Signal {
                    symbol_id: BTCUSDT_ID,
                    side: SignalSide::Long,
                    confidence_fp: 600_000,
                    edge_bps: 0,
                    ts_ns: 2,
                    expire_ts_ns: 50_000_002,
                }),
            },
        ];

        let summary = replay_records(&records, BTCUSDT_ID).unwrap();

        assert_eq!(summary.snapshots, 1);
        assert_eq!(summary.depth_updates, 1);
        assert_eq!(summary.applied, 1);
        assert_eq!(summary.features, 1);
        assert_eq!(summary.signals, 1);
        assert_eq!(summary.parity_mismatches, 0);
        assert_eq!(summary.final_update_id, 100);
    }

    #[test]
    fn replay_summary_reconstructs_bridge_applied_updates() {
        let records = vec![
            ReplayRecord {
                kind: ReplayKind::Snapshot,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 1,
                receive_ts_ns: 1,
                first_update_id: 99,
                final_update_id: 99,
                sync_state: BookSyncState::Live,
                last_book_update_id: 101,
                latency: LatencyTrace::default(),
                payload: ReplayPayload::Snapshot {
                    bids: vec![(100_000_000, 100_000)],
                    asks: vec![(102_000_000, 100_000)],
                },
            },
            ReplayRecord {
                kind: ReplayKind::Bridge,
                symbol_id: BTCUSDT_ID,
                exchange_ts_ns: 1,
                receive_ts_ns: 1,
                first_update_id: 99,
                final_update_id: 99,
                sync_state: BookSyncState::Live,
                last_book_update_id: 101,
                latency: LatencyTrace::default(),
                payload: ReplayPayload::Bridge {
                    result: engine::binance_md::BufferedApplyResult {
                        applied: 1,
                        ignored_stale: 0,
                        gap: None,
                        decisions: Vec::new(),
                    },
                    applied_updates: vec![engine::binance_md::ReplayBridgeUpdate {
                        symbol_id: BTCUSDT_ID,
                        exchange_ts_ns: 2,
                        receive_ts_ns: 2,
                        first_update_id: 100,
                        final_update_id: 101,
                        bids: vec![(101_000_000, 200_000)],
                        asks: vec![],
                    }],
                },
            },
        ];

        let summary = replay_records(&records, BTCUSDT_ID).unwrap();

        assert_eq!(summary.snapshots, 1);
        assert_eq!(summary.depth_updates, 0);
        assert_eq!(summary.parity_mismatches, 0);
        assert_eq!(summary.final_update_id, 101);
    }

    #[test]
    fn stream_url_uses_fast_depth_and_microsecond_time_unit() {
        let url = build_stream_url(BINANCE_SPOT_WS, "BTCUSDT");

        assert_eq!(
            url,
            "wss://stream.binance.com:9443/stream?streams=btcusdt@depth@100ms/btcusdt@bookTicker&timeUnit=MICROSECOND"
        );
    }

    #[test]
    fn recognizes_default_and_fast_depth_stream_names() {
        assert!(is_depth_stream("btcusdt@depth"));
        assert!(is_depth_stream("btcusdt@depth@100ms"));
        assert!(!is_depth_stream("btcusdt@bookTicker"));
    }
}
