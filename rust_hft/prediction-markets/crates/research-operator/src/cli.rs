use crate::dispatch;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(value) => value,
    None => "unbound-source-revision",
};

#[derive(Debug, Parser)]
#[command(name = "monday-prediction-operator", version = BUILD_SOURCE_REVISION,
          about = "Governed prediction-market research dispatch and readback")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Dispatch {
        #[command(subcommand)]
        command: DispatchCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DispatchCommand {
    Render(PredictionDispatchRenderArgs),
    Status(PredictionDispatchStatusArgs),
    Submit(PredictionDispatchSubmitArgs),
}

#[derive(Debug, Clone, Args)]
pub struct PredictionDispatchRenderArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub namespace: String,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionDispatchSubmitArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionDispatchStatusArgs {
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    #[arg(long)]
    pub job_name: String,
    #[arg(long)]
    pub evidence: Option<PathBuf>,
}

pub fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Dispatch { command } => match command {
            DispatchCommand::Render(args) => dispatch::render(args),
            DispatchCommand::Status(args) => dispatch::status(args),
            DispatchCommand::Submit(args) => dispatch::submit(args),
        },
    }
}

pub(crate) fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prediction_dispatch_render() {
        let args = "monday-prediction-operator dispatch render --submission submission.json --namespace monday-research";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[test]
    fn parses_prediction_dispatch_status_with_explicit_cluster_identity() {
        let args = "monday-prediction-operator dispatch status --context ack --namespace monday-research --job-name prediction-job";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }
}
