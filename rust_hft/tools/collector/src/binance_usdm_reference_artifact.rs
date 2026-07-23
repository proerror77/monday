//! Atomic publication and strict readback for Binance USD-M reference batches.

use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use data::binance_usdm_reference::{
    ActivePerpetualContract, CompleteReferenceBatch, MarkIndexFundingObservation,
    OpenInterestObservation, ReferenceCoverage, EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT,
    PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
};
use rand::random;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MANIFEST_SCHEMA: &str = "binance.usdm_reference_manifest.v1";
const VENUE: &str = "binance_usdm";
const DATASET: &str = "reference";
const DATA_NAME: &str = "reference.ndjson";
const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SUCCESS_BYTES: u64 = 65;
#[derive(Debug, Clone)]
pub struct ReferenceArtifactConfig {
    pub output_root: PathBuf,
    pub observed_at_ns: u64,
    pub max_staleness_ms: u64,
}
#[derive(Debug, Clone)]
pub struct PublishedReferenceArtifact {
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub data_sha256: String,
    pub manifest_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArtifactCoverage {
    active_contracts: u64,
    metadata_observations: u64,
    mark_index_funding_observations: u64,
    open_interest_observations: u64,
    stale_metadata: u64,
    stale_mark_index_funding: u64,
    stale_open_interest: u64,
    api_error_count: u64,
}

impl From<ReferenceCoverage> for ArtifactCoverage {
    fn from(value: ReferenceCoverage) -> Self {
        Self {
            active_contracts: value.active_contracts,
            metadata_observations: value.metadata_observations,
            mark_index_funding_observations: value.mark_index_funding_observations,
            open_interest_observations: value.open_interest_observations,
            stale_metadata: value.stale_metadata,
            stale_mark_index_funding: value.stale_mark_index_funding,
            stale_open_interest: value.stale_open_interest,
            api_error_count: 0,
        }
    }
}

impl ArtifactCoverage {
    fn is_complete(&self) -> bool {
        self.active_contracts > 0
            && self.active_contracts == self.metadata_observations
            && self.active_contracts == self.mark_index_funding_observations
            && self.active_contracts == self.open_interest_observations
            && self.stale_metadata == 0
            && self.stale_mark_index_funding == 0
            && self.stale_open_interest == 0
            && self.api_error_count == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ArtifactTimeBounds {
    min_source_time_ms: u64,
    max_source_time_ms: u64,
    min_received_at_ns: u64,
    max_received_at_ns: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReferenceManifest {
    schema: String,
    venue: String,
    dataset: String,
    data_schema: String,
    format: String,
    source_origin: String,
    source_endpoints: Vec<String>,
    file: String,
    bytes: u64,
    sha256: String,
    rows: u64,
    observed_at_ns: u64,
    max_staleness_ms: u64,
    coverage: ArtifactCoverage,
    time_bounds: ArtifactTimeBounds,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "observation", rename_all = "snake_case")]
enum ReferenceRecord {
    Metadata(ActivePerpetualContract),
    MarkIndexFunding(MarkIndexFundingObservation),
    OpenInterest(OpenInterestObservation),
}

pub fn publish_reference_batch(
    config: &ReferenceArtifactConfig,
    source_origin: &str,
    batch: &CompleteReferenceBatch,
) -> Result<PublishedReferenceArtifact> {
    publish_reference_batch_inner(config, source_origin, batch, false)
}

pub fn verify_reference_artifact(
    published: &PublishedReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<CompleteReferenceBatch> {
    validate_digest(expected_data_sha256, "expected data")?;
    validate_digest(expected_manifest_sha256, "expected manifest")?;
    validate_artifact_paths(published)?;
    let data = read_bound_file(&published.data_path, MAX_DATA_BYTES)?;
    let manifest_bytes = read_bound_file(&published.manifest_path, MAX_MANIFEST_BYTES)?;
    let success = read_bound_file(&published.success_path, MAX_SUCCESS_BYTES)?;
    if digest(&data) != expected_data_sha256
        || digest(&manifest_bytes) != expected_manifest_sha256
        || success != format!("{expected_data_sha256}\n").as_bytes()
    {
        bail!("reference artifact digest trust anchor does not match");
    }
    let manifest: ReferenceManifest =
        serde_json::from_slice(&manifest_bytes).context("parse reference manifest")?;
    validate_manifest_identity(&manifest, expected_data_sha256, data.len() as u64)?;
    validate_partition_path(&published.data_path, &manifest)?;
    let mut contracts = Vec::new();
    let mut marks = Vec::new();
    let mut open_interest = Vec::new();
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice::<ReferenceRecord>(line)
            .with_context(|| format!("parse reference row {}", index + 1))?
        {
            ReferenceRecord::Metadata(row) => contracts.push(row),
            ReferenceRecord::MarkIndexFunding(row) => marks.push(row),
            ReferenceRecord::OpenInterest(row) => open_interest.push(row),
        }
    }
    let batch = CompleteReferenceBatch::new(contracts, marks, open_interest)?;
    let coverage =
        ArtifactCoverage::from(batch.coverage(manifest.observed_at_ns, manifest.max_staleness_ms)?);
    if coverage != manifest.coverage
        || !coverage.is_complete()
        || time_bounds(&batch)? != manifest.time_bounds
        || manifest.rows != row_count(&batch)
    {
        bail!("reference manifest does not match complete artifact contents");
    }
    Ok(batch)
}

fn publish_reference_batch_inner(
    config: &ReferenceArtifactConfig,
    source_origin: &str,
    batch: &CompleteReferenceBatch,
    fail_after_data: bool,
) -> Result<PublishedReferenceArtifact> {
    if source_origin != OFFICIAL_USDM_SOURCE_ORIGIN {
        bail!("reference artifact source origin is not official Binance");
    }
    let coverage =
        ArtifactCoverage::from(batch.coverage(config.observed_at_ns, config.max_staleness_ms)?);
    if !coverage.is_complete() {
        bail!("reference artifact batch is incomplete or stale");
    }
    let records = batch
        .contracts()
        .iter()
        .cloned()
        .map(ReferenceRecord::Metadata)
        .chain(
            batch
                .mark_index_funding()
                .iter()
                .cloned()
                .map(ReferenceRecord::MarkIndexFunding),
        )
        .chain(
            batch
                .open_interest()
                .iter()
                .cloned()
                .map(ReferenceRecord::OpenInterest),
        );
    let mut data = Vec::new();
    for record in records {
        serde_json::to_writer(&mut data, &record)?;
        data.push(b'\n');
    }
    let data_sha256 = digest(&data);
    let partition = utc_partition(config.observed_at_ns)?;
    let hour_dir = config
        .output_root
        .join("lake/raw")
        .join(format!("venue={VENUE}"))
        .join(format!("dataset={DATASET}"))
        .join(format!("date={}", partition.0))
        .join(format!("hour={}", partition.1));
    ensure_canonical_directory(&hour_dir)?;
    let final_dir = hour_dir.join(format!("batch={}", config.observed_at_ns));
    if fs::symlink_metadata(&final_dir).is_ok() {
        bail!("reference artifact batch already exists");
    }
    let mut staging = StagingDir::create(&hour_dir)?;
    let manifest = ReferenceManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        venue: VENUE.to_owned(),
        dataset: DATASET.to_owned(),
        data_schema: REFERENCE_SCHEMA.to_owned(),
        format: "ndjson".to_owned(),
        source_origin: source_origin.to_owned(),
        source_endpoints: source_endpoints(),
        file: DATA_NAME.to_owned(),
        bytes: data.len() as u64,
        sha256: data_sha256.clone(),
        rows: row_count(batch),
        observed_at_ns: config.observed_at_ns,
        max_staleness_ms: config.max_staleness_ms,
        coverage,
        time_bounds: time_bounds(batch)?,
    };
    let mut manifest_bytes = serde_json::to_vec(&manifest)?;
    manifest_bytes.push(b'\n');
    let manifest_sha256 = digest(&manifest_bytes);
    write_new(&staging.path.join(DATA_NAME), &data)?;
    if fail_after_data {
        bail!("injected reference publication failure after data");
    }
    write_new(
        &staging.path.join(format!("{DATA_NAME}.manifest.json")),
        &manifest_bytes,
    )?;
    write_new(
        &staging.path.join(format!("{DATA_NAME}._SUCCESS")),
        format!("{data_sha256}\n").as_bytes(),
    )?;
    File::open(&staging.path)?.sync_all()?;
    rename_noreplace(&staging.path, &final_dir)?;
    staging.published = true;
    File::open(&hour_dir)?.sync_all()?;
    Ok(PublishedReferenceArtifact {
        data_path: final_dir.join(DATA_NAME),
        manifest_path: final_dir.join(format!("{DATA_NAME}.manifest.json")),
        success_path: final_dir.join(format!("{DATA_NAME}._SUCCESS")),
        data_sha256,
        manifest_sha256,
    })
}

struct StagingDir {
    path: PathBuf,
    published: bool,
}

impl StagingDir {
    fn create(parent: &Path) -> Result<Self> {
        for _ in 0..32 {
            let path = parent.join(format!(".reference-staging.{:016x}", random::<u64>()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        published: false,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not allocate reference artifact staging directory")
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn path_c_string(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow!("path contains a NUL byte: {}", path.display()))
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = path_c_string(source)?;
    let target = path_c_string(target)?;
    // SAFETY: both C strings are NUL-terminated and live for the call.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("no-clobber rename failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = path_c_string(source)?;
    let target = path_c_string(target)?;
    // SAFETY: both C strings are NUL-terminated and live for the call.
    if unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_EXCL,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("no-clobber rename failed");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace(_source: &Path, _target: &Path) -> Result<()> {
    bail!("atomic no-clobber rename is unsupported on this platform")
}

fn read_bound_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        bail!("reference artifact path must be absolute");
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open reference artifact {}", path.display()))?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if !opened.is_file()
        || !named.is_file()
        || named.file_type().is_symlink()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.len() > max_bytes
        || fs::canonicalize(path)? != path
    {
        bail!("reference artifact must be a bounded canonical regular file");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != opened.len() {
        bail!("reference artifact changed during readback");
    }
    Ok(bytes)
}

fn validate_artifact_paths(published: &PublishedReferenceArtifact) -> Result<()> {
    let parent = published
        .data_path
        .parent()
        .ok_or_else(|| anyhow!("reference artifact has no batch directory"))?;
    if published.manifest_path.parent() != Some(parent)
        || published.success_path.parent() != Some(parent)
        || published
            .data_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(DATA_NAME)
        || published
            .manifest_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(&format!("{DATA_NAME}.manifest.json"))
        || published
            .success_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(&format!("{DATA_NAME}._SUCCESS"))
        || fs::canonicalize(parent)? != parent
    {
        bail!("reference artifact paths are not one canonical sibling triplet");
    }
    Ok(())
}

fn validate_manifest_identity(
    manifest: &ReferenceManifest,
    expected_data_sha256: &str,
    data_bytes: u64,
) -> Result<()> {
    if manifest.schema != MANIFEST_SCHEMA
        || manifest.venue != VENUE
        || manifest.dataset != DATASET
        || manifest.data_schema != REFERENCE_SCHEMA
        || manifest.format != "ndjson"
        || manifest.source_origin != OFFICIAL_USDM_SOURCE_ORIGIN
        || manifest.source_endpoints != source_endpoints()
        || manifest.file != DATA_NAME
        || manifest.bytes != data_bytes
        || manifest.sha256 != expected_data_sha256
    {
        bail!("reference manifest identity is invalid");
    }
    Ok(())
}

fn validate_partition_path(data_path: &Path, manifest: &ReferenceManifest) -> Result<()> {
    let batch = data_path.parent().context("missing batch partition")?;
    let hour = batch.parent().context("missing hour partition")?;
    let date = hour.parent().context("missing date partition")?;
    let dataset = date.parent().context("missing dataset partition")?;
    let venue = dataset.parent().context("missing venue partition")?;
    let raw = venue.parent().context("missing raw partition")?;
    let lake = raw.parent().context("missing lake partition")?;
    let expected = utc_partition(manifest.observed_at_ns)?;
    if batch.file_name().and_then(|value| value.to_str())
        != Some(&format!("batch={}", manifest.observed_at_ns))
        || hour.file_name().and_then(|value| value.to_str())
            != Some(&format!("hour={}", expected.1))
        || date.file_name().and_then(|value| value.to_str())
            != Some(&format!("date={}", expected.0))
        || dataset.file_name().and_then(|value| value.to_str())
            != Some(&format!("dataset={DATASET}"))
        || venue.file_name().and_then(|value| value.to_str()) != Some(&format!("venue={VENUE}"))
        || raw.file_name().and_then(|value| value.to_str()) != Some("raw")
        || lake.file_name().and_then(|value| value.to_str()) != Some("lake")
    {
        bail!("reference artifact partition identity is invalid");
    }
    Ok(())
}

fn source_endpoints() -> Vec<String> {
    [
        SERVER_TIME_ENDPOINT,
        EXCHANGE_INFO_ENDPOINT,
        PREMIUM_INDEX_ENDPOINT,
        OPEN_INTEREST_ENDPOINT,
    ]
    .into_iter()
    .map(|endpoint| format!("{OFFICIAL_USDM_SOURCE_ORIGIN}{endpoint}"))
    .collect()
}

fn time_bounds(batch: &CompleteReferenceBatch) -> Result<ArtifactTimeBounds> {
    let source_times = batch
        .contracts()
        .iter()
        .map(|row| row.source_time_ms)
        .chain(
            batch
                .mark_index_funding()
                .iter()
                .map(|row| row.source_time_ms),
        )
        .chain(batch.open_interest().iter().map(|row| row.source_time_ms));
    let received_times = batch
        .contracts()
        .iter()
        .flat_map(|row| [row.source_clock_received_at_ns, row.received_at_ns])
        .chain(
            batch
                .mark_index_funding()
                .iter()
                .map(|row| row.received_at_ns),
        )
        .chain(batch.open_interest().iter().map(|row| row.received_at_ns));
    let sources = source_times.collect::<Vec<_>>();
    let received = received_times.collect::<Vec<_>>();
    Ok(ArtifactTimeBounds {
        min_source_time_ms: *sources
            .iter()
            .min()
            .context("reference batch has no source time")?,
        max_source_time_ms: *sources
            .iter()
            .max()
            .context("reference batch has no source time")?,
        min_received_at_ns: *received
            .iter()
            .min()
            .context("reference batch has no receive time")?,
        max_received_at_ns: *received
            .iter()
            .max()
            .context("reference batch has no receive time")?,
    })
}

fn row_count(batch: &CompleteReferenceBatch) -> u64 {
    (batch.contracts().len() + batch.mark_index_funding().len() + batch.open_interest().len())
        as u64
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_digest(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} SHA-256 must be 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn utc_partition(timestamp_ns: u64) -> Result<(String, String)> {
    let seconds = i64::try_from(timestamp_ns / 1_000_000_000)?;
    let nanos = u32::try_from(timestamp_ns % 1_000_000_000)?;
    let observed = DateTime::<Utc>::from_timestamp(seconds, nanos)
        .context("reference observed time is outside UTC range")?;
    Ok((
        observed.format("%Y-%m-%d").to_string(),
        observed.format("%H").to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_usdm_reference_collector::OFFICIAL_USDM_SOURCE_ORIGIN;
    use data::binance_usdm_reference::{
        ActivePerpetualContract, MarkIndexFundingObservation, OpenInterestObservation,
        EXCHANGE_INFO_ENDPOINT, OPEN_INTEREST_ENDPOINT, PREMIUM_INDEX_ENDPOINT, REFERENCE_SCHEMA,
        SERVER_TIME_ENDPOINT,
    };
    use rust_decimal::Decimal;
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const RECEIVED_NS: u64 = 1_700_000_000_500_000_000;

    fn sample_batch() -> CompleteReferenceBatch {
        CompleteReferenceBatch::new(
            vec![ActivePerpetualContract {
                schema: REFERENCE_SCHEMA.to_owned(),
                symbol: "BTCUSDT".to_owned(),
                pair: "BTCUSDT".to_owned(),
                base_asset: "BTC".to_owned(),
                quote_asset: "USDT".to_owned(),
                margin_asset: "USDT".to_owned(),
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

    fn config(root: PathBuf) -> ReferenceArtifactConfig {
        ReferenceArtifactConfig {
            output_root: root,
            observed_at_ns: RECEIVED_NS + 100,
            max_staleness_ms: 1_000,
        }
    }

    fn publish_fixture() -> (
        tempfile::TempDir,
        ReferenceArtifactConfig,
        PublishedReferenceArtifact,
    ) {
        let temp = tempdir().unwrap();
        let config = config(fs::canonicalize(temp.path()).unwrap());
        let published =
            publish_reference_batch(&config, OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch()).unwrap();
        (temp, config, published)
    }

    #[test]
    fn publishes_and_reads_back_one_complete_canonical_batch() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let published =
            publish_reference_batch(&config(root), OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch())
                .unwrap();

        let verified = verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .unwrap();
        assert_eq!(verified.contracts().len(), 1);
        assert_eq!(verified.mark_index_funding().len(), 1);
        assert_eq!(verified.open_interest().len(), 1);

        let manifest: ReferenceManifest =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.source_endpoints, source_endpoints());
        assert_eq!(manifest.coverage.api_error_count, 0);
        assert_eq!(manifest.coverage.stale_metadata, 0);
        assert_eq!(manifest.time_bounds.min_received_at_ns, RECEIVED_NS - 100);
        assert_eq!(manifest.time_bounds.max_received_at_ns, RECEIVED_NS + 50);
    }

    #[test]
    fn publication_failure_leaks_no_batch_and_retry_succeeds() {
        let temp = tempdir().unwrap();
        let config = config(fs::canonicalize(temp.path()).unwrap());
        let error = publish_reference_batch_inner(
            &config,
            OFFICIAL_USDM_SOURCE_ORIGIN,
            &sample_batch(),
            true,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("injected reference publication failure"));
        let partition = utc_partition(config.observed_at_ns).unwrap();
        let hour = config
            .output_root
            .join("lake/raw")
            .join(format!("venue={VENUE}"))
            .join(format!("dataset={DATASET}"))
            .join(format!("date={}", partition.0))
            .join(format!("hour={}", partition.1));
        assert_eq!(fs::read_dir(&hour).unwrap().count(), 0);

        let published =
            publish_reference_batch(&config, OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch()).unwrap();
        assert!(published.success_path.is_file());
    }

    #[test]
    fn stale_batches_and_non_official_sources_fail_closed() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let mut stale = config(root.clone());
        stale.observed_at_ns = (SOURCE_MS + 2_000) * 1_000_000;
        assert!(
            publish_reference_batch(&stale, OFFICIAL_USDM_SOURCE_ORIGIN, &sample_batch(),)
                .unwrap_err()
                .to_string()
                .contains("incomplete or stale")
        );
        assert!(
            publish_reference_batch(&config(root), "https://example.com", &sample_batch(),)
                .unwrap_err()
                .to_string()
                .contains("not official Binance")
        );
    }

    #[test]
    fn external_digest_anchors_and_file_contents_are_both_required() {
        let (_temp, _config, published) = publish_fixture();
        let wrong = "0".repeat(64);
        assert!(
            verify_reference_artifact(&published, &wrong, &published.manifest_sha256,).is_err()
        );

        let mut tampered = fs::read(&published.data_path).unwrap();
        tampered.extend_from_slice(b"{}\n");
        fs::write(&published.data_path, tampered).unwrap();
        assert!(verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .is_err());
    }

    #[test]
    fn symlinked_artifact_members_are_rejected() {
        let (_temp, _config, published) = publish_fixture();
        fs::remove_file(&published.success_path).unwrap();
        symlink(&published.data_path, &published.success_path).unwrap();
        assert!(verify_reference_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .is_err());
    }
}
