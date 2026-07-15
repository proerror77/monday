use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, File};
#[cfg(feature = "db")]
use std::io::BufReader;
#[cfg(feature = "polars-export")]
use std::io::Cursor;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(feature = "db")]
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(feature = "db")]
use std::time::Instant;

use anyhow::{Context, Result};
#[cfg(feature = "db")]
use chrono::Timelike;
use chrono::{DateTime, Utc};
use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(any(feature = "db", test))]
use crate::factors::normalized_underlying_symbol;
use crate::{DeribitFeatureSnapshot, FactorObservation, ResearchPmBookSnapshot};

pub const RESEARCH_SNAPSHOT_SCHEMA_VERSION: &str = "research_snapshot_v1";

#[cfg(any(feature = "db", test))]
const OFFICIAL_OUTCOME_AVAILABILITY_SQL: &str = r#"
    WITH governed_tokens AS (
        SELECT
            m.market_slug,
            trim(both '"' from token.value::text) AS token_id,
            CASE token.ordinality WHEN 1 THEN 'UP' ELSE 'DOWN' END AS side
        FROM pm_market_metadata m
        CROSS JOIN LATERAL jsonb_array_elements(
            (m.raw_market->'markets'->0->>'clobTokenIds')::jsonb
        ) WITH ORDINALITY AS token(value, ordinality)
        WHERE m.market_slug = ANY($1)
          AND token.ordinality <= 2
    ), current_resolution AS (
        SELECT
            g.market_slug,
            g.token_id,
            g.side,
            s.settled_price,
            s.resolved_at,
            s.fetched_at
        FROM pm_token_settlements s
        JOIN governed_tokens g
          ON g.market_slug = s.market_slug
         AND g.token_id = s.token_id
        WHERE s.resolved = TRUE
    )
    SELECT
        market_slug,
        CASE
            WHEN MAX(settled_price) FILTER (WHERE side = 'UP') = 1
             AND MAX(settled_price) FILTER (WHERE side = 'DOWN') = 0
                THEN TRUE
            WHEN MAX(settled_price) FILTER (WHERE side = 'UP') = 0
             AND MAX(settled_price) FILTER (WHERE side = 'DOWN') = 1
                THEN FALSE
            ELSE NULL
        END AS official_outcome_up,
        MAX(GREATEST(COALESCE(resolved_at, fetched_at), fetched_at))
            AS official_outcome_available_at
    FROM current_resolution
    GROUP BY market_slug
    HAVING COUNT(DISTINCT token_id) = 2
       AND COUNT(DISTINCT side) = 2
       AND COUNT(*) FILTER (WHERE settled_price IN (0, 1)) = 2
       AND MIN(settled_price) = 0
       AND MAX(settled_price) = 1
"#;

#[cfg(any(feature = "db", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OfficialOutcomeAvailability {
    outcome_up: bool,
    available_at: DateTime<Utc>,
}

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
    /// Content-addressed coverage/mismatch audit for every governed 300-second
    /// Chainlink settlement event represented by this snapshot. `None` is
    /// valid only for snapshots that contain no governed five-minute events;
    /// prediction-loop preflight requires this evidence.
    #[serde(default)]
    pub chainlink_oracle_settlement_audit: Option<crate::factors::ChainlinkOracleSettlementAudit>,
    /// Governed event-level proof behind `chainlink_oracle_settlement_audit`.
    /// The evaluator contract hash binds this complete collection together
    /// with the observation artifact so prediction preflight can verify every
    /// five-minute label against its exact oracle boundaries and official
    /// payout instead of trusting aggregate counters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chainlink_oracle_settlement_evidence:
        Vec<crate::factors::ChainlinkOracleSettlementEvidence>,
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

#[derive(Debug)]
struct ResearchSnapshotArtifactBytes {
    observations_json: Vec<u8>,
    deribit_snapshots_json: Vec<u8>,
    pm_book_snapshots_json: Vec<u8>,
    observations_parquet: Option<Vec<u8>>,
}

impl ResearchSnapshotArtifactBytes {
    fn read(snapshot_root: &SnapshotRoot, artifacts: &ResearchSnapshotArtifacts) -> Result<Self> {
        Ok(Self {
            observations_json: snapshot_root.read_file(&artifacts.observations_json)?,
            deribit_snapshots_json: snapshot_root.read_file(&artifacts.deribit_snapshots_json)?,
            pm_book_snapshots_json: snapshot_root.read_file(&artifacts.pm_book_snapshots_json)?,
            observations_parquet: artifacts
                .observations_parquet
                .as_deref()
                .map(|artifact| snapshot_root.read_file(artifact))
                .transpose()?,
        })
    }
}

pub fn load_research_snapshot(snapshot_dir: impl AsRef<Path>) -> Result<ResearchSnapshot> {
    let snapshot_dir = snapshot_dir.as_ref();
    let snapshot_root = SnapshotRoot::open(snapshot_dir)?;
    let manifest_bytes = snapshot_root
        .read_file("manifest.json")
        .context("read research snapshot manifest")?;
    let manifest: ResearchSnapshotManifest =
        serde_json::from_slice(&manifest_bytes).context("parse research snapshot manifest")?;

    if manifest.schema_version != RESEARCH_SNAPSHOT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported research snapshot schema {}; expected {}",
            manifest.schema_version,
            RESEARCH_SNAPSHOT_SCHEMA_VERSION
        );
    }
    let artifact_bytes = ResearchSnapshotArtifactBytes::read(&snapshot_root, &manifest.artifacts)
        .context("read governed research snapshot artifacts")?;
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

    let observations: Vec<FactorObservation> = parse_snapshot_json(
        &artifact_bytes.observations_json,
        &manifest.artifacts.observations_json,
    )
    .context("read snapshot observations")?;
    let deribit_snapshots = parse_snapshot_json(
        &artifact_bytes.deribit_snapshots_json,
        &manifest.artifacts.deribit_snapshots_json,
    )
    .context("read snapshot Deribit rows")?;
    let pm_book_snapshots = parse_snapshot_json(
        &artifact_bytes.pm_book_snapshots_json,
        &manifest.artifacts.pm_book_snapshots_json,
    )
    .context("read snapshot PM book rows")?;

    if manifest.chainlink_oracle_settlement_audit.is_some()
        || !manifest.chainlink_oracle_settlement_evidence.is_empty()
        || observations.iter().any(|row| row.event_window_secs == 300)
    {
        validate_governed_chainlink_5m_settlement_evidence(&manifest, &observations)
            .context("validate governed Chainlink five-minute snapshot evidence")?;
    }

    Ok(ResearchSnapshot {
        manifest,
        observations,
        deribit_snapshots,
        pm_book_snapshots,
    })
}

/// Recompute the governed snapshot digests from the exact in-memory rows before
/// a caller uses them for evaluation or model training.
///
/// This closes the gap between a previously verified on-disk artifact and a
/// mutable [`ResearchSnapshot`] value. When Parquet export is enabled, the
/// verifier deterministically regenerates the derived Parquet bytes through
/// the same serializer used by the governed writer.
#[cfg(feature = "ml")]
pub(crate) fn verify_research_snapshot_integrity(snapshot: &ResearchSnapshot) -> Result<()> {
    #[cfg(feature = "polars-export")]
    let observations_parquet = snapshot
        .manifest
        .artifacts
        .observations_parquet
        .as_ref()
        .map(|_| serialize_observations_parquet(&snapshot.observations))
        .transpose()?;
    #[cfg(not(feature = "polars-export"))]
    let observations_parquet = {
        anyhow::ensure!(
            snapshot.manifest.artifacts.observations_parquet.is_none(),
            "reverifying an observations_parquet artifact requires the polars-export feature"
        );
        None
    };
    let artifact_bytes = ResearchSnapshotArtifactBytes {
        observations_json: serialize_json(&snapshot.observations, "snapshot observations")?,
        deribit_snapshots_json: serialize_json(
            &snapshot.deribit_snapshots,
            "snapshot Deribit rows",
        )?,
        pm_book_snapshots_json: serialize_json(
            &snapshot.pm_book_snapshots,
            "snapshot PM book rows",
        )?,
        observations_parquet,
    };
    let recorded_contract_hash = snapshot
        .manifest
        .snapshot_contract_hash
        .as_deref()
        .context("research snapshot manifest is missing snapshot_contract_hash")?;
    let computed_contract_hash =
        compute_snapshot_contract_hash(&snapshot.manifest, &artifact_bytes)
            .context("recompute in-memory research snapshot evaluator contract hash")?;
    anyhow::ensure!(
        recorded_contract_hash == computed_contract_hash,
        "research snapshot evaluator contract hash mismatch: manifest={} computed={}",
        recorded_contract_hash,
        computed_contract_hash
    );
    let recorded_hash = snapshot
        .manifest
        .snapshot_hash
        .as_deref()
        .context("research snapshot manifest is missing snapshot_hash")?;
    let computed_hash = compute_snapshot_hash(&snapshot.manifest, &artifact_bytes)
        .context("recompute in-memory research snapshot content hash")?;
    anyhow::ensure!(
        recorded_hash == computed_hash,
        "research snapshot content hash mismatch: manifest={} computed={}",
        recorded_hash,
        computed_hash
    );
    Ok(())
}

pub fn write_research_snapshot(
    snapshot_dir: impl AsRef<Path>,
    mut snapshot: ResearchSnapshot,
) -> Result<ResearchSnapshotManifest> {
    if snapshot.manifest.optimizer_data_dir.is_none() {
        anyhow::bail!("research snapshot manifest requires optimizer_data_dir");
    }
    if snapshot
        .observations
        .iter()
        .any(|row| row.event_window_secs == 300)
        || snapshot
            .manifest
            .chainlink_oracle_settlement_audit
            .is_some()
        || !snapshot
            .manifest
            .chainlink_oracle_settlement_evidence
            .is_empty()
    {
        validate_governed_chainlink_5m_settlement_evidence(
            &snapshot.manifest,
            &snapshot.observations,
        )
        .context("refuse to write snapshot with invalid governed Chainlink evidence")?;
    }

    let snapshot_dir = snapshot_dir.as_ref();
    #[cfg(feature = "polars-export")]
    {
        snapshot
            .manifest
            .artifacts
            .observations_parquet
            .get_or_insert_with(|| "observations.parquet".to_string());
    }
    validate_snapshot_output_paths(&snapshot.manifest.artifacts)
        .context("validate governed research snapshot output paths")?;
    #[cfg(not(feature = "polars-export"))]
    if snapshot.manifest.artifacts.observations_parquet.is_some() {
        anyhow::bail!("writing observations_parquet requires the polars-export feature");
    }

    snapshot.manifest.row_counts = ResearchSnapshotRowCounts {
        observations: snapshot.observations.len(),
        deribit_snapshots: snapshot.deribit_snapshots.len(),
        pm_book_snapshots: snapshot.pm_book_snapshots.len(),
    };

    let observations_json = serialize_json(&snapshot.observations, "snapshot observations")?;
    let deribit_snapshots_json =
        serialize_json(&snapshot.deribit_snapshots, "snapshot Deribit rows")?;
    let pm_book_snapshots_json =
        serialize_json(&snapshot.pm_book_snapshots, "snapshot PM book rows")?;

    #[cfg(feature = "polars-export")]
    let observations_parquet = Some(serialize_observations_parquet(&snapshot.observations)?);
    #[cfg(not(feature = "polars-export"))]
    let observations_parquet = None;

    let artifact_bytes = ResearchSnapshotArtifactBytes {
        observations_json,
        deribit_snapshots_json,
        pm_book_snapshots_json,
        observations_parquet,
    };

    fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("create snapshot dir {}", snapshot_dir.display()))?;
    let snapshot_root = SnapshotRoot::open(snapshot_dir)?;
    snapshot_root.write_atomic(
        &snapshot.manifest.artifacts.observations_json,
        &artifact_bytes.observations_json,
    )?;
    snapshot_root.write_atomic(
        &snapshot.manifest.artifacts.deribit_snapshots_json,
        &artifact_bytes.deribit_snapshots_json,
    )?;
    snapshot_root.write_atomic(
        &snapshot.manifest.artifacts.pm_book_snapshots_json,
        &artifact_bytes.pm_book_snapshots_json,
    )?;
    if let (Some(path), Some(bytes)) = (
        snapshot.manifest.artifacts.observations_parquet.as_deref(),
        artifact_bytes.observations_parquet.as_deref(),
    ) {
        snapshot_root.write_atomic(path, bytes)?;
    }

    snapshot.manifest.snapshot_hash =
        Some(compute_snapshot_hash(&snapshot.manifest, &artifact_bytes)?);
    snapshot.manifest.snapshot_contract_hash = Some(compute_snapshot_contract_hash(
        &snapshot.manifest,
        &artifact_bytes,
    )?);

    let query_timings_json =
        serialize_json(&snapshot.manifest.phase_timings, "snapshot query timings")?;
    let quality_markdown = render_quality_markdown(&snapshot.manifest).into_bytes();
    let manifest_json = serialize_json(&snapshot.manifest, "snapshot manifest")?;
    snapshot_root.write_atomic(
        &snapshot.manifest.artifacts.query_timings_json,
        &query_timings_json,
    )?;
    snapshot_root.write_atomic(
        &snapshot.manifest.artifacts.quality_markdown,
        &quality_markdown,
    )?;
    // The manifest is the commit record for the snapshot and must be published last.
    snapshot_root.write_atomic("manifest.json", &manifest_json)?;

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

/// Validate the event-level Chainlink settlement evidence required by the
/// governed five-minute prediction lane. This is deliberately separate from
/// quote-cadence validation: the oracle boundary policy is fixed by its own
/// version and tolerance and cannot inherit a caller's quote-freshness value.
pub fn validate_governed_chainlink_5m_settlement_audit(
    manifest: &ResearchSnapshotManifest,
) -> Result<()> {
    let audit = manifest
        .chainlink_oracle_settlement_audit
        .as_ref()
        .context("snapshot is missing governed Chainlink five-minute settlement audit")?;
    validate_chainlink_5m_settlement_audit(audit)?;
    validate_chainlink_5m_settlement_manifest_evidence(manifest, audit)
}

/// Validate the complete governed event evidence and bind every five-minute
/// observation label to the matching Chainlink boundaries and official payout.
/// This is the prediction compiler/preflight boundary; aggregate counts alone
/// are never sufficient here.
pub fn validate_governed_chainlink_5m_settlement_evidence(
    manifest: &ResearchSnapshotManifest,
    observations: &[FactorObservation],
) -> Result<()> {
    validate_governed_chainlink_5m_settlement_audit(manifest)?;

    let evidence_event_ids = manifest
        .chainlink_oracle_settlement_evidence
        .iter()
        .map(|evidence| evidence.event_id.as_str())
        .collect::<HashSet<_>>();
    let observation_event_ids = observations
        .iter()
        .filter(|row| row.event_window_secs == 300)
        .map(|row| row.event_id.as_str())
        .collect::<HashSet<_>>();
    validate_governed_chainlink_event_set(&evidence_event_ids, &observation_event_ids)?;

    let evidence_by_event = manifest
        .chainlink_oracle_settlement_evidence
        .iter()
        .map(|evidence| (evidence.event_id.as_str(), evidence))
        .collect::<HashMap<_, _>>();
    for observation in observations
        .iter()
        .filter(|row| row.event_window_secs == 300)
    {
        let evidence = evidence_by_event
            .get(observation.event_id.as_str())
            .with_context(|| {
                format!(
                    "governed five-minute observation event {} is missing exact Chainlink settlement evidence",
                    observation.event_id
                )
            })?;
        if crate::factors::normalized_underlying_symbol(&observation.symbol)
            != crate::factors::normalized_underlying_symbol(&evidence.symbol)
        {
            anyhow::bail!(
                "governed five-minute observation event {} symbol {} does not match oracle evidence symbol {}",
                observation.event_id,
                observation.symbol,
                evidence.symbol
            );
        }
        if observation.tick_ts < evidence.start_time || observation.tick_ts > evidence.end_time {
            anyhow::bail!(
                "governed five-minute observation event {} tick {} is outside oracle evidence window {}..{}",
                observation.event_id,
                observation.tick_ts,
                evidence.start_time,
                evidence.end_time
            );
        }
        let observed_outcome = if observation.settlement_up == 1.0 {
            true
        } else if observation.settlement_up == 0.0 {
            false
        } else {
            anyhow::bail!(
                "governed five-minute observation event {} has non-binary settlement_up {}",
                observation.event_id,
                observation.settlement_up
            );
        };
        let official_outcome = evidence
            .official_outcome_up
            .context("complete Chainlink settlement evidence is missing official outcome")?;
        if observed_outcome != official_outcome {
            anyhow::bail!(
                "governed five-minute observation event {} settlement_up {} does not match official outcome {}",
                observation.event_id,
                observation.settlement_up,
                u8::from(official_outcome)
            );
        }
    }
    Ok(())
}

fn validate_governed_chainlink_event_set(
    evidence_event_ids: &HashSet<&str>,
    observation_event_ids: &HashSet<&str>,
) -> Result<()> {
    let mut evidence_without_observations = evidence_event_ids
        .difference(observation_event_ids)
        .copied()
        .collect::<Vec<_>>();
    let mut observations_without_evidence = observation_event_ids
        .difference(evidence_event_ids)
        .copied()
        .collect::<Vec<_>>();
    evidence_without_observations.sort_unstable();
    observations_without_evidence.sort_unstable();

    if !evidence_without_observations.is_empty() || !observations_without_evidence.is_empty() {
        anyhow::bail!(
            "governed Chainlink evidence and five-minute observation event sets do not match: evidence_without_observations={evidence_without_observations:?} observations_without_evidence={observations_without_evidence:?}"
        );
    }
    Ok(())
}

fn validate_chainlink_5m_settlement_manifest_evidence(
    manifest: &ResearchSnapshotManifest,
    audit: &crate::factors::ChainlinkOracleSettlementAudit,
) -> Result<()> {
    let evidence = &manifest.chainlink_oracle_settlement_evidence;
    if evidence.is_empty() {
        anyhow::bail!("snapshot is missing governed Chainlink five-minute settlement evidence");
    }
    let recomputed = crate::factors::ChainlinkOracleSettlementAudit::from_evidence(evidence);
    if &recomputed != audit {
        anyhow::bail!(
            "governed Chainlink five-minute settlement audit does not match event evidence: recorded={audit:?} recomputed={recomputed:?}"
        );
    }

    let snapshot_symbols = manifest
        .symbols
        .iter()
        .map(|symbol| crate::factors::normalized_underlying_symbol(symbol))
        .collect::<HashSet<_>>();
    let mut event_ids = HashSet::with_capacity(evidence.len());
    for event in evidence {
        if event.event_id.trim().is_empty() || !event_ids.insert(event.event_id.as_str()) {
            anyhow::bail!(
                "governed Chainlink five-minute settlement evidence has empty or duplicate event_id {:?}",
                event.event_id
            );
        }
        if event.policy_version != crate::factors::GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION {
            anyhow::bail!(
                "Chainlink settlement evidence event {} policy {} does not match governed {}",
                event.event_id,
                event.policy_version,
                crate::factors::GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION
            );
        }
        if event.end_time - event.start_time != chrono::Duration::seconds(300) {
            anyhow::bail!(
                "Chainlink settlement evidence event {} window {}..{} is not exactly 300 seconds",
                event.event_id,
                event.start_time,
                event.end_time
            );
        }
        if !snapshot_symbols.contains(&crate::factors::normalized_underlying_symbol(&event.symbol))
        {
            anyhow::bail!(
                "Chainlink settlement evidence event {} symbol {} is outside snapshot symbols {:?}",
                event.event_id,
                event.symbol,
                manifest.symbols
            );
        }
        if !event.reasons.is_empty() {
            anyhow::bail!(
                "Chainlink settlement evidence event {} contains failure reasons {:?}",
                event.event_id,
                event.reasons
            );
        }
        let open = event.open.as_ref().with_context(|| {
            format!(
                "Chainlink settlement event {} is missing open",
                event.event_id
            )
        })?;
        let close = event.close.as_ref().with_context(|| {
            format!(
                "Chainlink settlement event {} is missing close",
                event.event_id
            )
        })?;
        validate_chainlink_boundary_evidence(&event.event_id, "open", event.start_time, open)?;
        validate_chainlink_boundary_evidence(&event.event_id, "close", event.end_time, close)?;

        let computed_outcome = close.price >= open.price;
        if event.chainlink_outcome_up != Some(computed_outcome) {
            anyhow::bail!(
                "Chainlink settlement evidence event {} outcome {:?} does not match boundary prices open={} close={}",
                event.event_id,
                event.chainlink_outcome_up,
                open.price,
                close.price
            );
        }
        if event.official_outcome_up != Some(computed_outcome) {
            anyhow::bail!(
                "Chainlink settlement evidence event {} official outcome {:?} does not corroborate Chainlink outcome {}",
                event.event_id,
                event.official_outcome_up,
                computed_outcome
            );
        }
    }
    Ok(())
}

fn validate_chainlink_boundary_evidence(
    event_id: &str,
    boundary_name: &str,
    expected_boundary: DateTime<Utc>,
    boundary: &crate::factors::ChainlinkOracleBoundaryEvidence,
) -> Result<()> {
    let max_age =
        chrono::Duration::seconds(crate::factors::GOVERNED_CHAINLINK_BOUNDARY_MAX_AGE_SECS);
    if boundary.boundary_ts != expected_boundary {
        anyhow::bail!(
            "Chainlink settlement event {event_id} {boundary_name} boundary_ts {} does not match expected {}",
            boundary.boundary_ts,
            expected_boundary
        );
    }
    if !boundary.price.is_finite() || boundary.price <= 0.0 {
        anyhow::bail!(
            "Chainlink settlement event {event_id} {boundary_name} boundary price {} is invalid",
            boundary.price
        );
    }
    if boundary.source_ts > expected_boundary
        || expected_boundary - boundary.source_ts > max_age
        || boundary.received_at < boundary.source_ts
        || boundary.received_at > expected_boundary + max_age
    {
        anyhow::bail!(
            "Chainlink settlement event {event_id} {boundary_name} selected tick violates the governed source/arrival boundary tolerance"
        );
    }
    if boundary.confirmation_source_ts < expected_boundary
        || boundary.confirmation_source_ts - expected_boundary > max_age
        || boundary.confirmation_received_at < boundary.confirmation_source_ts
        || boundary.confirmation_received_at > expected_boundary + max_age
    {
        anyhow::bail!(
            "Chainlink settlement event {event_id} {boundary_name} confirmation tick violates the governed source/arrival boundary tolerance"
        );
    }
    Ok(())
}

fn validate_chainlink_5m_settlement_audit(
    audit: &crate::factors::ChainlinkOracleSettlementAudit,
) -> Result<()> {
    if audit.policy_version != crate::factors::GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION {
        anyhow::bail!(
            "Chainlink settlement policy {} does not match governed {}",
            audit.policy_version,
            crate::factors::GOVERNED_CHAINLINK_BOUNDARY_POLICY_VERSION
        );
    }
    if audit.max_boundary_age_secs != crate::factors::GOVERNED_CHAINLINK_BOUNDARY_MAX_AGE_SECS {
        anyhow::bail!(
            "Chainlink settlement boundary tolerance {} does not match governed {} seconds",
            audit.max_boundary_age_secs,
            crate::factors::GOVERNED_CHAINLINK_BOUNDARY_MAX_AGE_SECS
        );
    }
    if audit.expected_events == 0 {
        anyhow::bail!("governed Chainlink five-minute settlement audit has no expected events");
    }
    if audit.has_failures() {
        let failures = audit
            .failures
            .iter()
            .map(|failure| format!("{}:{:?}", failure.event_id, failure.reasons))
            .collect::<Vec<_>>()
            .join(",");
        anyhow::bail!(
            "governed Chainlink five-minute settlement audit failed: expected={} accepted={} missing_open={} missing_close={} missing_official={} payout_mismatch={} reasons=[{}]",
            audit.expected_events,
            audit.accepted_events,
            audit.missing_open_events,
            audit.missing_close_events,
            audit.missing_official_events,
            audit.payout_mismatch_events,
            failures
        );
    }
    Ok(())
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
    if manifest.chainlink_oracle_settlement_audit.is_some() {
        validate_governed_chainlink_5m_settlement_audit(manifest)
            .context("snapshot contains invalid governed Chainlink settlement evidence")?;
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

fn research_snapshot_quality_flags(
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
    pm_book_source: &ResearchSnapshotPmBookSource,
) -> Vec<String> {
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
async fn load_official_outcome_availability(
    pool: &sqlx::PgPool,
    event_ids: &[String],
) -> Result<HashMap<String, OfficialOutcomeAvailability>, sqlx::Error> {
    if event_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows =
        sqlx::query_as::<_, (String, bool, DateTime<Utc>)>(OFFICIAL_OUTCOME_AVAILABILITY_SQL)
            .bind(event_ids)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|(event_id, outcome_up, available_at)| {
            (
                event_id,
                OfficialOutcomeAvailability {
                    outcome_up,
                    available_at,
                },
            )
        })
        .collect())
}

#[cfg(any(feature = "db", test))]
fn bind_official_outcome_availability(
    evidence: &mut [crate::ChainlinkOracleSettlementEvidence],
    observations: &HashMap<String, OfficialOutcomeAvailability>,
) -> Result<()> {
    for event in evidence {
        let observed = observations.get(&event.event_id).with_context(|| {
            format!(
                "official resolution value and availability are missing for event {}",
                event.event_id
            )
        })?;
        let expected_outcome = event.official_outcome_up.with_context(|| {
            format!(
                "governed settlement evidence is missing the official outcome for event {}",
                event.event_id
            )
        })?;
        anyhow::ensure!(
            expected_outcome == observed.outcome_up,
            "official outcome changed while compiling snapshot for event {}: initial={} current={}",
            event.event_id,
            expected_outcome,
            observed.outcome_up
        );
        event.official_outcome_available_at = Some(observed.available_at);
    }
    Ok(())
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

#[cfg(feature = "db")]
pub async fn build_research_snapshot_from_database(
    pool: &sqlx::PgPool,
    options: ResearchSnapshotBuildOptions,
) -> Result<ResearchSnapshot> {
    use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
    use ploy_market_contracts::MarketUpdate;

    use crate::{
        build_factor_observations_with_lob_sampled_and_oracle_evidence,
        load_deribit_feature_snapshots_with_timings, load_research_lob_snapshots_sampled,
        load_research_pm_book_snapshots_sampled,
    };

    let mut phase_timings = Vec::new();
    let history_start = options.start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let historical_sample_secs = u32::try_from(options.lob_sample_secs.max(1)).unwrap_or(1);

    let started = Instant::now();
    let all_updates = load_from_database_with_options(
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
            ..Default::default()
        },
    )
    .await
    .context("load historical market updates")?;
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
    let factor_build = build_factor_observations_with_lob_sampled_and_oracle_evidence(
        updates_slice,
        lob_slice,
        options.max_quote_age_secs,
        options.observation_sample_secs,
    );
    let mut chainlink_oracle_settlement_evidence = factor_build.oracle_settlement_evidence;
    let settlement_event_ids = chainlink_oracle_settlement_evidence
        .iter()
        .map(|evidence| evidence.event_id.clone())
        .collect::<Vec<_>>();
    let settlement_started = Instant::now();
    let official_outcome_availability =
        load_official_outcome_availability(pool, &settlement_event_ids)
            .await
            .context("load official outcome availability clocks")?;
    bind_official_outcome_availability(
        &mut chainlink_oracle_settlement_evidence,
        &official_outcome_availability,
    )
    .context("bind official outcome values to availability clocks")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "official_outcome_availability".to_string(),
        elapsed_ms: settlement_started.elapsed().as_millis(),
        rows: Some(official_outcome_availability.len()),
    });
    let chainlink_oracle_settlement_audit = (factor_build.oracle_settlement_audit.expected_events
        > 0)
    .then_some(factor_build.oracle_settlement_audit);
    if let Some(audit) = chainlink_oracle_settlement_audit.as_ref() {
        validate_chainlink_5m_settlement_audit(audit).context(
            "refuse to compile research snapshot with incomplete or mismatched governed five-minute settlement evidence",
        )?;
    }
    let observations = factor_build.observations;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "factor_observations".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(observations.len()),
    });

    let quality_flags = research_snapshot_quality_flags(
        observations.len(),
        chainlink_reference_tick_rows,
        binance_price_tick_rows,
        binance_agg_trade_tick_rows,
        all_lob_snapshots.len(),
        deribit_snapshots.len(),
        all_pm_book_snapshots.len(),
        options.include_deribit,
        pm_book_sample_secs,
        options.max_quote_age_secs,
        &pm_book_source,
    );
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
            chainlink_oracle_settlement_audit,
            chainlink_oracle_settlement_evidence,
            immutable_input: true,
            source_kind: "tango_postgres_compiled_snapshot".to_string(),
            optimizer_data_dir: options.optimizer_data_dir,
            source_surfaces: vec![
                ResearchSnapshotSourceSurface {
                    name: "chainlink_reference_ticks".to_string(),
                    role: "opening_reference_and_settlement_authority".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(i64::from(historical_sample_secs)),
                    row_count: Some(chainlink_reference_tick_rows),
                    notes: "Arrival-timestamped Chainlink reference prices define the governed opening reference and settlement-source probability; Binance never replaces this authority.".to_string(),
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
                    notes: "Binance aggTrade flow sampled to one source tick per 5-second bucket and replayed at received_at.".to_string(),
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
                    gate_category: "required_for_execution".to_string(),
                    raw_full_fidelity: true,
                    snapshot_sampled: false,
                    sample_secs: None,
                    row_count: Some(official_outcome_availability.len()),
                    notes: "Official settlement labels are required when require_official_settlement=true; availability is the latest current-value fetched/resolved clock across both outcome tokens.".to_string(),
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
                        + deribit_snapshots.len(),
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

#[cfg(feature = "db")]
fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("parse {}", path.display()))
}

static SNAPSHOT_TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An opened, canonical research-snapshot root. All governed I/O is resolved
/// relative to this descriptor so later path replacement cannot redirect it.
#[derive(Debug)]
struct SnapshotRoot {
    fd: OwnedFd,
    canonical_path: PathBuf,
}

impl SnapshotRoot {
    fn open(snapshot_dir: &Path) -> Result<Self> {
        let canonical_path = fs::canonicalize(snapshot_dir).with_context(|| {
            format!(
                "canonicalize research snapshot root {}",
                snapshot_dir.display()
            )
        })?;
        let fd = rustix::fs::open(
            &canonical_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "open canonical research snapshot root without following symlinks {}",
                canonical_path.display()
            )
        })?;
        Ok(Self { fd, canonical_path })
    }

    fn read_file(&self, artifact: &str) -> Result<Vec<u8>> {
        let (parent, file_name) = self.open_parent(artifact, false)?;
        let fd = rustix::fs::openat(
            &parent,
            &file_name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "open research snapshot artifact with no-follow traversal {:?} under {}",
                artifact,
                self.canonical_path.display()
            )
        })?;
        let mut file = File::from(fd);
        if !file
            .metadata()
            .with_context(|| format!("inspect open snapshot artifact {artifact:?}"))?
            .is_file()
        {
            anyhow::bail!("research snapshot artifact must be a regular file: {artifact:?}");
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .with_context(|| format!("single-read research snapshot artifact {artifact:?}"))?;
        Ok(bytes)
    }

    fn write_atomic(&self, artifact: &str, bytes: &[u8]) -> Result<()> {
        let (parent, file_name) = self.open_parent(artifact, true)?;
        let (temporary_name, temporary_fd) = (0..128)
            .find_map(|_| {
                let sequence = SNAPSHOT_TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let candidate =
                    format!(".research-snapshot.tmp.{}.{}", std::process::id(), sequence);
                match rustix::fs::openat(
                    &parent,
                    candidate.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                ) {
                    Ok(fd) => Some(Ok((candidate, fd))),
                    Err(error) if error == rustix::io::Errno::EXIST => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()
            .with_context(|| {
                format!(
                    "create atomic snapshot temporary file for {artifact:?} under {}",
                    self.canonical_path.display()
                )
            })?
            .context("exhausted unique snapshot temporary file names")?;

        let mut renamed = false;
        let result = (|| -> Result<()> {
            let mut temporary_file = File::from(temporary_fd);
            temporary_file
                .write_all(bytes)
                .with_context(|| format!("write atomic snapshot temporary for {artifact:?}"))?;
            temporary_file
                .sync_all()
                .with_context(|| format!("sync atomic snapshot temporary for {artifact:?}"))?;
            drop(temporary_file);

            rustix::fs::renameat(&parent, temporary_name.as_str(), &parent, &file_name)
                .with_context(|| format!("atomically publish snapshot artifact {artifact:?}"))?;
            renamed = true;
            rustix::fs::fsync(&parent)
                .with_context(|| format!("sync snapshot artifact directory for {artifact:?}"))?;
            Ok(())
        })();

        if result.is_err() && !renamed {
            let _ = rustix::fs::unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
        }
        result
    }

    fn open_parent(&self, artifact: &str, create_directories: bool) -> Result<(OwnedFd, OsString)> {
        self.open_parent_with_sync(artifact, create_directories, |parent, _component_name| {
            rustix::fs::fsync(parent).context("fsync snapshot directory parent descriptor")
        })
    }

    fn open_parent_with_sync<F>(
        &self,
        artifact: &str,
        create_directories: bool,
        mut sync_created_parent: F,
    ) -> Result<(OwnedFd, OsString)>
    where
        F: FnMut(&OwnedFd, &std::ffi::OsStr) -> Result<()>,
    {
        let mut components = validate_relative_snapshot_path(artifact)?;
        let file_name = components
            .pop()
            .context("validated snapshot artifact path must have a file name")?;
        let mut parent = rustix::io::dup(&self.fd).context("duplicate snapshot root descriptor")?;
        for component in components {
            let component_name = component.as_os_str();
            let directory = match rustix::fs::openat(
                &parent,
                component_name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(directory) => directory,
                Err(error) if create_directories && error == rustix::io::Errno::NOENT => {
                    match rustix::fs::mkdirat(&parent, component_name, Mode::RWXU) {
                        Ok(()) => {}
                        Err(error) if error == rustix::io::Errno::EXIST => {}
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!(
                                    "create snapshot artifact directory {:?} with dir-FD anchoring",
                                    component_name
                                )
                            })
                        }
                    }
                    sync_created_parent(&parent, component_name).with_context(|| {
                        format!(
                            "sync parent directory after creating nested snapshot directory {:?}",
                            component_name
                        )
                    })?;
                    rustix::fs::openat(
                        &parent,
                        component_name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(),
                    )
                    .with_context(|| {
                        format!(
                            "open newly-created snapshot directory {:?} without following symlinks",
                            component_name
                        )
                    })?
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "open snapshot directory {:?} with no-follow dir-FD traversal",
                            component_name
                        )
                    })
                }
            };
            parent = directory;
        }
        Ok((parent, file_name))
    }
}

fn validate_relative_snapshot_path(artifact: &str) -> Result<Vec<OsString>> {
    let relative = Path::new(artifact);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        anyhow::bail!("unsafe research snapshot artifact path {artifact:?}");
    }
    let mut components = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            anyhow::bail!("unsafe research snapshot artifact path {artifact:?}");
        };
        components.push(component.to_os_string());
    }
    if components.is_empty() {
        anyhow::bail!("unsafe research snapshot artifact path {artifact:?}");
    }
    Ok(components)
}

fn validate_snapshot_output_paths(artifacts: &ResearchSnapshotArtifacts) -> Result<()> {
    let mut paths = vec![
        ("observations_json", artifacts.observations_json.as_str()),
        (
            "deribit_snapshots_json",
            artifacts.deribit_snapshots_json.as_str(),
        ),
        (
            "pm_book_snapshots_json",
            artifacts.pm_book_snapshots_json.as_str(),
        ),
        ("quality_markdown", artifacts.quality_markdown.as_str()),
        ("query_timings_json", artifacts.query_timings_json.as_str()),
        ("manifest", "manifest.json"),
    ];
    if let Some(parquet) = artifacts.observations_parquet.as_deref() {
        paths.push(("observations_parquet", parquet));
    }

    let mut normalized_paths = HashSet::new();
    for (name, path) in paths {
        validate_relative_snapshot_path(path)
            .with_context(|| format!("validate snapshot output {name}"))?;
        let path = PathBuf::from(path);
        if !normalized_paths.insert(path.clone()) {
            anyhow::bail!(
                "duplicate governed research snapshot output path: {}",
                path.display()
            );
        }
        if normalized_paths
            .iter()
            .any(|other| other != &path && (other.starts_with(&path) || path.starts_with(other)))
        {
            anyhow::bail!(
                "governed research snapshot output paths must not contain one another: {}",
                path.display()
            );
        }
    }
    Ok(())
}

fn parse_snapshot_json<T: for<'de> Deserialize<'de>>(bytes: &[u8], artifact: &str) -> Result<T> {
    serde_json::from_slice(bytes)
        .with_context(|| format!("parse research snapshot artifact {artifact}"))
}

fn serialize_json<T: Serialize>(value: &T, label: &str) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).with_context(|| format!("serialize {label}"))
}

#[cfg(feature = "polars-export")]
fn serialize_observations_parquet(observations: &[FactorObservation]) -> Result<Vec<u8>> {
    use polars::io::parquet::write::ParquetWriter;

    let mut frame = crate::observations_to_frame(observations)
        .context("build snapshot observations parquet frame")?;
    let mut cursor = Cursor::new(Vec::new());
    ParquetWriter::new(&mut cursor)
        .finish(&mut frame)
        .context("serialize snapshot observations parquet")?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, value).with_context(|| format!("write {}", path.display()))
}

fn compute_snapshot_hash(
    manifest: &ResearchSnapshotManifest,
    artifact_bytes: &ResearchSnapshotArtifactBytes,
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
        serde_json::to_string(&manifest.chainlink_oracle_settlement_audit)
            .context("serialize Chainlink settlement audit for snapshot hash")?
            .as_bytes(),
    );
    if !manifest.chainlink_oracle_settlement_evidence.is_empty() {
        update(
            &mut hash,
            serde_json::to_string(&manifest.chainlink_oracle_settlement_evidence)
                .context("serialize Chainlink settlement evidence for snapshot hash")?
                .as_bytes(),
        );
    }
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
            &manifest.artifacts.observations_json,
            artifact_bytes.observations_json.as_slice(),
        ),
        (
            &manifest.artifacts.deribit_snapshots_json,
            artifact_bytes.deribit_snapshots_json.as_slice(),
        ),
        (
            &manifest.artifacts.pm_book_snapshots_json,
            artifact_bytes.pm_book_snapshots_json.as_slice(),
        ),
    ] {
        update(&mut hash, artifact.as_bytes());
        update(&mut hash, bytes);
    }
    Ok(format!("{hash:016x}"))
}

fn compute_snapshot_contract_hash(
    manifest: &ResearchSnapshotManifest,
    artifact_bytes: &ResearchSnapshotArtifactBytes,
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
            artifact_bytes.observations_json.as_slice(),
        ),
        (
            manifest.artifacts.deribit_snapshots_json.as_str(),
            artifact_bytes.deribit_snapshots_json.as_slice(),
        ),
        (
            manifest.artifacts.pm_book_snapshots_json.as_str(),
            artifact_bytes.pm_book_snapshots_json.as_slice(),
        ),
    ] {
        update_framed(&mut hasher, artifact.as_bytes());
        update_framed(&mut hasher, bytes);
    }
    if let (Some(artifact), Some(bytes)) = (
        manifest.artifacts.observations_parquet.as_deref(),
        artifact_bytes.observations_parquet.as_deref(),
    ) {
        update_framed(&mut hasher, artifact.as_bytes());
        update_framed(&mut hasher, bytes);
    }

    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn render_quality_markdown(manifest: &ResearchSnapshotManifest) -> String {
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
    body.push_str("\n## Governed Chainlink 5m Settlement Audit\n\n");
    if let Some(audit) = manifest.chainlink_oracle_settlement_audit.as_ref() {
        body.push_str(&format!(
            "- Policy: `{}`; max_boundary_age_secs=`{}`\n",
            audit.policy_version, audit.max_boundary_age_secs
        ));
        body.push_str(&format!(
            "- Coverage: expected={}, accepted={}, missing_open={}, missing_close={}, missing_official={}, payout_mismatch={}\n",
            audit.expected_events,
            audit.accepted_events,
            audit.missing_open_events,
            audit.missing_close_events,
            audit.missing_official_events,
            audit.payout_mismatch_events
        ));
        if audit.failures.is_empty() {
            body.push_str("- Failures: `<none>`\n");
        } else {
            for failure in &audit.failures {
                body.push_str(&format!(
                    "- Failure event=`{}` reasons=`{:?}`\n",
                    failure.event_id, failure.reasons
                ));
            }
        }
    } else {
        body.push_str("- `<not-recorded>`\n");
    }
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
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_resolution_availability_query_tracks_current_two_token_value() {
        let normalized = OFFICIAL_OUTCOME_AVAILABILITY_SQL
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(normalized.contains("WITH governed_tokens AS"));
        assert!(normalized.contains("JOIN governed_tokens"));
        assert!(normalized.contains("AS official_outcome_up"));
        assert!(normalized.contains("settled_price"));
        assert!(normalized.contains("MAX(GREATEST(COALESCE(resolved_at, fetched_at), fetched_at))"));
        assert!(normalized.contains("resolved = TRUE"));
        assert!(normalized.contains("GROUP BY market_slug"));
        assert!(normalized.contains("HAVING COUNT(DISTINCT token_id) = 2"));
        assert!(normalized.contains("COUNT(DISTINCT side) = 2"));
        assert!(normalized.contains("COUNT(*) FILTER (WHERE settled_price IN (0, 1)) = 2"));
    }

    #[test]
    fn official_resolution_clock_is_bound_to_the_same_outcome_value() {
        let end_time = Utc::now();
        let evidence = crate::ChainlinkOracleSettlementEvidence {
            event_id: "event-1".to_string(),
            symbol: "BTC".to_string(),
            policy_version: "test".to_string(),
            start_time: end_time - chrono::Duration::minutes(5),
            end_time,
            open: None,
            close: None,
            chainlink_outcome_up: Some(true),
            official_outcome_up: Some(true),
            official_outcome_available_at: None,
            reasons: vec![],
        };
        let available_at = end_time + chrono::Duration::seconds(1);
        let matching = HashMap::from([(
            "event-1".to_string(),
            OfficialOutcomeAvailability {
                outcome_up: true,
                available_at,
            },
        )]);
        let mut matching_evidence = vec![evidence.clone()];
        bind_official_outcome_availability(&mut matching_evidence, &matching)
            .expect("matching value and clock must bind atomically");
        assert_eq!(
            matching_evidence[0].official_outcome_available_at,
            Some(available_at)
        );

        let corrected = HashMap::from([(
            "event-1".to_string(),
            OfficialOutcomeAvailability {
                outcome_up: false,
                available_at,
            },
        )]);
        let error = bind_official_outcome_availability(&mut [evidence], &corrected)
            .expect_err("a correction between reads must fail closed");
        assert!(error
            .to_string()
            .contains("official outcome changed while compiling snapshot"));
    }

    #[test]
    fn governed_chainlink_event_sets_must_match_bidirectionally() {
        let evidence_event_ids = HashSet::from(["event-1", "event-2"]);
        let complete_observation_event_ids = HashSet::from(["event-1", "event-2"]);
        validate_governed_chainlink_event_set(&evidence_event_ids, &complete_observation_event_ids)
            .expect("exact governed event coverage");

        let missing_observation_event_ids = HashSet::from(["event-1"]);
        let missing_observation = validate_governed_chainlink_event_set(
            &evidence_event_ids,
            &missing_observation_event_ids,
        )
        .expect_err("evidence without an observation must fail closed");
        let missing_observation = format!("{missing_observation:#}");
        assert!(missing_observation.contains("evidence_without_observations=[\"event-2\"]"));
        assert!(missing_observation.contains("observations_without_evidence=[]"));

        let unknown_observation_event_ids = HashSet::from(["event-1", "event-2", "event-3"]);
        let unknown_observation = validate_governed_chainlink_event_set(
            &evidence_event_ids,
            &unknown_observation_event_ids,
        )
        .expect_err("observation without evidence must fail closed");
        let unknown_observation = format!("{unknown_observation:#}");
        assert!(unknown_observation.contains("evidence_without_observations=[]"));
        assert!(unknown_observation.contains("observations_without_evidence=[\"event-3\"]"));
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
    fn snapshot_artifact_bytes_are_read_once_and_paths_fail_closed() {
        let root = std::env::temp_dir().join(format!(
            "ploy-research-snapshot-bytes-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create snapshot test root");
        let artifacts = ResearchSnapshotArtifacts::default();
        for artifact in [
            &artifacts.observations_json,
            &artifacts.deribit_snapshots_json,
            &artifacts.pm_book_snapshots_json,
        ] {
            fs::write(root.join(artifact), "[]").expect("write snapshot artifact");
        }
        let snapshot_root = SnapshotRoot::open(&root).expect("open snapshot test root");
        let captured = ResearchSnapshotArtifactBytes::read(&snapshot_root, &artifacts)
            .expect("capture snapshot artifacts once");

        fs::write(root.join(&artifacts.observations_json), "not-json")
            .expect("replace artifact after capture");
        let observations: Vec<FactorObservation> =
            parse_snapshot_json(&captured.observations_json, &artifacts.observations_json)
                .expect("parse the exact captured bytes");
        assert!(observations.is_empty());

        let mut traversal = artifacts.clone();
        traversal.observations_json = "../outside.json".to_string();
        assert!(
            ResearchSnapshotArtifactBytes::read(&snapshot_root, &traversal)
                .expect_err("parent traversal must fail")
                .to_string()
                .contains("unsafe research snapshot artifact path")
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let outside = root.with_extension("outside.json");
            fs::write(&outside, "[]").expect("write outside artifact");
            symlink(&outside, root.join("linked.json")).expect("link outside artifact");
            let mut linked = artifacts.clone();
            linked.observations_json = "linked.json".to_string();
            assert!(ResearchSnapshotArtifactBytes::read(&snapshot_root, &linked)
                .expect_err("symlinked artifact must fail")
                .to_string()
                .contains("no-follow"));
            fs::remove_file(outside).expect("remove outside artifact");
        }

        fs::remove_dir_all(root).expect("remove snapshot test root");
    }

    #[test]
    fn snapshot_root_descriptor_survives_root_path_replacement() {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let root = std::env::temp_dir().join(format!("ploy-snapshot-root-race-{suffix}"));
        let moved = std::env::temp_dir().join(format!("ploy-snapshot-root-moved-{suffix}"));
        fs::create_dir_all(&root).expect("create original snapshot root");
        fs::write(root.join("artifact.json"), b"original").expect("write original artifact");
        let snapshot_root = SnapshotRoot::open(&root).expect("open original snapshot root FD");

        fs::rename(&root, &moved).expect("move original root after FD capture");
        fs::create_dir_all(&root).expect("create attacker replacement root");
        fs::write(root.join("artifact.json"), b"replacement").expect("write replacement artifact");

        assert_eq!(
            snapshot_root
                .read_file("artifact.json")
                .expect("read remains anchored to original root"),
            b"original"
        );
        snapshot_root
            .write_atomic("anchored.json", b"anchored")
            .expect("write remains anchored to original root");
        assert_eq!(
            fs::read(moved.join("anchored.json")).expect("read anchored write"),
            b"anchored"
        );
        assert!(!root.join("anchored.json").exists());

        fs::remove_dir_all(root).expect("remove replacement root");
        fs::remove_dir_all(moved).expect("remove moved root");
    }

    #[test]
    fn nested_snapshot_directories_sync_each_parent_and_fail_closed_on_sync_error() {
        let root = std::env::temp_dir().join(format!(
            "ploy-snapshot-directory-sync-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).expect("create snapshot root");
        let snapshot_root = SnapshotRoot::open(&root).expect("open snapshot root");

        let mut synced_components = Vec::new();
        let (parent, file_name) = snapshot_root
            .open_parent_with_sync(
                "level-one/level-two/artifact.json",
                true,
                |_parent, component_name| {
                    synced_components.push(component_name.to_os_string());
                    Ok(())
                },
            )
            .expect("create every nested directory with a parent sync barrier");
        drop(parent);
        assert_eq!(file_name, std::ffi::OsStr::new("artifact.json"));
        assert_eq!(
            synced_components,
            [OsString::from("level-one"), OsString::from("level-two")]
        );

        let sync_error = snapshot_root
            .open_parent_with_sync(
                "sync-failure/child/artifact.json",
                true,
                |_parent, component_name| {
                    if component_name == std::ffi::OsStr::new("child") {
                        anyhow::bail!("injected directory fsync failure");
                    }
                    Ok(())
                },
            )
            .expect_err("a parent directory sync failure must abort traversal");
        let sync_error = format!("{sync_error:#}");
        assert!(sync_error
            .contains("sync parent directory after creating nested snapshot directory \"child\""));
        assert!(sync_error.contains("injected directory fsync failure"));
        assert!(!root.join("sync-failure/child/artifact.json").exists());

        snapshot_root
            .write_atomic("durable/level/artifact.json", b"durable")
            .expect("publish through real parent fsync barriers");
        drop(snapshot_root);
        let reopened = SnapshotRoot::open(&root).expect("reopen snapshot root after publication");
        assert_eq!(
            reopened
                .read_file("durable/level/artifact.json")
                .expect("read nested artifact through a fresh root descriptor"),
            b"durable"
        );
        drop(reopened);

        fs::remove_dir_all(root).expect("remove snapshot root");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_snapshot_write_replaces_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let root = std::env::temp_dir().join(format!("ploy-snapshot-write-link-{suffix}"));
        let outside = std::env::temp_dir().join(format!("ploy-snapshot-outside-{suffix}.json"));
        fs::create_dir_all(&root).expect("create snapshot root");
        fs::write(&outside, b"outside-sentinel").expect("write outside sentinel");
        symlink(&outside, root.join("artifact.json")).expect("create hostile artifact symlink");

        let snapshot_root = SnapshotRoot::open(&root).expect("open snapshot root");
        snapshot_root
            .write_atomic("artifact.json", b"governed")
            .expect("atomic write replaces directory entry, not symlink target");

        assert_eq!(
            fs::read(&outside).expect("read outside sentinel"),
            b"outside-sentinel"
        );
        assert_eq!(
            fs::read(root.join("artifact.json")).expect("read governed artifact"),
            b"governed"
        );
        assert!(!fs::symlink_metadata(root.join("artifact.json"))
            .expect("inspect governed artifact")
            .file_type()
            .is_symlink());

        fs::remove_dir_all(root).expect("remove snapshot root");
        fs::remove_file(outside).expect("remove outside sentinel");
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_write_rejects_symlinked_parent_escape() {
        use std::os::unix::fs::symlink;

        let suffix = format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let root = std::env::temp_dir().join(format!("ploy-snapshot-parent-link-{suffix}"));
        let outside = std::env::temp_dir().join(format!("ploy-snapshot-parent-outside-{suffix}"));
        fs::create_dir_all(&root).expect("create snapshot root");
        fs::create_dir_all(&outside).expect("create outside directory");
        symlink(&outside, root.join("nested")).expect("create hostile parent symlink");

        let snapshot_root = SnapshotRoot::open(&root).expect("open snapshot root");
        let error = snapshot_root
            .write_atomic("nested/artifact.json", b"must-not-escape")
            .expect_err("symlinked parent must fail closed");
        assert!(error.to_string().contains("no-follow"));
        assert!(!outside.join("artifact.json").exists());

        fs::remove_dir_all(root).expect("remove snapshot root");
        fs::remove_dir_all(outside).expect("remove outside directory");
    }

    #[test]
    fn snapshot_output_paths_are_all_validated_before_writing() {
        let mut artifacts = ResearchSnapshotArtifacts::default();
        artifacts.observations_json = "../outside.json".to_string();
        let traversal =
            validate_snapshot_output_paths(&artifacts).expect_err("parent traversal must fail");
        assert!(format!("{traversal:#}").contains("unsafe research snapshot artifact path"));

        artifacts.observations_json = "/tmp/outside.json".to_string();
        let absolute =
            validate_snapshot_output_paths(&artifacts).expect_err("absolute path must fail");
        assert!(format!("{absolute:#}").contains("unsafe research snapshot artifact path"));

        artifacts.observations_json = "observations.json".to_string();
        artifacts.observations_parquet = Some("../outside.parquet".to_string());
        let optional = validate_snapshot_output_paths(&artifacts)
            .expect_err("optional parquet traversal must fail");
        assert!(format!("{optional:#}").contains("unsafe research snapshot artifact path"));

        artifacts.observations_parquet = None;
        artifacts.observations_json = artifacts.quality_markdown.clone();
        assert!(validate_snapshot_output_paths(&artifacts)
            .expect_err("duplicate governed outputs must fail")
            .to_string()
            .contains("duplicate governed research snapshot output path"));
    }

    #[test]
    fn write_and_load_empty_snapshot_roundtrips_manifest() {
        let root = std::env::temp_dir().join(format!(
            "ploy-research-snapshot-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let start = "2026-04-24T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let subset_start = "2026-04-25T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let subset_end = "2026-04-27T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
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
                chainlink_oracle_settlement_audit: None,
                chainlink_oracle_settlement_evidence: vec![],
                immutable_input: true,
                source_kind: "unit_test".to_string(),
                optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
                source_surfaces: vec![ResearchSnapshotSourceSurface {
                    name: "unit_surface".to_string(),
                    role: "test".to_string(),
                    gate_category: "required_for_prediction".to_string(),
                    raw_full_fidelity: false,
                    snapshot_sampled: true,
                    sample_secs: Some(30),
                    row_count: Some(0),
                    notes: "unit test sampled surface".to_string(),
                }],
                input_artifacts: vec![ResearchSnapshotInputArtifact {
                    name: "unit_input".to_string(),
                    path: "/tmp/unit-input.parquet".to_string(),
                    content_hash: Some("abc123".to_string()),
                    row_count: Some(0),
                }],
                data_requirements: vec!["polymarket_quotes".to_string()],
                data_audit_status: Some("ok".to_string()),
                data_audit_report: Some("data-gap-audit.json".to_string()),
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts::default(),
                phase_timings: vec![ResearchSnapshotPhaseTiming {
                    phase: "unit".to_string(),
                    elapsed_ms: 1,
                    rows: Some(0),
                }],
                quality_flags: vec![],
                pm_book_source: ResearchSnapshotPmBookSource::default(),
            },
            observations: vec![],
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
        assert_eq!(loaded.manifest.source_surfaces.len(), 1);
        assert!(loaded.manifest.source_surfaces[0].snapshot_sampled);
        assert_eq!(
            loaded.manifest.source_surfaces[0].gate_category,
            "required_for_prediction"
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
        assert_eq!(loaded.manifest.row_counts.observations, 0);
        let quality = std::fs::read_to_string(root.join("quality.md")).expect("read quality");
        assert!(quality.contains("Snapshot contract hash: `sha256:"));
        assert!(quality.contains("## Source Surfaces"));
        assert!(quality.contains("gate_category=`required_for_prediction`"));
        assert!(quality.contains("snapshot_sampled=`true`"));
        assert!(quality.contains("## Input Artifacts"));
        assert!(quality.contains("## Governed Chainlink 5m Settlement Audit"));
        assert!(quality.contains("- `<not-recorded>`"));

        let mut legacy_manifest = serde_json::to_value(&loaded.manifest).expect("legacy manifest");
        legacy_manifest
            .as_object_mut()
            .expect("manifest object")
            .remove("snapshot_contract_hash");
        write_json(root.join("manifest.json"), &legacy_manifest).expect("write legacy manifest");
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
        let mut oracle_audit_tamper = loaded.manifest.clone();
        oracle_audit_tamper.chainlink_oracle_settlement_audit =
            Some(crate::factors::ChainlinkOracleSettlementAudit {
                expected_events: 1,
                accepted_events: 1,
                ..Default::default()
            });
        write_json(root.join("manifest.json"), &oracle_audit_tamper)
            .expect("tamper snapshot oracle audit");
        let tampered_audit =
            load_research_snapshot(&root).expect_err("oracle audit tamper must fail");
        assert!(tampered_audit
            .to_string()
            .contains("evaluator contract hash mismatch"));

        write_json(root.join("manifest.json"), &loaded.manifest)
            .expect("restore snapshot manifest after oracle audit tamper");
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
            chainlink_oracle_settlement_audit: None,
            chainlink_oracle_settlement_evidence: vec![],
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
        let flags = research_snapshot_quality_flags(
            100,
            10,
            10,
            10,
            10,
            0,
            10,
            false,
            300,
            30,
            &ResearchSnapshotPmBookSource::default(),
        );

        assert!(flags.contains(&"pm_book_sample_secs_gt_max_quote_age:300>30".to_string()));
        assert!(!flags.contains(&"no_factor_observations".to_string()));
        assert!(!flags.contains(&"no_pm_book_snapshots".to_string()));
    }

    #[test]
    fn quality_flags_accepts_pm_book_sampling_within_quote_age() {
        let flags = research_snapshot_quality_flags(
            100,
            10,
            10,
            10,
            10,
            0,
            10,
            false,
            30,
            30,
            &ResearchSnapshotPmBookSource::default(),
        );

        assert!(flags.is_empty());
    }

    #[test]
    fn quality_flags_archive_manifest_without_sampled_rows() {
        let flags = research_snapshot_quality_flags(
            100,
            10,
            10,
            10,
            10,
            0,
            0,
            false,
            30,
            30,
            &ResearchSnapshotPmBookSource {
                archive_manifest_rows: 1000,
                archive_status: "archive_loaded".to_string(),
                ..Default::default()
            },
        );

        assert!(flags.contains(&"no_pm_book_snapshots".to_string()));
        assert!(flags.contains(&"pm_book_archive_manifest_rows_but_no_sampled_rows".to_string()));
    }

    #[test]
    fn quality_flags_identify_empty_required_binance_surfaces() {
        let flags = research_snapshot_quality_flags(
            100,
            10,
            0,
            0,
            0,
            0,
            10,
            false,
            30,
            30,
            &ResearchSnapshotPmBookSource::default(),
        );

        assert!(flags.contains(&"no_binance_price_ticks".to_string()));
        assert!(flags.contains(&"no_binance_agg_trade_ticks".to_string()));
        assert!(flags.contains(&"no_binance_lob_snapshots".to_string()));
    }

    #[test]
    fn quality_flags_identify_missing_chainlink_settlement_reference() {
        let flags = research_snapshot_quality_flags(
            100,
            0,
            10,
            10,
            10,
            0,
            10,
            false,
            30,
            30,
            &ResearchSnapshotPmBookSource::default(),
        );

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
}
