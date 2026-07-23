use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use data::binance_usdm_reference::{CompleteReferenceBatch, ReferenceClockValidator};
use hft_collector::binance_usdm_reference_artifact::{
    publish_reference_batch, verify_reference_artifact, ReferenceArtifactConfig,
};
use hft_collector::binance_usdm_reference_collector::{
    collect_complete_reference_batch, HttpReferenceSource, ReferenceSource,
    OFFICIAL_USDM_SOURCE_ORIGIN,
};
use rand::random;
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info};

const HEALTH_SCHEMA: &str = "binance.usdm_reference_health.v1";
const LOCK_NAME: &str = ".binance-usdm-reference-collector.lock";

#[derive(Debug, Parser)]
#[command(name = "binance-usdm-reference-collector")]
struct Args {
    #[arg(long, default_value = "/data/monday/spool/binance-usdm-reference")]
    output_root: PathBuf,
    #[arg(long, default_value_t = 30)]
    interval_seconds: u64,
    #[arg(long, default_value_t = 10)]
    request_timeout_seconds: u64,
    #[arg(long, default_value_t = 8)]
    oi_concurrency: usize,
    #[arg(long, default_value_t = 30_000)]
    max_staleness_ms: u64,
    #[arg(long)]
    once: bool,
}

impl Args {
    fn validate(&self) -> Result<()> {
        if !self.output_root.is_absolute()
            || self
                .output_root
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            bail!("output root must be an absolute canonical path");
        }
        if !(1..=3_600).contains(&self.interval_seconds)
            || !(1..=60).contains(&self.request_timeout_seconds)
            || !(1..=64).contains(&self.oi_concurrency)
            || !(1..=300_000).contains(&self.max_staleness_ms)
        {
            bail!("collector interval, timeout, concurrency, and staleness must be bounded");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ReferenceHealth {
    schema: String,
    status: String,
    source_origin: String,
    last_attempt_at_ns: u64,
    last_success_at_ns: Option<u64>,
    api_error_count: u64,
    total_api_errors: u64,
    artifact_error_count: u64,
    total_artifact_errors: u64,
    last_error: Option<String>,
    data_path: Option<PathBuf>,
    data_sha256: Option<String>,
    manifest_sha256: Option<String>,
}

impl ReferenceHealth {
    fn new() -> Self {
        Self {
            schema: HEALTH_SCHEMA.to_owned(),
            status: "starting".to_owned(),
            source_origin: OFFICIAL_USDM_SOURCE_ORIGIN.to_owned(),
            ..Self::default()
        }
    }

    fn fail(&mut self, observed_at_ns: u64, error: &anyhow::Error, api: bool) {
        self.status = "error".to_owned();
        self.last_attempt_at_ns = observed_at_ns;
        if api {
            self.api_error_count = self.api_error_count.saturating_add(1);
            self.total_api_errors = self.total_api_errors.saturating_add(1);
        } else {
            self.artifact_error_count = self.artifact_error_count.saturating_add(1);
            self.total_artifact_errors = self.total_artifact_errors.saturating_add(1);
        }
        self.last_error = Some(error.to_string().chars().take(1_024).collect());
    }
}

struct SpoolLock {
    _file: File,
}

impl SpoolLock {
    fn acquire(output_root: &Path) -> Result<Self> {
        ensure_canonical_directory(output_root)?;
        let path = output_root.join(LOCK_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| format!("open collector lock {}", path.display()))?;
        if !file.metadata()?.is_file() {
            bail!("collector lock is not a regular file");
        }
        fs4::FileExt::try_lock(&file).map_err(|error| {
            anyhow!("USD-M reference output is already controlled or cannot be locked: {error}")
        })?;
        Ok(Self { _file: file })
    }
}

#[derive(Default)]
struct RuntimeClocks {
    rows: ReferenceClockValidator,
    server_time: Option<(u64, u64)>,
}

impl RuntimeClocks {
    fn observe_server_time(&mut self, batch: &CompleteReferenceBatch) -> Result<()> {
        let first = batch
            .contracts()
            .first()
            .context("reference batch has no metadata source clock")?;
        let current = (first.source_time_ms, first.source_clock_received_at_ns);
        if self
            .server_time
            .is_some_and(|previous| current.0 < previous.0 || current.1 < previous.1)
        {
            bail!("USD-M server-time source clock regressed");
        }
        self.server_time = Some(current);
        Ok(())
    }
}

async fn run_attempt(
    source: &dyn ReferenceSource,
    args: &Args,
    clocks: &mut RuntimeClocks,
    health: &mut ReferenceHealth,
    now: &dyn Fn() -> Result<u64>,
) -> Result<()> {
    health.last_attempt_at_ns = now()?;
    let collected = collect_complete_reference_batch(source, args.oi_concurrency, &mut clocks.rows)
        .await
        .and_then(|batch| {
            clocks.observe_server_time(batch.batch())?;
            Ok(batch)
        });
    let collected = match collected {
        Ok(batch) => batch,
        Err(error) => {
            health.fail(now()?, &error, true);
            write_health(&args.output_root, health)?;
            return Err(error);
        }
    };
    let observed_at_ns = now()?;
    let artifact = publish_reference_batch(
        &ReferenceArtifactConfig {
            output_root: args.output_root.clone(),
            observed_at_ns,
            max_staleness_ms: args.max_staleness_ms,
        },
        collected.source_origin(),
        collected.batch(),
    )
    .and_then(|published| {
        verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )?;
        Ok(published)
    });
    let artifact = match artifact {
        Ok(artifact) => artifact,
        Err(error) => {
            health.fail(observed_at_ns, &error, false);
            write_health(&args.output_root, health)?;
            return Err(error);
        }
    };
    health.status = "healthy".to_owned();
    health.last_success_at_ns = Some(now()?);
    health.api_error_count = 0;
    health.artifact_error_count = 0;
    health.last_error = None;
    health.data_path = Some(artifact.data_path);
    health.data_sha256 = Some(artifact.data_sha256);
    health.manifest_sha256 = Some(artifact.manifest_sha256);
    write_health(&args.output_root, health)
}

fn write_health(output_root: &Path, health: &ReferenceHealth) -> Result<()> {
    ensure_canonical_directory(output_root)?;
    let final_path = output_root.join("health.json");
    let temp_path = output_root.join(format!(".health.json.{:016x}.tmp", random::<u64>()));
    let mut temp = TemporaryFile::create(temp_path)?;
    let mut bytes = serde_json::to_vec(health)?;
    bytes.push(b'\n');
    temp.file.write_all(&bytes)?;
    temp.file.sync_all()?;
    fs::rename(&temp.path, &final_path).context("atomically replace reference health")?;
    temp.published = true;
    File::open(output_root)?.sync_all()?;
    Ok(())
}

struct TemporaryFile {
    path: PathBuf,
    file: File,
    published: bool,
}

impl TemporaryFile {
    fn create(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)?;
        Ok(Self {
            path,
            file,
            published: false,
        })
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn ensure_canonical_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("directory path must be absolute and canonical");
    }
    let mut existing = path;
    while !existing.exists() {
        existing = existing
            .parent()
            .context("directory has no existing ancestor")?;
    }
    if fs::symlink_metadata(existing)?.file_type().is_symlink()
        || fs::canonicalize(existing)? != existing
    {
        bail!("directory ancestor is a symlink or indirect path");
    }
    let mut current = existing.to_path_buf();
    for component in path.strip_prefix(existing)?.components() {
        current.push(component.as_os_str());
        match DirBuilder::new().mode(0o700).create(&current) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("directory component is a symlink or non-directory");
        }
    }
    if fs::canonicalize(path)? != path {
        bail!("directory is not canonical");
    }
    Ok(())
}

fn now_ns() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    )?)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    args.validate()?;
    let _lock = SpoolLock::acquire(&args.output_root)?;
    let source = HttpReferenceSource::new(
        OFFICIAL_USDM_SOURCE_ORIGIN,
        Duration::from_secs(args.request_timeout_seconds),
    )?;
    let mut clocks = RuntimeClocks::default();
    let mut health = ReferenceHealth::new();
    loop {
        let result = run_attempt(&source, &args, &mut clocks, &mut health, &now_ns).await;
        match result {
            Ok(()) => info!("published complete Binance USD-M reference batch"),
            Err(ref error) => error!(%error, "Binance USD-M reference attempt failed closed"),
        }
        if args.once {
            return result;
        }
        tokio::time::sleep(Duration::from_secs(args.interval_seconds)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hft_collector::binance_usdm_reference_collector::TimedJson;
    use serde_json::{json, Value};
    use std::cell::Cell;
    use tempfile::tempdir;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    struct FakeSource {
        fail: bool,
        server_received_at_ns: u64,
    }

    #[async_trait]
    impl ReferenceSource for FakeSource {
        fn source_origin(&self) -> &str {
            OFFICIAL_USDM_SOURCE_ORIGIN
        }
        async fn server_time(&self) -> Result<TimedJson> {
            if self.fail {
                bail!("injected Binance API failure");
            }
            Ok(timed(
                json!({"serverTime": SOURCE_MS}),
                self.server_received_at_ns,
            ))
        }
        async fn exchange_info(&self) -> Result<TimedJson> {
            Ok(timed(
                json!({"symbols":[{
                    "symbol":"BTCUSDT","pair":"BTCUSDT","contractType":"PERPETUAL",
                    "deliveryDate":4133404800000_u64,"onboardDate":1598252400000_u64,
                    "status":"TRADING","baseAsset":"BTC","quoteAsset":"USDT",
                    "marginAsset":"USDT"
                }]}),
                RECEIVED_NS - 50,
            ))
        }
        async fn premium_index(&self) -> Result<TimedJson> {
            Ok(timed(
                json!([{
                    "symbol":"BTCUSDT","markPrice":"101","indexPrice":"100",
                    "lastFundingRate":"0.0001","interestRate":"0.0001",
                    "nextFundingTime":SOURCE_MS + 28_800_000,"time":SOURCE_MS
                }]),
                RECEIVED_NS,
            ))
        }
        async fn open_interest(&self, _symbol: &str) -> Result<TimedJson> {
            Ok(timed(
                json!({
                    "symbol":"BTCUSDT","openInterest":"12.3","time":SOURCE_MS
                }),
                RECEIVED_NS + 50,
            ))
        }
    }

    fn timed(value: Value, received_at_ns: u64) -> TimedJson {
        TimedJson {
            value,
            received_at_ns,
        }
    }

    fn args(root: PathBuf) -> Args {
        Args {
            output_root: root,
            interval_seconds: 30,
            request_timeout_seconds: 10,
            oi_concurrency: 4,
            max_staleness_ms: 1_000,
            once: true,
        }
    }

    #[test]
    fn cli_accepts_bounded_once_collection_and_rejects_zero_limits() {
        let parsed = Args::try_parse_from([
            "collector",
            "--output-root",
            "/tmp/reference",
            "--interval-seconds",
            "30",
            "--request-timeout-seconds",
            "10",
            "--oi-concurrency",
            "4",
            "--max-staleness-ms",
            "30000",
            "--once",
        ])
        .unwrap();
        parsed.validate().unwrap();
        assert!(parsed.once);
        let mut invalid = parsed;
        invalid.oi_concurrency = 0;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn output_root_has_only_one_controller_and_rejects_lock_symlinks() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let first = SpoolLock::acquire(&root).unwrap();
        assert!(SpoolLock::acquire(&root).is_err());
        drop(first);
        fs::remove_file(root.join(LOCK_NAME)).unwrap();
        std::os::unix::fs::symlink(root.join("victim"), root.join(LOCK_NAME)).unwrap();
        assert!(SpoolLock::acquire(&root).is_err());
    }

    #[tokio::test]
    async fn api_failure_writes_error_health_without_publishing_a_batch() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let args = args(root.clone());
        let mut health = ReferenceHealth::new();
        let error = run_attempt(
            &FakeSource {
                fail: true,
                server_received_at_ns: RECEIVED_NS - 100,
            },
            &args,
            &mut RuntimeClocks::default(),
            &mut health,
            &|| Ok(RECEIVED_NS + 100),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("injected Binance API failure"));
        let stored: ReferenceHealth =
            serde_json::from_slice(&fs::read(root.join("health.json")).unwrap()).unwrap();
        assert_eq!(stored.status, "error");
        assert_eq!(stored.api_error_count, 1);
        assert!(!root.join("lake").exists());
    }

    #[tokio::test]
    async fn success_publishes_verified_artifact_and_resets_consecutive_errors() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let args = args(root.clone());
        let mut health = ReferenceHealth::new();
        health.api_error_count = 2;
        let calls = Cell::new(0_u64);
        let clock = || {
            let call = calls.get();
            calls.set(call + 1);
            Ok(match call {
                0 => RECEIVED_NS - 200,
                1 => RECEIVED_NS + 100,
                _ => RECEIVED_NS + 200,
            })
        };
        run_attempt(
            &FakeSource {
                fail: false,
                server_received_at_ns: RECEIVED_NS - 100,
            },
            &args,
            &mut RuntimeClocks::default(),
            &mut health,
            &clock,
        )
        .await
        .unwrap();
        let stored: ReferenceHealth =
            serde_json::from_slice(&fs::read(root.join("health.json")).unwrap()).unwrap();
        assert_eq!(stored.status, "healthy");
        assert_eq!(stored.api_error_count, 0);
        assert_eq!(stored.last_success_at_ns, Some(RECEIVED_NS + 200));
        let data_path = stored.data_path.unwrap();
        assert!(data_path.is_file());
        assert!(data_path
            .parent()
            .unwrap()
            .ends_with(format!("batch={}", RECEIVED_NS + 100)));
        assert_eq!(stored.data_sha256.unwrap().len(), 64);
        assert_eq!(stored.manifest_sha256.unwrap().len(), 64);
    }

    #[tokio::test]
    async fn server_time_receipt_regression_fails_before_a_second_publish() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let args = args(root);
        let mut health = ReferenceHealth::new();
        let mut clocks = RuntimeClocks::default();
        run_attempt(
            &FakeSource {
                fail: false,
                server_received_at_ns: RECEIVED_NS - 100,
            },
            &args,
            &mut clocks,
            &mut health,
            &|| Ok(RECEIVED_NS + 100),
        )
        .await
        .unwrap();
        let first_data = health.data_path.clone().unwrap();
        let error = run_attempt(
            &FakeSource {
                fail: false,
                server_received_at_ns: RECEIVED_NS - 200,
            },
            &args,
            &mut clocks,
            &mut health,
            &|| Ok(RECEIVED_NS + 200),
        )
        .await
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("server-time source clock regressed"));
        assert_eq!(health.api_error_count, 1);
        assert!(!first_data
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!("batch={}", RECEIVED_NS + 200))
            .exists());
    }
}
