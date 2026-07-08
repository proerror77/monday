//! Loop orchestration contracts for agentic alpha discovery.
//!
//! This crate does not call agents, train models, or execute orders. It defines
//! the loop state and evidence checks that keep those systems inside a
//! reproducible research-to-live-small harness.

use chrono::{DateTime, Utc};
use hft_factor_eval::EvaluationDecision;
use hft_live_small_supervisor::{LiveSmallAction, LiveSmallDecision};
use hft_promotion_gate::PromotionGateDecision;
use hft_research_manifest::ManifestRef;
use hft_search_protocol::ProposalArtifact;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LoopEngineError {
    #[error("loop run id cannot be empty")]
    EmptyRunId,
    #[error("loop goal cannot be empty")]
    EmptyGoal,
    #[error("loop max iterations must be greater than zero")]
    InvalidIterationBudget,
    #[error("loop current iteration cannot exceed max iterations")]
    CurrentIterationBeyondBudget,
    #[error("proposal artifact is invalid")]
    InvalidProposal,
    #[error("promotion cannot pass when evaluation failed")]
    PromotionPassedAfterEvaluationFailure,
    #[error("live-small rollout cannot be allowed when promotion failed")]
    LiveSmallAllowedAfterFailedPromotion,
    #[error("passed promotion requires a live-small decision")]
    MissingLiveSmallDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopTrigger {
    TurnBased,
    GoalBased,
    TimeBased,
    EventBased,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStageKind {
    GatherContext,
    GenerateCandidates,
    EvaluateCandidates,
    PromoteCandidate,
    LiveSmallSupervision,
    CaptureMemory,
    Audit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStageStatus {
    Pending,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopStageRecord {
    pub kind: LoopStageKind,
    pub status: LoopStageStatus,
    pub artifact_refs: Vec<ManifestRef>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoneCondition {
    StagePassed(LoopStageKind),
    IterationBudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRun {
    pub run_id: String,
    pub trigger: LoopTrigger,
    pub goal: String,
    pub started_at: DateTime<Utc>,
    pub current_iteration: u32,
    pub max_iterations: u32,
    pub done_condition: DoneCondition,
    pub stages: Vec<LoopStageRecord>,
}

impl LoopRun {
    pub fn validate(&self) -> Result<(), LoopEngineError> {
        if self.run_id.trim().is_empty() {
            return Err(LoopEngineError::EmptyRunId);
        }
        if self.goal.trim().is_empty() {
            return Err(LoopEngineError::EmptyGoal);
        }
        if self.max_iterations == 0 {
            return Err(LoopEngineError::InvalidIterationBudget);
        }
        if self.current_iteration > self.max_iterations {
            return Err(LoopEngineError::CurrentIterationBeyondBudget);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStopReason {
    DoneConditionMet,
    IterationBudgetExhausted,
    StageFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopNextAction {
    Continue { next_stage: LoopStageKind },
    Stop { reason: LoopStopReason },
}

pub fn evaluate_loop_run(run: &LoopRun) -> Result<LoopNextAction, LoopEngineError> {
    run.validate()?;
    if run
        .stages
        .iter()
        .any(|stage| stage.status == LoopStageStatus::Failed)
    {
        return Ok(LoopNextAction::Stop {
            reason: LoopStopReason::StageFailed,
        });
    }

    match &run.done_condition {
        DoneCondition::StagePassed(done_stage)
            if run.stages.iter().any(|stage| {
                &stage.kind == done_stage && stage.status == LoopStageStatus::Passed
            }) =>
        {
            return Ok(LoopNextAction::Stop {
                reason: LoopStopReason::DoneConditionMet,
            });
        }
        DoneCondition::IterationBudgetExhausted if run.current_iteration >= run.max_iterations => {
            return Ok(LoopNextAction::Stop {
                reason: LoopStopReason::IterationBudgetExhausted,
            });
        }
        _ => {}
    }

    if run.current_iteration >= run.max_iterations {
        return Ok(LoopNextAction::Stop {
            reason: LoopStopReason::IterationBudgetExhausted,
        });
    }

    let next_stage = run
        .stages
        .iter()
        .find(|stage| stage.status == LoopStageStatus::Pending)
        .map(|stage| stage.kind.clone())
        .unwrap_or(LoopStageKind::GenerateCandidates);

    Ok(LoopNextAction::Continue { next_stage })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateLoopEvidence {
    pub proposal: ProposalArtifact,
    pub evaluation: EvaluationDecision,
    pub promotion: PromotionGateDecision,
    pub live_small: Option<LiveSmallDecision>,
}

impl CandidateLoopEvidence {
    pub fn validate(&self) -> Result<(), LoopEngineError> {
        self.proposal
            .validate()
            .map_err(|_| LoopEngineError::InvalidProposal)?;
        if !self.evaluation.passed && self.promotion.passed {
            return Err(LoopEngineError::PromotionPassedAfterEvaluationFailure);
        }
        if self.promotion.passed && self.live_small.is_none() {
            return Err(LoopEngineError::MissingLiveSmallDecision);
        }
        if !self.promotion.passed
            && self
                .live_small
                .as_ref()
                .map(|decision| decision.action == LiveSmallAction::AllowRollout)
                .unwrap_or(false)
        {
            return Err(LoopEngineError::LiveSmallAllowedAfterFailedPromotion);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_factor_eval::EvaluationFailure;
    use hft_live_small_supervisor::LiveSmallDecision;
    use hft_promotion_gate::GateFailure;
    use hft_research_manifest::{ManifestId, ManifestRef};
    use hft_search_protocol::SearchEngineKind;
    use std::collections::BTreeMap;

    fn run(stages: Vec<LoopStageRecord>) -> LoopRun {
        LoopRun {
            run_id: "loop-1".to_string(),
            trigger: LoopTrigger::GoalBased,
            goal: "find and validate one deployable factor".to_string(),
            started_at: Utc::now(),
            current_iteration: 1,
            max_iterations: 3,
            done_condition: DoneCondition::StagePassed(LoopStageKind::Audit),
            stages,
        }
    }

    fn stage(kind: LoopStageKind, status: LoopStageStatus) -> LoopStageRecord {
        LoopStageRecord {
            kind,
            status,
            artifact_refs: vec![ManifestRef::new(
                ManifestId::new("artifact-1").unwrap(),
                "artifact_manifest",
            )
            .unwrap()],
            summary: "checked".to_string(),
        }
    }

    fn proposal() -> ProposalArtifact {
        ProposalArtifact {
            proposal_id: "proposal-1".to_string(),
            engine: SearchEngineKind::LlmProposer,
            search_manifest_id: ManifestId::new("search-1").unwrap(),
            parent_factor_ids: vec![],
            ast: FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
            mcts_trace: None,
            parameters: BTreeMap::new(),
            rationale: Some(
                "hypothesis: open interest change predicts short horizon flow".to_string(),
            ),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn stops_when_done_stage_passed() {
        assert_eq!(
            evaluate_loop_run(&run(vec![stage(
                LoopStageKind::Audit,
                LoopStageStatus::Passed
            )]))
            .unwrap(),
            LoopNextAction::Stop {
                reason: LoopStopReason::DoneConditionMet
            }
        );
    }

    #[test]
    fn stops_when_any_stage_failed() {
        assert_eq!(
            evaluate_loop_run(&run(vec![stage(
                LoopStageKind::EvaluateCandidates,
                LoopStageStatus::Failed
            )]))
            .unwrap(),
            LoopNextAction::Stop {
                reason: LoopStopReason::StageFailed
            }
        );
    }

    #[test]
    fn continues_to_next_pending_stage() {
        assert_eq!(
            evaluate_loop_run(&run(vec![
                stage(LoopStageKind::GatherContext, LoopStageStatus::Passed),
                stage(LoopStageKind::GenerateCandidates, LoopStageStatus::Pending),
            ]))
            .unwrap(),
            LoopNextAction::Continue {
                next_stage: LoopStageKind::GenerateCandidates
            }
        );
    }

    #[test]
    fn rejects_promotion_without_passing_evaluation() {
        let evidence = CandidateLoopEvidence {
            proposal: proposal(),
            evaluation: EvaluationDecision {
                passed: false,
                failures: vec![EvaluationFailure::RankIcBelowFloor],
            },
            promotion: PromotionGateDecision {
                passed: true,
                failures: vec![],
            },
            live_small: Some(LiveSmallDecision {
                action: LiveSmallAction::AllowRollout,
                blockers: vec![],
                rollback_trigger: None,
            }),
        };

        assert_eq!(
            evidence.validate().unwrap_err(),
            LoopEngineError::PromotionPassedAfterEvaluationFailure
        );
    }

    #[test]
    fn rejects_live_small_allow_after_failed_promotion() {
        let evidence = CandidateLoopEvidence {
            proposal: proposal(),
            evaluation: EvaluationDecision {
                passed: true,
                failures: vec![],
            },
            promotion: PromotionGateDecision {
                passed: false,
                failures: vec![GateFailure::ApprovalRequired],
            },
            live_small: Some(LiveSmallDecision {
                action: LiveSmallAction::AllowRollout,
                blockers: vec![],
                rollback_trigger: None,
            }),
        };

        assert_eq!(
            evidence.validate().unwrap_err(),
            LoopEngineError::LiveSmallAllowedAfterFailedPromotion
        );
    }
}
