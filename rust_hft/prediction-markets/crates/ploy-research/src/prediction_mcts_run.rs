//! Durable official runner for prediction-market MCTS research.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::prediction_loop::{
    build_prediction_prompt, current_prediction_policy_snapshot_id, validate_prediction_mission,
    validate_prediction_proposal, LoopRunStatus, LoopRunSummary, PredictionProposal,
    PredictionResearchMission, ProposalClient, ProposedProbabilityBlend,
};
use crate::prediction_loop_fs::{atomic_write_json, next_attempt_dir, read_json, OutputLock};
use crate::prediction_mcts::{
    PredictionMctsCandidate, PredictionMctsCheckpoint, PredictionMctsEngine,
    PredictionMctsEvaluation,
};

const RUN_STATE_VERSION: u32 = 1;
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
    deadline_unix_millis: u64,
    baseline_complete: bool,
    advisor_call_consumed: bool,
    advisor: Option<Vec<ProposedProbabilityBlend>>,
    advisor_failure: Option<String>,
    checkpoint: Option<PredictionMctsCheckpoint>,
    pending: Option<PredictionMctsCandidate>,
    training: Vec<TrainingRecord>,
    selected: Option<PredictionMctsCandidate>,
    held_out_complete: bool,
    pause_reason: Option<String>,
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
    validate_prediction_mission(&mission, &current_prediction_policy_snapshot_id())?;
    let _lock = OutputLock::acquire(output_dir)?;
    let state_path = output_dir.join("prediction-mcts-state.json");
    let mut state = if state_path.exists() {
        let state: PredictionMctsRunState = read_json(&state_path)?;
        if state.version != RUN_STATE_VERSION || state.mission != mission {
            return Err("prediction MCTS output belongs to a different mission".to_string());
        }
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
            deadline_unix_millis,
            baseline_complete: false,
            advisor_call_consumed: false,
            advisor: None,
            advisor_failure: None,
            checkpoint: None,
            pending: None,
            training: Vec::new(),
            selected: None,
            held_out_complete: false,
            pause_reason: None,
        }
    };
    state.pause_reason = None;
    checkpoint(&state_path, &state)?;

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
        checkpoint(&state_path, &state)?;
    }

    if mission.search_budget.max_candidates == 0 {
        return Ok(summary(
            &mission,
            &state_path,
            &state,
            LoopRunStatus::BudgetExhausted,
        ));
    }

    if state.advisor.is_none() {
        let advice = if mission.search_budget.max_llm_calls == 0 {
            Vec::new()
        } else if state.advisor_call_consumed {
            return Err(
                "prediction MCTS advisor call was interrupted before durable response".to_string(),
            );
        } else {
            state.advisor_call_consumed = true;
            checkpoint(&state_path, &state)?;
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
        checkpoint(&state_path, &state)?;
    }

    let mut engine = PredictionMctsEngine::new(
        &mission,
        baseline_blend(),
        state.advisor.clone().unwrap_or_default(),
        MCTS_SEED,
        MCTS_EXPLORATION,
        MCTS_MAX_DEPTH,
    )?;
    if let Some(saved) = state.checkpoint.clone() {
        engine.restore_checkpoint(saved)?;
    } else {
        state.checkpoint = Some(engine.checkpoint()?);
        checkpoint(&state_path, &state)?;
    }

    while state.training.len() < mission.search_budget.max_candidates {
        if remaining_time(&state).is_zero() {
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
            checkpoint(&state_path, &state)?;
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
        checkpoint(&state_path, &state)?;
    }

    if state.selected.is_none() {
        state.selected = state
            .training
            .iter()
            .min_by(|left, right| {
                training_loss(&left.evaluation).total_cmp(&training_loss(&right.evaluation))
            })
            .map(|record| record.candidate.clone());
        checkpoint(&state_path, &state)?;
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
        checkpoint(&state_path, &state)?;
    }

    Ok(summary(
        &mission,
        &state_path,
        &state,
        LoopRunStatus::BudgetExhausted,
    ))
}

fn baseline_blend() -> ProposedProbabilityBlend {
    ProposedProbabilityBlend {
        name: "shared_mcts_baseline".to_string(),
        hypothesis: "Equal-weight registered probability components".to_string(),
        market_midpoint_weight: 1.0,
        chainlink_digital_weight: 1.0,
        distance_lob_vol_weight: 1.0,
        event_surface_weight: 1.0,
        existing_model_weight: 1.0,
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
    evaluation.training_settlement.mean_brier_score + evaluation.training_settlement.mean_log_loss
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

fn checkpoint(path: &Path, state: &PredictionMctsRunState) -> Result<(), String> {
    atomic_write_json(path, state)
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
    use crate::prediction_mcts::{PredictionMctsIdentity, SettlementTrainingEvidence};

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
                training_settlement: SettlementTrainingEvidence {
                    candidate_id: candidate.candidate_id.clone(),
                    identity: candidate.identity.clone(),
                    probability_blend_sha256: candidate.probability_blend_sha256.clone(),
                    training_cohort_id: "train-before-boundary".to_string(),
                    event_count: 12,
                    mean_brier_score: 0.2,
                    mean_log_loss: 0.3,
                },
                held_out_settlement: None,
                execution: None,
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
}
