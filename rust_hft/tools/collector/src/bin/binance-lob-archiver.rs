use anyhow::Context;
use clap::Parser;
use futures::StreamExt;
use hft_collector::lob_archiver::{
    checkpoint_event, command_status_with_timeout, files_with_suffix, read_upload_status,
    recover_parts, segment_partition, send_or_shutdown, write_health, write_success_marker,
    write_upload_status, DepthDiff, Market, OrderBookState, PendingBudget, QueueHealth, Segment,
    SegmentConfig, SendOutcome, RAW_SCHEMA,
};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

#[derive(Debug, Parser)]
#[command(name = "binance-lob-archiver")]
struct Args {
    #[arg(long)]
    self_test: bool,

    #[arg(long, conflicts_with = "self_test")]
    upload_only: bool,
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
            .open(&path)
            .with_context(|| format!("failed to open spool lock {}", path.display()))?;
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

    fn stream_urls(&self) -> Vec<String> {
        let base = match self.market {
            Market::Spot => "wss://data-stream.binance.vision/stream?streams=",
            Market::Usdm => "wss://fstream.binance.com/stream?streams=",
        };
        self.active_symbols()
            .chunks(self.ws_shard_size)
            .map(|symbols| {
                format!(
                    "{base}{}",
                    symbols
                        .iter()
                        .map(|symbol| format!("{}@depth@100ms", symbol.to_ascii_lowercase()))
                        .collect::<Vec<_>>()
                        .join("/")
                )
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
enum Event {
    Diff {
        received_at_ns: u64,
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
}

#[derive(Debug)]
enum TaskExit {
    Stopped(Option<Event>),
    SnapshotComplete,
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessAction {
    None,
    Resnapshot(String),
    Excluded,
    InitialSnapshotsComplete,
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
    armed: AtomicBool,
    shutdown: AtomicBool,
}

impl ProcessWatchdog {
    fn start(timeout: Duration) -> anyhow::Result<Self> {
        let watchdog = Self {
            inner: Arc::new(ProcessWatchdogInner {
                started: Instant::now(),
                last_data_ms: AtomicU64::new(0),
                armed: AtomicBool::new(false),
                shutdown: AtomicBool::new(false),
            }),
        };
        let monitor = watchdog.clone();
        thread::Builder::new()
            .name("binance-lob-process-watchdog".into())
            .spawn(move || {
                let interval = timeout
                    .div_f64(4.0)
                    .clamp(Duration::from_secs(1), Duration::from_secs(10));
                while !monitor.inner.shutdown.load(Ordering::Relaxed) {
                    thread::sleep(interval);
                    let now_ms = monitor.elapsed_ms();
                    let last_ms = monitor.inner.last_data_ms.load(Ordering::Relaxed);
                    if monitor.inner.armed.load(Ordering::Relaxed)
                        && process_watchdog_expired(last_ms, now_ms, timeout)
                    {
                        error!(
                            silent_ms = now_ms.saturating_sub(last_ms),
                            "process watchdog exiting after market-data stall"
                        );
                        std::process::exit(75);
                    }
                }
            })?;
        watchdog.mark_data();
        watchdog.inner.armed.store(true, Ordering::Relaxed);
        Ok(watchdog)
    }

    fn mark_data(&self) {
        self.inner
            .last_data_ms
            .store(self.elapsed_ms(), Ordering::Relaxed);
    }

    fn stop(&self) {
        self.inner.shutdown.store(true, Ordering::Relaxed);
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.inner.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn process_watchdog_expired(last_data_ms: u64, now_ms: u64, timeout: Duration) -> bool {
    now_ms.saturating_sub(last_data_ms) > timeout.as_millis() as u64
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
    let shutdown_signal = tokio::spawn(wait_for_signal(shutdown_tx.clone()));

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
    let _ = shutdown_tx.send(true);
    watchdog.stop();
    upload_task.await?;
    shutdown_signal.abort();
    Ok(())
}

async fn run_session(
    config: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
    watchdog: ProcessWatchdog,
) -> anyhow::Result<()> {
    let session_id = format!("{:x}-{}", now_ns()?, std::process::id());
    let active_symbols = config.active_symbols();
    if active_symbols.is_empty() {
        anyhow::bail!("no active symbols remain after runtime exclusions");
    }
    let (sender, mut receiver) = mpsc::channel(config.max_buffered_diffs);
    let (session_stop_tx, session_stop_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    for url in config.stream_urls() {
        tasks.spawn(receive_url(
            url,
            sender.clone(),
            session_stop_rx.clone(),
            config.stall_timeout,
            watchdog.clone(),
        ));
    }
    tasks.spawn(produce_snapshots(
        config.clone(),
        sender.clone(),
        session_stop_rx.clone(),
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
            "websocket_shards": config.stream_urls().len(),
        }),
        now_ns()?,
    )?;
    let mut sequence_gaps = 0_u64;
    let mut last_health = Instant::now() - Duration::from_secs(60);
    let mut failure = None;
    let mut sync_deadline = None;

    loop {
        let mut pending_action = ProcessAction::None;
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                changed?;
                break;
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok(Ok(TaskExit::Stopped(Some(event))))) => {
                        pending_action = process_event(
                            &config, &mut segment, &mut states, &mut budget, &session_id, event, &mut sequence_gaps
                        )?;
                    }
                    Some(Ok(Ok(TaskExit::SnapshotComplete))) => {},
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
                            &config, &mut segment, &mut states, &mut budget, &session_id, event, &mut sequence_gaps
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
            ProcessAction::None => {}
            ProcessAction::Resnapshot(symbol) => {
                segment = rotate_segment(segment, &config, &states, &session_id, "sequence_gap")?;
                sync_deadline = Some(Instant::now() + config.sync_timeout);
                tasks.spawn(produce_snapshot(
                    config.clone(),
                    symbol,
                    sender.clone(),
                    session_stop_rx.clone(),
                ));
            }
            ProcessAction::Excluded => {
                segment =
                    rotate_segment(segment, &config, &states, &session_id, "symbol_excluded")?;
            }
            ProcessAction::InitialSnapshotsComplete => {
                sync_deadline = Some(Instant::now() + config.sync_timeout);
            }
        }

        // This must live outside the biased select: under continuous market
        // data receiver.recv() is always ready and the timer branch can starve.
        if segment_due(&segment, config.segment_seconds)? {
            segment = rotate_segment(segment, &config, &states, &session_id, "scheduled")?;
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
            let status = if states.values().all(|state| state.synced) {
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
                sequence_gaps,
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
    let _ = close_segment(segment, &config, &states, &session_id, "shutdown")?;
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
        sequence_gaps,
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
    sequence_gaps: &mut u64,
) -> anyhow::Result<ProcessAction> {
    match event {
        Event::Diff {
            received_at_ns,
            frame,
        } => {
            let diff = DepthDiff::from_frame(&frame)?;
            if config.is_excluded(&diff.symbol) {
                return Ok(ProcessAction::None);
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
                *sequence_gaps += 1;
                segment.write(
                    "sequence_gap",
                    json!({"session_id":session_id,"error":error.to_string()}),
                    now_ns()?,
                )?;
                if let Some(gap) = error.downcast_ref::<hft_collector::lob_archiver::SequenceGap>()
                {
                    let symbol = gap.symbol.clone();
                    state.invalidate_for_resync(budget);
                    return Ok(ProcessAction::Resnapshot(symbol));
                }
                return Err(error);
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
                *sequence_gaps += 1;
                segment.write(
                    "sequence_gap",
                    json!({"session_id":session_id,"error":error.to_string()}),
                    now_ns()?,
                )?;
                if let Some(gap) = error.downcast_ref::<hft_collector::lob_archiver::SequenceGap>()
                {
                    let symbol = gap.symbol.clone();
                    state.invalidate_for_resync(budget);
                    return Ok(ProcessAction::Resnapshot(symbol));
                }
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
    }
    Ok(ProcessAction::None)
}

fn segment_due(segment: &Segment, segment_seconds: u64) -> anyhow::Result<bool> {
    segment_due_at(segment.start_ns, now_ns()?, segment_seconds)
}

fn segment_due_at(start_ns: u64, now_ns: u64, segment_seconds: u64) -> anyhow::Result<bool> {
    Ok(
        now_ns.saturating_sub(start_ns) >= segment_seconds * 1_000_000_000
            || segment_partition(start_ns)? != segment_partition(now_ns)?,
    )
}

fn archive_only(segment: &mut Segment, session_id: &str, event: Event) -> anyhow::Result<()> {
    segment.mark_replay_unsafe();
    match event {
        Event::Diff {
            received_at_ns,
            frame,
        } => segment.write(
            "diff",
            json!({"session_id":session_id,"archived_only":true,"frame":frame}),
            received_at_ns,
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
    }
}

fn rotate_segment(
    segment: Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
) -> anyhow::Result<Segment> {
    close_segment(segment, config, states, session_id, reason)?;
    Segment::create(config.segment_config(), now_ns()?)
}

fn close_segment(
    mut segment: Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
) -> anyhow::Result<Option<hft_collector::lob_archiver::SegmentArtifacts>> {
    let catalog = config.segment_config();
    segment.update_catalog(
        catalog.symbols,
        catalog.security_token_symbols,
        catalog.excluded_symbols,
    );
    if states.values().all(|state| state.synced) {
        for state in states.values() {
            segment.write(
                "checkpoint",
                checkpoint_event(state.checkpoint(session_id)?, reason),
                now_ns()?,
            )?;
        }
    } else {
        segment.mark_replay_unsafe();
    }
    segment.close()
}

async fn receive_url(
    url: String,
    sender: mpsc::Sender<Event>,
    mut shutdown: watch::Receiver<bool>,
    stall_timeout: Duration,
    watchdog: ProcessWatchdog,
) -> anyhow::Result<TaskExit> {
    let (mut websocket, _) = tokio::time::timeout(Duration::from_secs(20), connect_async(&url))
        .await
        .context("websocket connect timed out")??;
    loop {
        let message = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                changed?;
                let _ = tokio::time::timeout(Duration::from_secs(5), websocket.close(None)).await;
                return Ok(TaskExit::Stopped(None));
            }
            message = tokio::time::timeout(stall_timeout, websocket.next()) => {
                message.context("websocket shard stalled")?
                    .ok_or_else(|| anyhow::anyhow!("websocket closed"))??
            }
        };
        if let Message::Text(text) = message {
            watchdog.mark_data();
            let event = Event::Diff {
                received_at_ns: now_ns()?,
                frame: serde_json::from_str(&text)?,
            };
            match send_or_shutdown(&sender, event, &mut shutdown).await? {
                SendOutcome::Sent => {}
                SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
            }
        }
    }
}

async fn produce_snapshots(
    config: Arc<Config>,
    sender: mpsc::Sender<Event>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<TaskExit> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let interval = Duration::from_secs_f64(1.0 / config.snapshot_requests_per_second.max(0.1));
    for symbol in config.active_symbols() {
        if *shutdown.borrow() {
            return Ok(TaskExit::Stopped(None));
        }
        let started = now_ns()?;
        let snapshot = match fetch_snapshot(&client, &config, &symbol, &mut shutdown).await {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) => return Ok(TaskExit::Stopped(None)),
            Err(error) => {
                if error.downcast_ref::<SnapshotUnavailable>().is_some() {
                    let event = Event::ExcludeSymbol {
                        symbol: symbol.clone(),
                        reason: error.to_string(),
                    };
                    match send_or_shutdown(&sender, event, &mut shutdown).await? {
                        SendOutcome::Sent => continue,
                        SendOutcome::Shutdown(event) => {
                            return Ok(TaskExit::Stopped(Some(event)));
                        }
                    }
                }
                return Err(error);
            }
        };
        let event = Event::Snapshot {
            received_at_ns: now_ns()?,
            symbol: symbol.clone(),
            request_started_at_ns: started,
            snapshot,
        };
        match send_or_shutdown(&sender, event, &mut shutdown).await? {
            SendOutcome::Sent => {}
            SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
        }
        if wait_or_shutdown(&mut shutdown, interval).await {
            return Ok(TaskExit::Stopped(None));
        }
    }
    match send_or_shutdown(&sender, Event::InitialSnapshotsComplete, &mut shutdown).await? {
        SendOutcome::Sent => {}
        SendOutcome::Shutdown(event) => return Ok(TaskExit::Stopped(Some(event))),
    }
    // Keep the producer alive so a normal completion is not mistaken for a session failure.
    shutdown.changed().await?;
    Ok(TaskExit::Stopped(None))
}

async fn produce_snapshot(
    config: Arc<Config>,
    symbol: String,
    sender: mpsc::Sender<Event>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<TaskExit> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    let started = now_ns()?;
    let snapshot = match fetch_snapshot(&client, &config, &symbol, &mut shutdown).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(TaskExit::Stopped(None)),
        Err(error) => {
            if error.downcast_ref::<SnapshotUnavailable>().is_some() {
                let event = Event::ExcludeSymbol {
                    symbol,
                    reason: error.to_string(),
                };
                return match send_or_shutdown(&sender, event, &mut shutdown).await? {
                    SendOutcome::Sent => Ok(TaskExit::SnapshotComplete),
                    SendOutcome::Shutdown(event) => Ok(TaskExit::Stopped(Some(event))),
                };
            }
            return Err(error);
        }
    };
    let event = Event::Snapshot {
        received_at_ns: now_ns()?,
        symbol,
        request_started_at_ns: started,
        snapshot,
    };
    match send_or_shutdown(&sender, event, &mut shutdown).await? {
        SendOutcome::Sent => Ok(TaskExit::SnapshotComplete),
        SendOutcome::Shutdown(event) => Ok(TaskExit::Stopped(Some(event))),
    }
}

async fn fetch_snapshot(
    client: &reqwest::Client,
    config: &Config,
    symbol: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<Value>> {
    let path = match config.market {
        Market::Spot => "/api/v3/depth",
        Market::Usdm => "/fapi/v1/depth",
    };
    let limit = config.snapshot_limit.to_string();
    for attempt in 0..config.snapshot_retry_attempts {
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
            return Ok(Some(snapshot));
        }
        let retryable =
            status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
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
        if wait_or_shutdown(shutdown, delay).await {
            return Ok(None);
        }
    }
    unreachable!("snapshot_retry_attempts is at least one")
}

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
    let path = match market {
        Market::Spot => "/api/v3/exchangeInfo",
        Market::Usdm => "/fapi/v1/exchangeInfo",
    };
    let payload: Value = reqwest::Client::new()
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
    let mut bytes = serde_json::to_vec(&json!({
        "schema": UPLOADED_CLEANUP_SCHEMA,
        "data": local_file_name(data)?,
        "manifest": manifest_name,
        "success": local_file_name(success)?,
    }))?;
    bytes.push(b'\n');
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    std::io::Write::write_all(&mut output, &bytes)?;
    output.sync_all()?;
    std::fs::rename(&temporary, &marker)?;
    sync_parent_directory(&marker)?;
    Ok(marker)
}

fn recover_uploaded_cleanups(spool_dir: &Path) -> anyhow::Result<usize> {
    let markers = files_with_suffix(spool_dir, UPLOADED_CLEANUP_SUFFIX)?;
    for marker in &markers {
        cleanup_uploaded_marker(marker)?;
    }
    Ok(markers.len())
}

fn cleanup_uploaded_marker(marker: &Path) -> anyhow::Result<()> {
    let metadata: Value = serde_json::from_reader(std::fs::File::open(marker)?)
        .with_context(|| format!("invalid uploaded cleanup marker {}", marker.display()))?;
    if metadata["schema"] != UPLOADED_CLEANUP_SCHEMA {
        anyhow::bail!(
            "invalid uploaded cleanup marker schema: {}",
            marker.display()
        );
    }
    for field in ["data", "manifest", "success"] {
        let path = cleanup_artifact_path(marker, &metadata, field)?;
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

fn cleanup_artifact_path(marker: &Path, metadata: &Value, field: &str) -> anyhow::Result<PathBuf> {
    let name = metadata[field]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("uploaded cleanup marker missing {field}"))?;
    if Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name) {
        anyhow::bail!("uploaded cleanup marker contains invalid {field} file name");
    }
    let path = marker
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cleanup marker has no parent: {}", marker.display()))?
        .join(name);
    if path == marker {
        anyhow::bail!("uploaded cleanup marker cannot reference itself");
    }
    Ok(path)
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

async fn wait_for_signal(shutdown: watch::Sender<bool>) {
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
    let _ = shutdown.send(true);
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

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
    async fn initial_snapshot_completion_is_emitted_after_last_snapshot() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 38\r\nConnection: close\r\n\r\n{\"lastUpdateId\":1,\"bids\":[],\"asks\":[]}";
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).unwrap();
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let mut config = test_config(format!("http://{address}"));
        config.symbols = vec!["BTCUSDT".into(), "ETHUSDT".into()];
        config.snapshot_requests_per_second = 1_000.0;
        let (sender, mut receiver) = mpsc::channel(8);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let producer = tokio::spawn(produce_snapshots(Arc::new(config), sender, shutdown_rx));

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
            &mut 0,
        )
        .unwrap();
        assert_eq!(action, ProcessAction::Excluded);
        assert_eq!(config.active_symbols(), ["BTCUSDT"]);
        assert!(config.is_excluded("BADUSDT"));
        assert!(!states.contains_key("BADUSDT"));

        let artifacts = close_segment(segment, &config, &states, "session-1", "test")
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

    #[test]
    fn snapshot_only_silent_symbol_is_synced_and_closes_replay_safe() {
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
        let states = HashMap::from([
            ("BTCUSDT".to_owned(), bridged),
            ("TUSDUSDT".to_owned(), silent),
        ]);

        assert!(!sync_timed_out(
            &states,
            Some(Instant::now() - Duration::from_secs(1)),
            Instant::now(),
        ));

        let segment = Segment::create(config.segment_config(), now_ns().unwrap()).unwrap();
        let artifacts = close_segment(segment, &config, &states, "session-1", "test")
            .unwrap()
            .unwrap();
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["has_replay_safe_checkpoint"], true);
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
        assert_eq!(checkpoint["replay_safe"], true);
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
        process_event(
            &config,
            &mut segment,
            &mut states,
            &mut budget,
            "session-1",
            Event::Diff {
                received_at_ns: now_ns().unwrap(),
                frame: json!({
                    "stream": "missingusdt@depth@100ms",
                    "data": {"s": "MISSINGUSDT", "U": 101, "u": 101, "b": [], "a": []}
                }),
            },
            &mut 0,
        )
        .unwrap();
        let now = Instant::now();
        assert!(sync_timed_out(
            &states,
            Some(now - Duration::from_millis(1)),
            now,
        ));

        let artifacts = close_segment(segment, &config, &states, "session-1", "test")
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
}
