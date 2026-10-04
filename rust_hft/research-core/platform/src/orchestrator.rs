//! Durable task lifecycle independent of GitHub Actions and provider admission.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    execution::{Backend, ExecutionHandle, Profile},
    identity, valid_digest,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Prepare,
    Train,
    Backtest,
    Explore,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    pub schema: u32,
    pub kind: TaskKind,
    pub run_manifest_sha256: String,
    pub view_manifest_sha256: String,
    pub source_sha256: String,
    pub image: String,
    pub command: Vec<String>,
    pub profile: Profile,
    pub timeout_ms: i64,
    pub max_attempts: u32,
    pub output_prefix: String,
    pub fit_identity_sha256: Option<String>,
}

impl TaskSpec {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1
                && valid_digest(&self.run_manifest_sha256)
                && valid_digest(&self.view_manifest_sha256)
                && valid_digest(&self.source_sha256),
            "invalid task identity"
        );
        ensure!(
            self.image
                .rsplit_once("@sha256:")
                .is_some_and(|(_, h)| valid_digest(h)),
            "image must be digest pinned"
        );
        ensure!(
            !self.command.is_empty()
                && self.command.len() <= 64
                && self
                    .command
                    .iter()
                    .all(|v| !v.is_empty() && v.len() <= 4096 && !v.contains('\0')),
            "invalid command contract"
        );
        ensure!(
            self.timeout_ms > 0
                && self.timeout_ms <= 7 * 24 * 60 * 60 * 1000
                && self.max_attempts > 0
                && self.max_attempts <= 16,
            "unbounded task"
        );
        ensure!(
            self.output_prefix.starts_with("research/")
                && !self.output_prefix.contains("..")
                && self
                    .output_prefix
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/-_".contains(&c)),
            "unsafe artifact prefix"
        );
        ensure!(
            self.profile.backend == Backend::AgentSandbox || self.kind != TaskKind::Explore,
            "interactive work requires Sandbox"
        );
        ensure!(
            self.profile.backend != Backend::AgentSandbox || self.kind == TaskKind::Explore,
            "fixed scientific tasks require a Job"
        );
        if let Some(fit) = &self.fit_identity_sha256 {
            ensure!(valid_digest(fit), "invalid fit identity");
        }
        self.profile.validate()?;
        Ok(())
    }
    pub fn id(&self) -> Result<String> {
        self.validate()?;
        identity(self)
    }
}

/// Imported only by the separately trusted native governance verifier. The
/// generic task API cannot issue grants, reserve trial budgets or sign releases.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub schema: u32,
    pub request_sha256: String,
    pub task_spec: TaskSpec,
    pub resource_reservation_receipt_sha256: String,
    pub scientific_grant_receipt_sha256: String,
    pub release_admission_receipt_sha256: String,
    pub max_attempts: u32,
}
impl Admission {
    pub fn validate(&self, spec: &TaskSpec) -> Result<()> {
        ensure!(
            self.schema == 1
                && self.task_spec == *spec
                && self.request_sha256 == spec.id()?
                && self.max_attempts >= spec.max_attempts,
            "admission does not cover exact request/attempt budget"
        );
        ensure!(
            [
                &self.resource_reservation_receipt_sha256,
                &self.scientific_grant_receipt_sha256,
                &self.release_admission_receipt_sha256
            ]
            .into_iter()
            .all(|s| valid_digest(s)),
            "missing native grant/reservation/release evidence"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Launching,
    Running,
    Stopping,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

impl State {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Launching => "launching",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub task_id: String,
    pub attempt: u32,
    pub fence: i64,
    pub owner: String,
    pub expires_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub key: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResultReceipt {
    pub task_id: String,
    pub attempt: u32,
    pub fence: i64,
    pub view_manifest_sha256: String,
    pub source_sha256: String,
    pub image: String,
    pub fit_identity_sha256: Option<String>,
    pub artifacts: Vec<Artifact>,
    pub checkpoint: Option<Artifact>,
    pub prepared_view: Option<hft_cex_research_input::data::PublishedView>,
}

impl ResultReceipt {
    /// The reconciler independently reads these artifacts before committing PG.
    pub fn validate(&self, spec: &TaskSpec, lease: &Lease) -> Result<()> {
        ensure!(
            self.task_id == lease.task_id
                && self.attempt == lease.attempt
                && self.fence == lease.fence,
            "stale or foreign result"
        );
        ensure!(
            self.view_manifest_sha256 == spec.view_manifest_sha256
                && self.source_sha256 == spec.source_sha256
                && self.image == spec.image
                && self.fit_identity_sha256 == spec.fit_identity_sha256,
            "result provenance mismatch"
        );
        ensure!(
            !self.artifacts.is_empty() && self.artifacts.len() <= 256,
            "empty/unbounded result"
        );
        let prefix = format!("{}/{}/{}/", spec.output_prefix, self.task_id, self.attempt);
        let mut seen = std::collections::BTreeSet::new();
        for artifact in self.artifacts.iter().chain(self.checkpoint.iter()) {
            ensure!(
                artifact.key.starts_with(&prefix)
                    && !artifact.key.contains("..")
                    && artifact
                        .key
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c))
                    && valid_digest(&artifact.sha256)
                    && artifact.bytes > 0
                    && seen.insert(&artifact.key),
                "invalid output identity"
            );
        }
        if spec.kind == TaskKind::Prepare {
            let view = self
                .prepared_view
                .as_ref()
                .context("preparation result lacks published view")?;
            view.verify(&identity(view)?)?;
            ensure!(
                view.prepared_id == identity(&(&lease.task_id, lease.attempt, lease.fence))?,
                "preparation generation changed"
            );
            ensure!(
                view.producer_image == spec.image,
                "preparation producer image changed"
            );
            for block in &view.blocks {
                ensure!(
                    self.artifacts.iter().any(|a| a.sha256 == block.sha256
                        && a.bytes == block.bytes
                        && a.key.ends_with(&format!("{}.mondaybin", block.sha256))),
                    "prepared block lacks independent artifact readback"
                );
            }
        } else {
            ensure!(
                self.prepared_view.is_none(),
                "non-preparation task cannot publish data"
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Checkpoint {
    pub task_id: String,
    pub attempt: u32,
    pub fence: i64,
    pub step: u64,
    pub view_manifest_sha256: String,
    pub source_sha256: String,
    pub image: String,
    pub fit_identity_sha256: Option<String>,
    pub artifact: Artifact,
}
impl Checkpoint {
    pub fn validate(&self, spec: &TaskSpec, lease: &Lease) -> Result<()> {
        ensure!(
            self.task_id == lease.task_id
                && self.attempt == lease.attempt
                && self.fence == lease.fence
                && self.step > 0,
            "stale checkpoint"
        );
        ensure!(
            self.view_manifest_sha256 == spec.view_manifest_sha256
                && self.source_sha256 == spec.source_sha256
                && self.image == spec.image
                && self.fit_identity_sha256 == spec.fit_identity_sha256,
            "checkpoint provenance changed"
        );
        let prefix = format!("{}/{}/{}/", spec.output_prefix, self.task_id, self.attempt);
        ensure!(
            self.artifact.key.starts_with(&prefix)
                && !self.artifact.key.contains("..")
                && self
                    .artifact
                    .key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c))
                && valid_digest(&self.artifact.sha256)
                && self.artifact.bytes > 0
                && self.artifact.bytes <= 4 * 1024 * 1024 * 1024,
            "invalid checkpoint artifact"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: String,
    pub spec: TaskSpec,
    pub state: State,
    pub attempt: u32,
    pub fence: i64,
    pub lease: Option<Lease>,
    pub deadline_ms: Option<i64>,
    pub execution: Option<ExecutionHandle>,
    pub stopping_as: Option<State>,
    pub retry_after_stop: bool,
    pub receipt: Option<ResultReceipt>,
    /// Verified, task-local recovery state; never shared between trials.
    pub checkpoint: Option<Checkpoint>,
}

impl Task {
    pub fn new(spec: TaskSpec) -> Result<Self> {
        let id = spec.id()?;
        Ok(Self {
            id,
            spec,
            state: State::Queued,
            attempt: 0,
            fence: 0,
            lease: None,
            deadline_ms: None,
            execution: None,
            stopping_as: None,
            retry_after_stop: false,
            receipt: None,
            checkpoint: None,
        })
    }

    pub fn claim(&mut self, owner: &str, now_ms: i64, lease_ms: i64) -> Result<Lease> {
        ensure!(
            self.state == State::Queued && self.attempt < self.spec.max_attempts,
            "task cannot be claimed"
        );
        ensure!(
            !owner.is_empty()
                && owner.len() <= 128
                && now_ms > 0
                && lease_ms > 0
                && lease_ms <= 300_000,
            "invalid lease"
        );
        ensure!(
            self.deadline_ms.is_none_or(|d| now_ms < d),
            "task deadline expired before retry"
        );
        self.attempt += 1;
        self.fence = self.fence.checked_add(1).context("fence overflow")?;
        let lease = Lease {
            task_id: self.id.clone(),
            attempt: self.attempt,
            fence: self.fence,
            owner: owner.into(),
            expires_ms: now_ms.checked_add(lease_ms).context("lease overflow")?,
        };
        if self.deadline_ms.is_none() {
            self.deadline_ms = Some(
                now_ms
                    .checked_add(self.spec.timeout_ms)
                    .context("deadline overflow")?,
            );
        }
        self.lease = Some(lease.clone());
        self.state = State::Launching;
        Ok(lease)
    }

    pub fn check_lease(&self, lease: &Lease, now_ms: i64) -> Result<()> {
        ensure!(
            !self.state.terminal()
                && self.lease.as_ref() == Some(lease)
                && now_ms < lease.expires_ms,
            "stale or expired lease"
        );
        Ok(())
    }

    pub fn heartbeat(&mut self, lease: &Lease, now_ms: i64, lease_ms: i64) -> Result<Lease> {
        self.check_lease(lease, now_ms)?;
        ensure!(lease_ms > 0 && lease_ms <= 300_000, "invalid heartbeat");
        let mut next = lease.clone();
        next.expires_ms = now_ms.checked_add(lease_ms).context("lease overflow")?;
        self.lease = Some(next.clone());
        Ok(next)
    }

    pub fn launched(&mut self, lease: &Lease, now_ms: i64, handle: ExecutionHandle) -> Result<()> {
        self.check_lease(lease, now_ms)?;
        ensure!(self.state == State::Launching, "not launching");
        handle.validate(lease, &self.spec)?;
        self.execution = Some(handle);
        self.state = State::Running;
        Ok(())
    }

    /// A timeout/cancel first becomes Stopping. Only an authenticated foreground
    /// deletion/process-tree termination acknowledgement allows terminal/retry.
    pub fn stop(&mut self, reason: State, retry: bool) -> Result<()> {
        ensure!(
            !self.state.terminal()
                && matches!(reason, State::Failed | State::Cancelled | State::TimedOut),
            "invalid stop"
        );
        if self.state == State::Stopping {
            // User cancellation supersedes an automatic retry; repeats do not
            // reset the original deadline or allocate another attempt.
            if reason == State::Cancelled {
                self.stopping_as = Some(reason);
                self.retry_after_stop = false;
                self.receipt = None;
            }
            return Ok(());
        }
        if self.state == State::Queued {
            self.state = reason;
            return Ok(());
        }
        self.state = State::Stopping;
        self.stopping_as = Some(reason);
        self.retry_after_stop =
            retry && reason == State::Failed && self.attempt < self.spec.max_attempts;
        Ok(())
    }

    pub fn expire(&mut self, now_ms: i64) -> Result<bool> {
        if self.state.terminal() || self.state == State::Stopping {
            return Ok(false);
        }
        if self.deadline_ms.is_some_and(|d| now_ms >= d) {
            self.stop(State::TimedOut, false)?;
            return Ok(true);
        }
        if self.lease.as_ref().is_some_and(|l| now_ms >= l.expires_ms) {
            self.stop(State::Failed, true)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn stopped(&mut self, expected_attempt: u32, expected_fence: i64) -> Result<()> {
        ensure!(
            self.state == State::Stopping
                && self.attempt == expected_attempt
                && self.fence == expected_fence,
            "stale stop acknowledgement"
        );
        self.state = if self.retry_after_stop {
            State::Queued
        } else {
            self.stopping_as.context("missing stop reason")?
        };
        self.lease = None;
        self.execution = None;
        // A retry retains the original task deadline, including queue wait.
        self.retry_after_stop = false;
        self.stopping_as = None;
        Ok(())
    }

    pub fn stage_checkpoint(
        &mut self,
        lease: &Lease,
        now_ms: i64,
        checkpoint: Checkpoint,
    ) -> Result<()> {
        self.check_lease(lease, now_ms)?;
        ensure!(
            self.state == State::Running && self.deadline_ms.is_some_and(|d| now_ms < d),
            "checkpoint outside live attempt"
        );
        checkpoint.validate(&self.spec, lease)?;
        if let Some(old) = &self.checkpoint {
            if old == &checkpoint {
                return Ok(());
            }
            ensure!(
                checkpoint.step > old.step,
                "checkpoint step must advance across attempts"
            );
        }
        self.checkpoint = Some(checkpoint);
        Ok(())
    }

    pub fn stage_result(
        &mut self,
        lease: &Lease,
        now_ms: i64,
        receipt: ResultReceipt,
    ) -> Result<()> {
        self.check_lease(lease, now_ms)?;
        ensure!(
            self.state == State::Running
                && now_ms < self.deadline_ms.context("missing deadline")?,
            "late result"
        );
        receipt.validate(&self.spec, lease)?;
        self.receipt = Some(receipt);
        self.state = State::Stopping;
        self.stopping_as = Some(State::Succeeded);
        self.retry_after_stop = false;
        Ok(())
    }
}
