//! Audit bundles for the Agentic Alpha Harness loop.

use hft_allocator_policy::{AllocatorPolicyError, AllocatorPolicyProposal};
use hft_factor_eval::EvaluationDecision;
use hft_live_small_supervisor::{LiveSmallAction, LiveSmallDecision};
use hft_promotion_gate::PromotionGateDecision;
use hft_research_memory::{ResearchMemoryError, ResearchMemoryEvent};
use hft_search_protocol::{ProposalArtifact, SearchProtocolError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuditTrailError {
    #[error("audit bundle id cannot be empty")]
    EmptyBundleId,
    #[error("audit bundle must include at least one proposal")]
    MissingProposal,
    #[error("invalid proposal: {0}")]
    InvalidProposal(#[from] SearchProtocolError),
    #[error("invalid allocator policy: {0}")]
    InvalidAllocator(#[from] AllocatorPolicyError),
    #[error("invalid memory event: {0}")]
    InvalidMemory(#[from] ResearchMemoryError),
    #[error("live rollout cannot be allowed when evaluation failed")]
    EvaluationFailedForLiveRollout,
    #[error("live rollout cannot be allowed when promotion failed")]
    PromotionFailedForLiveRollout,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessAuditBundle {
    pub bundle_id: String,
    pub proposals: Vec<ProposalArtifact>,
    pub evaluation: EvaluationDecision,
    pub promotion: PromotionGateDecision,
    pub allocator_policy: AllocatorPolicyProposal,
    pub live_small: LiveSmallDecision,
    pub memory_events: Vec<ResearchMemoryEvent>,
}

impl HarnessAuditBundle {
    pub fn validate(&self) -> Result<(), AuditTrailError> {
        if self.bundle_id.trim().is_empty() {
            return Err(AuditTrailError::EmptyBundleId);
        }
        if self.proposals.is_empty() {
            return Err(AuditTrailError::MissingProposal);
        }
        for proposal in &self.proposals {
            proposal.validate()?;
        }
        self.allocator_policy.validate()?;
        for event in &self.memory_events {
            event.validate()?;
        }
        if self.live_small.action == LiveSmallAction::AllowRollout && !self.evaluation.passed {
            return Err(AuditTrailError::EvaluationFailedForLiveRollout);
        }
        if self.live_small.action == LiveSmallAction::AllowRollout && !self.promotion.passed {
            return Err(AuditTrailError::PromotionFailedForLiveRollout);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hft_allocator_policy::FactorAllocation;
    use hft_factor_dsl::{FactorAst, FactorTerminal};
    use hft_factor_eval::EvaluationDecision;
    use hft_live_small_supervisor::LiveSmallDecision;
    use hft_promotion_gate::PromotionGateDecision;
    use hft_research_manifest::{ManifestId, ManifestRef};
    use hft_search_protocol::SearchEngineKind;
    use std::collections::BTreeMap;

    fn bundle() -> HarnessAuditBundle {
        HarnessAuditBundle {
            bundle_id: "audit-1".to_string(),
            proposals: vec![ProposalArtifact {
                proposal_id: "proposal-1".to_string(),
                engine: SearchEngineKind::ManualSeed,
                search_manifest_id: ManifestId::new("search-1").unwrap(),
                parent_factor_ids: vec![],
                ast: FactorAst::Terminal(FactorTerminal::Field("oi".to_string())),
                mcts_trace: None,
                parameters: BTreeMap::new(),
                rationale: None,
                created_at: chrono::Utc::now(),
            }],
            evaluation: EvaluationDecision {
                passed: true,
                failures: vec![],
            },
            promotion: PromotionGateDecision {
                passed: true,
                failures: vec![],
            },
            allocator_policy: AllocatorPolicyProposal {
                policy_id: "policy-1".to_string(),
                source_manifest: ManifestRef::new(
                    ManifestId::new("promotion-1").unwrap(),
                    "promotion_manifest",
                )
                .unwrap(),
                allocations: vec![FactorAllocation {
                    factor_id: "factor-1".to_string(),
                    weight: 0.05,
                }],
                requested_symbol_exposure: 0.1,
                live_small_limits: hft_live_small_supervisor::LiveSmallPolicyLimits {
                    max_factor_weight: 0.1,
                    max_symbol_exposure: 0.2,
                    max_account_drawdown: 0.03,
                },
            },
            live_small: LiveSmallDecision {
                action: LiveSmallAction::AllowRollout,
                blockers: vec![],
                rollback_trigger: None,
            },
            memory_events: vec![],
        }
    }

    #[test]
    fn accepts_complete_bundle() {
        assert_eq!(bundle().validate(), Ok(()));
    }

    #[test]
    fn rejects_allowed_rollout_when_evaluation_failed() {
        let mut bundle = bundle();
        bundle.evaluation.passed = false;

        assert_eq!(
            bundle.validate().unwrap_err(),
            AuditTrailError::EvaluationFailedForLiveRollout
        );
    }
}
