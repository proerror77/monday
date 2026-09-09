//! Atomic publication and strict readback for public Binance Spot rules.

use crate::binance_usdm_reference_artifact::{read_bound_file, rename_noreplace, write_new};
use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use data::binance_reference_common::validate_receive_clock;
use data::binance_spot_reference::{
    SpotInstrumentRules, SpotReferenceBatch, EXCHANGE_INFO_ENDPOINT, OFFICIAL_SOURCE_ORIGIN,
    REFERENCE_SCHEMA, SERVER_TIME_ENDPOINT,
};
use rand::random;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub const MANIFEST_SCHEMA: &str = "binance.spot_reference_manifest.v1";
pub const VENUE: &str = "binance_spot";
pub const DATASET: &str = "reference";
pub const DATA_NAME: &str = "reference.ndjson";
pub const MANIFEST_NAME: &str = "reference.ndjson.manifest.json";
pub const SUCCESS_NAME: &str = "reference.ndjson._SUCCESS";
const MAX_DATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SUCCESS_BYTES: u64 = 65;

#[derive(Debug, Clone)]
pub struct SpotReferenceArtifactConfig {
    pub output_root: PathBuf,
    pub observed_at_ns: u64,
    pub max_staleness_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PublishedSpotReferenceArtifact {
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub data_sha256: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VerifiedSpotReferenceCounts {
    pub metadata: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpotCoverage {
    active_spot_symbols: u64,
    metadata_observations: u64,
    stale_metadata: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpotReferenceManifest {
    schema: String,
    venue: String,
    market: String,
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
    source_time_ms: u64,
    source_clock_received_at_ns: u64,
    exchange_info_received_at_ns: u64,
    coverage: SpotCoverage,
}

pub fn publish_spot_reference(
    config: &SpotReferenceArtifactConfig,
    source_origin: &str,
    server_time_received_at_ns: u64,
    exchange_info_received_at_ns: u64,
    batch: &SpotReferenceBatch,
) -> Result<PublishedSpotReferenceArtifact> {
    validate_config(config)?;
    if source_origin != OFFICIAL_SOURCE_ORIGIN {
        bail!("Spot reference artifact source origin is not official Binance");
    }
    if server_time_received_at_ns > exchange_info_received_at_ns
        || exchange_info_received_at_ns > config.observed_at_ns
    {
        bail!("Spot reference receive clocks are not monotonic");
    }
    let source_times = batch.source_time_bounds();
    if source_times.0 != source_times.1 {
        bail!("Spot reference batch has inconsistent server-time clocks");
    }
    let received_times = batch.received_time_bounds();
    if received_times.0 != server_time_received_at_ns
        || received_times.1 != exchange_info_received_at_ns
    {
        bail!("Spot reference batch receive clocks do not match the collector");
    }
    let coverage = coverage(batch, config.observed_at_ns, config.max_staleness_ms)?;
    if coverage.stale_metadata != 0 {
        bail!("Spot reference batch is stale at publication");
    }
    let data = encode_batch(batch)?;
    let data_sha256 = digest(&data);
    let (date, hour) = utc_partition(config.observed_at_ns)?;
    let hour_dir = config
        .output_root
        .join("lake/raw")
        .join(format!("venue={VENUE}"))
        .join(format!("dataset={DATASET}"))
        .join(format!("date={date}"))
        .join(format!("hour={hour}"));
    ensure_canonical_directory(&hour_dir)?;
    let final_dir = hour_dir.join(format!("batch={}", config.observed_at_ns));
    if fs::symlink_metadata(&final_dir).is_ok() {
        bail!("Spot reference artifact batch already exists");
    }
    let mut staging = StagingDir::create(&hour_dir)?;
    let manifest = SpotReferenceManifest {
        schema: MANIFEST_SCHEMA.to_owned(),
        venue: VENUE.to_owned(),
        market: "spot".to_owned(),
        dataset: DATASET.to_owned(),
        data_schema: REFERENCE_SCHEMA.to_owned(),
        format: "ndjson".to_owned(),
        source_origin: source_origin.to_owned(),
        source_endpoints: source_endpoints(),
        file: DATA_NAME.to_owned(),
        bytes: data.len() as u64,
        sha256: data_sha256.clone(),
        rows: batch.rules().len() as u64,
        observed_at_ns: config.observed_at_ns,
        max_staleness_ms: config.max_staleness_ms,
        source_time_ms: source_times.0,
        source_clock_received_at_ns: server_time_received_at_ns,
        exchange_info_received_at_ns,
        coverage,
    };
    let mut manifest_bytes = serde_json::to_vec(&manifest)?;
    manifest_bytes.push(b'\n');
    let manifest_sha256 = digest(&manifest_bytes);
    write_new(&staging.path.join(DATA_NAME), &data)?;
    write_new(&staging.path.join(MANIFEST_NAME), &manifest_bytes)?;
    write_new(
        &staging.path.join(SUCCESS_NAME),
        format!("{data_sha256}\n").as_bytes(),
    )?;
    let staged = PublishedSpotReferenceArtifact {
        data_path: staging.path.join(DATA_NAME),
        manifest_path: staging.path.join(MANIFEST_NAME),
        success_path: staging.path.join(SUCCESS_NAME),
        data_sha256: data_sha256.clone(),
        manifest_sha256: manifest_sha256.clone(),
    };
    verify_spot_reference_artifact_inner(&staged, &data_sha256, &manifest_sha256, false)?;
    File::open(&staging.path)?.sync_all()?;
    rename_noreplace(&staging.path, &final_dir)?;
    staging.published = true;
    File::open(&hour_dir)?.sync_all()?;
    let published = PublishedSpotReferenceArtifact {
        data_path: final_dir.join(DATA_NAME),
        manifest_path: final_dir.join(MANIFEST_NAME),
        success_path: final_dir.join(SUCCESS_NAME),
        data_sha256,
        manifest_sha256,
    };
    verify_spot_reference_artifact(
        &published,
        &published.data_sha256,
        &published.manifest_sha256,
    )?;
    Ok(published)
}

pub fn verify_spot_reference_artifact(
    artifact: &PublishedSpotReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<SpotReferenceBatch> {
    verify_spot_reference_artifact_inner(
        artifact,
        expected_data_sha256,
        expected_manifest_sha256,
        true,
    )
}

fn verify_spot_reference_artifact_inner(
    artifact: &PublishedSpotReferenceArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
    require_final_partition: bool,
) -> Result<SpotReferenceBatch> {
    validate_digest(expected_data_sha256, "expected Spot data")?;
    validate_digest(expected_manifest_sha256, "expected Spot manifest")?;
    validate_artifact_paths(artifact)?;
    let data = read_bound_file(&artifact.data_path, MAX_DATA_BYTES)?;
    let manifest_bytes = read_bound_file(&artifact.manifest_path, MAX_MANIFEST_BYTES)?;
    let success = read_bound_file(&artifact.success_path, MAX_SUCCESS_BYTES)?;
    if digest(&data) != expected_data_sha256
        || digest(&manifest_bytes) != expected_manifest_sha256
        || success != format!("{expected_data_sha256}\n").as_bytes()
    {
        bail!("Spot reference artifact digest trust anchor does not match");
    }
    let manifest: SpotReferenceManifest =
        serde_json::from_slice(&manifest_bytes).context("parse Spot reference manifest")?;
    let batch = parse_batch(&data)?;
    validate_manifest(
        &manifest,
        artifact,
        &data,
        expected_data_sha256,
        &batch,
        require_final_partition,
    )?;
    Ok(batch)
}

fn validate_config(config: &SpotReferenceArtifactConfig) -> Result<()> {
    if !config.output_root.is_absolute()
        || config.observed_at_ns == 0
        || config.max_staleness_ms > 300_000
    {
        bail!("Spot reference artifact configuration is invalid");
    }
    ensure_canonical_directory(&config.output_root)
}

fn encode_batch(batch: &SpotReferenceBatch) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    for rule in batch.rules() {
        serde_json::to_writer(&mut data, rule)?;
        data.push(b'\n');
    }
    Ok(data)
}

fn parse_batch(data: &[u8]) -> Result<SpotReferenceBatch> {
    let mut rules = Vec::new();
    for (index, line) in data.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        rules.push(
            serde_json::from_slice::<SpotInstrumentRules>(line)
                .with_context(|| format!("parse Spot reference row {}", index + 1))?,
        );
    }
    SpotReferenceBatch::new(rules)
}

fn coverage(
    batch: &SpotReferenceBatch,
    observed_at_ns: u64,
    max_staleness_ms: u64,
) -> Result<SpotCoverage> {
    let observed_at_ms = observed_at_ns / 1_000_000;
    for rule in batch.rules() {
        validate_receive_clock(rule.source_time_ms, observed_at_ns)?;
    }
    let stale_metadata = batch
        .rules()
        .iter()
        .filter(|rule| observed_at_ms.saturating_sub(rule.source_time_ms) > max_staleness_ms)
        .count() as u64;
    Ok(SpotCoverage {
        active_spot_symbols: batch.rules().len() as u64,
        metadata_observations: batch.rules().len() as u64,
        stale_metadata,
    })
}

fn validate_manifest(
    manifest: &SpotReferenceManifest,
    artifact: &PublishedSpotReferenceArtifact,
    data: &[u8],
    expected_data_sha256: &str,
    batch: &SpotReferenceBatch,
    require_final_partition: bool,
) -> Result<()> {
    let source_times = batch.source_time_bounds();
    let received_times = batch.received_time_bounds();
    let expected_coverage = coverage(batch, manifest.observed_at_ns, manifest.max_staleness_ms)?;
    let observed_at_ms = manifest.observed_at_ns / 1_000_000;
    if manifest.schema != MANIFEST_SCHEMA
        || manifest.venue != VENUE
        || manifest.market != "spot"
        || manifest.dataset != DATASET
        || manifest.data_schema != REFERENCE_SCHEMA
        || manifest.format != "ndjson"
        || manifest.source_origin != OFFICIAL_SOURCE_ORIGIN
        || manifest.source_endpoints != source_endpoints()
        || manifest.file != DATA_NAME
        || manifest.bytes != data.len() as u64
        || manifest.sha256 != expected_data_sha256
        || manifest.rows != batch.rules().len() as u64
        || manifest.coverage != expected_coverage
        || manifest.coverage.stale_metadata != 0
        || manifest.source_time_ms != source_times.0
        || source_times.0 != source_times.1
        || manifest.source_clock_received_at_ns != received_times.0
        || manifest.exchange_info_received_at_ns != received_times.1
        || manifest.observed_at_ns == 0
        || manifest.max_staleness_ms > 300_000
        || received_times.0 > received_times.1
        || received_times.1 > manifest.observed_at_ns
        || observed_at_ms.saturating_sub(manifest.source_time_ms) > manifest.max_staleness_ms
    {
        bail!("Spot reference manifest identity or coverage is invalid");
    }
    if require_final_partition {
        validate_partition_path(&artifact.data_path, manifest.observed_at_ns)?;
    }
    Ok(())
}

fn validate_artifact_paths(artifact: &PublishedSpotReferenceArtifact) -> Result<()> {
    let parent = artifact
        .data_path
        .parent()
        .context("Spot reference artifact has no batch directory")?;
    if !artifact.data_path.is_absolute()
        || artifact.manifest_path.parent() != Some(parent)
        || artifact.success_path.parent() != Some(parent)
        || artifact
            .data_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(DATA_NAME)
        || artifact
            .manifest_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(MANIFEST_NAME)
        || artifact
            .success_path
            .file_name()
            .and_then(|name| name.to_str())
            != Some(SUCCESS_NAME)
        || fs::canonicalize(parent)? != parent
    {
        bail!("Spot reference artifact paths are not one canonical triplet");
    }
    Ok(())
}

fn validate_partition_path(data_path: &Path, observed_at_ns: u64) -> Result<()> {
    let batch = data_path
        .parent()
        .context("Spot reference batch is missing")?;
    let hour = batch.parent().context("Spot reference hour is missing")?;
    let date = hour.parent().context("Spot reference date is missing")?;
    let dataset = date.parent().context("Spot reference dataset is missing")?;
    let venue = dataset
        .parent()
        .context("Spot reference venue is missing")?;
    let raw = venue
        .parent()
        .context("Spot reference raw partition is missing")?;
    let lake = raw.parent().context("Spot reference lake is missing")?;
    let (expected_date, expected_hour) = utc_partition(observed_at_ns)?;
    if batch.file_name().and_then(|name| name.to_str()) != Some(&format!("batch={observed_at_ns}"))
        || hour.file_name().and_then(|name| name.to_str()) != Some(&format!("hour={expected_hour}"))
        || date.file_name().and_then(|name| name.to_str()) != Some(&format!("date={expected_date}"))
        || dataset.file_name().and_then(|name| name.to_str()) != Some(&format!("dataset={DATASET}"))
        || venue.file_name().and_then(|name| name.to_str()) != Some(&format!("venue={VENUE}"))
        || raw.file_name().and_then(|name| name.to_str()) != Some("raw")
        || lake.file_name().and_then(|name| name.to_str()) != Some("lake")
    {
        bail!("Spot reference artifact partition identity is invalid");
    }
    Ok(())
}

fn source_endpoints() -> Vec<String> {
    [SERVER_TIME_ENDPOINT, EXCHANGE_INFO_ENDPOINT]
        .into_iter()
        .map(|endpoint| format!("{OFFICIAL_SOURCE_ORIGIN}{endpoint}"))
        .collect()
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

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn utc_partition(timestamp_ns: u64) -> Result<(String, String)> {
    let seconds = i64::try_from(timestamp_ns / 1_000_000_000)?;
    let nanos = u32::try_from(timestamp_ns % 1_000_000_000)?;
    let observed = DateTime::<Utc>::from_timestamp(seconds, nanos)
        .context("Spot reference observed time is outside UTC range")?;
    Ok((
        observed.format("%Y-%m-%d").to_string(),
        observed.format("%H").to_string(),
    ))
}

struct StagingDir {
    path: PathBuf,
    published: bool,
}

impl StagingDir {
    fn create(parent: &Path) -> Result<Self> {
        for _ in 0..32 {
            let path = parent.join(format!(".spot-reference-staging.{:016x}", random::<u64>()));
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
        bail!("could not allocate Spot reference artifact staging directory");
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::binance_spot_reference::{SpotNotionalFilter, SpotPriceFilter, SpotQuantityFilter};
    use rust_decimal::Decimal;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    const SOURCE_MS: u64 = 1_700_000_000_000;
    const CLOCK_RECEIVED_NS: u64 = 1_700_000_000_100_000_000;
    const EXCHANGE_RECEIVED_NS: u64 = 1_700_000_000_200_000_000;
    const OBSERVED_NS: u64 = 1_700_000_000_300_000_000;

    fn batch() -> SpotReferenceBatch {
        SpotReferenceBatch::new(vec![SpotInstrumentRules {
            schema: REFERENCE_SCHEMA.to_owned(),
            venue: "binance".to_owned(),
            market: "spot".to_owned(),
            symbol: "BTCUSDT".to_owned(),
            base_asset: "BTC".to_owned(),
            quote_asset: "USDT".to_owned(),
            status: "TRADING".to_owned(),
            is_spot_trading_allowed: true,
            base_asset_precision: 8,
            quote_asset_precision: 8,
            price_filter: SpotPriceFilter {
                min_price: Decimal::ZERO,
                max_price: Decimal::ZERO,
                tick_size: Decimal::ZERO,
            },
            lot_size_filter: SpotQuantityFilter {
                min_quantity: Decimal::new(1, 5),
                max_quantity: Decimal::from(9_000),
                step_size: Decimal::new(1, 5),
            },
            market_lot_size_filter: Some(SpotQuantityFilter {
                min_quantity: Decimal::ZERO,
                max_quantity: Decimal::from(9_000),
                step_size: Decimal::ZERO,
            }),
            notional_filter: SpotNotionalFilter {
                filter_type: "MIN_NOTIONAL".to_owned(),
                min_notional: Decimal::from(5),
                max_notional: None,
                apply_min_to_market: true,
                apply_max_to_market: None,
                avg_price_mins: 5,
            },
            source_time_ms: SOURCE_MS,
            source_clock_received_at_ns: CLOCK_RECEIVED_NS,
            received_at_ns: EXCHANGE_RECEIVED_NS,
            source_endpoint: EXCHANGE_INFO_ENDPOINT.to_owned(),
            source_clock_endpoint: SERVER_TIME_ENDPOINT.to_owned(),
        }])
        .unwrap()
    }

    fn config(root: PathBuf) -> SpotReferenceArtifactConfig {
        SpotReferenceArtifactConfig {
            output_root: root,
            observed_at_ns: OBSERVED_NS,
            max_staleness_ms: 1_000,
        }
    }

    fn fixture() -> (tempfile::TempDir, PublishedSpotReferenceArtifact) {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let artifact = publish_spot_reference(
            &config(root),
            OFFICIAL_SOURCE_ORIGIN,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            &batch(),
        )
        .unwrap();
        (temp, artifact)
    }

    #[test]
    fn publishes_and_reads_back_an_immutable_spot_triplet() {
        let (_temp, artifact) = fixture();
        let verified = verify_spot_reference_artifact(
            &artifact,
            &artifact.data_sha256,
            &artifact.manifest_sha256,
        )
        .unwrap();
        assert_eq!(verified.rules().len(), 1);
        assert_eq!(verified.rules()[0].symbol, "BTCUSDT");
        assert_eq!(verified.rules()[0].price_filter.tick_size, Decimal::ZERO);
        assert_eq!(verified.rules()[0].notional_filter.avg_price_mins, 5);
        assert_eq!(
            verified.rules()[0]
                .market_lot_size_filter
                .as_ref()
                .map(|filter| filter.step_size),
            Some(Decimal::ZERO)
        );
    }

    #[test]
    fn hash_marker_path_and_repeat_creation_tampering_fail_closed() {
        let (temp, artifact) = fixture();
        assert!(publish_spot_reference(
            &config(fs::canonicalize(temp.path()).unwrap()),
            OFFICIAL_SOURCE_ORIGIN,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            &batch(),
        )
        .is_err());
        fs::write(&artifact.success_path, b"bad\n").unwrap();
        assert!(verify_spot_reference_artifact(
            &artifact,
            &artifact.data_sha256,
            &artifact.manifest_sha256,
        )
        .is_err());

        let (_temp, mut path_tampered) = fixture();
        let target = path_tampered.data_path.clone();
        let link = target.with_file_name("link.ndjson");
        symlink(&target, &link).unwrap();
        path_tampered.data_path = link;
        assert!(verify_spot_reference_artifact(
            &path_tampered,
            &path_tampered.data_sha256,
            &path_tampered.manifest_sha256,
        )
        .is_err());
    }

    #[test]
    fn publication_rejects_wrong_origin_and_expired_clock() {
        let temp = tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        assert!(publish_spot_reference(
            &config(root.clone()),
            "https://example.com",
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            &batch(),
        )
        .is_err());
        let mut expired = config(root);
        expired.observed_at_ns = EXCHANGE_RECEIVED_NS + 2_000_000_000;
        assert!(publish_spot_reference(
            &expired,
            OFFICIAL_SOURCE_ORIGIN,
            CLOCK_RECEIVED_NS,
            EXCHANGE_RECEIVED_NS,
            &batch(),
        )
        .is_err());
    }
}
