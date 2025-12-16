//! HFT Ops Agent CLI
//!
//! Usage:
//!   hft-agent --config configs/agent.toml
//!   hft-agent --dry-run  # Test mode without executing actions

use anyhow::{Context, Result};
use clap::Parser;
use hft_agent::{AgentConfig, HftOpsAgent};
use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

#[derive(Parser, Debug)]
#[command(name = "hft-agent")]
#[command(about = "HFT Ops Agent - LLM-powered autonomous trading system operations")]
#[command(version)]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "configs/agent.toml")]
    config: PathBuf,

    /// Enable dry-run mode (log actions without executing)
    #[arg(long)]
    dry_run: bool,

    /// Override the monitoring interval in seconds
    #[arg(long)]
    interval: Option<u64>,

    /// Override the log level (trace, debug, info, warn, error)
    #[arg(long)]
    log_level: Option<String>,

    /// gRPC server address override
    #[arg(long)]
    grpc_address: Option<String>,

    /// Run a single monitoring cycle and exit
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Load configuration
    let mut config = if args.config.exists() {
        AgentConfig::load(&args.config)
            .with_context(|| format!("Failed to load config from {:?}", args.config))?
    } else {
        info!("Config file not found, using defaults");
        AgentConfig::default()
    };

    // Apply CLI overrides
    if args.dry_run {
        config.agent.dry_run = true;
    }
    if let Some(interval) = args.interval {
        config.agent.monitor_interval_secs = interval;
    }
    if let Some(log_level) = args.log_level {
        config.logging.level = log_level;
    }
    if let Some(addr) = args.grpc_address {
        config.grpc.address = addr;
    }

    // Initialize logging
    let log_level: Level = config.logging.level.parse().unwrap_or(Level::INFO);
    let filter = EnvFilter::from_default_env()
        .add_directive(log_level.into());

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    info!(
        dry_run = config.agent.dry_run,
        interval = config.agent.monitor_interval_secs,
        grpc_address = %config.grpc.address,
        "Starting HFT Ops Agent"
    );

    // Create and run agent
    let mut agent = HftOpsAgent::new(config)
        .await
        .context("Failed to create HFT Ops Agent")?;

    if args.once {
        info!("Running single monitoring cycle");
        // For --once mode, we need to expose a method to run one cycle
        // For now, this will just start and exit after first cycle
        // TODO: Add run_once() method
        agent.run().await?;
    } else {
        agent.run().await?;
    }

    Ok(())
}
