//! One bounded preparation worker. It neither claims tasks nor publishes PG
//! results; the reconciler independently reads artifacts and commits completion.
use crate::{
    clickhouse::ClickHouse,
    orchestrator::{Artifact, ResultReceipt, Task},
    postgres::PreparationPermit,
    sha256,
};
use anyhow::{ensure, Context, Result};
use hft_cex_research_input::{
    data::{self, BlockOrder, BlockRef, Exit, PublishedView, TypedBlock},
    prepared,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareConfig {
    pub database_url_file: String,
    pub clickhouse_endpoint: String,
    pub clickhouse_user_file: String,
    pub clickhouse_password_file: String,
    #[serde(default)]
    pub clickhouse_tls: crate::transport::TlsConfig,
    pub artifact_gateway: String,
    pub artifact_token_file: String,
    #[serde(default)]
    pub artifact_tls: crate::transport::TlsConfig,
    pub rows_per_block: u16,
    pub max_blocks: u16,
}
/// The gateway must enforce per-attempt write identity and immutable object keys.
/// No bucket-wide key, signing key, deployment or database write key is needed.
pub struct Writer {
    client: reqwest::Client,
    base: reqwest::Url,
    token: String,
    prefix: String,
}
impl Writer {
    pub fn new(endpoint: &str, token: String, task: &Task) -> Result<Self> {
        Self::with_tls(
            endpoint,
            token,
            task,
            &crate::transport::TlsConfig::default(),
        )
    }
    pub fn with_tls(
        endpoint: &str,
        token: String,
        task: &Task,
        tls: &crate::transport::TlsConfig,
    ) -> Result<Self> {
        let base = reqwest::Url::parse(endpoint)?;
        ensure!(
            base.scheme() == "https"
                && base.username().is_empty()
                && base.password().is_none()
                && base.query().is_none()
                && base.path().ends_with('/')
                && !token.is_empty(),
            "invalid worker artifact identity"
        );
        Ok(Self {
            client: tls.client(std::time::Duration::from_secs(30), true)?,
            base,
            token,
            prefix: format!("{}/{}/{}/", task.spec.output_prefix, task.id, task.attempt),
        })
    }
    async fn put(&self, name: &str, bytes: Vec<u8>) -> Result<Artifact> {
        ensure!(
            name.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                && !name.contains("..")
                && !bytes.is_empty()
                && bytes.len() <= 16 * 1024 * 1024,
            "invalid worker output"
        );
        let artifact = Artifact {
            key: format!("{}{name}", self.prefix),
            sha256: sha256(&bytes),
            bytes: bytes.len() as u64,
        };
        let url = self.base.join(&artifact.key)?;
        let response = self
            .client
            .put(url.clone())
            .bearer_auth(&self.token)
            .header("If-None-Match", "*")
            .body(bytes)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("artifact upload unavailable"))?;
        if response.status() == reqwest::StatusCode::CONFLICT
            || response.status() == reqwest::StatusCode::PRECONDITION_FAILED
        {
            let mut response = self
                .client
                .get(url)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|_| anyhow::anyhow!("artifact upload recovery unavailable"))?
                .error_for_status()
                .map_err(|_| anyhow::anyhow!("artifact upload recovery rejected"))?;
            let mut actual = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow::anyhow!("artifact upload recovery interrupted"))?
            {
                ensure!(
                    actual.len() as u64 + chunk.len() as u64 <= artifact.bytes,
                    "conflicting output size"
                );
                actual.extend_from_slice(&chunk);
            }
            ensure!(
                actual.len() as u64 == artifact.bytes && sha256(&actual) == artifact.sha256,
                "conflicting immutable output"
            );
        } else {
            response
                .error_for_status()
                .map_err(|_| anyhow::anyhow!("artifact upload rejected"))?;
        }
        Ok(artifact)
    }
}
fn rows(block: &TypedBlock) -> usize {
    match block {
        TypedBlock::Features(v) => v.len(),
        TypedBlock::Training(v) => v.len(),
        TypedBlock::Replay(v) => v.len(),
    }
}
fn cursor(block: &TypedBlock) -> Option<crate::clickhouse::Cursor> {
    match block {
        TypedBlock::Features(v) => v
            .last()
            .map(|r| (r.available_ns, r.segment.clone(), r.ordinal)),
        TypedBlock::Training(v) => v.last().map(|r| {
            (
                r.feature.available_ns,
                r.feature.segment.clone(),
                r.feature.ordinal,
            )
        }),
        TypedBlock::Replay(v) => v
            .last()
            .map(|r| (r.available_ns, r.segment.clone(), r.ordinal)),
    }
}
pub async fn prepare(
    permit: &mut PreparationPermit,
    ch: &ClickHouse,
    writer: &Writer,
    rows_per_block: u16,
    max_blocks: u16,
) -> Result<ResultReceipt> {
    ensure!(
        (1..=4096).contains(&rows_per_block) && (3..=256).contains(&max_blocks),
        "unbounded preparation"
    );
    ch.prepare(permit).await?;
    let mut view = PublishedView {
        prepared_id: permit.generation().into(),
        spec: permit.plan().spec.clone(),
        blocks: Vec::new(),
        producer_image: permit.plan().producer_image.clone(),
        source_receipt_sha256: permit.plan().source_receipt_sha256.clone(),
    };
    let mut artifacts = Vec::new();
    for exit in [Exit::Features, Exit::Training, Exit::Replay] {
        let mut after = None;
        let mut order = BlockOrder::default();
        let before = view.blocks.len();
        loop {
            let block = ch
                .preparation_block(permit, exit.clone(), after, rows_per_block)
                .await?;
            if rows(&block) == 0 {
                break;
            }
            ensure!(
                view.blocks.len() < usize::from(max_blocks),
                "preparation block budget exceeded"
            );
            after = cursor(&block);
            let bytes = prepared::encode(&block)?;
            let decoded = prepared::decode(&bytes)?;
            let reference = BlockRef {
                sha256: sha256(&bytes),
                bytes: bytes.len() as u64,
                rows: rows(&decoded) as u64,
                decoded_bytes: data::memory_bytes(&decoded),
                exit: exit.clone(),
            };
            data::validate_block(&decoded, &reference, &view.spec)?;
            order.observe(&decoded)?;
            permit.check().await?;
            artifacts.push(
                writer
                    .put(&format!("{}.mondaybin", reference.sha256), bytes)
                    .await?,
            );
            view.blocks.push(reference);
        }
        ensure!(view.blocks.len() > before, "preparation exit is empty");
    }
    permit.check().await?;
    let task = permit.task();
    let receipt = ResultReceipt {
        task_id: task.id.clone(),
        attempt: task.attempt,
        fence: task.fence,
        view_manifest_sha256: task.spec.view_manifest_sha256.clone(),
        source_sha256: task.spec.source_sha256.clone(),
        image: task.spec.image.clone(),
        fit_identity_sha256: task.spec.fit_identity_sha256.clone(),
        artifacts,
        checkpoint: None,
        prepared_view: Some(view),
    };
    receipt.validate(
        &task.spec,
        task.lease.as_ref().context("worker lacks lease")?,
    )?;
    writer
        .put("receipt.json", serde_json::to_vec(&receipt)?)
        .await?;
    Ok(receipt)
}
