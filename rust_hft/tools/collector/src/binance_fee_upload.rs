use crate::binance_fee_artifact::{verify_fee_artifact, PublishedFeeArtifact};
use crate::binance_usdm_reference_upload::{
    upload_verified_triplet, OssTripletBatch, ReferenceUploadConfig,
};
use crate::lob_archiver::sha256_file;
use crate::polymarket_upload::{atomic_json, ensure_canonical_directory, read_status, utc_now};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

const DATA_NAME: &str = "fee.json";
const MANIFEST_NAME: &str = "fee.json.manifest.json";
const SUCCESS_NAME: &str = "fee.json._SUCCESS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FeeUploadSummary {
    pub uploaded_batches: usize,
    pub retried_batches: usize,
}

struct FeeBatch {
    triplet: OssTripletBatch,
    data_sha256: String,
    manifest_sha256: String,
}

fn direct_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect fee spool directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        bail!(
            "fee spool entry must be a direct directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn direct_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect fee artifact {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!(
            "fee artifact must be a direct regular file: {}",
            path.display()
        );
    }
    Ok(())
}

fn strict_component(depth: usize, name: &str) -> bool {
    match depth {
        0 => name == "raw",
        1 => matches!(name, "venue=binance_spot" | "venue=binance_usdm"),
        2 => name == "dataset=fee",
        3 => name.strip_prefix("account=").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }),
        4 => {
            let Some(date) = name.strip_prefix("date=") else {
                return false;
            };
            let bytes = date.as_bytes();
            bytes.len() == 10
                && bytes[4] == b'-'
                && bytes[7] == b'-'
                && bytes
                    .iter()
                    .enumerate()
                    .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
        }
        5 => name
            .strip_prefix("hour=")
            .is_some_and(|hour| hour.len() == 2 && hour.bytes().all(|byte| byte.is_ascii_digit())),
        6 => name.strip_prefix("batch=").is_some_and(|batch| {
            !batch.is_empty() && batch.bytes().all(|byte| byte.is_ascii_digit())
        }),
        _ => false,
    }
}

fn discover_batches(output_root: &Path) -> Result<Vec<FeeBatch>> {
    let lake = output_root.join("lake");
    if !lake.exists() {
        return Ok(Vec::new());
    }
    direct_directory(&lake)?;
    let mut batches = Vec::new();
    discover_at(output_root, &lake, 0, &mut batches)?;
    batches.sort_by(|left, right| left.triplet.dir.cmp(&right.triplet.dir));
    Ok(batches)
}

fn discover_at(
    output_root: &Path,
    parent: &Path,
    depth: usize,
    batches: &mut Vec<FeeBatch>,
) -> Result<()> {
    if depth == 7 {
        batches.push(fee_batch(output_root, parent)?);
        return Ok(());
    }
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow!("fee spool entry name is not UTF-8"))?;
        if name.starts_with(".fee-staging.") {
            continue;
        }
        if !strict_component(depth, &name) {
            bail!("unexpected fee lake entry: {name}");
        }
        direct_directory(&entry.path())?;
        discover_at(output_root, &entry.path(), depth + 1, batches)?;
    }
    Ok(())
}

fn fee_batch(output_root: &Path, dir: &Path) -> Result<FeeBatch> {
    let members = [
        dir.join(DATA_NAME),
        dir.join(MANIFEST_NAME),
        dir.join(SUCCESS_NAME),
    ];
    for member in &members {
        direct_regular_file(member)?;
    }
    let mut names = fs::read_dir(dir)?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| std::io::Error::other("fee batch entry name is not UTF-8"))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    if names != [DATA_NAME, SUCCESS_NAME, MANIFEST_NAME] {
        bail!("fee batch must contain exactly the data/manifest/_SUCCESS triplet");
    }
    let manifest_bytes = fs::read(&members[1])?;
    let data_sha256 = serde_json::from_slice::<Value>(&manifest_bytes)?["sha256"]
        .as_str()
        .filter(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .context("fee manifest has an invalid data SHA-256")?
        .to_string();
    let manifest_sha256 = sha256_file(&members[1])?;
    let published = PublishedFeeArtifact {
        data_path: members[0].clone(),
        manifest_path: members[1].clone(),
        success_path: members[2].clone(),
        data_sha256: data_sha256.clone(),
        manifest_sha256: manifest_sha256.clone(),
    };
    let snapshot = verify_fee_artifact(&published, &data_sha256, &manifest_sha256)?;
    let object_prefix = dir
        .strip_prefix(output_root)
        .context("fee batch escapes the output root")?
        .to_str()
        .context("fee batch partition is not UTF-8")?
        .to_string();
    let components = object_prefix.split('/').collect::<Vec<_>>();
    if components.get(2).copied() != Some(&format!("venue=binance_{}", snapshot.market))
        || components.get(4).copied() != Some(&format!("account={}", snapshot.account_fingerprint))
    {
        bail!("fee batch partition does not match the authenticated artifact");
    }
    Ok(FeeBatch {
        triplet: OssTripletBatch {
            dir: dir.to_path_buf(),
            object_prefix,
            members,
        },
        data_sha256,
        manifest_sha256,
    })
}

pub fn upload_pending(config: &ReferenceUploadConfig) -> Result<FeeUploadSummary> {
    upload_pending_with(config, |batch, config| {
        upload_verified_triplet(&batch.triplet, config, |downloaded| {
            verify_fee_artifact(
                &PublishedFeeArtifact {
                    data_path: downloaded[0].clone(),
                    manifest_path: downloaded[1].clone(),
                    success_path: downloaded[2].clone(),
                    data_sha256: batch.data_sha256.clone(),
                    manifest_sha256: batch.manifest_sha256.clone(),
                },
                &batch.data_sha256,
                &batch.manifest_sha256,
            )?;
            Ok(())
        })
    })
}

fn upload_pending_with<F>(config: &ReferenceUploadConfig, mut upload: F) -> Result<FeeUploadSummary>
where
    F: FnMut(&FeeBatch, &ReferenceUploadConfig) -> Result<bool>,
{
    config.validate()?;
    ensure_canonical_directory(&config.output_root)?;
    let status_path = config.output_root.join("upload-status.json");
    let mut status = read_status(&status_path)?;
    let mut failures = Vec::new();
    let mut uploaded_batches = 0;
    let mut retried_batches = 0;
    for batch in discover_batches(&config.output_root)? {
        let object = format!(
            "oss://{}/{}/{DATA_NAME}",
            config.bucket, batch.triplet.object_prefix
        );
        match upload(&batch, config) {
            Ok(retried) => {
                if retried {
                    retried_batches += 1;
                } else {
                    uploaded_batches += 1;
                }
                status.insert("last_success_at".to_string(), json!(utc_now()));
                status.insert("last_uploaded_object".to_string(), json!(object));
            }
            Err(error) => failures.push(json!({
                "batch": batch.triplet.dir.file_name().and_then(|name| name.to_str()),
                "error": error.to_string(),
            })),
        }
    }
    status.insert("updated_at".to_string(), json!(utc_now()));
    status.insert("uploaded_batches".to_string(), json!(uploaded_batches));
    status.insert("retried_batches".to_string(), json!(retried_batches));
    status.insert(
        "pending_batches".to_string(),
        json!(discover_batches(&config.output_root)?.len()),
    );
    status.insert("failed_batches".to_string(), Value::Array(failures.clone()));
    status.insert(
        "last_error_at".to_string(),
        if failures.is_empty() {
            Value::Null
        } else {
            json!(utc_now())
        },
    );
    status.insert(
        "last_error".to_string(),
        failures
            .last()
            .and_then(|failure| failure.get("error"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    atomic_json(&status_path, &Value::Object(status))?;
    if failures.is_empty() {
        Ok(FeeUploadSummary {
            uploaded_batches,
            retried_batches,
        })
    } else {
        bail!("{} Binance fee batch(es) failed", failures.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binance_fee_artifact::{
        publish_fee_snapshot, BinanceFeeSnapshot, SideFeeBps, FEE_SCHEMA,
    };
    use chrono::{TimeZone, Utc};
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, ExitStatus};
    use std::time::Duration;

    fn snapshot(root: &Path) -> PublishedFeeArtifact {
        let observed = Utc.timestamp_opt(1_783_987_200, 0).unwrap();
        publish_fee_snapshot(
            root,
            &BinanceFeeSnapshot {
                schema: FEE_SCHEMA.to_string(),
                venue: "binance".to_string(),
                market: "usdm".to_string(),
                symbol: "BTCUSDT".to_string(),
                runtime_account_id: "binance-main".to_string(),
                account_fingerprint: "a".repeat(64),
                maker_fee_bps: SideFeeBps {
                    buy: "2".to_string(),
                    sell: "2".to_string(),
                },
                taker_fee_bps: SideFeeBps {
                    buy: "5".to_string(),
                    sell: "5".to_string(),
                },
                calculation: "account_commission_rate".to_string(),
                source_endpoint: "/fapi/v1/commissionRate".to_string(),
                instrument_rules: None,
                rules_source_endpoint: None,
                requested_at: observed,
                received_at: observed,
            },
        )
        .unwrap()
    }

    fn run_oss(
        remote: &RefCell<BTreeMap<String, Vec<u8>>>,
        corrupt_success: bool,
        command: &mut Command,
        _timeout: Duration,
    ) -> Result<ExitStatus> {
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let source = &args[2];
        let destination = &args[3];
        if source.starts_with("oss://") {
            let mut bytes = remote
                .borrow()
                .get(source)
                .cloned()
                .context("remote object is absent")?;
            if corrupt_success && source.ends_with(SUCCESS_NAME) {
                bytes = format!("{}\n", "0".repeat(64)).into_bytes();
            }
            let destination = Path::new(destination);
            fs::create_dir_all(destination.parent().unwrap())?;
            fs::write(destination, bytes)?;
        } else {
            remote
                .borrow_mut()
                .insert(destination.clone(), fs::read(source)?);
        }
        Ok(ExitStatus::from_raw(0))
    }

    fn upload_with_fake_oss(
        batch: &FeeBatch,
        config: &ReferenceUploadConfig,
        remote: &RefCell<BTreeMap<String, Vec<u8>>>,
        corrupt_success: bool,
    ) -> Result<bool> {
        crate::binance_usdm_reference_upload::upload_verified_triplet_with(
            &batch.triplet,
            config,
            &mut |command, timeout| run_oss(remote, corrupt_success, command, timeout),
            |downloaded| {
                verify_fee_artifact(
                    &PublishedFeeArtifact {
                        data_path: downloaded[0].clone(),
                        manifest_path: downloaded[1].clone(),
                        success_path: downloaded[2].clone(),
                        data_sha256: batch.data_sha256.clone(),
                        manifest_sha256: batch.manifest_sha256.clone(),
                    },
                    &batch.data_sha256,
                    &batch.manifest_sha256,
                )?;
                Ok(())
            },
        )
    }

    #[test]
    fn failed_upload_retains_the_verified_source() {
        let root = tempfile::tempdir().unwrap();
        let published = snapshot(root.path());
        let config = ReferenceUploadConfig {
            output_root: root.path().to_path_buf(),
            bucket: "bucket".to_string(),
            endpoint: "endpoint".to_string(),
            region: "region".to_string(),
            profile: "profile".to_string(),
            oss_timeout: std::time::Duration::from_secs(1),
        };
        let remote = RefCell::new(BTreeMap::new());

        assert!(upload_pending_with(&config, |batch, config| {
            upload_with_fake_oss(batch, config, &remote, true)
        })
        .is_err());
        assert!(published.data_path.exists());
        assert!(published.manifest_path.exists());
        assert!(published.success_path.exists());
    }

    #[test]
    fn verified_upload_removes_the_source_batch() {
        let root = tempfile::tempdir().unwrap();
        let published = snapshot(root.path());
        let config = ReferenceUploadConfig {
            output_root: root.path().to_path_buf(),
            bucket: "bucket".to_string(),
            endpoint: "endpoint".to_string(),
            region: "region".to_string(),
            profile: "profile".to_string(),
            oss_timeout: std::time::Duration::from_secs(1),
        };
        let remote = RefCell::new(BTreeMap::new());

        let summary = upload_pending_with(&config, |batch, config| {
            upload_with_fake_oss(batch, config, &remote, false)
        })
        .unwrap();
        assert_eq!(summary.uploaded_batches, 1);
        assert_eq!(remote.borrow().len(), 3);
        assert!(!published.data_path.exists());
    }
}
