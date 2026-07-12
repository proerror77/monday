use crate::{
    cli::{print_json, EngineChoice, LearnMissionArgs, MissionStatusArgs, RunMissionArgs},
    data_mission,
};
use alpha_domain::MissionStatus;
use alpha_engine::{
    engines::{
        BayesianOptimizerEngine, GeneticProgrammingEngine, MctsEngine, OfflineRlEngine,
        OfflineTrace,
    },
    evaluation::{prepare_dataset, WalkForwardConfig},
    formula_evaluator::FormulaEvaluator,
    learning::{close_learning_loop, FailureCritic, LearningConfig},
    llm::{LlmConfig, LlmProposalEngine, OpenAiCompatibleClient},
    AutoResearchKernel, ProposalEngine, RunControl,
};
use alpha_store::{AlphaStore, StoreError};
use anyhow::{bail, Context};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchEngineAuthority {
    CandidateResearchOnly,
    LabSearchPolicyOnly,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MissionRunReport {
    pub mission_id: String,
    pub status: MissionStatus,
    pub terminal_reason: Option<alpha_domain::MissionTerminalReason>,
    pub total_iterations: usize,
    pub new_iterations: usize,
    pub engine: EngineChoice,
    pub engine_authority: ResearchEngineAuthority,
    pub dataset_manifest_id: String,
}

pub fn run_mission(args: RunMissionArgs, resume: bool) -> anyhow::Result<()> {
    print_json(&execute_mission(&args, resume)?)
}

pub fn execute_mission(args: &RunMissionArgs, resume: bool) -> anyhow::Result<MissionRunReport> {
    let mut store = AlphaStore::open(&args.db)?;
    let mission = store.get_mission(&args.mission_id)?;
    match (resume, &mission.status) {
        (false, MissionStatus::Pending | MissionStatus::Running) => {}
        (true, MissionStatus::Paused | MissionStatus::Running) => {}
        (false, _) => bail!("mission run requires a pending or running mission"),
        (true, _) => bail!("mission resume requires a paused or running mission"),
    }

    let manifest =
        data_mission::read_registered_research_dataset(&store, &args.dataset.dataset_manifest)?;
    if mission.dataset_manifest_id.as_str() != manifest.manifest_id() {
        bail!("mission dataset id does not match the supplied manifest");
    }
    let rows = manifest.load_rows(
        args.dataset.fee_bps,
        args.dataset.funding_bps,
        args.dataset.latency_bps,
    )?;
    let dataset = prepare_dataset(
        rows,
        &WalkForwardConfig {
            initial_train_rows: args.dataset.initial_train_rows,
            validation_rows: args.dataset.validation_rows,
            fold_count: args.dataset.fold_count,
            purge_rows: args.dataset.purge_rows,
            embargo_rows: args.dataset.embargo_rows,
            sealed_holdout_rows: args.dataset.sealed_holdout_rows,
        },
        format!("sealed:{}", manifest.manifest_id()),
    )?;
    let proposal_engine = build_engine(args, &dataset)?;
    let evaluator = FormulaEvaluator::for_mission(&mission).map_err(anyhow::Error::msg)?;
    let mut kernel = AutoResearchKernel::new(&mut store, proposal_engine, evaluator);
    let outcome = kernel.run(
        &args.mission_id,
        &dataset,
        RunControl {
            max_new_iterations: args.max_new_iterations,
        },
    )?;
    Ok(MissionRunReport {
        mission_id: args.mission_id.clone(),
        status: outcome.status,
        terminal_reason: outcome.terminal_reason,
        total_iterations: outcome.total_iterations,
        new_iterations: outcome.new_iterations,
        engine: args.engine,
        engine_authority: match args.engine {
            EngineChoice::OfflineRl => ResearchEngineAuthority::LabSearchPolicyOnly,
            _ => ResearchEngineAuthority::CandidateResearchOnly,
        },
        dataset_manifest_id: manifest.manifest_id().to_string(),
    })
}

pub fn mission_status(args: MissionStatusArgs) -> anyhow::Result<()> {
    let store = AlphaStore::open(&args.db)?;
    let lineage = store.mission_lineage(&args.mission_id)?;
    let checkpoint = match store.get_checkpoint(&args.mission_id) {
        Ok(checkpoint) => Some(checkpoint),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    print_json(&serde_json::json!({
        "mission": lineage.mission,
        "iteration_count": lineage.iterations.len(),
        "candidate_count": lineage.candidates.len(),
        "evaluation_count": lineage.evaluations.len(),
        "checkpoint": checkpoint,
    }))
}

pub fn learn_mission(args: LearnMissionArgs) -> anyhow::Result<()> {
    print_json(&execute_learning(&args)?)
}

pub fn execute_learning(
    args: &LearnMissionArgs,
) -> anyhow::Result<alpha_engine::learning::LearningOutcome> {
    let mut store = AlphaStore::open(&args.db)?;
    let critic = if args.llm_critic {
        Some(
            OpenAiCompatibleClient::new(LlmConfig::from_env().map_err(anyhow::Error::msg)?)
                .map_err(anyhow::Error::msg)?,
        )
    } else {
        None
    };
    let critic = critic.as_ref().map(|critic| critic as &dyn FailureCritic);
    Ok(close_learning_loop(
        &mut store,
        &args.mission_id,
        &LearningConfig {
            repeated_failure_threshold: args.repeated_failure_threshold,
            max_critic_tokens: args.max_critic_tokens,
        },
        critic,
    )?)
}

fn build_engine(
    args: &RunMissionArgs,
    dataset: &alpha_engine::evaluation::PreparedDataset,
) -> anyhow::Result<Box<dyn ProposalEngine>> {
    let fields = args
        .feature_fields
        .iter()
        .map(|field| field.trim().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    if fields.is_empty()
        || fields.contains("")
        || fields
            .iter()
            .any(|field| !dataset.feature_names().contains(field))
    {
        bail!(
            "feature fields must be non-empty and registered by the prepared dataset: {:?}",
            dataset.feature_names()
        );
    }
    let fields = fields.into_iter().collect::<Vec<_>>();
    let primary = fields[0].clone();
    let secondary = fields.get(1).cloned().unwrap_or_else(|| primary.clone());
    let engine: Box<dyn ProposalEngine> = match args.engine {
        EngineChoice::Gp => Box::new(
            GeneticProgrammingEngine::new(args.seed, fields.clone(), 32, 5)
                .map_err(anyhow::Error::msg)?,
        ),
        EngineChoice::Mcts => Box::new(
            MctsEngine::new(args.seed, primary.clone(), secondary, 1.414, 5)
                .map_err(anyhow::Error::msg)?,
        ),
        EngineChoice::Bayesian => Box::new(
            BayesianOptimizerEngine::new(primary.clone(), 2.0, 60.0, 30, 1e-6, 10.0, 0.01)
                .map_err(anyhow::Error::msg)?,
        ),
        EngineChoice::OfflineRl => {
            let path = args
                .offline_trace
                .as_ref()
                .context("--offline-trace is required for offline-rl")?;
            let traces: Vec<OfflineTrace> =
                serde_json::from_slice(&std::fs::read(path).with_context(|| {
                    format!("failed to read offline trace {}", path.display())
                })?)?;
            Box::new(
                OfflineRlEngine::train(primary, "offline-policy-v1", &traces, 3, 0.2, 0.9, 50)
                    .map_err(anyhow::Error::msg)?,
            )
        }
        EngineChoice::Llm => {
            let client =
                OpenAiCompatibleClient::new(LlmConfig::from_env().map_err(anyhow::Error::msg)?)
                    .map_err(anyhow::Error::msg)?;
            Box::new(LlmProposalEngine::new(client, fields).map_err(anyhow::Error::msg)?)
        }
    };
    Ok(engine)
}
