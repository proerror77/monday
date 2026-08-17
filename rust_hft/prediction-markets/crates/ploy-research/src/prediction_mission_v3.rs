use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::prediction_loop::{
    current_prediction_policy_snapshot_id, validate_prediction_search_budget, validate_sha256_id,
    PredictionSearchBudget,
};
use crate::prediction_loop_fs::{canonical_json_bytes, sha256_hex};
use crate::research_snapshot::{
    AuthenticatedResearchSnapshot, POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND,
};

// The sealed identity fields are wire-incompatible with the former v3 Mission
// document. Keep the adapter name stable for its #324 consumer, but require a
// distinct wire version instead of silently reinterpreting legacy Missions.
pub const PREDICTION_MISSION_V3_SCHEMA_VERSION: &str = "prediction_research_mission.v4";
pub const PREDICTION_MISSION_V3_CHECKPOINT_SCHEMA_VERSION: &str =
    "prediction_research_checkpoint.v4";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredictionProductSymbol {
    #[serde(rename = "BTC")]
    Btc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionProductIdentity {
    pub symbol: PredictionProductSymbol,
    pub event_horizon_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionTaskKind {
    SettlementProbability,
    UpExecution,
    DownExecution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionTokenSide {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMissionTask {
    pub kind: PredictionTaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side: Option<PredictionTokenSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction_horizon_secs: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionRunMode {
    PipelineSmoke,
    ResearchTrial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionAuthorityProfile {
    PolymarketChainlinkBaseline,
    PolymarketChainlinkBinance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredictionMissionCapability {
    PolymarketChainlink,
    BinanceContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionResearchMissionV3 {
    pub schema_version: String,
    pub mission_id: String,
    pub product: PredictionProductIdentity,
    pub task: PredictionMissionTask,
    pub run_mode: PredictionRunMode,
    pub authority_profile: PredictionAuthorityProfile,
    pub required_capabilities: BTreeSet<PredictionMissionCapability>,
    pub cohort_manifest_id: String,
    pub partition_digest: String,
    pub causal_projection_policy_id: String,
    pub snapshot_contract_id: String,
    pub snapshot_hash: String,
    pub search_policy_snapshot_id: String,
    pub search_budget: PredictionSearchBudget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmittedPredictionTask {
    SettlementProbability,
    UpExecution { prediction_horizon_secs: u32 },
    DownExecution { prediction_horizon_secs: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthenticatedDigest(String);

/// Opaque admission evidence produced only after independently rehashing the
/// cohort, snapshot, and policy inputs. Issue #320 owns that constructor; until
/// it lands, external callers cannot manufacture this authenticated handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedPredictionMissionV3Inputs {
    task: PredictionMissionTask,
    authority_profile: PredictionAuthorityProfile,
    capabilities: BTreeSet<PredictionMissionCapability>,
    cohort_manifest_id: AuthenticatedDigest,
    partition_digest: AuthenticatedDigest,
    causal_projection_policy_id: AuthenticatedDigest,
    snapshot_contract_id: AuthenticatedDigest,
    snapshot_hash: AuthenticatedDigest,
    search_policy_snapshot_id: AuthenticatedDigest,
    source_kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PredictionMissionV3CheckpointIdentity {
    pub schema_version: String,
    pub mission_sha256: String,
    pub cohort_manifest_id: String,
    pub partition_digest: String,
    pub causal_projection_policy_id: String,
    pub snapshot_contract_id: String,
    pub snapshot_hash: String,
    pub search_policy_snapshot_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedPredictionMissionV3 {
    pub mission_id: String,
    pub mission_sha256: String,
    pub product: PredictionProductIdentity,
    pub task: AdmittedPredictionTask,
    pub run_mode: PredictionRunMode,
    pub authority_profile: PredictionAuthorityProfile,
    pub cohort_manifest_id: String,
    pub partition_digest: String,
    pub causal_projection_policy_id: String,
    pub snapshot_contract_id: String,
    pub snapshot_hash: String,
    pub search_policy_snapshot_id: String,
}

impl AdmittedPredictionMissionV3 {
    pub fn checkpoint_identity(&self) -> PredictionMissionV3CheckpointIdentity {
        PredictionMissionV3CheckpointIdentity {
            schema_version: PREDICTION_MISSION_V3_CHECKPOINT_SCHEMA_VERSION.to_string(),
            mission_sha256: self.mission_sha256.clone(),
            cohort_manifest_id: self.cohort_manifest_id.clone(),
            partition_digest: self.partition_digest.clone(),
            causal_projection_policy_id: self.causal_projection_policy_id.clone(),
            snapshot_contract_id: self.snapshot_contract_id.clone(),
            snapshot_hash: self.snapshot_hash.clone(),
            search_policy_snapshot_id: self.search_policy_snapshot_id.clone(),
        }
    }
}

pub fn parse_prediction_mission_json(bytes: &[u8]) -> Result<PredictionResearchMissionV3, String> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("parse prediction mission JSON: {error}"))?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "prediction mission schema_version is required".to_string())?;
    if schema_version != PREDICTION_MISSION_V3_SCHEMA_VERSION {
        return Err(format!(
            "unsupported prediction mission schema_version {schema_version}"
        ));
    }
    serde_json::from_value(value).map_err(|error| format!("parse prediction Mission v4: {error}"))
}

pub fn prediction_mission_v3_sha256(
    mission: &PredictionResearchMissionV3,
) -> Result<String, String> {
    Ok(format!(
        "sha256:{}",
        sha256_hex(&canonical_json_bytes(mission)?)
    ))
}

pub fn validate_prediction_mission_v3(mission: &PredictionResearchMissionV3) -> Result<(), String> {
    if mission.schema_version != PREDICTION_MISSION_V3_SCHEMA_VERSION {
        return Err(format!(
            "mission.schema_version must be {PREDICTION_MISSION_V3_SCHEMA_VERSION}"
        ));
    }
    if mission.mission_id.trim().is_empty() || mission.mission_id.trim() != mission.mission_id {
        return Err("mission.mission_id must be a trimmed non-empty string".to_string());
    }
    if mission.product.symbol != PredictionProductSymbol::Btc
        || mission.product.event_horizon_secs != 300
    {
        return Err("Mission v4 currently supports only BTC x 300 seconds".to_string());
    }
    validate_task(&mission.task)?;
    validate_sha256_id(&mission.cohort_manifest_id, "mission.cohort_manifest_id")?;
    validate_sha256_id(&mission.partition_digest, "mission.partition_digest")?;
    validate_sha256_id(
        &mission.causal_projection_policy_id,
        "mission.causal_projection_policy_id",
    )?;
    validate_sha256_id(
        &mission.snapshot_contract_id,
        "mission.snapshot_contract_id",
    )?;
    validate_snapshot_hash(&mission.snapshot_hash)?;
    validate_sha256_id(
        &mission.search_policy_snapshot_id,
        "mission.search_policy_snapshot_id",
    )?;
    validate_prediction_search_budget(&mission.search_budget)?;
    if mission.run_mode == PredictionRunMode::PipelineSmoke
        && mission.search_budget.max_candidates != 0
    {
        return Err("pipeline_smoke search budget must not request candidates".into());
    }
    let baseline = BTreeSet::from([PredictionMissionCapability::PolymarketChainlink]);
    let full = BTreeSet::from([
        PredictionMissionCapability::PolymarketChainlink,
        PredictionMissionCapability::BinanceContext,
    ]);
    let expected = match mission.authority_profile {
        PredictionAuthorityProfile::PolymarketChainlinkBaseline => &baseline,
        PredictionAuthorityProfile::PolymarketChainlinkBinance => &full,
    };
    if &mission.required_capabilities != expected {
        return Err("mission capabilities do not match its authority profile".to_string());
    }
    Ok(())
}

/// Seal a Mission v4 admission request to the identities independently read
/// from a verified snapshot cache entry. Callers can provide a Mission, but
/// cannot manufacture the resulting admission evidence from raw strings.
pub fn authenticate_prediction_mission_v3_inputs(
    snapshot: &AuthenticatedResearchSnapshot,
    mission: &PredictionResearchMissionV3,
) -> Result<AuthenticatedPredictionMissionV3Inputs, String> {
    validate_prediction_mission_v3(mission)?;
    if mission.authority_profile != PredictionAuthorityProfile::PolymarketChainlinkBaseline
        || snapshot.source_kind() != POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND
    {
        return Err(
            "authenticated snapshot source does not match Mission v4 authority profile".to_string(),
        );
    }
    let current_policy = current_prediction_policy_snapshot_id();
    if snapshot.causal_projection_policy_id() != current_policy {
        return Err(
            "authenticated snapshot has a stale causal projection policy identity".to_string(),
        );
    }
    for (identity, field) in [
        (
            snapshot.cohort_manifest_id(),
            "authenticated cohort manifest",
        ),
        (
            snapshot.partition_digest(),
            "authenticated partition digest",
        ),
        (
            snapshot.causal_projection_policy_id(),
            "authenticated causal projection policy",
        ),
        (
            snapshot.snapshot_contract_id(),
            "authenticated snapshot contract",
        ),
    ] {
        validate_sha256_id(identity, field)?;
    }
    validate_snapshot_hash(snapshot.snapshot_hash())?;
    Ok(AuthenticatedPredictionMissionV3Inputs {
        task: mission.task.clone(),
        authority_profile: mission.authority_profile,
        capabilities: mission.required_capabilities.clone(),
        cohort_manifest_id: AuthenticatedDigest(snapshot.cohort_manifest_id().to_string()),
        partition_digest: AuthenticatedDigest(snapshot.partition_digest().to_string()),
        causal_projection_policy_id: AuthenticatedDigest(
            snapshot.causal_projection_policy_id().to_string(),
        ),
        snapshot_contract_id: AuthenticatedDigest(snapshot.snapshot_contract_id().to_string()),
        snapshot_hash: AuthenticatedDigest(snapshot.snapshot_hash().to_string()),
        search_policy_snapshot_id: AuthenticatedDigest(current_policy),
        source_kind: snapshot.source_kind().to_string(),
    })
}

fn validate_snapshot_hash(value: &str) -> Result<(), String> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(
            "mission.snapshot_hash must use a 16-character lowercase hexadecimal content hash"
                .to_string(),
        );
    }
    Ok(())
}

pub fn admit_prediction_mission_v3(
    mission: &PredictionResearchMissionV3,
    authenticated: &AuthenticatedPredictionMissionV3Inputs,
    checkpoint: Option<&PredictionMissionV3CheckpointIdentity>,
) -> Result<AdmittedPredictionMissionV3, String> {
    validate_prediction_mission_v3(mission)?;
    if authenticated.task != mission.task {
        return Err("authenticated task does not match Mission v4".to_string());
    }
    if authenticated.authority_profile != mission.authority_profile {
        return Err("authenticated authority profile does not match Mission v4".to_string());
    }
    if authenticated.capabilities != mission.required_capabilities {
        return Err("authenticated capabilities do not match Mission v4".to_string());
    }
    if mission.authority_profile != PredictionAuthorityProfile::PolymarketChainlinkBaseline
        || authenticated.source_kind != POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND
    {
        return Err(
            "authenticated snapshot source does not match Mission v4 authority profile".to_string(),
        );
    }
    for (actual, expected, field) in [
        (
            &authenticated.cohort_manifest_id.0,
            &mission.cohort_manifest_id,
            "cohort manifest",
        ),
        (
            &authenticated.partition_digest.0,
            &mission.partition_digest,
            "partition",
        ),
        (
            &authenticated.causal_projection_policy_id.0,
            &mission.causal_projection_policy_id,
            "causal projection policy",
        ),
        (
            &authenticated.snapshot_contract_id.0,
            &mission.snapshot_contract_id,
            "snapshot contract",
        ),
        (
            &authenticated.snapshot_hash.0,
            &mission.snapshot_hash,
            "snapshot hash",
        ),
        (
            &authenticated.search_policy_snapshot_id.0,
            &mission.search_policy_snapshot_id,
            "search policy snapshot",
        ),
    ] {
        if actual != expected {
            return Err(format!(
                "authenticated {field} identity does not match Mission v4"
            ));
        }
    }
    let mission_sha256 = prediction_mission_v3_sha256(mission)?;
    if let Some(checkpoint) = checkpoint {
        if checkpoint.schema_version != PREDICTION_MISSION_V3_CHECKPOINT_SCHEMA_VERSION
            || checkpoint.mission_sha256 != mission_sha256
            || checkpoint.cohort_manifest_id != mission.cohort_manifest_id
            || checkpoint.partition_digest != mission.partition_digest
            || checkpoint.causal_projection_policy_id != mission.causal_projection_policy_id
            || checkpoint.snapshot_contract_id != mission.snapshot_contract_id
            || checkpoint.snapshot_hash != mission.snapshot_hash
            || checkpoint.search_policy_snapshot_id != mission.search_policy_snapshot_id
        {
            return Err("checkpoint identity does not match Mission v4".to_string());
        }
    }
    Ok(AdmittedPredictionMissionV3 {
        mission_id: mission.mission_id.clone(),
        mission_sha256,
        product: mission.product.clone(),
        task: validate_task(&mission.task)?,
        run_mode: mission.run_mode,
        authority_profile: mission.authority_profile,
        cohort_manifest_id: mission.cohort_manifest_id.clone(),
        partition_digest: mission.partition_digest.clone(),
        causal_projection_policy_id: mission.causal_projection_policy_id.clone(),
        snapshot_contract_id: mission.snapshot_contract_id.clone(),
        snapshot_hash: mission.snapshot_hash.clone(),
        search_policy_snapshot_id: mission.search_policy_snapshot_id.clone(),
    })
}

fn validate_task(task: &PredictionMissionTask) -> Result<AdmittedPredictionTask, String> {
    match (task.kind, task.side, task.prediction_horizon_secs) {
        (PredictionTaskKind::SettlementProbability, None, None) => {
            Ok(AdmittedPredictionTask::SettlementProbability)
        }
        (PredictionTaskKind::UpExecution, Some(PredictionTokenSide::Up), Some(horizon))
            if matches!(horizon, 5 | 10 | 15 | 30) =>
        {
            Ok(AdmittedPredictionTask::UpExecution {
                prediction_horizon_secs: horizon,
            })
        }
        (PredictionTaskKind::DownExecution, Some(PredictionTokenSide::Down), Some(horizon))
            if matches!(horizon, 5 | 10 | 15 | 30) =>
        {
            Ok(AdmittedPredictionTask::DownExecution {
                prediction_horizon_secs: horizon,
            })
        }
        (PredictionTaskKind::SettlementProbability, _, _) => {
            Err("settlement_probability cannot carry a token side or execution horizon".into())
        }
        (PredictionTaskKind::UpExecution, _, _) => {
            Err("up_execution must bind side up and a 5, 10, 15, or 30 second horizon".into())
        }
        (PredictionTaskKind::DownExecution, _, _) => {
            Err("down_execution must bind side down and a 5, 10, 15, or 30 second horizon".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn settlement_mission() -> serde_json::Value {
        serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "btc-5m-settlement",
            "product": { "symbol": "BTC", "event_horizon_secs": 300 },
            "task": { "kind": "settlement_probability" },
            "run_mode": "research_trial",
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": sha('1'),
            "partition_digest": sha('2'),
            "causal_projection_policy_id": sha('3'),
            "snapshot_contract_id": sha('4'),
            "snapshot_hash": "5".repeat(16),
            "search_policy_snapshot_id": sha('6'),
            "search_budget": {
                "max_candidates": 8,
                "max_seconds": 60
            }
        })
    }

    fn parse_v3(value: serde_json::Value) -> PredictionResearchMissionV3 {
        parse_prediction_mission_json(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn authenticated(
        mission: &PredictionResearchMissionV3,
    ) -> AuthenticatedPredictionMissionV3Inputs {
        AuthenticatedPredictionMissionV3Inputs {
            task: mission.task.clone(),
            authority_profile: mission.authority_profile,
            capabilities: mission.required_capabilities.clone(),
            cohort_manifest_id: AuthenticatedDigest(mission.cohort_manifest_id.clone()),
            partition_digest: AuthenticatedDigest(mission.partition_digest.clone()),
            causal_projection_policy_id: AuthenticatedDigest(
                mission.causal_projection_policy_id.clone(),
            ),
            snapshot_contract_id: AuthenticatedDigest(mission.snapshot_contract_id.clone()),
            snapshot_hash: AuthenticatedDigest(mission.snapshot_hash.clone()),
            search_policy_snapshot_id: AuthenticatedDigest(
                mission.search_policy_snapshot_id.clone(),
            ),
            source_kind: POLYMARKET_CHAINLINK_BASELINE_SOURCE_KIND.to_string(),
        }
    }

    fn execution_mission(
        kind: PredictionTaskKind,
        side: PredictionTokenSide,
        horizon: u32,
    ) -> PredictionResearchMissionV3 {
        let mut mission = parse_v3(settlement_mission());
        mission.task = PredictionMissionTask {
            kind,
            side: Some(side),
            prediction_horizon_secs: Some(horizon),
        };
        mission
    }

    #[test]
    fn v3_json_admits_a_typed_settlement_task() {
        let mission = parse_v3(settlement_mission());

        let admitted =
            admit_prediction_mission_v3(&mission, &authenticated(&mission), None).unwrap();

        assert_eq!(admitted.task, AdmittedPredictionTask::SettlementProbability);
        assert_eq!(admitted.product.symbol, PredictionProductSymbol::Btc);
        assert_eq!(admitted.product.event_horizon_secs, 300);
        assert!(admitted.mission_sha256.starts_with("sha256:"));
    }

    #[test]
    fn v3_admits_only_side_bound_execution_horizons() {
        for horizon in [5, 10, 15, 30] {
            let up = execution_mission(
                PredictionTaskKind::UpExecution,
                PredictionTokenSide::Up,
                horizon,
            );
            assert_eq!(
                admit_prediction_mission_v3(&up, &authenticated(&up), None)
                    .unwrap()
                    .task,
                AdmittedPredictionTask::UpExecution {
                    prediction_horizon_secs: horizon
                }
            );
            let down = execution_mission(
                PredictionTaskKind::DownExecution,
                PredictionTokenSide::Down,
                horizon,
            );
            assert_eq!(
                admit_prediction_mission_v3(&down, &authenticated(&down), None)
                    .unwrap()
                    .task,
                AdmittedPredictionTask::DownExecution {
                    prediction_horizon_secs: horizon
                }
            );
        }
    }

    #[test]
    fn v3_rejects_settlement_execution_fields_and_execution_side_or_horizon_drift() {
        let mut settlement = parse_v3(settlement_mission());
        settlement.task.prediction_horizon_secs = Some(5);
        assert!(validate_prediction_mission_v3(&settlement)
            .unwrap_err()
            .contains("settlement_probability"));

        let wrong_side = execution_mission(
            PredictionTaskKind::UpExecution,
            PredictionTokenSide::Down,
            5,
        );
        assert!(validate_prediction_mission_v3(&wrong_side)
            .unwrap_err()
            .contains("bind side up"));

        let unsupported_horizon = execution_mission(
            PredictionTaskKind::DownExecution,
            PredictionTokenSide::Down,
            20,
        );
        assert!(validate_prediction_mission_v3(&unsupported_horizon)
            .unwrap_err()
            .contains("5, 10, 15, or 30"));

        let mut labeled_settlement = settlement_mission();
        labeled_settlement["task"]["execution_label"] = serde_json::json!("up_fill");
        assert!(
            parse_prediction_mission_json(&serde_json::to_vec(&labeled_settlement).unwrap())
                .unwrap_err()
                .contains("unknown field")
        );
    }

    #[test]
    fn v3_rejects_baseline_authority_requesting_binance_context() {
        let mut mission = parse_v3(settlement_mission());
        mission
            .required_capabilities
            .insert(PredictionMissionCapability::BinanceContext);

        assert_eq!(
            validate_prediction_mission_v3(&mission).unwrap_err(),
            "mission capabilities do not match its authority profile"
        );
    }

    #[test]
    fn admission_rejects_task_capability_authority_snapshot_and_policy_mismatches() {
        let mission = parse_v3(settlement_mission());

        let mut mismatched = authenticated(&mission);
        mismatched.task =
            execution_mission(PredictionTaskKind::UpExecution, PredictionTokenSide::Up, 5).task;
        assert!(admit_prediction_mission_v3(&mission, &mismatched, None)
            .unwrap_err()
            .contains("task"));

        let mut mismatched = authenticated(&mission);
        mismatched
            .capabilities
            .remove(&PredictionMissionCapability::PolymarketChainlink);
        assert!(admit_prediction_mission_v3(&mission, &mismatched, None)
            .unwrap_err()
            .contains("capabilities"));

        let mut mismatched = authenticated(&mission);
        mismatched.authority_profile = PredictionAuthorityProfile::PolymarketChainlinkBinance;
        assert!(admit_prediction_mission_v3(&mission, &mismatched, None)
            .unwrap_err()
            .contains("authority"));

        for (field, change) in [
            ("cohort", 0_u8),
            ("partition", 1_u8),
            ("causal projection", 2_u8),
            ("snapshot contract", 3_u8),
            ("snapshot hash", 4_u8),
            ("search policy", 5_u8),
        ] {
            let mut mismatched = authenticated(&mission);
            match change {
                0 => mismatched.cohort_manifest_id = AuthenticatedDigest(sha('4')),
                1 => mismatched.partition_digest = AuthenticatedDigest(sha('7')),
                2 => mismatched.causal_projection_policy_id = AuthenticatedDigest(sha('7')),
                3 => mismatched.snapshot_contract_id = AuthenticatedDigest(sha('7')),
                4 => mismatched.snapshot_hash = AuthenticatedDigest("7".repeat(16)),
                _ => mismatched.search_policy_snapshot_id = AuthenticatedDigest(sha('7')),
            }
            assert!(admit_prediction_mission_v3(&mission, &mismatched, None)
                .unwrap_err()
                .contains(field));
        }
    }

    #[test]
    fn v3_rejects_mutable_snapshot_identity() {
        let mut mission = parse_v3(settlement_mission());
        mission.snapshot_contract_id = "oss://snapshots/latest".to_string();

        assert!(validate_prediction_mission_v3(&mission)
            .unwrap_err()
            .contains("sha256:<64 lowercase hex>"));
    }

    #[test]
    fn v4_rejects_retired_proposal_provider_budget() {
        let mut mission = settlement_mission();
        mission["search_budget"]["max_llm_calls"] = serde_json::json!(1);

        assert!(
            parse_prediction_mission_json(&serde_json::to_vec(&mission).unwrap())
                .unwrap_err()
                .contains("max_llm_calls")
        );
    }

    #[test]
    fn mission_identity_binds_every_decision_field() {
        let base = parse_v3(settlement_mission());
        let mut variants = Vec::new();
        let mut changed = base.clone();
        changed.mission_id.push_str("-2");
        variants.push(changed);
        let mut changed = base.clone();
        changed.product.event_horizon_secs = 301;
        variants.push(changed);
        let mut changed =
            execution_mission(PredictionTaskKind::UpExecution, PredictionTokenSide::Up, 5);
        changed.mission_id = base.mission_id.clone();
        variants.push(changed);
        let mut changed = base.clone();
        changed.run_mode = PredictionRunMode::PipelineSmoke;
        changed.search_budget.max_candidates = 0;
        variants.push(changed);
        let mut changed = base.clone();
        changed.authority_profile = PredictionAuthorityProfile::PolymarketChainlinkBinance;
        changed.required_capabilities = BTreeSet::from([
            PredictionMissionCapability::PolymarketChainlink,
            PredictionMissionCapability::BinanceContext,
        ]);
        variants.push(changed);
        let mut changed = base.clone();
        changed.cohort_manifest_id = sha('4');
        variants.push(changed);
        let mut changed = base.clone();
        changed.partition_digest = sha('7');
        variants.push(changed);
        let mut changed = base.clone();
        changed.causal_projection_policy_id = sha('7');
        variants.push(changed);
        let mut changed = base.clone();
        changed.snapshot_contract_id = sha('7');
        variants.push(changed);
        let mut changed = base.clone();
        changed.snapshot_hash = "7".repeat(16);
        variants.push(changed);
        let mut changed = base.clone();
        changed.search_policy_snapshot_id = sha('4');
        variants.push(changed);
        let mut changed = base.clone();
        changed.search_budget.max_candidates += 1;
        variants.push(changed);

        let hashes = std::iter::once(base)
            .chain(variants)
            .map(|mission| prediction_mission_v3_sha256(&mission).unwrap())
            .collect::<BTreeSet<_>>();

        assert_eq!(hashes.len(), 13);
    }

    #[test]
    fn external_v2_mission_is_rejected() {
        let error = parse_prediction_mission_json(
            br#"{"schema_version":"prediction_research_mission.v2"}"#,
        )
        .unwrap_err();

        assert!(error.contains("unsupported prediction mission schema_version"));
    }

    #[test]
    fn v3_resume_rejects_legacy_v3_checkpoint_or_identity_drift() {
        let mission = parse_v3(settlement_mission());
        let admitted =
            admit_prediction_mission_v3(&mission, &authenticated(&mission), None).unwrap();
        let mut checkpoint = admitted.checkpoint_identity();
        checkpoint.schema_version = "prediction_research_checkpoint.v3".to_string();
        assert!(
            admit_prediction_mission_v3(&mission, &authenticated(&mission), Some(&checkpoint))
                .unwrap_err()
                .contains("checkpoint identity")
        );

        let mut checkpoint = admitted.checkpoint_identity();
        checkpoint.mission_sha256 = sha('9');
        assert!(
            admit_prediction_mission_v3(&mission, &authenticated(&mission), Some(&checkpoint))
                .unwrap_err()
                .contains("checkpoint identity")
        );
    }
}
