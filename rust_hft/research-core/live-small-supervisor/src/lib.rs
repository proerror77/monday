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
    #[error("runtime command id cannot be empty")]
    EmptyCommandId,
    #[error("runtime command boundary is dry-run only")]
    RuntimeCommandMustBeDryRun,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeCommandKind {
    StageLiveSmall,
    RollbackLiveSmall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveSmallRuntimeCommand {
    pub command_id: String,
    pub kind: RuntimeCommandKind,
    pub rollout_manifest: LiveRolloutManifest,
    pub dry_run: bool,
    pub reason: String,
}

impl LiveSmallRuntimeCommand {
    pub fn validate(&self) -> Result<(), LiveSmallSupervisorError> {
        if self.command_id.trim().is_empty() {
            return Err(LiveSmallSupervisorError::EmptyCommandId);
        }
        if !self.dry_run {
            return Err(LiveSmallSupervisorError::RuntimeCommandMustBeDryRun);
        }
        self.rollout_manifest.validate()?;
        Ok(())
    }
}

pub fn runtime_command_from_decision(
    command_id: impl Into<String>,
    rollout_manifest: LiveRolloutManifest,
    decision: &LiveSmallDecision,
) -> Option<LiveSmallRuntimeCommand> {
    match decision.action {
        LiveSmallAction::AllowRollout => Some(LiveSmallRuntimeCommand {
            command_id: command_id.into(),
            kind: RuntimeCommandKind::StageLiveSmall,
            rollout_manifest,
            dry_run: true,
            reason: "promotion and live-small limits passed".to_string(),
        }),
        LiveSmallAction::Rollback => Some(LiveSmallRuntimeCommand {
            command_id: command_id.into(),
            kind: RuntimeCommandKind::RollbackLiveSmall,
            rollout_manifest,
            dry_run: true,
            reason: format!("rollback requested: {:?}", decision.rollback_trigger),
        }),
        LiveSmallAction::BlockRollout => None,
    }
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

    #[test]
    fn runtime_command_is_dry_run_boundary() {
        let decision = LiveSmallDecision {
            action: LiveSmallAction::AllowRollout,
            blockers: vec![],
            rollback_trigger: None,
        };
        let command = runtime_command_from_decision("cmd-1", manifest(), &decision).unwrap();

        assert_eq!(command.kind, RuntimeCommandKind::StageLiveSmall);
        assert_eq!(command.validate(), Ok(()));
    }

    #[test]
    fn rejects_non_dry_run_runtime_command() {
        let mut command = runtime_command_from_decision(
            "cmd-1",
            manifest(),
            &LiveSmallDecision {
                action: LiveSmallAction::AllowRollout,
                blockers: vec![],
                rollback_trigger: None,
            },
        )
        .unwrap();
        command.dry_run = false;

        assert_eq!(
            command.validate().unwrap_err(),
            LiveSmallSupervisorError::RuntimeCommandMustBeDryRun
        );
    }
}
