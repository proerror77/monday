//! Read-only admission for parent-produced prediction research evidence.
//!
//! The sidecar receives locators in an existing queued request. This module is
//! the only authority that turns those locators into a typed receipt. It reads
//! bounded local bytes, rejects path/symlink escapes, and reuses the canonical
//! cohort, snapshot, Mission, result-receipt, and evaluator-report contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use ploy_market_data::diagnostics::PredictionMarketDataAuditReport;
use ploy_market_data::polymarket_evidence::PolymarketCatalogReceiptState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::prediction_loop::validate_sha256_id;
use crate::prediction_mcts_authenticated::{
    read_authenticated_prediction_experiment_manifest,
    read_authenticated_prediction_result_receipt, AuthenticatedPredictionExperimentManifestRef,
    AuthenticatedPredictionResultReceipt, AuthenticatedPredictionResultReceiptRef,
    AuthenticatedTaskMetrics,
};
use crate::prediction_mission_v3::{
    parse_prediction_mission_json, prediction_mission_v3_sha256, validate_prediction_mission_v3,
    PredictionResearchMissionV3, PredictionTaskKind,
};
use crate::research_snapshot::{
    admit_authenticated_snapshot_for_evidence, authenticate_ready_event_cohort, ResearchSnapshot,
    ResearchSnapshotManifest,
};
use crate::{read_catalog_partition_artifact, CatalogPartitionArtifactRef};

const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SNAPSHOT_TEXT_BYTES: usize = 128 * 1024;
const VERIFIED_EVIDENCE_SCHEMA: &str = "monday.prediction.verified_evidence.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceArtifactRef {
    pub path: String,
    pub artifact_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceMissionRef {
    #[serde(flatten)]
    pub artifact: PredictionEvidenceArtifactRef,
    pub mission_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceCatalogPartitionRef {
    #[serde(flatten)]
    pub artifact: PredictionEvidenceArtifactRef,
    pub payload_sha256: String,
    pub cohort_manifest_id: String,
    pub partition_digest: String,
    pub policy_snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceSnapshotRef {
    pub path: String,
    pub snapshot_hash: String,
    pub snapshot_contract_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceResultBundleRef {
    #[serde(flatten)]
    pub artifact: PredictionEvidenceArtifactRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceReportRef {
    #[serde(flatten)]
    pub artifact: PredictionEvidenceArtifactRef,
    pub report_sha256: String,
    pub report_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceTerminalReceiptRef {
    #[serde(flatten)]
    pub artifact: PredictionEvidenceArtifactRef,
    #[serde(alias = "receipt_sha256")]
    pub terminal_receipt_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionEvidenceRefs {
    pub artifact_root: String,
    pub mission: PredictionEvidenceMissionRef,
    #[serde(alias = "cohort", alias = "producer_verifier")]
    pub catalog_partition: PredictionEvidenceCatalogPartitionRef,
    pub snapshot: PredictionEvidenceSnapshotRef,
    #[serde(alias = "result")]
    pub result_bundle: PredictionEvidenceResultBundleRef,
    #[serde(default, alias = "report_refs")]
    pub reports: Vec<PredictionEvidenceReportRef>,
    #[serde(alias = "terminal")]
    pub terminal_receipt: PredictionEvidenceTerminalReceiptRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedPredictionEvidenceReceipt {
    schema_version: String,
    product: String,
    event_horizon_secs: u32,
    mission_id: String,
    mission_sha256: String,
    task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prediction_horizon_secs: Option<u32>,
    cohort_manifest_id: String,
    partition_digest: String,
    policy_snapshot_id: String,
    snapshot_hash: String,
    snapshot_contract_hash: String,
    producer_manifest_sha256s: Vec<String>,
    verifier_policy_sha256s: Vec<String>,
    result_bundle_sha256: String,
    terminal_receipt_sha256: String,
    report_sha256s: Vec<String>,
    data_audit: bool,
    full_depth: bool,
    executable_replay: bool,
    /// Reserved for a separately reviewed live/runtime evidence contract.
    realtime_runtime: bool,
    runtime_parity: bool,
}

impl VerifiedPredictionEvidenceReceipt {
    pub fn mission_id(&self) -> &str {
        &self.mission_id
    }

    pub fn task(&self) -> &str {
        &self.task
    }

    pub fn product(&self) -> &str {
        &self.product
    }

    pub fn event_horizon_secs(&self) -> u32 {
        self.event_horizon_secs
    }

    pub fn prediction_horizon_secs(&self) -> Option<u32> {
        self.prediction_horizon_secs
    }

    pub fn snapshot_hash(&self) -> &str {
        &self.snapshot_hash
    }

    pub fn report_count(&self) -> usize {
        self.report_sha256s.len()
    }

    pub fn data_audit(&self) -> bool {
        self.data_audit
    }

    pub fn full_depth(&self) -> bool {
        self.full_depth
    }

    pub fn executable_replay(&self) -> bool {
        self.executable_replay
    }

    pub fn realtime_runtime(&self) -> bool {
        self.realtime_runtime
    }

    pub fn runtime_parity(&self) -> bool {
        self.runtime_parity
    }

    pub fn prompt_summary(&self) -> String {
        format!(
            "verified_prediction_evidence schema={} product={} event_horizon_secs={} mission={} task={} side={} horizon={} snapshot={} reports={} data_audit={} full_depth={} executable_replay={} realtime_runtime={}",
            self.schema_version,
            self.product,
            self.event_horizon_secs,
            self.mission_sha256,
            self.task,
            self.side.as_deref().unwrap_or("none"),
            self.prediction_horizon_secs
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.snapshot_hash,
            self.report_sha256s.len(),
            self.data_audit,
            self.full_depth,
            self.executable_replay,
            self.realtime_runtime,
        )
    }
}

#[derive(Debug, Clone)]
struct VerifiedInputs {
    mission: PredictionResearchMissionV3,
    snapshot: ResearchSnapshot,
    cohort_manifest_id: String,
    partition_digest: String,
    policy_snapshot_id: String,
    producer_manifest_sha256s: Vec<String>,
    verifier_policy_sha256s: Vec<String>,
    result_bundle_sha256: String,
    terminal_receipt_sha256: String,
    report_sha256s: Vec<String>,
    data_audit: bool,
    full_depth: bool,
    executable_replay: bool,
}

/// Re-read and verify a complete immutable evidence set under `artifact_root`.
/// No network, command, database, model, or control-plane operation is used.
pub fn verify_prediction_evidence(
    refs: &PredictionEvidenceRefs,
) -> Result<VerifiedPredictionEvidenceReceipt, String> {
    let root = local_artifact_root(&refs.artifact_root)?;
    let inputs = verify_inputs(&root, refs)?;
    let mission = &inputs.mission;
    let task = task_name(&mission.task.kind);
    let (side, prediction_horizon_secs) = match mission.task.kind {
        PredictionTaskKind::SettlementProbability => (None, None),
        PredictionTaskKind::UpExecution => {
            (Some("up".to_string()), mission.task.prediction_horizon_secs)
        }
        PredictionTaskKind::DownExecution => (
            Some("down".to_string()),
            mission.task.prediction_horizon_secs,
        ),
    };
    Ok(VerifiedPredictionEvidenceReceipt {
        schema_version: VERIFIED_EVIDENCE_SCHEMA.to_string(),
        product: product_name(&mission.product.symbol).to_string(),
        event_horizon_secs: mission.product.event_horizon_secs,
        mission_id: mission.mission_id.clone(),
        mission_sha256: mission_hash(mission)?,
        task: task.to_string(),
        side,
        prediction_horizon_secs,
        cohort_manifest_id: inputs.cohort_manifest_id,
        partition_digest: inputs.partition_digest,
        policy_snapshot_id: inputs.policy_snapshot_id,
        snapshot_hash: inputs.snapshot.snapshot_hash().to_string(),
        snapshot_contract_hash: inputs.snapshot.snapshot_contract_hash().to_string(),
        producer_manifest_sha256s: inputs.producer_manifest_sha256s,
        verifier_policy_sha256s: inputs.verifier_policy_sha256s,
        result_bundle_sha256: inputs.result_bundle_sha256,
        terminal_receipt_sha256: inputs.terminal_receipt_sha256,
        report_sha256s: inputs.report_sha256s,
        data_audit: inputs.data_audit,
        full_depth: inputs.full_depth,
        executable_replay: inputs.executable_replay,
        realtime_runtime: false,
        runtime_parity: false,
    })
}

fn verify_inputs(root: &Path, refs: &PredictionEvidenceRefs) -> Result<VerifiedInputs, String> {
    let mission_bytes = read_artifact(root, &refs.mission.artifact, MAX_ARTIFACT_BYTES)?;
    let mission = parse_prediction_mission_json(&mission_bytes)?;
    validate_prediction_mission_v3(&mission)?;
    let expected_mission_sha = mission_hash(&mission)?;
    if normalize_sha256(&expected_mission_sha)? != normalize_sha256(&refs.mission.mission_sha256)? {
        return Err("prediction Mission digest does not match the referenced bytes".to_string());
    }

    let catalog_ref = catalog_ref(&refs.catalog_partition)?;
    let partition_artifact = read_catalog_partition_artifact(root, &catalog_ref)?;
    let cohort = authenticate_ready_event_cohort(
        partition_artifact.catalog(),
        partition_artifact.partition(),
    )
    .map_err(|rejection| format!("authenticate prediction cohort: {rejection:?}"))?;
    let partition = partition_artifact.partition();
    if cohort.manifest_id() != refs.catalog_partition.cohort_manifest_id
        || partition.digest() != refs.catalog_partition.partition_digest
        || partition.causal_projection_policy_id() != refs.catalog_partition.policy_snapshot_id
    {
        return Err(
            "catalog/cohort identity does not match the typed evidence reference".to_string(),
        );
    }

    let snapshot_dir = resolve_relative(root, &refs.snapshot.path)?;
    validate_snapshot_dir(root, &snapshot_dir)?;
    let snapshot = admit_authenticated_snapshot_for_evidence(
        &snapshot_dir,
        &cohort,
        &refs.snapshot.snapshot_contract_hash,
        &refs.snapshot.snapshot_hash,
    )?;
    let snapshot_hash = snapshot
        .manifest
        .snapshot_hash
        .as_deref()
        .ok_or_else(|| "prediction snapshot is missing snapshot_hash".to_string())?;
    let snapshot_contract_hash = snapshot
        .manifest
        .snapshot_contract_hash
        .as_deref()
        .ok_or_else(|| "prediction snapshot is missing snapshot_contract_hash".to_string())?;
    if snapshot_hash != refs.snapshot.snapshot_hash
        || snapshot_contract_hash != refs.snapshot.snapshot_contract_hash
    {
        return Err(
            "prediction snapshot identity does not match the typed evidence reference".to_string(),
        );
    }
    if mission.cohort_manifest_id != cohort.manifest_id()
        || mission.partition_digest != partition.digest()
        || mission.causal_projection_policy_id != partition.causal_projection_policy_id()
        || mission.search_policy_snapshot_id != partition.causal_projection_policy_id()
        || mission.snapshot_contract_id != snapshot_contract_hash
        || mission.snapshot_hash != snapshot_hash
    {
        return Err("Mission does not bind the verified cohort, policy, or snapshot".to_string());
    }
    verify_snapshot_episode_tokens(&snapshot, partition_artifact.catalog())?;

    let (result, result_bundle_sha256) = read_result_bundle(root, &refs.result_bundle, &mission)?;
    let terminal_receipt_sha256 = read_terminal_receipt(root, &refs.terminal_receipt, &result)?;
    let (report_sha256s, report_kinds, data_audit, full_depth) =
        read_reports(root, &refs.reports, &mission, &snapshot)?;
    let executable_replay = executable_replay_is_complete(&mission, &result);
    if mission.task.kind == PredictionTaskKind::SettlementProbability
        && !report_kinds.contains("settlement_baseline")
    {
        return Err(
            "settlement prediction task is missing its settlement-baseline report".to_string(),
        );
    }
    if mission.task.kind == PredictionTaskKind::UpExecution
        || mission.task.kind == PredictionTaskKind::DownExecution
    {
        let expected_side = match mission.task.kind {
            PredictionTaskKind::UpExecution => "up",
            PredictionTaskKind::DownExecution => "down",
            PredictionTaskKind::SettlementProbability => unreachable!(),
        };
        if !report_kinds.contains(&format!("full_depth_execution_{expected_side}")) {
            return Err(format!(
                "prediction {} task is missing its side-bound full-depth report",
                expected_side
            ));
        }
    }

    let producer_manifest_sha256s = partition_artifact
        .catalog()
        .receipts()
        .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready)
        .map(|receipt| receipt.manifest_sha256.clone())
        .collect::<Vec<_>>();
    let verifier_policy_sha256s = partition_artifact
        .catalog()
        .receipts()
        .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready)
        .map(|receipt| {
            serde_json::to_value(&receipt.verifier)
                .ok()
                .and_then(|value| {
                    value
                        .get("policy_sha256")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .ok_or_else(|| "catalog verifier identity is not serializable".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(VerifiedInputs {
        mission,
        snapshot,
        cohort_manifest_id: cohort.manifest_id().to_string(),
        partition_digest: partition.digest().to_string(),
        policy_snapshot_id: partition.causal_projection_policy_id().to_string(),
        producer_manifest_sha256s,
        verifier_policy_sha256s,
        result_bundle_sha256,
        terminal_receipt_sha256,
        report_sha256s,
        data_audit,
        full_depth,
        executable_replay,
    })
}

fn local_artifact_root(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty()
        || raw.contains('\0')
        || raw.starts_with("http://")
        || raw.starts_with("https://")
    {
        return Err("prediction evidence artifact_root must be a local path".to_string());
    }
    let root = PathBuf::from(raw);
    if !root.is_absolute() {
        return Err("prediction evidence artifact_root must be absolute".to_string());
    }
    let metadata = fs::symlink_metadata(&root)
        .map_err(|error| format!("inspect prediction evidence artifact_root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(
            "prediction evidence artifact_root must be a non-symlink directory".to_string(),
        );
    }
    Ok(root)
}

fn resolve_relative(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let relative = Path::new(raw);
    if raw.trim().is_empty()
        || relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("unsafe prediction evidence path {raw}"));
    }
    let path = root.join(relative);
    reject_symlink_components(root, &path)?;
    Ok(path)
}

fn reject_symlink_components(root: &Path, path: &Path) -> Result<(), String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "prediction evidence path escapes artifact root: {}",
            path.display()
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!(
                "unsafe prediction evidence path {}",
                path.display()
            ));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            format!(
                "inspect prediction evidence path {}: {error}",
                current.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "prediction evidence path contains a symlink: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

fn read_artifact(
    root: &Path,
    reference: &PredictionEvidenceArtifactRef,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let path = resolve_relative(root, &reference.path)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("inspect prediction evidence artifact: {error}"))?;
    if !metadata.is_file() {
        return Err(format!(
            "prediction evidence artifact is not a file: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    File::open(&path)
        .map_err(|error| format!("open prediction evidence artifact: {error}"))?
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read prediction evidence artifact: {error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "prediction evidence artifact exceeds {max_bytes} bytes"
        ));
    }
    let expected = normalize_sha256(&reference.artifact_sha256)?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        return Err(format!(
            "prediction evidence artifact hash mismatch for {}",
            reference.path
        ));
    }
    Ok(bytes)
}

fn catalog_ref(
    reference: &PredictionEvidenceCatalogPartitionRef,
) -> Result<CatalogPartitionArtifactRef, String> {
    serde_json::from_value(serde_json::json!({
        "path": reference.artifact.path.clone(),
        "artifact_sha256": reference.artifact.artifact_sha256.clone(),
        "payload_sha256": reference.payload_sha256.clone(),
    }))
    .map_err(|error| format!("parse catalog partition reference: {error}"))
}

fn validate_snapshot_dir(root: &Path, snapshot_dir: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(snapshot_dir)
        .map_err(|error| format!("inspect prediction snapshot directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("prediction snapshot path must be a non-symlink directory".to_string());
    }
    let manifest_path = resolve_relative(
        root,
        snapshot_dir
            .strip_prefix(root)
            .map_err(|_| "prediction snapshot escapes artifact root".to_string())?
            .join("manifest.json")
            .to_string_lossy()
            .as_ref(),
    )?;
    let manifest_bytes = read_file_bounded(&manifest_path, MAX_SNAPSHOT_TEXT_BYTES)?;
    let manifest: ResearchSnapshotManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("parse prediction snapshot manifest: {error}"))?;
    for artifact in [
        Some(manifest.artifacts.observations_json.as_str()),
        Some(manifest.artifacts.deribit_snapshots_json.as_str()),
        Some(manifest.artifacts.pm_book_snapshots_json.as_str()),
        manifest.artifacts.observations_parquet.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let artifact_path = snapshot_dir.join(artifact);
        let relative = artifact_path
            .strip_prefix(root)
            .map_err(|_| "prediction snapshot artifact escapes artifact root".to_string())?;
        resolve_relative(root, &relative.to_string_lossy())?;
    }
    Ok(())
}

fn read_file_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("inspect evidence file: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(format!(
            "evidence path is not a regular file: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    File::open(path)
        .map_err(|error| format!("open evidence file: {error}"))?
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read evidence file: {error}"))?;
    if bytes.len() > max_bytes {
        return Err(format!("evidence file exceeds {max_bytes} bytes"));
    }
    Ok(bytes)
}

fn verify_snapshot_episode_tokens(
    snapshot: &ResearchSnapshot,
    catalog: &ploy_market_data::polymarket_evidence::PolymarketReadyEventCatalog,
) -> Result<(), String> {
    let tokens = catalog
        .receipts()
        .filter(|receipt| receipt.state == PolymarketCatalogReceiptState::Ready)
        .map(|receipt| {
            Ok::<_, String>((
                receipt.market_id.clone(),
                (
                    receipt.up_token_id.clone().ok_or_else(|| {
                        format!("ready market {} is missing UP token", receipt.market_id)
                    })?,
                    receipt.down_token_id.clone().ok_or_else(|| {
                        format!("ready market {} is missing DOWN token", receipt.market_id)
                    })?,
                ),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut seen = BTreeSet::new();
    for row in &snapshot.observations {
        let Some((up, down)) = tokens.get(&row.event_id) else {
            return Err(format!(
                "snapshot observation {} is outside the verified catalog",
                row.event_id
            ));
        };
        if row.up_token_id != *up || row.down_token_id != *down {
            return Err(format!(
                "snapshot observation {} has mismatched UP/DOWN token identity",
                row.event_id
            ));
        }
        seen.insert(row.event_id.clone());
    }
    for book in &snapshot.pm_book_snapshots {
        let Some((up, down)) = tokens.get(&book.event_id) else {
            return Err(format!(
                "snapshot book {} is outside the verified catalog",
                book.event_id
            ));
        };
        if book.token_id != *up && book.token_id != *down {
            return Err(format!(
                "snapshot book {} has mismatched token identity",
                book.event_id
            ));
        }
        seen.insert(book.event_id.clone());
    }
    let expected = tokens.keys().cloned().collect::<BTreeSet<_>>();
    if seen.is_empty() {
        return Err("prediction snapshot has no catalog-bound episode rows".to_string());
    }
    if seen != expected {
        return Err(
            "prediction snapshot does not cover exactly the verified catalog episodes".to_string(),
        );
    }
    Ok(())
}

fn read_result_bundle(
    root: &Path,
    reference: &PredictionEvidenceResultBundleRef,
    mission: &PredictionResearchMissionV3,
) -> Result<(AuthenticatedPredictionResultReceipt, String), String> {
    let bytes = read_artifact(root, &reference.artifact, MAX_ARTIFACT_BYTES)?;
    let bundle_digest = format!("sha256:{}", sha256_hex(&bytes));
    if let Some(expected) = reference.receipt_sha256.as_deref() {
        validate_sha256_id(expected, "prediction result receipt")?;
    }
    if let Some(expected) = reference.manifest_sha256.as_deref() {
        validate_sha256_id(expected, "prediction experiment manifest")?;
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse prediction result bundle: {error}"))?;
    let (result_root, result_path) = result_namespace(root, &reference.artifact.path)?;
    if value.get("schema_version").and_then(Value::as_str)
        == Some("prediction_authenticated_experiment_manifest.v1")
    {
        let manifest_ref: AuthenticatedPredictionExperimentManifestRef = serde_json::from_value(
            serde_json::json!({
                "path": result_path.clone(),
                "artifact_sha256": reference.artifact.artifact_sha256.clone(),
                "manifest_sha256": reference.manifest_sha256.clone().ok_or_else(|| "experiment manifest reference is missing manifest_sha256".to_string())?,
            }),
        )
        .map_err(|error| format!("parse experiment manifest reference: {error}"))?;
        let manifest =
            read_authenticated_prediction_experiment_manifest(&result_root, &manifest_ref)?;
        let selected = match mission.task.kind {
            PredictionTaskKind::SettlementProbability => &manifest.settlement,
            PredictionTaskKind::UpExecution => &manifest.up_execution,
            PredictionTaskKind::DownExecution => &manifest.down_execution,
        };
        let result = read_authenticated_prediction_result_receipt(&result_root, selected)?;
        verify_result_identity(&result, mission)?;
        return Ok((result, bundle_digest));
    }
    let result_ref: AuthenticatedPredictionResultReceiptRef = serde_json::from_value(
        serde_json::json!({
            "path": result_path,
            "artifact_sha256": reference.artifact.artifact_sha256.clone(),
            "receipt_sha256": reference.receipt_sha256.clone().ok_or_else(|| "result receipt reference is missing receipt_sha256".to_string())?,
        }),
    )
    .map_err(|error| format!("parse result receipt reference: {error}"))?;
    let result = read_authenticated_prediction_result_receipt(&result_root, &result_ref)?;
    verify_result_identity(&result, mission)?;
    Ok((result, bundle_digest))
}

fn result_namespace(root: &Path, path: &str) -> Result<(PathBuf, String), String> {
    let relative = Path::new(path);
    let mut prefix = PathBuf::new();
    let mut suffix = PathBuf::new();
    let mut in_namespace = false;
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(format!("unsafe result bundle path {path}"));
        };
        if in_namespace {
            suffix.push(component);
        } else if component == std::ffi::OsStr::new("mcts-v4") {
            in_namespace = true;
            suffix.push(component);
        } else {
            prefix.push(component);
        }
    }
    if in_namespace {
        if prefix.as_os_str().is_empty() {
            Ok((root.to_path_buf(), suffix.to_string_lossy().into_owned()))
        } else {
            Ok((root.join(&prefix), suffix.to_string_lossy().into_owned()))
        }
    } else {
        Ok((root.to_path_buf(), path.to_string()))
    }
}

fn verify_result_identity(
    result: &AuthenticatedPredictionResultReceipt,
    mission: &PredictionResearchMissionV3,
) -> Result<(), String> {
    result.validate()?;
    if result.mission != *mission {
        return Err(
            "prediction result receipt Mission does not match the queued Mission".to_string(),
        );
    }
    if result.mission_sha256 != mission_hash(mission)? {
        return Err("prediction result receipt Mission digest mismatch".to_string());
    }
    if result.result_sealed_unix_millis == 0 {
        return Err("prediction result receipt is not terminal".to_string());
    }
    Ok(())
}

fn read_terminal_receipt(
    root: &Path,
    reference: &PredictionEvidenceTerminalReceiptRef,
    result: &AuthenticatedPredictionResultReceipt,
) -> Result<String, String> {
    let bytes = read_artifact(root, &reference.artifact, MAX_ARTIFACT_BYTES)?;
    let artifact_sha = format!("sha256:{}", sha256_hex(&bytes));
    let expected_terminal = normalize_sha256(&reference.terminal_receipt_sha256)?;
    if artifact_sha != format!("sha256:{expected_terminal}")
        && reference.terminal_receipt_sha256 != result.sha256
    {
        return Err("terminal receipt digest does not match its referenced bytes".to_string());
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse terminal prediction receipt: {error}"))?;
    if value.get("schema_version").and_then(Value::as_str)
        == Some("prediction_authenticated_result_receipt.v1")
    {
        let terminal: AuthenticatedPredictionResultReceipt = serde_json::from_value(value)
            .map_err(|error| format!("parse terminal result receipt: {error}"))?;
        verify_result_identity(&terminal, &result.mission)?;
        if terminal.sha256 != result.sha256 {
            return Err("terminal receipt is for a different result".to_string());
        }
        return Ok(terminal.sha256);
    }
    Err(
        "terminal prediction receipt must use the canonical authenticated result receipt"
            .to_string(),
    )
}

fn read_reports(
    root: &Path,
    references: &[PredictionEvidenceReportRef],
    mission: &PredictionResearchMissionV3,
    snapshot: &ResearchSnapshot,
) -> Result<(Vec<String>, BTreeSet<String>, bool, bool), String> {
    let mut hashes = Vec::with_capacity(references.len());
    let mut kinds = BTreeSet::new();
    let mut data_audit = false;
    let mut full_depth = false;
    for reference in references {
        let bytes = read_artifact(root, &reference.artifact, MAX_REPORT_BYTES)?;
        let digest = format!("sha256:{}", sha256_hex(&bytes));
        if normalize_sha256(&reference.report_sha256)? != normalize_sha256(&digest)? {
            return Err(format!(
                "report {} hash does not match its bytes",
                reference.artifact.path
            ));
        }
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("parse prediction evaluator report: {error}"))?;
        match reference.report_kind.as_str() {
            "data_audit" => {
                let audit: PredictionMarketDataAuditReport = serde_json::from_value(value)
                    .map_err(|error| format!("parse prediction data audit report: {error}"))?;
                audit
                    .validate_for_prediction_snapshot(
                        &snapshot.manifest.symbols,
                        snapshot.manifest.start,
                        snapshot.manifest.end,
                    )
                    .map_err(|error| format!("prediction data audit rejected: {error}"))?;
                if snapshot.manifest.data_audit_status.as_deref() != Some("ok") {
                    return Err(
                        "prediction snapshot does not carry an ok data-audit status".to_string()
                    );
                }
                require_data_audit_binding(snapshot, &digest)?;
                data_audit = true;
            }
            "settlement_baseline" => {
                verify_report_identity(&value, mission)?;
                require_settlement_baseline_report(&value, mission)?;
            }
            "full_depth_execution_up" => {
                verify_report_identity(&value, mission)?;
                require_full_depth_report(&value, "up", snapshot)?;
                full_depth = true;
            }
            "full_depth_execution_down" => {
                verify_report_identity(&value, mission)?;
                require_full_depth_report(&value, "down", snapshot)?;
                full_depth = true;
            }
            kind => return Err(format!("unsupported prediction report kind {kind}")),
        }
        hashes.push(digest);
        kinds.insert(reference.report_kind.clone());
    }
    Ok((hashes, kinds, data_audit, full_depth))
}

fn verify_report_identity(
    value: &Value,
    mission: &PredictionResearchMissionV3,
) -> Result<(), String> {
    if value.get("mission_id").and_then(Value::as_str) != Some(mission.mission_id.as_str())
        || value.get("snapshot_hash").and_then(Value::as_str)
            != Some(mission.snapshot_hash.as_str())
        || value.get("snapshot_contract_hash").and_then(Value::as_str)
            != Some(mission.snapshot_contract_id.as_str())
        || value
            .get("search_policy_snapshot_id")
            .and_then(Value::as_str)
            != Some(mission.search_policy_snapshot_id.as_str())
    {
        return Err("prediction evaluator report identity does not match Mission".to_string());
    }
    Ok(())
}

fn require_data_audit_binding(
    snapshot: &ResearchSnapshot,
    report_digest: &str,
) -> Result<(), String> {
    let uri = snapshot
        .manifest
        .data_audit_report
        .as_deref()
        .ok_or_else(|| "prediction snapshot is missing its canonical data-audit URI".to_string())?;
    let prefix = match snapshot.manifest.source_kind.as_str() {
        "verified_immutable_artifacts" => "verified+audit://sha256/",
        "verified_polymarket_chainlink_baseline" => {
            "verified+polymarket-chainlink-baseline-audit://sha256/"
        }
        other => {
            return Err(format!(
                "prediction snapshot source kind {other} has no canonical data-audit URI"
            ))
        }
    };
    let expected = uri
        .strip_prefix(prefix)
        .filter(|value| !value.contains('/') && !value.contains(':'))
        .ok_or_else(|| "prediction snapshot data-audit URI is not canonical".to_string())?;
    if normalize_sha256(expected)? != normalize_sha256(report_digest)? {
        return Err("prediction data audit bytes do not match snapshot audit hash".to_string());
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct SettlementBaselineReportPayload {
    schema_version: String,
    non_finite_floats: String,
    mission_id: String,
    search_policy_snapshot_id: String,
    snapshot_hash: String,
    snapshot_contract_hash: String,
    settlement_probability: SettlementProbabilityPayload,
    settlement_probability_walk_forward: SettlementWalkForwardPayload,
    settlement_verdict_walk_forward: SettlementWalkForwardPayload,
    promotion_gate: SettlementPromotionGatePayload,
}

#[derive(Debug, Deserialize)]
struct SettlementProbabilityPayload {
    baselines: Vec<Value>,
    calibration: Vec<Value>,
    edge_buckets: Vec<Value>,
    anti_overfit: Vec<Value>,
    symbol_holdouts: Vec<Value>,
    ablations: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct SettlementWalkForwardPayload {
    windows: Vec<Value>,
    aggregates: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct SettlementPromotionGatePayload {
    options: SettlementPromotionGateOptionsPayload,
    ready_for_dry_run_handoff: bool,
    gates: Vec<SettlementPromotionGateRowPayload>,
}

#[derive(Debug, Deserialize)]
struct SettlementPromotionGateOptionsPayload {
    stake_usd: f64,
    min_entry_fill_rate: f64,
    max_expected_calibration_error: f64,
    min_positive_window_ratio: f64,
    require_deribit: bool,
    include_deribit: bool,
    data_audit_status: Option<String>,
    data_quality_mode: String,
    event_complete_events: usize,
    event_complete_rows: usize,
    min_event_complete_events: usize,
    min_event_complete_rows: usize,
    global_full_depth_entry_fill_rate: Option<f64>,
    replay_parity_ready: bool,
    replay_parity_evidence: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SettlementPromotionGateRowPayload {
    gate: String,
    passed: bool,
    evidence: String,
}

#[derive(Debug, Deserialize)]
struct FullDepthExecutionRowPayload {
    market_id: String,
    symbol: String,
    tick_ts: DateTime<Utc>,
    token_id: String,
    opposite_token_id: String,
    side: String,
    stake_usd: f64,
    entry_fillable: bool,
}

fn require_settlement_baseline_report(
    value: &Value,
    mission: &PredictionResearchMissionV3,
) -> Result<(), String> {
    require_report_schema(value, "monday.polymarket.settlement_baseline.v1")?;
    let report: SettlementBaselineReportPayload = serde_json::from_value(value.clone())
        .map_err(|error| format!("parse typed settlement baseline report: {error}"))?;
    if report.schema_version != "monday.polymarket.settlement_baseline.v1"
        || report.non_finite_floats != "null"
        || report.mission_id != mission.mission_id
        || report.snapshot_hash != mission.snapshot_hash
        || report.snapshot_contract_hash != mission.snapshot_contract_id
        || report.search_policy_snapshot_id != mission.search_policy_snapshot_id
    {
        return Err("typed settlement baseline report identity is incomplete or mismatched".into());
    }
    if report.promotion_gate.gates.is_empty() {
        return Err(
            "typed settlement baseline report is missing its promotion-gate payload".into(),
        );
    }
    let options = &report.promotion_gate.options;
    if !options.stake_usd.is_finite()
        || options.stake_usd <= 0.0
        || !options.min_entry_fill_rate.is_finite()
        || !options.max_expected_calibration_error.is_finite()
        || !options.min_positive_window_ratio.is_finite()
        || !matches!(
            options.data_quality_mode.as_str(),
            "strict_continuous" | "event_complete"
        )
        || (options.require_deribit && !options.include_deribit)
        || (options.replay_parity_ready && options.replay_parity_evidence.is_none())
    {
        return Err("typed settlement baseline report has invalid promotion-gate options".into());
    }
    if options.data_audit_status.as_deref().is_none() {
        return Err("typed settlement baseline report is missing data-audit gate status".into());
    }
    if options
        .global_full_depth_entry_fill_rate
        .is_some_and(|rate| !rate.is_finite())
    {
        return Err("typed settlement baseline report has a non-finite global fill rate".into());
    }
    let _event_complete_counts = (
        options.event_complete_events,
        options.event_complete_rows,
        options.min_event_complete_events,
        options.min_event_complete_rows,
        options.global_full_depth_entry_fill_rate,
    );
    for (name, rows) in [
        ("baselines", &report.settlement_probability.baselines),
        ("calibration", &report.settlement_probability.calibration),
        ("edge_buckets", &report.settlement_probability.edge_buckets),
        ("anti_overfit", &report.settlement_probability.anti_overfit),
        (
            "symbol_holdouts",
            &report.settlement_probability.symbol_holdouts,
        ),
        ("ablations", &report.settlement_probability.ablations),
        (
            "walk_forward_windows",
            &report.settlement_probability_walk_forward.windows,
        ),
        (
            "walk_forward_aggregates",
            &report.settlement_probability_walk_forward.aggregates,
        ),
        (
            "verdict_walk_forward_windows",
            &report.settlement_verdict_walk_forward.windows,
        ),
        (
            "verdict_walk_forward_aggregates",
            &report.settlement_verdict_walk_forward.aggregates,
        ),
    ] {
        if rows.iter().any(|row| !row.is_object()) {
            return Err(format!(
                "typed settlement baseline report {name} contains a non-object row"
            ));
        }
    }
    for (name, rows, fields) in [
        (
            "baselines",
            &report.settlement_probability.baselines,
            &["model", "n", "avg_predicted_q", "actual_win_rate"][..],
        ),
        (
            "calibration",
            &report.settlement_probability.calibration,
            &[
                "model",
                "q_bucket",
                "count",
                "avg_predicted_q",
                "actual_win_rate",
            ][..],
        ),
        (
            "edge_buckets",
            &report.settlement_probability.edge_buckets,
            &["model", "edge_bucket", "count", "avg_edge"][..],
        ),
        (
            "anti_overfit",
            &report.settlement_probability.anti_overfit,
            &["model", "test", "n", "pass"][..],
        ),
        (
            "symbol_holdouts",
            &report.settlement_probability.symbol_holdouts,
            &["model", "symbol", "n", "pass"][..],
        ),
        (
            "ablations",
            &report.settlement_probability.ablations,
            &["model", "reference_model", "n", "improves_error"][..],
        ),
        (
            "walk_forward_windows",
            &report.settlement_probability_walk_forward.windows,
            &[
                "window_index",
                "model",
                "train_start",
                "train_end",
                "test_start",
                "test_end",
                "pass",
            ][..],
        ),
        (
            "walk_forward_aggregates",
            &report.settlement_probability_walk_forward.aggregates,
            &["model", "windows", "pass_window_ratio"][..],
        ),
        (
            "verdict_walk_forward_windows",
            &report.settlement_verdict_walk_forward.windows,
            &[
                "window_index",
                "model",
                "test_n",
                "test_brier_score",
                "pass",
            ][..],
        ),
        (
            "verdict_walk_forward_aggregates",
            &report.settlement_verdict_walk_forward.aggregates,
            &["model", "windows", "oos_rows", "pass_window_ratio"][..],
        ),
    ] {
        for row in rows {
            let Some(object) = row.as_object() else {
                return Err(format!(
                    "typed settlement baseline report {name} contains a non-object row"
                ));
            };
            if fields
                .iter()
                .any(|field| object.get(*field).is_none_or(Value::is_null))
            {
                return Err(format!(
                    "typed settlement baseline report {name} contains an incomplete row"
                ));
            }
        }
    }
    let required_gates = BTreeSet::from([
        "data_quality",
        "deribit_vol_surface",
        "full_depth_entry_capacity",
        "conservative_entry_capacity",
        "global_full_depth_entry_fillability",
        "probability_calibration",
        "full_depth_settlement_edge",
        "conservative_settlement_edge",
        "anti_overfit_diagnostics",
        "symbol_holdout",
        "walk_forward_oos",
        "recorded_replay_parity",
    ]);
    let mut actual_gates = BTreeSet::new();
    for gate in &report.promotion_gate.gates {
        if gate.gate.trim().is_empty() || gate.evidence.trim().is_empty() {
            return Err("typed settlement baseline report contains an incomplete gate".into());
        }
        if !actual_gates.insert(gate.gate.as_str()) {
            return Err("typed settlement baseline report contains duplicate gates".into());
        }
        if !required_gates.contains(gate.gate.as_str()) {
            return Err(format!(
                "typed settlement baseline report contains unsupported gate {}",
                gate.gate
            ));
        }
    }
    if actual_gates != required_gates {
        return Err("typed settlement baseline report is missing required promotion gates".into());
    }
    let expected_ready = report
        .promotion_gate
        .gates
        .iter()
        .filter(|gate| gate.gate != "recorded_replay_parity")
        .all(|gate| gate.passed);
    if report.promotion_gate.ready_for_dry_run_handoff != expected_ready {
        return Err(
            "typed settlement baseline report promotion readiness disagrees with its gates".into(),
        );
    }
    if report
        .promotion_gate
        .gates
        .iter()
        .find(|gate| gate.gate == "recorded_replay_parity")
        .is_some_and(|gate| gate.passed != options.replay_parity_ready)
    {
        return Err(
            "typed settlement baseline report replay-parity gate disagrees with its options".into(),
        );
    }
    // Keep the field typed and consumed even when all gate outcomes are false:
    // a failed evaluator artifact is still a valid report only if it carries
    // every canonical gate row and its evidence.
    let _ready_for_dry_run_handoff = report.promotion_gate.ready_for_dry_run_handoff;
    let _passed_gate_count = report
        .promotion_gate
        .gates
        .iter()
        .filter(|gate| gate.passed)
        .count();
    Ok(())
}

fn require_report_schema(value: &Value, expected: &str) -> Result<(), String> {
    if value.get("schema_version").and_then(Value::as_str) != Some(expected) {
        return Err(format!("prediction report schema must be {expected}"));
    }
    Ok(())
}

fn require_full_depth_report(
    value: &Value,
    side: &str,
    snapshot: &ResearchSnapshot,
) -> Result<(), String> {
    require_report_schema(value, "monday.polymarket.full_depth_execution.v2")?;
    if !value
        .get("side")
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(side))
    {
        return Err(format!(
            "full-depth {side} report is incomplete or side-mismatched"
        ));
    }
    let snapshot_events = snapshot.observations.iter().fold(
        BTreeMap::<String, (String, String, BTreeSet<DateTime<Utc>>)>::new(),
        |mut events, row| {
            let entry = events.entry(row.event_id.clone()).or_insert_with(|| {
                (
                    row.up_token_id.clone(),
                    row.down_token_id.clone(),
                    BTreeSet::new(),
                )
            });
            entry.2.insert(row.tick_ts);
            events
        },
    );
    if snapshot_events.is_empty() {
        return Err("full-depth report cannot bind to an empty snapshot".to_string());
    }
    for profile in ["observed", "conservative"] {
        let rows = value
            .pointer(&format!("/{profile}/rows"))
            .and_then(Value::as_array)
            .ok_or_else(|| format!("full-depth {side} report is missing {profile} rows"))?;
        if rows.is_empty() {
            return Err(format!(
                "full-depth {side} report has no substantive {profile} rows"
            ));
        }
        let typed_rows: Vec<FullDepthExecutionRowPayload> =
            serde_json::from_value(Value::Array(rows.clone())).map_err(|error| {
                format!("full-depth {side} {profile} rows are not canonical event rows: {error}")
            })?;
        let mut seen = BTreeSet::new();
        for row in typed_rows {
            if row.market_id.trim().is_empty()
                || row.symbol.trim().is_empty()
                || row.token_id.trim().is_empty()
                || row.opposite_token_id.trim().is_empty()
                || !row.side.eq_ignore_ascii_case(side)
                || !row.stake_usd.is_finite()
                || row.stake_usd <= 0.0
                || row.tick_ts < snapshot.manifest.start
                || row.tick_ts >= snapshot.manifest.end
            {
                return Err(format!(
                    "full-depth {side} {profile} row is not a substantive canonical event row"
                ));
            }
            let Some((up_token_id, down_token_id, valid_ticks)) =
                snapshot_events.get(&row.market_id)
            else {
                return Err(format!(
                    "full-depth {side} {profile} row {} is outside the verified snapshot",
                    row.market_id
                ));
            };
            if !snapshot
                .manifest
                .symbols
                .iter()
                .any(|expected_symbol| expected_symbol == &row.symbol)
            {
                return Err(format!(
                    "full-depth {side} {profile} row {} has an unexpected symbol",
                    row.market_id
                ));
            }
            let (expected_token_id, expected_opposite_token_id) = if side == "up" {
                (up_token_id, down_token_id)
            } else {
                (down_token_id, up_token_id)
            };
            if &row.token_id != expected_token_id
                || &row.opposite_token_id != expected_opposite_token_id
                || !valid_ticks.contains(&row.tick_ts)
            {
                return Err(format!(
                    "full-depth {side} {profile} row {} is not bound to its snapshot tokens/time",
                    row.market_id
                ));
            }
            let _ = (row.tick_ts, row.entry_fillable);
            seen.insert(row.market_id);
        }
        if seen != snapshot_events.keys().cloned().collect() {
            return Err(format!(
                "full-depth {side} {profile} rows do not cover the verified snapshot exactly: expected={:?} seen={:?}",
                snapshot_events.keys().collect::<BTreeSet<_>>(),
                seen
            ));
        }
    }
    Ok(())
}

fn executable_replay_is_complete(
    mission: &PredictionResearchMissionV3,
    result: &AuthenticatedPredictionResultReceipt,
) -> bool {
    if !matches!(
        mission.task.kind,
        PredictionTaskKind::UpExecution | PredictionTaskKind::DownExecution
    ) {
        return false;
    }
    match &result.metrics {
        AuthenticatedTaskMetrics::UpExecution(metrics)
        | AuthenticatedTaskMetrics::DownExecution(metrics) => {
            metrics.roundtrip_count > 0
                && metrics.mean_fill_rate.is_finite()
                && metrics.mean_fill_rate > 0.0
                && metrics.mean_total_fee_usd.is_finite()
                && metrics.mean_total_fee_usd >= 0.0
                && metrics.mean_capacity_usd.is_finite()
                && metrics.mean_capacity_usd >= 0.0
        }
        AuthenticatedTaskMetrics::Settlement(_) => false,
    }
}

fn mission_hash(mission: &PredictionResearchMissionV3) -> Result<String, String> {
    prediction_mission_v3_sha256(mission)
}

fn task_name(task: &PredictionTaskKind) -> &'static str {
    match task {
        PredictionTaskKind::SettlementProbability => "settlement_probability",
        PredictionTaskKind::UpExecution => "up_execution",
        PredictionTaskKind::DownExecution => "down_execution",
    }
}

fn product_name(product: &crate::prediction_mission_v3::PredictionProductSymbol) -> &'static str {
    match product {
        crate::prediction_mission_v3::PredictionProductSymbol::Btc => "BTC",
    }
}

fn normalize_sha256(value: &str) -> Result<String, String> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("evidence digest must be 64 lowercase hexadecimal characters".to_string());
    }
    Ok(value.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_normalization_is_strict() {
        assert_eq!(
            normalize_sha256(&format!("sha256:{}", "a".repeat(64))).unwrap(),
            "a".repeat(64)
        );
        assert!(normalize_sha256(&"A".repeat(64)).is_err());
        assert!(normalize_sha256("sha256:short").is_err());
    }

    #[test]
    fn result_namespace_reanchors_nested_mcts_receipts() {
        let (root, path) = result_namespace(
            Path::new("/tmp/evidence"),
            "research-trial/mcts-v4/experiment-manifests/manifest.json",
        )
        .expect("namespace");
        assert_eq!(root, PathBuf::from("/tmp/evidence/research-trial"));
        assert_eq!(path, "mcts-v4/experiment-manifests/manifest.json");
    }

    #[test]
    fn skeletal_settlement_report_is_rejected_before_evidence_capability() {
        let mission: PredictionResearchMissionV3 = serde_json::from_value(serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "settlement-skeletal-test",
            "product": { "symbol": "BTC", "event_horizon_secs": 300 },
            "task": { "kind": "settlement_probability" },
            "run_mode": "research_trial",
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": format!("sha256:{}", "1".repeat(64)),
            "partition_digest": format!("sha256:{}", "2".repeat(64)),
            "causal_projection_policy_id": format!("sha256:{}", "3".repeat(64)),
            "snapshot_contract_id": format!("sha256:{}", "4".repeat(64)),
            "snapshot_hash": "5".repeat(16),
            "search_policy_snapshot_id": format!("sha256:{}", "6".repeat(64)),
            "search_budget": { "max_candidates": 1, "max_seconds": 30 }
        }))
        .expect("test Mission");
        let skeletal = serde_json::json!({
            "schema_version": "monday.polymarket.settlement_baseline.v1",
            "non_finite_floats": "null",
            "mission_id": mission.mission_id,
            "search_policy_snapshot_id": mission.search_policy_snapshot_id,
            "snapshot_hash": mission.snapshot_hash,
            "snapshot_contract_hash": mission.snapshot_contract_id,
            "settlement_probability": {
                "baselines": [{}],
                "calibration": [],
                "edge_buckets": [],
                "anti_overfit": [],
                "symbol_holdouts": [],
                "ablations": []
            },
            "settlement_probability_walk_forward": { "windows": [], "aggregates": [] },
            "settlement_verdict_walk_forward": { "windows": [], "aggregates": [] },
            "promotion_gate": {
                "options": {},
                "ready_for_dry_run_handoff": false,
                "gates": []
            }
        });
        let error = require_settlement_baseline_report(&skeletal, &mission)
            .expect_err("skeletal evaluator report must be rejected");
        assert!(error.contains("parse typed settlement baseline report"));
    }
}
