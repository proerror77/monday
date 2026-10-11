//! Durable task lifecycle independent of GitHub Actions and provider admission.
use anyhow::{ensure, Context, Result};
use hft_research_worker_contracts::task::{Artifact, Lease, ResultReceipt, TaskKind, TaskSpec};
use serde::{Deserialize, Serialize};

use crate::{execution::ExecutionHandle, valid_digest};

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
    #[serde(default)]
    pub attempt_identity: Option<crate::execution::AttemptIdentityRef>,
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
            attempt_identity: None,
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
        self.attempt_identity = None;
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
        if self.spec.kind == TaskKind::CexCampaign {
            self.attempt_identity
                .as_ref()
                .context("Campaign launch lacks controlled identity")?
                .validate(&self.spec, lease)?;
        } else {
            ensure!(
                self.attempt_identity.is_none(),
                "unexpected late Campaign identity"
            );
        }
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
