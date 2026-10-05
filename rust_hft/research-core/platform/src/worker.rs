//! One bounded preparation worker. It neither claims tasks nor publishes PG
//! results; the reconciler independently reads artifacts and commits completion.
use crate::{
    artifact_io::Writer, clickhouse::ClickHouse, orchestrator::ResultReceipt,
    postgres::PreparationPermit, sha256,
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
