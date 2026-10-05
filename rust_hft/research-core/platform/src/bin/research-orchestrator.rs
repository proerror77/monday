use anyhow::{bail, Context, Result};
use hft_research_platform::{
    execution::Kubernetes,
    postgres::Ledger,
    service::{read_secret, ArtifactGateway, Reconciler, ServiceConfig},
};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 || args[1] != "--config" {
        bail!("usage: research-orchestrator --config CONFIG.json");
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(&args[2])?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        bail!("configuration exceeds bound");
    }
    let config: ServiceConfig = serde_json::from_slice(&bytes)?;
    anyhow::ensure!(
        (60_000..=300_000).contains(&config.lease_ms),
        "lease must cover bounded provider I/O"
    );
    let database =
        std::env::var("MONDAY_RESEARCH_DATABASE_URL").context("PG database URL is required")?;
    let reconciler = Reconciler {
        ledger: Ledger::connect(&database).await?,
        kubernetes: Kubernetes::new(
            &config.kubernetes_endpoint,
            read_secret(&config.kubernetes_token_file)?,
            config.cluster.clone(),
            &std::fs::read(&config.kubernetes_ca_file)?,
        )?,
        artifacts: ArtifactGateway::with_tls(
            &config.artifact_gateway,
            read_secret(&config.artifact_token_file)?,
            &config.artifact_tls,
        )?,
        owner: config.owner,
        lease_ms: config.lease_ms,
    };
    let api_handle = if let Some(api) = config.agent_api {
        api.validate()?;
        let ledger = reconciler.ledger.clone();
        Some(hft_research_platform::agent_api::start(api, ledger).await?)
    } else {
        None
    };
    loop {
        anyhow::ensure!(
            !api_handle.as_ref().is_some_and(|h| h.is_finished()),
            "research tool API stopped"
        );
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = reconciler.tick() => {
                // Transport errors must not leak credential-bearing URLs.
                let stage = if result.is_ok() { "reconciled" } else { "reconcile_blocked" };
                println!("{}", hft_research_platform::service::event("", stage));
                if result.is_err() { eprintln!("reconciliation blocked; existing task identities retained"); }
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    }
    if let Some(handle) = api_handle {
        handle.abort();
    }
    Ok(())
}
