//! Host-only issuance for an already admitted, leased PG Attempt.
//! Signatures and budget authority belong to the native source producer.
use crate::{identity, sha256, valid_digest};
use anyhow::{ensure, Context, Result};
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub token_sha256: String,
    pub expires_ms: u64,
    pub access: Access,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum Access {
    Reader {
        prefixes: Vec<String>,
    },
    AttemptWriter {
        tenant: String,
        task_id: String,
        attempt: u32,
        fence: i64,
    },
    /// Existing trusted publisher only; cannot write scientific outputs.
    Publisher {
        prefixes: Vec<String>,
    },
}

fn key_valid(key: &str) -> bool {
    key.starts_with("research/")
        && key.len() <= 2048
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        && key.split('/').count() <= 32
        && key
            .split('/')
            .all(|p| !p.is_empty() && p.len() <= 255 && !p.starts_with('.'))
}
fn prefix_valid(prefix: &str) -> bool {
    prefix.strip_suffix('/').is_some_and(key_valid)
        || (prefix.ends_with('/')
            && crate::retirement::source_receipt_key_valid(&format!("{prefix}receipt.json")))
}

pub(crate) fn read_capabilities(path: &std::path::Path) -> Result<Vec<Capability>> {
    private_directory(path.parent().context("projection parent absent")?)?;
    // The broker owns this file and its private parent, never the Agent.
    // O_NOFOLLOW excludes a substituted symlink even during atomic reload.
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = File::from(fd);
    let meta = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "capability file must be private"
        );
    }
    ensure!(
        meta.is_file() && meta.len() <= 1024 * 1024,
        "invalid broker projection"
    );
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "broker projection exceeds bound"
    );
    let caps: Vec<Capability> = serde_json::from_slice(&bytes)?;
    ensure!(caps.len() <= 1024, "invalid capability count");
    let mut seen = std::collections::BTreeSet::new();
    for cap in &caps {
        ensure!(
            valid_digest(&cap.token_sha256) && seen.insert(&cap.token_sha256),
            "invalid or duplicate capability"
        );
        match &cap.access {
            Access::Reader { prefixes } | Access::Publisher { prefixes } => {
                ensure!(
                    !prefixes.is_empty()
                        && prefixes.len() <= 256
                        && prefixes.iter().all(|p| prefix_valid(p)),
                    "invalid object scope"
                );
                if matches!(cap.access, Access::Publisher { .. }) {
                    ensure!(prefixes.iter().all(|p| {
                        let parts: Vec<_> = p.trim_end_matches('/').split('/').collect();
                        matches!(parts.as_slice(), ["research", "builds", id] if valid_digest(id))
                            || matches!(parts.as_slice(), ["research", "sources", commit] if commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    }), "publisher cannot write outside an exact Build/source");
                }
            }
            Access::AttemptWriter {
                tenant,
                task_id,
                attempt,
                fence,
            } => ensure!(
                !tenant.is_empty()
                    && tenant.len() <= 128
                    && valid_digest(task_id)
                    && *attempt > 0
                    && *fence > 0,
                "invalid Attempt capability"
            ),
        }
    }
    Ok(caps)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptIdentityConfig {
    pub capabilities_file: PathBuf,
    pub state_root: PathBuf,
    pub namespace_prefix: String,
    pub tls_identity_file: Option<PathBuf>,
}

pub struct AttemptIdentityIssuer {
    config: AttemptIdentityConfig,
}

// Transport tests exercise resource binding separately from the real-PG issuer
// test. This factory cannot exist in a production binary.
#[cfg(test)]
pub(crate) fn launcher_fixture(
    spec: &crate::orchestrator::TaskSpec,
    lease: &crate::orchestrator::Lease,
    trust_sha256: String,
) -> Result<(
    tempfile::TempDir,
    AttemptIdentityIssuer,
    IssuedAttemptIdentity,
)> {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().canonicalize()?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let state = root.join("state");
    std::fs::create_dir(&state)?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
    let projection = root.join("capabilities.json");
    let deadline = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )? + 120_000;
    install(
        &projection,
        &serde_json::to_vec(&vec![Capability {
            token_sha256: "f".repeat(64),
            expires_ms: u64::try_from(deadline)?,
            access: Access::Reader {
                prefixes: vec!["research/fixture/".into()],
            },
        }])?,
    )?;
    let issuer = AttemptIdentityIssuer::new(AttemptIdentityConfig {
        capabilities_file: projection,
        state_root: state,
        namespace_prefix: format!("{}/", spec.output_prefix),
        tls_identity_file: None,
    })?;
    let issued = issuer.project(
        "fixture".into(),
        lease.task_id.clone(),
        lease.attempt,
        lease.fence,
        deadline,
        format!(
            "{}/{}/{}/",
            spec.output_prefix, lease.task_id, lease.attempt
        ),
        "a".repeat(64),
        trust_sha256,
        b"{}".to_vec(),
    )?;
    Ok((temporary, issuer, issued))
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OwnedIdentity {
    schema: u32,
    issuer_sha256: String,
    scope_id: String,
    tenant: String,
    task_id: String,
    attempt: u32,
    fence: i64,
    deadline_ms: i64,
    artifact_prefix: String,
    native_evidence_sha256: String,
    native_trust_sha256: String,
    capability: Capability,
    files_sha256: BTreeMap<String, String>,
}

/// Only a live PG check can construct this object. It contains private bytes.
/// Never print or serialize it. The controlled launcher mounts only late_files().
pub struct IssuedAttemptIdentity {
    record: OwnedIdentity,
    directory: PathBuf,
    files: BTreeMap<String, Vec<u8>>,
}
impl IssuedAttemptIdentity {
    pub fn scope_id(&self) -> &str {
        &self.record.scope_id
    }
    pub fn native_evidence_sha256(&self) -> &str {
        &self.record.native_evidence_sha256
    }
    pub fn native_trust_sha256(&self) -> &str {
        &self.record.native_trust_sha256
    }
    pub fn tenant(&self) -> &str {
        &self.record.tenant
    }
    pub fn task_id(&self) -> &str {
        &self.record.task_id
    }
    pub fn attempt(&self) -> u32 {
        self.record.attempt
    }
    pub fn fence(&self) -> i64 {
        self.record.fence
    }
    pub fn deadline_ms(&self) -> i64 {
        self.record.deadline_ms
    }
    pub fn artifact_prefix(&self) -> &str {
        &self.record.artifact_prefix
    }
    pub fn late_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
    pub fn matches_context(&self, context: &crate::orchestrator::AttemptContext) -> Result<()> {
        context.validate()?;
        ensure!(
            context.lease.task_id == self.record.task_id
                && context.lease.attempt == self.record.attempt
                && context.lease.fence == self.record.fence
                && self.record.artifact_prefix
                    == format!(
                        "{}/{}/{}/",
                        context.spec.output_prefix, context.lease.task_id, context.lease.attempt
                    ),
            "issued identity differs from the admitted Attempt"
        );
        Ok(())
    }
}

#[derive(Serialize)]
pub struct IdentityCleanupReceipt {
    pub task_id: String,
    pub attempt: u32,
    pub fence: i64,
    pub owned_identity_sha256: String,
}

fn private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        path.is_absolute()
            && path.canonicalize()? == path
            && path.symlink_metadata()?.is_dir()
            && path.metadata()?.permissions().mode() & 0o777 == 0o700,
        "identity directory must be canonical and private"
    );
    Ok(())
}
fn private_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    use std::os::unix::fs::PermissionsExt;
    private_directory(path.parent().context("private parent absent")?)?;
    let file = File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.len() > 0
            && meta.len() <= limit
            && meta.permissions().mode() & 0o777 == 0o600,
        "identity file must be bounded, regular and 0600"
    );
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= limit,
        "identity input changed during read"
    );
    Ok(bytes)
}
fn install(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = File::from(rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
struct ProjectionLock(File);
impl Drop for ProjectionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl AttemptIdentityIssuer {
    pub fn new(config: AttemptIdentityConfig) -> Result<Self> {
        private_directory(&config.state_root)?;
        private_directory(
            config
                .capabilities_file
                .parent()
                .context("projection parent absent")?,
        )?;
        ensure!(
            config.capabilities_file.is_absolute()
                && !config.capabilities_file.starts_with(&config.state_root)
                && (config.namespace_prefix == "research/"
                    || prefix_valid(&config.namespace_prefix)),
            "invalid host projection scope"
        );
        read_capabilities(&config.capabilities_file)?;
        if let Some(path) = &config.tls_identity_file {
            let pem = private_bytes(path, 64 * 1024)?;
            reqwest::Identity::from_pem(&pem)?;
        }
        Ok(Self { config })
    }
    fn lock(&self) -> Result<ProjectionLock> {
        private_directory(
            self.config
                .capabilities_file
                .parent()
                .context("projection parent absent")?,
        )?;
        let path = self
            .config
            .capabilities_file
            .with_extension("identity.lock");
        let file = File::from(rustix::fs::open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600),
        )?);
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            file.metadata()?.is_file() && file.metadata()?.permissions().mode() & 0o777 == 0o600,
            "invalid projection lock"
        );
        file.lock()?;
        Ok(ProjectionLock(file))
    }
    fn replace(&self, caps: &[Capability]) -> Result<()> {
        let bytes = serde_json::to_vec(caps)?;
        ensure!(
            caps.len() <= 1024 && bytes.len() <= 1024 * 1024,
            "projection exceeds bound"
        );
        // Validate the existing file again before replacing it. All host writers
        // use this same sidecar lock; the gateway keeps its read-only hot reload.
        read_capabilities(&self.config.capabilities_file)?;
        let parent = self
            .config
            .capabilities_file
            .parent()
            .context("projection parent absent")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.config.capabilities_file)
            .map_err(|e| e.error)?;
        File::open(parent)?.sync_all()?;
        ensure!(
            read_capabilities(&self.config.capabilities_file)? == caps,
            "projection readback differs"
        );
        Ok(())
    }
    fn directory(&self, task: &str, attempt: u32, fence: i64) -> PathBuf {
        self.config
            .state_root
            .join(format!("{task}.{attempt}.{fence}"))
    }
    fn verify_owned_files(
        &self,
        directory: &Path,
        record: &OwnedIdentity,
    ) -> Result<std::collections::BTreeSet<String>> {
        private_directory(directory)?;
        let expected: std::collections::BTreeSet<_> = record
            .files_sha256
            .keys()
            .cloned()
            .chain(std::iter::once("identity.json".into()))
            .collect();
        let actual = std::fs::read_dir(directory)?
            .map(|e| e.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
        ensure!(
            actual == expected,
            "owned identity directory contains foreign entries"
        );
        let stored: OwnedIdentity =
            serde_json::from_slice(&private_bytes(&directory.join("identity.json"), 64 * 1024)?)?;
        ensure!(stored == *record, "owned identity journal changed");
        for (name, hash) in &record.files_sha256 {
            ensure!(
                sha256(&private_bytes(&directory.join(name), 64 * 1024)?) == *hash,
                "owned identity bytes changed"
            );
        }
        Ok(expected)
    }
    fn remove_files(&self, directory: &Path, record: &OwnedIdentity) -> Result<()> {
        for name in self.verify_owned_files(directory, record)? {
            std::fs::remove_file(directory.join(name))?;
        }
        std::fs::remove_dir(directory)?;
        File::open(&self.config.state_root)?.sync_all()?;
        Ok(())
    }
    pub fn cleanup(&self, issued: IssuedAttemptIdentity) -> Result<IdentityCleanupReceipt> {
        ensure!(
            issued.directory == self.directory(issued.task_id(), issued.attempt(), issued.fence()),
            "foreign identity cleanup"
        );
        let _lock = self.lock()?;
        let mut caps = read_capabilities(&self.config.capabilities_file)?;
        if let Some(index) = caps
            .iter()
            .position(|cap| cap.token_sha256 == issued.record.capability.token_sha256)
        {
            ensure!(
                caps[index] == issued.record.capability,
                "owned capability changed"
            );
            caps.remove(index);
            self.replace(&caps)?;
        }
        self.remove_files(&issued.directory, &issued.record)?;
        Ok(IdentityCleanupReceipt {
            task_id: issued.record.task_id.clone(),
            attempt: issued.record.attempt,
            fence: issued.record.fence,
            owned_identity_sha256: identity(&issued.record)?,
        })
    }

    /// Recover existing bytes for mechanical cleanup. Never mint, restore a
    /// capability, or require a still-active grant after process-tree stop.
    pub async fn recover_for_cleanup(
        &self,
        tx: &mut sqlx_core::transaction::Transaction<'_, sqlx_postgres::Postgres>,
        selected: &crate::orchestrator::Task,
    ) -> Result<Option<IssuedAttemptIdentity>> {
        use sqlx_core::{query::query, row::Row};
        ensure!(
            selected.state == crate::orchestrator::State::Stopping,
            "cleanup requires stopping Attempt"
        );
        let row = query("SELECT t.tenant,t.document,n.document AS native,n.trust_document,n.trust_sha256,n.evidence_sha256 FROM research.tasks t JOIN research.native_admission_imports n ON n.request_sha256=t.request_sha256 AND n.tenant=t.tenant WHERE t.task_id=$1 FOR SHARE OF t")
            .bind(&selected.id).fetch_one(&mut **tx).await?;
        let current: crate::orchestrator::Task = serde_json::from_value(row.get("document"))?;
        ensure!(
            current.id == selected.id
                && current.spec == selected.spec
                && current.attempt == selected.attempt
                && current.fence == selected.fence,
            "cleanup changed ledger Attempt"
        );
        let signed: crate::admission::SignedNativeAdmission =
            serde_json::from_value(row.get("native"))?;
        let trust: crate::admission::NativeAdmissionTrust =
            serde_json::from_value(row.get("trust_document"))?;
        let verified = trust.verify(&signed)?;
        ensure!(
            signed.evidence.admission.task_spec == current.spec
                && signed.evidence.admission.request_sha256 == current.id
                && signed.evidence.tenant == row.get::<String, _>("tenant")
                && signed.evidence_sha256 == row.get::<String, _>("evidence_sha256")
                && verified.trust_sha256() == row.get::<String, _>("trust_sha256"),
            "cleanup changed native history"
        );
        let _lock = self.lock()?;
        let directory = self.directory(&current.id, current.attempt, current.fence);
        if !directory.try_exists()? {
            return Ok(None);
        }
        let record: OwnedIdentity =
            serde_json::from_slice(&private_bytes(&directory.join("identity.json"), 64 * 1024)?)?;
        ensure!(
            record.schema == 1
                && record.issuer_sha256 == identity(&self.config)?
                && record.tenant == signed.evidence.tenant
                && record.task_id == current.id
                && record.attempt == current.attempt
                && record.fence == current.fence
                && record.native_evidence_sha256 == signed.evidence_sha256
                && record.native_trust_sha256 == verified.trust_sha256(),
            "foreign cleanup journal"
        );
        let expected_scope = identity(&(
            &record.issuer_sha256,
            &record.tenant,
            &record.task_id,
            record.attempt,
            record.fence,
            &record.native_evidence_sha256,
            &record.native_trust_sha256,
        ))?;
        ensure!(
            record.scope_id == expected_scope
                && record.artifact_prefix
                    == format!(
                        "{}/{}/{}/",
                        current.spec.output_prefix, current.id, current.attempt
                    )
                && record
                    .artifact_prefix
                    .starts_with(&self.config.namespace_prefix),
            "cleanup journal changed scope"
        );
        ensure!(
            record.capability.access
                == Access::AttemptWriter {
                    tenant: record.tenant.clone(),
                    task_id: record.task_id.clone(),
                    attempt: record.attempt,
                    fence: record.fence
                }
                && record.capability.expires_ms == u64::try_from(record.deadline_ms)?,
            "cleanup capability changed scope"
        );
        let mut files = BTreeMap::new();
        for name in record.files_sha256.keys() {
            ensure!(
                matches!(
                    name.as_str(),
                    "artifact.token" | "native-admission.json" | "tls.pem"
                ),
                "foreign cleanup file"
            );
            files.insert(
                name.clone(),
                private_bytes(&directory.join(name), 64 * 1024)?,
            );
        }
        self.verify_owned_files(&directory, &record)?;
        ensure!(
            files.get("native-admission.json") == Some(&serde_json::to_vec(&signed)?)
                && files
                    .get("artifact.token")
                    .is_some_and(|token| sha256(token) == record.capability.token_sha256),
            "cleanup credential changed"
        );
        Ok(Some(IssuedAttemptIdentity {
            record,
            directory,
            files,
        }))
    }

    /// Root's LockedTask wrapper supplies its existing transaction. The request
    /// selects a row; every authority and scope field comes from that locked row.
    pub async fn issue_for_task(
        &self,
        tx: &mut sqlx_core::transaction::Transaction<'_, sqlx_postgres::Postgres>,
        selected: &crate::orchestrator::Task,
    ) -> Result<IssuedAttemptIdentity> {
        use sqlx_core::{query::query, row::Row};
        let projection_lock = self.lock()?;
        let authority=query("SELECT mode,legacy_quiescence_sha256,migration_receipt_sha256 FROM research.authority WHERE singleton FOR SHARE").fetch_one(&mut **tx).await?;
        ensure!(
            authority.get::<String, _>("mode") == "postgres"
                && authority
                    .get::<Option<String>, _>("legacy_quiescence_sha256")
                    .is_some_and(|s| valid_digest(&s))
                && authority
                    .get::<Option<String>, _>("migration_receipt_sha256")
                    .is_some_and(|s| valid_digest(&s)),
            "artifact identity authority is paused"
        );
        let row = query("SELECT tenant,document FROM research.tasks WHERE task_id=$1 FOR SHARE")
            .bind(&selected.id)
            .fetch_one(&mut **tx)
            .await?;
        let tenant: String = row.get("tenant");
        let current: crate::orchestrator::Task = serde_json::from_value(row.get("document"))?;
        query("SELECT request_sha256 FROM research.admissions WHERE request_sha256=$1 FOR UPDATE")
            .bind(&current.id)
            .fetch_one(&mut **tx)
            .await?;
        let row=query("SELECT n.document,n.trust_document,n.trust_sha256,n.evidence_sha256,a.document AS admission,research.native_request_deadline_ms(n.request_sha256) AS cap,floor(extract(epoch FROM clock_timestamp())*1000)::bigint AS now_ms FROM research.native_admission_imports n JOIN research.admissions a USING(request_sha256) WHERE n.request_sha256=$1 AND n.tenant=$2 AND NOT EXISTS(SELECT 1 FROM research.revocations r WHERE r.request_sha256=n.request_sha256)").bind(&current.id).bind(&tenant).fetch_one(&mut **tx).await?;
        let signed: crate::admission::SignedNativeAdmission =
            serde_json::from_value(row.get("document"))?;
        let trust: crate::admission::NativeAdmissionTrust =
            serde_json::from_value(row.get("trust_document"))?;
        let verified = trust.verify(&signed)?;
        let now: i64 = row.get("now_ms");
        verified.evidence().active_at(now)?;
        let admission: crate::orchestrator::Admission =
            serde_json::from_value(row.get("admission"))?;
        admission.validate(&current.spec)?;
        ensure!(
            signed.evidence.admission == admission
                && signed.evidence.tenant == tenant
                && signed.evidence_sha256 == row.get::<String, _>("evidence_sha256")
                && verified.trust_sha256() == row.get::<String, _>("trust_sha256"),
            "imported native admission changed"
        );
        ensure!(
            matches!(
                current.state,
                crate::orchestrator::State::Launching | crate::orchestrator::State::Running
            ) && current.id == selected.id
                && current.spec == selected.spec
                && current.attempt == selected.attempt
                && current.fence == selected.fence
                && current
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.expires_ms > now)
                && selected
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.expires_ms > now
                        && current.lease.as_ref().is_some_and(|original| original.owner
                            == lease.owner
                            && original.task_id == lease.task_id
                            && original.attempt == lease.attempt
                            && original.fence == lease.fence)),
            "Attempt identity lacks current PG lease"
        );
        let deadline = current
            .deadline_ms
            .context("Attempt deadline absent")?
            .min(selected.deadline_ms.context("controller deadline absent")?)
            .min(row.get::<i64, _>("cap"))
            .min(
                now.checked_add(24 * 60 * 60 * 1000)
                    .context("identity deadline overflow")?,
            );
        ensure!(
            deadline > now,
            "Attempt expired or outside gateway namespace"
        );
        let native_bytes = serde_json::to_vec(&signed)?;
        ensure!(
            native_bytes.len() <= 64 * 1024,
            "native admission exceeds late mount bound"
        );
        let prefix = format!(
            "{}/{}/{}/",
            current.spec.output_prefix, current.id, current.attempt
        );
        ensure!(
            prefix_valid(&prefix) && prefix.starts_with(&self.config.namespace_prefix),
            "Attempt prefix is not canonical or outside gateway namespace"
        );
        let lease_expiry = current
            .lease
            .as_ref()
            .context("current lease absent")?
            .expires_ms;
        let issued = self.project(
            tenant,
            current.id,
            current.attempt,
            current.fence,
            deadline,
            prefix,
            signed.evidence_sha256,
            verified.trust_sha256().to_owned(),
            native_bytes,
        )?;
        let fresh = sqlx_core::query_scalar::query_scalar::<_, i64>(
            "SELECT floor(extract(epoch FROM clock_timestamp())*1000)::bigint",
        )
        .fetch_one(&mut **tx)
        .await;
        drop(projection_lock);
        match fresh {
            Ok(now) if now < issued.deadline_ms() && now < lease_expiry => Ok(issued),
            Ok(_) => {
                self.cleanup(issued)?;
                anyhow::bail!("Attempt expired during identity projection")
            }
            Err(error) => {
                self.cleanup(issued)?;
                Err(error.into())
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn project(
        &self,
        tenant: String,
        task: String,
        attempt: u32,
        fence: i64,
        deadline: i64,
        prefix: String,
        evidence_sha: String,
        trust_sha: String,
        native_bytes: Vec<u8>,
    ) -> Result<IssuedAttemptIdentity> {
        private_directory(&self.config.state_root)?;
        let issuer_sha256 = identity(&self.config)?;
        let scope_id = identity(&(
            &issuer_sha256,
            &tenant,
            &task,
            attempt,
            fence,
            &evidence_sha,
            &trust_sha,
        ))?;
        let directory = self.directory(&task, attempt, fence);
        let mut caps = read_capabilities(&self.config.capabilities_file)?;
        if directory.try_exists()? {
            let record: OwnedIdentity = serde_json::from_slice(&private_bytes(
                &directory.join("identity.json"),
                64 * 1024,
            )?)?;
            ensure!(
                record.scope_id == scope_id
                    && record.issuer_sha256 == issuer_sha256
                    && record.schema == 1
                    && record.tenant == tenant
                    && record.task_id == task
                    && record.attempt == attempt
                    && record.fence == fence
                    && record.deadline_ms <= deadline
                    && record.artifact_prefix == prefix
                    && record.native_evidence_sha256 == evidence_sha
                    && record.native_trust_sha256 == trust_sha,
                "stored identity differs from current native Attempt"
            );
            let mut files = BTreeMap::new();
            for (name, hash) in &record.files_sha256 {
                ensure!(
                    matches!(
                        name.as_str(),
                        "artifact.token" | "tls.pem" | "native-admission.json"
                    ),
                    "foreign late identity file"
                );
                let bytes = private_bytes(&directory.join(name), 64 * 1024)?;
                ensure!(sha256(&bytes) == *hash, "stored identity bytes changed");
                files.insert(name.clone(), bytes);
            }
            match &self.config.tls_identity_file {
                Some(path) => {
                    let current_tls = private_bytes(path, 64 * 1024)?;
                    reqwest::Identity::from_pem(&current_tls)?;
                    ensure!(
                        files.get("tls.pem") == Some(&current_tls),
                        "stored TLS identity differs from current host material"
                    );
                }
                None => ensure!(
                    !files.contains_key("tls.pem"),
                    "unexpected stored TLS identity"
                ),
            }
            ensure!(
                files.get("native-admission.json") == Some(&native_bytes)
                    && files
                        .get("artifact.token")
                        .is_some_and(|_token| record.files_sha256.get("artifact.token")
                            == Some(&record.capability.token_sha256))
                    && record.capability.access
                        == Access::AttemptWriter {
                            tenant: tenant.clone(),
                            task_id: task.clone(),
                            attempt,
                            fence
                        }
                    && record.capability.expires_ms == u64::try_from(record.deadline_ms)?,
                "stored Attempt credential changed"
            );
            if let Some(existing) = caps
                .iter()
                .find(|cap| cap.token_sha256 == record.capability.token_sha256)
            {
                ensure!(
                    *existing == record.capability,
                    "stored capability differs from projection"
                );
            } else {
                caps.push(record.capability.clone());
                self.replace(&caps)?;
            }
            return Ok(IssuedAttemptIdentity {
                record,
                directory,
                files,
            });
        }
        ensure!(caps.len() < 1024, "gateway capability limit reached");
        let mut entropy = [0u8; 32];
        getrandom::getrandom(&mut entropy)
            .map_err(|_| anyhow::anyhow!("OS entropy unavailable"))?;
        let token = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into_bytes();
        let cap = Capability {
            token_sha256: sha256(&token),
            expires_ms: u64::try_from(deadline)?,
            access: Access::AttemptWriter {
                tenant: tenant.clone(),
                task_id: task.clone(),
                attempt,
                fence,
            },
        };
        ensure!(
            caps.iter()
                .all(|existing| existing.token_sha256 != cap.token_sha256),
            "random capability collision"
        );
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::from([
            ("artifact.token".into(), token),
            ("native-admission.json".into(), native_bytes),
        ]);
        if let Some(path) = &self.config.tls_identity_file {
            let bytes = private_bytes(path, 64 * 1024)?;
            reqwest::Identity::from_pem(&bytes)?;
            files.insert("tls.pem".into(), bytes);
        }
        let record = OwnedIdentity {
            schema: 1,
            issuer_sha256,
            scope_id,
            tenant,
            task_id: task,
            attempt,
            fence,
            deadline_ms: deadline,
            artifact_prefix: prefix,
            native_evidence_sha256: evidence_sha,
            native_trust_sha256: trust_sha,
            capability: cap.clone(),
            files_sha256: files
                .iter()
                .map(|(name, bytes)| {
                    (
                        name.clone(),
                        if name == "artifact.token" {
                            cap.token_sha256.clone()
                        } else {
                            sha256(bytes)
                        },
                    )
                })
                .collect(),
        };
        let temporary = tempfile::Builder::new()
            .prefix(".attempt-")
            .tempdir_in(&self.config.state_root)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))?;
        for (name, bytes) in &files {
            install(&temporary.path().join(name), bytes)?;
        }
        install(
            &temporary.path().join("identity.json"),
            &serde_json::to_vec(&record)?,
        )?;
        File::open(temporary.path())?.sync_all()?;
        std::fs::rename(temporary.path(), &directory)?;
        File::open(&self.config.state_root)?.sync_all()?;
        caps.push(cap);
        if let Err(error) = self.replace(&caps) {
            let mut current = read_capabilities(&self.config.capabilities_file)?;
            if let Some(index) = current
                .iter()
                .position(|cap| cap.token_sha256 == record.capability.token_sha256)
            {
                ensure!(
                    current[index] == record.capability,
                    "failed issuance capability changed"
                );
                current.remove(index);
                self.replace(&current)?;
            }
            self.remove_files(&directory, &record)?;
            return Err(error);
        }
        Ok(IssuedAttemptIdentity {
            record,
            directory,
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    fn fixture(
        caps: Vec<Capability>,
    ) -> Result<(tempfile::TempDir, AttemptIdentityIssuer, Vec<Capability>)> {
        let temp = tempfile::tempdir()?;
        let parent = temp.path().canonicalize()?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        let state = parent.join("state");
        std::fs::create_dir(&state)?;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))?;
        let projection = parent.join("capabilities.json");
        install(&projection, &serde_json::to_vec(&caps)?)?;
        let issuer = AttemptIdentityIssuer::new(AttemptIdentityConfig {
            capabilities_file: projection,
            state_root: state,
            namespace_prefix: "research/".into(),
            tls_identity_file: None,
        })?;
        Ok((temp, issuer, caps))
    }
    #[test]
    fn paused_empty_projection_grants_no_reader_publisher_or_writer() -> Result<()> {
        let (_temp, issuer, caps) = fixture(Vec::new())?;
        assert!(caps.is_empty());
        assert!(read_capabilities(&issuer.config.capabilities_file)?.is_empty());
        Ok(())
    }

    fn others() -> Vec<Capability> {
        vec![
            Capability {
                token_sha256: "a".repeat(64),
                expires_ms: 10_000_000,
                access: Access::Reader {
                    prefixes: vec!["research/results/".into()],
                },
            },
            Capability {
                token_sha256: "b".repeat(64),
                expires_ms: 10_000_000,
                access: Access::Publisher {
                    prefixes: vec![format!("research/builds/{}/", "b".repeat(64))],
                },
            },
            Capability {
                token_sha256: "c".repeat(64),
                expires_ms: 10_000_000,
                access: Access::AttemptWriter {
                    tenant: "another".into(),
                    task_id: "c".repeat(64),
                    attempt: 2,
                    fence: 3,
                },
            },
        ]
    }
    // File-only fixtures cannot grant native authority. The independent PG fixtures
    // use actual admission and strict signed evidence instead.
    fn file_projection(
        issuer: &AttemptIdentityIssuer,
        task: &str,
    ) -> Result<IssuedAttemptIdentity> {
        let _lock = issuer.lock()?;
        issuer.project(
            "synthetic-file-only".into(),
            task.into(),
            1,
            1,
            1_000_000,
            format!("research/results/{task}/1/"),
            "d".repeat(64),
            "e".repeat(64),
            b"synthetic-file-only-not-native-authority".to_vec(),
        )
    }
    #[test]
    fn private_projection_recovers_the_same_token_and_cleans_only_owned_files() -> Result<()> {
        let (_temp, issuer, others) = fixture(others())?;
        let issued = file_projection(&issuer, &"1".repeat(64))?;
        let recovered = file_projection(&issuer, &"1".repeat(64))?;
        assert!(issued.late_files() == recovered.late_files());
        assert_eq!(issued.deadline_ms(), recovered.deadline_ms());
        assert_eq!(issued.scope_id(), recovered.scope_id());
        assert_eq!(issued.native_evidence_sha256(), "d".repeat(64));
        assert_eq!(
            read_capabilities(&issuer.config.capabilities_file)?.len(),
            4
        );
        for path in std::fs::read_dir(&issued.directory)? {
            assert_eq!(path?.metadata()?.permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(
            issued.directory.metadata()?.permissions().mode() & 0o777,
            0o700
        );
        let receipt = issuer.cleanup(recovered)?;
        assert_eq!(receipt.task_id, "1".repeat(64));
        assert!(read_capabilities(&issuer.config.capabilities_file)? == others);
        assert!(!issued.directory.exists());
        Ok(())
    }
    #[test]
    fn concurrent_host_projections_preserve_other_tokens() -> Result<()> {
        let (_temp, issuer, others) = fixture(others())?;
        let issuer = std::sync::Arc::new(issuer);
        let workers = (1..=4)
            .map(|n| {
                let issuer = issuer.clone();
                std::thread::spawn(move || file_projection(&issuer, &n.to_string().repeat(64)))
            })
            .collect::<Vec<_>>();
        let issued = workers
            .into_iter()
            .map(|worker| worker.join().expect("fixture thread"))
            .collect::<Result<Vec<_>>>()?;
        let caps = read_capabilities(&issuer.config.capabilities_file)?;
        assert_eq!(caps.len(), 7);
        assert!(others.iter().all(|cap| caps.contains(cap)));
        for item in issued {
            issuer.cleanup(item)?;
        }
        assert!(read_capabilities(&issuer.config.capabilities_file)? == others);
        Ok(())
    }
    #[test]
    fn failed_projection_removes_its_private_journal_without_changing_other_caps() -> Result<()> {
        let prefix = format!(
            "research/{}{}{}",
            format!("{}/", "a".repeat(255)).repeat(7),
            "b".repeat(242),
            "/"
        );
        let caps = vec![
            Capability {
                token_sha256: "a".repeat(64),
                expires_ms: 1,
                access: Access::Reader {
                    prefixes: vec![prefix.clone(); 256],
                },
            },
            Capability {
                token_sha256: "b".repeat(64),
                expires_ms: 1,
                access: Access::Reader {
                    prefixes: vec![prefix; 256],
                },
            },
        ];
        assert!(serde_json::to_vec(&caps)?.len() <= 1024 * 1024);
        let (_temp, issuer, original) = fixture(caps)?;
        assert!(file_projection(&issuer, &"1".repeat(64)).is_err());
        assert!(read_capabilities(&issuer.config.capabilities_file)? == original);
        assert_eq!(std::fs::read_dir(&issuer.config.state_root)?.count(), 0);
        Ok(())
    }
    #[test]
    fn symlink_fifo_and_public_identity_material_fail_before_projection() -> Result<()> {
        let (temp, issuer, original) = fixture(others())?;
        let root = temp.path().canonicalize()?;
        let file = root.join("tls.pem");
        install(&file, b"invalid pem")?;
        let link = root.join("linked.pem");
        symlink(&file, &link)?;
        assert!(private_bytes(&link, 64 * 1024).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644))?;
        assert!(private_bytes(&file, 64 * 1024).is_err());
        let fifo = root.join("fifo");
        ensure!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()?
                .success(),
            "FIFO fixture"
        );
        assert!(private_bytes(&fifo, 64 * 1024).is_err());
        assert!(read_capabilities(&issuer.config.capabilities_file)? == original);
        assert_eq!(std::fs::read_dir(&issuer.config.state_root)?.count(), 0);
        Ok(())
    }
}
