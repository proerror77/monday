//! Structured research memory and harness improvement contracts.

use chrono::{DateTime, Utc};
use hft_live_small_supervisor::{LiveSmallAction, LiveSmallDecision};
use hft_promotion_gate::{GateFailure, PromotionGateDecision};
use hft_research_manifest::ManifestRef;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResearchMemoryError {
    #[error("memory event id cannot be empty")]
    EmptyEventId,
    #[error("failure explanation cannot be empty")]
    EmptyExplanation,
    #[error("harness change proposal id cannot be empty")]
    EmptyProposalId,
    #[error("harness changes cannot grant live trading authority")]
    LiveAuthorityChangeDenied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    DataUnavailable,
    DataStale,
    ManifestMissing,
    SchemaMismatch,
    LeakageDetected,
    InsufficientSample,
    OverfitDetected,
    HighCorrelation,
    GateFailed,
    ApprovalRequired,
    RiskCapExceeded,
    RuntimeRejected,
    SentinelStopped,
    RollbackFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemorySource {
    Evaluation,
    PromotionGate,
    LiveSmallSupervisor,
    RuntimeFeedback,
    HarnessCritic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchMemoryEvent {
    pub event_id: String,
    pub source: MemorySource,
    pub failure_kind: FailureKind,
    pub related_manifest: Option<ManifestRef>,
    pub explanation: String,
    pub created_at: DateTime<Utc>,
}

impl ResearchMemoryEvent {
    pub fn validate(&self) -> Result<(), ResearchMemoryError> {
        if self.event_id.trim().is_empty() {
            return Err(ResearchMemoryError::EmptyEventId);
        }
        if self.explanation.trim().is_empty() {
            return Err(ResearchMemoryError::EmptyExplanation);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HarnessChangeKind {
    PromptTemplate,
    SearchSpace,
    EvaluatorRecipe,
    FeatureSet,
    MemoryRetrieval,
    WorkflowOrder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessChangeProposal {
    pub proposal_id: String,
    pub change_kind: HarnessChangeKind,
    pub source_event_ids: Vec<String>,
    pub description: String,
    pub grants_live_trading_authority: bool,
}

impl HarnessChangeProposal {
    pub fn validate(&self) -> Result<(), ResearchMemoryError> {
        if self.proposal_id.trim().is_empty() {
            return Err(ResearchMemoryError::EmptyProposalId);
        }
        if self.description.trim().is_empty() {
            return Err(ResearchMemoryError::EmptyExplanation);
        }
        if self.grants_live_trading_authority {
            return Err(ResearchMemoryError::LiveAuthorityChangeDenied);
        }
        Ok(())
    }
}

pub fn memory_from_promotion_gate(
    event_id: impl Into<String>,
    decision: &PromotionGateDecision,
    created_at: DateTime<Utc>,
) -> Option<ResearchMemoryEvent> {
    if decision.passed {
        return None;
    }
    Some(ResearchMemoryEvent {
        event_id: event_id.into(),
        source: MemorySource::PromotionGate,
        failure_kind: if decision.failures.contains(&GateFailure::ApprovalRequired) {
            FailureKind::ApprovalRequired
        } else {
            FailureKind::GateFailed
        },
        related_manifest: None,
        explanation: format!("promotion gate failed: {:?}", decision.failures),
        created_at,
    })
}

pub fn memory_from_live_small(
    event_id: impl Into<String>,
    decision: &LiveSmallDecision,
    created_at: DateTime<Utc>,
) -> Option<ResearchMemoryEvent> {
    match decision.action {
        LiveSmallAction::AllowRollout => None,
        LiveSmallAction::BlockRollout => Some(ResearchMemoryEvent {
            event_id: event_id.into(),
            source: MemorySource::LiveSmallSupervisor,
            failure_kind: FailureKind::RiskCapExceeded,
            related_manifest: None,
            explanation: format!("live-small rollout blocked: {:?}", decision.blockers),
            created_at,
        }),
        LiveSmallAction::Rollback => Some(ResearchMemoryEvent {
            event_id: event_id.into(),
            source: MemorySource::LiveSmallSupervisor,
            failure_kind: FailureKind::SentinelStopped,
            related_manifest: None,
            explanation: format!("live-small rollback: {:?}", decision.rollback_trigger),
            created_at,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_live_small_supervisor::{rollback, RollbackTrigger};

    #[test]
    fn captures_promotion_gate_failure() {
        let event = memory_from_promotion_gate(
            "event-1",
            &PromotionGateDecision {
                passed: false,
                failures: vec![GateFailure::ApprovalRequired],
            },
            Utc::now(),
        )
        .unwrap();

        assert_eq!(event.failure_kind, FailureKind::ApprovalRequired);
        assert_eq!(event.validate(), Ok(()));
    }

    #[test]
    fn captures_live_small_rollback() {
        let event = memory_from_live_small(
            "event-1",
            &rollback(RollbackTrigger::SentinelStopped),
            Utc::now(),
        )
        .unwrap();

        assert_eq!(event.failure_kind, FailureKind::SentinelStopped);
    }

    #[test]
    fn rejects_harness_change_that_grants_live_authority() {
        let proposal = HarnessChangeProposal {
            proposal_id: "change-1".to_string(),
            change_kind: HarnessChangeKind::WorkflowOrder,
            source_event_ids: vec!["event-1".to_string()],
            description: "try a different evaluator order".to_string(),
            grants_live_trading_authority: true,
        };

        assert_eq!(
            proposal.validate().unwrap_err(),
            ResearchMemoryError::LiveAuthorityChangeDenied
        );
    }
}
