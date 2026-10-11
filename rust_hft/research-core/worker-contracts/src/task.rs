//! Task, lease and output wire identities with pure validation.
use crate::{
    execution::{Backend, Profile},
    identity, valid_digest,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Prepare,
    Train,
    Backtest,
    /// The finalized canonical CEX Campaign consumes development-only typed
    /// input roles. It cannot be submitted as a generic Train/Backtest view.
    CexCampaign,
    Explore,
}

/// Actual immutable scientific/public configuration read back from Kubernetes.
/// Per-Attempt credentials and native admission signatures are late-bound;
/// including them here would make their task identity circular.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerConfigurationRef {
    pub schema: String,
    pub secret_name: String,
    pub secret_uid: String,
    pub configuration_sha256: String,
}
impl WorkerConfigurationRef {
    pub fn validate(&self, profile: &Profile) -> Result<()> {
        ensure!(
            self.schema == "monday.worker_configuration.v1"
                && profile.worker_secret.as_deref() == Some(self.secret_name.as_str())
                && !self.secret_uid.is_empty()
                && self.secret_uid.len() <= 128
                && self
                    .secret_uid
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && valid_digest(&self.configuration_sha256),
            "worker configuration identity missing or changed"
        );
        Ok(())
    }
}

/// One wire identity shared by the native producer, launcher, and initializer.
/// The caller must obtain this map from an actual immutable Secret readback;
/// computing a digest alone is not a source witness or admission.
pub fn worker_configuration_reference(
    namespace: &str,
    secret_name: &str,
    secret_uid: &str,
    data: &std::collections::BTreeMap<String, String>,
) -> Result<WorkerConfigurationRef> {
    ensure!(
        !namespace.is_empty()
            && namespace.len() <= 63
            && !secret_name.is_empty()
            && secret_name.len() <= 63
            && !secret_uid.is_empty()
            && secret_uid.len() <= 128,
        "configuration scope missing"
    );
    ensure!(
        data.contains_key("campaign.json")
            && data.contains_key("artifact-io.json")
            && data.keys().all(|k| matches!(
                k.as_str(),
                "campaign.json" | "artifact-io.json" | "ca.pem" | "native-trust.json"
            ))
            && data
                .values()
                .all(|v| !v.is_empty() && v.len() <= 2 * 1024 * 1024)
            && data.values().map(String::len).sum::<usize>() <= 2 * 1024 * 1024,
        "configuration requires bounded static inputs; late credentials are forbidden"
    );
    Ok(WorkerConfigurationRef {
        schema: "monday.worker_configuration.v1".into(),
        secret_name: secret_name.to_owned(),
        secret_uid: secret_uid.to_owned(),
        configuration_sha256: identity(&(
            "monday.worker_configuration_contents.v1",
            namespace,
            secret_name,
            secret_uid,
            data,
        ))?,
    })
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
    pub worker_configuration: Option<WorkerConfigurationRef>,
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
        if let Some(configuration) = &self.worker_configuration {
            configuration.validate(&self.profile)?;
        }
        if self.kind == TaskKind::CexCampaign {
            ensure!(self.max_attempts == 1 && self.worker_configuration.is_some(),
                "native Campaign requires one reserved Attempt and verified immutable configuration");
        }
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub task_id: String,
    pub attempt: u32,
    pub fence: i64,
    pub owner: String,
    pub expires_ms: i64,
}

/// Fixed context supplied by the reconciler's admitted Job. Workers cannot
/// claim an Attempt; PG and the artifact gateway independently enforce it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AttemptContext {
    pub spec: TaskSpec,
    pub lease: Lease,
}
impl AttemptContext {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.lease.task_id == self.spec.id()?
                && self.lease.attempt > 0
                && self.lease.attempt <= self.spec.max_attempts
                && self.lease.fence > 0
                && self.lease.expires_ms > 0
                && !self.lease.owner.is_empty()
                && self.lease.owner.len() <= 256,
            "invalid admitted Attempt context"
        );
        Ok(())
    }
    pub fn output_prefix(&self) -> String {
        format!(
            "{}/{}/{}/",
            self.spec.output_prefix, self.lease.task_id, self.lease.attempt
        )
    }
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
        if spec.kind == TaskKind::CexCampaign {
            ensure!(
                self.artifacts
                    .iter()
                    .filter(|a| a.key.ends_with("/cex-campaign.json"))
                    .count()
                    == 1,
                "native Campaign lacks typed scientific evidence receipt"
            );
        }
        Ok(())
    }
}
