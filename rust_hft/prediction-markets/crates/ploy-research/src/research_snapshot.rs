use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
#[cfg(feature = "db")]
use std::process::Command;
use std::sync::Arc;
#[cfg(feature = "db")]
use std::time::Instant;

use anyhow::{ensure, Context, Result};
#[cfg(feature = "db")]
use chrono::Timelike;
use chrono::{DateTime, Duration, Utc};
use data::binance_market_tape_artifact::VerifiedBinanceMarketTape;
use ploy_market_contracts::MarketUpdate;
use ploy_market_data::polymarket_evidence::VerifiedPolymarketEvidenceSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(any(feature = "db", test))]
use crate::factors::normalized_underlying_symbol;
use crate::{
    build_prediction_market_data_audit_from_verified_artifacts,
    project_verified_binance_market_tape, project_verified_polymarket_evidence,
    DeribitFeatureSnapshot, FactorObservation, ResearchPmBookSnapshot, ResearchPolymarketContract,
    ResearchPolymarketSettlement, VerifiedArtifactAuditRequest,
};
use crate::{
    prediction_loop_fs::{canonical_json_bytes, sha256_hex},
    EventCohortPartition,
};

pub const RESEARCH_SNAPSHOT_SCHEMA_VERSION: &str = "research_snapshot_v2";
pub const POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND: &str =
    "verified_polymarket_chainlink_baseline";
pub const POLYMARKET_CHAINLINK_BASELINE_REQUIREMENT: &str = "polymarket_chainlink_baseline";
pub const BINANCE_SURFACES_OMITTED_QUALITY_FLAG: &str = "binance_surfaces_intentionally_omitted";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotArtifacts {
    pub observations_json: String,
    pub deribit_snapshots_json: String,
    pub pm_book_snapshots_json: String,
    pub quality_markdown: String,
    pub query_timings_json: String,
    pub observations_parquet: Option<String>,
}

impl Default for ResearchSnapshotArtifacts {
    fn default() -> Self {
        Self {
            observations_json: "observations.json".to_string(),
            deribit_snapshots_json: "deribit_snapshots.json".to_string(),
            pm_book_snapshots_json: "pm_book_snapshots.json".to_string(),
            quality_markdown: "quality.md".to_string(),
            query_timings_json: "query_timings.json".to_string(),
            observations_parquet: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResearchSnapshotRowCounts {
    pub observations: usize,
    pub deribit_snapshots: usize,
    pub pm_book_snapshots: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotSourceSurface {
    pub name: String,
    pub role: String,
    #[serde(default = "default_source_surface_gate_category")]
    pub gate_category: String,
    pub raw_full_fidelity: bool,
    pub snapshot_sampled: bool,
    pub sample_secs: Option<i64>,
    pub row_count: Option<usize>,
    pub notes: String,
}

fn default_source_surface_gate_category() -> String {
    "optional_context".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotInputArtifact {
    pub name: String,
    pub path: String,
    pub content_hash: Option<String>,
    pub row_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotPhaseTiming {
    pub phase: String,
    pub elapsed_ms: u128,
    pub rows: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResearchSnapshotPmBookSource {
    pub hot_postgres_sampled_rows: usize,
    pub archive_sampled_rows: usize,
    pub archive_manifest_rows: usize,
    pub archive_files: usize,
    #[serde(default)]
    pub archive_token_windows: usize,
    pub merged_sampled_rows: usize,
    pub archive_dir: Option<String>,
    pub archive_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotManifest {
    pub schema_version: String,
    pub snapshot_hash: Option<String>,
    #[serde(default)]
    pub snapshot_contract_hash: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub git_sha: Option<String>,
    pub symbols: Vec<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub history_start: DateTime<Utc>,
    pub lob_sample_secs: i32,
    #[serde(default)]
    pub pm_book_sample_secs: Option<i32>,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
    pub immutable_input: bool,
    pub source_kind: String,
    pub optimizer_data_dir: Option<String>,
    #[serde(default)]
    pub source_surfaces: Vec<ResearchSnapshotSourceSurface>,
    #[serde(default)]
    pub input_artifacts: Vec<ResearchSnapshotInputArtifact>,
    #[serde(default)]
    pub data_requirements: Vec<String>,
    #[serde(default)]
    pub data_audit_status: Option<String>,
    #[serde(default)]
    pub data_audit_report: Option<String>,
    #[serde(default = "default_include_deribit")]
    pub include_deribit: bool,
    pub artifacts: ResearchSnapshotArtifacts,
    pub row_counts: ResearchSnapshotRowCounts,
    pub phase_timings: Vec<ResearchSnapshotPhaseTiming>,
    pub quality_flags: Vec<String>,
    #[serde(default)]
    pub pm_book_source: ResearchSnapshotPmBookSource,
}

fn default_include_deribit() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct ResearchSnapshot {
    pub manifest: ResearchSnapshotManifest,
    pub observations: Vec<FactorObservation>,
    pub deribit_snapshots: Vec<DeribitFeatureSnapshot>,
    pub pm_book_snapshots: Vec<ResearchPmBookSnapshot>,
}

#[derive(Debug, Clone, Copy)]
pub struct ResearchSnapshotRequest<'a> {
    pub symbols: &'a [String],
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub pm_book_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
}

const AUTHENTICATED_SNAPSHOT_CACHE_VERSION: &str = "authenticated_research_snapshot_cache.v1";
const AUTHENTICATED_COHORT_MANIFEST_SCHEMA_VERSION: &str = "authenticated_ready_event_cohort.v1";
const AUTHENTICATED_SNAPSHOT_SYMBOL: &str = "BTCUSDT";
const AUTHENTICATED_SNAPSHOT_WINDOW_SECS: i64 = 300;
const SEALED_SNAPSHOT_CACHE_MARKER: &str = ".authenticated-research-snapshot.sealed";
const AUTHENTICATED_COHORT_ARTIFACT: &str = "authenticated_ready_event_cohort";
const AUTHENTICATED_PARTITION_ARTIFACT: &str = "event_cohort_partition";
const AUTHENTICATED_COMPILER_SOURCE_ARTIFACT: &str = "research_snapshot_compiler_source";
const AUTHENTICATED_COMPILER_IMAGE_ARTIFACT: &str = "research_snapshot_compiler_image";
const AUTHENTICATED_BUILD_INPUT_ARTIFACT: &str = "research_snapshot_build_input";
const AUTHENTICATED_EVENT_EVIDENCE_ARTIFACT_PREFIX: &str =
    "authenticated_polymarket_evidence_triplet_";

/// A catalog-and-partition identity whose fields can only be assembled from an
/// authenticated ready-event catalog.
#[derive(Debug, Clone)]
pub struct AuthenticatedReadyEventCohort {
    manifest_id: String,
    partition_digest: String,
    causal_projection_policy_id: String,
    members: Vec<AuthenticatedReadyEvent>,
}

#[derive(Debug, Clone)]
struct AuthenticatedReadyEvent {
    receipt_sha256: String,
    market_id: String,
    content_sha256: String,
    manifest_sha256: String,
    qualification_sha256: String,
    success_sha256: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    up_token_id: String,
    down_token_id: String,
}

impl AuthenticatedReadyEventCohort {
    pub fn manifest_id(&self) -> &str {
        &self.manifest_id
    }
}

/// Stable caller input for the filesystem cache. Compiler identities must be
/// immutable SHA-256 IDs and are bound into the snapshot evaluator contract
/// before admission.
#[derive(Debug, Clone)]
pub struct AuthenticatedSnapshotMaterializationRequest {
    pub cache_root: PathBuf,
    pub compiler_source_identity: String,
    pub compiler_image_identity: String,
    /// Canonical digest of all opaque builder inputs captured by `build`.
    pub build_input_identity: String,
}

/// The only successful result of snapshot admission. Its fields are private so
/// callers cannot manufacture evidence without catalog authentication and a
/// fresh snapshot readback.
#[derive(Debug, Clone)]
pub struct AuthenticatedResearchSnapshot {
    cohort_manifest_id: String,
    snapshot_contract_id: String,
    snapshot_hash: String,
    snapshot_dir: PathBuf,
}

impl AuthenticatedResearchSnapshot {
    pub fn cohort_manifest_id(&self) -> &str {
        &self.cohort_manifest_id
    }

    pub fn snapshot_contract_id(&self) -> &str {
        &self.snapshot_contract_id
    }

    pub fn snapshot_hash(&self) -> &str {
        &self.snapshot_hash
    }

    pub fn snapshot_dir(&self) -> &Path {
        &self.snapshot_dir
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticatedResearchSnapshotRejection {
    CatalogPartitionMismatch { reason: String },
    InsufficientCohort { reason: String },
    UnsupportedTask { reason: String },
    PartialOrRejectedEvent { reason: String },
    FutureReferenceExposure { reason: String },
    TokenMismatch { reason: String },
    CorruptCachedSnapshot { reason: String },
    MaterializationFailed { reason: String },
    SnapshotRejected { reason: String },
}

#[derive(Serialize)]
struct AuthenticatedCohortPayload<'a> {
    schema_version: &'static str,
    partition_digest: &'a str,
    causal_projection_policy_id: &'a str,
    members: &'a [AuthenticatedReadyEvent],
}

impl Serialize for AuthenticatedReadyEvent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        (
            &self.receipt_sha256,
            &self.market_id,
            &self.content_sha256,
            &self.manifest_sha256,
            &self.qualification_sha256,
            &self.success_sha256,
            self.start,
            self.end,
            &self.up_token_id,
            &self.down_token_id,
        )
            .serialize(serializer)
    }
}

#[derive(Serialize)]
struct AuthenticatedSnapshotCacheKey<'a> {
    schema_version: &'static str,
    cohort_manifest_id: &'a str,
    partition_digest: &'a str,
    causal_projection_policy_id: &'a str,
    compiler_source_identity: &'a str,
    compiler_image_identity: &'a str,
    build_input_identity: &'a str,
    research_snapshot_schema_version: &'static str,
}

#[derive(Serialize)]
struct AuthenticatedEventEvidenceTriplet<'a> {
    schema_version: &'static str,
    market_id: &'a str,
    content_sha256: &'a str,
    manifest_sha256: &'a str,
    success_sha256: &'a str,
}

/// Authenticate #319's ready catalog against #322's already-derived
/// partition. This validates membership only; it never derives or resplits a
/// partition from snapshot rows.
pub fn authenticate_ready_event_cohort(
    catalog: &ploy_market_data::polymarket_evidence::PolymarketReadyEventCatalog,
    partition: &EventCohortPartition,
) -> std::result::Result<AuthenticatedReadyEventCohort, AuthenticatedResearchSnapshotRejection> {
    use ploy_market_data::polymarket_evidence::{
        PolymarketCatalogReceiptState, PolymarketResearchTask,
    };

    let ready = catalog
        .receipts()
        .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready)
        .collect::<Vec<_>>();
    if ready.is_empty() {
        return Err(AuthenticatedResearchSnapshotRejection::InsufficientCohort {
            reason: "ready-event catalog has no Ready entries".to_string(),
        });
    }
    if ready.len() != partition.ready_entries().len() {
        return Err(
            AuthenticatedResearchSnapshotRejection::CatalogPartitionMismatch {
                reason: "partition does not contain every Ready catalog entry".to_string(),
            },
        );
    }

    let validate_digest = |value: &str, field: &str| {
        crate::prediction_loop::validate_sha256_id(&format!("sha256:{value}"), field).map_err(
            |reason| AuthenticatedResearchSnapshotRejection::CatalogPartitionMismatch { reason },
        )
    };
    let mut members = Vec::with_capacity(ready.len());
    for (receipt, entry) in ready.into_iter().zip(partition.ready_entries()) {
        let reject = |reason: String| {
            AuthenticatedResearchSnapshotRejection::CatalogPartitionMismatch { reason }
        };
        validate_digest(&receipt.receipt_sha256, "catalog receipt_sha256")?;
        validate_digest(&receipt.content_sha256, "catalog content_sha256")?;
        validate_digest(&receipt.manifest_sha256, "catalog manifest_sha256")?;
        validate_digest(
            &receipt.qualification_sha256,
            "catalog qualification_sha256",
        )?;
        let success_sha256 = receipt.success_sha256.as_deref().ok_or_else(|| {
            reject(format!(
                "ready market {} is missing success_sha256",
                receipt.market_id
            ))
        })?;
        validate_digest(success_sha256, "catalog success_sha256")?;
        let start = receipt.event_start.ok_or_else(|| {
            reject(format!(
                "ready market {} is missing event_start",
                receipt.market_id
            ))
        })?;
        let end = receipt.event_end.ok_or_else(|| {
            reject(format!(
                "ready market {} is missing event_end",
                receipt.market_id
            ))
        })?;
        if receipt.receipt_sha256 != entry.receipt_sha256()
            || receipt.market_id != entry.market_id()
            || start.timestamp_millis() != entry.reference_path_start_ms()
            || end.timestamp_millis() != entry.reference_path_end_ms()
        {
            return Err(reject(format!(
                "partition entry does not match authenticated catalog market {}",
                receipt.market_id
            )));
        }
        if !receipt
            .supported_tasks
            .contains(&PolymarketResearchTask::Btc5mBacktest)
        {
            continue;
        }
        if end - start != Duration::seconds(AUTHENTICATED_SNAPSHOT_WINDOW_SECS) {
            return Err(AuthenticatedResearchSnapshotRejection::UnsupportedTask {
                reason: format!(
                    "ready market {} is not a 300-second event",
                    receipt.market_id
                ),
            });
        }
        let up_token_id = receipt.up_token_id.as_deref().ok_or_else(|| {
            reject(format!(
                "ready market {} is missing its Up token",
                receipt.market_id
            ))
        })?;
        let down_token_id = receipt.down_token_id.as_deref().ok_or_else(|| {
            reject(format!(
                "ready market {} is missing its Down token",
                receipt.market_id
            ))
        })?;
        if up_token_id.is_empty() || down_token_id.is_empty() || up_token_id == down_token_id {
            return Err(AuthenticatedResearchSnapshotRejection::TokenMismatch {
                reason: format!(
                    "ready market {} has invalid Up/Down token identity",
                    receipt.market_id
                ),
            });
        }
        members.push(AuthenticatedReadyEvent {
            receipt_sha256: receipt.receipt_sha256.clone(),
            market_id: receipt.market_id.clone(),
            content_sha256: receipt.content_sha256.clone(),
            manifest_sha256: receipt.manifest_sha256.clone(),
            qualification_sha256: receipt.qualification_sha256.clone(),
            success_sha256: success_sha256.to_string(),
            start,
            end,
            up_token_id: up_token_id.to_string(),
            down_token_id: down_token_id.to_string(),
        });
    }
    if members.is_empty() {
        return Err(AuthenticatedResearchSnapshotRejection::InsufficientCohort {
            reason: "ready-event catalog has no BTC x 300-second entries".to_string(),
        });
    }
    let payload = AuthenticatedCohortPayload {
        schema_version: AUTHENTICATED_COHORT_MANIFEST_SCHEMA_VERSION,
        partition_digest: partition.digest(),
        causal_projection_policy_id: partition.causal_projection_policy_id(),
        members: &members,
    };
    let manifest_id = format!(
        "sha256:{}",
        sha256_hex(&canonical_json_bytes(&payload).map_err(|error| {
            AuthenticatedResearchSnapshotRejection::CatalogPartitionMismatch { reason: error }
        })?,)
    );
    Ok(AuthenticatedReadyEventCohort {
        manifest_id,
        partition_digest: partition.digest().to_string(),
        causal_projection_policy_id: partition.causal_projection_policy_id().to_string(),
        members,
    })
}

/// Write once or reuse a verified content-addressed cache entry. A cache hit is
/// always read and rehashed before its opaque handle is returned.
pub fn materialize_authenticated_research_snapshot<F>(
    cohort: &AuthenticatedReadyEventCohort,
    request: &AuthenticatedSnapshotMaterializationRequest,
    build: F,
) -> std::result::Result<AuthenticatedResearchSnapshot, AuthenticatedResearchSnapshotRejection>
where
    F: FnOnce() -> Result<ResearchSnapshot>,
{
    for (identity, field) in [
        (
            &request.compiler_source_identity,
            "compiler_source_identity",
        ),
        (&request.compiler_image_identity, "compiler_image_identity"),
        (&request.build_input_identity, "build_input_identity"),
    ] {
        crate::prediction_loop::validate_sha256_id(identity, field).map_err(|reason| {
            AuthenticatedResearchSnapshotRejection::MaterializationFailed { reason }
        })?;
    }
    let key = AuthenticatedSnapshotCacheKey {
        schema_version: AUTHENTICATED_SNAPSHOT_CACHE_VERSION,
        cohort_manifest_id: &cohort.manifest_id,
        partition_digest: &cohort.partition_digest,
        causal_projection_policy_id: &cohort.causal_projection_policy_id,
        compiler_source_identity: &request.compiler_source_identity,
        compiler_image_identity: &request.compiler_image_identity,
        build_input_identity: &request.build_input_identity,
        research_snapshot_schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION,
    };
    let cache_key = sha256_hex(&canonical_json_bytes(&key).map_err(|error| {
        AuthenticatedResearchSnapshotRejection::MaterializationFailed { reason: error }
    })?);
    let snapshot_dir = request.cache_root.join(format!("sha256={cache_key}"));
    if snapshot_dir.exists() {
        return admit_cached_authenticated_snapshot(cohort, request, snapshot_dir);
    }

    let mut snapshot =
        build().map_err(
            |error| AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                reason: error.to_string(),
            },
        )?;
    bind_authenticated_snapshot_identity(&mut snapshot, cohort, request)?;
    validate_authenticated_snapshot(&snapshot, cohort, request, true)?;
    let staging_dir = create_authenticated_snapshot_staging_dir(&snapshot_dir)?;
    let published = (|| {
        write_research_snapshot(&staging_dir, snapshot).map_err(|error| {
            AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                reason: error.to_string(),
            }
        })?;
        let staged_snapshot = load_research_snapshot(&staging_dir).map_err(|error| {
            AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot {
                reason: error.to_string(),
            }
        })?;
        validate_authenticated_snapshot(&staged_snapshot, cohort, request, true)?;
        seal_snapshot_cache(&staging_dir, &staged_snapshot).map_err(|error| {
            AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                reason: error.to_string(),
            }
        })?;
        match fs::rename(&staging_dir, &snapshot_dir) {
            Ok(()) => admitted_snapshot(cohort, &staged_snapshot, snapshot_dir.clone()),
            Err(_) if snapshot_dir.exists() => {
                admit_cached_authenticated_snapshot(cohort, request, snapshot_dir.clone())
            }
            Err(error) => Err(
                AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                    reason: format!(
                        "publish authenticated snapshot cache {}: {error}",
                        snapshot_dir.display()
                    ),
                },
            ),
        }
    })();
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir).map_err(|error| {
            AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                reason: format!(
                    "remove authenticated snapshot staging {}: {error}",
                    staging_dir.display()
                ),
            }
        })?;
    }
    published
}

fn admit_cached_authenticated_snapshot(
    cohort: &AuthenticatedReadyEventCohort,
    request: &AuthenticatedSnapshotMaterializationRequest,
    snapshot_dir: PathBuf,
) -> std::result::Result<AuthenticatedResearchSnapshot, AuthenticatedResearchSnapshotRejection> {
    let snapshot = load_research_snapshot(&snapshot_dir).map_err(|error| {
        AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot {
            reason: error.to_string(),
        }
    })?;
    verify_sealed_snapshot_cache(&snapshot_dir, &snapshot).map_err(|error| {
        AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot {
            reason: error.to_string(),
        }
    })?;
    validate_authenticated_snapshot(&snapshot, cohort, request, true)?;
    admitted_snapshot(cohort, &snapshot, snapshot_dir)
}

fn create_authenticated_snapshot_staging_dir(
    snapshot_dir: &Path,
) -> std::result::Result<PathBuf, AuthenticatedResearchSnapshotRejection> {
    let parent = snapshot_dir.parent().ok_or_else(|| {
        AuthenticatedResearchSnapshotRejection::MaterializationFailed {
            reason: format!(
                "snapshot cache path has no parent: {}",
                snapshot_dir.display()
            ),
        }
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        AuthenticatedResearchSnapshotRejection::MaterializationFailed {
            reason: format!("create snapshot cache root {}: {error}", parent.display()),
        }
    })?;
    let name = snapshot_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(
            || AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                reason: format!(
                    "snapshot cache path has no UTF-8 name: {}",
                    snapshot_dir.display()
                ),
            },
        )?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(
            |error| AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                reason: format!("read snapshot staging clock: {error}"),
            },
        )?
        .as_nanos();
    for attempt in 0..64 {
        let staging_dir = parent.join(format!(".{name}.staging-{nonce}-{attempt}"));
        match fs::create_dir(&staging_dir) {
            Ok(()) => return Ok(staging_dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(
                    AuthenticatedResearchSnapshotRejection::MaterializationFailed {
                        reason: format!(
                            "create authenticated snapshot staging {}: {error}",
                            staging_dir.display()
                        ),
                    },
                );
            }
        }
    }
    Err(
        AuthenticatedResearchSnapshotRejection::MaterializationFailed {
            reason: format!(
                "allocate unique authenticated snapshot staging for {}",
                snapshot_dir.display()
            ),
        },
    )
}

fn bind_authenticated_snapshot_identity(
    snapshot: &mut ResearchSnapshot,
    cohort: &AuthenticatedReadyEventCohort,
    request: &AuthenticatedSnapshotMaterializationRequest,
) -> std::result::Result<(), AuthenticatedResearchSnapshotRejection> {
    let reserved = [
        AUTHENTICATED_COHORT_ARTIFACT,
        AUTHENTICATED_PARTITION_ARTIFACT,
        AUTHENTICATED_COMPILER_SOURCE_ARTIFACT,
        AUTHENTICATED_COMPILER_IMAGE_ARTIFACT,
        AUTHENTICATED_BUILD_INPUT_ARTIFACT,
    ];
    if snapshot.manifest.input_artifacts.iter().any(|artifact| {
        reserved.contains(&artifact.name.as_str())
            || artifact
                .name
                .starts_with(AUTHENTICATED_EVENT_EVIDENCE_ARTIFACT_PREFIX)
    }) {
        return Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected {
            reason: "snapshot already declares an authenticated materialization identity"
                .to_string(),
        });
    }
    bind_authenticated_event_evidence(snapshot, cohort)?;
    snapshot.manifest.input_artifacts.extend([
        ResearchSnapshotInputArtifact {
            name: AUTHENTICATED_COHORT_ARTIFACT.to_string(),
            path: "authenticated://ready-event-cohort".to_string(),
            content_hash: Some(cohort.manifest_id.clone()),
            row_count: Some(cohort.members.len()),
        },
        ResearchSnapshotInputArtifact {
            name: AUTHENTICATED_PARTITION_ARTIFACT.to_string(),
            path: "authenticated://event-cohort-partition".to_string(),
            content_hash: Some(cohort.partition_digest.clone()),
            row_count: Some(cohort.members.len()),
        },
        ResearchSnapshotInputArtifact {
            name: AUTHENTICATED_COMPILER_SOURCE_ARTIFACT.to_string(),
            path: "authenticated://compiler-source".to_string(),
            content_hash: Some(request.compiler_source_identity.clone()),
            row_count: None,
        },
        ResearchSnapshotInputArtifact {
            name: AUTHENTICATED_COMPILER_IMAGE_ARTIFACT.to_string(),
            path: "authenticated://compiler-image".to_string(),
            content_hash: Some(request.compiler_image_identity.clone()),
            row_count: None,
        },
        ResearchSnapshotInputArtifact {
            name: AUTHENTICATED_BUILD_INPUT_ARTIFACT.to_string(),
            path: "authenticated://build-input".to_string(),
            content_hash: Some(request.build_input_identity.clone()),
            row_count: None,
        },
    ]);
    Ok(())
}

fn authenticated_event_evidence_digest(
    member: &AuthenticatedReadyEvent,
) -> std::result::Result<String, AuthenticatedResearchSnapshotRejection> {
    let payload = AuthenticatedEventEvidenceTriplet {
        schema_version: AUTHENTICATED_COHORT_MANIFEST_SCHEMA_VERSION,
        market_id: &member.market_id,
        content_sha256: &member.content_sha256,
        manifest_sha256: &member.manifest_sha256,
        success_sha256: &member.success_sha256,
    };
    canonical_json_bytes(&payload)
        .map(|bytes| format!("sha256:{}", sha256_hex(&bytes)))
        .map_err(|reason| AuthenticatedResearchSnapshotRejection::SnapshotRejected { reason })
}

fn verified_polymarket_evidence_path(member: &AuthenticatedReadyEvent) -> String {
    format!(
        "verified+polymarket://sha256/{}/manifest/{}",
        member.content_sha256, member.manifest_sha256
    )
}

fn authenticated_event_evidence_name(index: usize) -> String {
    format!("{AUTHENTICATED_EVENT_EVIDENCE_ARTIFACT_PREFIX}{index:04}")
}

fn has_verified_polymarket_evidence_artifact(
    artifacts: &[ResearchSnapshotInputArtifact],
    member: &AuthenticatedReadyEvent,
) -> bool {
    let expected_path = verified_polymarket_evidence_path(member);
    let expected_content_hash = format!("sha256:{}", member.content_sha256);
    artifacts.iter().any(|artifact| {
        artifact.name.starts_with("polymarket_evidence_")
            && artifact.path == expected_path
            && artifact.content_hash.as_deref() == Some(expected_content_hash.as_str())
    })
}

fn bind_authenticated_event_evidence(
    snapshot: &mut ResearchSnapshot,
    cohort: &AuthenticatedReadyEventCohort,
) -> std::result::Result<(), AuthenticatedResearchSnapshotRejection> {
    for (index, member) in cohort.members.iter().enumerate() {
        if !has_verified_polymarket_evidence_artifact(&snapshot.manifest.input_artifacts, member) {
            return Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected {
                reason: format!(
                    "snapshot is missing verified Polymarket evidence input for {}",
                    member.market_id
                ),
            });
        }
        snapshot
            .manifest
            .input_artifacts
            .push(ResearchSnapshotInputArtifact {
                name: authenticated_event_evidence_name(index),
                path: format!("authenticated://polymarket-evidence/{}", member.market_id),
                content_hash: Some(authenticated_event_evidence_digest(member)?),
                row_count: Some(1),
            });
    }
    Ok(())
}

fn validate_authenticated_snapshot(
    snapshot: &ResearchSnapshot,
    cohort: &AuthenticatedReadyEventCohort,
    request: &AuthenticatedSnapshotMaterializationRequest,
    require_identity: bool,
) -> std::result::Result<(), AuthenticatedResearchSnapshotRejection> {
    if snapshot.manifest.schema_version != RESEARCH_SNAPSHOT_SCHEMA_VERSION
        || snapshot.manifest.symbols.as_slice() != [AUTHENTICATED_SNAPSHOT_SYMBOL]
    {
        return Err(AuthenticatedResearchSnapshotRejection::UnsupportedTask {
            reason: "authenticated materialization supports only BTC x 300-second snapshots"
                .to_string(),
        });
    }
    if require_identity {
        let artifacts = snapshot
            .manifest
            .input_artifacts
            .iter()
            .map(|artifact| (artifact.name.as_str(), artifact))
            .collect::<BTreeMap<_, _>>();
        let expected = [
            (
                AUTHENTICATED_COHORT_ARTIFACT,
                "authenticated://ready-event-cohort",
                Some(cohort.manifest_id.as_str()),
            ),
            (
                AUTHENTICATED_PARTITION_ARTIFACT,
                "authenticated://event-cohort-partition",
                Some(cohort.partition_digest.as_str()),
            ),
            (
                AUTHENTICATED_COMPILER_SOURCE_ARTIFACT,
                "authenticated://compiler-source",
                Some(request.compiler_source_identity.as_str()),
            ),
            (
                AUTHENTICATED_COMPILER_IMAGE_ARTIFACT,
                "authenticated://compiler-image",
                Some(request.compiler_image_identity.as_str()),
            ),
            (
                AUTHENTICATED_BUILD_INPUT_ARTIFACT,
                "authenticated://build-input",
                Some(request.build_input_identity.as_str()),
            ),
        ];
        for (name, path, content_hash) in expected {
            let artifact = artifacts.get(name).ok_or_else(|| {
                AuthenticatedResearchSnapshotRejection::SnapshotRejected {
                    reason: format!("snapshot is missing {name} identity"),
                }
            })?;
            if artifact.path != path || artifact.content_hash.as_deref() != content_hash {
                return Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected {
                    reason: format!("snapshot {name} identity does not match this request"),
                });
            }
        }
        validate_authenticated_event_evidence(snapshot, cohort)?;
    }
    crate::prediction_loop::validate_prediction_snapshot_coverage(
        snapshot,
        &authenticated_snapshot_coverage_mission(),
    )
    .map_err(|reason| AuthenticatedResearchSnapshotRejection::SnapshotRejected { reason })?;
    let events = cohort
        .members
        .iter()
        .map(|member| (member.market_id.as_str(), member))
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeMap::<&str, ()>::new();
    for row in &snapshot.observations {
        let member = events.get(row.event_id.as_str()).ok_or_else(|| {
            AuthenticatedResearchSnapshotRejection::PartialOrRejectedEvent {
                reason: format!(
                    "snapshot observation references unauthenticated event {}",
                    row.event_id
                ),
            }
        })?;
        if row.symbol != AUTHENTICATED_SNAPSHOT_SYMBOL
            || row.event_window_secs != AUTHENTICATED_SNAPSHOT_WINDOW_SECS
        {
            return Err(AuthenticatedResearchSnapshotRejection::UnsupportedTask {
                reason: format!(
                    "snapshot observation for {} is not BTC x 300 seconds",
                    row.event_id
                ),
            });
        }
        if row.up_token_id != member.up_token_id || row.down_token_id != member.down_token_id {
            return Err(AuthenticatedResearchSnapshotRejection::TokenMismatch {
                reason: format!(
                    "snapshot observation token identity mismatches {}",
                    row.event_id
                ),
            });
        }
        if row.event_end_ts != Some(member.end)
            || row.tick_ts < member.start
            || row.tick_ts >= member.end
        {
            return Err(
                AuthenticatedResearchSnapshotRejection::FutureReferenceExposure {
                    reason: format!(
                        "snapshot observation for {} crosses its event reference window",
                        row.event_id
                    ),
                },
            );
        }
        observed.insert(member.market_id.as_str(), ());
    }
    let mut books = BTreeMap::<&str, (bool, bool)>::new();
    for book in &snapshot.pm_book_snapshots {
        let member = events.get(book.event_id.as_str()).ok_or_else(|| {
            AuthenticatedResearchSnapshotRejection::PartialOrRejectedEvent {
                reason: format!(
                    "snapshot book references unauthenticated event {}",
                    book.event_id
                ),
            }
        })?;
        if book.ts < member.start || book.ts >= member.end {
            return Err(
                AuthenticatedResearchSnapshotRejection::FutureReferenceExposure {
                    reason: format!(
                        "snapshot book for {} crosses its event reference window",
                        book.event_id
                    ),
                },
            );
        }
        if book.bids.is_empty() || book.asks.is_empty() {
            return Err(
                AuthenticatedResearchSnapshotRejection::PartialOrRejectedEvent {
                    reason: format!(
                        "snapshot book for {} lacks full bid/ask depth",
                        book.event_id
                    ),
                },
            );
        }
        let sides = books.entry(member.market_id.as_str()).or_default();
        match book.token_id.as_str() {
            token if token == member.up_token_id && book.side.eq_ignore_ascii_case("up") => {
                sides.0 = true
            }
            token if token == member.down_token_id && book.side.eq_ignore_ascii_case("down") => {
                sides.1 = true
            }
            _ => {
                return Err(AuthenticatedResearchSnapshotRejection::TokenMismatch {
                    reason: format!("snapshot book token identity mismatches {}", book.event_id),
                })
            }
        }
    }
    for member in &cohort.members {
        if !observed.contains_key(member.market_id.as_str()) {
            return Err(AuthenticatedResearchSnapshotRejection::InsufficientCohort {
                reason: format!("snapshot has no observation for {}", member.market_id),
            });
        }
        if books.get(member.market_id.as_str()) != Some(&(true, true)) {
            return Err(
                AuthenticatedResearchSnapshotRejection::PartialOrRejectedEvent {
                    reason: format!(
                        "snapshot lacks paired Up/Down books for {}",
                        member.market_id
                    ),
                },
            );
        }
    }
    Ok(())
}

fn authenticated_snapshot_coverage_mission() -> crate::prediction_loop::PredictionResearchMission {
    crate::prediction_loop::PredictionResearchMission {
        schema_version: "prediction_research_mission.v1".to_string(),
        mission_id: "authenticated-snapshot-coverage".to_string(),
        lane: "prediction_market".to_string(),
        objective: "Validate authenticated BTC snapshot coverage.".to_string(),
        hypothesis_scope: "Coverage validation only.".to_string(),
        mutable_scope: Vec::new(),
        data_snapshot_id: format!("sha256:{}", "0".repeat(64)),
        target: "settlement_probability".to_string(),
        symbols: vec!["BTC".to_string()],
        horizon: "5m".to_string(),
        time_cohort_boundary_ms: 0,
        prompt_snapshot_id: format!("sha256:{}", "0".repeat(64)),
        search_policy_snapshot_id: format!("sha256:{}", "0".repeat(64)),
        search_budget: crate::prediction_loop::PredictionSearchBudget {
            max_candidates: 0,
            max_llm_calls: 0,
            max_seconds: 0,
        },
    }
}

fn validate_authenticated_event_evidence(
    snapshot: &ResearchSnapshot,
    cohort: &AuthenticatedReadyEventCohort,
) -> std::result::Result<(), AuthenticatedResearchSnapshotRejection> {
    for (index, member) in cohort.members.iter().enumerate() {
        if !has_verified_polymarket_evidence_artifact(&snapshot.manifest.input_artifacts, member) {
            return Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected {
                reason: format!(
                    "snapshot evidence input does not match authenticated catalog evidence for {}",
                    member.market_id
                ),
            });
        }
        let expected_name = authenticated_event_evidence_name(index);
        let expected_path = format!("authenticated://polymarket-evidence/{}", member.market_id);
        let expected_digest = authenticated_event_evidence_digest(member)?;
        let artifact = snapshot
            .manifest
            .input_artifacts
            .iter()
            .find(|artifact| artifact.name == expected_name)
            .ok_or_else(
                || AuthenticatedResearchSnapshotRejection::SnapshotRejected {
                    reason: format!(
                        "snapshot is missing authenticated evidence triplet for {}",
                        member.market_id
                    ),
                },
            )?;
        if artifact.path != expected_path
            || artifact.content_hash.as_deref() != Some(&expected_digest)
        {
            return Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected {
                reason: format!(
                    "snapshot authenticated evidence triplet does not match {}",
                    member.market_id
                ),
            });
        }
    }
    Ok(())
}

fn admitted_snapshot(
    cohort: &AuthenticatedReadyEventCohort,
    snapshot: &ResearchSnapshot,
    snapshot_dir: PathBuf,
) -> std::result::Result<AuthenticatedResearchSnapshot, AuthenticatedResearchSnapshotRejection> {
    let snapshot_contract_id = snapshot
        .manifest
        .snapshot_contract_hash
        .clone()
        .ok_or_else(
            || AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot {
                reason: "verified snapshot is missing snapshot_contract_hash".to_string(),
            },
        )?;
    let snapshot_hash = snapshot.manifest.snapshot_hash.clone().ok_or_else(|| {
        AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot {
            reason: "verified snapshot is missing snapshot_hash".to_string(),
        }
    })?;
    Ok(AuthenticatedResearchSnapshot {
        cohort_manifest_id: cohort.manifest_id.clone(),
        snapshot_contract_id,
        snapshot_hash,
        snapshot_dir,
    })
}

fn snapshot_cache_paths(snapshot_dir: &Path, snapshot: &ResearchSnapshot) -> Vec<PathBuf> {
    let mut paths = vec![
        snapshot_dir.join("manifest.json"),
        snapshot_dir.join(&snapshot.manifest.artifacts.observations_json),
        snapshot_dir.join(&snapshot.manifest.artifacts.deribit_snapshots_json),
        snapshot_dir.join(&snapshot.manifest.artifacts.pm_book_snapshots_json),
        snapshot_dir.join(&snapshot.manifest.artifacts.query_timings_json),
        snapshot_dir.join(&snapshot.manifest.artifacts.quality_markdown),
    ];
    if let Some(parquet) = &snapshot.manifest.artifacts.observations_parquet {
        paths.push(snapshot_dir.join(parquet));
    }
    paths
}

fn seal_snapshot_cache(snapshot_dir: &Path, snapshot: &ResearchSnapshot) -> Result<()> {
    for path in snapshot_cache_paths(snapshot_dir, snapshot) {
        let mut permissions = fs::metadata(&path)
            .with_context(|| format!("inspect authenticated snapshot cache {}", path.display()))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).with_context(|| {
            format!(
                "make authenticated snapshot cache read-only {}",
                path.display()
            )
        })?;
    }
    let marker = snapshot_dir.join(SEALED_SNAPSHOT_CACHE_MARKER);
    let contract_id = snapshot
        .manifest
        .snapshot_contract_hash
        .as_deref()
        .context("sealed snapshot cache requires snapshot_contract_hash")?;
    fs::write(&marker, contract_id).with_context(|| {
        format!(
            "write authenticated snapshot cache seal {}",
            marker.display()
        )
    })?;
    let mut permissions = fs::metadata(&marker)
        .with_context(|| {
            format!(
                "inspect authenticated snapshot cache seal {}",
                marker.display()
            )
        })?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&marker, permissions)
        .with_context(|| format!("seal authenticated snapshot cache {}", marker.display()))?;
    Ok(())
}

fn verify_sealed_snapshot_cache(snapshot_dir: &Path, snapshot: &ResearchSnapshot) -> Result<()> {
    for path in snapshot_cache_paths(snapshot_dir, snapshot) {
        ensure!(
            fs::metadata(&path)
                .with_context(|| format!(
                    "inspect authenticated snapshot cache {}",
                    path.display()
                ))?
                .permissions()
                .readonly(),
            "authenticated snapshot cache artifact is not read-only: {}",
            path.display()
        );
    }
    let marker = snapshot_dir.join(SEALED_SNAPSHOT_CACHE_MARKER);
    let metadata = fs::metadata(&marker).with_context(|| {
        format!(
            "read authenticated snapshot cache seal {}",
            marker.display()
        )
    })?;
    ensure!(
        metadata.permissions().readonly(),
        "authenticated snapshot cache seal is not read-only"
    );
    let expected = snapshot
        .manifest
        .snapshot_contract_hash
        .as_deref()
        .context("sealed snapshot cache requires snapshot_contract_hash")?;
    ensure!(
        fs::read_to_string(&marker).with_context(|| format!(
            "read authenticated snapshot cache seal {}",
            marker.display()
        ))? == expected,
        "authenticated snapshot cache seal does not match snapshot_contract_hash"
    );
    Ok(())
}

pub struct VerifiedArtifactSnapshotBuildOptions {
    pub symbol: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub pm_book_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub optimizer_data_dir: String,
    pub git_sha: Option<String>,
}

fn validate_verified_options(options: &VerifiedArtifactSnapshotBuildOptions) -> Result<()> {
    ensure!(
        matches!(options.symbol.as_str(), "BTCUSDT" | "SOLUSDT"),
        "verified artifact snapshots support BTCUSDT or SOLUSDT"
    );
    ensure!(options.start < options.end, "invalid snapshot window");
    ensure!(
        options.lob_sample_secs > 0
            && options.pm_book_sample_secs > 0
            && options.observation_sample_secs > 0
            && options.max_quote_age_secs > 0,
        "snapshot cadences must be positive"
    );
    ensure!(
        i64::from(options.pm_book_sample_secs) <= options.max_quote_age_secs,
        "pm_book_sample_secs must not exceed max_quote_age_secs"
    );
    ensure!(
        options.stake_usd.is_finite() && options.stake_usd > 0.0,
        "invalid stake_usd"
    );
    ensure!(
        !options.optimizer_data_dir.is_empty()
            && options.optimizer_data_dir.trim() == options.optimizer_data_dir,
        "invalid optimizer_data_dir"
    );
    Ok(())
}

fn append_verified_contracts(
    updates: &mut Vec<MarketUpdate>,
    contracts: &[ResearchPolymarketContract],
    settlements: &[ResearchPolymarketSettlement],
) -> Result<HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>> {
    let mut outcomes = HashMap::new();
    for contract in contracts {
        let mut matching = settlements
            .iter()
            .filter(|settlement| settlement.event_id == contract.event_id);
        let settlement = matching
            .next()
            .context("contract needs one official settlement")?;
        ensure!(
            matching.next().is_none(),
            "contract needs one official settlement"
        );
        let expected_winner = if settlement.resolved_up_won {
            &contract.up_token_id
        } else {
            &contract.down_token_id
        };
        ensure!(
            settlement.winning_token_id == *expected_winner,
            "settlement event/token mismatch"
        );
        ensure!(
            settlement.available_at >= contract.event_end,
            "early settlement clock"
        );
        let key = (
            contract.event_id.clone(),
            contract.up_token_id.clone(),
            contract.down_token_id.clone(),
        );
        ensure!(
            outcomes
                .insert(
                    key,
                    OfficialBinaryOutcome {
                        settlement_up: settlement.resolved_up_won,
                        observed_at: settlement.available_at,
                    },
                )
                .is_none(),
            "duplicate official settlement"
        );
        updates.push(MarketUpdate::EventDiscovered {
            event_id: Arc::from(contract.event_id.as_str()),
            symbol: Arc::from(contract.symbol.as_str()),
            up_token: Arc::from(contract.up_token_id.as_str()),
            down_token: Arc::from(contract.down_token_id.as_str()),
            end_time: contract.event_end,
            window_secs: 300,
            price_to_beat: Some(contract.price_to_beat),
            resolved_up_won: None,
        });
    }
    Ok(outcomes)
}

fn observation_is_available(
    tick_ts: DateTime<Utc>,
    contract: &ResearchPolymarketContract,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> bool {
    tick_ts >= start.max(contract.event_start).max(contract.available_at)
        && tick_ts < end.min(contract.event_end)
}

fn bind_and_filter_verified_observations(
    observations: Vec<FactorObservation>,
    contracts: &[ResearchPolymarketContract],
    outcomes: &HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>,
    symbol: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<FactorObservation>> {
    bind_and_filter_verified_observations_with_binance_requirement(
        observations,
        contracts,
        outcomes,
        symbol,
        start,
        end,
        true,
    )
}

fn bind_and_filter_polymarket_chainlink_baseline_observations(
    observations: Vec<FactorObservation>,
    contracts: &[ResearchPolymarketContract],
    outcomes: &HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>,
    symbol: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<FactorObservation>> {
    bind_and_filter_verified_observations_with_binance_requirement(
        observations,
        contracts,
        outcomes,
        symbol,
        start,
        end,
        false,
    )
}

pub(crate) fn clear_polymarket_chainlink_baseline_unavailable_features(
    row: &mut FactorObservation,
) {
    row.signed_distance_to_beat = f64::NAN;
    row.abs_distance_to_beat = f64::NAN;
    row.drift_10s = f64::NAN;
    row.drift_30s = f64::NAN;
    row.flip_age_secs = f64::NAN;
    row.post_flip_drift = f64::NAN;
    row.sigma_horizon = f64::NAN;
    row.implied_sigma_horizon = f64::NAN;
    row.vol_gap = f64::NAN;
    row.distance_over_sigma = f64::NAN;
    row.model_prob_up = f64::NAN;
    row.chainlink_prob_up = f64::NAN;
    row.model_edge_up = f64::NAN;
    row.obi = f64::NAN;
    row.spread_bps = f64::NAN;
    row.microprice_offset_bps = f64::NAN;
    row.bid_depth_near = f64::NAN;
    row.ask_depth_near = f64::NAN;
    row.depth_ratio = f64::NAN;
    row.depth_imbalance = f64::NAN;
    row.depth_far_ratio = f64::NAN;
    row.depth_acceleration = f64::NAN;
    row.obi_10 = f64::NAN;
    row.cum_obi_delta_5m = f64::NAN;
    row.cum_depth_delta_5m = f64::NAN;
    row.cum_mprice_drift_5m = f64::NAN;
    row.cum_trade_imbalance_5m = f64::NAN;
    row.cex_bar_return_30s = f64::NAN;
    row.cex_bar_return_60s = f64::NAN;
    row.cex_bar_volume_ratio_30s = f64::NAN;
    row.cex_bar_volume_trend_3 = f64::NAN;
    row.cex_signed_volume_ratio_30s = f64::NAN;
    row.cex_consecutive_up_bars = f64::NAN;
    row.cex_consecutive_down_bars = f64::NAN;
    row.cex_breakout_volume_score = f64::NAN;
}

fn bind_and_filter_verified_observations_with_binance_requirement(
    observations: Vec<FactorObservation>,
    contracts: &[ResearchPolymarketContract],
    outcomes: &HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>,
    symbol: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    require_binance: bool,
) -> Result<Vec<FactorObservation>> {
    let contracts_by_event = contracts
        .iter()
        .map(|contract| (contract.event_id.as_str(), contract))
        .collect::<HashMap<_, _>>();
    let mut covered_contracts = HashMap::new();
    let mut available = Vec::new();
    for mut row in observations {
        let contract = contracts_by_event
            .get(row.event_id.as_str())
            .context("unknown factor event")?;
        ensure!(
            row.symbol == symbol
                && contract.symbol == symbol
                && row.up_token_id == contract.up_token_id
                && row.down_token_id == contract.down_token_id,
            "factor event/token mismatch"
        );
        if let Some(event_end_ts) = row.event_end_ts {
            ensure!(
                event_end_ts == contract.event_end,
                "factor event end mismatch"
            );
        }
        row.event_end_ts = Some(contract.event_end);
        if !observation_is_available(row.tick_ts, contract, start, end) {
            continue;
        }
        if !require_binance {
            row.binance_spot_fresh = false;
            row.binance_lob_fresh = false;
            row.binance_agg_trade_fresh = false;
            clear_polymarket_chainlink_baseline_unavailable_features(&mut row);
        }
        if !row.chainlink_reference_fresh
            || (require_binance
                && (!row.binance_spot_fresh
                    || !row.binance_lob_fresh
                    || !row.binance_agg_trade_fresh))
        {
            continue;
        }
        let outcome = exact_official_binary_outcome(
            outcomes,
            &row.event_id,
            &row.up_token_id,
            &row.down_token_id,
        )
        .context("factor settlement mismatch")?;
        row.settlement_up = if outcome.settlement_up { 1.0 } else { 0.0 };
        row.official_resolution_observed_at = Some(outcome.observed_at);
        covered_contracts.insert(row.event_id.clone(), ());
        available.push(row);
    }
    for contract in contracts {
        ensure!(
            covered_contracts.contains_key(&contract.event_id),
            "verified artifact snapshot has no available factor observation for contract {}",
            contract.event_id
        );
    }
    Ok(available)
}

fn polymarket_chainlink_baseline_ticks(
    books: &[ResearchPmBookSnapshot],
    contracts: &[ResearchPolymarketContract],
) -> Result<Vec<MarketUpdate>> {
    let mut ticks = Vec::new();
    for contract in contracts {
        let mut event_books = books
            .iter()
            .filter(|book| book.event_id == contract.event_id)
            .collect::<Vec<_>>();
        event_books
            .sort_by(|left, right| (left.ts, &left.token_id).cmp(&(right.ts, &right.token_id)));
        let mut has_up = false;
        let mut has_down = false;
        let mut emitted = HashSet::new();
        for book in event_books {
            ensure!(
                !book.bids.is_empty() && !book.asks.is_empty(),
                "baseline token books must contain both bid and ask depth"
            );
            if book.token_id == contract.up_token_id && book.side.eq_ignore_ascii_case("up") {
                has_up = true;
            } else if book.token_id == contract.down_token_id
                && book.side.eq_ignore_ascii_case("down")
            {
                has_down = true;
            } else {
                anyhow::bail!("baseline token book identity does not match its event contract");
            }
            if has_up && has_down && emitted.insert(book.ts) {
                ticks.push(MarketUpdate::SpotPrice {
                    symbol: Arc::from(contract.symbol.as_str()),
                    price: contract.price_to_beat,
                    ts: book.ts,
                });
            }
        }
        ensure!(
            has_up && has_down && !emitted.is_empty(),
            "baseline event {} requires both Up and Down token books",
            contract.event_id
        );
    }
    Ok(ticks)
}

fn verified_source_surfaces(
    counts: [usize; 6],
    lob_sample_secs: i32,
    pm_book_sample_secs: i32,
) -> Vec<ResearchSnapshotSourceSurface> {
    let prediction = "required_for_prediction";
    let execution = "required_for_execution";
    let definitions = [
        ("chainlink_reference_ticks", None, false, true, prediction),
        (
            "binance_price_ticks",
            Some(i64::from(lob_sample_secs)),
            false,
            true,
            prediction,
        ),
        ("binance_agg_trade_ticks", Some(5), false, true, prediction),
        (
            "binance_lob_ticks",
            Some(i64::from(lob_sample_secs)),
            true,
            true,
            prediction,
        ),
        (
            "clob_orderbook_snapshots",
            Some(i64::from(pm_book_sample_secs)),
            true,
            true,
            execution,
        ),
        ("pm_token_settlements", None, true, false, prediction),
    ];
    definitions
        .into_iter()
        .zip(counts)
        .map(
            |(
                (name, sample_secs, raw_full_fidelity, snapshot_sampled, gate_category),
                row_count,
            )| ResearchSnapshotSourceSurface {
                name: name.to_string(),
                role: "verified_artifact_projection".to_string(),
                gate_category: gate_category.to_string(),
                raw_full_fidelity,
                snapshot_sampled,
                sample_secs,
                row_count: Some(row_count),
                notes: "Externally anchored verified immutable artifact.".to_string(),
            },
        )
        .collect()
}

pub fn build_research_snapshot_from_verified_artifacts(
    binance: &VerifiedBinanceMarketTape,
    polymarket: &VerifiedPolymarketEvidenceSet,
    options: VerifiedArtifactSnapshotBuildOptions,
) -> Result<ResearchSnapshot> {
    validate_verified_options(&options)?;
    let symbols = vec![options.symbol.clone()];
    let audit = build_prediction_market_data_audit_from_verified_artifacts(
        binance,
        polymarket,
        VerifiedArtifactAuditRequest {
            symbol: options.symbol.clone(),
            snapshot_start: options.start,
            snapshot_end: options.end,
        },
    )?;
    audit
        .validate_for_prediction_snapshot(&symbols, options.start, options.end)
        .map_err(anyhow::Error::msg)?;
    let history_start = audit
        .request
        .coverage_start()
        .context("verified artifact audit coverage window overflows")?;
    let binance_projection = project_verified_binance_market_tape(
        binance,
        &options.symbol,
        history_start,
        options.end,
        options.lob_sample_secs,
    )?;

    let mut polymarket_updates = Vec::new();
    let mut pm_book_snapshots = Vec::new();
    let mut contracts = Vec::new();
    let mut settlements = Vec::new();
    for member in polymarket.members() {
        let projection = project_verified_polymarket_evidence(
            member,
            i64::from(options.pm_book_sample_secs),
            audit.request.maximum_source_delay_secs,
        )?;
        polymarket_updates.extend(projection.updates);
        pm_book_snapshots.extend(projection.pm_book_snapshots);
        contracts.extend(projection.contracts);
        settlements.extend(projection.settlements);
    }

    contracts.retain(|contract| {
        contract.symbol == options.symbol
            && contract.event_start >= options.start
            && contract.event_end <= options.end
    });
    ensure!(
        !contracts.is_empty(),
        "verified artifact snapshot has no complete five-minute Polymarket contracts"
    );
    let selected_events = contracts
        .iter()
        .map(|contract| contract.event_id.as_str())
        .collect::<HashSet<_>>();
    let selected_tokens = contracts
        .iter()
        .flat_map(|contract| {
            [
                contract.up_token_id.as_str(),
                contract.down_token_id.as_str(),
            ]
        })
        .collect::<HashSet<_>>();
    polymarket_updates.retain(|update| {
        update.sort_ts() >= history_start
            && update.sort_ts() < options.end
            && match update {
                MarketUpdate::ReferencePrice { symbol, .. } => symbol.as_ref() == options.symbol,
                MarketUpdate::Quote { token_id, .. } => selected_tokens.contains(token_id.as_ref()),
                _ => false,
            }
    });
    pm_book_snapshots.retain(|book| {
        selected_events.contains(book.event_id.as_str())
            && book.ts >= history_start
            && book.ts < options.end
    });
    let chainlink_rows = polymarket_updates
        .iter()
        .filter(|update| {
            matches!(
                update,
                MarketUpdate::ReferencePrice {
                    source,
                    is_carried_forward: false,
                    received_at: Some(_),
                    ..
                } if source.eq_ignore_ascii_case("chainlink")
            )
        })
        .count();
    let outcomes = append_verified_contracts(&mut polymarket_updates, &contracts, &settlements)?;

    let mut updates = binance_projection.updates;
    updates.extend(polymarket_updates);
    updates.sort_by_key(MarketUpdate::sort_ts);
    pm_book_snapshots.sort_by(|left, right| {
        (left.ts, &left.event_id, &left.token_id).cmp(&(right.ts, &right.event_id, &right.token_id))
    });
    let observations =
        crate::factors::build_unlabeled_factor_observations_with_lob_sampled_and_source_clocks(
            &updates,
            &binance_projection.lob_snapshots,
            &binance_projection.source_clocks,
            options.max_quote_age_secs,
            options.observation_sample_secs,
        );
    let observations = bind_and_filter_verified_observations(
        observations,
        &contracts,
        &outcomes,
        &options.symbol,
        options.start,
        options.end,
    )?;

    let counts = [
        chainlink_rows,
        binance_projection.counts.spot_prices,
        binance_projection.counts.aggregate_trades,
        binance_projection.counts.lob_snapshots,
        pm_book_snapshots.len(),
        outcomes.len(),
    ];
    let mut input_artifacts = binance
        .segments()
        .iter()
        .enumerate()
        .map(|(index, segment)| ResearchSnapshotInputArtifact {
            name: format!("binance_market_tape_segment_{index:04}"),
            path: format!(
                "verified+binance://sha256/{}/manifest/{}",
                segment.content_sha256, segment.manifest_sha256
            ),
            content_hash: Some(format!("sha256:{}", segment.content_sha256)),
            row_count: usize::try_from(segment.events).ok(),
        })
        .collect::<Vec<_>>();
    for (index, identity) in polymarket.identities().enumerate() {
        input_artifacts.push(ResearchSnapshotInputArtifact {
            name: format!("polymarket_evidence_{index:04}"),
            path: format!(
                "verified+polymarket://sha256/{}/manifest/{}",
                identity.content_sha256, identity.manifest_sha256
            ),
            content_hash: Some(format!("sha256:{}", identity.content_sha256)),
            row_count: usize::try_from(identity.rows).ok(),
        });
    }
    let audit_bytes = serde_json::to_vec(&audit).context("serialize verified artifact audit")?;
    let audit_sha256 = format!("{:x}", Sha256::digest(audit_bytes));
    let row_counts = ResearchSnapshotRowCounts {
        observations: observations.len(),
        deribit_snapshots: 0,
        pm_book_snapshots: pm_book_snapshots.len(),
    };
    Ok(ResearchSnapshot {
        manifest: ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: None,
            snapshot_contract_hash: None,
            generated_at: audit.generated_at,
            git_sha: options.git_sha,
            symbols,
            start: options.start,
            end: options.end,
            history_start,
            lob_sample_secs: options.lob_sample_secs,
            pm_book_sample_secs: Some(options.pm_book_sample_secs),
            observation_sample_secs: options.observation_sample_secs,
            max_quote_age_secs: options.max_quote_age_secs,
            stake_usd: options.stake_usd,
            require_official_settlement: true,
            immutable_input: true,
            source_kind: "verified_immutable_artifacts".to_string(),
            optimizer_data_dir: Some(options.optimizer_data_dir),
            source_surfaces: verified_source_surfaces(
                counts,
                options.lob_sample_secs,
                options.pm_book_sample_secs,
            ),
            input_artifacts,
            data_requirements: [
                "chainlink_reference",
                "binance_price",
                "binance_agg_trades",
                "binance_lob",
                "polymarket_orderbook",
                "polymarket_official_settlement",
            ]
            .map(str::to_owned)
            .to_vec(),
            data_audit_status: Some("ok".to_string()),
            data_audit_report: Some(format!("verified+audit://sha256/{audit_sha256}")),
            include_deribit: false,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts,
            phase_timings: Vec::new(),
            quality_flags: Vec::new(),
            pm_book_source: ResearchSnapshotPmBookSource {
                hot_postgres_sampled_rows: 0,
                archive_sampled_rows: 0,
                archive_manifest_rows: 0,
                archive_files: 0,
                archive_token_windows: 0,
                merged_sampled_rows: pm_book_snapshots.len(),
                archive_dir: None,
                archive_status: "verified_artifact_projection".to_string(),
            },
        },
        observations,
        deribit_snapshots: Vec::new(),
        pm_book_snapshots,
    })
}

pub fn build_research_snapshot_from_polymarket_chainlink_baseline(
    polymarket: &VerifiedPolymarketEvidenceSet,
    options: VerifiedArtifactSnapshotBuildOptions,
) -> Result<ResearchSnapshot> {
    validate_verified_options(&options)?;
    ensure!(
        options.start.timestamp().rem_euclid(300) == 0
            && options.start.timestamp_subsec_nanos() == 0
            && options.end.timestamp().rem_euclid(300) == 0
            && options.end.timestamp_subsec_nanos() == 0,
        "Polymarket + Chainlink baseline window must align to full five-minute events"
    );
    ensure!(
        polymarket.event_start_gte() <= options.start && polymarket.event_start_lt() >= options.end,
        "verified Polymarket evidence does not cover the baseline snapshot window"
    );
    let history_start = options
        .start
        .checked_sub_signed(Duration::seconds(options.max_quote_age_secs))
        .context("baseline history window overflows")?;

    let mut polymarket_updates = Vec::new();
    let mut pm_book_snapshots = Vec::new();
    let mut contracts = Vec::new();
    let mut settlements = Vec::new();
    let mut generated_at = None;
    for member in polymarket.members() {
        let projection = project_verified_polymarket_evidence(
            member,
            i64::from(options.pm_book_sample_secs),
            options.max_quote_age_secs,
        )?;
        polymarket_updates.extend(projection.updates);
        pm_book_snapshots.extend(projection.pm_book_snapshots);
        contracts.extend(projection.contracts);
        settlements.extend(projection.settlements);
        generated_at = Some(generated_at.map_or(
            projection.evidence_available_through,
            |current: DateTime<Utc>| current.max(projection.evidence_available_through),
        ));
    }

    contracts.retain(|contract| {
        contract.symbol == options.symbol
            && contract.event_start >= options.start
            && contract.event_end <= options.end
    });
    ensure!(
        !contracts.is_empty(),
        "baseline snapshot has no complete five-minute Polymarket contracts"
    );
    let selected_events = contracts
        .iter()
        .map(|contract| contract.event_id.as_str())
        .collect::<HashSet<_>>();
    let selected_tokens = contracts
        .iter()
        .flat_map(|contract| {
            [
                contract.up_token_id.as_str(),
                contract.down_token_id.as_str(),
            ]
        })
        .collect::<HashSet<_>>();
    polymarket_updates.retain(|update| {
        update.sort_ts() >= history_start
            && update.sort_ts() < options.end
            && match update {
                MarketUpdate::ReferencePrice { symbol, .. } => symbol.as_ref() == options.symbol,
                MarketUpdate::Quote { token_id, .. } => selected_tokens.contains(token_id.as_ref()),
                _ => false,
            }
    });
    pm_book_snapshots.retain(|book| {
        selected_events.contains(book.event_id.as_str())
            && book.ts >= history_start
            && book.ts < options.end
    });
    let chainlink_rows = polymarket_updates
        .iter()
        .filter(|update| {
            matches!(
                update,
                MarketUpdate::ReferencePrice {
                    source,
                    is_carried_forward: false,
                    received_at: Some(_),
                    ..
                } if source.eq_ignore_ascii_case("chainlink")
            )
        })
        .count();
    ensure!(
        chainlink_rows >= contracts.len(),
        "baseline snapshot requires authenticated event-local Chainlink reference evidence"
    );
    let outcomes = append_verified_contracts(&mut polymarket_updates, &contracts, &settlements)?;
    polymarket_updates.extend(polymarket_chainlink_baseline_ticks(
        &pm_book_snapshots,
        &contracts,
    )?);
    polymarket_updates.sort_by_key(MarketUpdate::sort_ts);
    pm_book_snapshots.sort_by(|left, right| {
        (left.ts, &left.event_id, &left.token_id).cmp(&(right.ts, &right.event_id, &right.token_id))
    });

    let observations =
        crate::factors::build_unlabeled_factor_observations_with_lob_sampled_and_source_clocks(
            &polymarket_updates,
            &[],
            &[],
            options.max_quote_age_secs,
            options.observation_sample_secs,
        );
    let observations = bind_and_filter_polymarket_chainlink_baseline_observations(
        observations,
        &contracts,
        &outcomes,
        &options.symbol,
        options.start,
        options.end,
    )?;
    ensure!(
        observations.iter().all(|row| {
            !row.binance_spot_fresh && !row.binance_lob_fresh && !row.binance_agg_trade_fresh
        }),
        "baseline snapshot must not claim Binance freshness"
    );

    let input_artifacts = polymarket
        .identities()
        .enumerate()
        .map(|(index, identity)| ResearchSnapshotInputArtifact {
            name: format!("polymarket_evidence_{index:04}"),
            path: format!(
                "verified+polymarket://sha256/{}/manifest/{}",
                identity.content_sha256, identity.manifest_sha256
            ),
            content_hash: Some(format!("sha256:{}", identity.content_sha256)),
            row_count: usize::try_from(identity.rows).ok(),
        })
        .collect::<Vec<_>>();
    let counts = [
        chainlink_rows,
        0,
        0,
        0,
        pm_book_snapshots.len(),
        outcomes.len(),
    ];
    let mut source_surfaces =
        verified_source_surfaces(counts, options.lob_sample_secs, options.pm_book_sample_secs);
    for surface in &mut source_surfaces {
        if surface.name.starts_with("binance_") {
            surface.role = "intentionally_omitted".to_string();
            surface.gate_category = "optional_context".to_string();
            surface.snapshot_sampled = false;
            surface.sample_secs = None;
            surface.row_count = Some(0);
            surface.notes =
                "Intentionally omitted by the explicit Polymarket + Chainlink baseline profile."
                    .to_string();
        }
    }
    let generated_at = generated_at.context("verified Polymarket evidence set is empty")?;
    ensure!(
        generated_at >= options.end,
        "verified Polymarket evidence was not available through the baseline snapshot end"
    );
    let audit = serde_json::json!({
        "schema": "monday.polymarket_chainlink_baseline_audit.v1",
        "profile": POLYMARKET_CHAINLINK_BASELINE_REQUIREMENT,
        "symbol": &options.symbol,
        "snapshot_start": options.start,
        "snapshot_end": options.end,
        "generated_at": generated_at,
        "contracts": contracts.len(),
        "chainlink_references": chainlink_rows,
        "polymarket_books": pm_book_snapshots.len(),
        "official_settlements": outcomes.len(),
        "intentional_omissions": ["binance_price", "binance_agg_trades", "binance_lob"],
        "input_artifacts": &input_artifacts,
    });
    let audit_bytes = serde_json::to_vec(&audit).context("serialize baseline artifact audit")?;
    let audit_sha256 = format!("{:x}", Sha256::digest(audit_bytes));
    let row_counts = ResearchSnapshotRowCounts {
        observations: observations.len(),
        deribit_snapshots: 0,
        pm_book_snapshots: pm_book_snapshots.len(),
    };

    Ok(ResearchSnapshot {
        manifest: ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: None,
            snapshot_contract_hash: None,
            generated_at,
            git_sha: options.git_sha,
            symbols: vec![options.symbol],
            start: options.start,
            end: options.end,
            history_start,
            lob_sample_secs: options.lob_sample_secs,
            pm_book_sample_secs: Some(options.pm_book_sample_secs),
            observation_sample_secs: options.observation_sample_secs,
            max_quote_age_secs: options.max_quote_age_secs,
            stake_usd: options.stake_usd,
            require_official_settlement: true,
            immutable_input: true,
            source_kind: POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND.to_string(),
            optimizer_data_dir: Some(options.optimizer_data_dir),
            source_surfaces,
            input_artifacts,
            data_requirements: [
                POLYMARKET_CHAINLINK_BASELINE_REQUIREMENT,
                "chainlink_reference",
                "polymarket_orderbook",
                "polymarket_official_settlement",
            ]
            .map(str::to_owned)
            .to_vec(),
            data_audit_status: Some("ok".to_string()),
            data_audit_report: Some(format!(
                "verified+polymarket-chainlink-baseline-audit://sha256/{audit_sha256}"
            )),
            include_deribit: false,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts,
            phase_timings: Vec::new(),
            quality_flags: vec![BINANCE_SURFACES_OMITTED_QUALITY_FLAG.to_string()],
            pm_book_source: ResearchSnapshotPmBookSource {
                hot_postgres_sampled_rows: 0,
                archive_sampled_rows: 0,
                archive_manifest_rows: 0,
                archive_files: 0,
                archive_token_windows: 0,
                merged_sampled_rows: pm_book_snapshots.len(),
                archive_dir: None,
                archive_status: "verified_polymarket_chainlink_baseline_projection".to_string(),
            },
        },
        observations,
        deribit_snapshots: Vec::new(),
        pm_book_snapshots,
    })
}

#[derive(Debug, Clone)]
struct ResearchSnapshotArtifactBytes {
    observations_json: Vec<u8>,
    deribit_snapshots_json: Vec<u8>,
    pm_book_snapshots_json: Vec<u8>,
    observations_parquet: Option<Vec<u8>>,
}

fn load_snapshot_artifact_bytes_with<F>(
    manifest: &ResearchSnapshotManifest,
    include_contract_artifacts: bool,
    mut read: F,
) -> Result<ResearchSnapshotArtifactBytes>
where
    F: FnMut(&str) -> Result<Vec<u8>>,
{
    Ok(ResearchSnapshotArtifactBytes {
        observations_json: read(&manifest.artifacts.observations_json)?,
        deribit_snapshots_json: read(&manifest.artifacts.deribit_snapshots_json)?,
        pm_book_snapshots_json: read(&manifest.artifacts.pm_book_snapshots_json)?,
        observations_parquet: if include_contract_artifacts {
            manifest
                .artifacts
                .observations_parquet
                .as_deref()
                .map(&mut read)
                .transpose()?
        } else {
            None
        },
    })
}

pub fn load_research_snapshot(snapshot_dir: impl AsRef<Path>) -> Result<ResearchSnapshot> {
    let snapshot_dir = snapshot_dir.as_ref();
    let manifest: ResearchSnapshotManifest =
        read_json(snapshot_dir.join("manifest.json")).context("read research snapshot manifest")?;

    if manifest.schema_version != RESEARCH_SNAPSHOT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported research snapshot schema {}; expected {}",
            manifest.schema_version,
            RESEARCH_SNAPSHOT_SCHEMA_VERSION
        );
    }
    let artifact_bytes = load_snapshot_artifact_bytes_with(
        &manifest,
        manifest.snapshot_contract_hash.is_some(),
        |artifact| {
            fs::read(snapshot_dir.join(artifact))
                .with_context(|| format!("read snapshot artifact {artifact}"))
        },
    )?;
    if let Some(recorded_contract_hash) = manifest.snapshot_contract_hash.as_deref() {
        let contract_hex = recorded_contract_hash
            .strip_prefix("sha256:")
            .filter(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .context("research snapshot contract hash must use sha256:<64hex>")?;
        debug_assert_eq!(contract_hex.len(), 64);
        let computed_contract_hash = compute_snapshot_contract_hash(&manifest, &artifact_bytes)
            .context("verify research snapshot evaluator contract hash")?;
        if recorded_contract_hash != computed_contract_hash {
            anyhow::bail!(
                "research snapshot evaluator contract hash mismatch: manifest={} computed={}",
                recorded_contract_hash,
                computed_contract_hash
            );
        }
    }
    let recorded_hash = manifest
        .snapshot_hash
        .as_deref()
        .filter(|hash| !hash.is_empty())
        .context("research snapshot manifest is missing snapshot_hash")?;
    let computed_hash = compute_snapshot_hash(&manifest, &artifact_bytes)
        .context("verify research snapshot content hash")?;
    if recorded_hash != computed_hash {
        anyhow::bail!(
            "research snapshot content hash mismatch: manifest={} computed={}",
            recorded_hash,
            computed_hash
        );
    }

    let observations = serde_json::from_slice(&artifact_bytes.observations_json)
        .context("parse snapshot observations")?;
    let deribit_snapshots = serde_json::from_slice(&artifact_bytes.deribit_snapshots_json)
        .context("parse snapshot Deribit rows")?;
    let pm_book_snapshots = serde_json::from_slice(&artifact_bytes.pm_book_snapshots_json)
        .context("parse snapshot PM book rows")?;

    Ok(ResearchSnapshot {
        manifest,
        observations,
        deribit_snapshots,
        pm_book_snapshots,
    })
}

pub fn write_research_snapshot(
    snapshot_dir: impl AsRef<Path>,
    mut snapshot: ResearchSnapshot,
) -> Result<ResearchSnapshotManifest> {
    if snapshot.manifest.optimizer_data_dir.is_none() {
        anyhow::bail!("research snapshot manifest requires optimizer_data_dir");
    }

    let snapshot_dir = snapshot_dir.as_ref();
    fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("create snapshot dir {}", snapshot_dir.display()))?;

    snapshot.manifest.row_counts = ResearchSnapshotRowCounts {
        observations: snapshot.observations.len(),
        deribit_snapshots: snapshot.deribit_snapshots.len(),
        pm_book_snapshots: snapshot.pm_book_snapshots.len(),
    };

    let observations_json = serde_json::to_vec_pretty(&snapshot.observations)
        .context("serialize snapshot observations")?;
    let deribit_snapshots_json = serde_json::to_vec_pretty(&snapshot.deribit_snapshots)
        .context("serialize snapshot Deribit rows")?;
    let pm_book_snapshots_json = serde_json::to_vec_pretty(&snapshot.pm_book_snapshots)
        .context("serialize snapshot PM book rows")?;
    fs::write(
        snapshot_dir.join(&snapshot.manifest.artifacts.observations_json),
        &observations_json,
    )
    .context("write snapshot observations")?;
    fs::write(
        snapshot_dir.join(&snapshot.manifest.artifacts.deribit_snapshots_json),
        &deribit_snapshots_json,
    )
    .context("write snapshot Deribit rows")?;
    fs::write(
        snapshot_dir.join(&snapshot.manifest.artifacts.pm_book_snapshots_json),
        &pm_book_snapshots_json,
    )
    .context("write snapshot PM book rows")?;

    #[cfg(feature = "polars-export")]
    {
        let parquet_name = "observations.parquet";
        crate::export_observations_parquet(
            &snapshot.observations,
            &snapshot_dir.join(parquet_name),
        )
        .context("write snapshot observations parquet")?;
        snapshot.manifest.artifacts.observations_parquet = Some(parquet_name.to_string());
    }

    let observations_parquet = snapshot
        .manifest
        .artifacts
        .observations_parquet
        .as_deref()
        .map(|artifact| {
            fs::read(snapshot_dir.join(artifact))
                .with_context(|| format!("read written snapshot artifact {artifact}"))
        })
        .transpose()?;
    let artifact_bytes = ResearchSnapshotArtifactBytes {
        observations_json,
        deribit_snapshots_json,
        pm_book_snapshots_json,
        observations_parquet,
    };

    snapshot.manifest.snapshot_hash =
        Some(compute_snapshot_hash(&snapshot.manifest, &artifact_bytes)?);
    snapshot.manifest.snapshot_contract_hash = Some(compute_snapshot_contract_hash(
        &snapshot.manifest,
        &artifact_bytes,
    )?);

    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.query_timings_json),
        &snapshot.manifest.phase_timings,
    )?;
    write_quality_markdown(
        &snapshot_dir.join(&snapshot.manifest.artifacts.quality_markdown),
        &snapshot.manifest,
    )?;
    write_json(snapshot_dir.join("manifest.json"), &snapshot.manifest)?;

    Ok(snapshot.manifest)
}

pub fn validate_snapshot_request(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
) -> Result<()> {
    validate_snapshot_request_with_window_mode(manifest, request, true)
}

pub fn validate_snapshot_request_coverage(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
) -> Result<()> {
    validate_snapshot_request_with_window_mode(manifest, request, false)
}

fn validate_snapshot_request_with_window_mode(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
    require_exact_window: bool,
) -> Result<()> {
    let mut requested_symbols = request.symbols.to_vec();
    requested_symbols.sort();
    let mut snapshot_symbols = manifest.symbols.clone();
    snapshot_symbols.sort();
    if requested_symbols != snapshot_symbols {
        anyhow::bail!(
            "snapshot symbols {:?} do not match requested symbols {:?}",
            snapshot_symbols,
            requested_symbols
        );
    }
    if require_exact_window && (manifest.start != request.start || manifest.end != request.end) {
        anyhow::bail!(
            "snapshot window {} -> {} does not match requested window {} -> {}",
            manifest.start,
            manifest.end,
            request.start,
            request.end
        );
    }
    if !require_exact_window && (manifest.start > request.start || manifest.end < request.end) {
        anyhow::bail!(
            "snapshot window {} -> {} does not cover requested window {} -> {}",
            manifest.start,
            manifest.end,
            request.start,
            request.end
        );
    }
    if manifest.lob_sample_secs != request.lob_sample_secs {
        anyhow::bail!(
            "snapshot lob_sample_secs {} does not match requested {}",
            manifest.lob_sample_secs,
            request.lob_sample_secs
        );
    }
    let manifest_pm_book_sample_secs = manifest
        .pm_book_sample_secs
        .unwrap_or(manifest.lob_sample_secs)
        .max(1);
    let requested_pm_book_sample_secs = request.pm_book_sample_secs.max(1);
    if manifest_pm_book_sample_secs != requested_pm_book_sample_secs {
        anyhow::bail!(
            "snapshot pm_book_sample_secs {} does not match requested {}",
            manifest_pm_book_sample_secs,
            requested_pm_book_sample_secs
        );
    }
    if i64::from(manifest_pm_book_sample_secs) > request.max_quote_age_secs.max(1) {
        anyhow::bail!(
            "snapshot pm_book_sample_secs {} is coarser than requested max_quote_age_secs {}; full-depth execution claims require PM book cadence no coarser than quote-age gate",
            manifest_pm_book_sample_secs,
            request.max_quote_age_secs
        );
    }
    if manifest.observation_sample_secs != request.observation_sample_secs {
        anyhow::bail!(
            "snapshot observation_sample_secs {} does not match requested {}",
            manifest.observation_sample_secs,
            request.observation_sample_secs
        );
    }
    if manifest.max_quote_age_secs != request.max_quote_age_secs {
        anyhow::bail!(
            "snapshot max_quote_age_secs {} does not match requested {}",
            manifest.max_quote_age_secs,
            request.max_quote_age_secs
        );
    }
    if (manifest.stake_usd - request.stake_usd).abs() > 1e-9 {
        anyhow::bail!(
            "snapshot stake_usd {} does not match requested {}",
            manifest.stake_usd,
            request.stake_usd
        );
    }
    if manifest.require_official_settlement != request.require_official_settlement {
        anyhow::bail!(
            "snapshot require_official_settlement {} does not match requested {}",
            manifest.require_official_settlement,
            request.require_official_settlement
        );
    }
    if !manifest.immutable_input {
        anyhow::bail!("snapshot manifest is not marked immutable_input=true");
    }
    if manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("snapshot manifest is missing snapshot_hash");
    }
    Ok(())
}

#[cfg(feature = "db")]
pub struct ResearchSnapshotBuildOptions {
    pub symbols: Vec<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub pm_book_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
    pub optimizer_data_dir: Option<String>,
    pub git_sha: Option<String>,
    pub data_requirements: Vec<String>,
    pub data_audit_status: Option<String>,
    pub data_audit_report: Option<String>,
    pub include_deribit: bool,
    pub pm_book_archive_dir: Option<PathBuf>,
}

#[cfg(any(feature = "db", test))]
fn chainlink_reference_symbols(symbols: &[String]) -> Vec<String> {
    symbols
        .iter()
        .filter_map(|symbol| {
            let underlying = normalized_underlying_symbol(symbol).to_ascii_lowercase();
            (!underlying.is_empty()).then(|| format!("{underlying}/usd"))
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
#[cfg(any(feature = "db", test))]
struct ResearchSnapshotQualityInputs<'a> {
    observation_count: usize,
    chainlink_reference_tick_count: usize,
    binance_price_tick_count: usize,
    binance_agg_trade_tick_count: usize,
    binance_lob_snapshot_count: usize,
    deribit_snapshot_count: usize,
    pm_book_snapshot_count: usize,
    include_deribit: bool,
    pm_book_sample_secs: i32,
    max_quote_age_secs: i64,
    pm_book_source: &'a ResearchSnapshotPmBookSource,
}

#[cfg(any(feature = "db", test))]
fn research_snapshot_quality_flags(input: ResearchSnapshotQualityInputs<'_>) -> Vec<String> {
    let ResearchSnapshotQualityInputs {
        observation_count,
        chainlink_reference_tick_count,
        binance_price_tick_count,
        binance_agg_trade_tick_count,
        binance_lob_snapshot_count,
        deribit_snapshot_count,
        pm_book_snapshot_count,
        include_deribit,
        pm_book_sample_secs,
        max_quote_age_secs,
        pm_book_source,
    } = input;
    let mut quality_flags = Vec::new();
    if observation_count == 0 {
        quality_flags.push("no_factor_observations".to_string());
    }
    if chainlink_reference_tick_count == 0 {
        quality_flags.push("no_chainlink_reference_ticks".to_string());
    }
    if binance_price_tick_count == 0 {
        quality_flags.push("no_binance_price_ticks".to_string());
    }
    if binance_agg_trade_tick_count == 0 {
        quality_flags.push("no_binance_agg_trade_ticks".to_string());
    }
    if binance_lob_snapshot_count == 0 {
        quality_flags.push("no_binance_lob_snapshots".to_string());
    }
    if include_deribit && deribit_snapshot_count == 0 {
        quality_flags.push("no_deribit_snapshots".to_string());
    }
    if pm_book_snapshot_count == 0 {
        quality_flags.push("no_pm_book_snapshots".to_string());
    }
    if i64::from(pm_book_sample_secs.max(1)) > max_quote_age_secs.max(1) {
        quality_flags.push(format!(
            "pm_book_sample_secs_gt_max_quote_age:{pm_book_sample_secs}>{max_quote_age_secs}"
        ));
    }
    if pm_book_source.archive_status == "archive_configured_no_candidate_files" {
        quality_flags.push("pm_book_archive_configured_no_candidate_files".to_string());
    }
    if pm_book_source.archive_status == "archive_configured_no_token_windows" {
        quality_flags.push("pm_book_archive_configured_no_token_windows".to_string());
    }
    if pm_book_source.archive_manifest_rows > 0 && pm_book_source.archive_sampled_rows == 0 {
        quality_flags.push("pm_book_archive_manifest_rows_but_no_sampled_rows".to_string());
    }
    quality_flags
}

#[cfg(feature = "db")]
#[derive(Debug)]
struct ArchivedPmBookLoad {
    snapshots: Vec<ResearchPmBookSnapshot>,
    manifest_rows: usize,
    files: usize,
    token_windows: usize,
    status: String,
}

#[cfg(feature = "db")]
const MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS: usize = 250_000;

#[cfg(feature = "db")]
#[derive(Debug, Deserialize)]
struct ArchiveManifest {
    #[serde(default)]
    row_count: usize,
}

#[cfg(feature = "db")]
#[derive(Debug, Deserialize)]
struct ArchivePmBookRow {
    event_id: Option<String>,
    token_id: String,
    side: Option<String>,
    received_at: String,
    bids: String,
    asks: String,
}

#[cfg(feature = "db")]
#[derive(Debug, Clone)]
struct PmBookTokenWindow {
    market_slug: String,
    token_id: String,
    side: String,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
}

#[cfg(feature = "db")]
fn parse_duckdb_timestamptz(raw: &str) -> Result<DateTime<Utc>> {
    let compact_hour_offset = raw
        .len()
        .checked_sub(3)
        .and_then(|idx| raw.get(idx..))
        .filter(|suffix| {
            let bytes = suffix.as_bytes();
            matches!(bytes.first(), Some(b'+') | Some(b'-'))
                && bytes.get(1).is_some_and(u8::is_ascii_digit)
                && bytes.get(2).is_some_and(u8::is_ascii_digit)
        })
        .map(|_| format!("{raw}:00"));
    DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%:z")
        .or_else(|_| DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%z"))
        .or_else(|_| {
            compact_hour_offset
                .as_deref()
                .map(|normalized| DateTime::parse_from_str(normalized, "%Y-%m-%d %H:%M:%S%.f%:z"))
                .unwrap_or_else(|| DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%:z"))
        })
        .or_else(|_| DateTime::parse_from_rfc3339(raw))
        .map(|ts| ts.with_timezone(&Utc))
        .with_context(|| format!("parse duckdb timestamp {raw:?}"))
}

#[cfg(feature = "db")]
fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(feature = "db")]
fn archive_hour_dirs(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    if end <= start {
        return out;
    }
    let local_start = start + chrono::Duration::hours(8);
    let local_end = end + chrono::Duration::hours(8) - chrono::Duration::microseconds(1);
    let mut hour = local_start
        .date_naive()
        .and_hms_opt(local_start.hour(), 0, 0)
        .expect("valid local archive start hour");
    let end_hour = local_end
        .date_naive()
        .and_hms_opt(local_end.hour(), 0, 0)
        .expect("valid local archive end hour");
    while hour <= end_hour {
        out.push((hour.date().format("%Y-%m-%d").to_string(), hour.hour()));
        hour += chrono::Duration::hours(1);
    }
    out
}

#[cfg(feature = "db")]
fn candidate_archive_files(
    archive_dir: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<(Vec<PathBuf>, usize)> {
    let mut files = Vec::new();
    let mut manifest_rows = 0usize;
    for (date, hour) in archive_hour_dirs(start, end) {
        let hour_dir = archive_dir.join(format!("date={date}/hour={hour:02}"));
        let parquet = hour_dir.join("snapshots.parquet");
        if !parquet.exists() {
            continue;
        }
        let manifest = hour_dir.join("manifest.json");
        if manifest.exists() {
            let parsed: ArchiveManifest = read_json(manifest)?;
            manifest_rows = manifest_rows.saturating_add(parsed.row_count);
        }
        files.push(parquet);
    }
    Ok((files, manifest_rows))
}

#[cfg(feature = "db")]
async fn load_pm_book_token_windows(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_every_secs: i32,
) -> Result<Vec<PmBookTokenWindow>, sqlx::Error> {
    sqlx::query_as::<_, (String, String, String, DateTime<Utc>, DateTime<Utc>)>(
        r#"
        SELECT DISTINCT
            m.market_slug,
            trim(both '"' from token.value::text) AS token_id,
            CASE token.ordinality WHEN 1 THEN 'UP' ELSE 'DOWN' END AS side,
            GREATEST(
                $2::timestamptz,
                COALESCE(m.start_time, $2::timestamptz)
                    - make_interval(secs => $4::int)
            ) AS window_start,
            LEAST(
                $3::timestamptz,
                COALESCE(m.end_time, $3::timestamptz)
                    + make_interval(secs => $4::int)
            ) AS window_end
        FROM pm_market_metadata m
        CROSS JOIN LATERAL jsonb_array_elements(
            (m.raw_market->'markets'->0->>'clobTokenIds')::jsonb
        ) WITH ORDINALITY AS token(value, ordinality)
        WHERE m.symbol = ANY($1)
          AND m.end_time >= $2
          AND m.start_time <= $3
          AND m.raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY market_slug, side
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(sample_every_secs.max(1))
    .fetch_all(pool)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(
                |(market_slug, token_id, side, window_start, window_end)| PmBookTokenWindow {
                    market_slug,
                    token_id,
                    side,
                    window_start,
                    window_end,
                },
            )
            .collect()
    })
}

#[cfg(feature = "db")]
fn archive_token_window_values_sql(windows: &[PmBookTokenWindow]) -> String {
    windows
        .iter()
        .map(|window| {
            format!(
                "({}, {}, {}, TIMESTAMPTZ {}, TIMESTAMPTZ {})",
                sql_string_literal(&window.market_slug),
                sql_string_literal(&window.token_id),
                sql_string_literal(&window.side),
                sql_string_literal(&window.window_start.to_rfc3339()),
                sql_string_literal(&window.window_end.to_rfc3339())
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(feature = "db")]
fn load_archived_pm_book_snapshots_sampled(
    archive_dir: &Path,
    token_windows: &[PmBookTokenWindow],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    sample_every_secs: i32,
) -> Result<ArchivedPmBookLoad> {
    let (files, manifest_rows) = candidate_archive_files(archive_dir, start, end)?;
    if files.is_empty() {
        return Ok(ArchivedPmBookLoad {
            snapshots: Vec::new(),
            manifest_rows,
            files: 0,
            token_windows: token_windows.len(),
            status: "archive_configured_no_candidate_files".to_string(),
        });
    }
    if token_windows.is_empty() {
        return Ok(ArchivedPmBookLoad {
            snapshots: Vec::new(),
            manifest_rows,
            files: files.len(),
            token_windows: 0,
            status: "archive_configured_no_token_windows".to_string(),
        });
    }

    let token_window_values = archive_token_window_values_sql(token_windows);
    let sample_every_secs = sample_every_secs.max(1);
    let start_literal = sql_string_literal(&start.to_rfc3339());
    let end_literal = sql_string_literal(&end.to_rfc3339());
    let duckdb_temp_dir_literal = sql_string_literal(
        &std::env::temp_dir()
            .join("ploy-duckdb")
            .display()
            .to_string(),
    );
    let mut rows = Vec::new();

    for file in &files {
        let row_limit = MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS
            .saturating_sub(rows.len())
            .saturating_add(1);
        let file_literal = sql_string_literal(&file.display().to_string());
        let sql = format!(
            r#"
SET threads = 1;
SET memory_limit = '1024MB';
SET temp_directory = {duckdb_temp_dir_literal};
WITH token_map(event_id, token_id, side, window_start, window_end) AS (
  VALUES {token_window_values}
),
raw_keys AS (
  SELECT
    t.event_id,
    t.token_id,
    t.side,
    o.received_at,
    floor(epoch(o.received_at) / {sample_every_secs}) AS bucket
  FROM read_parquet({file_literal}) o
  JOIN token_map t
    ON o.token_id = t.token_id
   AND o.received_at >= t.window_start
   AND o.received_at < t.window_end
  WHERE o.received_at >= TIMESTAMPTZ {start_literal}
    AND o.received_at < TIMESTAMPTZ {end_literal}
),
ranked AS (
  SELECT
    event_id,
    token_id,
    side,
    received_at,
    row_number() OVER (
      PARTITION BY token_id, bucket
      ORDER BY received_at DESC
    ) AS rn
  FROM raw_keys
)
SELECT
  r.event_id,
  r.token_id,
  r.side,
  r.received_at,
  o.bids,
  o.asks
FROM ranked r
JOIN read_parquet({file_literal}) o
  ON o.token_id = r.token_id
 AND o.received_at = r.received_at
WHERE r.rn = 1
ORDER BY r.received_at
LIMIT {row_limit}
"#
        );
        rows.extend(run_duckdb_archive_pm_book_query(sql)?);
        if rows.len() > MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS {
            anyhow::bail!(
                "archived PM book sampled rows exceeded safety limit {} for {} token windows; narrow the window or raise pm_book_sample_secs",
                MAX_ARCHIVED_PM_BOOK_SAMPLED_ROWS,
                token_windows.len()
            );
        }
    }

    let snapshots = rows
        .into_iter()
        .map(|row| {
            let ts = parse_duckdb_timestamptz(&row.received_at)?;
            let bids: serde_json::Value = serde_json::from_str(&row.bids)
                .with_context(|| format!("parse archived bids JSON for {}", row.token_id))?;
            let asks: serde_json::Value = serde_json::from_str(&row.asks)
                .with_context(|| format!("parse archived asks JSON for {}", row.token_id))?;
            Ok(ResearchPmBookSnapshot {
                event_id: row.event_id.unwrap_or_default(),
                token_id: row.token_id,
                side: row.side.unwrap_or_default(),
                ts,
                bids: crate::factors::research_pm_book_levels_from_json(&bids, true),
                asks: crate::factors::research_pm_book_levels_from_json(&asks, false),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ArchivedPmBookLoad {
        snapshots,
        manifest_rows,
        files: files.len(),
        token_windows: token_windows.len(),
        status: "archive_loaded".to_string(),
    })
}

#[cfg(feature = "db")]
fn run_duckdb_archive_pm_book_query(sql: String) -> Result<Vec<ArchivePmBookRow>> {
    let duckdb_temp_dir = std::env::temp_dir().join("ploy-duckdb");
    fs::create_dir_all(&duckdb_temp_dir)
        .with_context(|| format!("create DuckDB temp dir {}", duckdb_temp_dir.display()))?;
    let sql_path = duckdb_temp_dir.join(format!(
        "archive-pm-books-{}-{}.sql",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    fs::write(&sql_path, sql)
        .with_context(|| format!("write DuckDB archive query {}", sql_path.display()))?;
    let sql_file = File::open(&sql_path)
        .with_context(|| format!("open DuckDB archive query {}", sql_path.display()))?;
    let output = Command::new("duckdb")
        .arg("-json")
        .stdin(sql_file)
        .output()
        .context("run duckdb for archived PM book snapshots")?;
    let _ = fs::remove_file(&sql_path);
    if !output.status.success() {
        anyhow::bail!(
            "duckdb archived PM book load failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse duckdb archived PM book JSON rows")
}

#[cfg(feature = "db")]
fn merge_pm_book_snapshots(
    mut hot_rows: Vec<ResearchPmBookSnapshot>,
    archive_rows: Vec<ResearchPmBookSnapshot>,
) -> Vec<ResearchPmBookSnapshot> {
    hot_rows.extend(archive_rows);
    hot_rows.sort_by(|a, b| {
        (a.ts, &a.event_id, &a.token_id, &a.side).cmp(&(b.ts, &b.event_id, &b.token_id, &b.side))
    });
    hot_rows.dedup_by(|a, b| {
        a.ts == b.ts && a.event_id == b.event_id && a.token_id == b.token_id && a.side == b.side
    });
    hot_rows
}

type OfficialOutcomeKey = (String, String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OfficialBinaryOutcome {
    settlement_up: bool,
    observed_at: DateTime<Utc>,
}

#[cfg(feature = "db")]
async fn load_official_binary_outcomes(
    pool: &sqlx::PgPool,
    observations: &[FactorObservation],
) -> Result<HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>> {
    let contracts = observations
        .iter()
        .map(|row| {
            (
                row.event_id.clone(),
                row.up_token_id.clone(),
                row.down_token_id.clone(),
            )
        })
        .collect::<Vec<_>>();
    let mut connection = pool
        .acquire()
        .await
        .context("acquire connection for official binary outcomes")?;
    load_official_binary_outcomes_for_contracts(&mut connection, &contracts).await
}

#[cfg(feature = "db")]
async fn load_official_binary_outcomes_for_contracts(
    connection: &mut sqlx::PgConnection,
    contracts: &[OfficialOutcomeKey],
) -> Result<HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>> {
    let mut contracts = contracts.to_vec();
    contracts.sort();
    contracts.dedup();
    if contracts.is_empty() {
        return Ok(HashMap::new());
    }

    let event_ids = contracts
        .iter()
        .map(|(event_id, _, _)| event_id.clone())
        .collect::<Vec<_>>();
    let up_token_ids = contracts
        .iter()
        .map(|(_, up_token_id, _)| up_token_id.clone())
        .collect::<Vec<_>>();
    let down_token_ids = contracts
        .iter()
        .map(|(_, _, down_token_id)| down_token_id.clone())
        .collect::<Vec<_>>();
    let rows: Vec<(String, String, String, bool, DateTime<Utc>)> = sqlx::query_as(
        r#"
        WITH requested(event_id, up_token_id, down_token_id) AS (
            SELECT *
            FROM UNNEST($1::text[], $2::text[], $3::text[])
        )
        SELECT
            requested.event_id,
            requested.up_token_id,
            requested.down_token_id,
            up_settlement.settled_price = 1::numeric AS settlement_up,
            GREATEST(
                COALESCE(up_settlement.resolved_at, up_settlement.fetched_at),
                up_settlement.fetched_at,
                COALESCE(down_settlement.resolved_at, down_settlement.fetched_at),
                down_settlement.fetched_at
            ) AS label_observed_at
        FROM requested
        JOIN pm_token_settlements AS up_settlement
          ON up_settlement.token_id = requested.up_token_id
         AND up_settlement.resolved = TRUE
        JOIN pm_token_settlements AS down_settlement
          ON down_settlement.token_id = requested.down_token_id
         AND down_settlement.resolved = TRUE
        WHERE up_settlement.settled_price IN (0::numeric, 1::numeric)
          AND down_settlement.settled_price IN (0::numeric, 1::numeric)
          AND up_settlement.settled_price + down_settlement.settled_price = 1::numeric
          AND (
                (up_settlement.condition_id IS NOT NULL
                 AND up_settlement.condition_id = down_settlement.condition_id)
             OR (up_settlement.market_id IS NOT NULL
                 AND up_settlement.market_id = down_settlement.market_id)
             OR (up_settlement.market_slug IS NOT NULL
                 AND up_settlement.market_slug = down_settlement.market_slug)
          )
          AND (up_settlement.condition_id IS NULL
               OR down_settlement.condition_id IS NULL
               OR up_settlement.condition_id = down_settlement.condition_id)
          AND (up_settlement.market_id IS NULL
               OR down_settlement.market_id IS NULL
               OR up_settlement.market_id = down_settlement.market_id)
          AND (up_settlement.market_slug IS NULL
               OR down_settlement.market_slug IS NULL
               OR up_settlement.market_slug = down_settlement.market_slug)
        "#,
    )
    .bind(&event_ids)
    .bind(&up_token_ids)
    .bind(&down_token_ids)
    .fetch_all(connection)
    .await
    .context("load exact official binary outcomes and observation clocks")?;

    Ok(rows
        .into_iter()
        .map(
            |(event_id, up_token_id, down_token_id, settlement_up, observed_at)| {
                (
                    (event_id, up_token_id, down_token_id),
                    OfficialBinaryOutcome {
                        settlement_up,
                        observed_at,
                    },
                )
            },
        )
        .collect())
}

fn exact_official_binary_outcome<'a>(
    outcomes: &'a HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>,
    event_id: &str,
    up_token_id: &str,
    down_token_id: &str,
) -> Option<&'a OfficialBinaryOutcome> {
    outcomes.get(&(
        event_id.to_string(),
        up_token_id.to_string(),
        down_token_id.to_string(),
    ))
}

#[cfg(feature = "db")]
fn bind_official_binary_outcomes(
    observations: &mut [FactorObservation],
    outcomes: &HashMap<OfficialOutcomeKey, OfficialBinaryOutcome>,
    require_official_settlement: bool,
) -> Result<()> {
    for row in observations {
        if let Some(outcome) = exact_official_binary_outcome(
            outcomes,
            &row.event_id,
            &row.up_token_id,
            &row.down_token_id,
        ) {
            row.settlement_up = f64::from(outcome.settlement_up);
            row.official_resolution_observed_at = Some(outcome.observed_at);
        } else {
            row.official_resolution_observed_at = None;
            if require_official_settlement {
                anyhow::bail!(
                    "exact complementary official settlement pair is missing for event {} tokens {}/{}",
                    row.event_id,
                    row.up_token_id,
                    row.down_token_id
                );
            }
        }
    }
    Ok(())
}

#[cfg(feature = "db")]
pub async fn build_research_snapshot_from_database(
    pool: &sqlx::PgPool,
    options: ResearchSnapshotBuildOptions,
) -> Result<ResearchSnapshot> {
    use ploy_feed_loaders::{
        load_from_database_with_options_and_source_clocks, HistoricalLoadOptions,
    };
    use ploy_market_contracts::MarketUpdate;

    use crate::{
        build_factor_observations_with_lob_sampled_and_source_clocks,
        load_deribit_feature_snapshots_with_timings, load_research_lob_snapshots_sampled,
        load_research_pm_book_snapshots_sampled,
    };

    let mut phase_timings = Vec::new();
    let history_start = options.start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let historical_sample_secs = u32::try_from(options.lob_sample_secs.max(1)).unwrap_or(1);

    let started = Instant::now();
    let historical_batch = load_from_database_with_options_and_source_clocks(
        pool,
        &options.symbols,
        history_start,
        options.end,
        &HistoricalLoadOptions {
            include_reference_prices: true,
            reference_symbols: chainlink_reference_symbols(&options.symbols),
            require_official_settlement: options.require_official_settlement,
            include_l2: false,
            spot_sample_secs: historical_sample_secs,
            lob_sample_secs: historical_sample_secs,
            max_source_delay_secs: u32::try_from(options.max_quote_age_secs.max(0))
                .unwrap_or(u32::MAX),
            ..Default::default()
        },
    )
    .await
    .context("load historical market updates")?;
    let all_updates = historical_batch.updates;
    let binance_source_clocks = historical_batch.binance_source_clocks;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "historical_updates".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_updates.len()),
    });
    let binance_price_tick_rows = all_updates
        .iter()
        .filter(|update| matches!(update, MarketUpdate::SpotPrice { .. }))
        .count();
    let binance_agg_trade_tick_rows = all_updates
        .iter()
        .filter(|update| matches!(update, MarketUpdate::AggTrade { .. }))
        .count();
    let chainlink_reference_tick_rows = all_updates
        .iter()
        .filter(|update| {
            matches!(
                update,
                MarketUpdate::ReferencePrice {
                    source,
                    is_carried_forward: false,
                    received_at: Some(_),
                    ..
                } if source.eq_ignore_ascii_case("chainlink")
            )
        })
        .count();

    let started = Instant::now();
    let all_lob_snapshots = load_research_lob_snapshots_sampled(
        pool,
        &options.symbols,
        history_start,
        options.end,
        options.lob_sample_secs,
        options.max_quote_age_secs,
    )
    .await
    .context("load CEX LOB snapshots")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "cex_lob_snapshots".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_lob_snapshots.len()),
    });

    let started = Instant::now();
    let pm_book_sample_secs = options.pm_book_sample_secs.max(1);
    let hot_pm_book_snapshots = load_research_pm_book_snapshots_sampled(
        pool,
        &options.symbols,
        history_start,
        options.end,
        pm_book_sample_secs,
    )
    .await
    .context("load PM book snapshots")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "pm_book_snapshots_hot_postgres".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(hot_pm_book_snapshots.len()),
    });

    let mut pm_book_source = ResearchSnapshotPmBookSource {
        hot_postgres_sampled_rows: hot_pm_book_snapshots.len(),
        archive_dir: options
            .pm_book_archive_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        archive_status: "archive_not_configured".to_string(),
        ..Default::default()
    };
    let archived_pm_book_snapshots =
        if let Some(archive_dir) = options.pm_book_archive_dir.as_deref() {
            let started = Instant::now();
            let token_windows = load_pm_book_token_windows(
                pool,
                &options.symbols,
                history_start,
                options.end,
                pm_book_sample_secs,
            )
            .await
            .context("load PM book token windows for archive snapshot")?;
            pm_book_source.archive_token_windows = token_windows.len();
            phase_timings.push(ResearchSnapshotPhaseTiming {
                phase: "pm_book_archive_token_windows".to_string(),
                elapsed_ms: started.elapsed().as_millis(),
                rows: Some(token_windows.len()),
            });

            let started = Instant::now();
            let archived = load_archived_pm_book_snapshots_sampled(
                archive_dir,
                &token_windows,
                history_start,
                options.end,
                pm_book_sample_secs,
            )
            .with_context(|| {
                format!(
                    "load archived PM book snapshots from {}",
                    archive_dir.display()
                )
            })?;
            pm_book_source.archive_sampled_rows = archived.snapshots.len();
            pm_book_source.archive_manifest_rows = archived.manifest_rows;
            pm_book_source.archive_files = archived.files;
            pm_book_source.archive_token_windows = archived.token_windows;
            pm_book_source.archive_status = archived.status;
            phase_timings.push(ResearchSnapshotPhaseTiming {
                phase: "pm_book_snapshots_archive".to_string(),
                elapsed_ms: started.elapsed().as_millis(),
                rows: Some(archived.snapshots.len()),
            });
            archived.snapshots
        } else {
            Vec::new()
        };
    let all_pm_book_snapshots =
        merge_pm_book_snapshots(hot_pm_book_snapshots, archived_pm_book_snapshots);
    pm_book_source.merged_sampled_rows = all_pm_book_snapshots.len();
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "pm_book_snapshots_merged".to_string(),
        elapsed_ms: 0,
        rows: Some(all_pm_book_snapshots.len()),
    });

    let deribit_snapshots = if options.include_deribit {
        let started = Instant::now();
        let deribit_result = load_deribit_feature_snapshots_with_timings(
            pool,
            &options.symbols,
            options.start,
            options.end,
            options.observation_sample_secs,
        )
        .await;
        let deribit_snapshots = deribit_result.snapshots;
        phase_timings.extend(deribit_result.phase_timings);
        phase_timings.push(ResearchSnapshotPhaseTiming {
            phase: "deribit_snapshots".to_string(),
            elapsed_ms: started.elapsed().as_millis(),
            rows: Some(deribit_snapshots.len()),
        });
        deribit_snapshots
    } else {
        phase_timings.push(ResearchSnapshotPhaseTiming {
            phase: "deribit_snapshots_skipped".to_string(),
            elapsed_ms: 0,
            rows: Some(0),
        });
        Vec::new()
    };

    let started = Instant::now();
    let updates_slice = slice_by_time(
        &all_updates,
        history_start,
        options.end,
        MarketUpdate::sort_ts,
    );
    let lob_slice = slice_by_time(&all_lob_snapshots, history_start, options.end, |snapshot| {
        snapshot.ts
    });
    let mut observations = build_factor_observations_with_lob_sampled_and_source_clocks(
        updates_slice,
        lob_slice,
        &binance_source_clocks,
        options.max_quote_age_secs,
        options.observation_sample_secs,
    );
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "factor_observations".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(observations.len()),
    });

    let started = Instant::now();
    let official_binary_outcomes = load_official_binary_outcomes(pool, &observations).await?;
    bind_official_binary_outcomes(
        &mut observations,
        &official_binary_outcomes,
        options.require_official_settlement,
    )?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "exact_official_binary_outcomes".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(official_binary_outcomes.len()),
    });

    let quality_flags = research_snapshot_quality_flags(ResearchSnapshotQualityInputs {
        observation_count: observations.len(),
        chainlink_reference_tick_count: chainlink_reference_tick_rows,
        binance_price_tick_count: binance_price_tick_rows,
        binance_agg_trade_tick_count: binance_agg_trade_tick_rows,
        binance_lob_snapshot_count: all_lob_snapshots.len(),
        deribit_snapshot_count: deribit_snapshots.len(),
        pm_book_snapshot_count: all_pm_book_snapshots.len(),
        include_deribit: options.include_deribit,
        pm_book_sample_secs,
        max_quote_age_secs: options.max_quote_age_secs,
        pm_book_source: &pm_book_source,
    });
    let symbols_csv = options.symbols.join(",");

    Ok(ResearchSnapshot {
        manifest: ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: None,
            snapshot_contract_hash: None,
            generated_at: Utc::now(),
            git_sha: options.git_sha,
            symbols: options.symbols,
            start: options.start,
            end: options.end,
            history_start,
            lob_sample_secs: options.lob_sample_secs,
            pm_book_sample_secs: Some(pm_book_sample_secs),
            observation_sample_secs: options.observation_sample_secs,
            max_quote_age_secs: options.max_quote_age_secs,
            stake_usd: options.stake_usd,
            require_official_settlement: options.require_official_settlement,
            immutable_input: true,
            source_kind: "tango_postgres_compiled_snapshot".to_string(),
            optimizer_data_dir: options.optimizer_data_dir,
            source_surfaces: vec![
                ResearchSnapshotSourceSurface {
                    name: "chainlink_reference_ticks".to_string(),
                    role: "opening_reference_and_expiry_price_source".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(historical_sample_secs)),
                    row_count: Some(chainlink_reference_tick_rows),
                    notes: "Arrival-timestamped Chainlink prices define the governed opening reference and expiry-price semantics; Polymarket official resolution remains the binary label, and Binance never replaces either authority.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "binance_price_ticks".to_string(),
                    role: "cex_reference_price".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(historical_sample_secs)),
                    row_count: Some(binance_price_tick_rows),
                    notes: "Binance spot ticks sampled by source time and replayed at received_at so unavailable prices cannot enter earlier observations.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "binance_agg_trade_ticks".to_string(),
                    role: "cex_trade_flow".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(5),
                    row_count: Some(binance_agg_trade_tick_rows),
                    notes: "Binance aggTrade flow aggregated by source-time 5-second bucket and aggressor side, then replayed at received_at.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "historical_market_updates".to_string(),
                    role: "prediction_context".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(historical_sample_secs)),
                    row_count: Some(all_updates.len()),
                    notes: "Combined DB MarketUpdate tape includes point-in-time Chainlink reference prices and sampled PM updates; suitable for factor search, not tick-complete replay.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "binance_lob_ticks".to_string(),
                    role: "prediction_lob_context".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(options.lob_sample_secs.max(1))),
                    row_count: Some(all_lob_snapshots.len()),
                    notes: "Binance partial-depth LOB snapshots sampled by source time and replayed at received_at; not a sequence-correct local book for queue-position evidence.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "clob_orderbook_snapshots".to_string(),
                    role: "execution_depth_context".to_string(),
                    gate_category: "required_for_execution".to_string(),
                    raw_full_fidelity: true,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(pm_book_sample_secs)),
                    row_count: Some(all_pm_book_snapshots.len()),
                    notes: "Raw Polymarket full-depth CLOB surface exists, but this research snapshot stores sampled book states.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "pm_token_settlements".to_string(),
                    role: "settlement_labels".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: true,
                    snapshot_sampled: false,
                    sample_secs: None,
                    row_count: Some(official_binary_outcomes.len()),
                    notes: "Exact complementary UP/DOWN official outcome pairs and their content-version availability clocks are required for prediction evaluation when require_official_settlement=true; they are not execution evidence.".to_string(),
                },
                ResearchSnapshotSourceSurface {
                    name: "deribit_feature_snapshots".to_string(),
                    role: "optional_vol_context".to_string(),
                    gate_category: "optional_context".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: options.include_deribit,
                    sample_secs: if options.include_deribit {
                        Some(options.observation_sample_secs)
                    } else {
                        None
                    },
                    row_count: Some(deribit_snapshots.len()),
                    notes: if options.include_deribit {
                        "Deribit context materialized at observation cadence.".to_string()
                    } else {
                        "Deribit context intentionally excluded for this profile.".to_string()
                    },
                },
            ],
            input_artifacts: vec![ResearchSnapshotInputArtifact {
                name: "tango_postgres_research_window".to_string(),
                path: format!(
                    "tango_postgres://research_snapshot?start={}&end={}&symbols={}",
                    options.start,
                    options.end,
                    symbols_csv
                ),
                content_hash: None,
                row_count: Some(
                    all_updates.len()
                        + all_lob_snapshots.len()
                        + all_pm_book_snapshots.len()
                        + deribit_snapshots.len()
                        + official_binary_outcomes.len(),
                ),
            }],
            data_requirements: options.data_requirements,
            data_audit_status: options.data_audit_status,
            data_audit_report: options.data_audit_report,
            include_deribit: options.include_deribit,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts {
                observations: observations.len(),
                deribit_snapshots: deribit_snapshots.len(),
                pm_book_snapshots: all_pm_book_snapshots.len(),
            },
            phase_timings,
            quality_flags,
            pm_book_source,
        },
        observations,
        deribit_snapshots,
        pm_book_snapshots: all_pm_book_snapshots,
    })
}

#[cfg(feature = "db")]
fn slice_by_time<T, F>(items: &[T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F) -> &[T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|item| ts_fn(item) < start);
    let hi = items.partition_point(|item| ts_fn(item) < end);
    &items[lo..hi]
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("parse {}", path.display()))
}

fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, value).with_context(|| format!("write {}", path.display()))
}

fn compute_snapshot_hash(
    manifest: &ResearchSnapshotManifest,
    artifacts: &ResearchSnapshotArtifactBytes,
) -> Result<String> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    let mut hash = FNV_OFFSET;
    update(&mut hash, RESEARCH_SNAPSHOT_SCHEMA_VERSION.as_bytes());
    update(&mut hash, manifest.start.to_rfc3339().as_bytes());
    update(&mut hash, manifest.end.to_rfc3339().as_bytes());
    update(&mut hash, manifest.symbols.join(",").as_bytes());
    update(&mut hash, manifest.stake_usd.to_string().as_bytes());
    update(
        &mut hash,
        manifest
            .pm_book_sample_secs
            .map(|value| value.to_string())
            .unwrap_or_default()
            .as_bytes(),
    );
    update(&mut hash, manifest.data_requirements.join(",").as_bytes());
    update(
        &mut hash,
        serde_json::to_string(&manifest.source_surfaces)
            .context("serialize source_surfaces for snapshot hash")?
            .as_bytes(),
    );
    update(
        &mut hash,
        serde_json::to_string(&manifest.input_artifacts)
            .context("serialize input_artifacts for snapshot hash")?
            .as_bytes(),
    );
    update(&mut hash, manifest.include_deribit.to_string().as_bytes());
    update(&mut hash, manifest.pm_book_source.archive_status.as_bytes());
    update(
        &mut hash,
        manifest
            .pm_book_source
            .archive_dir
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .pm_book_source
            .archive_manifest_rows
            .to_string()
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .pm_book_source
            .archive_token_windows
            .to_string()
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .data_audit_status
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .data_audit_report
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    for (artifact, bytes) in [
        (
            manifest.artifacts.observations_json.as_str(),
            artifacts.observations_json.as_slice(),
        ),
        (
            manifest.artifacts.deribit_snapshots_json.as_str(),
            artifacts.deribit_snapshots_json.as_slice(),
        ),
        (
            manifest.artifacts.pm_book_snapshots_json.as_str(),
            artifacts.pm_book_snapshots_json.as_slice(),
        ),
    ] {
        update(&mut hash, artifact.as_bytes());
        update(&mut hash, bytes);
    }
    Ok(format!("{hash:016x}"))
}

fn compute_snapshot_contract_hash(
    manifest: &ResearchSnapshotManifest,
    artifacts: &ResearchSnapshotArtifactBytes,
) -> Result<String> {
    fn update_framed(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
    }

    let mut contract_manifest = manifest.clone();
    contract_manifest.snapshot_hash = None;
    contract_manifest.snapshot_contract_hash = None;

    let manifest_bytes = serde_json::to_vec(&contract_manifest)
        .context("serialize manifest for research snapshot evaluator contract hash")?;
    let mut hasher = Sha256::new();
    update_framed(&mut hasher, b"ploy.research_snapshot.evaluator_contract.v1");
    update_framed(&mut hasher, &manifest_bytes);

    for (artifact, bytes) in [
        (
            manifest.artifacts.observations_json.as_str(),
            artifacts.observations_json.as_slice(),
        ),
        (
            manifest.artifacts.deribit_snapshots_json.as_str(),
            artifacts.deribit_snapshots_json.as_slice(),
        ),
        (
            manifest.artifacts.pm_book_snapshots_json.as_str(),
            artifacts.pm_book_snapshots_json.as_slice(),
        ),
    ] {
        update_framed(&mut hasher, artifact.as_bytes());
        update_framed(&mut hasher, bytes);
    }
    match (
        manifest.artifacts.observations_parquet.as_deref(),
        artifacts.observations_parquet.as_deref(),
    ) {
        (Some(artifact), Some(bytes)) => {
            update_framed(&mut hasher, artifact.as_bytes());
            update_framed(&mut hasher, bytes);
        }
        (None, None) => {}
        (Some(_), None) => anyhow::bail!(
            "snapshot manifest declares observations_parquet but its bytes are unavailable"
        ),
        (None, Some(_)) => {
            anyhow::bail!("snapshot artifact bytes contain undeclared observations_parquet data")
        }
    }

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn write_quality_markdown(path: &Path, manifest: &ResearchSnapshotManifest) -> Result<()> {
    let mut body = String::new();
    body.push_str("# Research Snapshot Quality\n\n");
    body.push_str(&format!("- Schema: `{}`\n", manifest.schema_version));
    body.push_str(&format!(
        "- Snapshot hash: `{}`\n",
        manifest.snapshot_hash.as_deref().unwrap_or("<missing>")
    ));
    body.push_str(&format!(
        "- Snapshot contract hash: `{}`\n",
        manifest
            .snapshot_contract_hash
            .as_deref()
            .unwrap_or("<legacy-unavailable>")
    ));
    body.push_str(&format!("- Generated at: `{}`\n", manifest.generated_at));
    body.push_str(&format!(
        "- Window: `{}` -> `{}`\n",
        manifest.start, manifest.end
    ));
    body.push_str(&format!("- Symbols: `{}`\n", manifest.symbols.join(",")));
    body.push_str(&format!(
        "- LOB sample secs: `{}`\n",
        manifest.lob_sample_secs
    ));
    body.push_str(&format!(
        "- PM book sample secs: `{}`\n",
        manifest
            .pm_book_sample_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<same-as-lob>".to_string())
    ));
    body.push_str(&format!(
        "- Observation sample secs: `{}`\n",
        manifest.observation_sample_secs
    ));
    body.push_str(&format!(
        "- Immutable input: `{}`\n",
        manifest.immutable_input
    ));
    body.push_str(&format!("- Source kind: `{}`\n", manifest.source_kind));
    body.push_str(&format!(
        "- Optimizer data dir: `{}`\n",
        manifest
            .optimizer_data_dir
            .as_deref()
            .unwrap_or("<missing>")
    ));
    body.push_str("\n## Source Surfaces\n\n");
    if manifest.source_surfaces.is_empty() {
        body.push_str("- `<not-recorded>`\n");
    } else {
        for surface in &manifest.source_surfaces {
            body.push_str(&format!(
                "- `{}` role=`{}` gate_category=`{}` raw_full_fidelity=`{}` snapshot_sampled=`{}` sample_secs=`{}` rows=`{}` notes=`{}`\n",
                surface.name,
                surface.role,
                surface.gate_category,
                surface.raw_full_fidelity,
                surface.snapshot_sampled,
                surface
                    .sample_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                surface
                    .row_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
                surface.notes
            ));
        }
    }
    body.push_str("\n## Input Artifacts\n\n");
    if manifest.input_artifacts.is_empty() {
        body.push_str("- `<database-or-remote-source>`\n");
    } else {
        for artifact in &manifest.input_artifacts {
            body.push_str(&format!(
                "- `{}` path=`{}` hash=`{}` rows=`{}`\n",
                artifact.name,
                artifact.path,
                artifact.content_hash.as_deref().unwrap_or("<missing>"),
                artifact
                    .row_count
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            ));
        }
    }
    body.push_str(&format!(
        "- Data requirements: `{}`\n",
        if manifest.data_requirements.is_empty() {
            "<unspecified>".to_string()
        } else {
            manifest.data_requirements.join(",")
        }
    ));
    body.push_str(&format!(
        "- Data audit status: `{}`\n",
        manifest
            .data_audit_status
            .as_deref()
            .unwrap_or("<not-recorded>")
    ));
    body.push_str(&format!(
        "- Data audit report: `{}`\n",
        manifest
            .data_audit_report
            .as_deref()
            .unwrap_or("<not-recorded>")
    ));
    body.push_str(&format!(
        "- Deribit included: `{}`\n",
        manifest.include_deribit
    ));
    body.push_str(&format!(
        "- Rows: observations={}, deribit={}, pm_books={}\n",
        manifest.row_counts.observations,
        manifest.row_counts.deribit_snapshots,
        manifest.row_counts.pm_book_snapshots
    ));
    body.push_str(&format!(
        "- PM book source: hot_postgres_sampled_rows={}, archive_sampled_rows={}, archive_manifest_rows={}, archive_files={}, archive_token_windows={}, merged_sampled_rows={}, archive_status=`{}` archive_dir=`{}`\n",
        manifest.pm_book_source.hot_postgres_sampled_rows,
        manifest.pm_book_source.archive_sampled_rows,
        manifest.pm_book_source.archive_manifest_rows,
        manifest.pm_book_source.archive_files,
        manifest.pm_book_source.archive_token_windows,
        manifest.pm_book_source.merged_sampled_rows,
        manifest.pm_book_source.archive_status,
        manifest
            .pm_book_source
            .archive_dir
            .as_deref()
            .unwrap_or("<not-configured>")
    ));
    body.push_str(&format!(
        "- Official settlement required: `{}`\n",
        manifest.require_official_settlement
    ));
    body.push_str("\n## Phase Timings\n\n");
    for timing in &manifest.phase_timings {
        body.push_str(&format!(
            "- `{}`: {} ms, rows={}\n",
            timing.phase,
            timing.elapsed_ms,
            timing
                .rows
                .map(|rows| rows.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }
    if !manifest.quality_flags.is_empty() {
        body.push_str("\n## Quality Flags\n\n");
        for flag in &manifest.quality_flags {
            body.push_str(&format!("- `{flag}`\n"));
        }
    }
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ResearchPmBookLevel;

    fn verified_contract(available_at: &str) -> ResearchPolymarketContract {
        ResearchPolymarketContract {
            event_id: "event-1".into(),
            symbol: "BTCUSDT".into(),
            up_token_id: "up".into(),
            down_token_id: "down".into(),
            event_start: "2026-07-17T05:30:00Z".parse().unwrap(),
            event_end: "2026-07-17T05:35:00Z".parse().unwrap(),
            price_to_beat: "63000".parse().unwrap(),
            available_at: available_at.parse().unwrap(),
        }
    }

    fn verified_observation(tick_ts: &str) -> FactorObservation {
        let mut row: FactorObservation = serde_json::from_value(serde_json::json!({
            "event_id": "event-1",
            "symbol": "BTCUSDT",
            "tick_ts": tick_ts,
            "chainlink_reference_fresh": true,
            "binance_spot_fresh": true,
            "binance_lob_fresh": true,
            "binance_agg_trade_fresh": true,
            "time_remaining_secs": 60,
            "signed_distance_to_beat": 0.0,
            "abs_distance_to_beat": 0.0,
            "drift_10s": 0.0,
            "drift_30s": 0.0,
            "post_flip_drift": 0.0,
            "sigma_horizon": 1.0,
            "distance_over_sigma": 0.0,
            "model_prob_up": 0.5,
            "obi": 0.0,
            "spread_bps": 0.0,
            "bid_depth_near": 1.0,
            "ask_depth_near": 1.0,
            "obi_10": 0.0,
            "pm_lag_secs": 0.0,
            "settlement_up": 0.0,
            "future_up_ask_change_30s": null,
            "future_up_ask_change_60s": null,
            "cum_obi_delta_5m": 0.0,
            "cum_depth_delta_5m": 0.0,
            "cum_mprice_drift_5m": 0.0,
            "cum_trade_imbalance_5m": 0.0,
            "cex_bar_return_30s": 0.0,
            "cex_consecutive_up_bars": 0.0,
            "cex_consecutive_down_bars": 0.0
        }))
        .unwrap();
        row.up_token_id = "up".into();
        row.down_token_id = "down".into();
        row.settlement_up = f64::NAN;
        row
    }

    fn authenticated_test_cohort() -> AuthenticatedReadyEventCohort {
        AuthenticatedReadyEventCohort {
            manifest_id: format!("sha256:{}", "a".repeat(64)),
            partition_digest: format!("sha256:{}", "b".repeat(64)),
            causal_projection_policy_id: format!("sha256:{}", "c".repeat(64)),
            members: vec![AuthenticatedReadyEvent {
                receipt_sha256: "d".repeat(64),
                market_id: "event-1".to_string(),
                content_sha256: "e".repeat(64),
                manifest_sha256: "f".repeat(64),
                qualification_sha256: "1".repeat(64),
                success_sha256: "2".repeat(64),
                start: "2026-07-17T05:30:00Z".parse().unwrap(),
                end: "2026-07-17T05:35:00Z".parse().unwrap(),
                up_token_id: "up".to_string(),
                down_token_id: "down".to_string(),
            }],
        }
    }

    fn authenticated_test_request(
        cache_root: PathBuf,
    ) -> AuthenticatedSnapshotMaterializationRequest {
        AuthenticatedSnapshotMaterializationRequest {
            cache_root,
            compiler_source_identity: format!("sha256:{}", "3".repeat(64)),
            compiler_image_identity: format!("sha256:{}", "4".repeat(64)),
            build_input_identity: format!("sha256:{}", "5".repeat(64)),
        }
    }

    fn authenticated_test_snapshot() -> ResearchSnapshot {
        let cohort = authenticated_test_cohort();
        let member = &cohort.members[0];
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let mut observation = bind_and_filter_polymarket_chainlink_baseline_observations(
            vec![verified_observation("2026-07-17T05:31:00Z")],
            std::slice::from_ref(&contract),
            &verified_outcomes(true),
            "BTCUSDT",
            member.start,
            member.end,
        )
        .expect("authenticated observation fixture")
        .remove(0);
        observation.event_window_secs = 300;
        observation.pm_up_bid = 0.4;
        observation.pm_up_ask = 0.6;
        observation.pm_down_bid = 0.4;
        observation.pm_down_ask = 0.6;
        let tick_ts = observation.tick_ts;
        let book = |token_id: &str, side: &str| ResearchPmBookSnapshot {
            event_id: member.market_id.clone(),
            token_id: token_id.to_string(),
            side: side.to_string(),
            ts: tick_ts,
            bids: vec![ResearchPmBookLevel {
                price: 0.4,
                size: 10.0,
            }],
            asks: vec![ResearchPmBookLevel {
                price: 0.6,
                size: 10.0,
            }],
        };
        ResearchSnapshot {
            manifest: ResearchSnapshotManifest {
                schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
                snapshot_hash: None,
                snapshot_contract_hash: None,
                generated_at: Utc::now(),
                git_sha: Some("test-sha".to_string()),
                symbols: vec!["BTCUSDT".to_string()],
                start: member.start,
                end: member.end,
                history_start: member.start,
                lob_sample_secs: 30,
                pm_book_sample_secs: Some(30),
                observation_sample_secs: 30,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
                immutable_input: true,
                source_kind: POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND.to_string(),
                optimizer_data_dir: Some("/tmp/authenticated-test".to_string()),
                source_surfaces: vec![],
                input_artifacts: vec![ResearchSnapshotInputArtifact {
                    name: "polymarket_evidence_0000".to_string(),
                    path: verified_polymarket_evidence_path(member),
                    content_hash: Some(format!("sha256:{}", member.content_sha256)),
                    row_count: Some(1),
                }],
                data_requirements: vec![],
                data_audit_status: Some("ok".to_string()),
                data_audit_report: None,
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts::default(),
                phase_timings: vec![],
                quality_flags: vec![],
                pm_book_source: ResearchSnapshotPmBookSource::default(),
            },
            observations: vec![observation],
            deribit_snapshots: vec![],
            pm_book_snapshots: vec![
                book(&member.up_token_id, "UP"),
                book(&member.down_token_id, "DOWN"),
            ],
        }
    }

    fn verified_outcomes(
        settlement_up: bool,
    ) -> HashMap<OfficialOutcomeKey, OfficialBinaryOutcome> {
        HashMap::from([(
            ("event-1".into(), "up".into(), "down".into()),
            OfficialBinaryOutcome {
                settlement_up,
                observed_at: "2026-07-17T05:35:02Z".parse().unwrap(),
            },
        )])
    }

    #[test]
    fn verified_observation_binding_is_exact_and_availability_safe() {
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let start = contract.event_start;
        let end = contract.event_end;
        for (settlement_up, expected) in [(true, 1.0), (false, 0.0)] {
            let row = verified_observation("2026-07-17T05:31:00Z");
            assert!(row.settlement_up.is_nan());
            let rows = bind_and_filter_verified_observations(
                vec![row],
                std::slice::from_ref(&contract),
                &verified_outcomes(settlement_up),
                "BTCUSDT",
                start,
                end,
            )
            .unwrap();
            assert_eq!(rows[0].settlement_up, expected);
            assert_eq!(rows[0].event_end_ts, Some(contract.event_end));
            assert_eq!(
                rows[0].official_resolution_observed_at.unwrap(),
                "2026-07-17T05:35:02Z".parse::<DateTime<Utc>>().unwrap()
            );
        }

        let outcomes = verified_outcomes(true);
        for (index, mut row) in [
            verified_observation("2026-07-17T05:31:00Z"),
            verified_observation("2026-07-17T05:31:00Z"),
        ]
        .into_iter()
        .enumerate()
        {
            if index == 0 {
                row.symbol = "SOLUSDT".into();
            } else {
                row.up_token_id = "wrong-up".into();
            }
            assert!(bind_and_filter_verified_observations(
                vec![row],
                std::slice::from_ref(&contract),
                &outcomes,
                "BTCUSDT",
                start,
                end,
            )
            .unwrap_err()
            .to_string()
            .contains("factor event/token mismatch"));
        }
        assert!(bind_and_filter_verified_observations(
            vec![verified_observation("2026-07-17T05:31:00Z")],
            std::slice::from_ref(&contract),
            &HashMap::new(),
            "BTCUSDT",
            start,
            end,
        )
        .unwrap_err()
        .to_string()
        .contains("factor settlement mismatch"));

        let rows = bind_and_filter_verified_observations(
            vec![
                verified_observation("2026-07-17T05:30:59Z"),
                verified_observation("2026-07-17T05:31:00Z"),
            ],
            std::slice::from_ref(&contract),
            &outcomes,
            "BTCUSDT",
            start,
            end,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tick_ts, contract.available_at);

        let later_contract = verified_contract("2026-07-17T05:31:01Z");
        assert!(bind_and_filter_verified_observations(
            vec![verified_observation("2026-07-17T05:31:00Z")],
            std::slice::from_ref(&later_contract),
            &outcomes,
            "BTCUSDT",
            start,
            end,
        )
        .unwrap_err()
        .to_string()
        .contains("no available factor observation"));
    }

    #[test]
    fn polymarket_chainlink_baseline_binding_clears_all_binance_freshness() {
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let row = verified_observation("2026-07-17T05:31:00Z");
        let rows = bind_and_filter_polymarket_chainlink_baseline_observations(
            vec![row],
            std::slice::from_ref(&contract),
            &verified_outcomes(true),
            "BTCUSDT",
            contract.event_start,
            contract.event_end,
        )
        .expect("available baseline observation");

        assert!(rows[0].chainlink_reference_fresh);
        assert!(!rows[0].binance_spot_fresh);
        assert!(!rows[0].binance_lob_fresh);
        assert!(!rows[0].binance_agg_trade_fresh);
        assert!(
            [
                rows[0].signed_distance_to_beat,
                rows[0].abs_distance_to_beat,
                rows[0].drift_10s,
                rows[0].drift_30s,
                rows[0].sigma_horizon,
                rows[0].distance_over_sigma,
                rows[0].model_prob_up,
                rows[0].chainlink_prob_up,
                rows[0].model_edge_up,
                rows[0].obi,
                rows[0].spread_bps,
                rows[0].bid_depth_near,
                rows[0].ask_depth_near,
                rows[0].cum_obi_delta_5m,
                rows[0].cum_trade_imbalance_5m,
                rows[0].cex_bar_return_30s,
            ]
            .iter()
            .all(|value| value.is_nan()),
            "baseline rows must not expose finite Binance-derived placeholders"
        );

        let mut stale_chainlink = verified_observation("2026-07-17T05:31:00Z");
        stale_chainlink.chainlink_reference_fresh = false;
        assert!(bind_and_filter_polymarket_chainlink_baseline_observations(
            vec![stale_chainlink],
            std::slice::from_ref(&contract),
            &verified_outcomes(true),
            "BTCUSDT",
            contract.event_start,
            contract.event_end,
        )
        .expect_err("baseline still requires fresh Chainlink evidence")
        .to_string()
        .contains("no available factor observation"));
    }

    #[test]
    fn polymarket_chainlink_baseline_ticks_require_both_token_books() {
        let contract = verified_contract("2026-07-17T05:29:59Z");
        let first = "2026-07-17T05:30:02Z".parse().unwrap();
        let second = "2026-07-17T05:30:03Z".parse().unwrap();
        let book = |token_id: &str, side: &str, ts| ResearchPmBookSnapshot {
            event_id: contract.event_id.clone(),
            token_id: token_id.to_string(),
            side: side.to_string(),
            ts,
            bids: vec![ResearchPmBookLevel {
                price: 0.4,
                size: 10.0,
            }],
            asks: vec![ResearchPmBookLevel {
                price: 0.6,
                size: 10.0,
            }],
        };
        let books = vec![
            book(&contract.up_token_id, "UP", first),
            book(&contract.down_token_id, "DOWN", second),
        ];

        let ticks = polymarket_chainlink_baseline_ticks(&books, std::slice::from_ref(&contract))
            .expect("both token books produce a baseline clock");
        assert_eq!(ticks.len(), 1);
        match &ticks[0] {
            MarketUpdate::SpotPrice { symbol, price, ts } => {
                assert_eq!(symbol.as_ref(), "BTCUSDT");
                assert_eq!(price, &contract.price_to_beat);
                assert_eq!(ts, &second);
            }
            other => panic!("expected derived baseline clock, got {other:?}"),
        }

        assert!(
            polymarket_chainlink_baseline_ticks(&books[..1], std::slice::from_ref(&contract),)
                .expect_err("one-sided token evidence must fail closed")
                .to_string()
                .contains("both Up and Down token books")
        );
    }

    #[test]
    fn verified_observation_event_end_binding_rejects_mismatch() {
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let mut row = verified_observation("2026-07-17T05:31:00Z");
        row.event_end_ts = Some(contract.event_end + chrono::Duration::microseconds(1));

        assert!(bind_and_filter_verified_observations(
            vec![row],
            std::slice::from_ref(&contract),
            &verified_outcomes(true),
            "BTCUSDT",
            contract.event_start,
            contract.event_end,
        )
        .unwrap_err()
        .to_string()
        .contains("factor event end mismatch"));
    }

    #[test]
    fn verified_observation_freshness_is_fail_closed() {
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let outcomes = verified_outcomes(true);
        let fresh = verified_observation("2026-07-17T05:31:00Z");
        let mut stale_rows = [fresh.clone(), fresh.clone(), fresh.clone(), fresh.clone()];
        stale_rows[0].chainlink_reference_fresh = false;
        stale_rows[1].binance_spot_fresh = false;
        stale_rows[2].binance_lob_fresh = false;
        stale_rows[3].binance_agg_trade_fresh = false;
        let bind = |rows| {
            bind_and_filter_verified_observations(
                rows,
                std::slice::from_ref(&contract),
                &outcomes,
                "BTCUSDT",
                contract.event_start,
                contract.event_end,
            )
        };

        for stale in stale_rows {
            assert_eq!(bind(vec![fresh.clone(), stale.clone()]).unwrap().len(), 1);
            assert!(bind(vec![stale])
                .unwrap_err()
                .to_string()
                .contains("no available factor observation"));
        }
    }

    #[test]
    fn verified_contract_clocks_and_token_binding_are_availability_safe() {
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let start = contract.event_start;
        let end = contract.event_end;
        assert!(!observation_is_available(
            "2026-07-17T05:30:59Z".parse().unwrap(),
            &contract,
            start,
            end
        ));
        assert!(observation_is_available(
            contract.available_at,
            &contract,
            start,
            end
        ));
        let settlement = |winning_token_id: &str, resolved_up_won: bool, available_at: &str| {
            ResearchPolymarketSettlement {
                event_id: contract.event_id.clone(),
                winning_token_id: winning_token_id.into(),
                resolved_up_won,
                available_at: available_at.parse().unwrap(),
            }
        };
        assert!(append_verified_contracts(
            &mut Vec::new(),
            std::slice::from_ref(&contract),
            &[settlement("up", true, "2026-07-17T05:34:59Z")]
        )
        .unwrap_err()
        .to_string()
        .contains("early settlement clock"));
        assert!(append_verified_contracts(
            &mut Vec::new(),
            std::slice::from_ref(&contract),
            &[settlement("down", true, "2026-07-17T05:35:02Z")]
        )
        .unwrap_err()
        .to_string()
        .contains("settlement event/token mismatch"));
        let mut updates = Vec::new();
        let outcomes = append_verified_contracts(
            &mut updates,
            std::slice::from_ref(&contract),
            &[settlement("up", true, "2026-07-17T05:35:02Z")],
        )
        .unwrap();
        assert!(matches!(
            updates.as_slice(),
            [MarketUpdate::EventDiscovered {
                resolved_up_won: None,
                ..
            }]
        ));
        assert!(exact_official_binary_outcome(&outcomes, "event-1", "wrong-up", "down").is_none());
        assert_eq!(
            exact_official_binary_outcome(&outcomes, "event-1", "up", "down")
                .unwrap()
                .observed_at,
            "2026-07-17T05:35:02Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert!(
            !exact_official_binary_outcome(
                &append_verified_contracts(
                    &mut Vec::new(),
                    std::slice::from_ref(&contract),
                    &[settlement("down", false, "2026-07-17T05:35:02Z")]
                )
                .unwrap(),
                "event-1",
                "up",
                "down"
            )
            .unwrap()
            .settlement_up
        );
    }

    #[test]
    fn maps_market_symbols_to_chainlink_reference_symbols() {
        assert_eq!(
            chainlink_reference_symbols(&[
                "BTCUSDT".to_string(),
                "SOL/USD".to_string(),
                "eth-usdc".to_string(),
            ]),
            ["btc/usd", "sol/usd", "eth/usd"]
        );
    }

    #[test]
    fn official_outcome_lookup_requires_the_exact_event_token_pair() {
        let observed_at = "2026-07-15T12:00:00.123456Z"
            .parse::<DateTime<Utc>>()
            .unwrap();
        let expected = OfficialBinaryOutcome {
            settlement_up: true,
            observed_at,
        };
        let mut outcomes = HashMap::new();
        outcomes.insert(
            (
                "event-1".to_string(),
                "stale-up".to_string(),
                "stale-down".to_string(),
            ),
            OfficialBinaryOutcome {
                settlement_up: false,
                observed_at: observed_at - chrono::Duration::hours(1),
            },
        );
        assert!(exact_official_binary_outcome(&outcomes, "event-1", "up", "down").is_none());

        outcomes.insert(
            ("event-1".to_string(), "up".to_string(), "down".to_string()),
            expected,
        );
        assert_eq!(
            exact_official_binary_outcome(&outcomes, "event-1", "up", "down"),
            Some(&expected)
        );
    }

    #[cfg(feature = "db")]
    #[tokio::test]
    #[ignore = "requires a migrated PostgreSQL database via PLOY_TEST_DATABASE_URL"]
    async fn postgres_official_outcomes_use_token_primary_keys_and_version_clocks() {
        let database_url = std::env::var("PLOY_TEST_DATABASE_URL").expect(
            "PLOY_TEST_DATABASE_URL is required for the ignored PostgreSQL integration test",
        );
        let pool = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect to PostgreSQL fixture");
        let mut transaction = pool.begin().await.expect("begin PostgreSQL fixture");
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let event_a = format!("1888667-{suffix}");
        let event_b = format!("1888668-{suffix}");
        let up_a = format!("up-a-{suffix}");
        let down_a = format!("down-a-{suffix}");
        let up_b = format!("up-b-{suffix}");
        let down_b = format!("down-b-{suffix}");
        let conflicting_down = format!("down-conflict-{suffix}");
        let slug_a = format!("btc-updown-5m-{suffix}");
        let slug_b = format!("sol-updown-5m-{suffix}");
        let condition_b = format!("condition-b-{suffix}");
        let conflicting_condition = format!("condition-conflict-{suffix}");
        let market_b = format!("market-b-{suffix}");
        let resolved_at = "2026-07-15T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let fetched_a_up = resolved_at + chrono::Duration::microseconds(10);
        let fetched_a_down = resolved_at + chrono::Duration::microseconds(20);
        let fetched_b_up = resolved_at + chrono::Duration::microseconds(30);
        let fetched_b_down = resolved_at + chrono::Duration::microseconds(40);

        let fixtures = [
            (&up_a, None, None, &slug_a, 1_i32, fetched_a_up),
            (&down_a, None, None, &slug_a, 0_i32, fetched_a_down),
            (
                &up_b,
                Some(condition_b.as_str()),
                Some(market_b.as_str()),
                &slug_b,
                0_i32,
                fetched_b_up,
            ),
            (
                &down_b,
                Some(condition_b.as_str()),
                Some(market_b.as_str()),
                &slug_b,
                1_i32,
                fetched_b_down,
            ),
            (
                &conflicting_down,
                Some(conflicting_condition.as_str()),
                Some(market_b.as_str()),
                &slug_b,
                1_i32,
                fetched_b_down,
            ),
        ];
        for (token_id, condition_id, market_id, market_slug, price, fetched_at) in fixtures {
            sqlx::query(
                r#"
                INSERT INTO pm_token_settlements (
                    token_id, condition_id, market_id, market_slug, outcome,
                    settled_price, resolved, resolved_at, fetched_at
                ) VALUES ($1, $2, $3, $4, 'fixture', $5::numeric, TRUE, $6, $7)
                "#,
            )
            .bind(token_id)
            .bind(condition_id)
            .bind(market_id)
            .bind(market_slug)
            .bind(price)
            .bind(resolved_at)
            .bind(fetched_at)
            .execute(&mut *transaction)
            .await
            .expect("insert official outcome fixture");
        }

        let contracts = vec![
            (event_a.clone(), up_a.clone(), down_a.clone()),
            (event_b.clone(), up_b.clone(), down_b.clone()),
            (format!("cross-{suffix}"), up_a.clone(), down_b.clone()),
            (
                format!("identity-conflict-{suffix}"),
                up_b.clone(),
                conflicting_down.clone(),
            ),
        ];
        let outcomes = load_official_binary_outcomes_for_contracts(&mut transaction, &contracts)
            .await
            .expect("load official outcome fixtures");
        assert_eq!(
            outcomes.get(&(event_a.clone(), up_a.clone(), down_a.clone())),
            Some(&OfficialBinaryOutcome {
                settlement_up: true,
                observed_at: fetched_a_down,
            })
        );
        assert_eq!(
            outcomes.get(&(event_b.clone(), up_b.clone(), down_b.clone())),
            Some(&OfficialBinaryOutcome {
                settlement_up: false,
                observed_at: fetched_b_down,
            })
        );
        assert!(!outcomes.contains_key(&(format!("cross-{suffix}"), up_a.clone(), down_b.clone())));
        assert!(!outcomes.contains_key(&(
            format!("identity-conflict-{suffix}"),
            up_b.clone(),
            conflicting_down.clone(),
        )));

        let correction_observed_at = resolved_at + chrono::Duration::microseconds(100);
        sqlx::query(
            r#"
            UPDATE pm_token_settlements
            SET settled_price = CASE WHEN token_id = $1 THEN 0 ELSE 1 END,
                fetched_at = $3
            WHERE token_id = ANY($2::text[])
            "#,
        )
        .bind(&up_a)
        .bind(vec![up_a.clone(), down_a.clone()])
        .bind(correction_observed_at)
        .execute(&mut *transaction)
        .await
        .expect("apply official outcome correction fixture");
        let corrected = load_official_binary_outcomes_for_contracts(
            &mut transaction,
            &[(event_a.clone(), up_a.clone(), down_a.clone())],
        )
        .await
        .expect("reload corrected official outcome");
        assert_eq!(
            corrected.get(&(event_a.clone(), up_a.clone(), down_a.clone())),
            Some(&OfficialBinaryOutcome {
                settlement_up: false,
                observed_at: correction_observed_at,
            })
        );

        sqlx::query("UPDATE pm_token_settlements SET settled_price = 0 WHERE token_id = $1")
            .bind(&down_b)
            .execute(&mut *transaction)
            .await
            .expect("make the second pair non-complementary");
        let non_complementary = load_official_binary_outcomes_for_contracts(
            &mut transaction,
            &[(event_b.clone(), up_b.clone(), down_b.clone())],
        )
        .await
        .expect("query non-complementary fixture");
        assert!(non_complementary.is_empty());

        transaction
            .rollback()
            .await
            .expect("roll back official outcome fixtures");
    }

    #[test]
    fn write_and_load_baseline_snapshot_roundtrips_manifest_and_unavailable_features() {
        let root = std::env::temp_dir().join(format!(
            "ploy-research-snapshot-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let contract = verified_contract("2026-07-17T05:31:00Z");
        let start = contract.event_start;
        let end = contract.event_end;
        let subset_start = start + Duration::seconds(60);
        let subset_end = start + Duration::seconds(120);
        let observations = bind_and_filter_polymarket_chainlink_baseline_observations(
            vec![verified_observation("2026-07-17T05:31:00Z")],
            std::slice::from_ref(&contract),
            &verified_outcomes(true),
            "BTCUSDT",
            start,
            end,
        )
        .expect("build baseline observation fixture");
        let snapshot = ResearchSnapshot {
            manifest: ResearchSnapshotManifest {
                schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
                snapshot_hash: None,
                snapshot_contract_hash: None,
                generated_at: Utc::now(),
                git_sha: Some("test-sha".to_string()),
                symbols: vec!["BTCUSDT".to_string()],
                start,
                end,
                history_start: start,
                lob_sample_secs: 30,
                pm_book_sample_secs: Some(30),
                observation_sample_secs: 30,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
                immutable_input: true,
                source_kind: POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND.to_string(),
                optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
                source_surfaces: vec![ResearchSnapshotSourceSurface {
                    name: "binance_price_ticks".to_string(),
                    role: "intentionally_omitted".to_string(),
                    gate_category: "optional_context".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: false,
                    sample_secs: None,
                    row_count: Some(0),
                    notes: "Intentionally omitted by the explicit baseline profile.".to_string(),
                }],
                input_artifacts: vec![ResearchSnapshotInputArtifact {
                    name: "unit_input".to_string(),
                    path: "/tmp/unit-input.parquet".to_string(),
                    content_hash: Some("abc123".to_string()),
                    row_count: Some(0),
                }],
                data_requirements: vec![POLYMARKET_CHAINLINK_BASELINE_REQUIREMENT.to_string()],
                data_audit_status: Some("ok".to_string()),
                data_audit_report: Some(
                    "verified+polymarket-chainlink-baseline-audit://sha256/test".to_string(),
                ),
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts::default(),
                phase_timings: vec![ResearchSnapshotPhaseTiming {
                    phase: "unit".to_string(),
                    elapsed_ms: 1,
                    rows: Some(0),
                }],
                quality_flags: vec![BINANCE_SURFACES_OMITTED_QUALITY_FLAG.to_string()],
                pm_book_source: ResearchSnapshotPmBookSource::default(),
            },
            observations,
            deribit_snapshots: vec![],
            pm_book_snapshots: vec![],
        };

        let written = write_research_snapshot(&root, snapshot).expect("write snapshot");
        let loaded = load_research_snapshot(&root).expect("load snapshot");
        assert_eq!(written.schema_version, RESEARCH_SNAPSHOT_SCHEMA_VERSION);
        assert!(written.snapshot_hash.is_some());
        let contract_hex = written
            .snapshot_contract_hash
            .as_deref()
            .and_then(|hash| hash.strip_prefix("sha256:"))
            .expect("SHA-256 contract hash");
        assert_eq!(contract_hex.len(), 64);
        assert!(contract_hex.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(loaded.manifest.git_sha.as_deref(), Some("test-sha"));
        assert_eq!(
            loaded.manifest.source_kind,
            POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND
        );
        assert!(loaded
            .manifest
            .quality_flags
            .iter()
            .any(|flag| flag == BINANCE_SURFACES_OMITTED_QUALITY_FLAG));
        assert_eq!(loaded.manifest.source_surfaces.len(), 1);
        assert!(!loaded.manifest.source_surfaces[0].snapshot_sampled);
        assert_eq!(
            loaded.manifest.source_surfaces[0].role,
            "intentionally_omitted"
        );
        assert_eq!(
            loaded.manifest.source_surfaces[0].gate_category,
            "optional_context"
        );
        assert_eq!(loaded.manifest.input_artifacts.len(), 1);
        validate_snapshot_request(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: loaded.manifest.start,
                end: loaded.manifest.end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                pm_book_sample_secs: loaded.manifest.pm_book_sample_secs.unwrap_or(30),
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        )
        .expect("snapshot request validation");
        validate_snapshot_request_coverage(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: subset_start,
                end: subset_end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                pm_book_sample_secs: loaded.manifest.pm_book_sample_secs.unwrap_or(30),
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        )
        .expect("snapshot coverage validation");
        let exact_subset_result = validate_snapshot_request(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: subset_start,
                end: subset_end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                pm_book_sample_secs: loaded.manifest.pm_book_sample_secs.unwrap_or(30),
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        );
        assert!(exact_subset_result.is_err());
        assert_eq!(loaded.manifest.row_counts.observations, 1);
        let quality = std::fs::read_to_string(root.join("quality.md")).expect("read quality");
        assert!(quality.contains("Snapshot contract hash: `sha256:"));
        assert!(quality.contains("## Source Surfaces"));
        assert!(quality.contains("gate_category=`optional_context`"));
        assert!(quality.contains("snapshot_sampled=`false`"));
        assert!(quality.contains("## Input Artifacts"));

        let mut artifact_reads = HashMap::<String, usize>::new();
        let captured = load_snapshot_artifact_bytes_with(&loaded.manifest, true, |artifact| {
            *artifact_reads.entry(artifact.to_string()).or_default() += 1;
            fs::read(root.join(artifact)).with_context(|| format!("read test artifact {artifact}"))
        })
        .expect("capture each evaluator artifact once");
        assert!(artifact_reads.values().all(|count| *count == 1));
        assert_eq!(
            Some(compute_snapshot_hash(&loaded.manifest, &captured).unwrap()),
            loaded.manifest.snapshot_hash
        );
        assert_eq!(
            Some(compute_snapshot_contract_hash(&loaded.manifest, &captured).unwrap()),
            loaded.manifest.snapshot_contract_hash
        );
        let captured_observations: Vec<FactorObservation> =
            serde_json::from_slice(&captured.observations_json).unwrap();
        assert_eq!(captured_observations.len(), 1);
        assert!(captured_observations[0].model_prob_up.is_nan());
        assert!(captured_observations[0].distance_over_sigma.is_nan());
        assert!(captured_observations[0].obi.is_nan());

        #[cfg(feature = "polars-export")]
        {
            let legacy_hash = compute_snapshot_hash(&loaded.manifest, &captured).unwrap();
            let strong_hash = compute_snapshot_contract_hash(&loaded.manifest, &captured).unwrap();
            let mut changed_parquet = captured.clone();
            changed_parquet
                .observations_parquet
                .as_mut()
                .expect("writer declares parquet")
                .push(0);
            assert_eq!(
                compute_snapshot_hash(&loaded.manifest, &changed_parquet).unwrap(),
                legacy_hash
            );
            assert_ne!(
                compute_snapshot_contract_hash(&loaded.manifest, &changed_parquet).unwrap(),
                strong_hash
            );
            changed_parquet.observations_parquet = None;
            assert!(compute_snapshot_contract_hash(&loaded.manifest, &changed_parquet).is_err());
        }

        let mut legacy_manifest = serde_json::to_value(&loaded.manifest).expect("legacy manifest");
        legacy_manifest
            .as_object_mut()
            .expect("manifest object")
            .remove("snapshot_contract_hash");
        write_json(root.join("manifest.json"), &legacy_manifest).expect("write legacy manifest");

        #[cfg(feature = "polars-export")]
        {
            let parquet_path = root.join(
                loaded
                    .manifest
                    .artifacts
                    .observations_parquet
                    .as_deref()
                    .expect("writer declares parquet"),
            );
            let parquet_bytes = fs::read(&parquet_path).expect("read parquet before deletion");
            fs::remove_file(&parquet_path).expect("remove optional legacy parquet");
            load_research_snapshot(&root)
                .expect("legacy snapshot must not depend on an unverified parquet artifact");

            write_json(root.join("manifest.json"), &loaded.manifest)
                .expect("restore strong snapshot manifest");
            let missing_parquet = load_research_snapshot(&root)
                .expect_err("strong snapshot must require its declared parquet artifact");
            assert!(missing_parquet
                .to_string()
                .contains("read snapshot artifact"));
            fs::write(&parquet_path, parquet_bytes).expect("restore parquet artifact");
        }

        #[cfg(not(feature = "polars-export"))]
        load_research_snapshot(&root).expect("legacy snapshot without contract hash must load");

        let mut semantic_tamper = loaded.manifest.clone();
        semantic_tamper.max_quote_age_secs += 1;
        write_json(root.join("manifest.json"), &semantic_tamper)
            .expect("tamper snapshot manifest semantics");
        let tampered_manifest =
            load_research_snapshot(&root).expect_err("semantic tamper must fail");
        assert!(tampered_manifest
            .to_string()
            .contains("evaluator contract hash mismatch"));

        write_json(root.join("manifest.json"), &loaded.manifest)
            .expect("restore snapshot manifest");
        std::fs::write(
            root.join(&loaded.manifest.artifacts.observations_json),
            "[]\n",
        )
        .expect("tamper snapshot observations");
        let tampered = load_research_snapshot(&root).expect_err("tampered snapshot must fail");
        assert!(tampered
            .to_string()
            .contains("evaluator contract hash mismatch"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_request_rejects_pm_book_cadence_mismatch_or_coarse_execution_gate() {
        let start = "2026-04-24T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-04-25T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let manifest = ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: Some("hash".to_string()),
            snapshot_contract_hash: None,
            generated_at: Utc::now(),
            git_sha: Some("test-sha".to_string()),
            symbols: vec!["BTCUSDT".to_string()],
            start,
            end,
            history_start: start,
            lob_sample_secs: 30,
            pm_book_sample_secs: Some(120),
            observation_sample_secs: 30,
            max_quote_age_secs: 120,
            stake_usd: 15.0,
            require_official_settlement: true,
            immutable_input: true,
            source_kind: "unit_test".to_string(),
            optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
            source_surfaces: vec![],
            input_artifacts: vec![],
            data_requirements: vec![],
            data_audit_status: Some("ok".to_string()),
            data_audit_report: None,
            include_deribit: false,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts::default(),
            phase_timings: vec![],
            quality_flags: vec![],
            pm_book_source: ResearchSnapshotPmBookSource::default(),
        };
        let symbols = vec!["BTCUSDT".to_string()];

        let mismatch = validate_snapshot_request_coverage(
            &manifest,
            ResearchSnapshotRequest {
                symbols: &symbols,
                start,
                end,
                lob_sample_secs: 30,
                pm_book_sample_secs: 30,
                observation_sample_secs: 30,
                max_quote_age_secs: 120,
                stake_usd: 15.0,
                require_official_settlement: true,
            },
        )
        .expect_err("PM book cadence mismatch should fail closed");
        assert!(mismatch
            .to_string()
            .contains("snapshot pm_book_sample_secs 120 does not match requested 30"));

        let coarse = validate_snapshot_request_coverage(
            &manifest,
            ResearchSnapshotRequest {
                symbols: &symbols,
                start,
                end,
                lob_sample_secs: 30,
                pm_book_sample_secs: 120,
                observation_sample_secs: 30,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
            },
        )
        .expect_err("PM book cadence coarser than quote-age gate should fail closed");
        assert!(coarse
            .to_string()
            .contains("full-depth execution claims require PM book cadence"));
    }

    #[test]
    fn quality_flags_sparse_pm_book_sampling_against_quote_age() {
        let pm_book_source = ResearchSnapshotPmBookSource::default();
        let flags = research_snapshot_quality_flags(ResearchSnapshotQualityInputs {
            observation_count: 100,
            chainlink_reference_tick_count: 10,
            binance_price_tick_count: 10,
            binance_agg_trade_tick_count: 10,
            binance_lob_snapshot_count: 10,
            deribit_snapshot_count: 0,
            pm_book_snapshot_count: 10,
            include_deribit: false,
            pm_book_sample_secs: 300,
            max_quote_age_secs: 30,
            pm_book_source: &pm_book_source,
        });

        assert!(flags.contains(&"pm_book_sample_secs_gt_max_quote_age:300>30".to_string()));
        assert!(!flags.contains(&"no_factor_observations".to_string()));
        assert!(!flags.contains(&"no_pm_book_snapshots".to_string()));
    }

    #[test]
    fn quality_flags_accepts_pm_book_sampling_within_quote_age() {
        let pm_book_source = ResearchSnapshotPmBookSource::default();
        let flags = research_snapshot_quality_flags(ResearchSnapshotQualityInputs {
            observation_count: 100,
            chainlink_reference_tick_count: 10,
            binance_price_tick_count: 10,
            binance_agg_trade_tick_count: 10,
            binance_lob_snapshot_count: 10,
            deribit_snapshot_count: 0,
            pm_book_snapshot_count: 10,
            include_deribit: false,
            pm_book_sample_secs: 30,
            max_quote_age_secs: 30,
            pm_book_source: &pm_book_source,
        });

        assert!(flags.is_empty());
    }

    #[test]
    fn quality_flags_archive_manifest_without_sampled_rows() {
        let pm_book_source = ResearchSnapshotPmBookSource {
            archive_manifest_rows: 1000,
            archive_status: "archive_loaded".to_string(),
            ..Default::default()
        };
        let flags = research_snapshot_quality_flags(ResearchSnapshotQualityInputs {
            observation_count: 100,
            chainlink_reference_tick_count: 10,
            binance_price_tick_count: 10,
            binance_agg_trade_tick_count: 10,
            binance_lob_snapshot_count: 10,
            deribit_snapshot_count: 0,
            pm_book_snapshot_count: 0,
            include_deribit: false,
            pm_book_sample_secs: 30,
            max_quote_age_secs: 30,
            pm_book_source: &pm_book_source,
        });

        assert!(flags.contains(&"no_pm_book_snapshots".to_string()));
        assert!(flags.contains(&"pm_book_archive_manifest_rows_but_no_sampled_rows".to_string()));
    }

    #[test]
    fn quality_flags_identify_empty_required_binance_surfaces() {
        let pm_book_source = ResearchSnapshotPmBookSource::default();
        let flags = research_snapshot_quality_flags(ResearchSnapshotQualityInputs {
            observation_count: 100,
            chainlink_reference_tick_count: 10,
            binance_price_tick_count: 0,
            binance_agg_trade_tick_count: 0,
            binance_lob_snapshot_count: 0,
            deribit_snapshot_count: 0,
            pm_book_snapshot_count: 10,
            include_deribit: false,
            pm_book_sample_secs: 30,
            max_quote_age_secs: 30,
            pm_book_source: &pm_book_source,
        });

        assert!(flags.contains(&"no_binance_price_ticks".to_string()));
        assert!(flags.contains(&"no_binance_agg_trade_ticks".to_string()));
        assert!(flags.contains(&"no_binance_lob_snapshots".to_string()));
    }

    #[test]
    fn quality_flags_identify_missing_chainlink_settlement_reference() {
        let pm_book_source = ResearchSnapshotPmBookSource::default();
        let flags = research_snapshot_quality_flags(ResearchSnapshotQualityInputs {
            observation_count: 100,
            chainlink_reference_tick_count: 0,
            binance_price_tick_count: 10,
            binance_agg_trade_tick_count: 10,
            binance_lob_snapshot_count: 10,
            deribit_snapshot_count: 0,
            pm_book_snapshot_count: 10,
            include_deribit: false,
            pm_book_sample_secs: 30,
            max_quote_age_secs: 30,
            pm_book_source: &pm_book_source,
        });

        assert!(flags.contains(&"no_chainlink_reference_ticks".to_string()));
    }

    #[cfg(feature = "db")]
    #[test]
    fn archive_hour_dirs_only_includes_overlapping_local_hours() {
        let start = "2026-05-18T16:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-05-18T18:00:00Z".parse::<DateTime<Utc>>().unwrap();

        let hours = archive_hour_dirs(start, end);

        assert_eq!(
            hours,
            vec![("2026-05-19".to_string(), 0), ("2026-05-19".to_string(), 1)]
        );
    }

    #[cfg(feature = "db")]
    #[test]
    fn parse_duckdb_timestamptz_accepts_compact_hour_offset() {
        let parsed = parse_duckdb_timestamptz("2026-05-19 06:55:13.870644+08")
            .expect("compact offset timestamp");

        assert_eq!(parsed.to_rfc3339(), "2026-05-18T22:55:13.870644+00:00");
    }

    #[cfg(feature = "db")]
    #[test]
    fn archive_token_window_values_sql_quotes_contract_fields() {
        let windows = vec![PmBookTokenWindow {
            market_slug: "btc-up's-test".to_string(),
            token_id: "123".to_string(),
            side: "UP".to_string(),
            window_start: "2026-05-18T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
            window_end: "2026-05-18T00:05:00Z".parse::<DateTime<Utc>>().unwrap(),
        }];

        let sql = archive_token_window_values_sql(&windows);

        assert!(sql.contains("'btc-up''s-test'"));
        assert!(sql.contains("TIMESTAMPTZ '2026-05-18T00:00:00+00:00'"));
        assert!(sql.contains("TIMESTAMPTZ '2026-05-18T00:05:00+00:00'"));
    }

    #[test]
    fn authenticated_snapshot_cache_reuses_a_verified_readback_without_a_hash_cycle() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());
        let mut builds = 0;

        let admitted = materialize_authenticated_research_snapshot(&cohort, &request, || {
            builds += 1;
            Ok(authenticated_test_snapshot())
        })
        .expect("first materialization admits the verified snapshot");
        let first_contract = admitted.snapshot_contract_id().to_string();

        let reused = materialize_authenticated_research_snapshot(&cohort, &request, || {
            panic!("cache hit must not rebuild, collect, or upload")
        })
        .expect("verified cache readback is admitted");

        assert_eq!(builds, 1);
        assert_eq!(reused.snapshot_contract_id(), first_contract);
        assert_eq!(reused.cohort_manifest_id(), cohort.manifest_id());
        assert!(!cohort.manifest_id().contains(&first_contract));
    }

    #[test]
    fn empty_ready_catalog_is_a_typed_insufficient_cohort_rejection() {
        let catalog = ploy_market_data::polymarket_evidence::PolymarketReadyEventCatalog::default();
        let partition = EventCohortPartition::from_ready_catalog(&catalog, 1_000)
            .expect("empty catalog still has a partition artifact");

        assert!(matches!(
            authenticate_ready_event_cohort(&catalog, &partition),
            Err(AuthenticatedResearchSnapshotRejection::InsufficientCohort { .. })
        ));
    }

    #[test]
    fn corrupt_cached_snapshot_is_rejected_without_rebuilding() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());
        let admitted = materialize_authenticated_research_snapshot(&cohort, &request, || {
            Ok(authenticated_test_snapshot())
        })
        .expect("materialize cache fixture");
        let observations = admitted.snapshot_dir().join("observations.json");
        std::fs::remove_file(&observations).unwrap();

        assert!(matches!(
            materialize_authenticated_research_snapshot(&cohort, &request, || {
                panic!("corrupt cache must reject before rebuilding")
            }),
            Err(AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot { .. })
        ));
    }

    #[test]
    fn stale_catalog_evidence_is_rejected_before_snapshot_write() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());

        assert!(matches!(
            materialize_authenticated_research_snapshot(&cohort, &request, || {
                let mut snapshot = authenticated_test_snapshot();
                snapshot.manifest.input_artifacts[0].content_hash =
                    Some(format!("sha256:{}", "0".repeat(64)));
                Ok(snapshot)
            }),
            Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected { .. })
        ));
    }

    #[test]
    fn missing_label_coverage_is_rejected_before_snapshot_write() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());

        assert!(matches!(
            materialize_authenticated_research_snapshot(&cohort, &request, || {
                let mut snapshot = authenticated_test_snapshot();
                snapshot.observations[0].official_resolution_observed_at = None;
                Ok(snapshot)
            }),
            Err(AuthenticatedResearchSnapshotRejection::SnapshotRejected { .. })
        ));
    }

    #[test]
    fn authenticated_snapshot_accepts_case_insensitive_book_sides() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());

        materialize_authenticated_research_snapshot(&cohort, &request, || {
            let mut snapshot = authenticated_test_snapshot();
            snapshot.pm_book_snapshots[0].side = "up".to_string();
            snapshot.pm_book_snapshots[1].side = "DoWn".to_string();
            Ok(snapshot)
        })
        .expect("case-insensitive book sides preserve authenticated token binding");
    }

    #[test]
    fn unsealed_cache_is_rejected_without_rebuilding() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());
        let admitted = materialize_authenticated_research_snapshot(&cohort, &request, || {
            Ok(authenticated_test_snapshot())
        })
        .expect("materialize cache fixture");
        std::fs::remove_file(admitted.snapshot_dir().join(SEALED_SNAPSHOT_CACHE_MARKER)).unwrap();

        assert!(matches!(
            materialize_authenticated_research_snapshot(&cohort, &request, || {
                panic!("unsealed cache must reject before rebuilding")
            }),
            Err(AuthenticatedResearchSnapshotRejection::CorruptCachedSnapshot { .. })
        ));
    }

    #[test]
    fn compiler_identity_requires_a_sha256_digest() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = AuthenticatedSnapshotMaterializationRequest {
            cache_root: cache.path().to_path_buf(),
            compiler_source_identity: format!("sha256:{}", "3".repeat(64)),
            compiler_image_identity: format!("sha256:{}", "4".repeat(64)),
            build_input_identity: "builder-input-label".to_string(),
        };

        assert!(matches!(
            materialize_authenticated_research_snapshot(&cohort, &request, || {
                panic!("malformed compiler identity must reject before building")
            }),
            Err(AuthenticatedResearchSnapshotRejection::MaterializationFailed { .. })
        ));
    }

    #[test]
    fn existing_final_cache_wins_an_atomic_publication_race() {
        let cache = tempfile::tempdir().expect("cache root");
        let cohort = authenticated_test_cohort();
        let request = authenticated_test_request(cache.path().to_path_buf());
        let mut existing_contract = None;

        let admitted = materialize_authenticated_research_snapshot(&cohort, &request, || {
            let existing = materialize_authenticated_research_snapshot(&cohort, &request, || {
                let mut snapshot = authenticated_test_snapshot();
                snapshot.manifest.git_sha = Some("competing-writer".to_string());
                Ok(snapshot)
            })
            .expect("competing writer publishes a sealed cache entry");
            existing_contract = Some(existing.snapshot_contract_id().to_string());
            let mut staged = authenticated_test_snapshot();
            staged.manifest.git_sha = Some("staged-writer".to_string());
            Ok(staged)
        })
        .expect("existing sealed cache wins publication race");

        assert_eq!(
            admitted.snapshot_contract_id(),
            existing_contract.as_deref().expect("competing contract")
        );
        assert!(std::fs::read_dir(cache.path())
            .expect("read cache root")
            .all(|entry| !entry
                .expect("cache entry")
                .file_name()
                .to_string_lossy()
                .contains(".staging-")));
    }
}
