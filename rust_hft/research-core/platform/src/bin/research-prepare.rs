use anyhow::{bail, Context, Result};
use hft_research_platform::{
    artifact_io::Writer, clickhouse::ClickHouse, orchestrator::AttemptContext, postgres::Ledger,
    service::read_secret, worker::PrepareConfig,
};
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || args[1] != "--config" {
        bail!("usage: research-prepare --config CONFIG.json");
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(&args[2])?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 1024 * 1024, "worker config exceeds bound");
    let config: PrepareConfig = serde_json::from_slice(&bytes)?;
    let id = std::env::var("MONDAY_TASK_ID").context("task identity required")?;
    let attempt: u32 = std::env::var("MONDAY_ATTEMPT")?.parse()?;
    let fence: i64 = std::env::var("MONDAY_FENCE")?.parse()?;
    let ledger = Ledger::connect(&read_secret(&config.database_url_file)?).await?;
    // Container launch may precede the controller's durable Running commit.
    // Wait on the same identity, never claim or create another attempt.
    let mut admitted = false;
    for _ in 0..30 {
        let task = ledger.read(&id).await?;
        anyhow::ensure!(
            task.attempt == attempt
                && task.fence == fence
                && !task.state.terminal()
                && task.state != hft_research_platform::orchestrator::State::Stopping,
            "attempt superseded during startup"
        );
        if task.state == hft_research_platform::orchestrator::State::Running {
            admitted = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::ensure!(admitted, "controller startup admission timed out");
    let mut permit = ledger.preparation(&id, attempt, fence).await?;
    let ch = ClickHouse::with_tls(
        &config.clickhouse_endpoint,
        read_secret(&config.clickhouse_user_file)?,
        read_secret(&config.clickhouse_password_file)?,
        &config.clickhouse_tls,
    )?;
    let writer = Writer::with_tls(
        &config.artifact_gateway,
        read_secret(&config.artifact_token_file)?,
        &AttemptContext {
            spec: permit.task().spec.clone(),
            lease: permit.task().lease.clone().context("worker lacks lease")?,
        },
        &config.artifact_tls,
    )?;
    hft_research_platform::worker::prepare(
        &mut permit,
        &ch,
        &writer,
        config.rows_per_block,
        config.max_blocks,
    )
    .await?;
    println!(
        "{}",
        hft_research_platform::service::event(&id, "prepared_receipt_written")
    );
    Ok(())
}
