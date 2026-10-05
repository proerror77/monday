//! PostgreSQL is the sole task/result ledger. There is no DuckDB fallback.
use anyhow::{ensure, Context, Result};
use hft_cex_research_input::data::PublishedView;
use serde_json::Value;
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, PgPoolOptions, Postgres};

use crate::{
    execution::Acceptance,
    identity,
    orchestrator::{ResultReceipt, State, Task, TaskKind, TaskSpec},
    preparation::PreparationPlan,
};

pub const MIGRATION: &str = include_str!("../sql/postgres.sql");
pub const BUILD_RELEASE_MIGRATION: &str = include_str!("../sql/verified_build_release.sql");
pub const NATIVE_ADMISSION_MIGRATION: &str = include_str!("../sql/native_admission.sql");
pub const NATIVE_CAMPAIGN_INPUTS_MIGRATION: &str =
    include_str!("../sql/native_campaign_inputs.sql");

#[derive(Clone)]
pub struct Ledger {
    pub(crate) pool: PgPool,
}

#[cfg(feature = "gateway")]
pub(crate) struct ArtifactWritePermit {
    // Keep the task, authority and admission shared locks through atomic link.
    // Cancellation, revocation, takeover and pause are ordered against publication.
    _tx: Transaction<'static, Postgres>,
    pub(crate) prefix: String,
}

pub const ARTIFACT_GATEWAY_MIGRATION: &str = include_str!("../sql/artifact_gateway.sql");

pub const SESSION_DELIVERY_MIGRATION: &str = include_str!("../sql/session_deliveries.sql");

pub fn completion_message(intent_id: &str, intent: &Value) -> Result<String> {
    ensure!(
        crate::valid_digest(intent_id),
        "invalid completion intent identity"
    );
    for name in ["session_sha256", "run_sha256", "task_id"] {
        ensure!(
            intent
                .get(name)
                .and_then(Value::as_str)
                .is_some_and(crate::valid_digest),
            "invalid completion identity"
        );
    }
    let state = intent
        .get("state")
        .and_then(Value::as_str)
        .context("completion state missing")?;
    ensure!(
        matches!(state, "succeeded" | "failed" | "cancelled" | "timed_out")
            && intent
                .get("terminal_revision")
                .and_then(Value::as_i64)
                .is_some_and(|v| v > 0),
        "completion must bind a terminal revision"
    );
    Ok(format!("Research Run {} finished with state {state}. Read research.status and research.artifacts for verified results. Completion intent: {}.", intent["run_sha256"].as_str().context("completion Run missing")?, intent_id))
}

/// An opaque, live PG admission plus a session-scoped lock for one DataView.
/// Dropping the detached connection releases the lock; it never returns to a pool.
pub struct PreparationPermit {
    connection: sqlx_postgres::PgConnection,
    pub(crate) task: Task,
    pub(crate) plan: PreparationPlan,
    pub(crate) generation: String,
}

impl PreparationPermit {
    pub fn plan(&self) -> &PreparationPlan {
        &self.plan
    }
    pub fn task(&self) -> &Task {
        &self.task
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub async fn check(&mut self) -> Result<()> {
        let row = query("SELECT t.document,floor(extract(epoch FROM clock_timestamp())*1000)::bigint AS now_ms,a.mode FROM research.tasks t CROSS JOIN research.authority a WHERE t.task_id=$1 AND a.singleton AND EXISTS(SELECT 1 FROM research.admissions d JOIN research.native_admission_imports n USING(request_sha256) WHERE d.request_sha256=t.request_sha256 AND n.tenant=t.tenant AND n.expires_ms>floor(extract(epoch FROM clock_timestamp())*1000)::bigint AND NOT EXISTS(SELECT 1 FROM research.revocations r WHERE r.request_sha256=d.request_sha256))")
            .bind(&self.task.id).fetch_one(&mut self.connection).await?;
        let current: Task = serde_json::from_value(row.get("document"))?;
        let now: i64 = row.get("now_ms");
        ensure!(
            row.get::<String, _>("mode") == "postgres"
                && current.state == State::Running
                && current.attempt == self.task.attempt
                && current.fence == self.task.fence
                && current.lease.as_ref().is_some_and(|l| l.expires_ms > now)
                && current.deadline_ms.is_some_and(|d| d > now),
            "preparation lease is no longer admitted"
        );
        self.task = current;
        Ok(())
    }
}

pub struct LockedTask {
    tx: Transaction<'static, Postgres>,
    revision: i64,
    pub task: Task,
    pub now_ms: i64,
}

async fn clock(tx: &mut Transaction<'_, Postgres>) -> Result<i64> {
    Ok(
        query_scalar::<_, i64>(
            "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint",
        )
        .fetch_one(&mut **tx)
        .await?,
    )
}

impl Ledger {
    #[cfg(feature = "gateway")]
    pub(crate) async fn artifact_write_permit(
        &self,
        tenant: &str,
        task: &str,
        attempt: u32,
        fence: i64,
    ) -> Result<ArtifactWritePermit> {
        let mut tx = self.pool.begin().await?;
        let prefix: String = query_scalar("SELECT research.artifact_write_permit($1,$2,$3,$4)")
            .bind(tenant)
            .bind(task)
            .bind(i32::try_from(attempt)?)
            .bind(fence)
            .fetch_one(&mut *tx)
            .await?;
        Ok(ArtifactWritePermit { prefix, _tx: tx })
    }

    #[cfg(all(test, feature = "gateway"))]
    pub(crate) fn gateway_fixture() -> Self {
        Self {
            pool: PgPoolOptions::new()
                .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/monday_foundation_test")
                .expect("fixture URL"),
        }
    }
    /// Read-only broker check. Tokens cannot keep a cancelled, revoked, expired
    /// or superseded Attempt alive. This never claims a task or enables PG.
    pub async fn artifact_writer(
        &self,
        tenant: &str,
        task: &str,
        attempt: u32,
        fence: i64,
    ) -> Result<String> {
        let row = query("SELECT t.document, floor(extract(epoch FROM clock_timestamp())*1000)::bigint AS now_ms FROM research.tasks t CROSS JOIN research.authority a WHERE t.task_id=$1 AND t.tenant=$2 AND a.singleton AND a.mode='postgres' AND EXISTS(SELECT 1 FROM research.admissions d JOIN research.native_admission_imports n USING(request_sha256) WHERE d.request_sha256=t.request_sha256 AND n.tenant=t.tenant AND n.expires_ms>floor(extract(epoch FROM clock_timestamp())*1000)::bigint AND NOT EXISTS(SELECT 1 FROM research.revocations r WHERE r.request_sha256=d.request_sha256))")
            .bind(task).bind(tenant).fetch_one(&self.pool).await?;
        let current: Task = serde_json::from_value(row.get("document"))?;
        let now_ms: i64 = row.get("now_ms");
        ensure!(
            current.state == State::Running
                && current.attempt == attempt
                && current.fence == fence
                && current
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.expires_ms > now_ms)
                && current.deadline_ms.is_some_and(|d| d > now_ms),
            "artifact writer is no longer admitted"
        );
        Ok(format!(
            "{}/{}/{}/",
            current.spec.output_prefix, current.id, attempt
        ))
    }

    pub async fn connect(url: &str) -> Result<Self> {
        // No migrations, authority changes or automatic backend enabling here.
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    async fn authority(tx: &mut Transaction<'_, Postgres>, exclusive: bool) -> Result<i32> {
        let sql = if exclusive {
            "SELECT mode, concurrency_limit, legacy_quiescence_sha256, migration_receipt_sha256 FROM research.authority WHERE singleton FOR UPDATE"
        } else {
            "SELECT mode, concurrency_limit, legacy_quiescence_sha256, migration_receipt_sha256 FROM research.authority WHERE singleton FOR SHARE"
        };
        let row = query(sql).fetch_one(&mut **tx).await?;
        ensure!(
            row.get::<String, _>("mode") == "postgres"
                && row
                    .get::<Option<String>, _>("legacy_quiescence_sha256")
                    .is_some_and(|s| crate::valid_digest(&s))
                && row
                    .get::<Option<String>, _>("migration_receipt_sha256")
                    .is_some_and(|s| crate::valid_digest(&s)),
            "PG authority is paused or legacy writer has not been retired"
        );
        Ok(row.get("concurrency_limit"))
    }

    pub async fn preparation(
        &self,
        id: &str,
        attempt: u32,
        fence: i64,
    ) -> Result<PreparationPermit> {
        let task = self.read(id).await?;
        ensure!(
            task.spec.kind == TaskKind::Prepare && task.attempt == attempt && task.fence == fence,
            "not the admitted preparation attempt"
        );
        let value: Value = query_scalar(
            "SELECT document FROM research.inputs WHERE manifest_sha256=$1 AND kind='plan'",
        )
        .bind(&task.spec.view_manifest_sha256)
        .fetch_one(&self.pool)
        .await?;
        let plan: PreparationPlan = serde_json::from_value(value)?;
        ensure!(
            plan.id()? == task.spec.view_manifest_sha256 && plan.producer_image == task.spec.image,
            "preparation plan changed"
        );
        let view_id = plan.spec.id()?;
        let lock_key = i64::from_str_radix(&view_id[..15], 16)?;
        let mut connection = self.pool.acquire().await?.detach();
        let acquired: bool = query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(lock_key)
            .fetch_one(&mut connection)
            .await?;
        ensure!(acquired, "DataView preparation is already owned");
        let exists: bool =
            query_scalar("SELECT EXISTS(SELECT 1 FROM research.views WHERE view_id=$1)")
                .bind(&view_id)
                .fetch_one(&mut connection)
                .await?;
        ensure!(
            !exists,
            "DataView is already published; reuse its immutable manifest"
        );
        let generation = identity(&(id, attempt, fence))?;
        let mut permit = PreparationPermit {
            connection,
            task,
            plan,
            generation,
        };
        permit.check().await?;
        Ok(permit)
    }

    pub async fn register_plan(&self, plan: &PreparationPlan) -> Result<String> {
        let id = plan.id()?;
        ensure!(
            plan.spec.split == hft_cex_research_input::data::Split::Train,
            "preparation worker supports only the training split"
        );
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, false).await?;
        query("INSERT INTO research.inputs(manifest_sha256,kind,document) VALUES($1,'plan',$2) ON CONFLICT DO NOTHING").bind(&id).bind(serde_json::to_value(plan)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn find_view(
        &self,
        spec: &hft_cex_research_input::data::DataViewSpec,
    ) -> Result<Option<PublishedView>> {
        let manifest: Option<String> =
            query_scalar("SELECT manifest_sha256 FROM research.views WHERE view_id=$1")
                .bind(spec.id()?)
                .fetch_optional(&self.pool)
                .await?;
        match manifest {
            Some(manifest) => Ok(Some(self.view(&manifest).await?)),
            None => Ok(None),
        }
    }

    pub async fn view(&self, manifest: &str) -> Result<PublishedView> {
        let value: Value =
            query_scalar("SELECT manifest FROM research.views WHERE manifest_sha256=$1")
                .bind(manifest)
                .fetch_one(&self.pool)
                .await?;
        let view: PublishedView = serde_json::from_value(value)?;
        view.verify(manifest)?;
        Ok(view)
    }

    pub async fn acceptance(&self, spec: &TaskSpec) -> Result<Acceptance> {
        let value: Value = query_scalar(
            "SELECT acceptance FROM research.backends WHERE acceptance_sha256=$1 AND enabled",
        )
        .bind(&spec.profile.acceptance_sha256)
        .fetch_one(&self.pool)
        .await?;
        let acceptance: Acceptance = serde_json::from_value(value)?;
        acceptance.admit(spec)?;
        Ok(acceptance)
    }

    pub async fn admission(
        &self,
        spec: &TaskSpec,
    ) -> Result<Option<crate::orchestrator::Admission>> {
        let row=query("SELECT a.document,n.document AS native_document,n.trust_document,n.trust_sha256,n.evidence_sha256,n.tenant,floor(extract(epoch FROM clock_timestamp())*1000)::bigint AS now_ms FROM research.admissions a JOIN research.native_admission_imports n USING(request_sha256) WHERE a.request_sha256=$1 AND n.expires_ms>floor(extract(epoch FROM clock_timestamp())*1000)::bigint AND NOT EXISTS(SELECT 1 FROM research.revocations r WHERE r.request_sha256=a.request_sha256)").bind(spec.id()?).fetch_optional(&self.pool).await?;
        let Some(row) = row else { return Ok(None) };
        let signed: crate::admission::SignedNativeAdmission =
            serde_json::from_value(row.get("native_document"))?;
        let trust: crate::admission::NativeAdmissionTrust =
            serde_json::from_value(row.get("trust_document"))?;
        let verified = trust.verify(&signed)?;
        ensure!(
            verified.trust_sha256() == row.get::<String, _>("trust_sha256")
                && signed.evidence_sha256 == row.get::<String, _>("evidence_sha256")
                && signed.evidence.tenant == row.get::<String, _>("tenant"),
            "native admission import identity changed"
        );
        verified.evidence().active_at(row.get("now_ms"))?;
        let value: Value = row.get("document");
        let admission: crate::orchestrator::Admission = serde_json::from_value(value)?;
        admission.validate(spec)?;
        ensure!(
            admission == signed.evidence.admission,
            "admission differs from native signed projection"
        );
        Ok(Some(admission))
    }

    pub async fn admits_launch(&self, spec: &TaskSpec, remaining_ms: i64) -> Result<bool> {
        ensure!(
            remaining_ms > 0 && remaining_ms <= spec.timeout_ms,
            "invalid remaining Job duration"
        );
        if self.admission(spec).await?.is_none() {
            return Ok(false);
        };
        let covers:bool=query_scalar("SELECT EXISTS(SELECT 1 FROM research.native_admission_imports WHERE request_sha256=$1 AND expires_ms>=floor(extract(epoch FROM clock_timestamp())*1000)::bigint+$2)")
            .bind(spec.id()?).bind(remaining_ms).fetch_one(&self.pool).await?;
        Ok(covers)
    }

    pub async fn register_experiment(
        &self,
        tenant: &str,
        experiment: &crate::research::Experiment,
    ) -> Result<String> {
        ensure!(
            !tenant.is_empty() && tenant.len() <= 128,
            "invalid principal"
        );
        let id = experiment.id()?;
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, false).await?;
        if let Some(parent) = &experiment.parent_experiment_sha256 {
            let exists:bool=query_scalar("SELECT EXISTS(SELECT 1 FROM research.experiments WHERE experiment_sha256=$1 AND tenant=$2)").bind(parent).bind(tenant).fetch_one(&mut *tx).await?;
            ensure!(exists && parent != &id, "unknown/foreign parent experiment");
        }
        query("INSERT INTO research.experiments(experiment_sha256,tenant,document) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&id).bind(tenant).bind(serde_json::to_value(experiment)?).execute(&mut *tx).await?;
        let owner: String =
            query_scalar("SELECT tenant FROM research.experiments WHERE experiment_sha256=$1")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            owner == tenant,
            "experiment already belongs to another principal"
        );
        tx.commit().await?;
        Ok(id)
    }
    /// Import the release verifier's signed proof. This can stage a Build while
    /// authority is paused; it neither admits a Run nor enables a backend.
    pub async fn register_build(
        &self,
        verified: &crate::release::VerifiedBuildRelease,
    ) -> Result<String> {
        let artifact = verified.artifact();
        let id = artifact.id()?;
        let mut tx = self.pool.begin().await?;
        query("SELECT mode FROM research.authority WHERE singleton FOR SHARE")
            .fetch_one(&mut *tx)
            .await?;
        query("INSERT INTO research.build_artifacts(artifact_sha256,build_sha256,document) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(&id).bind(artifact.build.id()?).bind(serde_json::to_value(artifact)?).execute(&mut *tx).await?;
        query("INSERT INTO research.build_releases(artifact_sha256,receipt_sha256,trust_sha256,document) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(&id).bind(&artifact.release_receipt_sha256).bind(verified.trust_sha256())
            .bind(serde_json::to_value(verified.signed())?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn build_artifact(&self, id: &str) -> Result<crate::build::BuildArtifact> {
        let row = query("SELECT a.document AS artifact,r.document AS release,r.receipt_sha256 FROM research.build_artifacts a JOIN research.build_releases r USING(artifact_sha256) WHERE artifact_sha256=$1")
            .bind(id).fetch_one(&self.pool).await?;
        let value: Value = row.get("artifact");
        let artifact: crate::build::BuildArtifact = serde_json::from_value(value)?;
        ensure!(artifact.id()? == id, "build artifact identity changed");
        let signed: crate::release::SignedBuildRelease =
            serde_json::from_value(row.get("release"))?;
        ensure!(
            row.get::<String, _>("receipt_sha256") == artifact.release_receipt_sha256,
            "stored release identity changed"
        );
        signed.validate_binding(&artifact)?;
        Ok(artifact)
    }
    pub async fn build_for_task(&self, task: &Task) -> Result<crate::build::BuildArtifact> {
        let value: Value = query_scalar("SELECT document FROM research.runs WHERE run_sha256=$1")
            .bind(&task.spec.run_manifest_sha256)
            .fetch_one(&self.pool)
            .await?;
        let run: crate::research::Run = serde_json::from_value(value)?;
        run.admit(&task.spec)?;
        let artifact = self.build_artifact(&run.build_artifact_sha256).await?;
        run.admit_build(&artifact)?;
        let arch = if artifact.build.target.starts_with("x86_64-") {
            "amd64"
        } else {
            "arm64"
        };
        ensure!(
            task.spec.profile.architecture == arch,
            "build/provider architecture mismatch"
        );
        Ok(artifact)
    }
    pub async fn register_run(&self, tenant: &str, run: &crate::research::Run) -> Result<String> {
        let id = run.id()?;
        run.admit_build(&self.build_artifact(&run.build_artifact_sha256).await?)?;
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, false).await?;
        let exists:bool=query_scalar("SELECT EXISTS(SELECT 1 FROM research.experiments WHERE experiment_sha256=$1 AND tenant=$2)").bind(&run.experiment_sha256).bind(tenant).fetch_one(&mut *tx).await?;
        ensure!(exists, "unknown/foreign experiment");
        query("INSERT INTO research.runs(run_sha256,experiment_sha256,build_artifact_sha256,tenant,document) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING").bind(&id).bind(&run.experiment_sha256).bind(&run.build_artifact_sha256).bind(tenant).bind(serde_json::to_value(run)?).execute(&mut *tx).await?;
        let owner: String = query_scalar("SELECT tenant FROM research.runs WHERE run_sha256=$1")
            .bind(&id)
            .fetch_one(&mut *tx)
            .await?;
        ensure!(owner == tenant, "run already belongs to another principal");
        tx.commit().await?;
        Ok(id)
    }
    pub async fn register_session(
        &self,
        tenant: &str,
        session: &crate::research::Session,
    ) -> Result<String> {
        let id = session.id()?;
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, false).await?;
        let exists:bool=query_scalar("SELECT EXISTS(SELECT 1 FROM research.experiments WHERE experiment_sha256=$1 AND tenant=$2)").bind(&session.experiment_sha256).bind(tenant).fetch_one(&mut *tx).await?;
        ensure!(exists, "unknown/foreign session experiment");
        query("INSERT INTO research.sessions(session_sha256,experiment_sha256,tenant,document) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(&id).bind(&session.experiment_sha256).bind(tenant).bind(serde_json::to_value(session)?).execute(&mut *tx).await?;
        let owner: String =
            query_scalar("SELECT tenant FROM research.sessions WHERE session_sha256=$1")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(owner == tenant, "session belongs to another principal");
        tx.commit().await?;
        Ok(id)
    }
    pub async fn session_for_tenant(
        &self,
        tenant: &str,
        id: &str,
    ) -> Result<crate::research::Session> {
        let value: Value = query_scalar(
            "SELECT document FROM research.sessions WHERE session_sha256=$1 AND tenant=$2",
        )
        .bind(id)
        .bind(tenant)
        .fetch_one(&self.pool)
        .await?;
        let session: crate::research::Session = serde_json::from_value(value)?;
        // The immutable record was validated by register_session. PG supplies
        // its primary key and tenant relationship; do not rehash it per poll.
        Ok(session)
    }
    /// Native terminal intents only. The exact terminal task revision is read
    /// back before delivery; a success-shaped caller payload cannot create one.
    pub async fn pending_completions(
        &self,
        tenant: &str,
        session: &str,
    ) -> Result<Vec<(String, Value)>> {
        self.session_for_tenant(tenant, session).await?;
        let rows: Vec<(String, Value)> = sqlx_core::query_as::query_as("SELECT i.intent_sha256,i.document FROM research.completion_intents i JOIN research.tasks t ON t.task_id=i.task_id AND t.tenant=$2 AND t.revision=i.terminal_revision WHERE i.session_sha256=$1 AND t.state IN ('succeeded','failed','cancelled','timed_out') AND NOT EXISTS(SELECT 1 FROM research.completion_deliveries d WHERE d.intent_sha256=i.intent_sha256) ORDER BY i.intent_sha256 LIMIT 32")
            .bind(session).bind(tenant).fetch_all(&self.pool).await?;
        for (id, value) in &rows {
            ensure!(
                crate::valid_digest(id)
                    && value.get("session_sha256").and_then(Value::as_str) == Some(session),
                "corrupt completion intent"
            );
        }
        Ok(rows)
    }
    pub async fn record_completion_delivery(
        &self,
        tenant: &str,
        session: &str,
        verified: &crate::session::VerifiedDelivery,
    ) -> Result<()> {
        let provider = self.session_for_tenant(tenant, session).await?;
        let record = verified.record();
        ensure!(
            provider.provider_thread_id == record.thread_id
                && provider.provider_binary_sha256 == verified.provider_binary_sha256(),
            "delivery belongs to another provider session"
        );
        let readback = record
            .native_readback_sha256
            .as_ref()
            .context("missing native readback")?;
        let mut tx = self.pool.begin().await?;
        let intent: Value = query_scalar("SELECT document FROM research.completion_intents WHERE intent_sha256=$1 AND session_sha256=$2 FOR UPDATE")
            .bind(&record.intent_sha256).bind(session).fetch_one(&mut *tx).await?;
        ensure!(
            verified.message() == completion_message(&record.intent_sha256, &intent)?,
            "completion payload identity changed"
        );
        query("INSERT INTO research.completion_deliveries(intent_sha256,process_generation,native_readback_sha256,document) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(&record.intent_sha256).bind(&record.process_generation).bind(readback).bind(serde_json::to_value(record)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn snapshot_session(
        &self,
        tenant: &str,
        snapshot: &crate::research::SessionSnapshot,
    ) -> Result<String> {
        let id = snapshot.id()?;
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, false).await?;
        let _:Value=query_scalar("SELECT document FROM research.sessions WHERE session_sha256=$1 AND tenant=$2 FOR UPDATE").bind(&snapshot.session_sha256).bind(tenant).fetch_one(&mut *tx).await?;
        let prior:Option<String>=query_scalar("SELECT snapshot_sha256 FROM research.session_snapshots WHERE session_sha256=$1 ORDER BY recorded_at DESC,snapshot_sha256 DESC LIMIT 1").bind(&snapshot.session_sha256).fetch_optional(&mut *tx).await?;
        if prior.as_ref() == Some(&id) {
            return Ok(id);
        };
        ensure!(
            prior == snapshot.parent_snapshot_sha256,
            "session snapshot does not extend current fixed checkpoint"
        );
        query("INSERT INTO research.session_snapshots(snapshot_sha256,session_sha256,parent_snapshot_sha256,document) VALUES($1,$2,$3,$4)").bind(&id).bind(&snapshot.session_sha256).bind(&snapshot.parent_snapshot_sha256).bind(serde_json::to_value(snapshot)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }
    pub async fn run_for_tenant(&self, tenant: &str, id: &str) -> Result<crate::research::Run> {
        let value: Value =
            query_scalar("SELECT document FROM research.runs WHERE run_sha256=$1 AND tenant=$2")
                .bind(id)
                .bind(tenant)
                .fetch_one(&self.pool)
                .await?;
        let run: crate::research::Run = serde_json::from_value(value)?;
        ensure!(run.id()? == id, "corrupt run identity");
        Ok(run)
    }
    pub async fn task_for_run(&self, tenant: &str, id: &str) -> Result<Task> {
        let value: Value = query_scalar(
            "SELECT document FROM research.tasks WHERE run_manifest_sha256=$1 AND tenant=$2",
        )
        .bind(id)
        .bind(tenant)
        .fetch_one(&self.pool)
        .await?;
        Ok(serde_json::from_value(value)?)
    }
    pub async fn result_for_run(&self, tenant: &str, id: &str) -> Result<Option<ResultReceipt>> {
        let value:Option<Value>=query_scalar("SELECT r.receipt FROM research.results r JOIN research.tasks t USING(task_id) WHERE t.run_manifest_sha256=$1 AND t.tenant=$2").bind(id).bind(tenant).fetch_optional(&self.pool).await?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }
    pub async fn approved_request(&self, tenant: &str, id: &str) -> Result<TaskSpec> {
        let value:Value=query_scalar("SELECT a.document FROM research.admissions a JOIN research.native_admission_imports n USING(request_sha256) WHERE a.request_sha256=$1 AND n.tenant=$2 AND n.expires_ms>floor(extract(epoch FROM clock_timestamp())*1000)::bigint AND NOT EXISTS(SELECT 1 FROM research.revocations r WHERE r.request_sha256=a.request_sha256)").bind(id).bind(tenant).fetch_one(&self.pool).await?;
        let admission: crate::orchestrator::Admission = serde_json::from_value(value)?;
        admission.validate(&admission.task_spec)?;
        self.admission(&admission.task_spec)
            .await?
            .context("native governance admission unavailable")?;
        ensure!(admission.task_spec.id()? == id, "request identity changed");
        self.run_for_tenant(tenant, &admission.task_spec.run_manifest_sha256)
            .await?
            .admit(&admission.task_spec)?;
        Ok(admission.task_spec)
    }

    pub async fn subscribe(&self, tenant: &str, session: &str, run: &str) -> Result<()> {
        self.run_for_tenant(tenant, run).await?;
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, true).await?;
        // Shares the terminal-commit lock; late subscribers read durable state.
        let document: Option<Value> = query_scalar(
            "SELECT document FROM research.tasks WHERE run_manifest_sha256=$1 FOR UPDATE",
        )
        .bind(run)
        .fetch_optional(&mut *tx)
        .await?;
        let owns: bool = query_scalar(
            "SELECT EXISTS(SELECT 1 FROM research.sessions WHERE session_sha256=$1 AND tenant=$2)",
        )
        .bind(session)
        .bind(tenant)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(owns, "unknown/foreign session");
        query("INSERT INTO research.subscriptions(session_sha256,run_sha256) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(session).bind(run).execute(&mut *tx).await?;
        if let Some(document) = document {
            let task: Task = serde_json::from_value(document)?;
            if task.state.terminal() {
                let revision: i64 =
                    query_scalar("SELECT revision FROM research.tasks WHERE task_id=$1")
                        .bind(&task.id)
                        .fetch_one(&mut *tx)
                        .await?;
                LockedTask::completion_intents(&mut tx, &task, revision).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn submit(&self, tenant: &str, key: &str, spec: TaskSpec) -> Result<String> {
        ensure!(
            !tenant.is_empty() && tenant.len() <= 128 && !key.is_empty() && key.len() <= 256,
            "invalid idempotency scope"
        );
        let task = Task::new(spec)?;
        let run = self
            .run_for_tenant(tenant, &task.spec.run_manifest_sha256)
            .await?;
        run.admit(&task.spec)?;
        self.build_for_task(&task).await?;
        ensure!(
            self.admission(&task.spec).await?.is_some(),
            "missing or revoked native governance admission"
        );
        self.acceptance(&task.spec).await?;
        let mut tx = self.pool.begin().await?;
        Self::authority(&mut tx, false).await?;
        let input = query("SELECT kind,document FROM research.inputs WHERE manifest_sha256=$1")
            .bind(&task.spec.view_manifest_sha256)
            .fetch_one(&mut *tx)
            .await?;
        let split = if task.spec.kind == TaskKind::Prepare {
            ensure!(
                input.get::<String, _>("kind") == "plan",
                "preparation input must be a registered plan"
            );
            let plan: PreparationPlan = serde_json::from_value(input.get("document"))?;
            ensure!(
                plan.id()? == task.spec.view_manifest_sha256
                    && plan.producer_image == task.spec.image,
                "plan identity mismatch"
            );
            ensure!(
                plan.spec.split == hft_cex_research_input::data::Split::Train,
                "preparation worker supports only the training split"
            );
            plan.spec.split
        } else if task.spec.kind == TaskKind::CexCampaign {
            ensure!(
                input.get::<String, _>("kind") == "cex_campaign",
                "native Campaign requires its typed collection"
            );
            let exists: bool = query_scalar("SELECT EXISTS(SELECT 1 FROM research.native_campaign_inputs WHERE request_sha256=$1 AND manifest_sha256=$2 AND tenant=$3)")
                .bind(&task.id).bind(&task.spec.view_manifest_sha256).bind(tenant).fetch_one(&mut *tx).await?;
            ensure!(
                exists,
                "native Campaign collection lacks exact tenant/request readback"
            );
            let collection: hft_cex_research_input::campaign::CampaignPreparedInputsV1 =
                serde_json::from_value(input.get("document"))?;
            ensure!(
                collection.id()? == task.spec.view_manifest_sha256,
                "native Campaign collection identity changed"
            );
            hft_cex_research_input::data::Split::Validation
        } else {
            ensure!(
                input.get::<String, _>("kind") == "prepared",
                "task requires published prepared input"
            );
            let view: PublishedView = serde_json::from_value(input.get("document"))?;
            view.verify(&task.spec.view_manifest_sha256)?;
            let exit = match task.spec.kind {
                TaskKind::Train => hft_cex_research_input::data::Exit::Training,
                TaskKind::Backtest => hft_cex_research_input::data::Exit::Replay,
                _ => hft_cex_research_input::data::Exit::Features,
            };
            ensure!(
                view.blocks.iter().any(|b| b.exit == exit),
                "published DataView lacks task exit"
            );
            view.spec.split
        };
        // This service has no sealed evaluation grant verifier. Holdout must
        // remain closed, rather than accepting a caller-supplied approval flag.
        ensure!(
            split != hft_cex_research_input::data::Split::Holdout,
            "sealed holdout requires separate governed evaluator admission"
        );
        query("INSERT INTO research.tasks(task_id,tenant,idempotency_key,request_sha256,view_manifest_sha256,state,document,run_manifest_sha256) VALUES($1,$2,$3,$1,$4,'queued',$5,$6) ON CONFLICT DO NOTHING")
            .bind(&task.id).bind(tenant).bind(key).bind(&task.spec.view_manifest_sha256).bind(serde_json::to_value(&task)?).bind(&task.spec.run_manifest_sha256).execute(&mut *tx).await?;
        let row = query("SELECT task_id,request_sha256,tenant,idempotency_key FROM research.tasks WHERE tenant=$1 AND idempotency_key=$2").bind(tenant).bind(key).fetch_optional(&mut *tx).await?.context("same scientific task is already bound to another submission scope")?;
        ensure!(
            row.get::<String, _>("request_sha256") == task.id,
            "idempotency key reused for another request"
        );
        tx.commit().await?;
        Ok(task.id)
    }

    pub async fn read(&self, id: &str) -> Result<Task> {
        let value: Value = query_scalar("SELECT document FROM research.tasks WHERE task_id=$1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Locks remain held through the bounded provider call. Lease/fence and PG
    /// row serialization prevent two reconcilers from launching/cancelling the
    /// same attempt concurrently. A crashed client rolls back; deterministic
    /// resource names make the next readback recover the same launch.
    pub async fn lock_next(&self, owner: &str, lease_ms: i64) -> Result<Option<LockedTask>> {
        let mut tx = self.pool.begin().await?;
        let authority = query("SELECT mode,concurrency_limit,legacy_quiescence_sha256,migration_receipt_sha256 FROM research.authority WHERE singleton FOR UPDATE").fetch_one(&mut *tx).await?;
        let admitted = authority.get::<String, _>("mode") == "postgres"
            && authority
                .get::<Option<String>, _>("legacy_quiescence_sha256")
                .is_some_and(|s| crate::valid_digest(&s))
            && authority
                .get::<Option<String>, _>("migration_receipt_sha256")
                .is_some_and(|s| crate::valid_digest(&s));
        let limit: i32 = authority.get("concurrency_limit");
        let now_ms = clock(&mut tx).await?;
        // Expired attempts count against quota until their process trees stop.
        let active: i64 = query_scalar(
            "SELECT count(*) FROM research.tasks WHERE state IN ('launching','running','stopping')",
        )
        .fetch_one(&mut *tx)
        .await?;
        if admitted && active < i64::from(limit) {
            let row = query("SELECT document,revision FROM research.tasks WHERE state='queued' ORDER BY created_at,task_id FOR UPDATE SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
            if let Some(row) = row {
                let mut task: Task = serde_json::from_value(row.get("document"))?;
                if task.expire(now_ms)? {
                    LockedTask {
                        tx,
                        revision: row.get("revision"),
                        task,
                        now_ms,
                    }
                    .commit("queued_deadline_expired")
                    .await?;
                    return Ok(None);
                }
                task.claim(owner, now_ms, lease_ms)?;
                let id = task.id.clone();
                // Persist the original attempt/deadline BEFORE provider I/O.
                LockedTask {
                    tx,
                    revision: row.get("revision"),
                    task,
                    now_ms,
                }
                .commit("claimed")
                .await?;
                return self.lock_id(&id).await.map(Some);
            }
        }
        let row = query("SELECT document,revision FROM research.tasks WHERE state IN ('launching','running','stopping') AND (state='stopping' OR document->'lease'->>'owner'=$1 OR (document->'lease'->>'expires_ms')::bigint <= $2) ORDER BY updated_at,task_id FOR UPDATE SKIP LOCKED LIMIT 1")
            .bind(owner).bind(now_ms).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut task: Task = serde_json::from_value(row.get("document"))?;
        // Pause stops new admission and drains existing attempts without retry.
        if !admitted && (task.state != State::Stopping || task.retry_after_stop) {
            task.stop(State::Cancelled, false)?;
        }
        Ok(Some(LockedTask {
            tx,
            revision: row.get("revision"),
            task,
            now_ms,
        }))
    }

    async fn lock_id(&self, id: &str) -> Result<LockedTask> {
        let mut tx = self.pool.begin().await?;
        let mode: String =
            query_scalar("SELECT mode FROM research.authority WHERE singleton FOR SHARE")
                .fetch_one(&mut *tx)
                .await?;
        let row = query("SELECT document,revision FROM research.tasks WHERE task_id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let now_ms = clock(&mut tx).await?;
        let mut task: Task = serde_json::from_value(row.get("document"))?;
        if mode != "postgres" && !task.state.terminal() {
            task.stop(State::Cancelled, false)?;
        }
        Ok(LockedTask {
            tx,
            revision: row.get("revision"),
            task,
            now_ms,
        })
    }

    pub async fn cancel(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let now_ms = clock(&mut tx).await?;
        let row = query("SELECT document,revision FROM research.tasks WHERE task_id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let mut task: Task = serde_json::from_value(row.get("document"))?;
        if task.state.terminal() {
            return Ok(());
        }
        task.stop(State::Cancelled, false)?;
        LockedTask {
            tx,
            revision: row.get("revision"),
            task,
            now_ms,
        }
        .commit("cancel_requested")
        .await
    }
}

impl LockedTask {
    pub async fn refresh_clock(&mut self) -> Result<i64> {
        self.now_ms = clock(&mut self.tx).await?;
        Ok(self.now_ms)
    }

    pub async fn commit(mut self, event: &str) -> Result<()> {
        if self.task.state == State::Succeeded {
            // Revocation inserts take a foreign-key lock on this admission.
            // This row lock orders them against terminal publication.
            let value: Value = query_scalar(
                "SELECT document FROM research.admissions WHERE request_sha256=$1 FOR UPDATE",
            )
            .bind(&self.task.id)
            .fetch_one(&mut *self.tx)
            .await?;
            let admission: crate::orchestrator::Admission = serde_json::from_value(value)?;
            admission.validate(&self.task.spec)?;
            let native:Value=query_scalar("SELECT document FROM research.native_admission_imports WHERE request_sha256=$1 AND expires_ms>floor(extract(epoch FROM clock_timestamp())*1000)::bigint")
                .bind(&self.task.id).fetch_one(&mut *self.tx).await?;
            let native: crate::admission::SignedNativeAdmission = serde_json::from_value(native)?;
            ensure!(
                native.evidence.admission == admission,
                "terminal result changed native admission"
            );
            let revoked: bool = query_scalar(
                "SELECT EXISTS(SELECT 1 FROM research.revocations WHERE request_sha256=$1)",
            )
            .bind(&self.task.id)
            .fetch_one(&mut *self.tx)
            .await?;
            ensure!(!revoked, "terminal publication admission was revoked");
        }
        let revision = self.revision.checked_add(1).context("revision overflow")?;
        let document = serde_json::to_value(&self.task)?;
        let updated = query("UPDATE research.tasks SET state=$1,document=$2,revision=$3,updated_at=clock_timestamp() WHERE task_id=$4 AND revision=$5")
            .bind(self.task.state.as_str()).bind(&document).bind(revision).bind(&self.task.id).bind(self.revision).execute(&mut *self.tx).await?;
        ensure!(updated.rows_affected() == 1, "task fence/revision changed");
        query("INSERT INTO research.events(task_id,revision,event,document) VALUES($1,$2,$3,$4)")
            .bind(&self.task.id)
            .bind(revision)
            .bind(event)
            .bind(document)
            .execute(&mut *self.tx)
            .await?;
        if let Some(receipt) = &self.task.receipt {
            ensure!(
                matches!(self.task.state, State::Stopping | State::Succeeded),
                "result without completion state"
            );
            if self.task.state == State::Succeeded {
                if let Some(view) = &receipt.prepared_view {
                    let plan: Value = query_scalar("SELECT document FROM research.inputs WHERE manifest_sha256=$1 AND kind='plan'").bind(&self.task.spec.view_manifest_sha256).fetch_one(&mut *self.tx).await?;
                    let plan: PreparationPlan = serde_json::from_value(plan)?;
                    ensure!(
                        plan.spec == view.spec
                            && plan.source_receipt_sha256 == view.source_receipt_sha256
                            && plan.producer_image == view.producer_image,
                        "prepared publication changed its fixed plan"
                    );
                    let manifest_sha = identity(view)?;
                    query("INSERT INTO research.inputs(manifest_sha256,kind,document) VALUES($1,'prepared',$2) ON CONFLICT DO NOTHING").bind(&manifest_sha).bind(serde_json::to_value(view)?).execute(&mut *self.tx).await?;
                    query("INSERT INTO research.views(view_id,manifest_sha256,manifest,source_receipt_sha256) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING").bind(view.spec.id()?).bind(&manifest_sha).bind(serde_json::to_value(view)?).bind(&view.source_receipt_sha256).execute(&mut *self.tx).await?;
                    let existing: String =
                        query_scalar("SELECT manifest_sha256 FROM research.views WHERE view_id=$1")
                            .bind(view.spec.id()?)
                            .fetch_one(&mut *self.tx)
                            .await?;
                    ensure!(
                        existing == manifest_sha,
                        "DataView was published with conflicting bytes"
                    );
                }
                Self::persist_result(&mut self.tx, receipt).await?;
            }
        }
        if self.task.state.terminal() {
            Self::completion_intents(&mut self.tx, &self.task, revision).await?;
        }
        self.tx.commit().await?;
        Ok(())
    }

    async fn completion_intents(
        tx: &mut Transaction<'_, Postgres>,
        task: &Task,
        revision: i64,
    ) -> Result<()> {
        let sessions: Vec<String> =
            query_scalar("SELECT session_sha256 FROM research.subscriptions WHERE run_sha256=$1")
                .bind(&task.spec.run_manifest_sha256)
                .fetch_all(&mut **tx)
                .await?;
        for session in sessions {
            let intent = serde_json::json!({"session_sha256":session,"run_sha256":task.spec.run_manifest_sha256,"task_id":task.id,"terminal_revision":revision,"state":task.state});
            query("INSERT INTO research.completion_intents(intent_sha256,session_sha256,run_sha256,task_id,terminal_revision,document) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(session_sha256,run_sha256) DO NOTHING").bind(identity(&intent)?).bind(&session).bind(&task.spec.run_manifest_sha256).bind(&task.id).bind(revision).bind(intent).execute(&mut **tx).await?;
        }
        Ok(())
    }
    async fn persist_result(
        tx: &mut Transaction<'_, Postgres>,
        receipt: &ResultReceipt,
    ) -> Result<()> {
        query("INSERT INTO research.results(task_id,attempt,fence,receipt_sha256,receipt) VALUES($1,$2,$3,$4,$5)")
            .bind(&receipt.task_id).bind(i32::try_from(receipt.attempt)?).bind(receipt.fence).bind(identity(receipt)?).bind(serde_json::to_value(receipt)?).execute(&mut **tx).await?;
        Ok(())
    }
}
