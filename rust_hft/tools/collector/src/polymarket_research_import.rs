use crate::lob_archiver::command_status_with_timeout;
use crate::polymarket_upload::{
    ensure_canonical_directory, scan_tape, validate_canonical_trade, validate_market_settlement,
    TRADE_COMPLETION_KIND,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, TimeDelta, Utc};
use rand::random;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ArtifactTriplet {
    pub data: PathBuf,
    pub manifest: PathBuf,
    pub success: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ResearchSegmentValidationConfig {
    pub market: ArtifactTriplet,
    pub references: Vec<ArtifactTriplet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SegmentIdentity {
    pub schema: String,
    pub venue: String,
    pub dataset: String,
    pub date: String,
    pub hour: String,
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub events: u64,
    pub start_sequence: u64,
    pub end_sequence: u64,
    pub sequence_gaps: u64,
    pub start_recorded_at: String,
    pub end_recorded_at: String,
    pub source_file: String,
    pub replay_scope: String,
    pub recording_policy: Value,
    pub record_id_versions: Value,
    pub event_types: BTreeMap<String, u64>,
    pub trade_completions: BTreeMap<String, TradeCompletionIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradeCompletionIdentity {
    pub condition_id: String,
    pub symbol: String,
    pub market_window_secs: u64,
    pub trade_count: u64,
    pub trade_record_ids_sha256: String,
    pub completion_sequence: u64,
    pub retrieved_at: String,
    pub completeness_basis: String,
    pub finalization_lag_secs: u64,
    pub stable_polls_required: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResearchSegmentValidationReport {
    pub schema: &'static str,
    pub market: SegmentIdentity,
    pub references: Vec<SegmentIdentity>,
}

struct ValidatedArtifact {
    manifest: Value,
    identity: SegmentIdentity,
    files: BoundTriplet,
    superseded_marker: PathBuf,
}

type FileIdentity = (u64, u64, u64);

const MARKET_EVENT_TYPES: [&str; 4] = [
    "event_discovered",
    "event_expired",
    "quote",
    "reference_price",
];
const EVENT_LOCAL_MARKET_EVENT_TYPES: [&str; 5] = [
    "event_discovered",
    "event_expired",
    "quote",
    "quote_collection_failure",
    "reference_price",
];
const REFERENCE_EVENT_TYPES: [&str; 4] = [
    "market_metadata",
    "polymarket_trade",
    "market_settlement",
    TRADE_COMPLETION_KIND,
];
const MAX_REFERENCE_SEGMENTS: usize = 26;
const MAX_REFERENCE_SOURCE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SUCCESS_BYTES: u64 = 65;

fn file_identity(metadata: &Metadata) -> FileIdentity {
    (metadata.dev(), metadata.ino(), metadata.len())
}

struct BoundFile {
    path: PathBuf,
    file: File,
    snapshot: File,
    identity: FileIdentity,
    sha256: String,
}

impl BoundFile {
    fn open(path: &Path, snapshot_path: &Path, max_bytes: u64) -> Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
        {
            bail!("artifact path must be absolute and canonical");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .context("open artifact without following links")?;
        let identity = file_identity(&file.metadata()?);
        let current = fs::symlink_metadata(path)?;
        if !current.is_file()
            || current.file_type().is_symlink()
            || file_identity(&current) != identity
            || fs::canonicalize(path)? != path
        {
            bail!("artifact path does not identify the opened regular file");
        }
        let mut snapshot = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(snapshot_path)?;
        if identity.2 > max_bytes {
            bail!("artifact exceeds snapshot byte limit");
        }
        let mut bounded = file.try_clone()?.take(
            max_bytes
                .checked_add(1)
                .context("snapshot byte limit overflow")?,
        );
        if std::io::copy(&mut bounded, &mut snapshot)? > max_bytes {
            bail!("artifact exceeds snapshot byte limit");
        }
        let sha256 = Self::hash(&snapshot)?;
        Ok(Self {
            path: path.to_owned(),
            file,
            snapshot,
            identity,
            sha256,
        })
    }

    fn read(&self) -> Result<Vec<u8>> {
        let mut file = self.snapshot.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    fn hash(source: &File) -> Result<String> {
        let mut file = source.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
        Ok(hex::encode(digest.finalize()))
    }

    fn verify(&self) -> Result<()> {
        let current = fs::symlink_metadata(&self.path)?;
        if file_identity(&self.file.metadata()?) != self.identity
            || !current.is_file()
            || current.file_type().is_symlink()
            || file_identity(&current) != self.identity
            || fs::canonicalize(&self.path)? != self.path
            || Self::hash(&self.file)? != self.sha256
            || Self::hash(&self.snapshot)? != self.sha256
        {
            bail!("artifact content or path identity changed during validation");
        }
        Ok(())
    }
}

struct BoundTriplet {
    data: BoundFile,
    manifest: BoundFile,
    success: BoundFile,
}

impl BoundTriplet {
    fn verify(&self) -> Result<()> {
        self.data.verify()?;
        self.manifest.verify()?;
        self.success.verify()
    }
}

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn create() -> Result<Self> {
        let parent = fs::canonicalize(std::env::temp_dir())?;
        ensure_canonical_directory(&parent)?;
        for _ in 0..32 {
            let path = parent.join(format!("monday-research-validate.{:016x}", random::<u64>()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!("could not allocate validation staging directory")
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn partition<'a>(path: &'a Path, prefix: &str) -> Result<&'a str> {
    path.file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or_else(|| anyhow!("content-addressed path requires {prefix}<value>"))
}

fn reject_superseded(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => bail!("adjacent SUPERSEDED marker rejects this research input"),
        Err(error) => Err(error.into()),
    }
}

fn text(manifest: &Value, field: &str) -> Result<String> {
    manifest
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("manifest {field} must be a string"))
}

fn has_only_event_types(manifest: &Value, allowed: &[&str]) -> bool {
    manifest["event_types"]
        .as_object()
        .is_some_and(|types| types.keys().all(|kind| allowed.contains(&kind.as_str())))
}
fn validate_triplet(
    triplet: &ArtifactTriplet,
    dataset: &str,
    scratch_name: &str,
    scratch: &Path,
    max_data_bytes: u64,
    require_complete: bool,
) -> Result<ValidatedArtifact> {
    let sha_dir = triplet
        .data
        .parent()
        .ok_or_else(|| anyhow!("data path has no parent"))?;
    if triplet.manifest.parent() != Some(sha_dir) || triplet.success.parent() != Some(sha_dir) {
        bail!("artifact triplet must share one content-addressed directory");
    }
    let data_name = triplet
        .data
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("data file name is not UTF-8"))?;
    if triplet
        .manifest
        .file_name()
        .and_then(|value| value.to_str())
        != Some(&format!("{data_name}.manifest.json"))
        || triplet.success.file_name().and_then(|value| value.to_str())
            != Some(&format!("{data_name}._SUCCESS"))
    {
        bail!("manifest and _SUCCESS names must derive from the data file");
    }
    let superseded_marker = sha_dir.join(format!("{data_name}.SUPERSEDED.json"));
    reject_superseded(&superseded_marker)?;
    let files = BoundTriplet {
        data: BoundFile::open(
            &triplet.data,
            &scratch.join(format!("{scratch_name}.data")),
            max_data_bytes,
        )?,
        manifest: BoundFile::open(
            &triplet.manifest,
            &scratch.join(format!("{scratch_name}.manifest")),
            MAX_MANIFEST_BYTES,
        )?,
        success: BoundFile::open(
            &triplet.success,
            &scratch.join(format!("{scratch_name}.success")),
            MAX_SUCCESS_BYTES,
        )?,
    };
    let digest = partition(sha_dir, "sha256=")?;
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("sha256 path digest must be 64 lowercase hex characters");
    }
    let hour_dir = sha_dir
        .parent()
        .ok_or_else(|| anyhow!("missing hour partition"))?;
    let date_dir = hour_dir
        .parent()
        .ok_or_else(|| anyhow!("missing date partition"))?;
    let dataset_dir = date_dir
        .parent()
        .ok_or_else(|| anyhow!("missing dataset partition"))?;
    let venue_dir = dataset_dir
        .parent()
        .ok_or_else(|| anyhow!("missing venue partition"))?;
    let raw_dir = venue_dir
        .parent()
        .ok_or_else(|| anyhow!("missing raw lake partition"))?;
    let lake_dir = raw_dir
        .parent()
        .ok_or_else(|| anyhow!("missing lake partition"))?;
    let hour = partition(hour_dir, "hour=")?;
    let date = partition(date_dir, "date=")?;
    if partition(dataset_dir, "dataset=")? != dataset
        || partition(venue_dir, "venue=")? != "polymarket"
        || raw_dir.file_name().and_then(|value| value.to_str()) != Some("raw")
        || lake_dir.file_name().and_then(|value| value.to_str()) != Some("lake")
    {
        bail!("content-addressed lake/raw/venue/dataset suffix is wrong");
    }
    let manifest: Value = serde_json::from_slice(&files.manifest.read()?)?;
    for (field, expected) in [
        ("schema", "monday.polymarket.raw.v1"),
        ("venue", "polymarket"),
        ("dataset", dataset),
        ("format", "ndjson.zst"),
    ] {
        if manifest.get(field).and_then(Value::as_str) != Some(expected) {
            bail!("manifest {field} must be {expected}");
        }
    }
    if manifest
        .get("source_session_closed")
        .and_then(Value::as_bool)
        != Some(true)
        || (require_complete
            && (manifest.get("canonical").and_then(Value::as_bool) != Some(true)
                || manifest.get("segment_complete").and_then(Value::as_bool) != Some(true)))
    {
        bail!("manifest must be canonical, segment-complete, and source-closed");
    }
    if manifest.get("superseded").and_then(Value::as_bool) == Some(true)
        || manifest
            .get("superseded_by")
            .is_some_and(|value| !value.is_null())
    {
        bail!("superseded segments are not valid research inputs");
    }
    let bytes = files.data.identity.2;
    if manifest.get("date").and_then(Value::as_str) != Some(date)
        || manifest.get("hour").and_then(Value::as_str) != Some(hour)
        || manifest.get("file").and_then(Value::as_str) != Some(data_name)
        || manifest.get("bytes").and_then(Value::as_u64) != Some(bytes)
        || manifest.get("sha256").and_then(Value::as_str) != Some(digest)
        || manifest.get("sequence_gaps").and_then(Value::as_u64) != Some(0)
    {
        bail!("manifest source identity or sequence declaration does not match staged path");
    }
    let start_recorded_at = text(&manifest, "start_recorded_at")?;
    let end_recorded_at = text(&manifest, "end_recorded_at")?;
    let partition = |value: &str| -> Result<String> {
        Ok(DateTime::parse_from_rfc3339(value)?
            .with_timezone(&Utc)
            .format("%Y-%m-%dT%H")
            .to_string())
    };
    if partition(&start_recorded_at)? != format!("{date}T{hour}")
        || partition(&end_recorded_at)? != format!("{date}T{hour}")
    {
        bail!("segment crosses its UTC-hour partition");
    }
    let start_sequence = manifest
        .get("start_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("manifest start_sequence is missing"))?;
    let end_sequence = manifest
        .get("end_sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("manifest end_sequence is missing"))?;
    let events = manifest
        .get("events")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("manifest events is missing"))?;
    if end_sequence < start_sequence || events != end_sequence - start_sequence + 1 {
        bail!("manifest sequence bounds do not match event count");
    }
    if files.data.sha256 != digest {
        bail!("data SHA-256 does not match sha256= path");
    }
    if files.success.read()? != format!("{digest}\n").as_bytes() {
        bail!("_SUCCESS must contain the exact data digest and newline");
    }
    let trade_completions = manifest
        .get("trade_completions")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("parse manifest event-local trade completions")?
        .unwrap_or_default();
    let event_types = serde_json::from_value(
        manifest
            .get("event_types")
            .cloned()
            .ok_or_else(|| anyhow!("manifest event_types is missing"))?,
    )
    .context("parse manifest event types")?;
    Ok(ValidatedArtifact {
        identity: SegmentIdentity {
            schema: text(&manifest, "schema")?,
            venue: text(&manifest, "venue")?,
            dataset: text(&manifest, "dataset")?,
            date: date.to_owned(),
            hour: hour.to_owned(),
            file: data_name.to_owned(),
            bytes,
            sha256: digest.to_owned(),
            events,
            start_sequence,
            end_sequence,
            sequence_gaps: 0,
            start_recorded_at,
            end_recorded_at,
            source_file: text(&manifest, "source_file")?,
            replay_scope: text(&manifest, "replay_scope")?,
            recording_policy: manifest["recording_policy"].clone(),
            record_id_versions: manifest["record_id_versions"].clone(),
            event_types,
            trade_completions,
        },
        manifest,
        files,
        superseded_marker,
    })
}

fn decompress_and_rescan(
    artifact: &ValidatedArtifact,
    dataset: &str,
    directory: &Path,
) -> Result<(PathBuf, Value)> {
    fs::create_dir(directory)?;
    let source_file = artifact
        .manifest
        .get("source_file")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("manifest source_file is missing"))?;
    let expected_source = artifact
        .identity
        .file
        .strip_suffix(".zst")
        .ok_or_else(|| anyhow!("compressed data file must end in .zst"))?;
    if source_file != expected_source || Path::new(source_file).components().count() != 1 {
        bail!("manifest source_file does not match compressed data name");
    }
    let raw = directory.join(source_file);
    let output = OpenOptions::new().write(true).create_new(true).open(&raw)?;
    let mut input = artifact.files.data.snapshot.try_clone()?;
    input.seek(SeekFrom::Start(0))?;
    let mut command = Command::new("zstd");
    command
        .args(["-q", "-d", "-c"])
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output.try_clone()?));
    let status = command_status_with_timeout(&mut command, Duration::from_secs(300))
        .context("run zstd decompression")?;
    if !status.success() {
        bail!("zstd decompression failed with {status}");
    }
    output.sync_all()?;
    if artifact
        .manifest
        .get("source_bytes")
        .and_then(Value::as_u64)
        != Some(fs::metadata(&raw)?.len())
    {
        bail!("decompressed source bytes do not match manifest");
    }
    let policy = &artifact.manifest["recording_policy"];
    let depth = policy
        .get("quote_depth_levels")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("recording_policy.quote_depth_levels is missing"))?;
    let sample_ms = policy
        .get("quote_sample_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("recording_policy.quote_sample_ms is missing"))?;
    let rescanned = scan_tape(&raw, dataset, usize::try_from(depth)?, sample_ms)?;
    for (field, value) in rescanned.as_object().expect("scan manifest is an object") {
        let authenticated_legacy_field = artifact.manifest.get(field).is_none()
            && match field.as_str() {
                "reference_context_complete" => value.as_bool() == Some(true),
                "trade_completions" => value.is_object(),
                _ => false,
            };
        if !authenticated_legacy_field && artifact.manifest.get(field) != Some(value) {
            bail!(
                "manifest {} field {field} does not match producer rescan: producer={}; rescan={}",
                artifact.identity.file,
                bounded_rescan_value(artifact.manifest.get(field)),
                bounded_rescan_value(Some(value)),
            );
        }
    }
    Ok((raw, rescanned))
}

const MAX_RESCAN_DIAGNOSTIC_CHARS: usize = 512;

fn bounded_rescan_value(value: Option<&Value>) -> String {
    let raw = value.map_or_else(|| "<missing>".to_owned(), Value::to_string);
    let mut chars = raw.chars();
    let prefix = chars
        .by_ref()
        .take(MAX_RESCAN_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}...<truncated>")
    } else {
        raw
    }
}

fn refresh_trade_completions_from_rescan(
    identity: &mut SegmentIdentity,
    rescanned: &Value,
) -> Result<()> {
    identity.trade_completions = serde_json::from_value(
        rescanned
            .get("trade_completions")
            .cloned()
            .ok_or_else(|| anyhow!("producer rescan trade_completions is missing"))?,
    )
    .context("parse producer rescan event-local trade completions")?;
    Ok(())
}

fn segment_hour(identity: &SegmentIdentity) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&format!("{}T{}:00:00Z", identity.date, identity.hour))
        .map(|value| value.with_timezone(&Utc))
        .context("invalid reference date/hour")
}

fn semantic_digest<'a>(values: impl IntoIterator<Item = Option<&'a Value>>) -> Result<[u8; 32]> {
    let values = values.into_iter().collect::<Vec<_>>();
    Ok(Sha256::digest(serde_json::to_vec(&values)?).into())
}

fn trade_merge_identity(update: &Value, line: usize) -> Result<(String, [u8; 32])> {
    let update = update
        .as_object()
        .ok_or_else(|| anyhow!("line {line}: polymarket_trade update must be an object"))?;
    let record_id = validate_canonical_trade(update, line)?;
    let fields = [
        "record_id_version",
        "market_id",
        "condition_id",
        "token_id",
        "symbol",
        "market_window_secs",
        "outcome",
        "source",
    ];
    Ok((
        record_id,
        semantic_digest(fields.into_iter().map(|field| update.get(field)))?,
    ))
}

fn settlement_merge_identity(update: &Value, line: usize) -> Result<(String, [u8; 32])> {
    let update = update
        .as_object()
        .ok_or_else(|| anyhow!("line {line}: market_settlement update must be an object"))?;
    validate_market_settlement(update, line)?;
    let market_id = update
        .get("market_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("line {line}: market_settlement requires market_id"))?
        .to_owned();
    let market = update
        .get("market")
        .and_then(Value::as_object)
        .expect("settlement validator checked market");
    let fields = [
        "condition_id",
        "symbol",
        "market_window_secs",
        "winning_token_id",
        "winning_outcome",
        "resolved_up_won",
        "resolution_source",
    ];
    let identity = semantic_digest(
        fields.into_iter().map(|field| update.get(field)).chain(
            ["conditionId", "clobTokenIds", "outcomes", "outcomePrices"]
                .into_iter()
                .map(|field| market.get(field)),
        ),
    );
    Ok((market_id, identity?))
}

fn combine_references(paths: &[PathBuf], scratch: &Path) -> Result<PathBuf> {
    let combined = scratch.join("references.ndjson");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&combined)?;
    let mut trade_ids = BTreeMap::<String, (usize, [u8; 32])>::new();
    let mut settlements = BTreeMap::<String, (usize, [u8; 32])>::new();
    for (segment, path) in paths.iter().enumerate() {
        for (index, line) in BufReader::new(File::open(path)?).lines().enumerate() {
            let line = line?;
            let line_number = index + 1;
            let row: Value = serde_json::from_str(&line)?;
            let update = &row["update"];
            let mut duplicate = false;
            if update["kind"] == "polymarket_trade" {
                let (record_id, identity) = trade_merge_identity(update, line_number)?;
                match trade_ids.get(&record_id) {
                    Some((existing_segment, _)) if *existing_segment == segment => {
                        bail!("duplicate polymarket_trade record_id within reference segment")
                    }
                    Some((_, existing)) if *existing != identity => {
                        bail!("conflicting polymarket_trade record_id across reference segments")
                    }
                    Some(_) => duplicate = true,
                    None => {
                        trade_ids.insert(record_id, (segment, identity));
                    }
                }
            } else if update["kind"] == "market_settlement" {
                let (market_id, identity) = settlement_merge_identity(update, line_number)?;
                match settlements.get(&market_id) {
                    Some((existing_segment, _)) if *existing_segment == segment => {
                        bail!("duplicate market_settlement within reference segment")
                    }
                    Some((_, existing)) if *existing != identity => {
                        bail!("conflicting market_settlement across reference segments")
                    }
                    Some(_) => duplicate = true,
                    None => {
                        settlements.insert(market_id, (segment, identity));
                    }
                }
            }
            if !duplicate {
                output.write_all(line.as_bytes())?;
                output.write_all(b"\n")?;
            }
        }
    }
    output.sync_all()?;
    Ok(combined)
}
fn validate_market_policy(
    manifest: &Value,
    allow_disclosed_quote_collection_failures: bool,
) -> Result<()> {
    let allowed_event_types = if allow_disclosed_quote_collection_failures {
        &EVENT_LOCAL_MARKET_EVENT_TYPES[..]
    } else {
        &MARKET_EVENT_TYPES[..]
    };
    if manifest
        .get("venue_depth_complete")
        .and_then(Value::as_bool)
        != Some(true)
        || manifest
            .get("event_context_complete")
            .and_then(Value::as_bool)
            != Some(true)
        || manifest
            .get("quote_quality_complete")
            .and_then(Value::as_bool)
            != Some(true)
        || manifest["recording_policy"]["quote_depth_levels"].as_u64() != Some(0)
        || manifest["recording_policy"]["quote_sample_ms"].as_u64() != Some(1_000)
        || manifest["recording_policy"]["event_scoped_quotes"].as_bool() != Some(true)
        || !has_only_event_types(manifest, allowed_event_types)
        || manifest["event_types"]["quote"]
            .as_u64()
            .unwrap_or_default()
            == 0
        || manifest["event_types"]["reference_price"]
            .as_u64()
            .unwrap_or_default()
            == 0
    {
        bail!("primary market segment must contain only event-scoped full visible L2 records");
    }
    Ok(())
}
fn validate_reference_policy(references: &[Value]) -> Result<()> {
    let mut event_types = BTreeMap::<String, u64>::new();
    for manifest in references {
        let versions = &manifest["record_id_versions"];
        if manifest
            .get("reference_context_complete")
            .and_then(Value::as_bool)
            != Some(true)
            || (versions != &Value::from(Vec::<String>::new())
                && versions != &Value::from(vec!["v2"]))
            || manifest["recording_policy"]["quote_depth_levels"].as_u64() != Some(0)
            || manifest["recording_policy"]["quote_sample_ms"].as_u64() != Some(0)
            || manifest["recording_policy"]["event_scoped_quotes"].as_bool() != Some(true)
            || !has_only_event_types(manifest, &REFERENCE_EVENT_TYPES)
        {
            bail!("reference segments must contain only metadata, v2 trades, settlements, and exact recording policy");
        }
        for (kind, count) in manifest["event_types"]
            .as_object()
            .expect("validated event types")
        {
            let total = event_types.entry(kind.clone()).or_default();
            *total = total
                .checked_add(count.as_u64().unwrap_or_default())
                .context("reference event count overflow")?;
        }
    }
    if ["market_metadata", "polymarket_trade", "market_settlement"]
        .iter()
        .any(|kind| event_types.get(*kind).copied().unwrap_or_default() == 0)
    {
        bail!("reference segment set requires metadata, v2 trades, and settlements");
    }
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferenceHourPolicy {
    Consecutive,
    Nondecreasing,
}

fn with_validated_research_segments_policy<T>(
    config: &ResearchSegmentValidationConfig,
    hour_policy: ReferenceHourPolicy,
    allow_event_local_market_recovery: bool,
    consume: impl FnOnce(&ResearchSegmentValidationReport, &Path, &Path) -> Result<T>,
) -> Result<T> {
    if config.references.is_empty() || config.references.len() > MAX_REFERENCE_SEGMENTS {
        bail!("reference segment count must be between 1 and {MAX_REFERENCE_SEGMENTS}");
    }
    let scratch = ScratchDir::create()?;
    let market = validate_triplet(
        &config.market,
        "crypto_expiry",
        "market",
        &scratch.0,
        MAX_REFERENCE_SOURCE_BYTES,
        !allow_event_local_market_recovery,
    )?;
    let mut references = Vec::<ValidatedArtifact>::with_capacity(config.references.len());
    let mut remaining_archive_bytes = MAX_REFERENCE_SOURCE_BYTES;
    for (index, triplet) in config.references.iter().enumerate() {
        let reference = validate_triplet(
            triplet,
            "crypto_expiry_reference",
            &format!("reference-{index}"),
            &scratch.0,
            remaining_archive_bytes,
            true,
        )?;
        remaining_archive_bytes = remaining_archive_bytes
            .checked_sub(reference.identity.bytes)
            .context("reference archive byte total overflow")?;
        if let Some(previous) = references.last() {
            let previous = segment_hour(&previous.identity)?;
            let current = segment_hour(&reference.identity)?;
            if current < previous
                || (hour_policy == ReferenceHourPolicy::Consecutive
                    && current > previous + TimeDelta::hours(1))
            {
                bail!("reference segments must be same or consecutive UTC hours");
            }
        }
        references.push(reference);
    }
    references.sort_by_key(|reference| {
        DateTime::parse_from_rfc3339(&reference.identity.start_recorded_at)
            .expect("validated reference start_recorded_at")
            .with_timezone(&Utc)
    });
    let source_bytes = references.iter().try_fold(0_u64, |total, reference| {
        total
            .checked_add(
                reference.manifest["source_bytes"]
                    .as_u64()
                    .unwrap_or(u64::MAX),
            )
            .context("reference source byte total overflow")
    })?;
    if source_bytes > MAX_REFERENCE_SOURCE_BYTES {
        bail!("reference segment set exceeds source byte limit");
    }
    validate_market_policy(&market.manifest, allow_event_local_market_recovery)?;
    let (market_raw, market_manifest) =
        decompress_and_rescan(&market, "crypto_expiry", &scratch.0.join("market"))?;
    let mut market_identity = market.identity.clone();
    refresh_trade_completions_from_rescan(&mut market_identity, &market_manifest)?;
    let rescanned_references = references
        .iter()
        .enumerate()
        .map(|(index, reference)| {
            decompress_and_rescan(
                reference,
                "crypto_expiry_reference",
                &scratch.0.join(format!("reference-{index}")),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let reference_manifests = rescanned_references
        .iter()
        .map(|(_, manifest)| manifest.clone())
        .collect::<Vec<_>>();
    validate_reference_policy(&reference_manifests)?;
    let reference_raws = rescanned_references
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let reference_raw = combine_references(&reference_raws, &scratch.0)?;
    let reference_identities = references
        .iter()
        .zip(rescanned_references.iter())
        .map(|(reference, (_, manifest))| {
            let mut identity = reference.identity.clone();
            refresh_trade_completions_from_rescan(&mut identity, manifest)?;
            Ok(identity)
        })
        .collect::<Result<Vec<_>>>()?;
    let report = ResearchSegmentValidationReport {
        schema: "monday.polymarket.research_segment_validation.v2",
        market: market_identity,
        references: reference_identities,
    };
    let result = consume(&report, &market_raw, &reference_raw)?;
    market.files.verify()?;
    reject_superseded(&market.superseded_marker)?;
    for artifact in &references {
        artifact.files.verify()?;
        reject_superseded(&artifact.superseded_marker)?;
    }
    Ok(result)
}

pub(crate) fn with_validated_research_segments<T>(
    config: &ResearchSegmentValidationConfig,
    consume: impl FnOnce(&ResearchSegmentValidationReport, &Path, &Path) -> Result<T>,
) -> Result<T> {
    with_validated_research_segments_policy(
        config,
        ReferenceHourPolicy::Consecutive,
        false,
        consume,
    )
}

pub(crate) fn with_event_local_validated_research_segments<T>(
    config: &ResearchSegmentValidationConfig,
    consume: impl FnOnce(&ResearchSegmentValidationReport, &Path, &Path) -> Result<T>,
) -> Result<T> {
    with_validated_research_segments_policy(
        config,
        ReferenceHourPolicy::Nondecreasing,
        true,
        consume,
    )
}

pub fn validate_research_segments(
    config: &ResearchSegmentValidationConfig,
) -> Result<ResearchSegmentValidationReport> {
    with_validated_research_segments(config, |report, _, _| Ok(report.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lob_archiver::sha256_file;
    use crate::polymarket_upload::{derived_trade_record_id, trade_record_ids_sha256};
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::fs::File;
    use std::io::Write;
    fn row(sequence: u64, update: Value) -> Value {
        json!({"sequence": sequence, "recorded_at": "2026-07-17T05:01:00Z", "update": update})
    }
    fn metadata(kind: &str) -> Value {
        let mut value = json!({
            "kind": kind, "market_id": "market-1", "condition_id": "0xcondition",
            "symbol": "BTCUSDT", "market_window_secs": 300, "source": "gamma_api",
            "retrieved_at": "2026-07-17T05:01:00Z",
            "market": {
                "id": "market-1", "conditionId": "0xcondition",
                "question": "Bitcoin Up or Down - 5 minutes", "slug": "btc-updown-5m-test",
                "startDate": "2026-07-17T05:00:00Z", "endDate": "2026-07-17T05:05:00Z",
                "resolutionSource": "https://data.chain.link/streams/btc-usd",
                "clobTokenIds": "[\"up-token\",\"down-token\"]",
                "outcomes": "[\"Up\",\"Down\"]", "makerBaseFee": 1000, "takerBaseFee": 1000,
            },
        });
        if kind == "market_settlement" {
            value["winning_token_id"] = json!("up-token");
            value["winning_outcome"] = json!("Up");
            value["resolved_up_won"] = json!(true);
            value["resolution_source"] = json!("gamma_api_closed_market");
            value["market"]["closed"] = json!(true);
            value["market"]["outcomePrices"] = json!("[\"0.999\",\"0.001\"]");
        }
        value
    }

    fn trade() -> Value {
        let raw = json!({
            "transactionHash": "0xtx", "conditionId": "0xcondition", "asset": "up-token",
            "side": "BUY", "timestamp": 1_784_084_995_i64, "proxyWallet": "0xwallet",
            "size": "10.0", "price": "0.780", "outcome": "Up", "outcomeIndex": 0,
        });
        let raw_object = raw.as_object().unwrap();
        json!({
            "kind": "polymarket_trade", "record_id": derived_trade_record_id(raw_object),
            "record_id_version": "v2", "market_id": "market-1", "condition_id": "0xcondition",
            "token_id": "up-token", "symbol": "BTCUSDT", "market_window_secs": 300,
            "side": "BUY", "size": "10.0", "price": "0.780",
            "trade_ts": "2026-07-15T03:09:55Z", "trade_ts_unix": 1_784_084_995_i64,
            "transaction_hash": "0xtx", "proxy_wallet": "0xwallet", "outcome": "Up",
            "outcome_index": 0, "source": "polymarket_data_api",
            "received_at": "2026-07-17T05:01:00Z", "trade": raw,
        })
    }

    fn later_trade() -> Value {
        let mut value = trade();
        value["trade"]["transactionHash"] = json!("0xlater");
        value["transaction_hash"] = json!("0xlater");
        value["record_id"] = json!(derived_trade_record_id(value["trade"].as_object().unwrap()));
        value
    }

    fn trade_completion_for(trades: &[Value]) -> Value {
        let mut record_ids = trades
            .iter()
            .map(|trade| trade["record_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        record_ids.sort();
        json!({
            "kind": TRADE_COMPLETION_KIND,
            "market_id": "market-1",
            "condition_id": "0xcondition",
            "symbol": "BTCUSDT",
            "market_window_secs": 300,
            "record_id_version": "v2",
            "trade_count": record_ids.len(),
            "trade_record_ids_sha256": trade_record_ids_sha256(record_ids.iter().map(String::as_str)),
            "source": "polymarket_data_api",
            "retrieved_at": "2026-07-17T05:01:00Z",
            "completeness_basis": crate::polymarket_upload::TRADE_COMPLETION_BASIS,
            "pagination_exhausted": true,
            "settlement_observed": true,
            "malformed_trade_rows": 0,
            "finalization_lag_secs": 60,
            "stable_polls_required": 2,
        })
    }

    fn trade_completion() -> Value {
        trade_completion_for(&[trade()])
    }

    fn market_rows() -> Vec<Value> {
        vec![
            row(
                0,
                json!({
                    "kind": "event_discovered", "event_id": "market-1", "symbol": "BTCUSDT",
                    "up_token": "up-token", "down_token": "down-token",
                    "end_time": "2026-07-17T05:05:00Z", "window_secs": 300,
                    "price_to_beat": "100", "resolved_up_won": null,
                }),
            ),
            row(
                1,
                json!({
                    "kind": "quote", "token_id": "up-token", "bid": "0.49", "ask": "0.51",
                    "bid_size": "10", "ask_size": "11",
                    "request_status": "success", "collection_result": "executable",
                    "bid_levels": [{"price": "0.49", "size": "10"}],
                    "ask_levels": [{"price": "0.51", "size": "11"}], "ts": "2026-07-17T05:00:59Z",
                }),
            ),
            row(
                2,
                json!({
                    "kind": "quote", "token_id": "down-token", "bid": "0.48", "ask": "0.52",
                    "bid_size": "12", "ask_size": "13",
                    "request_status": "success", "collection_result": "executable",
                    "bid_levels": [{"price": "0.48", "size": "12"}],
                    "ask_levels": [{"price": "0.52", "size": "13"}], "ts": "2026-07-17T05:00:59Z",
                }),
            ),
            row(
                3,
                json!({
                    "kind": "reference_price", "symbol": "btc/usd", "source": "chainlink",
                    "asset_class": "crypto", "price": "100", "full_accuracy_value": null,
                    "is_carried_forward": false, "ts": "2026-07-17T05:00:59Z",
                }),
            ),
        ]
    }

    #[rustfmt::skip]
    fn reference_rows(settlement: bool) -> Vec<Value> {
        let mut rows = vec![row(0, metadata("market_metadata")), row(1, trade())];
        if settlement {
            rows.push(row(2, metadata("market_settlement")));
        }
        rows
    }

    fn triplet(root: &Path, dataset: &str, rows: &[Value]) -> ArtifactTriplet {
        let raw_name = format!("market-updates.{dataset}.ndjson");
        let raw = root.join(&raw_name);
        let mut file = File::create(&raw).unwrap();
        for value in rows {
            serde_json::to_writer(&mut file, value).unwrap();
            file.write_all(b"\n").unwrap();
        }
        file.sync_all().unwrap();
        let sample_ms = if dataset == "crypto_expiry" { 1_000 } else { 0 };
        let mut manifest = scan_tape(&raw, dataset, 0, sample_ms).unwrap();
        let compressed = root.join(format!("{raw_name}.zst"));
        let output = File::create(&compressed).unwrap();
        assert!(Command::new("zstd")
            .args(["-q", "-3", "-c"])
            .arg(&raw)
            .stdout(Stdio::from(output))
            .status()
            .unwrap()
            .success());
        let digest = sha256_file(&compressed).unwrap();
        let directory = root
            .join("lake/raw/venue=polymarket")
            .join(format!("dataset={dataset}"))
            .join(format!(
                "date={}/hour={}",
                manifest["date"].as_str().unwrap(),
                manifest["hour"].as_str().unwrap()
            ))
            .join(format!("sha256={digest}"));
        fs::create_dir_all(&directory).unwrap();
        let data = directory.join(compressed.file_name().unwrap());
        fs::rename(compressed, &data).unwrap();
        manifest["file"] = json!(data.file_name().unwrap().to_str().unwrap());
        manifest["bytes"] = json!(fs::metadata(&data).unwrap().len());
        manifest["sha256"] = json!(digest);
        let manifest_path = directory.join(format!(
            "{}.manifest.json",
            manifest["file"].as_str().unwrap()
        ));
        fs::write(
            &manifest_path,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        let success = directory.join(format!("{}._SUCCESS", manifest["file"].as_str().unwrap()));
        fs::write(
            &success,
            format!("{}\n", manifest["sha256"].as_str().unwrap()),
        )
        .unwrap();
        ArtifactTriplet {
            data,
            manifest: manifest_path,
            success,
        }
    }

    fn fixture_rows(
        market_rows: &[Value],
        reference_rows: &[Value],
    ) -> (tempfile::TempDir, ResearchSegmentValidationConfig) {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", market_rows);
        let reference = triplet(&root, "crypto_expiry_reference", reference_rows);
        (
            temp,
            ResearchSegmentValidationConfig {
                market,
                references: vec![reference],
            },
        )
    }

    fn fixture(settlement: bool) -> (tempfile::TempDir, ResearchSegmentValidationConfig) {
        fixture_rows(&market_rows(), &reference_rows(settlement))
    }

    fn explicit_evidence_fixture(
        market_rows: &[Value],
    ) -> (
        tempfile::TempDir,
        crate::polymarket_research_normalize::PolymarketEvidenceConfig,
    ) {
        let mut reference = reference_rows(true);
        reference.push(row(3, trade_completion()));
        let (temp, segments) = fixture_rows(market_rows, &reference);
        (
            temp,
            crate::polymarket_research_normalize::PolymarketEvidenceConfig {
                segments,
                event_start_gte: "2026-07-17T05:00:00Z".to_owned(),
                event_start_lt: "2026-07-17T05:05:00Z".to_owned(),
                market_ids: vec!["market-1".to_owned()],
            },
        )
    }

    fn quote_failure(sequence: u64) -> Value {
        let mut failure = row(
            sequence,
            json!({
                "kind": "quote_collection_failure",
                "token_id": "up-token",
                "request_status": "failure",
                "collection_result": "api_failure",
                "request_started_at": "2026-07-17T05:01:00.900Z",
                "http_status": null,
                "error_kind": "websocket_payload",
                "ts": "2026-07-17T05:01:01Z",
            }),
        );
        failure["recorded_at"] = json!("2026-07-17T05:01:01Z");
        failure
    }

    fn quote_at(sequence: u64, token: &str, recorded_at: &str, source_at: &str) -> Value {
        let mut quote = market_rows()[1].clone();
        quote["sequence"] = json!(sequence);
        quote["recorded_at"] = json!(recorded_at);
        quote["update"]["token_id"] = json!(token);
        quote["update"]["ts"] = json!(source_at);
        quote
    }

    fn explicit_normalization_error(market_rows: &[Value]) -> String {
        let (_temp, config) = explicit_evidence_fixture(market_rows);
        crate::polymarket_research_normalize::normalize_polymarket_evidence(&config)
            .unwrap_err()
            .to_string()
    }

    fn explicit_manifest_error(mutate: impl FnOnce(&mut Value)) -> String {
        let (_temp, config) = explicit_evidence_fixture(&market_rows());
        let path = &config.segments.market.manifest;
        let mut manifest: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        mutate(&mut manifest);
        fs::write(
            path,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        crate::polymarket_research_normalize::normalize_polymarket_evidence(&config)
            .unwrap_err()
            .to_string()
    }

    fn rejects(config: &ResearchSegmentValidationConfig, expected: &str) {
        let error = validate_research_segments(config).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }

    #[test]
    fn bound_file_validates_the_captured_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let path = root.join("artifact");
        fs::write(&path, b"before").unwrap();
        let bound = BoundFile::open(&path, &root.join("snapshot"), 6).unwrap();
        fs::write(&path, b"after!").unwrap();
        assert_eq!(bound.read().unwrap(), b"before");
        assert!(bound.verify().is_err());

        let oversized = root.join("oversized");
        fs::write(&oversized, b"123456").unwrap();
        assert!(BoundFile::open(&oversized, &root.join("oversized.snapshot"), 5).is_err());
    }

    #[test]
    fn validates_complete_triplets_deterministically() {
        let (_temp, config) = fixture(true);
        let first = validate_research_segments(&config).unwrap();
        let second = validate_research_segments(&config).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.market.recording_policy["quote_depth_levels"], 0);
        assert_eq!(first.market.recording_policy["quote_sample_ms"], 1_000);
        assert_eq!(first.references[0].record_id_versions, json!(["v2"]));
        assert_eq!(first.references[0].recording_policy["quote_sample_ms"], 0);
    }

    #[test]
    fn validates_consecutive_reference_hours_as_one_input_set() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(&root, "crypto_expiry_reference", &reference_rows(false));
        let mut second = vec![
            row(0, metadata("market_metadata")),
            row(1, metadata("market_settlement")),
        ];
        for record in &mut second {
            record["recorded_at"] = json!("2026-07-17T06:01:00Z");
        }
        let second = triplet(&root, "crypto_expiry_reference", &second);
        let mut config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        let report = validate_research_segments(&config).unwrap();
        assert_eq!(report.references.len(), 2);
        assert_eq!(report.references[1].hour, "06");

        config.references.reverse();
        rejects(&config, "consecutive UTC hours");
    }

    #[test]
    fn validates_closed_reference_fragments_from_the_same_hour() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(
            &root,
            "crypto_expiry_reference",
            &[row(0, metadata("market_metadata")), row(1, trade())],
        );
        let second = triplet(
            &root,
            "crypto_expiry_reference",
            &[
                row(0, metadata("market_metadata")),
                row(1, metadata("market_settlement")),
            ],
        );
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        let report = validate_research_segments(&config).unwrap();

        assert_eq!(report.references.len(), 2);
        assert_eq!(report.references[0].hour, "05");
        assert_eq!(report.references[1].hour, "05");
    }

    #[test]
    fn rejects_skipped_reference_hours() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(
            &root,
            "crypto_expiry_reference",
            &[row(0, metadata("market_metadata")), row(1, trade())],
        );
        let mut second = vec![
            row(0, metadata("market_metadata")),
            row(1, metadata("market_settlement")),
        ];
        for record in &mut second {
            record["recorded_at"] = json!("2026-07-17T07:01:00Z");
        }
        let second = triplet(&root, "crypto_expiry_reference", &second);
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        rejects(&config, "same or consecutive UTC hours");
    }

    #[test]
    fn accepts_reference_segments_with_event_local_trade_completion_proof() {
        let mut reference = reference_rows(true);
        reference.push(row(3, trade_completion()));
        let (_temp, config) = fixture_rows(&market_rows(), &reference);

        let report = validate_research_segments(&config).unwrap();

        assert_eq!(report.references.len(), 1);
    }

    #[test]
    fn rejects_reference_segment_sets_over_resource_limits() {
        let (_temp, mut config) = fixture(true);
        config.references = vec![config.references[0].clone(); MAX_REFERENCE_SEGMENTS + 1];
        rejects(&config, "segment count");

        let (_temp, config) = fixture(true);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&config.references[0].manifest).unwrap()).unwrap();
        manifest["source_bytes"] = json!(MAX_REFERENCE_SOURCE_BYTES + 1);
        fs::write(
            &config.references[0].manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        rejects(&config, "source byte limit");
    }

    #[test]
    fn rejects_manifest_start_outside_its_partition() {
        let (_temp, config) = fixture(true);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&config.references[0].manifest).unwrap()).unwrap();
        manifest["start_recorded_at"] = json!("2026-07-17T04:59:59Z");
        fs::write(
            &config.references[0].manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        rejects(&config, "crosses its UTC-hour partition");
    }

    #[test]
    fn merges_identical_trade_ids_across_reference_segments_preserving_first_row() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(
            &root,
            "crypto_expiry_reference",
            &[row(0, metadata("market_metadata")), row(1, trade())],
        );
        let mut second = vec![
            row(0, metadata("market_metadata")),
            row(1, trade()),
            row(2, metadata("market_settlement")),
        ];
        for record in &mut second {
            record["recorded_at"] = json!("2026-07-17T05:02:00Z");
        }
        let second = triplet(&root, "crypto_expiry_reference", &second);
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![second, first],
        };

        let combined = with_validated_research_segments(&config, |_, _, references| {
            Ok(fs::read_to_string(references)?)
        })
        .unwrap();
        let trades = combined
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["update"]["kind"] == "polymarket_trade")
            .collect::<Vec<_>>();

        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0]["recorded_at"], "2026-07-17T05:01:00Z");
    }

    #[test]
    fn retains_later_only_trades_while_deduplicating_earlier_rows() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(
            &root,
            "crypto_expiry_reference",
            &[row(0, metadata("market_metadata")), row(1, trade())],
        );
        let mut second = vec![
            row(0, metadata("market_metadata")),
            row(1, trade()),
            row(2, later_trade()),
            row(3, metadata("market_settlement")),
        ];
        for record in &mut second {
            record["recorded_at"] = json!("2026-07-17T05:02:00Z");
        }
        let second = triplet(&root, "crypto_expiry_reference", &second);
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        let combined = with_validated_research_segments(&config, |_, _, references| {
            Ok(fs::read_to_string(references)?)
        })
        .unwrap();
        let trade_ids = combined
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["update"]["kind"] == "polymarket_trade")
            .map(|row| row["update"]["record_id"].as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(trade_ids.len(), 2);
        assert!(trade_ids.contains(trade()["record_id"].as_str().unwrap()));
        assert!(trade_ids.contains(later_trade()["record_id"].as_str().unwrap()));
    }

    #[test]
    fn event_local_trade_union_allows_sparse_reference_hours() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first_trade = trade();
        let missing_trade = later_trade();
        let first = triplet(
            &root,
            "crypto_expiry_reference",
            &[
                row(0, metadata("market_metadata")),
                row(1, first_trade.clone()),
            ],
        );
        let mut second = vec![
            row(0, metadata("market_metadata")),
            row(1, first_trade.clone()),
            row(2, missing_trade.clone()),
            row(3, metadata("market_settlement")),
            row(
                4,
                trade_completion_for(&[first_trade.clone(), missing_trade.clone()]),
            ),
        ];
        for record in &mut second {
            record["recorded_at"] = json!("2026-07-17T07:02:00Z");
        }
        let second = triplet(&root, "crypto_expiry_reference", &second);
        let config = crate::polymarket_research_normalize::PolymarketEvidenceConfig {
            segments: ResearchSegmentValidationConfig {
                market,
                references: vec![first, second],
            },
            event_start_gte: "2026-07-17T05:00:00Z".to_owned(),
            event_start_lt: "2026-07-17T05:05:00Z".to_owned(),
            market_ids: vec!["market-1".to_owned()],
        };

        let error = crate::polymarket_research_select::select_research_contracts(&config)
            .unwrap_err()
            .to_string();
        assert!(error.contains("same or consecutive UTC hours"));

        let normalized =
            crate::polymarket_research_normalize::normalize_polymarket_evidence(&config).unwrap();
        let trades = String::from_utf8(normalized.ndjson)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["surface"] == "polymarket_trade")
            .map(|row| {
                (
                    row["record_id"].as_str().unwrap().to_owned(),
                    row["available_at"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(normalized.report.surface_counts["polymarket_trade"], 2);
        assert_eq!(trades.len(), 2);
        assert_eq!(
            trades[first_trade["record_id"].as_str().unwrap()],
            "2026-07-17T05:01:00Z"
        );
        assert_eq!(
            trades[missing_trade["record_id"].as_str().unwrap()],
            "2026-07-17T07:02:00Z"
        );
    }

    #[test]
    fn explicit_market_normalization_accepts_bounded_quote_recovery_without_weakening_strict_import(
    ) {
        let mut market_rows = market_rows();
        market_rows.push(row(
            4,
            json!({
                "kind": "event_discovered", "event_id": "unrelated-market", "symbol": "BTCUSDT",
                "up_token": "unrelated-up", "down_token": "unrelated-down",
                "end_time": "2026-07-17T05:10:00Z", "window_secs": 300,
                "price_to_beat": "101", "resolved_up_won": null,
            }),
        ));
        market_rows.push(row(
            5,
            json!({"kind": "event_expired", "event_id": "unrelated-market", "end_time": null}),
        ));
        market_rows.push(quote_failure(6));
        market_rows.push(quote_at(
            7,
            "up-token",
            "2026-07-17T05:01:02Z",
            "2026-07-17T05:01:02Z",
        ));

        let (_temp, config) = explicit_evidence_fixture(&market_rows);

        rejects(
            &config.segments,
            "manifest must be canonical, segment-complete, and source-closed",
        );
        let normalized =
            crate::polymarket_research_normalize::normalize_polymarket_evidence(&config)
                .expect("explicit selected-market recovery remains causally usable");

        assert_eq!(normalized.report.market_ids, ["market-1"]);
        assert_eq!(normalized.report.surface_counts["orderbook_snapshot"], 3);
        let recovered_quote = std::str::from_utf8(&normalized.ndjson)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|row| row["surface"] == "orderbook_snapshot" && row["source_sequence"] == 7)
            .expect("recovered quote remains in normalized evidence");
        assert_eq!(recovered_quote["ts"], "2026-07-17T05:01:02Z");
        assert_eq!(recovered_quote["recorded_at"], "2026-07-17T05:01:02Z");
    }

    #[test]
    fn explicit_market_normalization_rejects_unbounded_or_missing_selected_token_coverage() {
        let mut unresolved = market_rows();
        unresolved.push(quote_failure(4));
        assert!(explicit_normalization_error(&unresolved).contains("unresolved"));

        let mut late_recovery = market_rows();
        late_recovery.push(quote_failure(4));
        late_recovery.push(quote_at(
            5,
            "up-token",
            "2026-07-17T05:01:32Z",
            "2026-07-17T05:01:32Z",
        ));
        assert!(explicit_normalization_error(&late_recovery).contains("over 30 seconds"));

        let mut silent_gap = market_rows();
        silent_gap.push(quote_at(
            4,
            "up-token",
            "2026-07-17T05:01:31Z",
            "2026-07-17T05:01:31Z",
        ));
        assert!(explicit_normalization_error(&silent_gap).contains("gap over 30 seconds"));

        let mut missing_down = market_rows();
        missing_down.remove(2);
        missing_down[2]["sequence"] = json!(2);
        assert!(explicit_normalization_error(&missing_down).contains("no causally available quote"));
    }

    #[test]
    fn explicit_market_normalization_rejects_quote_quality_contradictions() {
        let mut crossed = market_rows();
        crossed[1]["update"]["bid"] = json!("0.60");
        crossed[1]["update"]["ask"] = json!("0.50");
        crossed[1]["update"]["bid_levels"][0]["price"] = json!("0.60");
        crossed[1]["update"]["ask_levels"][0]["price"] = json!("0.50");
        crossed[1]["update"]["collection_result"] = json!("incomplete");

        assert!(explicit_normalization_error(&crossed).contains("full visible L2 records"));
    }

    #[test]
    fn explicit_market_recovery_does_not_relax_segment_identity_or_depth() {
        assert!(explicit_manifest_error(|manifest| {
            manifest["source_session_closed"] = json!(false);
        })
        .contains("source-closed"));
        assert!(explicit_manifest_error(|manifest| {
            manifest["sequence_gaps"] = json!(1);
        })
        .contains("source identity or sequence declaration"));
        assert!(explicit_manifest_error(|manifest| {
            manifest["recording_policy"]["quote_depth_levels"] = json!(1);
        })
        .contains("full visible L2 records"));
    }

    #[test]
    fn rejects_duplicate_trade_ids_with_conflicting_market_context() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(
            &root,
            "crypto_expiry_reference",
            &[row(0, metadata("market_metadata")), row(1, trade())],
        );
        let mut conflicting = trade();
        conflicting["symbol"] = json!("SOLUSDT");
        let second = triplet(
            &root,
            "crypto_expiry_reference",
            &[
                row(0, metadata("market_metadata")),
                row(1, conflicting),
                row(2, metadata("market_settlement")),
            ],
        );
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        rejects(&config, "conflicting polymarket_trade record_id");
    }

    #[test]
    fn merges_identical_settlements_preserving_first_row() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(&root, "crypto_expiry_reference", &reference_rows(true));
        let mut second = vec![
            row(0, metadata("market_metadata")),
            row(1, metadata("market_settlement")),
        ];
        for record in &mut second {
            record["recorded_at"] = json!("2026-07-17T05:02:00Z");
        }
        let second = triplet(&root, "crypto_expiry_reference", &second);
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        let combined = with_validated_research_segments(&config, |_, _, references| {
            Ok(fs::read_to_string(references)?)
        })
        .unwrap();
        let settlements = combined
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["update"]["kind"] == "market_settlement")
            .collect::<Vec<_>>();

        assert_eq!(settlements.len(), 1);
        assert_eq!(settlements[0]["recorded_at"], "2026-07-17T05:01:00Z");
    }

    #[test]
    fn rejects_duplicate_settlements_with_conflicting_winner() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let market = triplet(&root, "crypto_expiry", &market_rows());
        let first = triplet(&root, "crypto_expiry_reference", &reference_rows(true));
        let mut conflicting = metadata("market_settlement");
        conflicting["winning_token_id"] = json!("down-token");
        conflicting["winning_outcome"] = json!("Down");
        conflicting["resolved_up_won"] = json!(false);
        conflicting["market"]["outcomePrices"] = json!("[\"0.001\",\"0.999\"]");
        let second = triplet(
            &root,
            "crypto_expiry_reference",
            &[row(0, metadata("market_metadata")), row(1, conflicting)],
        );
        let config = ResearchSegmentValidationConfig {
            market,
            references: vec![first, second],
        };

        rejects(&config, "conflicting market_settlement");
    }

    #[test]
    fn authenticates_legacy_derived_manifest_fields_by_rescanning_bound_data() {
        let mut reference = reference_rows(true);
        reference.push(row(3, trade_completion()));
        let (_temp, config) = fixture_rows(&market_rows(), &reference);
        for triplet in std::iter::once(&config.market).chain(config.references.iter()) {
            let mut manifest: Value =
                serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
            let fields = manifest.as_object_mut().unwrap();
            fields.remove("reference_context_complete");
            fields.remove("trade_completions");
            fs::write(
                &triplet.manifest,
                format!("{}\n", serde_json::to_string(&manifest).unwrap()),
            )
            .unwrap();
        }

        let report = validate_research_segments(&config)
            .expect("a producer rescan must authenticate legacy derived fields");

        assert_eq!(report.references[0].dataset, "crypto_expiry_reference");
        assert!(report.market.trade_completions.is_empty());
        let completion = &report.references[0].trade_completions["market-1"];
        assert_eq!(completion.condition_id, "0xcondition");
        assert_eq!(completion.trade_count, 1);
        assert_eq!(completion.completion_sequence, 3);
    }

    #[test]
    fn rejects_present_trade_completions_that_contradict_producer_rescan() {
        let mut reference = reference_rows(true);
        reference.push(row(3, trade_completion()));
        let (_temp, config) = fixture_rows(&market_rows(), &reference);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&config.references[0].manifest).unwrap()).unwrap();
        manifest["trade_completions"] = json!({});
        fs::write(
            &config.references[0].manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();

        let error = validate_research_segments(&config).unwrap_err().to_string();
        let reference_file = config.references[0]
            .data
            .file_name()
            .unwrap()
            .to_string_lossy();
        assert!(error.contains(reference_file.as_ref()), "{error:#}");
        assert!(error.contains("field trade_completions"), "{error:#}");
        assert!(error.contains("producer={}"), "{error:#}");
        assert!(error.contains("rescan={\"market-1\":"), "{error:#}");
    }

    #[test]
    fn bounds_producer_rescan_diagnostic_values() {
        let value = json!("x".repeat(MAX_RESCAN_DIAGNOSTIC_CHARS + 1));
        let diagnostic = bounded_rescan_value(Some(&value));

        assert!(diagnostic.ends_with("...<truncated>"));
        assert_eq!(
            diagnostic.chars().count(),
            MAX_RESCAN_DIAGNOSTIC_CHARS + "...<truncated>".chars().count()
        );
    }

    #[test]
    fn preserves_producer_quality_ratio_across_manifest_parse() {
        let producer: Value = serde_json::from_str("0.9340463458110517").unwrap();
        let rescanned = json!(1048_f64 / 1122_f64);

        assert_eq!(producer, rescanned);
    }

    #[test]
    fn rejects_legacy_reference_manifest_when_rescan_finds_missing_metadata_context() {
        let reference = vec![row(0, trade()), row(1, metadata("market_settlement"))];
        let (_temp, config) = fixture_rows(&market_rows(), &reference);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&config.references[0].manifest).unwrap()).unwrap();
        manifest
            .as_object_mut()
            .unwrap()
            .remove("reference_context_complete");
        manifest["canonical"] = json!(true);
        manifest["segment_complete"] = json!(true);
        fs::write(
            &config.references[0].manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();

        rejects(&config, "producer rescan");
    }

    #[test]
    fn rejects_tampered_success_marker() {
        let (_temp, config) = fixture(true);
        fs::write(&config.market.success, b"tampered\n").unwrap();
        rejects(&config, "_SUCCESS");
    }

    #[test]
    fn rejects_invalid_reference_contracts() {
        let (_temp, config) = fixture(false);
        rejects(&config, "settlements");
        let (_temp, config) = fixture(true);
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&config.references[0].manifest).unwrap()).unwrap();
        manifest["recording_policy"]["quote_sample_ms"] = json!(1_000);
        fs::write(
            &config.references[0].manifest,
            format!("{}\n", serde_json::to_string(&manifest).unwrap()),
        )
        .unwrap();
        rejects(&config, "exact recording policy");
    }

    #[test]
    fn rejects_cross_hour_and_mixed_dataset_segments() {
        let mut market = market_rows();
        market.last_mut().unwrap()["recorded_at"] = json!("2026-07-17T06:00:00Z");
        let (_temp, config) = fixture_rows(&market, &reference_rows(true));
        rejects(&config, "crosses its UTC-hour partition");

        let mut reference = reference_rows(true);
        for mut record in market_rows().into_iter().take(3) {
            record["sequence"] = json!(reference.len());
            reference.push(record);
        }
        let (_temp, config) = fixture_rows(&market_rows(), &reference);
        rejects(&config, "only metadata");
    }

    #[test]
    fn rejects_adjacent_superseded_marker() {
        let (_temp, config) = fixture(true);
        let name = format!(
            "{}.SUPERSEDED.json",
            config.market.data.file_name().unwrap().to_str().unwrap()
        );
        fs::write(config.market.data.with_file_name(name), b"{}\n").unwrap();
        rejects(&config, "SUPERSEDED");
    }
}
