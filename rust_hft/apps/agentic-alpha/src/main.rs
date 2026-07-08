use clap::{Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "hft-agentic-alpha")]
#[command(about = "Agentic Alpha Harness loop orchestrator")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the MVP loop topology as JSON.
    Topology,
}

#[derive(Debug, Serialize)]
struct LoopSpec {
    name: &'static str,
    trigger: &'static str,
    output_artifact: &'static str,
    stop_condition: &'static str,
}

#[derive(Debug, Serialize)]
struct Topology {
    loops: Vec<LoopSpec>,
    contract_bindings: Vec<&'static str>,
}

fn topology() -> Vec<LoopSpec> {
    vec![
        LoopSpec {
            name: "daily_research_loop",
            trigger: "schedule",
            output_artifact: "proposal_artifacts_and_factor_bank_records",
            stop_condition: "budget_or_candidate_limit",
        },
        LoopSpec {
            name: "event_triggered_opportunity_loop",
            trigger: "market_or_external_event",
            output_artifact: "manifested_opportunity_proposals",
            stop_condition: "event_window_closed",
        },
        LoopSpec {
            name: "trading_loop",
            trigger: "approved_factor_or_policy",
            output_artifact: "promotion_decision_and_live_rollout_manifest",
            stop_condition: "gate_failed_or_rollout_complete",
        },
        LoopSpec {
            name: "live_decay_loop",
            trigger: "live_or_shadow_attribution_update",
            output_artifact: "decay_or_rollback_decision",
            stop_condition: "asset_retired_or_healthy",
        },
        LoopSpec {
            name: "harness_improvement_loop",
            trigger: "repeated_failure_cluster",
            output_artifact: "validated_harness_change_proposal",
            stop_condition: "held_out_regression_passed_or_rejected",
        },
    ]
}

fn contract_bindings() -> Vec<&'static str> {
    vec![
        std::any::type_name::<hft_factor_eval::EvaluationDecision>(),
        std::any::type_name::<hft_factor_store::FactorQuery>(),
        std::any::type_name::<hft_artifact_store::ArtifactRecord>(),
        std::any::type_name::<hft_experiment_store::ExperimentRun>(),
        std::any::type_name::<hft_promotion_gate::PromotionGateDecision>(),
    ]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    match args.command {
        Command::Topology => {
            println!(
                "{}",
                serde_json::to_string_pretty(&Topology {
                    loops: topology(),
                    contract_bindings: contract_bindings(),
                })?
            );
        }
    }
    Ok(())
}
