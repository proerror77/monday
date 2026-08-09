use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

pub const FEE_SCHEMA: &str = "binance.fee-snapshot.v1";
const DATA_NAME: &str = "fee.json";
const MANIFEST_NAME: &str = "fee.json.manifest.json";
const SUCCESS_NAME: &str = "fee.json._SUCCESS";
const MAX_DATA_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SUCCESS_BYTES: u64 = 65;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinanceFeeSnapshot {
    pub schema: String,
    pub venue: String,
    pub market: String,
    pub symbol: String,
    pub maker_fee_bps: String,
    pub taker_fee_bps: String,
    pub calculation: String,
    pub source_endpoint: String,
    pub instrument_rules: Option<BinanceInstrumentRules>,
    pub rules_source_endpoint: Option<String>,
    pub requested_at: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
}

impl BinanceFeeSnapshot {
    pub fn validate(&self) -> Result<()> {
        if self.schema != FEE_SCHEMA
            || self.venue != "binance"
            || !matches!(self.market.as_str(), "spot" | "usdm")
            || self.symbol.is_empty()
            || self.symbol.len() > 32
            || !self
                .symbol
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            || self.received_at < self.requested_at
            || !nonnegative_decimal(&self.maker_fee_bps)
            || !nonnegative_decimal(&self.taker_fee_bps)
            || match self.market.as_str() {
                "spot" => {
                    self.calculation != "standard_plus_special_plus_tax_without_asset_discount"
                        || self.source_endpoint != "/api/v3/account/commission"
                        || self.rules_source_endpoint.as_deref() != Some("/api/v3/exchangeInfo")
                        || self
                            .instrument_rules
                            .as_ref()
                            .is_none_or(|rules| !rules.valid())
                }
                "usdm" => {
                    self.calculation != "account_commission_rate"
                        || self.source_endpoint != "/fapi/v1/commissionRate"
                        || self.instrument_rules.is_some()
                        || self.rules_source_endpoint.is_some()
                }
                _ => true,
            }
        {
            bail!("Binance fee snapshot is invalid");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinanceInstrumentRules {
    pub tick_size: String,
    pub step_size: String,
    pub min_notional: String,
}

impl BinanceInstrumentRules {
    fn valid(&self) -> bool {
        [&self.tick_size, &self.step_size, &self.min_notional]
            .into_iter()
            .all(|value| {
                value
                    .parse::<rust_decimal::Decimal>()
                    .is_ok_and(|value| value > rust_decimal::Decimal::ZERO)
            })
    }
}

fn nonnegative_decimal(value: &str) -> bool {
    value
        .parse::<rust_decimal::Decimal>()
        .is_ok_and(|value| value >= rust_decimal::Decimal::ZERO)
}

#[derive(Debug, Clone)]
pub struct PublishedFeeArtifact {
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub data_sha256: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeeManifest {
    schema: String,
    data_schema: String,
    venue: String,
    market: String,
    symbol: String,
    file: String,
    bytes: u64,
    sha256: String,
    received_at: DateTime<Utc>,
}

pub fn publish_fee_snapshot(
    root: &Path,
    snapshot: &BinanceFeeSnapshot,
) -> Result<PublishedFeeArtifact> {
    snapshot.validate()?;
    let data = serde_json::to_vec(snapshot)?;
    let data_sha256 = hex::encode(Sha256::digest(&data));
    let received = snapshot.received_at;
    let dir = root.join(format!(
        "lake/raw/venue=binance_{}/dataset=fee/date={:04}-{:02}-{:02}/hour={:02}/batch={}",
        snapshot.market,
        received.year(),
        received.month(),
        received.day(),
        received.hour(),
        received
            .timestamp_nanos_opt()
            .context("fee timestamp is out of range")?
    ));
    let parent = dir
        .parent()
        .context("fee artifact has no partition parent")?;
    ensure_canonical_directory(parent)?;
    if dir.exists() {
        bail!("fee artifact batch already exists");
    }
    let staging = tempfile::Builder::new()
        .prefix(".fee-staging.")
        .tempdir_in(parent)?;
    let data_path = staging.path().join(DATA_NAME);
    let manifest_path = staging.path().join(MANIFEST_NAME);
    let success_path = staging.path().join(SUCCESS_NAME);
    let manifest = FeeManifest {
        schema: "binance.fee-artifact-manifest.v1".to_string(),
        data_schema: FEE_SCHEMA.to_string(),
        venue: "binance".to_string(),
        market: snapshot.market.clone(),
        symbol: snapshot.symbol.clone(),
        file: DATA_NAME.to_string(),
        bytes: data.len() as u64,
        sha256: data_sha256.clone(),
        received_at: snapshot.received_at,
    };
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    write_new(&data_path, &data)?;
    write_new(&manifest_path, &manifest_bytes)?;
    write_new(&success_path, format!("{data_sha256}\n").as_bytes())?;
    let published = PublishedFeeArtifact {
        data_path,
        manifest_path,
        success_path,
        data_sha256,
        manifest_sha256: hex::encode(Sha256::digest(&manifest_bytes)),
    };
    verify_fee_artifact(
        &published,
        &published.data_sha256,
        &published.manifest_sha256,
    )?;
    rename_noreplace(staging.path(), &dir)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(PublishedFeeArtifact {
        data_path: dir.join(DATA_NAME),
        manifest_path: dir.join(MANIFEST_NAME),
        success_path: dir.join(SUCCESS_NAME),
        data_sha256: published.data_sha256,
        manifest_sha256: published.manifest_sha256,
    })
}

pub fn verify_fee_artifact(
    artifact: &PublishedFeeArtifact,
    expected_data_sha256: &str,
    expected_manifest_sha256: &str,
) -> Result<BinanceFeeSnapshot> {
    validate_digest(expected_data_sha256, "expected data")?;
    validate_digest(expected_manifest_sha256, "expected manifest")?;
    validate_artifact_paths(artifact)?;
    let data = read_bound_file(&artifact.data_path, MAX_DATA_BYTES)?;
    let manifest_bytes = read_bound_file(&artifact.manifest_path, MAX_MANIFEST_BYTES)?;
    let success = read_bound_file(&artifact.success_path, MAX_SUCCESS_BYTES)?;
    let data_sha256 = hex::encode(Sha256::digest(&data));
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    if data_sha256 != expected_data_sha256.to_ascii_lowercase()
        || manifest_sha256 != expected_manifest_sha256.to_ascii_lowercase()
        || success != format!("{data_sha256}\n").as_bytes()
    {
        bail!("fee artifact digest readback failed");
    }
    let manifest: FeeManifest = serde_json::from_slice(&manifest_bytes)?;
    let snapshot: BinanceFeeSnapshot = serde_json::from_slice(&data)?;
    snapshot.validate()?;
    if manifest.schema != "binance.fee-artifact-manifest.v1"
        || manifest.data_schema != FEE_SCHEMA
        || manifest.venue != snapshot.venue
        || manifest.market != snapshot.market
        || manifest.symbol != snapshot.symbol
        || manifest.file != DATA_NAME
        || manifest.bytes != data.len() as u64
        || manifest.sha256 != data_sha256
        || manifest.received_at != snapshot.received_at
    {
        bail!("fee artifact manifest does not match data");
    }
    Ok(snapshot)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn validate_artifact_paths(artifact: &PublishedFeeArtifact) -> Result<()> {
    let parent = artifact
        .data_path
        .parent()
        .context("fee artifact has no parent")?;
    if !artifact.data_path.is_absolute()
        || artifact
            .data_path
            .file_name()
            .and_then(|value| value.to_str())
            != Some(DATA_NAME)
        || artifact.manifest_path != parent.join(MANIFEST_NAME)
        || artifact.success_path != parent.join(SUCCESS_NAME)
        || fs::canonicalize(parent)? != parent
    {
        bail!("fee artifact paths are not one canonical sibling triplet");
    }
    Ok(())
}

fn read_bound_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
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
        bail!("fee artifact must be a bounded canonical regular file");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.read_to_end(&mut bytes)?;
    if bytes.len() as u64 != opened.len() {
        bail!("fee artifact changed during readback");
    }
    Ok(bytes)
}

fn validate_digest(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} SHA-256 must be lowercase hexadecimal");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())?;
    let target = CString::new(target.as_os_str().as_bytes())?;
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
        return Err(std::io::Error::last_os_error()).context("no-clobber fee rename failed");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn rename_noreplace(source: &Path, target: &Path) -> Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())?;
    let target = CString::new(target.as_os_str().as_bytes())?;
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
        return Err(std::io::Error::last_os_error()).context("no-clobber fee rename failed");
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace(_source: &Path, _target: &Path) -> Result<()> {
    bail!("atomic no-clobber fee rename is unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn canonical_triplet_rejects_tampered_data_even_with_original_anchor() {
        let root = tempfile::tempdir().unwrap();
        let observed_at = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
        let published = publish_fee_snapshot(
            &fs::canonicalize(root.path()).unwrap(),
            &BinanceFeeSnapshot {
                schema: FEE_SCHEMA.to_string(),
                venue: "binance".to_string(),
                market: "spot".to_string(),
                symbol: "BTCUSDT".to_string(),
                maker_fee_bps: "10".to_string(),
                taker_fee_bps: "10".to_string(),
                calculation: "standard_plus_special_plus_tax_without_asset_discount".to_string(),
                source_endpoint: "/api/v3/account/commission".to_string(),
                instrument_rules: Some(BinanceInstrumentRules {
                    tick_size: "0.01".to_string(),
                    step_size: "0.00001".to_string(),
                    min_notional: "5".to_string(),
                }),
                rules_source_endpoint: Some("/api/v3/exchangeInfo".to_string()),
                requested_at: observed_at,
                received_at: observed_at,
            },
        )
        .unwrap();
        assert_eq!(
            verify_fee_artifact(
                &published,
                &published.data_sha256,
                &published.manifest_sha256,
            )
            .unwrap()
            .symbol,
            "BTCUSDT"
        );
        fs::write(&published.data_path, b"{}\n").unwrap();
        assert!(verify_fee_artifact(
            &published,
            &published.data_sha256,
            &published.manifest_sha256,
        )
        .is_err());
    }
}
