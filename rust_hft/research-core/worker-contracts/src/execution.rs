//! Immutable accepted execution profile; no provider client or mutation.
use crate::valid_digest;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    KubernetesJob,
    AcsJob,
    AgentSandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub backend: Backend,
    pub cluster: String,
    pub namespace: String,
    pub service_account: String,
    pub architecture: String,
    pub cpu_millis: u32,
    pub memory_mib: u32,
    pub scratch_mib: u32,
    pub gpu: u16,
    /// Profile acceptance binds scheduler, network, identity, cancellation,
    /// artifact access and (when used) the shared mount, on this exact target.
    pub acceptance_sha256: String,
    pub prepared_pvc: Option<String>,
    /// Operator-owned immutable secret containing scoped worker configuration.
    pub worker_secret: Option<String>,
}

pub fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.cluster.is_empty()
                && valid_dns_label(&self.namespace)
                && valid_dns_label(&self.service_account),
            "invalid execution target"
        );
        ensure!(
            matches!(self.architecture.as_str(), "amd64" | "arm64"),
            "unsupported architecture"
        );
        ensure!(
            self.cpu_millis > 0
                && self.cpu_millis <= 256_000
                && self.memory_mib > 0
                && self.scratch_mib > 0,
            "invalid resources"
        );
        ensure!(self.gpu == 0, "no accepted GPU trainer/provider contract");
        ensure!(
            valid_digest(&self.acceptance_sha256),
            "missing provider acceptance identity"
        );
        if let Some(pvc) = &self.prepared_pvc {
            ensure!(valid_dns_label(pvc), "invalid prepared PVC");
        }
        if let Some(secret) = &self.worker_secret {
            ensure!(valid_dns_label(secret), "invalid worker secret");
        }
        Ok(())
    }
}
