mod cli;
mod data_mission;
#[cfg(any(feature = "operator", test))]
mod governance;
#[cfg(feature = "operator")]
mod loop_control;
mod mission;
mod mission_calendar;
mod mission_campaign;
mod mission_dispatch;
#[cfg(feature = "operator")]
mod mission_fresh_inputs;
mod mission_metrics;
mod mission_objects;
mod mission_render;
mod mission_runner;
pub mod representation_plan;
#[cfg(feature = "operator")]
mod sec_orderflow;
pub use mission_campaign::representation::{
    bind_signed_prepared_representation_campaign, preview_representation_campaign_contracts,
};

use clap::Parser;

#[cfg(feature = "operator")]
pub async fn operator_main() -> anyhow::Result<()> {
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

#[cfg(feature = "scientific")]
pub async fn worker_main() -> anyhow::Result<()> {
    let argv = std::env::args_os().collect::<Vec<_>>();
    if argv
        .get(1)
        .is_some_and(|arg| arg == "--stage-configuration")
    {
        if argv.len() != 2 {
            anyhow::bail!("configuration staging accepts no path, execution, or network arguments");
        }
        let context = hft_research_platform::orchestrator::AttemptContext::from_environment()?;
        return hft_research_platform::worker_configuration::stage_configuration(&context);
    }
    let result = cli::run_worker(cli::WorkerCli::parse()).await;
    if result.is_err() {
        mission_runner::research_event(
            "monday-cex-worker",
            "command_failed",
            serde_json::json!({"reason_code":"command_failed"}),
        );
    }
    result
}

#[cfg(test)]
mod worker_boundary_tests {
    use super::*;
    #[cfg(feature = "operator")]
    #[test]
    fn operator_cannot_execute_a_scientific_campaign() {
        assert!(cli::Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "campaign-execute",
            "--pre-holdout",
            "--request",
            "/inputs/campaign.json",
            "--request-sha256",
            &"a".repeat(64),
            "--campaign-id",
            "campaign-unit",
            "--image-identity",
            &format!("sha256:{}", "b".repeat(64)),
            "--work-dir",
            "/work"
        ])
        .is_err());
    }
    #[cfg(feature = "scientific")]
    #[test]
    fn worker_accepts_native_campaign_argv_and_rejects_operator_routes() {
        cli::WorkerCli::try_parse_from([
            "monday-cex-worker",
            "mission",
            "campaign-execute",
            "--pre-holdout",
            "--request",
            "/inputs/campaign.json",
            "--request-sha256",
            &"a".repeat(64),
            "--campaign-id",
            "campaign-unit",
            "--image-identity",
            &format!("sha256:{}", "b".repeat(64)),
            "--work-dir",
            "/work",
        ])
        .unwrap();
        for command in [
            "dispatch",
            "run",
            "learn",
            "campaign-freeze",
            "campaign-finalize",
        ] {
            assert!(
                cli::WorkerCli::try_parse_from(["monday-cex-worker", "mission", command]).is_err()
            );
        }
        for command in ["data", "loop", "approval", "deployment", "research"] {
            assert!(cli::WorkerCli::try_parse_from(["monday-cex-worker", command]).is_err());
        }
    }
}
