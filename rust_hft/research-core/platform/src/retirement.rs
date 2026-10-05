//! Mechanical retirement of retained native Jobs. No execution, refund, grant,
//! scientific validation or Agent capability is issued here. Publication and
//! provider absence are independent readbacks; signatures alone cannot delete.
use crate::{
    execution::{Acceptance, Backend, ExecutionHandle, Kubernetes},
    identity,
    orchestrator::{Artifact, AttemptContext, Lease, State, TaskKind},
    postgres::Ledger,
    research::NativeTerminalSnapshot,
    service::ArtifactGateway,
    sha256,
    terminal_audit::{SignedNativeTerminalAuditWitness, VerifiedNativeTerminalAuditWitness},
    valid_digest,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row, transaction::Transaction};
use sqlx_postgres::Postgres;
use std::collections::{BTreeMap, BTreeSet};

pub const MIGRATION: &str = include_str!("../sql/native_terminal_retirement.sql");
const FILE_LIMIT: u64 = 512 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

pub(crate) fn source_receipt_key(family: &str, sequence: u64) -> Result<String> {
    ensure!(
        !family.is_empty()
            && family.len() <= 128
            && sequence > 0
            && family
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c)),
        "invalid Source family receipt scope"
    );
    Ok(format!(
        "research/campaign-ledger/family-id={family}/sequence={sequence:020}/receipt.json"
    ))
}
pub(crate) fn source_receipt_key_valid(key: &str) -> bool {
    let parts: Vec<_> = key.split('/').collect();
    let ["research", "campaign-ledger", family, sequence, "receipt.json"] = parts.as_slice() else {
        return false;
    };
    let Some(family) = family.strip_prefix("family-id=") else {
        return false;
    };
    let Some(sequence) = sequence.strip_prefix("sequence=") else {
        return false;
    };
    sequence.len() == 20
        && sequence.bytes().all(|b| b.is_ascii_digit())
        && sequence
            .parse::<u64>()
            .ok()
            .and_then(|n| source_receipt_key(family, n).ok())
            .is_some_and(|v| v == key)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementConfig {
    #[serde(default)]
    pub enabled: bool,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetirementRequest {
    pub tenant: String,
    pub task_id: String,
    pub witness_sha256: String,
    pub receipt_sequence: u64,
}
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetirementOutcome {
    Pending,
    Retired,
}

struct Scope {
    snapshot: NativeTerminalSnapshot,
    collection: Value,
    lease: Lease,
    handle: ExecutionHandle,
    acceptance: Acceptance,
}
impl Scope {
    fn new(
        snapshot: NativeTerminalSnapshot,
        collection: Value,
        acceptance: Acceptance,
    ) -> Result<Self> {
        let task = &snapshot.task;
        let event = snapshot
            .execution_event
            .as_ref()
            .context("terminal lacks original execution")?;
        let executed = &event.document;
        let reference = executed
            .attempt_identity
            .as_ref()
            .context("terminal lacks original controlled launch identity")?;
        let execution_lease = executed.lease.as_ref().context("execution lease missing")?;
        reference.validate(&task.spec, execution_lease)?;
        let lease = reference.launch_lease.clone();
        let handle = executed
            .execution
            .as_ref()
            .context("original execution handle missing")?
            .clone();
        AttemptContext {
            spec: task.spec.clone(),
            lease: lease.clone(),
        }
        .validate()?;
        handle.validate(&lease, &task.spec)?;
        ensure!(
            snapshot.schema == "monday.native_platform_terminal_snapshot.v1"
                && task.spec.kind == TaskKind::CexCampaign
                && task.spec.max_attempts == 1
                && matches!(handle.backend, Backend::KubernetesJob | Backend::AcsJob)
                && task.state.terminal()
                && task.attempt == 1
                && task.lease.is_none()
                && task.execution.is_none()
                && !task.retry_after_stop
                && snapshot.terminal_event.event == "stop_reconciled"
                && snapshot.terminal_event.document == *task
                && snapshot.terminal_event.revision == snapshot.terminal_revision
                && event.revision > 0
                && event.revision < snapshot.terminal_revision
                && executed.id == task.id
                && executed.spec == task.spec
                && executed.attempt == task.attempt
                && executed.fence == task.fence
                && task.attempt_identity.as_ref() == Some(reference)
                && reference.native_evidence_sha256 == snapshot.native_admission.evidence_sha256
                && reference.deadline_ms <= snapshot.native_admission.evidence.expires_ms
                && executed
                    .deadline_ms
                    .is_some_and(|d| reference.deadline_ms <= d),
            "retirement changed reconciled terminal or original launch scope"
        );
        let inputs: hft_cex_research_input::campaign::CampaignPreparedInputsV1 =
            serde_json::from_value(collection.clone())?;
        ensure!(
            inputs.id()? == task.spec.view_manifest_sha256,
            "stored Campaign collection changed"
        );
        acceptance.admit(&task.spec)?;
        Ok(Self {
            snapshot,
            collection,
            lease,
            handle,
            acceptance,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedManifest {
    schema: String,
    files: BTreeMap<String, FileIdentity>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    sha256: String,
    bytes: u64,
}
fn relative_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('/')
        && !name.contains("..")
        && name.split('/').all(|p| !p.is_empty())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
}
impl RetainedManifest {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == "monday.native_terminal_observation_files.v1"
                && (5..=520).contains(&self.files.len()),
            "invalid retained manifest"
        );
        let mut total = 0_u64;
        for (name, file) in &self.files {
            ensure!(
                relative_name(name)
                    && !matches!(
                        name.as_str(),
                        "terminal-audit.json" | "retained-manifest.json"
                    )
                    && valid_digest(&file.sha256)
                    && file.bytes > 0
                    && file.bytes <= FILE_LIMIT,
                "unsafe retained file identity"
            );
            total = total
                .checked_add(file.bytes)
                .context("retained size overflow")?;
            ensure!(
                total <= TOTAL_LIMIT,
                "retained archive exceeds readback budget"
            );
        }
        Ok(())
    }
}

/// Only actual bounded archive GETs construct this value. It cannot be restored
/// from caller JSON. PG verifies its immutable scope again before a permit.
struct PublishedAudit {
    scope: Scope,
    verified: VerifiedNativeTerminalAuditWitness,
    audit: Value,
    receipt_sequence: u64,
    recorded_at: String,
    job: Value,
    pod: Value,
}
impl PublishedAudit {
    fn document(&self) -> Result<Value> {
        Ok(
            json!({"schema":"monday.native_terminal_retirement_import.v1", "signed":self.verified.signed(), "trust_sha256":self.verified.trust_sha256(), "snapshot_sha256":identity(&self.scope.snapshot)?, "receipt_sequence":self.receipt_sequence, "audit":self.audit}),
        )
    }
}

async fn required(gateway: &ArtifactGateway, key: &str, max: u64) -> Result<Vec<u8>> {
    gateway
        .get(key, max)
        .await?
        .context("published terminal object missing")
}
fn transfer_matches(transfer: &Value, scope: &Scope) -> Result<()> {
    let native = &scope.snapshot.native_admission.evidence;
    let operation = transfer["operation_id"]
        .as_str()
        .context("Source operation missing")?;
    ensure!(
        identity(&operation)? == native.operation_sha256
            && transfer["tenant"] == native.tenant
            && transfer["request_sha256"] == scope.snapshot.task.id
            && transfer["run_sha256"] == scope.snapshot.run.id()?,
        "published transfer changed native operation/tenant/Run/request"
    );
    Ok(())
}
async fn readback(
    gateway: &ArtifactGateway,
    scope: Scope,
    witness_id: &str,
    sequence: u64,
) -> Result<PublishedAudit> {
    ensure!(
        valid_digest(witness_id),
        "invalid terminal witness identity"
    );
    let native = &scope.snapshot.native_admission.evidence;
    let signed_key = format!(
        "research/native-terminal-audits/{}/signed-{witness_id}.json",
        native.operation_sha256
    );
    let signed: SignedNativeTerminalAuditWitness =
        serde_json::from_slice(&required(gateway, &signed_key, 1024 * 1024).await?)?;
    let verified = scope.snapshot.native_trust.verify_terminal_audit(&signed)?;
    let witness = verified.evidence();
    ensure!(
        signed.evidence_sha256 == witness_id
            && witness.tenant == scope.snapshot.tenant
            && witness.operation_sha256 == native.operation_sha256
            && witness.request_sha256 == scope.snapshot.task.id
            && witness.task_id == scope.snapshot.task.id
            && witness.run_sha256 == scope.snapshot.run.id()?
            && witness.native_evidence_sha256 == scope.snapshot.native_admission.evidence_sha256
            && witness.attempt == scope.handle.attempt
            && witness.fence == scope.handle.fence
            && witness.job_uid == scope.handle.uid
            && witness.terminal_revision == scope.snapshot.terminal_revision,
        "published audit witness changed fixed PG scope"
    );
    let receipt_bytes = gateway.family_receipt(&native.family_id, sequence).await?;
    ensure!(
        sha256(&receipt_bytes) == witness.audit_receipt_sha256,
        "actual Source receipt bytes changed"
    );
    let wrapper: Value = serde_json::from_slice(&receipt_bytes)?;
    let receipt = &wrapper["receipt"];
    let recorded_at = receipt["recorded_at"]
        .as_str()
        .context("Source receipt time missing")?
        .to_owned();
    ensure!(
        receipt["schema_version"] == "monday.campaign_ledger_receipt.v1"
            && receipt["family_id"] == native.family_id
            && receipt["sequence"].as_u64() == Some(sequence)
            && receipt["event"]["kind"] == "platform_settled"
            && wrapper["content_sha256"].as_str().is_some_and(valid_digest)
            && wrapper["auth_tag"].as_str().is_some_and(valid_digest)
            && recorded_at.len() >= 20
            && recorded_at.len() <= 35
            && recorded_at.contains('T')
            && recorded_at.ends_with('Z'),
        "published Source receipt changed family/event/time"
    );
    let audit = receipt["event"]["audit"].clone();
    transfer_matches(&audit["transfer"], &scope)?;
    ensure!(
        audit["schema_version"] == "monday.campaign_platform_terminal_audit.v1"
            && audit["retained_manifest_sha256"] == witness.retained_manifest_sha256
            && audit["native_admission_sha256"] == identity(&scope.snapshot.native_admission)?
            && audit["native_trust_sha256"] == identity(&scope.snapshot.native_trust)?
            && audit["collection_sha256"] == scope.snapshot.task.spec.view_manifest_sha256
            && audit["task_id"] == witness.task_id
            && audit["attempt"].as_u64() == Some(u64::from(witness.attempt))
            && audit["fence"].as_i64() == Some(witness.fence)
            && audit["terminal_revision"].as_i64() == Some(witness.terminal_revision)
            && audit["job_uid"] == witness.job_uid
            && audit["pod_uid"] == witness.pod_uid
            && audit["terminal_event_sha256"] == identity(&scope.snapshot.terminal_event)?
            && audit["execution_event_sha256"] == identity(&scope.snapshot.execution_event)?
            && audit["platform_state"] == serde_json::to_value(scope.snapshot.task.state)?
            && audit["charging_trials"].as_u64() == Some(native.declared_trials)
            && audit["observer_release_sha256"]
                .as_str()
                .is_some_and(valid_digest),
        "Source audit changed terminal/native/source scope"
    );
    // Source and receiver share this Value-based grouping hash. The actual
    // receipt/manifest bytes retain their separate, unmodified raw digests.
    let prefix = format!(
        "research/native-terminal-audits/{}/{}",
        native.operation_sha256,
        identity(&audit)?
    );
    let audit_bytes = required(
        gateway,
        &format!("{prefix}/terminal-audit.json"),
        1024 * 1024,
    )
    .await?;
    ensure!(
        serde_json::from_slice::<Value>(&audit_bytes)? == audit,
        "raw audit differs from published Source receipt"
    );
    let manifest_bytes = required(
        gateway,
        &format!("{prefix}/retained-manifest.json"),
        1024 * 1024,
    )
    .await?;
    ensure!(
        sha256(&manifest_bytes) == witness.retained_manifest_sha256,
        "actual retained manifest changed"
    );
    let manifest: RetainedManifest = serde_json::from_slice(&manifest_bytes)?;
    manifest.validate()?;
    let mut metadata = BTreeMap::new();
    for (name, file) in &manifest.files {
        let artifact = Artifact {
            key: format!("{prefix}/{name}"),
            sha256: file.sha256.clone(),
            bytes: file.bytes,
        };
        if matches!(
            name.as_str(),
            "platform-snapshot.json"
                | "job.json"
                | "pod.json"
                | "source-transfer.json"
                | "prepared-inputs.json"
                | "cex-campaign.json"
        ) {
            let cap = if name == "prepared-inputs.json" {
                64 * 1024 * 1024
            } else {
                2 * 1024 * 1024
            };
            ensure!(file.bytes <= cap, "terminal metadata exceeds bound");
            let bytes = required(gateway, &artifact.key, artifact.bytes).await?;
            ensure!(
                bytes.len() as u64 == artifact.bytes && sha256(&bytes) == artifact.sha256,
                "actual retained metadata changed"
            );
            metadata.insert(
                name.clone(),
                (bytes.clone(), serde_json::from_slice::<Value>(&bytes)?),
            );
        } else {
            gateway.verify_artifact(&artifact).await?;
        }
    }
    let get = |name: &str| {
        metadata
            .get(name)
            .context("required retained metadata missing")
    };
    let snapshot = get("platform-snapshot.json")?;
    ensure!(
        sha256(&snapshot.0) == audit["platform_snapshot_sha256"]
            && snapshot.1 == serde_json::to_value(&scope.snapshot)?,
        "archive differs from actual PG terminal snapshot"
    );
    ensure!(
        get("prepared-inputs.json")?.1 == scope.collection,
        "archive changed imported native collection"
    );
    let source_transfer = get("source-transfer.json")?;
    ensure!(
        sha256(&source_transfer.0) == native.transfer_receipt_sha256,
        "original published transfer receipt changed"
    );
    transfer_matches(&source_transfer.1["receipt"]["event"]["transfer"], &scope)?;
    ensure!(
        source_transfer.1["receipt"]["family_id"] == native.family_id
            && source_transfer.1["receipt"]["event"]["kind"] == "platform_transferred",
        "archive changed original Source transfer event"
    );
    let job = get("job.json")?.1.clone();
    let pod = get("pod.json")?.1.clone();
    ensure!(
        identity(&job)? == audit["job_sha256"]
            && identity(&pod)? == audit["pod_sha256"]
            && pod["metadata"]["uid"] == witness.pod_uid,
        "archived provider identity changed"
    );
    crate::execution::verify_retirement_observation(
        &scope.snapshot.task.spec,
        &scope.lease,
        &scope.handle,
        &scope.acceptance,
        scope
            .snapshot
            .task
            .attempt_identity
            .as_ref()
            .context("launch identity absent")?,
        &job,
        &pod,
    )?;
    let mut expected: BTreeSet<String> = [
        "platform-snapshot.json",
        "job.json",
        "pod.json",
        "prepared-inputs.json",
        "source-transfer.json",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    if scope.snapshot.task.state == State::Succeeded {
        let result = scope
            .snapshot
            .result
            .as_ref()
            .context("successful terminal lacks immutable result")?;
        let campaign: crate::campaign_result::CexCampaignResultReceipt =
            serde_json::from_value(get("cex-campaign.json")?.1.clone())?;
        campaign.validate(&scope.snapshot.task.spec, &result.artifacts)?;
        ensure!(
            audit["scientific_status"] == "insufficient_evidence"
                && audit["known_scientific_consumption"].as_u64()
                    == Some(campaign.actual_consumed_trials)
                && audit["native_result_sha256"] == campaign.campaign_result.artifact.sha256
                && campaign.declared_trials == native.declared_trials,
            "successful Source audit changed original native result evidence"
        );
        expected.insert("cex-campaign.json".into());
        expected.insert("campaign-result.json".into());
        let campaign_meta: Vec<_> = result
            .artifacts
            .iter()
            .filter(|a| a.key.ends_with("/cex-campaign.json"))
            .collect();
        ensure!(campaign_meta.len() == 1, "ambiguous native result metadata");
        exact_file(
            &manifest,
            "cex-campaign.json",
            &campaign_meta[0].sha256,
            Some(campaign_meta[0].bytes),
        )?;
        exact_file(
            &manifest,
            "campaign-result.json",
            &campaign.campaign_result.artifact.sha256,
            Some(campaign.campaign_result.artifact.bytes),
        )?;
        for (index, round) in campaign.rounds.iter().enumerate() {
            let mission = format!("round-readback/round-{index}-mission.json");
            let zip = format!("round-readback/round-{index}-results.zip");
            exact_file(&manifest, &mission, &round.native_mission_sha256, None)?;
            exact_file(
                &manifest,
                &zip,
                &round.result_zip.artifact.sha256,
                Some(round.result_zip.artifact.bytes),
            )?;
            expected.insert(mission);
            expected.insert(zip);
        }
    } else {
        ensure!(
            audit["scientific_status"] == "unknown"
                && audit["known_scientific_consumption"].is_null()
                && audit["native_result_sha256"].is_null(),
            "negative compute fabricated scientific success"
        );
    }
    ensure!(
        manifest.files.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "retained archive coverage differs from original terminal"
    );
    Ok(PublishedAudit {
        scope,
        verified,
        audit,
        receipt_sequence: sequence,
        recorded_at,
        job,
        pod,
    })
}
fn exact_file(
    manifest: &RetainedManifest,
    name: &str,
    digest: &str,
    bytes: Option<u64>,
) -> Result<()> {
    let file = manifest
        .files
        .get(name)
        .context("required native raw artifact missing")?;
    ensure!(
        file.sha256 == digest && bytes.is_none_or(|n| n == file.bytes),
        "retained native artifact changed original output"
    );
    Ok(())
}

/// The durable fixed audit and delete intent already exist. The live task lock
/// serializes retirement consumers. No JSON constructor or execution authority.
pub struct RetirementPermit {
    tx: Transaction<'static, Postgres>,
    published: PublishedAudit,
}
impl RetirementPermit {
    pub(crate) fn spec(&self) -> &crate::orchestrator::TaskSpec {
        &self.published.scope.snapshot.task.spec
    }
    pub(crate) fn lease(&self) -> &Lease {
        &self.published.scope.lease
    }
    pub(crate) fn handle(&self) -> &ExecutionHandle {
        &self.published.scope.handle
    }
    pub(crate) fn job(&self) -> &Value {
        &self.published.job
    }
    pub(crate) fn pod(&self) -> &Value {
        &self.published.pod
    }
    pub(crate) fn acceptance(&self) -> &Acceptance {
        &self.published.scope.acceptance
    }
    pub(crate) fn identity(&self) -> &crate::execution::AttemptIdentityRef {
        self.published
            .scope
            .snapshot
            .task
            .attempt_identity
            .as_ref()
            .expect("verified scope")
    }
    async fn finish(mut self, retired: bool) -> Result<RetirementOutcome> {
        if retired {
            query("INSERT INTO research.native_terminal_retirement_events(evidence_sha256,event,document) VALUES($1,'retired',$2) ON CONFLICT DO NOTHING")
                .bind(&self.published.verified.signed().evidence_sha256).bind(retirement_event(&self.published)).execute(&mut *self.tx).await?;
        }
        self.tx.commit().await?;
        Ok(if retired {
            RetirementOutcome::Retired
        } else {
            RetirementOutcome::Pending
        })
    }
}
fn retirement_event(published: &PublishedAudit) -> Value {
    let witness = published.verified.evidence();
    json!({"task_id":witness.task_id,"terminal_revision":witness.terminal_revision,"job_uid":witness.job_uid,"pod_uid":witness.pod_uid})
}
impl Ledger {
    async fn retirement_scope(&self, tenant: &str, task: &str) -> Result<Scope> {
        let snapshot = self.native_terminal_snapshot(tenant, task).await?;
        let collection: Value = query_scalar("SELECT i.document FROM research.native_campaign_inputs c JOIN research.inputs i ON i.manifest_sha256=c.manifest_sha256 WHERE c.request_sha256=$1 AND c.tenant=$2 AND c.manifest_sha256=$3 AND i.kind='cex_campaign'")
            .bind(task).bind(tenant).bind(&snapshot.task.spec.view_manifest_sha256).fetch_one(&self.pool).await?;
        // Disabling a backend cannot issue execution, and does not erase its
        // historical profile acceptance needed to check an original terminal.
        let acceptance: Value =
            query_scalar("SELECT acceptance FROM research.backends WHERE acceptance_sha256=$1")
                .bind(&snapshot.task.spec.profile.acceptance_sha256)
                .fetch_one(&self.pool)
                .await?;
        Scope::new(snapshot, collection, serde_json::from_value(acceptance)?)
    }
    async fn retirement_permit(&self, published: PublishedAudit) -> Result<RetirementPermit> {
        let scope = &published.scope;
        let mut tx = self.pool.begin().await?;
        lock_terminal(&mut tx, &scope.snapshot).await?;
        let issued_ms: i64 =
            query_scalar("SELECT floor(extract(epoch FROM $1::timestamptz)*1000)::bigint")
                .bind(&published.recorded_at)
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            issued_ms == published.verified.evidence().issued_ms,
            "Source receipt time differs from signed witness"
        );
        let now: i64 =
            query_scalar("SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint")
                .fetch_one(&mut *tx)
                .await?;
        ensure!(
            issued_ms >= scope.snapshot.native_admission.evidence.issued_ms && issued_ms <= now,
            "terminal audit predates native admission or is in the future"
        );
        let document = published.document()?;
        let witness = published.verified.evidence();
        let id = &published.verified.signed().evidence_sha256;
        query("INSERT INTO research.native_terminal_retirement_audits(evidence_sha256,task_id,tenant,terminal_revision,document) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(id).bind(&witness.task_id).bind(&witness.tenant).bind(witness.terminal_revision).bind(&document).execute(&mut *tx).await?;
        let stored: Value = query_scalar("SELECT document FROM research.native_terminal_retirement_audits WHERE task_id=$1 AND evidence_sha256=$2")
            .bind(&witness.task_id).bind(id).fetch_one(&mut *tx).await?;
        ensure!(
            stored == document,
            "terminal already registered a different audit or signed identity"
        );
        query("INSERT INTO research.native_terminal_retirement_events(evidence_sha256,event,document) VALUES($1,'delete_requested',$2) ON CONFLICT DO NOTHING")
            .bind(id).bind(retirement_event(&published)).execute(&mut *tx).await?;
        // Persist the exact intent before the first provider mutation. A crash
        // resumes this same witness/UID, never a replacement operation.
        tx.commit().await?;
        let mut tx = self.pool.begin().await?;
        lock_terminal(&mut tx, &scope.snapshot).await?;
        Ok(RetirementPermit { tx, published })
    }
}
async fn lock_terminal(
    tx: &mut Transaction<'_, Postgres>,
    snapshot: &NativeTerminalSnapshot,
) -> Result<()> {
    let row = query(
        "SELECT document,revision FROM research.tasks WHERE task_id=$1 AND tenant=$2 FOR UPDATE",
    )
    .bind(&snapshot.task.id)
    .bind(&snapshot.tenant)
    .fetch_one(&mut **tx)
    .await?;
    ensure!(
        row.get::<Value, _>("document") == serde_json::to_value(&snapshot.task)?
            && row.get::<i64, _>("revision") == snapshot.terminal_revision,
        "PG terminal changed before retirement"
    );
    Ok(())
}

/// Operator-only bounded consumer. Agent APIs and the ordinary reconciliation
/// tick expose no retirement path. Historical expiry cannot issue execution.
pub async fn retire(
    config: &RetirementConfig,
    ledger: &Ledger,
    kubernetes: &Kubernetes,
    gateway: &ArtifactGateway,
    request: &RetirementRequest,
) -> Result<RetirementOutcome> {
    ensure!(config.enabled, "native terminal retirement is disabled");
    let scope = ledger
        .retirement_scope(&request.tenant, &request.task_id)
        .await?;
    let published = readback(
        gateway,
        scope,
        &request.witness_sha256,
        request.receipt_sequence,
    )
    .await?;
    let permit = ledger.retirement_permit(published).await?;
    let retired = kubernetes.retire_native_terminal(&permit).await?;
    permit.finish(retired).await
}

#[cfg(test)]
mod tests;
