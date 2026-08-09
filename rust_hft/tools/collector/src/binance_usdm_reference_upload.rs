//! Fail-closed OSS upload for verified Binance USD-M reference batches.

use crate::binance_usdm_reference_artifact::{
    verify_reference_artifact, PublishedReferenceArtifact,
};
use crate::lob_archiver::{command_status_with_timeout, sha256_file};
use crate::polymarket_upload::{
    atomic_json, ensure_canonical_directory, read_status, utc_now, ExclusiveTempDir,
};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

const DATA_NAME: &str = "reference.ndjson";
const MANIFEST_NAME: &str = "reference.ndjson.manifest.json";
const SUCCESS_NAME: &str = "reference.ndjson._SUCCESS";
const OSS_READBACK_ATTEMPTS: usize = 3;
const OSS_READBACK_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct ReferenceUploadConfig {
    pub output_root: PathBuf,
    pub bucket: String,
    pub endpoint: String,
    pub region: String,
    pub profile: String,
    pub oss_timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReferenceUploadSummary {
    pub uploaded_batches: usize,
    pub retried_batches: usize,
}

impl ReferenceUploadConfig {
    pub fn validate(&self) -> Result<()> {
        if self.bucket.trim().is_empty()
            || self.endpoint.trim().is_empty()
            || self.region.trim().is_empty()
            || self.profile.trim().is_empty()
            || self.oss_timeout.is_zero()
        {
            bail!("upload destination and timeout must be non-empty");
        }
        if !self.output_root.is_absolute() {
            bail!("output root must be an absolute canonical path");
        }
        Ok(())
    }
}

#[derive(Debug)]
struct ReferenceBatch {
    dir: PathBuf,
    object_prefix: String,
    published: PublishedReferenceArtifact,
}

fn strict_date(name: &str) -> bool {
    let Some(date) = name.strip_prefix("date=") else {
        return false;
    };
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
}

fn strict_hour(name: &str) -> bool {
    name.strip_prefix("hour=")
        .is_some_and(|hour| hour.len() == 2 && hour.parse::<u8>().is_ok_and(|hour| hour < 24))
}

fn strict_batch(name: &str) -> bool {
    name.strip_prefix("batch=")
        .and_then(|batch| batch.parse::<u64>().ok())
        .is_some()
}

fn direct_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect spool directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        bail!(
            "spool entry must be a direct directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn direct_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect reference artifact {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!(
            "reference artifact must be a direct regular file: {}",
            path.display()
        );
    }
    Ok(())
}

fn list_named_directories(parent: &Path, strict: fn(&str) -> bool) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("spool entry name is not UTF-8"))?;
        if name.starts_with(".reference-staging.") {
            continue;
        }
        if !strict(&name) {
            bail!("unexpected reference lake entry: {name}");
        }
        let path = entry.path();
        direct_directory(&path)?;
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

fn discover_batches(output_root: &Path) -> Result<Vec<ReferenceBatch>> {
    let lake = output_root.join("lake");
    if !lake.exists() {
        return Ok(Vec::new());
    }
    direct_directory(&lake)?;
    let mut batches = Vec::new();
    for raw in list_named_directories(&lake, |name| name == "raw")? {
        for venue in list_named_directories(&raw, |name| name == "venue=binance_usdm")? {
            for dataset in list_named_directories(&venue, |name| name == "dataset=reference")? {
                for date in list_named_directories(&dataset, strict_date)? {
                    for hour in list_named_directories(&date, strict_hour)? {
                        for batch in list_named_directories(&hour, strict_batch)? {
                            if let Some(batch) = reference_batch(output_root, &batch)? {
                                batches.push(batch);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(batches)
}

fn reference_batch(output_root: &Path, dir: &Path) -> Result<Option<ReferenceBatch>> {
    let data = dir.join(DATA_NAME);
    let manifest = dir.join(MANIFEST_NAME);
    let success = dir.join(SUCCESS_NAME);
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        names.push(
            entry?
                .file_name()
                .into_string()
                .map_err(|_| anyhow!("reference batch entry name is not UTF-8"))?,
        );
    }
    names.sort();
    if names.is_empty() {
        fs::remove_dir(dir)
            .with_context(|| format!("remove empty reference batch {}", dir.display()))?;
        if let Some(parent) = dir.parent() {
            File::open(parent)?.sync_all()?;
        }
        return Ok(None);
    }
    if names != [DATA_NAME, SUCCESS_NAME, MANIFEST_NAME] {
        bail!(
            "reference batch must contain exactly the data/manifest/_SUCCESS triplet: {}",
            dir.display()
        );
    }
    for path in [&data, &manifest, &success] {
        direct_regular_file(path)?;
    }
    let relative = dir
        .strip_prefix(output_root)
        .context("reference batch escapes the output root")?;
    let object_prefix = relative
        .to_str()
        .ok_or_else(|| anyhow!("reference batch partition is not UTF-8"))?
        .to_owned();
    let expected = utc_partition_components(&object_prefix)?;
    if expected != object_prefix {
        bail!("reference batch partition identity is invalid: {object_prefix}");
    }
    let manifest_bytes = fs::read(&manifest).context("read reference manifest")?;
    let manifest_sha256 = sha256_file(&manifest)?;
    let data_sha256 = serde_json::from_slice::<Value>(&manifest_bytes)?
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| anyhow!("reference manifest has an invalid data sha256"))?
        .to_owned();
    let published = PublishedReferenceArtifact {
        data_path: data,
        manifest_path: manifest,
        success_path: success,
        data_sha256: data_sha256.clone(),
        manifest_sha256: manifest_sha256.clone(),
    };
    verify_reference_artifact(&published, &data_sha256, &manifest_sha256)
        .context("local reference artifact failed canonical readback")?;
    Ok(Some(ReferenceBatch {
        dir: dir.to_path_buf(),
        object_prefix,
        published,
    }))
}

fn utc_partition_components(object_prefix: &str) -> Result<String> {
    let components: Vec<&str> = object_prefix.split('/').collect();
    if components.len() != 7
        || components[..3] != ["lake", "raw", "venue=binance_usdm"]
        || components[3] != "dataset=reference"
        || !strict_date(components[4])
        || !strict_hour(components[5])
        || !strict_batch(components[6])
    {
        bail!("reference object prefix is not a canonical partition: {object_prefix}");
    }
    Ok(components.join("/"))
}

fn oss_copy_command(source: &str, destination: &str, config: &ReferenceUploadConfig) -> Command {
    let mut command = Command::new("aliyun");
    command.args([
        "ossutil",
        "cp",
        source,
        destination,
        "--profile",
        &config.profile,
        "--endpoint",
        &config.endpoint,
        "--region",
        &config.region,
    ]);
    command
}

fn oss_upload_command(source: &str, destination: &str, config: &ReferenceUploadConfig) -> Command {
    let mut command = oss_copy_command(source, destination, config);
    command.arg("--ignore-existing");
    command
}

fn run_checked(command: &mut Command, timeout: Duration) -> Result<ExitStatus> {
    command.stdout(Stdio::null());
    let status = command_status_with_timeout(command, timeout)?;
    if !status.success() {
        bail!("child process exited with {status}");
    }
    Ok(status)
}

fn artifact_members(batch: &ReferenceBatch) -> [(&Path, &str); 3] {
    [
        (&batch.published.data_path, DATA_NAME),
        (&batch.published.manifest_path, MANIFEST_NAME),
        (&batch.published.success_path, SUCCESS_NAME),
    ]
}

fn download_remote_triplet_with<F>(
    batch: &ReferenceBatch,
    config: &ReferenceUploadConfig,
    runner: &mut F,
) -> Result<(ExclusiveTempDir, PublishedReferenceArtifact)>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let verify_dir = ExclusiveTempDir::create(&config.output_root, ".oss-verify")?;
    let partition = verify_dir.path().join(&batch.object_prefix);
    ensure_canonical_directory(&partition)?;
    let mut paths = Vec::new();
    for (_local, name) in artifact_members(batch) {
        let destination = partition.join(name);
        let remote = format!(
            "oss://{}/{}/{name}",
            config.bucket, batch.object_prefix
        );
        let mut command = oss_copy_command(
            &remote,
            destination
                .to_str()
                .ok_or_else(|| anyhow!("verification path is not UTF-8"))?,
            config,
        );
        runner(&mut command, config.oss_timeout)?;
        direct_regular_file(&destination)?;
        paths.push(destination);
    }
    let downloaded = PublishedReferenceArtifact {
        data_path: paths[0].clone(),
        manifest_path: paths[1].clone(),
        success_path: paths[2].clone(),
        data_sha256: batch.published.data_sha256.clone(),
        manifest_sha256: batch.published.manifest_sha256.clone(),
    };
    Ok((verify_dir, downloaded))
}

fn verify_remote_triplet(
    batch: &ReferenceBatch,
    downloaded: &PublishedReferenceArtifact,
) -> Result<()> {
    verify_reference_artifact(
        downloaded,
        &batch.published.data_sha256,
        &batch.published.manifest_sha256,
    )
    .context("remote reference artifact failed canonical readback")?;
    Ok(())
}

fn remote_triplet_matches_with<F>(
    batch: &ReferenceBatch,
    config: &ReferenceUploadConfig,
    runner: &mut F,
) -> Result<bool>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let Ok((_verify_dir, downloaded)) = download_remote_triplet_with(batch, config, runner)
    else {
        return Ok(false);
    };
    verify_remote_triplet(batch, &downloaded)?;
    Ok(true)
}

fn verify_remote_triplet_with_retries<F>(
    batch: &ReferenceBatch,
    config: &ReferenceUploadConfig,
    runner: &mut F,
) -> Result<()>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let mut last_error = None;
    for attempt in 0..OSS_READBACK_ATTEMPTS {
        match download_remote_triplet_with(batch, config, runner) {
            Ok((_verify_dir, downloaded)) => {
                return verify_remote_triplet(batch, &downloaded);
            }
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < OSS_READBACK_ATTEMPTS {
            std::thread::sleep(OSS_READBACK_RETRY_DELAY);
        }
    }
    Err(last_error
        .expect("readback retry loop must record an error")
        .context("remote reference artifacts remained unreadable after bounded retries"))
}

fn remove_local_batch(batch: &ReferenceBatch) -> Result<()> {
    for (path, _name) in artifact_members(batch) {
        direct_regular_file(path)?;
        fs::remove_file(path)
            .with_context(|| format!("remove uploaded reference artifact {}", path.display()))?;
    }
    fs::remove_dir(&batch.dir)
        .with_context(|| format!("remove uploaded reference batch {}", batch.dir.display()))?;
    if let Some(parent) = batch.dir.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Upload one verified batch; returns true when the remote triplet already matched.
fn upload_batch_with<F>(
    batch: &ReferenceBatch,
    config: &ReferenceUploadConfig,
    runner: &mut F,
) -> Result<bool>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    if remote_triplet_matches_with(batch, config, runner)? {
        remove_local_batch(batch)?;
        return Ok(true);
    }
    for (local, name) in artifact_members(batch) {
        let destination = format!(
            "oss://{}/{}/{name}",
            config.bucket, batch.object_prefix
        );
        let mut command = oss_upload_command(
            local
                .to_str()
                .ok_or_else(|| anyhow!("reference artifact path is not UTF-8"))?,
            &destination,
            config,
        );
        runner(&mut command, config.oss_timeout)?;
    }
    verify_remote_triplet_with_retries(batch, config, runner)?;
    remove_local_batch(batch)?;
    Ok(false)
}

/// Upload all verified reference batches, continuing past bad batches while returning failure.
pub fn upload_pending(config: &ReferenceUploadConfig) -> Result<ReferenceUploadSummary> {
    upload_pending_with(config, &mut run_checked)
}

pub(crate) struct OssTripletBatch {
    pub dir: PathBuf,
    pub object_prefix: String,
    pub members: [PathBuf; 3],
}

pub(crate) fn upload_verified_triplet(
    batch: &OssTripletBatch,
    config: &ReferenceUploadConfig,
    verifier: impl FnMut(&[PathBuf; 3]) -> Result<()>,
) -> Result<bool> {
    upload_verified_triplet_with(batch, config, &mut run_checked, verifier)
}

fn download_oss_triplet_with<F>(
    batch: &OssTripletBatch,
    config: &ReferenceUploadConfig,
    runner: &mut F,
) -> Result<(ExclusiveTempDir, [PathBuf; 3])>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    let verify_dir = ExclusiveTempDir::create(&config.output_root, ".oss-verify")?;
    let destination_dir = verify_dir.path().join(&batch.object_prefix);
    ensure_canonical_directory(&destination_dir)?;
    let mut downloaded = Vec::with_capacity(3);
    for local in &batch.members {
        direct_regular_file(local)?;
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .context("artifact filename is not UTF-8")?;
        let destination = destination_dir.join(name);
        let remote = format!("oss://{}/{}/{name}", config.bucket, batch.object_prefix);
        let mut command = oss_copy_command(
            &remote,
            destination
                .to_str()
                .context("verification path is not UTF-8")?,
            config,
        );
        runner(&mut command, config.oss_timeout)?;
        direct_regular_file(&destination)?;
        downloaded.push(destination);
    }
    Ok((
        verify_dir,
        downloaded
            .try_into()
            .map_err(|_| anyhow!("artifact triplet is incomplete"))?,
    ))
}

pub(crate) fn upload_verified_triplet_with<F, V>(
    batch: &OssTripletBatch,
    config: &ReferenceUploadConfig,
    runner: &mut F,
    mut verifier: V,
) -> Result<bool>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
    V: FnMut(&[PathBuf; 3]) -> Result<()>,
{
    config.validate()?;
    ensure_canonical_directory(&config.output_root)?;
    for member in &batch.members {
        direct_regular_file(member)?;
    }
    let already_present = match download_oss_triplet_with(batch, config, runner) {
        Ok((_verify_dir, downloaded)) => {
            verifier(&downloaded)?;
            true
        }
        Err(_) => false,
    };
    if !already_present {
        for local in &batch.members {
            direct_regular_file(local)?;
            let name = local
                .file_name()
                .and_then(|name| name.to_str())
                .context("artifact filename is not UTF-8")?;
            let destination = format!("oss://{}/{}/{name}", config.bucket, batch.object_prefix);
            let mut command = oss_upload_command(
                local.to_str().context("artifact path is not UTF-8")?,
                &destination,
                config,
            );
            runner(&mut command, config.oss_timeout)?;
        }
        let mut last_error = None;
        for attempt in 0..OSS_READBACK_ATTEMPTS {
            match download_oss_triplet_with(batch, config, runner) {
                Ok((_verify_dir, downloaded)) => {
                    verifier(&downloaded)?;
                    last_error = None;
                    break;
                }
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < OSS_READBACK_ATTEMPTS {
                std::thread::sleep(OSS_READBACK_RETRY_DELAY);
            }
        }
        if let Some(error) = last_error {
            return Err(error.context("remote artifacts remained unreadable after bounded retries"));
        }
    }
    for member in &batch.members {
        direct_regular_file(member)?;
        fs::remove_file(member)
            .with_context(|| format!("remove uploaded artifact {}", member.display()))?;
    }
    fs::remove_dir(&batch.dir)
        .with_context(|| format!("remove uploaded artifact directory {}", batch.dir.display()))?;
    if let Some(parent) = batch.dir.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(already_present)
}

fn upload_pending_with<F>(
    config: &ReferenceUploadConfig,
    runner: &mut F,
) -> Result<ReferenceUploadSummary>
where
    F: FnMut(&mut Command, Duration) -> Result<ExitStatus>,
{
    config.validate()?;
    ensure_canonical_directory(&config.output_root)?;
    let status_path = config.output_root.join("upload-status.json");
    let mut status = read_status(&status_path)?;
    let mut failures = Vec::new();
    let mut uploaded_batches = 0_usize;
    let mut retried_batches = 0_usize;
    for batch in discover_batches(&config.output_root)? {
        let object = format!(
            "oss://{}/{}/{DATA_NAME}",
            config.bucket, batch.object_prefix
        );
        match upload_batch_with(&batch, config, runner) {
            Ok(retried) => {
                if retried {
                    retried_batches += 1;
                } else {
                    uploaded_batches += 1;
                }
                status.insert("last_success_at".to_owned(), json!(utc_now()));
                status.insert("last_uploaded_object".to_owned(), json!(object));
            }
            Err(error) => {
                eprintln!(
                    "Binance USD-M reference upload failed for {}: {error}",
                    batch.dir.display()
                );
                failures.push(json!({
                    "batch": batch.dir.file_name().and_then(|name| name.to_str()),
                    "error": error.to_string(),
                }));
            }
        }
    }
    status.insert("updated_at".to_owned(), json!(utc_now()));
    status.insert("uploaded_batches".to_owned(), json!(uploaded_batches));
    status.insert("retried_batches".to_owned(), json!(retried_batches));
    status.insert(
        "pending_batches".to_owned(),
        json!(discover_batches(&config.output_root)?.len()),
    );
    status.insert("failed_batches".to_owned(), Value::Array(failures.clone()));
    status.insert(
        "last_error_at".to_owned(),
        if failures.is_empty() {
            Value::Null
        } else {
            json!(utc_now())
        },
    );
    status.insert(
        "last_error".to_owned(),
        failures
            .last()
            .and_then(|failure| failure.get("error"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    atomic_json(&status_path, &Value::Object(status))?;
    if failures.is_empty() {
        Ok(ReferenceUploadSummary {
            uploaded_batches,
            retried_batches,
        })
    } else {
        bail!(
            "{} Binance USD-M reference batch(es) failed",
            failures.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_usdm_reference_artifact::{
        publish_reference_batch, ReferenceArtifactConfig,
    };
    use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
    use data::binance_usdm_reference::{
        ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
        OpenInterestObservation, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
    };
    use rust_decimal::Decimal;
    use std::os::unix::fs::symlink;
    use std::os::unix::process::ExitStatusExt;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    struct TestDir {
        _temp: tempfile::TempDir,
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let temp = tempfile::Builder::new()
                .prefix("monday-usdm-reference-upload-test-")
                .tempdir()
                .unwrap();
            let path = fs::canonicalize(temp.path()).unwrap();
            Self { _temp: temp, path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn success_status() -> ExitStatus {
        ExitStatus::from_raw(0)
    }

    fn failure_status() -> ExitStatus {
        ExitStatus::from_raw(1)
    }

    #[derive(Default)]
    struct FakeOss {
        uploads: usize,
        corrupt_uploads: bool,
    }

    impl FakeOss {
        fn run(
            &mut self,
            bucket_root: &Path,
            command: &mut Command,
            _timeout: Duration,
        ) -> Result<ExitStatus> {
            assert_eq!(command.get_program(), "aliyun");
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert_eq!(args[0], "ossutil");
            assert_eq!(args[1], "cp");
            let source = &args[2];
            let destination = &args[3];
            let ignore_existing = args.iter().any(|arg| arg == "--ignore-existing");
            if let Some(key) = source.strip_prefix("oss://bucket/") {
                let remote = bucket_root.join(key);
                if !remote.is_file() {
                    return Ok(failure_status());
                }
                fs::copy(&remote, destination)?;
            } else {
                self.uploads += 1;
                let key = destination
                    .strip_prefix("oss://bucket/")
                    .expect("upload destination is an OSS object");
                let remote = bucket_root.join(key);
                if remote.exists() && ignore_existing {
                    return Ok(success_status());
                }
                fs::create_dir_all(remote.parent().expect("object has a parent"))?;
                let mut bytes = fs::read(source)?;
                if self.corrupt_uploads {
                    bytes.push(b'!');
                }
                fs::write(&remote, bytes)?;
            }
            Ok(success_status())
        }
    }

    fn sample_batch() -> CompleteReferenceBatch {
        CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                pair: "BTCUSDT".to_owned(),
                base_asset: "BTC".to_owned(),
                quote_asset: "USDT".to_owned(),
                margin_asset: "USDT".to_owned(),
                tick_size: Decimal::new(1, 1),
                step_size: Decimal::new(1, 3),
                min_notional: Decimal::new(5, 0),
                contract_type: "PERPETUAL".to_owned(),
                status: "TRADING".to_owned(),
                onboard_date_ms: 1,
                delivery_date_ms: 4_133_404_800_000,
                source_time_ms: SOURCE_MS,
                source_clock_received_at_ns: RECEIVED_NS - 100,
                received_at_ns: RECEIVED_NS - 50,
                source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
                source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
            }],
            vec![MarkIndexFundingObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                mark_price: Decimal::new(101, 0),
                index_price: Decimal::new(100, 0),
                basis: Decimal::ONE,
                basis_rate: Decimal::new(1, 2),
                last_funding_rate: Decimal::new(1, 4),
                interest_rate: Decimal::new(1, 4),
                next_funding_time_ms: SOURCE_MS + 28_800_000,
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS,
                source_endpoint: PREMIUM_INDEX_ENDPOINT.to_owned(),
            }],
            vec![OpenInterestObservation {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                open_interest: Decimal::new(12345, 3),
                source_time_ms: SOURCE_MS,
                received_at_ns: RECEIVED_NS + 50,
                source_endpoint: OPEN_INTEREST_ENDPOINT.to_owned(),
            }],
        )
        .unwrap()
    }

    fn publish_batch(root: &Path, observed_at_ns: u64) -> PublishedReferenceArtifact {
        publish_reference_batch(
            &ReferenceArtifactConfig {
                output_root: root.to_path_buf(),
                observed_at_ns,
                max_staleness_ms: 10_000,
            },
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &sample_batch(),
        )
        .unwrap()
    }

    fn config(root: &Path) -> ReferenceUploadConfig {
        ReferenceUploadConfig {
            output_root: root.to_path_buf(),
            bucket: "bucket".to_owned(),
            endpoint: "oss-ap-northeast-1-internal.aliyuncs.com".to_owned(),
            region: "ap-northeast-1".to_owned(),
            profile: "ecs-role".to_owned(),
            oss_timeout: Duration::from_secs(30),
        }
    }

    fn batch_dir(published: &PublishedReferenceArtifact) -> PathBuf {
        published
            .data_path
            .parent()
            .expect("published artifact has a batch directory")
            .to_path_buf()
    }

    fn remote_key(root: &Path, published: &PublishedReferenceArtifact, name: &str) -> PathBuf {
        batch_dir(published)
            .strip_prefix(root)
            .expect("batch is under the output root")
            .join(name)
    }

    #[test]
    fn uploads_verified_batches_and_removes_local_triplets() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let first = publish_batch(root.path(), RECEIVED_NS + 100);
        let second = publish_batch(root.path(), RECEIVED_NS + 200);
        let mut oss = FakeOss::default();
        let summary = upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .unwrap();
        assert_eq!(
            summary,
            ReferenceUploadSummary {
                uploaded_batches: 2,
                retried_batches: 0,
            }
        );
        assert_eq!(oss.uploads, 6);
        assert!(!batch_dir(&first).exists());
        assert!(!batch_dir(&second).exists());
        assert!(bucket
            .path()
            .join(remote_key(root.path(), &first, DATA_NAME))
            .is_file());
        assert!(bucket
            .path()
            .join(remote_key(root.path(), &second, SUCCESS_NAME))
            .is_file());
        let status: Value =
            serde_json::from_slice(&fs::read(root.path().join("upload-status.json")).unwrap())
                .unwrap();
        assert_eq!(status["last_error"], Value::Null);
        assert_eq!(status["pending_batches"], 0);

        let summary = upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .unwrap();
        assert_eq!(
            summary,
            ReferenceUploadSummary {
                uploaded_batches: 0,
                retried_batches: 0,
            }
        );
    }

    #[test]
    fn empty_batch_directory_does_not_block_complete_batches() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let published = publish_batch(root.path(), RECEIVED_NS + 200);
        let empty = batch_dir(&published)
            .parent()
            .unwrap()
            .join(format!("batch={}", RECEIVED_NS + 100));
        fs::create_dir(&empty).unwrap();

        let mut oss = FakeOss::default();
        let summary = upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .unwrap();

        assert_eq!(summary.uploaded_batches, 1);
        assert!(!batch_dir(&published).exists());
        assert!(!empty.exists());
    }

    #[test]
    fn partial_batch_directory_remains_fail_closed() {
        let root = TestDir::new();
        let published = publish_batch(root.path(), RECEIVED_NS + 100);
        fs::remove_file(&published.success_path).unwrap();

        let error = upload_pending_with(&config(root.path()), &mut |_, _| {
            panic!("partial batches must fail before OSS access")
        })
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("exactly the data/manifest/_SUCCESS triplet"));
        assert!(published.data_path.is_file());
        assert!(published.manifest_path.is_file());
        assert!(batch_dir(&published).is_dir());
    }

    #[test]
    fn matching_remote_triplet_is_an_idempotent_retry() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let published = publish_batch(root.path(), RECEIVED_NS + 100);
        for name in [DATA_NAME, MANIFEST_NAME, SUCCESS_NAME] {
            let remote = bucket.path().join(remote_key(root.path(), &published, name));
            fs::create_dir_all(remote.parent().unwrap()).unwrap();
            fs::copy(batch_dir(&published).join(name), &remote).unwrap();
        }
        let mut oss = FakeOss::default();
        let summary = upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .unwrap();
        assert_eq!(
            summary,
            ReferenceUploadSummary {
                uploaded_batches: 0,
                retried_batches: 1,
            }
        );
        assert_eq!(oss.uploads, 0);
        assert!(!batch_dir(&published).exists());
    }

    #[test]
    fn conflicting_remote_object_fails_closed_and_retains_the_local_batch() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let conflicted = publish_batch(root.path(), RECEIVED_NS + 100);
        let remote = bucket
            .path()
            .join(remote_key(root.path(), &conflicted, DATA_NAME));
        fs::create_dir_all(remote.parent().unwrap()).unwrap();
        fs::write(&remote, b"{\"foreign\":true}\n").unwrap();
        let healthy = publish_batch(root.path(), RECEIVED_NS + 200);
        let mut oss = FakeOss::default();
        let error = upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .unwrap_err();
        assert!(error.to_string().contains("batch(es) failed"));
        assert!(batch_dir(&conflicted).join(DATA_NAME).is_file());
        assert!(!batch_dir(&healthy).exists());
        let status: Value =
            serde_json::from_slice(&fs::read(root.path().join("upload-status.json")).unwrap())
                .unwrap();
        assert_eq!(status["failed_batches"].as_array().unwrap().len(), 1);
        assert_eq!(status["uploaded_batches"], 1);
        assert!(status["last_error"].as_str().unwrap().contains("readback")
            || status["last_error"].as_str().unwrap().contains("trust anchor"));
    }

    #[test]
    fn corrupted_readback_after_upload_fails_and_retains_the_local_batch() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let published = publish_batch(root.path(), RECEIVED_NS + 100);
        let mut oss = FakeOss {
            uploads: 0,
            corrupt_uploads: true,
        };
        assert!(upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .is_err());
        assert!(batch_dir(&published).join(MANIFEST_NAME).is_file());
    }

    #[test]
    fn symlinked_batch_member_is_rejected_before_any_upload() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let published = publish_batch(root.path(), RECEIVED_NS + 100);
        let real = batch_dir(&published).join("reference.ndjson.manifest.json.real");
        fs::rename(&published.manifest_path, &real).unwrap();
        symlink(&real, &published.manifest_path).unwrap();
        let mut oss = FakeOss::default();
        assert!(upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .is_err());
        assert_eq!(oss.uploads, 0);
        assert!(real.is_file());
    }

    #[test]
    fn unexpected_lake_entries_fail_closed() {
        let root = TestDir::new();
        let bucket = TestDir::new();
        let published = publish_batch(root.path(), RECEIVED_NS + 100);
        let malformed = batch_dir(&published).parent().unwrap().join("hour=99");
        fs::create_dir_all(&malformed).unwrap();
        let mut oss = FakeOss::default();
        assert!(upload_pending_with(&config(root.path()), &mut |command, timeout| {
            oss.run(bucket.path(), command, timeout)
        })
        .is_err());
        assert_eq!(oss.uploads, 0);
        assert!(malformed.is_dir());
        assert!(batch_dir(&published).join(DATA_NAME).is_file());
    }
}
