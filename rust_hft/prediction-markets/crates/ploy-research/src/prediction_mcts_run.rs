//! Durable official runner for prediction-market MCTS research.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::factors_v2::SettlementProbabilityComponentProfile;
use crate::prediction_loop::{
    build_prediction_prompt, current_prediction_policy_snapshot_id, validate_prediction_mission,
    validate_prediction_proposal, LoopRunStatus, LoopRunSummary, PredictionProposal,
    PredictionResearchMission, ProposalClient, ProposedProbabilityBlend,
};
use crate::prediction_loop_fs::{
    atomic_write_json, next_attempt_dir, read_json, write_content_addressed_json, ArtifactRef,
    OutputLock,
};
use crate::prediction_mcts::{
    PredictionMctsCandidate, PredictionMctsCheckpoint, PredictionMctsCheckpointArtifact,
    PredictionMctsEngine, PredictionMctsEvaluation, PredictionMctsIdentity,
    PredictionMctsTrainingEvidence,
};

const RUN_STATE_VERSION: u32 = 3;
const RUN_ARTIFACT_VERSION: &str = "prediction_mcts_checkpoint_artifact_v2";
const MCTS_SEED: u64 = 7;
const MCTS_EXPLORATION: f64 = 1.4;
const MCTS_MAX_DEPTH: usize = 3;

pub trait PredictionMctsRunEvaluator {
    fn evaluate_baseline(
        &mut self,
        mission: &PredictionResearchMission,
        snapshot_dir: &Path,
        artifact_dir: &Path,
        timeout: Duration,
    ) -> Result<(), String>;

    fn evaluate_training(
        &mut self,
        mission: &PredictionResearchMission,
        snapshot_dir: &Path,
        artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<PredictionMctsEvaluation, String>;

    fn evaluate_selected(
        &mut self,
        mission: &PredictionResearchMission,
        snapshot_dir: &Path,
        artifact_dir: &Path,
        candidate: &PredictionMctsCandidate,
        timeout: Duration,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrainingRecord {
    candidate: PredictionMctsCandidate,
    evaluation: PredictionMctsEvaluation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionMctsRunState {
    version: u32,
    mission: PredictionResearchMission,
    #[serde(default)]
    identity: Option<PredictionMctsIdentity>,
    deadline_unix_millis: u64,
    baseline_complete: bool,
    advisor_call_consumed: bool,
    advisor: Option<Vec<ProposedProbabilityBlend>>,
    advisor_failure: Option<String>,
    checkpoint: Option<PredictionMctsCheckpoint>,
    checkpoint_artifact: Option<ArtifactRef>,
    #[serde(default)]
    budget_exhausted: bool,
    pending: Option<PredictionMctsCandidate>,
    training: Vec<TrainingRecord>,
    selected: Option<PredictionMctsCandidate>,
    held_out_complete: bool,
    pause_reason: Option<String>,
}

#[derive(Serialize)]
struct PredictionMctsRunArtifact {
    version: &'static str,
    identity: PredictionMctsIdentity,
    prompt_snapshot_id: String,
    search_policy_snapshot_id: String,
    phase: &'static str,
    baseline_complete: bool,
    checkpoint: Option<PredictionMctsCheckpointArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending: Option<PredictionMctsCandidate>,
    training: Vec<PredictionMctsTrainingArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected: Option<PredictionMctsCandidate>,
    held_out_complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pause_reason: Option<String>,
}

#[derive(Serialize)]
struct PredictionMctsTrainingArtifact {
    candidate: PredictionMctsCandidate,
    training: PredictionMctsTrainingEvidence,
}

impl PredictionMctsRunState {
    fn identity(&self) -> Result<PredictionMctsIdentity, String> {
        let identity = self
            .identity
            .clone()
            .ok_or_else(|| "prediction MCTS state is missing its typed identity".to_string())?;
        identity.validate()?;
        if identity.mission_id != self.mission.mission_id
            || identity.data_snapshot_id != self.mission.data_snapshot_id
        {
            return Err("prediction MCTS state identity does not match its mission".to_string());
        }
        Ok(identity)
    }

    fn read_only_artifact(&self) -> Result<PredictionMctsRunArtifact, String> {
        let identity = self.identity()?;
        let checkpoint_state = self.checkpoint.as_ref();
        let checkpoint = checkpoint_state
            .map(PredictionMctsCheckpoint::read_only_artifact)
            .transpose()?;
        if checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.identity != identity)
        {
            return Err(
                "prediction MCTS checkpoint identity does not match the mission".to_string(),
            );
        }
        let training = self
            .training
            .iter()
            .map(|record| {
                if record.candidate.identity != identity
                    || record.evaluation.training.identity() != &identity
                    || record.evaluation.training.candidate_id() != record.candidate.candidate_id
                    || record.evaluation.training.probability_blend_sha256()
                        != record.candidate.probability_blend_sha256
                {
                    return Err("invalid prediction MCTS training artifact state".to_string());
                }
                Ok(PredictionMctsTrainingArtifact {
                    candidate: record.candidate.clone(),
                    training: record.evaluation.training.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(pending) = &self.pending {
            if pending.identity != identity {
                return Err(
                    "prediction MCTS candidate identity does not match the mission".to_string(),
                );
            }
            checkpoint_state
                .ok_or_else(|| "prediction MCTS pending candidate has no checkpoint".to_string())?
                .validate_pending_candidate(pending)?;
        }
        if let Some(selected) = &self.selected {
            if selected.identity != identity {
                return Err(
                    "prediction MCTS candidate identity does not match the mission".to_string(),
                );
            }
            if !self
                .training
                .iter()
                .any(|record| record.candidate == *selected)
            {
                return Err(
                    "prediction MCTS selected candidate is not backed by training evidence"
                        .to_string(),
                );
            }
        }
        Ok(PredictionMctsRunArtifact {
            version: RUN_ARTIFACT_VERSION,
            identity,
            prompt_snapshot_id: self.mission.prompt_snapshot_id.clone(),
            search_policy_snapshot_id: self.mission.search_policy_snapshot_id.clone(),
            phase: if self.pause_reason.is_some() {
                "paused"
            } else if self.held_out_complete {
                "held_out_complete"
            } else if self.budget_exhausted {
                "budget_exhausted"
            } else if self.selected.is_some() {
                "selected"
            } else if self.pending.is_some() {
                "training_pending"
            } else if self.checkpoint.is_some() {
                "training_ready"
            } else if self.baseline_complete {
                "baseline_complete"
            } else {
                "baseline_pending"
            },
            baseline_complete: self.baseline_complete,
            checkpoint,
            pending: self.pending.clone(),
            training,
            selected: self.selected.clone(),
            held_out_complete: self.held_out_complete,
            pause_reason: self.pause_reason.clone(),
        })
    }
}

/// Run the shared-kernel prediction controller after the caller has validated
/// the immutable snapshot against the mission. Evaluator failures are durable
/// pauses; rerunning with the same mission resumes the exact pending candidate.
pub fn run_or_resume_prediction_mcts<C: ProposalClient, E: PredictionMctsRunEvaluator>(
    mission: PredictionResearchMission,
    snapshot_dir: &Path,
    output_dir: &Path,
    client: &mut C,
    evaluator: &mut E,
) -> Result<LoopRunSummary, String> {
    run_or_resume_prediction_mcts_with_component_profile(
        mission,
        snapshot_dir,
        output_dir,
        client,
        evaluator,
        SettlementProbabilityComponentProfile::FullSurface,
    )
}

pub fn run_or_resume_prediction_mcts_with_component_profile<
    C: ProposalClient,
    E: PredictionMctsRunEvaluator,
>(
    mission: PredictionResearchMission,
    snapshot_dir: &Path,
    output_dir: &Path,
    client: &mut C,
    evaluator: &mut E,
    component_profile: SettlementProbabilityComponentProfile,
) -> Result<LoopRunSummary, String> {
    let identity = PredictionMctsIdentity::from_mission(&mission)?;
    run_or_resume_prediction_mcts_with_identity_and_component_profile(
        mission,
        identity,
        snapshot_dir,
        output_dir,
        client,
        evaluator,
        component_profile,
    )
}

pub(crate) fn run_or_resume_prediction_mcts_with_identity_and_component_profile<
    C: ProposalClient,
    E: PredictionMctsRunEvaluator,
>(
    mission: PredictionResearchMission,
    identity: PredictionMctsIdentity,
    snapshot_dir: &Path,
    output_dir: &Path,
    client: &mut C,
    evaluator: &mut E,
    component_profile: SettlementProbabilityComponentProfile,
) -> Result<LoopRunSummary, String> {
    validate_prediction_mission(&mission, &current_prediction_policy_snapshot_id())?;
    identity.validate()?;
    if identity.mission_id != mission.mission_id
        || identity.data_snapshot_id != mission.data_snapshot_id
    {
        return Err("prediction MCTS identity does not match its legacy bridge mission".into());
    }
    let output_dir = task_output_dir(output_dir, &identity)?;
    let _lock = OutputLock::acquire(&output_dir)?;
    let state_path = output_dir.join("prediction-mcts-state.json");
    let mut state = if state_path.exists() {
        let mut state: PredictionMctsRunState = read_json(&state_path)?;
        if state.version != RUN_STATE_VERSION
            || state.mission != mission
            || state.identity()? != identity
        {
            return Err(
                "prediction MCTS output uses an incompatible state version or mission".to_string(),
            );
        }
        state.identity = Some(identity.clone());
        state
    } else {
        let deadline_unix_millis = now_unix_millis()
            .checked_add(
                mission
                    .search_budget
                    .max_seconds
                    .checked_mul(1_000)
                    .ok_or_else(|| "prediction MCTS deadline overflow".to_string())?,
            )
            .ok_or_else(|| "prediction MCTS deadline overflow".to_string())?;
        PredictionMctsRunState {
            version: RUN_STATE_VERSION,
            mission: mission.clone(),
            identity: Some(identity.clone()),
            deadline_unix_millis,
            baseline_complete: false,
            advisor_call_consumed: false,
            advisor: None,
            advisor_failure: None,
            checkpoint: None,
            checkpoint_artifact: None,
            budget_exhausted: false,
            pending: None,
            training: Vec::new(),
            selected: None,
            held_out_complete: false,
            pause_reason: None,
        }
    };
    state.pause_reason = None;
    checkpoint(&state_path, &mut state)?;

    if state.budget_exhausted {
        return Ok(summary(
            &mission,
            &state_path,
            &state,
            LoopRunStatus::BudgetExhausted,
        ));
    }

    if !state.baseline_complete {
        let artifact_dir = next_attempt_dir(&output_dir.join("baseline"))?;
        if let Err(reason) = evaluator.evaluate_baseline(
            &mission,
            snapshot_dir,
            &artifact_dir,
            remaining_time(&state),
        ) {
            return pause(&mission, &state_path, &mut state, reason);
        }
        state.baseline_complete = true;
        checkpoint(&state_path, &mut state)?;
    }

    if mission.search_budget.max_candidates == 0 {
        state.budget_exhausted = true;
        checkpoint(&state_path, &mut state)?;
        return Ok(summary(
            &mission,
            &state_path,
            &state,
            LoopRunStatus::BudgetExhausted,
        ));
    }

    if state.advisor.is_none() {
        let advice = if component_profile
            == SettlementProbabilityComponentProfile::MarketMidpointOnly
        {
            state.advisor_failure = Some(
                "reduced-authority baseline has no LLM-expandable probability components"
                    .to_string(),
            );
            Vec::new()
        } else if mission.search_budget.max_llm_calls == 0 {
            Vec::new()
        } else if state.advisor_call_consumed {
            return Err(
                "prediction MCTS advisor call was interrupted before durable response".to_string(),
            );
        } else {
            state.advisor_call_consumed = true;
            checkpoint(&state_path, &mut state)?;
            let prompt =
                build_prediction_prompt(&mission, mission.search_budget.max_candidates, &[]);
            match client.propose(&prompt, remaining_time(&state)) {
                Ok(response) => {
                    match serde_json::from_str::<PredictionProposal>(&response.raw_response)
                        .map_err(|error| format!("strict proposal JSON rejected: {error}"))
                        .and_then(|proposal| {
                            validate_prediction_proposal(
                                proposal,
                                mission.search_budget.max_candidates,
                            )
                        }) {
                        Ok(blends) => blends.into_iter().map(proposed_blend).collect(),
                        Err(reason) => {
                            state.advisor_failure = Some(reason);
                            Vec::new()
                        }
                    }
                }
                Err(reason) => {
                    state.advisor_failure = Some(reason);
                    Vec::new()
                }
            }
        };
        state.advisor = Some(advice);
        checkpoint(&state_path, &mut state)?;
    }

    let mut engine = PredictionMctsEngine::new_with_identity_and_component_profile(
        &mission,
        identity,
        baseline_blend(component_profile),
        state.advisor.clone().unwrap_or_default(),
        MCTS_SEED,
        MCTS_EXPLORATION,
        MCTS_MAX_DEPTH,
        component_profile,
    )?;
    if let Some(saved) = state.checkpoint.clone() {
        engine.restore_checkpoint(saved)?;
    } else {
        state.checkpoint = Some(engine.checkpoint()?);
        checkpoint(&state_path, &mut state)?;
    }

    while state.training.len() < mission.search_budget.max_candidates {
        if remaining_time(&state).is_zero() {
            state.budget_exhausted = true;
            checkpoint(&state_path, &mut state)?;
            return Ok(summary(
                &mission,
                &state_path,
                &state,
                LoopRunStatus::BudgetExhausted,
            ));
        }
        let candidate = if let Some(pending) = state.pending.clone() {
            pending
        } else {
            let candidate = engine.propose()?;
            state.pending = Some(candidate.clone());
            state.checkpoint = Some(engine.checkpoint()?);
            checkpoint(&state_path, &mut state)?;
            candidate
        };
        let artifact_dir = next_attempt_dir(
            &output_dir
                .join("training")
                .join(format!("candidate-{:03}", state.training.len() + 1)),
        )?;
        let evaluation = match evaluator.evaluate_training(
            &mission,
            snapshot_dir,
            &artifact_dir,
            &candidate,
            remaining_time(&state),
        ) {
            Ok(evaluation) => evaluation,
            Err(reason) => return pause(&mission, &state_path, &mut state, reason),
        };
        engine.observe(&candidate.candidate_id, &evaluation)?;
        state.training.push(TrainingRecord {
            candidate,
            evaluation,
        });
        state.pending = None;
        state.checkpoint = Some(engine.checkpoint()?);
        checkpoint(&state_path, &mut state)?;
    }

    if state.selected.is_none() {
        state.selected = state
            .training
            .iter()
            .min_by(|left, right| {
                training_loss(&left.evaluation).total_cmp(&training_loss(&right.evaluation))
            })
            .map(|record| record.candidate.clone());
        checkpoint(&state_path, &mut state)?;
    }
    if !state.held_out_complete {
        let selected = state
            .selected
            .clone()
            .ok_or_else(|| "prediction MCTS exhausted without a selected candidate".to_string())?;
        let artifact_dir = next_attempt_dir(&output_dir.join("selected-held-out"))?;
        if let Err(reason) = evaluator.evaluate_selected(
            &mission,
            snapshot_dir,
            &artifact_dir,
            &selected,
            remaining_time(&state),
        ) {
            return pause(&mission, &state_path, &mut state, reason);
        }
        state.held_out_complete = true;
        checkpoint(&state_path, &mut state)?;
    }

    Ok(summary(
        &mission,
        &state_path,
        &state,
        LoopRunStatus::BudgetExhausted,
    ))
}

fn task_output_dir(
    output_dir: &Path,
    identity: &PredictionMctsIdentity,
) -> Result<PathBuf, String> {
    let Some(mission_sha256) = identity.sealed_mission_sha256() else {
        return Ok(output_dir.to_path_buf());
    };
    let mission_sha256 = mission_sha256
        .strip_prefix("sha256:")
        .ok_or_else(|| "sealed prediction MCTS mission identity is invalid".to_string())?;
    let task = match identity.task {
        crate::prediction_mcts::PredictionMctsTask::SettlementProbability => "settlement",
        crate::prediction_mcts::PredictionMctsTask::UpExecution {
            prediction_horizon_secs,
        } => {
            return Ok(output_dir
                .join("mcts-v4")
                .join(mission_sha256)
                .join(format!("up-execution-{prediction_horizon_secs}s")))
        }
        crate::prediction_mcts::PredictionMctsTask::DownExecution {
            prediction_horizon_secs,
        } => {
            return Ok(output_dir
                .join("mcts-v4")
                .join(mission_sha256)
                .join(format!("down-execution-{prediction_horizon_secs}s")))
        }
    };
    Ok(output_dir.join("mcts-v4").join(mission_sha256).join(task))
}

fn baseline_blend(
    component_profile: SettlementProbabilityComponentProfile,
) -> ProposedProbabilityBlend {
    match component_profile {
        SettlementProbabilityComponentProfile::FullSurface => ProposedProbabilityBlend {
            name: "shared_mcts_baseline".to_string(),
            hypothesis: "Equal-weight registered probability components".to_string(),
            market_midpoint_weight: 1.0,
            chainlink_digital_weight: 1.0,
            distance_lob_vol_weight: 1.0,
            event_surface_weight: 1.0,
            existing_model_weight: 1.0,
        },
        SettlementProbabilityComponentProfile::MarketMidpointOnly => ProposedProbabilityBlend {
            name: "market_midpoint_baseline".to_string(),
            hypothesis: "Verified Polymarket midpoint is the only eligible component".to_string(),
            market_midpoint_weight: 1.0,
            chainlink_digital_weight: 0.0,
            distance_lob_vol_weight: 0.0,
            event_surface_weight: 0.0,
            existing_model_weight: 0.0,
        },
    }
}

fn proposed_blend(blend: crate::LlmProbabilityBlendSpec) -> ProposedProbabilityBlend {
    ProposedProbabilityBlend {
        name: blend.name,
        hypothesis: blend.hypothesis,
        market_midpoint_weight: blend.market_midpoint_weight,
        chainlink_digital_weight: blend.chainlink_digital_weight,
        distance_lob_vol_weight: blend.distance_lob_vol_weight,
        event_surface_weight: blend.event_surface_weight,
        existing_model_weight: blend.existing_model_weight,
    }
}

fn training_loss(evaluation: &PredictionMctsEvaluation) -> f64 {
    -evaluation
        .training
        .reward()
        .expect("observed prediction MCTS training evidence is valid")
}

fn pause(
    mission: &PredictionResearchMission,
    state_path: &Path,
    state: &mut PredictionMctsRunState,
    reason: String,
) -> Result<LoopRunSummary, String> {
    state.pause_reason = Some(reason);
    checkpoint(state_path, state)?;
    Ok(summary(mission, state_path, state, LoopRunStatus::Paused))
}

fn summary(
    mission: &PredictionResearchMission,
    state_path: &Path,
    state: &PredictionMctsRunState,
    status: LoopRunStatus,
) -> LoopRunSummary {
    LoopRunSummary {
        mission_id: mission.mission_id.clone(),
        status,
        llm_calls_used: usize::from(state.advisor_call_consumed),
        candidates_evaluated: state.training.len(),
        iterations_completed: state.training.len(),
        keep_models: Vec::new(),
        reason: state.pause_reason.clone(),
        state_path: state_path.to_path_buf(),
    }
}

fn checkpoint(path: &Path, state: &mut PredictionMctsRunState) -> Result<(), String> {
    let artifact = state.read_only_artifact()?;
    let output_dir = path.parent().ok_or_else(|| {
        format!(
            "prediction MCTS state path has no parent: {}",
            path.display()
        )
    })?;
    state.checkpoint_artifact = Some(write_content_addressed_json(
        output_dir,
        &output_dir.join("prediction-mcts-artifacts"),
        "prediction-mcts-artifact",
        &artifact,
    )?);
    atomic_write_json(path, state)?;
    Ok(())
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn remaining_time(state: &PredictionMctsRunState) -> Duration {
    Duration::from_millis(state.deadline_unix_millis.saturating_sub(now_unix_millis()))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::prediction_loop::{
        current_prediction_policy_snapshot_id, research_brief_snapshot_id, LoopRunStatus,
        PredictionSearchBudget, ProposalCallOutput, PREDICTION_LOOP_TARGET,
        PREDICTION_MISSION_SCHEMA_VERSION,
    };
    use crate::prediction_loop_fs::sha256_hex;
    use crate::prediction_mcts::PredictionMctsIdentity;
    use crate::prediction_mission_v3::{
        AdmittedPredictionMissionV3, AdmittedPredictionTask, PredictionAuthorityProfile,
        PredictionProductIdentity, PredictionProductSymbol, PredictionRunMode,
    };

    fn mission(max_candidates: usize, max_llm_calls: usize) -> PredictionResearchMission {
        let mut mission = PredictionResearchMission {
            schema_version: PREDICTION_MISSION_SCHEMA_VERSION.to_string(),
            mission_id: "btc-5m-runner-test".to_string(),
            lane: "prediction_market".to_string(),
            objective: "Test the official shared-MCTS runner".to_string(),
            hypothesis_scope: "Mutate only typed probability blend weights".to_string(),
            mutable_scope: vec!["probability_blend_weights".to_string()],
            data_snapshot_id: format!("sha256:{}", "1".repeat(64)),
            target: PREDICTION_LOOP_TARGET.to_string(),
            symbols: vec!["BTC".to_string()],
            horizon: "5m".to_string(),
            time_cohort_boundary_ms: 300_000,
            prompt_snapshot_id: String::new(),
            search_policy_snapshot_id: current_prediction_policy_snapshot_id(),
            search_budget: PredictionSearchBudget {
                max_candidates,
                max_llm_calls,
                max_seconds: 60,
            },
        };
        mission.prompt_snapshot_id = research_brief_snapshot_id(&mission);
        mission
    }

    struct FakeClient {
        calls: usize,
    }

    impl ProposalClient for FakeClient {
        fn propose(
            &mut self,
            _prompt: &str,
            _timeout: Duration,
        ) -> Result<ProposalCallOutput, String> {
            self.calls += 1;
            Ok(ProposalCallOutput {
                raw_response: serde_json::json!({
                    "probability_blends": [{
                        "name": "advisor",
                        "hypothesis": "bounded advisor",
                        "market_midpoint_weight": 1.0,
                        "chainlink_digital_weight": 1.0,
                        "distance_lob_vol_weight": 1.0,
                        "event_surface_weight": 1.0,
                        "existing_model_weight": 1.0
                    }]
                })
                .to_string(),
                provider: "test".to_string(),
                model: "test".to_string(),
                usage: serde_json::json!({}),
            })
        }
    }

    #[derive(Default)]
    struct FakeEvaluator {
        calls: Vec<String>,
        fail_training: VecDeque<bool>,
        candidate_ids: Vec<String>,
    }

    impl PredictionMctsRunEvaluator for FakeEvaluator {
        fn evaluate_baseline(
            &mut self,
            _mission: &PredictionResearchMission,
            _snapshot_dir: &Path,
            _artifact_dir: &Path,
            _timeout: Duration,
        ) -> Result<(), String> {
            self.calls.push("baseline".to_string());
            Ok(())
        }

        fn evaluate_training(
            &mut self,
            _mission: &PredictionResearchMission,
            _snapshot_dir: &Path,
            _artifact_dir: &Path,
            candidate: &PredictionMctsCandidate,
            _timeout: Duration,
        ) -> Result<PredictionMctsEvaluation, String> {
            self.calls.push("training".to_string());
            self.candidate_ids.push(candidate.candidate_id.clone());
            if self.fail_training.pop_front().unwrap_or(false) {
                return Err("retryable training failure".to_string());
            }
            Ok(PredictionMctsEvaluation {
                training: PredictionMctsTrainingEvidence::SettlementProbability(
                    crate::prediction_mcts::SettlementTrainingEvidence {
                        candidate_id: candidate.candidate_id.clone(),
                        identity: candidate.identity.clone(),
                        probability_blend_sha256: candidate.probability_blend_sha256.clone(),
                        training_cohort_id: "train-before-boundary".to_string(),
                        event_count: 12,
                        mean_brier_score: 0.2,
                        mean_log_loss: 0.3,
                    },
                ),
            })
        }

        fn evaluate_selected(
            &mut self,
            _mission: &PredictionResearchMission,
            _snapshot_dir: &Path,
            _artifact_dir: &Path,
            candidate: &PredictionMctsCandidate,
            _timeout: Duration,
        ) -> Result<(), String> {
            self.calls.push("held_out".to_string());
            self.candidate_ids.push(candidate.candidate_id.clone());
            Ok(())
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ploy-prediction-mcts-run-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    fn published_artifact_path(output: &Path) -> PathBuf {
        let state: serde_json::Value = serde_json::from_slice(
            &std::fs::read(output.join("prediction-mcts-state.json")).unwrap(),
        )
        .unwrap();
        output.join(
            state["checkpoint_artifact"]["path"]
                .as_str()
                .expect("content-addressed checkpoint artifact path"),
        )
    }

    #[test]
    fn baseline_only_runs_once_without_llm_or_held_out() {
        let output = temp_dir("baseline");
        let mut client = FakeClient { calls: 0 };
        let mut evaluator = FakeEvaluator::default();

        let summary = run_or_resume_prediction_mcts(
            mission(0, 0),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("baseline-only run");

        assert_eq!(summary.status, LoopRunStatus::BudgetExhausted);
        assert_eq!(client.calls, 0);
        assert_eq!(evaluator.calls, ["baseline"]);
    }

    #[test]
    fn resume_reuses_the_exact_pending_candidate() {
        let output = temp_dir("resume");
        let mut client = FakeClient { calls: 0 };
        let mut first = FakeEvaluator {
            fail_training: VecDeque::from([true]),
            ..Default::default()
        };
        let first_summary = run_or_resume_prediction_mcts(
            mission(2, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut first,
        )
        .expect("retryable pause");
        assert_eq!(first_summary.status, LoopRunStatus::Paused);
        let pending = first.candidate_ids[0].clone();

        let mut resumed = FakeEvaluator::default();
        let summary = run_or_resume_prediction_mcts(
            mission(2, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut resumed,
        )
        .expect("resume pending candidate");

        assert_eq!(summary.status, LoopRunStatus::BudgetExhausted);
        assert_eq!(resumed.candidate_ids[0], pending);
        assert_eq!(client.calls, 1, "resume must not repeat advisor call");
    }

    #[test]
    fn held_out_runs_once_after_all_training_candidates() {
        let output = temp_dir("held-out");
        let mut client = FakeClient { calls: 0 };
        let mut evaluator = FakeEvaluator::default();

        run_or_resume_prediction_mcts(
            mission(2, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("search run");

        assert_eq!(
            evaluator.calls,
            ["baseline", "training", "training", "held_out"]
        );
        assert_eq!(evaluator.candidate_ids.len(), 3);
        assert_eq!(evaluator.candidate_ids[2], evaluator.candidate_ids[0]);
        assert_eq!(
            evaluator.candidate_ids[0].split(':').next().unwrap(),
            PredictionMctsIdentity::from_mission(&mission(2, 1))
                .unwrap()
                .mission_id
        );
    }

    #[test]
    fn runner_writes_a_stable_read_only_checkpoint_artifact() {
        let output = temp_dir("checkpoint-artifact");
        let mut client = FakeClient { calls: 0 };
        let mut evaluator = FakeEvaluator::default();

        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("search run");

        let artifact_path = published_artifact_path(&output);
        let first = std::fs::read(&artifact_path).expect("checkpoint artifact");
        let artifact: serde_json::Value =
            serde_json::from_slice(&first).expect("parse checkpoint artifact");
        assert_eq!(artifact["training"].as_array().unwrap().len(), 1);
        assert_eq!(
            artifact["prompt_snapshot_id"],
            mission(1, 1).prompt_snapshot_id
        );
        assert_eq!(
            artifact["search_policy_snapshot_id"],
            mission(1, 1).search_policy_snapshot_id
        );
        assert!(artifact["training"][0].get("held_out_settlement").is_none());
        assert!(artifact["training"][0].get("execution").is_none());
        assert!(artifact_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .contains(&sha256_hex(&first)));

        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("resume completed run");
        assert_eq!(std::fs::read(&artifact_path).unwrap(), first);
        assert_eq!(published_artifact_path(&output), artifact_path);
    }

    #[test]
    fn runner_omits_auxiliary_evidence_without_rejecting_the_training_result() {
        let output = temp_dir("auxiliary-evidence");
        let mut client = FakeClient { calls: 0 };
        let mut evaluator = FakeEvaluator {
            ..Default::default()
        };

        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("auxiliary evidence must not block training checkpoint publication");

        let artifact: serde_json::Value =
            serde_json::from_slice(&std::fs::read(published_artifact_path(&output)).unwrap())
                .unwrap();
        assert!(artifact["training"][0].get("held_out_settlement").is_none());
        assert!(artifact["training"][0].get("execution").is_none());
    }

    #[test]
    fn runner_rejects_forged_pending_candidate_before_evaluation() {
        let output = temp_dir("forged-pending-artifact");
        let mut client = FakeClient { calls: 0 };
        let mut paused = FakeEvaluator {
            fail_training: VecDeque::from([true]),
            ..Default::default()
        };
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut paused,
        )
        .expect("pause with a durable pending candidate");

        let state_path = output.join("prediction-mcts-state.json");
        let mut forged: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        forged["pending"]["candidate_id"] = serde_json::json!("forged-pending-candidate");
        std::fs::write(&state_path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();

        let mut resumed = FakeEvaluator::default();
        let error = run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut resumed,
        )
        .expect_err("forged pending candidate must be rejected before evaluation");
        assert!(error.contains("pending candidate"));
        assert!(resumed.calls.is_empty());
    }

    #[test]
    fn runner_publishes_deadline_exhaustion_as_a_terminal_phase() {
        let output = temp_dir("deadline-exhausted-artifact");
        let mut client = FakeClient { calls: 0 };
        let mut paused = FakeEvaluator {
            fail_training: VecDeque::from([true]),
            ..Default::default()
        };
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut paused,
        )
        .expect("pause with incomplete training");

        let state_path = output.join("prediction-mcts-state.json");
        let mut expired: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        expired["deadline_unix_millis"] = serde_json::json!(0);
        std::fs::write(&state_path, serde_json::to_vec_pretty(&expired).unwrap()).unwrap();

        let mut resumed = FakeEvaluator::default();
        let summary = run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut resumed,
        )
        .expect("expired run returns a terminal summary");
        assert_eq!(summary.status, LoopRunStatus::BudgetExhausted);
        assert!(resumed.calls.is_empty());

        let artifact: serde_json::Value =
            serde_json::from_slice(&std::fs::read(published_artifact_path(&output)).unwrap())
                .unwrap();
        assert_eq!(artifact["phase"], "budget_exhausted");
    }

    #[test]
    fn read_only_projection_does_not_change_when_an_unobserved_deadline_passes() {
        let output = temp_dir("stable-before-deadline");
        let mut client = FakeClient { calls: 0 };
        let mut paused = FakeEvaluator {
            fail_training: VecDeque::from([true]),
            ..Default::default()
        };
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut paused,
        )
        .expect("pause with a durable checkpoint");

        let mut state: PredictionMctsRunState =
            read_json(&output.join("prediction-mcts-state.json")).unwrap();
        state.pause_reason = None;
        state.deadline_unix_millis = now_unix_millis() + 20;
        let before = serde_json::to_vec(&state.read_only_artifact().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        let after = serde_json::to_vec(&state.read_only_artifact().unwrap()).unwrap();

        assert_eq!(after, before);
    }

    #[test]
    fn runner_rejects_v2_state_after_typed_training_evidence_upgrade() {
        let output = temp_dir("legacy-component-profile");
        let mut client = FakeClient { calls: 0 };
        let mut paused = FakeEvaluator {
            fail_training: VecDeque::from([true]),
            ..Default::default()
        };
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut paused,
        )
        .expect("pause with a durable v2 state");

        let state_path = output.join("prediction-mcts-state.json");
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        legacy["version"] = serde_json::json!(2);
        std::fs::write(&state_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let error = run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut FakeEvaluator::default(),
        )
        .expect_err("v2 state must not resume under typed training evidence");
        assert!(error.contains("incompatible state version"));
    }

    #[test]
    fn runner_rejects_a_v3_state_missing_its_typed_identity() {
        let output = temp_dir("missing-typed-identity");
        let mut client = FakeClient { calls: 0 };
        let mut paused = FakeEvaluator {
            fail_training: VecDeque::from([true]),
            ..Default::default()
        };
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut paused,
        )
        .expect("pause with a durable v3 state");

        let state_path = output.join("prediction-mcts-state.json");
        let mut forged: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        forged.as_object_mut().unwrap().remove("identity");
        std::fs::write(&state_path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();

        let error = run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut FakeEvaluator::default(),
        )
        .expect_err("typed state identity is mandatory once version 3 is written");
        assert!(error.contains("missing its typed identity"));
    }

    #[test]
    fn runner_rejects_a_selected_candidate_missing_from_training() {
        let output = temp_dir("forged-selected-artifact");
        let mut client = FakeClient { calls: 0 };
        let mut evaluator = FakeEvaluator::default();
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("completed run");

        let state_path = output.join("prediction-mcts-state.json");
        let mut forged: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        forged["selected"]["candidate_id"] = serde_json::json!("forged-selected-candidate");
        std::fs::write(&state_path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();

        let error = run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect_err("selected candidate must be backed by a training record");
        assert!(error.contains("selected candidate"));
    }

    #[test]
    fn runner_rejects_a_checkpoint_with_a_mismatched_identity_before_rewriting_it() {
        let output = temp_dir("forged-checkpoint-artifact");
        let mut client = FakeClient { calls: 0 };
        let mut evaluator = FakeEvaluator::default();
        run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect("search run");

        let state_path = output.join("prediction-mcts-state.json");
        let artifact_path = published_artifact_path(&output);
        let artifact_before_forgery = std::fs::read(&artifact_path).unwrap();
        let mut forged: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        forged["checkpoint"]["config"]["identity"]["symbol"] = serde_json::json!("SOL");
        let forged = serde_json::to_vec_pretty(&forged).unwrap();
        std::fs::write(&state_path, &forged).unwrap();

        let error = run_or_resume_prediction_mcts(
            mission(1, 1),
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut evaluator,
        )
        .expect_err("forged checkpoint must be rejected");
        assert!(error.contains("checkpoint identity does not match the mission"));
        assert_eq!(std::fs::read(&state_path).unwrap(), forged);
        assert_eq!(
            std::fs::read(&artifact_path).unwrap(),
            artifact_before_forgery
        );
    }

    #[test]
    fn sealed_v4_task_uses_an_isolated_checkpoint_namespace() {
        let output = temp_dir("sealed-v4-namespace");
        let mut bridge = mission(1, 1);
        bridge.mission_id = "btc-5m-up".to_string();
        bridge.data_snapshot_id = format!("sha256:{}", "5".repeat(64));
        bridge.prompt_snapshot_id = research_brief_snapshot_id(&bridge);
        let admitted = AdmittedPredictionMissionV3 {
            mission_id: bridge.mission_id.clone(),
            mission_sha256: format!("sha256:{}", "1".repeat(64)),
            product: PredictionProductIdentity {
                symbol: PredictionProductSymbol::Btc,
                event_horizon_secs: 300,
            },
            task: AdmittedPredictionTask::UpExecution {
                prediction_horizon_secs: 15,
            },
            run_mode: PredictionRunMode::ResearchTrial,
            authority_profile: PredictionAuthorityProfile::PolymarketChainlinkBaseline,
            cohort_manifest_id: format!("sha256:{}", "2".repeat(64)),
            partition_digest: format!("sha256:{}", "3".repeat(64)),
            causal_projection_policy_id: current_prediction_policy_snapshot_id(),
            snapshot_contract_id: bridge.data_snapshot_id.clone(),
            snapshot_hash: "6".repeat(16),
            search_policy_snapshot_id: current_prediction_policy_snapshot_id(),
        };
        let identity = PredictionMctsIdentity::from_admitted_mission(&admitted).unwrap();
        let mut client = FakeClient { calls: 0 };
        let error = run_or_resume_prediction_mcts_with_identity_and_component_profile(
            bridge,
            identity,
            Path::new("unused-snapshot"),
            &output,
            &mut client,
            &mut FakeEvaluator::default(),
            SettlementProbabilityComponentProfile::MarketMidpointOnly,
        )
        .expect_err("settlement-only evaluator cannot satisfy an Up execution task");
        assert!(error.contains("training evidence identity"));

        let state_path = output
            .join("mcts-v4")
            .join("1111111111111111111111111111111111111111111111111111111111111111")
            .join("up-execution-15s")
            .join("prediction-mcts-state.json");
        assert!(state_path.exists());
        assert!(!output.join("prediction-mcts-state.json").exists());
    }
}
