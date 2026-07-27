use crate::{
    cli::{print_json, EngineChoice, LearnMissionArgs, MissionStatusArgs, RunMissionArgs},
    data_mission,
};
use alpha_domain::MissionStatus;
use alpha_engine::{
    engines::{GeneticProgrammingEngine, MctsEngine, OfflineRlEngine, OfflineTrace},
    evaluation::prepare_dataset,
    formula_evaluator::FormulaEvaluator,
    learning::{close_learning_loop, FailureCritic, LearningConfig},
    llm::{LlmConfig, LlmProposalEngine, OpenAiCompatibleClient},
    AutoResearchKernel, ProposalEngine, RunControl,
};
use alpha_store::{AlphaStore, StoreError};
use anyhow::{bail, Context};
use hft_factor_dsl::{validate_live_formula, FactorAst, FactorTerminal, LiveEventDomain};

pub(crate) const BAYESIAN_WINDOW_SEARCH_LIVE_CAPABILITY_ERROR: &str =
    "Bayesian window search is research-only and cannot produce live-executable formulas";
pub(crate) const OFFLINE_RL_LIVE_CAPABILITY_ERROR: &str =
    "Offline RL search is research-only and cannot produce live-executable formulas";

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
    validate_live_mission_args(args)?;
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
        args.dataset.validation.fee_bps,
        args.dataset.validation.funding_bps,
        args.dataset.validation.latency_bps,
    )?;
    let labels = manifest.evaluation_label_spec()?;
    let protocol = args.dataset.validation.evaluation_protocol(&labels)?;
    let dataset = prepare_dataset(rows, &protocol)?;
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
    validate_live_mission_args(args)?;
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
    validate_live_feature_fields(&fields)?;
    let primary = fields[0].clone();
    let secondary = fields.get(1).cloned().unwrap_or_else(|| primary.clone());
    let engine: Box<dyn ProposalEngine> = match args.engine {
        EngineChoice::Gp => Box::new(
            GeneticProgrammingEngine::new(args.seed, fields.clone(), 32, 5)
                .map_err(anyhow::Error::msg)?,
        ),
        EngineChoice::Mcts => Box::new(
            MctsEngine::new_live(args.seed, primary.clone(), secondary, 1.414, 5)
                .map_err(anyhow::Error::msg)?,
        ),
        EngineChoice::Bayesian => bail!(BAYESIAN_WINDOW_SEARCH_LIVE_CAPABILITY_ERROR),
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

pub(crate) fn validate_live_formula_engine(engine: EngineChoice) -> anyhow::Result<()> {
    match engine {
        EngineChoice::Bayesian => bail!(BAYESIAN_WINDOW_SEARCH_LIVE_CAPABILITY_ERROR),
        EngineChoice::OfflineRl => bail!(OFFLINE_RL_LIVE_CAPABILITY_ERROR),
        _ => Ok(()),
    }
}

pub(crate) fn validate_live_mission_args(args: &RunMissionArgs) -> anyhow::Result<()> {
    validate_live_formula_engine(args.engine)?;
    if args.feature_fields.is_empty()
        || args
            .feature_fields
            .iter()
            .any(|field| field.trim().is_empty())
    {
        bail!("mission feature fields are required");
    }
    validate_live_feature_fields(&args.feature_fields)
}

pub(crate) fn validate_live_feature_fields(fields: &[String]) -> anyhow::Result<()> {
    let mut event_domain = None;
    for field in fields {
        let field = field.trim();
        let capability = validate_live_formula(&FactorAst::Terminal(FactorTerminal::Field(
            field.to_string(),
        )))
        .map_err(|error| {
            anyhow::anyhow!("feature field {field} is not live executable: {error}")
        })?;
        if let Some(existing) = event_domain {
            if existing != capability.event_domain {
                bail!(
                    "feature fields span live event domains: {} and {}",
                    live_event_domain_name(existing),
                    live_event_domain_name(capability.event_domain)
                );
            }
        } else {
            event_domain = Some(capability.event_domain);
        }
    }
    Ok(())
}

fn live_event_domain_name(domain: LiveEventDomain) -> &'static str {
    match domain {
        LiveEventDomain::Snapshot => "snapshot",
        LiveEventDomain::Bar => "bar",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{DatasetArgs, ValidationArgs};
    use std::path::PathBuf;

    #[test]
    fn live_feature_fields_reject_mixed_event_domains() {
        let error =
            validate_live_feature_fields(&["best_bid".to_string(), "bar_return".to_string()])
                .unwrap_err();

        assert_eq!(
            error.to_string(),
            "feature fields span live event domains: snapshot and bar"
        );
    }

    #[test]
    fn mission_preflight_rejects_live_capability_before_opening_store() {
        let cases = [
            (
                EngineChoice::Bayesian,
                vec!["book_imbalance".to_string()],
                "Bayesian window search is research-only",
            ),
            (
                EngineChoice::OfflineRl,
                vec!["book_imbalance".to_string()],
                "Offline RL search is research-only",
            ),
            (
                EngineChoice::Mcts,
                vec!["signal".to_string()],
                "feature field signal is not live executable",
            ),
            (
                EngineChoice::Mcts,
                vec!["best_bid".to_string(), "bar_return".to_string()],
                "feature fields span live event domains",
            ),
        ];

        for (engine, feature_fields, expected) in cases {
            for resume in [false, true] {
                let db = temporary_db_path("mission-live-capability");
                let args = RunMissionArgs {
                    db: db.clone(),
                    mission_id: "mission-1".to_string(),
                    engine,
                    seed: 7,
                    feature_fields: feature_fields.clone(),
                    offline_trace: None,
                    max_new_iterations: None,
                    dataset: DatasetArgs {
                        dataset_manifest: db.with_extension("dataset.json"),
                        validation: ValidationArgs {
                            initial_train_rows: 1,
                            validation_rows: 30,
                            fold_count: 2,
                            purge_rows: 1,
                            embargo_rows: 0,
                            sealed_holdout_rows: 30,
                            fee_bps: 1.0,
                            rebate_bps: 0.0,
                            funding_bps: 0.0,
                            latency_bps: 0.5,
                            slippage_bps: 0.0,
                            cross_spread: false,
                            position_notional_usd: 0.0,
                            capacity_depth_levels: 0,
                            max_book_depth_fraction: 0.0,
                            label_horizon_buckets: 1,
                            observation_frequency_millis: 60_000,
                        },
                    },
                };

                let error = execute_mission(&args, resume).unwrap_err().to_string();
                assert!(error.contains(expected), "unexpected error: {error}");
                assert!(
                    !db.exists(),
                    "mission preflight must happen before the durable store is opened"
                );
            }
        }
    }

    fn temporary_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "alpha-{name}-{}-{}.duckdb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock")
                .as_nanos()
        ))
    }
}
