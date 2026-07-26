use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::factors::{FactorObservation, ResearchPmBookSnapshot};
use crate::factors_v2::SettlementProbabilityComponentProfile;
use crate::prediction_loop::{
    current_prediction_policy_snapshot_id, research_brief_snapshot_id, LoopRunSummary,
    PredictionResearchMission, ProposalClient, PREDICTION_LOOP_TARGET,
    PREDICTION_MISSION_SCHEMA_VERSION,
};
use crate::prediction_loop_fs::{
    atomic_write_json, read_json, verify_artifact, write_content_addressed_json, ArtifactRef,
};
use crate::prediction_mcts::{
    PredictionMctsCandidate, PredictionMctsEvaluation, PredictionMctsIdentity,
};
use crate::prediction_mcts_run::{
    authenticated_selection_evidence, run_or_resume_authenticated_prediction_mcts, task_output_dir,
    PredictionMctsRunEvaluator, PredictionMctsSelectionEvidence,
};
use crate::prediction_mission_v3::{
    admit_prediction_mission_v3, authenticate_prediction_mission_v3_inputs,
    validate_prediction_mission_v3, AdmittedPredictionMissionV3, AdmittedPredictionTask,
    PredictionResearchMissionV3, PredictionRunMode,
};
use crate::research_snapshot::{AuthenticatedResearchSnapshot, ResearchSnapshot};

const RECEIPT_VERSION: &str = "authenticated_prediction_mcts_trial_receipt.v1";

#[derive(Debug, Clone)]
pub struct AuthenticatedTrainingSnapshot {
    market_ids: Vec<String>,
    observations: Vec<FactorObservation>,
    pm_book_snapshots: Vec<ResearchPmBookSnapshot>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedEvaluationArtifact {
    pub sha256: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPredictionMctsReceiptEvidence {
    pub schema_version: String,
    pub mission_sha256: String,
    pub cohort_manifest_id: String,
    pub partition_digest: String,
    pub causal_projection_policy_id: String,
    pub search_policy_snapshot_id: String,
    pub snapshot_contract_id: String,
    pub snapshot_hash: String,
    pub immutable_image_identity: String,
    pub common_time_boundary_ms: i64,
    pub train_market_ids: Vec<String>,
    pub crossing_excluded_market_ids: Vec<String>,
    pub held_out_market_ids: Vec<String>,
    pub checkpoint_sha256: String,
    pub selected_candidate_sha256: String,
    pub evaluator_artifact_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "task", rename_all = "snake_case")]
pub enum AuthenticatedPredictionMctsReceipt {
    SettlementProbability {
        #[serde(flatten)]
        evidence: AuthenticatedPredictionMctsReceiptEvidence,
    },
    UpExecution {
        prediction_horizon_secs: u32,
        #[serde(flatten)]
        evidence: AuthenticatedPredictionMctsReceiptEvidence,
    },
    DownExecution {
        prediction_horizon_secs: u32,
        #[serde(flatten)]
        evidence: AuthenticatedPredictionMctsReceiptEvidence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatedPredictionMctsReceiptArtifact {
    pub path: String,
    pub sha256: String,
}

pub struct AuthenticatedPredictionMctsTrialRun {
    pub summary: LoopRunSummary,
    pub receipt: Option<AuthenticatedPredictionMctsReceiptArtifact>,
}

struct EvaluatorAdapter<'a, E> {
    mission: &'a PredictionResearchMissionV3,
    views: &'a AuthenticatedSnapshotViews,
    output_dir: &'a Path,
    identity: &'a PredictionMctsIdentity,
    admitted: &'a AdmittedPredictionMissionV3,
    snapshot: &'a AuthenticatedResearchSnapshot,
    immutable_image_identity: &'a str,
    evaluator: &'a mut E,
    receipt: Option<AuthenticatedPredictionMctsReceiptArtifact>,
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
        let selection = authenticated_selection_evidence(self.output_dir, self.identity)?;
        let artifact = self.evaluator.evaluate_selected(
            self.mission,
            self.views.held_out(),
            artifact_dir,
            candidate,
            timeout,
        )?;
        crate::prediction_loop::validate_sha256_id(
            &artifact.sha256,
            "held-out evaluator artifact",
        )?;
        self.receipt = Some(publish_receipt(
            self.admitted,
            self.snapshot,
            self.immutable_image_identity,
            self.output_dir,
            self.identity,
            selection,
            artifact,
        )?);
        Ok(())
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
    let mut adapter = EvaluatorAdapter {
        mission,
        views: &views,
        output_dir,
        identity: &identity,
        admitted,
        snapshot,
        immutable_image_identity,
        evaluator,
        receipt: None,
    };
    let summary = run_or_resume_authenticated_prediction_mcts(
        bridge,
        identity.clone(),
        (mission, admitted),
        output_dir,
        client,
        &mut adapter,
        SettlementProbabilityComponentProfile::MarketMidpointOnly,
    )?;
    let task_dir = task_output_dir(output_dir, &identity)?;
    let receipt_path = task_dir.join("authenticated-receipt-ref.json");
    let receipt = if let Some(receipt) = adapter.receipt {
        Some(receipt)
    } else if receipt_path.exists() {
        Some(read_json(&receipt_path)?)
    } else {
        None
    };
    if let Some(receipt) = &receipt {
        if !authenticated_selection_evidence(output_dir, &identity)?.held_out_complete {
            return Err("authenticated receipt exists before held-out completion".to_string());
        }
        let artifact = ArtifactRef {
            path: receipt.path.clone(),
            sha256: receipt.sha256.clone(),
        };
        if !verify_artifact(output_dir, &artifact)?.starts_with(task_dir.join("receipts")) {
            return Err("authenticated receipt escaped its task namespace".to_string());
        }
    }
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

#[allow(clippy::too_many_arguments)]
fn publish_receipt(
    admitted: &AdmittedPredictionMissionV3,
    snapshot: &AuthenticatedResearchSnapshot,
    immutable_image_identity: &str,
    output_dir: &Path,
    identity: &PredictionMctsIdentity,
    selection: PredictionMctsSelectionEvidence,
    artifact: AuthenticatedEvaluationArtifact,
) -> Result<AuthenticatedPredictionMctsReceiptArtifact, String> {
    let view = snapshot.partition_view();
    let evidence = AuthenticatedPredictionMctsReceiptEvidence {
        schema_version: RECEIPT_VERSION.to_string(),
        mission_sha256: admitted.mission_sha256.clone(),
        cohort_manifest_id: admitted.cohort_manifest_id.clone(),
        partition_digest: admitted.partition_digest.clone(),
        causal_projection_policy_id: admitted.causal_projection_policy_id.clone(),
        search_policy_snapshot_id: admitted.search_policy_snapshot_id.clone(),
        snapshot_contract_id: admitted.snapshot_contract_id.clone(),
        snapshot_hash: admitted.snapshot_hash.clone(),
        immutable_image_identity: immutable_image_identity.to_string(),
        common_time_boundary_ms: view.common_time_boundary_ms(),
        train_market_ids: view.train_market_ids().to_vec(),
        crossing_excluded_market_ids: view.crossing_excluded_market_ids().to_vec(),
        held_out_market_ids: view.held_out_market_ids().to_vec(),
        checkpoint_sha256: format!("sha256:{}", selection.checkpoint_sha256),
        selected_candidate_sha256: format!("sha256:{}", selection.selected_candidate_sha256),
        evaluator_artifact_sha256: artifact.sha256,
    };
    let receipt = match admitted.task {
        AdmittedPredictionTask::SettlementProbability => {
            AuthenticatedPredictionMctsReceipt::SettlementProbability { evidence }
        }
        AdmittedPredictionTask::UpExecution {
            prediction_horizon_secs,
        } => AuthenticatedPredictionMctsReceipt::UpExecution {
            prediction_horizon_secs,
            evidence,
        },
        AdmittedPredictionTask::DownExecution {
            prediction_horizon_secs,
        } => AuthenticatedPredictionMctsReceipt::DownExecution {
            prediction_horizon_secs,
            evidence,
        },
    };
    let task_dir = task_output_dir(output_dir, identity)?;
    let ArtifactRef { path, sha256 } = write_content_addressed_json(
        output_dir,
        &task_dir.join("receipts"),
        "authenticated-prediction-mcts-receipt",
        &receipt,
    )?;
    let receipt = AuthenticatedPredictionMctsReceiptArtifact { path, sha256 };
    atomic_write_json(&task_dir.join("authenticated-receipt-ref.json"), &receipt)?;
    Ok(receipt)
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
}
