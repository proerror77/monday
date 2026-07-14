use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn default_json_object() -> serde_json::Value {
    serde_json::json!({})
}

fn default_json_array() -> serde_json::Value {
    serde_json::json!([])
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactorLifecycleStatus {
    Draft,
    Compiled,
    Evaluated,
    Candidate,
    DryRun,
    Approved,
    Production,
    Deprecated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorRegistryEntry {
    pub factor_id: String,
    pub factor_name: String,
    pub factor_family: String,
    pub status: FactorLifecycleStatus,
    pub hypothesis: String,
    pub economic_logic: String,
    pub dsl_source: String,
    pub dsl_hash: String,
    pub ast_json: serde_json::Value,
    #[serde(default)]
    pub runtime_contract: serde_json::Value,
    pub target: String,
    pub horizon: String,
    pub created_by_agent: String,
    pub created_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactorEvaluationRecord {
    pub eval_id: String,
    pub factor_id: String,
    pub run_id: String,
    pub data_snapshot_id: String,
    pub dataset_start_ts: DateTime<Utc>,
    pub dataset_end_ts: DateTime<Utc>,
    pub evidence_stage: String,
    pub evaluation_kind: String,
    pub candidate_replay_id: Option<String>,
    pub evaluator_version: String,
    #[serde(default)]
    pub runtime_contract: serde_json::Value,
    pub passed_gate: bool,
    pub promotion_decision: String,
    pub promotion_status: String,
    #[serde(default)]
    pub blockers_json: serde_json::Value,
    pub rejection_reason: Option<String>,
    pub metrics_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateReplayTapeRecord {
    pub candidate_replay_id: String,
    pub run_id: String,
    pub source_workflow: String,
    pub workflow_run_id: Option<String>,
    pub workflow_run_url: Option<String>,
    pub artifact_name: Option<String>,
    pub artifact_sha256: String,
    pub artifact_json: serde_json::Value,
    pub basis: String,
    pub evidence_stage: String,
    pub deployment_id: Option<String>,
    pub strategy_profile: String,
    pub runtime_score: String,
    pub data_snapshot_id: Option<String>,
    pub dsl_hash: Option<String>,
    pub target: Option<String>,
    pub horizon: Option<String>,
    pub recording_path: Option<String>,
    pub recording_sha256: Option<String>,
    pub config_path: Option<String>,
    pub config_sha256: Option<String>,
    pub runner_source: Option<String>,
    pub runner_git_sha: Option<String>,
    pub replay_window_start_ts: Option<DateTime<Utc>>,
    pub replay_window_end_ts: Option<DateTime<Utc>>,
    #[serde(default = "default_json_object")]
    pub decision_contract_json: serde_json::Value,
    #[serde(default = "default_json_object")]
    pub acceptance_criteria_json: serde_json::Value,
    #[serde(default = "default_json_object")]
    pub metrics_json: serde_json::Value,
    #[serde(default = "default_json_array")]
    pub blocking_risk_flags_json: serde_json::Value,
    pub promotion_ready: bool,
    pub promotion_decision: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FullDepthExecutionSurfaceRecord {
    pub full_depth_execution_surface_id: String,
    pub run_id: String,
    pub source_workflow: String,
    pub workflow_run_id: Option<String>,
    pub workflow_run_url: Option<String>,
    pub artifact_name: Option<String>,
    pub artifact_sha256: String,
    pub artifact_json: serde_json::Value,
    pub schema_version: String,
    pub surface: String,
    pub source: String,
    pub data_snapshot_id: Option<String>,
    pub window_start_ts: DateTime<Utc>,
    pub window_end_ts: DateTime<Utc>,
    pub checked_hours: i32,
    pub existing_hours: i32,
    pub exported_hours: i32,
    pub row_count: i64,
    pub full_fidelity: bool,
    pub incomplete: bool,
    pub valid: bool,
    #[serde(default = "default_json_array")]
    pub blockers_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentTraceRecord {
    pub trace_id: String,
    pub run_id: String,
    pub parent_trace_id: Option<String>,
    pub event_type: String,
    pub data_snapshot_id: Option<String>,
    pub dsl_hash: Option<String>,
    pub candidate_replay_id: Option<String>,
    pub full_depth_execution_surface_id: Option<String>,
    pub artifact_kind: String,
    pub evidence_stage: String,
    pub promotion_decision: Option<String>,
    pub agent_name: String,
    pub input_json: serde_json::Value,
    pub output_json: serde_json::Value,
    pub hash_prev: Option<String>,
    pub hash_current: String,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_lifecycle_status_uses_snake_case_contract() {
        let raw = serde_json::to_string(&FactorLifecycleStatus::DryRun).expect("serialize");
        assert_eq!(raw, "\"dry_run\"");
        let parsed: FactorLifecycleStatus = serde_json::from_str("\"candidate\"").expect("parse");
        assert_eq!(parsed, FactorLifecycleStatus::Candidate);
    }

    #[test]
    fn candidate_replay_tape_record_has_default_json_contracts() {
        let raw = r#"{
            "candidate_replay_id": "candidate_replay:abc",
            "run_id": "123",
            "source_workflow": "runtime-candidate-replay.yml",
            "workflow_run_id": null,
            "workflow_run_url": null,
            "artifact_name": null,
            "artifact_sha256": "abc",
            "artifact_json": {},
            "basis": "runtime_market_update_replay",
            "evidence_stage": "executable_replay",
            "deployment_id": null,
            "strategy_profile": "settlement_probability",
            "runtime_score": "autofactor_formula:x",
            "data_snapshot_id": null,
            "dsl_hash": null,
            "target": null,
            "horizon": null,
            "recording_path": null,
            "recording_sha256": null,
            "config_path": null,
            "config_sha256": null,
            "runner_source": null,
            "runner_git_sha": null,
            "replay_window_start_ts": null,
            "replay_window_end_ts": null,
            "promotion_ready": false,
            "promotion_decision": "blocked",
            "created_at": "2026-05-23T00:00:00Z"
        }"#;
        let parsed: CandidateReplayTapeRecord = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.decision_contract_json, serde_json::json!({}));
        assert_eq!(parsed.blocking_risk_flags_json, serde_json::json!([]));
        assert_eq!(parsed.candidate_replay_id, "candidate_replay:abc");
    }
}
