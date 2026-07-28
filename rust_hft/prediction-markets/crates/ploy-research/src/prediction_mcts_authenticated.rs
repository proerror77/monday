use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::factors::{FactorObservation, ResearchPmBookSnapshot};
use crate::factors_v2::{
    build_full_depth_execution_matrix_with_event_rows, joint_binary_market_probability,
    FullDepthExecutionEventRow, FullDepthExecutionMatrixOptions, ReviewSide,
    SettlementProbabilityComponentProfile,
};
use crate::prediction_loop::{
    current_prediction_policy_snapshot_id, research_brief_snapshot_id, LoopRunSummary,
    PredictionResearchMission, ProposalClient, PREDICTION_LOOP_TARGET,
    PREDICTION_MISSION_SCHEMA_VERSION,
};
use crate::prediction_loop_fs::{
    read_verified_artifact_bounded, write_content_addressed_json, ArtifactRef,
};
use crate::prediction_mcts::{
    ExecutionTrainingEvidence, PredictionMctsCandidate, PredictionMctsEvaluation,
    PredictionMctsIdentity, PredictionMctsTask, PredictionMctsTrainingEvidence,
    SettlementTrainingEvidence,
};
use crate::prediction_mcts_run::{
    authenticated_selection_evidence, run_or_resume_authenticated_prediction_mcts, task_output_dir,
    PredictionMctsHeldOutArtifact, PredictionMctsRunEvaluator,
};
use crate::prediction_mission_v3::{
    admit_prediction_mission_v3, authenticate_prediction_mission_v3_inputs,
    prediction_mission_v3_sha256, validate_prediction_mission_v3, AdmittedPredictionMissionV3,
    PredictionResearchMissionV3, PredictionRunMode, PredictionTaskKind,
};
use crate::research_snapshot::{AuthenticatedResearchSnapshot, ResearchSnapshot};

#[derive(Debug, Clone)]
pub struct AuthenticatedTrainingSnapshot {
    market_ids: Vec<String>,
    observations: Vec<FactorObservation>,
    pm_book_snapshots: Vec<ResearchPmBookSnapshot>,
    max_quote_age_secs: i64,
    stake_usd: f64,
}

impl AuthenticatedTrainingSnapshot {
    pub fn market_ids(&self) -> &[String] {
        &self.market_ids
    }

    pub fn observations(&self) -> &[FactorObservation] {
        &self.observations
    }

    pub fn pm_book_snapshots(&self) -> &[ResearchPmBookSnapshot] {
        &self.pm_book_snapshots
    }

    pub fn max_quote_age_secs(&self) -> i64 {
        self.max_quote_age_secs
    }

    pub fn stake_usd(&self) -> f64 {
        self.stake_usd
    }
}

#[derive(Debug, Clone)]
pub struct AuthenticatedHeldOutSnapshot(AuthenticatedTrainingSnapshot);

impl AuthenticatedHeldOutSnapshot {
    pub fn market_ids(&self) -> &[String] {
        self.0.market_ids()
    }

    pub fn observations(&self) -> &[FactorObservation] {
        self.0.observations()
    }

    pub fn pm_book_snapshots(&self) -> &[ResearchPmBookSnapshot] {
        self.0.pm_book_snapshots()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedEvaluationArtifact {
    pub schema_version: String,
    pub selected_candidate_sha256: String,
    pub metrics: AuthenticatedTaskMetrics,
    pub sha256: String,
}

const AUTHENTICATED_EVALUATION_SCHEMA_VERSION: &str = "prediction_authenticated_evaluation.v1";
const MAX_AUTHENTICATED_EVALUATION_BYTES: usize = 1024 * 1024;

impl AuthenticatedEvaluationArtifact {
    fn new(
        candidate: &PredictionMctsCandidate,
        metrics: AuthenticatedTaskMetrics,
    ) -> Result<Self, String> {
        let selected_candidate_sha256 = format!(
            "sha256:{}",
            crate::prediction_loop_fs::sha256_hex(
                &crate::prediction_loop_fs::canonical_json_bytes(candidate)?
            )
        );
        let mut artifact = Self {
            schema_version: AUTHENTICATED_EVALUATION_SCHEMA_VERSION.to_string(),
            selected_candidate_sha256,
            metrics,
            sha256: String::new(),
        };
        artifact =
            serde_json::from_slice(&crate::prediction_loop_fs::canonical_json_bytes(&artifact)?)
                .map_err(|error| format!("normalize authenticated evaluation artifact: {error}"))?;
        artifact.sha256 = artifact.expected_sha256()?;
        Ok(artifact)
    }

    fn expected_sha256(&self) -> Result<String, String> {
        let mut payload = self.clone();
        payload.sha256.clear();
        Ok(format!(
            "sha256:{}",
            crate::prediction_loop_fs::sha256_hex(
                &crate::prediction_loop_fs::canonical_json_bytes(&payload)?
            )
        ))
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != AUTHENTICATED_EVALUATION_SCHEMA_VERSION {
            return Err("authenticated evaluation schema is incompatible".to_string());
        }
        crate::prediction_loop::validate_sha256_id(
            &self.selected_candidate_sha256,
            "selected candidate",
        )?;
        crate::prediction_loop::validate_sha256_id(&self.sha256, "held-out evaluator artifact")?;
        self.metrics.validate()?;
        if self.sha256 != self.expected_sha256()? {
            return Err("held-out evaluator artifact digest mismatch".to_string());
        }
        Ok(())
    }

    fn content_sha256(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "sha256:{}",
            crate::prediction_loop_fs::sha256_hex(
                &crate::prediction_loop_fs::canonical_json_bytes(self)?
            )
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthenticatedTaskMetrics {
    Settlement(SettlementTaskMetrics),
    UpExecution(ExecutionTaskMetrics),
    DownExecution(ExecutionTaskMetrics),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettlementTaskMetrics {
    pub event_count: usize,
    pub decision_count: usize,
    pub mean_brier_score: f64,
    pub mean_log_loss: f64,
    pub expected_calibration_error: f64,
    pub up_executable_count: usize,
    pub down_executable_count: usize,
    pub total_up_full_depth_settlement_pnl_usd_micros: i64,
    pub total_down_full_depth_settlement_pnl_usd_micros: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTaskMetrics {
    pub joint_state_sha256: String,
    pub event_count: usize,
    pub decision_count: usize,
    pub roundtrip_count: usize,
    pub prediction_horizon_secs: u32,
    pub mean_fill_rate: f64,
    pub mean_entry_shares: f64,
    pub mean_entry_notional_usd: f64,
    pub mean_total_fee_usd: f64,
    pub mean_entry_slippage_bps: f64,
    pub mean_exit_slippage_bps: f64,
    pub mean_capacity_usd: f64,
    pub mean_joint_fair_probability: f64,
    pub mean_fair_edge_usd: f64,
    pub mean_reprice_pnl: f64,
    pub mean_complete_set_capacity_shares: f64,
    pub mean_complete_set_edge_usd: f64,
}

impl AuthenticatedTaskMetrics {
    fn validate(&self) -> Result<(), String> {
        let valid = match self {
            Self::Settlement(metrics) => {
                metrics.event_count > 0
                    && metrics.decision_count >= metrics.event_count
                    && (0.0..=1.0).contains(&metrics.mean_brier_score)
                    && metrics.mean_log_loss.is_finite()
                    && metrics.mean_log_loss >= 0.0
                    && (0.0..=1.0).contains(&metrics.expected_calibration_error)
                    && metrics.up_executable_count <= metrics.decision_count
                    && metrics.down_executable_count <= metrics.decision_count
                    && (metrics.up_executable_count > 0
                        || metrics.total_up_full_depth_settlement_pnl_usd_micros == 0)
                    && (metrics.down_executable_count > 0
                        || metrics.total_down_full_depth_settlement_pnl_usd_micros == 0)
            }
            Self::UpExecution(metrics) | Self::DownExecution(metrics) => {
                crate::prediction_loop::validate_sha256_id(
                    &metrics.joint_state_sha256,
                    "execution joint state",
                )?;
                let expected_fill_rate =
                    metrics.roundtrip_count as f64 / metrics.decision_count as f64;
                metrics.event_count > 0
                    && metrics.decision_count >= metrics.event_count
                    && metrics.roundtrip_count <= metrics.decision_count
                    && matches!(metrics.prediction_horizon_secs, 5 | 10 | 15 | 30)
                    && (0.0..=1.0).contains(&metrics.mean_fill_rate)
                    && metrics.mean_fill_rate == expected_fill_rate
                    && (0.0..=1.0).contains(&metrics.mean_joint_fair_probability)
                    && [
                        metrics.mean_entry_shares,
                        metrics.mean_entry_notional_usd,
                        metrics.mean_total_fee_usd,
                        metrics.mean_capacity_usd,
                        metrics.mean_complete_set_capacity_shares,
                    ]
                    .into_iter()
                    .all(|value| value.is_finite() && value >= 0.0)
                    && [
                        metrics.mean_entry_slippage_bps,
                        metrics.mean_exit_slippage_bps,
                        metrics.mean_fair_edge_usd,
                        metrics.mean_reprice_pnl,
                        metrics.mean_complete_set_edge_usd,
                    ]
                    .into_iter()
                    .all(f64::is_finite)
            }
        };
        valid
            .then_some(())
            .ok_or_else(|| "authenticated task metrics are invalid".to_string())
    }
}

const AUTHENTICATED_RESULT_RECEIPT_SCHEMA_VERSION: &str =
    "prediction_authenticated_result_receipt.v1";
const MAX_AUTHENTICATED_RESULT_RECEIPT_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPredictionResultReceipt {
    pub schema_version: String,
    pub mission: PredictionResearchMissionV3,
    pub mission_sha256: String,
    pub immutable_image_identity: String,
    pub run_started_unix_millis: u64,
    pub held_out_completed_unix_millis: u64,
    pub result_sealed_unix_millis: u64,
    pub checkpoint_sha256: String,
    pub selected_candidate_sha256: String,
    pub evaluator_artifact_sha256: String,
    pub metrics: AuthenticatedTaskMetrics,
    pub sha256: String,
}

impl AuthenticatedPredictionResultReceipt {
    fn new(
        mission: &PredictionResearchMissionV3,
        admitted: &AdmittedPredictionMissionV3,
        immutable_image_identity: &str,
        selection: &crate::prediction_mcts_run::PredictionMctsSelectionEvidence,
        artifact: &AuthenticatedEvaluationArtifact,
    ) -> Result<Self, String> {
        artifact.validate()?;
        if !selection.held_out_complete
            || selection.immutable_image_identity.as_deref() != Some(immutable_image_identity)
            || selection.selected_candidate_sha256 != artifact.selected_candidate_sha256
        {
            return Err("result receipt does not match the durable held-out selection".to_string());
        }
        let held_out_artifact = selection
            .held_out_artifact
            .as_ref()
            .ok_or_else(|| "result receipt is missing its durable held-out artifact".to_string())?;
        let evaluator_artifact_sha256 = artifact.content_sha256()?;
        if held_out_artifact.sha256
            != evaluator_artifact_sha256
                .strip_prefix("sha256:")
                .unwrap_or_default()
        {
            return Err(
                "result receipt evaluator artifact does not match the durable selection"
                    .to_string(),
            );
        }
        let mission_sha256 = prediction_mission_v3_sha256(mission)?;
        if admitted.mission_sha256 != mission_sha256 {
            return Err("result receipt Mission digest does not match admission".to_string());
        }
        let mut receipt = Self {
            schema_version: AUTHENTICATED_RESULT_RECEIPT_SCHEMA_VERSION.to_string(),
            mission: mission.clone(),
            mission_sha256,
            immutable_image_identity: immutable_image_identity.to_string(),
            run_started_unix_millis: selection.run_started_unix_millis,
            held_out_completed_unix_millis: selection.held_out_completed_unix_millis,
            result_sealed_unix_millis: selection.result_sealed_unix_millis,
            checkpoint_sha256: selection.checkpoint_sha256.clone(),
            selected_candidate_sha256: selection.selected_candidate_sha256.clone(),
            evaluator_artifact_sha256,
            metrics: artifact.metrics.clone(),
            sha256: String::new(),
        };
        receipt.sha256 = receipt.expected_sha256()?;
        receipt.validate()?;
        Ok(receipt)
    }

    fn expected_sha256(&self) -> Result<String, String> {
        let mut payload = self.clone();
        payload.sha256.clear();
        Ok(format!(
            "sha256:{}",
            crate::prediction_loop_fs::sha256_hex(
                &crate::prediction_loop_fs::canonical_json_bytes(&payload)?
            )
        ))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != AUTHENTICATED_RESULT_RECEIPT_SCHEMA_VERSION {
            return Err("authenticated result receipt schema is incompatible".to_string());
        }
        validate_prediction_mission_v3(&self.mission)?;
        if self.mission.run_mode != PredictionRunMode::ResearchTrial {
            return Err(
                "authenticated result receipt requires ResearchTrial authority".to_string(),
            );
        }
        if self.mission_sha256 != prediction_mission_v3_sha256(&self.mission)? {
            return Err("authenticated result receipt Mission digest mismatch".to_string());
        }
        if self.run_started_unix_millis == 0
            || self.held_out_completed_unix_millis < self.run_started_unix_millis
            || self.result_sealed_unix_millis < self.held_out_completed_unix_millis
        {
            return Err(
                "authenticated result receipt lifecycle timestamps are invalid".to_string(),
            );
        }
        for (digest, field) in [
            (&self.mission_sha256, "receipt Mission"),
            (&self.immutable_image_identity, "receipt immutable image"),
            (&self.checkpoint_sha256, "receipt checkpoint"),
            (
                &self.selected_candidate_sha256,
                "receipt selected candidate",
            ),
            (
                &self.evaluator_artifact_sha256,
                "receipt evaluator artifact",
            ),
            (&self.sha256, "authenticated result receipt"),
        ] {
            crate::prediction_loop::validate_sha256_id(digest, field)?;
        }
        let metrics_match = match (&self.mission.task.kind, &self.metrics) {
            (
                PredictionTaskKind::SettlementProbability,
                AuthenticatedTaskMetrics::Settlement(_),
            ) => true,
            (PredictionTaskKind::UpExecution, AuthenticatedTaskMetrics::UpExecution(metrics))
            | (
                PredictionTaskKind::DownExecution,
                AuthenticatedTaskMetrics::DownExecution(metrics),
            ) => self.mission.task.prediction_horizon_secs == Some(metrics.prediction_horizon_secs),
            _ => false,
        };
        if !metrics_match {
            return Err("authenticated result receipt metrics do not match its task".to_string());
        }
        let mut artifact = AuthenticatedEvaluationArtifact {
            schema_version: AUTHENTICATED_EVALUATION_SCHEMA_VERSION.to_string(),
            selected_candidate_sha256: self.selected_candidate_sha256.clone(),
            metrics: self.metrics.clone(),
            sha256: String::new(),
        };
        artifact.sha256 = artifact.expected_sha256()?;
        if self.evaluator_artifact_sha256 != artifact.content_sha256()? {
            return Err("authenticated result receipt evaluator artifact mismatch".to_string());
        }
        if self.sha256 != self.expected_sha256()? {
            return Err("authenticated result receipt digest mismatch".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPredictionResultReceiptRef {
    path: String,
    artifact_sha256: String,
    receipt_sha256: String,
}

impl AuthenticatedPredictionResultReceiptRef {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub fn receipt_sha256(&self) -> &str {
        &self.receipt_sha256
    }

    fn artifact_ref(&self) -> Result<ArtifactRef, String> {
        crate::prediction_loop::validate_sha256_id(
            &self.artifact_sha256,
            "result receipt artifact",
        )?;
        crate::prediction_loop::validate_sha256_id(&self.receipt_sha256, "result receipt")?;
        let sha256 = self
            .artifact_sha256
            .strip_prefix("sha256:")
            .ok_or_else(|| "result receipt artifact digest is invalid".to_string())?;
        Ok(ArtifactRef {
            path: self.path.clone(),
            sha256: sha256.to_string(),
        })
    }
}

fn write_authenticated_prediction_result_receipt(
    output_root: &Path,
    admitted: &AdmittedPredictionMissionV3,
    receipt: &AuthenticatedPredictionResultReceipt,
) -> Result<AuthenticatedPredictionResultReceiptRef, String> {
    receipt.validate()?;
    if receipt.mission_sha256 != admitted.mission_sha256 {
        return Err("result receipt does not match its admitted Mission".to_string());
    }
    let identity = PredictionMctsIdentity::from_admitted_mission(admitted)?;
    let task_dir = task_output_dir(output_root, &identity)?;
    let artifact = write_content_addressed_json(
        output_root,
        &task_dir,
        "authenticated-result-receipt",
        receipt,
    )?;
    let reference = AuthenticatedPredictionResultReceiptRef {
        path: artifact.path,
        artifact_sha256: format!("sha256:{}", artifact.sha256),
        receipt_sha256: receipt.sha256.clone(),
    };
    if read_authenticated_prediction_result_receipt(output_root, &reference)? != *receipt {
        return Err("authenticated result receipt readback mismatch".to_string());
    }
    Ok(reference)
}

pub fn read_authenticated_prediction_result_receipt(
    output_root: &Path,
    reference: &AuthenticatedPredictionResultReceiptRef,
) -> Result<AuthenticatedPredictionResultReceipt, String> {
    let bytes = read_verified_artifact_bounded(
        output_root,
        &reference.artifact_ref()?,
        MAX_AUTHENTICATED_RESULT_RECEIPT_BYTES,
    )?;
    let receipt: AuthenticatedPredictionResultReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse authenticated result receipt: {error}"))?;
    receipt.validate()?;
    if receipt.sha256 != reference.receipt_sha256 {
        return Err("authenticated result receipt reference mismatch".to_string());
    }
    Ok(receipt)
}

const AUTHENTICATED_EXPERIMENT_MANIFEST_SCHEMA_VERSION: &str =
    "prediction_authenticated_experiment_manifest.v1";
const MAX_AUTHENTICATED_EXPERIMENT_MANIFEST_BYTES: usize = 1024 * 1024;
const RESEARCH_TRIAL_PROFITABILITY_CLAIM: &str = "not evaluated by ResearchTrial";
const RESEARCH_TRIAL_PROMOTION_CLAIM: &str = "outside ResearchTrial authority";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedUnavailableClaims {
    pub profitability: String,
    pub promotion: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPredictionExperimentManifest {
    pub schema_version: String,
    pub product: crate::prediction_mission_v3::PredictionProductIdentity,
    pub cohort_manifest_id: String,
    pub partition_digest: String,
    pub causal_projection_policy_id: String,
    pub snapshot_contract_id: String,
    pub snapshot_hash: String,
    pub search_policy_snapshot_id: String,
    pub immutable_image_identity: String,
    pub settlement: AuthenticatedPredictionResultReceiptRef,
    pub up_execution: AuthenticatedPredictionResultReceiptRef,
    pub down_execution: AuthenticatedPredictionResultReceiptRef,
    pub unavailable_claims: AuthenticatedUnavailableClaims,
    pub sha256: String,
}

impl AuthenticatedPredictionExperimentManifest {
    fn new(
        output_root: &Path,
        references: &[AuthenticatedPredictionResultReceiptRef],
    ) -> Result<Self, String> {
        if references.len() != 3 {
            return Err("experiment manifest requires exactly three result receipts".to_string());
        }
        let receipts = read_exact_three_receipts(
            output_root,
            [&references[0], &references[1], &references[2]],
        )?;
        validate_experiment_receipts(&receipts)?;
        let base = &receipts.settlement.1;
        let mut manifest = Self {
            schema_version: AUTHENTICATED_EXPERIMENT_MANIFEST_SCHEMA_VERSION.to_string(),
            product: base.mission.product.clone(),
            cohort_manifest_id: base.mission.cohort_manifest_id.clone(),
            partition_digest: base.mission.partition_digest.clone(),
            causal_projection_policy_id: base.mission.causal_projection_policy_id.clone(),
            snapshot_contract_id: base.mission.snapshot_contract_id.clone(),
            snapshot_hash: base.mission.snapshot_hash.clone(),
            search_policy_snapshot_id: base.mission.search_policy_snapshot_id.clone(),
            immutable_image_identity: base.immutable_image_identity.clone(),
            settlement: receipts.settlement.0,
            up_execution: receipts.up_execution.0,
            down_execution: receipts.down_execution.0,
            unavailable_claims: AuthenticatedUnavailableClaims {
                profitability: RESEARCH_TRIAL_PROFITABILITY_CLAIM.to_string(),
                promotion: RESEARCH_TRIAL_PROMOTION_CLAIM.to_string(),
            },
            sha256: String::new(),
        };
        manifest.sha256 = manifest.expected_sha256()?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn expected_sha256(&self) -> Result<String, String> {
        let mut payload = self.clone();
        payload.sha256.clear();
        Ok(format!(
            "sha256:{}",
            crate::prediction_loop_fs::sha256_hex(
                &crate::prediction_loop_fs::canonical_json_bytes(&payload)?
            )
        ))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != AUTHENTICATED_EXPERIMENT_MANIFEST_SCHEMA_VERSION {
            return Err("authenticated experiment manifest schema is incompatible".to_string());
        }
        for (digest, field) in [
            (&self.cohort_manifest_id, "experiment cohort manifest"),
            (&self.partition_digest, "experiment partition"),
            (
                &self.causal_projection_policy_id,
                "experiment causal projection policy",
            ),
            (&self.snapshot_contract_id, "experiment snapshot contract"),
            (&self.search_policy_snapshot_id, "experiment search policy"),
            (&self.immutable_image_identity, "experiment immutable image"),
            (&self.sha256, "authenticated experiment manifest"),
        ] {
            crate::prediction_loop::validate_sha256_id(digest, field)?;
        }
        for reference in [&self.settlement, &self.up_execution, &self.down_execution] {
            reference.artifact_ref()?;
        }
        if self.snapshot_hash.len() != 16
            || !self
                .snapshot_hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.unavailable_claims.profitability != RESEARCH_TRIAL_PROFITABILITY_CLAIM
            || self.unavailable_claims.promotion != RESEARCH_TRIAL_PROMOTION_CLAIM
        {
            return Err("authenticated experiment manifest claims are invalid".to_string());
        }
        if self.sha256 != self.expected_sha256()? {
            return Err("authenticated experiment manifest digest mismatch".to_string());
        }
        Ok(())
    }

    fn validate_receipt_readback(&self, output_root: &Path) -> Result<(), String> {
        let receipts = read_exact_three_receipts(
            output_root,
            [&self.settlement, &self.up_execution, &self.down_execution],
        )?;
        validate_experiment_receipts(&receipts)?;
        let base = &receipts.settlement.1;
        if receipts.settlement.0 != self.settlement
            || receipts.up_execution.0 != self.up_execution
            || receipts.down_execution.0 != self.down_execution
            || self.product != base.mission.product
            || self.cohort_manifest_id != base.mission.cohort_manifest_id
            || self.partition_digest != base.mission.partition_digest
            || self.causal_projection_policy_id != base.mission.causal_projection_policy_id
            || self.snapshot_contract_id != base.mission.snapshot_contract_id
            || self.snapshot_hash != base.mission.snapshot_hash
            || self.search_policy_snapshot_id != base.mission.search_policy_snapshot_id
            || self.immutable_image_identity != base.immutable_image_identity
        {
            return Err("experiment manifest does not match its receipt readback".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPredictionExperimentManifestRef {
    path: String,
    artifact_sha256: String,
    manifest_sha256: String,
}

impl AuthenticatedPredictionExperimentManifestRef {
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }

    fn artifact_ref(&self) -> Result<ArtifactRef, String> {
        crate::prediction_loop::validate_sha256_id(
            &self.artifact_sha256,
            "experiment manifest artifact",
        )?;
        crate::prediction_loop::validate_sha256_id(&self.manifest_sha256, "experiment manifest")?;
        Ok(ArtifactRef {
            path: self.path.clone(),
            sha256: self
                .artifact_sha256
                .strip_prefix("sha256:")
                .ok_or_else(|| "experiment manifest artifact digest is invalid".to_string())?
                .to_string(),
        })
    }
}

type ExperimentReceipt = (
    AuthenticatedPredictionResultReceiptRef,
    AuthenticatedPredictionResultReceipt,
);

struct ExperimentReceipts {
    settlement: ExperimentReceipt,
    up_execution: ExperimentReceipt,
    down_execution: ExperimentReceipt,
}

fn read_exact_three_receipts(
    output_root: &Path,
    references: [&AuthenticatedPredictionResultReceiptRef; 3],
) -> Result<ExperimentReceipts, String> {
    let mut settlement = None;
    let mut up_execution = None;
    let mut down_execution = None;
    for reference in references {
        let receipt = read_authenticated_prediction_result_receipt(output_root, reference)?;
        let slot = match receipt.mission.task.kind {
            PredictionTaskKind::SettlementProbability => &mut settlement,
            PredictionTaskKind::UpExecution => &mut up_execution,
            PredictionTaskKind::DownExecution => &mut down_execution,
        };
        if slot.replace((reference.clone(), receipt)).is_some() {
            return Err("experiment manifest contains a duplicate task receipt".to_string());
        }
    }
    Ok(ExperimentReceipts {
        settlement: settlement
            .ok_or_else(|| "experiment manifest is missing settlement receipt".to_string())?,
        up_execution: up_execution
            .ok_or_else(|| "experiment manifest is missing Up receipt".to_string())?,
        down_execution: down_execution
            .ok_or_else(|| "experiment manifest is missing Down receipt".to_string())?,
    })
}

fn validate_experiment_receipts(receipts: &ExperimentReceipts) -> Result<(), String> {
    let settlement = &receipts.settlement.1;
    for receipt in [&receipts.up_execution.1, &receipts.down_execution.1] {
        if receipt.mission.product != settlement.mission.product
            || receipt.mission.run_mode != settlement.mission.run_mode
            || receipt.mission.authority_profile != settlement.mission.authority_profile
            || receipt.mission.cohort_manifest_id != settlement.mission.cohort_manifest_id
            || receipt.mission.partition_digest != settlement.mission.partition_digest
            || receipt.mission.causal_projection_policy_id
                != settlement.mission.causal_projection_policy_id
            || receipt.mission.snapshot_contract_id != settlement.mission.snapshot_contract_id
            || receipt.mission.snapshot_hash != settlement.mission.snapshot_hash
            || receipt.mission.search_policy_snapshot_id
                != settlement.mission.search_policy_snapshot_id
            || receipt.immutable_image_identity != settlement.immutable_image_identity
        {
            return Err("experiment receipts do not share one authenticated identity".to_string());
        }
    }
    let AuthenticatedTaskMetrics::UpExecution(up) = &receipts.up_execution.1.metrics else {
        return Err("experiment Up receipt has substituted metrics".to_string());
    };
    let AuthenticatedTaskMetrics::DownExecution(down) = &receipts.down_execution.1.metrics else {
        return Err("experiment Down receipt has substituted metrics".to_string());
    };
    if up.prediction_horizon_secs != down.prediction_horizon_secs
        || up.joint_state_sha256 != down.joint_state_sha256
    {
        return Err("experiment Up and Down receipts do not share one joint state".to_string());
    }
    Ok(())
}

pub fn write_authenticated_prediction_experiment_manifest(
    output_root: &Path,
    receipts: &[AuthenticatedPredictionResultReceiptRef],
) -> Result<AuthenticatedPredictionExperimentManifestRef, String> {
    let manifest = AuthenticatedPredictionExperimentManifest::new(output_root, receipts)?;
    let directory = output_root.join("mcts-v4").join("experiment-manifests");
    let artifact = write_content_addressed_json(
        output_root,
        &directory,
        "authenticated-experiment-manifest",
        &manifest,
    )?;
    let reference = AuthenticatedPredictionExperimentManifestRef {
        path: artifact.path,
        artifact_sha256: format!("sha256:{}", artifact.sha256),
        manifest_sha256: manifest.sha256.clone(),
    };
    if read_authenticated_prediction_experiment_manifest(output_root, &reference)? != manifest {
        return Err("authenticated experiment manifest readback mismatch".to_string());
    }
    Ok(reference)
}

pub fn read_authenticated_prediction_experiment_manifest(
    output_root: &Path,
    reference: &AuthenticatedPredictionExperimentManifestRef,
) -> Result<AuthenticatedPredictionExperimentManifest, String> {
    let bytes = read_verified_artifact_bounded(
        output_root,
        &reference.artifact_ref()?,
        MAX_AUTHENTICATED_EXPERIMENT_MANIFEST_BYTES,
    )?;
    let manifest: AuthenticatedPredictionExperimentManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse authenticated experiment manifest: {error}"))?;
    manifest.validate()?;
    if manifest.sha256 != reference.manifest_sha256 {
        return Err("authenticated experiment manifest reference mismatch".to_string());
    }
    manifest.validate_receipt_readback(output_root)?;
    Ok(manifest)
}

fn builtin_task_metrics(
    mission: &PredictionResearchMissionV3,
    snapshot: &AuthenticatedTrainingSnapshot,
) -> Result<AuthenticatedTaskMetrics, String> {
    validate_prediction_mission_v3(mission)?;
    match mission.task.kind {
        PredictionTaskKind::SettlementProbability => {
            settlement_task_metrics(snapshot).map(AuthenticatedTaskMetrics::Settlement)
        }
        PredictionTaskKind::UpExecution => execution_task_metrics(
            snapshot,
            ReviewSide::Up,
            mission
                .task
                .prediction_horizon_secs
                .ok_or_else(|| "Up execution horizon is missing".to_string())?,
        )
        .map(AuthenticatedTaskMetrics::UpExecution),
        PredictionTaskKind::DownExecution => execution_task_metrics(
            snapshot,
            ReviewSide::Down,
            mission
                .task
                .prediction_horizon_secs
                .ok_or_else(|| "Down execution horizon is missing".to_string())?,
        )
        .map(AuthenticatedTaskMetrics::DownExecution),
    }
}

fn settlement_task_metrics(
    snapshot: &AuthenticatedTrainingSnapshot,
) -> Result<SettlementTaskMetrics, String> {
    if snapshot.market_ids().is_empty() || snapshot.observations().is_empty() {
        return Err("settlement metrics require a non-empty authenticated cohort".to_string());
    }
    let samples = snapshot
        .observations()
        .iter()
        .map(|row| {
            let q = joint_binary_market_probability(
                row.pm_up_bid,
                row.pm_up_ask,
                row.pm_down_bid,
                row.pm_down_ask,
            )
            .ok_or_else(|| "settlement decision has no complete joint quote".to_string())?;
            if !matches!(row.settlement_up, 0.0 | 1.0) {
                return Err("settlement decision has no binary train label".to_string());
            }
            Ok(((row.event_id.as_str(), row.tick_ts), q, row.settlement_up))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let decision_ids = samples
        .iter()
        .map(|(identity, ..)| *identity)
        .collect::<BTreeSet<_>>();
    if decision_ids.len() != samples.len() {
        return Err("settlement metrics contain duplicate train decisions".to_string());
    }
    let event_ids = decision_ids
        .iter()
        .map(|(market_id, _)| *market_id)
        .collect::<BTreeSet<_>>();
    if event_ids != snapshot.market_ids().iter().map(String::as_str).collect() {
        return Err("settlement metrics do not cover every authenticated train market".to_string());
    }
    let (_, full_depth_rows) = build_full_depth_execution_matrix_with_event_rows(
        snapshot.observations(),
        snapshot.pm_book_snapshots(),
        FullDepthExecutionMatrixOptions {
            stakes_usd: vec![snapshot.stake_usd()],
            min_bucket_observations: 1,
            max_quote_age_secs: snapshot.max_quote_age_secs(),
            ..Default::default()
        },
    );
    let expected_full_depth_ids = decision_ids
        .iter()
        .flat_map(|(market_id, tick_ts)| {
            [(*market_id, *tick_ts, "up"), (*market_id, *tick_ts, "down")]
        })
        .collect::<BTreeSet<_>>();
    let full_depth_ids = full_depth_rows
        .iter()
        .map(|row| (row.market_id.as_str(), row.tick_ts, row.side.as_str()))
        .collect::<BTreeSet<_>>();
    if full_depth_ids != expected_full_depth_ids || full_depth_rows.len() != full_depth_ids.len() {
        return Err("settlement PnL does not cover both books for every decision".to_string());
    }
    let up_pnl = full_depth_rows
        .iter()
        .filter(|row| row.side == ReviewSide::Up)
        .filter_map(|row| row.settlement_pnl)
        .collect::<Vec<_>>();
    let down_pnl = full_depth_rows
        .iter()
        .filter(|row| row.side == ReviewSide::Down)
        .filter_map(|row| row.settlement_pnl)
        .collect::<Vec<_>>();
    let mut calibration = BTreeMap::<usize, (usize, f64, f64)>::new();
    for (_, q, win) in &samples {
        let bucket = calibration
            .entry(((*q * 10.0).floor() as usize).min(9))
            .or_default();
        bucket.0 += 1;
        bucket.1 += q;
        bucket.2 += win;
    }
    let decision_count = samples.len();
    Ok(SettlementTaskMetrics {
        event_count: event_ids.len(),
        decision_count,
        mean_brier_score: mean_or_zero(samples.iter().map(|(_, q, win)| (*q - *win).powi(2))),
        mean_log_loss: mean_or_zero(samples.iter().map(|(_, q, win)| {
            let outcome_probability = if *win == 1.0 { *q } else { 1.0 - *q };
            -outcome_probability.max(f64::MIN_POSITIVE).ln()
        })),
        expected_calibration_error: calibration
            .values()
            .map(|(count, q, win)| {
                (q / *count as f64 - win / *count as f64).abs()
                    * (*count as f64 / decision_count as f64)
            })
            .sum(),
        up_executable_count: up_pnl.len(),
        down_executable_count: down_pnl.len(),
        total_up_full_depth_settlement_pnl_usd_micros: total_usd_micros(&up_pnl)?,
        total_down_full_depth_settlement_pnl_usd_micros: total_usd_micros(&down_pnl)?,
    })
}

fn total_usd_micros(values: &[f64]) -> Result<i64, String> {
    values.iter().try_fold(0_i64, |total, value| {
        let micros = (*value * 1_000_000.0).round();
        if !micros.is_finite() || micros < i64::MIN as f64 || micros > i64::MAX as f64 {
            return Err("settlement PnL exceeds fixed-point receipt range".to_string());
        }
        total
            .checked_add(micros as i64)
            .ok_or_else(|| "settlement PnL receipt total overflow".to_string())
    })
}

fn execution_task_metrics(
    snapshot: &AuthenticatedTrainingSnapshot,
    side: ReviewSide,
    prediction_horizon_secs: u32,
) -> Result<ExecutionTaskMetrics, String> {
    if !snapshot.stake_usd().is_finite()
        || snapshot.stake_usd() <= 0.0
        || snapshot.max_quote_age_secs() <= 0
    {
        return Err("authenticated execution settings are invalid".to_string());
    }
    let decision_observations = snapshot
        .observations()
        .iter()
        .filter(|row| row.time_remaining_secs >= i64::from(prediction_horizon_secs))
        .collect::<Vec<_>>();
    let expected_decision_ids = decision_observations
        .iter()
        .map(|row| (row.event_id.as_str(), row.tick_ts))
        .collect::<BTreeSet<_>>();
    if decision_observations.is_empty()
        || expected_decision_ids.len() != decision_observations.len()
    {
        return Err("execution metrics require unique horizon-eligible decisions".to_string());
    }
    validate_execution_horizon_evidence(
        snapshot,
        side,
        prediction_horizon_secs,
        &decision_observations,
    )?;
    let mut observations = snapshot.observations().to_vec();
    for row in &mut observations {
        row.settlement_up = f64::NAN;
        row.official_resolution_observed_at = None;
    }
    let (_, rows) = build_full_depth_execution_matrix_with_event_rows(
        &observations,
        snapshot.pm_book_snapshots(),
        FullDepthExecutionMatrixOptions {
            stakes_usd: vec![snapshot.stake_usd()],
            min_bucket_observations: 1,
            max_quote_age_secs: snapshot.max_quote_age_secs(),
            ..Default::default()
        },
    );
    let rows = rows
        .iter()
        .filter(|row| {
            row.side == side
                && expected_decision_ids.contains(&(row.market_id.as_str(), row.tick_ts))
        })
        .collect::<Vec<_>>();
    let decision_ids = rows
        .iter()
        .map(|row| (row.market_id.as_str(), row.tick_ts))
        .collect::<BTreeSet<_>>();
    let has_invalid_joint_depth = rows.iter().any(|row| {
        row.joint_state_sha256.as_deref().is_none_or(|digest| {
            crate::prediction_loop::validate_sha256_id(digest, "joint binary book").is_err()
        }) || row
            .joint_fair_probability
            .is_none_or(|value| !value.is_finite() || !(0.0..=1.0).contains(&value))
            || row
                .up_down_best_ask_sum
                .is_none_or(|value| !value.is_finite() || value <= 0.0)
            || row
                .complete_set_capacity_shares
                .is_none_or(|value| !value.is_finite() || value <= 0.0)
            || row
                .complete_set_cost_usd
                .is_none_or(|value| !value.is_finite() || value <= 0.0)
            || row
                .complete_set_fee_usd
                .is_none_or(|value| !value.is_finite() || value < 0.0)
            || row
                .complete_set_edge_usd
                .is_none_or(|value| !value.is_finite())
    });
    if rows.is_empty()
        || rows.len() != decision_observations.len()
        || decision_ids.len() != rows.len()
        || decision_ids != expected_decision_ids
        || has_invalid_joint_depth
    {
        return Err("execution metrics do not exactly cover authenticated train decisions".into());
    }
    let mut joint_states = rows
        .iter()
        .map(|row| {
            (
                row.market_id.as_str(),
                row.tick_ts,
                row.joint_state_sha256.as_deref().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    joint_states.sort();
    let joint_state_sha256 = format!(
        "sha256:{}",
        crate::prediction_loop_fs::sha256_hex(&crate::prediction_loop_fs::canonical_json_bytes(
            &joint_states
        )?)
    );
    let event_ids = decision_ids
        .iter()
        .map(|(market_id, _)| *market_id)
        .collect::<BTreeSet<_>>();
    if event_ids != snapshot.market_ids().iter().map(String::as_str).collect() {
        return Err("execution metrics do not cover every authenticated train market".to_string());
    }
    let roundtrip = rows
        .iter()
        .filter_map(|row| {
            let (exit_fillable, exit_fee, exit_slippage, reprice_pnl) =
                execution_horizon(row, prediction_horizon_secs).ok()?;
            (row.entry_fillable && exit_fillable).then_some((
                row.entry_fee_usd
                    .zip(exit_fee)
                    .map(|(entry, exit)| entry + exit),
                exit_slippage,
                reprice_pnl,
            ))
        })
        .collect::<Vec<_>>();
    Ok(ExecutionTaskMetrics {
        joint_state_sha256,
        event_count: event_ids.len(),
        decision_count: rows.len(),
        roundtrip_count: roundtrip.len(),
        prediction_horizon_secs,
        mean_fill_rate: roundtrip.len() as f64 / rows.len() as f64,
        mean_entry_shares: mean_or_zero(rows.iter().filter_map(|row| row.entry_shares)),
        mean_entry_notional_usd: mean_or_zero(rows.iter().filter_map(|row| row.entry_notional_usd)),
        mean_total_fee_usd: mean_or_zero(roundtrip.iter().filter_map(|row| row.0)),
        mean_entry_slippage_bps: mean_or_zero(rows.iter().filter_map(|row| row.entry_slippage_bps)),
        mean_exit_slippage_bps: mean_or_zero(roundtrip.iter().filter_map(|row| row.1)),
        mean_capacity_usd: mean_or_zero(rows.iter().filter_map(|row| row.entry_capacity_usd)),
        mean_joint_fair_probability: mean_or_zero(
            rows.iter().filter_map(|row| row.joint_fair_probability),
        ),
        mean_fair_edge_usd: mean_or_zero(rows.iter().filter_map(|row| row.fair_edge_usd)),
        mean_reprice_pnl: mean_or_zero(roundtrip.iter().filter_map(|row| row.2)),
        mean_complete_set_capacity_shares: mean_or_zero(
            rows.iter()
                .filter_map(|row| row.complete_set_capacity_shares),
        ),
        mean_complete_set_edge_usd: mean_or_zero(
            rows.iter().filter_map(|row| row.complete_set_edge_usd),
        ),
    })
}

fn validate_execution_horizon_evidence(
    snapshot: &AuthenticatedTrainingSnapshot,
    side: ReviewSide,
    prediction_horizon_secs: u32,
    decision_observations: &[&FactorObservation],
) -> Result<(), String> {
    let mut by_event = BTreeMap::<&str, Vec<&FactorObservation>>::new();
    for row in snapshot.observations() {
        by_event.entry(row.event_id.as_str()).or_default().push(row);
    }
    for rows in by_event.values_mut() {
        rows.sort_by_key(|row| row.tick_ts);
    }
    let max_age_millis = snapshot.max_quote_age_secs().saturating_mul(1_000);
    let side_key = match side {
        ReviewSide::Up => "UP",
        ReviewSide::Down => "DOWN",
    };
    for source in decision_observations {
        let target = source.tick_ts + chrono::Duration::seconds(i64::from(prediction_horizon_secs));
        let future = by_event
            .get(source.event_id.as_str())
            .and_then(|rows| rows.iter().copied().find(|row| row.tick_ts >= target))
            .ok_or_else(|| {
                format!(
                    "execution decision {} at {} has no requested horizon observation",
                    source.event_id, source.tick_ts
                )
            })?;
        let (token_id, reference_bid) = match side {
            ReviewSide::Up => (&future.up_token_id, future.pm_up_bid),
            ReviewSide::Down => (&future.down_token_id, future.pm_down_bid),
        };
        if !reference_bid.is_finite() || !(0.0..1.0).contains(&reference_bid) {
            return Err("execution horizon observation has no valid side bid".to_string());
        }
        let book = snapshot
            .pm_book_snapshots()
            .iter()
            .filter(|book| {
                book.event_id == future.event_id
                    && book.token_id == *token_id
                    && book.side.eq_ignore_ascii_case(side_key)
                    && book.ts <= future.tick_ts
                    && (future.tick_ts - book.ts).num_milliseconds() <= max_age_millis
            })
            .max_by_key(|book| book.ts)
            .ok_or_else(|| "execution decision has no fresh requested horizon book".to_string())?;
        if !book.bids.iter().any(|level| {
            level.price.is_finite()
                && (0.0..1.0).contains(&level.price)
                && level.size.is_finite()
                && level.size > 0.0
        }) {
            return Err("execution horizon book has no valid bid depth".to_string());
        }
    }
    Ok(())
}

type ExecutionHorizonMetrics = (bool, Option<f64>, Option<f64>, Option<f64>);

fn execution_horizon(
    row: &FullDepthExecutionEventRow,
    prediction_horizon_secs: u32,
) -> Result<ExecutionHorizonMetrics, String> {
    match prediction_horizon_secs {
        5 => Ok((
            row.exit_5s_fillable,
            row.exit_5s_fee_usd,
            row.exit_5s_slippage_bps,
            row.exit_5s_reprice_pnl,
        )),
        10 => Ok((
            row.exit_10s_fillable,
            row.exit_10s_fee_usd,
            row.exit_10s_slippage_bps,
            row.exit_10s_reprice_pnl,
        )),
        15 => Ok((
            row.exit_15s_fillable,
            row.exit_15s_fee_usd,
            row.exit_15s_slippage_bps,
            row.exit_15s_reprice_pnl,
        )),
        30 => Ok((
            row.exit_30s_fillable,
            row.exit_30s_fee_usd,
            row.exit_30s_slippage_bps,
            row.exit_30s_reprice_pnl,
        )),
        other => Err(format!("unsupported execution horizon {other}s")),
    }
}

fn mean_or_zero(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, count) = values.fold((0.0, 0usize), |(sum, count), value| {
        (sum + value, count + 1)
    });
    if count == 0 {
        0.0
    } else {
        sum / count as f64
    }
}

pub struct BuiltInAuthenticatedPredictionMctsEvaluator;

impl sealed::Evaluator for BuiltInAuthenticatedPredictionMctsEvaluator {}

impl AuthenticatedPredictionMctsEvaluator for BuiltInAuthenticatedPredictionMctsEvaluator {
    fn evaluate_baseline(
        &mut self,
        mission: &PredictionResearchMissionV3,
        training: &AuthenticatedTrainingSnapshot,
        _artifact_dir: &Path,
        timeout: Duration,
    ) -> Result<(), String> {
        validate_prediction_mission_v3(mission)?;
        if timeout.is_zero()
            || training.market_ids().is_empty()
            || training.observations().is_empty()
        {
            return Err("authenticated training cohort is unavailable".to_string());
        }
        Ok(())
    }

    fn evaluate_training(
        &mut self,
        mission: &PredictionResearchMissionV3,
        training: &AuthenticatedTrainingSnapshot,
        _artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<PredictionMctsEvaluation, String> {
        if timeout.is_zero() {
            return Err("authenticated training evaluator deadline exhausted".to_string());
        }
        candidate_matches_mission(mission, candidate)?;
        let started = Instant::now();
        let metrics = builtin_task_metrics(mission, training)?;
        ensure_metric_computation_within_timeout(started, timeout)?;
        let common = (
            candidate.candidate_id.clone(),
            candidate.identity.clone(),
            candidate.probability_blend_sha256.clone(),
            mission.partition_digest.clone(),
        );
        let training = match metrics {
            AuthenticatedTaskMetrics::Settlement(metrics) => {
                PredictionMctsTrainingEvidence::SettlementProbability(SettlementTrainingEvidence {
                    candidate_id: common.0,
                    identity: common.1,
                    probability_blend_sha256: common.2,
                    training_cohort_id: common.3,
                    event_count: metrics.event_count,
                    mean_brier_score: metrics.mean_brier_score,
                    mean_log_loss: metrics.mean_log_loss,
                })
            }
            AuthenticatedTaskMetrics::UpExecution(metrics) => {
                PredictionMctsTrainingEvidence::UpExecution(execution_training_evidence(
                    common, metrics,
                ))
            }
            AuthenticatedTaskMetrics::DownExecution(metrics) => {
                PredictionMctsTrainingEvidence::DownExecution(execution_training_evidence(
                    common, metrics,
                ))
            }
        };
        Ok(PredictionMctsEvaluation { training })
    }

    fn evaluate_selected(
        &mut self,
        mission: &PredictionResearchMissionV3,
        held_out: &AuthenticatedHeldOutSnapshot,
        _artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<AuthenticatedEvaluationArtifact, String> {
        if timeout.is_zero() {
            return Err("authenticated held-out evaluator deadline exhausted".to_string());
        }
        candidate_matches_mission(mission, candidate)?;
        let started = Instant::now();
        let metrics = builtin_task_metrics(mission, &held_out.0)?;
        ensure_metric_computation_within_timeout(started, timeout)?;
        AuthenticatedEvaluationArtifact::new(candidate, metrics)
    }
}

fn ensure_metric_computation_within_timeout(
    started: Instant,
    timeout: Duration,
) -> Result<(), String> {
    if timeout.is_zero() || started.elapsed() >= timeout {
        return Err("authenticated evaluator deadline exhausted during metric computation".into());
    }
    Ok(())
}

fn execution_training_evidence(
    common: (String, PredictionMctsIdentity, String, String),
    metrics: ExecutionTaskMetrics,
) -> ExecutionTrainingEvidence {
    ExecutionTrainingEvidence {
        candidate_id: common.0,
        identity: common.1,
        probability_blend_sha256: common.2,
        training_cohort_id: common.3,
        event_count: metrics.event_count,
        prediction_horizon_secs: metrics.prediction_horizon_secs,
        mean_fill_rate: metrics.mean_fill_rate,
        mean_fee_usd: metrics.mean_total_fee_usd,
        mean_entry_slippage_bps: metrics.mean_entry_slippage_bps,
        mean_exit_slippage_bps: metrics.mean_exit_slippage_bps,
        mean_capacity_usd: metrics.mean_capacity_usd,
        mean_reprice_pnl: metrics.mean_reprice_pnl,
    }
}

fn candidate_matches_mission(
    mission: &PredictionResearchMissionV3,
    candidate: &PredictionMctsCandidate,
) -> Result<(), String> {
    validate_prediction_mission_v3(mission)?;
    candidate.identity.validate()?;
    crate::prediction_loop::validate_sha256_id(
        &candidate.probability_blend_sha256,
        "candidate probability blend",
    )?;
    let expected_task = match mission.task.kind {
        PredictionTaskKind::SettlementProbability => PredictionMctsTask::SettlementProbability,
        PredictionTaskKind::UpExecution => PredictionMctsTask::UpExecution {
            prediction_horizon_secs: mission.task.prediction_horizon_secs.unwrap_or_default(),
        },
        PredictionTaskKind::DownExecution => PredictionMctsTask::DownExecution {
            prediction_horizon_secs: mission.task.prediction_horizon_secs.unwrap_or_default(),
        },
    };
    if candidate.identity.mission_id != mission.mission_id
        || candidate.identity.data_snapshot_id != mission.snapshot_contract_id
        || candidate.identity.task != expected_task
    {
        return Err("prediction candidate does not match its authenticated Mission".to_string());
    }
    Ok(())
}

pub(crate) mod sealed {
    pub trait Evaluator {}
}

pub trait AuthenticatedPredictionMctsEvaluator: sealed::Evaluator {
    fn evaluate_baseline(
        &mut self,
        mission: &PredictionResearchMissionV3,
        training: &AuthenticatedTrainingSnapshot,
        artifact_dir: &Path,
        timeout: Duration,
    ) -> Result<(), String>;

    fn evaluate_training(
        &mut self,
        mission: &PredictionResearchMissionV3,
        training: &AuthenticatedTrainingSnapshot,
        artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<PredictionMctsEvaluation, String>;

    fn evaluate_selected(
        &mut self,
        mission: &PredictionResearchMissionV3,
        held_out: &AuthenticatedHeldOutSnapshot,
        artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<AuthenticatedEvaluationArtifact, String>;
}

pub struct AuthenticatedPredictionMctsTrialRun {
    pub summary: LoopRunSummary,
    pub receipt: AuthenticatedPredictionResultReceiptRef,
}

struct EvaluatorAdapter<'a, E> {
    mission: &'a PredictionResearchMissionV3,
    views: &'a AuthenticatedSnapshotViews,
    evaluator: &'a mut E,
    held_out_artifact: Option<PredictionMctsHeldOutArtifact>,
}

impl<E: AuthenticatedPredictionMctsEvaluator> PredictionMctsRunEvaluator
    for EvaluatorAdapter<'_, E>
{
    fn evaluate_baseline(
        &mut self,
        _mission: &PredictionResearchMission,
        _snapshot_dir: &Path,
        artifact_dir: &Path,
        timeout: Duration,
    ) -> Result<(), String> {
        self.evaluator
            .evaluate_baseline(self.mission, &self.views.training, artifact_dir, timeout)
    }

    fn evaluate_training(
        &mut self,
        _mission: &PredictionResearchMission,
        _snapshot_dir: &Path,
        artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<PredictionMctsEvaluation, String> {
        self.evaluator.evaluate_training(
            self.mission,
            &self.views.training,
            artifact_dir,
            candidate,
            timeout,
        )
    }

    fn evaluate_selected(
        &mut self,
        _mission: &PredictionResearchMission,
        _snapshot_dir: &Path,
        artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<(), String> {
        let artifact = self.evaluator.evaluate_selected(
            self.mission,
            self.views.held_out(),
            artifact_dir,
            candidate,
            timeout,
        )?;
        artifact.validate()?;
        self.held_out_artifact = Some(PredictionMctsHeldOutArtifact::from_serializable(&artifact)?);
        Ok(())
    }

    fn take_held_out_artifact(&mut self) -> Result<Option<PredictionMctsHeldOutArtifact>, String> {
        Ok(self.held_out_artifact.take())
    }
}

pub fn run_or_resume_authenticated_prediction_mcts_trial<
    C: ProposalClient,
    E: AuthenticatedPredictionMctsEvaluator,
>(
    mission: &PredictionResearchMissionV3,
    admitted: &AdmittedPredictionMissionV3,
    snapshot: &AuthenticatedResearchSnapshot,
    immutable_image_identity: &str,
    output_dir: &Path,
    client: &mut C,
    evaluator: &mut E,
) -> Result<AuthenticatedPredictionMctsTrialRun, String> {
    validate_prediction_mission_v3(mission)?;
    crate::prediction_loop::validate_sha256_id(immutable_image_identity, "immutable image")?;
    let expected = admit_prediction_mission_v3(
        mission,
        &authenticate_prediction_mission_v3_inputs(snapshot, mission)?,
        None,
    )?;
    if mission.run_mode != PredictionRunMode::ResearchTrial
        || expected != *admitted
        || admitted.search_policy_snapshot_id != current_prediction_policy_snapshot_id()
    {
        return Err("ResearchTrial identity does not match its authenticated snapshot".to_string());
    }
    let views = authenticated_snapshot_views(snapshot)?;
    let bridge = legacy_bridge(mission, snapshot.partition_view().common_time_boundary_ms());
    let identity = PredictionMctsIdentity::from_admitted_mission(admitted)?;
    let summary = {
        let mut adapter = EvaluatorAdapter {
            mission,
            views: &views,
            evaluator,
            held_out_artifact: None,
        };
        run_or_resume_authenticated_prediction_mcts(
            bridge,
            identity.clone(),
            (mission, admitted),
            output_dir,
            client,
            &mut adapter,
            SettlementProbabilityComponentProfile::MarketMidpointOnly,
            immutable_image_identity,
        )?
    };
    let task_dir = task_output_dir(output_dir, &identity)?;
    let selection = authenticated_selection_evidence(output_dir, &identity)?;
    if !selection.held_out_complete {
        return Err("held-out evaluation is not durably complete".to_string());
    }
    if selection.immutable_image_identity.as_deref() != Some(immutable_image_identity) {
        return Err("immutable evaluator image does not match the durable run".to_string());
    }
    let artifact_ref = selection
        .held_out_artifact
        .as_ref()
        .ok_or_else(|| "durable held-out evaluation artifact is missing".to_string())?;
    let artifact: AuthenticatedEvaluationArtifact =
        serde_json::from_slice(&read_verified_artifact_bounded(
            &task_dir,
            artifact_ref,
            MAX_AUTHENTICATED_EVALUATION_BYTES,
        )?)
        .map_err(|error| format!("parse authenticated held-out evaluation: {error}"))?;
    artifact.validate()?;
    let receipt = AuthenticatedPredictionResultReceipt::new(
        mission,
        admitted,
        immutable_image_identity,
        &selection,
        &artifact,
    )?;
    let receipt = write_authenticated_prediction_result_receipt(output_dir, admitted, &receipt)?;
    Ok(AuthenticatedPredictionMctsTrialRun { summary, receipt })
}

fn legacy_bridge(
    mission: &PredictionResearchMissionV3,
    common_time_boundary_ms: i64,
) -> PredictionResearchMission {
    let mut bridge = PredictionResearchMission {
        schema_version: PREDICTION_MISSION_SCHEMA_VERSION.to_string(),
        mission_id: mission.mission_id.clone(),
        lane: "prediction_market".to_string(),
        objective: "Run the admitted Mission v4 task on its authenticated cohort".to_string(),
        hypothesis_scope: "Mutate only typed probability blend weights".to_string(),
        mutable_scope: vec!["probability_blend_weights".to_string()],
        data_snapshot_id: mission.snapshot_contract_id.clone(),
        target: PREDICTION_LOOP_TARGET.to_string(),
        symbols: vec!["BTC".to_string()],
        horizon: "5m".to_string(),
        time_cohort_boundary_ms: common_time_boundary_ms,
        prompt_snapshot_id: String::new(),
        search_policy_snapshot_id: mission.search_policy_snapshot_id.clone(),
        search_budget: mission.search_budget.clone(),
    };
    bridge.prompt_snapshot_id = research_brief_snapshot_id(&bridge);
    bridge
}

pub(crate) struct AuthenticatedSnapshotViews {
    pub training: AuthenticatedTrainingSnapshot,
    held_out: AuthenticatedHeldOutSnapshot,
}

impl AuthenticatedSnapshotViews {
    pub(crate) fn held_out(&self) -> &AuthenticatedHeldOutSnapshot {
        &self.held_out
    }
}

pub(crate) fn authenticated_snapshot_views(
    authenticated: &AuthenticatedResearchSnapshot,
) -> Result<AuthenticatedSnapshotViews, String> {
    let snapshot = authenticated.verified_readback()?;
    let view = authenticated.partition_view();
    let mut available = snapshot
        .observations
        .iter()
        .map(|row| row.event_id.as_str())
        .chain(
            snapshot
                .pm_book_snapshots
                .iter()
                .map(|row| row.event_id.as_str()),
        )
        .collect::<BTreeSet<_>>();
    available.extend(
        view.crossing_excluded_market_ids()
            .iter()
            .map(String::as_str),
    );
    let available = available.into_iter().collect::<Vec<_>>();
    let train = view
        .train_market_ids()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let crossing = view
        .crossing_excluded_market_ids()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let held_out = view
        .held_out_market_ids()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    partition_market_ids(&train, &crossing, &held_out, &available)?;
    Ok(AuthenticatedSnapshotViews {
        training: snapshot_view(&snapshot, view.train_market_ids())?,
        held_out: AuthenticatedHeldOutSnapshot(snapshot_view(
            &snapshot,
            view.held_out_market_ids(),
        )?),
    })
}

fn snapshot_view(
    snapshot: &ResearchSnapshot,
    ordered_market_ids: &[String],
) -> Result<AuthenticatedTrainingSnapshot, String> {
    let mut observations = Vec::new();
    let mut pm_book_snapshots = Vec::new();
    for market_id in ordered_market_ids {
        let before = observations.len();
        observations.extend(
            snapshot
                .observations
                .iter()
                .filter(|row| row.event_id == *market_id)
                .cloned(),
        );
        if observations.len() == before {
            return Err(format!(
                "authenticated snapshot has no observations for {market_id}"
            ));
        }
        pm_book_snapshots.extend(
            snapshot
                .pm_book_snapshots
                .iter()
                .filter(|row| row.event_id == *market_id)
                .cloned(),
        );
    }
    Ok(AuthenticatedTrainingSnapshot {
        market_ids: ordered_market_ids.to_vec(),
        observations,
        pm_book_snapshots,
        max_quote_age_secs: snapshot.manifest.max_quote_age_secs,
        stake_usd: snapshot.manifest.stake_usd,
    })
}

fn partition_market_ids<'a>(
    train: &[&'a str],
    crossing: &[&str],
    held_out: &[&'a str],
    available: &[&str],
) -> Result<(Vec<&'a str>, Vec<&'a str>), String> {
    let assigned = train
        .iter()
        .chain(crossing)
        .chain(held_out)
        .copied()
        .collect::<BTreeSet<_>>();
    let available = available.iter().copied().collect::<BTreeSet<_>>();
    if assigned != available
        || train
            .iter()
            .any(|id| crossing.contains(id) || held_out.contains(id))
        || crossing.iter().any(|id| held_out.contains(id))
    {
        return Err("authenticated snapshot does not exactly match its sealed partition".into());
    }
    Ok((train.to_vec(), held_out.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prediction_mission_v3::{
        AdmittedPredictionTask, PredictionAuthorityProfile, PredictionMissionCapability,
        PredictionMissionTask, PredictionProductIdentity, PredictionProductSymbol,
        PredictionTaskKind, PredictionTokenSide, PREDICTION_MISSION_V3_SCHEMA_VERSION,
    };
    use chrono::Utc;

    #[test]
    fn authenticated_partition_preserves_catalog_order_and_excludes_crossing() {
        let result = partition_market_ids(
            &["train-b", "train-a"],
            &["crossing"],
            &["held-b", "held-a"],
            &["held-a", "train-a", "crossing", "train-b", "held-b"],
        )
        .expect("sealed partition view");

        assert_eq!(result.0, ["train-b", "train-a"]);
        assert_eq!(result.1, ["held-b", "held-a"]);
    }

    #[test]
    fn authenticated_partition_rejects_missing_or_unassigned_markets() {
        assert!(partition_market_ids(&["train"], &[], &["held"], &["train"]).is_err());
        assert!(
            partition_market_ids(&["train"], &[], &["held"], &["train", "held", "extra"]).is_err()
        );
    }

    #[test]
    fn builtin_metrics_keep_settlement_and_side_execution_surfaces_separate() {
        let snapshot = authenticated_training_snapshot();
        let settlement_mission = mission(PredictionTaskKind::SettlementProbability, None, None);
        let settlement =
            builtin_task_metrics(&settlement_mission, &snapshot).expect("settlement metrics");
        let up_mission = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let up = builtin_task_metrics(&up_mission, &snapshot).expect("Up execution metrics");
        let down = builtin_task_metrics(
            &mission(
                PredictionTaskKind::DownExecution,
                Some(PredictionTokenSide::Down),
                Some(10),
            ),
            &snapshot,
        )
        .expect("Down execution metrics");

        let AuthenticatedTaskMetrics::Settlement(settlement_metrics) = &settlement else {
            panic!("typed settlement metrics");
        };
        assert_eq!(settlement_metrics.event_count, 1);
        assert_eq!(settlement_metrics.decision_count, 2);

        let AuthenticatedTaskMetrics::UpExecution(up_metrics) = &up else {
            panic!("typed Up metrics");
        };
        let AuthenticatedTaskMetrics::DownExecution(down_metrics) = &down else {
            panic!("typed Down metrics");
        };
        assert_eq!(
            up_metrics.joint_state_sha256,
            down_metrics.joint_state_sha256
        );
        assert!(up_metrics.mean_entry_shares > 0.0);
        assert!(up_metrics.mean_entry_notional_usd > 0.0);
        assert!(up_metrics.mean_total_fee_usd > 0.0);
        assert!(up_metrics.mean_capacity_usd > 0.0);
        assert!(up_metrics.mean_complete_set_capacity_shares > 0.0);

        let mut no_depth = snapshot.clone();
        no_depth.pm_book_snapshots.clear();
        assert!(builtin_task_metrics(&settlement_mission, &no_depth).is_err());

        let mut flipped_labels = snapshot.clone();
        for row in &mut flipped_labels.observations {
            row.settlement_up = 1.0 - row.settlement_up;
        }
        assert_eq!(
            builtin_task_metrics(&up_mission, &flipped_labels,)
                .expect("execution ignores settlement"),
            up
        );

        let mut missing_quote = snapshot.clone();
        missing_quote.observations[1].pm_up_bid = f64::NAN;
        assert!(builtin_task_metrics(&settlement_mission, &missing_quote).is_err());
        let mut missing_book = snapshot.clone();
        let missing_book_at = snapshot.observations[0].tick_ts;
        missing_book
            .pm_book_snapshots
            .retain(|book| book.ts != missing_book_at || book.side != "DOWN");
        assert!(builtin_task_metrics(&up_mission, &missing_book).is_err());

        let candidate = candidate(&up_mission);
        let mut evaluator = BuiltInAuthenticatedPredictionMctsEvaluator;
        let training = evaluator
            .evaluate_training(
                &up_mission,
                &snapshot,
                Path::new("unused"),
                &candidate,
                Duration::from_secs(60),
            )
            .expect("typed training evidence");
        let PredictionMctsTrainingEvidence::UpExecution(training) = training.training else {
            panic!("Up mission must emit only Up evidence");
        };
        assert_eq!(training.prediction_horizon_secs, 10);
        assert!(training.mean_capacity_usd > 0.0);

        let held_out = AuthenticatedHeldOutSnapshot(snapshot);
        let selected = evaluator
            .evaluate_selected(
                &up_mission,
                &held_out,
                Path::new("unused"),
                &candidate,
                Duration::from_secs(60),
            )
            .expect("typed selected evidence");
        selected.validate().expect("self-authenticated artifact");
        assert!(matches!(
            selected.metrics,
            AuthenticatedTaskMetrics::UpExecution(_)
        ));
    }

    #[test]
    fn execution_metrics_use_snapshot_bound_stake_and_quote_age() {
        let mission = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let mut snapshot = authenticated_training_snapshot();
        snapshot.stake_usd = 25.0;
        let AuthenticatedTaskMetrics::UpExecution(metrics) =
            builtin_task_metrics(&mission, &snapshot).expect("snapshot-bound execution metrics")
        else {
            panic!("typed Up metrics");
        };
        assert!((metrics.mean_entry_notional_usd - 25.0).abs() < 1e-9);

        let future_ts = snapshot.observations[1].tick_ts;
        snapshot.max_quote_age_secs = 1;
        for book in &mut snapshot.pm_book_snapshots {
            if book.ts == future_ts {
                book.ts -= chrono::Duration::seconds(2);
            }
        }
        assert!(builtin_task_metrics(&mission, &snapshot).is_err());
    }

    #[test]
    fn execution_metrics_reject_a_decision_without_requested_horizon_evidence() {
        let mission = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let mut snapshot = authenticated_training_snapshot();
        snapshot.observations.truncate(1);
        assert!(builtin_task_metrics(&mission, &snapshot).is_err());
    }

    #[test]
    fn execution_metrics_reject_hashed_but_invalid_joint_depth() {
        let mission = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let mut snapshot = authenticated_training_snapshot();
        let decision_ts = snapshot.observations[0].tick_ts;
        for book in &mut snapshot.pm_book_snapshots {
            if book.ts == decision_ts {
                for level in book.bids.iter_mut().chain(&mut book.asks) {
                    level.size = 0.0;
                }
            }
        }
        assert!(builtin_task_metrics(&mission, &snapshot).is_err());
    }

    #[test]
    fn settlement_metrics_reject_an_empty_held_out_view() {
        let mission = mission(PredictionTaskKind::SettlementProbability, None, None);
        let mut snapshot = authenticated_training_snapshot();
        snapshot.market_ids.clear();
        snapshot.observations.clear();
        snapshot.pm_book_snapshots.clear();
        assert!(builtin_task_metrics(&mission, &snapshot).is_err());
    }

    #[test]
    fn settlement_log_loss_stays_finite_at_probability_boundary() {
        let mission = mission(PredictionTaskKind::SettlementProbability, None, None);
        let mut snapshot = authenticated_training_snapshot();
        for row in &mut snapshot.observations {
            row.pm_up_bid = 1.0 - f64::EPSILON / 2.0;
            row.pm_up_ask = 1.0 - f64::EPSILON / 2.0;
            row.pm_down_bid = f64::from_bits(1);
            row.pm_down_ask = f64::from_bits(1);
        }
        assert_eq!(
            joint_binary_market_probability(
                snapshot.observations[0].pm_up_bid,
                snapshot.observations[0].pm_up_ask,
                snapshot.observations[0].pm_down_bid,
                snapshot.observations[0].pm_down_ask,
            ),
            Some(1.0)
        );
        for outcome in [1.0, 0.0] {
            for row in &mut snapshot.observations {
                row.settlement_up = outcome;
            }
            let AuthenticatedTaskMetrics::Settlement(metrics) =
                builtin_task_metrics(&mission, &snapshot).expect("boundary settlement metrics")
            else {
                panic!("typed settlement metrics");
            };
            assert!(metrics.mean_log_loss.is_finite());
        }
    }

    #[test]
    fn settlement_metrics_bind_both_sides_full_depth_pnl() {
        let mission = mission(PredictionTaskKind::SettlementProbability, None, None);
        let AuthenticatedTaskMetrics::Settlement(mut metrics) =
            builtin_task_metrics(&mission, &authenticated_training_snapshot()).unwrap()
        else {
            unreachable!()
        };

        assert_eq!(metrics.up_executable_count, 2);
        assert_eq!(metrics.down_executable_count, 2);
        assert!(metrics.total_up_full_depth_settlement_pnl_usd_micros > 0);
        assert!(metrics.total_down_full_depth_settlement_pnl_usd_micros < 0);
        metrics.up_executable_count = metrics.decision_count + 1;
        assert!(AuthenticatedTaskMetrics::Settlement(metrics)
            .validate()
            .is_err());
    }

    #[test]
    fn synchronous_metric_computation_cannot_outlive_its_timeout() {
        let mission = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let snapshot = authenticated_training_snapshot();
        let candidate = candidate(&mission);
        let error = BuiltInAuthenticatedPredictionMctsEvaluator
            .evaluate_training(
                &mission,
                &snapshot,
                Path::new("unused"),
                &candidate,
                Duration::from_nanos(1),
            )
            .expect_err("expired synchronous computation must fail closed");
        assert!(error.contains("deadline"));
    }

    #[test]
    fn evaluator_artifact_digest_survives_canonical_json_readback() {
        let mission = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let candidate = candidate(&mission);
        let AuthenticatedTaskMetrics::UpExecution(mut metrics) =
            builtin_task_metrics(&mission, &authenticated_training_snapshot()).unwrap()
        else {
            unreachable!()
        };
        metrics.mean_complete_set_edge_usd = -1.0554999999999999;
        let artifact = AuthenticatedEvaluationArtifact::new(
            &candidate,
            AuthenticatedTaskMetrics::UpExecution(metrics),
        )
        .unwrap();
        let output = tempfile::tempdir().unwrap();
        let artifact_ref = write_content_addressed_json(
            output.path(),
            output.path(),
            "prediction-mcts-held-out-evaluation",
            &artifact,
        )
        .unwrap();
        let readback: AuthenticatedEvaluationArtifact = serde_json::from_slice(
            &read_verified_artifact_bounded(
                output.path(),
                &artifact_ref,
                MAX_AUTHENTICATED_EVALUATION_BYTES,
            )
            .unwrap(),
        )
        .unwrap();

        readback
            .validate()
            .expect("canonical JSON readback must preserve the sealed artifact digest");
        let image = format!("sha256:{}", "7".repeat(64));
        let mut selection = selection_evidence(&image, &artifact).unwrap();
        selection.held_out_artifact = Some(artifact_ref);
        AuthenticatedPredictionResultReceipt::new(
            &mission,
            &admitted(&mission),
            &image,
            &selection,
            &readback,
        )
        .unwrap()
        .validate()
        .unwrap();
    }

    #[test]
    fn result_receipt_rejects_held_out_artifact_mutated_after_selection_seal() {
        let mission = mission(PredictionTaskKind::SettlementProbability, None, None);
        let candidate = candidate(&mission);
        let metrics = builtin_task_metrics(&mission, &authenticated_training_snapshot()).unwrap();
        let original = AuthenticatedEvaluationArtifact::new(&candidate, metrics.clone()).unwrap();
        let image = format!("sha256:{}", "7".repeat(64));
        let selection = selection_evidence(&image, &original).unwrap();
        let AuthenticatedTaskMetrics::Settlement(mut changed) = metrics else {
            unreachable!()
        };
        changed.mean_brier_score += 0.01;
        let changed = AuthenticatedEvaluationArtifact::new(
            &candidate,
            AuthenticatedTaskMetrics::Settlement(changed),
        )
        .unwrap();

        let error = AuthenticatedPredictionResultReceipt::new(
            &mission,
            &admitted(&mission),
            &image,
            &selection,
            &changed,
        )
        .expect_err("receipt must reject a held-out artifact changed after selection was sealed");
        assert!(error.contains("durable selection"));
    }

    #[test]
    fn experiment_manifest_requires_exactly_three_matching_typed_receipts() {
        let output = tempfile::tempdir().expect("create private experiment directory");
        let output = output.path();
        let settlement = mission(PredictionTaskKind::SettlementProbability, None, None);
        let up = mission(
            PredictionTaskKind::UpExecution,
            Some(PredictionTokenSide::Up),
            Some(10),
        );
        let down = mission(
            PredictionTaskKind::DownExecution,
            Some(PredictionTokenSide::Down),
            Some(10),
        );
        let snapshot = authenticated_training_snapshot();
        let settlement_metrics = builtin_task_metrics(&settlement, &snapshot).unwrap();
        let up_metrics = builtin_task_metrics(&up, &snapshot).unwrap();
        let down_metrics = builtin_task_metrics(&down, &snapshot).unwrap();
        let mut invalid_settlement = settlement_metrics.clone();
        let AuthenticatedTaskMetrics::Settlement(metrics) = &mut invalid_settlement else {
            unreachable!()
        };
        metrics.mean_brier_score = 9.0;
        assert!(result_receipt(&settlement, invalid_settlement).is_err());
        let mut invalid_up = up_metrics.clone();
        if let AuthenticatedTaskMetrics::UpExecution(metrics) = &mut invalid_up {
            metrics.mean_fill_rate = 7.0;
        }
        assert!(result_receipt(&up, invalid_up.clone()).is_err());
        if let AuthenticatedTaskMetrics::UpExecution(metrics) = &mut invalid_up {
            metrics.mean_fill_rate = 0.0;
        }
        assert!(result_receipt(&up, invalid_up.clone()).is_err());
        if let AuthenticatedTaskMetrics::UpExecution(metrics) = &mut invalid_up {
            metrics.joint_state_sha256 = "not-a-digest".to_string();
        }
        assert!(result_receipt(&up, invalid_up).is_err());
        let error = result_receipt(&settlement, up_metrics.clone())
            .expect_err("settlement receipt must reject execution metrics");
        assert!(error.contains("metrics do not match"));
        let settlement_ref = write_test_receipt(output, &settlement, settlement_metrics.clone());
        let up_ref = write_test_receipt(output, &up, up_metrics);
        let down_ref = write_test_receipt(output, &down, down_metrics.clone());

        assert!(settlement_ref.path().contains("/settlement/"));
        assert!(up_ref.path().contains("/up-execution-10s/"));
        assert!(down_ref.path().contains("/down-execution-10s/"));

        assert!(write_authenticated_prediction_experiment_manifest(
            output,
            &[settlement_ref.clone(), up_ref.clone()]
        )
        .is_err());
        assert!(write_authenticated_prediction_experiment_manifest(
            output,
            &[settlement_ref.clone(), up_ref.clone(), up_ref.clone()]
        )
        .is_err());

        let manifest_ref = write_authenticated_prediction_experiment_manifest(
            output,
            &[down_ref.clone(), settlement_ref.clone(), up_ref.clone()],
        )
        .expect("exact-three manifest");
        let manifest =
            read_authenticated_prediction_experiment_manifest(output, &manifest_ref).unwrap();
        assert_eq!(manifest.settlement, settlement_ref);
        assert_eq!(manifest.up_execution, up_ref);
        assert_eq!(manifest.down_execution, down_ref);
        assert!(!manifest.unavailable_claims.profitability.trim().is_empty());
        assert!(!manifest.unavailable_claims.promotion.trim().is_empty());

        let mut positive_claim = manifest.clone();
        positive_claim.unavailable_claims.profitability = "proven profitable".to_string();
        positive_claim.sha256 = positive_claim.expected_sha256().unwrap();
        assert!(positive_claim.validate().is_err());
        positive_claim.unavailable_claims = manifest.unavailable_claims.clone();
        positive_claim.unavailable_claims.promotion = "approved".to_string();
        positive_claim.sha256 = positive_claim.expected_sha256().unwrap();
        assert!(positive_claim.validate().is_err());

        let mut changed_settlement_metrics = settlement_metrics;
        let AuthenticatedTaskMetrics::Settlement(metrics) = &mut changed_settlement_metrics else {
            unreachable!()
        };
        metrics.mean_brier_score += 0.01;
        let changed_settlement_ref =
            write_test_receipt(output, &settlement, changed_settlement_metrics);
        let changed_receipt =
            read_authenticated_prediction_result_receipt(output, &changed_settlement_ref).unwrap();
        let original_receipt =
            read_authenticated_prediction_result_receipt(output, &manifest.settlement).unwrap();
        assert!(
            original_receipt.held_out_completed_unix_millis
                >= original_receipt.run_started_unix_millis
        );
        assert!(
            original_receipt.result_sealed_unix_millis
                >= original_receipt.held_out_completed_unix_millis
        );
        let mut reversed_lifecycle = original_receipt.clone();
        reversed_lifecycle.run_started_unix_millis =
            reversed_lifecycle.held_out_completed_unix_millis + 1;
        reversed_lifecycle.sha256 = reversed_lifecycle.expected_sha256().unwrap();
        assert!(reversed_lifecycle.validate().is_err());
        assert_eq!(
            original_receipt.mission_sha256,
            crate::prediction_mission_v3::prediction_mission_v3_sha256(&settlement).unwrap()
        );
        assert_eq!(
            changed_receipt.checkpoint_sha256,
            original_receipt.checkpoint_sha256
        );
        assert_eq!(
            changed_receipt.selected_candidate_sha256,
            original_receipt.selected_candidate_sha256
        );
        assert_ne!(
            changed_receipt.evaluator_artifact_sha256,
            original_receipt.evaluator_artifact_sha256
        );
        assert_ne!(changed_receipt.sha256, original_receipt.sha256);
        let changed_manifest_ref = write_authenticated_prediction_experiment_manifest(
            output,
            &[
                changed_settlement_ref,
                manifest.up_execution,
                manifest.down_execution,
            ],
        )
        .unwrap();
        assert_ne!(
            changed_manifest_ref.manifest_sha256(),
            manifest_ref.manifest_sha256()
        );

        let mut drifted_down_metrics = down_metrics;
        let AuthenticatedTaskMetrics::DownExecution(metrics) = &mut drifted_down_metrics else {
            unreachable!()
        };
        metrics.joint_state_sha256 = format!("sha256:{}", "9".repeat(64));
        let drifted_down_ref = write_test_receipt(output, &down, drifted_down_metrics);
        let error = write_authenticated_prediction_experiment_manifest(
            output,
            &[settlement_ref, up_ref.clone(), drifted_down_ref],
        )
        .expect_err("Up and Down must bind the same joint state");
        assert!(error.contains("joint state"));

        std::fs::write(output.join(up_ref.path()), b"{}\n").unwrap();
        assert!(read_authenticated_prediction_experiment_manifest(output, &manifest_ref).is_err());
    }

    fn mission(
        kind: PredictionTaskKind,
        side: Option<PredictionTokenSide>,
        prediction_horizon_secs: Option<u32>,
    ) -> PredictionResearchMissionV3 {
        PredictionResearchMissionV3 {
            schema_version: PREDICTION_MISSION_V3_SCHEMA_VERSION.to_string(),
            mission_id: format!("test-{kind:?}"),
            product: PredictionProductIdentity {
                symbol: PredictionProductSymbol::Btc,
                event_horizon_secs: 300,
            },
            task: PredictionMissionTask {
                kind,
                side,
                prediction_horizon_secs,
            },
            run_mode: PredictionRunMode::ResearchTrial,
            authority_profile: PredictionAuthorityProfile::PolymarketChainlinkBaseline,
            required_capabilities: BTreeSet::from([
                PredictionMissionCapability::PolymarketChainlink,
            ]),
            cohort_manifest_id: format!("sha256:{}", "1".repeat(64)),
            partition_digest: format!("sha256:{}", "2".repeat(64)),
            causal_projection_policy_id: current_prediction_policy_snapshot_id(),
            snapshot_contract_id: format!("sha256:{}", "3".repeat(64)),
            snapshot_hash: "4".repeat(16),
            search_policy_snapshot_id: current_prediction_policy_snapshot_id(),
            search_budget: crate::prediction_loop::PredictionSearchBudget {
                max_candidates: 1,
                max_llm_calls: 1,
                max_seconds: 60,
            },
        }
    }

    fn authenticated_training_snapshot() -> AuthenticatedTrainingSnapshot {
        let tick_ts = Utc::now();
        let mut current = observation(tick_ts);
        current.settlement_up = 1.0;
        let mut future = current.clone();
        future.tick_ts += chrono::Duration::seconds(10);
        future.time_remaining_secs = 0;
        future.pm_up_bid = 0.70;
        future.pm_down_bid = 0.20;
        AuthenticatedTrainingSnapshot {
            market_ids: vec![current.event_id.clone()],
            observations: vec![current, future],
            pm_book_snapshots: books(tick_ts),
            max_quote_age_secs: 30,
            stake_usd: 15.0,
        }
    }

    fn admitted(mission: &PredictionResearchMissionV3) -> AdmittedPredictionMissionV3 {
        let task = match mission.task.kind {
            PredictionTaskKind::SettlementProbability => {
                AdmittedPredictionTask::SettlementProbability
            }
            PredictionTaskKind::UpExecution => AdmittedPredictionTask::UpExecution {
                prediction_horizon_secs: mission.task.prediction_horizon_secs.unwrap(),
            },
            PredictionTaskKind::DownExecution => AdmittedPredictionTask::DownExecution {
                prediction_horizon_secs: mission.task.prediction_horizon_secs.unwrap(),
            },
        };
        AdmittedPredictionMissionV3 {
            mission_id: mission.mission_id.clone(),
            mission_sha256: crate::prediction_mission_v3::prediction_mission_v3_sha256(mission)
                .unwrap(),
            product: mission.product.clone(),
            task,
            run_mode: mission.run_mode,
            authority_profile: mission.authority_profile,
            cohort_manifest_id: mission.cohort_manifest_id.clone(),
            partition_digest: mission.partition_digest.clone(),
            causal_projection_policy_id: mission.causal_projection_policy_id.clone(),
            snapshot_contract_id: mission.snapshot_contract_id.clone(),
            snapshot_hash: mission.snapshot_hash.clone(),
            search_policy_snapshot_id: mission.search_policy_snapshot_id.clone(),
        }
    }

    fn result_receipt(
        mission: &PredictionResearchMissionV3,
        metrics: AuthenticatedTaskMetrics,
    ) -> Result<AuthenticatedPredictionResultReceipt, String> {
        let admitted = admitted(mission);
        let candidate = candidate(mission);
        let artifact = AuthenticatedEvaluationArtifact::new(&candidate, metrics)?;
        let image = format!("sha256:{}", "7".repeat(64));
        let selection = selection_evidence(&image, &artifact)?;
        AuthenticatedPredictionResultReceipt::new(mission, &admitted, &image, &selection, &artifact)
    }

    fn selection_evidence(
        image: &str,
        artifact: &AuthenticatedEvaluationArtifact,
    ) -> Result<crate::prediction_mcts_run::PredictionMctsSelectionEvidence, String> {
        Ok(
            crate::prediction_mcts_run::PredictionMctsSelectionEvidence {
                held_out_complete: true,
                immutable_image_identity: Some(image.to_string()),
                run_started_unix_millis: 1,
                held_out_completed_unix_millis: 2,
                result_sealed_unix_millis: 3,
                held_out_artifact: Some(ArtifactRef {
                    path: "test-held-out-evaluation.json".to_string(),
                    sha256: artifact
                        .content_sha256()?
                        .strip_prefix("sha256:")
                        .unwrap()
                        .to_string(),
                }),
                checkpoint_sha256: format!("sha256:{}", "8".repeat(64)),
                selected_candidate_sha256: artifact.selected_candidate_sha256.clone(),
            },
        )
    }

    fn write_test_receipt(
        output: &Path,
        mission: &PredictionResearchMissionV3,
        metrics: AuthenticatedTaskMetrics,
    ) -> AuthenticatedPredictionResultReceiptRef {
        let receipt = result_receipt(mission, metrics).unwrap();
        write_authenticated_prediction_result_receipt(output, &admitted(mission), &receipt).unwrap()
    }

    fn candidate(mission: &PredictionResearchMissionV3) -> PredictionMctsCandidate {
        let admitted = admitted(mission);
        PredictionMctsCandidate {
            candidate_id: "candidate-1".to_string(),
            identity: PredictionMctsIdentity::from_admitted_mission(&admitted).unwrap(),
            probability_blend_sha256: format!("sha256:{}", "6".repeat(64)),
            source: crate::prediction_mcts::PredictionExpansionSource::Baseline,
            probability_blend: crate::autofactor::LlmProbabilityBlendSpec {
                name: "market_midpoint".to_string(),
                hypothesis: "joint binary-book midpoint".to_string(),
                market_midpoint_weight: 1.0,
                chainlink_digital_weight: 0.0,
                distance_lob_vol_weight: 0.0,
                event_surface_weight: 0.0,
                existing_model_weight: 0.0,
            },
        }
    }

    fn observation(tick_ts: chrono::DateTime<Utc>) -> FactorObservation {
        serde_json::from_value(serde_json::json!({
            "event_id": "event-1",
            "symbol": "BTCUSDT",
            "tick_ts": tick_ts,
            "up_token_id": "up-token",
            "down_token_id": "down-token",
            "time_remaining_secs": 60,
            "pm_up_bid": 0.49,
            "pm_up_ask": 0.51,
            "pm_down_bid": 0.49,
            "pm_down_ask": 0.51,
            "pm_lag_secs": 0.0,
            "settlement_up": 1.0,
            "future_up_ask_change_30s": null,
            "future_up_ask_change_60s": null
        }))
        .expect("test observation")
    }

    fn books(tick_ts: chrono::DateTime<Utc>) -> Vec<ResearchPmBookSnapshot> {
        [tick_ts, tick_ts + chrono::Duration::seconds(10)]
            .into_iter()
            .flat_map(|ts| {
                [
                    (
                        "up-token",
                        "UP",
                        if ts == tick_ts { 0.49 } else { 0.70 },
                        if ts == tick_ts { 0.51 } else { 0.72 },
                    ),
                    (
                        "down-token",
                        "DOWN",
                        if ts == tick_ts { 0.49 } else { 0.20 },
                        if ts == tick_ts { 0.51 } else { 0.22 },
                    ),
                ]
                .into_iter()
                .map(move |(token_id, side, bid, ask)| ResearchPmBookSnapshot {
                    event_id: "event-1".to_string(),
                    token_id: token_id.to_string(),
                    side: side.to_string(),
                    ts,
                    bids: vec![crate::factors::ResearchPmBookLevel {
                        price: bid,
                        size: 100.0,
                    }],
                    asks: vec![crate::factors::ResearchPmBookLevel {
                        price: ask,
                        size: 100.0,
                    }],
                })
            })
            .collect()
    }
}
