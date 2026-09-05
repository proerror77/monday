//! Transactional DuckDB source of truth for the bounded Loop Engineer control plane.

pub mod approval_revocations;

use alpha_domain::{
    canonical_json_hash, AttributionKind, AttributionMode, CandidateArtifact, CandidateEvaluation,
    CexBaselineArtifactV1, CexFactorBankRevisionV2, CexFinalPrecommitV1,
    CexFourStageStrategyCandidateV1, CexResearchContentRefV1, CexResearchMissionArtifactV1,
    CexSealedHoldoutClaimV1, EngineKind, EvaluationProtocolV1, FormulaEvaluatorConfig,
    IterationVerdict, LearningDirective, LoopRun, MissionStatus, MissionTerminalReason,
    PromotionRecord, ResearchIteration, ResearchMission, RuntimeAttributionEvent,
    SearchBudgetUsage, SearchPolicyRevision, StrategyBundle, StrategyBundleArtifact,
    VerifiedRuntimeAttributionEvent, CEX_FINAL_PRECOMMIT_REGISTRY_KIND,
    CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND, ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION,
    ONNX_WALK_FORWARD_EVALUATOR_VERSION, SEALED_HOLDOUT_EVALUATOR_VERSION,
    WALK_FORWARD_EVALUATOR_VERSION,
};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection, Transaction};
use governance::{
    AllowedIntentType, DeploymentEnvelope, LiveSmallEligibilityEvidence, SignedDeploymentEnvelope,
};
use hmac::{Hmac, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const MIGRATION_001: &str = include_str!("../migrations/001_control_plane.sql");
const MIGRATION_002: &str = include_str!("../migrations/002_promotion_bundles.sql");
const MIGRATION_003: &str = include_str!("../migrations/003_loop_runs_and_engine_checkpoints.sql");
const MIGRATION_004: &str = include_str!("../migrations/004_approval_revocations.sql");
const INTEGRITY_KEY_ENV: &str = "ALPHA_STORE_INTEGRITY_KEY_HEX";
const INTEGRITY_KEY_BYTES: usize = 32;
const MISSION_EVALUATION_PROTOCOL_KIND: &str = "mission_evaluation_protocol";

fn sealed_evaluation_revision_id(candidate_id: &str, evaluator_version: &str) -> String {
    format!("sealed-evaluation:{evaluator_version}:{candidate_id}")
}

fn mission_evaluation_protocol_revision_id(mission_id: &str) -> String {
    format!("mission-evaluation-protocol:{mission_id}")
}

fn validate_mission_evaluation_protocol_revision(
    revision: &RegistryRevision,
    mission_id: &str,
) -> Result<MissionEvaluationProtocolBinding, StoreError> {
    if revision.registry_kind != MISSION_EVALUATION_PROTOCOL_KIND || revision.asset_id != mission_id
    {
        return Err(StoreError::Domain(
            "mission evaluation protocol binding is invalid".to_string(),
        ));
    }
    let binding: MissionEvaluationProtocolBinding =
        serde_json::from_value(revision.payload.clone()).map_err(serialization_error)?;
    binding
        .evaluation_protocol
        .validate()
        .map_err(domain_error)?;
    if binding
        .evaluation_protocol
        .content_hash()
        .map_err(domain_error)?
        != binding.evaluation_protocol_hash
    {
        return Err(StoreError::Domain(
            "mission evaluation protocol binding is invalid".to_string(),
        ));
    }
    Ok(binding)
}

fn mission_evaluation_protocol_binding(
    connection: &Connection,
    mission_id: &str,
) -> Result<MissionEvaluationProtocolBinding, StoreError> {
    let revision: RegistryRevision = read_json_row(
        connection,
        "SELECT payload_json, content_hash FROM registry_revisions WHERE revision_id = ?",
        &mission_evaluation_protocol_revision_id(mission_id),
    )?;
    validate_mission_evaluation_protocol_revision(&revision, mission_id)
}

fn ensure_cex_search_open(connection: &Connection, mission_id: &str) -> Result<(), StoreError> {
    let count = connection
        .query_row(
            "SELECT COUNT(*) FROM registry_revisions WHERE registry_kind = ? AND asset_id = ?",
            params![CEX_FINAL_PRECOMMIT_REGISTRY_KIND, mission_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(database_error)?;
    if count == 0 {
        Ok(())
    } else {
        Err(StoreError::Domain(
            "CEX final precommit makes mission search and resume terminal".to_string(),
        ))
    }
}

fn require_registry_payload_reference(
    connection: &Connection,
    reference: &CexResearchContentRefV1,
    registry_kind: &str,
) -> Result<RegistryRevision, StoreError> {
    reference.validate().map_err(domain_error)?;
    let revision: RegistryRevision = read_json_row(
        connection,
        "SELECT payload_json, content_hash FROM registry_revisions WHERE revision_id = ?",
        &reference.id,
    )?;
    let payload_hash = canonical_json_hash(&revision.payload).map_err(domain_error)?;
    if revision.registry_kind != registry_kind || payload_hash != reference.content_sha256 {
        return Err(StoreError::Domain(format!(
            "CEX final precommit reference {} expected {registry_kind}/{} but read {}/{}",
            reference.id, reference.content_sha256, revision.registry_kind, payload_hash
        )));
    }
    Ok(revision)
}

fn require_typed_registry_payload_reference<T>(
    connection: &Connection,
    reference: &CexResearchContentRefV1,
    registry_kind: &str,
) -> Result<T, StoreError>
where
    T: DeserializeOwned + Serialize,
{
    reference.validate().map_err(domain_error)?;
    let revision: RegistryRevision = read_json_row(
        connection,
        "SELECT payload_json, content_hash FROM registry_revisions WHERE revision_id = ?",
        &reference.id,
    )?;
    let payload: T = serde_json::from_value(revision.payload).map_err(serialization_error)?;
    let payload_hash = canonical_json_hash(&payload).map_err(domain_error)?;
    if revision.registry_kind != registry_kind || payload_hash != reference.content_sha256 {
        return Err(StoreError::Domain(format!(
            "CEX final precommit typed reference {} expected {registry_kind}/{} but read {}/{}",
            reference.id, reference.content_sha256, revision.registry_kind, payload_hash
        )));
    }
    Ok(payload)
}

fn validate_cex_precommit_dependencies(
    connection: &Connection,
    precommit: &CexFinalPrecommitV1,
    strategy: &CexFourStageStrategyCandidateV1,
) -> Result<(), StoreError> {
    strategy
        .validate_against_precommit(precommit)
        .map_err(domain_error)?;
    let mission_revision =
        require_registry_payload_reference(connection, &precommit.mission, "cex_research_mission")?;
    let control_mission: CexResearchMissionArtifactV1 =
        serde_json::from_value(mission_revision.payload).map_err(serialization_error)?;
    control_mission.validate().map_err(domain_error)?;
    if control_mission.semantic_id().map_err(domain_error)? != precommit.mission.id
        || control_mission.spec.inputs.snapshot != precommit.snapshot
        || control_mission.spec.inputs.dataset != precommit.dataset
        || control_mission.spec.inputs.partition != precommit.partition
        || control_mission.spec.inputs.source != precommit.source
        || control_mission.spec.policies.weight != precommit.weight_policy
        || control_mission.spec.policies.evaluation != precommit.evaluation_protocol
        || control_mission.spec.holdout.holdout_id != precommit.holdout_id
        || control_mission.spec.holdout.state != precommit.holdout_state
        || control_mission.spec.instrument.venue != strategy.venue
        || control_mission.spec.instrument.market != strategy.market
        || control_mission.spec.instrument.symbol != strategy.symbol
    {
        return Err(StoreError::Domain(
            "CEX final precommit does not match its Mission inputs and policies".to_string(),
        ));
    }
    let factor_bank: CexFactorBankRevisionV2 = require_typed_registry_payload_reference(
        connection,
        &precommit.factor_bank,
        "cex_factor_bank",
    )?;
    factor_bank.validate().map_err(domain_error)?;
    strategy
        .validate_against_factor_bank(&factor_bank)
        .map_err(domain_error)?;
    for (reference, kind) in [
        (&precommit.ridge_baseline, "cex_baseline_ridge"),
        (&precommit.cart_baseline, "cex_baseline_cart"),
    ] {
        let baseline: CexBaselineArtifactV1 =
            require_typed_registry_payload_reference(connection, reference, kind)?;
        baseline.validate().map_err(domain_error)?;
    }
    require_registry_payload_reference(connection, &precommit.baseline_gate, "cex_baseline_gate")?;
    let replay = require_registry_payload_reference(
        connection,
        &precommit.replay_receipt,
        "cex_event_replay_receipt",
    )?;
    let replay_reference =
        |field: &str| -> Result<CexResearchContentRefV1, StoreError> {
            serde_json::from_value(replay.payload.get(field).cloned().ok_or_else(|| {
                StoreError::Domain("CEX replay receipt is incomplete".to_string())
            })?)
            .map_err(serialization_error)
        };
    if replay.asset_id != precommit.mission.id
        || replay.parent_revision_id.as_deref() != Some(precommit.four_stage_strategy.id.as_str())
        || replay
            .payload
            .get("receipt_id")
            .and_then(serde_json::Value::as_str)
            != Some(precommit.replay_receipt.id.as_str())
        || replay
            .payload
            .get("mission_id")
            .and_then(serde_json::Value::as_str)
            != Some(precommit.mission.id.as_str())
        || replay_reference("strategy")? != precommit.four_stage_strategy
        || replay_reference("dataset")? != precommit.dataset
        || replay_reference("source")? != precommit.source
        || replay
            .payload
            .pointer("/gate/passed")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || replay
            .payload
            .get("holdout_id")
            .and_then(serde_json::Value::as_str)
            != Some(precommit.holdout_id.as_str())
        || replay
            .payload
            .get("holdout_state")
            .and_then(serde_json::Value::as_str)
            != Some("unopened")
        || replay
            .payload
            .get("deployment_authority")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || replay
            .payload
            .get("order_submission_authority")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
        || replay
            .payload
            .get("capabilities_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(precommit.replay_capabilities_sha256.as_str())
    {
        return Err(StoreError::Domain(
            "CEX replay receipt does not prove the exact passing precommit strategy".to_string(),
        ));
    }
    Ok(())
}

fn validate_cex_sealed_revision(
    revision: &RegistryRevision,
    claim: &CexSealedHoldoutClaimV1,
    precommit: &CexFinalPrecommitV1,
) -> Result<CandidateEvaluation, StoreError> {
    let evaluation: CandidateEvaluation =
        serde_json::from_value(revision.payload.get("evaluation").cloned().ok_or_else(|| {
            StoreError::Domain("sealed evaluation receipt is incomplete".to_string())
        })?)
        .map_err(serialization_error)?;
    evaluation.validate().map_err(domain_error)?;
    let (_, protocol_hash) = evaluation.protocol_binding().map_err(domain_error)?;
    let expected_precommit_hash = canonical_json_hash(precommit).map_err(domain_error)?;
    if revision.revision_id
        != sealed_evaluation_revision_id(&claim.candidate.id, SEALED_HOLDOUT_EVALUATOR_VERSION)
        || revision.registry_kind != "sealed_evaluation"
        || revision.asset_id != claim.candidate.id
        || revision.parent_revision_id.as_deref() != Some(claim.claim_id.as_str())
        || revision
            .payload
            .get("mission_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.mission_id.as_str())
        || revision
            .payload
            .get("candidate_content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(claim.candidate.content_sha256.as_str())
        || revision
            .payload
            .get("dataset_manifest_id")
            .and_then(serde_json::Value::as_str)
            != Some(precommit.dataset_manifest_id.as_str())
        || revision
            .payload
            .get("evaluation_protocol_hash")
            .and_then(serde_json::Value::as_str)
            != Some(claim.evaluation_protocol.content_sha256.as_str())
        || revision
            .payload
            .get("precommit_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.precommit.id.as_str())
        || revision
            .payload
            .get("precommit_content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(expected_precommit_hash.as_str())
        || revision
            .payload
            .get("sealed_access_claim_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.claim_id.as_str())
        || revision
            .payload
            .get("holdout_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.holdout_id.as_str())
        || evaluation.evaluator_version != SEALED_HOLDOUT_EVALUATOR_VERSION
        || protocol_hash != claim.evaluation_protocol.content_sha256
    {
        return Err(StoreError::Domain(
            "sealed evaluation receipt does not match the exact final precommit".to_string(),
        ));
    }
    Ok(evaluation)
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("record already exists")]
    DuplicateRecord,
    #[error("record was not found")]
    NotFound,
    #[error("nonce has already been consumed")]
    NonceReplay,
    #[error("invalid domain record: {0}")]
    Domain(String),
    #[error("DuckDB error: {0}")]
    Database(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("stored content hash does not match the payload")]
    ContentHashMismatch,
    #[error("stored record authenticity tag does not match the payload")]
    AuthenticityMismatch,
    #[error("stored record has no authenticity tag and cannot be trusted")]
    MissingAuthenticityTag,
    #[error(
        "legacy checkpoint has no exact engine state; use mission recover-legacy-checkpoint with a new mission id"
    )]
    LegacyCheckpoint,
    #[error("checkpoint does not match its mission iteration")]
    CheckpointMismatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCheckpoint {
    pub mission_id: String,
    pub last_iteration_id: Option<String>,
    pub budget_usage: SearchBudgetUsage,
    /// `None` is accepted only for checkpoint records written before protocol binding existed.
    #[serde(default)]
    pub evaluation_protocol_hash: Option<String>,
    pub engine_kind: EngineKind,
    pub engine_version: u32,
    pub engine_state: serde_json::Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredCandidate {
    pub candidate_id: String,
    pub mission_id: String,
    pub iteration_id: String,
    pub artifact: CandidateArtifact,
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionLineage {
    pub mission: ResearchMission,
    pub iterations: Vec<ResearchIteration>,
    pub candidates: Vec<StoredCandidate>,
    pub evaluations: Vec<StoredEvaluation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationRecord {
    pub evaluation_id: String,
    pub mission_id: String,
    pub candidate_id: String,
    #[serde(default)]
    pub dataset_manifest_id: String,
    #[serde(default)]
    pub evaluation_protocol_hash: String,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEvaluation {
    pub record: EvaluationRecord,
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryRevision {
    pub revision_id: String,
    pub registry_kind: String,
    pub asset_id: String,
    pub parent_revision_id: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissionEvaluationProtocolBinding {
    evaluation_protocol: EvaluationProtocolV1,
    evaluation_protocol_hash: String,
    #[serde(default)]
    legacy_history_unbound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub event_id: String,
    pub mission_id: Option<String>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub approval_id: String,
    pub approval_class: String,
    pub subject_id: String,
    pub payload: serde_json::Value,
    #[serde(default)]
    pub signer_id: Option<String>,
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub revoked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub revoked_by: Option<String>,
    #[serde(default)]
    pub revocation_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ApprovalRecord {
    pub fn validate(&self) -> Result<(), StoreError> {
        require_text(&self.approval_id)?;
        require_text(&self.approval_class)?;
        require_text(&self.subject_id)?;
        let signer = self
            .signer_id
            .as_deref()
            .ok_or_else(|| StoreError::Domain("approval signer_id is required".to_string()))?;
        require_text(signer)?;
        let valid_from = self
            .valid_from
            .ok_or_else(|| StoreError::Domain("approval valid_from is required".to_string()))?;
        let expires_at = self
            .expires_at
            .ok_or_else(|| StoreError::Domain("approval expires_at is required".to_string()))?;
        if valid_from >= expires_at || self.created_at > valid_from {
            return Err(StoreError::Domain(
                "approval validity window is invalid".to_string(),
            ));
        }
        match self.revoked_at {
            Some(revoked_at)
                if revoked_at < self.created_at
                    || self
                        .revoked_by
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty())
                    || self
                        .revocation_reason
                        .as_deref()
                        .is_none_or(|value| value.trim().is_empty()) =>
            {
                return Err(StoreError::Domain(
                    "revoked approval requires a valid time, actor, and reason".to_string(),
                ));
            }
            None if self.revoked_by.is_some() || self.revocation_reason.is_some() => {
                return Err(StoreError::Domain(
                    "approval revocation metadata is incomplete".to_string(),
                ));
            }
            _ => {}
        }
        Ok(())
    }

    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        self.signer_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self.valid_from.is_some_and(|from| now >= from)
            && self.expires_at.is_some_and(|until| now < until)
            && self.revoked_at.is_none_or(|revoked_at| now < revoked_at)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredPromotion {
    pub record: PromotionRecord,
    pub content_hash: String,
}

pub struct AlphaStore {
    path: PathBuf,
    connection: Connection,
    integrity_key: [u8; INTEGRITY_KEY_BYTES],
}

impl AlphaStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| StoreError::Database(error.to_string()))?;
        }
        let connection = Connection::open(path).map_err(database_error)?;
        let integrity_key = load_or_create_integrity_key(path)?;
        let mut store = Self {
            path: path.to_path_buf(),
            connection,
            integrity_key,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory().map_err(database_error)?;
        let integrity_key = generate_integrity_key()?;
        let mut store = Self {
            path: PathBuf::from(":memory:"),
            connection,
            integrity_key,
        };
        store.migrate()?;
        Ok(store)
    }

    /// Opens an existing database through a DuckDB read-only connection. Unlike
    /// [`Self::open`] this never creates directories, database files, or
    /// integrity key material, and never runs migrations.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(StoreError::Database(format!(
                "database {} does not exist",
                path.display()
            )));
        }
        let config = duckdb::Config::default()
            .access_mode(duckdb::AccessMode::ReadOnly)
            .map_err(database_error)?;
        let connection = Connection::open_with_flags(path, config).map_err(database_error)?;
        let integrity_key = load_existing_integrity_key(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            connection,
            integrity_key,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn migrate(&mut self) -> Result<(), StoreError> {
        self.connection
            .execute_batch(MIGRATION_001)
            .map_err(database_error)?;
        self.connection
            .execute_batch(MIGRATION_002)
            .map_err(database_error)?;
        self.connection
            .execute_batch(MIGRATION_003)
            .map_err(database_error)?;
        self.connection
            .execute_batch(MIGRATION_004)
            .map_err(database_error)
    }

    pub fn create_mission(&mut self, mission: &ResearchMission) -> Result<(), StoreError> {
        mission.validate().map_err(domain_error)?;
        let (json, hash) = encoded(mission)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        ensure_absent(&transaction, "missions", "mission_id", &mission.mission_id)?;
        transaction
            .execute(
                "INSERT INTO missions VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    mission.mission_id,
                    enum_name(&mission.status)?,
                    json,
                    hash,
                    mission.created_at.to_rfc3339(),
                    mission.updated_at.to_rfc3339()
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(&mission.mission_id),
            "mission_created",
            &mission.mission_id,
            &hash,
            mission.created_at,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn get_mission(&self, mission_id: &str) -> Result<ResearchMission, StoreError> {
        read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM missions WHERE mission_id = ?",
            mission_id,
        )
    }

    pub fn transition_mission(
        &mut self,
        mission_id: &str,
        next: MissionStatus,
        at: DateTime<Utc>,
    ) -> Result<ResearchMission, StoreError> {
        let mut mission = self.get_mission(mission_id)?;
        mission.transition_to(next, at).map_err(domain_error)?;
        let (json, hash) = encoded(&mission)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE missions SET status = ?, payload_json = ?, content_hash = ?, updated_at = ?
                 WHERE mission_id = ?",
                params![
                    enum_name(&mission.status)?,
                    json,
                    hash,
                    mission.updated_at.to_rfc3339(),
                    mission_id
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(mission_id),
            "mission_transitioned",
            mission_id,
            &hash,
            at,
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(mission)
    }

    pub fn finish_mission(
        &mut self,
        mission_id: &str,
        reason: MissionTerminalReason,
        at: DateTime<Utc>,
    ) -> Result<ResearchMission, StoreError> {
        let mut mission = self.get_mission(mission_id)?;
        mission.finish(reason, at).map_err(domain_error)?;
        let (json, hash) = encoded(&mission)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE missions SET status = ?, payload_json = ?, content_hash = ?, updated_at = ?
                 WHERE mission_id = ?",
                params![
                    enum_name(&mission.status)?,
                    json,
                    hash,
                    mission.updated_at.to_rfc3339(),
                    mission_id
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(mission_id),
            "mission_finished",
            mission_id,
            &hash,
            at,
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(mission)
    }

    pub fn create_loop_run(&mut self, run: &LoopRun) -> Result<(), StoreError> {
        run.validate().map_err(domain_error)?;
        let (json, hash) = encoded(run)?;
        let auth_tag =
            authentication_tag(&self.integrity_key, "loop_run", &run.loop_run_id, &json)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        ensure_present(&transaction, "missions", "mission_id", &run.root_mission_id)?;
        ensure_absent(&transaction, "loop_runs", "loop_run_id", &run.loop_run_id)?;
        transaction
            .execute(
                "INSERT INTO loop_runs (
                    loop_run_id, root_mission_id, status, payload_json, content_hash, auth_tag,
                    created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    run.loop_run_id,
                    run.root_mission_id,
                    enum_name(&run.status)?,
                    json,
                    hash,
                    auth_tag,
                    run.created_at.to_rfc3339(),
                    run.updated_at.to_rfc3339(),
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(&run.root_mission_id),
            "loop_run_created",
            &run.loop_run_id,
            &hash,
            run.created_at,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn save_loop_run(&mut self, run: &LoopRun) -> Result<(), StoreError> {
        run.validate().map_err(domain_error)?;
        let existing = self.get_loop_run(&run.loop_run_id)?;
        if existing.root_mission_id != run.root_mission_id
            || existing.completion_policy != run.completion_policy
            || existing.created_at != run.created_at
            || !run
                .child_mission_ids
                .starts_with(&existing.child_mission_ids)
            || !run.stage_records.starts_with(&existing.stage_records)
        {
            return Err(StoreError::Domain(
                "loop run history is immutable and may only be appended".to_string(),
            ));
        }
        for mission_id in &run.child_mission_ids {
            ensure_present(&self.connection, "missions", "mission_id", mission_id)?;
        }
        let (json, hash) = encoded(run)?;
        let auth_tag =
            authentication_tag(&self.integrity_key, "loop_run", &run.loop_run_id, &json)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE loop_runs SET status = ?, payload_json = ?, content_hash = ?, auth_tag = ?, updated_at = ?
                 WHERE loop_run_id = ?",
                params![
                    enum_name(&run.status)?,
                    json,
                    hash,
                    auth_tag,
                    run.updated_at.to_rfc3339(),
                    run.loop_run_id,
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(&run.root_mission_id),
            "loop_run_saved",
            &run.loop_run_id,
            &hash,
            run.updated_at,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn get_loop_run(&self, loop_run_id: &str) -> Result<LoopRun, StoreError> {
        let (json, hash, auth_tag) = self
            .connection
            .query_row(
                "SELECT payload_json, content_hash, auth_tag FROM loop_runs WHERE loop_run_id = ?",
                params![loop_run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .map_err(map_query_error)?;
        let auth_tag = auth_tag.ok_or(StoreError::MissingAuthenticityTag)?;
        decode_authenticated(
            &self.integrity_key,
            "loop_run",
            loop_run_id,
            &json,
            &hash,
            &auth_tag,
        )
    }

    pub fn append_iteration(
        &mut self,
        iteration: &ResearchIteration,
        candidate: Option<(&str, &CandidateArtifact)>,
        evaluation: Option<&EvaluationRecord>,
    ) -> Result<(), StoreError> {
        ensure_cex_search_open(&self.connection, &iteration.mission_id)?;
        validate_iteration_records(iteration, candidate, evaluation)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let iteration_hash =
            insert_iteration_records(&transaction, iteration, candidate, evaluation)?;
        append_journal(
            &transaction,
            Some(&iteration.mission_id),
            "iteration_appended",
            &iteration.iteration_id,
            &iteration_hash,
            iteration.created_at,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn append_iteration_with_checkpoint(
        &mut self,
        iteration: &ResearchIteration,
        candidate: Option<(&str, &CandidateArtifact)>,
        evaluation: Option<&EvaluationRecord>,
        checkpoint: &RunCheckpoint,
    ) -> Result<(), StoreError> {
        self.append_iteration_with_checkpoint_mode(
            iteration, candidate, evaluation, checkpoint, false,
        )
    }

    pub fn append_iteration_with_legacy_checkpoint_upgrade(
        &mut self,
        iteration: &ResearchIteration,
        candidate: Option<(&str, &CandidateArtifact)>,
        evaluation: Option<&EvaluationRecord>,
        checkpoint: &RunCheckpoint,
    ) -> Result<(), StoreError> {
        self.append_iteration_with_checkpoint_mode(
            iteration, candidate, evaluation, checkpoint, true,
        )
    }

    fn append_iteration_with_checkpoint_mode(
        &mut self,
        iteration: &ResearchIteration,
        candidate: Option<(&str, &CandidateArtifact)>,
        evaluation: Option<&EvaluationRecord>,
        checkpoint: &RunCheckpoint,
        allow_legacy_protocol_upgrade: bool,
    ) -> Result<(), StoreError> {
        ensure_cex_search_open(&self.connection, &iteration.mission_id)?;
        validate_iteration_records(iteration, candidate, evaluation)?;
        validate_checkpoint_for_iteration(checkpoint, iteration)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let iteration_hash =
            insert_iteration_records(&transaction, iteration, candidate, evaluation)?;
        let checkpoint_hash = upsert_checkpoint(
            &transaction,
            checkpoint,
            &self.integrity_key,
            allow_legacy_protocol_upgrade,
        )?;
        append_journal(
            &transaction,
            Some(&iteration.mission_id),
            "iteration_appended",
            &iteration.iteration_id,
            &iteration_hash,
            iteration.created_at,
        )?;
        append_journal(
            &transaction,
            Some(&checkpoint.mission_id),
            "checkpoint_saved",
            &checkpoint.mission_id,
            &checkpoint_hash,
            checkpoint.updated_at,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn save_checkpoint(&mut self, checkpoint: &RunCheckpoint) -> Result<(), StoreError> {
        ensure_cex_search_open(&self.connection, &checkpoint.mission_id)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let hash = upsert_checkpoint(&transaction, checkpoint, &self.integrity_key, false)?;
        append_journal(
            &transaction,
            Some(&checkpoint.mission_id),
            "checkpoint_saved",
            &checkpoint.mission_id,
            &hash,
            checkpoint.updated_at,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn get_checkpoint(&self, mission_id: &str) -> Result<RunCheckpoint, StoreError> {
        self.connection
            .query_row(
                "SELECT checkpoint_json, content_hash, auth_tag FROM checkpoints WHERE mission_id = ?",
                params![mission_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .map_err(map_query_error)
            .and_then(|(checkpoint_json, content_hash, auth_tag)| {
                let checkpoint_json = checkpoint_json.ok_or(StoreError::LegacyCheckpoint)?;
                let content_hash = content_hash.ok_or(StoreError::LegacyCheckpoint)?;
                let auth_tag = auth_tag.ok_or(StoreError::MissingAuthenticityTag)?;
                decode_authenticated(
                    &self.integrity_key,
                    "checkpoint",
                    mission_id,
                    &checkpoint_json,
                    &content_hash,
                    &auth_tag,
                )
            })
    }

    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn rewrite_checkpoint_as_legacy_unbound_test_fixture(
        &mut self,
        mission_id: &str,
    ) -> Result<(), StoreError> {
        let checkpoint = self.get_checkpoint(mission_id)?;
        let mut value = serde_json::to_value(checkpoint).map_err(serialization_error)?;
        value
            .as_object_mut()
            .ok_or_else(|| StoreError::Serialization("checkpoint is not an object".to_string()))?
            .remove("evaluation_protocol_hash");
        let checkpoint_json = serde_json::to_string(&value).map_err(serialization_error)?;
        let content_hash = hex::encode(Sha256::digest(checkpoint_json.as_bytes()));
        let auth_tag = authentication_tag(
            &self.integrity_key,
            "checkpoint",
            mission_id,
            &checkpoint_json,
        )?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE checkpoints SET checkpoint_json = ?, content_hash = ?, auth_tag = ? WHERE mission_id = ?",
                params![checkpoint_json, content_hash, auth_tag, mission_id],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "DELETE FROM registry_revisions WHERE revision_id = ?",
                params![mission_evaluation_protocol_revision_id(mission_id)],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub fn fork_legacy_checkpoint(
        &mut self,
        mission_id: &str,
        replacement_mission_id: &str,
        at: DateTime<Utc>,
    ) -> Result<ResearchMission, StoreError> {
        require_text(mission_id)?;
        require_text(replacement_mission_id)?;
        if mission_id == replacement_mission_id {
            return Err(StoreError::Domain(
                "legacy checkpoint recovery requires a new mission id".to_string(),
            ));
        }
        let recovery_reason = match self.get_checkpoint(mission_id) {
            Err(StoreError::LegacyCheckpoint | StoreError::MissingAuthenticityTag) => {
                "legacy checkpoint lacks exact engine state and cannot be resumed safely"
                    .to_string()
            }
            Ok(checkpoint)
                if checkpoint.engine_kind == EngineKind::Mcts && checkpoint.engine_version < 2 =>
            {
                format!(
                    "MCTS checkpoint version {} predates the live-only checkpoint contract and cannot be resumed safely",
                    checkpoint.engine_version
                )
            }
            Ok(_) => return Err(StoreError::Domain(
                "checkpoint is resumable; recovery forks only legacy or pre-v2 MCTS checkpoints"
                    .to_string(),
            )),
            Err(error) => return Err(error),
        };

        let mut source = self.get_mission(mission_id)?;
        let mut replacement = source.clone();
        replacement.mission_id = replacement_mission_id.to_string();
        replacement.status = MissionStatus::Pending;
        replacement.terminal_reason = None;
        replacement.created_at = at;
        replacement.updated_at = at;
        replacement.validate().map_err(domain_error)?;

        if matches!(
            source.status,
            MissionStatus::Pending | MissionStatus::Paused
        ) {
            source
                .transition_to(MissionStatus::Running, at)
                .map_err(domain_error)?;
        }
        source
            .finish(
                MissionTerminalReason::Failed {
                    code: format!("legacy_checkpoint_forked_to:{replacement_mission_id}"),
                },
                at,
            )
            .map_err(domain_error)?;

        let (source_json, source_hash) = encoded(&source)?;
        let (replacement_json, replacement_hash) = encoded(&replacement)?;
        let recovery = MemoryRecord {
            event_id: format!("legacy-checkpoint-fork:{mission_id}:{replacement_mission_id}"),
            mission_id: Some(mission_id.to_string()),
            payload: serde_json::json!({
                "kind": "legacy_checkpoint_fork",
                "source_mission_id": mission_id,
                "replacement_mission_id": replacement_mission_id,
                "reason": recovery_reason,
            }),
            created_at: at,
        };
        let (recovery_json, recovery_hash) = encoded(&recovery)?;

        let transaction = self.connection.transaction().map_err(database_error)?;
        ensure_absent(
            &transaction,
            "missions",
            "mission_id",
            replacement_mission_id,
        )?;
        ensure_absent(
            &transaction,
            "research_memory",
            "event_id",
            &recovery.event_id,
        )?;
        transaction
            .execute(
                "UPDATE missions SET status = ?, payload_json = ?, content_hash = ?, updated_at = ?
                 WHERE mission_id = ?",
                params![
                    enum_name(&source.status)?,
                    source_json,
                    source_hash,
                    source.updated_at.to_rfc3339(),
                    mission_id,
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO missions VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    replacement.mission_id,
                    enum_name(&replacement.status)?,
                    replacement_json,
                    replacement_hash,
                    replacement.created_at.to_rfc3339(),
                    replacement.updated_at.to_rfc3339(),
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO research_memory VALUES (?, ?, ?, ?, ?)",
                params![
                    recovery.event_id,
                    recovery.mission_id,
                    recovery_json,
                    recovery_hash,
                    recovery.created_at.to_rfc3339(),
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(mission_id),
            "mission_finished",
            mission_id,
            &source_hash,
            at,
        )?;
        append_journal(
            &transaction,
            Some(replacement_mission_id),
            "mission_created",
            replacement_mission_id,
            &replacement_hash,
            at,
        )?;
        append_journal(
            &transaction,
            Some(mission_id),
            "legacy_checkpoint_forked",
            &recovery.event_id,
            &recovery_hash,
            at,
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(replacement)
    }

    pub fn mission_lineage(&self, mission_id: &str) -> Result<MissionLineage, StoreError> {
        let mission = self.get_mission(mission_id)?;
        let iterations = read_json_rows(
            &self.connection,
            "SELECT payload_json, content_hash FROM iterations WHERE mission_id = ? ORDER BY created_at, iteration_id",
            mission_id,
        )?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT candidate_id, iteration_id, payload_json, content_hash, created_at
                 FROM candidate_artifacts WHERE mission_id = ? ORDER BY created_at, candidate_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![mission_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(database_error)?;
        let mut candidates = Vec::new();
        for row in rows {
            let (candidate_id, iteration_id, payload_json, content_hash, created_at) =
                row.map_err(database_error)?;
            verify_hash(&payload_json, &content_hash)?;
            candidates.push(StoredCandidate {
                candidate_id,
                mission_id: mission_id.to_string(),
                iteration_id,
                artifact: serde_json::from_str(&payload_json).map_err(serialization_error)?,
                content_hash,
                created_at: DateTime::parse_from_rfc3339(&created_at)
                    .map_err(|error| StoreError::Serialization(error.to_string()))?
                    .with_timezone(&Utc),
            });
        }
        let mut statement = self
            .connection
            .prepare(
                "SELECT payload_json, content_hash FROM evaluation_artifacts
                 WHERE mission_id = ? ORDER BY created_at, evaluation_id",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![mission_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        let mut evaluations = Vec::new();
        for row in rows {
            let (json, content_hash) = row.map_err(database_error)?;
            verify_hash(&json, &content_hash)?;
            evaluations.push(StoredEvaluation {
                record: serde_json::from_str(&json).map_err(serialization_error)?,
                content_hash,
            });
        }
        Ok(MissionLineage {
            mission,
            iterations,
            candidates,
            evaluations,
        })
    }

    pub fn put_registry_revision(&mut self, revision: &RegistryRevision) -> Result<(), StoreError> {
        require_text(&revision.revision_id)?;
        require_text(&revision.registry_kind)?;
        require_text(&revision.asset_id)?;
        self.insert_json_record(
            "registry_revisions",
            &revision.revision_id,
            revision,
            (None, "registry_revision_added", revision.created_at),
            |transaction, json, hash| {
                transaction.execute(
                    "INSERT INTO registry_revisions VALUES (?, ?, ?, ?, ?, ?, ?)",
                    params![
                        revision.revision_id,
                        revision.registry_kind,
                        revision.asset_id,
                        revision.parent_revision_id,
                        json,
                        hash,
                        revision.created_at.to_rfc3339()
                    ],
                )?;
                Ok(())
            },
        )
    }

    pub fn get_registry_revision(&self, revision_id: &str) -> Result<RegistryRevision, StoreError> {
        read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM registry_revisions WHERE revision_id = ?",
            revision_id,
        )
    }

    pub fn put_cex_final_precommit(
        &mut self,
        iteration: &ResearchIteration,
        candidate_id: &str,
        candidate: &CandidateArtifact,
        evaluation: &EvaluationRecord,
        precommit: &CexFinalPrecommitV1,
    ) -> Result<RegistryRevision, StoreError> {
        precommit.validate().map_err(domain_error)?;
        validate_iteration_records(iteration, Some((candidate_id, candidate)), Some(evaluation))?;
        let CandidateArtifact::CexFourStage(strategy) = candidate else {
            return Err(StoreError::Domain(
                "CEX final precommit requires a four-stage candidate".to_string(),
            ));
        };
        strategy.validate().map_err(domain_error)?;
        let mission = self.get_mission(&iteration.mission_id)?;
        if !matches!(
            mission.status,
            MissionStatus::Completed | MissionStatus::BudgetExhausted
        ) || iteration.mission_id != precommit.mission.id
            || iteration.engine != EngineKind::Mcts
            || iteration.verdict != IterationVerdict::Keep
            || mission.dataset_manifest_id != precommit.dataset_manifest_id
        {
            return Err(StoreError::Domain(
                "CEX final precommit does not match its terminal mission".to_string(),
            ));
        }
        if candidate_id != precommit.final_candidate.id
            || strategy.precommit_id != precommit.precommit_id
            || strategy.mission_id != precommit.mission.id
        {
            return Err(StoreError::Domain(
                "CEX final precommit does not match its candidate identity".to_string(),
            ));
        }
        if strategy.strategy_artifact_id != precommit.four_stage_strategy.id
            || strategy.strategy_artifact_sha256 != precommit.four_stage_strategy.content_sha256
        {
            return Err(StoreError::Domain(
                "CEX final precommit does not match its four-stage strategy content".to_string(),
            ));
        }
        if strategy.evaluation_protocol_hash != precommit.evaluation_protocol.content_sha256 {
            return Err(StoreError::Domain(
                "CEX final precommit does not match its evaluation protocol".to_string(),
            ));
        }
        let (_, candidate_hash) = encoded(candidate)?;
        if candidate_hash != precommit.final_candidate.content_sha256 {
            return Err(StoreError::Domain(
                "CEX final candidate identity drifted".to_string(),
            ));
        }
        let typed_evaluation: CandidateEvaluation =
            serde_json::from_value(evaluation.payload.clone()).map_err(serialization_error)?;
        typed_evaluation.validate().map_err(domain_error)?;
        if evaluation.evaluation_id != precommit.final_walk_forward_evaluation.id
            || canonical_json_hash(&typed_evaluation).map_err(domain_error)?
                != precommit.final_walk_forward_evaluation.content_sha256
        {
            return Err(StoreError::Domain(
                "CEX final walk-forward identity drifted".to_string(),
            ));
        }
        let (_, protocol_hash) = typed_evaluation.protocol_binding().map_err(domain_error)?;
        let strategy_artifact: serde_json::Value =
            serde_json::from_str(&strategy.strategy_artifact_json).map_err(serialization_error)?;
        let selected_evaluation: CandidateEvaluation = serde_json::from_value(
            strategy_artifact
                .pointer("/walk_forward_evidence/selected/evaluation")
                .cloned()
                .ok_or_else(|| {
                    StoreError::Domain(
                        "CEX four-stage candidate has no selected walk-forward evidence"
                            .to_string(),
                    )
                })?,
        )
        .map_err(serialization_error)?;
        if !typed_evaluation.passed
            || typed_evaluation.evaluator_version != WALK_FORWARD_EVALUATOR_VERSION
            || typed_evaluation != selected_evaluation
            || protocol_hash != precommit.evaluation_protocol.content_sha256
            || typed_evaluation.formula_config().map_err(domain_error)?
                != FormulaEvaluatorConfig::for_mission(&mission).map_err(domain_error)?
        {
            return Err(StoreError::Domain(
                "CEX final candidate lacks its exact passing walk-forward evidence".to_string(),
            ));
        }
        self.require_mission_evaluation_protocol(
            &mission.mission_id,
            typed_evaluation.protocol_binding().map_err(domain_error)?.0,
        )?;
        validate_cex_precommit_dependencies(&self.connection, precommit, strategy)?;

        let revision = RegistryRevision {
            revision_id: precommit.precommit_id.clone(),
            registry_kind: CEX_FINAL_PRECOMMIT_REGISTRY_KIND.to_string(),
            asset_id: precommit.mission.id.clone(),
            parent_revision_id: Some(precommit.replay_receipt.id.clone()),
            payload: serde_json::to_value(precommit).map_err(serialization_error)?,
            created_at: iteration.created_at,
        };
        match self.get_registry_revision(&revision.revision_id) {
            Ok(existing) => {
                let stored_iteration: ResearchIteration = read_json_row(
                    &self.connection,
                    "SELECT payload_json, content_hash FROM iterations WHERE iteration_id = ?",
                    &iteration.iteration_id,
                )?;
                let stored_candidate: CandidateArtifact = read_json_row(
                    &self.connection,
                    "SELECT payload_json, content_hash FROM candidate_artifacts WHERE candidate_id = ?",
                    candidate_id,
                )?;
                let stored_evaluation: EvaluationRecord = read_json_row(
                    &self.connection,
                    "SELECT payload_json, content_hash FROM evaluation_artifacts WHERE evaluation_id = ?",
                    &evaluation.evaluation_id,
                )?;
                if existing == revision
                    && stored_iteration == *iteration
                    && stored_candidate == *candidate
                    && stored_evaluation == *evaluation
                {
                    return Ok(existing);
                }
                return Err(StoreError::Domain(
                    "CEX final precommit conflicts with existing mission truth".to_string(),
                ));
            }
            Err(StoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let competing = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM registry_revisions WHERE registry_kind = ? AND asset_id = ?",
                params![CEX_FINAL_PRECOMMIT_REGISTRY_KIND, precommit.mission.id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(database_error)?;
        if competing != 0 {
            return Err(StoreError::Domain(
                "a Mission can have only one final precommit".to_string(),
            ));
        }

        let transaction = self.connection.transaction().map_err(database_error)?;
        let iteration_hash = insert_iteration_records(
            &transaction,
            iteration,
            Some((candidate_id, candidate)),
            Some(evaluation),
        )?;
        let (revision_json, revision_hash) = encoded(&revision)?;
        ensure_absent(
            &transaction,
            "registry_revisions",
            "revision_id",
            &revision.revision_id,
        )?;
        transaction
            .execute(
                "INSERT INTO registry_revisions VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    revision.revision_id,
                    revision.registry_kind,
                    revision.asset_id,
                    revision.parent_revision_id,
                    revision_json,
                    revision_hash,
                    revision.created_at.to_rfc3339(),
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(&iteration.mission_id),
            "iteration_appended",
            &iteration.iteration_id,
            &iteration_hash,
            iteration.created_at,
        )?;
        append_journal(
            &transaction,
            Some(&iteration.mission_id),
            "cex_final_precommit_added",
            &revision.revision_id,
            &revision_hash,
            revision.created_at,
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(revision)
    }

    pub fn claim_cex_sealed_holdout(
        &mut self,
        claim: &CexSealedHoldoutClaimV1,
        at: DateTime<Utc>,
    ) -> Result<Option<RegistryRevision>, StoreError> {
        claim.validate().map_err(domain_error)?;
        let precommit_revision = self.get_registry_revision(&claim.precommit.id)?;
        let precommit: CexFinalPrecommitV1 =
            serde_json::from_value(precommit_revision.payload.clone())
                .map_err(serialization_error)?;
        precommit.validate().map_err(domain_error)?;
        if precommit_revision.registry_kind != CEX_FINAL_PRECOMMIT_REGISTRY_KIND
            || precommit_revision.asset_id != claim.mission_id
            || canonical_json_hash(&precommit).map_err(domain_error)?
                != claim.precommit.content_sha256
            || precommit.final_candidate != claim.candidate
            || precommit.evaluation_protocol != claim.evaluation_protocol
            || precommit.holdout_id != claim.holdout_id
        {
            return Err(StoreError::Domain(
                "sealed holdout claim does not match the final precommit".to_string(),
            ));
        }
        let sealed_id =
            sealed_evaluation_revision_id(&claim.candidate.id, SEALED_HOLDOUT_EVALUATOR_VERSION);
        match self.get_registry_revision(&sealed_id) {
            Ok(existing) => {
                validate_cex_sealed_revision(&existing, claim, &precommit)?;
                return Ok(Some(existing));
            }
            Err(StoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let claim_revision = RegistryRevision {
            revision_id: claim.claim_id.clone(),
            registry_kind: CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND.to_string(),
            asset_id: claim.mission_id.clone(),
            parent_revision_id: Some(claim.precommit.id.clone()),
            payload: serde_json::to_value(claim).map_err(serialization_error)?,
            created_at: at,
        };
        match self.put_registry_revision(&claim_revision) {
            Ok(()) => Ok(None),
            Err(StoreError::DuplicateRecord) => {
                let existing_claim = self.get_registry_revision(&claim.claim_id)?;
                let stored: CexSealedHoldoutClaimV1 =
                    serde_json::from_value(existing_claim.payload.clone())
                        .map_err(serialization_error)?;
                if existing_claim.registry_kind != CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND
                    || stored != *claim
                {
                    return Err(StoreError::Domain(
                        "sealed holdout opening conflicts with an existing claim".to_string(),
                    ));
                }
                match self.get_registry_revision(&sealed_id) {
                    Ok(existing) => {
                        validate_cex_sealed_revision(&existing, claim, &precommit)?;
                        Ok(Some(existing))
                    }
                    Err(StoreError::NotFound) => Err(StoreError::Domain(
                        "sealed holdout is already claimed; concurrent or incomplete opening fails closed"
                            .to_string(),
                    )),
                    Err(error) => Err(error),
                }
            }
            Err(error) => Err(error),
        }
    }

    pub fn has_cex_sealed_holdout_claim(&self, holdout_id: &str) -> Result<bool, StoreError> {
        let revisions: Vec<RegistryRevision> = read_json_rows(
            &self.connection,
            "SELECT payload_json, content_hash FROM registry_revisions WHERE registry_kind = ?",
            CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND,
        )?;
        Ok(revisions.iter().any(|revision| {
            revision
                .payload
                .get("holdout_id")
                .and_then(serde_json::Value::as_str)
                == Some(holdout_id)
        }))
    }

    pub fn put_cex_sealed_evaluation(
        &mut self,
        claim: &CexSealedHoldoutClaimV1,
        evaluation: &CandidateEvaluation,
        at: DateTime<Utc>,
    ) -> Result<RegistryRevision, StoreError> {
        claim.validate().map_err(domain_error)?;
        evaluation.validate().map_err(domain_error)?;
        let (_, protocol_hash) = evaluation.protocol_binding().map_err(domain_error)?;
        let claim_revision = self.get_registry_revision(&claim.claim_id)?;
        let stored_claim: CexSealedHoldoutClaimV1 =
            serde_json::from_value(claim_revision.payload.clone()).map_err(serialization_error)?;
        let precommit_revision = self.get_registry_revision(&claim.precommit.id)?;
        let precommit: CexFinalPrecommitV1 =
            serde_json::from_value(precommit_revision.payload.clone())
                .map_err(serialization_error)?;
        let mission = self.get_mission(&claim.mission_id)?;
        if claim_revision.registry_kind != CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND
            || stored_claim != *claim
            || evaluation.evaluator_version != SEALED_HOLDOUT_EVALUATOR_VERSION
            || protocol_hash != claim.evaluation_protocol.content_sha256
            || evaluation.formula_config().map_err(domain_error)?
                != FormulaEvaluatorConfig::for_mission(&mission).map_err(domain_error)?
        {
            return Err(StoreError::Domain(
                "sealed evaluation does not match its unique access claim".to_string(),
            ));
        }
        let revision = RegistryRevision {
            revision_id: sealed_evaluation_revision_id(
                &claim.candidate.id,
                SEALED_HOLDOUT_EVALUATOR_VERSION,
            ),
            registry_kind: "sealed_evaluation".to_string(),
            asset_id: claim.candidate.id.clone(),
            parent_revision_id: Some(claim.claim_id.clone()),
            payload: serde_json::json!({
                "mission_id": claim.mission_id,
                "candidate_content_hash": claim.candidate.content_sha256,
                "dataset_manifest_id": precommit.dataset_manifest_id,
                "evaluation_protocol_hash": claim.evaluation_protocol.content_sha256,
                "precommit_id": claim.precommit.id,
                "precommit_content_hash": claim.precommit.content_sha256,
                "sealed_access_claim_id": claim.claim_id,
                "holdout_id": claim.holdout_id,
                "evaluation": evaluation,
            }),
            created_at: at,
        };
        match self.get_registry_revision(&revision.revision_id) {
            Ok(existing) => {
                validate_cex_sealed_revision(&existing, claim, &precommit)?;
                if existing == revision {
                    Ok(existing)
                } else {
                    Err(StoreError::Domain(
                        "sealed evaluation conflicts with the existing receipt".to_string(),
                    ))
                }
            }
            Err(StoreError::NotFound) => {
                self.put_registry_revision(&revision)?;
                Ok(revision)
            }
            Err(error) => Err(error),
        }
    }

    pub fn bind_mission_evaluation_protocol(
        &mut self,
        mission_id: &str,
        has_legacy_history: bool,
        protocol: &EvaluationProtocolV1,
        at: DateTime<Utc>,
    ) -> Result<String, StoreError> {
        self.get_mission(mission_id)?;
        let evaluation_protocol_hash = protocol.content_hash().map_err(domain_error)?;
        let revision_id = mission_evaluation_protocol_revision_id(mission_id);
        match self.get_registry_revision(&revision_id) {
            Ok(revision) => {
                let binding = validate_mission_evaluation_protocol_revision(&revision, mission_id)?;
                if binding.evaluation_protocol != *protocol
                    || binding.evaluation_protocol_hash != evaluation_protocol_hash
                {
                    return Err(StoreError::Domain(
                        "mission evaluation protocol drift is not allowed".to_string(),
                    ));
                }
            }
            Err(StoreError::NotFound) => {
                let binding = MissionEvaluationProtocolBinding {
                    evaluation_protocol: protocol.clone(),
                    evaluation_protocol_hash: evaluation_protocol_hash.clone(),
                    legacy_history_unbound: has_legacy_history,
                };
                self.put_registry_revision(&RegistryRevision {
                    revision_id,
                    registry_kind: MISSION_EVALUATION_PROTOCOL_KIND.to_string(),
                    asset_id: mission_id.to_string(),
                    parent_revision_id: None,
                    payload: serde_json::to_value(binding).map_err(serialization_error)?,
                    created_at: at,
                })?;
            }
            Err(error) => return Err(error),
        }
        Ok(evaluation_protocol_hash)
    }

    pub fn require_mission_evaluation_protocol(
        &self,
        mission_id: &str,
        protocol: &EvaluationProtocolV1,
    ) -> Result<String, StoreError> {
        let binding = mission_evaluation_protocol_binding(&self.connection, mission_id)?;
        let requested_hash = protocol.content_hash().map_err(domain_error)?;
        if binding.evaluation_protocol != *protocol
            || binding.evaluation_protocol_hash != requested_hash
        {
            return Err(StoreError::Domain(
                "mission evaluation protocol drift is not allowed".to_string(),
            ));
        }
        Ok(requested_hash)
    }

    fn canonical_walk_forward_protocol_hash(
        &self,
        mission: &ResearchMission,
        candidate_id: &str,
        candidate_iteration: &ResearchIteration,
        candidate_artifact: &CandidateArtifact,
    ) -> Result<Option<String>, StoreError> {
        let expected_version = match candidate_artifact {
            CandidateArtifact::Formula(_) | CandidateArtifact::CexFourStage(_) => {
                WALK_FORWARD_EVALUATOR_VERSION
            }
            CandidateArtifact::OnnxModel(_) => ONNX_WALK_FORWARD_EVALUATOR_VERSION,
            _ => return Ok(None),
        };
        let Some(evaluation_id) = candidate_iteration.evaluation_artifact_id.as_deref() else {
            return Ok(None);
        };
        let stored = match read_json_row::<EvaluationRecord>(
            &self.connection,
            "SELECT payload_json, content_hash FROM evaluation_artifacts WHERE evaluation_id = ?",
            evaluation_id,
        ) {
            Ok(stored) => stored,
            Err(StoreError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let Ok(evaluation) = serde_json::from_value::<CandidateEvaluation>(stored.payload.clone())
        else {
            return Ok(None);
        };
        if evaluation.validate().is_err() {
            return Ok(None);
        }
        let (_, protocol_hash) = evaluation.protocol_binding().map_err(domain_error)?;
        let expected_config = FormulaEvaluatorConfig::for_mission(mission).map_err(domain_error)?;
        Ok((candidate_iteration.mission_id == mission.mission_id
            && candidate_iteration.candidate_artifact_id.as_deref() == Some(candidate_id)
            && candidate_iteration.verdict == IterationVerdict::Keep
            && stored.mission_id == mission.mission_id
            && stored.candidate_id == candidate_id
            && stored.evaluation_id == evaluation_id
            && stored.dataset_manifest_id == mission.dataset_manifest_id.as_str()
            && stored.evaluation_protocol_hash == protocol_hash
            && evaluation.passed
            && evaluation.evaluator_version == expected_version
            && evaluation.formula_config().map_err(domain_error)? == expected_config)
            .then(|| protocol_hash.to_string()))
    }

    pub fn promote_candidate(
        &mut self,
        bundle: &StrategyBundle,
        promotion: &PromotionRecord,
    ) -> Result<StoredPromotion, StoreError> {
        bundle.validate().map_err(domain_error)?;
        promotion.validate(bundle).map_err(domain_error)?;

        let (candidate_mission_id, candidate_iteration_id, candidate_json, candidate_hash) = self
            .connection
            .query_row(
                "SELECT mission_id, iteration_id, payload_json, content_hash FROM candidate_artifacts WHERE candidate_id = ?",
                params![bundle.candidate_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
            )
            .map_err(map_query_error)?;
        verify_hash(&candidate_json, &candidate_hash)?;
        let candidate_artifact: CandidateArtifact =
            serde_json::from_str(&candidate_json).map_err(serialization_error)?;
        let expected_bundle_artifact = candidate_artifact
            .to_governed_strategy_bundle_artifact()
            .map_err(domain_error)?;
        if candidate_mission_id != promotion.mission_id
            || candidate_hash != bundle.candidate_content_hash
            || expected_bundle_artifact != bundle.artifact
        {
            return Err(StoreError::Domain(
                "promotion candidate binding does not match stored truth".to_string(),
            ));
        }
        let candidate_iteration: ResearchIteration = read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM iterations WHERE iteration_id = ?",
            &candidate_iteration_id,
        )?;
        if candidate_iteration.mission_id != promotion.mission_id
            || candidate_iteration.candidate_artifact_id.as_deref()
                != Some(bundle.candidate_id.as_str())
            || candidate_iteration.engine == EngineKind::OfflineReinforcementLearning
        {
            return Err(StoreError::Domain(
                "candidate provenance is not eligible for promotion".to_string(),
            ));
        }
        let mission = self.get_mission(&promotion.mission_id)?;
        if mission.dataset_manifest_id != bundle.dataset_manifest_id {
            return Err(StoreError::Domain(
                "promotion dataset does not match mission".to_string(),
            ));
        }
        let Some(walk_forward_protocol_hash) = self.canonical_walk_forward_protocol_hash(
            &mission,
            &bundle.candidate_id,
            &candidate_iteration,
            &candidate_artifact,
        )?
        else {
            return Err(StoreError::Domain(
                "promotion candidate lacks canonical walk-forward evidence".to_string(),
            ));
        };
        let mission_protocol_hash =
            mission_evaluation_protocol_binding(&self.connection, &promotion.mission_id)?
                .evaluation_protocol_hash;
        if mission_protocol_hash != walk_forward_protocol_hash
            || mission_protocol_hash != bundle.evaluation_protocol_hash
            || mission_protocol_hash != promotion.evaluation_protocol_hash
        {
            return Err(StoreError::Domain(
                "promotion evaluation protocol does not match the immutable mission binding"
                    .to_string(),
            ));
        }
        let expected_sealed_evaluation_id =
            sealed_evaluation_revision_id(&bundle.candidate_id, &bundle.evaluator_version);
        if promotion.sealed_evaluation_id != expected_sealed_evaluation_id {
            return Err(StoreError::Domain(
                "promotion sealed evaluation revision id is not canonical".to_string(),
            ));
        }
        let sealed = self.get_registry_revision(&promotion.sealed_evaluation_id)?;
        let sealed_evaluation = sealed
            .payload
            .get("evaluation")
            .ok_or_else(|| StoreError::Domain("sealed evaluation payload is incomplete".into()))?;
        let typed_evaluation: CandidateEvaluation =
            serde_json::from_value(sealed_evaluation.clone()).map_err(serialization_error)?;
        typed_evaluation.validate().map_err(domain_error)?;
        let (_, sealed_protocol_hash) =
            typed_evaluation.protocol_binding().map_err(domain_error)?;
        let expected_evaluator_config =
            FormulaEvaluatorConfig::for_mission(&mission).map_err(domain_error)?;
        let evaluator_matches_artifact = match &candidate_artifact {
            CandidateArtifact::Formula(_) | CandidateArtifact::CexFourStage(_) => {
                typed_evaluation.evaluator_version == SEALED_HOLDOUT_EVALUATOR_VERSION
            }
            CandidateArtifact::OnnxModel(_) => {
                typed_evaluation.evaluator_version == ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
            }
            _ => false,
        };
        if !typed_evaluation.passed
            || !evaluator_matches_artifact
            || typed_evaluation.formula_config().map_err(domain_error)? != expected_evaluator_config
        {
            return Err(StoreError::Domain(
                "sealed evaluation does not use the governed evaluator policy".to_string(),
            ));
        }
        let stored_candidate_hash = sealed
            .payload
            .get("candidate_content_hash")
            .and_then(serde_json::Value::as_str);
        let stored_mission_id = sealed
            .payload
            .get("mission_id")
            .and_then(serde_json::Value::as_str);
        let stored_dataset = sealed
            .payload
            .get("dataset_manifest_id")
            .and_then(serde_json::Value::as_str);
        let stored_evaluator_version = sealed_evaluation
            .get("evaluator_version")
            .and_then(serde_json::Value::as_str);
        let stored_protocol_hash = sealed
            .payload
            .get("evaluation_protocol_hash")
            .and_then(serde_json::Value::as_str);
        let stored_evaluator_config = sealed_evaluation
            .get("evaluator_config")
            .ok_or_else(|| StoreError::Domain("sealed evaluator config is missing".into()))?;
        let stored_evaluation_metrics = sealed_evaluation
            .get("metrics")
            .ok_or_else(|| StoreError::Domain("sealed evaluation metrics are missing".into()))?;
        let evaluator_config_hash =
            canonical_json_hash(stored_evaluator_config).map_err(domain_error)?;
        let evaluation_metrics_hash =
            canonical_json_hash(stored_evaluation_metrics).map_err(domain_error)?;
        let evaluation_hash = canonical_json_hash(sealed_evaluation).map_err(domain_error)?;
        if stored_protocol_hash != Some(sealed_protocol_hash)
            || sealed_protocol_hash != walk_forward_protocol_hash.as_str()
            || sealed_protocol_hash != bundle.evaluation_protocol_hash.as_str()
        {
            return Err(StoreError::Domain(
                "promotion evaluation protocol does not match walk-forward and sealed evidence"
                    .to_string(),
            ));
        }
        if sealed.registry_kind != "sealed_evaluation"
            || sealed.asset_id != bundle.candidate_id
            || stored_mission_id != Some(promotion.mission_id.as_str())
            || stored_candidate_hash != Some(bundle.candidate_content_hash.as_str())
            || stored_dataset != Some(bundle.dataset_manifest_id.as_str())
            || stored_evaluator_version != Some(bundle.evaluator_version.as_str())
            || evaluator_config_hash != bundle.evaluator_config_hash
            || evaluation_metrics_hash != bundle.evaluation_metrics_hash
            || evaluation_hash != bundle.sealed_evaluation_hash
        {
            return Err(StoreError::Domain(
                "promotion evidence does not match sealed evaluation".to_string(),
            ));
        }
        if let CandidateArtifact::CexFourStage(strategy) = &candidate_artifact {
            let precommit_revision = self.get_registry_revision(&strategy.precommit_id)?;
            let precommit: CexFinalPrecommitV1 =
                serde_json::from_value(precommit_revision.payload.clone())
                    .map_err(serialization_error)?;
            precommit.validate().map_err(domain_error)?;
            validate_cex_precommit_dependencies(&self.connection, &precommit, strategy)?;
            let claim =
                CexSealedHoldoutClaimV1::from_precommit(&precommit).map_err(domain_error)?;
            let claim_revision = self.get_registry_revision(&claim.claim_id)?;
            let stored_claim: CexSealedHoldoutClaimV1 =
                serde_json::from_value(claim_revision.payload.clone())
                    .map_err(serialization_error)?;
            if precommit_revision.registry_kind != CEX_FINAL_PRECOMMIT_REGISTRY_KIND
                || precommit_revision.asset_id != promotion.mission_id
                || precommit.final_candidate.id != bundle.candidate_id
                || precommit.final_candidate.content_sha256 != bundle.candidate_content_hash
                || precommit.dataset_manifest_id != bundle.dataset_manifest_id
                || precommit.evaluation_protocol.content_sha256 != bundle.evaluation_protocol_hash
                || precommit.four_stage_strategy.id != strategy.strategy_artifact_id
                || precommit.four_stage_strategy.content_sha256 != strategy.strategy_artifact_sha256
                || claim_revision.registry_kind != CEX_SEALED_HOLDOUT_CLAIM_REGISTRY_KIND
                || stored_claim != claim
            {
                return Err(StoreError::Domain(
                    "CEX promotion does not match its final precommit and access claim".to_string(),
                ));
            }
            validate_cex_sealed_revision(&sealed, &claim, &precommit)?;
        }

        let (bundle_json, bundle_content_hash) = encoded(bundle)?;
        let (promotion_json, promotion_content_hash) = encoded(promotion)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        ensure_absent(
            &transaction,
            "strategy_bundles",
            "bundle_id",
            &bundle.bundle_id,
        )?;
        ensure_absent(
            &transaction,
            "promotions",
            "promotion_id",
            &promotion.promotion_id,
        )?;
        transaction
            .execute(
                "INSERT INTO strategy_bundles VALUES (?, ?, ?, ?, ?)",
                params![
                    bundle.bundle_id,
                    bundle.candidate_id,
                    bundle_json,
                    bundle_content_hash,
                    bundle.created_at.to_rfc3339()
                ],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO promotions VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    promotion.promotion_id,
                    promotion.mission_id,
                    promotion.candidate_id,
                    promotion.bundle_id,
                    promotion_json,
                    promotion_content_hash,
                    promotion.created_at.to_rfc3339()
                ],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            Some(&promotion.mission_id),
            "strategy_bundle_added",
            &bundle.bundle_id,
            &bundle.bundle_hash,
            bundle.created_at,
        )?;
        append_journal(
            &transaction,
            Some(&promotion.mission_id),
            "candidate_promoted",
            &promotion.promotion_id,
            &promotion_content_hash,
            promotion.created_at,
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(StoredPromotion {
            record: promotion.clone(),
            content_hash: promotion_content_hash,
        })
    }

    pub fn get_strategy_bundle(&self, bundle_id: &str) -> Result<StrategyBundle, StoreError> {
        let bundle: StrategyBundle = read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM strategy_bundles WHERE bundle_id = ?",
            bundle_id,
        )?;
        bundle.validate_for_readback().map_err(domain_error)?;
        Ok(bundle)
    }

    pub fn get_canonical_strategy_bundle(
        &self,
        bundle_id: &str,
    ) -> Result<StrategyBundle, StoreError> {
        let bundle = self.get_strategy_bundle(bundle_id)?;
        bundle.validate().map_err(domain_error)?;
        Ok(bundle)
    }

    pub fn get_promotion(&self, promotion_id: &str) -> Result<StoredPromotion, StoreError> {
        let (record, content_hash): (PromotionRecord, String) = read_json_row_with_hash(
            &self.connection,
            "SELECT payload_json, content_hash FROM promotions WHERE promotion_id = ?",
            promotion_id,
        )?;
        let bundle = self.get_strategy_bundle(&record.bundle_id)?;
        record
            .validate_for_readback(&bundle)
            .map_err(domain_error)?;
        Ok(StoredPromotion {
            record,
            content_hash,
        })
    }

    pub fn get_canonical_promotion(
        &self,
        promotion_id: &str,
    ) -> Result<StoredPromotion, StoreError> {
        let promotion = self.get_promotion(promotion_id)?;
        let bundle = self.get_canonical_strategy_bundle(&promotion.record.bundle_id)?;
        promotion.record.validate(&bundle).map_err(domain_error)?;
        let mission_protocol_hash =
            mission_evaluation_protocol_binding(&self.connection, &promotion.record.mission_id)?
                .evaluation_protocol_hash;
        if mission_protocol_hash != promotion.record.evaluation_protocol_hash {
            return Err(StoreError::Domain(
                "promotion does not match the immutable mission evaluation protocol".to_string(),
            ));
        }
        Ok(promotion)
    }

    pub fn validate_deployment_binding(
        &self,
        envelope: &DeploymentEnvelope,
    ) -> Result<(StoredPromotion, StrategyBundle), StoreError> {
        let promotion = self.get_canonical_promotion(&envelope.promotion_id)?;
        let bundle = self.get_canonical_strategy_bundle(&envelope.bundle_id)?;
        if promotion.record.bundle_id != envelope.bundle_id
            || promotion.record.bundle_hash != envelope.bundle_hash
            || promotion.record.candidate_id != envelope.asset_revision_id
            || promotion.content_hash != envelope.promotion_manifest_hash
            || bundle.bundle_hash != envelope.bundle_hash
        {
            return Err(StoreError::Domain(
                "deployment envelope does not match persisted promotion and bundle".to_string(),
            ));
        }
        Ok((promotion, bundle))
    }

    pub fn put_search_policy_revision(
        &mut self,
        mut revision: SearchPolicyRevision,
    ) -> Result<SearchPolicyRevision, StoreError> {
        revision.validate().map_err(domain_error)?;
        revision.adopted = match revision.parent_revision_id.as_deref() {
            Some(parent_id) => {
                let parent = self.get_search_policy_revision(parent_id)?;
                parent.adopted && revision.validator_score > parent.validator_score
            }
            None => true,
        };
        if !revision.adopted && revision.rollback_reason.is_none() {
            revision.rollback_reason = Some("validator did not beat parent revision".to_string());
        }
        self.put_registry_revision(&RegistryRevision {
            revision_id: revision.revision_id.clone(),
            registry_kind: "search_policy".to_string(),
            asset_id: "alpha-search-policy".to_string(),
            parent_revision_id: revision.parent_revision_id.clone(),
            payload: serde_json::to_value(&revision).map_err(serialization_error)?,
            created_at: revision.created_at,
        })?;
        Ok(revision)
    }

    pub fn get_search_policy_revision(
        &self,
        revision_id: &str,
    ) -> Result<SearchPolicyRevision, StoreError> {
        let revision = self.get_registry_revision(revision_id)?;
        if revision.registry_kind != "search_policy" {
            return Err(StoreError::Domain(
                "registry revision is not a search policy".to_string(),
            ));
        }
        serde_json::from_value(revision.payload).map_err(serialization_error)
    }

    pub fn find_adopted_search_policy_child(
        &self,
        parent_revision_id: &str,
        evidence_event_ids: &[String],
    ) -> Result<Option<SearchPolicyRevision>, StoreError> {
        if evidence_event_ids.is_empty() {
            return Ok(None);
        }
        let revisions: Vec<RegistryRevision> = read_json_rows(
            &self.connection,
            "SELECT payload_json, content_hash FROM registry_revisions
             WHERE registry_kind = 'search_policy' AND parent_revision_id = ?
             ORDER BY created_at DESC, revision_id DESC",
            parent_revision_id,
        )?;
        for record in revisions {
            let revision: SearchPolicyRevision =
                serde_json::from_value(record.payload).map_err(serialization_error)?;
            if revision.adopted
                && revision
                    .evidence_event_ids
                    .iter()
                    .any(|event_id| evidence_event_ids.contains(event_id))
            {
                return Ok(Some(revision));
            }
        }
        Ok(None)
    }

    pub fn append_memory(&mut self, record: &MemoryRecord) -> Result<(), StoreError> {
        require_text(&record.event_id)?;
        self.insert_json_record(
            "research_memory",
            &record.event_id,
            record,
            (
                record.mission_id.as_deref(),
                "research_memory_added",
                record.created_at,
            ),
            |transaction, json, hash| {
                transaction.execute(
                    "INSERT INTO research_memory VALUES (?, ?, ?, ?, ?)",
                    params![
                        record.event_id,
                        record.mission_id,
                        json,
                        hash,
                        record.created_at.to_rfc3339()
                    ],
                )?;
                Ok(())
            },
        )
    }

    pub fn get_memory(&self, event_id: &str) -> Result<MemoryRecord, StoreError> {
        read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM research_memory WHERE event_id = ?",
            event_id,
        )
    }

    pub fn append_memory_idempotent(&mut self, record: &MemoryRecord) -> Result<bool, StoreError> {
        match self.get_memory(&record.event_id) {
            Ok(existing) if existing == *record => Ok(false),
            Ok(_) => Err(StoreError::DuplicateRecord),
            Err(StoreError::NotFound) => {
                self.append_memory(record)?;
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    pub fn append_learning_directive(
        &mut self,
        directive: &LearningDirective,
    ) -> Result<bool, StoreError> {
        directive.validate().map_err(domain_error)?;
        self.append_memory_idempotent(&MemoryRecord {
            event_id: directive.directive_id.clone(),
            mission_id: Some(directive.mission_id.clone()),
            payload: serde_json::json!({
                "kind": "learning_directive",
                "directive": directive,
            }),
            created_at: directive.created_at,
        })
    }

    pub fn ingest_runtime_attribution(
        &mut self,
        event: VerifiedRuntimeAttributionEvent,
    ) -> Result<bool, StoreError> {
        Ok(self.ingest_runtime_attributions(vec![event])? == 1)
    }

    pub fn ingest_runtime_attributions(
        &mut self,
        events: Vec<VerifiedRuntimeAttributionEvent>,
    ) -> Result<usize, StoreError> {
        if events.is_empty() {
            return Err(StoreError::Domain(
                "runtime attribution batch cannot be empty".to_string(),
            ));
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        let mut inserted = 0_usize;
        for event in events {
            inserted += usize::from(ingest_runtime_attribution(
                &transaction,
                event.into_event(),
            )?);
        }
        transaction.commit().map_err(database_error)?;
        Ok(inserted)
    }

    pub fn get_runtime_attribution(
        &self,
        event_id: &str,
    ) -> Result<RuntimeAttributionEvent, StoreError> {
        let memory = self.get_memory(event_id)?;
        serde_json::from_value(
            memory
                .payload
                .get("event")
                .cloned()
                .ok_or_else(|| StoreError::Domain("memory is not attribution".to_string()))?,
        )
        .map_err(serialization_error)
    }

    pub fn runtime_attributions_for_mission(
        &self,
        mission_id: &str,
    ) -> Result<Vec<RuntimeAttributionEvent>, StoreError> {
        let records: Vec<MemoryRecord> = read_json_rows(
            &self.connection,
            "SELECT payload_json, content_hash FROM research_memory
             WHERE mission_id = ? ORDER BY created_at, event_id",
            mission_id,
        )?;
        records
            .into_iter()
            .filter(|record| {
                record
                    .payload
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    == Some("runtime_attribution")
            })
            .map(|record| {
                serde_json::from_value(record.payload.get("event").cloned().ok_or_else(|| {
                    StoreError::Domain("runtime attribution memory is malformed".to_string())
                })?)
                .map_err(serialization_error)
            })
            .collect()
    }

    pub fn sealed_passed_candidate_for_mission(
        &self,
        mission_id: &str,
    ) -> Result<Option<String>, StoreError> {
        require_text(mission_id)?;
        let mission = self.get_mission(mission_id)?;
        let revisions = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT payload_json, content_hash FROM registry_revisions
                     WHERE registry_kind = 'sealed_evaluation'
                     ORDER BY created_at DESC, revision_id DESC",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(database_error)?;
            let mut revisions = Vec::new();
            for row in rows {
                let (json, hash) = row.map_err(database_error)?;
                verify_hash(&json, &hash)?;
                revisions.push(
                    serde_json::from_str::<RegistryRevision>(&json).map_err(serialization_error)?,
                );
            }
            revisions
        };
        for revision in revisions {
            if revision
                .payload
                .get("mission_id")
                .and_then(serde_json::Value::as_str)
                != Some(mission_id)
            {
                continue;
            }
            let Some(evaluation_value) = revision.payload.get("evaluation") else {
                continue;
            };
            let Some(evaluator_version) = evaluation_value
                .get("evaluator_version")
                .and_then(serde_json::Value::as_str)
            else {
                continue;
            };
            if !matches!(
                evaluator_version,
                SEALED_HOLDOUT_EVALUATOR_VERSION | ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
            ) || revision.revision_id
                != sealed_evaluation_revision_id(&revision.asset_id, evaluator_version)
            {
                continue;
            }
            let evaluation: CandidateEvaluation =
                serde_json::from_value(evaluation_value.clone()).map_err(serialization_error)?;
            if evaluation.validate().is_err() {
                continue;
            }
            let (_, sealed_protocol_hash) = evaluation.protocol_binding().map_err(domain_error)?;
            if !evaluation.passed
                || evaluation.formula_config().map_err(domain_error)?
                    != FormulaEvaluatorConfig::for_mission(&mission).map_err(domain_error)?
            {
                continue;
            }
            let candidate = self
                .connection
                .query_row(
                    "SELECT iteration_id, payload_json, content_hash FROM candidate_artifacts
                     WHERE candidate_id = ? AND mission_id = ?",
                    params![revision.asset_id, mission_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(map_query_error);
            let (candidate_iteration_id, candidate_json, candidate_hash) = match candidate {
                Ok(candidate) => candidate,
                Err(StoreError::NotFound) => continue,
                Err(error) => return Err(error),
            };
            verify_hash(&candidate_json, &candidate_hash)?;
            let candidate_artifact: CandidateArtifact =
                serde_json::from_str(&candidate_json).map_err(serialization_error)?;
            let evaluator_matches_artifact = matches!(
                (&candidate_artifact, evaluator_version),
                (
                    CandidateArtifact::Formula(_),
                    SEALED_HOLDOUT_EVALUATOR_VERSION
                ) | (
                    CandidateArtifact::CexFourStage(_),
                    SEALED_HOLDOUT_EVALUATOR_VERSION
                ) | (
                    CandidateArtifact::OnnxModel(_),
                    ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
                )
            );
            let candidate_iteration: ResearchIteration = read_json_row(
                &self.connection,
                "SELECT payload_json, content_hash FROM iterations WHERE iteration_id = ?",
                &candidate_iteration_id,
            )?;
            let walk_forward_protocol_hash = self.canonical_walk_forward_protocol_hash(
                &mission,
                &revision.asset_id,
                &candidate_iteration,
                &candidate_artifact,
            )?;
            if candidate_iteration.engine == EngineKind::OfflineReinforcementLearning
                || candidate_iteration.candidate_artifact_id.as_deref()
                    != Some(revision.asset_id.as_str())
                || walk_forward_protocol_hash.as_deref() != Some(sealed_protocol_hash)
                || revision
                    .payload
                    .get("evaluation_protocol_hash")
                    .and_then(serde_json::Value::as_str)
                    != Some(sealed_protocol_hash)
            {
                continue;
            }
            let candidate_hash_matches = revision
                .payload
                .get("candidate_content_hash")
                .and_then(serde_json::Value::as_str)
                == Some(candidate_hash.as_str());
            let dataset_matches = revision
                .payload
                .get("dataset_manifest_id")
                .and_then(serde_json::Value::as_str)
                == Some(mission.dataset_manifest_id.as_str());
            if candidate_hash_matches && dataset_matches && evaluator_matches_artifact {
                return Ok(Some(revision.asset_id));
            }
        }
        Ok(None)
    }

    pub fn live_small_eligibility_approval(
        &self,
        mission_id: &str,
        candidate_id: &str,
        at: DateTime<Utc>,
    ) -> Result<Option<String>, StoreError> {
        require_text(mission_id)?;
        require_text(candidate_id)?;
        let promotion = read_json_row::<PromotionRecord>(
            &self.connection,
            "SELECT payload_json, content_hash FROM promotions
             WHERE candidate_id = ? ORDER BY created_at DESC LIMIT 1",
            candidate_id,
        );
        let promotion = match promotion {
            Ok(promotion)
                if promotion.candidate_id == candidate_id && promotion.mission_id == mission_id =>
            {
                promotion
            }
            Ok(_) | Err(StoreError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let bundle = self.get_strategy_bundle(&promotion.bundle_id)?;
        promotion.validate(&bundle).map_err(domain_error)?;
        if mission_evaluation_protocol_binding(&self.connection, mission_id)?
            .evaluation_protocol_hash
            != promotion.evaluation_protocol_hash
        {
            return Ok(None);
        }

        let approvals = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT approval_id FROM approvals
                     WHERE approval_class = 'human_live_small' AND subject_id = ?
                     ORDER BY created_at DESC, approval_id DESC",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map(params![promotion.promotion_id], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(database_error)?;
            let mut approvals = Vec::new();
            for row in rows {
                approvals.push(row.map_err(database_error)?);
            }
            approvals
        };
        for approval_id in approvals {
            let approval = self.get_approval(&approval_id)?;
            if approval.validate().is_err() || !approval.is_active_at(at) {
                continue;
            }
            let Some(payload) = approval.payload.get("eligibility") else {
                continue;
            };
            let Ok(evidence) =
                serde_json::from_value::<LiveSmallEligibilityEvidence>(payload.clone())
            else {
                continue;
            };
            if evidence.validate().is_err() {
                continue;
            }
            if evidence.candidate_id == candidate_id && evidence.bundle_id == bundle.bundle_id {
                return Ok(Some(approval.approval_id));
            }
        }
        Ok(None)
    }

    pub fn record_approval(&mut self, approval: &ApprovalRecord) -> Result<(), StoreError> {
        approval.validate()?;
        if approval.revoked_at.is_some() {
            return Err(StoreError::Domain(
                "record the original approval, then append its revocation".to_string(),
            ));
        }
        self.insert_json_record(
            "approvals",
            &approval.approval_id,
            approval,
            (None, "approval_recorded", approval.created_at),
            |transaction, json, hash| {
                transaction.execute(
                    "INSERT INTO approvals (
                        approval_id, approval_class, subject_id, payload_json, content_hash,
                        created_at, signer_id, valid_from, expires_at, revoked_at, revoked_by,
                        revocation_reason
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        approval.approval_id,
                        approval.approval_class,
                        approval.subject_id,
                        json,
                        hash,
                        approval.created_at.to_rfc3339(),
                        approval.signer_id,
                        approval.valid_from.map(|value| value.to_rfc3339()),
                        approval.expires_at.map(|value| value.to_rfc3339()),
                        approval.revoked_at.map(|value| value.to_rfc3339()),
                        approval.revoked_by,
                        approval.revocation_reason,
                    ],
                )?;
                Ok(())
            },
        )
    }

    pub fn store_deployment(
        &mut self,
        signed: &SignedDeploymentEnvelope,
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let id = &signed.envelope.deployment_id;
        require_text(id)?;
        self.validate_deployment_binding(&signed.envelope)?;
        self.insert_json_record(
            "deployment_envelopes",
            id,
            signed,
            (None, "deployment_stored", at),
            |transaction, json, hash| {
                transaction.execute(
                    "INSERT INTO deployment_envelopes VALUES (?, ?, ?, ?)",
                    params![id, json, hash, at.to_rfc3339()],
                )?;
                Ok(())
            },
        )
    }

    pub fn get_deployment(
        &self,
        deployment_id: &str,
    ) -> Result<SignedDeploymentEnvelope, StoreError> {
        read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM deployment_envelopes WHERE deployment_id = ?",
            deployment_id,
        )
    }

    pub fn consume_nonce(&mut self, nonce: &str, at: DateTime<Utc>) -> Result<(), StoreError> {
        require_text(nonce)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        if record_exists(&transaction, "consumed_nonces", "nonce", nonce)? {
            return Err(StoreError::NonceReplay);
        }
        transaction
            .execute(
                "INSERT INTO consumed_nonces VALUES (?, ?)",
                params![nonce, at.to_rfc3339()],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub fn nonce_consumed(&self, nonce: &str) -> Result<bool, StoreError> {
        record_exists(&self.connection, "consumed_nonces", "nonce", nonce)
    }

    fn insert_json_record<T, F>(
        &mut self,
        table: &str,
        id: &str,
        record: &T,
        journal: (Option<&str>, &str, DateTime<Utc>),
        insert: F,
    ) -> Result<(), StoreError>
    where
        T: Serialize,
        F: FnOnce(&Transaction<'_>, String, String) -> Result<(), duckdb::Error>,
    {
        let (json, hash) = encoded(record)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let id_column = match table {
            "registry_revisions" => "revision_id",
            "research_memory" => "event_id",
            "approvals" => "approval_id",
            "deployment_envelopes" => "deployment_id",
            _ => return Err(StoreError::Database("unsupported record table".to_string())),
        };
        ensure_absent(&transaction, table, id_column, id)?;
        insert(&transaction, json, hash.clone()).map_err(database_error)?;
        append_journal(&transaction, journal.0, journal.1, id, &hash, journal.2)?;
        transaction.commit().map_err(database_error)
    }
}

fn ingest_runtime_attribution(
    transaction: &Transaction<'_>,
    mut event: RuntimeAttributionEvent,
) -> Result<bool, StoreError> {
    event.validate().map_err(domain_error)?;
    let signed: SignedDeploymentEnvelope = read_json_row(
        transaction,
        "SELECT payload_json, content_hash FROM deployment_envelopes WHERE deployment_id = ?",
        &event.deployment_id,
    )?;
    let envelope = &signed.envelope;
    if envelope.asset_revision_id != event.asset_revision_id {
        return Err(StoreError::Domain(
            "attribution asset does not match deployment".to_string(),
        ));
    }
    let required_intent = match event.mode {
        AttributionMode::Paper => AllowedIntentType::StartPaper,
        AttributionMode::Shadow => AllowedIntentType::StartShadow,
        AttributionMode::LiveSmall => AllowedIntentType::StartLiveSmall,
    };
    if !envelope.allowed_intent_types.contains(&required_intent) {
        return Err(StoreError::Domain(
            "attribution mode is not allowed by deployment".to_string(),
        ));
    }

    let mission_id =
        mission_for_asset(transaction, &event.asset_revision_id)?.ok_or_else(|| {
            StoreError::Domain("attribution asset has no canonical mission lineage".to_string())
        })?;
    if event
        .mission_id
        .as_deref()
        .is_some_and(|provided| provided != mission_id)
    {
        return Err(StoreError::Domain(
            "attribution mission does not match deployment lineage".to_string(),
        ));
    }
    event.mission_id = Some(mission_id);
    bind_scope_value(
        &mut event.account_id,
        &envelope.account_id,
        "attribution account",
        false,
    )?;
    bind_scope_value(&mut event.venue, &envelope.venue, "attribution venue", true)?;
    if let Some(symbol) = event.symbol.as_deref() {
        if !envelope.instruments.iter().any(|allowed| allowed == symbol) {
            return Err(StoreError::Domain(
                "attribution symbol is not in deployment scope".to_string(),
            ));
        }
    }

    let bundle: StrategyBundle = read_json_row(
        transaction,
        "SELECT payload_json, content_hash FROM strategy_bundles WHERE bundle_id = ?",
        &envelope.bundle_id,
    )?;
    bundle.validate().map_err(domain_error)?;
    match event.kind {
        AttributionKind::Fill | AttributionKind::Reject | AttributionKind::Cancel => {
            validate_strategy_scope(&event, &bundle, true)?;
        }
        AttributionKind::PortfolioSnapshot => {
            validate_strategy_scope(&event, &bundle, false)?;
        }
        AttributionKind::Activation => {
            if event.strategy_id.is_some() || event.symbol.is_some() || event.order_id.is_some() {
                return Err(StoreError::Domain(
                    "activation attribution must be deployment scoped".to_string(),
                ));
            }
        }
        AttributionKind::StreamGap => {
            if event.strategy_id.is_some() || event.order_id.is_some() {
                return Err(StoreError::Domain(
                    "stream-gap attribution must be deployment scoped".to_string(),
                ));
            }
        }
    }
    event.validate().map_err(domain_error)?;

    let observed_at = event.observed_at;
    append_memory_idempotent(
        transaction,
        &MemoryRecord {
            event_id: event.event_id.clone(),
            mission_id: event.mission_id.clone(),
            payload: serde_json::json!({
                "kind": "runtime_attribution",
                "event": event,
            }),
            created_at: observed_at,
        },
    )
}

fn validate_strategy_scope(
    event: &RuntimeAttributionEvent,
    bundle: &StrategyBundle,
    require_symbol: bool,
) -> Result<(), StoreError> {
    let strategy_id = event.strategy_id.as_deref().ok_or_else(|| {
        StoreError::Domain("strategy-scoped attribution requires strategy_id".to_string())
    })?;
    let symbol = event.symbol.as_deref();
    if require_symbol && symbol.is_none() {
        return Err(StoreError::Domain(
            "order attribution requires symbol".to_string(),
        ));
    }
    let expected = match &bundle.artifact {
        StrategyBundleArtifact::Formula { .. } => {
            let symbol = symbol.ok_or_else(|| {
                StoreError::Domain(
                    "formula strategy attribution requires one instrument".to_string(),
                )
            })?;
            format!("{}:{symbol}", bundle.bundle_id)
        }
        StrategyBundleArtifact::Onnx { .. } => bundle.bundle_id.clone(),
        StrategyBundleArtifact::CexFourStage { .. } => {
            let symbol = symbol.ok_or_else(|| {
                StoreError::Domain(
                    "four-stage CEX strategy attribution requires one instrument".to_string(),
                )
            })?;
            format!("{}:{symbol}", bundle.bundle_id)
        }
    };
    if strategy_id != expected {
        return Err(StoreError::Domain(
            "attribution strategy does not match signed bundle".to_string(),
        ));
    }
    Ok(())
}

fn bind_scope_value(
    value: &mut Option<String>,
    canonical: &str,
    name: &str,
    ascii_case_insensitive: bool,
) -> Result<(), StoreError> {
    if let Some(provided) = value.as_deref() {
        let matches = if ascii_case_insensitive {
            provided.eq_ignore_ascii_case(canonical)
        } else {
            provided == canonical
        };
        if !matches {
            return Err(StoreError::Domain(format!(
                "{name} does not match signed deployment"
            )));
        }
    }
    *value = Some(canonical.to_string());
    Ok(())
}

fn append_memory_idempotent(
    transaction: &Transaction<'_>,
    record: &MemoryRecord,
) -> Result<bool, StoreError> {
    match read_json_row::<MemoryRecord>(
        transaction,
        "SELECT payload_json, content_hash FROM research_memory WHERE event_id = ?",
        &record.event_id,
    ) {
        Ok(existing) if existing == *record => Ok(false),
        Ok(_) => Err(StoreError::DuplicateRecord),
        Err(StoreError::NotFound) => {
            require_text(&record.event_id)?;
            let (json, hash) = encoded(record)?;
            transaction
                .execute(
                    "INSERT INTO research_memory VALUES (?, ?, ?, ?, ?)",
                    params![
                        record.event_id,
                        record.mission_id,
                        json,
                        hash,
                        record.created_at.to_rfc3339(),
                    ],
                )
                .map_err(database_error)?;
            append_journal(
                transaction,
                record.mission_id.as_deref(),
                "research_memory_added",
                &record.event_id,
                &hash,
                record.created_at,
            )?;
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn mission_for_asset(
    connection: &Connection,
    asset_id: &str,
) -> Result<Option<String>, StoreError> {
    let typed = read_json_row::<PromotionRecord>(
        connection,
        "SELECT payload_json, content_hash FROM promotions
         WHERE candidate_id = ? ORDER BY created_at DESC LIMIT 1",
        asset_id,
    );
    match typed {
        Ok(promotion) => return Ok(Some(promotion.mission_id)),
        Err(StoreError::NotFound) => {}
        Err(error) => return Err(error),
    }
    let result = read_json_row::<RegistryRevision>(
        connection,
        "SELECT payload_json, content_hash FROM registry_revisions
         WHERE asset_id = ? AND registry_kind = 'promotion'
         ORDER BY created_at DESC LIMIT 1",
        asset_id,
    );
    match result {
        Ok(revision) => Ok(revision
            .payload
            .get("mission_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)),
        Err(StoreError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

fn validate_iteration_records(
    iteration: &ResearchIteration,
    candidate: Option<(&str, &CandidateArtifact)>,
    evaluation: Option<&EvaluationRecord>,
) -> Result<(), StoreError> {
    iteration.validate().map_err(domain_error)?;
    match (
        &iteration.candidate_artifact_id,
        candidate.as_ref().map(|(id, _)| *id),
    ) {
        (Some(expected), Some(actual)) if expected == actual => {}
        (None, None) => {}
        _ => {
            return Err(StoreError::Domain(
                "iteration candidate id does not match stored artifact".to_string(),
            ))
        }
    }
    match (&iteration.evaluation_artifact_id, evaluation) {
        (Some(expected), Some(actual))
            if expected == &actual.evaluation_id
                && actual.mission_id == iteration.mission_id
                && iteration.candidate_artifact_id.as_deref()
                    == Some(actual.candidate_id.as_str()) =>
        {
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(StoreError::Domain(
            "iteration evaluation does not match stored evidence".to_string(),
        )),
    }
}

fn validate_checkpoint_for_iteration(
    checkpoint: &RunCheckpoint,
    iteration: &ResearchIteration,
) -> Result<(), StoreError> {
    if checkpoint.mission_id != iteration.mission_id
        || checkpoint.last_iteration_id.as_deref() != Some(iteration.iteration_id.as_str())
        || checkpoint.budget_usage != iteration.budget_usage
        || checkpoint.engine_kind != iteration.engine
        || checkpoint.updated_at != iteration.created_at
    {
        return Err(StoreError::CheckpointMismatch);
    }
    Ok(())
}

fn insert_iteration_records(
    transaction: &Transaction<'_>,
    iteration: &ResearchIteration,
    candidate: Option<(&str, &CandidateArtifact)>,
    evaluation: Option<&EvaluationRecord>,
) -> Result<String, StoreError> {
    let (iteration_json, iteration_hash) = encoded(iteration)?;
    ensure_present(transaction, "missions", "mission_id", &iteration.mission_id)?;
    ensure_absent(
        transaction,
        "iterations",
        "iteration_id",
        &iteration.iteration_id,
    )?;
    transaction
        .execute(
            "INSERT INTO iterations VALUES (?, ?, ?, ?, ?, ?)",
            params![
                iteration.iteration_id,
                iteration.mission_id,
                enum_name(&iteration.verdict)?,
                iteration_json,
                iteration_hash,
                iteration.created_at.to_rfc3339()
            ],
        )
        .map_err(database_error)?;
    if let Some((candidate_id, artifact)) = candidate {
        require_text(candidate_id)?;
        let (candidate_json, candidate_hash) = encoded(artifact)?;
        ensure_absent(
            transaction,
            "candidate_artifacts",
            "candidate_id",
            candidate_id,
        )?;
        transaction
            .execute(
                "INSERT INTO candidate_artifacts VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    candidate_id,
                    iteration.mission_id,
                    iteration.iteration_id,
                    candidate_json,
                    candidate_hash,
                    iteration.created_at.to_rfc3339()
                ],
            )
            .map_err(database_error)?;
    }
    if let Some(evaluation) = evaluation {
        require_text(&evaluation.evaluation_id)?;
        let (evaluation_json, evaluation_hash) = encoded(evaluation)?;
        ensure_absent(
            transaction,
            "evaluation_artifacts",
            "evaluation_id",
            &evaluation.evaluation_id,
        )?;
        transaction
            .execute(
                "INSERT INTO evaluation_artifacts VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    evaluation.evaluation_id,
                    evaluation.mission_id,
                    evaluation.candidate_id,
                    evaluation_json,
                    evaluation_hash,
                    evaluation.created_at.to_rfc3339()
                ],
            )
            .map_err(database_error)?;
    }
    Ok(iteration_hash)
}

fn upsert_checkpoint(
    transaction: &Transaction<'_>,
    checkpoint: &RunCheckpoint,
    integrity_key: &[u8; INTEGRITY_KEY_BYTES],
    allow_legacy_protocol_upgrade: bool,
) -> Result<String, StoreError> {
    require_text(&checkpoint.mission_id)?;
    let evaluation_protocol_hash = checkpoint
        .evaluation_protocol_hash
        .as_deref()
        .ok_or(StoreError::CheckpointMismatch)?;
    if checkpoint.engine_version == 0
        || evaluation_protocol_hash.len() != 64
        || !evaluation_protocol_hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(StoreError::CheckpointMismatch);
    }
    ensure_present(
        transaction,
        "missions",
        "mission_id",
        &checkpoint.mission_id,
    )?;
    let mission_binding = mission_evaluation_protocol_binding(transaction, &checkpoint.mission_id)?;
    if mission_binding.evaluation_protocol_hash != evaluation_protocol_hash {
        return Err(StoreError::CheckpointMismatch);
    }
    let existing = transaction
        .query_row(
            "SELECT checkpoint_json, content_hash, auth_tag FROM checkpoints WHERE mission_id = ?",
            params![checkpoint.mission_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(map_query_error);
    match existing {
        Ok((checkpoint_json, content_hash, auth_tag)) => {
            let existing: RunCheckpoint = decode_authenticated(
                integrity_key,
                "checkpoint",
                &checkpoint.mission_id,
                checkpoint_json
                    .as_deref()
                    .ok_or(StoreError::LegacyCheckpoint)?,
                content_hash
                    .as_deref()
                    .ok_or(StoreError::LegacyCheckpoint)?,
                auth_tag
                    .as_deref()
                    .ok_or(StoreError::MissingAuthenticityTag)?,
            )?;
            match existing.evaluation_protocol_hash.as_deref() {
                Some(existing_hash) if existing_hash != evaluation_protocol_hash => {
                    return Err(StoreError::CheckpointMismatch)
                }
                None if !allow_legacy_protocol_upgrade
                    || !mission_binding.legacy_history_unbound =>
                {
                    return Err(StoreError::CheckpointMismatch)
                }
                Some(_) | None => {}
            }
        }
        Err(StoreError::NotFound) => {}
        Err(error) => return Err(error),
    }
    if let Some(iteration_id) = checkpoint.last_iteration_id.as_deref() {
        ensure_present(transaction, "iterations", "iteration_id", iteration_id)?;
        let iteration: ResearchIteration = read_json_row(
            transaction,
            "SELECT payload_json, content_hash FROM iterations WHERE iteration_id = ?",
            iteration_id,
        )?;
        validate_checkpoint_for_iteration(checkpoint, &iteration)?;
    }
    let budget_json =
        serde_json::to_string(&checkpoint.budget_usage).map_err(serialization_error)?;
    let (checkpoint_json, checkpoint_hash) = encoded(checkpoint)?;
    let auth_tag = authentication_tag(
        integrity_key,
        "checkpoint",
        &checkpoint.mission_id,
        &checkpoint_json,
    )?;
    transaction
        .execute(
            "INSERT INTO checkpoints (
                mission_id, last_iteration_id, budget_usage_json, updated_at,
                engine_kind, engine_version, checkpoint_json, content_hash, auth_tag
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (mission_id) DO UPDATE SET
                last_iteration_id = excluded.last_iteration_id,
                budget_usage_json = excluded.budget_usage_json,
                updated_at = excluded.updated_at,
                engine_kind = excluded.engine_kind,
                engine_version = excluded.engine_version,
                checkpoint_json = excluded.checkpoint_json,
                content_hash = excluded.content_hash,
                auth_tag = excluded.auth_tag",
            params![
                checkpoint.mission_id,
                checkpoint.last_iteration_id,
                budget_json,
                checkpoint.updated_at.to_rfc3339(),
                enum_name(&checkpoint.engine_kind)?,
                checkpoint.engine_version,
                checkpoint_json,
                checkpoint_hash,
                auth_tag,
            ],
        )
        .map_err(database_error)?;
    Ok(checkpoint_hash)
}

fn append_journal(
    transaction: &Transaction<'_>,
    mission_id: Option<&str>,
    event_kind: &str,
    record_id: &str,
    content_hash: &str,
    created_at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let event_id = format!("{event_kind}:{record_id}:{content_hash}");
    transaction
        .execute(
            "INSERT OR IGNORE INTO run_journal VALUES (?, ?, ?, ?, ?, ?)",
            params![
                event_id,
                mission_id,
                event_kind,
                record_id,
                content_hash,
                created_at.to_rfc3339()
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

fn ensure_absent(
    connection: &Connection,
    table: &str,
    column: &str,
    value: &str,
) -> Result<(), StoreError> {
    if record_exists(connection, table, column, value)? {
        return Err(StoreError::DuplicateRecord);
    }
    Ok(())
}

fn ensure_present(
    connection: &Connection,
    table: &str,
    column: &str,
    value: &str,
) -> Result<(), StoreError> {
    if !record_exists(connection, table, column, value)? {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

fn record_exists(
    connection: &Connection,
    table: &str,
    column: &str,
    value: &str,
) -> Result<bool, StoreError> {
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} = ?)");
    connection
        .query_row(&sql, params![value], |row| row.get(0))
        .map_err(database_error)
}

fn read_json_row<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
) -> Result<T, StoreError> {
    let (json, hash) = connection
        .query_row(sql, params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map_query_error)?;
    verify_hash(&json, &hash)?;
    serde_json::from_str(&json).map_err(serialization_error)
}

fn decode_authenticated<T: DeserializeOwned>(
    key: &[u8; INTEGRITY_KEY_BYTES],
    domain: &str,
    record_id: &str,
    json: &str,
    hash: &str,
    auth_tag: &str,
) -> Result<T, StoreError> {
    verify_hash(json, hash)?;
    verify_authentication_tag(key, domain, record_id, json, auth_tag)?;
    serde_json::from_str(json).map_err(serialization_error)
}

fn read_json_row_with_hash<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
) -> Result<(T, String), StoreError> {
    let (json, hash) = connection
        .query_row(sql, params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(map_query_error)?;
    verify_hash(&json, &hash)?;
    Ok((
        serde_json::from_str(&json).map_err(serialization_error)?,
        hash,
    ))
}

fn read_json_rows<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    id: &str,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(sql).map_err(database_error)?;
    let rows = statement
        .query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(database_error)?;
    let mut values = Vec::new();
    for row in rows {
        let (json, hash) = row.map_err(database_error)?;
        verify_hash(&json, &hash)?;
        values.push(serde_json::from_str(&json).map_err(serialization_error)?);
    }
    Ok(values)
}

fn encoded<T: Serialize>(value: &T) -> Result<(String, String), StoreError> {
    let json = serde_json::to_string(value).map_err(serialization_error)?;
    let hash = hex::encode(Sha256::digest(json.as_bytes()));
    Ok((json, hash))
}

fn verify_hash(json: &str, expected: &str) -> Result<(), StoreError> {
    if hex::encode(Sha256::digest(json.as_bytes())) != expected {
        return Err(StoreError::ContentHashMismatch);
    }
    Ok(())
}

fn authentication_tag(
    key: &[u8; INTEGRITY_KEY_BYTES],
    domain: &str,
    record_id: &str,
    json: &str,
) -> Result<String, StoreError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|error| StoreError::Database(error.to_string()))?;
    mac.update(domain.as_bytes());
    mac.update(&[0]);
    mac.update(record_id.as_bytes());
    mac.update(&[0]);
    mac.update(json.as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn verify_authentication_tag(
    key: &[u8; INTEGRITY_KEY_BYTES],
    domain: &str,
    record_id: &str,
    json: &str,
    expected: &str,
) -> Result<(), StoreError> {
    let expected = hex::decode(expected).map_err(|_| StoreError::AuthenticityMismatch)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key)
        .map_err(|error| StoreError::Database(error.to_string()))?;
    mac.update(domain.as_bytes());
    mac.update(&[0]);
    mac.update(record_id.as_bytes());
    mac.update(&[0]);
    mac.update(json.as_bytes());
    mac.verify_slice(&expected)
        .map_err(|_| StoreError::AuthenticityMismatch)
}

fn integrity_key_from_env() -> Result<Option<[u8; INTEGRITY_KEY_BYTES]>, StoreError> {
    let Some(value) = std::env::var_os(INTEGRITY_KEY_ENV) else {
        return Ok(None);
    };
    let value = value.into_string().map_err(|_| {
        StoreError::Database(format!("{INTEGRITY_KEY_ENV} must be valid UTF-8 hex"))
    })?;
    let bytes = hex::decode(value.trim())
        .map_err(|_| StoreError::Database(format!("{INTEGRITY_KEY_ENV} must be 32-byte hex")))?;
    bytes
        .try_into()
        .map(Some)
        .map_err(|_| StoreError::Database(format!("{INTEGRITY_KEY_ENV} must be 32-byte hex")))
}

fn load_existing_integrity_key(path: &Path) -> Result<[u8; INTEGRITY_KEY_BYTES], StoreError> {
    if let Some(key) = integrity_key_from_env()? {
        return Ok(key);
    }
    let key_path = integrity_key_path(path);
    read_integrity_key(&key_path)?.ok_or_else(|| {
        StoreError::Database(format!(
            "integrity key {} does not exist",
            key_path.display()
        ))
    })
}

fn load_or_create_integrity_key(path: &Path) -> Result<[u8; INTEGRITY_KEY_BYTES], StoreError> {
    if let Some(key) = integrity_key_from_env()? {
        return Ok(key);
    }

    let key_path = integrity_key_path(path);
    if let Some(key) = read_integrity_key(&key_path)? {
        return Ok(key);
    }

    let key = generate_integrity_key()?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(&key_path) {
        Ok(mut file) => {
            file.write_all(&key)
                .and_then(|_| file.sync_all())
                .map_err(|error| StoreError::Database(error.to_string()))?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            for _ in 0..50 {
                if let Some(key) = read_integrity_key(&key_path)? {
                    return Ok(key);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(StoreError::Database(format!(
                "integrity key {} was not initialized atomically",
                key_path.display()
            )))
        }
        Err(error) => Err(StoreError::Database(error.to_string())),
    }
}

fn generate_integrity_key() -> Result<[u8; INTEGRITY_KEY_BYTES], StoreError> {
    let mut key = [0_u8; INTEGRITY_KEY_BYTES];
    getrandom::fill(&mut key).map_err(|error| StoreError::Database(error.to_string()))?;
    Ok(key)
}

fn integrity_key_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".integrity-key");
    PathBuf::from(value)
}

fn read_integrity_key(path: &Path) -> Result<Option<[u8; INTEGRITY_KEY_BYTES]>, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(StoreError::Database(error.to_string())),
    };
    if bytes.len() < INTEGRITY_KEY_BYTES {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        let mode = std::fs::metadata(path)
            .map_err(|error| StoreError::Database(error.to_string()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(StoreError::Database(format!(
                "integrity key {} must not be group/world accessible",
                path.display()
            )));
        }
    }
    bytes
        .try_into()
        .map(Some)
        .map_err(|_| StoreError::Database("integrity key must contain 32 bytes".to_string()))
}

fn enum_name<T: Serialize>(value: &T) -> Result<String, StoreError> {
    let json = serde_json::to_string(value).map_err(serialization_error)?;
    Ok(json.trim_matches('"').to_string())
}

fn require_text(value: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(StoreError::Domain("identifier cannot be empty".to_string()));
    }
    Ok(())
}

fn domain_error(error: alpha_domain::DomainError) -> StoreError {
    StoreError::Domain(error.to_string())
}

fn database_error(error: duckdb::Error) -> StoreError {
    StoreError::Database(error.to_string())
}

fn map_query_error(error: duckdb::Error) -> StoreError {
    match error {
        duckdb::Error::QueryReturnedNoRows => StoreError::NotFound,
        other => database_error(other),
    }
}

fn serialization_error(error: serde_json::Error) -> StoreError {
    StoreError::Serialization(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_domain::{
        canonical_json_hash, sign_runtime_attribution_event, verify_runtime_attribution_event,
        AttributionKind, AttributionMode, AttributionOutcome, EngineKind, EvaluationCostsV1,
        EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1, IterationVerdict,
        LoopCompletionPolicy, LoopRunStatus, LoopTargetStage, MissionCompletionPolicy,
        MissionStatus, PromotionRecord, RuntimeAttributionEvent, SearchBudget,
        SearchPolicyRevision, StrategyBundle, StrategyBundleArtifact, ValidatorMode,
    };
    use ed25519_dalek::SigningKey;
    use governance::{sign_envelope, AllowedIntentType, ApprovalClass, DeploymentEnvelope};
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_research_manifest::ManifestId;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn evaluation_record_hash_binds_dataset_and_protocol() {
        let record = EvaluationRecord {
            evaluation_id: "evaluation-1".to_string(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            dataset_manifest_id: "dataset-a".to_string(),
            evaluation_protocol_hash: "a".repeat(64),
            payload: serde_json::json!({"score": 1.0}),
            created_at: Utc::now(),
        };
        let original_hash = encoded(&record).unwrap().1;
        let mut changed_dataset = record.clone();
        changed_dataset.dataset_manifest_id = "dataset-b".to_string();
        let mut changed_protocol = record;
        changed_protocol.evaluation_protocol_hash = "b".repeat(64);

        assert_ne!(encoded(&changed_dataset).unwrap().1, original_hash);
        assert_ne!(encoded(&changed_protocol).unwrap().1, original_hash);
    }

    fn mission() -> ResearchMission {
        let now = Utc::now();
        ResearchMission {
            mission_id: "mission-1".to_string(),
            objective: "find stable flow factor".to_string(),
            hypothesis_scope: "LOB flow".to_string(),
            mutable_scope: vec!["factor_ast".to_string()],
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({"metric": "rank_ic"}),
            search_budget: SearchBudget {
                max_candidates: 3,
                max_expansions: 10,
                max_tokens: 0,
                max_seconds: 30,
            },
            completion_policy: MissionCompletionPolicy::default(),
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn verified_attribution(event: RuntimeAttributionEvent) -> VerifiedRuntimeAttributionEvent {
        let key = SigningKey::from_bytes(&[13_u8; 32]);
        verify_runtime_attribution_event(
            &sign_runtime_attribution_event(event, "runtime-feedback", &key).unwrap(),
            &std::collections::BTreeMap::from([(
                "runtime-feedback".to_string(),
                key.verifying_key(),
            )]),
        )
        .unwrap()
    }

    fn iteration() -> ResearchIteration {
        ResearchIteration {
            iteration_id: "iteration-1".to_string(),
            mission_id: "mission-1".to_string(),
            parent_candidate_ids: vec![],
            engine: EngineKind::ManualSeed,
            hypothesis: "imbalance predicts next return".to_string(),
            candidate_artifact_id: Some("candidate-1".to_string()),
            evaluation_artifact_id: None,
            budget_usage: SearchBudgetUsage {
                candidates: 1,
                expansions: 1,
                tokens: 0,
                elapsed_ms: 2,
            },
            verdict: IterationVerdict::Keep,
            failure_class: None,
            failure_explanation: None,
            created_at: Utc::now(),
        }
    }

    fn iteration_without_candidate() -> ResearchIteration {
        let mut iteration = iteration();
        iteration.candidate_artifact_id = None;
        iteration
    }

    fn evaluation_protocol() -> EvaluationProtocolV1 {
        EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: 200,
                validation_rows: 30,
                fold_count: 2,
                purge_rows: 5,
                embargo_rows: 1,
                sealed_holdout_rows: 30,
            },
            EvaluationCostsV1 {
                fee_bps: 1.0,
                rebate_bps: 0.0,
                funding_bps: 0.0,
                latency_bps: 0.5,
                slippage_bps: 0.0,
                cross_spread: false,
                position_notional_usd: 0.0,
                capacity_depth_levels: 0,
                max_book_depth_fraction: 0.0,
            },
            EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            },
        )
        .unwrap()
    }

    fn cex_final_precommit() -> CexFinalPrecommitV1 {
        let reference = |id: &str| CexResearchContentRefV1 {
            id: id.to_string(),
            content_sha256: "a".repeat(64),
        };
        CexFinalPrecommitV1 {
            schema_version: alpha_domain::CEX_FINAL_PRECOMMIT_SCHEMA_V1.to_string(),
            precommit_id: String::new(),
            mission: reference("mission-1"),
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            snapshot: reference("snapshot-1"),
            dataset: reference("dataset-1"),
            partition: reference("partition-1"),
            source: reference("source-1"),
            factor_bank: reference("factor-bank-1"),
            ridge_baseline: reference("ridge-1"),
            cart_baseline: reference("cart-1"),
            baseline_gate: reference("baseline-gate-1"),
            mcts_checkpoint: reference("mcts-checkpoint-1"),
            mcts_subset: reference("mcts-subset-1"),
            weight_policy: reference("weight-policy-1"),
            four_stage_strategy: reference("four-stage-strategy-1"),
            combination_evidence: reference("combination-evidence-1"),
            fixed_weights_sha256: "b".repeat(64),
            replay_receipt: reference("replay-receipt-1"),
            replay_capabilities_sha256: "c".repeat(64),
            evaluation_protocol: reference("evaluation-protocol-1"),
            final_candidate: reference("candidate-1"),
            final_walk_forward_evaluation: reference("walk-forward-1"),
            holdout_id: "holdout-1".to_string(),
            holdout_state: alpha_domain::CexResearchHoldoutStateV1::Unopened,
            implementation_source_revision: "d".repeat(40),
            configuration_sha256: "e".repeat(64),
            deployment_authority: false,
            order_submission_authority: false,
        }
        .finalize()
        .unwrap()
    }

    #[test]
    fn concurrent_cex_sealed_holdout_claims_have_one_winner() {
        let mut readback = AlphaStore::open_in_memory().unwrap();
        let precommit = cex_final_precommit();
        readback
            .put_registry_revision(&RegistryRevision {
                revision_id: precommit.precommit_id.clone(),
                registry_kind: CEX_FINAL_PRECOMMIT_REGISTRY_KIND.to_string(),
                asset_id: precommit.mission.id.clone(),
                parent_revision_id: None,
                payload: serde_json::to_value(&precommit).unwrap(),
                created_at: Utc::now(),
            })
            .unwrap();
        let claim = CexSealedHoldoutClaimV1::from_precommit(&precommit).unwrap();
        let make_store = || AlphaStore {
            path: readback.path.clone(),
            connection: readback.connection.try_clone().unwrap(),
            integrity_key: readback.integrity_key,
        };
        let mut first = make_store();
        let mut second = make_store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let at = Utc::now();

        let (first_result, second_result) = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_claim = claim.clone();
            let first = scope.spawn(move || {
                first_barrier.wait();
                first.claim_cex_sealed_holdout(&first_claim, at)
            });
            let second_barrier = barrier.clone();
            let second_claim = claim.clone();
            let second = scope.spawn(move || {
                second_barrier.wait();
                second.claim_cex_sealed_holdout(&second_claim, at)
            });
            (first.join().unwrap(), second.join().unwrap())
        });
        let results = [first_result, second_result];

        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Ok(None)))
                .count(),
            1
        );
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert!(readback
            .has_cex_sealed_holdout_claim(&claim.holdout_id)
            .unwrap());
        assert_eq!(
            readback
                .get_registry_revision(&claim.claim_id)
                .unwrap()
                .payload,
            serde_json::to_value(claim).unwrap()
        );
    }

    fn bind_evaluation_protocol(store: &mut AlphaStore) -> String {
        let protocol = evaluation_protocol();
        store
            .bind_mission_evaluation_protocol("mission-1", false, &protocol, Utc::now())
            .unwrap()
    }

    fn evaluation_fixture_with_protocol(
        evaluator_version: &str,
        fold_count: usize,
        protocol: &EvaluationProtocolV1,
    ) -> serde_json::Value {
        let config = FormulaEvaluatorConfig::for_trials(3).unwrap();
        let raw_score = 4.0;
        let adjusted_score = config.adjusted_score(raw_score).unwrap();
        let predictive_folds = (1..=fold_count)
            .map(|fold_index| {
                serde_json::json!({
                    "fold_index": fold_index,
                    "row_count": 30,
                    "time_series_ic": 0.1,
                    "time_series_rank_ic": 0.1
                })
            })
            .collect::<Vec<_>>();
        let trading_folds = (1..=fold_count)
            .map(|fold_index| {
                serde_json::json!({
                    "fold_index": fold_index,
                    "row_count": 30,
                    "trade_count": 30,
                    "mean_net_return": 0.001,
                    "cumulative_net_return": 0.03,
                    "max_drawdown": 0.01,
                    "net_sharpe": 1.0,
                    "raw_score": raw_score
                })
            })
            .collect::<Vec<_>>();
        let icir = (fold_count > 1).then_some(0.1 / f64::EPSILON);
        let protocol_hash = protocol.content_hash().unwrap();
        serde_json::json!({
            "passed": true,
            "score": adjusted_score,
            "failure_reasons": [],
            "evaluator_version": evaluator_version,
            "evaluator_config": config,
            "evaluation_protocol": protocol,
            "evaluation_protocol_hash": protocol_hash,
            "metrics": {
                "predictive": {
                    "row_count": fold_count * 30,
                    "time_series_ic": 0.1,
                    "time_series_rank_ic": 0.1,
                    "time_series_icir": icir,
                    "time_series_rank_icir": icir,
                    "positive_ic_ratio": 1.0,
                    "folds": predictive_folds
                },
                "row_count": fold_count * 30,
                "trade_count": fold_count * 30,
                "mean_net_return": 0.001,
                "cumulative_net_return": fold_count as f64 * 0.03,
                "max_drawdown": 0.01,
                "net_sharpe": 1.0,
                "raw_score": raw_score,
                "adjusted_score": adjusted_score,
                "folds": trading_folds
            }
        })
    }

    fn evaluation_fixture(evaluator_version: &str, fold_count: usize) -> serde_json::Value {
        evaluation_fixture_with_protocol(evaluator_version, fold_count, &evaluation_protocol())
    }

    fn sealed_evaluation() -> serde_json::Value {
        evaluation_fixture(SEALED_HOLDOUT_EVALUATOR_VERSION, 1)
    }

    fn walk_forward_evaluation() -> serde_json::Value {
        evaluation_fixture(WALK_FORWARD_EVALUATOR_VERSION, 2)
    }

    fn persist_formula_promotion(
        store: &mut AlphaStore,
        now: DateTime<Utc>,
    ) -> (StoredPromotion, StrategyBundle) {
        try_persist_formula_promotion(store, now, EngineKind::ManualSeed).unwrap()
    }

    fn try_persist_formula_promotion(
        store: &mut AlphaStore,
        now: DateTime<Utc>,
        engine: EngineKind,
    ) -> Result<(StoredPromotion, StrategyBundle), StoreError> {
        try_persist_formula_promotion_with_evidence(store, now, engine, true, None, None, true)
    }

    fn try_persist_formula_promotion_with_evidence(
        store: &mut AlphaStore,
        now: DateTime<Utc>,
        engine: EngineKind,
        include_walk_forward: bool,
        sealed_revision_id: Option<&str>,
        sealed_protocol: Option<EvaluationProtocolV1>,
        include_mission_binding: bool,
    ) -> Result<(StoredPromotion, StrategyBundle), StoreError> {
        let candidate = CandidateArtifact::Formula(FactorAst::Terminal(FactorTerminal::Field(
            "mid_price".to_string(),
        )));
        let mut candidate_iteration = iteration();
        candidate_iteration.engine = engine;
        let walk_forward_protocol = evaluation_protocol();
        if include_mission_binding {
            store.bind_mission_evaluation_protocol(
                "mission-1",
                false,
                &walk_forward_protocol,
                now,
            )?;
        }
        let walk_forward = include_walk_forward.then(|| EvaluationRecord {
            evaluation_id: "evaluation-1".to_string(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            dataset_manifest_id: "dataset-1".to_string(),
            evaluation_protocol_hash: walk_forward_protocol.content_hash().unwrap(),
            payload: evaluation_fixture_with_protocol(
                WALK_FORWARD_EVALUATOR_VERSION,
                2,
                &walk_forward_protocol,
            ),
            created_at: now,
        });
        candidate_iteration.evaluation_artifact_id = walk_forward
            .as_ref()
            .map(|evaluation| evaluation.evaluation_id.clone());
        store
            .append_iteration(
                &candidate_iteration,
                Some(("candidate-1", &candidate)),
                walk_forward.as_ref(),
            )
            .unwrap();
        let candidate_hash = store.mission_lineage("mission-1").unwrap().candidates[0]
            .content_hash
            .clone();
        let sealed_protocol = sealed_protocol.unwrap_or_else(evaluation_protocol);
        let evaluation =
            evaluation_fixture_with_protocol(SEALED_HOLDOUT_EVALUATOR_VERSION, 1, &sealed_protocol);
        let evaluation_protocol_hash = evaluation
            .get("evaluation_protocol_hash")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string();
        let sealed = RegistryRevision {
            revision_id: sealed_revision_id.map(str::to_owned).unwrap_or_else(|| {
                format!(
                    "sealed-evaluation:{}:candidate-1",
                    SEALED_HOLDOUT_EVALUATOR_VERSION
                )
            }),
            registry_kind: "sealed_evaluation".to_string(),
            asset_id: "candidate-1".to_string(),
            parent_revision_id: None,
            payload: serde_json::json!({
                "mission_id": "mission-1",
                "candidate_content_hash": candidate_hash,
                "dataset_manifest_id": "dataset-1",
                "evaluation_protocol_hash": evaluation_protocol_hash,
                "evaluation": evaluation,
            }),
            created_at: now,
        };
        store.put_registry_revision(&sealed).unwrap();
        let persisted = store.get_registry_revision(&sealed.revision_id).unwrap();
        let persisted_evaluation = persisted.payload.get("evaluation").unwrap();
        let evaluator_config_hash =
            canonical_json_hash(persisted_evaluation.get("evaluator_config").unwrap()).unwrap();
        let evaluation_metrics_hash =
            canonical_json_hash(persisted_evaluation.get("metrics").unwrap()).unwrap();
        let evaluation_hash = canonical_json_hash(persisted_evaluation).unwrap();
        let bundle = StrategyBundle::new(
            "bundle:candidate-1".to_string(),
            "candidate-1".to_string(),
            candidate_hash.clone(),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            evaluation_protocol_hash.clone(),
            evaluator_config_hash.clone(),
            evaluation_metrics_hash.clone(),
            evaluation_hash.clone(),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(FactorTerminal::Field("mid_price".to_string())),
            },
            now,
        )
        .unwrap();
        let promotion = PromotionRecord {
            promotion_id: "promotion-1".to_string(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            candidate_content_hash: candidate_hash,
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            evaluator_version: SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            evaluation_protocol_hash,
            evaluator_config_hash,
            evaluation_metrics_hash,
            sealed_evaluation_id: sealed.revision_id,
            sealed_evaluation_hash: evaluation_hash,
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            created_at: now,
        };
        let stored = store.promote_candidate(&bundle, &promotion)?;
        Ok((stored, bundle))
    }

    #[test]
    fn migrations_are_idempotent() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
    }

    #[test]
    fn migration_002_preserves_legacy_approval_payloads() {
        let path = temp_db("legacy-approval");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(MIGRATION_001).unwrap();
        let now = Utc::now();
        let payload = serde_json::json!({
            "approval_id": "legacy-approval",
            "approval_class": "paper",
            "subject_id": "promotion-1",
            "payload": {},
            "created_at": now,
        });
        let json = serde_json::to_string(&payload).unwrap();
        let hash = hex::encode(Sha256::digest(json.as_bytes()));
        connection
            .execute(
                "INSERT INTO approvals (approval_id, approval_class, subject_id, payload_json, content_hash, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    "legacy-approval",
                    "paper",
                    "promotion-1",
                    json,
                    hash,
                    now.to_rfc3339()
                ],
            )
            .unwrap();
        drop(connection);

        let store = AlphaStore::open(&path).unwrap();
        let approval = store.get_approval("legacy-approval").unwrap();
        assert_eq!(approval.signer_id, None);
        assert!(!approval.is_active_at(now));
    }

    #[test]
    fn migration_002_preserves_legacy_deployment_readback() {
        let path = temp_db("legacy-deployment");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(MIGRATION_001).unwrap();
        let now = Utc::now();
        let payload = serde_json::json!({
            "envelope": {
                "deployment_id": "legacy-deployment",
                "asset_revision_id": "candidate-legacy",
                "promotion_manifest_hash": "a".repeat(64),
                "runtime_config_hash": "b".repeat(64),
                "risk_policy_hash": "c".repeat(64),
                "account_id": "account-1",
                "venue": "binance",
                "instruments": ["BTCUSDT"],
                "allowed_intent_types": ["StartPaper"],
                "max_notional": 100.0,
                "max_symbol_exposure": 50.0,
                "max_order_size": 10.0,
                "max_slippage_bps": 2.0,
                "valid_from": now,
                "expires_at": now + chrono::Duration::minutes(5),
                "nonce": "legacy-nonce",
                "approval_class": "Paper",
                "approval_signatures": ["legacy-approval"],
                "payload_hash": "legacy-payload-hash"
            },
            "key_id": "legacy-key",
            "signature_hex": "legacy-signature"
        });
        let json = serde_json::to_string(&payload).unwrap();
        let hash = hex::encode(Sha256::digest(json.as_bytes()));
        connection
            .execute(
                "INSERT INTO deployment_envelopes VALUES (?, ?, ?, ?)",
                params!["legacy-deployment", json, hash, now.to_rfc3339()],
            )
            .unwrap();
        drop(connection);

        let store = AlphaStore::open(&path).unwrap();
        let signed = store.get_deployment("legacy-deployment").unwrap();
        assert!(signed.envelope.promotion_id.is_empty());
        assert!(signed.envelope.bundle_id.is_empty());
        assert!(signed.envelope.bundle_hash.is_empty());
        assert!(signed.envelope.validate().is_err());
    }

    #[test]
    fn typed_promotion_and_bundle_round_trip_atomically() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let (promotion, bundle) = persist_formula_promotion(&mut store, Utc::now());

        assert_eq!(store.get_promotion("promotion-1").unwrap(), promotion);
        assert_eq!(
            store.get_strategy_bundle("bundle:candidate-1").unwrap(),
            bundle
        );
    }

    #[test]
    fn canonical_promotion_eligibility_requires_the_mission_protocol_binding() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let (promotion, _) = persist_formula_promotion(&mut store, Utc::now());
        store
            .connection
            .execute(
                "DELETE FROM registry_revisions WHERE revision_id = ?",
                params![mission_evaluation_protocol_revision_id("mission-1")],
            )
            .unwrap();

        assert!(store.get_promotion(&promotion.record.promotion_id).is_ok());
        assert!(store
            .get_canonical_promotion(&promotion.record.promotion_id)
            .is_err());
    }

    #[test]
    fn legacy_promotion_and_bundle_are_readable_but_not_canonical() {
        #[derive(Serialize)]
        struct LegacySignableBundle<'a> {
            candidate_content_hash: &'a str,
            dataset_manifest_id: &'a ManifestId,
            evaluator_version: &'a str,
            evaluator_config_hash: &'a str,
            evaluation_metrics_hash: &'a str,
            sealed_evaluation_hash: &'a str,
            artifact: &'a StrategyBundleArtifact,
        }

        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let (promotion, bundle) = persist_formula_promotion(&mut store, Utc::now());
        let legacy_bundle_hash = canonical_json_hash(&LegacySignableBundle {
            candidate_content_hash: &bundle.candidate_content_hash,
            dataset_manifest_id: &bundle.dataset_manifest_id,
            evaluator_version: &bundle.evaluator_version,
            evaluator_config_hash: &bundle.evaluator_config_hash,
            evaluation_metrics_hash: &bundle.evaluation_metrics_hash,
            sealed_evaluation_hash: &bundle.sealed_evaluation_hash,
            artifact: &bundle.artifact,
        })
        .unwrap();

        let mut legacy_bundle = serde_json::to_value(&bundle).unwrap();
        let bundle_object = legacy_bundle.as_object_mut().unwrap();
        bundle_object.remove("evaluation_protocol_hash");
        bundle_object.insert(
            "bundle_hash".to_string(),
            serde_json::Value::String(legacy_bundle_hash.clone()),
        );
        let legacy_bundle_json = serde_json::to_string(&legacy_bundle).unwrap();
        let legacy_bundle_content_hash = hex::encode(Sha256::digest(legacy_bundle_json.as_bytes()));

        let mut legacy_promotion = serde_json::to_value(&promotion.record).unwrap();
        let promotion_object = legacy_promotion.as_object_mut().unwrap();
        promotion_object.remove("evaluation_protocol_hash");
        promotion_object.insert(
            "bundle_hash".to_string(),
            serde_json::Value::String(legacy_bundle_hash),
        );
        let legacy_promotion_json = serde_json::to_string(&legacy_promotion).unwrap();
        let legacy_promotion_content_hash =
            hex::encode(Sha256::digest(legacy_promotion_json.as_bytes()));

        store
            .connection
            .execute(
                "UPDATE strategy_bundles SET payload_json = ?, content_hash = ? WHERE bundle_id = ?",
                params![
                    legacy_bundle_json,
                    legacy_bundle_content_hash,
                    bundle.bundle_id
                ],
            )
            .unwrap();
        store
            .connection
            .execute(
                "UPDATE promotions SET payload_json = ?, content_hash = ? WHERE promotion_id = ?",
                params![
                    legacy_promotion_json,
                    legacy_promotion_content_hash,
                    promotion.record.promotion_id
                ],
            )
            .unwrap();

        let readable_bundle = store.get_strategy_bundle(&bundle.bundle_id).unwrap();
        let readable_promotion = store.get_promotion(&promotion.record.promotion_id).unwrap();
        assert!(readable_bundle.evaluation_protocol_hash.is_empty());
        assert!(readable_promotion
            .record
            .evaluation_protocol_hash
            .is_empty());
        assert!(store
            .get_canonical_strategy_bundle(&bundle.bundle_id)
            .is_err());
        assert!(store
            .get_canonical_promotion(&promotion.record.promotion_id)
            .is_err());
    }

    #[test]
    fn promotion_rejects_missing_walk_forward_evidence_without_partial_bundle() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();

        let error = try_persist_formula_promotion_with_evidence(
            &mut store,
            Utc::now(),
            EngineKind::ManualSeed,
            false,
            None,
            None,
            true,
        )
        .unwrap_err();

        assert!(error.to_string().contains("canonical walk-forward"));
        assert!(matches!(
            store.get_strategy_bundle("bundle:candidate-1"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn promotion_rejects_missing_mission_protocol_binding_without_partial_bundle() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();

        assert!(try_persist_formula_promotion_with_evidence(
            &mut store,
            Utc::now(),
            EngineKind::ManualSeed,
            true,
            None,
            None,
            false,
        )
        .is_err());
        assert!(matches!(
            store.get_strategy_bundle("bundle:candidate-1"),
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.get_promotion("promotion-1"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn promotion_rejects_any_sealed_protocol_drift_without_partial_bundle() {
        let base = evaluation_protocol();
        let mut variants = Vec::new();

        let mut changed = base.clone();
        changed.costs.fee_bps += 1.0;
        variants.push(("fee", changed));
        let mut changed = base.clone();
        changed.costs.funding_bps += 1.0;
        variants.push(("funding", changed));
        let mut changed = base.clone();
        changed.costs.latency_bps += 1.0;
        variants.push(("latency", changed));
        let mut changed = base.clone();
        changed.walk_forward.initial_train_rows += 1;
        variants.push(("initial_train", changed));
        let mut changed = base.clone();
        changed.walk_forward.validation_rows += 1;
        variants.push(("validation", changed));
        let mut changed = base.clone();
        changed.walk_forward.fold_count += 1;
        variants.push(("fold_count", changed));
        let mut changed = base.clone();
        changed.walk_forward.purge_rows += 1;
        variants.push(("purge", changed));
        let mut changed = base.clone();
        changed.walk_forward.embargo_rows += 1;
        variants.push(("embargo", changed));
        let mut changed = base.clone();
        changed.walk_forward.sealed_holdout_rows += 1;
        variants.push(("sealed_holdout", changed));
        let mut changed = base.clone();
        changed.labels.horizon_buckets = 4;
        variants.push(("label_horizon", changed));
        let mut changed = base;
        changed.labels.observation_frequency_millis += 1;
        variants.push(("label_frequency", changed));

        for (field, sealed_protocol) in variants {
            let mut store = AlphaStore::open_in_memory().unwrap();
            store.create_mission(&mission()).unwrap();

            let error = try_persist_formula_promotion_with_evidence(
                &mut store,
                Utc::now(),
                EngineKind::ManualSeed,
                true,
                None,
                Some(sealed_protocol),
                true,
            )
            .expect_err(field);

            assert!(error.to_string().contains("protocol"), "{field}: {error}");
            assert!(matches!(
                store.get_strategy_bundle("bundle:candidate-1"),
                Err(StoreError::NotFound)
            ));
            assert!(matches!(
                store.get_promotion("promotion-1"),
                Err(StoreError::NotFound)
            ));
        }
    }

    #[test]
    fn noncanonical_sealed_revision_cannot_promote_or_advance_holdout() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();

        let error = try_persist_formula_promotion_with_evidence(
            &mut store,
            Utc::now(),
            EngineKind::ManualSeed,
            true,
            Some("legacy-sealed-evaluation:candidate-1"),
            None,
            true,
        )
        .unwrap_err();

        assert!(error.to_string().contains("revision id is not canonical"));
        assert!(matches!(
            store.get_strategy_bundle("bundle:candidate-1"),
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.get_promotion("promotion-1"),
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            store
                .sealed_passed_candidate_for_mission("mission-1")
                .unwrap(),
            None
        );
    }

    #[test]
    fn malformed_walk_forward_evidence_is_not_canonical() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mission = mission();
        store.create_mission(&mission).unwrap();
        let candidate = CandidateArtifact::Formula(FactorAst::Terminal(FactorTerminal::Field(
            "mid_price".to_string(),
        )));
        let mut iteration = iteration();
        iteration.evaluation_artifact_id = Some("evaluation-1".to_string());
        let mut payload = walk_forward_evaluation();
        payload["metrics"]
            .as_object_mut()
            .unwrap()
            .remove("predictive");
        let evaluation = EvaluationRecord {
            evaluation_id: "evaluation-1".to_string(),
            mission_id: mission.mission_id.clone(),
            candidate_id: "candidate-1".to_string(),
            dataset_manifest_id: mission.dataset_manifest_id.as_str().to_string(),
            evaluation_protocol_hash: evaluation_protocol().content_hash().unwrap(),
            payload,
            created_at: Utc::now(),
        };
        store
            .append_iteration(
                &iteration,
                Some(("candidate-1", &candidate)),
                Some(&evaluation),
            )
            .unwrap();

        assert!(store
            .canonical_walk_forward_protocol_hash(&mission, "candidate-1", &iteration, &candidate,)
            .unwrap()
            .is_none());
    }

    #[test]
    fn promotion_recomputes_sealed_config_and_metrics_hashes() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let (stored, bundle) = persist_formula_promotion(&mut store, Utc::now());

        for (suffix, forge_config) in [("config", true), ("metrics", false)] {
            let mut forged_bundle = bundle.clone();
            forged_bundle.bundle_id = format!("bundle:forged-{suffix}");
            if forge_config {
                forged_bundle.evaluator_config_hash = "f".repeat(64);
            } else {
                forged_bundle.evaluation_metrics_hash = "f".repeat(64);
            }
            forged_bundle.bundle_hash = forged_bundle.calculated_hash().unwrap();

            let mut forged_promotion = stored.record.clone();
            forged_promotion.promotion_id = format!("promotion-forged-{suffix}");
            forged_promotion.bundle_id = forged_bundle.bundle_id.clone();
            forged_promotion.bundle_hash = forged_bundle.bundle_hash.clone();
            forged_promotion.evaluator_config_hash = forged_bundle.evaluator_config_hash.clone();
            forged_promotion.evaluation_metrics_hash =
                forged_bundle.evaluation_metrics_hash.clone();

            assert!(store
                .promote_candidate(&forged_bundle, &forged_promotion)
                .is_err());
            assert!(matches!(
                store.get_strategy_bundle(&forged_bundle.bundle_id),
                Err(StoreError::NotFound)
            ));
        }
    }

    #[test]
    fn offline_rl_provenance_cannot_reach_sealed_or_promotion_authority() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();

        assert!(try_persist_formula_promotion(
            &mut store,
            Utc::now(),
            EngineKind::OfflineReinforcementLearning,
        )
        .is_err());
        assert_eq!(
            store
                .sealed_passed_candidate_for_mission("mission-1")
                .unwrap(),
            None
        );
        assert!(matches!(
            store.get_strategy_bundle("bundle:candidate-1"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn deployment_store_rejects_arbitrary_bundle_binding() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let now = Utc::now();
        let (promotion, bundle) = persist_formula_promotion(&mut store, now);
        let signed = sign_envelope(
            DeploymentEnvelope {
                deployment_id: "deployment-bad-binding".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                promotion_id: promotion.record.promotion_id,
                promotion_manifest_hash: promotion.content_hash,
                bundle_id: bundle.bundle_id,
                bundle_hash: "f".repeat(64),
                runtime_config_hash: "c".repeat(64),
                risk_policy_hash: "d".repeat(64),
                account_id: "account-1".to_string(),
                venue: "binance".to_string(),
                instruments: vec!["BTCUSDT".to_string()],
                allowed_intent_types: vec![AllowedIntentType::StartPaper],
                max_notional: 100.0,
                max_symbol_exposure: 50.0,
                max_order_size: 10.0,
                max_slippage_bps: 2.0,
                valid_from: now,
                expires_at: now + chrono::Duration::minutes(1),
                nonce: "nonce-bad-binding".to_string(),
                approval_class: ApprovalClass::Paper,
                approval_signatures: vec!["approval-1".to_string()],
                payload_hash: String::new(),
            },
            "key-1",
            &SigningKey::from_bytes(&[7_u8; 32]),
        )
        .unwrap();

        assert!(store.store_deployment(&signed, now).is_err());
    }

    #[test]
    fn promotion_rejects_candidate_hash_mismatch_without_partial_bundle() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let candidate = CandidateArtifact::Formula(FactorAst::Terminal(FactorTerminal::Field(
            "mid_price".to_string(),
        )));
        store
            .append_iteration(&iteration(), Some(("candidate-1", &candidate)), None)
            .unwrap();
        let now = Utc::now();
        let evaluation = sealed_evaluation();
        let evaluation_protocol_hash = evaluation
            .get("evaluation_protocol_hash")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: format!(
                    "sealed-evaluation:{}:candidate-1",
                    SEALED_HOLDOUT_EVALUATOR_VERSION
                ),
                registry_kind: "sealed_evaluation".to_string(),
                asset_id: "candidate-1".to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "mission_id": "mission-1",
                    "candidate_content_hash": "c".repeat(64),
                    "dataset_manifest_id": "dataset-1",
                    "evaluation_protocol_hash": evaluation_protocol_hash,
                    "evaluation": evaluation,
                }),
                created_at: now,
            })
            .unwrap();
        let evaluator_config_hash =
            canonical_json_hash(evaluation.get("evaluator_config").unwrap()).unwrap();
        let evaluation_metrics_hash =
            canonical_json_hash(evaluation.get("metrics").unwrap()).unwrap();
        let evaluation_hash = canonical_json_hash(&evaluation).unwrap();
        let bundle = StrategyBundle::new(
            "bundle:bad".to_string(),
            "candidate-1".to_string(),
            "c".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            evaluation_protocol_hash.clone(),
            evaluator_config_hash.clone(),
            evaluation_metrics_hash.clone(),
            evaluation_hash.clone(),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(FactorTerminal::Field("mid_price".to_string())),
            },
            now,
        )
        .unwrap();
        let promotion = PromotionRecord {
            promotion_id: "promotion-bad".to_string(),
            mission_id: "mission-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            candidate_content_hash: "c".repeat(64),
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            evaluator_version: SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            evaluation_protocol_hash,
            evaluator_config_hash,
            evaluation_metrics_hash,
            sealed_evaluation_id: format!(
                "sealed-evaluation:{}:candidate-1",
                SEALED_HOLDOUT_EVALUATOR_VERSION
            ),
            sealed_evaluation_hash: evaluation_hash,
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            created_at: now,
        };

        assert!(store.promote_candidate(&bundle, &promotion).is_err());
        assert!(matches!(
            store.get_strategy_bundle("bundle:bad"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn approvals_require_identity_window_and_honor_revocation() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let now = Utc::now();
        let approval = ApprovalRecord {
            approval_id: "approval-1".to_string(),
            approval_class: "paper".to_string(),
            subject_id: "promotion-1".to_string(),
            payload: serde_json::json!({"scope_hash": "scope"}),
            signer_id: Some("reviewer-1".to_string()),
            valid_from: Some(now),
            expires_at: Some(now + chrono::Duration::minutes(10)),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: now,
        };
        store.record_approval(&approval).unwrap();
        let original_evidence = |store: &AlphaStore| {
            store.connection.query_row(
                "SELECT payload_json, content_hash FROM approvals WHERE approval_id = 'approval-1'",
                [], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ).unwrap()
        };
        let before = original_evidence(&store);
        assert!(store.get_approval("approval-1").unwrap().is_active_at(now));

        store
            .revoke_approval(
                "approval-1",
                "reviewer-2",
                "withdrawn",
                now + chrono::Duration::seconds(1),
            )
            .unwrap();
        assert_eq!(
            before,
            original_evidence(&store),
            "revocation must preserve original approval evidence"
        );
        assert!(!store
            .get_approval("approval-1")
            .unwrap()
            .is_active_at(now + chrono::Duration::seconds(1)));

        let legacy: ApprovalRecord = serde_json::from_value(serde_json::json!({
            "approval_id": "legacy",
            "approval_class": "paper",
            "subject_id": "promotion-1",
            "payload": {},
            "created_at": now,
        }))
        .unwrap();
        assert!(!legacy.is_active_at(now));
    }

    #[test]
    fn live_small_eligibility_requires_active_human_external_evidence() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let now = Utc::now();
        let (promotion, bundle) = persist_formula_promotion(&mut store, now);
        let approval = ApprovalRecord {
            approval_id: "live-eligibility-1".to_string(),
            approval_class: "human_live_small".to_string(),
            subject_id: promotion.record.promotion_id.clone(),
            payload: serde_json::json!({
                "eligibility": {
                    "candidate_id": promotion.record.candidate_id,
                    "bundle_id": bundle.bundle_id,
                    "reconciliation_evidence_sha256": "a".repeat(64),
                    "reduce_only_exit_evidence_sha256": "b".repeat(64),
                    "shadow_soak_evidence_sha256": "c".repeat(64)
                }
            }),
            signer_id: Some("risk-officer-1".to_string()),
            valid_from: Some(now),
            expires_at: Some(now + chrono::Duration::minutes(10)),
            revoked_at: None,
            revoked_by: None,
            revocation_reason: None,
            created_at: now,
        };
        store.record_approval(&approval).unwrap();
        assert_eq!(
            store
                .live_small_eligibility_approval(
                    "mission-1",
                    "candidate-1",
                    now + chrono::Duration::seconds(1),
                )
                .unwrap()
                .as_deref(),
            Some("live-eligibility-1")
        );
        store
            .revoke_approval(
                "live-eligibility-1",
                "risk-officer-2",
                "external acceptance withdrawn",
                now + chrono::Duration::seconds(2),
            )
            .unwrap();
        assert_eq!(
            store
                .live_small_eligibility_approval(
                    "mission-1",
                    "candidate-1",
                    now + chrono::Duration::seconds(3),
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn mission_round_trips() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mission = mission();
        store.create_mission(&mission).unwrap();
        assert_eq!(store.get_mission("mission-1").unwrap(), mission);
    }

    #[test]
    fn mission_evaluation_protocol_binding_is_immutable() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let protocol = evaluation_protocol();
        let hash = store
            .bind_mission_evaluation_protocol("mission-1", false, &protocol, Utc::now())
            .unwrap();
        assert_eq!(
            store
                .require_mission_evaluation_protocol("mission-1", &protocol)
                .unwrap(),
            hash
        );

        let mut drifted = protocol;
        drifted.costs.fee_bps += 1.0;
        assert!(store
            .bind_mission_evaluation_protocol("mission-1", false, &drifted, Utc::now())
            .is_err());
        assert!(store
            .require_mission_evaluation_protocol("mission-1", &drifted)
            .is_err());
    }

    #[test]
    fn mission_read_rejects_payload_tampering() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        store
            .connection
            .execute(
                "UPDATE missions SET payload_json = '{}' WHERE mission_id = 'mission-1'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.get_mission("mission-1"),
            Err(StoreError::ContentHashMismatch)
        ));
    }

    #[test]
    fn iterations_are_append_only() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let iteration = iteration();
        let candidate = CandidateArtifact::Program(serde_json::json!({"op": "rank"}));
        store
            .append_iteration(&iteration, Some(("candidate-1", &candidate)), None)
            .unwrap();
        assert!(matches!(
            store.append_iteration(&iteration, Some(("candidate-1", &candidate)), None),
            Err(StoreError::DuplicateRecord)
        ));
    }

    #[test]
    fn checkpoint_survives_reopen() {
        let path = temp_db("checkpoint");
        let iteration = iteration_without_candidate();
        let checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
            evaluation_protocol_hash: Some(evaluation_protocol().content_hash().unwrap()),
            engine_kind: iteration.engine.clone(),
            engine_version: 1,
            engine_state: serde_json::json!({"mode": "test"}),
            updated_at: iteration.created_at,
        };
        {
            let mut store = AlphaStore::open(&path).unwrap();
            store.create_mission(&mission()).unwrap();
            bind_evaluation_protocol(&mut store);
            store.append_iteration(&iteration, None, None).unwrap();
            store.save_checkpoint(&checkpoint).unwrap();
        }
        let reopened = AlphaStore::open(&path).unwrap();
        assert_eq!(reopened.get_checkpoint("mission-1").unwrap(), checkpoint);
    }

    #[test]
    fn checkpoint_rejects_unbound_or_rewritten_protocol_hashes() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let protocol_hash = bind_evaluation_protocol(&mut store);
        let iteration = iteration_without_candidate();
        let mut checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
            evaluation_protocol_hash: None,
            engine_kind: iteration.engine.clone(),
            engine_version: 1,
            engine_state: serde_json::json!({"mode": "exact"}),
            updated_at: iteration.created_at,
        };
        assert!(store
            .append_iteration_with_checkpoint(&iteration, None, None, &checkpoint)
            .is_err());
        assert!(store
            .mission_lineage("mission-1")
            .unwrap()
            .iterations
            .is_empty());

        checkpoint.evaluation_protocol_hash = Some(protocol_hash);
        store
            .append_iteration_with_checkpoint(&iteration, None, None, &checkpoint)
            .unwrap();
        let canonical = store.get_checkpoint("mission-1").unwrap();
        checkpoint.evaluation_protocol_hash = Some("f".repeat(64));
        assert!(store.save_checkpoint(&checkpoint).is_err());
        assert_eq!(store.get_checkpoint("mission-1").unwrap(), canonical);
    }

    #[test]
    fn iteration_and_checkpoint_commit_atomically() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        bind_evaluation_protocol(&mut store);
        let iteration = iteration_without_candidate();
        let checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
            evaluation_protocol_hash: Some(evaluation_protocol().content_hash().unwrap()),
            engine_kind: iteration.engine.clone(),
            engine_version: 0,
            engine_state: serde_json::json!({}),
            updated_at: iteration.created_at,
        };

        assert!(store
            .append_iteration_with_checkpoint(&iteration, None, None, &checkpoint)
            .is_err());
        assert!(store
            .mission_lineage("mission-1")
            .unwrap()
            .iterations
            .is_empty());
        assert!(matches!(
            store.get_checkpoint("mission-1"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn checkpoint_read_fails_closed_on_legacy_or_tampered_payloads() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        bind_evaluation_protocol(&mut store);
        let iteration = iteration_without_candidate();
        store.append_iteration(&iteration, None, None).unwrap();
        let checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
            evaluation_protocol_hash: Some(evaluation_protocol().content_hash().unwrap()),
            engine_kind: iteration.engine,
            engine_version: 1,
            engine_state: serde_json::json!({"mode": "exact"}),
            updated_at: iteration.created_at,
        };
        store.save_checkpoint(&checkpoint).unwrap();
        store
            .connection
            .execute(
                "UPDATE checkpoints SET checkpoint_json = '{}' WHERE mission_id = 'mission-1'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.get_checkpoint("mission-1"),
            Err(StoreError::ContentHashMismatch)
        ));
        store
            .connection
            .execute(
                "UPDATE checkpoints SET checkpoint_json = NULL, content_hash = NULL WHERE mission_id = 'mission-1'",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.get_checkpoint("mission-1"),
            Err(StoreError::LegacyCheckpoint)
        ));
    }

    #[test]
    fn checkpoint_and_loop_run_require_keyed_authenticity() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        bind_evaluation_protocol(&mut store);
        let iteration = iteration_without_candidate();
        store.append_iteration(&iteration, None, None).unwrap();
        let mut checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
            evaluation_protocol_hash: Some(evaluation_protocol().content_hash().unwrap()),
            engine_kind: iteration.engine,
            engine_version: 1,
            engine_state: serde_json::json!({"mode": "exact"}),
            updated_at: iteration.created_at,
        };
        store.save_checkpoint(&checkpoint).unwrap();
        checkpoint.engine_state = serde_json::json!({"mode": "forged"});
        let checkpoint_json = serde_json::to_string(&checkpoint).unwrap();
        let checkpoint_hash = hex::encode(Sha256::digest(checkpoint_json.as_bytes()));
        store
            .connection
            .execute(
                "UPDATE checkpoints SET checkpoint_json = ?, content_hash = ? WHERE mission_id = ?",
                params![checkpoint_json, checkpoint_hash, "mission-1"],
            )
            .unwrap();
        assert!(matches!(
            store.get_checkpoint("mission-1"),
            Err(StoreError::AuthenticityMismatch)
        ));

        let now = Utc::now();
        let run = LoopRun {
            loop_run_id: "loop-auth".to_string(),
            root_mission_id: "mission-1".to_string(),
            completion_policy: LoopCompletionPolicy {
                target_stage: LoopTargetStage::ShadowHealthy,
                max_research_missions: 2,
            },
            child_mission_ids: vec![],
            stage_records: vec![],
            status: LoopRunStatus::Pending,
            stop_reason: None,
            created_at: now,
            updated_at: now,
        };
        store.create_loop_run(&run).unwrap();
        let mut forged = run.clone();
        forged.updated_at = now + chrono::Duration::seconds(1);
        let loop_json = serde_json::to_string(&forged).unwrap();
        let loop_hash = hex::encode(Sha256::digest(loop_json.as_bytes()));
        store
            .connection
            .execute(
                "UPDATE loop_runs SET payload_json = ?, content_hash = ? WHERE loop_run_id = ?",
                params![loop_json, loop_hash, "loop-auth"],
            )
            .unwrap();
        assert!(matches!(
            store.get_loop_run("loop-auth"),
            Err(StoreError::AuthenticityMismatch)
        ));
    }

    #[test]
    fn missing_checkpoint_authenticity_is_not_backfilled_on_reopen() {
        let path = temp_db("missing-auth-tag");
        {
            let mut store = AlphaStore::open(&path).unwrap();
            store.create_mission(&mission()).unwrap();
            bind_evaluation_protocol(&mut store);
            let iteration = iteration_without_candidate();
            store.append_iteration(&iteration, None, None).unwrap();
            store
                .save_checkpoint(&RunCheckpoint {
                    mission_id: iteration.mission_id.clone(),
                    last_iteration_id: Some(iteration.iteration_id.clone()),
                    budget_usage: iteration.budget_usage.clone(),
                    evaluation_protocol_hash: Some(evaluation_protocol().content_hash().unwrap()),
                    engine_kind: iteration.engine,
                    engine_version: 1,
                    engine_state: serde_json::json!({"mode": "exact"}),
                    updated_at: iteration.created_at,
                })
                .unwrap();
            store
                .connection
                .execute("UPDATE checkpoints SET auth_tag = NULL", [])
                .unwrap();
        }

        let reopened = AlphaStore::open(&path).unwrap();
        assert!(matches!(
            reopened.get_checkpoint("mission-1"),
            Err(StoreError::MissingAuthenticityTag)
        ));
    }

    #[test]
    fn legacy_checkpoint_can_be_forked_without_destroying_evidence() {
        let path = temp_db("legacy-checkpoint-recovery");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(MIGRATION_001).unwrap();
        connection.execute_batch(MIGRATION_002).unwrap();
        let now = Utc::now();
        let mut legacy_mission = mission();
        legacy_mission.status = MissionStatus::Paused;
        legacy_mission.updated_at = now;
        let (mission_json, mission_hash) = encoded(&legacy_mission).unwrap();
        connection
            .execute(
                "INSERT INTO missions VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    legacy_mission.mission_id,
                    "Paused",
                    mission_json,
                    mission_hash,
                    legacy_mission.created_at.to_rfc3339(),
                    legacy_mission.updated_at.to_rfc3339(),
                ],
            )
            .unwrap();
        let legacy_iteration = iteration_without_candidate();
        let (iteration_json, iteration_hash) = encoded(&legacy_iteration).unwrap();
        connection
            .execute(
                "INSERT INTO iterations VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    legacy_iteration.iteration_id,
                    legacy_iteration.mission_id,
                    "Keep",
                    iteration_json,
                    iteration_hash,
                    legacy_iteration.created_at.to_rfc3339(),
                ],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO checkpoints VALUES (?, ?, ?, ?)",
                params![
                    "mission-1",
                    "iteration-1",
                    serde_json::to_string(&legacy_iteration.budget_usage).unwrap(),
                    legacy_iteration.created_at.to_rfc3339(),
                ],
            )
            .unwrap();
        drop(connection);

        let mut store = AlphaStore::open(&path).unwrap();
        assert!(matches!(
            store.get_checkpoint("mission-1"),
            Err(StoreError::LegacyCheckpoint)
        ));
        let replacement = store
            .fork_legacy_checkpoint("mission-1", "mission-recovered", now)
            .unwrap();
        assert_eq!(replacement.mission_id, "mission-recovered");
        assert_eq!(replacement.status, MissionStatus::Pending);
        assert!(store
            .mission_lineage("mission-recovered")
            .unwrap()
            .iterations
            .is_empty());
        assert_eq!(
            store.mission_lineage("mission-1").unwrap().iterations.len(),
            1
        );
        assert_eq!(
            store.get_mission("mission-1").unwrap().status,
            MissionStatus::Failed
        );
        assert!(matches!(
            store.get_checkpoint("mission-recovered"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn pre_live_mcts_checkpoint_can_be_forked_without_resume() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        bind_evaluation_protocol(&mut store);
        let mut iteration = iteration_without_candidate();
        iteration.engine = EngineKind::Mcts;
        let checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
            evaluation_protocol_hash: Some(evaluation_protocol().content_hash().unwrap()),
            engine_kind: EngineKind::Mcts,
            engine_version: 1,
            engine_state: serde_json::json!({"mode": "pre-live-only"}),
            updated_at: iteration.created_at,
        };
        store.append_iteration(&iteration, None, None).unwrap();
        store.save_checkpoint(&checkpoint).unwrap();

        let replacement = store
            .fork_legacy_checkpoint("mission-1", "mission-v2", Utc::now())
            .unwrap();

        assert_eq!(replacement.mission_id, "mission-v2");
        assert_eq!(replacement.status, MissionStatus::Pending);
        assert_eq!(
            store.get_mission("mission-1").unwrap().status,
            MissionStatus::Failed
        );
        assert!(matches!(
            store.get_checkpoint("mission-v2"),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn loop_run_round_trips_and_rejects_history_rewrite() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let now = Utc::now();
        let mut run = LoopRun {
            loop_run_id: "loop-1".to_string(),
            root_mission_id: "mission-1".to_string(),
            completion_policy: LoopCompletionPolicy {
                target_stage: LoopTargetStage::ShadowHealthy,
                max_research_missions: 2,
            },
            child_mission_ids: vec![],
            stage_records: vec![],
            status: LoopRunStatus::Pending,
            stop_reason: None,
            created_at: now,
            updated_at: now,
        };
        store.create_loop_run(&run).unwrap();
        run.start(now).unwrap();
        store.save_loop_run(&run).unwrap();
        assert_eq!(store.get_loop_run("loop-1").unwrap(), run);

        let mut rewritten = run.clone();
        rewritten.created_at = now + chrono::Duration::seconds(1);
        assert!(store.save_loop_run(&rewritten).is_err());
    }

    #[test]
    fn lineage_returns_candidate_artifact() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let iteration = iteration();
        let candidate = CandidateArtifact::Program(serde_json::json!({"op": "rank"}));
        store
            .append_iteration(&iteration, Some(("candidate-1", &candidate)), None)
            .unwrap();
        let lineage = store.mission_lineage("mission-1").unwrap();
        assert_eq!(lineage.iterations, vec![iteration]);
        assert_eq!(lineage.candidates[0].artifact, candidate);
    }

    #[test]
    fn nonce_is_durable_and_single_use() {
        let path = temp_db("nonce");
        AlphaStore::open(&path)
            .unwrap()
            .consume_nonce("nonce-1", Utc::now())
            .unwrap();
        let mut reopened = AlphaStore::open(&path).unwrap();
        assert!(reopened.nonce_consumed("nonce-1").unwrap());
        assert!(matches!(
            reopened.consume_nonce("nonce-1", Utc::now()),
            Err(StoreError::NonceReplay)
        ));
    }

    #[test]
    fn runtime_attribution_is_idempotent_and_links_back_to_mission() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let now = Utc::now();
        let (promotion, bundle) = persist_formula_promotion(&mut store, now);
        let signed = sign_envelope(
            DeploymentEnvelope {
                deployment_id: "deployment-1".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                promotion_id: "promotion-1".to_string(),
                promotion_manifest_hash: promotion.content_hash,
                bundle_id: bundle.bundle_id,
                bundle_hash: bundle.bundle_hash,
                runtime_config_hash: "c".repeat(64),
                risk_policy_hash: "d".repeat(64),
                account_id: "account-1".to_string(),
                venue: "binance".to_string(),
                instruments: vec!["BTCUSDT".to_string()],
                allowed_intent_types: vec![AllowedIntentType::StartPaper],
                max_notional: 100.0,
                max_symbol_exposure: 50.0,
                max_order_size: 10.0,
                max_slippage_bps: 2.0,
                valid_from: now - chrono::Duration::minutes(1),
                expires_at: now + chrono::Duration::minutes(1),
                nonce: "nonce-1".to_string(),
                approval_class: ApprovalClass::Paper,
                approval_signatures: vec!["approval-1".to_string()],
                payload_hash: String::new(),
            },
            "key-1",
            &SigningKey::from_bytes(&[7_u8; 32]),
        )
        .unwrap();
        store.store_deployment(&signed, now).unwrap();
        let event = RuntimeAttributionEvent {
            event_id: "attribution-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::Activation,
            strategy_id: None,
            order_id: None,
            account_id: None,
            venue: None,
            symbol: None,
            metrics: std::collections::BTreeMap::from([("pnl".to_string(), 1.0)]),
            reason: None,
            observed_at: now,
        };
        assert!(store
            .ingest_runtime_attribution(verified_attribution(event.clone()))
            .unwrap());
        assert!(!store
            .ingest_runtime_attribution(verified_attribution(event))
            .unwrap());
        assert_eq!(
            store
                .get_runtime_attribution("attribution-1")
                .unwrap()
                .mission_id
                .as_deref(),
            Some("mission-1")
        );
    }

    #[test]
    fn runtime_attribution_batch_is_atomic_and_binds_signed_scope() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let now = Utc::now();
        let (promotion, bundle) = persist_formula_promotion(&mut store, now);
        let signed = sign_envelope(
            DeploymentEnvelope {
                deployment_id: "deployment-batch".to_string(),
                asset_revision_id: "candidate-1".to_string(),
                promotion_id: promotion.record.promotion_id,
                promotion_manifest_hash: promotion.content_hash,
                bundle_id: bundle.bundle_id,
                bundle_hash: bundle.bundle_hash,
                runtime_config_hash: "c".repeat(64),
                risk_policy_hash: "d".repeat(64),
                account_id: "account-1".to_string(),
                venue: "binance".to_string(),
                instruments: vec!["BTCUSDT".to_string()],
                allowed_intent_types: vec![AllowedIntentType::StartPaper],
                max_notional: 100.0,
                max_symbol_exposure: 50.0,
                max_order_size: 10.0,
                max_slippage_bps: 2.0,
                valid_from: now - chrono::Duration::minutes(1),
                expires_at: now + chrono::Duration::minutes(1),
                nonce: "nonce-batch".to_string(),
                approval_class: ApprovalClass::Paper,
                approval_signatures: vec!["approval-1".to_string()],
                payload_hash: String::new(),
            },
            "key-1",
            &SigningKey::from_bytes(&[7_u8; 32]),
        )
        .unwrap();
        store.store_deployment(&signed, now).unwrap();
        let activation = RuntimeAttributionEvent {
            event_id: "batch-activation".to_string(),
            deployment_id: "deployment-batch".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Activated,
            kind: AttributionKind::Activation,
            strategy_id: None,
            order_id: None,
            account_id: None,
            venue: None,
            symbol: None,
            metrics: std::collections::BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        let forged_fill = RuntimeAttributionEvent {
            event_id: "batch-forged-fill".to_string(),
            deployment_id: "deployment-batch".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: Some("mission-1".to_string()),
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::Fill,
            strategy_id: Some("other-strategy".to_string()),
            order_id: Some("order-1".to_string()),
            account_id: Some("other-account".to_string()),
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            metrics: std::collections::BTreeMap::new(),
            reason: None,
            observed_at: now,
        };

        assert!(store
            .ingest_runtime_attributions(vec![
                verified_attribution(activation.clone()),
                verified_attribution(forged_fill),
            ])
            .is_err());
        assert!(matches!(
            store.get_runtime_attribution("batch-activation"),
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            store
                .ingest_runtime_attributions(vec![verified_attribution(activation)])
                .unwrap(),
            1
        );
        let stored = store.get_runtime_attribution("batch-activation").unwrap();
        assert_eq!(stored.mission_id.as_deref(), Some("mission-1"));
        assert_eq!(stored.account_id.as_deref(), Some("account-1"));
        assert_eq!(stored.venue.as_deref(), Some("binance"));
    }

    #[test]
    fn sealed_passed_candidate_is_queryable_for_loop_progression() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        persist_formula_promotion(&mut store, Utc::now());
        assert_eq!(
            store
                .sealed_passed_candidate_for_mission("mission-1")
                .unwrap()
                .as_deref(),
            Some("candidate-1")
        );
    }

    #[test]
    fn sealed_candidate_requires_the_canonical_evaluator_version() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let candidate = CandidateArtifact::Formula(FactorAst::Terminal(FactorTerminal::Field(
            "mid_price".to_string(),
        )));
        store
            .append_iteration(&iteration(), Some(("candidate-1", &candidate)), None)
            .unwrap();
        let candidate_hash = store.mission_lineage("mission-1").unwrap().candidates[0]
            .content_hash
            .clone();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: format!(
                    "sealed-evaluation:{}:candidate-1",
                    SEALED_HOLDOUT_EVALUATOR_VERSION
                ),
                registry_kind: "sealed_evaluation".to_string(),
                asset_id: "candidate-1".to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "mission_id": "mission-1",
                    "candidate_content_hash": candidate_hash,
                    "dataset_manifest_id": "dataset-1",
                    "evaluation": {
                        "passed": true,
                        "evaluator_version": "forged-evaluator-v99"
                    }
                }),
                created_at: Utc::now(),
            })
            .unwrap();

        assert_eq!(
            store
                .sealed_passed_candidate_for_mission("mission-1")
                .unwrap(),
            None
        );
    }

    #[test]
    fn search_policy_adopts_only_a_strict_validator_improvement() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let base = store
            .put_search_policy_revision(SearchPolicyRevision {
                revision_id: "policy-1".to_string(),
                parent_revision_id: None,
                policy: serde_json::json!({"engine": "gp"}),
                evidence_event_ids: vec![],
                validator_score: 1.0,
                adopted: false,
                rollback_reason: None,
                created_at: Utc::now(),
            })
            .unwrap();
        assert!(base.adopted);
        let worse = store
            .put_search_policy_revision(SearchPolicyRevision {
                revision_id: "policy-2".to_string(),
                parent_revision_id: Some("policy-1".to_string()),
                policy: serde_json::json!({"engine": "mcts"}),
                evidence_event_ids: vec!["attribution-1".to_string()],
                validator_score: 0.9,
                adopted: true,
                rollback_reason: None,
                created_at: Utc::now(),
            })
            .unwrap();
        assert!(!worse.adopted);
        let better = store
            .put_search_policy_revision(SearchPolicyRevision {
                revision_id: "policy-3".to_string(),
                parent_revision_id: Some("policy-1".to_string()),
                policy: serde_json::json!({"engine": "mcts"}),
                evidence_event_ids: vec!["attribution-1".to_string()],
                validator_score: 1.1,
                adopted: false,
                rollback_reason: None,
                created_at: Utc::now(),
            })
            .unwrap();
        assert!(better.adopted);
    }

    fn temp_db(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alpha-store-{prefix}-{nanos}.duckdb"))
    }
}
