mod cli;
mod data_mission;
mod governance;
mod mission;

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    cli::run(cli::Cli::parse()).await
}
