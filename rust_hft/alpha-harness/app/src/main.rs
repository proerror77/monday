mod baseline_evidence;
mod cli;
mod data_mission;
mod governance;
mod loop_control;
mod mission;
mod mission_runner;
mod prediction_dispatch;
mod prediction_runner;
mod prediction_snapshot;

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    cli::run(cli::Cli::parse()).await
}
