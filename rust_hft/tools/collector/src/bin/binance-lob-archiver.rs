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
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
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
}

#[derive(Debug, Clone)]
struct Config {
    market: Market,
    dataset: String,
    shard_id: String,
    symbols: Vec<String>,
    ws_shard_size: usize,
    snapshot_limit: u64,
    snapshot_requests_per_second: f64,
    segment_seconds: u64,
    spool_dir: PathBuf,
    max_buffered_diffs: usize,
    max_pending_diffs: usize,
    stall_timeout: Duration,
    rest_base: String,
    oss_bucket: String,
    oss_endpoint: String,
    oss_region: String,
    aliyun_profile: String,
    zstd_timeout: Duration,
    oss_copy_timeout: Duration,
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
        let symbols = if setting.eq_ignore_ascii_case("ALL") {
            discover_symbols(market, &rest_base).await?
        } else {
            setting
                .split(',')
                .map(|symbol| symbol.trim().to_ascii_uppercase())
                .filter(|symbol| !symbol.is_empty())
                .collect()
        };
        if symbols.is_empty() {
            anyhow::bail!("SYMBOLS must not be empty");
        }
        Ok(Self {
            market,
            dataset: env_string("DATASET", &format!("{}_all", market.as_str())),
            shard_id: env_string("SHARD_ID", "all"),
            symbols,
            ws_shard_size: env_parse("WS_SHARD_SIZE", 100_usize)?.max(1),
            snapshot_limit: env_parse("SNAPSHOT_LIMIT", 100_u64)?,
            snapshot_requests_per_second: env_parse("SNAPSHOT_REQUESTS_PER_SECOND", 15_f64)?,
            segment_seconds: env_parse("SEGMENT_SECONDS", 3600_u64)?.max(60),
            spool_dir: PathBuf::from(env_string("SPOOL_DIR", "/data/monday/spool/binance-lob")),
            max_buffered_diffs: env_parse("MAX_BUFFERED_DIFFS", 250_000_usize)?.max(1),
            max_pending_diffs: env_parse("MAX_PENDING_DIFFS_TOTAL", 250_000_usize)?.max(1),
            stall_timeout: Duration::from_secs(env_parse("STALL_TIMEOUT_SECONDS", 60_u64)?),
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
            symbols: self.symbols.clone(),
            security_token_symbols: Vec::new(),
            excluded_symbols: Vec::new(),
            snapshot_limit: self.snapshot_limit,
            zstd_timeout: self.zstd_timeout,
        }
    }

    fn stream_urls(&self) -> Vec<String> {
        let base = match self.market {
            Market::Spot => "wss://data-stream.binance.vision/stream?streams=",
            Market::Usdm => "wss://fstream.binance.com/stream?streams=",
        };
        self.symbols
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
}

#[derive(Debug)]
enum TaskExit {
    Stopped(Option<Event>),
    SnapshotComplete,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    if Args::parse().self_test {
        return self_test();
    }

    let config = Arc::new(Config::from_env().await?);
    std::fs::create_dir_all(&config.spool_dir)?;
    let recovered = recover_parts(&config.segment_config())?;
    if !recovered.is_empty() {
        info!(segments = recovered.len(), "recovered interrupted segments");
    }
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let upload_task = tokio::spawn(upload_loop(config.clone(), shutdown_rx.clone()));
    let shutdown_signal = tokio::spawn(wait_for_signal(shutdown_tx.clone()));

    let mut backoff = 1_u64;
    while !*shutdown_rx.borrow() {
        match run_session(config.clone(), shutdown_rx.clone()).await {
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
    upload_task.await?;
    shutdown_signal.abort();
    Ok(())
}

async fn run_session(
    config: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let session_id = format!("{:x}-{}", now_ns()?, std::process::id());
    let (sender, mut receiver) = mpsc::channel(config.max_buffered_diffs);
    let (session_stop_tx, session_stop_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();
    for url in config.stream_urls() {
        tasks.spawn(receive_url(
            url,
            sender.clone(),
            session_stop_rx.clone(),
            config.stall_timeout,
        ));
    }
    tasks.spawn(produce_snapshots(
        config.clone(),
        sender.clone(),
        session_stop_rx.clone(),
    ));
    let mut states = config
        .symbols
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
            "symbols": config.symbols.len(),
            "websocket_shards": config.stream_urls().len(),
        }),
        now_ns()?,
    )?;
    let mut sequence_gaps = 0_u64;
    let mut last_health = Instant::now() - Duration::from_secs(60);
    let mut failure = None;

    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                changed?;
                break;
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok(Ok(TaskExit::Stopped(Some(event))))) => {
                        if let Some(symbol) = process_event(
                            &mut segment, &mut states, &mut budget, &session_id, event, &mut sequence_gaps
                        )? {
                            segment = rotate_segment(segment, &config, &states, &session_id, "sequence_gap")?;
                            tasks.spawn(produce_snapshot(config.clone(), symbol, sender.clone(), session_stop_rx.clone()));
                        }
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
                            &mut segment, &mut states, &mut budget, &session_id, event, &mut sequence_gaps
                        ) {
                            Ok(Some(symbol)) => {
                                segment = rotate_segment(segment, &config, &states, &session_id, "sequence_gap")?;
                                tasks.spawn(produce_snapshot(config.clone(), symbol, sender.clone(), session_stop_rx.clone()));
                            }
                            Ok(None) => {}
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
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if segment_due(&segment, config.segment_seconds)? {
                    segment = rotate_segment(segment, &config, &states, &session_id, "scheduled")?;
                }
            }
        }

        if last_health.elapsed() >= Duration::from_secs(30) {
            let status = if states.values().all(|state| state.synced && state.bridged) {
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
    let _ = close_segment(segment, &states, &session_id, "shutdown")?;
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

fn process_event(
    segment: &mut Segment,
    states: &mut HashMap<String, OrderBookState>,
    budget: &mut PendingBudget,
    session_id: &str,
    event: Event,
    sequence_gaps: &mut u64,
) -> anyhow::Result<Option<String>> {
    match event {
        Event::Diff {
            received_at_ns,
            frame,
        } => {
            segment.write(
                "diff",
                json!({"session_id":session_id,"frame":frame}),
                received_at_ns,
            )?;
            let diff = DepthDiff::from_frame(&frame)?;
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
                    return Ok(Some(symbol));
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
                    return Ok(Some(symbol));
                }
                return Err(error);
            }
        }
    }
    Ok(None)
}

fn segment_due(segment: &Segment, segment_seconds: u64) -> anyhow::Result<bool> {
    let now = now_ns()?;
    Ok(
        now.saturating_sub(segment.start_ns) >= segment_seconds * 1_000_000_000
            || segment_partition(segment.start_ns)? != segment_partition(now)?,
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
    }
}

fn rotate_segment(
    segment: Segment,
    config: &Config,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
) -> anyhow::Result<Segment> {
    close_segment(segment, states, session_id, reason)?;
    Segment::create(config.segment_config(), now_ns()?)
}

fn close_segment(
    mut segment: Segment,
    states: &HashMap<String, OrderBookState>,
    session_id: &str,
    reason: &str,
) -> anyhow::Result<Option<hft_collector::lob_archiver::SegmentArtifacts>> {
    if states.values().all(|state| state.synced && state.bridged) {
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
    for symbol in &config.symbols {
        if *shutdown.borrow() {
            return Ok(TaskExit::Stopped(None));
        }
        let started = now_ns()?;
        let path = match config.market {
            Market::Spot => "/api/v3/depth",
            Market::Usdm => "/fapi/v1/depth",
        };
        let limit = config.snapshot_limit.to_string();
        let snapshot = client
            .get(format!("{}{path}", config.rest_base))
            .query(&[("symbol", symbol.as_str()), ("limit", limit.as_str())])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
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
        tokio::time::sleep(interval).await;
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
    let path = match config.market {
        Market::Spot => "/api/v3/depth",
        Market::Usdm => "/fapi/v1/depth",
    };
    let limit = config.snapshot_limit.to_string();
    let snapshot = client
        .get(format!("{}{path}", config.rest_base))
        .query(&[("symbol", symbol.as_str()), ("limit", limit.as_str())])
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
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

async fn discover_symbols(market: Market, rest_base: &str) -> anyhow::Result<Vec<String>> {
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
    let mut symbols = payload["symbols"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["status"] == "TRADING")
        .filter(|item| market != Market::Usdm || item["contractType"] == "PERPETUAL")
        .filter_map(|item| item["symbol"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    symbols.sort();
    Ok(symbols)
}

async fn upload_loop(config: Arc<Config>, mut shutdown: watch::Receiver<bool>) {
    loop {
        let mut status = read_upload_status(&config.spool_dir);
        match upload_pending(&config).await {
            Ok(uploaded) => {
                if uploaded > 0 {
                    status.last_success_at = Some(chrono::Utc::now().to_rfc3339());
                }
                status.last_error_at = None;
                status.last_error = None;
            }
            Err(error) => {
                let now = chrono::Utc::now().to_rfc3339();
                status.last_error_at = Some(now);
                status.last_error = Some(error.to_string().chars().take(500).collect());
                warn!(error = %error, "pending OSS upload failed; files retained");
            }
        }
        if let Err(error) = write_upload_status(&config.spool_dir, &status) {
            error!(error = %error, "failed to persist OSS upload status");
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

async fn upload_pending(config: &Config) -> anyhow::Result<usize> {
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
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

fn upload_one(config: &Config, manifest: &Path) -> anyhow::Result<()> {
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
    std::fs::remove_file(data)?;
    std::fs::remove_file(manifest)?;
    std::fs::remove_file(success)?;
    Ok(())
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
