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
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

const BINANCE_SPOT_WS: &str = "wss://stream.binance.com:9443/stream";
const BTCUSDT_ID: u32 = 1;
const LATENCY_HISTOGRAM_MAX_NS: u64 = 120_000_000_000;
const MIN_P999_SAMPLES: u64 = 10_000;

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
    /// Write benchmark-grade latency evidence after a bounded live run.
    #[arg(long, requires = "latency_context_in")]
    latency_report_out: Option<PathBuf>,
    /// JSON benchmark context containing build, host, clock, and cohort provenance.
    #[arg(long)]
    latency_context_in: Option<PathBuf>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Depth,
    BookTicker,
    Other,
}

struct EvidenceHistogram {
    histogram: Histogram<u64>,
    excluded_negative: u64,
    excluded_overflow: u64,
}

impl EvidenceHistogram {
    fn new() -> Self {
        Self {
            histogram: Histogram::new_with_max(LATENCY_HISTOGRAM_MAX_NS, 3)
                .expect("valid latency evidence histogram"),
            excluded_negative: 0,
            excluded_overflow: 0,
        }
    }

    fn record(&mut self, value: i64) {
        if value < 0 {
            self.excluded_negative += 1;
        } else if value as u64 > LATENCY_HISTOGRAM_MAX_NS
            || self.histogram.record(value as u64).is_err()
        {
            self.excluded_overflow += 1;
        }
    }

    fn report(&self, stage: &'static str, benchmark_context_ready: bool) -> LatencyEvidenceStage {
        let count = self.histogram.len();
        let p999_sample_sufficient = count >= MIN_P999_SAMPLES;
        LatencyEvidenceStage {
            stage,
            count,
            p50_ns: self.histogram.value_at_quantile(0.50),
            p95_ns: self.histogram.value_at_quantile(0.95),
            p99_ns: self.histogram.value_at_quantile(0.99),
            p999_ns: self.histogram.value_at_quantile(0.999),
            max_ns: self.histogram.max(),
            p999_sample_status: if p999_sample_sufficient {
                P999SampleStatus::Sufficient
            } else {
                P999SampleStatus::InsufficientSample
            },
            benchmark_gate_eligible: p999_sample_sufficient
                && benchmark_context_ready
                && self.excluded_negative == 0
                && self.excluded_overflow == 0,
            exclusions: LatencyExclusions {
                negative_duration: self.excluded_negative,
                above_histogram_range: self.excluded_overflow,
            },
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum P999SampleStatus {
    Sufficient,
    InsufficientSample,
}

#[derive(Debug, Serialize)]
struct LatencyExclusions {
    negative_duration: u64,
    above_histogram_range: u64,
}

#[derive(Debug, Serialize)]
struct LatencyEvidenceStage {
    stage: &'static str,
    count: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    p999_ns: u64,
    max_ns: u64,
    p999_sample_status: P999SampleStatus,
    benchmark_gate_eligible: bool,
    exclusions: LatencyExclusions,
}

#[derive(Serialize)]
struct LatencyEvidenceArtifact<'a> {
    schema_version: u8,
    evidence_kind: &'static str,
    generated_at_micros: u64,
    capture_started_at_micros: u64,
    capture_ended_at_micros: u64,
    minimum_p999_samples: u64,
    benchmark_context: &'a BenchmarkContext,
    capture_provenance: CaptureProvenance<'a>,
    capture_exclusions: CaptureExclusions,
    stages: [LatencyEvidenceStage; 5],
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkContext {
    git_sha: String,
    build_profile: String,
    feature_flags: Vec<String>,
    host_identity: String,
    instance_type: String,
    availability_zone: String,
    cpu_affinity: String,
    nic_irq_settings: String,
    clock_source: String,
    clock_offset_ns: i64,
    clock_max_error_ns: u64,
    clock_synchronized: bool,
    cohort_id: String,
    cohort_role: String,
    comparison_cohort_id: String,
    comparison_design: String,
}

impl BenchmarkContext {
    fn validate(&self) -> Result<(), &'static str> {
        let required = [
            self.git_sha.as_str(),
            self.build_profile.as_str(),
            self.host_identity.as_str(),
            self.instance_type.as_str(),
            self.availability_zone.as_str(),
            self.cpu_affinity.as_str(),
            self.nic_irq_settings.as_str(),
            self.clock_source.as_str(),
            self.cohort_id.as_str(),
            self.cohort_role.as_str(),
            self.comparison_cohort_id.as_str(),
            self.comparison_design.as_str(),
        ];
        if required.iter().any(|value| value.trim().is_empty()) {
            return Err("benchmark context fields must be non-empty");
        }
        if self.feature_flags.is_empty()
            || self
                .feature_flags
                .iter()
                .any(|value| value.trim().is_empty())
        {
            return Err("benchmark feature_flags must be non-empty");
        }
        if !(7..=40).contains(&self.git_sha.len())
            || !self.git_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("benchmark git_sha must be a 7-40 character hexadecimal SHA");
        }
        if self.clock_max_error_ns == 0 {
            return Err("benchmark clock_max_error_ns must be positive");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct CaptureProvenance<'a> {
    venue: &'static str,
    symbol: &'a str,
    websocket_endpoint: &'a str,
    protocol: &'static str,
    streams: &'static str,
    receive_boundary: &'static str,
    clock: &'static str,
    unit: &'static str,
    eligible_observation: &'static str,
    artifact_emission_policy: &'static str,
    span_capture_locations: [SpanCaptureLocation; 5],
}

#[derive(Serialize)]
struct SpanCaptureLocation {
    stage: &'static str,
    producer: &'static str,
    start: &'static str,
    end: &'static str,
}

#[derive(Serialize)]
struct CaptureExclusions {
    warmup_duration_micros: u64,
    warmup_depth_updates: u64,
    post_bridge_non_apply_updates: u64,
    reconnects: u64,
    sequence_gaps: u64,
    queue_overflows: u64,
    parser_failures: u64,
}

struct LatencyHistograms {
    parse: EvidenceHistogram,
    book: EvidenceHistogram,
    feature: EvidenceHistogram,
    signal: EvidenceHistogram,
    total: EvidenceHistogram,
}

impl LatencyHistograms {
    fn new() -> Self {
        Self {
            parse: EvidenceHistogram::new(),
            book: EvidenceHistogram::new(),
            feature: EvidenceHistogram::new(),
            signal: EvidenceHistogram::new(),
            total: EvidenceHistogram::new(),
        }
    }

    fn record(&mut self, latency: LatencyTrace) {
        self.parse.record(latency.parse_latency_ns());
        self.book.record(latency.book_latency_ns());
        self.feature.record(latency.feature_latency_ns());
        self.signal.record(latency.signal_latency_ns());
        self.total.record(latency.total_latency_ns());
    }

    fn print(&self, label: &str) {
        print_histogram(label, "parse", &self.parse);
        print_histogram(label, "book", &self.book);
        print_histogram(label, "feature", &self.feature);
        print_histogram(label, "signal", &self.signal);
        print_histogram(label, "total", &self.total);
    }

    fn reports(&self, benchmark_context_ready: bool) -> [LatencyEvidenceStage; 5] {
        [
            self.parse.report("parse", benchmark_context_ready),
            self.book.report("book", benchmark_context_ready),
            self.feature.report("feature", benchmark_context_ready),
            self.signal.report("signal", benchmark_context_ready),
            self.total.report("total", benchmark_context_ready),
        ]
    }
}

fn record_evidence_sample(
    histograms: &mut LatencyHistograms,
    stats: &mut LiveStats,
    latency: LatencyTrace,
    decision: SequenceDecision,
) {
    if decision != SequenceDecision::Apply {
        stats.evidence_non_apply_exclusions += 1;
    } else {
        histograms.record(latency);
    }
}

struct LiveStats {
    depth_messages: u64,
    warmup_depth_updates: u64,
    warmup_duration_micros: Option<u64>,
    evidence_non_apply_exclusions: u64,
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
            warmup_depth_updates: 0,
            warmup_duration_micros: None,
            evidence_non_apply_exclusions: 0,
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
    let benchmark_context = prepare_benchmark_context(
        args.latency_report_out.as_ref(),
        args.latency_context_in.as_ref(),
    )?;
    let mut lane = MarketDataLane::<50>::new(args.symbol_id, SignalRules::default());
    let mut histograms = LatencyHistograms::new();
    let mut stats = LiveStats::new();
    let mut replay_writer = match &args.replay_out {
        Some(path) => Some(BufWriter::new(File::create(path)?)),
        None => None,
    };
    let record_replay = replay_writer.is_some();

    lane.start_buffering();
    let ws_url = build_stream_url(&args.ws_base_url, &symbol);
    info!("connecting Binance stream: {}", ws_url);
    let (mut ws, _) = connect_async(&ws_url).await?;
    info!("connected; buffering first diff before REST snapshot");

    let start = Instant::now();
    let capture_started_at_micros = now_micros();
    let mut last_report = Instant::now();
    let mut snapshot_bridged = false;
    let mut bounded_capture_complete = false;
    let mut latest_book_ticker: Option<BookTickerSnapshot> = None;

    loop {
        if args
            .max_runtime_secs
            .is_some_and(|secs| start.elapsed() >= Duration::from_secs(secs))
        {
            bounded_capture_complete = true;
            break;
        }
        if args
            .max_messages
            .is_some_and(|max| stats.depth_messages >= max)
        {
            bounded_capture_complete = true;
            break;
        }

        let remaining = remaining_runtime(&start, args.max_runtime_secs);
        let next_message = match await_with_optional_timeout(ws.next(), remaining).await {
            Ok(message) => message,
            Err(()) => {
                bounded_capture_complete = true;
                break;
            }
        };
        let Some(message) = next_message else {
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
        let recv_ts_ns = now_ns();
        let recv_latency_ns = elapsed_ns(&start);

        match classify_stream(&bytes) {
            StreamKind::BookTicker => {
                stats.book_ticker_messages += 1;
                let ticker: BookTickerEnvelope<'_> = serde_json::from_slice(&bytes)?;
                latest_book_ticker = Some(parse_book_ticker(&ticker.data)?);
                continue;
            }
            StreamKind::Depth => {}
            StreamKind::Other => {
                debug!("skipping non-depth Binance frame");
                continue;
            }
        }

        let depth: DepthEnvelope<'_> = serde_json::from_slice(&bytes)?;
        let update = normalize_depth_update(depth.data, args.symbol_id, recv_ts_ns)?;

        if !snapshot_bridged {
            stats.warmup_depth_updates += 1;
            lane.buffer_depth_update(update);
            let Some(snapshot) = fetch_snapshot_within_runtime(
                &symbol,
                args.depth_levels,
                remaining_runtime(&start, args.max_runtime_secs),
            )
            .await?
            else {
                break;
            };
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
            stats.warmup_duration_micros = Some(start.elapsed().as_micros() as u64);
            info!(
                "snapshot bridge live: applied={} ignored_stale={} last_update_id={}",
                bridge.result.applied,
                bridge.result.ignored_stale,
                lane.sync().last_update_id()
            );
            continue;
        }

        let latency = LatencyTrace {
            recv_ns: recv_latency_ns,
            parse_done_ns: elapsed_ns(&start),
            ..LatencyTrace::default()
        };
        let outcome = if record_replay {
            let raw_record_update = update.clone();
            let mut outcome =
                lane.process_depth_update_with_clock(&update, latency, &mut || elapsed_ns(&start))?;
            outcome.replay.raw = Some(raw_replay_record(
                args.symbol_id,
                &raw_record_update,
                outcome.latency,
                lane.sync().state(),
                lane.sync().last_update_id(),
                bytes,
            ));
            Some(outcome)
        } else {
            let outcome = lane
                .process_depth_update_fast_with_clock(&update, latency, &mut || elapsed_ns(&start));
            stats.depth_messages += 1;
            if outcome.signal.is_some() {
                stats.signals += 1;
            }
            if let Some(ticker) = latest_book_ticker {
                stats.book_ticker_checks += 1;
                if book_ticker_mismatch(&lane, ticker) {
                    stats.book_ticker_mismatches += 1;
                }
            }
            let sequence_gap = lane.sync().state() == BookSyncState::RebuildRequired;
            record_evidence_sample(
                &mut histograms,
                &mut stats,
                outcome.latency,
                outcome.decision,
            );
            if sequence_gap {
                stats.rebuilds += 1;
                let Some(snapshot) = fetch_snapshot_within_runtime(
                    &symbol,
                    args.depth_levels,
                    remaining_runtime(&start, args.max_runtime_secs),
                )
                .await?
                else {
                    break;
                };
                let replay = lane.apply_replay_snapshot(
                    snapshot.last_update_id,
                    &snapshot.bids,
                    &snapshot.asks,
                    now_ns(),
                );
                write_batch(&mut replay_writer, &replay, &mut stats)?;
            }
            continue;
        };
        let outcome = outcome.expect("record_replay path produces process outcome");

        stats.depth_messages += 1;
        if outcome.signal.is_some() {
            stats.signals += 1;
        }
        if let Some(ticker) = latest_book_ticker {
            stats.book_ticker_checks += 1;
            if book_ticker_mismatch(&lane, ticker) {
                stats.book_ticker_mismatches += 1;
            }
        }

        write_batch(&mut replay_writer, &outcome.replay, &mut stats)?;

        let sequence_gap = lane.sync().state() == BookSyncState::RebuildRequired;
        record_evidence_sample(
            &mut histograms,
            &mut stats,
            outcome.latency,
            outcome.decision,
        );
        if sequence_gap {
            stats.rebuilds += 1;
            let Some(snapshot) = fetch_snapshot_within_runtime(
                &symbol,
                args.depth_levels,
                remaining_runtime(&start, args.max_runtime_secs),
            )
            .await?
            else {
                break;
            };
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
    if let Some(path) = &args.latency_report_out {
        write_latency_evidence_if_complete(
            bounded_capture_complete && snapshot_bridged,
            path,
            &symbol,
            &ws_url,
            benchmark_context
                .as_ref()
                .expect("latency report context validated above"),
            capture_started_at_micros,
            now_micros(),
            &stats,
            &histograms,
        )?;
    }
    Ok(())
}

fn load_benchmark_context(
    path: &PathBuf,
) -> Result<BenchmarkContext, Box<dyn std::error::Error + Send + Sync>> {
    let context: BenchmarkContext = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    context
        .validate()
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
    Ok(context)
}

fn prepare_benchmark_context(
    report_path: Option<&PathBuf>,
    context_path: Option<&PathBuf>,
) -> Result<Option<BenchmarkContext>, Box<dyn std::error::Error + Send + Sync>> {
    if let (Some(report), Some(context)) = (report_path, context_path) {
        let aliases = report == context
            || matches!(
                (std::fs::canonicalize(report), std::fs::canonicalize(context)),
                (Ok(report), Ok(context)) if report == context
            );
        if aliases {
            return Err("latency report and context paths must be distinct".into());
        }
    }
    if let Some(path) = report_path {
        invalidate_latency_evidence(path)?;
    }
    match (report_path, context_path) {
        (Some(_), Some(path)) => Ok(Some(load_benchmark_context(path)?)),
        (Some(_), None) => Err("--latency-report-out requires --latency-context-in".into()),
        (None, Some(_)) => Err("--latency-context-in requires --latency-report-out".into()),
        (None, None) => Ok(None),
    }
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

async fn fetch_snapshot_within_runtime(
    symbol: &str,
    depth_levels: u16,
    remaining: Option<Duration>,
) -> Result<Option<FixedSnapshot>, Box<dyn std::error::Error + Send + Sync>> {
    match await_with_optional_timeout(fetch_snapshot(symbol, depth_levels), remaining).await {
        Ok(snapshot) => snapshot.map(Some),
        Err(()) => Ok(None),
    }
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

fn classify_stream(raw: &[u8]) -> StreamKind {
    if contains_bytes(raw, b"@depth") {
        StreamKind::Depth
    } else if contains_bytes(raw, b"@bookTicker") {
        StreamKind::BookTicker
    } else {
        StreamKind::Other
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn now_ns() -> i64 {
    (now_micros() as i64) * 1_000
}

fn elapsed_ns(origin: &Instant) -> i64 {
    origin.elapsed().as_nanos() as i64
}

async fn await_with_optional_timeout<F: std::future::Future>(
    future: F,
    timeout: Option<Duration>,
) -> Result<F::Output, ()> {
    match timeout {
        Some(timeout) if timeout.is_zero() => Err(()),
        Some(timeout) => tokio::time::timeout(timeout, future).await.map_err(|_| ()),
        None => Ok(future.await),
    }
}

fn remaining_runtime(start: &Instant, max_runtime_secs: Option<u64>) -> Option<Duration> {
    max_runtime_secs.map(|secs| Duration::from_secs(secs).saturating_sub(start.elapsed()))
}

fn print_histogram(label: &str, stage: &'static str, histogram: &EvidenceHistogram) {
    let report = histogram.report(stage, false);
    if report.count == 0 {
        info!("{label} {stage}: no samples");
        return;
    }
    info!(
        "{label} {stage}: p50={}ns p95={}ns p99={}ns p999={}ns max={}ns count={} p999_sample_status={:?} gate_eligible={} excluded_negative={} excluded_overflow={}",
        report.p50_ns,
        report.p95_ns,
        report.p99_ns,
        report.p999_ns,
        report.max_ns,
        report.count,
        report.p999_sample_status,
        report.benchmark_gate_eligible,
        report.exclusions.negative_duration,
        report.exclusions.above_histogram_range
    );
}

fn write_latency_evidence(
    path: &PathBuf,
    symbol: &str,
    websocket_endpoint: &str,
    benchmark_context: &BenchmarkContext,
    capture_started_at_micros: u64,
    capture_ended_at_micros: u64,
    stats: &LiveStats,
    histograms: &LatencyHistograms,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let artifact = LatencyEvidenceArtifact {
        schema_version: 1,
        evidence_kind: "benchmark",
        generated_at_micros: now_micros(),
        capture_started_at_micros,
        capture_ended_at_micros,
        minimum_p999_samples: MIN_P999_SAMPLES,
        benchmark_context,
        capture_provenance: CaptureProvenance {
            venue: "binance_spot",
            symbol,
            websocket_endpoint,
            protocol: "websocket_tls",
            streams: "diff_depth_100ms_and_book_ticker",
            receive_boundary: "after_websocket_message_to_owned_bytes",
            clock: "std_time_instant_monotonic",
            unit: "nanoseconds",
            eligible_observation: "post_bridge_applied_depth_update",
            artifact_emission_policy: "bounded_capture_success_only",
            span_capture_locations: [
                SpanCaptureLocation {
                    stage: "parse",
                    producer: "std_time_instant_monotonic",
                    start: "after_websocket_message_to_owned_bytes",
                    end: "after_depth_json_normalization",
                },
                SpanCaptureLocation {
                    stage: "book",
                    producer: "std_time_instant_monotonic",
                    start: "after_depth_json_normalization",
                    end: "after_canonical_book_apply",
                },
                SpanCaptureLocation {
                    stage: "feature",
                    producer: "std_time_instant_monotonic",
                    start: "after_canonical_book_apply",
                    end: "after_feature_computation",
                },
                SpanCaptureLocation {
                    stage: "signal",
                    producer: "std_time_instant_monotonic",
                    start: "after_feature_computation",
                    end: "after_signal_evaluation",
                },
                SpanCaptureLocation {
                    stage: "total",
                    producer: "std_time_instant_monotonic",
                    start: "after_websocket_message_to_owned_bytes",
                    end: "after_signal_evaluation",
                },
            ],
        },
        capture_exclusions: CaptureExclusions {
            warmup_duration_micros: stats
                .warmup_duration_micros
                .expect("completed capture must bridge a snapshot"),
            warmup_depth_updates: stats.warmup_depth_updates,
            post_bridge_non_apply_updates: stats.evidence_non_apply_exclusions,
            reconnects: 0,
            sequence_gaps: stats.rebuilds,
            queue_overflows: 0,
            parser_failures: 0,
        },
        // Context is operator-supplied provenance, not runtime attestation.
        stages: histograms.reports(false),
    };
    let temporary_path = temporary_evidence_path(path);
    let publish_result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, &artifact)?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        drop(writer);
        std::fs::rename(&temporary_path, path)?;
        Ok(())
    })();
    if publish_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    publish_result?;
    info!("wrote latency evidence: {}", path.display());
    Ok(())
}

fn temporary_evidence_path(path: &PathBuf) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("latency-evidence.json");
    path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        now_micros()
    ))
}

fn write_latency_evidence_if_complete(
    bounded_capture_complete: bool,
    path: &PathBuf,
    symbol: &str,
    websocket_endpoint: &str,
    benchmark_context: &BenchmarkContext,
    capture_started_at_micros: u64,
    capture_ended_at_micros: u64,
    stats: &LiveStats,
    histograms: &LatencyHistograms,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if !bounded_capture_complete {
        invalidate_latency_evidence(path)?;
        warn!(
            "latency evidence not written because the capture did not reach its configured bound"
        );
        return Ok(false);
    }
    write_latency_evidence(
        path,
        symbol,
        websocket_endpoint,
        benchmark_context,
        capture_started_at_micros,
        capture_ended_at_micros,
        stats,
        histograms,
    )?;
    Ok(true)
}

fn invalidate_latency_evidence(path: &PathBuf) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn report_live(stats: &LiveStats, histograms: &LatencyHistograms) {
    info!(
        "live stats: depth={} warmup_depth_excluded={} non_apply_excluded={} bookTicker={} signals={} rebuilds={} bookTicker_mismatch={} replay_records={}",
        stats.depth_messages,
        stats.warmup_depth_updates,
        stats.evidence_non_apply_exclusions,
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
    fn classifies_combined_stream_names_without_json_parsing() {
        assert_eq!(
            classify_stream(br#"{"stream":"btcusdt@depth","data":{}}"#),
            StreamKind::Depth
        );
        assert_eq!(
            classify_stream(br#"{"stream":"btcusdt@depth@100ms","data":{}}"#),
            StreamKind::Depth
        );
        assert_eq!(
            classify_stream(br#"{"stream":"btcusdt@bookTicker","data":{}}"#),
            StreamKind::BookTicker
        );
        assert_eq!(
            classify_stream(br#"{"stream":"btcusdt@trade","data":{}}"#),
            StreamKind::Other
        );
    }

    fn benchmark_context() -> BenchmarkContext {
        BenchmarkContext {
            git_sha: "0123456789abcdef0123456789abcdef01234567".to_string(),
            build_profile: "release".to_string(),
            feature_flags: vec!["default".to_string()],
            host_identity: "i-test".to_string(),
            instance_type: "c7i.xlarge".to_string(),
            availability_zone: "ap-northeast-1a".to_string(),
            cpu_affinity: "2-3".to_string(),
            nic_irq_settings: "irqbalance-disabled;rx-irq=2".to_string(),
            clock_source: "chrony".to_string(),
            clock_offset_ns: 10,
            clock_max_error_ns: 1_000,
            clock_synchronized: true,
            cohort_id: "after-change".to_string(),
            cohort_role: "treatment".to_string(),
            comparison_cohort_id: "before-change".to_string(),
            comparison_design: "adjacent-window".to_string(),
        }
    }

    #[test]
    fn benchmark_context_must_be_complete_before_gating() {
        let mut context = benchmark_context();
        assert!(context.validate().is_ok());

        context.host_identity.clear();
        assert!(context.validate().is_err());
    }

    #[test]
    fn evidence_report_separates_tail_sufficiency_and_exclusions() {
        let mut sufficient = EvidenceHistogram::new();
        for _ in 0..9_989 {
            sufficient.record(100);
        }
        for _ in 0..11 {
            sufficient.record(100_000);
        }

        let sufficient_report = sufficient.report("total", true);
        assert_eq!(sufficient_report.count, 10_000);
        assert!(sufficient_report.p99_ns < sufficient_report.p999_ns);
        assert_eq!(
            sufficient_report.p999_sample_status,
            P999SampleStatus::Sufficient
        );
        assert!(sufficient_report.benchmark_gate_eligible);

        let mut insufficient = EvidenceHistogram::new();
        for _ in 0..9_999 {
            insufficient.record(100);
        }
        insufficient.record(-1);
        insufficient.record(LATENCY_HISTOGRAM_MAX_NS as i64 + 1);

        let insufficient_report = insufficient.report("parse", true);
        assert_eq!(insufficient_report.count, 9_999);
        assert_eq!(
            insufficient_report.p999_sample_status,
            P999SampleStatus::InsufficientSample
        );
        assert!(!insufficient_report.benchmark_gate_eligible);
        assert_eq!(insufficient_report.exclusions.negative_duration, 1);
        assert_eq!(insufficient_report.exclusions.above_histogram_range, 1);

        let mut excluded_tail = EvidenceHistogram::new();
        for _ in 0..10_000 {
            excluded_tail.record(100);
        }
        excluded_tail.record(-1);
        excluded_tail.record(LATENCY_HISTOGRAM_MAX_NS as i64 + 1);
        let excluded_report = excluded_tail.report("parse", true);
        assert_eq!(excluded_report.count, 10_000);
        assert!(!excluded_report.benchmark_gate_eligible);
    }

    #[test]
    fn invalid_context_removes_stale_evidence_before_returning() {
        let id = format!("{}-{}", std::process::id(), now_micros());
        let report_path = std::env::temp_dir().join(format!("hft-latency-report-{id}.json"));
        let context_path = std::env::temp_dir().join(format!("hft-latency-context-{id}.json"));
        std::fs::write(&report_path, b"stale evidence").unwrap();
        std::fs::write(&context_path, b"not-json").unwrap();

        let result = prepare_benchmark_context(Some(&report_path), Some(&context_path));
        let _ = std::fs::remove_file(context_path);

        assert!(result.is_err());
        assert!(!report_path.exists());
    }

    #[test]
    fn aliased_report_and_context_path_is_rejected_without_deletion() {
        let path = std::env::temp_dir().join(format!(
            "hft-latency-aliased-path-{}-{}.json",
            std::process::id(),
            now_micros()
        ));
        std::fs::write(&path, serde_json::to_vec(&benchmark_context()).unwrap()).unwrap();

        let result = prepare_benchmark_context(Some(&path), Some(&path));
        let still_exists = path.exists();
        std::fs::remove_file(path).unwrap();

        assert!(result.is_err());
        assert!(still_exists);
    }

    #[test]
    fn non_apply_decisions_are_excluded_from_evidence_samples() {
        let mut histograms = LatencyHistograms::new();
        let mut stats = LiveStats::new();

        record_evidence_sample(
            &mut histograms,
            &mut stats,
            LatencyTrace::default(),
            SequenceDecision::IgnoreStale,
        );

        assert_eq!(histograms.total.report("total", true).count, 0);
        assert_eq!(stats.evidence_non_apply_exclusions, 1);

        record_evidence_sample(
            &mut histograms,
            &mut stats,
            LatencyTrace::default(),
            SequenceDecision::Apply,
        );
        assert_eq!(histograms.total.report("total", true).count, 1);
    }

    #[test]
    fn incomplete_capture_does_not_write_evidence() {
        let path = std::env::temp_dir().join(format!(
            "hft-latency-evidence-{}-{}.json",
            std::process::id(),
            now_micros()
        ));
        std::fs::write(&path, b"stale evidence").unwrap();
        let written = write_latency_evidence_if_complete(
            false,
            &path,
            "BTCUSDT",
            BINANCE_SPOT_WS,
            &benchmark_context(),
            1,
            2,
            &LiveStats::new(),
            &LatencyHistograms::new(),
        )
        .unwrap();
        let exists = path.exists();
        if exists {
            let _ = std::fs::remove_file(&path);
        }

        assert!(!written);
        assert!(!exists);
    }

    #[test]
    fn complete_capture_publishes_one_atomic_benchmark_artifact() {
        let path = std::env::temp_dir().join(format!(
            "hft-latency-evidence-complete-{}-{}.json",
            std::process::id(),
            now_micros()
        ));
        let mut stats = LiveStats::new();
        stats.warmup_duration_micros = Some(500);
        let written = write_latency_evidence_if_complete(
            true,
            &path,
            "BTCUSDT",
            BINANCE_SPOT_WS,
            &benchmark_context(),
            1,
            2,
            &stats,
            &LatencyHistograms::new(),
        )
        .unwrap();
        let artifact: serde_json::Value =
            serde_json::from_reader(File::open(&path).unwrap()).unwrap();
        assert!(written);
        assert_eq!(artifact["evidence_kind"], "benchmark");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn failed_atomic_publish_leaves_no_partial_file() {
        let root = std::env::temp_dir().join(format!(
            "hft-latency-evidence-failure-{}-{}",
            std::process::id(),
            now_micros()
        ));
        let path = root.join("report.json");
        std::fs::create_dir_all(&path).unwrap();

        let result = write_latency_evidence(
            &path,
            "BTCUSDT",
            BINANCE_SPOT_WS,
            &benchmark_context(),
            1,
            2,
            &LiveStats {
                warmup_duration_micros: Some(1),
                ..LiveStats::new()
            },
            &LatencyHistograms::new(),
        );
        let entries: Vec<_> = std::fs::read_dir(&root).unwrap().collect();
        std::fs::remove_dir_all(&root).unwrap();

        assert!(result.is_err());
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn runtime_bound_interrupts_a_stalled_receive() {
        let result = await_with_optional_timeout(
            std::future::pending::<()>(),
            Some(Duration::from_millis(1)),
        )
        .await;

        assert_eq!(result, Err(()));
    }
}
