//! Live-small rollout supervision contracts.
//!
//! This crate does not execute orders. It converts validated promotion evidence and
//! policy limits into a rollout/rollback decision for the runtime boundary.

use hft_promotion_gate::PromotionGateDecision;
use hft_research_manifest::{LiveRolloutManifest, ManifestError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LiveSmallSupervisorError {
    #[error("rollout manifest is invalid: {0}")]
    InvalidManifest(#[from] ManifestError),
    #[error("requested risk must be finite and non-negative")]
    InvalidRequestedRisk,
    #[error("configured risk cap must be finite and non-negative")]
    InvalidRiskCap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSmallPolicyLimits {
    pub max_factor_weight: f64,
    pub max_symbol_exposure: f64,
    pub max_account_drawdown: f64,
}

impl LiveSmallPolicyLimits {
    pub fn validate(&self) -> Result<(), LiveSmallSupervisorError> {
        for value in [
            self.max_factor_weight,
            self.max_symbol_exposure,
            self.max_account_drawdown,
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(LiveSmallSupervisorError::InvalidRiskCap);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSmallRolloutRequest {
    pub rollout_manifest: LiveRolloutManifest,
    pub promotion_decision: PromotionGateDecision,
    pub policy_limits: LiveSmallPolicyLimits,
    pub requested_factor_weight: f64,
    pub requested_symbol_exposure: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveSmallBlocker {
    InvalidPromotion,
    FactorWeightAboveCap,
    SymbolExposureAboveCap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollbackTrigger {
    RuntimeRejected,
    SentinelStopped,
    DrawdownAbovePolicy,
    DataStale,
    ManualStop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveSmallAction {
    AllowRollout,
    BlockRollout,
    Rollback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSmallDecision {
    pub action: LiveSmallAction,
    pub blockers: Vec<LiveSmallBlocker>,
    pub rollback_trigger: Option<RollbackTrigger>,
}

pub fn supervise_rollout(
    request: &LiveSmallRolloutRequest,
) -> Result<LiveSmallDecision, LiveSmallSupervisorError> {
    request.rollout_manifest.validate()?;
    request.policy_limits.validate()?;
    for value in [
        request.requested_factor_weight,
        request.requested_symbol_exposure,
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(LiveSmallSupervisorError::InvalidRequestedRisk);
        }
    }

    let mut blockers = Vec::new();
    if !request.promotion_decision.passed {
        blockers.push(LiveSmallBlocker::InvalidPromotion);
    }
    if request.requested_factor_weight > request.policy_limits.max_factor_weight {
        blockers.push(LiveSmallBlocker::FactorWeightAboveCap);
    }
    if request.requested_symbol_exposure > request.policy_limits.max_symbol_exposure {
        blockers.push(LiveSmallBlocker::SymbolExposureAboveCap);
    }

    Ok(LiveSmallDecision {
        action: if blockers.is_empty() {
            LiveSmallAction::AllowRollout
        } else {
            LiveSmallAction::BlockRollout
        },
        blockers,
        rollback_trigger: None,
    })
}

pub fn rollback(trigger: RollbackTrigger) -> LiveSmallDecision {
    LiveSmallDecision {
        action: LiveSmallAction::Rollback,
        blockers: vec![],
        rollback_trigger: Some(trigger),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use hft_research_manifest::{ManifestId, ManifestRef};

    fn manifest() -> LiveRolloutManifest {
        LiveRolloutManifest {
            id: ManifestId::new("rollout-1").unwrap(),
            promotion_manifest: ManifestRef::new(
                ManifestId::new("promotion-1").unwrap(),
                "promotion_manifest",
            )
            .unwrap(),
            runtime_config_ref: "runtime-config-demo".to_string(),
            risk_policy_ref: "risk-policy-demo".to_string(),
            started_at: Utc::now(),
            ended_at: None,
            attribution: Default::default(),
            rollback_result: None,
        }
    }

    fn limits() -> LiveSmallPolicyLimits {
        LiveSmallPolicyLimits {
            max_factor_weight: 0.1,
            max_symbol_exposure: 0.2,
            max_account_drawdown: 0.03,
        }
    }

    #[test]
    fn allows_rollout_when_gate_and_limits_pass() {
        let decision = supervise_rollout(&LiveSmallRolloutRequest {
            rollout_manifest: manifest(),
            promotion_decision: PromotionGateDecision {
                passed: true,
                failures: vec![],
            },
            policy_limits: limits(),
            requested_factor_weight: 0.05,
            requested_symbol_exposure: 0.1,
        })
        .unwrap();

        assert_eq!(decision.action, LiveSmallAction::AllowRollout);
        assert!(decision.blockers.is_empty());
    }

    #[test]
    fn blocks_rollout_when_weight_exceeds_policy() {
        let decision = supervise_rollout(&LiveSmallRolloutRequest {
            rollout_manifest: manifest(),
            promotion_decision: PromotionGateDecision {
                passed: true,
                failures: vec![],
            },
            policy_limits: limits(),
            requested_factor_weight: 0.2,
            requested_symbol_exposure: 0.1,
        })
        .unwrap();

        assert_eq!(decision.action, LiveSmallAction::BlockRollout);
        assert!(decision
            .blockers
            .contains(&LiveSmallBlocker::FactorWeightAboveCap));
    }

    #[test]
    fn rollback_is_explicit_and_idempotent() {
        assert_eq!(
            rollback(RollbackTrigger::SentinelStopped),
            rollback(RollbackTrigger::SentinelStopped)
        );
    }
}
