use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CString, OsStr};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::ops::Range;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const MANIFEST_SCHEMA: &str = "monday.polymarket.evidence_artifact.v1";
const INPUT_SCHEMA: &str = "monday.polymarket.research_segment_validation.v1";
const INPUT_SCHEMA_V2: &str = "monday.polymarket.research_segment_validation.v2";
const ROW_SCHEMA: &str = "monday.polymarket.evidence_row.v1";
const PUBLISHED_MODE: u32 = 0o444;
const MAX_DATA_BYTES: u64 = 512 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ROWS: usize = 5_000_000;
const SYMBOLS: [&str; 2] = ["BTCUSDT", "SOLUSDT"];
#[rustfmt::skip]
const SURFACES: [&str; 5] = ["chainlink_reference", "market_contract", "orderbook_snapshot", "polymarket_trade", "official_settlement_evidence"];
const EVENT_SELECTION: &str = "event_start in [event_start_gte,event_start_lt)";
const EVIDENCE_SCOPE: &str =
    "immutable collector evidence only; not an execution authorization or evaluator label artifact";
const DIGEST_SEMANTICS: &str =
    "content_sha256 binds the published NDJSON bytes only; it is not a snapshot_contract_hash";
const TRUST_BOUNDARY: &str = "typed collector staging evidence only; not an evaluator label snapshot or snapshot_contract_hash; validated staged triplets and adjacent local supersession markers; omitted remote-prefix markers are not proven absent";
const TRADE_SEMANTICS: &str = "exact market_id association using canonical v2 records; selected event count and record IDs match a collector completion proof; trade_ts may fall outside the event lifetime";
const REFERENCE_SEMANTICS: &str =
    "typed Chainlink BTC/USD or SOL/USD with source timestamp in [event_start - 30 seconds, event_end)";
const SETTLEMENT_SEMANTICS: &str =
    "gamma_api_closed_market closed-market evidence joined by exact market_id";
const AVAILABILITY_SEMANTICS: &str =
    "point-in-time rows expose the latest validated recorded or retrieved clock as available_at";
const TRADE_COMPLETION_BASIS: &str =
    "polymarket_data_api_exhausted_after_settlement_and_stable_polls_v1";

#[derive(Debug, Clone)]
pub struct PolymarketEvidenceTriplet {
    pub data: PathBuf,
    pub manifest: PathBuf,
    pub success: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolymarketEvidenceTrustAnchor {
    expected_content_sha256: [u8; 32],
    expected_manifest_sha256: [u8; 32],
}

impl PolymarketEvidenceTrustAnchor {
    pub fn from_lower_hex(content: &str, manifest: &str) -> Result<Self> {
        Ok(Self {
            expected_content_sha256: parse_digest(content, "expected content SHA-256")?,
            expected_manifest_sha256: parse_digest(manifest, "expected manifest SHA-256")?,
        })
    }
}

#[derive(Debug)]
pub struct SealedPolymarketEvidenceTriplet {
    manifest: EvidenceManifest,
    manifest_sha256: String,
    data: Vec<u8>,
    frames: Vec<Range<usize>>,
}

impl SealedPolymarketEvidenceTriplet {
    pub fn content_sha256(&self) -> &str {
        &self.manifest.content_sha256
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    pub fn content_bytes(&self) -> u64 {
        self.data.len() as u64
    }

    pub fn rows(&self) -> u64 {
        self.frames.len() as u64
    }

    pub fn events(&self) -> u64 {
        self.manifest.events
    }

    pub(super) fn framed_rows(&self) -> impl Iterator<Item = &[u8]> {
        self.frames.iter().map(|frame| &self.data[frame.clone()])
    }

    pub(super) fn selection_bounds(&self) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        Ok((
            parse_time(&self.manifest.event_start_gte, "event_start_gte")?,
            parse_time(&self.manifest.event_start_lt, "event_start_lt")?,
        ))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceManifest {
    schema: String,
    file: String,
    format: String,
    content_sha256: String,
    content_bytes: u64,
    rows: u64,
    events: u64,
    surface_counts: BTreeMap<String, u64>,
    event_start_gte: String,
    event_start_lt: String,
    symbols: [String; 2],
    window_secs: u64,
    event_selection: String,
    evidence_scope: String,
    content_digest_semantics: String,
    recording_semantics: RecordingSemantics,
    trust_boundary: String,
    validated_inputs: ValidatedInputs,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RecordingSemantics {
    orderbook: OrderBookSemantics,
    trades: String,
    references: String,
    settlement: String,
    availability_clock: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OrderBookSemantics {
    level: String,
    depth: String,
    quote_sample_ms: u64,
    venue_depth_complete: bool,
    temporal_updates_complete: bool,
    l3_order_ids_available: bool,
    queue_position_modeled: bool,
    endogenous_impact_modeled: bool,
    capacity_modeled: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ValidatedInputs {
    V1(Box<ValidatedInputsV1>),
    V2(Box<ValidatedInputsV2>),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatedInputsV1 {
    schema: String,
    market: SegmentIdentity,
    reference: SegmentIdentity,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidatedInputsV2 {
    schema: String,
    market: SegmentIdentity,
    references: Vec<SegmentIdentity>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentIdentity {
    schema: String,
    venue: String,
    dataset: String,
    date: String,
    hour: String,
    file: String,
    bytes: u64,
    sha256: String,
    events: u64,
    start_sequence: u64,
    end_sequence: u64,
    sequence_gaps: u64,
    start_recorded_at: String,
    end_recorded_at: String,
    source_file: String,
    replay_scope: String,
    recording_policy: RecordingPolicy,
    record_id_versions: Vec<String>,
    #[serde(default)]
    trade_completions: BTreeMap<String, TradeCompletionIdentity>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TradeCompletionIdentity {
    condition_id: String,
    symbol: String,
    market_window_secs: u64,
    trade_count: u64,
    trade_record_ids_sha256: String,
    completion_sequence: u64,
    retrieved_at: String,
    completeness_basis: String,
    finalization_lag_secs: u64,
    stable_polls_required: u64,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RecordingPolicy {
    quote_sample_ms: u64,
    quote_depth_levels: u64,
    event_scoped_quotes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    bytes: u64,
    mode: u32,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            mode: metadata.mode() & 0o777,
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}
struct BoundFile {
    path: PathBuf,
    name: CString,
    file: File,
    identity: FileIdentity,
}

pub fn seal_polymarket_evidence_triplet(
    triplet: &PolymarketEvidenceTriplet,
    trust: &PolymarketEvidenceTrustAnchor,
) -> Result<SealedPolymarketEvidenceTriplet> {
    seal_with_hook(triplet, trust, || Ok(()))
}

fn seal_with_hook(
    triplet: &PolymarketEvidenceTriplet,
    trust: &PolymarketEvidenceTrustAnchor,
    after_open: impl FnOnce() -> Result<()>,
) -> Result<SealedPolymarketEvidenceTriplet> {
    let parent = common_canonical_parent(triplet)?;
    let directory = bind_directory(&parent)?;
    let mut data = BoundFile::open(&directory, &triplet.data, MAX_DATA_BYTES)?;
    let mut manifest = BoundFile::open(&directory, &triplet.manifest, MAX_MANIFEST_BYTES)?;
    let mut success = BoundFile::open(&directory, &triplet.success, 65)?;
    after_open()?;
    let data_bytes = data.read_bounded(MAX_DATA_BYTES)?;
    let manifest_bytes = manifest.read_bounded(MAX_MANIFEST_BYTES)?;
    let success_bytes = success.read_bounded(65)?;
    for file in [&data, &manifest, &success] {
        file.verify(&directory)?;
    }
    verify_bound_directory(&parent, &directory)?;
    if <[u8; 32]>::from(Sha256::digest(&data_bytes)) != trust.expected_content_sha256
        || <[u8; 32]>::from(Sha256::digest(&manifest_bytes)) != trust.expected_manifest_sha256
    {
        bail!("evidence bytes do not match the trusted digest anchor");
    }

    let parsed = parse_manifest(&manifest_bytes)?;
    validate_manifest(&parsed, triplet, &success_bytes)?;
    let trade_completions = validated_reference_trade_completions(&parsed.validated_inputs)?;
    if u64::try_from(data_bytes.len())? != parsed.content_bytes
        || format!("{:x}", Sha256::digest(&data_bytes)) != parsed.content_sha256
    {
        bail!("evidence data bytes do not match the manifest identity");
    }
    if parse_digest(&parsed.content_sha256, "content_sha256")? != trust.expected_content_sha256 {
        bail!("evidence manifest content digest does not match the trusted anchor");
    }
    let frames = frame_ndjson(
        &data_bytes,
        &parsed.surface_counts,
        &trade_completions,
    )?;
    if u64::try_from(frames.len())? != parsed.rows {
        bail!("evidence NDJSON row count does not match the manifest");
    }
    Ok(SealedPolymarketEvidenceTriplet {
        manifest: parsed,
        manifest_sha256: format!("{:x}", Sha256::digest(&manifest_bytes)),
        data: data_bytes,
        frames,
    })
}

fn common_canonical_parent(triplet: &PolymarketEvidenceTriplet) -> Result<PathBuf> {
    for path in [&triplet.data, &triplet.manifest, &triplet.success] {
        if !path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::CurDir | Component::ParentDir | Component::Prefix(_)
                )
            })
        {
            bail!("evidence triplet paths must be absolute and canonical");
        }
    }
    let parent = triplet
        .data
        .parent()
        .ok_or_else(|| anyhow!("evidence data path has no parent"))?;
    if triplet.manifest.parent() != Some(parent) || triplet.success.parent() != Some(parent) {
        bail!("evidence triplet must share one content-addressed directory");
    }
    let canonical = fs::canonicalize(parent).context("canonicalize evidence directory")?;
    if canonical != parent {
        bail!("evidence directory must be an absolute canonical path");
    }
    Ok(canonical)
}

impl BoundFile {
    fn open(directory: &File, path: &Path, max_bytes: u64) -> Result<Self> {
        let name = component_name(
            path.file_name()
                .ok_or_else(|| anyhow!("evidence file has no name"))?,
        )?;
        let file = open_entry(directory, &name, path)?;
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        if identity.bytes > max_bytes {
            bail!(
                "evidence file exceeds its resource bound: {}",
                path.display()
            );
        }
        if identity.mode != PUBLISHED_MODE {
            bail!(
                "evidence file permissions are {:o}, expected {:o}: {}",
                identity.mode,
                PUBLISHED_MODE,
                path.display()
            );
        }
        Ok(Self {
            path: path.to_owned(),
            name,
            file,
            identity,
        })
    }

    fn read_bounded(&mut self, max_bytes: u64) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.file
            .by_ref()
            .take(max_bytes + 1)
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len())? > max_bytes {
            bail!(
                "evidence file exceeds its resource bound: {}",
                self.path.display()
            );
        }
        Ok(bytes)
    }

    fn verify(&self, directory: &File) -> Result<()> {
        let current = open_entry(directory, &self.name, &self.path)?;
        if FileIdentity::from_metadata(&self.file.metadata()?) != self.identity
            || FileIdentity::from_metadata(&current.metadata()?) != self.identity
        {
            bail!(
                "evidence file changed while reading: {}",
                self.path.display()
            );
        }
        Ok(())
    }
}

fn bind_directory(path: &Path) -> Result<File> {
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/")
        .context("bind filesystem root")?;
    for part in path.components() {
        let Component::Normal(name) = part else {
            if matches!(part, Component::RootDir) {
                continue;
            }
            bail!("evidence directory must contain only absolute normal components");
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
                .with_context(|| format!("bind evidence directory {}", path.display()));
        }
        directory = unsafe { File::from_raw_fd(descriptor) };
    }
    verify_bound_directory(path, &directory)?;
    Ok(directory)
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
        bail!("evidence directory identity changed: {}", path.display());
    }
    Ok(())
}

fn open_entry(directory: &File, name: &CString, path: &Path) -> Result<File> {
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("open evidence file {}", path.display()));
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    if !file.metadata()?.is_file() {
        bail!("evidence target is not a regular file: {}", path.display());
    }
    Ok(file)
}

fn directory_identity(metadata: &Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn component_name(name: &OsStr) -> Result<CString> {
    CString::new(name.as_bytes()).context("evidence path component contains NUL")
}

fn parse_manifest(bytes: &[u8]) -> Result<EvidenceManifest> {
    if bytes.is_empty()
        || bytes.last() != Some(&b'\n')
        || bytes[..bytes.len() - 1].contains(&b'\n')
        || bytes.contains(&b'\r')
    {
        bail!("evidence manifest must be one JSON line ending in one newline");
    }
    serde_json::from_slice(bytes).context("parse evidence manifest")
}

fn validate_manifest(
    manifest: &EvidenceManifest,
    triplet: &PolymarketEvidenceTriplet,
    success: &[u8],
) -> Result<()> {
    let orderbook = &manifest.recording_semantics.orderbook;
    if manifest.schema != MANIFEST_SCHEMA
        || manifest.format != "ndjson"
        || manifest.symbols != SYMBOLS.map(str::to_owned)
        || manifest.window_secs != 300
        || manifest.event_selection != EVENT_SELECTION
        || manifest.evidence_scope != EVIDENCE_SCOPE
        || manifest.content_digest_semantics != DIGEST_SEMANTICS
        || manifest.trust_boundary != TRUST_BOUNDARY
        || orderbook.level != "L2"
        || orderbook.depth != "full visible depth as received"
        || orderbook.quote_sample_ms != 1_000
        || !orderbook.venue_depth_complete
        || orderbook.temporal_updates_complete
        || orderbook.l3_order_ids_available
        || orderbook.queue_position_modeled
        || orderbook.endogenous_impact_modeled
        || orderbook.capacity_modeled
        || manifest.recording_semantics.trades != TRADE_SEMANTICS
        || manifest.recording_semantics.references != REFERENCE_SEMANTICS
        || manifest.recording_semantics.settlement != SETTLEMENT_SEMANTICS
        || manifest.recording_semantics.availability_clock != AVAILABILITY_SEMANTICS
    {
        bail!("unsupported immutable evidence manifest contract");
    }
    validate_inputs(&manifest.validated_inputs)?;
    parse_digest(&manifest.content_sha256, "content_sha256")?;
    let lower = parse_time(&manifest.event_start_gte, "event_start_gte")?;
    let upper = parse_time(&manifest.event_start_lt, "event_start_lt")?;
    let keys = manifest
        .surface_counts
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let rows = manifest
        .surface_counts
        .values()
        .try_fold(0u64, |sum, count| sum.checked_add(*count))
        .ok_or_else(|| anyhow!("evidence surface counts overflow"))?;
    if lower >= upper || manifest.rows != rows || keys != BTreeSet::from(SURFACES) {
        bail!("immutable evidence manifest identity is inconsistent");
    }
    let name = triplet
        .data
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("evidence data name is not UTF-8"))?;
    if manifest.file != name
        || triplet
            .data
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str)
            != Some(format!("sha256={}", manifest.content_sha256).as_str())
        || triplet.manifest.file_name().and_then(OsStr::to_str)
            != Some(format!("{name}.manifest.json").as_str())
        || triplet.success.file_name().and_then(OsStr::to_str)
            != Some(format!("{name}._SUCCESS").as_str())
        || success != format!("{}\n", manifest.content_sha256).as_bytes()
    {
        bail!("evidence file names or _SUCCESS marker do not match the manifest");
    }
    Ok(())
}

fn validate_inputs(inputs: &ValidatedInputs) -> Result<()> {
    match inputs {
        ValidatedInputs::V1(inputs) => {
            if inputs.schema != INPUT_SCHEMA {
                bail!("unsupported validated input contract");
            }
            validate_segment(&inputs.market, "crypto_expiry")?;
            if inputs.reference.record_id_versions != ["v2"] {
                bail!("validated reference input must contain v2 trades");
            }
            validate_segment(&inputs.reference, "crypto_expiry_reference")
        }
        ValidatedInputs::V2(inputs) => {
            if inputs.schema != INPUT_SCHEMA_V2 || inputs.references.is_empty() {
                bail!("unsupported validated input contract");
            }
            validate_segment(&inputs.market, "crypto_expiry")?;
            for reference in &inputs.references {
                validate_segment(reference, "crypto_expiry_reference")?;
            }
            if !inputs
                .references
                .iter()
                .any(|reference| reference.record_id_versions == ["v2"])
            {
                bail!("validated reference inputs must contain v2 trades");
            }
            for pair in inputs.references.windows(2) {
                let hour = |segment: &SegmentIdentity| {
                    parse_time(
                        &format!("{}T{}:00:00Z", segment.date, segment.hour),
                        "validated reference date/hour",
                    )
                };
                if hour(&pair[1])? != hour(&pair[0])? + chrono::Duration::hours(1) {
                    bail!("validated reference inputs must be consecutive UTC hours");
                }
            }
            aggregate_reference_trade_completions(
                inputs
                    .references
                    .iter()
                    .map(|reference| &reference.trade_completions),
            )
            .map(|_| ())
        }
    }
}

fn validate_segment(segment: &SegmentIdentity, dataset: &str) -> Result<()> {
    let (quote_sample_ms, replay_scope) = match dataset {
        "crypto_expiry" => (1_000, "complete_full_depth_sampled_normalized_hour_segment"),
        "crypto_expiry_reference" => (0, "complete_reference_hour_segment"),
        _ => unreachable!("validated input dataset is fixed by the manifest contract"),
    };
    let versions_valid = segment.record_id_versions.is_empty()
        || (dataset == "crypto_expiry_reference" && segment.record_id_versions == ["v2"]);
    parse_digest(&segment.sha256, "validated input sha256")?;
    let start = parse_time(&segment.start_recorded_at, "start_recorded_at")?;
    let end = parse_time(&segment.end_recorded_at, "end_recorded_at")?;
    let events = segment
        .end_sequence
        .checked_sub(segment.start_sequence)
        .and_then(|distance| distance.checked_add(1));
    let source_file = segment.file.strip_suffix(".zst").unwrap_or_default();
    if segment.schema != "monday.polymarket.raw.v1"
        || segment.venue != "polymarket"
        || segment.dataset != dataset
        || end.format("%Y-%m-%d").to_string() != segment.date
        || end.format("%H").to_string() != segment.hour
        || source_file != segment.source_file
        || Path::new(source_file).components().count() != 1
        || segment.bytes == 0
        || segment.events == 0
        || segment.sequence_gaps != 0
        || events != Some(segment.events)
        || start > end
        || segment.replay_scope != replay_scope
        || !versions_valid
        || segment.recording_policy
            != (RecordingPolicy {
                quote_sample_ms,
                quote_depth_levels: 0,
                event_scoped_quotes: true,
            })
    {
        bail!("validated {dataset} input identity is inconsistent");
    }
    validate_trade_completions(segment, dataset, start, end)?;
    Ok(())
}

fn validate_trade_completions(
    segment: &SegmentIdentity,
    dataset: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<()> {
    if dataset == "crypto_expiry" {
        if !segment.trade_completions.is_empty() {
            bail!("validated market input must not declare trade completions");
        }
        return Ok(());
    }
    if segment.trade_completions.is_empty() {
        return Ok(());
    }
    for (market_id, completion) in &segment.trade_completions {
        let retrieved_at = parse_time(&completion.retrieved_at, "trade completion retrieved_at")?;
        if market_id.is_empty()
            || completion.condition_id.is_empty()
            || !SYMBOLS.contains(&completion.symbol.as_str())
            || !matches!(completion.market_window_secs, 300 | 900)
            || parse_digest(
                &completion.trade_record_ids_sha256,
                "trade completion record ID digest",
            )
            .is_err()
            || completion.completion_sequence < segment.start_sequence
            || completion.completion_sequence > segment.end_sequence
            || retrieved_at < start
            || retrieved_at > end
            || completion.completeness_basis != TRADE_COMPLETION_BASIS
            || completion.finalization_lag_secs == 0
            || completion.stable_polls_required == 0
        {
            bail!("validated reference event-local trade completion identity is inconsistent");
        }
    }
    Ok(())
}

fn validated_reference_trade_completions(
    inputs: &ValidatedInputs,
) -> Result<BTreeMap<String, TradeCompletionIdentity>> {
    match inputs {
        ValidatedInputs::V1(inputs) => {
            if inputs.reference.trade_completions.is_empty() {
                bail!("validated reference event-local trade completion identity is inconsistent");
            }
            aggregate_reference_trade_completions([&inputs.reference.trade_completions])
        }
        ValidatedInputs::V2(inputs) => aggregate_reference_trade_completions(
            inputs
                .references
                .iter()
                .map(|reference| &reference.trade_completions),
        ),
    }
}

fn aggregate_reference_trade_completions<'a>(
    maps: impl IntoIterator<Item = &'a BTreeMap<String, TradeCompletionIdentity>>,
) -> Result<BTreeMap<String, TradeCompletionIdentity>> {
    let mut completions = BTreeMap::new();
    for map in maps {
        for (market_id, completion) in map {
            if completions
                .insert(market_id.clone(), completion.clone())
                .is_some()
            {
                bail!("validated reference event-local trade completion identity is inconsistent");
            }
        }
    }
    Ok(completions)
}

fn parse_digest(value: &str, label: &str) -> Result<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} must be 64 lowercase hexadecimal characters");
    }
    let mut digest = [0; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(digest)
}

fn parse_time(value: &str, label: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .with_context(|| format!("invalid {label}: {value}"))
}

fn frame_ndjson(
    bytes: &[u8],
    expected_counts: &BTreeMap<String, u64>,
    trade_completions: &BTreeMap<String, TradeCompletionIdentity>,
) -> Result<Vec<Range<usize>>> {
    if bytes.is_empty() || bytes.last() != Some(&b'\n') || bytes.contains(&b'\r') {
        bail!("evidence data must be non-empty newline-terminated NDJSON");
    }
    let mut frames = Vec::new();
    let mut counts = BTreeMap::new();
    let mut market_contexts = BTreeMap::<String, (String, String, u64)>::new();
    let mut trade_record_ids = BTreeMap::<String, BTreeSet<String>>::new();
    let mut start = 0;
    for (end, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        if end == start || end - start > MAX_LINE_BYTES || frames.len() == MAX_ROWS {
            bail!("evidence NDJSON contains an empty, oversized, or excess row");
        }
        let value: Value = serde_json::from_slice(&bytes[start..end])
            .with_context(|| format!("parse evidence row {}", frames.len() + 1))?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow!("evidence row must be a JSON object"))?;
        if object.get("schema").and_then(Value::as_str) != Some(ROW_SCHEMA) {
            bail!("unsupported evidence row schema");
        }
        let surface = object
            .get("surface")
            .and_then(Value::as_str)
            .filter(|surface| SURFACES.contains(surface))
            .ok_or_else(|| anyhow!("unsupported evidence surface"))?;
        *counts.entry(surface.to_owned()).or_default() += 1;
        if matches!(surface, "market_contract" | "polymarket_trade") {
            let market_id = object
                .get("market_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("evidence {surface} row requires market_id"))?;
            if surface == "market_contract" {
                let condition_id = object
                    .get("condition_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("evidence market_contract row requires condition_id"))?;
                let symbol = object
                    .get("symbol")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("evidence market_contract row requires symbol"))?;
                let window_secs = object
                    .get("window_secs")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("evidence market_contract row requires window_secs"))?;
                market_contexts.insert(
                    market_id.to_owned(),
                    (condition_id.to_owned(), symbol.to_owned(), window_secs),
                );
            } else {
                let record_id = object
                    .get("record_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| anyhow!("evidence trade row requires record_id"))?;
                if !trade_record_ids
                    .entry(market_id.to_owned())
                    .or_default()
                    .insert(record_id.to_owned())
                {
                    bail!("evidence rows contain a duplicate Polymarket trade record_id");
                }
            }
        }
        frames.push(start..end);
        start = end + 1;
    }
    if &counts != expected_counts {
        bail!("evidence surface counts do not match the manifest");
    }
    for (market_id, (condition_id, symbol, window_secs)) in market_contexts {
        let completion = trade_completions.get(&market_id).ok_or_else(|| {
            anyhow!("evidence trade rows do not match event-local completion proof")
        })?;
        if completion.condition_id != condition_id
            || completion.symbol != symbol
            || completion.market_window_secs != window_secs
        {
            bail!("evidence trade rows do not match event-local completion proof");
        }
        let record_ids = trade_record_ids.remove(&market_id).unwrap_or_default();
        let mut digest = Sha256::new();
        for record_id in &record_ids {
            digest.update(record_id.as_bytes());
            digest.update(b"\n");
        }
        if completion.trade_count != u64::try_from(record_ids.len())?
            || completion.trade_record_ids_sha256 != format!("{:x}", digest.finalize())
        {
            bail!("evidence trade rows do not match event-local completion proof");
        }
    }
    if !trade_record_ids.is_empty() {
        bail!("evidence trade rows do not match event-local completion proof");
    }
    Ok(frames)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::os::unix::fs::PermissionsExt;

    #[rustfmt::skip]
    fn inputs(rows: &[Value]) -> Value {
        let mut contexts = BTreeMap::<String, (String, String, u64)>::new();
        let mut trade_ids = BTreeMap::<String, BTreeSet<String>>::new();
        for row in rows {
            let Some(market_id) = row.get("market_id").and_then(Value::as_str) else { continue };
            match row.get("surface").and_then(Value::as_str) {
                Some("market_contract") => { contexts.insert(market_id.to_owned(), (row["condition_id"].as_str().unwrap().to_owned(), row["symbol"].as_str().unwrap().to_owned(), row["window_secs"].as_u64().unwrap())); }
                Some("polymarket_trade") => { trade_ids.entry(market_id.to_owned()).or_default().insert(row["record_id"].as_str().unwrap().to_owned()); }
                _ => {}
            }
        }
        let trade_completions = contexts.into_iter().map(|(market_id, (condition_id, symbol, market_window_secs))| {
            let record_ids = trade_ids.remove(&market_id).unwrap_or_default();
            let mut digest = Sha256::new(); for record_id in &record_ids { digest.update(record_id.as_bytes()); digest.update(b"\n"); }
            (market_id, json!({"condition_id":condition_id,"symbol":symbol,"market_window_secs":market_window_secs,"trade_count":record_ids.len(),"trade_record_ids_sha256":format!("{:x}", digest.finalize()),"completion_sequence":2,"retrieved_at":"2026-07-17T05:01:00Z","completeness_basis":TRADE_COMPLETION_BASIS,"finalization_lag_secs":60,"stable_polls_required":2}))
        }).collect::<serde_json::Map<_, _>>();
        let segment = |dataset: &str, sample: u64, versions: Value| {
            let replay = if sample == 0 { "complete_reference_hour_segment" } else { "complete_full_depth_sampled_normalized_hour_segment" };
            let trade_completions = if sample == 0 { Value::Object(trade_completions.clone()) } else { json!({}) };
            json!({
                "schema":"monday.polymarket.raw.v1", "venue":"polymarket", "dataset":dataset,
                "date":"2026-07-17", "hour":"05", "file":format!("{dataset}.ndjson.zst"), "bytes":100,
                "sha256":"1".repeat(64), "events":2, "start_sequence":1, "end_sequence":2, "sequence_gaps":0,
                "start_recorded_at":"2026-07-17T05:00:00Z", "end_recorded_at":"2026-07-17T05:01:00Z",
                "source_file":format!("{dataset}.ndjson"), "replay_scope":replay, "record_id_versions":versions,
                "recording_policy":{"quote_sample_ms":sample,"quote_depth_levels":0,"event_scoped_quotes":true},
                "trade_completions":trade_completions,
            })
        };
        json!({"schema":INPUT_SCHEMA, "market":segment("crypto_expiry", 1000, json!([])),
            "reference":segment("crypto_expiry_reference", 0, json!(["v2"]))})
    }

    fn plural_inputs(second_hour: &str) -> Value {
        let rows = SURFACES.map(|surface| {
            let mut row = json!({"schema":ROW_SCHEMA,"surface":surface,"market_id":"market-1","condition_id":"condition-1","symbol":"BTCUSDT","window_secs":300});
            if surface == "polymarket_trade" {
                row["record_id"] = json!("trade-1");
            }
            row
        });
        let value = inputs(&rows);
        let mut first = value["reference"].clone();
        first["trade_completions"] = json!({});
        let mut second = value["reference"].clone();
        second["hour"] = json!(second_hour);
        second["start_recorded_at"] = json!(format!("2026-07-17T{second_hour}:00:00Z"));
        second["end_recorded_at"] = json!(format!("2026-07-17T{second_hour}:01:00Z"));
        second["record_id_versions"] = json!([]);
        for completion in second["trade_completions"]
            .as_object_mut()
            .expect("reference trade completions")
            .values_mut()
        {
            completion["retrieved_at"] = json!(format!("2026-07-17T{second_hour}:01:00Z"));
        }
        json!({"schema":INPUT_SCHEMA_V2,"market":value["market"].clone(),"references":[first,second]})
    }

    #[rustfmt::skip]
    fn write_triplet(temp: &tempfile::TempDir) -> PolymarketEvidenceTriplet {
        let rows = SURFACES.map(|surface| {
            let mut row = json!({"schema":ROW_SCHEMA,"surface":surface,"market_id":"market-1","condition_id":"condition-1","symbol":"BTCUSDT","window_secs":300});
            if surface == "polymarket_trade" { row["record_id"] = json!("trade-1"); }
            row
        });
        write_triplet_rows(temp, &rows)
    }

    #[rustfmt::skip]
    pub(in crate::polymarket_evidence) fn write_triplet_rows(temp: &tempfile::TempDir, rows: &[Value]) -> PolymarketEvidenceTriplet {
        let mut data_bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut data_bytes, row).unwrap();
            data_bytes.push(b'\n');
        }
        let digest = format!("{:x}", Sha256::digest(&data_bytes));
        let root = fs::canonicalize(temp.path()).unwrap().join(format!("sha256={digest}"));
        fs::create_dir(&root).unwrap();
        let name = format!("polymarket-btc-sol-5m.{digest}.ndjson");
        let data = root.join(&name);
        let manifest = root.join(format!("{name}.manifest.json"));
        let success = root.join(format!("{name}._SUCCESS"));
        fs::write(&data, &data_bytes).unwrap();
        let mut counts = BTreeMap::new();
        for row in rows { *counts.entry(row["surface"].as_str().unwrap()).or_insert(0) += 1; }
        let manifest_value = json!({
            "schema":MANIFEST_SCHEMA,"file":name,"format":"ndjson",
            "content_sha256":digest,"content_bytes":data_bytes.len(),"rows":rows.len(),"events":counts["market_contract"],
            "surface_counts":counts,"event_start_gte":"2026-07-17T05:30:00Z",
            "event_start_lt":"2026-07-17T05:35:00Z","symbols":SYMBOLS,"window_secs":300,
            "event_selection":EVENT_SELECTION,"evidence_scope":EVIDENCE_SCOPE,
            "content_digest_semantics":DIGEST_SEMANTICS,
            "recording_semantics":{
                "orderbook":{"level":"L2","depth":"full visible depth as received","quote_sample_ms":1000,
                    "venue_depth_complete":true,"temporal_updates_complete":false,"l3_order_ids_available":false,
                    "queue_position_modeled":false,"endogenous_impact_modeled":false,"capacity_modeled":false},
                "trades":TRADE_SEMANTICS,"references":REFERENCE_SEMANTICS,
                "settlement":SETTLEMENT_SEMANTICS,"availability_clock":AVAILABILITY_SEMANTICS},
            "trust_boundary":TRUST_BOUNDARY,"validated_inputs":inputs(rows)
        });
        fs::write(&manifest, format!("{}\n", serde_json::to_string(&manifest_value).unwrap())).unwrap();
        fs::write(&success, format!("{digest}\n")).unwrap();
        for path in [&data, &manifest, &success] {
            fs::set_permissions(path, fs::Permissions::from_mode(PUBLISHED_MODE)).unwrap();
        }
        PolymarketEvidenceTriplet { data, manifest, success }
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[rustfmt::skip]
    pub(in crate::polymarket_evidence) fn trust(triplet: &PolymarketEvidenceTriplet) -> PolymarketEvidenceTrustAnchor {
        let digest = |path: &Path| format!("{:x}", Sha256::digest(fs::read(path).unwrap()));
        PolymarketEvidenceTrustAnchor::from_lower_hex(&digest(&triplet.data), &digest(&triplet.manifest)).unwrap()
    }

    fn rewrite_read_only(path: &Path, bytes: impl AsRef<[u8]>) {
        set_mode(path, 0o644);
        fs::write(path, bytes).unwrap();
        set_mode(path, PUBLISHED_MODE);
    }

    #[test]
    #[rustfmt::skip]
    fn seals_a_bound_typed_triplet() {
        let temp = tempfile::tempdir().unwrap(); let triplet = write_triplet(&temp);
        let sealed = seal_polymarket_evidence_triplet(&triplet, &trust(&triplet)).unwrap();
        assert_eq!((sealed.rows(), sealed.events()), (5, 1));
        assert_eq!(sealed.manifest_sha256(), format!("{:x}", Sha256::digest(fs::read(&triplet.manifest).unwrap())));
    }

    #[test]
    fn seals_consecutive_plural_references_and_rejects_a_gap() {
        for (hour, accepted) in [("06", true), ("07", false)] {
            let temp = tempfile::tempdir().unwrap();
            let triplet = write_triplet(&temp);
            let mut manifest: Value = serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
            manifest["validated_inputs"] = plural_inputs(hour);
            rewrite_read_only(&triplet.manifest, format!("{}\n", serde_json::to_string(&manifest).unwrap()));
            assert_eq!(seal_polymarket_evidence_triplet(&triplet, &trust(&triplet)).is_ok(), accepted);
        }
    }

    #[test]
    #[rustfmt::skip]
    fn rejects_same_inode_mutation_after_open() {
        let temp = tempfile::tempdir().unwrap();
        let triplet = write_triplet(&temp);
        let (anchor, data) = (trust(&triplet), triplet.data.clone());
        let error = seal_with_hook(&triplet, &anchor, || { rewrite_read_only(&data, fs::read(&data)?); Ok(()) }).unwrap_err();
        assert!(error.to_string().contains("changed while reading"), "{error:#}");
    }

    #[test]
    #[rustfmt::skip]
    fn rejects_tampered_data_and_weak_provenance() {
        let temp = tempfile::tempdir().unwrap(); let triplet = write_triplet(&temp);
        let anchor = trust(&triplet);
        rewrite_read_only(&triplet.data, b"tampered\n");
        assert!(seal_polymarket_evidence_triplet(&triplet, &anchor).is_err());

        let temp = tempfile::tempdir().unwrap(); let triplet = write_triplet(&temp);
        let anchor = trust(&triplet);
        let mut value: Value = serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        value["validated_inputs"]["market"]["sha256"] = json!("2".repeat(64));
        rewrite_read_only(&triplet.manifest, format!("{}\n", serde_json::to_string(&value).unwrap()));
        assert!(seal_polymarket_evidence_triplet(&triplet, &anchor).is_err());
    }

    #[test]
    #[rustfmt::skip]
    fn rejects_manifest_without_event_local_trade_completion_identity() {
        let temp = tempfile::tempdir().unwrap();
        let triplet = write_triplet(&temp);
        let mut value: Value = serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        value["validated_inputs"]["reference"].as_object_mut().unwrap().remove("trade_completions");
        rewrite_read_only(&triplet.manifest, format!("{}\n", serde_json::to_string(&value).unwrap()));
        let error = seal_polymarket_evidence_triplet(&triplet, &trust(&triplet)).unwrap_err();
        assert!(error.to_string().contains("event-local trade completion identity"));
    }

    #[test]
    fn seals_v2_references_when_the_completion_moves_to_the_next_hour() {
        let temp = tempfile::tempdir().unwrap();
        let triplet = write_triplet(&temp);
        let mut value: Value = serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        value["validated_inputs"] = plural_inputs("06");
        rewrite_read_only(&triplet.manifest, format!("{}\n", serde_json::to_string(&value).unwrap()));
        assert!(seal_polymarket_evidence_triplet(&triplet, &trust(&triplet)).is_ok());
    }

    #[test]
    #[rustfmt::skip]
    fn rejects_trade_rows_not_bound_by_the_completion_digest() {
        let temp = tempfile::tempdir().unwrap();
        let rows = SURFACES.map(|surface| {
            let mut row = json!({"schema":ROW_SCHEMA,"surface":surface,"market_id":"market-1","condition_id":"condition-1","symbol":"BTCUSDT","window_secs":300});
            if surface == "polymarket_trade" { row["record_id"] = json!("trade-not-in-completion"); }
            row
        });
        let triplet = write_triplet_rows(&temp, &rows);
        let mut value: Value = serde_json::from_slice(&fs::read(&triplet.manifest).unwrap()).unwrap();
        value["validated_inputs"]["reference"]["trade_completions"]["market-1"]["trade_record_ids_sha256"] = json!("2".repeat(64));
        rewrite_read_only(&triplet.manifest, format!("{}\n", serde_json::to_string(&value).unwrap()));
        let error = seal_polymarket_evidence_triplet(&triplet, &trust(&triplet)).unwrap_err();
        assert!(error.to_string().contains("trade rows do not match event-local completion proof"));
    }

    #[test]
    #[rustfmt::skip]
    fn rejects_trade_rows_for_an_unselected_market() {
        let temp = tempfile::tempdir().unwrap();
        let mut rows = Vec::from(SURFACES.map(|surface| {
            let mut row = json!({"schema":ROW_SCHEMA,"surface":surface,"market_id":"market-1","condition_id":"condition-1","symbol":"BTCUSDT","window_secs":300});
            if surface == "polymarket_trade" { row["record_id"] = json!("trade-1"); }
            row
        }));
        rows.push(json!({"schema":ROW_SCHEMA,"surface":"polymarket_trade","market_id":"market-2","condition_id":"condition-2","symbol":"BTCUSDT","window_secs":300,"record_id":"trade-2"}));
        let triplet = write_triplet_rows(&temp, &rows);
        let error = seal_polymarket_evidence_triplet(&triplet, &trust(&triplet)).unwrap_err();
        assert!(error.to_string().contains("trade rows do not match event-local completion proof"));
    }

    #[test]
    #[rustfmt::skip]
    fn rejects_noncanonical_or_nonregular_data() {
        assert!(frame_ndjson(b"{}\r\n", &BTreeMap::new(), &BTreeMap::new()).is_err());
        let temp = tempfile::tempdir().unwrap();
        let triplet = write_triplet(&temp);
        let fifo_anchor = trust(&triplet);
        fs::remove_file(&triplet.data).unwrap();
        let path = CString::new(triplet.data.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), PUBLISHED_MODE.try_into().unwrap()) }, 0);
        assert!(seal_polymarket_evidence_triplet(&triplet, &fifo_anchor).is_err());
    }
}
