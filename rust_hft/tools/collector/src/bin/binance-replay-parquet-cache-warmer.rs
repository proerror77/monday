use anyhow::{bail, Context, Result};
use clap::Parser;
use data::binance_lob_replay::source_revision;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const DATASET_KIND: &str = "backtest_canonical_replay_parquet";
const SCHEMA_VERSION: &str = "binance-replay-parquet-v1";
const FORMAT: &str = "parquet";
const PARQUET_SCHEMA: &str = "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8";
const READY_MARKER: &str = ".ready";

#[derive(Debug, Parser)]
#[command(
    name = "binance-replay-parquet-cache-warmer",
    about = "Verify and atomically publish a canonical replay Parquet cache entry"
)]
struct Args {
    /// Canonical manifest from the source mirror.
    #[arg(long)]
    manifest: PathBuf,
    /// Private local cache root (for example, a PVC or ESSD mount).
    #[arg(long)]
    cache_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CanonicalManifest {
    dataset_kind: String,
    schema_version: String,
    format: String,
    parquet_schema: String,
    mission_id: String,
    market: String,
    symbol: String,
    dataset: String,
    modalities: Vec<String>,
    source_revision: String,
    source_segments: Vec<SourceSegmentEvidence>,
    rows: usize,
    first_event_time_us: i64,
    last_event_time_us: i64,
    sequence_start: u64,
    sequence_end: u64,
    artifact_path: PathBuf,
    artifact_sha256: String,
    point_in_time: bool,
}

#[derive(Debug, Deserialize)]
struct SourceSegmentEvidence {
    file: String,
    sha256: String,
    collector_manifest_sha256: String,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Serialize)]
struct PublishedCacheEntry {
    cache_entry: PathBuf,
    manifest_path: PathBuf,
    artifact_path: PathBuf,
    ready_marker: PathBuf,
    manifest_sha256: String,
    artifact_sha256: String,
    source_revision: String,
    cache_hit: bool,
}

fn main() -> Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), &warm(&Args::parse())?)?;
    println!();
    Ok(())
}

fn warm(args: &Args) -> Result<PublishedCacheEntry> {
    let manifest_path = fs::canonicalize(&args.manifest)
        .with_context(|| format!("cannot resolve manifest {}", args.manifest.display()))?;
    let manifest_bytes = fs::read(&manifest_path)
        .with_context(|| format!("cannot read manifest {}", manifest_path.display()))?;
    let manifest_sha256 = sha256_bytes(&manifest_bytes);
    let manifest: CanonicalManifest =
        serde_json::from_slice(&manifest_bytes).context("cannot parse canonical manifest")?;
    validate_manifest(&manifest)?;

    fs::create_dir_all(&args.cache_dir)
        .with_context(|| format!("cannot create cache root {}", args.cache_dir.display()))?;
    let cache_root = fs::canonicalize(&args.cache_dir)
        .with_context(|| format!("cannot resolve cache root {}", args.cache_dir.display()))?;
    let entry = cache_root.join(&manifest_sha256);
    if entry.exists() {
        return inspect_entry(&entry, &manifest, &manifest_bytes, &manifest_sha256, true);
    }

    let source_artifact = manifest_path
        .parent()
        .context("canonical manifest has no parent directory")?
        .join(&manifest.artifact_path);
    let staging = tempfile::Builder::new()
        .prefix(".canonical-cache-")
        .tempdir_in(&cache_root)
        .context("cannot create private cache staging directory")?;
    let artifact_name = manifest
        .artifact_path
        .file_name()
        .context("canonical artifact path has no file name")?;
    let staged_artifact = staging.path().join(artifact_name);
    copy_and_sync(&source_artifact, &staged_artifact).with_context(|| {
        format!(
            "cannot stage canonical artifact {}",
            source_artifact.display()
        )
    })?;
    let actual_artifact_sha = sha256_file(&staged_artifact)?;
    if actual_artifact_sha != manifest.artifact_sha256 {
        bail!(
            "canonical artifact SHA-256 mismatch: expected {}, actual {actual_artifact_sha}",
            manifest.artifact_sha256
        );
    }
    let staged_manifest = staging.path().join("canonical-manifest.json");
    write_and_sync(&staged_manifest, &manifest_bytes)?;
    let staged_ready = staging.path().join(READY_MARKER);
    write_and_sync(&staged_ready, format!("{manifest_sha256}\n").as_bytes())?;

    match fs::rename(staging.path(), &entry) {
        Ok(()) => inspect_entry(&entry, &manifest, &manifest_bytes, &manifest_sha256, false),
        Err(error) if entry.exists() => {
            inspect_entry(&entry, &manifest, &manifest_bytes, &manifest_sha256, true)
                .with_context(|| format!("cache publication raced with existing entry: {error}"))
        }
        Err(error) => {
            Err(error).with_context(|| format!("cannot publish cache entry {}", entry.display()))
        }
    }
}

fn inspect_entry(
    entry: &Path,
    manifest: &CanonicalManifest,
    expected_manifest_bytes: &[u8],
    manifest_sha256: &str,
    cache_hit: bool,
) -> Result<PublishedCacheEntry> {
    if !entry.is_dir() {
        bail!(
            "cache entry path is not a directory; refusing overwrite: {}",
            entry.display()
        );
    }
    let ready = entry.join(READY_MARKER);
    if fs::read_to_string(&ready)
        .with_context(|| format!("cache entry is missing ready marker: {}", ready.display()))?
        .trim()
        != manifest_sha256
    {
        bail!("cache ready marker is not bound to the canonical manifest SHA");
    }
    let cached_manifest = fs::read(entry.join("canonical-manifest.json"))
        .context("cache entry is missing canonical manifest")?;
    if cached_manifest != expected_manifest_bytes
        || sha256_bytes(&cached_manifest) != manifest_sha256
    {
        bail!("cache entry canonical manifest conflicts with requested manifest");
    }
    let artifact_name = manifest
        .artifact_path
        .file_name()
        .context("canonical artifact path has no file name")?;
    let artifact = entry.join(artifact_name);
    if sha256_file(&artifact)? != manifest.artifact_sha256 {
        bail!("cache entry artifact SHA-256 mismatch; refusing consumption");
    }
    Ok(PublishedCacheEntry {
        cache_entry: entry.to_path_buf(),
        manifest_path: entry.join("canonical-manifest.json"),
        artifact_path: artifact,
        ready_marker: ready,
        manifest_sha256: manifest_sha256.to_string(),
        artifact_sha256: manifest.artifact_sha256.clone(),
        source_revision: manifest.source_revision.clone(),
        cache_hit,
    })
}

fn validate_manifest(manifest: &CanonicalManifest) -> Result<()> {
    if manifest.dataset_kind != DATASET_KIND
        || manifest.schema_version != SCHEMA_VERSION
        || manifest.format != FORMAT
        || manifest.parquet_schema != PARQUET_SCHEMA
        || manifest.mission_id.trim().is_empty()
        || manifest.market.trim().is_empty()
        || manifest.symbol.trim().is_empty()
        || manifest.dataset.trim().is_empty()
        || manifest.modalities != vec!["lob".to_string()]
        || !manifest.point_in_time
        || manifest.rows == 0
        || manifest.source_segments.is_empty()
        || manifest.first_event_time_us > manifest.last_event_time_us
        || manifest.sequence_start != 1
        || manifest.sequence_end < manifest.sequence_start
        || manifest.sequence_end - manifest.sequence_start + 1 != manifest.rows as u64
        || manifest.artifact_path.is_absolute()
        || manifest
            .artifact_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("canonical manifest is incomplete or has an unsupported schema");
    }
    valid_sha256(&manifest.artifact_sha256, "manifest.artifact_sha256")?;
    valid_sha256(&manifest.source_revision, "manifest.source_revision")?;
    let expected_artifact_name = format!("{}.parquet", manifest.artifact_sha256);
    if manifest.artifact_path != Path::new(&expected_artifact_name) {
        bail!("canonical manifest artifact path is not content addressed");
    }
    let mut source_hashes = Vec::with_capacity(manifest.source_segments.len());
    let mut unique_source_hashes = HashSet::new();
    for segment in &manifest.source_segments {
        if segment.file.trim().is_empty()
            || segment.events == 0
            || segment.start_received_at_ns > segment.end_received_at_ns
            || !unique_source_hashes.insert(&segment.sha256)
        {
            bail!("canonical manifest source segment evidence is incomplete");
        }
        valid_sha256(&segment.sha256, "source segment sha256")?;
        valid_sha256(
            &segment.collector_manifest_sha256,
            "source collector manifest sha256",
        )?;
        valid_sha256(
            &segment.success_marker_sha256,
            "source success marker sha256",
        )?;
        source_hashes.push(segment.sha256.as_str());
    }
    if source_revision(source_hashes) != manifest.source_revision {
        bail!("canonical manifest source revision does not match source segments");
    }
    Ok(())
}

fn valid_sha256(value: &str, field: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{field} must be a 64-character hex SHA-256");
    }
    Ok(())
}

fn copy_and_sync(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    Ok(())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
