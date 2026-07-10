use clap::{Parser, Subcommand};
use duckdb::{params, Connection};
use hft_allocator_policy::{AllocatorPolicyProposal, FactorAllocation};
use hft_artifact_store::{ArtifactRecord, ArtifactStore, FileArtifactStore, InMemoryArtifactStore};
use hft_audit_trail::HarnessAuditBundle;
use hft_experiment_store::{
    ExperimentRun, ExperimentStore, FileExperimentStore, InMemoryExperimentStore,
};
use hft_factor_bank::{FactorAsset, FactorLineage, FactorMetrics, FactorStatus, FactorType};
use hft_factor_dsl::{FactorAst, FactorTerminal};
use hft_factor_eval::{
    evaluate_factor, evaluate_replay_csv, EvaluationDecision, EvaluationFailure, EvaluationInput,
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
    FailureKind, HarnessChangeKind, HarnessChangeProposal, LearningDirective, MemorySource,
    ResearchMemoryEvent,
};
use hft_search_protocol::{
    run_budgeted_lab_search, SearchBudget, SearchEngineKind, SearchRunRequest,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Run candidate generation, failure explanation, and learning directive loop.
    AgentLoopDemo {
        output: PathBuf,
        #[arg(long, default_value_t = 3)]
        max_candidates_per_engine: usize,
        #[arg(long, default_value_t = 0.0)]
        max_live_risk_pct: f64,
    },
    /// Run candidate generation and evaluation against a DuckDB replay table.
    DuckdbAgentLoop {
        duckdb_path: PathBuf,
        output: PathBuf,
        #[arg(long, default_value = "factor_replay")]
        table: String,
        #[arg(long, default_value_t = 3)]
        max_candidates_per_engine: usize,
        #[arg(long, default_value_t = 0.0)]
        max_live_risk_pct: f64,
        #[arg(long)]
        bootstrap_fixture: bool,
        #[arg(long)]
        factor_bank: Option<PathBuf>,
    },
    /// Emit a live-small runtime command. Non-dry-run requires --approval-ref.
    LiveCommandDemo {
        output: PathBuf,
        #[arg(long)]
        approval_ref: Option<String>,
    },
    /// Smoke-test real exchange REST and optional EVM JSON-RPC connectivity.
    ConnectivitySmoke {
        output: PathBuf,
        #[arg(long, default_value = "https://api.binance.com/api/v3/ping")]
        exchange_ping_url: String,
        #[arg(long)]
        evm_rpc_url: Option<String>,
        #[arg(long, default_value_t = 3000)]
        timeout_ms: u64,
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

#[derive(Debug, Serialize)]
struct AgentLoopCandidateReport {
    proposal_id: String,
    engine: SearchEngineKind,
    formula: String,
    evaluation: EvaluationDecision,
    validation_evaluation: Option<EvaluationDecision>,
    test_evaluation: Option<EvaluationDecision>,
    failure_explanation: Option<String>,
}

#[derive(Debug, Serialize)]
struct AgentLoopDemoReport {
    output: String,
    data_source: Option<String>,
    data_rows: Option<usize>,
    chronological_holdout: Option<ChronologicalHoldoutReport>,
    factor_bank_path: Option<String>,
    stored_factors: usize,
    max_live_risk_pct: f64,
    proposals: usize,
    passed: usize,
    failed: usize,
    candidates: Vec<AgentLoopCandidateReport>,
    memory_events: Vec<ResearchMemoryEvent>,
    learning_directive: LearningDirective,
}

#[derive(Debug, Serialize)]
struct ChronologicalHoldoutReport {
    train_rows: usize,
    validation_rows: usize,
    test_rows: usize,
}

#[derive(Debug, Clone)]
struct ReplayDataRow {
    oi_delta_5m: f64,
    cvd_slope_5m: f64,
    spread_bps: f64,
    funding_rate: f64,
    depth_imbalance: f64,
    forward_return: f64,
    available_time: bool,
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

fn duckdb_agent_loop(
    duckdb_path: &Path,
    output: &Path,
    table: &str,
    max_candidates_per_engine: usize,
    max_live_risk_pct: f64,
    bootstrap_fixture: bool,
    factor_bank: Option<&Path>,
) -> Result<AgentLoopDemoReport, Box<dyn std::error::Error>> {
    if !is_simple_identifier(table) {
        return Err("DuckDB table must be a simple identifier".into());
    }
    let conn = Connection::open(duckdb_path)?;
    if bootstrap_fixture {
        bootstrap_duckdb_replay_fixture(&conn, table)?;
    }
    let rows = read_duckdb_replay_rows(&conn, table)?;
    if rows.is_empty() {
        return Err("DuckDB replay table has no rows".into());
    }
    let thresholds = EvaluationThresholds {
        min_sample_count: 8,
        min_rank_ic: 0.03,
        min_net_sharpe: 0.5,
        max_drawdown: 0.20,
        max_correlation: 0.85,
    };
    let min_holdout_rows = usize::try_from(thresholds.min_sample_count)
        .map_err(|_| "evaluation threshold sample count does not fit usize")?;
    let (train_rows, validation_rows, test_rows) = chronological_holdout_split(&rows);
    if validation_rows.len() < min_holdout_rows || test_rows.len() < min_holdout_rows {
        return Err(
            "DuckDB replay table needs enough rows for chronological holdout validation and test windows"
                .into(),
        );
    }

    let now = chrono::Utc::now();
    let base_ast = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
    let engines = [
        ("mcts-duckdb-loop", SearchEngineKind::Mcts, 12, 0),
        (
            "rl-duckdb-loop",
            SearchEngineKind::ReinforcementLearning,
            24,
            0,
        ),
        ("llm-duckdb-loop", SearchEngineKind::LlmProposer, 0, 4_000),
    ];
    let dataset_manifest = reference("duckdb-agent-loop-dataset-1", "data_manifest")?;
    let mut candidates = Vec::new();
    let mut memory_events = Vec::new();
    let mut factor_store = factor_bank.map(FileFactorStore::new);
    let mut stored_factors = 0;

    for (run_id, engine, max_expansions, max_tokens) in engines {
        let report = run_budgeted_lab_search(
            SearchRunRequest {
                run_id: run_id.to_string(),
                engine,
                search_manifest_id: ManifestId::new("search-duckdb-agent-loop-1")?,
                budget: SearchBudget {
                    max_candidates: max_candidates_per_engine,
                    max_expansions,
                    max_tokens,
                    max_seconds: 10,
                },
            },
            base_ast.clone(),
            now,
        )?;

        for proposal in report.proposals {
            let validation_report = evaluate_candidate_on_rows(
                &proposal.ast,
                validation_rows,
                dataset_manifest.clone(),
                &thresholds,
            )?;
            let test_report = evaluate_candidate_on_rows(
                &proposal.ast,
                test_rows,
                dataset_manifest.clone(),
                &thresholds,
            )?;
            let evaluation = EvaluationDecision {
                passed: validation_report.decision.passed && test_report.decision.passed,
                failures: if validation_report.decision.passed {
                    test_report.decision.failures.clone()
                } else {
                    validation_report.decision.failures.clone()
                },
            };
            let failure_explanation = (!evaluation.passed).then(|| {
                format!(
                    "{} failed DuckDB chronological holdout evaluation: {:?}",
                    proposal.proposal_id, evaluation.failures
                )
            });
            if let Some(explanation) = &failure_explanation {
                let event = ResearchMemoryEvent {
                    event_id: format!("duckdb-agent-loop-memory-{}", memory_events.len() + 1),
                    source: MemorySource::Evaluation,
                    failure_kind: evaluation_failure_kind(&evaluation.failures),
                    related_manifest: Some(dataset_manifest.clone()),
                    explanation: explanation.clone(),
                    created_at: now,
                };
                event.validate()?;
                memory_events.push(event);
            }
            if evaluation.passed {
                if let Some(store) = factor_store.as_mut() {
                    store.upsert_factor(factor_asset_from_proposal(
                        &proposal,
                        test_report.metrics.clone(),
                        now,
                    )?)?;
                    stored_factors += 1;
                }
            }
            candidates.push(AgentLoopCandidateReport {
                proposal_id: proposal.proposal_id,
                engine: proposal.engine,
                formula: proposal.ast.to_string(),
                evaluation,
                validation_evaluation: Some(validation_report.decision),
                test_evaluation: Some(test_report.decision),
                failure_explanation,
            });
        }
    }

    let learning_directive =
        learning_directive_from_memory("duckdb-agent-loop-learning-1", &memory_events).unwrap_or(
            LearningDirective {
                directive_id: "duckdb-agent-loop-learning-1".to_string(),
                source_event_ids: vec![],
                actions: vec![],
                explanation: "no failures; keep current DuckDB search recipe".to_string(),
            },
        );
    learning_directive.validate()?;

    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let passed = candidates
        .iter()
        .filter(|candidate| candidate.evaluation.passed)
        .count();
    let report = AgentLoopDemoReport {
        output: output.display().to_string(),
        data_source: Some(format!("duckdb:{}", table)),
        data_rows: Some(rows.len()),
        chronological_holdout: Some(ChronologicalHoldoutReport {
            train_rows: train_rows.len(),
            validation_rows: validation_rows.len(),
            test_rows: test_rows.len(),
        }),
        factor_bank_path: factor_bank.map(|path| path.display().to_string()),
        stored_factors,
        max_live_risk_pct,
        proposals: candidates.len(),
        passed,
        failed: candidates.len() - passed,
        candidates,
        memory_events,
        learning_directive,
    };
    std::fs::write(output, serde_json::to_string_pretty(&report)?)?;
    register_artifact("duckdb-agent-loop-1", output)?;
    Ok(report)
}

fn agent_loop_demo(
    output: &Path,
    max_candidates_per_engine: usize,
    max_live_risk_pct: f64,
) -> Result<AgentLoopDemoReport, Box<dyn std::error::Error>> {
    if max_live_risk_pct < 0.0 || !max_live_risk_pct.is_finite() {
        return Err("max live risk pct must be finite and non-negative".into());
    }
    let now = chrono::Utc::now();
    let ast = FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string()));
    let engines = [
        ("mcts-agent-loop", SearchEngineKind::Mcts, 12, 0),
        (
            "rl-agent-loop",
            SearchEngineKind::ReinforcementLearning,
            24,
            0,
        ),
        ("llm-agent-loop", SearchEngineKind::LlmProposer, 0, 4_000),
    ];
    let thresholds = EvaluationThresholds {
        min_sample_count: 80,
        min_rank_ic: 0.03,
        min_net_sharpe: 1.0,
        max_drawdown: 0.08,
        max_correlation: 0.85,
    };
    let dataset_manifest = reference("agent-loop-dataset-1", "data_manifest")?;
    let mut candidates = Vec::new();
    let mut memory_events = Vec::new();

    for (run_id, engine, max_expansions, max_tokens) in engines {
        let report = run_budgeted_lab_search(
            SearchRunRequest {
                run_id: run_id.to_string(),
                engine,
                search_manifest_id: ManifestId::new("search-agent-loop-1")?,
                budget: SearchBudget {
                    max_candidates: max_candidates_per_engine,
                    max_expansions,
                    max_tokens,
                    max_seconds: 10,
                },
            },
            ast.clone(),
            now,
        )?;

        for proposal in report.proposals {
            let idx = candidates.len();
            let metrics = candidate_metrics(idx);
            let evaluation = evaluate_factor(
                &EvaluationInput {
                    dataset_manifest: dataset_manifest.clone(),
                    has_available_time: true,
                    sample_count: 64 + idx as u64 * 16,
                    metrics,
                },
                &thresholds,
                (idx % 5 == 4).then_some(0.92),
            );
            let failure_explanation = (!evaluation.passed).then(|| {
                format!(
                    "{} failed evaluation: {:?}",
                    proposal.proposal_id, evaluation.failures
                )
            });
            if let Some(explanation) = &failure_explanation {
                let event = ResearchMemoryEvent {
                    event_id: format!("agent-loop-memory-{}", memory_events.len() + 1),
                    source: MemorySource::Evaluation,
                    failure_kind: evaluation_failure_kind(&evaluation.failures),
                    related_manifest: Some(dataset_manifest.clone()),
                    explanation: explanation.clone(),
                    created_at: now,
                };
                event.validate()?;
                memory_events.push(event);
            }
            candidates.push(AgentLoopCandidateReport {
                proposal_id: proposal.proposal_id,
                engine: proposal.engine,
                formula: proposal.ast.to_string(),
                evaluation,
                validation_evaluation: None,
                test_evaluation: None,
                failure_explanation,
            });
        }
    }

    let learning_directive =
        learning_directive_from_memory("agent-loop-learning-1", &memory_events).unwrap_or(
            LearningDirective {
                directive_id: "agent-loop-learning-1".to_string(),
                source_event_ids: vec![],
                actions: vec![],
                explanation: "no failures; keep current search recipe".to_string(),
            },
        );
    learning_directive.validate()?;

    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let passed = candidates
        .iter()
        .filter(|candidate| candidate.evaluation.passed)
        .count();
    let report = AgentLoopDemoReport {
        output: output.display().to_string(),
        data_source: None,
        data_rows: None,
        chronological_holdout: None,
        factor_bank_path: None,
        stored_factors: 0,
        max_live_risk_pct,
        proposals: candidates.len(),
        passed,
        failed: candidates.len() - passed,
        candidates,
        memory_events,
        learning_directive,
    };
    std::fs::write(output, serde_json::to_string_pretty(&report)?)?;
    register_artifact("agent-loop-demo-1", output)?;
    Ok(report)
}

fn candidate_metrics(idx: usize) -> FactorMetrics {
    FactorMetrics {
        rank_ic: Some(0.015 + idx as f64 * 0.004),
        icir: Some(0.8 + idx as f64 * 0.1),
        net_sharpe: Some(0.7 + idx as f64 * 0.12),
        max_drawdown: Some(0.05 + (idx % 4) as f64 * 0.015),
        turnover: Some(1.0 + idx as f64 * 0.1),
        custom: BTreeMap::new(),
    }
}

fn evaluation_failure_kind(failures: &[EvaluationFailure]) -> FailureKind {
    if failures.contains(&EvaluationFailure::InsufficientSample) {
        FailureKind::InsufficientSample
    } else if failures.contains(&EvaluationFailure::CorrelationTooHigh) {
        FailureKind::HighCorrelation
    } else if failures.contains(&EvaluationFailure::MaxDrawdownAboveCeiling) {
        FailureKind::RiskCapExceeded
    } else {
        FailureKind::GateFailed
    }
}

fn chronological_holdout_split(
    rows: &[ReplayDataRow],
) -> (&[ReplayDataRow], &[ReplayDataRow], &[ReplayDataRow]) {
    let len = rows.len();
    let train_end = ((len * 60) / 100).max(1).min(len);
    let validation_end = ((len * 80) / 100).max(train_end + 1).min(len);
    (
        &rows[..train_end],
        &rows[train_end..validation_end],
        &rows[validation_end..],
    )
}

fn evaluate_candidate_on_rows(
    ast: &FactorAst,
    rows: &[ReplayDataRow],
    dataset_manifest: ManifestRef,
    thresholds: &EvaluationThresholds,
) -> Result<hft_factor_eval::ReplayEvaluationReport, Box<dyn std::error::Error>> {
    let signals = eval_factor_ast_series(ast, rows)?;
    let replay_csv = replay_rows_to_csv(rows, &signals);
    Ok(evaluate_replay_csv(
        &replay_csv,
        &ReplayCsvConfig {
            dataset_manifest,
            signal_column: "signal".to_string(),
            label_column: "forward_return".to_string(),
            available_time_column: Some("available_time".to_string()),
        },
        thresholds,
        None,
    )?)
}

fn factor_asset_from_proposal(
    proposal: &hft_search_protocol::ProposalArtifact,
    metrics: FactorMetrics,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<FactorAsset, Box<dyn std::error::Error>> {
    let asset = FactorAsset {
        factor_id: format!("duckdb-{}", proposal.proposal_id),
        factor_type: FactorType::Formula,
        ast: proposal.ast.clone(),
        lineage: FactorLineage {
            parent_factor_ids: proposal.parent_factor_ids.clone(),
            source_engine: format!("{:?}", proposal.engine),
            search_manifest_id: proposal.search_manifest_id.clone(),
        },
        data_manifest: reference("duckdb-agent-loop-dataset-1", "data_manifest")?,
        feature_manifest: reference("duckdb-agent-loop-feature-1", "feature_manifest")?,
        label_manifest: reference("duckdb-agent-loop-label-1", "label_manifest")?,
        evaluation_manifests: vec![reference(
            "duckdb-agent-loop-eval-1",
            "evaluation_manifest",
        )?],
        metrics,
        correlation_cluster: None,
        regime_metrics: BTreeMap::new(),
        symbol_metrics: BTreeMap::new(),
        promotion_status: FactorStatus::FullBacktestPassed,
        live_decay_state: None,
        created_at: now,
        updated_at: now,
    };
    asset.validate()?;
    Ok(asset)
}

fn is_simple_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn bootstrap_duckdb_replay_fixture(
    conn: &Connection,
    table: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (
                ts TIMESTAMP,
                symbol TEXT,
                oi_delta_5m DOUBLE,
                cvd_slope_5m DOUBLE,
                spread_bps DOUBLE,
                funding_rate DOUBLE,
                depth_imbalance DOUBLE,
                forward_return DOUBLE,
                available_time BOOLEAN
            )"
        ),
        [],
    )?;
    let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })?;
    if count > 0 {
        return Ok(());
    }
    for idx in 0..24 {
        let oi = idx as f64 * 0.08 - 0.6;
        let cvd = (idx as f64 % 5.0) * 0.04 - 0.08;
        let spread = 1.0 + (idx % 4) as f64 * 0.2;
        let funding = if idx % 2 == 0 { 0.01 } else { -0.005 };
        let depth = 0.2 + (idx % 6) as f64 * 0.05;
        let forward = oi * 0.012 + cvd * 0.04 - spread * 0.001;
        conn.execute(
            &format!("INSERT INTO {table} VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"),
            params![
                format!("2026-01-01 00:{idx:02}:00"),
                "BTCUSDT",
                oi,
                cvd,
                spread,
                funding,
                depth,
                forward,
                true
            ],
        )?;
    }
    Ok(())
}

fn read_duckdb_replay_rows(
    conn: &Connection,
    table: &str,
) -> Result<Vec<ReplayDataRow>, Box<dyn std::error::Error>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT oi_delta_5m, cvd_slope_5m, spread_bps, funding_rate, depth_imbalance, forward_return, available_time
         FROM {table}
         ORDER BY ts"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok(ReplayDataRow {
            oi_delta_5m: row.get(0)?,
            cvd_slope_5m: row.get(1)?,
            spread_bps: row.get(2)?,
            funding_rate: row.get(3)?,
            depth_imbalance: row.get(4)?,
            forward_return: row.get(5)?,
            available_time: row.get(6)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn eval_factor_ast_series(
    ast: &FactorAst,
    rows: &[ReplayDataRow],
) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
    match ast {
        FactorAst::Terminal(FactorTerminal::Field(name)) => rows
            .iter()
            .map(|row| field_value(row, name))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into),
        FactorAst::Terminal(FactorTerminal::Constant(value)) => {
            let value = value.parse::<f64>()?;
            Ok(vec![value; rows.len()])
        }
        FactorAst::Call { operator, args } => {
            let series = args
                .iter()
                .map(|arg| eval_factor_ast_series(arg, rows))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(match operator {
                hft_factor_dsl::FactorOperator::Add => zip2(&series[0], &series[1], |a, b| a + b),
                hft_factor_dsl::FactorOperator::Sub => zip2(&series[0], &series[1], |a, b| a - b),
                hft_factor_dsl::FactorOperator::Mul => zip2(&series[0], &series[1], |a, b| a * b),
                hft_factor_dsl::FactorOperator::Div => {
                    zip2(
                        &series[0],
                        &series[1],
                        |a, b| if b == 0.0 { 0.0 } else { a / b },
                    )
                }
                hft_factor_dsl::FactorOperator::Abs => {
                    series[0].iter().map(|value| value.abs()).collect()
                }
                hft_factor_dsl::FactorOperator::Rank => rank_series(&series[0]),
                hft_factor_dsl::FactorOperator::Delta => lagged_delta(
                    &series[0],
                    series[1].first().copied().unwrap_or(1.0) as usize,
                ),
                hft_factor_dsl::FactorOperator::Mean => rolling_mean(
                    &series[0],
                    series[1].first().copied().unwrap_or(1.0) as usize,
                ),
                hft_factor_dsl::FactorOperator::ZScore => zscore(
                    &series[0],
                    series[1].first().copied().unwrap_or(1.0) as usize,
                ),
                _ => return Err("unsupported factor operator for DuckDB agent loop".into()),
            })
        }
    }
}

fn field_value(row: &ReplayDataRow, name: &str) -> Result<f64, String> {
    match name {
        "oi_delta_5m" => Ok(row.oi_delta_5m),
        "cvd_slope_5m" => Ok(row.cvd_slope_5m),
        "spread_bps" => Ok(row.spread_bps),
        "funding_rate" => Ok(row.funding_rate),
        "depth_imbalance" => Ok(row.depth_imbalance),
        "forward_return" => Ok(row.forward_return),
        _ => Err(format!("unknown DuckDB replay field {name}")),
    }
}

fn zip2(left: &[f64], right: &[f64], op: impl Fn(f64, f64) -> f64) -> Vec<f64> {
    left.iter()
        .zip(right)
        .map(|(left, right)| op(*left, *right))
        .collect()
}

fn rank_series(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    values
        .iter()
        .map(|value| {
            sorted
                .iter()
                .position(|candidate| candidate == value)
                .unwrap_or(0) as f64
                / values.len().max(1) as f64
        })
        .collect()
}

fn lagged_delta(values: &[f64], window: usize) -> Vec<f64> {
    let window = window.max(1);
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            if idx < window {
                0.0
            } else {
                value - values[idx - window]
            }
        })
        .collect()
}

fn rolling_mean(values: &[f64], window: usize) -> Vec<f64> {
    let window = window.max(1);
    values
        .iter()
        .enumerate()
        .map(|(idx, _)| {
            let start = idx.saturating_sub(window - 1);
            let slice = &values[start..=idx];
            slice.iter().sum::<f64>() / slice.len() as f64
        })
        .collect()
}

fn zscore(values: &[f64], window: usize) -> Vec<f64> {
    let mean = rolling_mean(values, window);
    values
        .iter()
        .enumerate()
        .map(|(idx, value)| {
            let start = idx.saturating_sub(window.max(1) - 1);
            let slice = &values[start..=idx];
            let variance = slice
                .iter()
                .map(|sample| (sample - mean[idx]).powi(2))
                .sum::<f64>()
                / slice.len() as f64;
            let std = variance.sqrt();
            if std == 0.0 {
                0.0
            } else {
                (value - mean[idx]) / std
            }
        })
        .collect()
}

fn replay_rows_to_csv(rows: &[ReplayDataRow], signals: &[f64]) -> String {
    let mut csv = "signal,forward_return,available_time\n".to_string();
    for (row, signal) in rows.iter().zip(signals) {
        csv.push_str(&format!(
            "{},{},{}\n",
            signal, row.forward_return, row.available_time
        ));
    }
    csv
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
struct EndpointSmoke {
    name: &'static str,
    url: String,
    ok: bool,
    http_status: Option<u16>,
    chain_id: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ConnectivitySmokeReport {
    output: String,
    exchange: EndpointSmoke,
    on_chain: Option<EndpointSmoke>,
}

fn connectivity_smoke(
    output: &Path,
    exchange_ping_url: &str,
    evm_rpc_url: Option<&str>,
    timeout_ms: u64,
) -> Result<ConnectivitySmokeReport, Box<dyn std::error::Error>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .build()?;
    let exchange = exchange_ping(&client, exchange_ping_url);
    let on_chain = evm_rpc_url.map(|url| evm_chain_id(&client, url));

    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let report = ConnectivitySmokeReport {
        output: output.display().to_string(),
        exchange,
        on_chain,
    };
    std::fs::write(output, serde_json::to_string_pretty(&report)?)?;
    register_artifact("connectivity-smoke-1", output)?;
    Ok(report)
}

fn exchange_ping(client: &reqwest::blocking::Client, url: &str) -> EndpointSmoke {
    match client.get(url).send() {
        Ok(response) => EndpointSmoke {
            name: "exchange_rest_ping",
            url: url.to_string(),
            ok: response.status().is_success(),
            http_status: Some(response.status().as_u16()),
            chain_id: None,
            error: None,
        },
        Err(error) => EndpointSmoke {
            name: "exchange_rest_ping",
            url: url.to_string(),
            ok: false,
            http_status: None,
            chain_id: None,
            error: Some(error.to_string()),
        },
    }
}

fn evm_chain_id(client: &reqwest::blocking::Client, url: &str) -> EndpointSmoke {
    match evm_rpc_string(client, url, "eth_chainId", serde_json::json!([])) {
        Ok(chain_id) => EndpointSmoke {
            name: "evm_rpc_chain_id",
            url: url.to_string(),
            ok: true,
            http_status: Some(200),
            chain_id: Some(chain_id),
            error: None,
        },
        Err(error) => EndpointSmoke {
            name: "evm_rpc_chain_id",
            url: url.to_string(),
            ok: false,
            http_status: None,
            chain_id: None,
            error: Some(error.to_string()),
        },
    }
}

fn evm_rpc_string(
    client: &reqwest::blocking::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<String, Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params
    });

    let response = client.post(url).json(&body).send()?;
    let status = response.status();
    let value = response.json::<serde_json::Value>()?;
    if !status.is_success() {
        return Err(format!("EVM RPC HTTP status {}: {}", status.as_u16(), value).into());
    }
    if let Some(error) = value.get("error") {
        return Err(format!("EVM RPC error: {}", error).into());
    }
    value
        .get("result")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| "EVM RPC response missing string result".into())
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
        Command::AgentLoopDemo {
            output,
            max_candidates_per_engine,
            max_live_risk_pct,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&agent_loop_demo(
                    &output,
                    max_candidates_per_engine,
                    max_live_risk_pct,
                )?)?
            );
        }
        Command::DuckdbAgentLoop {
            duckdb_path,
            output,
            table,
            max_candidates_per_engine,
            max_live_risk_pct,
            bootstrap_fixture,
            factor_bank,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&duckdb_agent_loop(
                    &duckdb_path,
                    &output,
                    &table,
                    max_candidates_per_engine,
                    max_live_risk_pct,
                    bootstrap_fixture,
                    factor_bank.as_deref(),
                )?)?
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
        Command::ConnectivitySmoke {
            output,
            exchange_ping_url,
            evm_rpc_url,
            timeout_ms,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&connectivity_smoke(
                    &output,
                    &exchange_ping_url,
                    evm_rpc_url.as_deref(),
                    timeout_ms,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn research_binary_has_no_live_actuation_surface() {
        let source = include_str!("main.rs")
            .split("\n#[cfg(test)]")
            .next()
            .unwrap_or_default();
        let forbidden = [
            ["Binance", "Order"].concat(),
            ["Evm", "RawTx"].concat(),
            ["Execution", "Client"].concat(),
            ["Order", "Intent"].concat(),
        ];
        for forbidden in forbidden {
            assert!(
                !source.contains(&forbidden),
                "forbidden research authority: {forbidden}"
            );
        }
    }

    #[test]
    fn chronological_holdout_split_is_non_overlapping() {
        let rows = replay_fixture_rows(10);
        let (train, validation, test) = chronological_holdout_split(&rows);
        assert_eq!((train.len(), validation.len(), test.len()), (6, 2, 2));
        assert!(std::ptr::eq(train.last().unwrap(), &rows[5]));
        assert!(std::ptr::eq(validation.first().unwrap(), &rows[6]));
        assert!(std::ptr::eq(test.first().unwrap(), &rows[8]));
    }

    #[test]
    fn duckdb_agent_loop_rejects_too_short_replay_data() {
        let root = temp_test_dir("chronological-holdout-short");
        let duckdb_path = root.join("replay.duckdb");
        let output = root.join("report.json");
        let conn = Connection::open(&duckdb_path).unwrap();
        bootstrap_duckdb_replay_fixture_table(&conn, "factor_replay", &replay_fixture_rows(4))
            .unwrap();

        let error = duckdb_agent_loop(&duckdb_path, &output, "factor_replay", 1, 0.0, false, None)
            .unwrap_err()
            .to_string();

        assert_eq!(
            error,
            "DuckDB replay table needs enough rows for chronological holdout validation and test windows"
        );
    }

    #[test]
    fn file_factor_store_round_trips_passed_factor_asset() {
        let root = temp_test_dir("chronological-holdout-factor-store");
        let path = root.join("factor-bank.json");
        let mut store = FileFactorStore::new(&path);
        let asset = replay_factor_asset("chronological-holdout-factor").unwrap();

        store.upsert_factor(asset.clone()).unwrap();

        assert_eq!(store.get_factor(&asset.factor_id).unwrap(), asset);
    }

    fn replay_fixture_rows(len: usize) -> Vec<ReplayDataRow> {
        (0..len)
            .map(|idx| ReplayDataRow {
                oi_delta_5m: idx as f64 * 0.08 - 0.6,
                cvd_slope_5m: (idx as f64 % 5.0) * 0.04 - 0.08,
                spread_bps: 1.0 + (idx % 4) as f64 * 0.2,
                funding_rate: if idx % 2 == 0 { 0.01 } else { -0.005 },
                depth_imbalance: 0.2 + (idx % 6) as f64 * 0.05,
                forward_return: (idx as f64 * 0.08 - 0.6) * 0.012
                    + ((idx as f64 % 5.0) * 0.04 - 0.08) * 0.04
                    - (1.0 + (idx % 4) as f64 * 0.2) * 0.001,
                available_time: true,
            })
            .collect()
    }

    fn bootstrap_duckdb_replay_fixture_table(
        conn: &Connection,
        table: &str,
        rows: &[ReplayDataRow],
    ) -> Result<(), Box<dyn std::error::Error>> {
        conn.execute(
            &format!(
                "CREATE TABLE {table} (
                    ts TIMESTAMP,
                    symbol TEXT,
                    oi_delta_5m DOUBLE,
                    cvd_slope_5m DOUBLE,
                    spread_bps DOUBLE,
                    funding_rate DOUBLE,
                    depth_imbalance DOUBLE,
                    forward_return DOUBLE,
                    available_time BOOLEAN
                )"
            ),
            [],
        )?;
        for (idx, row) in rows.iter().enumerate() {
            conn.execute(
                &format!("INSERT INTO {table} VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"),
                params![
                    format!("2026-01-01 00:{idx:02}:00"),
                    "BTCUSDT",
                    row.oi_delta_5m,
                    row.cvd_slope_5m,
                    row.spread_bps,
                    row.funding_rate,
                    row.depth_imbalance,
                    row.forward_return,
                    row.available_time
                ],
            )?;
        }
        Ok(())
    }

    fn replay_factor_asset(factor_id: &str) -> Result<FactorAsset, Box<dyn std::error::Error>> {
        let now = chrono::Utc::now();
        let asset = FactorAsset {
            factor_id: factor_id.to_string(),
            factor_type: FactorType::Formula,
            ast: FactorAst::Terminal(FactorTerminal::Field("oi_delta_5m".to_string())),
            lineage: FactorLineage {
                parent_factor_ids: vec![],
                source_engine: "test".to_string(),
                search_manifest_id: ManifestId::new("search-chronological-holdout-1")?,
            },
            data_manifest: reference("data-chronological-holdout-1", "data_manifest")?,
            feature_manifest: reference("feature-chronological-holdout-1", "feature_manifest")?,
            label_manifest: reference("label-chronological-holdout-1", "label_manifest")?,
            evaluation_manifests: vec![reference(
                "eval-chronological-holdout-1",
                "evaluation_manifest",
            )?],
            metrics: factor_metrics(),
            correlation_cluster: None,
            regime_metrics: BTreeMap::new(),
            symbol_metrics: BTreeMap::new(),
            promotion_status: FactorStatus::FullBacktestPassed,
            live_decay_state: None,
            created_at: now,
            updated_at: now,
        };
        asset.validate()?;
        Ok(asset)
    }

    fn temp_test_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
