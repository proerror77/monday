//! Transactional DuckDB source of truth for the Agentic Alpha control plane.

use alpha_domain::{
    canonical_json_hash, AllowedIntentType, AttributionKind, AttributionMode, CandidateArtifact,
    DeploymentEnvelope, EngineKind, LearningDirective, LiveSmallEligibilityEvidence, LoopRun,
    MissionStatus, MissionTerminalReason, PromotionRecord, ResearchIteration, ResearchMission,
    RuntimeAttributionEvent, SearchBudgetUsage, SearchPolicyRevision, SignedDeploymentEnvelope,
    StrategyBundle, StrategyBundleArtifact, SEALED_HOLDOUT_EVALUATOR_VERSION,
};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection, Transaction};
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
const INTEGRITY_KEY_ENV: &str = "ALPHA_STORE_INTEGRITY_KEY_HEX";
const INTEGRITY_KEY_BYTES: usize = 32;

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
        validate_iteration_records(iteration, candidate, evaluation)?;
        validate_checkpoint_for_iteration(checkpoint, iteration)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        let iteration_hash =
            insert_iteration_records(&transaction, iteration, candidate, evaluation)?;
        let checkpoint_hash = upsert_checkpoint(&transaction, checkpoint, &self.integrity_key)?;
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
        let transaction = self.connection.transaction().map_err(database_error)?;
        let hash = upsert_checkpoint(&transaction, checkpoint, &self.integrity_key)?;
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
        match self.get_checkpoint(mission_id) {
            Err(StoreError::LegacyCheckpoint | StoreError::MissingAuthenticityTag) => {}
            Ok(_) => {
                return Err(StoreError::Domain(
                    "checkpoint already has resumable engine state".to_string(),
                ))
            }
            Err(error) => return Err(error),
        }

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
                "reason": "legacy checkpoint lacks exact engine state and cannot be resumed safely",
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

    pub fn promote_candidate(
        &mut self,
        bundle: &StrategyBundle,
        promotion: &PromotionRecord,
    ) -> Result<StoredPromotion, StoreError> {
        bundle.validate().map_err(domain_error)?;
        promotion.validate(bundle).map_err(domain_error)?;

        let (candidate_mission_id, candidate_json, candidate_hash) = self
            .connection
            .query_row(
                "SELECT mission_id, payload_json, content_hash FROM candidate_artifacts WHERE candidate_id = ?",
                params![bundle.candidate_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
            )
            .map_err(map_query_error)?;
        verify_hash(&candidate_json, &candidate_hash)?;
        let candidate_artifact: CandidateArtifact =
            serde_json::from_str(&candidate_json).map_err(serialization_error)?;
        let expected_bundle_artifact = candidate_artifact
            .to_strategy_bundle_artifact()
            .map_err(domain_error)?;
        if candidate_mission_id != promotion.mission_id
            || candidate_hash != bundle.candidate_content_hash
            || expected_bundle_artifact != bundle.artifact
        {
            return Err(StoreError::Domain(
                "promotion candidate binding does not match stored truth".to_string(),
            ));
        }
        let mission = self.get_mission(&promotion.mission_id)?;
        if mission.dataset_manifest_id != bundle.dataset_manifest_id {
            return Err(StoreError::Domain(
                "promotion dataset does not match mission".to_string(),
            ));
        }
        let sealed = self.get_registry_revision(&promotion.sealed_evaluation_id)?;
        let sealed_evaluation = sealed
            .payload
            .get("evaluation")
            .ok_or_else(|| StoreError::Domain("sealed evaluation payload is incomplete".into()))?;
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
        let evaluation_hash = canonical_json_hash(sealed_evaluation).map_err(domain_error)?;
        if sealed.registry_kind != "sealed_evaluation"
            || sealed.asset_id != bundle.candidate_id
            || stored_mission_id != Some(promotion.mission_id.as_str())
            || stored_candidate_hash != Some(bundle.candidate_content_hash.as_str())
            || stored_dataset != Some(bundle.dataset_manifest_id.as_str())
            || stored_evaluator_version != Some(bundle.evaluator_version.as_str())
            || evaluation_hash != bundle.sealed_evaluation_hash
        {
            return Err(StoreError::Domain(
                "promotion evidence does not match sealed evaluation".to_string(),
            ));
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
        record.validate(&bundle).map_err(domain_error)?;
        Ok(StoredPromotion {
            record,
            content_hash,
        })
    }

    pub fn validate_deployment_binding(
        &self,
        envelope: &DeploymentEnvelope,
    ) -> Result<(StoredPromotion, StrategyBundle), StoreError> {
        let promotion = self.get_promotion(&envelope.promotion_id)?;
        let bundle = self.get_strategy_bundle(&envelope.bundle_id)?;
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
        event: RuntimeAttributionEvent,
    ) -> Result<bool, StoreError> {
        Ok(self.ingest_runtime_attributions(vec![event])? == 1)
    }

    pub fn ingest_runtime_attributions(
        &mut self,
        events: Vec<RuntimeAttributionEvent>,
    ) -> Result<usize, StoreError> {
        if events.is_empty() {
            return Err(StoreError::Domain(
                "runtime attribution batch cannot be empty".to_string(),
            ));
        }
        let transaction = self.connection.transaction().map_err(database_error)?;
        let mut inserted = 0_usize;
        for event in events {
            inserted += usize::from(ingest_runtime_attribution(&transaction, event)?);
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
                || revision
                    .payload
                    .get("evaluation")
                    .and_then(|evaluation| evaluation.get("passed"))
                    .and_then(serde_json::Value::as_bool)
                    != Some(true)
            {
                continue;
            }
            let candidate = self
                .connection
                .query_row(
                    "SELECT payload_json, content_hash FROM candidate_artifacts
                     WHERE candidate_id = ? AND mission_id = ?",
                    params![revision.asset_id, mission_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(map_query_error);
            let (candidate_json, candidate_hash) = match candidate {
                Ok(candidate) => candidate,
                Err(StoreError::NotFound) => continue,
                Err(error) => return Err(error),
            };
            verify_hash(&candidate_json, &candidate_hash)?;
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
            let evaluator_version_matches = revision
                .payload
                .get("evaluation")
                .and_then(|evaluation| evaluation.get("evaluator_version"))
                .and_then(serde_json::Value::as_str)
                == Some(SEALED_HOLDOUT_EVALUATOR_VERSION);
            if candidate_hash_matches && dataset_matches && evaluator_version_matches {
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

        let approvals = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT payload_json, content_hash FROM approvals
                     WHERE approval_class = 'human_live_small' AND subject_id = ?
                     ORDER BY created_at DESC, approval_id DESC",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map(params![promotion.promotion_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(database_error)?;
            let mut approvals = Vec::new();
            for row in rows {
                let (json, hash) = row.map_err(database_error)?;
                verify_hash(&json, &hash)?;
                approvals.push(
                    serde_json::from_str::<ApprovalRecord>(&json).map_err(serialization_error)?,
                );
            }
            approvals
        };
        for approval in approvals {
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

    pub fn get_approval(&self, approval_id: &str) -> Result<ApprovalRecord, StoreError> {
        read_json_row(
            &self.connection,
            "SELECT payload_json, content_hash FROM approvals WHERE approval_id = ?",
            approval_id,
        )
    }

    pub fn revoke_approval(
        &mut self,
        approval_id: &str,
        revoked_by: &str,
        reason: &str,
        at: DateTime<Utc>,
    ) -> Result<ApprovalRecord, StoreError> {
        require_text(revoked_by)?;
        require_text(reason)?;
        let mut approval = self.get_approval(approval_id)?;
        if approval.revoked_at.is_some() {
            return Err(StoreError::Domain(
                "approval is already revoked".to_string(),
            ));
        }
        approval.revoked_at = Some(at);
        approval.revoked_by = Some(revoked_by.to_string());
        approval.revocation_reason = Some(reason.to_string());
        approval.validate()?;
        let (json, hash) = encoded(&approval)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        transaction
            .execute(
                "UPDATE approvals SET payload_json = ?, content_hash = ?, revoked_at = ?, revoked_by = ?, revocation_reason = ? WHERE approval_id = ?",
                params![json, hash, at.to_rfc3339(), revoked_by, reason, approval_id],
            )
            .map_err(database_error)?;
        append_journal(
            &transaction,
            None,
            "approval_revoked",
            approval_id,
            &hash,
            at,
        )?;
        transaction.commit().map_err(database_error)?;
        Ok(approval)
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
) -> Result<String, StoreError> {
    require_text(&checkpoint.mission_id)?;
    if checkpoint.engine_version == 0 {
        return Err(StoreError::CheckpointMismatch);
    }
    ensure_present(
        transaction,
        "missions",
        "mission_id",
        &checkpoint.mission_id,
    )?;
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

fn load_or_create_integrity_key(path: &Path) -> Result<[u8; INTEGRITY_KEY_BYTES], StoreError> {
    if let Some(value) = std::env::var_os(INTEGRITY_KEY_ENV) {
        let value = value.into_string().map_err(|_| {
            StoreError::Database(format!("{INTEGRITY_KEY_ENV} must be valid UTF-8 hex"))
        })?;
        let bytes = hex::decode(value.trim()).map_err(|_| {
            StoreError::Database(format!("{INTEGRITY_KEY_ENV} must be 32-byte hex"))
        })?;
        return bytes
            .try_into()
            .map_err(|_| StoreError::Database(format!("{INTEGRITY_KEY_ENV} must be 32-byte hex")));
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
        canonical_json_hash, sign_envelope, AllowedIntentType, ApprovalClass, AttributionKind,
        AttributionMode, AttributionOutcome, DeploymentEnvelope, EngineKind, IterationVerdict,
        LoopCompletionPolicy, LoopRunStatus, LoopTargetStage, MissionCompletionPolicy,
        MissionStatus, PromotionRecord, RuntimeAttributionEvent, SearchBudget,
        SearchPolicyRevision, StrategyBundle, StrategyBundleArtifact, ValidatorMode,
    };
    use ed25519_dalek::SigningKey;
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_research_manifest::ManifestId;
    use std::time::{SystemTime, UNIX_EPOCH};

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

    fn persist_formula_promotion(
        store: &mut AlphaStore,
        now: DateTime<Utc>,
    ) -> (StoredPromotion, StrategyBundle) {
        let candidate = CandidateArtifact::Formula(FactorAst::Terminal(FactorTerminal::Field(
            "signal".to_string(),
        )));
        store
            .append_iteration(&iteration(), Some(("candidate-1", &candidate)), None)
            .unwrap();
        let candidate_hash = store.mission_lineage("mission-1").unwrap().candidates[0]
            .content_hash
            .clone();
        let evaluation = serde_json::json!({
            "passed": true,
            "score": 1.0,
            "failure_reasons": [],
            "evaluator_version": "sealed-holdout-v1",
        });
        let sealed = RegistryRevision {
            revision_id: "sealed-evaluation:candidate-1".to_string(),
            registry_kind: "sealed_evaluation".to_string(),
            asset_id: "candidate-1".to_string(),
            parent_revision_id: None,
            payload: serde_json::json!({
                "mission_id": "mission-1",
                "candidate_content_hash": candidate_hash,
                "dataset_manifest_id": "dataset-1",
                "evaluation": evaluation,
            }),
            created_at: now,
        };
        store.put_registry_revision(&sealed).unwrap();
        let evaluation_hash = canonical_json_hash(&evaluation).unwrap();
        let bundle = StrategyBundle::new(
            "bundle:candidate-1".to_string(),
            "candidate-1".to_string(),
            candidate_hash.clone(),
            ManifestId::new("dataset-1").unwrap(),
            "sealed-holdout-v1".to_string(),
            evaluation_hash.clone(),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(FactorTerminal::Field("signal".to_string())),
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
            evaluator_version: "sealed-holdout-v1".to_string(),
            sealed_evaluation_id: sealed.revision_id,
            sealed_evaluation_hash: evaluation_hash,
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            created_at: now,
        };
        let stored = store.promote_candidate(&bundle, &promotion).unwrap();
        (stored, bundle)
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
            "signal".to_string(),
        )));
        store
            .append_iteration(&iteration(), Some(("candidate-1", &candidate)), None)
            .unwrap();
        let now = Utc::now();
        let evaluation = serde_json::json!({
            "passed": true,
            "score": 1.0,
            "failure_reasons": [],
            "evaluator_version": "sealed-holdout-v1",
        });
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: "sealed-evaluation:candidate-1".to_string(),
                registry_kind: "sealed_evaluation".to_string(),
                asset_id: "candidate-1".to_string(),
                parent_revision_id: None,
                payload: serde_json::json!({
                    "mission_id": "mission-1",
                    "candidate_content_hash": "c".repeat(64),
                    "dataset_manifest_id": "dataset-1",
                    "evaluation": evaluation,
                }),
                created_at: now,
            })
            .unwrap();
        let evaluation_hash = canonical_json_hash(&evaluation).unwrap();
        let bundle = StrategyBundle::new(
            "bundle:bad".to_string(),
            "candidate-1".to_string(),
            "c".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            "sealed-holdout-v1".to_string(),
            evaluation_hash.clone(),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(FactorTerminal::Field("signal".to_string())),
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
            evaluator_version: "sealed-holdout-v1".to_string(),
            sealed_evaluation_id: "sealed-evaluation:candidate-1".to_string(),
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
        assert!(store.get_approval("approval-1").unwrap().is_active_at(now));

        store
            .revoke_approval(
                "approval-1",
                "reviewer-2",
                "withdrawn",
                now + chrono::Duration::seconds(1),
            )
            .unwrap();
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
            engine_kind: iteration.engine.clone(),
            engine_version: 1,
            engine_state: serde_json::json!({"mode": "test"}),
            updated_at: iteration.created_at,
        };
        {
            let mut store = AlphaStore::open(&path).unwrap();
            store.create_mission(&mission()).unwrap();
            store.append_iteration(&iteration, None, None).unwrap();
            store.save_checkpoint(&checkpoint).unwrap();
        }
        let reopened = AlphaStore::open(&path).unwrap();
        assert_eq!(reopened.get_checkpoint("mission-1").unwrap(), checkpoint);
    }

    #[test]
    fn iteration_and_checkpoint_commit_atomically() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let iteration = iteration_without_candidate();
        let checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
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
        let iteration = iteration_without_candidate();
        store.append_iteration(&iteration, None, None).unwrap();
        let checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
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
        let iteration = iteration_without_candidate();
        store.append_iteration(&iteration, None, None).unwrap();
        let mut checkpoint = RunCheckpoint {
            mission_id: iteration.mission_id.clone(),
            last_iteration_id: Some(iteration.iteration_id.clone()),
            budget_usage: iteration.budget_usage.clone(),
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
            let iteration = iteration_without_candidate();
            store.append_iteration(&iteration, None, None).unwrap();
            store
                .save_checkpoint(&RunCheckpoint {
                    mission_id: iteration.mission_id.clone(),
                    last_iteration_id: Some(iteration.iteration_id.clone()),
                    budget_usage: iteration.budget_usage.clone(),
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
        assert!(store.ingest_runtime_attribution(event.clone()).unwrap());
        assert!(!store.ingest_runtime_attribution(event).unwrap());
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
            .ingest_runtime_attributions(vec![activation.clone(), forged_fill])
            .is_err());
        assert!(matches!(
            store.get_runtime_attribution("batch-activation"),
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            store.ingest_runtime_attributions(vec![activation]).unwrap(),
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
            "signal".to_string(),
        )));
        store
            .append_iteration(&iteration(), Some(("candidate-1", &candidate)), None)
            .unwrap();
        let candidate_hash = store.mission_lineage("mission-1").unwrap().candidates[0]
            .content_hash
            .clone();
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: "sealed-evaluation:candidate-1".to_string(),
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
