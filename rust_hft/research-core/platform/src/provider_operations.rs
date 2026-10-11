//! Durable, single-send provider creation. Unknown HTTP outcomes retain quota.
//! Public request bytes exclude Secret data; credentials never enter PostgreSQL.
use anyhow::{ensure, Context, Result};
use hft_research_worker_contracts::{execution::Backend, task::TaskKind};
use serde_json::Value;
use sqlx_core::{query::query, row::Row};

use crate::{
    execution::resource_name,
    identity,
    orchestrator::{State, Task},
    postgres::{Ledger, LockedTask},
};

pub const MIGRATION: &str = include_str!("../sql/provider_create_operations.sql");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreateKind {
    Execution,
    Identity,
}
impl CreateKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Execution => "execution",
            Self::Identity => "identity",
        }
    }
    fn name(self, task: &Task) -> Result<String> {
        let name = resource_name(
            task.lease
                .as_ref()
                .context("provider operation lacks lease")?,
        );
        Ok(match self {
            Self::Execution => name,
            Self::Identity => format!("{name}-identity"),
        })
    }
}

/// Exact first request identity. For a Secret only the data digest is retained.
#[derive(Clone, Debug)]
pub struct CreateRequest {
    pub(crate) sha256: String,
    pub(crate) public_body: Value,
    pub(crate) secret_data_sha256: Option<String>,
}
impl CreateRequest {
    fn new(kind: CreateKind, task: &Task, body: &Value) -> Result<Self> {
        let expected_kind = match kind {
            CreateKind::Identity => "Secret",
            CreateKind::Execution if task.spec.profile.backend == Backend::AgentSandbox => {
                "Sandbox"
            }
            CreateKind::Execution => "Job",
        };
        ensure!(
            body["kind"] == expected_kind
                && body["metadata"]["name"] == kind.name(task)?
                && body["metadata"]["namespace"] == task.spec.profile.namespace,
            "provider request changed resource scope"
        );
        ensure!(
            body["metadata"]["labels"]["monday.io/task"]
                == task.id.chars().take(40).collect::<String>()
                && body["metadata"]["labels"]["monday.io/attempt"]
                    .as_str()
                    .and_then(|v| v.parse::<u32>().ok())
                    == Some(task.attempt)
                && body["metadata"]["labels"]["monday.io/fence"]
                    .as_str()
                    .and_then(|v| v.parse::<i64>().ok())
                    == Some(task.fence),
            "provider request changed Attempt fence"
        );
        if kind == CreateKind::Execution {
            let context = crate::execution::read_launch_context(
                &body["spec"]["template"]["spec"]["containers"][0],
            )?;
            ensure!(
                context.spec == task.spec
                    && context.lease
                        == *task.lease.as_ref().context("create lacks launch lease")?,
                "provider request changed original launch context"
            );
        }
        let mut public_body = body.clone();
        let secret_data_sha256 = if kind == CreateKind::Identity {
            ensure!(
                task.spec.kind == TaskKind::CexCampaign,
                "unexpected identity create"
            );
            ensure!(
                body.get("stringData").is_none(),
                "Secret stringData is not journalable"
            );
            let data = public_body
                .as_object_mut()
                .context("provider request is not an object")?
                .remove("data")
                .context("Secret data absent")?;
            ensure!(data.is_object(), "Secret data is not an object");
            Some(identity(&data)?)
        } else {
            ensure!(
                body.get("data").is_none() && body.get("stringData").is_none(),
                "unexpected secret payload"
            );
            None
        };
        Ok(Self {
            sha256: identity(body)?,
            public_body,
            secret_data_sha256,
        })
    }

    pub(crate) fn verify(&self, kind: CreateKind, task: &Task, object: &Value) -> Result<String> {
        let uid = object["metadata"]["uid"]
            .as_str()
            .context("provider UID missing")?;
        ensure!(!uid.is_empty() && uid.len() <= 128, "invalid provider UID");
        let mut original = self.public_body.clone();
        if kind == CreateKind::Identity {
            ensure!(
                object.get("stringData").is_none(),
                "unadmitted Secret stringData"
            );
            ensure!(
                Some(identity(&object["data"])?) == self.secret_data_sha256,
                "Secret data changed original request"
            );
            original["data"] = object["data"].clone();
        }
        ensure!(
            identity(&original)? == self.sha256,
            "original provider request digest mismatch"
        );
        crate::execution::verify_create_observation(&original, object, &task.spec.profile)?;
        Ok(uid.into())
    }
}

/// Exists only in the process which durably won NotStarted -> Pending.
/// Neither restart nor GET recovery can reconstruct this permission.
pub struct FirstSend {
    kind: CreateKind,
    request_sha256: String,
}
pub enum CreateDecision {
    FirstSend(FirstSend),
    ReadbackOnly,
    ResolvedAbsent,
}

pub(crate) trait CreateJournal {
    async fn release_for_io(&mut self) -> Result<()>;
    async fn original(&mut self, kind: CreateKind) -> Result<Option<CreateRequest>>;
    async fn begin(&mut self, kind: CreateKind, body: &Value) -> Result<CreateDecision>;
    async fn observed(&mut self, kind: CreateKind, object: &Value) -> Result<()>;
    async fn rejected(&mut self, permit: FirstSend, status: u16) -> Result<()>;
    async fn absence_resolved(&mut self, kind: CreateKind) -> Result<bool>;
}

/// Short transactions own intent/observation changes; provider I/O owns no PG
/// locks. Reacquisition always reads the current task, never reapplies a snapshot.
pub struct ProviderMutation {
    ledger: Ledger,
    locked: Option<LockedTask>,
    original_task: Task,
}
impl ProviderMutation {
    pub fn new(ledger: Ledger, locked: LockedTask) -> Self {
        Self {
            ledger,
            original_task: locked.task.clone(),
            locked: Some(locked),
        }
    }
    pub async fn locked_mut(&mut self) -> Result<&mut LockedTask> {
        self.acquire().await?;
        Ok(self.locked.as_mut().expect("reacquired mutation lock"))
    }
    pub async fn into_locked(mut self) -> Result<LockedTask> {
        self.acquire().await?;
        Ok(self.locked.take().expect("reacquired mutation lock"))
    }

    /// The only public first-send entry point; returned permits cannot be made
    /// from a persisted Pending row. Exposed for the PG protocol contract tests.
    pub async fn begin_create(&mut self, kind: CreateKind, body: &Value) -> Result<CreateDecision> {
        self.begin(kind, body).await
    }
    pub async fn record_observation(&mut self, kind: CreateKind, object: &Value) -> Result<()> {
        self.observed(kind, object).await
    }
    pub async fn record_rejection(&mut self, permit: FirstSend, status: u16) -> Result<()> {
        self.rejected(permit, status).await
    }
    pub async fn permits_absence(&mut self, kind: CreateKind) -> Result<bool> {
        self.absence_resolved(kind).await
    }

    async fn acquire(&mut self) -> Result<()> {
        if self.locked.is_none() {
            let locked = self.ledger.lock_id(&self.original_task.id).await?;
            ensure!(
                locked.task.attempt == self.original_task.attempt
                    && locked.task.fence == self.original_task.fence
                    && locked.task.spec == self.original_task.spec,
                "provider mutation lost original Attempt"
            );
            self.locked = Some(locked);
        }
        Ok(())
    }

    async fn operation(&mut self, kind: CreateKind) -> Result<Operation> {
        self.acquire().await?;
        let task = &self.original_task;
        let row = query("SELECT cluster,namespace,resource_name,state,request_sha256,public_request,secret_data_sha256,observed_uid FROM research.provider_create_operations WHERE task_id=$1 AND attempt=$2 AND fence=$3 AND kind=$4 FOR UPDATE")
            .bind(&task.id).bind(task.attempt as i32).bind(task.fence).bind(kind.as_str())
            .fetch_optional(&mut *self.locked.as_mut().context("mutation lock lost")?.tx).await?
            .context("legacy/untracked provider create outcome is unknown")?;
        ensure!(
            row.get::<String, _>("cluster") == task.spec.profile.cluster
                && row.get::<String, _>("namespace") == task.spec.profile.namespace
                && row.get::<String, _>("resource_name") == kind.name(task)?,
            "provider operation scope changed"
        );
        let sha256: Option<String> = row.get("request_sha256");
        let request = sha256.map(|sha256| CreateRequest {
            sha256,
            public_body: row.get("public_request"),
            secret_data_sha256: row.get("secret_data_sha256"),
        });
        Ok(Operation {
            state: row.get("state"),
            request,
            uid: row.get("observed_uid"),
        })
    }

    async fn persist(&mut self, event: &str) -> Result<bool> {
        let locked = self.locked.take().context("mutation lock lost")?;
        let expected_revision = locked
            .revision
            .checked_add(1)
            .context("revision overflow")?;
        let expected = locked.task.clone();
        locked.commit(event).await?;
        self.acquire().await?;
        let locked = self.locked.as_ref().context("mutation lock lost")?;
        let unchanged = locked.revision == expected_revision && locked.task == expected;
        Ok(unchanged)
    }

    async fn set_outcome(
        &mut self,
        kind: CreateKind,
        state: &str,
        uid: Option<&str>,
        code: Option<u16>,
    ) -> Result<()> {
        let task = &self.original_task;
        let updated = query("UPDATE research.provider_create_operations SET state=$1,observed_uid=$2,rejection_code=$3 WHERE task_id=$4 AND attempt=$5 AND fence=$6 AND kind=$7 AND state='pending'")
            .bind(state).bind(uid).bind(code.map(i32::from)).bind(&task.id).bind(task.attempt as i32).bind(task.fence).bind(kind.as_str())
            .execute(&mut *self.locked.as_mut().context("mutation lock lost")?.tx).await?;
        ensure!(
            updated.rows_affected() == 1,
            "create outcome lost Pending operation"
        );
        self.persist("provider_create_resolved").await?;
        self.release_for_io().await?;
        Ok(())
    }
}
#[derive(Clone)]
struct Operation {
    state: String,
    request: Option<CreateRequest>,
    uid: Option<String>,
}

impl CreateJournal for ProviderMutation {
    async fn release_for_io(&mut self) -> Result<()> {
        if let Some(locked) = self.locked.take() {
            locked.commit("provider_io_started").await?;
        }
        Ok(())
    }
    async fn original(&mut self, kind: CreateKind) -> Result<Option<CreateRequest>> {
        let request = self.operation(kind).await?.request;
        self.release_for_io().await?;
        Ok(request)
    }
    async fn begin(&mut self, kind: CreateKind, body: &Value) -> Result<CreateDecision> {
        let operation = self.operation(kind).await?;
        match operation.state.as_str() {
            "pending" | "observed" => {
                self.release_for_io().await?;
                return Ok(CreateDecision::ReadbackOnly);
            }
            "rejected" | "not_sent" => {
                self.release_for_io().await?;
                return Ok(CreateDecision::ResolvedAbsent);
            }
            "not_started" => {}
            _ => anyhow::bail!("invalid create operation state"),
        }
        let request = CreateRequest::new(kind, &self.original_task, body)?;
        let task = &self.original_task;
        let updated = query("UPDATE research.provider_create_operations SET state='pending',request_sha256=$1,public_request=$2,secret_data_sha256=$3 WHERE task_id=$4 AND attempt=$5 AND fence=$6 AND kind=$7 AND state='not_started'")
            .bind(&request.sha256).bind(&request.public_body).bind(&request.secret_data_sha256)
            .bind(&task.id).bind(task.attempt as i32).bind(task.fence).bind(kind.as_str())
            .execute(&mut *self.locked.as_mut().context("mutation lock lost")?.tx).await?;
        ensure!(updated.rows_affected() == 1, "FirstSend already consumed");
        // Crash after this commit is conservatively unknown, even before HTTP.
        let unchanged = self.persist("provider_create_pending").await?;
        let locked = self.locked_mut().await?;
        let native_deadline = locked.native_request_deadline_ms().await?;
        let now = locked.refresh_clock().await?;
        locked.task.expire(now)?;
        let admitted = unchanged
            && locked.task.state == State::Launching
            && native_deadline.is_some_and(|deadline| deadline > now);
        if !admitted {
            // Only this CAS winner knows that its HTTP call has not happened.
            // A recovery observer never enters this NotSent transition.
            if locked.task.state == State::Launching {
                locked.task.stop(State::Cancelled, false)?;
            }
            self.set_outcome(kind, "not_sent", None, None).await?;
            return Ok(CreateDecision::ResolvedAbsent);
        }
        // This is the admission linearization point. Subsequent cancellation
        // drains this one issued operation; Pending forbids premature refund.
        // No PG lock spans send(). The permit is neither stored nor cloneable.
        self.release_for_io().await?;
        Ok(CreateDecision::FirstSend(FirstSend {
            kind,
            request_sha256: request.sha256,
        }))
    }
    async fn observed(&mut self, kind: CreateKind, object: &Value) -> Result<()> {
        let operation = self.operation(kind).await?;
        let request = operation
            .request
            .context("observed resource has no original create intent")?;
        let uid = request.verify(kind, &self.original_task, object)?;
        match operation.state.as_str() {
            "observed" => ensure!(
                operation.uid.as_deref() == Some(&uid),
                "observed provider UID changed"
            ),
            "pending" => self.set_outcome(kind, "observed", Some(&uid), None).await?,
            _ => anyhow::bail!("resource exists despite known unsent/rejected create"),
        }
        self.release_for_io().await?;
        Ok(())
    }
    async fn rejected(&mut self, permit: FirstSend, status: u16) -> Result<()> {
        ensure!(
            definite_rejection(status),
            "provider response does not prove rejection"
        );
        let operation = self.operation(permit.kind).await?;
        ensure!(
            operation.state == "pending"
                && operation
                    .request
                    .is_some_and(|r| r.sha256 == permit.request_sha256),
            "rejection changed FirstSend identity"
        );
        self.set_outcome(permit.kind, "rejected", None, Some(status))
            .await
    }
    async fn absence_resolved(&mut self, kind: CreateKind) -> Result<bool> {
        let resolved = matches!(
            self.operation(kind).await?.state.as_str(),
            "not_started" | "not_sent" | "rejected" | "observed"
        );
        self.release_for_io().await?;
        Ok(resolved)
    }
}

pub(crate) fn definite_rejection(status: u16) -> bool {
    // 408, 409, 429, all 5xx, transport errors and timeouts stay unknown.
    matches!(
        status,
        400 | 401 | 403 | 404 | 405 | 406 | 410 | 411 | 413 | 414 | 415 | 422
    )
}

pub(crate) async fn initialize(locked: &mut LockedTask) -> Result<()> {
    let task = &locked.task;
    for kind in [CreateKind::Execution, CreateKind::Identity] {
        if kind == CreateKind::Identity && task.spec.kind != TaskKind::CexCampaign {
            continue;
        }
        query("INSERT INTO research.provider_create_operations(task_id,attempt,fence,kind,cluster,namespace,resource_name,task_revision,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'not_started')")
            .bind(&task.id).bind(task.attempt as i32).bind(task.fence).bind(kind.as_str())
            .bind(&task.spec.profile.cluster).bind(&task.spec.profile.namespace).bind(kind.name(task)?)
            .bind(locked.revision.checked_add(1).context("revision overflow")?)
            .execute(&mut *locked.tx).await?;
    }
    Ok(())
}

/// Transport fixture only. Real crash/reconnect and settlement behavior is
/// independently exercised through ProviderMutation and PostgreSQL.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct MemoryJournal {
    task: Task,
    operations: std::collections::BTreeMap<&'static str, Operation>,
}
#[cfg(test)]
impl MemoryJournal {
    pub(crate) fn new(task: Task) -> Self {
        Self {
            task,
            operations: ["execution", "identity"]
                .into_iter()
                .map(|kind| {
                    (
                        kind,
                        Operation {
                            state: "not_started".into(),
                            request: None,
                            uid: None,
                        },
                    )
                })
                .collect(),
        }
    }
}
#[cfg(test)]
impl CreateJournal for MemoryJournal {
    async fn release_for_io(&mut self) -> Result<()> {
        Ok(())
    }
    async fn original(&mut self, kind: CreateKind) -> Result<Option<CreateRequest>> {
        Ok(self.operations[kind.as_str()].request.clone())
    }
    async fn begin(&mut self, kind: CreateKind, body: &Value) -> Result<CreateDecision> {
        let operation = self.operations.get_mut(kind.as_str()).unwrap();
        match operation.state.as_str() {
            "not_started" => {
                let request = CreateRequest::new(kind, &self.task, body)?;
                let permit = FirstSend {
                    kind,
                    request_sha256: request.sha256.clone(),
                };
                operation.request = Some(request);
                operation.state = "pending".into();
                Ok(CreateDecision::FirstSend(permit))
            }
            "pending" | "observed" => Ok(CreateDecision::ReadbackOnly),
            _ => Ok(CreateDecision::ResolvedAbsent),
        }
    }
    async fn observed(&mut self, kind: CreateKind, object: &Value) -> Result<()> {
        let operation = self.operations.get_mut(kind.as_str()).unwrap();
        let uid = operation
            .request
            .as_ref()
            .context("fixture lacks original request")?
            .verify(kind, &self.task, object)?;
        ensure!(
            matches!(operation.state.as_str(), "pending" | "observed"),
            "unexpected fixture observation"
        );
        ensure!(
            operation.uid.as_ref().is_none_or(|old| old == &uid),
            "fixture UID changed"
        );
        operation.uid = Some(uid);
        operation.state = "observed".into();
        Ok(())
    }
    async fn rejected(&mut self, permit: FirstSend, status: u16) -> Result<()> {
        ensure!(definite_rejection(status), "unknown fixture rejection");
        let operation = self.operations.get_mut(permit.kind.as_str()).unwrap();
        ensure!(
            operation.state == "pending"
                && operation
                    .request
                    .as_ref()
                    .is_some_and(|r| r.sha256 == permit.request_sha256),
            "fixture permit changed"
        );
        operation.state = "rejected".into();
        Ok(())
    }
    async fn absence_resolved(&mut self, kind: CreateKind) -> Result<bool> {
        Ok(self.operations[kind.as_str()].state != "pending")
    }
}
