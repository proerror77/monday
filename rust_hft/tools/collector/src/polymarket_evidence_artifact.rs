use crate::polymarket_research_import::ResearchSegmentValidationReport;
use crate::polymarket_research_normalize::{
    normalize_polymarket_candidate_evidence, normalize_polymarket_evidence,
    NormalizedPolymarketCandidateEvidence, NormalizedPolymarketEvidence,
    PolymarketCandidateEvidenceReport, PolymarketEvidenceConfig, PolymarketEvidenceReport,
    EXPLICIT_MARKET_ID_SELECTION,
};
use crate::polymarket_research_select::semantic_index;
use crate::polymarket_upload::ensure_canonical_directory;
use anyhow::{anyhow, bail, ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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
const CANDIDATE_SURFACES: [&str; 5] = ["down_book", "reference", "settlement", "trades", "up_book"];

const CONTENT_DIGEST_SEMANTICS: &str =
    "content_sha256 binds the published NDJSON bytes only; it is not a snapshot_contract_hash";
const PUBLISHED_MODE: u32 = 0o444;
const PRODUCER_VERIFIER_CONTRACT: &str = "monday.polymarket.normalized_evidence.v2";
const CANDIDATE_VERIFIER_CONTRACT: &str = "monday.polymarket.normalized_candidate_evidence.v1";

#[derive(Debug, Clone)]
pub struct PolymarketEvidenceArtifactConfig {
    pub evidence: PolymarketEvidenceConfig,
    pub output_root: PathBuf,
    pub qualification: PolymarketProducerQualificationConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedPolymarketCandidateEvidence {
    pub schema: &'static str,
    pub outcome: ImmutablePublicationOutcome,
    pub data_path: PathBuf,
    pub manifest_path: PathBuf,
    pub success_path: PathBuf,
    pub published_digests: PublishedPolymarketEvidenceDigests,
    pub evidence: PolymarketCandidateEvidenceReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedPolymarketEvidenceBatch {
    pub evidence: Vec<PublishedPolymarketEvidence>,
    pub candidates: Vec<PublishedPolymarketCandidateEvidence>,
    pub qualifications: Vec<PublishedPolymarketEventQualification>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequiredEvidenceSurface {
    UpBook,
    DownBook,
    Trades,
    Reference,
    Settlement,
}
impl RequiredEvidenceSurface {
    pub const ALL: [Self; 5] = [
        Self::UpBook,
        Self::DownBook,
        Self::Trades,
        Self::Reference,
        Self::Settlement,
    ];
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProducerRequestStatus {
    Succeeded,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerRequestOutcome {
    pub surface: RequiredEvidenceSurface,
    pub status: ProducerRequestStatus,
    pub completed_at: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSurfaceStatus {
    Complete,
    Incomplete,
    Contradictory,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerSourceClocks {
    pub opened_at: String,
    pub closed_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerSequenceState {
    pub start: u64,
    pub end: u64,
    pub gaps: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolymarketProducerProvenance {
    pub source_sha: String,
    pub image_digest: String,
    pub configuration_sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PolymarketEventQualificationInput {
    pub market_id: String,
    pub symbol: String,
    pub event_start: String,
    pub event_end: String,
    pub up_token_id: String,
    pub down_token_id: String,
    #[serde(skip)]
    pub verified_token_ids: Option<[String; 2]>,
    pub source_closed: bool,
    pub up_book: EvidenceSurfaceStatus,
    pub down_book: EvidenceSurfaceStatus,
    pub trades: EvidenceSurfaceStatus,
    pub reference: EvidenceSurfaceStatus,
    pub settlement: EvidenceSurfaceStatus,
    pub request_outcomes: Option<Vec<ProducerRequestOutcome>>,
    pub source_clocks: Option<ProducerSourceClocks>,
    pub sequence: ProducerSequenceState,
    pub evidence_digests: PublishedPolymarketEvidenceDigests,
    pub token_identity_matches: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolymarketProducerQualificationConfig {
    pub producer: PolymarketProducerProvenance,
    pub events: Vec<PolymarketEventQualificationInput>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolymarketEventQualificationState {
    Ready,
    Partial,
    Rejected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PolymarketEventQualificationReason {
    SourceStillOpen,
    UpBookIncomplete,
    DownBookIncomplete,
    TradesIncomplete,
    ReferenceIncomplete,
    SettlementIncomplete,
    ContradictoryEvidence,
    MissingRequestOutcome,
    FailedRequestWithCompleteEvidence,
    MissingSourceClocks,
    InvalidSourceClocks,
    SequenceGap,
    TokenIdentityMismatch,
    InvalidProducerProvenance,
    InvalidEvidenceDigest,
    UnsupportedProductContract,
    VerifierRejected,
    IndependentVerificationRequired,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolymarketEventQualificationRecord {
    pub schema: &'static str,
    pub verifier_contract: &'static str,
    pub market_id: String,
    pub symbol: String,
    pub event_start: String,
    pub event_end: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub verified_token_ids: Option<[String; 2]>,
    pub state: PolymarketEventQualificationState,
    pub reasons: Vec<PolymarketEventQualificationReason>,
    pub retry: bool,
    pub producer: PolymarketProducerProvenance,
    pub source_closed: bool,
    pub up_book: EvidenceSurfaceStatus,
    pub down_book: EvidenceSurfaceStatus,
    pub trades: EvidenceSurfaceStatus,
    pub reference: EvidenceSurfaceStatus,
    pub settlement: EvidenceSurfaceStatus,
    pub request_outcomes: Option<Vec<ProducerRequestOutcome>>,
    pub source_clocks: Option<ProducerSourceClocks>,
    pub sequence: ProducerSequenceState,
    pub evidence_digests: PublishedPolymarketEvidenceDigests,
    pub token_identity_matches: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImmutablePublicationOutcome {
    Published,
    Unchanged,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublishedPolymarketEventQualification {
    pub path: PathBuf,
    pub outcome: ImmutablePublicationOutcome,
    pub record: PolymarketEventQualificationRecord,
}
fn classify_polymarket_event(
    input: &PolymarketEventQualificationInput,
    producer: &PolymarketProducerProvenance,
    verifier_rejected: bool,
) -> PolymarketEventQualificationRecord {
    let mut reasons = Vec::new();
    let request_surfaces = input.request_outcomes.as_ref().map(|outcomes| {
        outcomes
            .iter()
            .map(|outcome| outcome.surface)
            .collect::<BTreeSet<_>>()
    });
    let mut permanent_failure = request_surfaces.as_ref().is_none_or(|surfaces| {
        surfaces.len() != RequiredEvidenceSurface::ALL.len()
            || input
                .request_outcomes
                .as_ref()
                .is_none_or(|outcomes| outcomes.len() != surfaces.len())
    });
    if permanent_failure {
        reasons.push(PolymarketEventQualificationReason::MissingRequestOutcome);
    }
    if input.request_outcomes.as_ref().is_some_and(|outcomes| {
        outcomes.iter().any(|outcome| {
            outcome.status == ProducerRequestStatus::Failed
                && match outcome.surface {
                    RequiredEvidenceSurface::UpBook => input.up_book,
                    RequiredEvidenceSurface::DownBook => input.down_book,
                    RequiredEvidenceSurface::Trades => input.trades,
                    RequiredEvidenceSurface::Reference => input.reference,
                    RequiredEvidenceSurface::Settlement => input.settlement,
                } == EvidenceSurfaceStatus::Complete
        })
    }) {
        reasons.push(PolymarketEventQualificationReason::FailedRequestWithCompleteEvidence);
        permanent_failure = true;
    }
    if input.sequence.gaps != 0 || input.sequence.end < input.sequence.start {
        reasons.push(PolymarketEventQualificationReason::SequenceGap);
        permanent_failure = true;
    }
    if !input.token_identity_matches
        || input.up_token_id.is_empty()
        || input.down_token_id.is_empty()
        || input.up_token_id == input.down_token_id
    {
        reasons.push(PolymarketEventQualificationReason::TokenIdentityMismatch);
        permanent_failure = true;
    }
    match &input.source_clocks {
        None => {
            reasons.push(PolymarketEventQualificationReason::MissingSourceClocks);
            permanent_failure = true;
        }
        Some(clocks) => {
            let parsed = [
                input.event_start.as_str(),
                input.event_end.as_str(),
                clocks.opened_at.as_str(),
                clocks.closed_at.as_str(),
            ]
            .map(|value| {
                DateTime::parse_from_rfc3339(value).map(|value| value.with_timezone(&Utc))
            });
            if let [Ok(event_start), Ok(event_end), Ok(opened_at), Ok(closed_at)] = parsed {
                let request_clock_invalid =
                    input.request_outcomes.as_ref().is_some_and(|outcomes| {
                        outcomes.iter().any(|outcome| {
                            DateTime::parse_from_rfc3339(&outcome.completed_at)
                                .map(|completed_at| {
                                    let completed_at = completed_at.with_timezone(&Utc);
                                    completed_at < opened_at
                                        || completed_at > closed_at
                                        || (outcome.surface == RequiredEvidenceSurface::Settlement
                                            && outcome.status == ProducerRequestStatus::Succeeded
                                            && completed_at < event_end)
                                })
                                .unwrap_or(true)
                        })
                    });
                if event_end <= event_start
                    || opened_at > event_start
                    || closed_at < event_end
                    || request_clock_invalid
                {
                    reasons.push(PolymarketEventQualificationReason::InvalidSourceClocks);
                    permanent_failure = true;
                }
                if !["BTCUSDT", "SOLUSDT"].contains(&input.symbol.as_str())
                    || (event_end - event_start).num_seconds() != 300
                {
                    reasons.push(PolymarketEventQualificationReason::UnsupportedProductContract);
                    permanent_failure = true;
                }
            } else {
                reasons.push(PolymarketEventQualificationReason::InvalidSourceClocks);
                permanent_failure = true;
            }
        }
    }
    if [
        input.up_book,
        input.down_book,
        input.trades,
        input.reference,
        input.settlement,
    ]
    .contains(&EvidenceSurfaceStatus::Contradictory)
    {
        reasons.push(PolymarketEventQualificationReason::ContradictoryEvidence);
        permanent_failure = true;
    }
    if !is_lower_hex(&producer.source_sha, 40)
        || !producer
            .image_digest
            .strip_prefix("sha256:")
            .is_some_and(|digest| is_lower_hex(digest, 64))
        || !is_lower_hex(&producer.configuration_sha256, 64)
    {
        reasons.push(PolymarketEventQualificationReason::InvalidProducerProvenance);
        permanent_failure = true;
    }
    if !is_lower_hex(&input.evidence_digests.expected_content_sha256, 64)
        || !is_lower_hex(&input.evidence_digests.expected_manifest_sha256, 64)
    {
        reasons.push(PolymarketEventQualificationReason::InvalidEvidenceDigest);
        permanent_failure = true;
    }
    if !input.source_closed {
        reasons.push(PolymarketEventQualificationReason::SourceStillOpen);
    }
    for (status, reason) in [
        (
            input.up_book,
            PolymarketEventQualificationReason::UpBookIncomplete,
        ),
        (
            input.down_book,
            PolymarketEventQualificationReason::DownBookIncomplete,
        ),
        (
            input.trades,
            PolymarketEventQualificationReason::TradesIncomplete,
        ),
        (
            input.reference,
            PolymarketEventQualificationReason::ReferenceIncomplete,
        ),
        (
            input.settlement,
            PolymarketEventQualificationReason::SettlementIncomplete,
        ),
    ] {
        if status == EvidenceSurfaceStatus::Incomplete {
            reasons.push(reason);
        }
    }
    let state = if permanent_failure {
        PolymarketEventQualificationState::Rejected
    } else if reasons.is_empty() {
        PolymarketEventQualificationState::Ready
    } else {
        PolymarketEventQualificationState::Partial
    };
    if verifier_rejected {
        reasons.push(PolymarketEventQualificationReason::VerifierRejected);
        permanent_failure = true;
    }
    let state = if permanent_failure {
        PolymarketEventQualificationState::Rejected
    } else {
        state
    };
    let mut request_outcomes = input.request_outcomes.clone();
    if let Some(outcomes) = &mut request_outcomes {
        outcomes.sort_by_key(|outcome| outcome.surface);
    }
    PolymarketEventQualificationRecord {
        schema: "monday.polymarket.event_qualification.v1",
        verifier_contract: PRODUCER_VERIFIER_CONTRACT,
        market_id: input.market_id.clone(),
        symbol: input.symbol.clone(),
        event_start: input.event_start.clone(),
        event_end: input.event_end.clone(),
        up_token_id: input.up_token_id.clone(),
        down_token_id: input.down_token_id.clone(),
        verified_token_ids: input.verified_token_ids.clone(),
        state,
        reasons,
        retry: state == PolymarketEventQualificationState::Partial,
        producer: producer.clone(),
        source_closed: input.source_closed,
        up_book: input.up_book,
        down_book: input.down_book,
        trades: input.trades,
        reference: input.reference,
        settlement: input.settlement,
        request_outcomes,
        source_clocks: input.source_clocks.clone(),
        sequence: input.sequence.clone(),
        evidence_digests: input.evidence_digests.clone(),
        token_identity_matches: input.token_identity_matches,
    }
}

fn is_lower_hex(value: &str, bytes: usize) -> bool {
    value.len() == bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn publish_qualification_record(
    output_root: &Path,
    record: PolymarketEventQualificationRecord,
) -> Result<PublishedPolymarketEventQualification> {
    require_secure_publication_platform()?;
    if record.market_id.is_empty()
        || !record
            .market_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("qualification market_id is not a safe immutable path identity");
    }
    ensure_canonical_directory(output_root)?;
    let digest = &record.evidence_digests.expected_content_sha256;
    let identity = if is_lower_hex(digest, 64) {
        digest.clone()
    } else {
        hex::encode(Sha256::digest(digest.as_bytes()))
    };
    let directory_path = output_root.join(format!("sha256={identity}"));
    ensure_canonical_directory(&directory_path)?;
    let path = directory_path.join(format!(
        "polymarket-event-qualification.{}.{}.json",
        record.market_id, identity
    ));
    let mut bytes = serde_json::to_vec(&record)?;
    bytes.push(b'\n');
    let directory = bind_directory(&directory_path)?;
    let outcome = if install_no_clobber(&directory_path, &directory, &path, &bytes)? {
        ImmutablePublicationOutcome::Published
    } else {
        ImmutablePublicationOutcome::Unchanged
    };
    Ok(PublishedPolymarketEventQualification {
        path,
        outcome,
        record,
    })
}

fn verified_event_input(
    evidence: &NormalizedPolymarketEvidence,
    declared: &PolymarketEventQualificationInput,
) -> Result<PolymarketEventQualificationInput> {
    validate_dataset(evidence)?;
    ensure!(
        evidence.report.market_ids == [declared.market_id.clone()],
        "event-local verifier result does not match its declared market"
    );
    let contract = evidence
        .ndjson
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|row| row["surface"] == "market_contract")
        .collect::<Vec<_>>();
    ensure!(
        contract.len() == 1,
        "event-local verifier must emit one contract"
    );
    let contract = &contract[0];
    let tokens: [String; 2] = serde_json::from_value(contract["source_token_ids"].clone())?;
    let outcomes: [String; 2] = serde_json::from_value(contract["source_outcomes"].clone())?;
    let up = semantic_index(&outcomes, true)?;
    let down = semantic_index(&outcomes, false)?;
    for (field, expected) in [
        ("market_id", declared.market_id.as_str()),
        ("symbol", declared.symbol.as_str()),
        ("event_start", declared.event_start.as_str()),
        ("event_end", declared.event_end.as_str()),
    ] {
        ensure!(
            contract[field].as_str() == Some(expected),
            "verified contract {field} does not match declaration"
        );
    }
    let mut verified = declared.clone();
    verified.token_identity_matches =
        declared.up_token_id == tokens[up] && declared.down_token_id == tokens[down];
    verified.verified_token_ids = Some([tokens[up].clone(), tokens[down].clone()]);
    verified.source_closed = true;
    verified.up_book = EvidenceSurfaceStatus::Complete;
    verified.down_book = EvidenceSurfaceStatus::Complete;
    verified.trades = EvidenceSurfaceStatus::Complete;
    verified.reference = EvidenceSurfaceStatus::Complete;
    verified.settlement = EvidenceSurfaceStatus::Complete;
    verified.evidence_digests.expected_content_sha256 = evidence.report.content_sha256.clone();
    Ok(verified)
}

fn candidate_event_input(
    evidence: &NormalizedPolymarketCandidateEvidence,
    declared: &PolymarketEventQualificationInput,
) -> Result<PolymarketEventQualificationInput> {
    validate_candidate_dataset(evidence)?;
    ensure!(
        evidence.report.market_ids == [declared.market_id.clone()],
        "event-local candidate does not match its declared market"
    );
    let contracts = evidence
        .ndjson
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(serde_json::from_slice::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|row| row["surface"] == "market_contract")
        .collect::<Vec<_>>();
    ensure!(contracts.len() == 1, "candidate must emit one contract");
    let contract = &contracts[0];
    for (field, expected) in [
        ("market_id", declared.market_id.as_str()),
        ("symbol", declared.symbol.as_str()),
        ("event_start", declared.event_start.as_str()),
        ("event_end", declared.event_end.as_str()),
    ] {
        ensure!(
            contract[field].as_str() == Some(expected),
            "candidate contract {field} does not match declaration"
        );
    }
    let tokens: [String; 2] = serde_json::from_value(contract["source_token_ids"].clone())?;
    let outcomes: [String; 2] = serde_json::from_value(contract["source_outcomes"].clone())?;
    let up = semantic_index(&outcomes, true)?;
    let down = semantic_index(&outcomes, false)?;
    let mut candidate = declared.clone();
    candidate.token_identity_matches =
        candidate.up_token_id == tokens[up] && candidate.down_token_id == tokens[down];
    candidate.verified_token_ids = Some([tokens[up].clone(), tokens[down].clone()]);
    candidate.evidence_digests.expected_content_sha256 = evidence.report.content_sha256.clone();
    Ok(candidate)
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
    market_ids: &'a [String],
    symbols: &'a [String],
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
    if report.schema != "monday.polymarket.normalized_evidence.v2"
        || digest != report.content_sha256
        || report.content_bytes != u64::try_from(dataset.ndjson.len())?
        || report.rows != rows
        || report.rows != report.surface_counts.values().sum::<u64>()
        || report.events == 0
        || report.events != u64::try_from(report.market_ids.len())?
        || report.surface_counts.get("market_contract") != Some(&report.events)
        || report.surface_counts.get("official_settlement_evidence") != Some(&report.events)
        || report.market_ids.is_empty()
        || report
            .market_ids
            .iter()
            .any(|market_id| market_id.is_empty() || market_id.chars().any(char::is_whitespace))
        || report.market_ids.iter().collect::<BTreeSet<_>>().len() != report.market_ids.len()
        || report.symbols.is_empty()
        || report
            .symbols
            .iter()
            .any(|symbol| !["BTCUSDT", "SOLUSDT"].contains(&symbol.as_str()))
        || report.symbols.iter().collect::<BTreeSet<_>>().len() != report.symbols.len()
        || report.window_secs != 300
        || report.event_selection != EXPLICIT_MARKET_ID_SELECTION
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
        schema: "monday.polymarket.evidence_artifact.v3",
        file,
        format: "ndjson",
        content_sha256: &report.content_sha256,
        content_bytes: report.content_bytes,
        rows: report.rows,
        events: report.events,
        surface_counts: &report.surface_counts,
        event_start_gte: &report.event_start_gte,
        event_start_lt: &report.event_start_lt,
        market_ids: &report.market_ids,
        symbols: &report.symbols,
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

fn validate_candidate_dataset(dataset: &NormalizedPolymarketCandidateEvidence) -> Result<()> {
    let report = &dataset.report;
    let rows = u64::try_from(dataset.ndjson.iter().filter(|byte| **byte == b'\n').count())?;
    if report.schema != "monday.polymarket.normalized_candidate_evidence.v1"
        || report.content_sha256 != hex::encode(Sha256::digest(&dataset.ndjson))
        || report.content_bytes != u64::try_from(dataset.ndjson.len())?
        || report.contract_rows != 1
        || report.rows != rows
        || report.rows != report.contract_rows + report.surface_counts.values().sum::<u64>()
        || report.market_ids.len() != 1
        || report.market_ids[0].is_empty()
        || report.market_ids[0].chars().any(char::is_whitespace)
        || report.symbols.len() != 1
        || !["BTCUSDT", "SOLUSDT"].contains(&report.symbols[0].as_str())
        || report.window_secs != 300
        || report.event_selection != EXPLICIT_MARKET_ID_SELECTION
        || report.surface_counts.len() != CANDIDATE_SURFACES.len()
        || CANDIDATE_SURFACES
            .iter()
            .any(|surface| !report.surface_counts.contains_key(*surface))
    {
        bail!("normalized polymarket candidate identity is inconsistent");
    }
    Ok(())
}

fn candidate_manifest_bytes(
    report: &PolymarketCandidateEvidenceReport,
    file: &str,
) -> Result<Vec<u8>> {
    let mut semantics = recording_semantics();
    semantics.trades = "canonical v2 records when present; a collector completion proof is verified when present but may be absent";
    let manifest = serde_json::json!({
        "schema": "monday.polymarket.candidate_evidence_artifact.v1",
        "file": file,
        "format": "ndjson",
        "content_sha256": report.content_sha256,
        "content_bytes": report.content_bytes,
        "rows": report.rows,
        "contract_rows": report.contract_rows,
        "surface_counts": report.surface_counts,
        "event_start_gte": report.event_start_gte,
        "event_start_lt": report.event_start_lt,
        "market_ids": report.market_ids,
        "symbols": report.symbols,
        "window_secs": report.window_secs,
        "event_selection": report.event_selection,
        "evidence_scope": "untrusted producer candidate only; not Ready, execution authorization, or evaluator labels",
        "content_digest_semantics": CONTENT_DIGEST_SEMANTICS,
        "recording_semantics": semantics,
        "trust_boundary": report.trust_boundary,
        "validated_inputs": report.validated_inputs,
    });
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
fn install_no_clobber(_: &Path, _: &File, _: &Path, _: &[u8]) -> Result<bool> {
    require_secure_publication_platform()?;
    Ok(false)
}

#[cfg(target_os = "linux")]
fn install_no_clobber(
    directory_path: &Path,
    directory: &File,
    path: &Path,
    bytes: &[u8],
) -> Result<bool> {
    if exact_or_missing(directory_path, directory, path, bytes)? {
        return Ok(false);
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
    let created = if link_anonymous(directory, &output, &name)? != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            if !exact_or_missing(directory_path, directory, path, bytes)? {
                bail!("immutable artifact target disappeared during publication");
            }
            false
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
    } else {
        if !exact_or_missing(directory_path, directory, path, bytes)? {
            bail!("immutable artifact target disappeared after publication");
        }
        true
    };
    directory.sync_all()?;
    verify_bound_directory(directory_path, directory)?;
    Ok(created)
}

fn publish_triplet(
    data_path: &Path,
    manifest_path: &Path,
    success_path: &Path,
    bytes: &ArtifactBytes<'_>,
) -> Result<ImmutablePublicationOutcome> {
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
        return Ok(ImmutablePublicationOutcome::Unchanged);
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
    Ok(ImmutablePublicationOutcome::Published)
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
    let data_name = format!("polymarket-evidence-5m.{digest}.ndjson");
    let data_path = directory.join(&data_name);
    let manifest_path = directory.join(format!("{data_name}.manifest.json"));
    let success_path = directory.join(format!("{data_name}._SUCCESS"));
    let manifest = manifest_bytes(&evidence.report, &data_name)?;
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest));
    let success = format!("{digest}\n");
    let _ = publish_triplet(
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

fn publish_candidate_normalized(
    output_root: &Path,
    evidence: NormalizedPolymarketCandidateEvidence,
) -> Result<PublishedPolymarketCandidateEvidence> {
    validate_candidate_dataset(&evidence)?;
    ensure_canonical_directory(output_root)?;
    let digest = &evidence.report.content_sha256;
    let directory = output_root.join(format!("sha256={digest}"));
    ensure_canonical_directory(&directory)?;
    let data_name = format!("polymarket-candidate-evidence-5m.{digest}.ndjson");
    let data_path = directory.join(&data_name);
    let manifest_path = directory.join(format!("{data_name}.manifest.json"));
    let success_path = directory.join(format!("{data_name}._SUCCESS"));
    let manifest = candidate_manifest_bytes(&evidence.report, &data_name)?;
    let published_digests = PublishedPolymarketEvidenceDigests {
        expected_content_sha256: digest.clone(),
        expected_manifest_sha256: hex::encode(Sha256::digest(&manifest)),
    };
    let success = format!("{digest}\n");
    let outcome = publish_triplet(
        &data_path,
        &manifest_path,
        &success_path,
        &ArtifactBytes {
            data: &evidence.ndjson,
            manifest: &manifest,
            success: success.as_bytes(),
        },
    )?;
    Ok(PublishedPolymarketCandidateEvidence {
        schema: "monday.polymarket.published_candidate_evidence.v1",
        outcome,
        data_path,
        manifest_path,
        success_path,
        published_digests,
        evidence: evidence.report,
    })
}

pub fn publish_polymarket_evidence(
    config: &PolymarketEvidenceArtifactConfig,
) -> Result<PublishedPolymarketEvidenceBatch> {
    publish_polymarket_evidence_with(
        config,
        normalize_polymarket_evidence,
        normalize_polymarket_candidate_evidence,
    )
}

fn retryable_infrastructure_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() != std::io::ErrorKind::InvalidData)
    })
}

fn publish_polymarket_evidence_with(
    config: &PolymarketEvidenceArtifactConfig,
    mut normalize: impl FnMut(&PolymarketEvidenceConfig) -> Result<NormalizedPolymarketEvidence>,
    mut normalize_candidate: impl FnMut(
        &PolymarketEvidenceConfig,
    ) -> Result<NormalizedPolymarketCandidateEvidence>,
) -> Result<PublishedPolymarketEvidenceBatch> {
    require_secure_publication_platform()?;
    let declared = config
        .qualification
        .events
        .iter()
        .map(|event| (event.market_id.as_str(), event))
        .collect::<BTreeMap<_, _>>();
    ensure!(
        declared.len() == config.qualification.events.len()
            && declared.len() == config.evidence.market_ids.len()
            && config
                .evidence
                .market_ids
                .iter()
                .all(|market_id| declared.contains_key(market_id.as_str())),
        "qualification inputs must match the selected market IDs exactly"
    );
    let mut published_evidence = Vec::new();
    let mut published_candidates = Vec::new();
    let mut qualifications = Vec::new();
    let mut infrastructure_failure = None;
    for market_id in &config.evidence.market_ids {
        let declared = declared[market_id.as_str()];
        let declared_record =
            classify_polymarket_event(declared, &config.qualification.producer, false);
        if declared_record.state != PolymarketEventQualificationState::Ready {
            if !declared.source_closed {
                qualifications.push(publish_qualification_record(
                    &config.output_root,
                    declared_record,
                )?);
                continue;
            }
            let mut event_config = config.evidence.clone();
            event_config.market_ids = vec![market_id.clone()];
            match normalize_candidate(&event_config).and_then(|evidence| {
                let candidate = candidate_event_input(&evidence, declared)?;
                Ok((evidence, candidate))
            }) {
                Ok((evidence, mut candidate)) => {
                    let published = publish_candidate_normalized(&config.output_root, evidence)?;
                    candidate.evidence_digests = published.published_digests.clone();
                    let mut record = classify_polymarket_event(
                        &candidate,
                        &config.qualification.producer,
                        false,
                    );
                    if record.state == PolymarketEventQualificationState::Ready {
                        record.state = PolymarketEventQualificationState::Partial;
                        record.reasons = vec![
                            PolymarketEventQualificationReason::IndependentVerificationRequired,
                        ];
                        record.retry = true;
                    }
                    record.verifier_contract = CANDIDATE_VERIFIER_CONTRACT;
                    qualifications.push(publish_qualification_record(&config.output_root, record)?);
                    published_candidates.push(published);
                }
                Err(error) if retryable_infrastructure_failure(&error) => {
                    infrastructure_failure.get_or_insert(error);
                }
                Err(_deterministic_failure) => {
                    let mut record =
                        classify_polymarket_event(declared, &config.qualification.producer, true);
                    record.verifier_contract = CANDIDATE_VERIFIER_CONTRACT;
                    qualifications.push(publish_qualification_record(&config.output_root, record)?);
                }
            }
            continue;
        }
        let mut event_config = config.evidence.clone();
        event_config.market_ids = vec![market_id.clone()];
        match normalize(&event_config).and_then(|evidence| {
            let verified = verified_event_input(&evidence, declared)?;
            Ok((evidence, verified))
        }) {
            Ok((evidence, mut verified)) => {
                let published = publish_normalized(&config.output_root, evidence)?;
                verified.evidence_digests = published.published_digests.clone();
                let record =
                    classify_polymarket_event(&verified, &config.qualification.producer, false);
                qualifications.push(publish_qualification_record(&config.output_root, record)?);
                published_evidence.push(published);
            }
            Err(error) if retryable_infrastructure_failure(&error) => {
                infrastructure_failure.get_or_insert(error);
            }
            Err(_deterministic_failure) => {
                let record =
                    classify_polymarket_event(declared, &config.qualification.producer, true);
                qualifications.push(publish_qualification_record(&config.output_root, record)?);
            }
        }
    }
    if let Some(error) = infrastructure_failure {
        return Err(error.context("retryable event-local evidence verification failure"));
    }
    Ok(PublishedPolymarketEvidenceBatch {
        evidence: published_evidence,
        candidates: published_candidates,
        qualifications,
    })
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

    use PolymarketEventQualificationReason::*;
    use PolymarketEventQualificationState::*;
    fn complete_event(market_id: &str) -> PolymarketEventQualificationInput {
        PolymarketEventQualificationInput {
            market_id: market_id.to_owned(),
            symbol: "BTCUSDT".to_owned(),
            event_start: "2026-07-17T05:30:00Z".to_owned(),
            event_end: "2026-07-17T05:35:00Z".to_owned(),
            up_token_id: format!("{market_id}-up"),
            down_token_id: format!("{market_id}-down"),
            verified_token_ids: None,
            source_closed: true,
            up_book: EvidenceSurfaceStatus::Complete,
            down_book: EvidenceSurfaceStatus::Complete,
            trades: EvidenceSurfaceStatus::Complete,
            reference: EvidenceSurfaceStatus::Complete,
            settlement: EvidenceSurfaceStatus::Complete,
            request_outcomes: Some(
                RequiredEvidenceSurface::ALL
                    .into_iter()
                    .map(|surface| ProducerRequestOutcome {
                        surface,
                        status: ProducerRequestStatus::Succeeded,
                        completed_at: "2026-07-17T05:36:00Z".to_owned(),
                    })
                    .collect(),
            ),
            source_clocks: Some(ProducerSourceClocks {
                opened_at: "2026-07-17T05:29:59Z".to_owned(),
                closed_at: "2026-07-17T05:36:00Z".to_owned(),
            }),
            sequence: ProducerSequenceState {
                start: 1,
                end: 7,
                gaps: 0,
            },
            evidence_digests: PublishedPolymarketEvidenceDigests {
                expected_content_sha256: "1".repeat(64),
                expected_manifest_sha256: "2".repeat(64),
            },
            token_identity_matches: true,
        }
    }
    fn producer_provenance() -> PolymarketProducerProvenance {
        PolymarketProducerProvenance {
            source_sha: "3".repeat(40),
            image_digest: format!("sha256:{}", "4".repeat(64)),
            configuration_sha256: "5".repeat(64),
        }
    }
    #[cfg(target_os = "linux")]
    fn candidate_evidence(market_id: &str) -> NormalizedPolymarketCandidateEvidence {
        fn segment(dataset: &str) -> SegmentIdentity {
            SegmentIdentity {
                schema: "monday.polymarket.raw.v1".into(),
                venue: "polymarket".into(),
                dataset: dataset.into(),
                date: "2026-07-17".into(),
                hour: "05".into(),
                file: format!("{dataset}.ndjson.zst"),
                bytes: 1,
                sha256: "0".repeat(64),
                events: 1,
                start_sequence: 1,
                end_sequence: 1,
                sequence_gaps: 0,
                start_recorded_at: "2026-07-17T05:30:00Z".into(),
                end_recorded_at: "2026-07-17T05:35:00Z".into(),
                source_file: format!("{dataset}.ndjson"),
                replay_scope: "fixture".into(),
                recording_policy: serde_json::json!({}),
                record_id_versions: serde_json::json!([]),
                event_types: BTreeMap::new(),
                trade_completions: BTreeMap::new(),
            }
        }
        let ndjson = format!(
            "{{\"schema\":\"monday.polymarket.evidence_row.v1\",\"surface\":\"market_contract\",\"market_id\":\"{market_id}\",\"symbol\":\"BTCUSDT\",\"event_start\":\"2026-07-17T05:30:00Z\",\"event_end\":\"2026-07-17T05:35:00Z\",\"source_token_ids\":[\"{market_id}-up\",\"{market_id}-down\"],\"source_outcomes\":[\"Up\",\"Down\"]}}\n{{\"schema\":\"monday.polymarket.evidence_row.v1\",\"surface\":\"orderbook_snapshot\",\"market_id\":\"{market_id}\",\"token_id\":\"{market_id}-up\"}}\n"
        )
        .into_bytes();
        NormalizedPolymarketCandidateEvidence {
            report: PolymarketCandidateEvidenceReport {
                schema: "monday.polymarket.normalized_candidate_evidence.v1",
                content_sha256: hex::encode(Sha256::digest(&ndjson)),
                content_bytes: u64::try_from(ndjson.len()).unwrap(),
                rows: 2,
                contract_rows: 1,
                surface_counts: BTreeMap::from([
                    ("down_book".into(), 0),
                    ("reference".into(), 0),
                    ("settlement".into(), 0),
                    ("trades".into(), 0),
                    ("up_book".into(), 1),
                ]),
                event_start_gte: "2026-07-17T05:30:00Z".into(),
                event_start_lt: "2026-07-17T05:35:00Z".into(),
                market_ids: vec![market_id.into()],
                symbols: vec!["BTCUSDT".into()],
                window_secs: 300,
                event_selection: EXPLICIT_MARKET_ID_SELECTION,
                trust_boundary: "untrusted producer candidate",
                validated_inputs: ResearchSegmentValidationReport {
                    schema: "monday.polymarket.research_segment_validation.v2",
                    market: segment("crypto_expiry"),
                    references: vec![segment("crypto_expiry_reference")],
                },
            },
            ndjson,
        }
    }
    fn assert_classification(
        event: PolymarketEventQualificationInput,
        verifier_rejected: bool,
        state: PolymarketEventQualificationState,
        reason: PolymarketEventQualificationReason,
    ) {
        let record = classify_polymarket_event(&event, &producer_provenance(), verifier_rejected);
        assert_eq!(record.state, state);
        assert!(record.reasons.contains(&reason));
    }
    #[test]
    fn counterexamples_are_partial_or_terminal_without_synthesized_evidence() {
        assert!(!retryable_infrastructure_failure(
            &std::io::Error::new(std::io::ErrorKind::InvalidData, "corrupt evidence").into()
        ));
        assert!(retryable_infrastructure_failure(
            &std::io::Error::from(std::io::ErrorKind::TimedOut).into()
        ));
        let mut event = complete_event("market-1");
        event.down_book = EvidenceSurfaceStatus::Incomplete;
        event.request_outcomes.as_mut().unwrap()[1].status = ProducerRequestStatus::Failed;
        assert_classification(event.clone(), false, Partial, DownBookIncomplete);
        assert_classification(event, true, Rejected, VerifierRejected);
        let mut event = complete_event("market-1");
        event.request_outcomes = None;
        assert_classification(event, false, Rejected, MissingRequestOutcome);
        let mut event = complete_event("market-1");
        event.sequence.gaps = 1;
        assert_classification(event, false, Rejected, SequenceGap);
        let mut event = complete_event("market-1");
        event.token_identity_matches = false;
        assert_classification(event, false, Rejected, TokenIdentityMismatch);
        let mut event = complete_event("market-1");
        event.settlement = EvidenceSurfaceStatus::Incomplete;
        event.request_outcomes.as_mut().unwrap()[4].status = ProducerRequestStatus::Failed;
        assert_classification(event, false, Partial, SettlementIncomplete);
        let mut event = complete_event("market-1");
        event.request_outcomes.as_mut().unwrap()[4].completed_at = "2026-07-17T05:34:59Z".into();
        assert_classification(event, false, Rejected, InvalidSourceClocks);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn source_closed_partial_events_publish_candidate_triplets_without_ready() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let mut partial = complete_event("market-partial");
        partial.down_book = EvidenceSurfaceStatus::Incomplete;
        partial.request_outcomes.as_mut().unwrap()[1].status = ProducerRequestStatus::Failed;
        let mut config = PolymarketEvidenceArtifactConfig {
            evidence: PolymarketEvidenceConfig {
                segments: crate::polymarket_research_import::ResearchSegmentValidationConfig {
                    market: crate::polymarket_research_import::ArtifactTriplet {
                        data: PathBuf::new(),
                        manifest: PathBuf::new(),
                        success: PathBuf::new(),
                    },
                    references: Vec::new(),
                },
                event_start_gte: "2026-07-17T05:30:00Z".into(),
                event_start_lt: "2026-07-17T05:35:00Z".into(),
                market_ids: vec![partial.market_id.clone()],
            },
            output_root: root,
            qualification: PolymarketProducerQualificationConfig {
                producer: producer_provenance(),
                events: vec![partial],
            },
        };

        let result = publish_polymarket_evidence_with(
            &config,
            |_| panic!("partial candidate must not enter complete normalization"),
            |selection| Ok(candidate_evidence(&selection.market_ids[0])),
        )
        .unwrap();

        assert_eq!(result.qualifications[0].record.state, Partial);
        assert_eq!(
            result.qualifications[0].record.evidence_digests,
            result.candidates[0].published_digests
        );
        assert_eq!(result.candidates[0].evidence.surface_counts["down_book"], 0);
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&result.candidates[0].manifest_path).unwrap())
                .unwrap();
        assert!(manifest["recording_semantics"]["trades"]
            .as_str()
            .unwrap()
            .contains("may be absent"));

        let mut repaired = complete_event("market-repaired");
        repaired.token_identity_matches = false;
        config.evidence.market_ids = vec![repaired.market_id.clone()];
        config.qualification.events = vec![repaired];
        let result = publish_polymarket_evidence_with(
            &config,
            |_| unreachable!(),
            |selection| Ok(candidate_evidence(&selection.market_ids[0])),
        )
        .unwrap();
        assert_eq!(result.qualifications[0].record.state, Partial);
        assert_eq!(
            result.qualifications[0].record.reasons,
            [IndependentVerificationRequired]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn candidate_triplets_are_event_local_and_no_clobber() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let first = publish_candidate_normalized(&root, candidate_evidence("market-1")).unwrap();
        let replay = publish_candidate_normalized(&root, candidate_evidence("market-1")).unwrap();
        let sibling = publish_candidate_normalized(&root, candidate_evidence("market-2")).unwrap();
        assert_eq!(first.outcome, ImmutablePublicationOutcome::Published);
        assert_eq!(replay.outcome, ImmutablePublicationOutcome::Unchanged);
        assert_eq!(first.evidence.market_ids, ["market-1"]);
        assert_eq!(sibling.evidence.market_ids, ["market-2"]);
        assert_ne!(first.data_path, sibling.data_path);

        fs::set_permissions(&first.data_path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(&first.data_path, b"conflict\n").unwrap();
        assert!(publish_candidate_normalized(&root, candidate_evidence("market-1")).is_err());
    }
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
    fn batch_qualifies_siblings_independently_and_returns_exact_triplet_digests() {
        fn segment(dataset: &str) -> SegmentIdentity {
            let is_reference = dataset == "crypto_expiry_reference";
            let trade_completions = if is_reference {
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
            } else {
                BTreeMap::new()
            };
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

        let ndjson = b"{\"surface\":\"market_contract\",\"market_id\":\"market-1\",\"symbol\":\"BTCUSDT\",\"event_start\":\"2026-07-17T05:30:00Z\",\"event_end\":\"2026-07-17T05:35:00Z\",\"source_token_ids\":[\"market-1-up\",\"market-1-down\"],\"source_outcomes\":[\"Up\",\"Down\"]}\n{}\n{}\n{}\n{}\n".to_vec();
        let content_sha256 = hex::encode(Sha256::digest(&ndjson));
        let evidence = NormalizedPolymarketEvidence {
            report: PolymarketEvidenceReport {
                schema: "monday.polymarket.normalized_evidence.v2",
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
                market_ids: vec!["market-1".to_owned()],
                symbols: vec!["BTCUSDT".to_owned()],
                window_secs: 300,
                event_selection: EXPLICIT_MARKET_ID_SELECTION,
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
        let mismatches: [fn(&mut PolymarketEventQualificationInput); 4] = [
            |event| event.market_id = "market-2".into(),
            |event| event.symbol = "SOLUSDT".into(),
            |event| event.event_start = "2026-07-17T05:30:01Z".into(),
            |event| event.event_end = "2026-07-17T05:35:01Z".into(),
        ];
        for mismatch in mismatches {
            let mut mismatched = complete_event("market-1");
            mismatch(&mut mismatched);
            assert!(verified_event_input(&evidence, &mismatched).is_err());
        }

        let mut rejected = complete_event("market-rejected");
        rejected.down_book = EvidenceSurfaceStatus::Contradictory;
        let mut partial = complete_event("market-partial");
        partial.down_book = EvidenceSurfaceStatus::Incomplete;
        partial.request_outcomes.as_mut().unwrap()[1].status = ProducerRequestStatus::Failed;
        let config = PolymarketEvidenceArtifactConfig {
            evidence: PolymarketEvidenceConfig {
                segments: crate::polymarket_research_import::ResearchSegmentValidationConfig {
                    market: crate::polymarket_research_import::ArtifactTriplet {
                        data: PathBuf::new(),
                        manifest: PathBuf::new(),
                        success: PathBuf::new(),
                    },
                    references: Vec::new(),
                },
                event_start_gte: "2026-07-17T05:30:00Z".into(),
                event_start_lt: "2026-07-17T05:35:00Z".into(),
                market_ids: vec![
                    "market-rejected".into(),
                    "market-partial".into(),
                    "market-1".into(),
                ],
            },
            output_root: root.clone(),
            qualification: PolymarketProducerQualificationConfig {
                producer: producer_provenance(),
                events: vec![rejected, partial, complete_event("market-1")],
            },
        };
        let run = || {
            publish_polymarket_evidence_with(
                &config,
                |selection| {
                    if selection.market_ids == ["market-1"] {
                        Ok(evidence.clone())
                    } else {
                        panic!("declared non-ready event must not enter the verifier")
                    }
                },
                |selection| Ok(candidate_evidence(&selection.market_ids[0])),
            )
        };
        let first = run().unwrap();
        let second = run().unwrap();
        assert_eq!(
            first.qualifications[0].record.state,
            PolymarketEventQualificationState::Rejected
        );
        assert_eq!(
            first.qualifications[1].record.state,
            PolymarketEventQualificationState::Partial
        );
        assert!(first.qualifications[1].record.verified_token_ids.is_some());
        assert_eq!(
            first.qualifications[2].record.state,
            PolymarketEventQualificationState::Ready
        );
        assert!(second
            .qualifications
            .iter()
            .all(|published| published.outcome == ImmutablePublicationOutcome::Unchanged));

        let mut malformed = evidence.clone();
        malformed.report.market_ids = vec!["market 1".to_owned()];
        assert!(validate_dataset(&malformed).is_err());

        let published = publish_normalized(&root, evidence).unwrap();

        assert_eq!(
            published.published_digests.expected_content_sha256,
            content_sha256
        );
        assert_eq!(
            published.published_digests.expected_manifest_sha256,
            hex::encode(Sha256::digest(fs::read(&published.manifest_path).unwrap()))
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&published.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["schema"], "monday.polymarket.evidence_artifact.v3");
        assert_eq!(manifest["market_ids"], serde_json::json!(["market-1"]));
        assert_eq!(manifest["symbols"], serde_json::json!(["BTCUSDT"]));
        assert_eq!(manifest["event_selection"], EXPLICIT_MARKET_ID_SELECTION);
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
