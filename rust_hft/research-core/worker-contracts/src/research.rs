//! Fixed scientific Run identity; no ledger, scheduler or build execution.
use crate::{
    identity,
    task::{TaskKind, TaskSpec},
    valid_digest,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

pub fn valid_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
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
                && valid_commit(&self.code_commit)
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
