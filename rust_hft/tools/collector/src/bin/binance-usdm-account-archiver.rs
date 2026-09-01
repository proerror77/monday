use anyhow::{bail, Context, Result};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use hft_collector::binance_fee_artifact::valid_runtime_account_id;
use hft_collector::lob_archiver::{command_status_with_timeout, segment_partition, sha256_file};
use integration::signing::{BinanceCredentials, BinanceSigner};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info};

const DATA_SCHEMA: &str = "binance.usdm_account_events.v1";
const MANIFEST_SCHEMA: &str = "binance.usdm_account_manifest.v1";
const SUCCESS_SCHEMA: &str = "binance.usdm_account_success.v1";
const DATASET: &str = "usdm_account_events";
const REST_ORIGIN: &str = "https://fapi.binance.com";
const PRIVATE_STREAM_ORIGIN: &str = "wss://fstream.binance.com/ws";
const LISTEN_KEY_PATH: &str = "/fapi/v1/listenKey";
const ACCOUNT_PATH: &str = "/fapi/v3/account";
const LOCK_FILE: &str = ".binance-usdm-account-archiver.lock";
const ZSTD_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_MAX_SPOOL_BYTES: u64 = 50 * 1024 * 1024 * 1024;
const DEFAULT_MIN_FREE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(revision) => revision,
    None => "unbound-source-revision",
};

#[derive(Debug, Parser)]
#[command(
    name = "binance-usdm-account-archiver",
    version = BUILD_SOURCE_REVISION
)]
struct Args {
    #[arg(long, default_value = "/data/monday/spool/binance-usdm-account")]
    output_root: PathBuf,

    #[arg(long, default_value_t = 3_600)]
    segment_seconds: u64,

    #[arg(long, default_value_t = 60)]
    reconciliation_seconds: u64,

    #[arg(long, default_value_t = 1_800)]
    keepalive_seconds: u64,

    #[arg(long, default_value_t = 10)]
    request_timeout_seconds: u64,

    #[arg(long, default_value_t = 82_800)]
    max_session_seconds: u64,

    #[arg(long, default_value_t = DEFAULT_MAX_SPOOL_BYTES)]
    max_spool_bytes: u64,

    #[arg(long, default_value_t = DEFAULT_MIN_FREE_BYTES)]
    min_free_bytes: u64,

    #[arg(long, required_unless_present = "self_test")]
    account_secret_file: Option<PathBuf>,

    #[arg(long)]
    self_test: bool,
}

impl Args {
    fn validate(&self) -> Result<()> {
        if !self.output_root.is_absolute()
            || self
                .output_root
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            bail!("output root must be an absolute path without . or .. components");
        }
        if self.account_secret_file.as_ref().is_some_and(|path| {
            !path.is_absolute()
                || path
                    .components()
                    .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        }) {
            bail!("account secret file must be an absolute path without . or .. components");
        }
        if !(60..=86_400).contains(&self.segment_seconds)
            || !(10..=3_600).contains(&self.reconciliation_seconds)
            || !(60..=3_599).contains(&self.keepalive_seconds)
            || !(1..=60).contains(&self.request_timeout_seconds)
            || !(300..=86_400).contains(&self.max_session_seconds)
            || self.max_spool_bytes == 0
            || self.min_free_bytes == 0
        {
            bail!("collector arguments must stay within their safe bounds");
        }
        Ok(())
    }
}

#[derive(Clone)]
struct AccountSource {
    client: reqwest::Client,
    api_key: String,
    signer: BinanceSigner,
    runtime_account_id: String,
    account_fingerprint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BinanceAccountSecret {
    runtime_account_id: String,
    api_key: String,
    secret: String,
}

impl AccountSource {
    fn from_secret_file(path: &Path, timeout: Duration) -> Result<Self> {
        let secret = read_account_secret(path)?;
        let runtime_account_id = secret.runtime_account_id;
        let api_key = secret.api_key;
        let account_fingerprint = hex::encode(Sha256::digest(api_key.as_bytes()));
        let signer = BinanceSigner::new(BinanceCredentials::new(api_key.clone(), secret.secret));
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .context("build Binance account HTTP client")?,
            api_key,
            signer,
            runtime_account_id,
            account_fingerprint,
        })
    }

    async fn start_listen_key(&self) -> Result<String> {
        let response = self
            .client
            .post(format!("{REST_ORIGIN}{LISTEN_KEY_PATH}"))
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Binance listen-key request failed"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("Binance listen-key response failed"))?;
        if !status.is_success() {
            bail!("Binance listen-key request returned HTTP {status}");
        }
        parse_listen_key(&bytes)
    }

    async fn keepalive(&self, expected_listen_key: &str) -> Result<()> {
        let response = self
            .client
            .put(format!("{REST_ORIGIN}{LISTEN_KEY_PATH}"))
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Binance listen-key keepalive failed"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("Binance listen-key keepalive response failed"))?;
        if !status.is_success() {
            bail!("Binance listen-key keepalive returned HTTP {status}");
        }
        if parse_listen_key(&bytes)? != expected_listen_key {
            bail!("Binance listen-key changed during keepalive");
        }
        Ok(())
    }

    async fn reconcile(&self) -> Result<TimedAccountSnapshot> {
        let requested_at_ns = now_ns()?;
        let mut params = HashMap::from([("recvWindow".to_owned(), "5000".to_owned())]);
        let query = self.signer.sign_request(&mut params);
        let response = self
            .client
            .get(format!("{REST_ORIGIN}{ACCOUNT_PATH}?{query}"))
            .header("X-MBX-APIKEY", &self.api_key)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("Binance account reconciliation request failed"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .await
            .map_err(|_| anyhow::anyhow!("Binance account reconciliation response failed"))?;
        let received_at_ns = now_ns()?;
        if !status.is_success() {
            bail!("Binance account reconciliation returned HTTP {status}");
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("Binance account reconciliation returned invalid JSON"))?;
        if !value.is_object() {
            bail!("Binance account reconciliation returned a non-object payload");
        }
        Ok(TimedAccountSnapshot {
            requested_at_ns,
            received_at_ns,
            value,
        })
    }
}

fn read_account_secret(path: &Path) -> Result<BinanceAccountSecret> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| anyhow::anyhow!("Binance account credential is unavailable"))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("Binance account credential must be a direct private regular file");
    }
    let value = fs::read_to_string(path)
        .map_err(|_| anyhow::anyhow!("Binance account credential is unreadable"))?;
    let secret: BinanceAccountSecret = serde_json::from_str(&value)
        .map_err(|_| anyhow::anyhow!("Binance account credential has an invalid schema"))?;
    if !valid_runtime_account_id(&secret.runtime_account_id)
        || secret.api_key.trim().is_empty()
        || secret.secret.trim().is_empty()
    {
        bail!("Binance account credential has invalid fields");
    }
    Ok(secret)
}

struct TimedAccountSnapshot {
    requested_at_ns: u64,
    received_at_ns: u64,
    value: Value,
}

fn parse_listen_key(bytes: &[u8]) -> Result<String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("Binance listen-key response was invalid JSON"))?;
    let listen_key = value
        .get("listenKey")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Binance listen-key response omitted listenKey"))?;
    if listen_key.is_empty()
        || listen_key.len() > 256
        || !listen_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("Binance listen-key response contained an invalid listenKey");
    }
    Ok(listen_key.to_owned())
}

fn private_stream_url(listen_key: &str) -> Result<String> {
    parse_listen_key(serde_json::to_string(&json!({"listenKey":listen_key}))?.as_bytes())?;
    Ok(format!("{PRIVATE_STREAM_ORIGIN}/{listen_key}"))
}

#[derive(Debug, Default)]
struct UserEventClockValidator {
    clocks: HashMap<String, (u64, u64, Value)>,
}

impl UserEventClockValidator {
    fn observe(&mut self, event: &Value) -> Result<String> {
        let event_type = event
            .get("e")
            .and_then(Value::as_str)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
            .ok_or_else(|| anyhow::anyhow!("user event has an invalid event type"))?
            .to_owned();
        if !matches!(event_type.as_str(), "ORDER_TRADE_UPDATE" | "ACCOUNT_UPDATE") {
            return Ok(event_type);
        }
        let event_time = event
            .get("E")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("{event_type} omitted event time E"))?;
        let transaction_time = event
            .get("T")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("{event_type} omitted transaction time T"))?;
        validate_required_payload(&event_type, event)?;
        if let Some((previous_transaction_time, previous_event_time, previous_event)) =
            self.clocks.get(&event_type)
        {
            if event == previous_event {
                bail!("{event_type} duplicated the previous event");
            }
            if transaction_time < *previous_transaction_time || event_time < *previous_event_time {
                bail!(
                    "{event_type} out-of-order clock: previous_T={previous_transaction_time} previous_E={previous_event_time} observed_T={transaction_time} observed_E={event_time}"
                );
            }
        }
        self.clocks.insert(
            event_type.clone(),
            (transaction_time, event_time, event.clone()),
        );
        Ok(event_type)
    }
}

fn validate_required_payload(event_type: &str, event: &Value) -> Result<()> {
    match event_type {
        "ORDER_TRADE_UPDATE" => {
            let order = event
                .get("o")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("ORDER_TRADE_UPDATE omitted order payload"))?;
            for field in ["s", "c", "i", "x", "X", "q", "z", "l", "L", "t", "m"] {
                if !order.contains_key(field) {
                    bail!("ORDER_TRADE_UPDATE omitted required order field {field}");
                }
            }
        }
        "ACCOUNT_UPDATE" => {
            let account = event
                .get("a")
                .and_then(Value::as_object)
                .ok_or_else(|| anyhow::anyhow!("ACCOUNT_UPDATE omitted account payload"))?;
            if !account.get("m").is_some_and(Value::is_string)
                || !account.get("B").is_some_and(Value::is_array)
            {
                bail!("ACCOUNT_UPDATE omitted reason or balance changes");
            }
            if account.get("P").is_some_and(|value| !value.is_array()) {
                bail!("ACCOUNT_UPDATE positions must be an array when present");
            }
        }
        _ => unreachable!("caller filters target user event types"),
    }
    Ok(())
}

fn spool_usage_bytes(root: &Path) -> Result<u64> {
    let mut directories = vec![root.to_path_buf()];
    let mut bytes = 0_u64;
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                bail!("private account spool contains a symlink");
            }
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() {
                bytes = bytes
                    .checked_add(entry.metadata()?.len())
                    .context("private account spool size overflowed")?;
            } else {
                bail!("private account spool contains a special file");
            }
        }
    }
    Ok(bytes)
}

fn enforce_spool_bounds(root: &Path, max_spool_bytes: u64, min_free_bytes: u64) -> Result<u64> {
    let used = spool_usage_bytes(root)?;
    if used > max_spool_bytes || fs4::available_space(root)? < min_free_bytes {
        bail!("private account spool crossed its fail-closed storage bound");
    }
    Ok(used)
}

struct AccountSegment {
    start_ns: u64,
    end_ns: u64,
    root: PathBuf,
    path: PathBuf,
    writer: BufWriter<File>,
    counts: BTreeMap<String, u64>,
    zstd_timeout: Duration,
    runtime_account_id: String,
    account_fingerprint: String,
    base_spool_bytes: u64,
    max_spool_bytes: u64,
    min_free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AccountSegmentArtifacts {
    data: PathBuf,
    manifest: PathBuf,
    success: PathBuf,
    data_sha256: String,
    manifest_sha256: String,
}

impl AccountSegment {
    fn create(
        root: &Path,
        start_ns: u64,
        zstd_timeout: Duration,
        runtime_account_id: &str,
        account_fingerprint: &str,
        max_spool_bytes: u64,
        min_free_bytes: u64,
    ) -> Result<Self> {
        let base_spool_bytes = enforce_spool_bounds(root, max_spool_bytes, min_free_bytes)?;
        let (date, hour) = segment_partition(start_ns)?;
        let directory = root
            .join(format!("date={date}"))
            .join(format!("hour={hour}"));
        ensure_private_directory(&directory)?;
        let path = directory.join(format!("part-{start_ns}.jsonl.part"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .with_context(|| format!("create private account segment {}", path.display()))?;
        Ok(Self {
            start_ns,
            end_ns: start_ns,
            root: root.to_path_buf(),
            path,
            writer: BufWriter::with_capacity(64 * 1024, file),
            counts: BTreeMap::new(),
            zstd_timeout,
            runtime_account_id: runtime_account_id.to_owned(),
            account_fingerprint: account_fingerprint.to_owned(),
            base_spool_bytes,
            max_spool_bytes,
            min_free_bytes,
        })
    }

    fn write(&mut self, event_type: &str, payload: Value, received_at_ns: u64) -> Result<()> {
        if received_at_ns < self.start_ns {
            bail!("private account event predates its segment");
        }
        let row = json!({
            "schema": DATA_SCHEMA,
            "received_at_ns": received_at_ns,
            "type": event_type,
            "runtime_account_id": self.runtime_account_id,
            "account_fingerprint": self.account_fingerprint,
            "payload": payload,
        });
        let mut bytes = serde_json::to_vec(&row)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        let segment_bytes = self.writer.get_ref().metadata()?.len();
        if self.base_spool_bytes.saturating_add(segment_bytes) > self.max_spool_bytes
            || fs4::available_space(&self.root)? < self.min_free_bytes
        {
            bail!("private account spool crossed its fail-closed storage bound");
        }
        self.end_ns = self.end_ns.max(received_at_ns);
        *self.counts.entry(event_type.to_owned()).or_default() += 1;
        Ok(())
    }

    fn close(mut self) -> Result<Option<AccountSegmentArtifacts>> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        if self.path.metadata()?.len() == 0 {
            fs::remove_file(self.path)?;
            return Ok(None);
        }

        let raw_bytes = self.path.metadata()?.len();
        if fs4::available_space(&self.root)? < self.min_free_bytes.saturating_add(raw_bytes) {
            bail!("private account spool lacks room to seal its active segment");
        }

        let data = self
            .path
            .with_file_name(format!("part-{}.jsonl.zst", self.start_ns));
        let temporary_data = data.with_extension("zst.tmp");
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary_data)?;
        let mut command = Command::new("zstd");
        command
            .args(["-q", "-f", "-T1", "-3"])
            .arg(&self.path)
            .arg("-o")
            .arg(&temporary_data);
        let status = command_status_with_timeout(&mut command, self.zstd_timeout)?;
        if !status.success() {
            bail!("zstd failed with {status}");
        }
        File::open(&temporary_data)?.sync_all()?;
        fs::rename(&temporary_data, &data)?;
        sync_parent(&data)?;
        let data_sha256 = sha256_file(&data)?;
        let (date, hour) = segment_partition(self.start_ns)?;
        let manifest = data.with_file_name(format!(
            "{}.manifest.json",
            data.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        ));
        let events = self.counts.values().sum::<u64>();
        let capture_gap_events = self.counts.get("CAPTURE_GAP").copied().unwrap_or(0);
        let order_lifecycle_truth = if capture_gap_events == 0 {
            "session_observed_only"
        } else {
            "incomplete_gap_recorded"
        };
        atomic_write_json(
            &manifest,
            &json!({
                "schema": MANIFEST_SCHEMA,
                "data_schema": DATA_SCHEMA,
                "venue": "binance",
                "market": "usdm",
                "dataset": DATASET,
                "source_revision": BUILD_SOURCE_REVISION,
                "runtime_account_id": self.runtime_account_id,
                "account_fingerprint": self.account_fingerprint,
                "private_data": true,
                "credentials_in_artifact": false,
                "stream_types": ["ORDER_TRADE_UPDATE", "ACCOUNT_UPDATE"],
                "ordering_contract": "nondecreasing E and T per event type; exact consecutive duplicates fail closed within one websocket session",
                "gap_detection": "no exchange sequence id; CAPTURE_GAP is terminal for missed order lifecycle events",
                "rest_reconciliation_scope": "current balances and positions only; never recovers missed order events",
                "capture_gap_events": capture_gap_events,
                "order_lifecycle_truth": order_lifecycle_truth,
                "events": events,
                "event_types": self.counts,
                "start_received_at_ns": self.start_ns,
                "end_received_at_ns": self.end_ns,
                "date": date,
                "hour": hour,
                "file": data.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                "bytes": data.metadata()?.len(),
                "sha256": data_sha256,
            }),
        )?;
        let manifest_sha256 = sha256_file(&manifest)?;
        let success = data.with_file_name(format!(
            "{}._SUCCESS",
            data.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        ));
        atomic_write_json(
            &success,
            &json!({
                "schema": SUCCESS_SCHEMA,
                "data_file": data.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                "data_sha256": data_sha256,
                "manifest_file": manifest.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                "manifest_sha256": manifest_sha256,
            }),
        )?;
        fs::remove_file(&self.path)?;
        sync_parent(&data)?;
        enforce_spool_bounds(&self.root, self.max_spool_bytes, self.min_free_bytes)?;
        Ok(Some(AccountSegmentArtifacts {
            data,
            manifest,
            success,
            data_sha256,
            manifest_sha256,
        }))
    }

    fn should_rotate(&self, received_at_ns: u64, segment_seconds: u64) -> bool {
        received_at_ns.saturating_sub(self.start_ns)
            >= segment_seconds.saturating_mul(1_000_000_000)
    }
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "private account spool is not a real directory: {}",
            path.display()
        );
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        bail!("private account spool permissions must exclude group and other access");
    }
    Ok(())
}

fn incomplete_artifacts(root: &Path) -> Result<Vec<PathBuf>> {
    let mut directories = vec![root.to_path_buf()];
    let mut incomplete = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                bail!("private account spool contains a symlink");
            }
            if file_type.is_dir() {
                directories.push(entry.path());
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".jsonl.part") || name.ends_with(".tmp") {
                incomplete.push(entry.path());
            }
        }
    }
    incomplete.sort();
    Ok(incomplete)
}

fn verify_spool_ready(root: &Path, max_spool_bytes: u64, min_free_bytes: u64) -> Result<()> {
    enforce_spool_bounds(root, max_spool_bytes, min_free_bytes)?;
    let incomplete = incomplete_artifacts(root)?;
    if !incomplete.is_empty() {
        bail!(
            "private account spool contains {} incomplete artifacts",
            incomplete.len()
        );
    }
    Ok(())
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_file_name(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("account-artifact")
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().context("artifact has no parent directory")?)?.sync_all()?;
    Ok(())
}

struct SpoolLock {
    _file: File,
}

impl SpoolLock {
    fn acquire(root: &Path) -> Result<Self> {
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            bail!("private account spool lock is not a regular file");
        }
        if let Err(error) = fs4::FileExt::try_lock(&file) {
            let error = std::io::Error::from(error);
            if error.kind() == std::io::ErrorKind::WouldBlock {
                bail!("private account spool is already locked");
            }
            return Err(error.into());
        }
        Ok(Self { _file: file })
    }
}

async fn rotate_if_due(
    segment: &mut AccountSegment,
    args: &Args,
    session_id: &str,
    received_at_ns: u64,
) -> Result<()> {
    if !segment.should_rotate(received_at_ns, args.segment_seconds) {
        return Ok(());
    }
    let mut next = AccountSegment::create(
        &segment.root,
        received_at_ns,
        ZSTD_TIMEOUT,
        &segment.runtime_account_id,
        &segment.account_fingerprint,
        segment.max_spool_bytes,
        segment.min_free_bytes,
    )?;
    next.write(
        "SESSION_CONTINUED",
        json!({"session_id":session_id}),
        received_at_ns,
    )?;
    let previous = std::mem::replace(segment, next);
    previous.close()?;
    Ok(())
}

fn spawn_reconciliation(
    source: AccountSource,
    delay: Duration,
) -> JoinHandle<Result<TimedAccountSnapshot>> {
    tokio::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        source.reconcile().await
    })
}

fn spawn_keepalive(
    source: AccountSource,
    listen_key: String,
    delay: Duration,
) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        source.keepalive(&listen_key).await
    })
}

async fn run_session(
    source: &AccountSource,
    args: &Args,
    mut shutdown: watch::Receiver<bool>,
) -> Result<bool> {
    let listen_key = source.start_listen_key().await?;
    let url = private_stream_url(&listen_key)?;
    let (mut websocket, _) = connect_async(&url)
        .await
        .map_err(|_| anyhow::anyhow!("Binance private websocket connection failed"))?;
    let session_id = format!("{:x}-{}", now_ns()?, std::process::id());
    let start_ns = now_ns()?;
    let mut segment = AccountSegment::create(
        &args.output_root,
        start_ns,
        ZSTD_TIMEOUT,
        &source.runtime_account_id,
        &source.account_fingerprint,
        args.max_spool_bytes,
        args.min_free_bytes,
    )?;
    segment.write(
        "SESSION_START",
        json!({
            "session_id":session_id,
            "stream_types":["ORDER_TRADE_UPDATE","ACCOUNT_UPDATE"],
        }),
        start_ns,
    )?;
    let mut reconciliation = spawn_reconciliation(source.clone(), Duration::ZERO);
    let keepalive_delay = Duration::from_secs(args.keepalive_seconds);
    let mut keepalive = spawn_keepalive(source.clone(), listen_key.clone(), keepalive_delay);
    let result: Result<bool> = async {
        let mut clocks = UserEventClockValidator::default();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(args.max_session_seconds);

        loop {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break Ok(true);
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break Ok(false);
                }
                kept_alive = &mut keepalive => {
                    if kept_alive.is_err() || kept_alive.is_ok_and(|result| result.is_err()) {
                        break Err(anyhow::anyhow!("private websocket keepalive failed"));
                    }
                    keepalive = spawn_keepalive(source.clone(), listen_key.clone(), keepalive_delay);
                }
                reconciled = &mut reconciliation => {
                    let snapshot = reconciled
                        .map_err(|_| anyhow::anyhow!("private account reconciliation task failed"))?
                        .map_err(|_| anyhow::anyhow!("private account reconciliation failed"))?;
                    rotate_if_due(&mut segment, args, &session_id, snapshot.received_at_ns).await?;
                    segment.write(
                        "REST_ACCOUNT_RECONCILIATION",
                        json!({
                            "session_id":session_id,
                            "requested_at_ns":snapshot.requested_at_ns,
                            "account":snapshot.value,
                        }),
                        snapshot.received_at_ns,
                    )?;
                    reconciliation = spawn_reconciliation(
                        source.clone(),
                        Duration::from_secs(args.reconciliation_seconds),
                    );
                }
                message = websocket.next() => {
                    let received_at_ns = now_ns()?;
                    match message {
                        Some(Ok(Message::Text(text))) => {
                            let event: Value = match serde_json::from_str(&text) {
                                Ok(event) => event,
                                Err(_) => {
                                    segment.write(
                                        "MALFORMED_USER_EVENT",
                                        json!({"session_id":session_id}),
                                        received_at_ns,
                                    )?;
                                    break Err(anyhow::anyhow!("private websocket returned invalid JSON"));
                                }
                            };
                            let event_type = match clocks.observe(&event) {
                                Ok(event_type) => event_type,
                                Err(error) => {
                                    segment.write(
                                        "USER_EVENT_VALIDATION_FAILURE",
                                        json!({"session_id":session_id,"error":error.to_string(),"event":event}),
                                        received_at_ns,
                                    )?;
                                    break Err(anyhow::anyhow!("private user event validation failed"));
                                }
                            };
                            rotate_if_due(&mut segment, args, &session_id, received_at_ns).await?;
                            segment.write(
                                &event_type,
                                json!({"session_id":session_id,"event":event}),
                                received_at_ns,
                            )?;
                            if event_type == "listenKeyExpired" {
                                break Err(anyhow::anyhow!("private listen key expired"));
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            websocket
                                .send(Message::Pong(payload))
                                .await
                                .map_err(|_| anyhow::anyhow!("private websocket pong failed"))?;
                        }
                        Some(Ok(Message::Pong(_))) => {}
                        Some(Ok(Message::Close(_))) | None => {
                            break Err(anyhow::anyhow!("private websocket closed"));
                        }
                        Some(Ok(_)) => {
                            break Err(anyhow::anyhow!("private websocket returned a non-text event"));
                        }
                        Some(Err(_)) => {
                            break Err(anyhow::anyhow!("private websocket receive failed"));
                        }
                    }
                }
            }
        }
    }
    .await;

    reconciliation.abort();
    keepalive.abort();

    let end_ns = now_ns()?;
    let reason = match &result {
        Ok(true) => "shutdown".to_owned(),
        Ok(false) => "session_rollover".to_owned(),
        Err(error) => error.to_string(),
    };
    let gap_write = segment.write(
        "CAPTURE_GAP",
        json!({
            "session_id":session_id,
            "reason":reason,
            "terminal_for_order_lifecycle":true,
            "rest_reconciliation_recovers_order_history":false,
        }),
        end_ns,
    );
    let end_write = segment.write(
        "SESSION_END",
        json!({"session_id":session_id,"reason":reason}),
        end_ns,
    );
    let final_close = segment.close();
    gap_write?;
    end_write?;
    final_close?;
    result
}

async fn wait_for_signal(shutdown: watch::Sender<bool>) -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    let _ = shutdown.send(true);
    Ok(())
}

fn now_ns() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

fn self_test() -> Result<()> {
    let root = tempfile::Builder::new()
        .prefix("monday-usdm-account-self-test-")
        .tempdir()?;
    let output = fs::canonicalize(root.path())?;
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
    ensure_private_directory(&output)?;
    let start_ns = now_ns()?;
    let mut segment = AccountSegment::create(
        &output,
        start_ns,
        Duration::from_secs(30),
        "self-test-account",
        &"0".repeat(64),
        u64::MAX,
        0,
    )?;
    segment.write(
        "ORDER_TRADE_UPDATE",
        json!({"event":{"e":"ORDER_TRADE_UPDATE","E":1,"T":1,"o":{"s":"BTCUSDT","c":"self-test","i":1,"x":"NEW","X":"NEW","q":"1","z":"0","l":"0","L":"0","t":0,"m":false}}}),
        start_ns,
    )?;
    let artifacts = segment
        .close()?
        .context("self-test did not seal a segment")?;
    if sha256_file(&artifacts.data)? != artifacts.data_sha256
        || sha256_file(&artifacts.manifest)? != artifacts.manifest_sha256
    {
        bail!("self-test artifact hash mismatch");
    }
    println!("self-test: ok");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    args.validate()?;
    if args.self_test {
        return self_test();
    }
    ensure_private_directory(&args.output_root)?;
    let _lock = SpoolLock::acquire(&args.output_root)?;
    verify_spool_ready(&args.output_root, args.max_spool_bytes, args.min_free_bytes)?;
    let secret_file = args
        .account_secret_file
        .as_deref()
        .context("account secret file is required")?;
    let source = AccountSource::from_secret_file(
        secret_file,
        Duration::from_secs(args.request_timeout_seconds),
    )?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let signal_task = tokio::spawn(wait_for_signal(shutdown_tx));
    let mut backoff = 1_u64;
    while !*shutdown_rx.borrow() {
        match run_session(&source, &args, shutdown_rx.clone()).await {
            Ok(true) => break,
            Ok(false) => backoff = 1,
            Err(error) => {
                if let Err(spool_error) =
                    verify_spool_ready(&args.output_root, args.max_spool_bytes, args.min_free_bytes)
                {
                    signal_task.abort();
                    return Err(
                        spool_error.context("private account spool is unsafe after failure")
                    );
                }
                error!(error = %error, backoff, "private account session failed; reconnecting");
                let mut retry_shutdown = shutdown_rx.clone();
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                    changed = retry_shutdown.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
                backoff = (backoff * 2).min(30);
            }
        }
    }
    signal_task.abort();
    info!("Binance USD-M private account archiver stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order_event(event_time: u64, transaction_time: u64) -> Value {
        json!({
            "e":"ORDER_TRADE_UPDATE",
            "E":event_time,
            "T":transaction_time,
            "o":{
                "s":"BTCUSDT","c":"client-1","i":42,"x":"TRADE","X":"PARTIALLY_FILLED",
                "q":"1","z":"0.2","l":"0.2","L":"100","t":7,"m":true
            }
        })
    }

    fn account_event(event_time: u64, transaction_time: u64) -> Value {
        json!({
            "e":"ACCOUNT_UPDATE",
            "E":event_time,
            "T":transaction_time,
            "a":{"m":"ORDER","B":[],"P":[]}
        })
    }

    #[test]
    fn target_user_events_require_payloads_and_ordered_per_type_clocks() {
        let mut validator = UserEventClockValidator::default();
        assert_eq!(
            validator.observe(&order_event(10, 9)).unwrap(),
            "ORDER_TRADE_UPDATE"
        );
        assert_eq!(
            validator.observe(&account_event(8, 7)).unwrap(),
            "ACCOUNT_UPDATE"
        );
        assert!(validator.observe(&order_event(10, 9)).is_err());
        let mut same_clock_different_order = order_event(10, 9);
        same_clock_different_order["o"]["i"] = json!(43);
        assert_eq!(
            validator.observe(&same_clock_different_order).unwrap(),
            "ORDER_TRADE_UPDATE"
        );
        assert!(validator.observe(&account_event(9, 6)).is_err());

        let mut missing = order_event(11, 10);
        missing["o"].as_object_mut().unwrap().remove("i");
        assert!(validator.observe(&missing).is_err());
    }

    #[test]
    fn segment_seals_data_manifest_and_receipt_without_credentials() {
        let root = tempfile::Builder::new()
            .prefix("monday-usdm-account-segment-")
            .tempdir()
            .unwrap();
        let output = fs::canonicalize(root.path()).unwrap();
        let start_ns = 1_700_000_000_000_000_000;
        let mut segment = AccountSegment::create(
            &output,
            start_ns,
            Duration::from_secs(30),
            "research-account",
            &"a".repeat(64),
            u64::MAX,
            0,
        )
        .unwrap();
        segment
            .write(
                "ORDER_TRADE_UPDATE",
                json!({"event":order_event(1_700_000_000_000,1_700_000_000_000)}),
                start_ns,
            )
            .unwrap();
        segment
            .write(
                "REST_ACCOUNT_RECONCILIATION",
                json!({"account":{"assets":[],"positions":[]}}),
                start_ns + 1,
            )
            .unwrap();
        segment
            .write(
                "CAPTURE_GAP",
                json!({"terminal_for_order_lifecycle":true}),
                start_ns + 2,
            )
            .unwrap();
        let artifacts = segment.close().unwrap().unwrap();
        assert_eq!(sha256_file(&artifacts.data).unwrap(), artifacts.data_sha256);
        assert_eq!(
            sha256_file(&artifacts.manifest).unwrap(),
            artifacts.manifest_sha256
        );
        let decompressed = Command::new("zstd")
            .args(["-q", "-d", "-c"])
            .arg(&artifacts.data)
            .output()
            .unwrap();
        assert!(decompressed.status.success());
        let first_row: Value = serde_json::from_slice(
            decompressed
                .stdout
                .split(|byte| *byte == b'\n')
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(first_row["payload"]["event"]["o"]["c"], "client-1");
        assert_eq!(first_row["payload"]["event"]["o"]["i"], 42);
        assert_eq!(first_row["runtime_account_id"], "research-account");
        assert_eq!(first_row["account_fingerprint"], "a".repeat(64));
        let manifest: Value =
            serde_json::from_reader(File::open(&artifacts.manifest).unwrap()).unwrap();
        assert_eq!(manifest["dataset"], DATASET);
        assert_eq!(manifest["source_revision"], BUILD_SOURCE_REVISION);
        assert_eq!(manifest["credentials_in_artifact"], false);
        assert_eq!(manifest["runtime_account_id"], "research-account");
        assert_eq!(manifest["account_fingerprint"], "a".repeat(64));
        assert_eq!(manifest["capture_gap_events"], 1);
        assert_eq!(manifest["order_lifecycle_truth"], "incomplete_gap_recorded");
        assert_eq!(
            manifest["stream_types"],
            json!(["ORDER_TRADE_UPDATE", "ACCOUNT_UPDATE"])
        );
        assert_eq!(manifest["event_types"]["ORDER_TRADE_UPDATE"], 1);
        let success: Value =
            serde_json::from_reader(File::open(&artifacts.success).unwrap()).unwrap();
        assert_eq!(success["data_sha256"], artifacts.data_sha256);
        assert_eq!(success["manifest_sha256"], artifacts.manifest_sha256);
        let metadata = fs::metadata(&artifacts.data).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
    }

    #[test]
    fn listen_key_is_validated_before_building_a_private_url() {
        assert_eq!(
            private_stream_url("abc_123-XYZ").unwrap(),
            "wss://fstream.binance.com/ws/abc_123-XYZ"
        );
        assert!(private_stream_url("../secret").is_err());
        assert!(parse_listen_key(br#"{"listenKey":""}"#).is_err());
    }

    #[test]
    fn startup_detects_incomplete_private_segments_without_deleting_them() {
        let root = tempfile::Builder::new()
            .prefix("monday-usdm-account-incomplete-")
            .tempdir()
            .unwrap();
        let output = fs::canonicalize(root.path()).unwrap();
        let part = output.join("orphan.jsonl.part");
        fs::write(&part, b"preserve me\n").unwrap();
        assert_eq!(incomplete_artifacts(&output).unwrap(), vec![part.clone()]);
        assert!(verify_spool_ready(&output, u64::MAX, 0).is_err());
        assert!(part.is_file());
    }

    #[test]
    fn private_spool_bounds_fail_closed_without_deleting_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sealed-data");
        fs::write(&path, b"preserve me").unwrap();
        assert!(enforce_spool_bounds(root.path(), 1, 0).is_err());
        assert!(enforce_spool_bounds(root.path(), u64::MAX, u64::MAX).is_err());
        assert!(path.is_file());
    }

    #[test]
    fn account_secret_must_be_a_private_exact_schema_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("binance-account.json");
        fs::write(
            &path,
            br#"{"runtime_account_id":"research-account","api_key":"fixture-key","secret":"fixture-secret"}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let secret = read_account_secret(&path).unwrap();
        assert_eq!(secret.runtime_account_id, "research-account");

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_account_secret(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            &path,
            br#"{"runtime_account_id":"research-account","api_key":"fixture-key","secret":"fixture-secret","unexpected":true}"#,
        )
        .unwrap();
        assert!(read_account_secret(&path).is_err());
    }
}
