//! Scientific lineage is separate from execution state. A Session may propose
//! many Experiments; one fixed Run owns one compute task and its Attempts.
use crate::{
    identity,
    orchestrator::{TaskKind, TaskSpec},
    valid_digest,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
fn commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Experiment {
    pub schema: u32,
    pub hypothesis: String,
    pub parent_experiment_sha256: Option<String>,
    pub variant: BTreeMap<String, String>,
}
impl Experiment {
    pub fn id(&self) -> Result<String> {
        ensure!(
            self.schema == 1
                && !self.hypothesis.trim().is_empty()
                && self.hypothesis.len() <= 4096
                && self.variant.len() <= 128
                && self
                    .variant
                    .iter()
                    .all(|(k, v)| !k.is_empty() && k.len() <= 128 && v.len() <= 4096),
            "invalid experiment variant"
        );
        ensure!(
            self.parent_experiment_sha256
                .as_ref()
                .is_none_or(|s| valid_digest(s)),
            "invalid parent experiment"
        );
        identity(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub schema: u32,
    pub experiment_sha256: String,
    pub kind: TaskKind,
    pub build_artifact_sha256: String,
    pub configuration_sha256: String,
    pub command: Vec<String>,
    pub code_commit: String,
    pub source_manifest_sha256: String,
    pub image: String,
    /// For Prepare this fixes a plan; other kinds require a published DataView.
    pub data_manifest_sha256: String,
    pub seed: u64,
    pub evaluator_sha256: String,
    pub evaluation_protocol_sha256: String,
    pub fit_identity_sha256: Option<String>,
}
impl Run {
    pub fn id(&self) -> Result<String> {
        ensure!(
            self.schema == 1
                && commit(&self.code_commit)
                && [
                    &self.experiment_sha256,
                    &self.build_artifact_sha256,
                    &self.configuration_sha256,
                    &self.source_manifest_sha256,
                    &self.data_manifest_sha256,
                    &self.evaluator_sha256,
                    &self.evaluation_protocol_sha256
                ]
                .into_iter()
                .all(|s| valid_digest(s))
                && !self.command.is_empty()
                && self.command.len() <= 64
                && self
                    .command
                    .iter()
                    .all(|s| !s.is_empty() && s.len() <= 4096 && !s.contains('\0')),
            "invalid fixed scientific run"
        );
        ensure!(
            self.image
                .rsplit_once("@sha256:")
                .is_some_and(|(_, s)| valid_digest(s))
                && self
                    .fit_identity_sha256
                    .as_ref()
                    .is_none_or(|s| valid_digest(s)),
            "unpinned run image/fit"
        );
        identity(self)
    }
    pub fn admit_build(&self, artifact: &crate::build::BuildArtifact) -> Result<()> {
        ensure!(
            self.build_artifact_sha256 == artifact.id()?
                && self.code_commit == artifact.build.code_commit
                && self.source_manifest_sha256 == artifact.build.source_manifest_sha256
                && self.image == artifact.image,
            "Run changed build/source/released image"
        );
        artifact.admits_command(&self.command)?;
        Ok(())
    }
    pub fn admit(&self, spec: &TaskSpec) -> Result<()> {
        ensure!(
            self.id()? == spec.run_manifest_sha256
                && self.kind == spec.kind
                && self.source_manifest_sha256 == spec.source_sha256
                && self.image == spec.image
                && self.data_manifest_sha256 == spec.view_manifest_sha256
                && self.fit_identity_sha256 == spec.fit_identity_sha256
                && self.command == spec.command,
            "task changed fixed run inputs/code/fit"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodingAgent {
    CodexAppServer,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub schema: u32,
    pub experiment_sha256: String,
    pub provider: CodingAgent,
    pub provider_version: String,
    pub provider_thread_id: String,
    pub provider_binary_sha256: String,
    pub capability_policy_receipt_sha256: String,
}
impl Session {
    pub fn id(&self) -> Result<String> {
        ensure!(
            self.schema == 1
                && [
                    &self.experiment_sha256,
                    &self.provider_binary_sha256,
                    &self.capability_policy_receipt_sha256
                ]
                .into_iter()
                .all(|s| valid_digest(s))
                && !self.provider_version.is_empty()
                && self.provider_version.len() <= 128
                && !self.provider_thread_id.is_empty()
                && self.provider_thread_id.len() <= 256,
            "invalid provider session identity"
        );
        identity(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshot {
    pub session_sha256: String,
    pub parent_snapshot_sha256: Option<String>,
    pub code_commit: String,
    pub workspace_manifest_sha256: String,
    pub transcript_manifest_sha256: String,
    /// Native CODEX_HOME/thread state, independently persisted and verified.
    pub native_state_manifest_sha256: String,
}
impl SessionSnapshot {
    pub fn id(&self) -> Result<String> {
        ensure!(
            commit(&self.code_commit)
                && [
                    &self.session_sha256,
                    &self.workspace_manifest_sha256,
                    &self.transcript_manifest_sha256,
                    &self.native_state_manifest_sha256
                ]
                .into_iter()
                .all(|s| valid_digest(s))
                && self
                    .parent_snapshot_sha256
                    .as_ref()
                    .is_none_or(|s| valid_digest(s)),
            "invalid session snapshot"
        );
        identity(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResearchTool {
    #[serde(rename = "research.submit")]
    Submit {
        request_sha256: String,
        idempotency_key: String,
    },
    #[serde(rename = "research.status")]
    Status { run_sha256: String },
    #[serde(rename = "research.artifacts")]
    Artifacts { run_sha256: String },
}
impl ResearchTool {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Submit {
                request_sha256,
                idempotency_key,
            } => ensure!(
                valid_digest(request_sha256)
                    && !idempotency_key.is_empty()
                    && idempotency_key.len() <= 256,
                "invalid tool submission"
            ),
            Self::Status { run_sha256 } | Self::Artifacts { run_sha256 } => {
                ensure!(valid_digest(run_sha256), "invalid run reference")
            }
        };
        Ok(())
    }
}
