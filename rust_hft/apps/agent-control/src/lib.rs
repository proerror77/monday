//! Pure contract model for restartable engineering agents.
//!
//! This library does not persist, spawn, lock, authenticate evidence, or enforce
//! resource limits. The adapter must atomically persist each accepted transition
//! before emitting its effect, and independently authenticate all observations.
//! In particular, workspace restoration must never restore this control state.

use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id(String);

impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        {
            return Err(Error::InvalidSpec);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Digest([u8; 32]);

impl Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskBindingV1 {
    pub task_id: Id,
    pub contract_id: Id,
    pub workspace_id: Id,
    pub writer_id: Id,
    /// Hash of the immutable admitted packet, not its credentials or contents.
    pub spec_sha256: Digest,
    pub source_revision: String,
    pub image_sha256: Digest,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub model_tokens: u64,
    /// Cumulative CPU time, distinct from requested CPU capacity.
    pub cpu_millis: u64,
}

impl Usage {
    fn within(self, ceiling: Self) -> bool {
        self.model_tokens <= ceiling.model_tokens && self.cpu_millis <= ceiling.cpu_millis
    }

    fn checked_add(self, other: Self) -> Result<Self, Error> {
        Ok(Self {
            model_tokens: self
                .model_tokens
                .checked_add(other.model_tokens)
                .ok_or(Error::BudgetExceeded)?,
            cpu_millis: self
                .cpu_millis
                .checked_add(other.cpu_millis)
                .ok_or(Error::BudgetExceeded)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSpecV1 {
    pub binding: TaskBindingV1,
    /// Absolute Unix time in seconds; no transition can extend it.
    pub deadline: u64,
    pub max_invocations: u32,
    pub budget: Usage,
    /// Requested capacity only. A later substrate must prove enforcement.
    pub cpu_millicores: u32,
    pub memory_bytes: u64,
    /// References to admitted capabilities, never secret values or commands.
    pub capabilities: BTreeSet<Id>,
}

impl TaskSpecV1 {
    fn validate(&self) -> Result<(), Error> {
        if self.deadline == 0
            || self.max_invocations == 0
            || self.cpu_millicores == 0
            || self.memory_bytes == 0
            || self.binding.source_revision.len() != 40
            || !self
                .binding
                .source_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::InvalidSpec);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationKeyV1 {
    pub task: TaskBindingV1,
    pub ordinal: u32,
    /// Persisted before provider submission; reused to reconcile a lost response.
    pub operation_id: Id,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationV1 {
    pub key: InvocationKeyV1,
    pub started_at: u64,
    pub reservation: Usage,
    pub parent_checkpoint: Option<Digest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionRefV1 {
    pub invocation: InvocationKeyV1,
    pub provider_id: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Pending,
    Submitting,
    Running,
    PauseRequested,
    Paused,
    Exited,
    Verified,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartDecision {
    /// Persist the changed control record before calling the provider.
    Submit(InvocationV1),
    /// Observe the original operation; this never authorizes another submission.
    Reconcile(InvocationV1),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuiescenceReceiptV1 {
    pub execution: ExecutionRefV1,
    pub observed_at: u64,
    /// Authenticated provider/runner evidence, verified by the caller.
    pub evidence_sha256: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointReceiptV1 {
    pub execution: ExecutionRefV1,
    pub quiescence_sha256: Digest,
    pub parent_checkpoint: Option<Digest>,
    pub artifact_sha256: Digest,
    pub committed_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalReceiptV1 {
    pub execution: ExecutionRefV1,
    pub parent_checkpoint: Option<Digest>,
    pub exited_at: u64,
    pub exit_code: i32,
    pub output_sha256: Digest,
    pub stdout_sha256: Digest,
    pub stderr_sha256: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifiedOutcome {
    Accepted,
    /// A valid negative finding, not a process failure.
    Negative,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutcomeReadbackV1 {
    /// Exact typed terminal record independently read back by the verifier.
    pub terminal: TerminalReceiptV1,
    pub observed_output_sha256: Digest,
    pub verifier_evidence_sha256: Digest,
    pub outcome: VerifiedOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageReceiptV1 {
    pub execution: ExecutionRefV1,
    pub actual: Usage,
    pub evidence_sha256: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalOperationV1 {
    /// The creating invocation stays bound across later workspace restorations.
    pub invocation: InvocationKeyV1,
    pub operation_id: Id,
    pub request_sha256: Digest,
    pub capability: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalOutcome {
    Succeeded,
    Failed,
    NotSubmitted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalResolutionV1 {
    pub operation: ExternalOperationV1,
    pub outcome: ExternalOutcome,
    pub evidence_sha256: Digest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationDecision {
    Submit,
    Reconcile,
    AlreadyResolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountingV1 {
    pub consumed: Usage,
    pub reserved: Usage,
    pub invocations: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSpec,
    StaleRevision,
    IdentityMismatch,
    InvalidPhase,
    DeadlineExpired,
    Revoked,
    BudgetExceeded,
    UsageUnresolved,
    ExternalOperationUnresolved,
    CapabilityDenied,
    CheckpointMismatch,
    InvalidTime,
    ProcessFailed,
    OwnershipConflict,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

struct ExternalRecord {
    operation: ExternalOperationV1,
    resolution: Option<ExternalResolutionV1>,
}

/// Authoritative model state, deliberately not Clone or part of a checkpoint.
/// A revision check is a transition precondition, not a distributed CAS or lock.
pub struct TaskControlV1 {
    spec: TaskSpecV1,
    revision: u64,
    phase: Phase,
    revoked: bool,
    accounting: AccountingV1,
    invocation: Option<InvocationV1>,
    provider_id: Option<Id>,
    used_operation_ids: BTreeSet<Id>,
    usage_receipt: Option<UsageReceiptV1>,
    checkpoint: Option<CheckpointReceiptV1>,
    terminal: Option<TerminalReceiptV1>,
    readback: Option<OutcomeReadbackV1>,
    external: BTreeMap<Id, ExternalRecord>,
}

impl TaskControlV1 {
    pub fn admit(spec: TaskSpecV1) -> Result<Self, Error> {
        spec.validate()?;
        Ok(Self {
            spec,
            revision: 0,
            phase: Phase::Pending,
            revoked: false,
            accounting: AccountingV1 {
                consumed: Usage::default(),
                reserved: Usage::default(),
                invocations: 0,
            },
            invocation: None,
            provider_id: None,
            used_operation_ids: BTreeSet::new(),
            usage_receipt: None,
            checkpoint: None,
            terminal: None,
            readback: None,
            external: BTreeMap::new(),
        })
    }

    pub fn spec(&self) -> &TaskSpecV1 {
        &self.spec
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn accounting(&self) -> AccountingV1 {
        self.accounting
    }
    pub fn checkpoint(&self) -> Option<&CheckpointReceiptV1> {
        self.checkpoint.as_ref()
    }
    pub fn terminal(&self) -> Option<&TerminalReceiptV1> {
        self.terminal.as_ref()
    }
    pub fn outcome(&self) -> Option<VerifiedOutcome> {
        self.readback.as_ref().map(|r| r.outcome)
    }

    fn expect_revision(&self, expected: u64) -> Result<(), Error> {
        if self.revision != expected || expected == u64::MAX {
            return Err(Error::StaleRevision);
        }
        Ok(())
    }

    fn effect_allowed(&self, now: u64) -> Result<(), Error> {
        if self.revoked {
            return Err(Error::Revoked);
        }
        if now >= self.spec.deadline {
            return Err(Error::DeadlineExpired);
        }
        Ok(())
    }

    fn execution_matches(&self, execution: &ExecutionRefV1) -> Result<(), Error> {
        if self.invocation.as_ref().map(|i| &i.key) != Some(&execution.invocation)
            || self.provider_id.as_ref() != Some(&execution.provider_id)
        {
            return Err(Error::IdentityMismatch);
        }
        Ok(())
    }

    fn observation_time(&self, time: u64, now: u64) -> Result<(), Error> {
        if self.invocation.as_ref().is_none_or(|i| time < i.started_at) || time > now {
            return Err(Error::InvalidTime);
        }
        Ok(())
    }

    fn no_unknown_operations(&self) -> Result<(), Error> {
        if self.external.values().any(|v| v.resolution.is_none()) {
            return Err(Error::ExternalOperationUnresolved);
        }
        Ok(())
    }

    pub fn revoke(&mut self, expected: u64) -> Result<(), Error> {
        self.expect_revision(expected)?;
        if !self.revoked {
            self.revoked = true;
            self.revision += 1;
        }
        Ok(())
    }

    /// A fresh process may start only from Pending or an exact committed file
    /// checkpoint. Accounting and external operations are never restored from it.
    pub fn start(
        &mut self,
        expected: u64,
        operation_id: Id,
        reservation: Usage,
        now: u64,
        restore: Option<(&CheckpointReceiptV1, &[u8])>,
    ) -> Result<StartDecision, Error> {
        self.expect_revision(expected)?;
        if let Some(current) = &self.invocation {
            if current.key.operation_id == operation_id {
                if current.reservation != reservation {
                    return Err(Error::IdentityMismatch);
                }
                return Ok(StartDecision::Reconcile(current.clone()));
            }
        }
        self.effect_allowed(now)?;
        if !matches!(self.phase, Phase::Pending | Phase::Paused) {
            return Err(Error::InvalidPhase);
        }
        self.no_unknown_operations()?;
        if self.invocation.is_some() && self.usage_receipt.is_none() {
            return Err(Error::UsageUnresolved);
        }
        if self.used_operation_ids.contains(&operation_id) {
            return Err(Error::IdentityMismatch);
        }
        let parent_checkpoint = match (self.phase, restore, &self.checkpoint) {
            (Phase::Pending, None, None) => None,
            (Phase::Paused, Some((receipt, bytes)), Some(retained))
                if receipt == retained && Digest::of(bytes) == retained.artifact_sha256 =>
            {
                Some(retained.artifact_sha256)
            }
            _ => return Err(Error::CheckpointMismatch),
        };
        if self
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| now < checkpoint.committed_at)
        {
            return Err(Error::InvalidTime);
        }
        if self.accounting.invocations >= self.spec.max_invocations
            || !self
                .accounting
                .consumed
                .checked_add(reservation)?
                .within(self.spec.budget)
        {
            return Err(Error::BudgetExceeded);
        }
        let invocation = InvocationV1 {
            key: InvocationKeyV1 {
                task: self.spec.binding.clone(),
                ordinal: self.accounting.invocations + 1,
                operation_id: operation_id.clone(),
            },
            started_at: now,
            reservation,
            parent_checkpoint,
        };
        self.used_operation_ids.insert(operation_id);
        self.accounting.invocations += 1;
        self.accounting.reserved = reservation;
        self.invocation = Some(invocation.clone());
        self.provider_id = None;
        self.usage_receipt = None;
        self.terminal = None;
        self.phase = Phase::Submitting;
        self.revision += 1;
        Ok(StartDecision::Submit(invocation))
    }

    /// Reconcile the original submission. An unknown operation has no transition
    /// back to Pending, even if a lookup failed or the controller disappeared.
    pub fn observe_running(
        &mut self,
        expected: u64,
        execution: ExecutionRefV1,
    ) -> Result<(), Error> {
        self.expect_revision(expected)?;
        if self.invocation.as_ref().map(|i| &i.key) != Some(&execution.invocation) {
            return Err(Error::IdentityMismatch);
        }
        if self.phase == Phase::Running {
            return self.execution_matches(&execution);
        }
        if self.phase != Phase::Submitting {
            return Err(Error::InvalidPhase);
        }
        self.provider_id = Some(execution.provider_id);
        self.phase = Phase::Running;
        self.revision += 1;
        Ok(())
    }

    pub fn request_pause(&mut self, expected: u64) -> Result<(), Error> {
        self.expect_revision(expected)?;
        if self.phase == Phase::PauseRequested {
            return Ok(());
        }
        if self.phase != Phase::Running {
            return Err(Error::InvalidPhase);
        }
        self.phase = Phase::PauseRequested;
        self.revision += 1;
        Ok(())
    }

    pub fn confirm_pause(
        &mut self,
        expected: u64,
        quiescence: &QuiescenceReceiptV1,
        checkpoint: CheckpointReceiptV1,
        artifact: &[u8],
        now: u64,
    ) -> Result<(), Error> {
        self.expect_revision(expected)?;
        if self.phase != Phase::PauseRequested {
            return Err(Error::InvalidPhase);
        }
        self.execution_matches(&quiescence.execution)?;
        self.execution_matches(&checkpoint.execution)?;
        self.observation_time(quiescence.observed_at, now)?;
        self.observation_time(checkpoint.committed_at, now)?;
        if checkpoint.committed_at < quiescence.observed_at
            || checkpoint.quiescence_sha256 != quiescence.evidence_sha256
            || checkpoint.parent_checkpoint
                != self.invocation.as_ref().and_then(|i| i.parent_checkpoint)
            || checkpoint.artifact_sha256 != Digest::of(artifact)
        {
            return Err(Error::CheckpointMismatch);
        }
        self.checkpoint = Some(checkpoint);
        self.phase = Phase::Paused;
        self.revision += 1;
        Ok(())
    }

    pub fn observe_exit(
        &mut self,
        expected: u64,
        receipt: TerminalReceiptV1,
        now: u64,
    ) -> Result<(), Error> {
        self.expect_revision(expected)?;
        self.execution_matches(&receipt.execution)?;
        if self.terminal.as_ref() == Some(&receipt) {
            return Ok(());
        }
        if !matches!(self.phase, Phase::Running | Phase::PauseRequested) {
            return Err(Error::InvalidPhase);
        }
        self.observation_time(receipt.exited_at, now)?;
        if receipt.parent_checkpoint != self.invocation.as_ref().and_then(|i| i.parent_checkpoint) {
            return Err(Error::CheckpointMismatch);
        }
        self.terminal = Some(receipt);
        self.phase = Phase::Exited;
        self.revision += 1;
        Ok(())
    }

    /// The reservation remains charged until authenticated usage is known and
    /// all external effects are resolved. A checkpoint cannot refund it.
    pub fn settle_usage(&mut self, expected: u64, receipt: UsageReceiptV1) -> Result<(), Error> {
        self.expect_revision(expected)?;
        self.execution_matches(&receipt.execution)?;
        if self.usage_receipt.as_ref() == Some(&receipt) {
            return Ok(());
        }
        if !matches!(self.phase, Phase::Paused | Phase::Exited) || self.usage_receipt.is_some() {
            return Err(Error::InvalidPhase);
        }
        self.no_unknown_operations()?;
        if !receipt.actual.within(self.accounting.reserved) {
            return Err(Error::BudgetExceeded);
        }
        let consumed = self.accounting.consumed.checked_add(receipt.actual)?;
        self.accounting.consumed = consumed;
        self.accounting.reserved = Usage::default();
        self.usage_receipt = Some(receipt);
        self.revision += 1;
        Ok(())
    }

    pub fn record_external(
        &mut self,
        expected: u64,
        operation: ExternalOperationV1,
        now: u64,
    ) -> Result<OperationDecision, Error> {
        self.expect_revision(expected)?;
        if let Some(retained) = self.external.get(&operation.operation_id) {
            if retained.operation != operation {
                return Err(Error::IdentityMismatch);
            }
            return Ok(if retained.resolution.is_some() {
                OperationDecision::AlreadyResolved
            } else {
                OperationDecision::Reconcile
            });
        }
        self.effect_allowed(now)?;
        if self.phase != Phase::Running {
            return Err(Error::InvalidPhase);
        }
        if self.invocation.as_ref().map(|i| &i.key) != Some(&operation.invocation) {
            return Err(Error::IdentityMismatch);
        }
        if !self.spec.capabilities.contains(&operation.capability) {
            return Err(Error::CapabilityDenied);
        }
        self.no_unknown_operations()?;
        self.external.insert(
            operation.operation_id.clone(),
            ExternalRecord {
                operation,
                resolution: None,
            },
        );
        self.revision += 1;
        Ok(OperationDecision::Submit)
    }

    pub fn resolve_external(
        &mut self,
        expected: u64,
        resolution: ExternalResolutionV1,
    ) -> Result<(), Error> {
        self.expect_revision(expected)?;
        let retained = self
            .external
            .get_mut(&resolution.operation.operation_id)
            .ok_or(Error::IdentityMismatch)?;
        if retained.operation != resolution.operation {
            return Err(Error::IdentityMismatch);
        }
        if let Some(previous) = &retained.resolution {
            return if previous == &resolution {
                Ok(())
            } else {
                Err(Error::IdentityMismatch)
            };
        }
        retained.resolution = Some(resolution);
        self.revision += 1;
        Ok(())
    }

    pub fn verify_outcome(
        &mut self,
        expected: u64,
        readback: OutcomeReadbackV1,
    ) -> Result<(), Error> {
        self.expect_revision(expected)?;
        if self.readback.as_ref() == Some(&readback) {
            return Ok(());
        }
        if self.phase != Phase::Exited {
            return Err(Error::InvalidPhase);
        }
        if self.terminal.as_ref() != Some(&readback.terminal)
            || readback.observed_output_sha256 != readback.terminal.output_sha256
        {
            return Err(Error::IdentityMismatch);
        }
        if readback.terminal.exit_code != 0 {
            return Err(Error::ProcessFailed);
        }
        self.no_unknown_operations()?;
        if self.usage_receipt.is_none() {
            return Err(Error::UsageUnresolved);
        }
        self.readback = Some(readback);
        self.phase = Phase::Verified;
        self.revision += 1;
        Ok(())
    }
}

/// In-memory ownership admission model shared by task and legacy-lease claims.
/// The later integration must perform this check atomically in the *existing*
/// shared ownership store; creating a separate live store would not exclude it.
#[derive(Default)]
pub struct OwnershipClaimsV1 {
    claims: BTreeMap<Id, TaskBindingV1>,
}

impl OwnershipClaimsV1 {
    pub fn claim(&mut self, binding: TaskBindingV1) -> Result<(), Error> {
        if let Some(existing) = self.claims.get(&binding.task_id) {
            return if existing == &binding {
                Ok(())
            } else {
                Err(Error::OwnershipConflict)
            };
        }
        if self
            .claims
            .values()
            .any(|v| v.workspace_id == binding.workspace_id || v.contract_id == binding.contract_id)
        {
            return Err(Error::OwnershipConflict);
        }
        self.claims.insert(binding.task_id.clone(), binding);
        Ok(())
    }
}
