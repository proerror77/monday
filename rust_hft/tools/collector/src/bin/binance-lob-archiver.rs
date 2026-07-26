use anyhow::{bail, Context};
use clap::Parser;
use data::binance_market_tape::{
    AggregateTrade, AggregateTradeSequenceValidator, DepthSourceClock,
    DepthSourceClockSequenceValidator,
};
use data::binance_market_tape_artifact::{
    seal_binance_market_tape_triplet, verify_binance_market_tape_for_strict_gate,
    verify_binance_market_tape_with_required_trade_summaries,
    BinanceAggregateTradeContinuityVerifier, BinanceMarketTapeTriplet,
    BinanceMarketTapeTrustAnchor,
};
use futures::{SinkExt, StreamExt};
use hft_collector::lob_archiver::{
    checkpoint_event, command_status_with_timeout, files_with_suffix, read_upload_status,
    recover_parts, segment_partition, send_or_shutdown, write_health, write_success_marker,
    write_upload_status, DepthDiff, Market, OrderBookState, PendingBudget, QueueHealth, Segment,
    SegmentConfig, SendOutcome, RAW_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::future::Future;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(revision) => revision,
    None => "unbound-source-revision",
};

#[derive(Debug, Parser)]
#[command(name = "binance-lob-archiver", version = BUILD_SOURCE_REVISION)]
struct Args {
    #[arg(long)]
    self_test: bool,

    #[arg(long, conflicts_with_all = ["self_test", "verify_segment"])]
    upload_only: bool,

    #[arg(
        long,
        conflicts_with_all = ["self_test", "upload_only"],
        requires_all = ["segment_content_sha256", "segment_manifest_sha256"]
    )]
    verify_segment: Vec<PathBuf>,

    #[arg(long, requires = "verify_segment")]
    segment_content_sha256: Vec<String>,

    #[arg(long, requires = "verify_segment")]
    segment_manifest_sha256: Vec<String>,

    #[arg(long, requires = "verify_segment")]
    require_lob_continuity: bool,

    #[arg(
        long,
        requires = "verify_segment",
        conflicts_with = "require_lob_continuity"
    )]
    verify_aggregate_trade_continuity: bool,
}

#[derive(Debug, Clone)]
struct Config {
    market: Market,
    dataset: String,
    shard_id: String,
    symbols: Vec<String>,
    security_token_symbols: Vec<String>,
    excluded_symbols: Arc<RwLock<BTreeSet<String>>>,
    ws_shard_size: usize,
    snapshot_limit: u64,
    snapshot_requests_per_second: f64,
    segment_seconds: u64,
    spool_dir: PathBuf,
    max_buffered_diffs: usize,
    max_pending_diffs: usize,
    stall_timeout: Duration,
    sync_timeout: Duration,
    process_watchdog_timeout: Duration,
    snapshot_retry_attempts: usize,
    rest_base: String,
    oss_bucket: String,
    oss_endpoint: String,
    oss_region: String,
    aliyun_profile: String,
    zstd_timeout: Duration,
    oss_copy_timeout: Duration,
}

#[derive(Debug, Clone)]
struct UploadConfig {
    spool_dir: PathBuf,
    oss_bucket: String,
    oss_endpoint: String,
    oss_region: String,
    aliyun_profile: String,
    oss_copy_timeout: Duration,
}

const UPLOADED_CLEANUP_SCHEMA: &str = "monday.binance_lob.uploaded_cleanup.v1";
const UPLOADED_CLEANUP_SUFFIX: &str = ".uploaded-cleanup.json";
const UPLOADED_CLEANUP_TMP_SUFFIX: &str = ".uploaded-cleanup.json.tmp";
const SPOOL_LOCK_FILE: &str = ".binance-lob-archiver.lock";
const SUBSCRIPTION_PROOF_ID: u64 = 1;
const SUBSCRIPTION_PROOF_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RECONNECT_PROOF_BUFFERED_EVENTS: usize = 16_384;

#[derive(Debug, Clone)]
struct StreamShard {
    url: String,
    streams: BTreeSet<String>,
}

#[derive(Debug)]
struct SpoolLock {
    _file: std::fs::File,
}

impl SpoolLock {
    fn acquire(spool_dir: &Path) -> anyhow::Result<Self> {
        let path = spool_dir.join(SPOOL_LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| format!("failed to open spool lock {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("failed to inspect spool lock {}", path.display()))?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("spool lock is not a regular file: {}", path.display());
        }
        if let Err(error) = fs4::FileExt::try_lock(&file) {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::WouldBlock {
                anyhow::bail!("spool is already locked: {}", spool_dir.display());
            }
            return Err(error)
                .with_context(|| format!("failed to lock spool {}", spool_dir.display()));
        }
        Ok(Self { _file: file })
    }
}

impl UploadConfig {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            spool_dir: PathBuf::from(env_string("SPOOL_DIR", "/data/monday/spool/binance-lob")),
            oss_bucket: env_string("OSS_BUCKET", "monday-lob-apne1-1045353359"),
            oss_endpoint: env_string("OSS_ENDPOINT", "oss-ap-northeast-1-internal.aliyuncs.com"),
            oss_region: env_string("OSS_REGION", "ap-northeast-1"),
            aliyun_profile: env_string("ALIYUN_PROFILE", "ecs-role"),
            oss_copy_timeout: Duration::from_secs(env_parse("OSS_COPY_TIMEOUT_SECONDS", 300_u64)?),
        })
    }
}

impl From<&Config> for UploadConfig {
    fn from(config: &Config) -> Self {
        Self {
            spool_dir: config.spool_dir.clone(),
            oss_bucket: config.oss_bucket.clone(),
            oss_endpoint: config.oss_endpoint.clone(),
            oss_region: config.oss_region.clone(),
            aliyun_profile: config.aliyun_profile.clone(),
            oss_copy_timeout: config.oss_copy_timeout,
        }
    }
}

impl Config {
    async fn from_env() -> anyhow::Result<Self> {
        if env_string("DEPTH_MODE", "diff") != "diff" {
            anyhow::bail!("snapshot reconciliation requires DEPTH_MODE=diff");
        }
        let market: Market = env_string("MARKET", "spot")
            .parse()
            .map_err(anyhow::Error::msg)?;
        let rest_base = env::var("BINANCE_REST_BASE").unwrap_or_else(|_| match market {
            Market::Spot => "https://data-api.binance.vision".into(),
            Market::Usdm => "https://fapi.binance.com".into(),
        });
        let setting = env_string("SYMBOLS", "btcusdt");
        let catalog = if setting.eq_ignore_ascii_case("ALL") {
            discover_symbols(market, &rest_base).await?
        } else {
            SymbolCatalog {
                symbols: setting
                    .split(',')
                    .map(|symbol| symbol.trim().to_ascii_uppercase())
                    .filter(|symbol| !symbol.is_empty())
                    .collect(),
                security_token_symbols: Vec::new(),
                excluded_symbols: Vec::new(),
            }
        };
        if catalog.symbols.is_empty() {
            anyhow::bail!("SYMBOLS must not be empty");
        }
        Ok(Self {
            market,
            dataset: env_string("DATASET", &format!("{}_all", market.as_str())),
            shard_id: env_string("SHARD_ID", "all"),
            symbols: catalog.symbols,
            security_token_symbols: catalog.security_token_symbols,
            excluded_symbols: Arc::new(RwLock::new(catalog.excluded_symbols.into_iter().collect())),
            ws_shard_size: env_parse("WS_SHARD_SIZE", 100_usize)?.max(1),
            snapshot_limit: env_parse("SNAPSHOT_LIMIT", 100_u64)?,
            snapshot_requests_per_second: env_parse("SNAPSHOT_REQUESTS_PER_SECOND", 15_f64)?,
            segment_seconds: env_parse("SEGMENT_SECONDS", 3600_u64)?.max(60),
            spool_dir: PathBuf::from(env_string("SPOOL_DIR", "/data/monday/spool/binance-lob")),
            max_buffered_diffs: env_parse("MAX_BUFFERED_DIFFS", 250_000_usize)?.max(1),
            max_pending_diffs: env_parse("MAX_PENDING_DIFFS_TOTAL", 250_000_usize)?.max(1),
            stall_timeout: Duration::from_secs(env_parse("STALL_TIMEOUT_SECONDS", 60_u64)?),
            sync_timeout: Duration::from_secs(env_parse("SYNC_TIMEOUT_SECONDS", 120_u64)?),
            process_watchdog_timeout: Duration::from_secs(env_parse(
                "PROCESS_WATCHDOG_SECONDS",
                180_u64,
            )?),
            snapshot_retry_attempts: env_parse("SNAPSHOT_RETRY_ATTEMPTS", 6_usize)?.max(1),
            rest_base,
            oss_bucket: env_string("OSS_BUCKET", "monday-lob-apne1-1045353359"),
            oss_endpoint: env_string("OSS_ENDPOINT", "oss-ap-northeast-1-internal.aliyuncs.com"),
            oss_region: env_string("OSS_REGION", "ap-northeast-1"),
            aliyun_profile: env_string("ALIYUN_PROFILE", "ecs-role"),
            zstd_timeout: Duration::from_secs(env_parse("ZSTD_TIMEOUT_SECONDS", 300_u64)?),
            oss_copy_timeout: Duration::from_secs(env_parse("OSS_COPY_TIMEOUT_SECONDS", 300_u64)?),
        })
    }

    fn segment_config(&self) -> SegmentConfig {
        SegmentConfig {
            spool_dir: self.spool_dir.clone(),
            market: self.market,
            dataset: self.dataset.clone(),
            shard_id: self.shard_id.clone(),
            symbols: self.active_symbols(),
            security_token_symbols: self.security_token_symbols.clone(),
            excluded_symbols: self.excluded_symbols(),
            snapshot_limit: self.snapshot_limit,
            zstd_timeout: self.zstd_timeout,
        }
    }

    fn stream_shards(&self) -> Vec<StreamShard> {
        self.active_symbols()
            .chunks(self.ws_shard_size)
            .flat_map(|symbols| {
                let depth_streams = symbols
                    .iter()
                    .map(|symbol| format!("{}@depth@100ms", symbol.to_ascii_lowercase()))
                    .collect::<BTreeSet<_>>();
                let aggregate_trade_streams = symbols
                    .iter()
                    .map(|symbol| format!("{}@aggTrade", symbol.to_ascii_lowercase()))
                    .collect::<BTreeSet<_>>();
                let depth = depth_streams.iter().cloned().collect::<Vec<_>>().join("/");
                let aggregate_trades = aggregate_trade_streams
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("/");
                match self.market {
                    Market::Spot => vec![
                        StreamShard {
                            url: format!("wss://data-stream.binance.vision/stream?streams={depth}"),
                            streams: depth_streams,
                        },
                        StreamShard {
                            url: format!(
                                "wss://data-stream.binance.vision/stream?streams={aggregate_trades}"
                            ),
                            streams: aggregate_trade_streams,
                        },
                    ],
                    Market::Usdm => vec![
                        StreamShard {
                            url: format!("wss://fstream.binance.com/public/stream?streams={depth}"),
                            streams: depth_streams,
                        },
                        StreamShard {
                            url: format!(
                                "wss://fstream.binance.com/market/stream?streams={aggregate_trades}"
                            ),
                            streams: aggregate_trade_streams,
                        },
                    ],
                }
            })
            .collect()
    }

    fn active_symbols(&self) -> Vec<String> {
        let excluded = self.excluded_symbols.read().expect("catalog lock poisoned");
        self.symbols
            .iter()
            .filter(|symbol| !excluded.contains(*symbol))
            .cloned()
            .collect()
    }

    fn excluded_symbols(&self) -> Vec<String> {
        self.excluded_symbols
            .read()
            .expect("catalog lock poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn exclude_symbol(&self, symbol: &str) {
        self.excluded_symbols
            .write()
            .expect("catalog lock poisoned")
            .insert(symbol.to_owned());
    }

    fn is_excluded(&self, symbol: &str) -> bool {
        self.excluded_symbols
            .read()
            .expect("catalog lock poisoned")
            .contains(symbol)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolCatalog {
    symbols: Vec<String>,
    security_token_symbols: Vec<String>,
    excluded_symbols: Vec<String>,
}

#[derive(Debug)]
struct ValidatedDepth {
    diff: DepthDiff,
    source_clock: DepthSourceClock,
}

#[derive(Debug)]
enum Event {
    Diff {
        received_at_ns: u64,
        frame: Value,
        depth: Box<ValidatedDepth>,
    },
    AggregateTrade {
        trade: AggregateTrade,
        frame: Value,
    },
    Snapshot {
        received_at_ns: u64,
        symbol: String,
        request_started_at_ns: u64,
        snapshot: Value,
    },
    ExcludeSymbol {
        symbol: String,
        reason: String,
    },
    InitialSnapshotsComplete,
    StreamCoverageVerified {
        shards: Vec<Vec<String>>,
    },
    StreamDisconnected {
        streams: Vec<String>,
        reason: String,
    },
    StreamReconnected {
        streams: Vec<String>,
    },
    RotationBarrier {
        producer_id: usize,
        epoch: u64,
    },
}

#[derive(Debug)]
enum TaskExit {
    Stopped(Option<Event>),
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessAction {
    None,
    Excluded,
    InitialSnapshotsComplete,
    RestartSession,
}

impl ProcessAction {
    fn restarts_capture_session(&self) -> bool {
        matches!(self, Self::Excluded | Self::RestartSession)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RotationBarrierResult {
    Ready { initial_snapshots_complete: bool },
    RestartSession,
}

enum ProducerWait<T> {
    Ready(T),
    PauseRequested,
    Stopped,
}

#[derive(Debug, Default)]
struct ProcessState {
    sequence_gaps: u64,
    depth_source_clocks: DepthSourceClockSequenceValidator,
    aggregate_trades: AggregateTradeSequenceValidator,
    stream_coverage_trusted: bool,
    stream_coverage_shards: Vec<Vec<String>>,
    reconnecting_shards: Vec<BTreeSet<String>>,
    reconnecting_depth_shards: Vec<BTreeSet<String>>,
}

impl ProcessState {
    fn new(stream_coverage_trusted: bool) -> Self {
        Self {
            stream_coverage_trusted,
            ..Self::default()
        }
    }

    fn streams_healthy(&self) -> bool {
        self.reconnecting_shards.is_empty()
    }

    fn depth_streams_healthy(&self) -> bool {
        self.reconnecting_depth_shards.is_empty()
    }

    fn mark_shard_disconnected(&mut self, streams: Vec<String>) {
        let shard = streams.into_iter().collect::<BTreeSet<_>>();
        if shard.is_empty() {
            return;
        }
        if !self.reconnecting_shards.iter().any(|known| known == &shard) {
            self.reconnecting_shards.push(shard.clone());
        }
        let depth_streams = shard
            .iter()
            .filter(|stream| stream.ends_with("@depth@100ms"))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !depth_streams.is_empty()
            && !self
                .reconnecting_depth_shards
                .iter()
                .any(|known| known == &depth_streams)
        {
            self.reconnecting_depth_shards.push(depth_streams);
        }
    }

    fn mark_stream_observed(&mut self, stream: &str) {
        if let Some(index) = self
            .reconnecting_shards
            .iter()
            .position(|shard| shard.contains(stream))
        {
            self.reconnecting_shards[index].remove(stream);
            if self.reconnecting_shards[index].is_empty() {
                self.reconnecting_shards.remove(index);
            }
        }
        if stream.ends_with("@depth@100ms") {
            if let Some(index) = self
                .reconnecting_depth_shards
                .iter()
                .position(|shard| shard.contains(stream))
            {
                self.reconnecting_depth_shards[index].remove(stream);
                if self.reconnecting_depth_shards[index].is_empty() {
                    self.reconnecting_depth_shards.remove(index);
                }
            }
        }
    }
}

#[derive(Debug)]
struct SnapshotUnavailable {
    symbol: String,
    status: reqwest::StatusCode,
}

impl std::fmt::Display for SnapshotUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "snapshot unavailable symbol={} status={}",
            self.symbol, self.status
        )
    }
}

impl std::error::Error for SnapshotUnavailable {}

#[derive(Clone)]
struct ProcessWatchdog {
    inner: Arc<ProcessWatchdogInner>,
}

struct ProcessWatchdogInner {
    started: Instant,
    last_data_ms: AtomicU64,
    state: AtomicU8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ProcessWatchdogState {
    Disarmed = 0,
    Armed = 1,
    Firing = 2,
    Stopped = 3,
}

impl ProcessWatchdog {
    fn new_state() -> Self {
        Self {
            inner: Arc::new(ProcessWatchdogInner {
                started: Instant::now(),
                last_data_ms: AtomicU64::new(0),
                state: AtomicU8::new(ProcessWatchdogState::Disarmed as u8),
            }),
        }
    }

    fn start(timeout: Duration) -> anyhow::Result<Self> {
        let watchdog = Self::new_state();
        let monitor = watchdog.clone();
        thread::Builder::new()
            .name("binance-lob-process-watchdog".into())
            .spawn(move || {
                let interval = timeout
                    .div_f64(4.0)
                    .clamp(Duration::from_secs(1), Duration::from_secs(10));
                while monitor.state() != ProcessWatchdogState::Stopped {
                    thread::sleep(interval);
                    let now_ms = monitor.elapsed_ms();
                    if monitor.try_begin_exit_at(now_ms, timeout) {
                        let last_ms = monitor.inner.last_data_ms.load(Ordering::Relaxed);
                        error!(
                            silent_ms = now_ms.saturating_sub(last_ms),
                            "process watchdog exiting after market-data stall"
                        );
                        std::process::exit(75);
                    }
                }
            })?;
        watchdog.mark_data();
        anyhow::ensure!(watchdog.arm(), "process watchdog failed to arm");
        Ok(watchdog)
    }

    fn mark_data(&self) {
        self.inner
            .last_data_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    fn arm(&self) -> bool {
        self.inner
            .state
            .compare_exchange(
                ProcessWatchdogState::Disarmed as u8,
                ProcessWatchdogState::Armed as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn disarm(&self) -> bool {
        loop {
            match self.state() {
                ProcessWatchdogState::Armed => {
                    if self
                        .inner
                        .state
                        .compare_exchange(
                            ProcessWatchdogState::Armed as u8,
                            ProcessWatchdogState::Disarmed as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                }
                ProcessWatchdogState::Disarmed | ProcessWatchdogState::Stopped => return true,
                ProcessWatchdogState::Firing => return false,
            }
        }
    }

    fn stop(&self) {
        loop {
            let current = self.state();
            match current {
                ProcessWatchdogState::Stopped | ProcessWatchdogState::Firing => return,
                ProcessWatchdogState::Disarmed | ProcessWatchdogState::Armed => {
                    if self
                        .inner
                        .state
                        .compare_exchange(
                            current as u8,
                            ProcessWatchdogState::Stopped as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
            }
        }
    }

    fn try_begin_exit_at(&self, now_ms: u64, timeout: Duration) -> bool {
        if !process_watchdog_expired(
            self.inner.last_data_ms.load(Ordering::Relaxed),
            now_ms,
            timeout,
        ) {
            return false;
        }
        self.inner
            .state
            .compare_exchange(
                ProcessWatchdogState::Armed as u8,
                ProcessWatchdogState::Firing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn state(&self) -> ProcessWatchdogState {
        match self.inner.state.load(Ordering::Acquire) {
            0 => ProcessWatchdogState::Disarmed,
            1 => ProcessWatchdogState::Armed,
            2 => ProcessWatchdogState::Firing,
            3 => ProcessWatchdogState::Stopped,
            state => unreachable!("invalid process watchdog state {state}"),
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.inner.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn process_watchdog_expired(last_data_ms: u64, now_ms: u64, timeout: Duration) -> bool {
    now_ms.saturating_sub(last_data_ms) > timeout.as_millis() as u64
}

fn publish_global_shutdown(shutdown: &watch::Sender<bool>, watchdog: &ProcessWatchdog) -> bool {
    // Publish shutdown only after the watchdog is disarmed. Receivers may spend
    // up to ZSTD_TIMEOUT_SECONDS closing their final segment, which is expected
    // progress and must not be mistaken for a market-data stall.
    if !watchdog.disarm() {
        return false;
    }
    let _ = shutdown.send(true);
    true
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if args.self_test {
        return self_test();
    }
    if args.upload_only {
        return upload_only(&UploadConfig::from_env()?).await;
    }
    if !args.verify_segment.is_empty() {
        return verify_segments(&args);
    }

    let spool_dir = PathBuf::from(env_string("SPOOL_DIR", "/data/monday/spool/binance-lob"));
    std::fs::create_dir_all(&spool_dir)?;
    let _spool_lock = SpoolLock::acquire(&spool_dir)?;
    let config = Arc::new(Config::from_env().await?);
    let recovered = recover_parts(&config.segment_config())?;
    if !recovered.is_empty() {
        info!(segments = recovered.len(), "recovered interrupted segments");
    }
    let watchdog = ProcessWatchdog::start(config.process_watchdog_timeout)?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let upload_task = tokio::spawn(upload_loop(config.clone(), shutdown_rx.clone()));
    let shutdown_signal = tokio::spawn(wait_for_signal(shutdown_tx.clone(), watchdog.clone()));

    let mut backoff = 1_u64;
    while !*shutdown_rx.borrow() {
        match run_session(config.clone(), shutdown_rx.clone(), watchdog.clone()).await {
            Ok(()) if *shutdown_rx.borrow() => break,
            Ok(()) => backoff = 1,
            Err(error) => {
                error!(error = %error, backoff, "session failed; reconnecting");
                tokio::time::sleep(Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(30);
            }
        }
    }
    publish_global_shutdown(&shutdown_tx, &watchdog);
    watchdog.stop();
    upload_task.await?;
    shutdown_signal.abort();
    Ok(())
}

fn verify_segments(args: &Args) -> anyhow::Result<()> {
    let count = args.verify_segment.len();
    if count == 0
        || args.segment_content_sha256.len() != count
        || args.segment_manifest_sha256.len() != count
    {
        bail!(
            "--verify-segment, --segment-content-sha256, and --segment-manifest-sha256 must have equal nonzero lengths"
        );
    }

    let mut content_hashes = BTreeSet::new();
    let mut trusted_segments = Vec::with_capacity(count);
    for ((path, content_sha256), manifest_sha256) in args
        .verify_segment
        .iter()
        .zip(&args.segment_content_sha256)
        .zip(&args.segment_manifest_sha256)
    {
        if !content_hashes.insert(content_sha256) {
            bail!("duplicate market-tape segment supplied");
        }
        let trust = BinanceMarketTapeTrustAnchor::from_lower_hex(content_sha256, manifest_sha256)?;
        trusted_segments.push((path, trust));
    }

    if args.verify_aggregate_trade_continuity {
        let mut verifier = BinanceAggregateTradeContinuityVerifier::default();
        for (path, trust) in trusted_segments {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .context("market-tape segment path has no UTF-8 file name")?;
            let triplet = BinanceMarketTapeTriplet {
                data: path.clone(),
                manifest: path.with_file_name(format!("{file_name}.manifest.json")),
                success: path.with_file_name(format!("{file_name}._SUCCESS")),
            };
            verifier.observe_segment(seal_binance_market_tape_triplet(&triplet, &trust)?)?;
        }
        println!(
            "aggregate-trade continuity verification: ok ({} segments)",
            count
        );
        return Ok(());
    }

    let mut sealed = Vec::with_capacity(count);
    for (path, trust) in trusted_segments {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("market-tape segment path has no UTF-8 file name")?;
        let triplet = BinanceMarketTapeTriplet {
            data: path.clone(),
            manifest: path.with_file_name(format!("{file_name}.manifest.json")),
            success: path.with_file_name(format!("{file_name}._SUCCESS")),
        };
        sealed.push(seal_binance_market_tape_triplet(&triplet, &trust)?);
    }
    if args.require_lob_continuity {
        verify_binance_market_tape_for_strict_gate(sealed)?;
    } else {
        verify_binance_market_tape_with_required_trade_summaries(sealed)?;
    }
    println!(
        "strict market-tape verification: ok ({} segments)",
        count
    );
    Ok(())
}

async fn run_session(
    config: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
    watchdog: ProcessWatchdog,
) -> anyhow::Result<()> {
    let session_id = format!("{:x}-{}", now_ns()?, std::process::id());
    let mut process_state = ProcessState::new(false);
    let active_symbols = config.active_symbols();
    if active_symbols.is_empty() {
        anyhow::bail!("no active symbols remain after runtime exclusions");
    }
    let (sender, mut receiver) = mpsc::channel(config.max_buffered_diffs);
    let (rotation_pause_tx, rotation_pause_rx) = watch::channel(0_u64);
    let (rotation_resume_tx, rotation_resume_rx) = watch::channel(0_u64);
    let mut rotation_epoch = 0_u64;
    let (session_stop_tx, session_stop_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    let stream_shards = config.stream_shards();
    let expected_shards = stream_shards.len();
    let expected_rotation_producers = expected_shards
        .checked_add(1)
        .context("rotation producer count overflow")?;
    let expected_streams = stream_shards
        .iter()
        .map(|shard| shard.streams.len())
        .sum::<usize>();
    let (stream_connected_tx, stream_connected_rx) = mpsc::channel(expected_shards);
    for (producer_id, shard) in stream_shards.into_iter().enumerate() {
        tasks.spawn(receive_url(
            shard,
            sender.clone(),
            stream_connected_tx.clone(),
            session_stop_rx.clone(),
            config.stall_timeout,
            SUBSCRIPTION_PROOF_TIMEOUT,
            watchdog.clone(),
            producer_id,
            rotation_pause_rx.clone(),
            rotation_resume_rx.clone(),
        ));
    }
    drop(stream_connected_tx);
    tasks.spawn(produce_snapshots_after_streams_connect(
        config.clone(),
        sender.clone(),
        session_stop_rx.clone(),
        stream_connected_rx,
        expected_shards,
        expected_shards,
        rotation_pause_rx.clone(),
        rotation_resume_rx.clone(),
    ));
    let mut states = active_symbols
        .iter()
        .map(|symbol| (symbol.clone(), OrderBookState::new(symbol, config.market)))
        .collect::<HashMap<_, _>>();
    let mut budget = PendingBudget::new(config.max_pending_diffs);
    let mut segment = Segment::create(config.segment_config(), now_ns()?)?;
    segment.write(
        "session_start",
        json!({
            "session_id": session_id,
            "market": config.market.as_str(),
            "symbols": active_symbols.len(),
            "websocket_shards": expected_shards,
            "websocket_streams": expected_streams,
        }),
        now_ns()?,
    )?;
    let mut last_health = Instant::now() - Duration::from_secs(60);
    let mut failure = None;
    let mut sync_deadline = None;

    loop {
        let mut pending_action = ProcessAction::None;
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    // Defensive in case a future shutdown publisher bypasses
                    // publish_global_shutdown: disarm before task drain and
                    // final segment compression begin.
                    watchdog.disarm();
                    break;
                }
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok(Ok(TaskExit::Stopped(Some(event))))) => {
                        pending_action = process_event(
                            &config, &mut segment, &mut states, &mut budget, &session_id, event,
                            &mut process_state,
                        )?;
                    }
                    Some(Ok(Ok(TaskExit::Stopped(None)))) if *session_stop_rx.borrow() => {},
                    Some(Ok(Ok(TaskExit::Stopped(None)))) => {
                        failure = Some(anyhow::anyhow!("producer stopped unexpectedly"));
                        break;
                    }
                    Some(Ok(Err(error))) => {
                        failure = Some(error);
                        break;
                    }
                    Some(Err(error)) => {
                        failure = Some(error.into());
                        break;
                    }
                    None => break,
                }
            }
            event = receiver.recv() => {
                match event {
                    Some(event) => {
                        match process_event(
                            &config, &mut segment, &mut states, &mut budget, &session_id, event,
                            &mut process_state,
                        ) {
                            Ok(action) => pending_action = action,
                            Err(error) => {
                                failure = Some(error);
                                break;
                            }
                        }
                    }
                    None => {
                        failure = Some(anyhow::anyhow!("archive queue closed"));
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }

        match pending_action {
            action if action.restarts_capture_session() => break,
            ProcessAction::None => {}
            ProcessAction::Excluded => unreachable!("excluded action must restart the session"),
            ProcessAction::RestartSession => {
                unreachable!("recovered reconnect must restart the session")
            }
            ProcessAction::InitialSnapshotsComplete => {
                sync_deadline = Some(Instant::now() + config.sync_timeout);
            }
        }

        // This must live outside the biased select: under continuous market
        // data receiver.recv() is always ready and the timer branch can starve.
        if segment_due(&segment, config.segment_seconds)? {
            // Each producer emits its barrier after every event whose receive
            // timestamp it has already captured on that producer's sender.
            rotation_epoch = match rotation_epoch.checked_add(1) {
                Some(epoch) => epoch,
                None => {
                    failure = Some(anyhow::anyhow!("segment rotation epoch overflow"));
                    break;
                }
            };
            if rotation_pause_tx.send(rotation_epoch).is_err() {
                failure = Some(anyhow::anyhow!(
                    "collector producers stopped before segment rotation"
                ));
                break;
            }
            let barriers = match await_rotation_barriers(
                &config,
                &mut receiver,
                &mut tasks,
                &mut segment,
                &mut states,
                &mut budget,
                &session_id,
                &mut process_state,
                expected_rotation_producers,
                rotation_epoch,
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    let _ = rotation_resume_tx.send(rotation_epoch);
                    failure = Some(error);
                    break;
                }
            };
            let initial_snapshots_complete = match barriers {
                RotationBarrierResult::Ready {
                    initial_snapshots_complete,
                } => initial_snapshots_complete,
                RotationBarrierResult::RestartSession => {
                    let _ = rotation_resume_tx.send(rotation_epoch);
                    break;
                }
            };
            if initial_snapshots_complete {
                sync_deadline = Some(Instant::now() + config.sync_timeout);
            }
            let next_segment = match begin_segment_rotation(
                &mut segment,
                &config,
                &states,
                &session_id,
                "scheduled",
                &process_state,
            ) {
                Ok(next_segment) => next_segment,
                Err(error) => {
                    let _ = rotation_resume_tx.send(rotation_epoch);
                    failure = Some(error);
                    break;
                }
            };
            if let Err(error) = finish_segment_rotation(
                &mut segment,
                next_segment,
                &rotation_resume_tx,
                rotation_epoch,
            ) {
                failure = Some(error);
                break;
            }
        }

        if states.values().all(|state| state.synced) {
            sync_deadline = None;
        } else if sync_timed_out(&states, sync_deadline, Instant::now()) {
            let missing = states
                .iter()
                .filter(|(_, state)| !state.synced)
                .map(|(symbol, _)| symbol.as_str())
                .collect::<Vec<_>>();
            failure = Some(anyhow::anyhow!("snapshot sync timed out: {missing:?}"));
            break;
        }

        if last_health.elapsed() >= Duration::from_secs(30) {
            let status =
                if !process_state.streams_healthy() || !process_state.depth_streams_healthy() {
                    "reconnecting"
                } else if states.values().all(|state| state.synced) {
                    "synced"
                } else {
                    "syncing"
                };
            write_health(
                &config.spool_dir,
                config.market,
                &config.dataset,
                &session_id,
                status,
                process_state.sequence_gaps,
                files_with_suffix(&config.spool_dir, ".manifest.json")?.len(),
                QueueHealth::from_sender(&sender),
                &states,
            )?;
            last_health = Instant::now();
        }
    }

    let _ = session_stop_tx.send(true);
    let final_queue_health = QueueHealth::from_sender(&sender);
    drop(sender);
    while let Some(joined) = tasks.join_next().await {
        if let Ok(Ok(TaskExit::Stopped(Some(event)))) = joined {
            archive_only(&mut segment, &session_id, event)?;
        }
    }
    while let Ok(event) = receiver.try_recv() {
        archive_only(&mut segment, &session_id, event)?;
    }
    if failure.is_some() {
        segment.mark_replay_unsafe();
    }
    let _ = close_segment(
        segment,
        &config,
        &states,
        &session_id,
        "shutdown",
        &process_state,
    )?;
    write_health(
        &config.spool_dir,
        config.market,
        &config.dataset,
        &session_id,
        if failure.is_some() {
            "reconnecting"
        } else {
            "stopped"
        },
        process_state.sequence_gaps,
        files_with_suffix(&config.spool_dir, ".manifest.json")?.len(),
        final_queue_health,
        &states,
    )?;
    if let Some(error) = failure {
        Err(error)
    } else {
        Ok(())
    }
}

fn sync_timed_out(
    states: &HashMap<String, OrderBookState>,
    deadline: Option<Instant>,
    now: Instant,
) -> bool {
    deadline.is_some_and(|deadline| now > deadline && states.values().any(|state| !state.synced))
}

fn process_event(
    config: &Config,
    segment: &mut Segment,
    states: &mut HashMap<String, OrderBookState>,
    budget: &mut PendingBudget,
    session_id: &str,
    event: Event,
    process_state: &mut ProcessState,
) -> anyhow::Result<ProcessAction> {
    match event {
        Event::Diff {
            received_at_ns,
            frame,
            depth,
        } => {
            let ValidatedDepth { diff, source_clock } = *depth;
            if config.is_excluded(&diff.symbol) {
                return Ok(ProcessAction::None);
            }
            let stream_name = format!("{}@depth@100ms", diff.symbol.to_ascii_lowercase());
            if source_clock.symbol != diff.symbol {
                anyhow::bail!("depth sequence and source-clock symbols disagree");
            }
            if let Err(error) = process_state.depth_source_clocks.observe(&source_clock) {
                segment.mark_replay_unsafe();
                process_state.sequence_gaps += 1;
                segment.write(
                    "sequence_gap",
                    json!({
                        "session_id":session_id,
                        "kind":"depth_source_clock",
                        "symbol":diff.symbol,
                        "error":error.to_string(),
                        "frame":frame,
                    }),
                    received_at_ns,
                )?;
                return Err(error);
            }
            segment.write(
                "diff",
                json!({"session_id":session_id,"frame":frame}),
                received_at_ns,
            )?;
            let state = states
                .get_mut(&diff.symbol)
                .ok_or_else(|| anyhow::anyhow!("unconfigured symbol {}", diff.symbol))?;
            if let Err(error) = state.apply_diff(diff, budget) {
                segment.mark_replay_unsafe();
                process_state.sequence_gaps += 1;
                segment.write(
                    "sequence_gap",
                    json!({"session_id":session_id,"error":error.to_string()}),
                    now_ns()?,
                )?;
                return Err(error);
            }
            let reconnecting_before = !process_state.streams_healthy();
            process_state.mark_stream_observed(&stream_name);
            if reconnecting_before && process_state.streams_healthy() {
                return Ok(ProcessAction::RestartSession);
            }
        }
        Event::AggregateTrade { trade, frame } => {
            if config.is_excluded(&trade.symbol) {
                return Ok(ProcessAction::None);
            }
            if !states.contains_key(&trade.symbol) {
                anyhow::bail!("unconfigured symbol {}", trade.symbol);
            }
            if let Err(error) = process_state.aggregate_trades.observe(&trade) {
                segment.mark_replay_unsafe();
                process_state.sequence_gaps += 1;
                segment.write(
                    "aggregate_trade_gap",
                    json!({
                        "session_id":session_id,
                        "symbol":trade.symbol,
                        "error":error.to_string(),
                        "frame":frame,
                    }),
                    trade.received_at_ns,
                )?;
                return Err(error);
            }
            segment.write(
                "agg_trade",
                json!({"session_id":session_id,"frame":frame}),
                trade.received_at_ns,
            )?;
            let stream_name = format!("{}@aggTrade", trade.symbol.to_ascii_lowercase());
            let reconnecting_before = !process_state.streams_healthy();
            process_state.mark_stream_observed(&stream_name);
            if reconnecting_before && process_state.streams_healthy() {
                return Ok(ProcessAction::RestartSession);
            }
        }
        Event::Snapshot {
            received_at_ns,
            symbol,
            request_started_at_ns,
            snapshot,
        } => {
            if config.is_excluded(&symbol) {
                return Ok(ProcessAction::None);
            }
            segment.write(
                "snapshot",
                json!({
                    "session_id":session_id,
                    "symbol":symbol,
                    "request_started_at_ns":request_started_at_ns,
                    "snapshot":snapshot,
                }),
                received_at_ns,
            )?;
            let state = states
                .get_mut(&symbol)
                .ok_or_else(|| anyhow::anyhow!("unconfigured symbol {symbol}"))?;
            if let Err(error) = state.install_snapshot(&snapshot, budget) {
                segment.mark_replay_unsafe();
                process_state.sequence_gaps += 1;
                segment.write(
                    "sequence_gap",
                    json!({"session_id":session_id,"error":error.to_string()}),
                    now_ns()?,
                )?;
                return Err(error);
            }
        }
        Event::ExcludeSymbol { symbol, reason } => {
            config.exclude_symbol(&symbol);
            if let Some(mut state) = states.remove(&symbol) {
                state.invalidate_for_resync(budget);
            }
            segment.mark_replay_unsafe();
            segment.write(
                "symbol_excluded",
                json!({"session_id":session_id,"symbol":symbol,"reason":reason}),
                now_ns()?,
            )?;
            return Ok(ProcessAction::Excluded);
        }
        Event::InitialSnapshotsComplete => {
            return Ok(ProcessAction::InitialSnapshotsComplete);
        }
        Event::StreamCoverageVerified { shards } => {
            let stream_count = validate_stream_coverage_shards(&shards, states)?;
            let shard_count = shards.len();
            info!(
                shard_count,
                stream_count, "websocket stream coverage verified"
            );
            segment.write(
                "stream_coverage",
                json!({"session_id":session_id,"shards":shards}),
                now_ns()?,
            )?;
            process_state.stream_coverage_trusted = true;
            process_state.stream_coverage_shards = shards;
            for state in states.values_mut() {
                state.verify_stream_coverage();
            }
        }
        Event::StreamDisconnected { streams, reason } => {
            process_state.mark_shard_disconnected(streams);
            segment.mark_replay_unsafe();
            info!(reason, "websocket shard reconnecting");
        }
        Event::StreamReconnected { streams } => {
            info!(streams = ?streams, "websocket shard connection restored; awaiting market data");
        }
        Event::RotationBarrier { .. } => {
            anyhow::bail!("rotation barrier reached normal event processing");
        }
    }
    Ok(ProcessAction::None)
}

#[allow(clippy::too_many_arguments)]
async fn await_rotation_barriers(
    config: &Config,
    receiver: &mut mpsc::Receiver<Event>,
    tasks: &mut JoinSet<anyhow::Result<TaskExit>>,
    segment: &mut Segment,
    states: &mut HashMap<String, OrderBookState>,
    budget: &mut PendingBudget,
    session_id: &str,
    process_state: &mut ProcessState,
    expected_producers: usize,
    epoch: u64,
) -> anyhow::Result<RotationBarrierResult> {
    let acknowledgement_timeout = config
        .stall_timeout
        .max(SUBSCRIPTION_PROOF_TIMEOUT.saturating_add(Duration::from_secs(1)));
    let deadline = tokio::time::Instant::now() + acknowledgement_timeout;
    let mut acknowledged = BTreeSet::new();
    let mut saw_initial_snapshots_complete = false;
    while acknowledged.len() < expected_producers {
        let event = tokio::select! {
            event = receiver.recv() => {
                event.context("archive queue closed while waiting for segment rotation barriers")?
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok(Ok(TaskExit::Stopped(Some(event))))) => {
                        process_event(
                            config,
                            segment,
                            states,
                            budget,
                            session_id,
                            event,
                            process_state,
                        )?;
                        return Err(anyhow::anyhow!(
                            "collector producer stopped unexpectedly while awaiting segment rotation barrier"
                        ));
                    }
                    Some(Ok(Ok(TaskExit::Stopped(None)))) => {
                        return Err(anyhow::anyhow!(
                            "collector producer stopped unexpectedly while awaiting segment rotation barrier"
                        ));
                    }
                    Some(Ok(Err(error))) => return Err(error),
                    Some(Err(error)) => return Err(error.into()),
                    None => return Err(anyhow::anyhow!(
                        "collector producer set emptied while awaiting segment rotation barrier"
                    )),
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(anyhow::anyhow!(
                    "collector producers did not acknowledge segment rotation epoch {epoch} within {}s",
                    acknowledgement_timeout.as_secs()
                ));
            }
        };
        match event {
            Event::RotationBarrier {
                producer_id,
                epoch: event_epoch,
            } => {
                anyhow::ensure!(
                    event_epoch == epoch,
                    "rotation barrier epoch mismatch: expected {epoch}, received {event_epoch}"
                );
                anyhow::ensure!(
                    producer_id < expected_producers,
                    "rotation barrier producer {producer_id} is outside expected range 0..{expected_producers}"
                );
                anyhow::ensure!(
                    acknowledged.insert(producer_id),
                    "duplicate rotation barrier from producer {producer_id} for epoch {epoch}"
                );
            }
            event => {
                let action = process_event(
                    config,
                    segment,
                    states,
                    budget,
                    session_id,
                    event,
                    process_state,
                )?;
                if action.restarts_capture_session() {
                    return Ok(RotationBarrierResult::RestartSession);
                }
                saw_initial_snapshots_complete |=
                    matches!(action, ProcessAction::InitialSnapshotsComplete);
            }
        }
    }
    Ok(RotationBarrierResult::Ready {
        initial_snapshots_complete: saw_initial_snapshots_complete,
    })
}

fn segment_due(segment: &Segment, segment_seconds: u64) -> anyhow::Result<bool> {
    segment_due_at(segment.start_ns, now_ns()?, segment_seconds)
}

fn segment_due_at(start_ns: u64, now_ns: u64, segment_seconds: u64) -> anyhow::Result<bool> {
    let segment_ns = segment_seconds
        .checked_mul(1_000_000_000)
        .context("segment duration overflow")?;
    let next_segment_boundary_ns = (start_ns / segment_ns)
        .checked_add(1)
        .and_then(|bucket| bucket.checked_mul(segment_ns))
        .context("next segment boundary overflow")?;
    Ok(now_ns >= next_segment_boundary_ns
        || segment_partition(start_ns)? != segment_partition(now_ns)?)
}

fn archive_only(segment: &mut Segment, session_id: &str, event: Event) -> anyhow::Result<()> {
    if !matches!(
        &event,
        Event::InitialSnapshotsComplete | Event::StreamCoverageVerified { .. }
    ) {
        segment.mark_replay_unsafe();
    }
    match event {
        Event::Diff {
            received_at_ns,
            frame,
            ..
        } => segment.write(
            "diff",
            json!({"session_id":session_id,"archived_only":true,"frame":frame}),
            received_at_ns,
        ),
        Event::AggregateTrade { trade, frame } => segment.write(
            "agg_trade",
            json!({"session_id":session_id,"archived_only":true,"frame":frame}),
            trade.received_at_ns,
        ),
        Event::Snapshot {
            received_at_ns,
            symbol,
            request_started_at_ns,
            snapshot,
        } => segment.write(
            "snapshot",
            json!({
                "session_id":session_id,
                "archived_only":true,
                "symbol":symbol,
                "request_started_at_ns":request_started_at_ns,
                "snapshot":snapshot,
            }),
            received_at_ns,
        ),
        Event::ExcludeSymbol { symbol, reason } => segment.write(
            "symbol_excluded",
            json!({
                "session_id":session_id,
                "archived_only":true,
                "symbol":symbol,
                "reason":reason,
            }),
            now_ns()?,
        ),
        Event::InitialSnapshotsComplete => Ok(()),
        Event::StreamCoverageVerified { shards } => segment.write(
            "stream_coverage",
            json!({"session_id":session_id,"shards":shards}),
            now_ns()?,
        ),
        Event::StreamDisconnected { .. }
        | Event::StreamReconnected { .. }
        | Event::RotationBarrier { .. } => Ok(()),
    }
}

fn validate_stream_coverage_shards(
    shards: &[Vec<String>],
    states: &HashMap<String, OrderBookState>,
) -> anyhow::Result<usize> {
    anyhow::ensure!(
        !shards.is_empty() && shards.iter().all(|shard| !shard.is_empty()),
        "stream coverage has an empty websocket shard"
    );
    let stream_count = shards.iter().map(Vec::len).sum::<usize>();
    let actual = shards.iter().flatten().cloned().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        actual.len() == stream_count,
        "stream coverage contains duplicate streams"
    );
    let expected = states
        .keys()
        .flat_map(|symbol| {
            let symbol = symbol.to_ascii_lowercase();
            [
                format!("{symbol}@depth@100ms"),
                format!("{symbol}@aggTrade"),
            ]
        })
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        actual == expected,
        "stream coverage does not match the active catalog"
    );
    Ok(stream_count)
}

fn write_stream_coverage(
    segment: &mut Segment,
    session_id: &str,
    shards: &[Vec<String>],
    received_at_ns: u64,
) -> anyhow::Result<()> {
    anyhow::ensure!(!shards.is_empty(), "stream coverage evidence is empty");
    segment.write(
        "stream_coverage",
        json!({"session_id":session_id,"shards":shards}),
        received_at_ns,
    )
}

fn begin_segment_rotation(
    segment: &mut Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
    process_state: &ProcessState,
) -> anyhow::Result<Segment> {
    let rotation_boundary_ns = now_ns()?;
    let seed_next = reason == "scheduled" && replay_seed_ready(segment, states, process_state);
    prepare_segment_for_close(
        segment,
        config,
        states,
        session_id,
        reason,
        process_state,
        rotation_boundary_ns,
    )?;
    let mut next = Segment::create(config.segment_config(), rotation_boundary_ns)?;
    if seed_next {
        write_stream_coverage(
            &mut next,
            session_id,
            &process_state.stream_coverage_shards,
            rotation_boundary_ns,
        )?;
        write_checkpoints(
            &mut next,
            states,
            session_id,
            "segment_open",
            true,
            rotation_boundary_ns,
        )?;
    } else {
        next.mark_replay_unsafe();
    }
    Ok(next)
}

fn finish_segment_rotation(
    segment: &mut Segment,
    next_segment: Segment,
    rotation_resume_tx: &watch::Sender<u64>,
    rotation_epoch: u64,
) -> anyhow::Result<()> {
    let closing_segment = std::mem::replace(segment, next_segment);
    let _ = rotation_resume_tx.send(rotation_epoch);
    closing_segment.close()?;
    Ok(())
}

#[cfg(test)]
fn rotate_segment(
    segment: Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
    process_state: &ProcessState,
) -> anyhow::Result<Segment> {
    let mut closing_segment = segment;
    let next = begin_segment_rotation(
        &mut closing_segment,
        config,
        states,
        session_id,
        reason,
        process_state,
    )?;
    closing_segment.close()?;
    Ok(next)
}

fn replay_checkpoint_ready(
    segment: &Segment,
    states: &HashMap<String, OrderBookState>,
    process_state: &ProcessState,
) -> bool {
    segment.is_replay_safe() && replay_seed_ready(segment, states, process_state)
}

fn replay_seed_ready(
    segment: &Segment,
    states: &HashMap<String, OrderBookState>,
    process_state: &ProcessState,
) -> bool {
    // A transport reconnect cannot seed a checkpoint until every affected
    // market stream has passed through the sequence validators.
    process_state.streams_healthy()
        && process_state.depth_streams_healthy()
        && process_state.sequence_gaps == 0
        && process_state.stream_coverage_trusted
        && segment.event_count("agg_trade") > 0
        && !states.is_empty()
        && states.values().all(OrderBookState::continuity_complete)
}

fn write_checkpoints(
    segment: &mut Segment,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
    replay_safe: bool,
    received_at_ns: u64,
) -> anyhow::Result<()> {
    for state in states.values() {
        segment.write(
            "checkpoint",
            checkpoint_event(state.checkpoint(session_id)?, reason, replay_safe),
            received_at_ns,
        )?;
    }
    Ok(())
}

fn close_segment(
    segment: Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
    process_state: &ProcessState,
) -> anyhow::Result<Option<hft_collector::lob_archiver::SegmentArtifacts>> {
    close_segment_at(
        segment,
        config,
        states,
        session_id,
        reason,
        process_state,
        now_ns()?,
    )
}

#[allow(clippy::too_many_arguments)]
fn close_segment_at(
    mut segment: Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
    process_state: &ProcessState,
    checkpoint_received_at_ns: u64,
) -> anyhow::Result<Option<hft_collector::lob_archiver::SegmentArtifacts>> {
    prepare_segment_for_close(
        &mut segment,
        config,
        states,
        session_id,
        reason,
        process_state,
        checkpoint_received_at_ns,
    )?;
    segment.close()
}

#[allow(clippy::too_many_arguments)]
fn prepare_segment_for_close(
    segment: &mut Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
    process_state: &ProcessState,
    checkpoint_received_at_ns: u64,
) -> anyhow::Result<()> {
    let catalog = config.segment_config();
    segment.update_catalog(
        catalog.symbols,
        catalog.security_token_symbols,
        catalog.excluded_symbols,
    );
    if !replay_checkpoint_ready(segment, states, process_state) {
        segment.mark_replay_unsafe();
    }
    if states.values().all(|state| state.synced) {
        let checkpoint_replay_safe = segment.is_replay_safe();
        write_checkpoints(
            segment,
            states,
            session_id,
            reason,
            checkpoint_replay_safe,
            checkpoint_received_at_ns,
        )?;
    } else {
        segment.mark_replay_unsafe();
    }
    Ok(())
}

async fn send_stream_event(
    sender: &mpsc::Sender<Event>,
    event: Event,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<TaskExit>> {
    match send_or_shutdown(sender, event, shutdown).await? {
        SendOutcome::Sent => Ok(None),
        SendOutcome::Shutdown(event) => Ok(Some(TaskExit::Stopped(Some(event)))),
    }
}

async fn acknowledge_rotation_pause(
    producer_id: usize,
    sender: &mpsc::Sender<Event>,
    rotation_pause: &mut watch::Receiver<u64>,
    rotation_resume: &mut watch::Receiver<u64>,
    last_pause_epoch: &mut u64,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<TaskExit>> {
    let epoch = *rotation_pause.borrow_and_update();
    if epoch <= *last_pause_epoch {
        return Ok(None);
    }
    *last_pause_epoch = epoch;
    // The controller waits for every marker before setting the next boundary.
    match send_or_shutdown(
        sender,
        Event::RotationBarrier { producer_id, epoch },
        shutdown,
    )
    .await?
    {
        SendOutcome::Sent => {}
        SendOutcome::Shutdown(event) => return Ok(Some(TaskExit::Stopped(Some(event)))),
    }
    loop {
        if *rotation_resume.borrow_and_update() >= epoch {
            return Ok(None);
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(Some(TaskExit::Stopped(None)));
                }
            }
            changed = rotation_resume.changed() => {
                changed.context("segment rotation controller stopped before producer resume")?;
            }
        }
    }
}

async fn wait_for_rotation_or_shutdown<T>(
    wait: impl Future<Output = T>,
    shutdown: &mut watch::Receiver<bool>,
    rotation_pause: &mut watch::Receiver<u64>,
) -> anyhow::Result<ProducerWait<T>> {
    if *shutdown.borrow() {
        return Ok(ProducerWait::Stopped);
    }
    tokio::select! {
        biased;
        changed = shutdown.changed() => {
            if changed.is_err() || *shutdown.borrow() {
                Ok(ProducerWait::Stopped)
            } else {
                anyhow::bail!("shutdown watch changed without shutdown")
            }
        }
        changed = rotation_pause.changed() => {
            changed.context("segment rotation controller stopped before producer pause")?;
            Ok(ProducerWait::PauseRequested)
        }
        value = wait => Ok(ProducerWait::Ready(value)),
    }
}

async fn wait_for_stream_reconnect(
    sender: &mpsc::Sender<Event>,
    producer_id: usize,
    rotation_pause: &mut watch::Receiver<u64>,
    rotation_resume: &mut watch::Receiver<u64>,
    last_pause_epoch: &mut u64,
    shutdown: &mut watch::Receiver<bool>,
    backoff: &mut u64,
) -> anyhow::Result<Option<TaskExit>> {
    if let Some(exit) = acknowledge_rotation_pause(
        producer_id,
        sender,
        rotation_pause,
        rotation_resume,
        last_pause_epoch,
        shutdown,
    )
    .await?
    {
        return Ok(Some(exit));
    }
    let delay = *backoff;
    *backoff = (*backoff * 2).min(30);
    match wait_for_rotation_or_shutdown(
        tokio::time::sleep(Duration::from_secs(delay)),
        shutdown,
        rotation_pause,
    )
    .await?
    {
        ProducerWait::Ready(()) => Ok(None),
        ProducerWait::Stopped => Ok(Some(TaskExit::Stopped(None))),
        ProducerWait::PauseRequested => {
            acknowledge_rotation_pause(
                producer_id,
                sender,
                rotation_pause,
                rotation_resume,
                last_pause_epoch,
                shutdown,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive_url(
    shard: StreamShard,
    sender: mpsc::Sender<Event>,
    stream_connected: mpsc::Sender<Vec<String>>,
    mut shutdown: watch::Receiver<bool>,
    stall_timeout: Duration,
    subscription_proof_timeout: Duration,
    watchdog: ProcessWatchdog,
    producer_id: usize,
    mut rotation_pause: watch::Receiver<u64>,
    mut rotation_resume: watch::Receiver<u64>,
) -> anyhow::Result<TaskExit> {
    let streams = shard.streams.iter().cloned().collect::<Vec<_>>();
    let mut coverage_announced = false;
    let mut reconnect_backoff = 1_u64;
    let mut last_pause_epoch = 0_u64;

    loop {
        if let Some(exit) = acknowledge_rotation_pause(
            producer_id,
            &sender,
            &mut rotation_pause,
            &mut rotation_resume,
            &mut last_pause_epoch,
            &mut shutdown,
        )
        .await?
        {
            return Ok(exit);
        }
        let connection = match wait_for_rotation_or_shutdown(
            tokio::time::timeout(Duration::from_secs(20), connect_async(&shard.url)),
            &mut shutdown,
            &mut rotation_pause,
        )
        .await?
        {
            ProducerWait::Ready(connection) => connection,
            ProducerWait::PauseRequested => {
                if let Some(exit) = acknowledge_rotation_pause(
                    producer_id,
                    &sender,
                    &mut rotation_pause,
                    &mut rotation_resume,
                    &mut last_pause_epoch,
                    &mut shutdown,
                )
                .await?
                {
                    return Ok(exit);
                }
                continue;
            }
            ProducerWait::Stopped => return Ok(TaskExit::Stopped(None)),
        };
        let mut websocket = match connection {
            Ok(Ok((websocket, _))) => websocket,
            Ok(Err(error)) if coverage_announced => {
                warn!(error = %error, "websocket reconnect failed");
                if let Some(exit) = wait_for_stream_reconnect(
                    &sender,
                    producer_id,
                    &mut rotation_pause,
                    &mut rotation_resume,
                    &mut last_pause_epoch,
                    &mut shutdown,
                    &mut reconnect_backoff,
                )
                .await?
                {
                    return Ok(exit);
                }
                continue;
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) if coverage_announced => {
                warn!("websocket reconnect timed out");
                if let Some(exit) = wait_for_stream_reconnect(
                    &sender,
                    producer_id,
                    &mut rotation_pause,
                    &mut rotation_resume,
                    &mut last_pause_epoch,
                    &mut shutdown,
                    &mut reconnect_backoff,
                )
                .await?
                {
                    return Ok(exit);
                }
                continue;
            }
            Err(_) => return Err(anyhow::anyhow!("websocket connect timed out")),
        };

        if let Some(exit) = acknowledge_rotation_pause(
            producer_id,
            &sender,
            &mut rotation_pause,
            &mut rotation_resume,
            &mut last_pause_epoch,
            &mut shutdown,
        )
        .await?
        {
            return Ok(exit);
        }

        if let Err(error) = websocket
            .send(Message::Text(
                json!({"method":"LIST_SUBSCRIPTIONS","id":SUBSCRIPTION_PROOF_ID})
                    .to_string()
                    .into(),
            ))
            .await
        {
            if !coverage_announced {
                return Err(error.into());
            }
            warn!(error = %error, "websocket subscription proof request failed");
            if let Some(exit) = wait_for_stream_reconnect(
                &sender,
                producer_id,
                &mut rotation_pause,
                &mut rotation_resume,
                &mut last_pause_epoch,
                &mut shutdown,
                &mut reconnect_backoff,
            )
            .await?
            {
                return Ok(exit);
            }
            continue;
        }

        let mut subscription_proof_deadline =
            tokio::time::Instant::now() + subscription_proof_timeout;
        let mut proof_failure = None;
        let mut proof_events = Vec::new();
        loop {
            let message = match wait_for_rotation_or_shutdown(
                receive_before_subscription_proof_deadline(
                    subscription_proof_deadline,
                    websocket.next(),
                ),
                &mut shutdown,
                &mut rotation_pause,
            )
            .await?
            {
                ProducerWait::Ready(message) => match message {
                    Ok(message) => match message {
                        Some(message) => match message {
                            Ok(message) => message,
                            Err(error) => {
                                proof_failure = Some(error.to_string());
                                break;
                            }
                        },
                        None => {
                            proof_failure =
                                Some("websocket closed before subscription proof".into());
                            break;
                        }
                    },
                    Err(error) => {
                        proof_failure = Some(error.to_string());
                        break;
                    }
                },
                ProducerWait::PauseRequested => {
                    let buffered_proof_interrupted = !proof_events.is_empty();
                    if let Some(exit) = acknowledge_rotation_pause(
                        producer_id,
                        &sender,
                        &mut rotation_pause,
                        &mut rotation_resume,
                        &mut last_pause_epoch,
                        &mut shutdown,
                    )
                    .await?
                    {
                        return Ok(exit);
                    }
                    if buffered_proof_interrupted {
                        proof_failure =
                            Some("segment rotation interrupted buffered subscription proof".into());
                        break;
                    }
                    subscription_proof_deadline =
                        tokio::time::Instant::now() + subscription_proof_timeout;
                    continue;
                }
                ProducerWait::Stopped => {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(5), websocket.close(None)).await;
                    return Ok(TaskExit::Stopped(None));
                }
            };
            if let Message::Text(text) = message {
                watchdog.mark_data();
                let received_at_ns = now_ns()?;
                let frame: Value = serde_json::from_str(&text)?;
                if frame.get("id").and_then(Value::as_u64) == Some(SUBSCRIPTION_PROOF_ID) {
                    let listed = validate_subscription_listing(&frame, &shard.streams)?;
                    if coverage_announced {
                        if let Some(exit) = send_stream_event(
                            &sender,
                            Event::StreamReconnected {
                                streams: streams.clone(),
                            },
                            &mut shutdown,
                        )
                        .await?
                        {
                            return Ok(exit);
                        }
                    } else {
                        stream_connected
                            .send(listed)
                            .await
                            .context("stream connection receiver dropped")?;
                        coverage_announced = true;
                    }
                    for event in proof_events.drain(..) {
                        match send_or_shutdown(&sender, event, &mut shutdown).await? {
                            SendOutcome::Sent => {}
                            SendOutcome::Shutdown(event) => {
                                return Ok(TaskExit::Stopped(Some(event)));
                            }
                        }
                    }
                    reconnect_backoff = 1;
                    break;
                }
                let event = event_from_frame(frame, received_at_ns)?;
                if coverage_announced {
                    if proof_events.len() >= MAX_RECONNECT_PROOF_BUFFERED_EVENTS {
                        anyhow::bail!(
                            "websocket reconnect proof buffer exceeded {} events",
                            MAX_RECONNECT_PROOF_BUFFERED_EVENTS
                        );
                    }
                    proof_events.push(event);
                } else {
                    match receive_before_subscription_proof_deadline(
                        subscription_proof_deadline,
                        send_or_shutdown(&sender, event, &mut shutdown),
                    )
                    .await??
                    {
                        SendOutcome::Sent => {}
                        SendOutcome::Shutdown(event) => {
                            return Ok(TaskExit::Stopped(Some(event)));
                        }
                    }
                }
            }
        }
        if let Some(reason) = proof_failure {
            if !coverage_announced {
                return Err(anyhow::anyhow!(reason));
            }
            warn!(reason, "websocket subscription proof failed; reconnecting");
            if let Some(exit) = send_stream_event(
                &sender,
                Event::StreamDisconnected {
                    streams: streams.clone(),
                    reason,
                },
                &mut shutdown,
            )
            .await?
            {
                return Ok(exit);
            }
            if let Some(exit) = wait_for_stream_reconnect(
                &sender,
                producer_id,
                &mut rotation_pause,
                &mut rotation_resume,
                &mut last_pause_epoch,
                &mut shutdown,
                &mut reconnect_backoff,
            )
            .await?
            {
                return Ok(exit);
            }
            continue;
        }

        // Only verified exact stream coverage unlocks snapshot requests. Market
        // events received while waiting for the proof were already buffered above.
        let reason = loop {
            if let Some(exit) = acknowledge_rotation_pause(
                producer_id,
                &sender,
                &mut rotation_pause,
                &mut rotation_resume,
                &mut last_pause_epoch,
                &mut shutdown,
            )
            .await?
            {
                return Ok(exit);
            }
            let message = tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    changed?;
                    let _ = tokio::time::timeout(Duration::from_secs(5), websocket.close(None)).await;
                    return Ok(TaskExit::Stopped(None));
                }
                changed = rotation_pause.changed() => {
                    changed.context("segment rotation controller stopped before producer pause")?;
                    continue;
                }
                message = tokio::time::timeout(stall_timeout, websocket.next()) => match message {
                    Ok(Some(Ok(message))) => message,
                    Ok(Some(Err(error))) => break format!("websocket receive failed: {error}"),
                    Ok(None) => break "websocket closed".into(),
                    Err(_) => break format!("websocket shard stalled for {}s", stall_timeout.as_secs()),
                }
            };
            if let Message::Text(text) = message {
                watchdog.mark_data();
                let received_at_ns = now_ns()?;
                let event = event_from_frame(serde_json::from_str(&text)?, received_at_ns)?;
                match send_or_shutdown(&sender, event, &mut shutdown).await? {
                    SendOutcome::Sent => {}
                    SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
                }
            }
        };
        warn!(reason, "websocket shard disconnected; reconnecting");
        if let Some(exit) = send_stream_event(
            &sender,
            Event::StreamDisconnected {
                streams: streams.clone(),
                reason,
            },
            &mut shutdown,
        )
        .await?
        {
            return Ok(exit);
        }
        if let Some(exit) = wait_for_stream_reconnect(
            &sender,
            producer_id,
            &mut rotation_pause,
            &mut rotation_resume,
            &mut last_pause_epoch,
            &mut shutdown,
            &mut reconnect_backoff,
        )
        .await?
        {
            return Ok(exit);
        }
    }
}

async fn receive_before_subscription_proof_deadline<F, T>(
    deadline: tokio::time::Instant,
    next: F,
) -> anyhow::Result<T>
where
    F: Future<Output = T>,
{
    if tokio::time::Instant::now() >= deadline {
        bail!("websocket subscription proof timed out");
    }
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => {
            bail!("websocket subscription proof timed out")
        }
        value = next => {
            if tokio::time::Instant::now() >= deadline {
                bail!("websocket subscription proof timed out");
            }
            Ok(value)
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn produce_snapshots_after_streams_connect(
    config: Arc<Config>,
    sender: mpsc::Sender<Event>,
    mut shutdown: watch::Receiver<bool>,
    mut stream_connected: mpsc::Receiver<Vec<String>>,
    expected_shards: usize,
    producer_id: usize,
    mut rotation_pause: watch::Receiver<u64>,
    mut rotation_resume: watch::Receiver<u64>,
) -> anyhow::Result<TaskExit> {
    let mut shards = Vec::with_capacity(expected_shards);
    let mut last_pause_epoch = 0_u64;
    while shards.len() < expected_shards {
        if let Some(exit) = acknowledge_rotation_pause(
            producer_id,
            &sender,
            &mut rotation_pause,
            &mut rotation_resume,
            &mut last_pause_epoch,
            &mut shutdown,
        )
        .await?
        {
            return Ok(exit);
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                changed?;
                return Ok(TaskExit::Stopped(None));
            }
            changed = rotation_pause.changed() => {
                changed.context("segment rotation controller stopped before snapshot producer pause")?;
            }
            connected = stream_connected.recv() => {
                shards.push(connected.context("websocket producer stopped before connecting")?);
            }
        }
    }
    shards.sort();
    match send_or_shutdown(
        &sender,
        Event::StreamCoverageVerified { shards },
        &mut shutdown,
    )
    .await?
    {
        SendOutcome::Sent => {}
        SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
    }
    produce_snapshots(
        config,
        sender,
        shutdown,
        producer_id,
        rotation_pause,
        rotation_resume,
        last_pause_epoch,
    )
    .await
}

fn validate_subscription_listing(
    frame: &Value,
    expected: &BTreeSet<String>,
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(
        frame.get("id").and_then(Value::as_u64) == Some(SUBSCRIPTION_PROOF_ID),
        "subscription proof response has the wrong id"
    );
    let listed = frame
        .get("result")
        .and_then(Value::as_array)
        .context("subscription proof response has no result array")?;
    let actual = listed
        .iter()
        .map(|stream| {
            stream
                .as_str()
                .map(str::to_owned)
                .context("subscription proof contains a non-string stream")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    anyhow::ensure!(
        actual.len() == listed.len(),
        "subscription coverage mismatch: duplicate streams returned"
    );
    anyhow::ensure!(
        &actual == expected,
        "subscription coverage mismatch: expected={expected:?} actual={actual:?}"
    );
    Ok(actual.into_iter().collect())
}

fn event_from_frame(frame: Value, received_at_ns: u64) -> anyhow::Result<Event> {
    let stream = frame
        .get("stream")
        .and_then(Value::as_str)
        .context("Binance combined frame has no stream")?;
    if stream.contains("@depth") {
        let source_clock = DepthSourceClock::from_frame(&frame, received_at_ns)?;
        let diff = DepthDiff::from_frame(&frame)?;
        return Ok(Event::Diff {
            received_at_ns,
            frame,
            depth: Box::new(ValidatedDepth { diff, source_clock }),
        });
    }
    if !stream
        .rsplit_once('@')
        .is_some_and(|(_, channel)| channel.eq_ignore_ascii_case("aggTrade"))
    {
        anyhow::bail!("unsupported Binance research stream {stream}");
    }
    let trade = AggregateTrade::from_frame(&frame, received_at_ns)?;
    Ok(Event::AggregateTrade { trade, frame })
}

async fn produce_snapshots(
    config: Arc<Config>,
    sender: mpsc::Sender<Event>,
    mut shutdown: watch::Receiver<bool>,
    producer_id: usize,
    mut rotation_pause: watch::Receiver<u64>,
    mut rotation_resume: watch::Receiver<u64>,
    mut last_pause_epoch: u64,
) -> anyhow::Result<TaskExit> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let interval = Duration::from_secs_f64(1.0 / config.snapshot_requests_per_second.max(0.1));
    'symbols: for symbol in config.active_symbols() {
        let started = now_ns()?;
        let mut attempt = 0_usize;
        let mut retry_deadline = None;
        let snapshot = loop {
            if let Some(exit) = acknowledge_rotation_pause(
                producer_id,
                &sender,
                &mut rotation_pause,
                &mut rotation_resume,
                &mut last_pause_epoch,
                &mut shutdown,
            )
            .await?
            {
                return Ok(exit);
            }
            if *shutdown.borrow() {
                return Ok(TaskExit::Stopped(None));
            }
            if let Some(deadline) = retry_deadline {
                match wait_for_rotation_or_shutdown(
                    tokio::time::sleep_until(deadline),
                    &mut shutdown,
                    &mut rotation_pause,
                )
                .await?
                {
                    ProducerWait::Ready(()) => retry_deadline = None,
                    ProducerWait::Stopped => return Ok(TaskExit::Stopped(None)),
                    ProducerWait::PauseRequested => {
                        if let Some(exit) = acknowledge_rotation_pause(
                            producer_id,
                            &sender,
                            &mut rotation_pause,
                            &mut rotation_resume,
                            &mut last_pause_epoch,
                            &mut shutdown,
                        )
                        .await?
                        {
                            return Ok(exit);
                        }
                        continue;
                    }
                }
            }
            match wait_for_rotation_or_shutdown(
                fetch_snapshot_attempt(&client, &config, &symbol, attempt),
                &mut shutdown,
                &mut rotation_pause,
            )
            .await?
            {
                ProducerWait::Ready(Ok(SnapshotFetchAttempt::Snapshot(snapshot))) => {
                    break snapshot;
                }
                ProducerWait::Ready(Ok(SnapshotFetchAttempt::Retry(delay))) => {
                    attempt += 1;
                    retry_deadline = Some(tokio::time::Instant::now() + delay);
                }
                ProducerWait::Stopped => return Ok(TaskExit::Stopped(None)),
                ProducerWait::PauseRequested => {
                    if let Some(exit) = acknowledge_rotation_pause(
                        producer_id,
                        &sender,
                        &mut rotation_pause,
                        &mut rotation_resume,
                        &mut last_pause_epoch,
                        &mut shutdown,
                    )
                    .await?
                    {
                        return Ok(exit);
                    }
                }
                ProducerWait::Ready(Err(error)) => {
                    if error.downcast_ref::<SnapshotUnavailable>().is_some() {
                        let event = Event::ExcludeSymbol {
                            symbol: symbol.clone(),
                            reason: error.to_string(),
                        };
                        match send_or_shutdown(&sender, event, &mut shutdown).await? {
                            SendOutcome::Sent => continue 'symbols,
                            SendOutcome::Shutdown(event) => {
                                return Ok(TaskExit::Stopped(Some(event)));
                            }
                        }
                    }
                    return Err(error);
                }
            }
        };
        if snapshot
            .get("bids")
            .and_then(Value::as_array)
            .zip(snapshot.get("asks").and_then(Value::as_array))
            .is_some_and(|(bids, asks)| bids.is_empty() || asks.is_empty())
        {
            let event = Event::ExcludeSymbol {
                symbol: symbol.clone(),
                reason: "one-sided initial snapshot is not replay-complete".to_owned(),
            };
            match send_or_shutdown(&sender, event, &mut shutdown).await? {
                SendOutcome::Sent => {
                    match wait_for_rotation_or_shutdown(
                        tokio::time::sleep(interval),
                        &mut shutdown,
                        &mut rotation_pause,
                    )
                    .await?
                    {
                        ProducerWait::Ready(()) => continue 'symbols,
                        ProducerWait::Stopped => return Ok(TaskExit::Stopped(None)),
                        ProducerWait::PauseRequested => {
                            if let Some(exit) = acknowledge_rotation_pause(
                                producer_id,
                                &sender,
                                &mut rotation_pause,
                                &mut rotation_resume,
                                &mut last_pause_epoch,
                                &mut shutdown,
                            )
                            .await?
                            {
                                return Ok(exit);
                            }
                            continue 'symbols;
                        }
                    }
                }
                SendOutcome::Shutdown(event) => {
                    return Ok(TaskExit::Stopped(Some(event)));
                }
            }
        }
        let received_at_ns = now_ns()?;
        let event = Event::Snapshot {
            received_at_ns,
            symbol: symbol.clone(),
            request_started_at_ns: started,
            snapshot,
        };
        match send_or_shutdown(&sender, event, &mut shutdown).await? {
            SendOutcome::Sent => {}
            SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
        }
        match wait_for_rotation_or_shutdown(
            tokio::time::sleep(interval),
            &mut shutdown,
            &mut rotation_pause,
        )
        .await?
        {
            ProducerWait::Ready(()) => {}
            ProducerWait::Stopped => return Ok(TaskExit::Stopped(None)),
            ProducerWait::PauseRequested => {
                if let Some(exit) = acknowledge_rotation_pause(
                    producer_id,
                    &sender,
                    &mut rotation_pause,
                    &mut rotation_resume,
                    &mut last_pause_epoch,
                    &mut shutdown,
                )
                .await?
                {
                    return Ok(exit);
                }
            }
        }
    }
    match send_or_shutdown(&sender, Event::InitialSnapshotsComplete, &mut shutdown).await? {
        SendOutcome::Sent => {}
        SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
    }
    // Keep the producer alive so a normal completion is not mistaken for a session failure.
    loop {
        if let Some(exit) = acknowledge_rotation_pause(
            producer_id,
            &sender,
            &mut rotation_pause,
            &mut rotation_resume,
            &mut last_pause_epoch,
            &mut shutdown,
        )
        .await?
        {
            return Ok(exit);
        }
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                changed?;
                return Ok(TaskExit::Stopped(None));
            }
            changed = rotation_pause.changed() => {
                changed.context("segment rotation controller stopped before snapshot producer pause")?;
            }
        }
    }
}

enum SnapshotFetchAttempt {
    Snapshot(Value),
    Retry(Duration),
}

async fn fetch_snapshot_attempt(
    client: &reqwest::Client,
    config: &Config,
    symbol: &str,
    attempt: usize,
) -> anyhow::Result<SnapshotFetchAttempt> {
    let path = match config.market {
        Market::Spot => "/api/v3/depth",
        Market::Usdm => "/fapi/v1/depth",
    };
    let limit = config.snapshot_limit.to_string();
    let response = client
        .get(format!("{}{path}", config.rest_base))
        .query(&[("symbol", symbol), ("limit", limit.as_str())])
        .send()
        .await?;
    let status = response.status();
    if status.is_success() {
        let snapshot = response.json::<Value>().await?;
        if snapshot
            .get("lastUpdateId")
            .and_then(Value::as_u64)
            .is_none()
        {
            anyhow::bail!("snapshot missing lastUpdateId for {symbol}");
        }
        return Ok(SnapshotFetchAttempt::Snapshot(snapshot));
    }
    let retryable = status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    if !retryable || attempt + 1 == config.snapshot_retry_attempts {
        let body = response.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::BAD_REQUEST
            && serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|payload| payload["code"].as_i64())
                == Some(-1121)
        {
            return Err(SnapshotUnavailable {
                symbol: symbol.to_owned(),
                status,
            }
            .into());
        }
        anyhow::bail!("snapshot failed symbol={symbol} status={status} body={body}");
    }
    let delay = snapshot_retry_delay(
        response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok()),
        attempt,
    );
    warn!(%symbol, %status, ?delay, "snapshot request retrying");
    Ok(SnapshotFetchAttempt::Retry(delay))
}

#[cfg(test)]
async fn fetch_snapshot(
    client: &reqwest::Client,
    config: &Config,
    symbol: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<Value>> {
    for attempt in 0..config.snapshot_retry_attempts {
        match fetch_snapshot_attempt(client, config, symbol, attempt).await? {
            SnapshotFetchAttempt::Snapshot(snapshot) => return Ok(Some(snapshot)),
            SnapshotFetchAttempt::Retry(delay) => {
                if wait_or_shutdown(shutdown, delay).await {
                    return Ok(None);
                }
            }
        }
    }
    unreachable!("snapshot_retry_attempts is at least one")
}

#[cfg(test)]
async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        biased;
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
        _ = tokio::time::sleep(delay) => false,
    }
}

fn snapshot_retry_delay(retry_after: Option<&str>, attempt: usize) -> Duration {
    retry_after
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or_else(|| Duration::from_secs(1_u64 << attempt.min(5)))
        .min(Duration::from_secs(60))
}

async fn discover_symbols(market: Market, rest_base: &str) -> anyhow::Result<SymbolCatalog> {
    discover_symbols_with_timeout(market, rest_base, Duration::from_secs(15)).await
}

async fn discover_symbols_with_timeout(
    market: Market,
    rest_base: &str,
    timeout: Duration,
) -> anyhow::Result<SymbolCatalog> {
    let path = match market {
        Market::Spot => "/api/v3/exchangeInfo",
        Market::Usdm => "/fapi/v1/exchangeInfo",
    };
    let payload: Value = reqwest::Client::builder()
        .timeout(timeout)
        .build()?
        .get(format!("{rest_base}{path}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    parse_symbol_catalog(market, &payload)
}

fn parse_symbol_catalog(market: Market, payload: &Value) -> anyhow::Result<SymbolCatalog> {
    let entries = payload["symbols"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("exchangeInfo missing symbols"))?;
    let mut symbols = BTreeSet::new();
    let mut security_tokens = BTreeSet::new();
    let mut excluded = BTreeSet::new();
    for item in entries {
        let Some(symbol) = item["symbol"].as_str().map(str::to_owned) else {
            continue;
        };
        if item["status"] != "TRADING" {
            continue;
        }
        if market == Market::Usdm && item["contractType"] != "PERPETUAL" {
            continue;
        }
        if market == Market::Spot
            && item.get("isSpotTradingAllowed").and_then(Value::as_bool) == Some(false)
        {
            excluded.insert(symbol);
            continue;
        }
        let is_security_token = item["permissionSets"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group.as_array().into_iter().flatten())
            .any(|permission| permission == "TRD_GRP_261");
        if is_security_token {
            security_tokens.insert(symbol.clone());
        }
        symbols.insert(symbol);
    }
    Ok(SymbolCatalog {
        symbols: symbols.into_iter().collect(),
        security_token_symbols: security_tokens.into_iter().collect(),
        excluded_symbols: excluded.into_iter().collect(),
    })
}

async fn upload_loop(config: Arc<Config>, mut shutdown: watch::Receiver<bool>) {
    let upload_config = UploadConfig::from(config.as_ref());
    loop {
        if let Err(error) = upload_pending_with_status(&upload_config).await {
            warn!(error = %error, "pending OSS upload failed; files retained");
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(300)) => {}
        }
    }
}

async fn upload_only(config: &UploadConfig) -> anyhow::Result<()> {
    if !config.spool_dir.is_dir() {
        anyhow::bail!(
            "SPOOL_DIR is not a directory: {}",
            config.spool_dir.display()
        );
    }
    let _spool_lock = SpoolLock::acquire(&config.spool_dir)?;
    let mut incomplete = Vec::new();
    for suffix in [".jsonl.part", ".zst.tmp", ".part.corrupt"] {
        incomplete.extend(files_with_suffix(&config.spool_dir, suffix)?);
    }
    if !incomplete.is_empty() {
        anyhow::bail!(
            "upload-only drain blocked by {} incomplete segment artifacts; recover them with the collector release that created them",
            incomplete.len()
        );
    }
    let uploaded = upload_pending_with_status(config).await?;
    let pending = files_with_suffix(&config.spool_dir, ".manifest.json")?.len();
    if pending > 0 {
        anyhow::bail!("upload-only drain incomplete: {pending} manifests remain");
    }
    let mut residual = Vec::new();
    for suffix in [
        ".jsonl.part",
        ".zst.tmp",
        ".part.corrupt",
        ".jsonl.zst",
        "._SUCCESS",
        UPLOADED_CLEANUP_SUFFIX,
        UPLOADED_CLEANUP_TMP_SUFFIX,
    ] {
        residual.extend(files_with_suffix(&config.spool_dir, suffix)?);
    }
    if !residual.is_empty() {
        anyhow::bail!(
            "upload-only drain incomplete: {} local segment artifacts remain",
            residual.len()
        );
    }
    println!("upload-only: uploaded={uploaded} pending=0");
    Ok(())
}

async fn upload_pending_with_status(config: &UploadConfig) -> anyhow::Result<usize> {
    let mut status = read_upload_status(&config.spool_dir);
    let result = upload_pending(config).await;
    match &result {
        Ok(uploaded) => {
            if *uploaded > 0 {
                status.last_success_at = Some(chrono::Utc::now().to_rfc3339());
            }
            status.last_error_at = None;
            status.last_error = None;
        }
        Err(error) => {
            status.last_error_at = Some(chrono::Utc::now().to_rfc3339());
            status.last_error = Some(error.to_string().chars().take(500).collect());
            status.failure_count = status.failure_count.saturating_add(1);
        }
    }
    write_upload_status(&config.spool_dir, &status)
        .context("failed to persist OSS upload status")?;
    result
}

async fn upload_pending(config: &UploadConfig) -> anyhow::Result<usize> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        let recovered = recover_uploaded_cleanups(&config.spool_dir)?;
        if recovered > 0 {
            info!(recovered, "completed interrupted local upload cleanup");
        }
        let mut failures = 0_usize;
        let mut uploaded = 0_usize;
        for manifest in files_with_suffix(&config.spool_dir, ".manifest.json")? {
            if let Err(error) = upload_one(&config, &manifest) {
                failures += 1;
                error!(manifest = %manifest.display(), error = %error, "OSS upload retained for retry");
            } else {
                uploaded += 1;
            }
        }
        if failures > 0 {
            anyhow::bail!("{failures} pending OSS uploads failed");
        }
        anyhow::Ok(uploaded)
    })
    .await?
}

fn write_uploaded_cleanup_marker(
    data: &Path,
    manifest: &Path,
    success: &Path,
) -> anyhow::Result<PathBuf> {
    let parent = manifest
        .parent()
        .ok_or_else(|| anyhow::anyhow!("manifest has no parent: {}", manifest.display()))?;
    if data.parent() != Some(parent) || success.parent() != Some(parent) {
        anyhow::bail!("uploaded cleanup artifacts must share one directory");
    }
    let manifest_name = local_file_name(manifest)?;
    let marker = manifest.with_file_name(format!("{manifest_name}{UPLOADED_CLEANUP_SUFFIX}"));
    let temporary = marker.with_file_name(format!("{}.tmp", local_file_name(&marker)?));
    match std::fs::symlink_metadata(&marker) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => anyhow::bail!(
            "uploaded cleanup marker already exists: {}",
            marker.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect {}", marker.display()))
        }
    }
    let mut bytes = serde_json::to_vec(&json!({
        "schema": UPLOADED_CLEANUP_SCHEMA,
        "data": local_file_name(data)?,
        "manifest": manifest_name,
        "success": local_file_name(success)?,
    }))?;
    bytes.push(b'\n');
    let mut output = create_cleanup_temporary(&temporary)?;
    std::io::Write::write_all(&mut output, &bytes)?;
    output.sync_all()?;
    std::fs::rename(&temporary, &marker)?;
    sync_parent_directory(&marker)?;
    Ok(marker)
}

fn create_cleanup_temporary(path: &Path) -> anyhow::Result<std::fs::File> {
    for attempt in 0..2 {
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                let metadata = std::fs::symlink_metadata(path).with_context(|| {
                    format!("failed to inspect stale cleanup temp {}", path.display())
                })?;
                if !metadata.file_type().is_file() {
                    anyhow::bail!("refusing non-regular cleanup temp path: {}", path.display());
                }
                std::fs::remove_file(path).with_context(|| {
                    format!("failed to remove stale cleanup temp {}", path.display())
                })?;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create cleanup temp {}", path.display()));
            }
        }
    }
    unreachable!("cleanup temp creation retries are bounded")
}

fn recover_uploaded_cleanups(spool_dir: &Path) -> anyhow::Result<usize> {
    let markers = files_with_suffix(spool_dir, UPLOADED_CLEANUP_SUFFIX)?;
    for marker in &markers {
        cleanup_uploaded_marker(marker)?;
    }
    Ok(markers.len())
}

fn cleanup_uploaded_marker(marker: &Path) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(marker) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => anyhow::bail!(
            "refusing non-regular uploaded cleanup marker: {}",
            marker.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect uploaded cleanup marker {}",
                    marker.display()
                )
            });
        }
    }
    let marker_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(marker)
        .with_context(|| {
            format!(
                "failed to open uploaded cleanup marker {}",
                marker.display()
            )
        })?;
    if !marker_file
        .metadata()
        .with_context(|| {
            format!(
                "failed to inspect uploaded cleanup marker {}",
                marker.display()
            )
        })?
        .file_type()
        .is_file()
    {
        anyhow::bail!(
            "uploaded cleanup marker is not a regular file: {}",
            marker.display()
        );
    }
    let metadata: Value = serde_json::from_reader(marker_file)
        .with_context(|| format!("invalid uploaded cleanup marker {}", marker.display()))?;
    if metadata["schema"] != UPLOADED_CLEANUP_SCHEMA {
        anyhow::bail!(
            "invalid uploaded cleanup marker schema: {}",
            marker.display()
        );
    }
    let paths = cleanup_artifact_paths(marker, &metadata)?;
    match std::fs::symlink_metadata(marker) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => anyhow::bail!(
            "refusing non-regular uploaded cleanup marker: {}",
            marker.display()
        ),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect uploaded cleanup marker {}",
                    marker.display()
                )
            });
        }
    }
    for path in &paths {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                anyhow::bail!("refusing non-regular uploaded artifact: {}", path.display())
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect uploaded artifact {}", path.display())
                });
            }
        }
    }
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to clean uploaded artifact {}", path.display())
                });
            }
        }
    }
    sync_parent_directory(marker)?;
    std::fs::remove_file(marker)?;
    sync_parent_directory(marker)
}

fn cleanup_artifact_paths(marker: &Path, metadata: &Value) -> anyhow::Result<[PathBuf; 3]> {
    let marker_name = local_file_name(marker)?;
    let manifest_name = marker_name
        .strip_suffix(UPLOADED_CLEANUP_SUFFIX)
        .ok_or_else(|| anyhow::anyhow!("invalid uploaded cleanup marker name: {marker_name}"))?;
    let data_name = manifest_name
        .strip_suffix(".manifest.json")
        .ok_or_else(|| {
            anyhow::anyhow!("cleanup marker is not bound to a manifest: {marker_name}")
        })?;
    let _segment_id = data_name
        .strip_prefix("part-")
        .and_then(|value| value.strip_suffix(".jsonl.zst"))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or_else(|| {
            anyhow::anyhow!("cleanup marker has an invalid segment name: {marker_name}")
        })?;
    let success_name = format!("{data_name}._SUCCESS");
    let expected = [
        ("data", data_name),
        ("manifest", manifest_name),
        ("success", success_name.as_str()),
    ];
    for (field, expected_name) in expected {
        let actual = metadata[field]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("uploaded cleanup marker missing {field}"))?;
        if actual != expected_name {
            anyhow::bail!(
                "uploaded cleanup marker {field} does not match its segment: expected {expected_name}, got {actual}"
            );
        }
    }
    let parent = marker
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cleanup marker has no parent: {}", marker.display()))?;
    Ok([
        parent.join(data_name),
        parent.join(manifest_name),
        parent.join(success_name),
    ])
}

fn local_file_name(path: &Path) -> anyhow::Result<&str> {
    path.file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("path has no UTF-8 file name: {}", path.display()))
}

fn sync_parent_directory(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn upload_one(config: &UploadConfig, manifest: &Path) -> anyhow::Result<()> {
    let metadata: Value = serde_json::from_reader(std::fs::File::open(manifest)?)?;
    let data = manifest.with_file_name(
        metadata["file"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("manifest missing file"))?,
    );
    let digest = metadata["sha256"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("manifest missing sha256"))?;
    let success = write_success_marker(&data, digest)?;
    let prefix = format!(
        "lake/raw/venue=binance/market={}/dataset={}/shard={}/date={}/hour={}",
        metadata["market"].as_str().unwrap_or_default(),
        metadata["dataset"].as_str().unwrap_or_default(),
        metadata["shard_id"].as_str().unwrap_or_default(),
        metadata["date"].as_str().unwrap_or_default(),
        metadata["hour"].as_str().unwrap_or_default(),
    );
    for source in [&data, manifest, &success] {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let mut command = Command::new("aliyun");
        command
            .args(["ossutil", "cp"])
            .arg(source)
            .arg(format!("oss://{}/{prefix}/{name}", config.oss_bucket))
            .args([
                "--profile",
                &config.aliyun_profile,
                "--endpoint",
                &config.oss_endpoint,
                "--region",
                &config.oss_region,
                "--force",
            ]);
        let status = command_status_with_timeout(&mut command, config.oss_copy_timeout)?;
        if !status.success() {
            anyhow::bail!(
                "aliyun ossutil failed for {} with {status}",
                source.display()
            );
        }
    }
    let marker = write_uploaded_cleanup_marker(&data, manifest, &success)?;
    cleanup_uploaded_marker(&marker)
}

async fn wait_for_signal(shutdown: watch::Sender<bool>, watchdog: ProcessWatchdog) {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    publish_global_shutdown(&shutdown, &watchdog);
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name)
        .unwrap_or_else(|_| default.to_owned())
        .trim()
        .to_owned()
}

fn env_parse<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr + std::fmt::Display,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse()
        .with_context(|| format!("invalid {name}"))
}

fn now_ns() -> anyhow::Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

fn self_test() -> anyhow::Result<()> {
    let mut budget = PendingBudget::new(10);
    let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
    state.install_snapshot(
        &json!({
            "lastUpdateId": 100,
            "bids": [["100.00000000", "2.00000000"]],
            "asks": [["101.00000000", "3.00000000"]],
        }),
        &mut budget,
    )?;
    let root = env::temp_dir().join(format!("monday-lob-self-test-{}", std::process::id()));
    let mut segment = Segment::create(
        SegmentConfig {
            spool_dir: root.clone(),
            market: Market::Spot,
            dataset: "spot_all".into(),
            shard_id: "all".into(),
            symbols: vec!["BTCUSDT".into()],
            security_token_symbols: vec![],
            excluded_symbols: vec![],
            snapshot_limit: 100,
            zstd_timeout: Duration::from_secs(30),
        },
        1_700_000_000_000_000_000,
    )?;
    segment.write(
        "checkpoint",
        serde_json::to_value(state.checkpoint("self-test")?)?,
        segment.start_ns,
    )?;
    let artifacts = segment.close()?.expect("non-empty self-test segment");
    assert_eq!(
        serde_json::from_reader::<_, Value>(std::fs::File::open(artifacts.manifest)?)?["schema"],
        RAW_SCHEMA
    );
    std::fs::remove_dir_all(root)?;
    println!("self-test: ok");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Barrier;

    #[test]
    fn version_reports_bound_source_revision() {
        let error = Args::try_parse_from(["binance-lob-archiver", "--version"])
            .expect_err("--version must exit after displaying build identity");
        assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(BUILD_SOURCE_REVISION));
    }

    #[test]
    fn upload_only_cli_is_explicit_and_exclusive() {
        let args = Args::try_parse_from(["binance-lob-archiver", "--upload-only"]).unwrap();
        assert!(args.upload_only);
        assert!(!args.self_test);
        assert!(
            Args::try_parse_from(["binance-lob-archiver", "--upload-only", "--self-test",])
                .is_err()
        );
    }

    #[test]
    fn verify_segments_cli_requires_equal_explicit_trust_anchors() {
        let args = Args::try_parse_from([
            "binance-lob-archiver",
            "--verify-segment",
            "/tmp/part-1.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
            "--segment-manifest-sha256",
            &"b".repeat(64),
            "--verify-segment",
            "/tmp/part-2.jsonl.zst",
            "--segment-content-sha256",
            &"c".repeat(64),
            "--segment-manifest-sha256",
            &"d".repeat(64),
        ])
        .unwrap();
        assert_eq!(args.verify_segment.len(), 2);
        assert_eq!(args.segment_content_sha256.len(), 2);
        assert_eq!(args.segment_manifest_sha256.len(), 2);

        let unequal = Args::try_parse_from([
            "binance-lob-archiver",
            "--verify-segment",
            "/tmp/part.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
            "--segment-content-sha256",
            &"b".repeat(64),
            "--segment-manifest-sha256",
            &"c".repeat(64),
        ])
        .unwrap();
        assert!(verify_segments(&unequal)
            .unwrap_err()
            .to_string()
            .contains("must have equal nonzero lengths"));

        assert!(Args::try_parse_from([
            "binance-lob-archiver",
            "--verify-segment",
            "/tmp/part.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
        ])
        .is_err());
        assert!(Args::try_parse_from([
            "binance-lob-archiver",
            "--self-test",
            "--verify-segment",
            "/tmp/part.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
            "--segment-manifest-sha256",
            &"b".repeat(64),
        ])
        .is_err());
    }

    #[test]
    fn strict_lob_continuity_is_an_explicit_segment_verifier_mode() {
        let args = Args::try_parse_from([
            "binance-lob-archiver",
            "--verify-segment",
            "/tmp/part.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
            "--segment-manifest-sha256",
            &"b".repeat(64),
            "--require-lob-continuity",
        ])
        .unwrap();
        assert!(args.require_lob_continuity);
        assert!(
            Args::try_parse_from(["binance-lob-archiver", "--require-lob-continuity",]).is_err()
        );

        let aggregate = Args::try_parse_from([
            "binance-lob-archiver",
            "--verify-segment",
            "/tmp/part.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
            "--segment-manifest-sha256",
            &"b".repeat(64),
            "--verify-aggregate-trade-continuity",
        ])
        .unwrap();
        assert!(aggregate.verify_aggregate_trade_continuity);
        assert!(Args::try_parse_from([
            "binance-lob-archiver",
            "--verify-segment",
            "/tmp/part.jsonl.zst",
            "--segment-content-sha256",
            &"a".repeat(64),
            "--segment-manifest-sha256",
            &"b".repeat(64),
            "--verify-aggregate-trade-continuity",
            "--require-lob-continuity",
        ])
        .is_err());
    }

    #[tokio::test]
    async fn upload_only_rejects_a_missing_spool() {
        let spool_dir =
            env::temp_dir().join(format!("monday-upload-only-missing-{}", now_ns().unwrap()));
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        let error = upload_only(&config).await.unwrap_err();
        assert!(error.to_string().contains("SPOOL_DIR is not a directory"));
        assert!(!spool_dir.exists());
    }

    #[tokio::test]
    async fn upload_only_accepts_an_empty_spool_without_network_access() {
        let spool_dir =
            env::temp_dir().join(format!("monday-upload-only-empty-{}", now_ns().unwrap()));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        upload_only(&config).await.unwrap();
        assert!(spool_dir.join("upload-status.json").is_file());
        assert!(spool_dir.join(SPOOL_LOCK_FILE).is_file());
        assert!(files_with_suffix(&spool_dir, ".manifest.json")
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn upload_only_fails_while_spool_lock_is_held_and_succeeds_after_release() {
        let spool_dir = env::temp_dir().join(format!("monday-spool-lock-{}", now_ns().unwrap()));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        let first = SpoolLock::acquire(&spool_dir).unwrap();
        let error = upload_only(&config).await.unwrap_err();
        assert!(error.to_string().contains("spool is already locked"));
        assert!(!spool_dir.join("upload-status.json").exists());

        drop(first);
        upload_only(&config).await.unwrap();
        assert!(spool_dir.join(SPOOL_LOCK_FILE).is_file());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn upload_only_rejects_a_symlink_spool_lock_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let temp_dir = tempfile::tempdir().unwrap();
        let spool_dir = temp_dir.path().join("spool");
        std::fs::create_dir_all(&spool_dir).unwrap();
        let victim = spool_dir.join("victim");
        std::fs::write(&victim, b"keep-me").unwrap();
        let lock_path = spool_dir.join(SPOOL_LOCK_FILE);
        symlink(&victim, &lock_path).unwrap();
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        let error = upload_only(&config).await.unwrap_err();
        assert!(error.to_string().contains("failed to open spool lock"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep-me");
        assert!(std::fs::symlink_metadata(&lock_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(!spool_dir.join("upload-status.json").exists());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn upload_only_rejects_an_interrupted_segment() {
        for suffix in ["jsonl.part", "jsonl.zst.tmp", "jsonl.part.corrupt"] {
            let spool_dir = env::temp_dir().join(format!(
                "monday-upload-only-interrupted-{suffix}-{}",
                now_ns().unwrap()
            ));
            std::fs::create_dir_all(&spool_dir).unwrap();
            let artifact = spool_dir.join(format!("part-1700000000000000000.{suffix}"));
            std::fs::write(&artifact, b"unfinished").unwrap();
            let config = UploadConfig {
                spool_dir: spool_dir.clone(),
                oss_bucket: "unused".into(),
                oss_endpoint: "unused".into(),
                oss_region: "ap-northeast-1".into(),
                aliyun_profile: "unused".into(),
                oss_copy_timeout: Duration::from_secs(1),
            };

            let error = upload_only(&config).await.unwrap_err();
            assert!(error.to_string().contains("incomplete segment artifacts"));
            assert!(artifact.is_file());
            std::fs::remove_dir_all(spool_dir).unwrap();
        }
    }

    #[tokio::test]
    async fn upload_only_rejects_an_orphaned_compressed_segment() {
        let spool_dir =
            env::temp_dir().join(format!("monday-upload-only-zst-{}", now_ns().unwrap()));
        std::fs::create_dir_all(&spool_dir).unwrap();
        std::fs::write(
            spool_dir.join("part-1700000000000000000.jsonl.zst"),
            b"orphaned",
        )
        .unwrap();
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        let error = upload_only(&config).await.unwrap_err();
        assert!(error.to_string().contains("local segment artifacts remain"));
        assert!(spool_dir
            .join("part-1700000000000000000.jsonl.zst")
            .is_file());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[test]
    fn uploaded_cleanup_recovery_is_idempotent_after_every_interruption_point() {
        for removed in 0..=3 {
            let spool_dir = env::temp_dir().join(format!(
                "monday-upload-cleanup-{removed}-{}",
                now_ns().unwrap()
            ));
            std::fs::create_dir_all(&spool_dir).unwrap();
            let data = spool_dir.join("part-1700000000000000000.jsonl.zst");
            let manifest = spool_dir.join("part-1700000000000000000.jsonl.zst.manifest.json");
            let success = spool_dir.join("part-1700000000000000000.jsonl.zst._SUCCESS");
            for path in [&data, &manifest, &success] {
                std::fs::write(path, b"artifact").unwrap();
            }
            let marker = write_uploaded_cleanup_marker(&data, &manifest, &success).unwrap();

            for path in [&data, &manifest, &success].into_iter().take(removed) {
                std::fs::remove_file(path).unwrap();
            }

            assert_eq!(recover_uploaded_cleanups(&spool_dir).unwrap(), 1);
            for path in [&data, &manifest, &success, &marker] {
                assert!(!path.exists(), "{} was not cleaned", path.display());
            }
            assert_eq!(recover_uploaded_cleanups(&spool_dir).unwrap(), 0);
            std::fs::remove_dir_all(spool_dir).unwrap();
        }
    }

    #[test]
    fn uploaded_cleanup_marker_replaces_only_a_stale_regular_temp() {
        let spool_dir = env::temp_dir().join(format!(
            "monday-upload-cleanup-stale-temp-{}",
            now_ns().unwrap()
        ));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let data = spool_dir.join("part-1700000000000000000.jsonl.zst");
        let manifest = spool_dir.join("part-1700000000000000000.jsonl.zst.manifest.json");
        let success = spool_dir.join("part-1700000000000000000.jsonl.zst._SUCCESS");
        for path in [&data, &manifest, &success] {
            std::fs::write(path, b"artifact").unwrap();
        }
        let temporary = spool_dir.join(format!(
            "{}{}.tmp",
            local_file_name(&manifest).unwrap(),
            UPLOADED_CLEANUP_SUFFIX
        ));
        std::fs::write(&temporary, b"stale").unwrap();

        let marker = write_uploaded_cleanup_marker(&data, &manifest, &success).unwrap();
        assert!(marker.is_file());
        assert!(!temporary.exists());
        cleanup_uploaded_marker(&marker).unwrap();
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn uploaded_cleanup_marker_refuses_a_symlink_temp_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let spool_dir = env::temp_dir().join(format!(
            "monday-upload-cleanup-symlink-temp-{}",
            now_ns().unwrap()
        ));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let data = spool_dir.join("part-1700000000000000000.jsonl.zst");
        let manifest = spool_dir.join("part-1700000000000000000.jsonl.zst.manifest.json");
        let success = spool_dir.join("part-1700000000000000000.jsonl.zst._SUCCESS");
        for path in [&data, &manifest, &success] {
            std::fs::write(path, b"artifact").unwrap();
        }
        let victim = spool_dir.join("victim");
        std::fs::write(&victim, b"keep-me").unwrap();
        let temporary = spool_dir.join(format!(
            "{}{}.tmp",
            local_file_name(&manifest).unwrap(),
            UPLOADED_CLEANUP_SUFFIX
        ));
        symlink(&victim, &temporary).unwrap();

        let error = write_uploaded_cleanup_marker(&data, &manifest, &success).unwrap_err();
        assert!(error.to_string().contains("non-regular cleanup temp path"));
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep-me");
        assert!(std::fs::symlink_metadata(&temporary)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[test]
    fn uploaded_cleanup_marker_cannot_delete_a_sibling_segment() {
        let spool_dir = env::temp_dir().join(format!(
            "monday-upload-cleanup-cross-segment-{}",
            now_ns().unwrap()
        ));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let data = spool_dir.join("part-1700000000000000000.jsonl.zst");
        let manifest = spool_dir.join("part-1700000000000000000.jsonl.zst.manifest.json");
        let success = spool_dir.join("part-1700000000000000000.jsonl.zst._SUCCESS");
        let sibling_data = spool_dir.join("part-1700000000000000001.jsonl.zst");
        let sibling_manifest = spool_dir.join("part-1700000000000000001.jsonl.zst.manifest.json");
        let sibling_success = spool_dir.join("part-1700000000000000001.jsonl.zst._SUCCESS");
        for path in [
            &data,
            &manifest,
            &success,
            &sibling_data,
            &sibling_manifest,
            &sibling_success,
        ] {
            std::fs::write(path, b"artifact").unwrap();
        }
        let marker = write_uploaded_cleanup_marker(&data, &manifest, &success).unwrap();
        let mut metadata: Value =
            serde_json::from_reader(std::fs::File::open(&marker).unwrap()).unwrap();
        metadata["manifest"] = json!(local_file_name(&sibling_manifest).unwrap());
        std::fs::write(&marker, serde_json::to_vec(&metadata).unwrap()).unwrap();

        let error = cleanup_uploaded_marker(&marker).unwrap_err();
        assert!(error.to_string().contains("does not match its segment"));
        for path in [
            &data,
            &manifest,
            &success,
            &sibling_data,
            &sibling_manifest,
            &sibling_success,
            &marker,
        ] {
            assert!(path.exists(), "{} was removed", path.display());
        }
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[test]
    fn uploaded_cleanup_marker_is_removed_last() {
        let spool_dir =
            env::temp_dir().join(format!("monday-upload-cleanup-last-{}", now_ns().unwrap()));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let data = spool_dir.join("part-1700000000000000000.jsonl.zst");
        let manifest = spool_dir.join("part-1700000000000000000.jsonl.zst.manifest.json");
        let success = spool_dir.join("part-1700000000000000000.jsonl.zst._SUCCESS");
        for path in [&data, &manifest, &success] {
            std::fs::write(path, b"artifact").unwrap();
        }
        let marker = write_uploaded_cleanup_marker(&data, &manifest, &success).unwrap();
        std::fs::remove_file(&success).unwrap();
        std::fs::create_dir(&success).unwrap();

        recover_uploaded_cleanups(&spool_dir).unwrap_err();
        assert!(marker.is_file());
        assert!(data.is_file());
        assert!(manifest.is_file());
        assert!(success.is_dir());

        std::fs::remove_dir(&success).unwrap();
        assert_eq!(recover_uploaded_cleanups(&spool_dir).unwrap(), 1);
        assert!(!marker.exists());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn upload_pending_recovers_cleanup_before_reading_manifests() {
        let spool_dir = env::temp_dir().join(format!(
            "monday-upload-cleanup-before-manifests-{}",
            now_ns().unwrap()
        ));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let data = spool_dir.join("part-1700000000000000000.jsonl.zst");
        let manifest = spool_dir.join("part-1700000000000000000.jsonl.zst.manifest.json");
        let success = spool_dir.join("part-1700000000000000000.jsonl.zst._SUCCESS");
        for path in [&data, &manifest, &success] {
            std::fs::write(path, b"not valid manifest JSON").unwrap();
        }
        let marker = write_uploaded_cleanup_marker(&data, &manifest, &success).unwrap();
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        assert_eq!(upload_pending(&config).await.unwrap(), 0);
        assert!(!marker.exists());
        assert!(files_with_suffix(&spool_dir, ".manifest.json")
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    #[tokio::test]
    async fn upload_only_rejects_a_residual_cleanup_marker() {
        let spool_dir = env::temp_dir().join(format!(
            "monday-upload-cleanup-residual-{}",
            now_ns().unwrap()
        ));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let marker = spool_dir.join(format!(
            "part-1700000000000000000.jsonl.zst.manifest.json{UPLOADED_CLEANUP_SUFFIX}"
        ));
        std::fs::write(&marker, b"{}\n").unwrap();
        let config = UploadConfig {
            spool_dir: spool_dir.clone(),
            oss_bucket: "unused".into(),
            oss_endpoint: "unused".into(),
            oss_region: "ap-northeast-1".into(),
            aliyun_profile: "unused".into(),
            oss_copy_timeout: Duration::from_secs(1),
        };

        let error = upload_only(&config).await.unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid uploaded cleanup marker schema"));
        assert!(marker.is_file());
        std::fs::remove_dir_all(spool_dir).unwrap();
    }

    fn test_config(rest_base: String) -> Config {
        Config {
            market: Market::Spot,
            dataset: "spot_all".into(),
            shard_id: "all".into(),
            symbols: vec!["BTCUSDT".into()],
            security_token_symbols: vec![],
            excluded_symbols: Arc::new(RwLock::new(BTreeSet::new())),
            ws_shard_size: 100,
            snapshot_limit: 100,
            snapshot_requests_per_second: 15.0,
            segment_seconds: 3600,
            spool_dir: env::temp_dir(),
            max_buffered_diffs: 100,
            max_pending_diffs: 100,
            stall_timeout: Duration::from_secs(60),
            sync_timeout: Duration::from_secs(120),
            process_watchdog_timeout: Duration::from_secs(180),
            snapshot_retry_attempts: 3,
            rest_base,
            oss_bucket: "bucket".into(),
            oss_endpoint: "endpoint".into(),
            oss_region: "region".into(),
            aliyun_profile: "profile".into(),
            zstd_timeout: Duration::from_secs(30),
            oss_copy_timeout: Duration::from_secs(30),
        }
    }

    fn archive_first_btc_aggregate_trade(
        config: &Config,
        segment: &mut Segment,
        states: &mut HashMap<String, OrderBookState>,
        budget: &mut PendingBudget,
        process_state: &mut ProcessState,
    ) -> ProcessAction {
        archive_btc_aggregate_trade(config, segment, states, budget, process_state, 9)
    }

    fn archive_btc_aggregate_trade(
        config: &Config,
        segment: &mut Segment,
        states: &mut HashMap<String, OrderBookState>,
        budget: &mut PendingBudget,
        process_state: &mut ProcessState,
        aggregate_trade_id: u64,
    ) -> ProcessAction {
        let received_at_ns = now_ns().unwrap();
        let event_time_ms = received_at_ns / 1_000_000 - 1;
        let event = event_from_frame(
            json!({
                "stream": "btcusdt@aggTrade",
                "data": {"e":"aggTrade","E":event_time_ms,"s":"BTCUSDT","a":aggregate_trade_id,"f":aggregate_trade_id,"l":aggregate_trade_id,"p":"101","q":"0.2","T":event_time_ms,"m":true}
            }),
            received_at_ns,
        )
        .unwrap();
        process_event(
            config,
            segment,
            states,
            budget,
            "session-1",
            event,
            process_state,
        )
        .unwrap()
    }

    fn trusted_process_state(symbols: &[String]) -> ProcessState {
        let mut state = ProcessState::new(true);
        state.stream_coverage_shards = vec![symbols
            .iter()
            .flat_map(|symbol| {
                let symbol = symbol.to_ascii_lowercase();
                [
                    format!("{symbol}@depth@100ms"),
                    format!("{symbol}@aggTrade"),
                ]
            })
            .collect()];
        state
    }

    #[test]
    fn market_tape_subscribes_depth_and_aggregate_trades_separately() {
        let spot = test_config("http://unused".into()).stream_shards();
        assert_eq!(spot.len(), 2);
        assert!(spot.iter().any(|shard| {
            shard.streams.contains("btcusdt@depth@100ms")
                && !shard.streams.contains("btcusdt@aggTrade")
        }));
        assert!(spot.iter().any(|shard| {
            shard.streams.contains("btcusdt@aggTrade")
                && !shard.streams.contains("btcusdt@depth@100ms")
        }));

        let mut usdm_config = test_config("http://unused".into());
        usdm_config.market = Market::Usdm;
        usdm_config.dataset = "usdm_all".into();
        let usdm = usdm_config.stream_shards();
        assert!(usdm.iter().any(|shard| {
            shard
                .url
                .starts_with("wss://fstream.binance.com/public/stream")
                && shard.streams.contains("btcusdt@depth@100ms")
        }));
        assert!(usdm.iter().any(|shard| {
            shard
                .url
                .starts_with("wss://fstream.binance.com/market/stream")
                && shard.streams.contains("btcusdt@aggTrade")
        }));
        assert!(spot
            .iter()
            .chain(&usdm)
            .all(|shard| !shard.streams.contains("btcusdt@trade")));
    }

    #[test]
    fn subscription_listing_requires_exact_stream_set() {
        let expected = BTreeSet::from([
            "bnsollsol@aggTrade".to_owned(),
            "bnsollsol@depth@100ms".to_owned(),
        ]);
        let exact = json!({
            "id":SUBSCRIPTION_PROOF_ID,
            "result":["bnsollsol@depth@100ms","bnsollsol@aggTrade"]
        });
        validate_subscription_listing(&exact, &expected).unwrap();

        let missing = json!({
            "id":SUBSCRIPTION_PROOF_ID,
            "result":["bnsollsol@aggTrade"]
        });
        assert!(validate_subscription_listing(&missing, &expected)
            .unwrap_err()
            .to_string()
            .contains("subscription coverage mismatch"));

        let extra = json!({
            "id":SUBSCRIPTION_PROOF_ID,
            "result":[
                "bnsollsol@depth@100ms",
                "bnsollsol@aggTrade",
                "btcusdt@depth@100ms"
            ]
        });
        assert!(validate_subscription_listing(&extra, &expected).is_err());
        let string_id = json!({
            "id":"1",
            "result":["bnsollsol@depth@100ms","bnsollsol@aggTrade"]
        });
        assert!(validate_subscription_listing(&string_id, &expected).is_err());
    }

    #[tokio::test]
    async fn subscription_proof_uses_one_absolute_deadline() {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        receive_before_subscription_proof_deadline(
            deadline,
            tokio::time::sleep(Duration::from_millis(60)),
        )
        .await
        .unwrap();

        let error = receive_before_subscription_proof_deadline(
            deadline,
            tokio::time::sleep(Duration::from_millis(60)),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("subscription proof timed out"));
    }

    #[tokio::test]
    async fn expired_subscription_deadline_wins_over_a_ready_market_frame() {
        let error = receive_before_subscription_proof_deadline(
            tokio::time::Instant::now(),
            std::future::ready(()),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("subscription proof timed out"));
    }

    #[tokio::test]
    async fn subscription_proof_deadline_bounds_a_full_archive_queue() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let event_time_ms = now_ns().unwrap() / 1_000_000;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = websocket.next().await.unwrap().unwrap();
            assert!(request
                .to_text()
                .unwrap()
                .contains("LIST_SUBSCRIPTIONS"));
            websocket
                .send(Message::Text(
                    json!({
                        "stream":"btcusdt@aggTrade",
                        "data":{"e":"aggTrade","E":event_time_ms,"s":"BTCUSDT","a":1,"f":1,"l":1,"p":"100","q":"1","T":event_time_ms,"m":false}
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let _ = websocket.next().await;
        });
        let (sender, _receiver) = mpsc::channel(1);
        sender.send(Event::InitialSnapshotsComplete).await.unwrap();
        let (stream_connected, _stream_connected_receiver) = mpsc::channel(1);
        let (_shutdown_sender, shutdown) = watch::channel(false);
        let (_pause_tx, pause_rx) = watch::channel(0_u64);
        let (_resume_tx, resume_rx) = watch::channel(0_u64);
        let shard = StreamShard {
            url: format!("ws://{address}"),
            streams: BTreeSet::from(["btcusdt@aggTrade".to_owned()]),
        };

        let error = tokio::time::timeout(
            Duration::from_millis(250),
            receive_url(
                shard,
                sender,
                stream_connected,
                shutdown,
                Duration::from_secs(1),
                Duration::from_millis(50),
                ProcessWatchdog::new_state(),
                0,
                pause_rx,
                resume_rx,
            ),
        )
        .await
        .expect("full archive queue bypassed subscription proof deadline")
        .unwrap_err();

        assert!(error.to_string().contains("subscription proof timed out"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn transient_websocket_close_is_reconnected_without_failing_the_stream_task() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let event_time_ms = now_ns().unwrap() / 1_000_000;
        let server = tokio::spawn(async move {
            for connection in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let request = websocket.next().await.unwrap().unwrap();
                assert!(request.to_text().unwrap().contains("LIST_SUBSCRIPTIONS"));
                if connection == 1 {
                    websocket
                        .send(Message::Text(
                            json!({
                                "stream": "btcusdt@aggTrade",
                                "data": {
                                    "e": "aggTrade",
                                    "E": event_time_ms,
                                    "s": "BTCUSDT",
                                    "a": 1,
                                    "f": 1,
                                    "l": 1,
                                    "p": "100",
                                    "q": "1",
                                    "T": event_time_ms,
                                    "m": false
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                }
                websocket
                    .send(Message::Text(
                        json!({
                            "id": SUBSCRIPTION_PROOF_ID,
                            "result": ["btcusdt@aggTrade", "btcusdt@depth@100ms"]
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                if connection == 0 {
                    websocket.close(None).await.unwrap();
                    continue;
                }
                websocket
                    .send(Message::Text(
                        json!({
                            "stream": "btcusdt@depth@100ms",
                            "data": {
                                "e": "depthUpdate",
                                "E": event_time_ms,
                                "T": event_time_ms,
                                "s": "BTCUSDT",
                                "U": 1,
                                "u": 1,
                                "b": [],
                                "a": []
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .unwrap();
                let _ = websocket.next().await;
            }
        });
        let (sender, mut receiver) = mpsc::channel(8);
        let (stream_connected, _stream_connected_receiver) = mpsc::channel(2);
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (_pause_tx, pause_rx) = watch::channel(0_u64);
        let (_resume_tx, resume_rx) = watch::channel(0_u64);
        let shard = StreamShard {
            url: format!("ws://{address}"),
            streams: BTreeSet::from([
                "btcusdt@aggTrade".to_owned(),
                "btcusdt@depth@100ms".to_owned(),
            ]),
        };

        let task = tokio::spawn(receive_url(
            shard,
            sender,
            stream_connected,
            shutdown,
            Duration::from_secs(1),
            Duration::from_millis(50),
            ProcessWatchdog::new_state(),
            0,
            pause_rx,
            resume_rx,
        ));
        let mut saw_disconnect = false;
        let mut saw_reconnect = false;
        let mut saw_aggregate_trade = false;
        let mut saw_depth = false;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match receiver.recv().await {
                    Some(Event::AggregateTrade { .. }) if saw_reconnect => {
                        saw_aggregate_trade = true;
                    }
                    Some(Event::Diff { .. }) if saw_reconnect => saw_depth = true,
                    Some(Event::StreamDisconnected { .. }) => saw_disconnect = true,
                    Some(Event::StreamReconnected { .. }) => saw_reconnect = true,
                    Some(_) => {}
                    None => panic!("stream task exited after the transient close"),
                }
                if saw_aggregate_trade && saw_depth {
                    break;
                }
            }
        })
        .await
        .expect("stream task did not reconnect after the transient close");
        assert!(saw_disconnect);
        assert!(saw_reconnect);
        assert!(saw_aggregate_trade);
        assert!(saw_depth);
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn pause_during_websocket_connect_acknowledges_rotation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        let (sender, mut receiver) = mpsc::channel(2);
        let (stream_connected, _stream_connected_receiver) = mpsc::channel(1);
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let task = tokio::spawn(receive_url(
            StreamShard {
                url: format!("ws://{address}"),
                streams: BTreeSet::from(["btcusdt@aggTrade".to_owned()]),
            },
            sender,
            stream_connected,
            shutdown,
            Duration::from_secs(1),
            Duration::from_secs(1),
            ProcessWatchdog::new_state(),
            0,
            pause_rx,
            resume_rx,
        ));
        accepted_rx.await.unwrap();
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn pause_during_subscription_proof_acknowledges_rotation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (proof_tx, proof_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = websocket.next().await.unwrap().unwrap();
            assert!(request.to_text().unwrap().contains("LIST_SUBSCRIPTIONS"));
            let _ = proof_tx.send(());
            let _ = websocket.next().await;
        });
        let (sender, mut receiver) = mpsc::channel(2);
        let (stream_connected, _stream_connected_receiver) = mpsc::channel(1);
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let task = tokio::spawn(receive_url(
            StreamShard {
                url: format!("ws://{address}"),
                streams: BTreeSet::from(["btcusdt@aggTrade".to_owned()]),
            },
            sender,
            stream_connected,
            shutdown,
            Duration::from_secs(1),
            Duration::from_secs(20),
            ProcessWatchdog::new_state(),
            0,
            pause_rx,
            resume_rx,
        ));
        proof_rx.await.unwrap();
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn buffered_reconnect_proof_event_is_abandoned_after_rotation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let event_time_ms = now_ns().unwrap() / 1_000_000;
        let (market_frame_tx, market_frame_rx) = tokio::sync::oneshot::channel();
        let (proof_tx, proof_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = websocket.next().await.unwrap().unwrap();
            assert!(request.to_text().unwrap().contains("LIST_SUBSCRIPTIONS"));
            websocket
                .send(Message::Text(
                    json!({
                        "id": SUBSCRIPTION_PROOF_ID,
                        "result": ["btcusdt@aggTrade", "btcusdt@depth@100ms"]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = websocket.next().await.unwrap().unwrap();
            assert!(request.to_text().unwrap().contains("LIST_SUBSCRIPTIONS"));
            websocket
                .send(Message::Text(
                    json!({
                        "stream": "btcusdt@aggTrade",
                        "data": {
                            "e": "aggTrade",
                            "E": event_time_ms,
                            "s": "BTCUSDT",
                            "a": 1,
                            "f": 1,
                            "l": 1,
                            "p": "100",
                            "q": "1",
                            "T": event_time_ms,
                            "m": false
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            market_frame_tx.send(()).unwrap();
            proof_rx.await.unwrap();
            websocket
                .send(Message::Text(
                    json!({
                        "id": SUBSCRIPTION_PROOF_ID,
                        "result": ["btcusdt@aggTrade", "btcusdt@depth@100ms"]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let _ = websocket.next().await;
        });
        let (sender, mut receiver) = mpsc::channel(8);
        let (stream_connected, _stream_connected_receiver) = mpsc::channel(2);
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let task = tokio::spawn(receive_url(
            StreamShard {
                url: format!("ws://{address}"),
                streams: BTreeSet::from([
                    "btcusdt@aggTrade".to_owned(),
                    "btcusdt@depth@100ms".to_owned(),
                ]),
            },
            sender,
            stream_connected,
            shutdown,
            Duration::from_secs(1),
            Duration::from_secs(20),
            ProcessWatchdog::new_state(),
            0,
            pause_rx,
            resume_rx,
        ));

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap(),
            Some(Event::StreamDisconnected { .. })
        ));
        market_frame_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        proof_tx.send(()).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::StreamDisconnected { .. })
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn buffered_reconnect_proof_timeout_acknowledges_rotation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let event_time_ms = now_ns().unwrap() / 1_000_000;
        let (market_frame_tx, market_frame_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = websocket.next().await.unwrap().unwrap();
            assert!(request.to_text().unwrap().contains("LIST_SUBSCRIPTIONS"));
            websocket
                .send(Message::Text(
                    json!({
                        "id": SUBSCRIPTION_PROOF_ID,
                        "result": ["btcusdt@aggTrade", "btcusdt@depth@100ms"]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();

            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let request = websocket.next().await.unwrap().unwrap();
            assert!(request.to_text().unwrap().contains("LIST_SUBSCRIPTIONS"));
            websocket
                .send(Message::Text(
                    json!({
                        "stream": "btcusdt@aggTrade",
                        "data": {
                            "e": "aggTrade",
                            "E": event_time_ms,
                            "s": "BTCUSDT",
                            "a": 1,
                            "f": 1,
                            "l": 1,
                            "p": "100",
                            "q": "1",
                            "T": event_time_ms,
                            "m": false
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            market_frame_tx.send(()).unwrap();
            let _ = websocket.next().await;
        });
        let (sender, mut receiver) = mpsc::channel(8);
        let (stream_connected, _stream_connected_receiver) = mpsc::channel(2);
        let (shutdown_tx, shutdown) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let task = tokio::spawn(receive_url(
            StreamShard {
                url: format!("ws://{address}"),
                streams: BTreeSet::from([
                    "btcusdt@aggTrade".to_owned(),
                    "btcusdt@depth@100ms".to_owned(),
                ]),
            },
            sender,
            stream_connected,
            shutdown,
            Duration::from_secs(1),
            Duration::from_millis(400),
            ProcessWatchdog::new_state(),
            0,
            pause_rx,
            resume_rx,
        ));

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap(),
            Some(Event::StreamDisconnected { .. })
        ));
        market_frame_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::StreamDisconnected { .. })
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn aggregate_trade_is_validated_and_archived_without_mutating_lob_state() {
        let root = env::temp_dir().join(format!(
            "monday-binance-research-ticks-{}",
            now_ns().unwrap()
        ));
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.clone();
        let mut states = HashMap::from([(
            "BTCUSDT".to_owned(),
            OrderBookState::new("BTCUSDT", Market::Spot),
        )]);
        let mut budget = PendingBudget::new(1);
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        archive_first_btc_aggregate_trade(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            &mut process_state,
        );
        assert_eq!(segment.event_count("agg_trade"), 1);
        assert!(!states["BTCUSDT"].synced);
        drop(segment);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_aggregate_trade_is_rejected_before_archival() {
        let received_at_ns = now_ns().unwrap();
        let event_time_ms = received_at_ns / 1_000_000 - 1;
        let frame = json!({
            "stream": "btcusdt@aggTrade",
            "data": {"e":"aggTrade","E":event_time_ms,"s":"BTCUSDT","a":9,"f":11,"l":10,"p":"101","q":"0.2","T":event_time_ms,"m":true}
        });
        assert!(event_from_frame(frame, received_at_ns)
            .unwrap_err()
            .to_string()
            .contains("id range is reversed"));
    }

    #[test]
    fn depth_without_source_clock_is_rejected_before_archival() {
        let frame = json!({
            "stream": "btcusdt@depth@100ms",
            "data": {"e":"depthUpdate","s":"BTCUSDT","U":10,"u":11,"b":[],"a":[]}
        });
        assert!(event_from_frame(frame, now_ns().unwrap())
            .unwrap_err()
            .to_string()
            .contains("field E is missing"));
    }

    #[test]
    fn depth_gap_ends_the_capture_epoch() {
        let root = tempfile::Builder::new()
            .prefix("monday-depth-gap-epoch-test-")
            .tempdir()
            .unwrap();
        let mut config = test_config("http://unused".into());
        config.market = Market::Usdm;
        config.spool_dir = root.path().to_path_buf();
        let mut budget = PendingBudget::new(10);
        let mut state = OrderBookState::new("BTCUSDT", Market::Usdm);
        state
            .install_snapshot(
                &json!({
                    "lastUpdateId": 11_075_153_756_947_u64,
                    "bids": [["100", "1"]],
                    "asks": [["101", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        let mut states = HashMap::from([("BTCUSDT".to_owned(), state)]);
        let mut segment =
            Segment::create(config.segment_config(), 1_784_349_725_319_895_632).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        let received_at_ns = 1_784_349_725_538_670_685;
        let event = event_from_frame(
            json!({
                "stream": "btcusdt@depth@100ms",
                "data": {
                    "e": "depthUpdate",
                    "E": 1_784_349_725_538_u64,
                    "T": 1_784_349_725_538_u64,
                    "s": "BTCUSDT",
                    "U": 11_075_153_761_705_u64,
                    "u": 11_075_153_767_256_u64,
                    "pu": 11_075_153_761_591_u64,
                    "b": [],
                    "a": []
                }
            }),
            received_at_ns,
        )
        .unwrap();

        let error = process_event(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            "session-1",
            event,
            &mut process_state,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("expected=11075153756947 received=11075153761591-11075153767256"));
        assert_eq!(process_state.sequence_gaps, 1);
        assert_eq!(segment.event_count("sequence_gap"), 1);
    }

    #[test]
    fn all_catalog_excludes_non_spot_and_security_tokens_and_records_manifest_sets() {
        let catalog = parse_symbol_catalog(
            Market::Spot,
            &json!({"symbols":[
                {"symbol":"BTCUSDT","status":"TRADING","isSpotTradingAllowed":true,"permissionSets":[["SPOT"]]},
                {"symbol":"BLOCKEDUSDT","status":"TRADING","isSpotTradingAllowed":false,"permissionSets":[["SPOT"]]},
                {"symbol":"SECURITYUSDT","status":"TRADING","isSpotTradingAllowed":true,"permissionSets":[["SPOT","TRD_GRP_261"]]},
                {"symbol":"HALTEDUSDT","status":"BREAK","isSpotTradingAllowed":true,"permissionSets":[["SPOT"]]}
            ]}),
        )
        .unwrap();
        assert_eq!(catalog.symbols, ["BTCUSDT", "SECURITYUSDT"]);
        assert_eq!(catalog.security_token_symbols, ["SECURITYUSDT"]);
        assert_eq!(catalog.excluded_symbols, ["BLOCKEDUSDT"]);

        let mut config = test_config("http://unused".into());
        config.symbols = catalog.symbols;
        config.security_token_symbols = catalog.security_token_symbols;
        config.excluded_symbols =
            Arc::new(RwLock::new(catalog.excluded_symbols.into_iter().collect()));
        let manifest = config.segment_config();
        assert_eq!(manifest.security_token_symbols, ["SECURITYUSDT"]);
        assert_eq!(manifest.excluded_symbols, ["BLOCKEDUSDT"]);
    }

    #[tokio::test]
    async fn symbol_discovery_times_out_when_exchange_info_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            thread::sleep(Duration::from_millis(250));
        });

        let started = Instant::now();
        let error = discover_symbols_with_timeout(
            Market::Spot,
            &format!("http://{address}"),
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(reqwest::Error::is_timeout)
        }));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn snapshot_retries_429_and_5xx_then_succeeds() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for response in [
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 38\r\nConnection: close\r\n\r\n{\"lastUpdateId\":1,\"bids\":[],\"asks\":[]}",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let config = test_config(format!("http://{address}"));
        let (_shutdown_tx, mut shutdown) = watch::channel(false);
        let snapshot = fetch_snapshot(&reqwest::Client::new(), &config, "BTCUSDT", &mut shutdown)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot["lastUpdateId"], 1);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn snapshots_wait_until_every_websocket_subscription_set_is_verified() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicU64::new(0));
        let server_requests = requests.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            server_requests.fetch_add(1, Ordering::SeqCst);
            let body = r#"{"lastUpdateId":1,"bids":[["100","1"]],"asks":[["101","1"]]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let config = Arc::new(test_config(format!("http://{address}")));
        let (sender, mut receiver) = mpsc::channel(8);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (connected_tx, connected_rx) = mpsc::channel(2);
        let (_pause_tx, pause_rx) = watch::channel(0_u64);
        let (_resume_tx, resume_rx) = watch::channel(0_u64);
        let producer = tokio::spawn(produce_snapshots_after_streams_connect(
            config,
            sender,
            shutdown_rx,
            connected_rx,
            2,
            2,
            pause_rx,
            resume_rx,
        ));

        connected_tx
            .send(vec!["btcusdt@depth@100ms".into()])
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receiver.recv())
                .await
                .is_err()
        );
        assert_eq!(requests.load(Ordering::SeqCst), 0);

        connected_tx
            .send(vec!["btcusdt@aggTrade".into()])
            .await
            .unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(Event::StreamCoverageVerified { shards })
                if shards == vec![
                    vec!["btcusdt@aggTrade".to_owned()],
                    vec!["btcusdt@depth@100ms".to_owned()]
                ]
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(Event::Snapshot { .. })
        ));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(matches!(
            receiver.recv().await,
            Some(Event::InitialSnapshotsComplete)
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            producer.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn initial_snapshot_completion_is_emitted_after_last_snapshot() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let body = r#"{"lastUpdateId":1,"bids":[["100","1"]],"asks":[["101","1"]]}"#;
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let mut config = test_config(format!("http://{address}"));
        config.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
        config.snapshot_requests_per_second = 1_000.0;
        let (sender, mut receiver) = mpsc::channel(8);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (_pause_tx, pause_rx) = watch::channel(0_u64);
        let (_resume_tx, resume_rx) = watch::channel(0_u64);
        let producer = tokio::spawn(produce_snapshots(
            Arc::new(config),
            sender,
            shutdown_rx,
            0,
            pause_rx,
            resume_rx,
            0,
        ));

        assert!(matches!(
            receiver.recv().await,
            Some(Event::Snapshot { .. })
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(Event::Snapshot { .. })
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(Event::InitialSnapshotsComplete)
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            producer.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn one_sided_initial_snapshot_is_explicitly_excluded() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for body in [
                r#"{"lastUpdateId":1,"bids":[],"asks":[["438.00","50.0"]]}"#,
                r#"{"lastUpdateId":1,"bids":[["100","1"]],"asks":[["101","1"]]}"#,
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let mut config = test_config(format!("http://{address}"));
        config.symbols = vec!["SCRIDR".into(), "BTCUSDT".into()];
        config.snapshot_requests_per_second = 5.0;
        let (sender, mut receiver) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (_pause_tx, pause_rx) = watch::channel(0_u64);
        let (_resume_tx, resume_rx) = watch::channel(0_u64);

        let producer = tokio::spawn(produce_snapshots(
            Arc::new(config),
            sender,
            shutdown_rx,
            0,
            pause_rx,
            resume_rx,
            0,
        ));

        match receiver.recv().await {
            Some(Event::ExcludeSymbol { symbol, reason }) => {
                assert_eq!(symbol, "SCRIDR");
                assert!(reason.contains("one-sided initial snapshot"));
            }
            event => panic!("expected explicit one-sided exclusion, got {event:?}"),
        }
        let exclusion_received = Instant::now();
        assert!(matches!(
            receiver.recv().await,
            Some(Event::Snapshot { symbol, .. }) if symbol == "BTCUSDT"
        ));
        assert!(exclusion_received.elapsed() >= Duration::from_millis(150));
        assert!(matches!(
            receiver.recv().await,
            Some(Event::InitialSnapshotsComplete)
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            producer.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn pause_during_snapshot_fetch_acknowledges_rotation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (first_request_tx, first_request_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            assert!(first.read(&mut request).await.unwrap() > 0);
            let _ = first_request_tx.send(());
            let first_reader = tokio::spawn(async move {
                let mut ignored = [0_u8; 1];
                let _ = first.read(&mut ignored).await;
            });
            let (mut second, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            assert!(second.read(&mut request).await.unwrap() > 0);
            let body = r#"{"lastUpdateId":1,"bids":[["100","1"]],"asks":[["101","1"]]}"#;
            second
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            first_reader.abort();
        });
        let mut config = test_config(format!("http://{address}"));
        config.snapshot_requests_per_second = 1_000.0;
        let (sender, mut receiver) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let producer = tokio::spawn(produce_snapshots(
            Arc::new(config),
            sender,
            shutdown_rx,
            0,
            pause_rx,
            resume_rx,
            0,
        ));

        first_request_rx.await.unwrap();
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(Event::Snapshot { symbol, .. }) if symbol == "BTCUSDT"
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(Event::InitialSnapshotsComplete)
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            producer.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn pause_during_snapshot_backoff_preserves_retry_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (first_response_tx, first_response_rx) = tokio::sync::oneshot::channel();
        let (second_request_tx, second_request_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            assert!(first.read(&mut request).await.unwrap() > 0);
            first
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 5\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await
                .unwrap();
            first_response_tx.send(()).unwrap();

            let (mut second, _) = listener.accept().await.unwrap();
            assert!(second.read(&mut request).await.unwrap() > 0);
            let _ = second_request_tx.send(());
        });
        let mut config = test_config(format!("http://{address}"));
        config.snapshot_requests_per_second = 1_000.0;
        let (sender, mut receiver) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let producer = tokio::spawn(produce_snapshots(
            Arc::new(config),
            sender,
            shutdown_rx,
            0,
            pause_rx,
            resume_rx,
            0,
        ));

        first_response_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(250), second_request_rx)
                .await
                .is_err(),
            "snapshot retry ignored Retry-After after a rotation pause"
        );

        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), producer)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            TaskExit::Stopped(None)
        ));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn pause_during_snapshot_interval_acknowledges_rotation() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"lastUpdateId":1,"bids":[["100","1"]],"asks":[["101","1"]]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let mut config = test_config(format!("http://{address}"));
        config.snapshot_requests_per_second = 0.1;
        let (sender, mut receiver) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (pause_tx, pause_rx) = watch::channel(0_u64);
        let (resume_tx, resume_rx) = watch::channel(0_u64);
        let producer = tokio::spawn(produce_snapshots(
            Arc::new(config),
            sender,
            shutdown_rx,
            0,
            pause_rx,
            resume_rx,
            0,
        ));

        assert!(matches!(
            receiver.recv().await,
            Some(Event::Snapshot { symbol, .. }) if symbol == "BTCUSDT"
        ));
        pause_tx.send(1).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap(),
            Some(Event::RotationBarrier {
                producer_id: 0,
                epoch: 1
            })
        ));
        resume_tx.send(1).unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(Event::InitialSnapshotsComplete)
        ));
        shutdown_tx.send(true).unwrap();
        assert!(matches!(
            producer.await.unwrap().unwrap(),
            TaskExit::Stopped(None)
        ));
        server.join().unwrap();
    }

    #[test]
    fn runtime_exclusion_requires_a_new_capture_session() {
        assert!(ProcessAction::Excluded.restarts_capture_session());
        assert!(ProcessAction::RestartSession.restarts_capture_session());
        assert!(!ProcessAction::None.restarts_capture_session());
        assert!(!ProcessAction::InitialSnapshotsComplete.restarts_capture_session());
    }

    #[test]
    fn reconnecting_shard_stays_unhealthy_until_market_data_returns() {
        let mut process_state = ProcessState::new(true);
        process_state.mark_shard_disconnected(vec![
            "btcusdt@depth@100ms".into(),
            "ethusdt@depth@100ms".into(),
            "btcusdt@aggTrade".into(),
            "ethusdt@aggTrade".into(),
        ]);
        assert!(!process_state.streams_healthy());
        assert!(!process_state.depth_streams_healthy());

        process_state.mark_stream_observed("btcusdt@aggTrade");
        assert!(!process_state.streams_healthy());
        assert!(!process_state.depth_streams_healthy());

        process_state.mark_stream_observed("btcusdt@depth@100ms");
        assert!(!process_state.streams_healthy());
        assert!(!process_state.depth_streams_healthy());

        process_state.mark_stream_observed("ethusdt@aggTrade");
        assert!(!process_state.streams_healthy());

        process_state.mark_stream_observed("ethusdt@depth@100ms");
        assert!(process_state.streams_healthy());
        assert!(process_state.depth_streams_healthy());
    }

    #[test]
    fn aggregate_trade_reconnect_requires_a_new_capture_session() {
        let root = tempfile::tempdir().unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut states = HashMap::from([(
            "BTCUSDT".to_owned(),
            OrderBookState::new("BTCUSDT", Market::Spot),
        )]);
        let mut budget = PendingBudget::new(1);
        let mut process_state = trusted_process_state(&config.symbols);
        assert_eq!(
            process_event(
                &config,
                &mut segment,
                &mut states,
                &mut budget,
                "session-1",
                Event::StreamDisconnected {
                    streams: vec!["btcusdt@aggTrade".into()],
                    reason: "test".into(),
                },
                &mut process_state,
            )
            .unwrap(),
            ProcessAction::None
        );
        assert!(!segment.is_replay_safe());

        assert_eq!(
            archive_first_btc_aggregate_trade(
                &config,
                &mut segment,
                &mut states,
                &mut budget,
                &mut process_state,
            ),
            ProcessAction::RestartSession
        );
    }

    #[tokio::test]
    async fn invalid_snapshot_is_not_retried_and_becomes_persistent_manifest_exclusion() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let body = "{\"code\":-1121,\"msg\":\"Invalid symbol.\"}";
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let root = env::temp_dir().join(format!("monday-exclusion-test-{}", now_ns().unwrap()));
        let mut config = test_config(format!("http://{address}"));
        config.spool_dir = root.clone();
        config.symbols = vec!["BTCUSDT".into(), "BADUSDT".into()];
        let (_shutdown_tx, mut shutdown) = watch::channel(false);
        let error = fetch_snapshot(&reqwest::Client::new(), &config, "BADUSDT", &mut shutdown)
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<SnapshotUnavailable>().is_some());
        server.join().unwrap();

        if Command::new("zstd").arg("--version").output().is_err() {
            return;
        }
        let mut states = config
            .active_symbols()
            .into_iter()
            .map(|symbol| (symbol.clone(), OrderBookState::new(symbol, Market::Spot)))
            .collect::<HashMap<_, _>>();
        let mut budget = PendingBudget::new(10);
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        let action = process_event(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            "session-1",
            Event::ExcludeSymbol {
                symbol: "BADUSDT".into(),
                reason: error.to_string(),
            },
            &mut process_state,
        )
        .unwrap();
        assert_eq!(action, ProcessAction::Excluded);
        assert_eq!(config.active_symbols(), ["BTCUSDT"]);
        assert!(config.is_excluded("BADUSDT"));
        assert!(!states.contains_key("BADUSDT"));

        let artifacts = close_segment(
            segment,
            &config,
            &states,
            "session-1",
            "test",
            &trusted_process_state(&config.symbols),
        )
        .unwrap()
        .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["symbols"], json!(["BTCUSDT"]));
        assert_eq!(manifest["excluded_symbols"], json!(["BADUSDT"]));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn retry_after_backoff_stops_immediately_on_shutdown() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let response = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 60\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        let config = test_config(format!("http://{address}"));
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            fetch_snapshot(
                &reqwest::Client::new(),
                &config,
                "BTCUSDT",
                &mut shutdown_rx,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        shutdown_tx.send(true).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown must beat 60s Retry-After")
            .unwrap()
            .unwrap();
        assert!(result.is_none());
        server.join().unwrap();
    }

    #[test]
    fn sync_and_process_watchdogs_trip_only_after_deadline() {
        let states = HashMap::from([(
            "BTCUSDT".into(),
            OrderBookState::new("BTCUSDT", Market::Spot),
        )]);
        let now = Instant::now();
        assert!(!sync_timed_out(
            &states,
            Some(now + Duration::from_secs(1)),
            now
        ));
        assert!(sync_timed_out(
            &states,
            Some(now - Duration::from_millis(1)),
            now
        ));
        assert!(!process_watchdog_expired(
            1_000,
            181_000,
            Duration::from_secs(180)
        ));
        assert!(process_watchdog_expired(
            1_000,
            181_001,
            Duration::from_secs(180)
        ));
    }

    fn armed_watchdog() -> ProcessWatchdog {
        let watchdog = ProcessWatchdog::new_state();
        watchdog.inner.last_data_ms.store(1_000, Ordering::Relaxed);
        assert!(watchdog.arm());
        watchdog
    }

    #[tokio::test]
    async fn global_shutdown_receiver_observes_a_disarmed_watchdog() {
        let watchdog = armed_watchdog();
        let observed_watchdog = watchdog.clone();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let receiver = tokio::spawn(async move {
            shutdown_rx
                .changed()
                .await
                .expect("observe global shutdown");
            assert!(*shutdown_rx.borrow());
            assert_eq!(observed_watchdog.state(), ProcessWatchdogState::Disarmed);
            assert!(!observed_watchdog.try_begin_exit_at(181_001, Duration::from_secs(180)));
        });

        assert!(publish_global_shutdown(&shutdown_tx, &watchdog));
        receiver.await.expect("join shutdown observer");
    }

    #[test]
    fn shutdown_disarm_and_watchdog_firing_are_one_atomic_race() {
        for _ in 0..32 {
            let watchdog = armed_watchdog();
            let publisher_watchdog = watchdog.clone();
            let firing_watchdog = watchdog.clone();
            let barrier = Arc::new(Barrier::new(3));
            let publisher_barrier = barrier.clone();
            let firing_barrier = barrier.clone();
            let (shutdown_tx, shutdown_rx) = watch::channel(false);

            let publisher = thread::spawn(move || {
                publisher_barrier.wait();
                publish_global_shutdown(&shutdown_tx, &publisher_watchdog)
            });
            let firing = thread::spawn(move || {
                firing_barrier.wait();
                firing_watchdog.try_begin_exit_at(181_001, Duration::from_secs(180))
            });
            barrier.wait();

            let published = publisher.join().expect("join shutdown publisher");
            let claimed_exit = firing.join().expect("join watchdog firing claimant");
            assert_ne!(published, claimed_exit);
            assert_eq!(*shutdown_rx.borrow(), published);
            assert_eq!(
                watchdog.state(),
                if published {
                    ProcessWatchdogState::Disarmed
                } else {
                    ProcessWatchdogState::Firing
                }
            );
        }
    }

    #[tokio::test]
    async fn run_session_task_failure_keeps_watchdog_armed_for_reconnect() {
        let watchdog = armed_watchdog();
        let spool_dir = env::temp_dir().join(format!(
            "monday-watchdog-reconnect-test-{}",
            now_ns().unwrap()
        ));
        std::fs::create_dir_all(&spool_dir).unwrap();
        let mut config = test_config("http://[::1".into());
        // The newline makes the websocket URL invalid while the malformed REST
        // base makes the snapshot task fail too. Both failures happen only
        // after run_session has created its segment and spawned its producers.
        config.symbols = vec!["BAD\nSYMBOL".into()];
        config.spool_dir = spool_dir.clone();
        config.snapshot_retry_attempts = 1;
        let config = Arc::new(config);

        for attempt in 1..=2 {
            let (_shutdown_tx, shutdown_rx) = watch::channel(false);
            let error = tokio::time::timeout(Duration::from_secs(5), async {
                run_session(config.clone(), shutdown_rx, watchdog.clone()).await
            })
            .await
            .unwrap_or_else(|_| panic!("session attempt {attempt} did not fail promptly"))
            .expect_err("invalid producers must return to the reconnect loop");
            assert!(!error.to_string().is_empty());
            assert_eq!(watchdog.state(), ProcessWatchdogState::Armed);
        }

        std::fs::remove_dir_all(spool_dir).unwrap();
        assert!(watchdog.try_begin_exit_at(181_001, Duration::from_secs(180)));
    }

    #[test]
    fn snapshot_only_silent_symbol_closes_replay_unsafe() {
        assert!(Command::new("zstd")
            .arg("--version")
            .output()
            .expect("zstd is required for segment lifecycle tests")
            .status
            .success());
        let root = env::temp_dir().join(format!("monday-silent-symbol-test-{}", now_ns().unwrap()));
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.clone();
        config.symbols = vec!["BTCUSDT".into(), "TUSDUSDT".into()];
        let mut silent = OrderBookState::new("TUSDUSDT", Market::Spot);
        let mut bridged = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(2);
        silent
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["1", "1"]],
                    "asks": [["2", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        bridged
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["100", "1"]],
                    "asks": [["102", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        bridged
            .apply_diff(
                DepthDiff {
                    symbol: "BTCUSDT".into(),
                    first_update_id: 101,
                    final_update_id: 101,
                    previous_update_id: None,
                    bids: vec![["100".into(), "2".into()]],
                    asks: vec![],
                },
                &mut budget,
            )
            .unwrap();
        assert!(silent.synced);
        assert!(!silent.bridged);
        assert!(bridged.synced);
        assert!(bridged.bridged);
        let mut states = HashMap::from([
            ("BTCUSDT".to_owned(), bridged),
            ("TUSDUSDT".to_owned(), silent),
        ]);

        assert!(!sync_timed_out(
            &states,
            Some(Instant::now() - Duration::from_secs(1)),
            Instant::now(),
        ));

        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        archive_first_btc_aggregate_trade(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            &mut process_state,
        );
        let artifacts = close_segment(
            segment,
            &config,
            &states,
            "session-1",
            "test",
            &process_state,
        )
        .unwrap()
        .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], false);
        assert_eq!(manifest["snapshot_ready_count"], 2);
        assert_eq!(manifest["bridged_count"], 1);
        assert_eq!(manifest["snapshot_only_symbols"], json!(["TUSDUSDT"]));
        assert_eq!(manifest["all_symbols_bridged"], false);
        let output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(artifacts.data)
            .output()
            .unwrap();
        assert!(output.status.success());
        let checkpoint = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["symbol"] == "TUSDUSDT")
            .unwrap();
        assert_eq!(checkpoint["type"], "checkpoint");
        assert_eq!(checkpoint["replay_safe"], false);
        assert_eq!(checkpoint["synced"], true);
        assert_eq!(checkpoint["bridged"], false);

        write_health(
            &root,
            config.market,
            &config.dataset,
            "session-1",
            "synced",
            0,
            1,
            QueueHealth {
                capacity: 1,
                remaining_capacity: 1,
                saturated: false,
            },
            &states,
        )
        .unwrap();
        let health: Value =
            serde_json::from_reader(std::fs::File::open(root.join("health.json")).unwrap())
                .unwrap();
        assert_eq!(health["snapshot_ready_count"], 2);
        assert_eq!(health["bridged_count"], 1);
        assert_eq!(health["snapshot_only_symbols"], json!(["TUSDUSDT"]));
        assert_eq!(health["all_symbols_bridged"], false);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn trusted_epoch_without_aggregate_trade_closes_replay_unsafe() {
        assert!(Command::new("zstd")
            .arg("--version")
            .output()
            .expect("zstd is required for segment lifecycle tests")
            .status
            .success());
        let root = env::temp_dir().join(format!(
            "monday-untrusted-market-tape-test-{}",
            now_ns().unwrap()
        ));
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.clone();
        config.symbols = vec!["BTCUSDT".into()];
        let mut bridged = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(1);
        bridged
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["100", "1"]],
                    "asks": [["102", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        bridged
            .apply_diff(
                DepthDiff {
                    symbol: "BTCUSDT".into(),
                    first_update_id: 101,
                    final_update_id: 101,
                    previous_update_id: None,
                    bids: vec![["100".into(), "2".into()]],
                    asks: vec![],
                },
                &mut budget,
            )
            .unwrap();
        let states = HashMap::from([("BTCUSDT".to_owned(), bridged)]);
        let segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let artifacts = close_segment(
            segment,
            &config,
            &states,
            "session-1",
            "test",
            &trusted_process_state(&config.symbols),
        )
        .unwrap()
        .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], false);
        let output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(artifacts.data)
            .output()
            .unwrap();
        assert!(output.status.success());
        let checkpoint = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["type"] == "checkpoint")
            .unwrap();
        assert_eq!(checkpoint["replay_safe"], false);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_trade_stream_with_one_real_trade_certifies_static_symbols() {
        let root = tempfile::Builder::new()
            .prefix("monday-per-symbol-aggregate-trade-test-")
            .tempdir()
            .unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        config.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
        let mut budget = PendingBudget::new(2);
        let mut states = HashMap::new();
        for symbol in &config.symbols {
            let mut state = OrderBookState::new(symbol, Market::Spot);
            state.verify_stream_coverage();
            state
                .install_snapshot(
                    &json!({
                        "lastUpdateId": 100,
                        "bids": [["100", "1"]],
                        "asks": [["102", "1"]]
                    }),
                    &mut budget,
                )
                .unwrap();
            if symbol == "BTCUSDT" {
                state
                    .apply_diff(
                        DepthDiff {
                            symbol: symbol.clone(),
                            first_update_id: 101,
                            final_update_id: 101,
                            previous_update_id: None,
                            bids: vec![["100".into(), "2".into()]],
                            asks: vec![],
                        },
                        &mut budget,
                    )
                    .unwrap();
            }
            states.insert(symbol.clone(), state);
        }

        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let segment_start = segment.start_ns;
        write_checkpoints(
            &mut segment,
            &states,
            "session-1",
            "segment_open",
            true,
            segment_start,
        )
        .unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        archive_first_btc_aggregate_trade(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            &mut process_state,
        );
        let artifacts = close_segment(
            segment,
            &config,
            &states,
            "session-1",
            "test",
            &process_state,
        )
        .unwrap()
        .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], true);
        assert!(manifest["trade_summaries"].get("ETHUSDT").is_none());
        assert_eq!(manifest["lob_continuity"]["missing_symbols"], json!([]));
        assert_eq!(
            manifest["lob_continuity"]["symbols"]["ETHUSDT"]["diff_count"],
            0
        );
        assert_eq!(
            manifest["lob_continuity"]["symbols"]["ETHUSDT"]["stream_coverage_verified"],
            true
        );

        let output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(artifacts.data)
            .output()
            .unwrap();
        assert!(output.status.success());
        let eth_checkpoint = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["type"] == "checkpoint" && event["symbol"] == "ETHUSDT")
            .unwrap();
        assert_eq!(eth_checkpoint["replay_safe"], true);
    }

    #[test]
    fn unseeded_rotation_stays_replay_unsafe_after_later_trade() {
        let root = tempfile::Builder::new()
            .prefix("monday-unseeded-rotation-test-")
            .tempdir()
            .unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        let mut budget = PendingBudget::new(1);
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        state
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["100", "1"]],
                    "asks": [["102", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        state
            .apply_diff(
                DepthDiff {
                    symbol: "BTCUSDT".into(),
                    first_update_id: 101,
                    final_update_id: 101,
                    previous_update_id: None,
                    bids: vec![["100".into(), "2".into()]],
                    asks: vec![],
                },
                &mut budget,
            )
            .unwrap();
        let mut states = HashMap::from([("BTCUSDT".to_owned(), state)]);
        let process_state = trusted_process_state(&config.symbols);
        let segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut next = rotate_segment(
            segment,
            &config,
            &states,
            "session-1",
            "scheduled",
            &process_state,
        )
        .unwrap();

        let mut process_state = process_state;
        archive_first_btc_aggregate_trade(
            &config,
            &mut next,
            &mut states,
            &mut budget,
            &mut process_state,
        );
        let artifacts = close_segment(next, &config, &states, "session-1", "test", &process_state)
            .unwrap()
            .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], false);
    }

    #[test]
    fn reconnecting_shard_cannot_publish_a_replay_safe_checkpoint() {
        assert!(Command::new("zstd")
            .arg("--version")
            .output()
            .expect("zstd is required for segment lifecycle tests")
            .status
            .success());
        let root = tempfile::Builder::new()
            .prefix("monday-reconnecting-shard-test-")
            .tempdir()
            .unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        let mut budget = PendingBudget::new(1);
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        state
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["100", "1"]],
                    "asks": [["102", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        state
            .apply_diff(
                DepthDiff {
                    symbol: "BTCUSDT".into(),
                    first_update_id: 101,
                    final_update_id: 101,
                    previous_update_id: None,
                    bids: vec![["100".into(), "2".into()]],
                    asks: vec![],
                },
                &mut budget,
            )
            .unwrap();
        let mut states = HashMap::from([("BTCUSDT".to_owned(), state)]);
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        archive_first_btc_aggregate_trade(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            &mut process_state,
        );
        process_state.mark_shard_disconnected(vec![
            "btcusdt@depth@100ms".into(),
            "btcusdt@aggTrade".into(),
        ]);
        process_state.mark_stream_observed("btcusdt@aggTrade");
        assert!(!process_state.streams_healthy());
        assert!(!process_state.depth_streams_healthy());

        let mut aggregate_only_disconnect = trusted_process_state(&config.symbols);
        aggregate_only_disconnect.mark_shard_disconnected(vec!["btcusdt@aggTrade".into()]);
        assert!(!replay_checkpoint_ready(
            &segment,
            &states,
            &aggregate_only_disconnect
        ));

        let artifacts = close_segment(
            segment,
            &config,
            &states,
            "session-1",
            "test",
            &process_state,
        )
        .unwrap()
        .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], false);
    }

    #[test]
    fn scheduled_rotation_seeds_but_sequence_gap_does_not() {
        assert!(Command::new("zstd")
            .arg("--version")
            .output()
            .expect("zstd is required for segment lifecycle tests")
            .status
            .success());
        let root = tempfile::Builder::new()
            .prefix("monday-scheduled-segment-open-test-")
            .tempdir()
            .unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        config.symbols = vec!["BTCUSDT".into()];
        let mut state = OrderBookState::new("BTCUSDT", Market::Spot);
        let mut budget = PendingBudget::new(1);
        state
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["100", "1"]],
                    "asks": [["102", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        state
            .apply_diff(
                DepthDiff {
                    symbol: "BTCUSDT".into(),
                    first_update_id: 101,
                    final_update_id: 101,
                    previous_update_id: None,
                    bids: vec![["100".into(), "2".into()]],
                    asks: vec![],
                },
                &mut budget,
            )
            .unwrap();
        let mut states = HashMap::from([("BTCUSDT".to_owned(), state)]);
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        archive_first_btc_aggregate_trade(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            &mut process_state,
        );

        let mut next = rotate_segment(
            segment,
            &config,
            &states,
            "session-1",
            "scheduled",
            &process_state,
        )
        .unwrap();
        let next_start_ns = next.start_ns;
        let received_at_ns = now_ns().unwrap();
        let event_time_ms = received_at_ns / 1_000_000;
        let trade = event_from_frame(
            json!({
                "stream": "btcusdt@aggTrade",
                "data": {"e":"aggTrade","E":event_time_ms,"s":"BTCUSDT","a":10,"f":12,"l":13,"p":"101","q":"0.2","T":event_time_ms,"m":true}
            }),
            received_at_ns,
        )
        .unwrap();
        process_event(
            &config,
            &mut next,
            &mut states,
            &mut budget,
            "session-1",
            trade,
            &mut process_state,
        )
        .unwrap();
        let artifacts = close_segment(next, &config, &states, "session-1", "test", &process_state)
            .unwrap()
            .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], true);
        let output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(artifacts.data)
            .output()
            .unwrap();
        assert!(output.status.success());
        let opening = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["type"] == "checkpoint" && event["reason"] == "segment_open")
            .expect("scheduled rotation must seed the next segment");
        assert_eq!(opening["session_id"], "session-1");
        assert_eq!(opening["received_at_ns"], next_start_ns);
        assert_eq!(opening["replay_safe"], true);
        assert_eq!(opening["bridged"], true);

        let mut gap_segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        archive_btc_aggregate_trade(
            &config,
            &mut gap_segment,
            &mut states,
            &mut budget,
            &mut process_state,
            11,
        );
        process_state.sequence_gaps = 1;
        let after_gap = rotate_segment(
            gap_segment,
            &config,
            &states,
            "session-1",
            "scheduled",
            &process_state,
        )
        .unwrap();
        let gap_artifacts = close_segment(
            after_gap,
            &config,
            &states,
            "session-1",
            "test",
            &process_state,
        )
        .unwrap()
        .unwrap();
        let gap_output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(gap_artifacts.data)
            .output()
            .unwrap();
        assert!(gap_output.status.success());
        assert!(!String::from_utf8(gap_output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .any(|event| event["type"] == "checkpoint" && event["reason"] == "segment_open"));
    }

    #[test]
    fn post_warmup_rotation_can_seed_after_an_incomplete_segment() {
        assert!(Command::new("zstd")
            .arg("--version")
            .output()
            .expect("zstd is required for segment lifecycle tests")
            .status
            .success());
        let root = tempfile::Builder::new()
            .prefix("monday-post-warmup-seed-test-")
            .tempdir()
            .unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        config.symbols = vec!["BTCUSDT".into()];
        let mut states = HashMap::from([(
            "BTCUSDT".to_owned(),
            OrderBookState::new("BTCUSDT", Market::Spot),
        )]);
        let mut budget = PendingBudget::new(1);
        let mut process_state = trusted_process_state(&config.symbols);
        let segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();

        // The first rotation occurs before snapshots complete, so its output
        // remains replay-unsafe and the next segment starts without a seed.
        let mut next = rotate_segment(
            segment,
            &config,
            &states,
            "session-1",
            "scheduled",
            &process_state,
        )
        .unwrap();
        assert!(!next.is_replay_safe());
        let state = states.get_mut("BTCUSDT").unwrap();
        state
            .install_snapshot(
                &json!({
                    "lastUpdateId": 100,
                    "bids": [["100", "1"]],
                    "asks": [["102", "1"]]
                }),
                &mut budget,
            )
            .unwrap();
        state
            .apply_diff(
                DepthDiff {
                    symbol: "BTCUSDT".into(),
                    first_update_id: 101,
                    final_update_id: 101,
                    previous_update_id: None,
                    bids: vec![["100".into(), "2".into()]],
                    asks: vec![],
                },
                &mut budget,
            )
            .unwrap();
        archive_first_btc_aggregate_trade(
            &config,
            &mut next,
            &mut states,
            &mut budget,
            &mut process_state,
        );

        let mut seeded = rotate_segment(
            next,
            &config,
            &states,
            "session-1",
            "scheduled",
            &process_state,
        )
        .unwrap();
        let seeded_start_ns = seeded.start_ns;
        let received_at_ns = now_ns().unwrap();
        let event_time_ms = received_at_ns / 1_000_000;
        let trade = event_from_frame(
            json!({
                "stream": "btcusdt@aggTrade",
                "data": {"e":"aggTrade","E":event_time_ms,"s":"BTCUSDT","a":10,"f":12,"l":13,"p":"101","q":"0.2","T":event_time_ms,"m":true}
            }),
            received_at_ns,
        )
        .unwrap();
        process_event(
            &config,
            &mut seeded,
            &mut states,
            &mut budget,
            "session-1",
            trade,
            &mut process_state,
        )
        .unwrap();
        let artifacts = close_segment(seeded, &config, &states, "session-1", "test", &process_state)
            .unwrap()
            .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], true);
        let output = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(artifacts.data)
            .output()
            .unwrap();
        assert!(output.status.success());
        let opening = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|event| event["type"] == "checkpoint" && event["reason"] == "segment_open")
            .expect("post-warmup rotation must seed the next segment");
        assert_eq!(opening["received_at_ns"], seeded_start_ns);
        assert_eq!(opening["replay_safe"], true);
    }

    #[test]
    fn missing_snapshot_times_out_and_closes_replay_unsafe() {
        assert!(Command::new("zstd")
            .arg("--version")
            .output()
            .expect("zstd is required for segment lifecycle tests")
            .status
            .success());
        let root = env::temp_dir().join(format!(
            "monday-missing-snapshot-test-{}",
            now_ns().unwrap()
        ));
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.clone();
        config.symbols = vec!["MISSINGUSDT".into()];
        let mut states = HashMap::from([(
            "MISSINGUSDT".to_owned(),
            OrderBookState::new("MISSINGUSDT", Market::Spot),
        )]);
        let mut budget = PendingBudget::new(1);
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut process_state = trusted_process_state(&config.symbols);
        let received_at_ns = now_ns().unwrap();
        let event_time_ms = received_at_ns / 1_000_000 - 1;
        let event = event_from_frame(
            json!({
                "stream": "missingusdt@depth@100ms",
                "data": {"e":"depthUpdate","E":event_time_ms,"s": "MISSINGUSDT", "U": 101, "u": 101, "b": [], "a": []}
            }),
            received_at_ns,
        )
        .unwrap();
        process_event(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            "session-1",
            event,
            &mut process_state,
        )
        .unwrap();
        let now = Instant::now();
        assert!(sync_timed_out(
            &states,
            Some(now - Duration::from_millis(1)),
            now,
        ));

        let artifacts = close_segment(
            segment,
            &config,
            &states,
            "session-1",
            "test",
            &trusted_process_state(&config.symbols),
        )
        .unwrap()
        .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], false);
        assert_eq!(manifest["snapshot_ready_count"], 0);
        assert_eq!(manifest["bridged_count"], 0);
        assert_eq!(manifest["snapshot_only_symbols"], json!([]));
        assert_eq!(manifest["all_symbols_bridged"], false);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapshot_backoff_honors_retry_after_and_caps_exponential_delay() {
        assert_eq!(
            snapshot_retry_delay(Some("0.25"), 0),
            Duration::from_millis(250)
        );
        assert_eq!(snapshot_retry_delay(None, 0), Duration::from_secs(1));
        assert_eq!(snapshot_retry_delay(None, 99), Duration::from_secs(32));
        assert_eq!(snapshot_retry_delay(Some("NaN"), 0), Duration::from_secs(1));
        assert_eq!(snapshot_retry_delay(Some("-1"), 1), Duration::from_secs(2));
    }

    #[tokio::test]
    async fn hot_queue_cannot_starve_post_select_rotation_check() {
        let (sender, mut receiver) = mpsc::channel(8);
        for value in 0..8 {
            sender.send(value).await.unwrap();
        }
        let start_ns = 1_700_000_000_000_000_000_u64;
        let mut now_ns = start_ns;
        let mut rotated = false;
        for _ in 0..8 {
            tokio::select! {
                biased;
                value = receiver.recv() => assert!(value.is_some()),
                _ = tokio::time::sleep(Duration::from_secs(3600)) => {
                    panic!("hot queue unexpectedly selected timer")
                }
            }
            now_ns += 10_000_000_000;
            if segment_due_at(start_ns, now_ns, 60).unwrap() {
                rotated = true;
                break;
            }
        }
        assert!(rotated, "post-select maintenance must rotate a hot queue");
    }

    #[tokio::test]
    async fn rotation_barriers_drain_captured_events_before_segment_boundary() {
        let root = tempfile::tempdir().unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut states = config
            .symbols
            .iter()
            .map(|symbol| (symbol.clone(), OrderBookState::new(symbol, config.market)))
            .collect::<HashMap<_, _>>();
        let mut budget = PendingBudget::new(config.max_pending_diffs);
        let mut process_state = ProcessState::new(false);
        let (sender, mut receiver) = mpsc::channel(1);
        let mut tasks = JoinSet::new();
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (pause_tx, mut pause_rx) = watch::channel(0_u64);
        let (resume_tx, mut resume_rx) = watch::channel(0_u64);
        let (captured_at_tx, captured_at_rx) = tokio::sync::oneshot::channel();
        sender
            .send(Event::StreamDisconnected {
                streams: vec!["btcusdt@depth@100ms".into()],
                reason: "test".into(),
            })
            .await
            .unwrap();
        let blocked_sender = sender.clone();
        let blocked_task = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            let received_at_ns = now_ns()?;
            captured_at_tx.send(received_at_ns).unwrap();
            send_or_shutdown(
                &blocked_sender,
                Event::Snapshot {
                    received_at_ns,
                    symbol: "BTCUSDT".into(),
                    request_started_at_ns: received_at_ns,
                    snapshot: json!({
                        "lastUpdateId": 1,
                        "bids": [["100", "1"]],
                        "asks": [["101", "1"]]
                    }),
                },
                &mut shutdown_rx,
            )
            .await?;
            let mut last_pause_epoch = 0;
            acknowledge_rotation_pause(
                0,
                &blocked_sender,
                &mut pause_rx,
                &mut resume_rx,
                &mut last_pause_epoch,
                &mut shutdown_rx,
            )
            .await
        });
        let captured_at_ns = captured_at_rx.await.unwrap();
        pause_tx.send(1).unwrap();

        let barriers = tokio::time::timeout(
            Duration::from_secs(1),
            await_rotation_barriers(
                &config,
                &mut receiver,
                &mut tasks,
                &mut segment,
                &mut states,
                &mut budget,
                "session-1",
                &mut process_state,
                1,
                1,
            ),
        )
        .await
        .expect("rotation barriers deadlocked on a full archive queue")
        .unwrap();

        assert_eq!(
            barriers,
            RotationBarrierResult::Ready {
                initial_snapshots_complete: false
            }
        );
        assert!(captured_at_ns < now_ns().unwrap());
        assert_eq!(segment.event_count("snapshot"), 1);
        assert!(receiver.try_recv().is_err());
        assert!(!segment.is_replay_safe());
        let mut closing_segment = segment;
        let next_segment = begin_segment_rotation(
            &mut closing_segment,
            &config,
            &states,
            "session-1",
            "scheduled",
            &process_state,
        )
        .unwrap();
        assert!(captured_at_ns <= next_segment.start_ns);
        tokio::task::yield_now().await;
        assert!(!blocked_task.is_finished());
        resume_tx.send(1).unwrap();
        assert!(blocked_task.await.unwrap().unwrap().is_none());
        drop(next_segment);
        drop(closing_segment);
    }

    #[tokio::test]
    async fn rotation_barrier_observes_unexpected_producer_exit() {
        let root = tempfile::tempdir().unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        let mut segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let mut states = config
            .symbols
            .iter()
            .map(|symbol| (symbol.clone(), OrderBookState::new(symbol, config.market)))
            .collect::<HashMap<_, _>>();
        let mut budget = PendingBudget::new(config.max_pending_diffs);
        let mut process_state = ProcessState::new(false);
        let (_sender, mut receiver) = mpsc::channel(1);
        let mut tasks = JoinSet::new();
        tasks.spawn(async { Ok::<_, anyhow::Error>(TaskExit::Stopped(None)) });

        let result = tokio::time::timeout(
            Duration::from_millis(250),
            await_rotation_barriers(
                &config,
                &mut receiver,
                &mut tasks,
                &mut segment,
                &mut states,
                &mut budget,
                "session-1",
                &mut process_state,
                1,
                1,
            ),
        )
        .await
        .expect("rotation barrier wait must observe an exited producer");
        assert!(result.is_err());
        assert!(tasks.is_empty());
    }

    #[test]
    fn failed_rotation_close_leaves_next_segment_for_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let mut config = test_config("http://unused".into());
        config.spool_dir = root.path().to_path_buf();
        let start_ns = now_ns().unwrap();
        let mut segment = Segment::create(config.segment_config(), start_ns).unwrap();
        segment
            .write("session_start", json!({"session_id":"session-1"}), start_ns)
            .unwrap();
        let (date, hour) = segment_partition(start_ns).unwrap();
        std::fs::remove_file(
            root.path()
                .join(format!("date={date}"))
                .join(format!("hour={hour}"))
                .join(format!("part-{start_ns}.jsonl.part")),
        )
        .unwrap();
        let next_segment = Segment::create(config.segment_config(), start_ns + 1).unwrap();
        let (resume_tx, mut resume_rx) = watch::channel(0_u64);

        assert!(finish_segment_rotation(&mut segment, next_segment, &resume_tx, 1).is_err());
        assert_eq!(*resume_rx.borrow_and_update(), 1);

        segment.mark_replay_unsafe();
        assert!(segment.close().unwrap().is_none());
    }

    #[test]
    fn segment_rotation_aligns_to_utc_boundaries() {
        let minute_ns = 60 * 1_000_000_000_u64;
        let start_ns = 49 * minute_ns + 58 * 1_000_000_000;

        assert!(
            !segment_due_at(start_ns, 49 * minute_ns + 59 * 1_000_000_000, 600).unwrap()
        );
        assert!(segment_due_at(start_ns, 50 * minute_ns, 600).unwrap());
        assert!(
            !segment_due_at(50 * minute_ns, 59 * minute_ns + 58 * 1_000_000_000, 600).unwrap()
        );
        assert!(segment_due_at(50 * minute_ns, 60 * minute_ns, 600).unwrap());
    }
}
