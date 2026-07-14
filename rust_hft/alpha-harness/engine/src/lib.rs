//! Persistent, budgeted AutoResearch mission kernel.

pub mod engines;
pub mod evaluation;
pub mod formula_evaluator;
#[cfg(feature = "kernel")]
pub mod learning;
pub mod llm;

use alpha_domain::{CandidateArtifact, EngineKind};
pub use alpha_domain::{
    CandidateEvaluation, EvaluationMetrics, FoldEvaluationMetrics, FoldPredictiveMetrics,
    PredictiveMetrics,
};
#[cfg(feature = "kernel")]
use alpha_domain::{
    IterationVerdict, MissionStatus, MissionTerminalReason, ResearchIteration, SearchBudgetLimit,
};
#[cfg(feature = "kernel")]
use alpha_store::{AlphaStore, EvaluationRecord, MissionLineage, RunCheckpoint, StoreError};
#[cfg(feature = "kernel")]
use chrono::Utc;
#[cfg(feature = "kernel")]
use evaluation::PreparedDataset;
use evaluation::{EngineContext, ProposalContext};
use serde::{Deserialize, Serialize};
#[cfg(feature = "kernel")]
use thiserror::Error;

#[derive(Debug, Error)]
#[cfg(feature = "kernel")]
pub enum EngineError {
    #[error("control-plane store failed: {0}")]
    Store(#[from] StoreError),
    #[error("mission is already terminal")]
    TerminalMission,
    #[error("proposal engine failed: {0}")]
    Proposal(String),
    #[error("candidate evaluator failed: {0}")]
    Evaluation(String),
    #[error("engine checkpoint failed: {0}")]
    Checkpoint(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineProposal {
    pub candidate_id: String,
    pub hypothesis: String,
    pub artifact: CandidateArtifact,
    pub expansions: u64,
    pub tokens: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoricalObservation {
    pub proposal: EngineProposal,
    pub evaluation: CandidateEvaluation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalEngineCheckpoint {
    pub kind: EngineKind,
    pub version: u32,
    pub state: serde_json::Value,
}

pub trait ProposalEngine {
    fn kind(&self) -> EngineKind;
    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        context: &ProposalContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String>;

    fn observe(&mut self, _proposal: &EngineProposal, _evaluation: &CandidateEvaluation) {}

    fn abandon(&mut self, _proposal: &EngineProposal) {}

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        for observation in observations {
            self.observe(&observation.proposal, &observation.evaluation);
        }
        Ok(())
    }

    fn checkpoint(&self) -> Result<ProposalEngineCheckpoint, String> {
        Ok(ProposalEngineCheckpoint {
            kind: self.kind(),
            version: 1,
            state: serde_json::json!({"mode": "history_replay"}),
        })
    }

    fn restore_checkpoint(
        &mut self,
        checkpoint: &ProposalEngineCheckpoint,
        observations: &[HistoricalObservation],
    ) -> Result<(), String> {
        if checkpoint.kind != self.kind()
            || checkpoint.version != 1
            || checkpoint
                .state
                .get("mode")
                .and_then(serde_json::Value::as_str)
                != Some("history_replay")
        {
            return Err("proposal engine checkpoint kind, version, or mode mismatch".to_string());
        }
        self.restore(observations)
    }
}

impl<T> ProposalEngine for Box<T>
where
    T: ProposalEngine + ?Sized,
{
    fn kind(&self) -> EngineKind {
        (**self).kind()
    }

    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        context: &ProposalContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String> {
        (**self).propose(mission_id, iteration_index, context, remaining)
    }

    fn observe(&mut self, proposal: &EngineProposal, evaluation: &CandidateEvaluation) {
        (**self).observe(proposal, evaluation);
    }

    fn abandon(&mut self, proposal: &EngineProposal) {
        (**self).abandon(proposal);
    }

    fn restore(&mut self, observations: &[HistoricalObservation]) -> Result<(), String> {
        (**self).restore(observations)
    }

    fn checkpoint(&self) -> Result<ProposalEngineCheckpoint, String> {
        (**self).checkpoint()
    }

    fn restore_checkpoint(
        &mut self,
        checkpoint: &ProposalEngineCheckpoint,
        observations: &[HistoricalObservation],
    ) -> Result<(), String> {
        (**self).restore_checkpoint(checkpoint, observations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingBudget {
    pub candidates: usize,
    pub expansions: u64,
    pub tokens: u64,
    pub milliseconds: u64,
}

pub trait CandidateEvaluator {
    fn evaluate(
        &self,
        proposal: &EngineProposal,
        context: &EngineContext<'_>,
    ) -> Result<CandidateEvaluation, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg(feature = "kernel")]
pub struct RunControl {
    pub max_new_iterations: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(feature = "kernel")]
pub struct RunOutcome {
    pub status: MissionStatus,
    pub terminal_reason: Option<MissionTerminalReason>,
    pub total_iterations: usize,
    pub new_iterations: usize,
}

#[cfg(feature = "kernel")]
pub struct AutoResearchKernel<'a, P, E> {
    store: &'a mut AlphaStore,
    proposal_engine: P,
    evaluator: E,
}

#[cfg(feature = "kernel")]
impl<'a, P, E> AutoResearchKernel<'a, P, E>
where
    P: ProposalEngine,
    E: CandidateEvaluator,
{
    pub fn new(store: &'a mut AlphaStore, proposal_engine: P, evaluator: E) -> Self {
        Self {
            store,
            proposal_engine,
            evaluator,
        }
    }

    pub fn run(
        &mut self,
        mission_id: &str,
        dataset: &PreparedDataset,
        control: RunControl,
    ) -> Result<RunOutcome, EngineError> {
        let now = Utc::now();
        let mission = self.store.get_mission(mission_id)?;
        match mission.status {
            MissionStatus::Pending | MissionStatus::Paused | MissionStatus::Running => {}
            MissionStatus::Completed | MissionStatus::BudgetExhausted | MissionStatus::Failed => {
                return Err(EngineError::TerminalMission)
            }
        }

        let lineage = self.store.mission_lineage(mission_id)?;
        let checkpoint = match self.store.get_checkpoint(mission_id) {
            Ok(checkpoint) => Some(checkpoint),
            Err(StoreError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        let expected_last_iteration = lineage
            .iterations
            .last()
            .map(|iteration| iteration.iteration_id.as_str());
        if checkpoint.as_ref().is_some_and(|checkpoint| {
            checkpoint.last_iteration_id.as_deref() != expected_last_iteration
        }) || (checkpoint.is_none() && expected_last_iteration.is_some())
        {
            return Err(EngineError::Checkpoint(
                "checkpoint lineage does not match the persisted iterations".to_string(),
            ));
        }
        let mut usage = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.budget_usage.clone())
            .unwrap_or_default();
        if lineage
            .iterations
            .last()
            .is_some_and(|iteration| iteration.budget_usage != usage)
        {
            return Err(EngineError::Checkpoint(
                "checkpoint budget does not match the last iteration".to_string(),
            ));
        }
        let mut total_iterations = lineage.iterations.len();
        let mut kept = lineage
            .iterations
            .iter()
            .filter(|iteration| iteration.verdict == IterationVerdict::Keep)
            .count();
        let historical = historical_observations(&lineage, &self.proposal_engine.kind())?;
        if let Some(checkpoint) = &checkpoint {
            self.proposal_engine
                .restore_checkpoint(
                    &ProposalEngineCheckpoint {
                        kind: checkpoint.engine_kind.clone(),
                        version: checkpoint.engine_version,
                        state: checkpoint.engine_state.clone(),
                    },
                    &historical,
                )
                .map_err(EngineError::Checkpoint)?;
        } else {
            self.proposal_engine
                .restore(&historical)
                .map_err(EngineError::Proposal)?;
        }
        if matches!(
            mission.status,
            MissionStatus::Pending | MissionStatus::Paused
        ) {
            self.store
                .transition_mission(mission_id, MissionStatus::Running, now)?;
        }
        let mut seen_artifacts = lineage
            .candidates
            .iter()
            .map(|candidate| serde_json::to_string(&candidate.artifact))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(|error| EngineError::Evaluation(error.to_string()))?;
        let mut new_iterations = 0;
        let proposal_context = dataset.proposal_context();
        let evaluation_context = dataset.engine_context();

        if kept >= mission.completion_policy.min_kept_candidates {
            let reason = MissionTerminalReason::CompletionPolicySatisfied {
                kept_candidates: kept,
            };
            self.store
                .finish_mission(mission_id, reason.clone(), Utc::now())?;
            return Ok(RunOutcome {
                status: MissionStatus::Completed,
                terminal_reason: Some(reason),
                total_iterations,
                new_iterations,
            });
        }

        while !budget_exhausted(&mission.search_budget, &usage) {
            if control
                .max_new_iterations
                .is_some_and(|limit| new_iterations >= limit)
            {
                self.store
                    .transition_mission(mission_id, MissionStatus::Paused, Utc::now())?;
                return Ok(RunOutcome {
                    status: MissionStatus::Paused,
                    terminal_reason: None,
                    total_iterations,
                    new_iterations,
                });
            }

            let index = total_iterations + 1;
            let iteration_id = format!("{mission_id}-iteration-{index}");
            let created_at = Utc::now();
            let remaining = remaining_budget(&mission.search_budget, &usage);
            let started = std::time::Instant::now();
            let proposal =
                self.proposal_engine
                    .propose(mission_id, index, &proposal_context, &remaining);
            let (iteration, candidate, evaluation) = match proposal {
                Ok(mut proposal) => {
                    proposal.elapsed_ms = proposal
                        .elapsed_ms
                        .max(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
                    usage.candidates += 1;
                    usage.expansions = usage.expansions.saturating_add(proposal.expansions);
                    usage.tokens = usage.tokens.saturating_add(proposal.tokens);
                    usage.elapsed_ms = usage.elapsed_ms.saturating_add(proposal.elapsed_ms);
                    let within_budget = proposal.expansions <= remaining.expansions
                        && proposal.tokens <= remaining.tokens
                        && proposal.elapsed_ms <= remaining.milliseconds;
                    let fingerprint = serde_json::to_string(&proposal.artifact)
                        .map_err(|error| EngineError::Evaluation(error.to_string()))?;
                    let is_novel = seen_artifacts.insert(fingerprint);
                    let evaluated = if !is_novel {
                        Err("proposal duplicated an existing candidate artifact".to_string())
                    } else if within_budget {
                        self.evaluator
                            .evaluate(&proposal, &evaluation_context)
                            .and_then(|result| {
                                result.validate().map_err(|error| error.to_string())?;
                                Ok(result)
                            })
                    } else {
                        Err("proposal exceeded remaining search budget".to_string())
                    };
                    match evaluated {
                        Ok(result) => {
                            self.proposal_engine.observe(&proposal, &result);
                            let evaluation_id = format!("{mission_id}-evaluation-{index}");
                            let evaluation = EvaluationRecord {
                                evaluation_id: evaluation_id.clone(),
                                mission_id: mission_id.to_string(),
                                candidate_id: proposal.candidate_id.clone(),
                                payload: serde_json::to_value(&result)
                                    .map_err(|error| EngineError::Evaluation(error.to_string()))?,
                                created_at,
                            };
                            let verdict = if result.passed {
                                kept += 1;
                                IterationVerdict::Keep
                            } else {
                                IterationVerdict::Discard
                            };
                            (
                                ResearchIteration {
                                    iteration_id,
                                    mission_id: mission_id.to_string(),
                                    parent_candidate_ids: vec![],
                                    engine: self.proposal_engine.kind(),
                                    hypothesis: proposal.hypothesis.clone(),
                                    candidate_artifact_id: Some(proposal.candidate_id.clone()),
                                    evaluation_artifact_id: Some(evaluation_id),
                                    budget_usage: usage.clone(),
                                    verdict,
                                    failure_class: None,
                                    failure_explanation: (!result.passed)
                                        .then(|| result.failure_reasons.join("; ")),
                                    created_at,
                                },
                                Some((proposal.candidate_id, proposal.artifact)),
                                Some(evaluation),
                            )
                        }
                        Err(error) => {
                            self.proposal_engine.abandon(&proposal);
                            let candidate_id = is_novel.then(|| proposal.candidate_id.clone());
                            let candidate =
                                is_novel.then_some((proposal.candidate_id, proposal.artifact));
                            (
                                ResearchIteration {
                                    iteration_id,
                                    mission_id: mission_id.to_string(),
                                    parent_candidate_ids: vec![],
                                    engine: self.proposal_engine.kind(),
                                    hypothesis: proposal.hypothesis.clone(),
                                    candidate_artifact_id: candidate_id,
                                    evaluation_artifact_id: None,
                                    budget_usage: usage.clone(),
                                    verdict: IterationVerdict::Crash,
                                    failure_class: Some(if is_novel {
                                        "evaluation_error".to_string()
                                    } else {
                                        "duplicate_candidate".to_string()
                                    }),
                                    failure_explanation: Some(error),
                                    created_at,
                                },
                                candidate,
                                None,
                            )
                        }
                    }
                }
                Err(error) => {
                    usage.candidates += 1;
                    (
                        ResearchIteration {
                            iteration_id,
                            mission_id: mission_id.to_string(),
                            parent_candidate_ids: vec![],
                            engine: self.proposal_engine.kind(),
                            hypothesis: "proposal engine crashed".to_string(),
                            candidate_artifact_id: None,
                            evaluation_artifact_id: None,
                            budget_usage: usage.clone(),
                            verdict: IterationVerdict::Crash,
                            failure_class: Some("proposal_error".to_string()),
                            failure_explanation: Some(error),
                            created_at,
                        },
                        None,
                        None,
                    )
                }
            };

            let engine_checkpoint = self
                .proposal_engine
                .checkpoint()
                .map_err(EngineError::Checkpoint)?;
            if engine_checkpoint.kind != iteration.engine || engine_checkpoint.version == 0 {
                return Err(EngineError::Checkpoint(
                    "proposal engine returned an invalid checkpoint envelope".to_string(),
                ));
            }
            let checkpoint = RunCheckpoint {
                mission_id: mission_id.to_string(),
                last_iteration_id: Some(iteration.iteration_id.clone()),
                budget_usage: usage.clone(),
                engine_kind: engine_checkpoint.kind,
                engine_version: engine_checkpoint.version,
                engine_state: engine_checkpoint.state,
                updated_at: created_at,
            };
            self.store.append_iteration_with_checkpoint(
                &iteration,
                candidate
                    .as_ref()
                    .map(|(candidate_id, artifact)| (candidate_id.as_str(), artifact)),
                evaluation.as_ref(),
                &checkpoint,
            )?;
            total_iterations += 1;
            new_iterations += 1;
            if kept >= mission.completion_policy.min_kept_candidates {
                let reason = MissionTerminalReason::CompletionPolicySatisfied {
                    kept_candidates: kept,
                };
                self.store
                    .finish_mission(mission_id, reason.clone(), Utc::now())?;
                return Ok(RunOutcome {
                    status: MissionStatus::Completed,
                    terminal_reason: Some(reason),
                    total_iterations,
                    new_iterations,
                });
            }
        }

        let reason = MissionTerminalReason::SearchBudgetExhausted {
            exhausted_limits: exhausted_budget_limits(&mission.search_budget, &usage),
        };
        self.store
            .finish_mission(mission_id, reason.clone(), Utc::now())?;
        Ok(RunOutcome {
            status: MissionStatus::BudgetExhausted,
            terminal_reason: Some(reason),
            total_iterations,
            new_iterations,
        })
    }
}

#[cfg(feature = "kernel")]
fn historical_observations(
    lineage: &MissionLineage,
    engine_kind: &EngineKind,
) -> Result<Vec<HistoricalObservation>, EngineError> {
    let mut observations = Vec::new();
    for iteration in lineage
        .iterations
        .iter()
        .filter(|iteration| &iteration.engine == engine_kind)
    {
        let (Some(candidate_id), Some(evaluation_id)) = (
            iteration.candidate_artifact_id.as_deref(),
            iteration.evaluation_artifact_id.as_deref(),
        ) else {
            continue;
        };
        let candidate = lineage
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
            .ok_or_else(|| {
                EngineError::Evaluation("historical candidate is missing".to_string())
            })?;
        let evaluation = lineage
            .evaluations
            .iter()
            .find(|evaluation| evaluation.record.evaluation_id == evaluation_id)
            .ok_or_else(|| {
                EngineError::Evaluation("historical evaluation is missing".to_string())
            })?;
        let evaluation =
            historical_evaluation(&evaluation.record.payload).map_err(EngineError::Evaluation)?;
        observations.push(HistoricalObservation {
            proposal: EngineProposal {
                candidate_id: candidate_id.to_string(),
                hypothesis: iteration.hypothesis.clone(),
                artifact: candidate.artifact.clone(),
                expansions: 0,
                tokens: 0,
                elapsed_ms: 0,
            },
            evaluation,
        });
    }
    Ok(observations)
}

#[cfg(feature = "kernel")]
fn historical_evaluation(payload: &serde_json::Value) -> Result<CandidateEvaluation, String> {
    let evaluation: CandidateEvaluation =
        serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
    // These exact prior-release schemas may be replayed for search state only.
    let pre_predictive_schema = matches!(
        evaluation.evaluator_version.as_str(),
        "purged-walk-forward-v2" | "onnx-purged-walk-forward-v1"
    );
    if !pre_predictive_schema {
        evaluation
            .validate()
            .map_err(|error| format!("historical evaluation is invalid: {error}"))?;
    }
    Ok(evaluation)
}

#[cfg(feature = "kernel")]
fn budget_exhausted(
    budget: &alpha_domain::SearchBudget,
    usage: &alpha_domain::SearchBudgetUsage,
) -> bool {
    usage.candidates >= budget.max_candidates
        || (budget.max_expansions > 0 && usage.expansions >= budget.max_expansions)
        || (budget.max_tokens > 0 && usage.tokens >= budget.max_tokens)
        || (budget.max_seconds > 0 && usage.elapsed_ms >= budget.max_seconds.saturating_mul(1_000))
}

#[cfg(feature = "kernel")]
fn exhausted_budget_limits(
    budget: &alpha_domain::SearchBudget,
    usage: &alpha_domain::SearchBudgetUsage,
) -> Vec<SearchBudgetLimit> {
    let mut limits = Vec::new();
    if usage.candidates >= budget.max_candidates {
        limits.push(SearchBudgetLimit::Candidates);
    }
    if budget.max_expansions > 0 && usage.expansions >= budget.max_expansions {
        limits.push(SearchBudgetLimit::Expansions);
    }
    if budget.max_tokens > 0 && usage.tokens >= budget.max_tokens {
        limits.push(SearchBudgetLimit::Tokens);
    }
    if budget.max_seconds > 0 && usage.elapsed_ms >= budget.max_seconds.saturating_mul(1_000) {
        limits.push(SearchBudgetLimit::Time);
    }
    limits
}

#[cfg(feature = "kernel")]
fn remaining_budget(
    budget: &alpha_domain::SearchBudget,
    usage: &alpha_domain::SearchBudgetUsage,
) -> RemainingBudget {
    RemainingBudget {
        candidates: budget.max_candidates.saturating_sub(usage.candidates),
        expansions: remaining_limit(budget.max_expansions, usage.expansions),
        tokens: remaining_limit(budget.max_tokens, usage.tokens),
        milliseconds: remaining_limit(budget.max_seconds.saturating_mul(1_000), usage.elapsed_ms),
    }
}

#[cfg(feature = "kernel")]
fn remaining_limit(limit: u64, used: u64) -> u64 {
    if limit == 0 {
        u64::MAX
    } else {
        limit.saturating_sub(used)
    }
}

#[cfg(all(test, feature = "kernel"))]
mod tests {
    use super::*;
    use crate::engines::{
        BayesianOptimizerEngine, GeneticProgrammingEngine, MctsEngine, OfflineRlEngine,
        OfflineTrace,
    };
    use crate::evaluation::{prepare_dataset, ResearchRow, WalkForwardConfig};
    use alpha_domain::{MissionCompletionPolicy, ResearchMission, SearchBudget, ValidatorMode};
    use chrono::Duration;
    use hft_research_manifest::ManifestId;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingEngine {
        calls: Arc<AtomicUsize>,
        crash: bool,
    }

    impl ProposalEngine for CountingEngine {
        fn kind(&self) -> EngineKind {
            EngineKind::ManualSeed
        }

        fn propose(
            &mut self,
            mission_id: &str,
            iteration_index: usize,
            context: &ProposalContext<'_>,
            remaining: &RemainingBudget,
        ) -> Result<EngineProposal, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(context.row_count(), 40);
            assert!(remaining.candidates > 0);
            if self.crash {
                return Err("fixture crash".to_string());
            }
            Ok(EngineProposal {
                candidate_id: format!("{mission_id}-candidate-{iteration_index}"),
                hypothesis: "signal predicts label".to_string(),
                artifact: CandidateArtifact::Program(
                    serde_json::json!({"op": "identity", "iteration": iteration_index}),
                ),
                expansions: 1,
                tokens: 0,
                elapsed_ms: 1,
            })
        }
    }

    struct PassingEvaluator;

    impl CandidateEvaluator for PassingEvaluator {
        fn evaluate(
            &self,
            _proposal: &EngineProposal,
            context: &EngineContext<'_>,
        ) -> Result<CandidateEvaluation, String> {
            Ok(CandidateEvaluation {
                passed: context.folds().len() == 3,
                score: 1.0,
                failure_reasons: vec![],
                evaluator_version: "fixture-v1".to_string(),
                evaluator_config: serde_json::json!({"fixture": true}),
                metrics: EvaluationMetrics {
                    predictive: PredictiveMetrics::from_folds(vec![FoldPredictiveMetrics {
                        fold_index: 1,
                        row_count: context.rows().len(),
                        time_series_ic: Some(1.0),
                        time_series_rank_ic: Some(1.0),
                    }]),
                    row_count: context.rows().len(),
                    trade_count: context.rows().len(),
                    mean_net_return: 1.0,
                    cumulative_net_return: context.rows().len() as f64,
                    max_drawdown: 0.0,
                    net_sharpe: 1.0,
                    raw_score: 1.0,
                    adjusted_score: 1.0,
                    folds: vec![FoldEvaluationMetrics {
                        fold_index: 1,
                        row_count: context.rows().len(),
                        trade_count: context.rows().len(),
                        mean_net_return: 1.0,
                        cumulative_net_return: context.rows().len() as f64,
                        max_drawdown: 0.0,
                        net_sharpe: 1.0,
                        raw_score: 1.0,
                    }],
                },
            })
        }
    }

    fn mission() -> ResearchMission {
        let now = Utc::now();
        ResearchMission {
            mission_id: "mission-1".to_string(),
            objective: "find one candidate".to_string(),
            hypothesis_scope: "fixture".to_string(),
            mutable_scope: vec!["candidate".to_string()],
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({"score": 1.0}),
            search_budget: SearchBudget {
                max_candidates: 3,
                max_expansions: 10,
                max_tokens: 0,
                max_seconds: 30,
            },
            completion_policy: MissionCompletionPolicy {
                min_kept_candidates: 3,
            },
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn dataset() -> PreparedDataset {
        let start = Utc::now();
        let rows = (0..50)
            .map(|index| ResearchRow {
                available_time: start + Duration::seconds(index),
                signal: index as f64,
                features: std::collections::BTreeMap::new(),
                label: index as f64 * 0.01,
                fee_bps: 1.0,
                funding_bps: 0.1,
                latency_bps: 0.2,
            })
            .collect();
        prepare_dataset(
            rows,
            &WalkForwardConfig {
                initial_train_rows: 20,
                validation_rows: 5,
                fold_count: 3,
                purge_rows: 2,
                embargo_rows: 1,
                sealed_holdout_rows: 10,
            },
            "holdout-1",
        )
        .unwrap()
    }

    #[test]
    fn historical_evaluation_compatibility_is_limited_to_pre_predictive_versions() {
        fn remove_predictive_fields(payload: &mut serde_json::Value) {
            let metrics = payload["metrics"].as_object_mut().unwrap();
            metrics.remove("predictive");
            metrics.remove("net_sharpe");
            for fold in metrics["folds"].as_array_mut().unwrap() {
                fold.as_object_mut().unwrap().remove("net_sharpe");
            }
        }

        let input = dataset();
        let evaluation = PassingEvaluator
            .evaluate(
                &EngineProposal {
                    candidate_id: "candidate-1".to_string(),
                    hypothesis: "fixture".to_string(),
                    artifact: CandidateArtifact::Program(serde_json::json!({"op": "identity"})),
                    expansions: 1,
                    tokens: 0,
                    elapsed_ms: 1,
                },
                &input.engine_context(),
            )
            .unwrap();
        let mut malformed_current = serde_json::to_value(&evaluation).unwrap();
        remove_predictive_fields(&mut malformed_current);
        assert!(historical_evaluation(&malformed_current).is_err());

        let mut legacy = malformed_current;
        legacy["evaluator_version"] = serde_json::json!("purged-walk-forward-v2");
        let restored = historical_evaluation(&legacy).unwrap();
        assert_eq!(restored.score, evaluation.score);
        assert!(restored.metrics.net_sharpe.is_nan());
        assert!(restored.validate().is_err());
    }

    #[test]
    fn resume_skips_completed_iterations() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        {
            let engine = CountingEngine {
                calls: calls.clone(),
                crash: false,
            };
            let mut kernel = AutoResearchKernel::new(&mut store, engine, PassingEvaluator);
            let outcome = kernel
                .run(
                    "mission-1",
                    &dataset(),
                    RunControl {
                        max_new_iterations: Some(2),
                    },
                )
                .unwrap();
            assert_eq!(outcome.status, MissionStatus::Paused);
        }
        {
            let engine = CountingEngine {
                calls: calls.clone(),
                crash: false,
            };
            let mut kernel = AutoResearchKernel::new(&mut store, engine, PassingEvaluator);
            let outcome = kernel
                .run("mission-1", &dataset(), RunControl::default())
                .unwrap();
            assert_eq!(outcome.status, MissionStatus::Completed);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let lineage = store.mission_lineage("mission-1").unwrap();
        assert_eq!(lineage.iterations.len(), 3);
        assert_eq!(lineage.evaluations.len(), 3);
    }

    #[test]
    fn completion_policy_stops_search_before_budget_exhaustion() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mut mission = mission();
        mission.completion_policy.min_kept_candidates = 1;
        store.create_mission(&mission).unwrap();
        let outcome = AutoResearchKernel::new(
            &mut store,
            CountingEngine {
                calls: calls.clone(),
                crash: false,
            },
            PassingEvaluator,
        )
        .run("mission-1", &dataset(), RunControl::default())
        .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(outcome.status, MissionStatus::Completed);
        assert_eq!(
            outcome.terminal_reason,
            Some(MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 })
        );
        assert_eq!(
            store.get_mission("mission-1").unwrap().terminal_reason,
            outcome.terminal_reason
        );
    }

    #[test]
    fn resume_rejects_engine_kind_change_before_mission_state_transition() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        AutoResearchKernel::new(
            &mut store,
            CountingEngine {
                calls: Arc::new(AtomicUsize::new(0)),
                crash: false,
            },
            PassingEvaluator,
        )
        .run(
            "mission-1",
            &dataset(),
            RunControl {
                max_new_iterations: Some(1),
            },
        )
        .unwrap();

        let error = AutoResearchKernel::new(
            &mut store,
            GeneticProgrammingEngine::new(7, vec!["signal".to_string()], 4, 3).unwrap(),
            PassingEvaluator,
        )
        .run("mission-1", &dataset(), RunControl::default())
        .unwrap_err();
        assert!(matches!(error, EngineError::Checkpoint(_)));
        assert_eq!(
            store.get_mission("mission-1").unwrap().status,
            MissionStatus::Paused
        );
    }

    #[test]
    fn proposal_crashes_remain_queryable() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut store = AlphaStore::open_in_memory().unwrap();
        store.create_mission(&mission()).unwrap();
        let engine = CountingEngine { calls, crash: true };
        let mut kernel = AutoResearchKernel::new(&mut store, engine, PassingEvaluator);
        let outcome = kernel
            .run("mission-1", &dataset(), RunControl::default())
            .unwrap();
        assert_eq!(outcome.status, MissionStatus::BudgetExhausted);
        assert!(store
            .mission_lineage("mission-1")
            .unwrap()
            .iterations
            .iter()
            .all(|iteration| iteration.verdict == IterationVerdict::Crash));
    }

    #[test]
    fn gp_resume_restores_history_and_keeps_candidate_artifacts_unique() {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mut mission = mission();
        mission.search_budget.max_candidates = 4;
        mission.search_budget.max_expansions = 50;
        mission.completion_policy.min_kept_candidates = 4;
        store.create_mission(&mission).unwrap();
        {
            let engine =
                GeneticProgrammingEngine::new(7, vec!["signal".to_string()], 8, 5).unwrap();
            AutoResearchKernel::new(&mut store, engine, PassingEvaluator)
                .run(
                    "mission-1",
                    &dataset(),
                    RunControl {
                        max_new_iterations: Some(2),
                    },
                )
                .unwrap();
        }
        {
            let engine =
                GeneticProgrammingEngine::new(7, vec!["signal".to_string()], 8, 5).unwrap();
            AutoResearchKernel::new(&mut store, engine, PassingEvaluator)
                .run("mission-1", &dataset(), RunControl::default())
                .unwrap();
        }
        let lineage = store.mission_lineage("mission-1").unwrap();
        let unique = lineage
            .candidates
            .iter()
            .map(|candidate| serde_json::to_string(&candidate.artifact).unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(lineage.candidates.len(), 4);
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn expansion_budget_stops_additional_proposals() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mut mission = mission();
        mission.search_budget.max_expansions = 2;
        store.create_mission(&mission).unwrap();
        let engine = CountingEngine {
            calls: calls.clone(),
            crash: false,
        };
        let mut kernel = AutoResearchKernel::new(&mut store, engine, PassingEvaluator);
        kernel
            .run("mission-1", &dataset(), RunControl::default())
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn real_engines_run_through_persistent_kernel() {
        let fields = vec!["oi".to_string(), "imbalance".to_string()];
        let gp_left =
            run_single_engine(GeneticProgrammingEngine::new(9, fields.clone(), 4, 4).unwrap());
        let gp_right = run_single_engine(GeneticProgrammingEngine::new(9, fields, 4, 4).unwrap());
        assert_eq!(gp_left, gp_right);

        let mcts_left = run_single_engine(MctsEngine::new(9, "oi", "imbalance", 1.4, 3).unwrap());
        let mcts_right = run_single_engine(MctsEngine::new(9, "oi", "imbalance", 1.4, 3).unwrap());
        assert_eq!(mcts_left, mcts_right);

        assert!(!run_single_engine(
            BayesianOptimizerEngine::new("oi", 5.0, 60.0, 12, 1e-6, 10.0, 0.01).unwrap()
        )
        .is_empty());

        let traces = vec![
            OfflineTrace {
                state: "positive".to_string(),
                action: "rank".to_string(),
                reward: 1.0,
                next_state: "positive".to_string(),
                terminal: false,
            },
            OfflineTrace {
                state: "positive".to_string(),
                action: "mean".to_string(),
                reward: -1.0,
                next_state: "flat".to_string(),
                terminal: true,
            },
            OfflineTrace {
                state: "flat".to_string(),
                action: "rank".to_string(),
                reward: 0.2,
                next_state: "positive".to_string(),
                terminal: false,
            },
        ];
        assert!(!run_single_engine(
            OfflineRlEngine::train("oi", "policy-1", &traces, 3, 0.2, 0.9, 20).unwrap()
        )
        .is_empty());
    }

    #[test]
    fn mcts_kernel_resume_matches_uninterrupted_run() {
        fn candidates(split_after: Option<usize>) -> Vec<String> {
            let mut store = AlphaStore::open_in_memory().unwrap();
            let mut mission = mission();
            mission.search_budget.max_candidates = 4;
            mission.search_budget.max_expansions = 100;
            mission.completion_policy.min_kept_candidates = 4;
            store.create_mission(&mission).unwrap();
            if let Some(limit) = split_after {
                AutoResearchKernel::new(
                    &mut store,
                    MctsEngine::new(9, "signal", "signal", 1.4, 4).unwrap(),
                    PassingEvaluator,
                )
                .run(
                    "mission-1",
                    &dataset(),
                    RunControl {
                        max_new_iterations: Some(limit),
                    },
                )
                .unwrap();
            }
            AutoResearchKernel::new(
                &mut store,
                MctsEngine::new(9, "signal", "signal", 1.4, 4).unwrap(),
                PassingEvaluator,
            )
            .run("mission-1", &dataset(), RunControl::default())
            .unwrap();
            store
                .mission_lineage("mission-1")
                .unwrap()
                .candidates
                .into_iter()
                .map(|candidate| serde_json::to_string(&candidate.artifact).unwrap())
                .collect()
        }

        assert_eq!(candidates(Some(2)), candidates(None));
    }

    fn run_single_engine<P: ProposalEngine>(engine: P) -> String {
        let mut store = AlphaStore::open_in_memory().unwrap();
        let mut mission = mission();
        mission.search_budget.max_candidates = 1;
        store.create_mission(&mission).unwrap();
        AutoResearchKernel::new(&mut store, engine, PassingEvaluator)
            .run("mission-1", &dataset(), RunControl::default())
            .unwrap();
        match &store.mission_lineage("mission-1").unwrap().candidates[0].artifact {
            CandidateArtifact::Formula(ast) => ast.to_string(),
            other => panic!("expected formula candidate, got {other:?}"),
        }
    }
}
