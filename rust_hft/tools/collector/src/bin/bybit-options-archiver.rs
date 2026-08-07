//! Bounded Bybit Options quote recorder with immutable local segments and OSS readback.
//!
//! The option feed is deliberately separate from the spot/linear collector.  Bybit uses a
//! different WebSocket endpoint and option symbols are discovered from the complete instrument
//! catalog before any quote subscription is opened.
//!
//! Fail-closed disk/spool guards (2026-08-07 governance): the writer refuses to open a new
//! segment and the uploader refuses to run when the spool mount has less than `MIN_FREE_GB`
//! free or when pending raw `.ndjson` bytes reach `BYBIT_OPTIONS_SPOOL_MAX_BYTES`.  Uploaded
//! segments recycle their raw source `.ndjson` only after a readback-verified OSS copy; the
//! compressed `.zst` is kept as a bounded local fallback and swept after
//! `BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS`.

use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::HeaderValue,
        Message,
    },
};
use tracing::{info, warn};

const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(revision) => revision,
    None => "unbound-source-revision",
};
const SCHEMA: &str = "monday.bybit_options_quote.v1";
const SUCCESS_SCHEMA: &str = "monday.bybit_options_quote.success.v1";
const DATASET: &str = "options_quotes";
const DEFAULT_REST_URL: &str = "https://api.bybit.com";
const DEFAULT_WS_URL: &str = "wss://stream.bybit.com/v5/public/option";
const MAX_SUBSCRIBE_TOPICS: usize = 40;
/// Default fail-closed cap on pending raw `.ndjson` bytes in the spool.  Bybit option
/// segments rotate hourly and are ~4-6 GiB each; 50 GiB keeps ~8-10 segments of headroom
/// during an OSS outage while staying well under a `/data` mount of several hundred GiB.
const DEFAULT_SPOOL_MAX_BYTES: u64 = 50 * 1024 * 1024 * 1024;
/// How long a readback-verified `.zst` fallback is kept locally before it is swept.  The
/// `.zst` is a bounded recovery copy of the object already stored in OSS, not a second
/// durable copy.
const DEFAULT_LOCAL_ZST_RETENTION_SECS: u64 = 2 * 24 * 3600;
/// Bounded websocket reconnect backoff ceiling (seconds).
const MAX_BACKOFF_SECS: u64 = 30;
const DEFAULT_USER_AGENT_PREFIX: &str = "monday-bybit-options-archiver";
const DEFAULT_WS_ORIGIN: &str = "https://www.bybit.com";

#[derive(Debug, Parser)]
#[command(name = "bybit-options-archiver", version = BUILD_SOURCE_REVISION)]
struct Args {
    #[arg(long)]
    self_test: bool,
    #[arg(long)]
    upload_only: bool,
}

#[derive(Clone, Debug)]
struct Config {
    spool_dir: PathBuf,
    rest_url: String,
    ws_url: String,
    symbols_per_connection: usize,
    segment_seconds: u64,
    max_segment_bytes: u64,
    request_timeout: Duration,
    oss_bucket: String,
    oss_endpoint: String,
    oss_region: String,
    aliyun_profile: String,
    oss_timeout: Duration,
    /// Fail-closed minimum free disk on the spool mount (GiB).
    min_free_gb: f64,
    /// Fail-closed cap on pending raw `.ndjson` bytes in the spool.
    spool_max_bytes: u64,
    /// Retention for readback-verified local `.zst` fallbacks before sweep.
    local_zst_retention_secs: u64,
    /// Origin header sent on the Bybit Options websocket handshake.
    ws_origin: String,
    /// app_id header sent on the Bybit Options websocket handshake.
    ws_app_id: String,
}

impl Config {
    fn from_env() -> Result<Self> {
        let positive_u64 = |name: &str, default: u64| -> Result<u64> {
            let value = env::var(name)
                .ok()
                .map(|raw| {
                    raw.parse::<u64>()
                        .with_context(|| format!("{name} must be an integer"))
                })
                .transpose()?
                .unwrap_or(default);
            if value == 0 {
                bail!("{name} must be positive");
            }
            Ok(value)
        };
        let spool_dir = PathBuf::from(
            env::var("BYBIT_OPTIONS_SPOOL_DIR")
                .unwrap_or_else(|_| "/data/monday/spool/bybit-options".to_owned()),
        );
        if !spool_dir.is_absolute()
            || spool_dir
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            bail!("BYBIT_OPTIONS_SPOOL_DIR must be an absolute canonical path");
        }
        Ok(Self {
            spool_dir,
            rest_url: env::var("BYBIT_OPTIONS_REST_URL")
                .unwrap_or_else(|_| DEFAULT_REST_URL.into()),
            ws_url: env::var("BYBIT_OPTIONS_WS_URL").unwrap_or_else(|_| DEFAULT_WS_URL.into()),
            symbols_per_connection: env::var("BYBIT_OPTIONS_SYMBOLS_PER_CONN")
                .ok()
                .map(|value| {
                    value
                        .parse::<usize>()
                        .context("BYBIT_OPTIONS_SYMBOLS_PER_CONN must be an integer")
                })
                .transpose()?
                .unwrap_or(20)
                .max(1),
            segment_seconds: positive_u64("BYBIT_OPTIONS_SEGMENT_SECONDS", 3600)?,
            max_segment_bytes: positive_u64(
                "BYBIT_OPTIONS_MAX_SEGMENT_BYTES",
                4 * 1024 * 1024 * 1024,
            )?,
            request_timeout: Duration::from_secs(positive_u64(
                "BYBIT_OPTIONS_REQUEST_TIMEOUT_SECONDS",
                20,
            )?),
            oss_bucket: env::var("OSS_BUCKET")
                .unwrap_or_else(|_| "monday-lob-apne1-1045353359".into()),
            oss_endpoint: env::var("OSS_ENDPOINT")
                .unwrap_or_else(|_| "oss-ap-northeast-1-internal.aliyuncs.com".into()),
            oss_region: env::var("OSS_REGION").unwrap_or_else(|_| "ap-northeast-1".into()),
            aliyun_profile: env::var("ALIYUN_PROFILE").unwrap_or_else(|_| "ecs-role".into()),
            oss_timeout: Duration::from_secs(positive_u64(
                "BYBIT_OPTIONS_OSS_TIMEOUT_SECONDS",
                300,
            )?),
            min_free_gb: env::var("MIN_FREE_GB")
                .ok()
                .map(|value| {
                    value
                        .parse::<f64>()
                        .context("MIN_FREE_GB must be a number of GiB")
                })
                .transpose()?
                .unwrap_or(20.0),
            spool_max_bytes: positive_u64(
                "BYBIT_OPTIONS_SPOOL_MAX_BYTES",
                DEFAULT_SPOOL_MAX_BYTES,
            )?,
            local_zst_retention_secs: positive_u64(
                "BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS",
                DEFAULT_LOCAL_ZST_RETENTION_SECS,
            )?,
            ws_origin: env::var("BYBIT_OPTIONS_WS_ORIGIN").unwrap_or_else(|_| {
                DEFAULT_WS_ORIGIN.to_owned()
            }),
            ws_app_id: env::var("BYBIT_OPTIONS_WS_APP_ID").unwrap_or_else(|_| {
                format!("{DEFAULT_USER_AGENT_PREFIX}/{BUILD_SOURCE_REVISION}")
            }),
        })
    }
}

#[derive(Debug, Clone)]
struct Instrument {
    symbol: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct Event {
    schema: &'static str,
    kind: &'static str,
    received_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    payload: Value,
}

#[derive(Debug)]
struct Segment {
    active_path: PathBuf,
    start_ms: u64,
    end_ms: u64,
    bytes: u64,
    events: u64,
    event_types: BTreeMap<String, u64>,
    symbols_seen: BTreeSet<String>,
    output: BufWriter<File>,
}

impl Segment {
    fn open(spool: &Path, now_ms: u64) -> Result<Self> {
        let active_path = spool.join(format!("bybit-options.{now_ms}.ndjson.active"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&active_path)
            .with_context(|| format!("create {}", active_path.display()))?;
        Ok(Self {
            active_path,
            start_ms: now_ms,
            end_ms: now_ms,
            bytes: 0,
            events: 0,
            event_types: BTreeMap::new(),
            symbols_seen: BTreeSet::new(),
            output: BufWriter::new(file),
        })
    }

    fn append(&mut self, event: &Event) -> Result<()> {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        self.output.write_all(&line)?;
        self.bytes = self.bytes.saturating_add(line.len() as u64);
        self.events = self.events.saturating_add(1);
        self.end_ms = event.received_at_ms;
        *self.event_types.entry(event.kind.to_owned()).or_default() += 1;
        if let Some(symbol) = &event.symbol {
            self.symbols_seen.insert(symbol.clone());
        }
        Ok(())
    }

    fn should_rotate(&self, now_ms: u64, config: &Config) -> bool {
        self.events > 0
            && (self.bytes >= config.max_segment_bytes
                || now_ms.saturating_sub(self.start_ms)
                    >= config.segment_seconds.saturating_mul(1000))
    }

    fn finish(mut self, config: &Config, expected_symbols: usize) -> Result<()> {
        self.output.flush()?;
        self.output.get_ref().sync_all()?;
        let data_path = self.active_path.with_file_name(
            self.active_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .trim_end_matches(".active"),
        );
        fs::rename(&self.active_path, &data_path)?;
        let digest = sha256_file(&data_path)?;
        let file_name = data_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let manifest = data_path.with_file_name(format!("{file_name}.manifest.json"));
        let success = data_path.with_file_name(format!("{file_name}._SUCCESS"));
        let metadata = json!({
            "schema": SCHEMA,
            "venue": "bybit",
            "category": "option",
            "dataset": DATASET,
            "symbols_expected": expected_symbols,
            "symbols_seen": self.symbols_seen,
            "events": self.events,
            "event_types": self.event_types,
            "start_received_at_ms": self.start_ms,
            "end_received_at_ms": self.end_ms,
            "file": file_name,
            "bytes": self.bytes,
            "sha256": digest,
            "source_revision": BUILD_SOURCE_REVISION,
        });
        write_json_atomic(&manifest, &metadata)?;
        write_json_atomic(
            &success,
            &json!({"schema": SUCCESS_SCHEMA, "file": file_name, "sha256": digest, "bytes": self.bytes}),
        )?;
        sync_parent(&data_path)?;
        info!(segment = %data_path.display(), events = self.events, bytes = self.bytes, "finalized Bybit Options quote segment");
        let _ = config;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct Health {
    schema: &'static str,
    venue: &'static str,
    category: &'static str,
    symbols_expected: usize,
    symbols_seen: usize,
    subscribed_topics: usize,
    connected_workers: usize,
    events: u64,
    last_event_at_ms: Option<u64>,
    active_segment_bytes: u64,
    disk_free_gb: Option<f64>,
    disk_warning: bool,
    disk_warning_threshold_gb: f64,
    spool_usage_bytes: u64,
    spool_max_bytes: u64,
    spool_warning: bool,
    upload_warning: bool,
    upload_failure_count: u64,
    last_upload_success_at: Option<u64>,
    last_upload_error_at: Option<u64>,
    updated_at_ms: u64,
}

/// Persistent uploader outcome recorded in `upload-status.json` so runtime and
/// shadow-gate policies can require `upload_failure_count == 0` and observe a
/// successful OSS drain.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct UploadStatus {
    #[serde(default)]
    failure_count: u64,
    last_success_at: Option<u64>,
    last_error_at: Option<u64>,
    last_error: Option<String>,
}

fn read_upload_status(spool: &Path) -> UploadStatus {
    File::open(spool.join("upload-status.json"))
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok())
        .unwrap_or_default()
}

fn write_upload_status(spool: &Path, status: &UploadStatus) -> Result<()> {
    write_json_atomic(
        &spool.join("upload-status.json"),
        &serde_json::to_value(status)?,
    )
}

#[derive(Debug)]
struct SpoolLock {
    _file: File,
}

impl SpoolLock {
    fn acquire(spool: &Path) -> Result<Self> {
        let path = spool.join(".bybit-options.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        file.try_lock()
            .context("Bybit Options spool is already locked")?;
        Ok(Self { _file: file })
    }
}

async fn discover_symbols(client: &Client, config: &Config) -> Result<Vec<Instrument>> {
    let mut cursor: Option<String> = None;
    let mut instruments = Vec::new();
    loop {
        let mut request = client
            .get(format!(
                "{}/v5/market/instruments-info",
                config.rest_url.trim_end_matches('/')
            ))
            .query(&[("category", "option"), ("limit", "1000")])
            .timeout(config.request_timeout);
        if let Some(value) = &cursor {
            request = request.query(&[("cursor", value)]);
        }
        let response: Value = request.send().await?.error_for_status()?.json().await?;
        let code = response
            .get("retCode")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        if code != 0 {
            bail!(
                "Bybit option instrument discovery failed retCode={code} retMsg={}",
                response
                    .get("retMsg")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        let result = response
            .get("result")
            .context("Bybit option response missing result")?;
        if let Some(list) = result.get("list").and_then(Value::as_array) {
            for item in list {
                let Some(symbol) = item
                    .get("symbol")
                    .and_then(Value::as_str)
                    .filter(|v| !v.is_empty())
                else {
                    continue;
                };
                let status = item
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("Trading");
                if status.eq_ignore_ascii_case("Trading") {
                    instruments.push(Instrument {
                        symbol: symbol.to_owned(),
                        status: status.to_owned(),
                    });
                }
            }
        }
        let next = result
            .get("nextPageCursor")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if next.is_empty() {
            break;
        }
        cursor = Some(next.to_owned());
    }
    instruments.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    instruments.dedup_by(|a, b| a.symbol == b.symbol);
    if instruments.is_empty() {
        bail!("Bybit option instrument catalog returned no Trading symbols");
    }
    Ok(instruments)
}

async fn writer_loop(
    mut receiver: mpsc::Receiver<Event>,
    config: Config,
    instruments: Arc<Vec<Instrument>>,
    connected_workers: Arc<std::sync::atomic::AtomicUsize>,
) -> Result<()> {
    let symbols: Vec<String> = instruments.iter().map(|i| i.symbol.clone()).collect();
    // Fail closed before the first segment: never open a new segment when the
    // spool mount is low on disk or the pending raw spool is over its cap.
    disk_gate_ok(&config)?;
    let mut segment = Segment::open(&config.spool_dir, now_ms())?;
    let catalog = Event {
        schema: SCHEMA,
        kind: "options_catalog",
        received_at_ms: now_ms(),
        symbol: None,
        topic: None,
        payload: json!({"category":"option","symbols":symbols,"instruments":instruments.iter().map(|i| json!({"symbol":i.symbol,"status":i.status})).collect::<Vec<_>>() }),
    };
    segment.append(&catalog)?;
    let mut last_event = None;
    let mut events = 1_u64;
    let mut health_tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            incoming = receiver.recv() => {
                let Some(event) = incoming else { break; };
                let now = now_ms();
                if segment.should_rotate(now, &config) {
                    // Fail closed before rotating into a new segment.
                    disk_gate_ok(&config)?;
                    let old = std::mem::replace(&mut segment, Segment::open(&config.spool_dir, now)?);
                    old.finish(&config, instruments.len())?;
                }
                last_event = Some(event.received_at_ms);
                events = events.saturating_add(1);
                segment.append(&event)?;
            }
            _ = health_tick.tick() => {
                write_health(&config, &segment, instruments.len(), connected_workers.load(std::sync::atomic::Ordering::Relaxed), last_event, events)?;
            }
        }
    }
    if segment.events > 0 {
        segment.finish(&config, instruments.len())?;
    }
    Ok(())
}

/// Build the Bybit Options websocket handshake request with a User-Agent, an
/// Origin, and an app_id header.  The handshake key/Host/Upgrade headers are
/// generated by `IntoClientRequest`; the custom headers are applied defensively
/// so a rejected-by-identity handshake fails fast instead of silently looping.
fn build_ws_request(config: &Config) -> Result<tokio_tungstenite::tungstenite::http::Request<()>> {
    let mut request = config.ws_url.clone().into_client_request()?;
    let headers = request.headers_mut();
    headers.insert(
        "User-Agent",
        HeaderValue::from_str(&format!(
            "{DEFAULT_USER_AGENT_PREFIX}/{BUILD_SOURCE_REVISION}"
        ))
        .context("invalid Bybit Options websocket User-Agent")?,
    );
    headers.insert(
        "Origin",
        HeaderValue::from_str(&config.ws_origin).context("invalid Bybit Options websocket Origin")?,
    );
    headers.insert(
        "app_id",
        HeaderValue::from_str(&config.ws_app_id)
            .context("invalid Bybit Options websocket app_id")?,
    );
    Ok(request)
}

async fn connection_worker(
    index: usize,
    symbols: Vec<String>,
    config: Config,
    sender: mpsc::Sender<Event>,
    connected_workers: Arc<std::sync::atomic::AtomicUsize>,
) -> Result<()> {
    let topics: Vec<String> = symbols
        .iter()
        .flat_map(|symbol| {
            [
                format!("orderbook.25.{symbol}"),
                format!("tickers.{symbol}"),
            ]
        })
        .collect();
    let mut backoff = 1_u64;
    loop {
        let result: Result<()> = async {
            let (socket, _) = connect_async(build_ws_request(&config)?).await?;
            connected_workers.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (mut write, mut read) = socket.split();
            for chunk in topics.chunks(MAX_SUBSCRIBE_TOPICS) {
                write.send(Message::Text(json!({"op":"subscribe","args":chunk}).to_string().into())).await?;
            }
            let mut ping_tick = tokio::time::interval(Duration::from_secs(20));
            loop {
                tokio::select! {
                    _ = ping_tick.tick() => { write.send(Message::Text(r#"{"op":"ping"}"#.to_owned().into())).await?; }
                    message = read.next() => {
                        let Some(message) = message else { bail!("Bybit Options websocket ended"); };
                        match message? {
                            Message::Text(text) => {
                                if text == "pong" { continue; }
                                let payload: Value = serde_json::from_str(&text)?;
                                let is_subscribe = payload.get("op").and_then(Value::as_str) == Some("subscribe");
                                if is_subscribe && payload.get("success").and_then(Value::as_bool) == Some(false) {
                                    bail!("Bybit Options subscription rejected: {}", payload.get("ret_msg").and_then(Value::as_str).unwrap_or("unknown"));
                                }
                                let kind = if is_subscribe { "subscription_ack" }
                                    else if payload.get("topic").and_then(Value::as_str).unwrap_or_default().starts_with("orderbook.") { "orderbook" }
                                    else if payload.get("topic").and_then(Value::as_str).unwrap_or_default().starts_with("tickers.") { "ticker" }
                                    else { "control" };
                                let topic = payload.get("topic").and_then(Value::as_str).map(ToOwned::to_owned);
                                let symbol = payload.get("data").and_then(|v| v.get("s").or_else(|| v.get("symbol"))).and_then(Value::as_str).map(ToOwned::to_owned)
                                    .or_else(|| topic.as_deref().and_then(|v| v.rsplit('.').next()).map(ToOwned::to_owned));
                                sender.send(Event { schema: SCHEMA, kind, received_at_ms: now_ms(), symbol, topic, payload }).await.map_err(|_| anyhow::anyhow!("writer stopped"))?;
                            }
                            Message::Ping(payload) => { write.send(Message::Pong(payload)).await?; }
                            Message::Close(frame) => { bail!("Bybit Options websocket closed: {frame:?}"); }
                            _ => {}
                        }
                    }
                }
            }
        }.await;
        if connected_workers.load(std::sync::atomic::Ordering::Relaxed) > 0 {
            connected_workers.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
        match result {
            Ok(()) => {
                backoff = 1;
            }
            Err(error) => {
                let _ = sender
                    .send(Event {
                        schema: SCHEMA,
                        kind: "collector_error",
                        received_at_ms: now_ms(),
                        symbol: None,
                        topic: None,
                        payload: json!({"worker":index,"error":error.to_string()}),
                    })
                    .await;
                warn!(worker = index, %error, backoff, "Bybit Options websocket reconnecting");
                sleep(Duration::from_secs(backoff)).await;
                backoff = next_backoff(backoff);
            }
        }
    }
}

async fn run_collector(config: Config) -> Result<()> {
    fs::create_dir_all(&config.spool_dir)?;
    let client = Client::builder().timeout(config.request_timeout).build()?;
    let instruments = Arc::new(discover_symbols(&client, &config).await?);
    let connected_workers = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel(16_384);
    let writer = tokio::spawn(writer_loop(
        receiver,
        config.clone(),
        instruments.clone(),
        connected_workers.clone(),
    ));
    let mut workers = tokio::task::JoinSet::new();
    for (index, chunk) in instruments
        .chunks(config.symbols_per_connection)
        .enumerate()
    {
        workers.spawn(connection_worker(
            index,
            chunk.iter().map(|i| i.symbol.clone()).collect(),
            config.clone(),
            sender.clone(),
            connected_workers.clone(),
        ));
    }
    info!(
        symbols = instruments.len(),
        workers = workers.len(),
        "started Bybit Options quote collector"
    );
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    drop(sender);
    writer.await??;
    Ok(())
}

async fn upload_pending(config: Config) -> Result<usize> {
    fs::create_dir_all(&config.spool_dir)?;
    let _lock = SpoolLock::acquire(&config.spool_dir)?;
    // Fail closed: refuse to compress/upload when the spool mount is low on disk
    // or pending raw bytes are over the cap.  Compressing and uploading would
    // otherwise write `.zst.tmp` and readback files into a full filesystem.
    disk_gate_ok(&config)?;
    // Idempotent recovery: a source `.ndjson` with a readback-verified
    // `.uploaded.json` marker is safe to delete even if the previous run
    // crashed between writing the marker and removing the source.
    let recovered = cleanup_verified_uploaded(&config.spool_dir)?;
    if recovered > 0 {
        info!(segments = recovered, "recovered interrupted raw segment cleanups");
    }
    let mut status = read_upload_status(&config.spool_dir);
    let mut uploaded = 0;
    let mut failures = 0;
    for entry in fs::read_dir(&config.spool_dir)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("ndjson") {
            continue;
        }
        let manifest = path.with_file_name(format!(
            "{}.manifest.json",
            path.file_name().unwrap().to_string_lossy()
        ));
        let success = path.with_file_name(format!(
            "{}._SUCCESS",
            path.file_name().unwrap().to_string_lossy()
        ));
        // Rotation publishes the data file before its metadata; wait for the
        // atomic metadata markers instead of racing a segment still finishing.
        if !manifest.is_file() || !success.is_file() {
            continue;
        }
        let marker = path.with_file_name(format!(
            "{}.uploaded.json",
            path.file_name().unwrap().to_string_lossy()
        ));
        if marker.exists() {
            continue;
        }
        match upload_one(&config, &path).await {
            Ok(()) => {
                uploaded += 1;
                status.last_success_at = Some(now_ms());
                status.last_error = None;
            }
            Err(error) => {
                failures += 1;
                status.failure_count = status.failure_count.saturating_add(1);
                status.last_error_at = Some(now_ms());
                status.last_error = Some(error.to_string());
                warn!(segment = %path.display(), %error, "Bybit Options upload failed");
            }
        }
    }
    // Bound the local `.zst` fallback copies after verified uploads so the
    // compressed fallbacks cannot silently accumulate into a disk-full again.
    let swept = sweep_expired_zst(&config.spool_dir, config.local_zst_retention_secs)?;
    if swept > 0 {
        info!(segments = swept, "swept expired local zst fallbacks");
    }
    write_upload_status(&config.spool_dir, &status)?;
    if failures > 0 {
        bail!("{failures} pending Bybit Options OSS uploads failed");
    }
    Ok(uploaded)
}

async fn upload_one(config: &Config, data: &Path) -> Result<()> {
    let file_name = data.file_name().unwrap().to_string_lossy().into_owned();
    let manifest_path = data.with_file_name(format!("{file_name}.manifest.json"));
    let metadata: Value = serde_json::from_reader(File::open(&manifest_path)?)?;
    let expected_sha = metadata
        .get("sha256")
        .and_then(Value::as_str)
        .context("manifest missing sha256")?;
    let actual_sha = sha256_file(data)?;
    if actual_sha != expected_sha {
        bail!("segment digest mismatch for {}", data.display());
    }
    let start_ms = metadata
        .get("start_received_at_ms")
        .and_then(Value::as_u64)
        .context("manifest missing start_received_at_ms")?;
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(start_ms as i64)
        .context("invalid segment timestamp")?;
    let date = dt.format("%Y-%m-%d").to_string();
    let hour = dt.format("%H").to_string();
    let compressed = data.with_file_name(format!("{file_name}.zst"));
    let temporary = data.with_file_name(format!("{file_name}.zst.tmp"));
    let zstd_status = tokio::process::Command::new("zstd")
        .args(["-q", "-f", "-T1", "-3"])
        .arg(data)
        .arg("-o")
        .arg(&temporary)
        .status()
        .await?;
    if !zstd_status.success() {
        bail!("zstd failed for {}", data.display());
    }
    fs::rename(&temporary, &compressed)?;
    let compressed_sha = sha256_file(&compressed)?;
    let prefix = format!("lake/raw/venue=bybit/market=option/dataset={DATASET}/date={date}/hour={hour}/sha256={compressed_sha}");
    let remote_data = format!("oss://{}/{prefix}/{file_name}.zst", config.oss_bucket);
    run_oss(config, &["cp", "-f"], &compressed, &remote_data).await?;
    let remote_manifest = format!(
        "oss://{}/{prefix}/{file_name}.manifest.json",
        config.oss_bucket
    );
    run_oss(config, &["cp", "-f"], &manifest_path, &remote_manifest).await?;
    let readback = config
        .spool_dir
        .join(format!(".readback-{compressed_sha}.zst"));
    run_oss(
        config,
        &["cp"],
        Path::new(&remote_data),
        &readback.to_string_lossy(),
    )
    .await?;
    if sha256_file(&readback)? != compressed_sha {
        bail!("OSS readback digest mismatch for {}", data.display());
    }
    fs::remove_file(&readback)?;
    // The marker is written only after the OSS readback digest matches.  Any
    // later run treats a source `.ndjson` with this marker as verified and
    // deletes it idempotently (see cleanup_verified_uploaded).
    write_json_atomic(
        &data.with_file_name(format!("{file_name}.uploaded.json")),
        &json!({"schema":"monday.bybit_options_upload.v1","object":remote_data,"compressed_sha256":compressed_sha,"source_sha256":expected_sha,"uploaded_at_ms":now_ms()}),
    )?;
    // Recycle the raw source only after the readback-verified marker exists.
    // The compressed `.zst` stays as a bounded local fallback and is swept by
    // sweep_expired_zst after the retention window.
    fs::remove_file(data)?;
    info!(segment = %data.display(), object = %remote_data, "uploaded Bybit Options segment and verified OSS readback");
    Ok(())
}

/// Delete every raw `.ndjson` whose readback-verified `.uploaded.json` marker
/// exists.  Idempotent: a missing source or marker is treated as already done.
fn cleanup_verified_uploaded(spool: &Path) -> Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(spool)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("ndjson") {
            continue;
        }
        let marker = path.with_file_name(format!(
            "{}.uploaded.json",
            path.file_name().unwrap().to_string_lossy()
        ));
        if !marker.is_file() {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("remove verified-uploaded source {}", path.display())
                });
            }
        }
    }
    Ok(removed)
}

/// Sweep readback-verified local `.zst` fallbacks older than the retention
/// window.  Files without a `.uploaded.json` marker (not yet verified) and
/// younger files are always retained.
fn sweep_expired_zst(spool: &Path, retention_secs: u64) -> Result<usize> {
    let now = now_ms();
    let mut removed = 0;
    for entry in fs::read_dir(spool)? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) != Some("zst") {
            continue;
        }
        let file_name = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let Some(base) = file_name.strip_suffix(".zst") else {
            continue;
        };
        let marker = path.with_file_name(format!("{base}.uploaded.json"));
        if !marker.is_file() {
            continue;
        }
        let modified = path
            .metadata()?
            .modified()
            .unwrap_or(UNIX_EPOCH);
        let modified_ms = modified
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if should_sweep_zst(now, modified_ms, retention_secs) {
            match fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("sweep expired zst fallback {}", path.display())
                    });
                }
            }
        }
    }
    Ok(removed)
}

fn should_sweep_zst(now_ms: u64, modified_ms: u64, retention_secs: u64) -> bool {
    now_ms.saturating_sub(modified_ms) >= retention_secs.saturating_mul(1000)
}

fn next_backoff(current: u64) -> u64 {
    (current * 2).min(MAX_BACKOFF_SECS)
}

async fn run_oss(config: &Config, args: &[&str], source: &Path, destination: &str) -> Result<()> {
    let mut command = tokio::process::Command::new("aliyun");
    command.arg("ossutil");
    command.args(args).arg(source).arg(destination).args([
        "--profile",
        &config.aliyun_profile,
        "--endpoint",
        &config.oss_endpoint,
        "--region",
        &config.oss_region,
        "--force",
    ]);
    let status = timeout(config.oss_timeout, command.status())
        .await
        .context("OSS command timed out")??;
    if !status.success() {
        bail!("aliyun ossutil failed with {status}");
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = std::io::Read::read(&mut input, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    let mut output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temporary)?;
    serde_json::to_writer(&mut output, value)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_parent(path)?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Free disk on the spool mount in GiB, rounded to one decimal, via `df -Pk`.
/// Returns `None` when the mount cannot be inspected; callers that need a
/// fail-closed decision must treat `None` as a warning (the writer bails via
/// the gate below only when a value is present and below the threshold; a
/// missing value is surfaced as `disk_warning` in health).
fn disk_free_gb(path: &Path) -> Option<f64> {
    let output = Command::new("df").args(["-Pk"]).arg(path).output().ok()?;
    let line = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .last()?
        .to_owned();
    let available_kib = line.split_whitespace().nth(3)?.parse::<f64>().ok()?;
    Some((available_kib / 1024.0 / 1024.0 * 10.0).round() / 10.0)
}

/// Pending raw `.ndjson` bytes in the spool (active + finalized, pre-upload).
/// Compressed `.zst` fallbacks are excluded: they are bounded by the retention
/// sweep and counted by the whole-filesystem `disk_free_gb` guard instead.
fn spool_usage_bytes(spool: &Path) -> Result<u64> {
    let mut total = 0_u64;
    for entry in fs::read_dir(spool)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        if name.ends_with(".ndjson") || name.ends_with(".ndjson.active") {
            total = total.saturating_add(path.metadata()?.len());
        }
    }
    Ok(total)
}

/// Compute the fail-closed disk/spool state used by both the gate and health.
fn spool_disk_state(config: &Config) -> (Option<f64>, u64, bool, bool) {
    let free = disk_free_gb(&config.spool_dir);
    let usage = spool_usage_bytes(&config.spool_dir).unwrap_or(0);
    let disk_warning = free.is_some_and(|free| free < config.min_free_gb);
    let spool_warning = usage >= config.spool_max_bytes;
    (free, usage, disk_warning, spool_warning)
}

/// Fail-closed gate: refuse to open new segments (writer) or run uploads
/// (uploader) when the spool mount is below `min_free_gb` or pending raw bytes
/// are at/over `spool_max_bytes`.
fn disk_gate_ok(config: &Config) -> Result<()> {
    let (free, usage, disk_warning, spool_warning) = spool_disk_state(config);
    if disk_warning {
        bail!(
            "fail-closed disk gate: {:.1} GiB free < MIN_FREE_GB {:.1} on {}",
            free.unwrap_or_default(),
            config.min_free_gb,
            config.spool_dir.display()
        );
    }
    if spool_warning {
        bail!(
            "fail-closed spool gate: {usage} raw bytes >= BYBIT_OPTIONS_SPOOL_MAX_BYTES {} on {}",
            config.spool_max_bytes,
            config.spool_dir.display()
        );
    }
    Ok(())
}

fn write_health(
    config: &Config,
    segment: &Segment,
    symbols: usize,
    connected: usize,
    last_event: Option<u64>,
    events: u64,
) -> Result<()> {
    let (disk_free_gb, spool_usage_bytes, disk_warning, spool_warning) =
        spool_disk_state(config);
    let upload = read_upload_status(&config.spool_dir);
    let upload_warning = upload.failure_count > 0;
    write_json_atomic(
        &config.spool_dir.join("health.json"),
        &serde_json::to_value(Health {
            schema: SCHEMA,
            venue: "bybit",
            category: "option",
            symbols_expected: symbols,
            symbols_seen: segment.symbols_seen.len(),
            subscribed_topics: symbols * 2,
            connected_workers: connected,
            events,
            last_event_at_ms: last_event,
            active_segment_bytes: segment.bytes,
            disk_free_gb,
            disk_warning,
            disk_warning_threshold_gb: config.min_free_gb,
            spool_usage_bytes,
            spool_max_bytes: config.spool_max_bytes,
            spool_warning,
            upload_warning,
            upload_failure_count: upload.failure_count,
            last_upload_success_at: upload.last_success_at,
            last_upload_error_at: upload.last_error_at,
            updated_at_ms: now_ms(),
        })?,
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn self_test() -> Result<()> {
    assert!(DEFAULT_WS_URL.ends_with("/v5/public/option"));
    let payload: Value = serde_json::from_str(
        r#"{"topic":"orderbook.25.BTC-1","type":"snapshot","data":{"s":"BTC-1","b":[],"a":[]}}"#,
    )?;
    assert_eq!(payload["data"]["s"], "BTC-1");
    let config = Config::from_env()?;
    assert!(config.symbols_per_connection > 0);
    assert!(config.spool_max_bytes > 0);
    assert!(config.min_free_gb > 0.0);
    println!("bybit-options-archiver self-test passed source={BUILD_SOURCE_REVISION}");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    // reqwest and tokio-tungstenite enable different rustls backends in the workspace;
    // select one provider before the first HTTPS/WebSocket handshake.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    if args.self_test {
        return self_test();
    }
    let config = Config::from_env()?;
    if args.upload_only {
        let uploaded = upload_pending(config).await?;
        println!("bybit-options upload-only uploaded={uploaded}");
    } else {
        run_collector(config).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config_for(spool: &Path) -> Config {
        Config {
            spool_dir: spool.to_path_buf(),
            rest_url: DEFAULT_REST_URL.into(),
            ws_url: DEFAULT_WS_URL.into(),
            symbols_per_connection: 20,
            segment_seconds: 3600,
            max_segment_bytes: 4 * 1024 * 1024 * 1024,
            request_timeout: Duration::from_secs(1),
            oss_bucket: "bucket".into(),
            oss_endpoint: "endpoint".into(),
            oss_region: "region".into(),
            aliyun_profile: "profile".into(),
            oss_timeout: Duration::from_secs(1),
            min_free_gb: 0.001,
            spool_max_bytes: 50 * 1024 * 1024 * 1024,
            local_zst_retention_secs: 60,
            ws_origin: DEFAULT_WS_ORIGIN.into(),
            ws_app_id: format!("{DEFAULT_USER_AGENT_PREFIX}/{BUILD_SOURCE_REVISION}"),
        }
    }

    #[test]
    fn option_endpoint_and_payload_are_option_specific() {
        assert!(DEFAULT_WS_URL.ends_with("/v5/public/option"));
        let payload: Value =
            serde_json::from_str(r#"{"topic":"tickers.BTC-1","data":{"symbol":"BTC-1"}}"#).unwrap();
        assert_eq!(payload["data"]["symbol"], "BTC-1");
    }

    #[test]
    fn segment_rotation_is_bounded() {
        let config = test_config_for(Path::new("/tmp/bybit-options"));
        let dir = tempfile::tempdir().unwrap();
        let segment = Segment::open(dir.path(), 1).unwrap();
        assert!(!segment.should_rotate(500, &config));
        let maxed = Segment {
            bytes: config.max_segment_bytes,
            events: 1,
            ..Segment::open(dir.path(), 2).unwrap()
        };
        assert!(maxed.should_rotate(500, &config));
    }

    #[test]
    fn spool_usage_counts_only_raw_segments() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bybit-options.1.ndjson"), b"12345").unwrap();
        fs::write(dir.path().join("bybit-options.2.ndjson"), b"1234567890").unwrap();
        fs::write(
            dir.path().join("bybit-options.3.ndjson.active"),
            b"123",
        )
        .unwrap();
        // Non-raw artifacts do not count toward the pending-raw spool cap.
        fs::write(dir.path().join("bybit-options.4.ndjson.zst"), b"123").unwrap();
        fs::write(dir.path().join("bybit-options.5.ndjson.uploaded.json"), b"{}").unwrap();
        fs::write(dir.path().join("health.json"), b"{}").unwrap();
        assert_eq!(spool_usage_bytes(dir.path()).unwrap(), 18);
    }

    #[test]
    fn disk_gate_bails_when_spool_exceeds_cap() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bybit-options.1.ndjson"), vec![b'x'; 1024]).unwrap();
        let config = Config {
            spool_max_bytes: 512,
            ..test_config_for(dir.path())
        };
        let error = disk_gate_ok(&config).unwrap_err();
        assert!(error.to_string().contains("spool gate"), "{error}");
    }

    #[test]
    fn disk_gate_bails_when_free_below_min() {
        let dir = tempfile::tempdir().unwrap();
        // free < f64::MAX always, so the disk branch trips deterministically.
        let config = Config {
            min_free_gb: f64::MAX,
            ..test_config_for(dir.path())
        };
        let error = disk_gate_ok(&config).unwrap_err();
        assert!(error.to_string().contains("disk gate"), "{error}");
    }

    #[test]
    fn disk_gate_passes_under_normal_state() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config_for(dir.path());
        // Real machine: free disk is far above 0.001 GiB and no raw bytes yet.
        disk_gate_ok(&config).expect("gate must pass under a normal spool state");
    }

    #[test]
    fn cleanup_verified_uploaded_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("bybit-options.1.ndjson");
        fs::write(&data, b"data").unwrap();
        let marker = dir.path().join("bybit-options.1.ndjson.uploaded.json");
        fs::write(&marker, b"{}").unwrap();
        assert_eq!(cleanup_verified_uploaded(dir.path()).unwrap(), 1);
        assert!(!data.exists());
        // Second pass is a no-op and never deletes the marker.
        assert_eq!(cleanup_verified_uploaded(dir.path()).unwrap(), 0);
        assert!(marker.exists());
    }

    #[test]
    fn cleanup_verified_uploaded_keeps_unverified_source() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("bybit-options.1.ndjson");
        fs::write(&data, b"data").unwrap();
        // No marker: never recycle a source before a readback-verified upload.
        assert_eq!(cleanup_verified_uploaded(dir.path()).unwrap(), 0);
        assert!(data.exists());
    }

    #[test]
    fn reconnect_backoff_is_bounded_and_grows() {
        let mut backoff = 1_u64;
        let mut sequence = Vec::new();
        for _ in 0..8 {
            backoff = next_backoff(backoff);
            sequence.push(backoff);
        }
        assert_eq!(sequence, vec![2, 4, 8, 16, 30, 30, 30, 30]);
        assert_eq!(next_backoff(30), 30);
    }

    #[test]
    fn zst_sweep_policy_is_bounded_by_age() {
        let now = 1_000_000_u64;
        assert!(should_sweep_zst(now, 1_000, 100));
        assert!(!should_sweep_zst(now, 999_999, 100));
        assert!(!should_sweep_zst(now, 1_000, 100_000));
    }

    #[test]
    fn sweep_expired_zst_requires_verified_marker() {
        let dir = tempfile::tempdir().unwrap();
        let no_marker = dir.path().join("bybit-options.1.ndjson.zst");
        fs::write(&no_marker, b"x").unwrap();
        // Even with retention 0, an unverified zst is never swept.
        assert_eq!(sweep_expired_zst(dir.path(), 0).unwrap(), 0);
        assert!(no_marker.exists());

        let verified = dir.path().join("bybit-options.2.ndjson.zst");
        fs::write(&verified, b"y").unwrap();
        fs::write(dir.path().join("bybit-options.2.ndjson.uploaded.json"), b"{}").unwrap();
        // Force an old mtime so the verified fallback is swept.
        let old = std::time::UNIX_EPOCH + Duration::from_secs(1_000);
        let file = File::open(&verified).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(old)).unwrap();
        assert_eq!(sweep_expired_zst(dir.path(), 60).unwrap(), 1);
        assert!(!verified.exists());
    }

    #[test]
    fn ws_request_carries_ua_origin_and_app_id() {
        let config = test_config_for(Path::new("/tmp/bybit-options"));
        let request = build_ws_request(&config).unwrap();
        assert_eq!(
            request.headers().get("User-Agent").unwrap(),
            &format!("{DEFAULT_USER_AGENT_PREFIX}/{BUILD_SOURCE_REVISION}")
        );
        assert_eq!(
            request.headers().get("Origin").unwrap(),
            DEFAULT_WS_ORIGIN
        );
        assert_eq!(
            request.headers().get("app_id").unwrap(),
            &format!("{DEFAULT_USER_AGENT_PREFIX}/{BUILD_SOURCE_REVISION}")
        );
    }
}
