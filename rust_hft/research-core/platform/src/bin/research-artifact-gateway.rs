//! Storage host only. Capabilities are provisioned by the trusted identity broker.
use anyhow::{ensure, Context, Result};
use hft_research_platform::{
    artifact_gateway::{Gateway, GatewayConfig},
    postgres::Ledger,
};
use std::{io::Read, sync::Arc};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [flag, path] = args.as_slice() else {
        anyhow::bail!("usage: research-artifact-gateway --config CONFIG.json");
    };
    ensure!(
        flag == "--config",
        "usage: research-artifact-gateway --config CONFIG.json"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "gateway configuration exceeds bound"
    );
    let config: GatewayConfig = serde_json::from_slice(&bytes)?;
    let ledger = Ledger::connect(
        &std::env::var("MONDAY_RESEARCH_DATABASE_URL")
            .context("readonly gateway PG identity required")?,
    )
    .await?;
    Arc::new(Gateway::new(config, ledger)?).serve().await
}
