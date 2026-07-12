//! Trust-boundary contracts for bounded Loop Engineer research and runtime deployment.

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hft_factor_dsl::FactorAst;
use hft_research_manifest::{ArtifactRef, ManifestId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const MAX_ONNX_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ONNX_TENSOR_ELEMENTS: usize = 4 * 1024 * 1024;
pub const SEALED_HOLDOUT_EVALUATOR_VERSION: &str = "sealed-holdout-v2";
pub const WALK_FORWARD_EVALUATOR_VERSION: &str = "purged-walk-forward-v2";
pub const ONNX_WALK_FORWARD_EVALUATOR_VERSION: &str = "onnx-purged-walk-forward-v1";
pub const ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION: &str = "onnx-sealed-holdout-v1";
pub const LOB_ONNX_PREPROCESSING_VERSION: &str = "lob-relative-price-log-size-v1";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("{0} cannot be empty")]
    EmptyField(&'static str),
    #[error("search budget must include at least one candidate and one positive limit")]
    InvalidSearchBudget,
    #[error("invalid mission status transition")]
    InvalidMissionTransition,
    #[error("mission completion policy is invalid")]
    InvalidMissionCompletionPolicy,
    #[error("mission terminal reason does not match its status or policy")]
    InvalidMissionTerminalReason,
    #[error("loop run is invalid")]
    InvalidLoopRun,
    #[error("deployment limits must be finite and non-negative")]
    InvalidDeploymentLimit,
    #[error("deployment hashes must be lowercase SHA-256 values")]
    InvalidDeploymentHash,
    #[error("deployment validity window is invalid")]
    InvalidValidityWindow,
    #[error("deployment envelope is not valid yet")]
    NotYetValid,
    #[error("deployment envelope has expired")]
    Expired,
    #[error("deployment payload hash does not match")]
    PayloadHashMismatch,
    #[error("deployment signing key is not trusted")]
    UnknownSigningKey,
    #[error("deployment signature is invalid")]
    InvalidSignature,
    #[error("deployment signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("deployment nonce has already been consumed")]
    NonceReplay,
    #[error("deployment binding does not match the runtime")]
    RuntimeBindingMismatch,
    #[error("deployment exceeds a runtime hard limit")]
    RuntimeLimitExceeded,
    #[error("deployment approval evidence does not match runtime-owned policy")]
    ApprovalEvidenceMismatch,
    #[error("canonical deployment serialization failed")]
    CanonicalSerialization,
    #[error("runtime attribution metrics must be finite")]
    InvalidAttributionMetric,
    #[error("runtime attribution outcome does not match its event kind")]
    InvalidAttributionOutcome,
    #[error("runtime attribution payload hash does not match")]
    AttributionPayloadHashMismatch,
    #[error("runtime attribution signing key is not trusted")]
    UnknownAttributionSigningKey,
    #[error("runtime attribution signature is invalid")]
    InvalidAttributionSignature,
    #[error("runtime attribution signature encoding is invalid")]
    InvalidAttributionSignatureEncoding,
    #[error("search-policy validator scores must be finite")]
    InvalidPolicyScore,
    #[error("candidate artifact is research-only and cannot be promoted")]
    ResearchOnlyArtifact,
    #[error("strategy bundle is invalid")]
    InvalidStrategyBundle,
    #[error("strategy bundle hash does not match its canonical payload")]
    StrategyBundleHashMismatch,
    #[error("promotion record does not match its candidate, evidence, or bundle")]
    PromotionBindingMismatch,
    #[error("evaluation evidence is inconsistent")]
    InvalidEvaluationEvidence,
    #[error("formula evaluator configuration is invalid")]
    InvalidEvaluatorConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MultipleTestingAdjustment {
    GaussianExpectedMaximum,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormulaEvaluatorConfig {
    pub min_validation_rows: usize,
    pub min_trades: usize,
    pub min_fold_mean_return: f64,
    pub min_aggregate_score: f64,
    pub max_drawdown: f64,
    pub multiple_testing_trials: usize,
    pub multiple_testing_adjustment: MultipleTestingAdjustment,
    pub max_abs_signal: f64,
}

impl Default for FormulaEvaluatorConfig {
    fn default() -> Self {
        Self {
            min_validation_rows: 30,
            min_trades: 30,
            min_fold_mean_return: 0.000_001,
            min_aggregate_score: 2.0,
            max_drawdown: 0.20,
            multiple_testing_trials: 1,
            multiple_testing_adjustment: MultipleTestingAdjustment::GaussianExpectedMaximum,
            max_abs_signal: 1.0e12,
        }
    }
}

impl FormulaEvaluatorConfig {
    pub fn for_trials(multiple_testing_trials: usize) -> Result<Self, DomainError> {
        let config = Self {
            multiple_testing_trials,
            ..Self::default()
        };
        config.validate()?;
        Ok(config)
    }

    pub fn for_mission(mission: &ResearchMission) -> Result<Self, DomainError> {
        let multiple_testing_trials = match mission.validator_spec.get("multiple_testing_trials") {
            Some(value) => usize::try_from(
                value
                    .as_u64()
                    .ok_or(DomainError::InvalidEvaluatorConfiguration)?,
            )
            .map_err(|_| DomainError::InvalidEvaluatorConfiguration)?,
            None => mission.search_budget.max_candidates,
        };
        if multiple_testing_trials < mission.search_budget.max_candidates {
            return Err(DomainError::InvalidEvaluatorConfiguration);
        }
        Self::for_trials(multiple_testing_trials)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.min_validation_rows < 2
            || self.min_trades == 0
            || self.min_trades > self.min_validation_rows
            || !self.min_fold_mean_return.is_finite()
            || self.min_fold_mean_return <= 0.0
            || !self.min_aggregate_score.is_finite()
            || self.min_aggregate_score <= 0.0
            || !self.max_drawdown.is_finite()
            || !(0.0..=1.0).contains(&self.max_drawdown)
            || self.max_drawdown <= 0.0
            || self.multiple_testing_trials == 0
            || !self.max_abs_signal.is_finite()
            || self.max_abs_signal <= 0.0
        {
            return Err(DomainError::InvalidEvaluatorConfiguration);
        }
        Ok(())
    }

    pub fn adjusted_score(&self, raw_score: f64) -> Result<f64, DomainError> {
        self.validate()?;
        if !raw_score.is_finite() {
            return Err(DomainError::InvalidEvaluationEvidence);
        }
        let penalty = match self.multiple_testing_adjustment {
            MultipleTestingAdjustment::GaussianExpectedMaximum
                if self.multiple_testing_trials <= 1 =>
            {
                0.0
            }
            MultipleTestingAdjustment::GaussianExpectedMaximum => {
                (2.0 * (self.multiple_testing_trials as f64).ln()).sqrt()
            }
        };
        Ok(raw_score - penalty)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FoldEvaluationMetrics {
    pub fold_index: usize,
    pub row_count: usize,
    pub trade_count: usize,
    pub mean_net_return: f64,
    pub cumulative_net_return: f64,
    pub max_drawdown: f64,
    pub raw_score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    pub row_count: usize,
    pub trade_count: usize,
    pub mean_net_return: f64,
    pub cumulative_net_return: f64,
    pub max_drawdown: f64,
    pub raw_score: f64,
    pub adjusted_score: f64,
    pub folds: Vec<FoldEvaluationMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub passed: bool,
    pub score: f64,
    pub failure_reasons: Vec<String>,
    pub evaluator_version: String,
    pub evaluator_config: serde_json::Value,
    pub metrics: EvaluationMetrics,
}

impl CandidateEvaluation {
    pub fn validate(&self) -> Result<(), DomainError> {
        let finite_metrics = [
            self.metrics.mean_net_return,
            self.metrics.cumulative_net_return,
            self.metrics.max_drawdown,
            self.metrics.raw_score,
            self.metrics.adjusted_score,
        ]
        .iter()
        .all(|value| value.is_finite())
            && self.metrics.folds.iter().all(|fold| {
                [
                    fold.mean_net_return,
                    fold.cumulative_net_return,
                    fold.max_drawdown,
                    fold.raw_score,
                ]
                .iter()
                .all(|value| value.is_finite())
                    && fold.fold_index > 0
                    && fold.row_count > 0
                    && fold.trade_count <= fold.row_count
                    && fold.max_drawdown >= 0.0
            });
        let row_count: usize = self.metrics.folds.iter().map(|fold| fold.row_count).sum();
        let trade_count: usize = self.metrics.folds.iter().map(|fold| fold.trade_count).sum();
        let cumulative_net_return = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.cumulative_net_return)
            .sum();
        let weighted_mean = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.mean_net_return * fold.row_count as f64)
            .sum::<f64>()
            / row_count.max(1) as f64;
        let maximum_drawdown = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.max_drawdown)
            .fold(0.0_f64, f64::max);
        let raw_score = self
            .metrics
            .folds
            .iter()
            .map(|fold| fold.raw_score)
            .sum::<f64>()
            / self.metrics.folds.len().max(1) as f64;
        if self.evaluator_version.trim().is_empty()
            || !self.evaluator_config.is_object()
            || self.metrics.row_count == 0
            || self.metrics.folds.is_empty()
            || !self.score.is_finite()
            || !finite_metrics
            || self.metrics.max_drawdown < 0.0
            || self.score.to_bits() != self.metrics.adjusted_score.to_bits()
            || self.passed != self.failure_reasons.is_empty()
            || row_count != self.metrics.row_count
            || trade_count != self.metrics.trade_count
            || !approximately_equal(cumulative_net_return, self.metrics.cumulative_net_return)
            || !approximately_equal(weighted_mean, self.metrics.mean_net_return)
            || !approximately_equal(maximum_drawdown, self.metrics.max_drawdown)
            || !approximately_equal(raw_score, self.metrics.raw_score)
        {
            return Err(DomainError::InvalidEvaluationEvidence);
        }
        if matches!(
            self.evaluator_version.as_str(),
            WALK_FORWARD_EVALUATOR_VERSION
                | SEALED_HOLDOUT_EVALUATOR_VERSION
                | ONNX_WALK_FORWARD_EVALUATOR_VERSION
                | ONNX_SEALED_HOLDOUT_EVALUATOR_VERSION
        ) {
            let config = self.formula_config()?;
            let policy_passed = self.metrics.folds.iter().all(|fold| {
                fold.row_count >= config.min_validation_rows
                    && fold.trade_count >= config.min_trades
                    && fold.mean_net_return > config.min_fold_mean_return
                    && fold.max_drawdown <= config.max_drawdown
            }) && self.metrics.adjusted_score >= config.min_aggregate_score;
            if self.passed != policy_passed
                || !approximately_equal(
                    config.adjusted_score(self.metrics.raw_score)?,
                    self.metrics.adjusted_score,
                )
            {
                return Err(DomainError::InvalidEvaluationEvidence);
            }
        }
        Ok(())
    }

    pub fn formula_config(&self) -> Result<FormulaEvaluatorConfig, DomainError> {
        let config: FormulaEvaluatorConfig = serde_json::from_value(self.evaluator_config.clone())
            .map_err(|_| DomainError::InvalidEvaluatorConfiguration)?;
        config.validate()?;
        Ok(config)
    }
}

fn approximately_equal(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= f64::EPSILON * 16.0 * scale
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidatorMode {
    MissionValidator,
    ArchitectArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionStatus {
    Pending,
    Running,
    Paused,
    Completed,
    BudgetExhausted,
    Failed,
}

impl MissionStatus {
    pub fn can_transition_to(&self, next: &Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Running)
                | (Self::Running, Self::Paused)
                | (Self::Paused, Self::Running)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionCompletionPolicy {
    pub min_kept_candidates: usize,
}

impl Default for MissionCompletionPolicy {
    fn default() -> Self {
        Self {
            min_kept_candidates: 1,
        }
    }
}

impl MissionCompletionPolicy {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.min_kept_candidates == 0 {
            return Err(DomainError::InvalidMissionCompletionPolicy);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchBudgetLimit {
    Candidates,
    Expansions,
    Tokens,
    Time,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionTerminalReason {
    CompletionPolicySatisfied {
        kept_candidates: usize,
    },
    SearchBudgetExhausted {
        exhausted_limits: Vec<SearchBudgetLimit>,
    },
    Failed {
        code: String,
    },
}

impl MissionTerminalReason {
    pub fn status(&self) -> MissionStatus {
        match self {
            Self::CompletionPolicySatisfied { .. } => MissionStatus::Completed,
            Self::SearchBudgetExhausted { .. } => MissionStatus::BudgetExhausted,
            Self::Failed { .. } => MissionStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBudget {
    pub max_candidates: usize,
    pub max_expansions: u64,
    pub max_tokens: u64,
    pub max_seconds: u64,
}

impl SearchBudget {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.max_candidates == 0
            || (self.max_expansions == 0 && self.max_tokens == 0 && self.max_seconds == 0)
        {
            return Err(DomainError::InvalidSearchBudget);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchMission {
    pub mission_id: String,
    pub objective: String,
    pub hypothesis_scope: String,
    pub mutable_scope: Vec<String>,
    pub dataset_manifest_id: ManifestId,
    pub baseline_artifact_id: Option<String>,
    pub validation_mode: ValidatorMode,
    pub validator_spec: serde_json::Value,
    pub search_budget: SearchBudget,
    #[serde(default)]
    pub completion_policy: MissionCompletionPolicy,
    pub prompt_snapshot_id: Option<String>,
    pub search_policy_snapshot_id: String,
    pub status: MissionStatus,
    #[serde(default)]
    pub terminal_reason: Option<MissionTerminalReason>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ResearchMission {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("mission_id", &self.mission_id)?;
        require_text("objective", &self.objective)?;
        require_text("hypothesis_scope", &self.hypothesis_scope)?;
        require_text("search_policy_snapshot_id", &self.search_policy_snapshot_id)?;
        if self.mutable_scope.is_empty()
            || self.mutable_scope.iter().any(|item| item.trim().is_empty())
        {
            return Err(DomainError::EmptyField("mutable_scope"));
        }
        self.search_budget.validate()?;
        self.completion_policy.validate()?;
        self.validate_terminal_state()
    }

    pub fn transition_to(
        &mut self,
        next: MissionStatus,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if !self.status.can_transition_to(&next) {
            return Err(DomainError::InvalidMissionTransition);
        }
        self.status = next;
        self.updated_at = at;
        Ok(())
    }

    pub fn finish(
        &mut self,
        reason: MissionTerminalReason,
        at: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if self.status != MissionStatus::Running {
            return Err(DomainError::InvalidMissionTransition);
        }
        validate_mission_terminal_reason(&self.completion_policy, &reason)?;
        self.status = reason.status();
        self.terminal_reason = Some(reason);
        self.updated_at = at;
        Ok(())
    }

    fn validate_terminal_state(&self) -> Result<(), DomainError> {
        match (&self.status, &self.terminal_reason) {
            (MissionStatus::Pending | MissionStatus::Running | MissionStatus::Paused, None) => {
                Ok(())
            }
            (status, Some(reason)) if *status == reason.status() => {
                validate_mission_terminal_reason(&self.completion_policy, reason)
            }
            _ => Err(DomainError::InvalidMissionTerminalReason),
        }
    }
}

fn validate_mission_terminal_reason(
    policy: &MissionCompletionPolicy,
    reason: &MissionTerminalReason,
) -> Result<(), DomainError> {
    match reason {
        MissionTerminalReason::CompletionPolicySatisfied { kept_candidates }
            if *kept_candidates >= policy.min_kept_candidates =>
        {
            Ok(())
        }
        MissionTerminalReason::SearchBudgetExhausted { exhausted_limits }
            if !exhausted_limits.is_empty() =>
        {
            Ok(())
        }
        MissionTerminalReason::Failed { code } if !code.trim().is_empty() => Ok(()),
        _ => Err(DomainError::InvalidMissionTerminalReason),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopTargetStage {
    Researching,
    WalkForwardKept,
    HoldoutPassed,
    PaperHealthy,
    ShadowHealthy,
    LiveSmallEligible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopCompletionPolicy {
    pub target_stage: LoopTargetStage,
    pub max_research_missions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopRunStatus {
    Pending,
    Running,
    Paused,
    Completed,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStage {
    Researching,
    WalkForwardKept,
    HoldoutPassed,
    PaperHealthy,
    ShadowHealthy,
    LiveSmallEligible,
    Learning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStageStatus {
    Completed,
    Paused,
    BudgetExhausted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopStageRecord {
    pub record_id: String,
    pub mission_id: String,
    pub stage: LoopStage,
    pub status: LoopStageStatus,
    pub reason: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopStopReason {
    TargetStageReached {
        mission_id: String,
        stage: LoopTargetStage,
    },
    AwaitingEvidence {
        mission_id: String,
        stage: LoopTargetStage,
    },
    MissionPaused {
        mission_id: String,
    },
    ResearchBudgetExhausted {
        mission_id: String,
    },
    MissionFailed {
        mission_id: String,
        code: String,
    },
    MissionLimitReached {
        attempted: usize,
    },
}

impl LoopStopReason {
    pub fn status(&self) -> LoopRunStatus {
        match self {
            Self::TargetStageReached { .. } => LoopRunStatus::Completed,
            Self::AwaitingEvidence { .. } | Self::MissionPaused { .. } => LoopRunStatus::Paused,
            Self::ResearchBudgetExhausted { .. } | Self::MissionLimitReached { .. } => {
                LoopRunStatus::BudgetExhausted
            }
            Self::MissionFailed { .. } => LoopRunStatus::Failed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRun {
    pub loop_run_id: String,
    pub root_mission_id: String,
    pub completion_policy: LoopCompletionPolicy,
    pub child_mission_ids: Vec<String>,
    pub stage_records: Vec<LoopStageRecord>,
    pub status: LoopRunStatus,
    pub stop_reason: Option<LoopStopReason>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl LoopRun {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("loop_run_id", &self.loop_run_id)?;
        require_text("loop root_mission_id", &self.root_mission_id)?;
        if self.completion_policy.max_research_missions == 0
            || self
                .child_mission_ids
                .iter()
                .any(|mission_id| mission_id.trim().is_empty())
            || self
                .child_mission_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.child_mission_ids.len()
        {
            return Err(DomainError::InvalidLoopRun);
        }
        let mut record_ids = std::collections::BTreeSet::new();
        for record in &self.stage_records {
            if record.record_id.trim().is_empty()
                || record.mission_id.trim().is_empty()
                || record.reason.trim().is_empty()
                || !record_ids.insert(&record.record_id)
                || (record.mission_id != self.root_mission_id
                    && !self.child_mission_ids.contains(&record.mission_id))
            {
                return Err(DomainError::InvalidLoopRun);
            }
        }
        match (&self.status, &self.stop_reason) {
            (LoopRunStatus::Pending | LoopRunStatus::Running, None) => Ok(()),
            (status, Some(reason)) if *status == reason.status() => Ok(()),
            _ => Err(DomainError::InvalidLoopRun),
        }
    }

    pub fn start(&mut self, at: DateTime<Utc>) -> Result<(), DomainError> {
        if !matches!(self.status, LoopRunStatus::Pending | LoopRunStatus::Paused) {
            return Err(DomainError::InvalidLoopRun);
        }
        self.status = LoopRunStatus::Running;
        self.stop_reason = None;
        self.updated_at = at;
        self.validate()
    }

    pub fn add_child_mission(&mut self, mission_id: String) -> Result<(), DomainError> {
        require_text("loop child mission_id", &mission_id)?;
        if mission_id == self.root_mission_id {
            return Err(DomainError::InvalidLoopRun);
        }
        if !self.child_mission_ids.contains(&mission_id) {
            self.child_mission_ids.push(mission_id);
            if let Err(error) = self.validate() {
                self.child_mission_ids.pop();
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn append_stage(&mut self, record: LoopStageRecord) -> Result<(), DomainError> {
        if self.status != LoopRunStatus::Running {
            return Err(DomainError::InvalidLoopRun);
        }
        self.stage_records.push(record);
        if let Err(error) = self.validate() {
            self.stage_records.pop();
            return Err(error);
        }
        Ok(())
    }

    pub fn stop(&mut self, reason: LoopStopReason, at: DateTime<Utc>) -> Result<(), DomainError> {
        if self.status != LoopRunStatus::Running {
            return Err(DomainError::InvalidLoopRun);
        }
        self.status = reason.status();
        self.stop_reason = Some(reason);
        self.updated_at = at;
        self.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineKind {
    GeneticProgramming,
    Mcts,
    BayesianOptimizer,
    LlmProposer,
    OfflineReinforcementLearning,
    ManualSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributionMode {
    Paper,
    Shadow,
    LiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttributionOutcome {
    Activated,
    Healthy,
    Decayed,
    RolledBack,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AttributionKind {
    #[default]
    Activation,
    Fill,
    Reject,
    Cancel,
    PortfolioSnapshot,
    StreamGap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeAttributionEvent {
    pub event_id: String,
    pub deployment_id: String,
    pub asset_revision_id: String,
    pub mission_id: Option<String>,
    pub mode: AttributionMode,
    pub outcome: AttributionOutcome,
    #[serde(default)]
    pub kind: AttributionKind,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub order_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    pub metrics: BTreeMap<String, f64>,
    pub reason: Option<String>,
    pub observed_at: DateTime<Utc>,
}

impl RuntimeAttributionEvent {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("attribution event_id", &self.event_id)?;
        require_text("attribution deployment_id", &self.deployment_id)?;
        require_text("attribution asset_revision_id", &self.asset_revision_id)?;
        if self.metrics.values().any(|value| !value.is_finite()) {
            return Err(DomainError::InvalidAttributionMetric);
        }
        for value in [
            self.strategy_id.as_deref(),
            self.order_id.as_deref(),
            self.account_id.as_deref(),
            self.venue.as_deref(),
            self.symbol.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.trim().is_empty() {
                return Err(DomainError::EmptyField("attribution scope"));
            }
        }
        match self.kind {
            AttributionKind::Fill | AttributionKind::Reject | AttributionKind::Cancel
                if self.strategy_id.is_none()
                    || self.order_id.is_none()
                    || self.account_id.is_none()
                    || self.venue.is_none()
                    || self.symbol.is_none() =>
            {
                return Err(DomainError::EmptyField("attribution order scope"));
            }
            AttributionKind::PortfolioSnapshot
                if self.strategy_id.is_none()
                    || self.account_id.is_none()
                    || self.venue.is_none() =>
            {
                return Err(DomainError::EmptyField("attribution portfolio scope"));
            }
            AttributionKind::StreamGap if self.reason.as_deref().is_none_or(str::is_empty) => {
                return Err(DomainError::EmptyField("attribution stream gap reason"));
            }
            _ => {}
        }
        match (&self.kind, &self.outcome) {
            (AttributionKind::Fill | AttributionKind::Cancel, AttributionOutcome::Healthy)
            | (AttributionKind::Reject | AttributionKind::StreamGap, AttributionOutcome::Failed)
            | (
                AttributionKind::PortfolioSnapshot,
                AttributionOutcome::Healthy
                | AttributionOutcome::Decayed
                | AttributionOutcome::RolledBack
                | AttributionOutcome::Failed,
            )
            | (AttributionKind::Activation, _) => Ok(()),
            _ => Err(DomainError::InvalidAttributionOutcome),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedRuntimeAttributionEvent {
    pub event: RuntimeAttributionEvent,
    pub key_id: String,
    pub content_hash: String,
    pub signature_hex: String,
}

pub fn sign_runtime_attribution_event(
    event: RuntimeAttributionEvent,
    key_id: impl Into<String>,
    signing_key: &SigningKey,
) -> Result<SignedRuntimeAttributionEvent, DomainError> {
    event.validate()?;
    let key_id = key_id.into();
    require_text("runtime attribution key_id", &key_id)?;
    let content_hash = canonical_json_hash(&event)?;
    let signature = signing_key.sign(content_hash.as_bytes());
    Ok(SignedRuntimeAttributionEvent {
        event,
        key_id,
        content_hash,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_runtime_attribution_event(
    signed: &SignedRuntimeAttributionEvent,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
) -> Result<RuntimeAttributionEvent, DomainError> {
    signed.event.validate()?;
    require_text("runtime attribution key_id", &signed.key_id)?;
    let expected_hash = canonical_json_hash(&signed.event)?;
    if expected_hash != signed.content_hash {
        return Err(DomainError::AttributionPayloadHashMismatch);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(DomainError::UnknownAttributionSigningKey)?;
    let signature_bytes = hex::decode(&signed.signature_hex)
        .map_err(|_| DomainError::InvalidAttributionSignatureEncoding)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| DomainError::InvalidAttributionSignatureEncoding)?;
    key.verify(signed.content_hash.as_bytes(), &signature)
        .map_err(|_| DomainError::InvalidAttributionSignature)?;
    Ok(signed.event.clone())
}

pub fn runtime_stage_is_healthy(
    events: &[RuntimeAttributionEvent],
    candidate_id: &str,
    mode: AttributionMode,
) -> bool {
    #[derive(Default)]
    struct DeploymentHealth {
        activated: bool,
        unhealthy: bool,
        strategies: BTreeMap<String, StrategyHealth>,
    }

    #[derive(Default)]
    struct StrategyHealth {
        healthy_snapshot: bool,
        fill: bool,
    }

    let mut by_deployment = BTreeMap::<String, DeploymentHealth>::new();
    for event in events
        .iter()
        .filter(|event| event.asset_revision_id == candidate_id && event.mode == mode)
    {
        let health = by_deployment
            .entry(event.deployment_id.clone())
            .or_default();
        if matches!(
            event.outcome,
            AttributionOutcome::Failed
                | AttributionOutcome::Decayed
                | AttributionOutcome::RolledBack
        ) || event.kind == AttributionKind::StreamGap
        {
            health.unhealthy = true;
            continue;
        }
        if event.kind == AttributionKind::Activation
            && event.outcome == AttributionOutcome::Activated
        {
            health.activated = true;
        }
        if event.outcome == AttributionOutcome::Healthy
            && event.kind == AttributionKind::PortfolioSnapshot
        {
            if let Some(strategy_id) = event.strategy_id.as_ref() {
                health
                    .strategies
                    .entry(strategy_id.clone())
                    .or_default()
                    .healthy_snapshot = true;
            }
        }
        if event.kind == AttributionKind::Fill {
            if let Some(strategy_id) = event.strategy_id.as_ref() {
                health
                    .strategies
                    .entry(strategy_id.clone())
                    .or_default()
                    .fill = true;
            }
        }
    }

    by_deployment.values().any(|health| {
        !health.unhealthy
            && health.activated
            && health
                .strategies
                .values()
                .any(|strategy| strategy.healthy_snapshot && strategy.fill)
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningDirective {
    pub directive_id: String,
    pub mission_id: String,
    pub failure_class: String,
    pub evidence_iteration_ids: Vec<String>,
    #[serde(default)]
    pub runtime_evidence_event_ids: Vec<String>,
    pub follow_up_mission_id: String,
    pub search_policy_revision_id: String,
    pub created_at: DateTime<Utc>,
}

impl LearningDirective {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("directive_id", &self.directive_id)?;
        require_text("directive mission_id", &self.mission_id)?;
        require_text("failure_class", &self.failure_class)?;
        require_text("follow_up_mission_id", &self.follow_up_mission_id)?;
        require_text(
            "directive search_policy_revision_id",
            &self.search_policy_revision_id,
        )?;
        if self.evidence_iteration_ids.is_empty() && self.runtime_evidence_event_ids.is_empty() {
            return Err(DomainError::EmptyField("directive evidence"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchPolicyRevision {
    pub revision_id: String,
    pub parent_revision_id: Option<String>,
    pub policy: serde_json::Value,
    pub evidence_event_ids: Vec<String>,
    pub validator_score: f64,
    pub adopted: bool,
    pub rollback_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SearchPolicyRevision {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("search policy revision_id", &self.revision_id)?;
        if !self.validator_score.is_finite() {
            return Err(DomainError::InvalidPolicyScore);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TensorSpec {
    pub name: String,
    pub element_type: TensorElementType,
    pub dimensions: Vec<Option<usize>>,
}

impl TensorSpec {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("tensor name", &self.name)?;
        if self.dimensions.is_empty() || self.dimensions.contains(&Some(0)) {
            return Err(DomainError::InvalidStrategyBundle);
        }
        let mut known_elements = 1_usize;
        for dimension in self.dimensions.iter().flatten() {
            known_elements = known_elements
                .checked_mul(*dimension)
                .ok_or(DomainError::InvalidStrategyBundle)?;
            if known_elements > MAX_ONNX_TENSOR_ELEMENTS {
                return Err(DomainError::InvalidStrategyBundle);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TensorElementType {
    Float32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OnnxModelCandidate {
    pub artifact: ArtifactRef,
    pub byte_len: u64,
    pub opset: u32,
    pub preprocessing_version: String,
    pub inputs: Vec<TensorSpec>,
    pub output: TensorSpec,
}

impl OnnxModelCandidate {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("onnx artifact uri", &self.artifact.uri)?;
        if self.artifact.content_type != "application/onnx"
            || self.byte_len == 0
            || self.byte_len > MAX_ONNX_ARTIFACT_BYTES
            || self.opset == 0
            || self.preprocessing_version != LOB_ONNX_PREPROCESSING_VERSION
            || self.inputs.is_empty()
        {
            return Err(DomainError::InvalidStrategyBundle);
        }
        let checksum = self
            .artifact
            .checksum
            .as_deref()
            .ok_or(DomainError::InvalidStrategyBundle)?;
        validate_sha256(checksum)?;
        self.inputs.iter().try_for_each(TensorSpec::validate)?;
        self.output.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CandidateArtifact {
    Formula(FactorAst),
    OnnxModel(OnnxModelCandidate),
    Program(serde_json::Value),
    ModelConfig(serde_json::Value),
    ModelArtifact(ArtifactRef),
    Ensemble(serde_json::Value),
    AllocatorPolicy(serde_json::Value),
}

impl CandidateArtifact {
    pub fn to_governed_strategy_bundle_artifact(
        &self,
    ) -> Result<StrategyBundleArtifact, DomainError> {
        match self {
            Self::Formula(ast) => {
                ast.validate()
                    .map_err(|_| DomainError::InvalidStrategyBundle)?;
                Ok(StrategyBundleArtifact::Formula { ast: ast.clone() })
            }
            Self::OnnxModel(model) => {
                model.validate()?;
                Ok(StrategyBundleArtifact::Onnx {
                    model: model.clone(),
                })
            }
            Self::Program(_)
            | Self::ModelConfig(_)
            | Self::ModelArtifact(_)
            | Self::Ensemble(_)
            | Self::AllocatorPolicy(_) => Err(DomainError::ResearchOnlyArtifact),
        }
    }
}

/// Runtime-loadable artifact schema. Governed evaluator v2 currently produces Formula only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StrategyBundleArtifact {
    Formula { ast: FactorAst },
    Onnx { model: OnnxModelCandidate },
}

impl StrategyBundleArtifact {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Formula { ast } => ast
                .validate()
                .map_err(|_| DomainError::InvalidStrategyBundle),
            Self::Onnx { model } => model.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrategyBundle {
    pub bundle_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub dataset_manifest_id: ManifestId,
    pub evaluator_version: String,
    pub evaluator_config_hash: String,
    pub evaluation_metrics_hash: String,
    pub sealed_evaluation_hash: String,
    pub artifact: StrategyBundleArtifact,
    pub bundle_hash: String,
    pub created_at: DateTime<Utc>,
}

impl StrategyBundle {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        bundle_id: String,
        candidate_id: String,
        candidate_content_hash: String,
        dataset_manifest_id: ManifestId,
        evaluator_version: String,
        evaluator_config_hash: String,
        evaluation_metrics_hash: String,
        sealed_evaluation_hash: String,
        artifact: StrategyBundleArtifact,
        created_at: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let mut bundle = Self {
            bundle_id,
            candidate_id,
            candidate_content_hash,
            dataset_manifest_id,
            evaluator_version,
            evaluator_config_hash,
            evaluation_metrics_hash,
            sealed_evaluation_hash,
            artifact,
            bundle_hash: String::new(),
            created_at,
        };
        bundle.validate_fields()?;
        bundle.bundle_hash = bundle.calculated_hash()?;
        Ok(bundle)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.validate_fields()?;
        validate_sha256(&self.bundle_hash)?;
        if self.bundle_hash != self.calculated_hash()? {
            return Err(DomainError::StrategyBundleHashMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        require_text("bundle_id", &self.bundle_id)?;
        require_text("bundle candidate_id", &self.candidate_id)?;
        require_text("evaluator_version", &self.evaluator_version)?;
        validate_sha256(&self.candidate_content_hash)?;
        validate_sha256(&self.evaluator_config_hash)?;
        validate_sha256(&self.evaluation_metrics_hash)?;
        validate_sha256(&self.sealed_evaluation_hash)?;
        self.dataset_manifest_id
            .validate()
            .map_err(|_| DomainError::InvalidStrategyBundle)?;
        self.artifact.validate()
    }

    pub fn calculated_hash(&self) -> Result<String, DomainError> {
        #[derive(Serialize)]
        struct SignableBundle<'a> {
            candidate_content_hash: &'a str,
            dataset_manifest_id: &'a ManifestId,
            evaluator_version: &'a str,
            evaluator_config_hash: &'a str,
            evaluation_metrics_hash: &'a str,
            sealed_evaluation_hash: &'a str,
            artifact: &'a StrategyBundleArtifact,
        }
        canonical_json_hash(&SignableBundle {
            candidate_content_hash: &self.candidate_content_hash,
            dataset_manifest_id: &self.dataset_manifest_id,
            evaluator_version: &self.evaluator_version,
            evaluator_config_hash: &self.evaluator_config_hash,
            evaluation_metrics_hash: &self.evaluation_metrics_hash,
            sealed_evaluation_hash: &self.sealed_evaluation_hash,
            artifact: &self.artifact,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionRecord {
    pub promotion_id: String,
    pub mission_id: String,
    pub candidate_id: String,
    pub candidate_content_hash: String,
    pub dataset_manifest_id: ManifestId,
    pub evaluator_version: String,
    pub evaluator_config_hash: String,
    pub evaluation_metrics_hash: String,
    pub sealed_evaluation_id: String,
    pub sealed_evaluation_hash: String,
    pub bundle_id: String,
    pub bundle_hash: String,
    pub created_at: DateTime<Utc>,
}

impl PromotionRecord {
    pub fn validate(&self, bundle: &StrategyBundle) -> Result<(), DomainError> {
        for (name, value) in [
            ("promotion_id", self.promotion_id.as_str()),
            ("promotion mission_id", self.mission_id.as_str()),
            ("promotion candidate_id", self.candidate_id.as_str()),
            ("sealed_evaluation_id", self.sealed_evaluation_id.as_str()),
            ("promotion bundle_id", self.bundle_id.as_str()),
            (
                "promotion evaluator_version",
                self.evaluator_version.as_str(),
            ),
        ] {
            require_text(name, value)?;
        }
        validate_sha256(&self.candidate_content_hash)?;
        validate_sha256(&self.evaluator_config_hash)?;
        validate_sha256(&self.evaluation_metrics_hash)?;
        validate_sha256(&self.sealed_evaluation_hash)?;
        validate_sha256(&self.bundle_hash)?;
        if self.candidate_id != bundle.candidate_id
            || self.candidate_content_hash != bundle.candidate_content_hash
            || self.dataset_manifest_id != bundle.dataset_manifest_id
            || self.evaluator_version != bundle.evaluator_version
            || self.evaluator_config_hash != bundle.evaluator_config_hash
            || self.evaluation_metrics_hash != bundle.evaluation_metrics_hash
            || self.sealed_evaluation_hash != bundle.sealed_evaluation_hash
            || self.bundle_id != bundle.bundle_id
            || self.bundle_hash != bundle.bundle_hash
        {
            return Err(DomainError::PromotionBindingMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum IterationVerdict {
    Keep,
    Discard,
    Crash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchIteration {
    pub iteration_id: String,
    pub mission_id: String,
    pub parent_candidate_ids: Vec<String>,
    pub engine: EngineKind,
    pub hypothesis: String,
    pub candidate_artifact_id: Option<String>,
    pub evaluation_artifact_id: Option<String>,
    pub budget_usage: SearchBudgetUsage,
    pub verdict: IterationVerdict,
    pub failure_class: Option<String>,
    pub failure_explanation: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl ResearchIteration {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("iteration_id", &self.iteration_id)?;
        require_text("mission_id", &self.mission_id)?;
        require_text("hypothesis", &self.hypothesis)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SearchBudgetUsage {
    pub candidates: usize,
    pub expansions: u64,
    pub tokens: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllowedIntentType {
    LoadFactor,
    LoadModel,
    LoadAllocatorPolicy,
    StartPaper,
    StartShadow,
    StartLiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalClass {
    Paper,
    Shadow,
    HumanApprovedLiveSmall,
    SameClassAutoLiveSmall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeApprovalEvidence {
    pub approval_id: String,
    pub approval_class: ApprovalClass,
    pub subject_id: String,
    pub scope_hash: String,
    pub signer_id: String,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl RuntimeApprovalEvidence {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("runtime approval_id", &self.approval_id)?;
        require_text("runtime approval subject_id", &self.subject_id)?;
        require_text("runtime approval signer_id", &self.signer_id)?;
        validate_deployment_hash(&self.scope_hash)?;
        if self.approval_class == ApprovalClass::SameClassAutoLiveSmall
            || self.valid_from >= self.expires_at
            || self
                .revoked_at
                .is_some_and(|revoked_at| revoked_at < self.valid_from)
        {
            return Err(DomainError::ApprovalEvidenceMismatch);
        }
        Ok(())
    }

    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.valid_from
            && now < self.expires_at
            && self.revoked_at.is_none_or(|revoked_at| now < revoked_at)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSmallEligibilityEvidence {
    pub candidate_id: String,
    pub bundle_id: String,
    pub reconciliation_evidence_sha256: String,
    pub reduce_only_exit_evidence_sha256: String,
    pub shadow_soak_evidence_sha256: String,
}

impl LiveSmallEligibilityEvidence {
    pub fn validate(&self) -> Result<(), DomainError> {
        require_text("eligibility candidate_id", &self.candidate_id)?;
        require_text("eligibility bundle_id", &self.bundle_id)?;
        for hash in [
            &self.reconciliation_evidence_sha256,
            &self.reduce_only_exit_evidence_sha256,
            &self.shadow_soak_evidence_sha256,
        ] {
            validate_sha256(hash)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeploymentEnvelope {
    pub deployment_id: String,
    pub asset_revision_id: String,
    #[serde(default)]
    pub promotion_id: String,
    pub promotion_manifest_hash: String,
    #[serde(default)]
    pub bundle_id: String,
    #[serde(default)]
    pub bundle_hash: String,
    pub runtime_config_hash: String,
    pub risk_policy_hash: String,
    pub account_id: String,
    pub venue: String,
    pub instruments: Vec<String>,
    pub allowed_intent_types: Vec<AllowedIntentType>,
    pub max_notional: f64,
    pub max_symbol_exposure: f64,
    pub max_order_size: f64,
    pub max_slippage_bps: f64,
    pub valid_from: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub approval_class: ApprovalClass,
    pub approval_signatures: Vec<String>,
    pub payload_hash: String,
}

impl DeploymentEnvelope {
    pub fn validate(&self) -> Result<(), DomainError> {
        for (name, value) in [
            ("deployment_id", self.deployment_id.as_str()),
            ("asset_revision_id", self.asset_revision_id.as_str()),
            ("promotion_id", self.promotion_id.as_str()),
            (
                "promotion_manifest_hash",
                self.promotion_manifest_hash.as_str(),
            ),
            ("bundle_id", self.bundle_id.as_str()),
            ("bundle_hash", self.bundle_hash.as_str()),
            ("runtime_config_hash", self.runtime_config_hash.as_str()),
            ("risk_policy_hash", self.risk_policy_hash.as_str()),
            ("account_id", self.account_id.as_str()),
            ("venue", self.venue.as_str()),
            ("nonce", self.nonce.as_str()),
        ] {
            require_text(name, value)?;
        }
        for value in [
            self.promotion_manifest_hash.as_str(),
            self.bundle_hash.as_str(),
            self.runtime_config_hash.as_str(),
            self.risk_policy_hash.as_str(),
        ] {
            validate_deployment_hash(value)?;
        }
        if self.instruments.is_empty() || self.instruments.iter().any(|item| item.trim().is_empty())
        {
            return Err(DomainError::EmptyField("instruments"));
        }
        if self.allowed_intent_types.is_empty() {
            return Err(DomainError::EmptyField("allowed_intent_types"));
        }
        if self.approval_signatures.is_empty()
            || self
                .approval_signatures
                .iter()
                .any(|item| item.trim().is_empty())
        {
            return Err(DomainError::EmptyField("approval_signatures"));
        }
        if [
            self.max_notional,
            self.max_symbol_exposure,
            self.max_order_size,
            self.max_slippage_bps,
        ]
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(DomainError::InvalidDeploymentLimit);
        }
        if self.valid_from >= self.expires_at {
            return Err(DomainError::InvalidValidityWindow);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedDeploymentEnvelope {
    pub envelope: DeploymentEnvelope,
    pub key_id: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedDeploymentEnvelope(pub DeploymentEnvelope);

#[derive(Debug, Clone)]
pub struct RuntimeEnvelopePolicy {
    pub account_id: String,
    pub venue: String,
    pub allowed_instruments: Vec<String>,
    pub allowed_intent_types: Vec<AllowedIntentType>,
    pub runtime_config_hash: String,
    pub risk_policy_hash: String,
    pub max_notional: f64,
    pub max_symbol_exposure: f64,
    pub max_order_size: f64,
    pub max_slippage_bps: f64,
    pub approvals: Vec<RuntimeApprovalEvidence>,
}

pub fn sign_envelope(
    mut envelope: DeploymentEnvelope,
    key_id: impl Into<String>,
    signing_key: &SigningKey,
) -> Result<SignedDeploymentEnvelope, DomainError> {
    envelope.validate()?;
    let key_id = key_id.into();
    require_text("key_id", &key_id)?;
    let bytes = canonical_payload(&envelope)?;
    envelope.payload_hash = hex::encode(Sha256::digest(&bytes));
    let signature = signing_key.sign(envelope.payload_hash.as_bytes());
    Ok(SignedDeploymentEnvelope {
        envelope,
        key_id,
        signature_hex: hex::encode(signature.to_bytes()),
    })
}

pub fn verify_envelope(
    signed: &SignedDeploymentEnvelope,
    trusted_keys: &BTreeMap<String, VerifyingKey>,
    policy: &RuntimeEnvelopePolicy,
    now: DateTime<Utc>,
    nonce_consumed: impl Fn(&str) -> bool,
) -> Result<VerifiedDeploymentEnvelope, DomainError> {
    signed.envelope.validate()?;
    validate_runtime_policy(policy)?;
    require_text("key_id", &signed.key_id)?;
    let expected_hash = hex::encode(Sha256::digest(canonical_payload(&signed.envelope)?));
    if expected_hash != signed.envelope.payload_hash {
        return Err(DomainError::PayloadHashMismatch);
    }
    let key = trusted_keys
        .get(&signed.key_id)
        .ok_or(DomainError::UnknownSigningKey)?;
    let signature_bytes =
        hex::decode(&signed.signature_hex).map_err(|_| DomainError::InvalidSignatureEncoding)?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| DomainError::InvalidSignatureEncoding)?;
    key.verify(signed.envelope.payload_hash.as_bytes(), &signature)
        .map_err(|_| DomainError::InvalidSignature)?;
    if now < signed.envelope.valid_from {
        return Err(DomainError::NotYetValid);
    }
    if now >= signed.envelope.expires_at {
        return Err(DomainError::Expired);
    }
    if nonce_consumed(&signed.envelope.nonce) {
        return Err(DomainError::NonceReplay);
    }
    if signed.envelope.account_id != policy.account_id
        || signed.envelope.venue != policy.venue
        || signed.envelope.runtime_config_hash != policy.runtime_config_hash
        || signed.envelope.risk_policy_hash != policy.risk_policy_hash
        || signed
            .envelope
            .instruments
            .iter()
            .any(|instrument| !policy.allowed_instruments.contains(instrument))
        || signed
            .envelope
            .allowed_intent_types
            .iter()
            .any(|intent| !policy.allowed_intent_types.contains(intent))
    {
        return Err(DomainError::RuntimeBindingMismatch);
    }
    if signed.envelope.max_notional > policy.max_notional
        || signed.envelope.max_symbol_exposure > policy.max_symbol_exposure
        || signed.envelope.max_order_size > policy.max_order_size
        || signed.envelope.max_slippage_bps > policy.max_slippage_bps
    {
        return Err(DomainError::RuntimeLimitExceeded);
    }
    verify_runtime_approvals(&signed.envelope, policy, now)?;
    Ok(VerifiedDeploymentEnvelope(signed.envelope.clone()))
}

fn verify_runtime_approvals(
    envelope: &DeploymentEnvelope,
    policy: &RuntimeEnvelopePolicy,
    now: DateTime<Utc>,
) -> Result<(), DomainError> {
    if envelope.approval_class == ApprovalClass::SameClassAutoLiveSmall {
        return Err(DomainError::ApprovalEvidenceMismatch);
    }
    let scope_hash = deployment_scope_hash(envelope)?;
    for approval_id in &envelope.approval_signatures {
        let approval = policy
            .approvals
            .iter()
            .find(|approval| approval.approval_id == *approval_id)
            .ok_or(DomainError::ApprovalEvidenceMismatch)?;
        if !approval.is_active_at(now)
            || approval.approval_class != envelope.approval_class
            || approval.subject_id != envelope.promotion_id
            || approval.scope_hash != scope_hash
        {
            return Err(DomainError::ApprovalEvidenceMismatch);
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct SignableEnvelope<'a> {
    deployment_id: &'a str,
    asset_revision_id: &'a str,
    promotion_id: &'a str,
    promotion_manifest_hash: &'a str,
    bundle_id: &'a str,
    bundle_hash: &'a str,
    runtime_config_hash: &'a str,
    risk_policy_hash: &'a str,
    account_id: &'a str,
    venue: &'a str,
    instruments: &'a [String],
    allowed_intent_types: &'a [AllowedIntentType],
    max_notional: f64,
    max_symbol_exposure: f64,
    max_order_size: f64,
    max_slippage_bps: f64,
    valid_from: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    nonce: &'a str,
    approval_class: &'a ApprovalClass,
    approval_signatures: &'a [String],
}

fn canonical_payload(envelope: &DeploymentEnvelope) -> Result<Vec<u8>, DomainError> {
    serde_json::to_vec(&SignableEnvelope {
        deployment_id: &envelope.deployment_id,
        asset_revision_id: &envelope.asset_revision_id,
        promotion_id: &envelope.promotion_id,
        promotion_manifest_hash: &envelope.promotion_manifest_hash,
        bundle_id: &envelope.bundle_id,
        bundle_hash: &envelope.bundle_hash,
        runtime_config_hash: &envelope.runtime_config_hash,
        risk_policy_hash: &envelope.risk_policy_hash,
        account_id: &envelope.account_id,
        venue: &envelope.venue,
        instruments: &envelope.instruments,
        allowed_intent_types: &envelope.allowed_intent_types,
        max_notional: envelope.max_notional,
        max_symbol_exposure: envelope.max_symbol_exposure,
        max_order_size: envelope.max_order_size,
        max_slippage_bps: envelope.max_slippage_bps,
        valid_from: envelope.valid_from,
        expires_at: envelope.expires_at,
        nonce: &envelope.nonce,
        approval_class: &envelope.approval_class,
        approval_signatures: &envelope.approval_signatures,
    })
    .map_err(|_| DomainError::CanonicalSerialization)
}

pub fn canonical_json_hash(value: &impl Serialize) -> Result<String, DomainError> {
    let bytes = serde_json::to_vec(value).map_err(|_| DomainError::CanonicalSerialization)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub fn deployment_scope_hash(envelope: &DeploymentEnvelope) -> Result<String, DomainError> {
    let mut instruments = envelope.instruments.clone();
    instruments.sort();
    instruments.dedup();
    let mut allowed_intent_types = envelope.allowed_intent_types.clone();
    allowed_intent_types.sort_by_key(intent_sort_key);
    allowed_intent_types.dedup();
    let scope = serde_json::json!({
        "account_id": envelope.account_id,
        "venue": envelope.venue,
        "instruments": instruments,
        "allowed_intent_types": allowed_intent_types,
        "max_notional": envelope.max_notional,
        "max_symbol_exposure": envelope.max_symbol_exposure,
        "max_order_size": envelope.max_order_size,
        "max_slippage_bps": envelope.max_slippage_bps,
    });
    canonical_json_hash(&scope)
}

fn intent_sort_key(intent: &AllowedIntentType) -> u8 {
    match intent {
        AllowedIntentType::LoadFactor => 0,
        AllowedIntentType::LoadModel => 1,
        AllowedIntentType::LoadAllocatorPolicy => 2,
        AllowedIntentType::StartPaper => 3,
        AllowedIntentType::StartShadow => 4,
        AllowedIntentType::StartLiveSmall => 5,
    }
}

fn validate_sha256(value: &str) -> Result<(), DomainError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DomainError::InvalidStrategyBundle);
    }
    Ok(())
}

fn validate_deployment_hash(value: &str) -> Result<(), DomainError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DomainError::InvalidDeploymentHash);
    }
    Ok(())
}

fn require_text(name: &'static str, value: &str) -> Result<(), DomainError> {
    if value.trim().is_empty() {
        return Err(DomainError::EmptyField(name));
    }
    Ok(())
}

fn validate_runtime_policy(policy: &RuntimeEnvelopePolicy) -> Result<(), DomainError> {
    require_text("runtime account_id", &policy.account_id)?;
    require_text("runtime venue", &policy.venue)?;
    require_text("runtime_config_hash", &policy.runtime_config_hash)?;
    require_text("risk_policy_hash", &policy.risk_policy_hash)?;
    validate_deployment_hash(&policy.runtime_config_hash)?;
    validate_deployment_hash(&policy.risk_policy_hash)?;
    if policy.allowed_instruments.is_empty()
        || policy
            .allowed_instruments
            .iter()
            .any(|item| item.trim().is_empty())
        || policy.allowed_intent_types.is_empty()
    {
        return Err(DomainError::RuntimeBindingMismatch);
    }
    let mut approval_ids = BTreeSet::new();
    for approval in &policy.approvals {
        approval.validate()?;
        if !approval_ids.insert(&approval.approval_id) {
            return Err(DomainError::ApprovalEvidenceMismatch);
        }
    }
    if [
        policy.max_notional,
        policy.max_symbol_exposure,
        policy.max_order_size,
        policy.max_slippage_bps,
    ]
    .iter()
    .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(DomainError::InvalidDeploymentLimit);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn envelope(now: DateTime<Utc>) -> DeploymentEnvelope {
        DeploymentEnvelope {
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "factor-1@3".to_string(),
            promotion_id: "promotion-1".to_string(),
            promotion_manifest_hash: "a".repeat(64),
            bundle_id: "bundle-1".to_string(),
            bundle_hash: "b".repeat(64),
            runtime_config_hash: "c".repeat(64),
            risk_policy_hash: "d".repeat(64),
            account_id: "account-1".to_string(),
            venue: "binance".to_string(),
            instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![AllowedIntentType::StartPaper],
            max_notional: 1_000.0,
            max_symbol_exposure: 500.0,
            max_order_size: 100.0,
            max_slippage_bps: 5.0,
            valid_from: now - Duration::minutes(1),
            expires_at: now + Duration::minutes(10),
            nonce: "nonce-1".to_string(),
            approval_class: ApprovalClass::Paper,
            approval_signatures: vec!["approval-1".to_string()],
            payload_hash: String::new(),
        }
    }

    fn policy() -> RuntimeEnvelopePolicy {
        let now = Utc::now();
        let approval_envelope = envelope(now);
        RuntimeEnvelopePolicy {
            account_id: "account-1".to_string(),
            venue: "binance".to_string(),
            allowed_instruments: vec!["BTCUSDT".to_string()],
            allowed_intent_types: vec![AllowedIntentType::StartPaper],
            runtime_config_hash: "c".repeat(64),
            risk_policy_hash: "d".repeat(64),
            max_notional: 1_000.0,
            max_symbol_exposure: 500.0,
            max_order_size: 100.0,
            max_slippage_bps: 5.0,
            approvals: vec![RuntimeApprovalEvidence {
                approval_id: "approval-1".to_string(),
                approval_class: ApprovalClass::Paper,
                subject_id: "promotion-1".to_string(),
                scope_hash: deployment_scope_hash(&approval_envelope).unwrap(),
                signer_id: "risk-officer-1".to_string(),
                valid_from: now - Duration::hours(1),
                expires_at: now + Duration::hours(1),
                revoked_at: None,
            }],
        }
    }

    fn signed(now: DateTime<Utc>) -> (SignedDeploymentEnvelope, BTreeMap<String, VerifyingKey>) {
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let signed = sign_envelope(envelope(now), "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);
        (signed, trusted)
    }

    fn mission(now: DateTime<Utc>) -> ResearchMission {
        ResearchMission {
            mission_id: "mission-1".to_string(),
            objective: "find one stable factor".to_string(),
            hypothesis_scope: "LOB imbalance".to_string(),
            mutable_scope: vec!["factor_ast".to_string()],
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({"metric": "rank_ic"}),
            search_budget: SearchBudget {
                max_candidates: 2,
                max_expansions: 10,
                max_tokens: 0,
                max_seconds: 30,
            },
            completion_policy: MissionCompletionPolicy::default(),
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn mission_rejects_zero_candidate_budget() {
        let budget = SearchBudget {
            max_candidates: 0,
            max_expansions: 1,
            max_tokens: 0,
            max_seconds: 0,
        };
        assert_eq!(budget.validate(), Err(DomainError::InvalidSearchBudget));
    }

    #[test]
    fn mission_rejects_invalid_terminal_transition() {
        assert!(!MissionStatus::Completed.can_transition_to(&MissionStatus::Running));
    }

    #[test]
    fn mission_finishes_only_with_a_policy_bound_reason() {
        let now = Utc::now();
        let mut mission = mission(now);
        mission.transition_to(MissionStatus::Running, now).unwrap();
        assert_eq!(
            mission.finish(
                MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 0 },
                now,
            ),
            Err(DomainError::InvalidMissionTerminalReason)
        );
        mission
            .finish(
                MissionTerminalReason::CompletionPolicySatisfied { kept_candidates: 1 },
                now,
            )
            .unwrap();
        assert_eq!(mission.status, MissionStatus::Completed);
        assert!(mission.validate().is_ok());
    }

    #[test]
    fn loop_run_persists_stage_history_and_explicit_stop_reason() {
        let now = Utc::now();
        let mut run = LoopRun {
            loop_run_id: "loop-1".to_string(),
            root_mission_id: "mission-1".to_string(),
            completion_policy: LoopCompletionPolicy {
                target_stage: LoopTargetStage::ShadowHealthy,
                max_research_missions: 2,
            },
            child_mission_ids: vec![],
            stage_records: vec![],
            status: LoopRunStatus::Pending,
            stop_reason: None,
            created_at: now,
            updated_at: now,
        };
        run.start(now).unwrap();
        run.append_stage(LoopStageRecord {
            record_id: "loop-1:mission-1:researching".to_string(),
            mission_id: "mission-1".to_string(),
            stage: LoopStage::Researching,
            status: LoopStageStatus::Completed,
            reason: "completion policy satisfied".to_string(),
            recorded_at: now,
        })
        .unwrap();
        run.stop(
            LoopStopReason::TargetStageReached {
                mission_id: "mission-1".to_string(),
                stage: LoopTargetStage::Researching,
            },
            now,
        )
        .unwrap();

        assert_eq!(run.status, LoopRunStatus::Completed);
        assert_eq!(run.stage_records.len(), 1);
        assert!(run.validate().is_ok());
    }

    #[test]
    fn awaiting_evidence_pauses_loop_runs() {
        assert_eq!(
            LoopStopReason::AwaitingEvidence {
                mission_id: "mission-1".to_string(),
                stage: LoopTargetStage::PaperHealthy,
            }
            .status(),
            LoopRunStatus::Paused
        );
    }

    #[test]
    fn order_attribution_requires_complete_strategy_scope() {
        let now = Utc::now();
        let mut event = RuntimeAttributionEvent {
            event_id: "fill-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::Fill,
            strategy_id: None,
            order_id: Some("order-1".to_string()),
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: Some("BTCUSDT".to_string()),
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        assert!(event.validate().is_err());
        event.strategy_id = Some("strategy-1".to_string());
        assert!(event.validate().is_ok());
        event.outcome = AttributionOutcome::Activated;
        assert_eq!(
            event.validate(),
            Err(DomainError::InvalidAttributionOutcome)
        );
    }

    #[test]
    fn portfolio_snapshot_requires_strategy_account_and_venue() {
        let now = Utc::now();
        let mut event = RuntimeAttributionEvent {
            event_id: "snapshot-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Paper,
            outcome: AttributionOutcome::Healthy,
            kind: AttributionKind::PortfolioSnapshot,
            strategy_id: None,
            order_id: None,
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: None,
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        assert!(event.validate().is_err());
        event.strategy_id = Some("strategy-1".to_string());
        assert!(event.validate().is_ok());
    }

    #[test]
    fn runtime_attribution_requires_a_trusted_signature() {
        let key = SigningKey::from_bytes(&[9_u8; 32]);
        let event = RuntimeAttributionEvent {
            event_id: "activation-1".to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Shadow,
            outcome: AttributionOutcome::Activated,
            kind: AttributionKind::Activation,
            strategy_id: None,
            order_id: None,
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: None,
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: Utc::now(),
        };
        let signed = sign_runtime_attribution_event(event.clone(), "feedback-1", &key).unwrap();
        let trusted = BTreeMap::from([("feedback-1".to_string(), key.verifying_key())]);

        assert_eq!(
            verify_runtime_attribution_event(&signed, &trusted).unwrap(),
            event
        );
        assert_eq!(
            verify_runtime_attribution_event(&signed, &BTreeMap::new()).unwrap_err(),
            DomainError::UnknownAttributionSigningKey
        );

        let mut tampered = signed;
        tampered.event.asset_revision_id = "candidate-forged".to_string();
        assert_eq!(
            verify_runtime_attribution_event(&tampered, &trusted).unwrap_err(),
            DomainError::AttributionPayloadHashMismatch
        );
    }

    #[test]
    fn runtime_health_requires_activation_fill_and_snapshot_from_one_strategy() {
        let now = Utc::now();
        let scoped = |id: &str,
                      kind: AttributionKind,
                      outcome: AttributionOutcome,
                      strategy_id: Option<&str>| RuntimeAttributionEvent {
            event_id: id.to_string(),
            deployment_id: "deployment-1".to_string(),
            asset_revision_id: "candidate-1".to_string(),
            mission_id: None,
            mode: AttributionMode::Shadow,
            outcome,
            kind,
            strategy_id: strategy_id.map(str::to_string),
            order_id: strategy_id.map(|_| "order-1".to_string()),
            account_id: Some("account-1".to_string()),
            venue: Some("binance".to_string()),
            symbol: strategy_id.map(|_| "BTCUSDT".to_string()),
            metrics: BTreeMap::new(),
            reason: None,
            observed_at: now,
        };
        let mut events = vec![scoped(
            "activation",
            AttributionKind::Activation,
            AttributionOutcome::Activated,
            None,
        )];
        assert!(!runtime_stage_is_healthy(
            &events,
            "candidate-1",
            AttributionMode::Shadow
        ));
        events.push(scoped(
            "fill",
            AttributionKind::Fill,
            AttributionOutcome::Healthy,
            Some("strategy-1"),
        ));
        events.push(scoped(
            "snapshot",
            AttributionKind::PortfolioSnapshot,
            AttributionOutcome::Healthy,
            Some("strategy-1"),
        ));
        assert!(runtime_stage_is_healthy(
            &events,
            "candidate-1",
            AttributionMode::Shadow
        ));
    }

    #[test]
    fn mission_rejects_empty_id() {
        let mut mission = mission(Utc::now());
        mission.mission_id = " ".to_string();
        assert_eq!(
            mission.validate(),
            Err(DomainError::EmptyField("mission_id"))
        );
    }

    #[test]
    fn evaluation_evidence_recomputes_policy_and_adjusted_score() {
        let config = FormulaEvaluatorConfig::for_trials(10).unwrap();
        let raw_score = 5.0;
        let adjusted_score = config.adjusted_score(raw_score).unwrap();
        let mut evaluation = CandidateEvaluation {
            passed: true,
            score: adjusted_score,
            failure_reasons: vec![],
            evaluator_version: SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            evaluator_config: serde_json::to_value(&config).unwrap(),
            metrics: EvaluationMetrics {
                row_count: 30,
                trade_count: 30,
                mean_net_return: 0.001,
                cumulative_net_return: 0.03,
                max_drawdown: 0.01,
                raw_score,
                adjusted_score,
                folds: vec![FoldEvaluationMetrics {
                    fold_index: 1,
                    row_count: 30,
                    trade_count: 30,
                    mean_net_return: 0.001,
                    cumulative_net_return: 0.03,
                    max_drawdown: 0.01,
                    raw_score,
                }],
            },
        };
        assert!(evaluation.validate().is_ok());

        evaluation.metrics.trade_count = 0;
        evaluation.metrics.folds[0].trade_count = 0;
        assert_eq!(
            evaluation.validate(),
            Err(DomainError::InvalidEvaluationEvidence)
        );
    }

    #[test]
    fn mission_can_preregister_a_larger_multiple_testing_family() {
        let mut mission = mission(Utc::now());
        mission.validator_spec = serde_json::json!({"multiple_testing_trials": 100});
        assert_eq!(
            FormulaEvaluatorConfig::for_mission(&mission)
                .unwrap()
                .multiple_testing_trials,
            100
        );

        mission.validator_spec = serde_json::json!({"multiple_testing_trials": 1});
        assert_eq!(
            FormulaEvaluatorConfig::for_mission(&mission),
            Err(DomainError::InvalidEvaluatorConfiguration)
        );
    }

    #[test]
    fn strategy_bundle_hash_detects_tampering() {
        let mut bundle = StrategyBundle::new(
            "bundle-1".to_string(),
            "candidate-1".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            StrategyBundleArtifact::Formula {
                ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field(
                    "signal".to_string(),
                )),
            },
            Utc::now(),
        )
        .unwrap();
        assert!(bundle.validate().is_ok());

        bundle.evaluator_version = "forged".to_string();
        assert_eq!(
            bundle.validate(),
            Err(DomainError::StrategyBundleHashMismatch)
        );
    }

    #[test]
    fn strategy_bundle_hash_is_stable_across_storage_metadata() {
        let artifact = StrategyBundleArtifact::Formula {
            ast: FactorAst::Terminal(hft_factor_dsl::FactorTerminal::Field("signal".to_string())),
        };
        let first = StrategyBundle::new(
            "bundle-1".to_string(),
            "candidate-1".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            artifact.clone(),
            Utc::now(),
        )
        .unwrap();
        let second = StrategyBundle::new(
            "bundle-2".to_string(),
            "candidate-alias".to_string(),
            "a".repeat(64),
            ManifestId::new("dataset-1").unwrap(),
            SEALED_HOLDOUT_EVALUATOR_VERSION.to_string(),
            "c".repeat(64),
            "d".repeat(64),
            "b".repeat(64),
            artifact,
            first.created_at + chrono::Duration::seconds(1),
        )
        .unwrap();

        assert_eq!(first.bundle_hash, second.bundle_hash);
    }

    #[test]
    fn onnx_candidate_rejects_oversized_artifacts_and_tensors() {
        let candidate = |byte_len, dimensions| OnnxModelCandidate {
            artifact: ArtifactRef {
                uri: "model.onnx".to_string(),
                content_type: "application/onnx".to_string(),
                checksum: Some("a".repeat(64)),
            },
            byte_len,
            opset: 17,
            preprocessing_version: LOB_ONNX_PREPROCESSING_VERSION.to_string(),
            inputs: vec![TensorSpec {
                name: "lob".to_string(),
                element_type: TensorElementType::Float32,
                dimensions,
            }],
            output: TensorSpec {
                name: "signal".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1)],
            },
        };

        assert_eq!(
            candidate(
                MAX_ONNX_ARTIFACT_BYTES + 1,
                vec![Some(1), Some(4), Some(2), Some(2)]
            )
            .validate(),
            Err(DomainError::InvalidStrategyBundle)
        );
        assert_eq!(
            candidate(1, vec![Some(1), Some(4), Some(MAX_ONNX_TENSOR_ELEMENTS)]).validate(),
            Err(DomainError::InvalidStrategyBundle)
        );
    }

    #[test]
    fn legacy_deployment_envelope_deserializes_but_cannot_be_activated() {
        let now = Utc::now();
        let mut value = serde_json::to_value(envelope(now)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("promotion_id");
        object.remove("bundle_id");
        object.remove("bundle_hash");

        let legacy: DeploymentEnvelope = serde_json::from_value(value).unwrap();
        assert!(legacy.promotion_id.is_empty());
        assert!(legacy.bundle_id.is_empty());
        assert!(legacy.bundle_hash.is_empty());
        assert_eq!(
            legacy.validate(),
            Err(DomainError::EmptyField("promotion_id"))
        );
    }

    #[test]
    fn opaque_candidate_artifacts_are_research_only() {
        let artifact = CandidateArtifact::ModelArtifact(ArtifactRef {
            uri: "artifact://model".to_string(),
            content_type: "application/onnx".to_string(),
            checksum: Some("a".repeat(64)),
        });

        assert_eq!(
            artifact.to_governed_strategy_bundle_artifact(),
            Err(DomainError::ResearchOnlyArtifact)
        );
        let onnx = CandidateArtifact::OnnxModel(OnnxModelCandidate {
            artifact: ArtifactRef {
                uri: "model.onnx".to_string(),
                content_type: "application/onnx".to_string(),
                checksum: Some("a".repeat(64)),
            },
            byte_len: 1,
            opset: 17,
            preprocessing_version: LOB_ONNX_PREPROCESSING_VERSION.to_string(),
            inputs: vec![TensorSpec {
                name: "input".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(1)],
            }],
            output: TensorSpec {
                name: "output".to_string(),
                element_type: TensorElementType::Float32,
                dimensions: vec![Some(1), Some(1)],
            },
        });
        assert!(matches!(
            onnx.to_governed_strategy_bundle_artifact(),
            Ok(StrategyBundleArtifact::Onnx { .. })
        ));
    }

    #[test]
    fn deployment_envelope_round_trips_valid_signature() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let verified = verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap();
        assert_eq!(verified.0.deployment_id, "deployment-1");
    }

    #[test]
    fn deployment_envelope_rejects_self_asserted_approval_ids() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut forged = envelope(now);
        forged.approval_signatures = vec!["self-asserted".to_string()];
        let signed = sign_envelope(forged, "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);

        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::ApprovalEvidenceMismatch
        );
    }

    #[test]
    fn deployment_envelope_rejects_wrong_or_inactive_runtime_approval_evidence() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);

        let mut wrong_class = policy();
        wrong_class.approvals[0].approval_class = ApprovalClass::Shadow;
        let mut wrong_subject = policy();
        wrong_subject.approvals[0].subject_id = "promotion-2".to_string();
        let mut wrong_scope = policy();
        wrong_scope.approvals[0].scope_hash = "f".repeat(64);
        let mut expired = policy();
        expired.approvals[0].expires_at = now;
        let mut revoked = policy();
        revoked.approvals[0].revoked_at = Some(now);

        for invalid_policy in [wrong_class, wrong_subject, wrong_scope, expired, revoked] {
            assert_eq!(
                verify_envelope(&signed, &trusted, &invalid_policy, now, |_| false).unwrap_err(),
                DomainError::ApprovalEvidenceMismatch
            );
        }
    }

    #[test]
    fn deployment_envelope_rejects_forged_payload() {
        let now = Utc::now();
        let (mut signed, trusted) = signed(now);
        signed.envelope.max_notional += 1.0;
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::PayloadHashMismatch
        );
    }

    #[test]
    fn deployment_envelope_rejects_invalid_signature() {
        let now = Utc::now();
        let (mut signed, trusted) = signed(now);
        let replacement = if signed.signature_hex.starts_with('0') {
            "1"
        } else {
            "0"
        };
        signed.signature_hex.replace_range(0..1, replacement);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::InvalidSignature
        );
    }

    #[test]
    fn deployment_envelope_rejects_unknown_key() {
        let now = Utc::now();
        let (signed, _) = signed(now);
        assert_eq!(
            verify_envelope(&signed, &BTreeMap::new(), &policy(), now, |_| false).unwrap_err(),
            DomainError::UnknownSigningKey
        );
    }

    #[test]
    fn deployment_envelope_rejects_expiry() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut expired = envelope(now);
        expired.valid_from = now - Duration::minutes(2);
        expired.expires_at = now - Duration::minutes(1);
        let signed = sign_envelope(expired, "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::Expired
        );
    }

    #[test]
    fn deployment_envelope_rejects_replayed_nonce() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |nonce| nonce
                == "nonce-1")
            .unwrap_err(),
            DomainError::NonceReplay
        );
    }

    #[test]
    fn deployment_envelope_rejects_runtime_binding_mismatch() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let mut wrong_policy = policy();
        wrong_policy.account_id = "another-account".to_string();
        assert_eq!(
            verify_envelope(&signed, &trusted, &wrong_policy, now, |_| false).unwrap_err(),
            DomainError::RuntimeBindingMismatch
        );
    }

    #[test]
    fn deployment_envelope_rejects_runtime_limit_increase() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let mut tighter_policy = policy();
        tighter_policy.max_notional = 999.0;
        assert_eq!(
            verify_envelope(&signed, &trusted, &tighter_policy, now, |_| false).unwrap_err(),
            DomainError::RuntimeLimitExceeded
        );
    }

    #[test]
    fn deployment_envelope_rejects_non_finite_runtime_limit() {
        let now = Utc::now();
        let (signed, trusted) = signed(now);
        let mut invalid_policy = policy();
        invalid_policy.max_notional = f64::NAN;
        assert_eq!(
            verify_envelope(&signed, &trusted, &invalid_policy, now, |_| false).unwrap_err(),
            DomainError::InvalidDeploymentLimit
        );
    }

    #[test]
    fn deployment_envelope_rejects_unlisted_instrument() {
        let now = Utc::now();
        let key = SigningKey::from_bytes(&[7_u8; 32]);
        let mut unlisted = envelope(now);
        unlisted.instruments = vec!["ETHUSDT".to_string()];
        let signed = sign_envelope(unlisted, "key-1", &key).unwrap();
        let trusted = BTreeMap::from([("key-1".to_string(), key.verifying_key())]);
        assert_eq!(
            verify_envelope(&signed, &trusted, &policy(), now, |_| false).unwrap_err(),
            DomainError::RuntimeBindingMismatch
        );
    }
}
