mod cli;
mod prediction_runner;
mod prediction_snapshot;
use clap::Parser;
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    cli::run(cli::Cli::parse()).await
}
