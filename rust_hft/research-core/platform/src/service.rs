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
    #[serde(default)]
    pub artifact_tls: crate::transport::TlsConfig,
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
        Self::with_tls(endpoint, token, &crate::transport::TlsConfig::default())
    }
    pub fn with_tls(
        endpoint: &str,
        token: String,
        tls: &crate::transport::TlsConfig,
    ) -> Result<Self> {
        let base = reqwest::Url::parse(endpoint)?;
        ensure!(
            base.scheme() == "https"
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.fragment().is_none()
                && base.host_str().is_some()
                && base.path().ends_with('/'),
            "invalid artifact gateway"
        );
        ensure!(!token.is_empty(), "missing artifact identity");
        Ok(Self {
            client: tls.client(std::time::Duration::from_secs(15), true)?,
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
        // Prepared blocks are checked on the exact bytes being decoded. Reuse
        // that readback within this receipt, including its object key and size.
        let mut verified = std::collections::BTreeSet::new();
        if let Some(view) = &receipt.prepared_view {
            let mut orders = std::collections::BTreeMap::<
                hft_cex_research_input::data::Exit,
                hft_cex_research_input::data::BlockOrder,
            >::new();
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
                    bytes.len() as u64 == block.bytes && sha256(&bytes) == block.sha256,
                    "prepared block changed during publication"
                );
                let typed = hft_cex_research_input::prepared::decode(&bytes)?;
                hft_cex_research_input::data::validate_block(&typed, block, &view.spec)?;
                orders
                    .entry(block.exit.clone())
                    .or_default()
                    .observe(&typed)?;
                verified.insert((&artifact.key, &artifact.sha256, artifact.bytes));
            }
        }
        // Other keys are independently read even when their digests match.
        // This set is request-local; a later receipt/launch starts fresh.
        for artifact in receipt.artifacts.iter().chain(receipt.checkpoint.iter()) {
            ensure!(
                artifact.bytes <= 4 * 1024 * 1024 * 1024,
                "artifact exceeds admitted readback limit"
            );
            if !verified.contains(&(&artifact.key, &artifact.sha256, artifact.bytes)) {
                self.verify_artifact(artifact).await?;
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
        if self.ledger.admission(&locked.task.spec).await?.is_none() {
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
                if self.ledger.admission(&locked.task.spec).await?.is_none() {
                    locked.task.stop(State::Cancelled, false)?;
                }
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
            if !self
                .ledger
                .admits_launch(
                    &locked.task.spec,
                    locked.task.deadline_ms.context("launch lacks deadline")? - now,
                )
                .await?
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        execution::{Backend, Profile},
        orchestrator::{Artifact, Task, TaskKind, TaskSpec},
        sha256,
    };
    use hft_cex_research_input::{
        data::{
            BlockRef, DataViewSpec, Exit, FeatureFrame, PublishedView, Split, TypedBlock, Window,
        },
        identity, prepared,
    };
    use std::{collections::BTreeMap, sync::Arc};

    #[tokio::test]
    async fn prepared_receipt_verifies_each_key_even_when_digests_match() -> Result<()> {
        let digest = "a".repeat(64);
        let spec = TaskSpec {
            schema: 1,
            kind: TaskKind::Prepare,
            run_manifest_sha256: digest.clone(),
            view_manifest_sha256: digest.clone(),
            source_sha256: digest.clone(),
            image: format!("fixture@sha256:{digest}"),
            command: vec!["/app/prepare".into()],
            profile: Profile {
                backend: Backend::KubernetesJob,
                cluster: "fixture".into(),
                namespace: "research".into(),
                service_account: "worker".into(),
                architecture: "amd64".into(),
                cpu_millis: 1000,
                memory_mib: 128,
                scratch_mib: 64,
                gpu: 0,
                acceptance_sha256: digest.clone(),
                prepared_pvc: None,
                worker_secret: None,
            },
            timeout_ms: 5000,
            max_attempts: 1,
            output_prefix: "research/fixture".into(),
            fit_identity_sha256: None,
        };
        let mut task = Task::new(spec)?;
        let lease = task.claim("owner", 1000, 1000)?;
        let typed = TypedBlock::Features(vec![FeatureFrame {
            segment: "fixture".into(),
            ordinal: 0,
            event_ns: 199,
            available_ns: 200,
            values: vec![1.0],
        }]);
        let block = prepared::encode(&typed)?;
        let block_sha = sha256(&block);
        let prefix = format!("{}/{}/{}/", task.spec.output_prefix, task.id, task.attempt);
        let artifact = Artifact {
            key: format!("{prefix}{block_sha}.mondaybin"),
            sha256: block_sha.clone(),
            bytes: block.len() as u64,
        };
        let missing = Artifact {
            key: format!("{prefix}missing.mondaybin"),
            ..artifact.clone()
        };
        let receipt = ResultReceipt {
            task_id: task.id.clone(),
            attempt: lease.attempt,
            fence: lease.fence,
            view_manifest_sha256: task.spec.view_manifest_sha256.clone(),
            source_sha256: task.spec.source_sha256.clone(),
            image: task.spec.image.clone(),
            fit_identity_sha256: None,
            artifacts: vec![artifact.clone(), missing],
            checkpoint: None,
            prepared_view: Some(PublishedView {
                prepared_id: identity(&(&lease.task_id, lease.attempt, lease.fence))?,
                spec: DataViewSpec {
                    schema: 1,
                    venue: "fixture".into(),
                    instrument: "fixture".into(),
                    market: "usdm".into(),
                    depth: 2,
                    sources: vec![digest.clone()],
                    normalizer_sha256: digest.clone(),
                    feature_sql_sha256: digest.clone(),
                    feature_names: vec!["x".into()],
                    window: Window {
                        start_ns: 100,
                        end_ns: 1000,
                    },
                    lookback_ns: 50,
                    horizons_ns: vec![100],
                    label_tolerance_ns: 0,
                    fit_cutoff_ns: 1000,
                    split: Split::Train,
                },
                blocks: vec![BlockRef {
                    sha256: block_sha,
                    bytes: artifact.bytes,
                    rows: 1,
                    decoded_bytes: hft_cex_research_input::data::memory_bytes(&typed),
                    exit: Exit::Features,
                }],
                producer_image: task.spec.image.clone(),
                source_receipt_sha256: digest,
            }),
        };
        receipt.validate(&task.spec, &lease)?;
        let block_key = artifact.key.clone();
        let requests = Arc::new(std::sync::Mutex::new(BTreeMap::<String, usize>::new()));
        let received = requests.clone();
        let objects = Arc::new(BTreeMap::from([
            (
                format!("{prefix}receipt.json"),
                serde_json::to_vec(&receipt)?,
            ),
            (artifact.key, block),
        ]));
        let app = axum::Router::new().route(
            "/*key",
            axum::routing::get(
                move |axum::extract::Path(key): axum::extract::Path<String>| {
                    let objects = objects.clone();
                    let received = received.clone();
                    async move {
                        *received.lock().unwrap().entry(key.clone()).or_default() += 1;
                        match objects.get(&key) {
                            Some(bytes) => (reqwest::StatusCode::OK, bytes.clone()),
                            None => (reqwest::StatusCode::NOT_FOUND, Vec::new()),
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = reqwest::Url::parse(&format!("http://{}/", listener.local_addr()?))?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        // Production construction still requires HTTPS. Only this fixture uses loopback.
        let gateway = ArtifactGateway {
            client: reqwest::Client::new(),
            base,
            token: "fixture".into(),
        };
        let result = gateway.receipt(&task).await;
        server.abort();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("artifact readback rejected"));
        assert_eq!(requests.lock().unwrap().get(&block_key), Some(&1));
        Ok(())
    }
}
