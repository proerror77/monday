use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "harnessctl")]
#[command(about = "Agentic Alpha Harness operator helper")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print validation lanes for local development.
    ValidationLanes,
}

fn main() {
    let args = Args::parse();
    match args.command {
        Command::ValidationLanes => {
            println!("contracts: cargo test -p <changed-research-crate> --locked");
            println!("stores: cargo test -p <changed-store-crate> --locked");
            println!("orchestrator: cargo check -p hft-agentic-alpha --locked");
            println!("runtime: cargo check -p hft-live --locked");
        }
    }
}
