mod cli;
mod data_mission;
mod governance;
mod loop_control;
mod mission;
mod mission_campaign;
mod mission_dispatch;
mod mission_render;
mod mission_runner;
mod prediction_dispatch;
mod prediction_runner;
mod prediction_snapshot;

use clap::Parser;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let result = cli::run(cli::Cli::parse()).await;
    if result.is_err() {
        mission_runner::research_event(
            "alpha-harness",
            "command_failed",
            serde_json::json!({"reason_code": "command_failed"}),
        );
    }
    result
}
