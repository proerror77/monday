//! Standalone research reconciler. PG is authority; provider I/O is bounded;
//! no dependency on workflow_run, Actions artifacts, or CI resource controllers.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    execution::{Backend, Kubernetes, Observation},
    orchestrator::{ResultReceipt, State, Task},
    postgres::Ledger,
    sha256,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub owner: String,
    pub cluster: String,
    pub kubernetes_endpoint: String,
    pub kubernetes_ca_file: String,
    pub kubernetes_token_file: String,
    pub artifact_gateway: String,
    pub artifact_token_file: String,
    pub lease_ms: i64,
    pub agent_api: Option<crate::agent_api::AgentApiConfig>,
}

/// Gateway credentials are controller-only, scoped to result prefix readback.
/// Workers have separate per-attempt write credentials via the identity broker.
pub struct ArtifactGateway {
    client: reqwest::Client,
    base: reqwest::Url,
    token: String,
}

impl ArtifactGateway {
    pub fn new(endpoint: &str, token: String) -> Result<Self> {
        let base = reqwest::Url::parse(endpoint)?;
        ensure!(
            base.scheme() == "https"
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.path().ends_with('/'),
            "invalid artifact gateway"
        );
        ensure!(!token.is_empty(), "missing artifact identity");
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            token,
        })
    }

    async fn get(&self, key: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        ensure!(
            key.starts_with("research/")
                && !key.contains("..")
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/-_.".contains(&c)),
            "unsafe artifact key"
        );
        let mut response = self
            .client
            .get(self.base.join(key)?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("artifact gateway unavailable"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        response = response
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("artifact gateway rejected readback"))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("artifact gateway interrupted"))?
        {
            ensure!(
                bytes.len() as u64 + chunk.len() as u64 <= max_bytes,
                "artifact exceeds readback bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(bytes))
    }

    pub async fn receipt(&self, task: &Task) -> Result<Option<ResultReceipt>> {
        let key = format!(
            "{}/{}/{}/receipt.json",
            task.spec.output_prefix, task.id, task.attempt
        );
        let Some(bytes) = self.get(&key, 1024 * 1024).await? else {
            return Ok(None);
        };
        let receipt: ResultReceipt = serde_json::from_slice(&bytes)?;
        receipt.validate(
            &task.spec,
            task.lease.as_ref().context("receipt without lease")?,
        )?;
        // Readback streams large artifacts through bounded buffers; a receipt
        // alone, an object HEAD, or a worker's claimed success is insufficient.
        for artifact in receipt.artifacts.iter().chain(receipt.checkpoint.iter()) {
            ensure!(
                artifact.bytes <= 4 * 1024 * 1024 * 1024,
                "artifact exceeds admitted readback limit"
            );
            if !receipt.prepared_view.as_ref().is_some_and(|v| {
                v.blocks
                    .iter()
                    .any(|b| b.sha256 == artifact.sha256 && b.bytes == artifact.bytes)
            }) {
                self.verify_artifact(artifact).await?;
            }
        }
        if let Some(view) = &receipt.prepared_view {
            let mut orders =
                std::collections::BTreeMap::<crate::data::Exit, crate::data::BlockOrder>::new();
            for block in &view.blocks {
                let artifact = receipt
                    .artifacts
                    .iter()
                    .find(|a| a.sha256 == block.sha256 && a.bytes == block.bytes)
                    .context("prepared coverage incomplete")?;
                let bytes = self
                    .get(&artifact.key, block.bytes)
                    .await?
                    .context("prepared block missing")?;
                ensure!(
                    sha256(&bytes) == block.sha256,
                    "prepared block changed during publication"
                );
                let typed = crate::prepared::decode(&bytes)?;
                crate::data::validate_block(&typed, block, &view.spec)?;
                orders
                    .entry(block.exit.clone())
                    .or_default()
                    .observe(&typed)?;
            }
        }
        Ok(Some(receipt))
    }

    pub async fn checkpoint(&self, task: &Task) -> Result<Option<crate::orchestrator::Checkpoint>> {
        let key = format!(
            "{}/{}/{}/checkpoint.json",
            task.spec.output_prefix, task.id, task.attempt
        );
        let Some(bytes) = self.get(&key, 1024 * 1024).await? else {
            return Ok(None);
        };
        let checkpoint: crate::orchestrator::Checkpoint = serde_json::from_slice(&bytes)?;
        checkpoint.validate(
            &task.spec,
            task.lease.as_ref().context("checkpoint lacks lease")?,
        )?;
        self.verify_artifact(&checkpoint.artifact).await?;
        Ok(Some(checkpoint))
    }

    pub async fn verify_build(&self, build: &crate::build::BuildArtifact) -> Result<()> {
        build.id()?;
        for executable in &build.executables {
            self.verify_artifact(&executable.blob).await?;
        }
        Ok(())
    }

    async fn verify_artifact(&self, artifact: &crate::orchestrator::Artifact) -> Result<()> {
        use sha2::{Digest, Sha256};
        let mut response = self
            .client
            .get(self.base.join(&artifact.key)?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("artifact readback unavailable"))?
            .error_for_status()
            .map_err(|_| anyhow::anyhow!("artifact readback rejected"))?;
        let mut hash = Sha256::new();
        let mut count = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("artifact readback interrupted"))?
        {
            count = count
                .checked_add(chunk.len() as u64)
                .context("artifact size overflow")?;
            ensure!(count <= artifact.bytes, "artifact too large");
            hash.update(&chunk);
        }
        ensure!(
            count == artifact.bytes && format!("{:x}", hash.finalize()) == artifact.sha256,
            "artifact integrity failure"
        );
        Ok(())
    }
}

pub struct Reconciler {
    pub ledger: Ledger,
    pub kubernetes: Kubernetes,
    pub artifacts: ArtifactGateway,
    pub owner: String,
    pub lease_ms: i64,
}

impl Reconciler {
    /// One bounded reconciliation. Caller waits interruptibly between polls.
    pub async fn tick(&self) -> Result<bool> {
        let Some(mut locked) = self.ledger.lock_next(&self.owner, self.lease_ms).await? else {
            return Ok(false);
        };
        println!("{}", event(&locked.task.id, "reconcile_started"));
        locked.task.expire(locked.now_ms)?;
        if locked.task.state != State::Stopping
            && self.ledger.admission(&locked.task.spec).await?.is_none()
        {
            locked.task.stop(State::Cancelled, false)?;
        }
        if locked.task.state == State::Stopping {
            let lease = locked
                .task
                .lease
                .as_ref()
                .context("stop lacks execution lease")?
                .clone();
            if self
                .kubernetes
                .stop(&locked.task.spec, &lease, locked.task.execution.as_ref())
                .await?
            {
                locked.task.stopped(lease.attempt, lease.fence)?;
            }
            locked.commit("stop_reconciled").await?;
            return Ok(true);
        }
        let mut lease = locked
            .task
            .lease
            .as_ref()
            .context("active task lacks lease")?
            .clone();
        if locked.task.state == State::Launching {
            let build = self.ledger.build_for_task(&locked.task).await?;
            self.artifacts.verify_build(&build).await?;
            let now = locked.refresh_clock().await?;
            if locked.task.expire(now)? {
                locked.commit("build_readback_expired").await?;
                return Ok(true);
            }
            lease = locked.task.heartbeat(&lease, now, self.lease_ms)?;
            if self.ledger.admission(&locked.task.spec).await?.is_none() {
                locked.task.stop(State::Cancelled, false)?;
                locked.commit("launch_admission_revoked").await?;
                return Ok(true);
            }
            let acceptance = self.ledger.acceptance(&locked.task.spec).await?;
            let handle = self
                .kubernetes
                .launch(
                    &locked.task.spec,
                    &lease,
                    &acceptance,
                    locked.task.checkpoint.as_ref(),
                    locked.task.deadline_ms.context("launch lacks deadline")? - locked.now_ms,
                )
                .await?;
            let now = locked.refresh_clock().await?;
            if locked.task.expire(now)? {
                // Resource identity is still recovered by name during stopping.
                locked.commit("launch_expired").await?;
                return Ok(true);
            }
            locked.task.launched(&lease, now, handle)?;
            // Commit Running before worker admission or a second provider call.
            locked.commit("launched").await?;
            return Ok(true);
        }
        let handle = locked
            .task
            .execution
            .as_ref()
            .context("missing execution handle")?;
        let observed = self.kubernetes.observe(handle).await?;
        match observed {
            Observation::Failed | Observation::Gone => locked.task.stop(State::Failed, true)?,
            Observation::Succeeded | Observation::Running | Observation::Pending => {
                // A ready Sandbox still requires a worker terminal receipt.
                // Scientific Jobs additionally require Kubernetes completion.
                let complete = observed == Observation::Succeeded
                    || locked.task.spec.profile.backend == Backend::AgentSandbox;
                if complete {
                    if let Some(receipt) = self.artifacts.receipt(&locked.task).await? {
                        let now = locked.refresh_clock().await?;
                        if !locked.task.expire(now)? {
                            locked.task.stage_result(&lease, now, receipt)?;
                        }
                    }
                }
                if locked.task.state == State::Running {
                    if let Some(checkpoint) = self.artifacts.checkpoint(&locked.task).await? {
                        let now = locked.refresh_clock().await?;
                        if !locked.task.expire(now)? {
                            locked.task.stage_checkpoint(&lease, now, checkpoint)?;
                        }
                    }
                }
                if locked.task.state == State::Running {
                    let now = locked.refresh_clock().await?;
                    if !locked.task.expire(now)? {
                        lease = locked.task.heartbeat(&lease, now, self.lease_ms)?;
                        ensure!(
                            lease.owner == self.owner,
                            "cannot renew another owner's live lease"
                        );
                    }
                }
            }
        }
        locked.commit("execution_reconciled").await?;
        Ok(true)
    }
}

pub fn read_secret(path: &str) -> Result<String> {
    use std::io::Read;
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 64 * 1024,
        "unbounded secret file"
    );
    let mut secret = String::new();
    std::fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_string(&mut secret)?;
    ensure!(
        secret.len() <= 64 * 1024 && !secret.trim().is_empty(),
        "invalid secret file"
    );
    Ok(secret.trim().into())
}

pub fn event(task_id: &str, stage: &str) -> serde_json::Value {
    serde_json::json!({"schema":"monday.research_event.v2","task_id":task_id,"stage":stage})
}

pub fn config_identity(config: &ServiceConfig) -> Result<String> {
    Ok(sha256(&serde_json::to_vec(config)?))
}
