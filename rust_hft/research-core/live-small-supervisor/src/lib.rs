//! Live-small rollout supervision contracts.
//!
//! This crate does not execute orders. It converts validated promotion evidence and
//! policy limits into a rollout/rollback decision for the runtime boundary.

use hft_promotion_gate::PromotionGateDecision;
use hft_research_manifest::{LiveRolloutManifest, ManifestError, ManifestRef};
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
    #[error("non-dry-run runtime command requires approval manifest")]
    RuntimeCommandRequiresApproval,
    #[error("live actuation requires non-dry-run runtime command")]
    LiveActuationRequiresArmedCommand,
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
    pub approval_ref: Option<ManifestRef>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeConnectorKind {
    Exchange { venue: String },
    OnChain { network: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeActuationMode {
    DryRun,
    Paper,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeCommandStatus {
    Prepared,
    Submitted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeActuationResult {
    pub command_id: String,
    pub connector: RuntimeConnectorKind,
    pub mode: RuntimeActuationMode,
    pub status: RuntimeCommandStatus,
    pub approval_ref: Option<ManifestRef>,
    pub message: String,
}

impl LiveSmallRuntimeCommand {
    pub fn validate(&self) -> Result<(), LiveSmallSupervisorError> {
        if self.command_id.trim().is_empty() {
            return Err(LiveSmallSupervisorError::EmptyCommandId);
        }
        if !self.dry_run && self.approval_ref.is_none() {
            return Err(LiveSmallSupervisorError::RuntimeCommandRequiresApproval);
        }
        self.rollout_manifest.validate()?;
        if let Some(approval_ref) = &self.approval_ref {
            approval_ref.validate()?;
        }
        Ok(())
    }
}

pub fn arm_runtime_command(
    mut command: LiveSmallRuntimeCommand,
    approval_ref: ManifestRef,
) -> Result<LiveSmallRuntimeCommand, LiveSmallSupervisorError> {
    approval_ref.validate()?;
    command.dry_run = false;
    command.approval_ref = Some(approval_ref);
    command.validate()?;
    Ok(command)
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
            approval_ref: None,
            reason: "promotion and live-small limits passed".to_string(),
        }),
        LiveSmallAction::Rollback => Some(LiveSmallRuntimeCommand {
            command_id: command_id.into(),
            kind: RuntimeCommandKind::RollbackLiveSmall,
            rollout_manifest,
            dry_run: true,
            approval_ref: None,
            reason: format!("rollback requested: {:?}", decision.rollback_trigger),
        }),
        LiveSmallAction::BlockRollout => None,
    }
}

pub fn execute_runtime_command(
    command: &LiveSmallRuntimeCommand,
    connector: RuntimeConnectorKind,
    mode: RuntimeActuationMode,
) -> Result<RuntimeActuationResult, LiveSmallSupervisorError> {
    command.validate()?;
    if mode == RuntimeActuationMode::Live && command.dry_run {
        return Err(LiveSmallSupervisorError::LiveActuationRequiresArmedCommand);
    }

    Ok(RuntimeActuationResult {
        command_id: command.command_id.clone(),
        connector,
        mode,
        status: if command.dry_run {
            RuntimeCommandStatus::Prepared
        } else {
            RuntimeCommandStatus::Submitted
        },
        approval_ref: command.approval_ref.clone(),
        message: match command.kind {
            RuntimeCommandKind::StageLiveSmall => "stage live-small command accepted",
            RuntimeCommandKind::RollbackLiveSmall => "rollback live-small command accepted",
        }
        .to_string(),
    })
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
    fn rejects_non_dry_run_runtime_command_without_approval() {
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
            LiveSmallSupervisorError::RuntimeCommandRequiresApproval
        );
    }

    #[test]
    fn allows_non_dry_run_runtime_command_with_approval() {
        let command = runtime_command_from_decision(
            "cmd-1",
            manifest(),
            &LiveSmallDecision {
                action: LiveSmallAction::AllowRollout,
                blockers: vec![],
                rollback_trigger: None,
            },
        )
        .unwrap();

        let command = arm_runtime_command(
            command,
            ManifestRef::new(ManifestId::new("approval-1").unwrap(), "human_approval").unwrap(),
        )
        .unwrap();

        assert!(!command.dry_run);
        assert!(command.approval_ref.is_some());
    }

    #[test]
    fn live_actuation_requires_armed_command() {
        let command = runtime_command_from_decision(
            "cmd-1",
            manifest(),
            &LiveSmallDecision {
                action: LiveSmallAction::AllowRollout,
                blockers: vec![],
                rollback_trigger: None,
            },
        )
        .unwrap();

        assert_eq!(
            execute_runtime_command(
                &command,
                RuntimeConnectorKind::Exchange {
                    venue: "binance".to_string(),
                },
                RuntimeActuationMode::Live,
            )
            .unwrap_err(),
            LiveSmallSupervisorError::LiveActuationRequiresArmedCommand
        );
    }

    #[test]
    fn submits_armed_command_to_runtime_connector() {
        let command = arm_runtime_command(
            runtime_command_from_decision(
                "cmd-1",
                manifest(),
                &LiveSmallDecision {
                    action: LiveSmallAction::AllowRollout,
                    blockers: vec![],
                    rollback_trigger: None,
                },
            )
            .unwrap(),
            ManifestRef::new(ManifestId::new("approval-1").unwrap(), "human_approval").unwrap(),
        )
        .unwrap();

        let result = execute_runtime_command(
            &command,
            RuntimeConnectorKind::OnChain {
                network: "ethereum-mainnet".to_string(),
            },
            RuntimeActuationMode::Live,
        )
        .unwrap();

        assert_eq!(result.status, RuntimeCommandStatus::Submitted);
        assert_eq!(result.approval_ref, command.approval_ref);
    }
}
