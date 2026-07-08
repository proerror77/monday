use clap::{Parser, Subcommand};
use hft_allocator_policy::{AllocatorPolicyProposal, FactorAllocation};
use hft_artifact_store::{ArtifactRecord, ArtifactStore, FileArtifactStore, InMemoryArtifactStore};
use hft_audit_trail::HarnessAuditBundle;
use hft_experiment_store::{
    ExperimentRun, ExperimentStore, FileExperimentStore, InMemoryExperimentStore,
};
use hft_factor_bank::{FactorAsset, FactorLineage, FactorMetrics, FactorStatus, FactorType};
use hft_factor_dsl::{FactorAst, FactorTerminal};
use hft_factor_eval::{
    evaluate_factor, evaluate_replay_csv, EvaluationDecision, EvaluationInput,
    EvaluationThresholds, ReplayCsvConfig,
};
use hft_factor_store::{FactorQuery, FactorStore, FileFactorStore, InMemoryFactorStore};
use hft_live_small_supervisor::{
    arm_runtime_command, rollback, runtime_command_from_decision, supervise_rollout,
    LiveSmallAction, LiveSmallDecision, LiveSmallPolicyLimits, LiveSmallRolloutRequest,
    LiveSmallRuntimeCommand, RollbackTrigger,
};
use hft_loop_engine::{
    evaluate_loop_run, CandidateLoopEvidence, DoneCondition, LoopNextAction, LoopRun,
    LoopStageKind, LoopStageRecord, LoopStageStatus, LoopTrigger,
};
use hft_promotion_gate::{evaluate_promotion, PromotionGateInput, TargetStage};
use hft_prototype_adapter::{
    known_python_prototypes, PrototypeBackendKind, PrototypeProposalAdapter, PrototypeRunRequest,
    StaticPrototypeAdapter,
};
use hft_research_manifest::{ArtifactRef, LiveRolloutManifest, ManifestId, ManifestRef};
use hft_research_memory::{
    learning_directive_from_memory, memory_from_live_small, memory_from_promotion_gate,
    HarnessChangeKind, HarnessChangeProposal, LearningDirective, ResearchMemoryEvent,
};
use hft_search_protocol::{
    run_budgeted_lab_search, SearchBudget, SearchEngineKind, SearchRunRequest,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// Run live-small supervision readback without touching execution.
    LiveSmallDemo,
    /// Convert failures into structured memory and harness-change proposal.
    MemoryDemo,
    /// Persist a learned next-loop directive from research memory.
    LearningDemo { output: PathBuf },
    /// Validate an allocator/risk policy proposal without mutating live weights.
    AllocatorDemo,
    /// Build a validated audit bundle for a local harness loop.
    AuditDemo,
    /// Export a validated audit bundle to a JSON file.
    ExportAudit { output: PathBuf },
    /// Run the deterministic loop-engine progress check.
    LoopEngineDemo,
    /// Run budgeted MCTS/RL/LLM proposal loops without real data.
    EngineLoopDemo { output: PathBuf },
    /// Emit a live-small runtime command. Non-dry-run requires --approval-ref.
    LiveCommandDemo {
        output: PathBuf,
        #[arg(long)]
        approval_ref: Option<String>,
    },
    /// Persist a generated factor candidate into a file-backed candidate pool.
    FactorPoolDemo { output: PathBuf },
    /// Persist a prototype-backed experiment run into a file-backed log.
    ExperimentLogDemo { output: PathBuf },
    /// Run all registered Python prototypes as lab-only proposal backends.
    PrototypeLabDemo { output: PathBuf },
    /// Print Python prototype retirement status.
    PythonRetirementDemo,
    /// Evaluate a replay CSV and persist the resulting candidate.
    ReplayEval {
        input: PathBuf,
        factor_pool: PathBuf,
        report: PathBuf,
        #[arg(long, default_value = "oi_delta_5m")]
        signal_column: String,
        #[arg(long, default_value = "forward_return")]
        label_column: String,
    },
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
    live_small_action: LiveSmallAction,
    allocator_policy_id: String,
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
        std::any::type_name::<hft_live_small_supervisor::LiveSmallDecision>(),
        std::any::type_name::<hft_promotion_gate::PromotionGateDecision>(),
        std::any::type_name::<hft_prototype_adapter::PrototypeBackend>(),
        std::any::type_name::<hft_research_memory::ResearchMemoryEvent>(),
        std::any::type_name::<hft_allocator_policy::AllocatorPolicyProposal>(),
        std::any::type_name::<hft_audit_trail::HarnessAuditBundle>(),
        std::any::type_name::<hft_loop_engine::LoopRun>(),
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
    let rollout = live_small_decision(promotion.clone())?;

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
        live_small_action: rollout.action,
        allocator_policy_id: allocator_policy()?.policy_id,
    })
}

#[derive(Debug, Serialize)]
struct FactorPoolDemoReport {
    output: String,
    stored_factors: usize,
    factor_id: String,
}

fn factor_pool_demo(output: &Path) -> Result<FactorPoolDemoReport, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let asset = FactorAsset {
        factor_id: "factor-demo-1".to_string(),
        factor_type: FactorType::Formula,
        ast: FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
        lineage: FactorLineage {
            parent_factor_ids: vec![],
            source_engine: "llm_proposer".to_string(),
            search_manifest_id: ManifestId::new("search-demo-1")?,
        },
        data_manifest: reference("data-demo-1", "data_manifest")?,
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
    let mut store = FileFactorStore::new(output);
    store.upsert_factor(asset.clone())?;
    let stored_factors = store
        .list_factors(FactorQuery {
            status: Some(FactorStatus::LiveShadow),
            limit: 10,
        })?
        .len();

    Ok(FactorPoolDemoReport {
        output: store.path().display().to_string(),
        stored_factors,
        factor_id: asset.factor_id,
    })
}

#[derive(Debug, Serialize)]
struct ExperimentLogDemoReport {
    output: String,
    experiment_id: String,
    stored_proposals: usize,
}

fn experiment_log_demo(
    output: &Path,
) -> Result<ExperimentLogDemoReport, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let search_manifest_id = ManifestId::new("search-demo-1")?;
    let prototype = StaticPrototypeAdapter::new(
        known_python_prototypes().remove(0),
        FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
    )?;
    let proposals = prototype.propose(
        &PrototypeRunRequest {
            search_manifest_id: search_manifest_id.clone(),
            max_candidates: 1,
        },
        now,
    )?;
    let run = ExperimentRun {
        experiment_id: "experiment-demo-1".to_string(),
        search_manifest_id,
        proposals,
        started_at: now,
        completed_at: Some(now),
    };
    let mut store = FileExperimentStore::new(output);
    store.put_run(run)?;
    let run = store.get_run("experiment-demo-1")?;

    Ok(ExperimentLogDemoReport {
        output: store.path().display().to_string(),
        experiment_id: run.experiment_id,
        stored_proposals: run.proposals.len(),
    })
}

#[derive(Debug, Serialize)]
struct PrototypeLabDemoReport {
    output: String,
    experiment_id: String,
    backends: usize,
    stored_proposals: usize,
}

fn prototype_lab_demo(output: &Path) -> Result<PrototypeLabDemoReport, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let search_manifest_id = ManifestId::new("search-prototype-lab-1")?;
    let mut proposals = Vec::new();
    for backend in known_python_prototypes() {
        let adapter = StaticPrototypeAdapter::new(
            backend,
            FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
        )?;
        proposals.extend(adapter.propose(
            &PrototypeRunRequest {
                search_manifest_id: search_manifest_id.clone(),
                max_candidates: 1,
            },
            now,
        )?);
    }
    let backends = proposals.len();
    let run = ExperimentRun {
        experiment_id: "prototype-lab-demo-1".to_string(),
        search_manifest_id,
        proposals,
        started_at: now,
        completed_at: Some(now),
    };
    let mut store = FileExperimentStore::new(output);
    store.put_run(run)?;
    let run = store.get_run("prototype-lab-demo-1")?;

    Ok(PrototypeLabDemoReport {
        output: store.path().display().to_string(),
        experiment_id: run.experiment_id,
        backends,
        stored_proposals: run.proposals.len(),
    })
}

#[derive(Debug, Serialize)]
struct EngineLoopDemoReport {
    output: String,
    runs: usize,
    stored_proposals: usize,
}

fn engine_loop_demo(output: &Path) -> Result<EngineLoopDemoReport, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let ast = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
    let engines = [
        ("mcts-lab", SearchEngineKind::Mcts, 8, 0),
        ("rl-lab", SearchEngineKind::ReinforcementLearning, 16, 0),
        ("llm-lab", SearchEngineKind::LlmProposer, 0, 2_000),
    ];
    let mut proposals = Vec::new();
    for (run_id, engine, max_expansions, max_tokens) in engines {
        let report = run_budgeted_lab_search(
            SearchRunRequest {
                run_id: run_id.to_string(),
                engine,
                search_manifest_id: ManifestId::new("search-engine-loop-1")?,
                budget: SearchBudget {
                    max_candidates: 1,
                    max_expansions,
                    max_tokens,
                    max_seconds: 5,
                },
            },
            ast.clone(),
            now,
        )?;
        proposals.extend(report.proposals);
    }
    let run = ExperimentRun {
        experiment_id: "engine-loop-demo-1".to_string(),
        search_manifest_id: ManifestId::new("search-engine-loop-1")?,
        proposals,
        started_at: now,
        completed_at: Some(now),
    };
    let mut store = FileExperimentStore::new(output);
    store.put_run(run)?;
    let run = store.get_run("engine-loop-demo-1")?;

    Ok(EngineLoopDemoReport {
        output: store.path().display().to_string(),
        runs: 3,
        stored_proposals: run.proposals.len(),
    })
}

#[derive(Debug, Serialize)]
struct LiveCommandDemoReport {
    output: String,
    command: LiveSmallRuntimeCommand,
}

fn live_command_demo(
    output: &Path,
    approval_ref: Option<&str>,
) -> Result<LiveCommandDemoReport, Box<dyn std::error::Error>> {
    let promotion = hft_promotion_gate::PromotionGateDecision {
        passed: true,
        failures: vec![],
    };
    let rollout_manifest = live_rollout_manifest()?;
    let decision = live_small_decision(promotion)?;
    let mut command =
        runtime_command_from_decision("live-small-command-1", rollout_manifest, &decision)
            .ok_or("live-small decision did not produce runtime command")?;
    if let Some(approval_ref) = approval_ref {
        command = arm_runtime_command(
            command,
            ManifestRef::new(ManifestId::new(approval_ref)?, "human_approval")?,
        )?;
    }
    command.validate()?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let report = LiveCommandDemoReport {
        output: output.display().to_string(),
        command,
    };
    std::fs::write(output, serde_json::to_string_pretty(&report)?)?;
    register_artifact("live-small-command-1", output)?;
    Ok(report)
}

#[derive(Debug, Serialize)]
struct PythonRetirementRecord {
    backend_id: String,
    kind: PrototypeBackendKind,
    source_path: String,
    current_status: &'static str,
    python_required_for_harness: bool,
    next_step: &'static str,
}

fn python_retirement_demo() -> Vec<PythonRetirementRecord> {
    known_python_prototypes()
        .into_iter()
        .map(|backend| PythonRetirementRecord {
            backend_id: backend.backend_id,
            kind: backend.kind,
            source_path: backend.source_path,
            current_status: "replaced_by_rust_harness_contract",
            python_required_for_harness: false,
            next_step: "keep source as parity reference until production engine exists",
        })
        .collect()
}

#[derive(Debug, Serialize)]
struct ReplayEvalReport {
    report: String,
    factor_pool: String,
    factor_id: String,
    metrics: FactorMetrics,
    evaluation: EvaluationDecision,
    stored_factors: usize,
}

fn replay_eval(
    input: &Path,
    factor_pool: &Path,
    report: &Path,
    signal_column: &str,
    label_column: &str,
) -> Result<ReplayEvalReport, Box<dyn std::error::Error>> {
    let csv = std::fs::read_to_string(input)?;
    let evaluation = evaluate_replay_csv(
        &csv,
        &ReplayCsvConfig {
            dataset_manifest: reference("data-replay-1", "data_manifest")?,
            signal_column: signal_column.to_string(),
            label_column: label_column.to_string(),
            available_time_column: Some("available_time".to_string()),
        },
        &EvaluationThresholds {
            min_sample_count: 4,
            min_rank_ic: 0.03,
            min_net_sharpe: 1.0,
            max_drawdown: 0.05,
            max_correlation: 0.8,
        },
        Some(0.2),
    )?;
    let now = chrono::Utc::now();
    let factor_id = format!("replay-{signal_column}");
    let asset = FactorAsset {
        factor_id: factor_id.clone(),
        factor_type: FactorType::Formula,
        ast: FactorAst::Terminal(FactorTerminal::Field(signal_column.to_string())),
        lineage: FactorLineage {
            parent_factor_ids: vec![],
            source_engine: "replay_eval".to_string(),
            search_manifest_id: ManifestId::new("search-replay-1")?,
        },
        data_manifest: reference("data-replay-1", "data_manifest")?,
        feature_manifest: reference("feature-replay-1", "feature_manifest")?,
        label_manifest: reference("label-replay-1", "label_manifest")?,
        evaluation_manifests: vec![reference("eval-replay-1", "evaluation_manifest")?],
        metrics: evaluation.metrics.clone(),
        correlation_cluster: None,
        regime_metrics: BTreeMap::new(),
        symbol_metrics: BTreeMap::new(),
        promotion_status: if evaluation.decision.passed {
            FactorStatus::FullBacktestPassed
        } else {
            FactorStatus::Generated
        },
        live_decay_state: None,
        created_at: now,
        updated_at: now,
    };
    let mut factor_store = FileFactorStore::new(factor_pool);
    factor_store.upsert_factor(asset)?;
    let stored_factors = factor_store
        .list_factors(FactorQuery {
            status: None,
            limit: 100,
        })?
        .len();

    if let Some(parent) = report
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let output = ReplayEvalReport {
        report: report.display().to_string(),
        factor_pool: factor_store.path().display().to_string(),
        factor_id,
        metrics: evaluation.metrics,
        evaluation: evaluation.decision,
        stored_factors,
    };
    std::fs::write(report, serde_json::to_string_pretty(&output)?)?;
    register_artifact("eval-replay-1", report)?;
    Ok(output)
}

fn register_artifact(manifest_id: &str, artifact: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let index_root = artifact
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join("artifact-index"))
        .unwrap_or_else(|| PathBuf::from("artifact-index"));
    let mut store = FileArtifactStore::new(index_root);
    store.put_artifact(ArtifactRecord {
        manifest_id: ManifestId::new(manifest_id)?,
        artifact: ArtifactRef {
            uri: artifact.to_string_lossy().to_string(),
            content_type: "application/json".to_string(),
            checksum: None,
        },
    })?;
    Ok(())
}

fn live_rollout_manifest() -> Result<LiveRolloutManifest, Box<dyn std::error::Error>> {
    Ok(LiveRolloutManifest {
        id: ManifestId::new("rollout-demo-1")?,
        promotion_manifest: reference("promotion-demo-1", "promotion_manifest")?,
        runtime_config_ref: "runtime-config-demo".to_string(),
        risk_policy_ref: "risk-policy-demo".to_string(),
        started_at: chrono::Utc::now(),
        ended_at: None,
        attribution: BTreeMap::new(),
        rollback_result: None,
    })
}

fn live_small_decision(
    promotion_decision: hft_promotion_gate::PromotionGateDecision,
) -> Result<LiveSmallDecision, Box<dyn std::error::Error>> {
    let policy = allocator_policy()?;
    Ok(supervise_rollout(&LiveSmallRolloutRequest {
        rollout_manifest: live_rollout_manifest()?,
        promotion_decision,
        policy_limits: policy.live_small_limits.clone(),
        requested_factor_weight: policy.requested_factor_weight(),
        requested_symbol_exposure: policy.requested_symbol_exposure,
    })?)
}

fn allocator_policy() -> Result<AllocatorPolicyProposal, Box<dyn std::error::Error>> {
    let policy = AllocatorPolicyProposal {
        policy_id: "alloc-policy-demo-1".to_string(),
        source_manifest: reference("promotion-demo-1", "promotion_manifest")?,
        allocations: vec![FactorAllocation {
            factor_id: "factor-demo-1".to_string(),
            weight: 0.05,
        }],
        requested_symbol_exposure: 0.1,
        live_small_limits: LiveSmallPolicyLimits {
            max_factor_weight: 0.1,
            max_symbol_exposure: 0.2,
            max_account_drawdown: 0.03,
        },
    };
    policy.validate()?;
    Ok(policy)
}

#[derive(Debug, Serialize)]
struct LiveSmallDemoReport {
    rollout: LiveSmallDecision,
    rollback: LiveSmallDecision,
}

fn live_small_demo() -> Result<LiveSmallDemoReport, Box<dyn std::error::Error>> {
    Ok(LiveSmallDemoReport {
        rollout: live_small_decision(hft_promotion_gate::PromotionGateDecision {
            passed: true,
            failures: vec![],
        })?,
        rollback: rollback(RollbackTrigger::SentinelStopped),
    })
}

#[derive(Debug, Serialize)]
struct MemoryDemoReport {
    events: Vec<ResearchMemoryEvent>,
    harness_change: HarnessChangeProposal,
}

fn memory_demo() -> Result<MemoryDemoReport, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let promotion_failure = hft_promotion_gate::PromotionGateDecision {
        passed: false,
        failures: vec![hft_promotion_gate::GateFailure::ApprovalRequired],
    };
    let rollback_decision = rollback(RollbackTrigger::SentinelStopped);
    let mut events = Vec::new();
    if let Some(event) =
        memory_from_promotion_gate("memory-event-promotion-1", &promotion_failure, now)
    {
        event.validate()?;
        events.push(event);
    }
    if let Some(event) = memory_from_live_small("memory-event-rollback-1", &rollback_decision, now)
    {
        event.validate()?;
        events.push(event);
    }

    let harness_change = HarnessChangeProposal {
        proposal_id: "harness-change-1".to_string(),
        change_kind: HarnessChangeKind::EvaluatorRecipe,
        source_event_ids: events.iter().map(|event| event.event_id.clone()).collect(),
        description: "tighten approval evidence and rollback attribution checks".to_string(),
        grants_live_trading_authority: false,
    };
    harness_change.validate()?;

    Ok(MemoryDemoReport {
        events,
        harness_change,
    })
}

#[derive(Debug, Serialize)]
struct LearningDemoReport {
    output: String,
    directive: LearningDirective,
}

fn learning_demo(output: &Path) -> Result<LearningDemoReport, Box<dyn std::error::Error>> {
    let events = memory_demo()?.events;
    let directive = learning_directive_from_memory("learning-directive-1", &events)
        .ok_or("memory demo did not produce learning events")?;
    directive.validate()?;

    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let report = LearningDemoReport {
        output: output.display().to_string(),
        directive,
    };
    std::fs::write(output, serde_json::to_string_pretty(&report)?)?;
    register_artifact("learning-directive-1", output)?;
    Ok(report)
}

#[derive(Debug, Serialize)]
struct AllocatorDemoReport {
    policy: AllocatorPolicyProposal,
    requested_factor_weight: f64,
}

fn allocator_demo() -> Result<AllocatorDemoReport, Box<dyn std::error::Error>> {
    let policy = allocator_policy()?;
    Ok(AllocatorDemoReport {
        requested_factor_weight: policy.requested_factor_weight(),
        policy,
    })
}

fn audit_bundle() -> Result<HarnessAuditBundle, Box<dyn std::error::Error>> {
    let now = chrono::Utc::now();
    let ast = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
    let search_manifest_id = ManifestId::new("search-demo-1")?;
    let prototype = StaticPrototypeAdapter::new(known_python_prototypes().remove(0), ast)?;
    let proposals = prototype.propose(
        &PrototypeRunRequest {
            search_manifest_id,
            max_candidates: 1,
        },
        now,
    )?;
    let evaluation = evaluate_factor(
        &EvaluationInput {
            dataset_manifest: reference("data-demo-1", "data_manifest")?,
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
    let promotion = hft_promotion_gate::PromotionGateDecision {
        passed: true,
        failures: vec![],
    };
    let bundle = HarnessAuditBundle {
        bundle_id: "audit-demo-1".to_string(),
        proposals,
        evaluation,
        promotion: promotion.clone(),
        allocator_policy: allocator_policy()?,
        live_small: live_small_decision(promotion)?,
        memory_events: memory_demo()?.events,
    };
    bundle.validate()?;
    Ok(bundle)
}

#[derive(Debug, Serialize)]
struct ExportAuditReport {
    output: String,
    artifact_index: String,
    registered_artifacts: usize,
}

fn export_audit(output: &Path) -> Result<ExportAuditReport, Box<dyn std::error::Error>> {
    let bundle = audit_bundle()?;
    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, hft_audit_trail::audit_bundle_json(&bundle)?)?;

    let manifest_id = ManifestId::new(bundle.bundle_id)?;
    let index_root = parent
        .map(|parent| parent.join("artifact-index"))
        .unwrap_or_else(|| PathBuf::from("artifact-index"));
    let mut artifact_store = FileArtifactStore::new(index_root);
    artifact_store.put_artifact(ArtifactRecord {
        manifest_id: manifest_id.clone(),
        artifact: ArtifactRef {
            uri: output.to_string_lossy().to_string(),
            content_type: "application/json".to_string(),
            checksum: None,
        },
    })?;
    let registered_artifacts = artifact_store.get_artifacts(&manifest_id)?.len();

    Ok(ExportAuditReport {
        output: output.display().to_string(),
        artifact_index: artifact_store.root().display().to_string(),
        registered_artifacts,
    })
}

#[derive(Debug, Serialize)]
struct LoopEngineDemoReport {
    run: LoopRun,
    next_action: LoopNextAction,
    evidence_valid: bool,
}

fn loop_engine_demo() -> Result<LoopEngineDemoReport, Box<dyn std::error::Error>> {
    let promotion = hft_promotion_gate::PromotionGateDecision {
        passed: true,
        failures: vec![],
    };
    let evidence = CandidateLoopEvidence {
        proposal: audit_bundle()?.proposals.remove(0),
        evaluation: evaluate_factor(
            &EvaluationInput {
                dataset_manifest: reference("data-demo-1", "data_manifest")?,
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
        ),
        promotion: promotion.clone(),
        live_small: Some(live_small_decision(promotion)?),
    };
    evidence.validate()?;
    let run = LoopRun {
        run_id: "loop-demo-1".to_string(),
        trigger: LoopTrigger::GoalBased,
        goal: "find one auditable live-small candidate without granting live authority".to_string(),
        started_at: chrono::Utc::now(),
        current_iteration: 1,
        max_iterations: 3,
        done_condition: DoneCondition::StagePassed(LoopStageKind::Audit),
        stages: vec![
            loop_stage(LoopStageKind::GatherContext, LoopStageStatus::Passed)?,
            loop_stage(LoopStageKind::GenerateCandidates, LoopStageStatus::Passed)?,
            loop_stage(LoopStageKind::EvaluateCandidates, LoopStageStatus::Passed)?,
            loop_stage(LoopStageKind::PromoteCandidate, LoopStageStatus::Passed)?,
            loop_stage(LoopStageKind::LiveSmallSupervision, LoopStageStatus::Passed)?,
            loop_stage(LoopStageKind::CaptureMemory, LoopStageStatus::Passed)?,
            loop_stage(LoopStageKind::Audit, LoopStageStatus::Passed)?,
        ],
    };
    let next_action = evaluate_loop_run(&run)?;

    Ok(LoopEngineDemoReport {
        run,
        next_action,
        evidence_valid: true,
    })
}

fn loop_stage(
    kind: LoopStageKind,
    status: LoopStageStatus,
) -> Result<LoopStageRecord, Box<dyn std::error::Error>> {
    Ok(LoopStageRecord {
        kind,
        status,
        artifact_refs: vec![reference("audit-demo-1", "audit_bundle")?],
        summary: "validated by local harness contracts".to_string(),
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
        Command::LiveSmallDemo => {
            println!("{}", serde_json::to_string_pretty(&live_small_demo()?)?);
        }
        Command::MemoryDemo => {
            println!("{}", serde_json::to_string_pretty(&memory_demo()?)?);
        }
        Command::LearningDemo { output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&learning_demo(&output)?)?
            );
        }
        Command::AllocatorDemo => {
            println!("{}", serde_json::to_string_pretty(&allocator_demo()?)?);
        }
        Command::AuditDemo => {
            println!("{}", serde_json::to_string_pretty(&audit_bundle()?)?);
        }
        Command::ExportAudit { output } => {
            println!("{}", serde_json::to_string_pretty(&export_audit(&output)?)?);
        }
        Command::LoopEngineDemo => {
            println!("{}", serde_json::to_string_pretty(&loop_engine_demo()?)?);
        }
        Command::EngineLoopDemo { output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&engine_loop_demo(&output)?)?
            );
        }
        Command::LiveCommandDemo {
            output,
            approval_ref,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&live_command_demo(
                    &output,
                    approval_ref.as_deref()
                )?)?
            );
        }
        Command::FactorPoolDemo { output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&factor_pool_demo(&output)?)?
            );
        }
        Command::ExperimentLogDemo { output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&experiment_log_demo(&output)?)?
            );
        }
        Command::PrototypeLabDemo { output } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&prototype_lab_demo(&output)?)?
            );
        }
        Command::PythonRetirementDemo => {
            println!(
                "{}",
                serde_json::to_string_pretty(&python_retirement_demo())?
            );
        }
        Command::ReplayEval {
            input,
            factor_pool,
            report,
            signal_column,
            label_column,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&replay_eval(
                    &input,
                    &factor_pool,
                    &report,
                    &signal_column,
                    &label_column,
                )?)?
            );
        }
    }
    Ok(())
}
