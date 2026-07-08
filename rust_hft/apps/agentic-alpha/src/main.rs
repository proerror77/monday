use clap::{Parser, Subcommand};
use hft_artifact_store::{ArtifactRecord, ArtifactStore, InMemoryArtifactStore};
use hft_experiment_store::{ExperimentRun, ExperimentStore, InMemoryExperimentStore};
use hft_factor_bank::{FactorAsset, FactorLineage, FactorMetrics, FactorStatus, FactorType};
use hft_factor_dsl::{FactorAst, FactorTerminal};
use hft_factor_eval::{evaluate_factor, EvaluationInput, EvaluationThresholds};
use hft_factor_store::{FactorQuery, FactorStore, InMemoryFactorStore};
use hft_promotion_gate::{evaluate_promotion, PromotionGateInput, TargetStage};
use hft_prototype_adapter::{
    known_python_prototypes, PrototypeProposalAdapter, PrototypeRunRequest, StaticPrototypeAdapter,
};
use hft_research_manifest::{ArtifactRef, ManifestId, ManifestRef};
use serde::Serialize;
use std::collections::BTreeMap;

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
    /// Run a local in-memory research-to-promotion smoke loop.
    DemoLoop,
    /// Print the Python prototypes currently wrapped by Rust contracts.
    PrototypeBackends,
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

#[derive(Debug, Serialize)]
struct DemoLoopReport {
    experiment_id: String,
    stored_proposals: usize,
    stored_factors: usize,
    evaluation_passed: bool,
    promotion_passed: bool,
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
        std::any::type_name::<hft_prototype_adapter::PrototypeBackend>(),
    ]
}

fn reference(id: &str, kind: &str) -> Result<ManifestRef, Box<dyn std::error::Error>> {
    Ok(ManifestRef::new(ManifestId::new(id)?, kind)?)
}

fn factor_metrics() -> FactorMetrics {
    FactorMetrics {
        rank_ic: Some(0.04),
        icir: Some(1.3),
        net_sharpe: Some(1.5),
        max_drawdown: Some(0.04),
        turnover: Some(1.8),
        custom: BTreeMap::new(),
    }
}

fn demo_loop() -> Result<DemoLoopReport, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let ast = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
    let search_manifest_id = ManifestId::new("search-demo-1")?;
    let prototype = StaticPrototypeAdapter::new(known_python_prototypes().remove(0), ast.clone())?;
    let proposals = prototype.propose(
        &PrototypeRunRequest {
            search_manifest_id: search_manifest_id.clone(),
            max_candidates: 1,
        },
        now,
    )?;

    let mut experiment_store = InMemoryExperimentStore::default();
    experiment_store.put_run(ExperimentRun {
        experiment_id: "experiment-demo-1".to_string(),
        search_manifest_id,
        proposals,
        started_at: now,
        completed_at: Some(now),
    })?;
    let run = experiment_store.get_run("experiment-demo-1")?;

    let data_manifest = reference("data-demo-1", "data_manifest")?;
    let evaluation = evaluate_factor(
        &EvaluationInput {
            dataset_manifest: data_manifest.clone(),
            has_available_time: true,
            sample_count: 100,
            metrics: factor_metrics(),
        },
        &EvaluationThresholds {
            min_sample_count: 50,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown: 0.05,
            max_correlation: 0.8,
        },
        Some(0.2),
    );

    let asset = FactorAsset {
        factor_id: "factor-demo-1".to_string(),
        factor_type: FactorType::Formula,
        ast,
        lineage: FactorLineage {
            parent_factor_ids: vec![],
            source_engine: "manual_seed".to_string(),
            search_manifest_id: ManifestId::new("search-demo-1")?,
        },
        data_manifest,
        feature_manifest: reference("feature-demo-1", "feature_manifest")?,
        label_manifest: reference("label-demo-1", "label_manifest")?,
        evaluation_manifests: vec![reference("eval-demo-1", "evaluation_manifest")?],
        metrics: factor_metrics(),
        correlation_cluster: None,
        regime_metrics: BTreeMap::new(),
        symbol_metrics: BTreeMap::new(),
        promotion_status: FactorStatus::LiveShadow,
        live_decay_state: None,
        created_at: now,
        updated_at: now,
    };

    let mut factor_store = InMemoryFactorStore::default();
    factor_store.upsert_factor(asset.clone())?;
    let stored_factors = factor_store
        .list_factors(FactorQuery {
            status: Some(FactorStatus::LiveShadow),
            limit: 10,
        })?
        .len();

    let promotion = evaluate_promotion(
        &asset,
        &PromotionGateInput {
            target_stage: TargetStage::LiveSmall,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown_ceiling: 0.05,
            first_same_class_approval_present: true,
        },
    );

    let mut artifact_store = InMemoryArtifactStore::default();
    artifact_store.put_artifact(ArtifactRecord {
        manifest_id: ManifestId::new("eval-demo-1")?,
        artifact: ArtifactRef {
            uri: "artifact://demo-loop/evaluation.json".to_string(),
            content_type: "application/json".to_string(),
            checksum: None,
        },
    })?;

    Ok(DemoLoopReport {
        experiment_id: run.experiment_id,
        stored_proposals: run.proposals.len(),
        stored_factors,
        evaluation_passed: evaluation.passed,
        promotion_passed: promotion.passed,
    })
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
        Command::DemoLoop => {
            println!("{}", serde_json::to_string_pretty(&demo_loop()?)?);
        }
        Command::PrototypeBackends => {
            println!(
                "{}",
                serde_json::to_string_pretty(&known_python_prototypes())?
            );
        }
    }
    Ok(())
}
