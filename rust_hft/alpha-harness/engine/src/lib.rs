//! Persistent, budgeted AutoResearch mission kernel.

pub mod evaluation;

use alpha_domain::{
    CandidateArtifact, EngineKind, IterationVerdict, MissionStatus, ResearchIteration,
};
use alpha_store::{AlphaStore, EvaluationRecord, RunCheckpoint, StoreError};
use chrono::Utc;
use evaluation::{EngineContext, PreparedDataset};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("control-plane store failed: {0}")]
    Store(#[from] StoreError),
    #[error("mission is already terminal")]
    TerminalMission,
    #[error("proposal engine failed: {0}")]
    Proposal(String),
    #[error("candidate evaluator failed: {0}")]
    Evaluation(String),
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

pub trait ProposalEngine {
    fn kind(&self) -> EngineKind;
    fn propose(
        &mut self,
        mission_id: &str,
        iteration_index: usize,
        context: &EngineContext<'_>,
        remaining: &RemainingBudget,
    ) -> Result<EngineProposal, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingBudget {
    pub candidates: usize,
    pub expansions: u64,
    pub tokens: u64,
    pub milliseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub passed: bool,
    pub score: f64,
    pub failure_reasons: Vec<String>,
    pub evaluator_version: String,
}

pub trait CandidateEvaluator {
    fn evaluate(
        &self,
        proposal: &EngineProposal,
        context: &EngineContext<'_>,
    ) -> Result<CandidateEvaluation, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunControl {
    pub max_new_iterations: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub status: MissionStatus,
    pub total_iterations: usize,
    pub new_iterations: usize,
}

pub struct AutoResearchKernel<'a, P, E> {
    store: &'a mut AlphaStore,
    proposal_engine: P,
    evaluator: E,
}

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
            MissionStatus::Pending | MissionStatus::Paused => {
                self.store
                    .transition_mission(mission_id, MissionStatus::Running, now)?;
            }
            MissionStatus::Running => {}
            MissionStatus::Completed | MissionStatus::BudgetExhausted | MissionStatus::Failed => {
                return Err(EngineError::TerminalMission)
            }
        }

        let lineage = self.store.mission_lineage(mission_id)?;
        let mut usage = lineage
            .iterations
            .last()
            .map(|iteration| iteration.budget_usage.clone())
            .or_else(|| {
                self.store
                    .get_checkpoint(mission_id)
                    .ok()
                    .map(|value| value.budget_usage)
            })
            .unwrap_or_default();
        let mut total_iterations = lineage.iterations.len();
        let mut kept = lineage
            .iterations
            .iter()
            .filter(|iteration| iteration.verdict == IterationVerdict::Keep)
            .count();
        let mut new_iterations = 0;
        let context = dataset.engine_context();

        while !budget_exhausted(&mission.search_budget, &usage) {
            if control
                .max_new_iterations
                .is_some_and(|limit| new_iterations >= limit)
            {
                self.store
                    .transition_mission(mission_id, MissionStatus::Paused, Utc::now())?;
                return Ok(RunOutcome {
                    status: MissionStatus::Paused,
                    total_iterations,
                    new_iterations,
                });
            }

            let index = total_iterations + 1;
            let iteration_id = format!("{mission_id}-iteration-{index}");
            let created_at = Utc::now();
            let remaining = remaining_budget(&mission.search_budget, &usage);
            let started = std::time::Instant::now();
            let proposal = self
                .proposal_engine
                .propose(mission_id, index, &context, &remaining);
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
                    let evaluated = if within_budget {
                        self.evaluator
                            .evaluate(&proposal, &context)
                            .and_then(|result| {
                                result.score.is_finite().then_some(result).ok_or_else(|| {
                                    "evaluator returned non-finite score".to_string()
                                })
                            })
                    } else {
                        Err("proposal exceeded remaining search budget".to_string())
                    };
                    match evaluated {
                        Ok(result) => {
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
                        Err(error) => (
                            ResearchIteration {
                                iteration_id,
                                mission_id: mission_id.to_string(),
                                parent_candidate_ids: vec![],
                                engine: self.proposal_engine.kind(),
                                hypothesis: proposal.hypothesis.clone(),
                                candidate_artifact_id: Some(proposal.candidate_id.clone()),
                                evaluation_artifact_id: None,
                                budget_usage: usage.clone(),
                                verdict: IterationVerdict::Crash,
                                failure_class: Some("evaluation_error".to_string()),
                                failure_explanation: Some(error),
                                created_at,
                            },
                            Some((proposal.candidate_id, proposal.artifact)),
                            None,
                        ),
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

            self.store.append_iteration(
                &iteration,
                candidate
                    .as_ref()
                    .map(|(candidate_id, artifact)| (candidate_id.as_str(), artifact)),
                evaluation.as_ref(),
            )?;
            self.store.save_checkpoint(&RunCheckpoint {
                mission_id: mission_id.to_string(),
                last_iteration_id: Some(iteration.iteration_id.clone()),
                budget_usage: usage.clone(),
                updated_at: created_at,
            })?;
            total_iterations += 1;
            new_iterations += 1;
        }

        let status = if kept > 0 {
            MissionStatus::Completed
        } else {
            MissionStatus::BudgetExhausted
        };
        self.store
            .transition_mission(mission_id, status.clone(), Utc::now())?;
        Ok(RunOutcome {
            status,
            total_iterations,
            new_iterations,
        })
    }
}

fn budget_exhausted(
    budget: &alpha_domain::SearchBudget,
    usage: &alpha_domain::SearchBudgetUsage,
) -> bool {
    usage.candidates >= budget.max_candidates
        || (budget.max_expansions > 0 && usage.expansions >= budget.max_expansions)
        || (budget.max_tokens > 0 && usage.tokens >= budget.max_tokens)
        || (budget.max_seconds > 0 && usage.elapsed_ms >= budget.max_seconds.saturating_mul(1_000))
}

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

fn remaining_limit(limit: u64, used: u64) -> u64 {
    if limit == 0 {
        u64::MAX
    } else {
        limit.saturating_sub(used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::{prepare_dataset, ResearchRow, WalkForwardConfig};
    use alpha_domain::{ResearchMission, SearchBudget, ValidatorMode};
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
            context: &EngineContext<'_>,
            remaining: &RemainingBudget,
        ) -> Result<EngineProposal, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(context.rows().len(), 40);
            assert!(remaining.candidates > 0);
            if self.crash {
                return Err("fixture crash".to_string());
            }
            Ok(EngineProposal {
                candidate_id: format!("{mission_id}-candidate-{iteration_index}"),
                hypothesis: "signal predicts label".to_string(),
                artifact: CandidateArtifact::Program(serde_json::json!({"op": "identity"})),
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
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
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
}
