//! Transactional DuckDB source of truth for the Agentic Alpha control plane.

use alpha_domain::{
    CandidateArtifact, ResearchIteration, ResearchMission, SearchBudgetUsage,
    SignedDeploymentEnvelope,
};
use chrono::{DateTime, Utc};
use duckdb::{params, Connection, Transaction};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MIGRATION_001: &str = include_str!("../migrations/001_control_plane.sql");

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCheckpoint {
    pub mission_id: String,
    pub last_iteration_id: Option<String>,
    pub budget_usage: SearchBudgetUsage,
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
    pub created_at: DateTime<Utc>,
}

pub struct AlphaStore {
    path: PathBuf,
    connection: Connection,
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
        let mut store = Self {
            path: path.to_path_buf(),
            connection,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory().map_err(database_error)?;
        let mut store = Self {
            path: PathBuf::from(":memory:"),
            connection,
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

    pub fn append_iteration(
        &mut self,
        iteration: &ResearchIteration,
        candidate: Option<(&str, &CandidateArtifact)>,
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
        let (iteration_json, iteration_hash) = encoded(iteration)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        ensure_present(
            &transaction,
            "missions",
            "mission_id",
            &iteration.mission_id,
        )?;
        ensure_absent(
            &transaction,
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
                &transaction,
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

    pub fn save_checkpoint(&mut self, checkpoint: &RunCheckpoint) -> Result<(), StoreError> {
        require_text(&checkpoint.mission_id)?;
        let budget_json =
            serde_json::to_string(&checkpoint.budget_usage).map_err(serialization_error)?;
        let (_, hash) = encoded(checkpoint)?;
        let transaction = self.connection.transaction().map_err(database_error)?;
        ensure_present(
            &transaction,
            "missions",
            "mission_id",
            &checkpoint.mission_id,
        )?;
        transaction
            .execute(
                "INSERT INTO checkpoints VALUES (?, ?, ?, ?)
                 ON CONFLICT (mission_id) DO UPDATE SET
                    last_iteration_id = excluded.last_iteration_id,
                    budget_usage_json = excluded.budget_usage_json,
                    updated_at = excluded.updated_at",
                params![
                    checkpoint.mission_id,
                    checkpoint.last_iteration_id,
                    budget_json,
                    checkpoint.updated_at.to_rfc3339()
                ],
            )
            .map_err(database_error)?;
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
                "SELECT last_iteration_id, budget_usage_json, updated_at FROM checkpoints WHERE mission_id = ?",
                params![mission_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(map_query_error)
            .and_then(|(last_iteration_id, budget_json, updated_at)| {
                Ok(RunCheckpoint {
                    mission_id: mission_id.to_string(),
                    last_iteration_id,
                    budget_usage: serde_json::from_str(&budget_json).map_err(serialization_error)?,
                    updated_at: DateTime::parse_from_rfc3339(&updated_at)
                        .map_err(|error| StoreError::Serialization(error.to_string()))?
                        .with_timezone(&Utc),
                })
            })
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
        Ok(MissionLineage {
            mission,
            iterations,
            candidates,
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

    pub fn record_approval(&mut self, approval: &ApprovalRecord) -> Result<(), StoreError> {
        require_text(&approval.approval_id)?;
        require_text(&approval.approval_class)?;
        require_text(&approval.subject_id)?;
        self.insert_json_record(
            "approvals",
            &approval.approval_id,
            approval,
            (None, "approval_recorded", approval.created_at),
            |transaction, json, hash| {
                transaction.execute(
                    "INSERT INTO approvals VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        approval.approval_id,
                        approval.approval_class,
                        approval.subject_id,
                        json,
                        hash,
                        approval.created_at.to_rfc3339()
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
    use alpha_domain::{EngineKind, IterationVerdict, MissionStatus, SearchBudget, ValidatorMode};
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
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
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

    #[test]
    fn migrations_are_idempotent() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.migrate().unwrap();
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
            .append_iteration(&iteration, Some(("candidate-1", &candidate)))
            .unwrap();
        assert!(matches!(
            store.append_iteration(&iteration, Some(("candidate-1", &candidate))),
            Err(StoreError::DuplicateRecord)
        ));
    }

    #[test]
    fn checkpoint_survives_reopen() {
        let path = temp_db("checkpoint");
        let checkpoint = RunCheckpoint {
            mission_id: "mission-1".to_string(),
            last_iteration_id: Some("iteration-1".to_string()),
            budget_usage: SearchBudgetUsage {
                candidates: 1,
                expansions: 4,
                tokens: 0,
                elapsed_ms: 25,
            },
            updated_at: Utc::now(),
        };
        {
            let mut store = AlphaStore::open(&path).unwrap();
            store.create_mission(&mission()).unwrap();
            store
                .append_iteration(&iteration_without_candidate(), None)
                .unwrap();
            store.save_checkpoint(&checkpoint).unwrap();
        }
        let reopened = AlphaStore::open(&path).unwrap();
        assert_eq!(reopened.get_checkpoint("mission-1").unwrap(), checkpoint);
    }

    #[test]
    fn lineage_returns_candidate_artifact() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let iteration = iteration();
        let candidate = CandidateArtifact::Program(serde_json::json!({"op": "rank"}));
        store
            .append_iteration(&iteration, Some(("candidate-1", &candidate)))
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

    fn temp_db(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("alpha-store-{prefix}-{nanos}.duckdb"))
    }
}
