use crate::polymarket_research_import::ResearchSegmentValidationReport;
use crate::polymarket_research_normalize::{
    normalize_polymarket_evidence, NormalizedPolymarketEvidence, PolymarketEvidenceConfig,
    PolymarketEvidenceReport,
};
use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const SURFACES: [&str; 5] = [
    "chainlink_reference",
    "market_contract",
    "orderbook_snapshot",
    "polymarket_trade",
    "official_settlement_evidence",
];

const CONTENT_DIGEST_SEMANTICS: &str =
    "content_sha256 binds the published NDJSON bytes only; it is not a snapshot_contract_hash";
const PUBLISHED_MODE: u32 = 0o444;

#[derive(Debug, Clone)]
pub struct PolymarketEvidenceArtifactConfig {
    pub evidence: PolymarketEvidenceConfig,
    pub output_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedPolymarketEvidenceDigests {
    pub expected_content_sha256: String,
    pub expected_manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedPolymarketEvidence {
    pub schema: &'static str,
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub published_digests: PublishedPolymarketEvidenceDigests,
    pub evidence: PolymarketEvidenceReport,
}

#[derive(Serialize)]
struct OrderBookSemantics {
    level: &'static str,
    depth: &'static str,
    quote_sample_ms: u64,
    venue_depth_complete: bool,
    temporal_updates_complete: bool,
    l3_order_ids_available: bool,
    queue_position_modeled: bool,
    endogenous_impact_modeled: bool,
    capacity_modeled: bool,
}

#[derive(Serialize)]
struct RecordingSemantics {
    orderbook: OrderBookSemantics,
    trades: &'static str,
    references: &'static str,
    settlement: &'static str,
    availability_clock: &'static str,
}

#[derive(Serialize)]
struct PolymarketEvidenceManifest<'a> {
    schema: &'static str,
    file: &'a str,
    format: &'static str,
    content_sha256: &'a str,
    content_bytes: u64,
    rows: u64,
    events: u64,
    surface_counts: &'a BTreeMap<String, u64>,
    event_start_gte: &'a str,
    event_start_lt: &'a str,
    symbols: [&'static str; 2],
    window_secs: u64,
    event_selection: &'a str,
    evidence_scope: &'static str,
    content_digest_semantics: &'static str,
    recording_semantics: RecordingSemantics,
    trust_boundary: &'a str,
    validated_inputs: &'a ResearchSegmentValidationReport,
}

struct ArtifactBytes<'a> {
    data: &'a [u8],
    manifest: &'a [u8],
    success: &'a [u8],
}

fn recording_semantics() -> RecordingSemantics {
    RecordingSemantics {
        orderbook: OrderBookSemantics {
            level: "L2",
            depth: "full visible depth as received",
            quote_sample_ms: 1_000,
            venue_depth_complete: true,
            temporal_updates_complete: false,
            l3_order_ids_available: false,
            queue_position_modeled: false,
            endogenous_impact_modeled: false,
            capacity_modeled: false,
        },
        trades: "exact market_id association using canonical v2 records; selected event count and record IDs match a collector completion proof; trade_ts may fall outside the event lifetime",
        references: "typed Chainlink BTC/USD or SOL/USD with source timestamp in [event_start - 30 seconds, event_end)",
        settlement: "gamma_api_closed_market closed-market evidence joined by exact market_id",
        availability_clock: "point-in-time rows expose the latest validated recorded or retrieved clock as available_at",
    }
}

fn validate_dataset(dataset: &NormalizedPolymarketEvidence) -> Result<()> {
    let report = &dataset.report;
    let digest = hex::encode(Sha256::digest(&dataset.ndjson));
    let rows = u64::try_from(dataset.ndjson.iter().filter(|byte| **byte == b'\n').count())?;
    if report.schema != "monday.polymarket.normalized_evidence.v1"
        || digest != report.content_sha256
        || report.content_bytes != u64::try_from(dataset.ndjson.len())?
        || report.rows != rows
        || report.rows != report.surface_counts.values().sum::<u64>()
        || report.events == 0
        || report.surface_counts.get("market_contract") != Some(&report.events)
        || report.surface_counts.get("official_settlement_evidence") != Some(&report.events)
        || report.symbols != ["BTCUSDT", "SOLUSDT"]
        || report.window_secs != 300
        || report.surface_counts.len() != SURFACES.len()
        || SURFACES.iter().any(|surface| {
            report
                .surface_counts
                .get(*surface)
                .copied()
                .unwrap_or_default()
                == 0
        })
    {
        bail!("normalized polymarket evidence identity or completeness is inconsistent");
    }
    Ok(())
}

fn manifest_bytes(report: &PolymarketEvidenceReport, file: &str) -> Result<Vec<u8>> {
    let manifest = PolymarketEvidenceManifest {
        schema: "monday.polymarket.evidence_artifact.v2",
        file,
        format: "ndjson",
        content_sha256: &report.content_sha256,
        content_bytes: report.content_bytes,
        rows: report.rows,
        events: report.events,
        surface_counts: &report.surface_counts,
        event_start_gte: &report.event_start_gte,
        event_start_lt: &report.event_start_lt,
        symbols: report.symbols,
        window_secs: report.window_secs,
        event_selection: report.event_selection,
        evidence_scope: "immutable collector evidence only; not an execution authorization or evaluator label artifact",
        content_digest_semantics: CONTENT_DIGEST_SEMANTICS,
        recording_semantics: recording_semantics(),
        trust_boundary: report.trust_boundary,
        validated_inputs: &report.validated_inputs,
    };
    let mut bytes = serde_json::to_vec(&manifest)?;
    bytes.push(b'\n');
    Ok(bytes)
}

type FileIdentity = (u64, u64, u64, u32);
type DirectoryIdentity = (u64, u64);

fn file_identity(metadata: &Metadata) -> FileIdentity {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mode() & 0o777,
    )
}

fn directory_identity(metadata: &Metadata) -> DirectoryIdentity {
    (metadata.dev(), metadata.ino())
}

fn component(path: &Path) -> Result<CString> {
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("artifact target has no file name"))?;
    component_name(name)
}

fn component_name(name: &std::ffi::OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).context("artifact path component contains NUL")
}

fn verify_bound_directory(path: &Path, directory: &File) -> Result<()> {
    let before = fs::symlink_metadata(path)?;
    let canonical = fs::canonicalize(path)?;
    let after = fs::symlink_metadata(path)?;
    let bound = directory_identity(&directory.metadata()?);
    if before.file_type().is_symlink()
        || !before.is_dir()
        || after.file_type().is_symlink()
        || !after.is_dir()
        || directory_identity(&before) != bound
        || directory_identity(&after) != bound
        || canonical != path
    {
        bail!("artifact directory identity changed: {}", path.display());
    }
    Ok(())
}

fn bind_directory(path: &Path) -> Result<File> {
    ensure_canonical_directory(path)?;
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")
        .context("bind artifact filesystem root")?;
    for part in path.components() {
        let Component::Normal(name) = part else {
            if matches!(part, Component::RootDir) {
                continue;
            }
            bail!("artifact directory must contain only absolute normal components");
        };
        let name = component_name(name)?;
        let descriptor = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("bind artifact directory {}", path.display()));
        }
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    verify_bound_directory(path, &directory)?;
    Ok(directory)
}

fn entry_identity(directory: &File, name: &CString, path: &Path) -> Result<Option<FileIdentity>> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOENT) {
            return Ok(None);
        }
        return Err(error).with_context(|| format!("inspect artifact target {}", path.display()));
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        bail!(
            "immutable artifact target is not a regular file: {}",
            path.display()
        );
    }
    // `dev_t` is signed on macOS and already `u64` on Linux. Keep the checked
    // conversion for the former without turning the latter into a CI-only lint.
    #[allow(clippy::useless_conversion)]
    let device = u64::try_from(stat.st_dev)?;
    #[allow(clippy::unnecessary_cast)]
    let mode = (stat.st_mode as u32) & 0o777;
    Ok(Some((
        device,
        stat.st_ino,
        u64::try_from(stat.st_size)?,
        mode,
    )))
}

fn read_existing(
    directory_path: &Path,
    directory: &File,
    path: &Path,
) -> Result<Option<(Vec<u8>, u32)>> {
    verify_bound_directory(directory_path, directory)?;
    let name = component(path)?;
    let Some(before) = entry_identity(directory, &name, path)? else {
        return Ok(None);
    };
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open immutable artifact {}", path.display()));
    }
    let mut file = unsafe { File::from_raw_fd(descriptor) };
    let opened = file_identity(&file.metadata()?);
    if opened != before {
        bail!(
            "immutable artifact target identity changed: {}",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if file_identity(&file.metadata()?) != opened
        || entry_identity(directory, &name, path)? != Some(opened)
    {
        bail!(
            "immutable artifact target changed while reading: {}",
            path.display()
        );
    }
    verify_bound_directory(directory_path, directory)?;
    Ok(Some((bytes, opened.3)))
}

fn exact_or_missing(
    directory_path: &Path,
    directory: &File,
    path: &Path,
    expected: &[u8],
) -> Result<bool> {
    match read_existing(directory_path, directory, path)? {
        Some((actual, _)) if actual != expected => bail!(
            "immutable artifact differs from expected bytes: {}",
            path.display()
        ),
        Some((_, PUBLISHED_MODE)) => Ok(true),
        Some((_, mode)) => bail!(
            "immutable artifact permissions are {mode:o}, expected {PUBLISHED_MODE:o}: {}",
            path.display()
        ),
        None => Ok(false),
    }
}

fn require_secure_publication_platform() -> Result<()> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        bail!("secure artifact publication requires Linux O_TMPFILE and proc-fd linking")
    }
}

#[cfg(target_os = "linux")]
fn anonymous_file(directory: &File) -> Result<File> {
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_WRONLY | libc::O_TMPFILE | libc::O_CLOEXEC,
            0o600,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error()).context("create anonymous artifact file");
    }
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(target_os = "linux")]
fn link_anonymous(directory: &File, output: &File, name: &CString) -> Result<libc::c_int> {
    let source = CString::new(format!("/proc/self/fd/{}", output.as_raw_fd()))?;
    Ok(unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    })
}

#[cfg(not(target_os = "linux"))]
// Non-Linux builds remain available for development, but publication fails closed.
fn install_no_clobber(_: &Path, _: &File, _: &Path, _: &[u8]) -> Result<()> {
    require_secure_publication_platform()
}

#[cfg(target_os = "linux")]
fn install_no_clobber(
    directory_path: &Path,
    directory: &File,
    path: &Path,
    bytes: &[u8],
) -> Result<()> {
    if exact_or_missing(directory_path, directory, path, bytes)? {
        return Ok(());
    }
    let name = component(path)?;
    verify_bound_directory(directory_path, directory)?;
    let mut output = anonymous_file(directory)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    output.set_permissions(fs::Permissions::from_mode(PUBLISHED_MODE))?;
    output.sync_all()?;
    let temporary_identity = file_identity(&output.metadata()?);
    verify_bound_directory(directory_path, directory)?;
    if link_anonymous(directory, &output, &name)? != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            if !exact_or_missing(directory_path, directory, path, bytes)? {
                bail!("immutable artifact target disappeared during publication");
            }
        } else {
            return Err(error.into());
        }
    } else if file_identity(&output.metadata()?) != temporary_identity
        || entry_identity(directory, &name, path)? != Some(temporary_identity)
    {
        bail!(
            "artifact target is not the anonymous inode: {}",
            path.display()
        );
    } else if !exact_or_missing(directory_path, directory, path, bytes)? {
        bail!("immutable artifact target disappeared after publication");
    }
    directory.sync_all()?;
    verify_bound_directory(directory_path, directory)
}

fn publish_triplet(
    data_path: &Path,
    manifest_path: &Path,
    success_path: &Path,
    bytes: &ArtifactBytes<'_>,
) -> Result<()> {
    require_secure_publication_platform()?;
    let directory_path = data_path
        .parent()
        .ok_or_else(|| anyhow!("artifact target has no parent"))?;
    if manifest_path.parent() != Some(directory_path)
        || success_path.parent() != Some(directory_path)
    {
        bail!("artifact triplet must share one directory");
    }
    let directory = bind_directory(directory_path)?;
    let data_exists = exact_or_missing(directory_path, &directory, data_path, bytes.data)?;
    let manifest_exists =
        exact_or_missing(directory_path, &directory, manifest_path, bytes.manifest)?;
    let success_exists = exact_or_missing(directory_path, &directory, success_path, bytes.success)?;
    if success_exists && (!data_exists || !manifest_exists) {
        bail!("pre-existing _SUCCESS marker is missing its immutable payload");
    }
    if success_exists {
        return Ok(());
    }
    install_no_clobber(directory_path, &directory, data_path, bytes.data)?;
    install_no_clobber(directory_path, &directory, manifest_path, bytes.manifest)?;
    let data_ready = exact_or_missing(directory_path, &directory, data_path, bytes.data)?;
    let manifest_ready =
        exact_or_missing(directory_path, &directory, manifest_path, bytes.manifest)?;
    if !data_ready || !manifest_ready {
        bail!("artifact payload disappeared before success publication");
    }
    install_no_clobber(directory_path, &directory, success_path, bytes.success)?;
    if !exact_or_missing(directory_path, &directory, success_path, bytes.success)? {
        bail!("artifact success marker disappeared after publication");
    }
    Ok(())
}

fn publish_normalized(
    output_root: &Path,
    evidence: NormalizedPolymarketEvidence,
) -> Result<PublishedPolymarketEvidence> {
    validate_dataset(&evidence)?;
    ensure_canonical_directory(output_root)?;
    let digest = &evidence.report.content_sha256;
    let directory = output_root.join(format!("sha256={digest}"));
    ensure_canonical_directory(&directory)?;
    let data_name = format!("polymarket-btc-sol-5m.{digest}.ndjson");
    let data_path = directory.join(&data_name);
    let manifest_path = directory.join(format!("{data_name}.manifest.json"));
    let success_path = directory.join(format!("{data_name}._SUCCESS"));
    let manifest = manifest_bytes(&evidence.report, &data_name)?;
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest));
    let success = format!("{digest}\n");
    publish_triplet(
        &data_path,
        &manifest_path,
        &success_path,
        &ArtifactBytes {
            data: &evidence.ndjson,
            manifest: &manifest,
            success: success.as_bytes(),
        },
    )?;
    Ok(PublishedPolymarketEvidence {
        schema: "monday.polymarket.published_evidence.v1",
        data_path,
        manifest_path,
        success_path,
        published_digests: PublishedPolymarketEvidenceDigests {
            expected_content_sha256: digest.clone(),
            expected_manifest_sha256: manifest_sha256,
        },
        evidence: evidence.report,
    })
}

pub fn publish_polymarket_evidence(
    config: &PolymarketEvidenceArtifactConfig,
) -> Result<PublishedPolymarketEvidence> {
    require_secure_publication_platform()?;
    let evidence = normalize_polymarket_evidence(&config.evidence)?;
    publish_normalized(&config.output_root, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::polymarket_research_import::{
        ResearchSegmentValidationReport, SegmentIdentity, TradeCompletionIdentity,
    };
    use std::fs;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn publication_requires_linux_anonymous_temporary_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let target = root.join("evidence.ndjson");

        let error = install_no_clobber(&root, &bind_directory(&root).unwrap(), &target, b"data\n")
            .unwrap_err();
        assert!(error.to_string().contains("requires Linux O_TMPFILE"));
        assert!(!target.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn named_temp_poison_cannot_replace_the_anonymous_inode() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let directory = bind_directory(&root).unwrap();
        let target = root.join("evidence.ndjson");
        let mut output = anonymous_file(&directory).unwrap();
        output.write_all(b"expected\n").unwrap();
        assert_eq!(output.metadata().unwrap().nlink(), 0);

        let poison = root.join(".evidence.ndjson.attacker.tmp");
        fs::write(&poison, b"attacker\n").unwrap();
        link_anonymous(&directory, &output, &component(&target).unwrap()).unwrap();
        let published = file_identity(&fs::metadata(target).unwrap());
        assert_eq!(published, file_identity(&output.metadata().unwrap()));
        assert_eq!(fs::read(poison).unwrap(), b"attacker\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn publishes_data_manifest_then_success_without_clobbering() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let data = root.join("evidence.ndjson");
        let manifest = root.join("evidence.ndjson.manifest.json");
        let success = root.join("evidence.ndjson._SUCCESS");
        let triplet = ArtifactBytes {
            data: b"{\"surface\":\"test\"}\n",
            manifest: b"{\"schema\":\"test\"}\n",
            success: b"digest\n",
        };

        publish_triplet(&data, &manifest, &success, &triplet).unwrap();
        assert_eq!(fs::read(&data).unwrap(), triplet.data);
        assert_eq!(fs::read(&manifest).unwrap(), triplet.manifest);
        assert_eq!(fs::read(&success).unwrap(), triplet.success);
        for path in [&data, &manifest, &success] {
            let mode = fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o444);
            assert_eq!(mode & 0o222, 0);
        }
        publish_triplet(&data, &manifest, &success, &triplet).unwrap();

        fs::set_permissions(&data, fs::Permissions::from_mode(0o644)).unwrap();
        let error = publish_triplet(&data, &manifest, &success, &triplet).unwrap_err();
        assert!(error.to_string().contains("permissions"));
        assert_eq!(
            fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o644
        );

        fs::write(&data, b"changed\n").unwrap();
        let error = publish_triplet(&data, &manifest, &success, &triplet).unwrap_err();
        assert!(error.to_string().contains("differs from expected bytes"));
        assert_eq!(fs::read(data).unwrap(), b"changed\n");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn published_result_returns_digests_for_the_exact_triplet() {
        fn segment(dataset: &str) -> SegmentIdentity {
            let is_reference = dataset == "crypto_expiry_reference";
            let trade_completions = (dataset == "crypto_expiry_reference")
                .then(|| {
                    BTreeMap::from([(
                        "market-1".to_owned(),
                        TradeCompletionIdentity {
                            condition_id: "condition-1".to_owned(),
                            symbol: "BTCUSDT".to_owned(),
                            market_window_secs: 300,
                            trade_count: 1,
                            trade_record_ids_sha256: "1".repeat(64),
                            completion_sequence: 1,
                            retrieved_at: "2026-07-17T05:00:01Z".to_owned(),
                            completeness_basis: crate::polymarket_upload::TRADE_COMPLETION_BASIS
                                .to_owned(),
                            finalization_lag_secs: 60,
                            stable_polls_required: 2,
                        },
                    )])
                })
                .unwrap_or_default();
            SegmentIdentity {
                schema: "monday.polymarket.raw.v1".into(),
                venue: "polymarket".into(),
                dataset: dataset.into(),
                date: "2026-07-17".into(),
                hour: "05".into(),
                file: format!("{dataset}.ndjson.zst"),
                bytes: 1,
                sha256: "0".repeat(64),
                events: if is_reference { 3 } else { 1 },
                start_sequence: 1,
                end_sequence: if is_reference { 3 } else { 1 },
                sequence_gaps: 0,
                start_recorded_at: "2026-07-17T05:00:00Z".into(),
                end_recorded_at: "2026-07-17T05:00:01Z".into(),
                source_file: format!("{dataset}.ndjson"),
                replay_scope: "fixture".into(),
                recording_policy: serde_json::json!({}),
                record_id_versions: if is_reference {
                    serde_json::json!(["v2"])
                } else {
                    serde_json::json!([])
                },
                event_types: if is_reference {
                    BTreeMap::from([
                        ("market_metadata".to_owned(), 1),
                        ("polymarket_trade".to_owned(), 1),
                        ("market_settlement".to_owned(), 1),
                    ])
                } else {
                    BTreeMap::from([("quote".to_owned(), 1)])
                },
                trade_completions,
            }
        }

        let ndjson = b"{}\n{}\n{}\n{}\n{}\n".to_vec();
        let content_sha256 = hex::encode(Sha256::digest(&ndjson));
        let evidence = NormalizedPolymarketEvidence {
            report: PolymarketEvidenceReport {
                schema: "monday.polymarket.normalized_evidence.v1",
                content_sha256: content_sha256.clone(),
                content_bytes: u64::try_from(ndjson.len()).unwrap(),
                rows: 5,
                events: 1,
                surface_counts: SURFACES
                    .into_iter()
                    .map(|surface| (surface.to_owned(), 1))
                    .collect(),
                event_start_gte: "2026-07-17T05:30:00Z".into(),
                event_start_lt: "2026-07-17T05:35:00Z".into(),
                symbols: ["BTCUSDT", "SOLUSDT"],
                window_secs: 300,
                event_selection: "event_start in [event_start_gte,event_start_lt)",
                trust_boundary: "fixture",
                validated_inputs: ResearchSegmentValidationReport {
                    schema: "monday.polymarket.research_segment_validation.v2",
                    market: segment("crypto_expiry"),
                    references: vec![segment("crypto_expiry_reference")],
                },
            },
            ndjson,
        };
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();

        let published = publish_normalized(&root, evidence).unwrap();

        assert_eq!(
            published.published_digests.expected_content_sha256,
            content_sha256
        );
        assert_eq!(
            published.published_digests.expected_manifest_sha256,
            hex::encode(Sha256::digest(fs::read(published.manifest_path).unwrap()))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refuses_to_repair_a_success_marker_missing_its_payload() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let data = root.join("evidence.ndjson");
        let manifest = root.join("evidence.ndjson.manifest.json");
        let success = root.join("evidence.ndjson._SUCCESS");
        fs::write(&success, b"digest\n").unwrap();
        fs::set_permissions(&success, fs::Permissions::from_mode(PUBLISHED_MODE)).unwrap();
        let triplet = ArtifactBytes {
            data: b"data\n",
            manifest: b"manifest\n",
            success: b"digest\n",
        };

        assert!(publish_triplet(&data, &manifest, &success, &triplet)
            .unwrap_err()
            .to_string()
            .contains("pre-existing _SUCCESS"));
        assert!(!data.exists());
        assert!(!manifest.exists());
    }

    #[test]
    fn manifest_semantics_disclose_content_digest_and_recording_limits() {
        let semantics = serde_json::to_value(recording_semantics()).unwrap();
        let orderbook = &semantics["orderbook"];
        assert_eq!(orderbook["level"], "L2");
        assert_eq!(orderbook["quote_sample_ms"], 1_000);
        assert_eq!(orderbook["temporal_updates_complete"], false);
        assert_eq!(orderbook["l3_order_ids_available"], false);
        assert_eq!(orderbook["queue_position_modeled"], false);
        assert_eq!(orderbook["endogenous_impact_modeled"], false);
        assert_eq!(orderbook["capacity_modeled"], false);
        assert!(semantics["trades"]
            .as_str()
            .unwrap()
            .contains("collector completion proof"));
        assert_eq!(
            semantics["references"],
            "typed Chainlink BTC/USD or SOL/USD with source timestamp in [event_start - 30 seconds, event_end)"
        );
        assert!(CONTENT_DIGEST_SEMANTICS.contains("not a snapshot_contract_hash"));
        assert!(!CONTENT_DIGEST_SEMANTICS.contains("snapshot_id"));
        assert!(!CONTENT_DIGEST_SEMANTICS.contains("pm_token_settlements"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bound_parent_swap_fails_before_writing_artifact_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("output");
        let moved = base.join("moved");
        fs::create_dir(&root).unwrap();
        let directory = bind_directory(&root).unwrap();
        fs::rename(&root, &moved).unwrap();
        fs::create_dir(&root).unwrap();
        let target = root.join("evidence.ndjson");

        assert!(install_no_clobber(&root, &directory, &target, b"data\n")
            .unwrap_err()
            .to_string()
            .contains("directory identity changed"));
        assert!(!target.exists());
        assert!(!moved.join("evidence.ndjson").exists());
    }
}
