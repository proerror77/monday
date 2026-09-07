use crate::{
    cli::{print_json, EngineChoice, LearnMissionArgs, MissionStatusArgs, RunMissionArgs},
    data_mission,
};
use alpha_domain::{
    canonical_json_hash, CexBaselineArtifactV1, CexBaselineGateV1, CexBaselineModelKindV1,
    CexBaselinePolicyV1, CexFactorBankRevisionV2, CexGpPolicyV1, CexResearchContentRefV1,
    CexResearchMissionArtifactV1, MissionStatus, ResearchMission,
};
use alpha_engine::{
    baselines::verify_cex_baseline_artifact,
    engines::{GeneticProgrammingEngine, OfflineRlEngine, OfflineTrace},
    evaluation::{prepare_dataset, EngineContext},
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
const CEX_FACTOR_BANK_REGISTRY_KIND: &str = "cex_factor_bank";
const CEX_BASELINE_POLICY_REGISTRY_KIND: &str = "cex_baseline_policy";
const CEX_BASELINE_RIDGE_REGISTRY_KIND: &str = "cex_baseline_ridge";
const CEX_BASELINE_CART_REGISTRY_KIND: &str = "cex_baseline_cart";
const CEX_BASELINE_GATE_REGISTRY_KIND: &str = "cex_baseline_gate";
const CEX_FACTOR_BANK_MCTS_OPERATOR_ERROR: &str =
    "CEX Factor-Bank subset MCTS is available only through the Campaign execution seam";

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
    pub research_dataset: CexResearchContentRefV1,
    pub walk_forward_partition: CexResearchContentRefV1,
}

pub fn run_mission(args: RunMissionArgs, resume: bool) -> anyhow::Result<()> {
    print_json(&execute_mission(&args, resume)?)
}

pub fn execute_mission(args: &RunMissionArgs, resume: bool) -> anyhow::Result<MissionRunReport> {
    execute_mission_inner(args, resume, None)
}

pub(crate) fn execute_governed_gp_mission(
    args: &RunMissionArgs,
    resume: bool,
    policy: &CexGpPolicyV1,
    candidate_namespace: &str,
) -> anyhow::Result<MissionRunReport> {
    execute_mission_inner(args, resume, Some((policy, candidate_namespace)))
}

fn execute_mission_inner(
    args: &RunMissionArgs,
    resume: bool,
    governed_gp: Option<(&CexGpPolicyV1, &str)>,
) -> anyhow::Result<MissionRunReport> {
    validate_mission_args(args, governed_gp.is_some())?;
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
    let labels = manifest.evaluation_label_spec()?;
    let protocol = args.dataset.validation.evaluation_protocol(&labels)?;
    let rows = manifest.load_rows(&protocol.costs)?;
    let evaluation_protocol_hash = protocol.content_hash()?;
    let dataset = prepare_dataset(rows, &protocol)?;
    let research_context = dataset.engine_context();
    let research_dataset_sha256 = canonical_json_hash(&research_context.rows())?;
    let research_dataset = CexResearchContentRefV1 {
        id: format!("cex-research-dataset-{research_dataset_sha256}"),
        content_sha256: research_dataset_sha256,
    };
    let walk_forward_partition_sha256 = canonical_json_hash(&serde_json::json!({
        "research_dataset": &research_dataset,
        "folds": research_context.folds(),
    }))?;
    let walk_forward_partition = CexResearchContentRefV1 {
        id: format!("cex-walk-forward-partition-{walk_forward_partition_sha256}"),
        content_sha256: walk_forward_partition_sha256,
    };
    if matches!(args.engine, EngineChoice::Mcts) {
        validate_mcts_baseline_gate(
            &store,
            &mission,
            Some(&research_context),
            &research_dataset,
            &walk_forward_partition,
            &evaluation_protocol_hash,
        )?;
        bail!(CEX_FACTOR_BANK_MCTS_OPERATOR_ERROR);
    }
    let evaluator = if let Some((gp_policy, _)) = governed_gp {
        FormulaEvaluator::for_governed_mission(&mission, gp_policy)
    } else {
        FormulaEvaluator::for_mission(&mission)
    }
    .map_err(anyhow::Error::msg)?;
    let proposal_engine = build_engine(args, &dataset, &mission, governed_gp)?;
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
        research_dataset,
        walk_forward_partition,
    })
}

fn validate_mcts_baseline_gate(
    store: &AlphaStore,
    mission: &ResearchMission,
    research_context: Option<&EngineContext<'_>>,
    research_dataset: &CexResearchContentRefV1,
    walk_forward_partition: &CexResearchContentRefV1,
    evaluation_protocol_hash: &str,
) -> anyhow::Result<()> {
    let gate_id = mission
        .baseline_artifact_id
        .as_deref()
        .context("MCTS requires a baseline gate identity")?;
    let gate_revision = store
        .get_registry_revision(gate_id)
        .with_context(|| format!("MCTS baseline gate registry revision {gate_id} is missing"))?;
    if gate_revision.registry_kind != CEX_BASELINE_GATE_REGISTRY_KIND
        || gate_revision.revision_id != gate_id
    {
        bail!("MCTS baseline gate registry kind or identity is invalid");
    }
    let gate: CexBaselineGateV1 = serde_json::from_value(gate_revision.payload.clone())
        .context("MCTS baseline gate payload is invalid")?;
    gate.validate().map_err(anyhow::Error::msg)?;

    let factor_bank_revision = store
        .get_registry_revision(&gate.factor_bank_revision_id)
        .with_context(|| {
            format!(
                "MCTS Factor Bank registry revision {} is missing",
                gate.factor_bank_revision_id
            )
        })?;
    if factor_bank_revision.registry_kind != CEX_FACTOR_BANK_REGISTRY_KIND
        || factor_bank_revision.revision_id != gate.factor_bank_revision_id
        || factor_bank_revision.parent_revision_id.as_deref()
            != Some(mission.dataset_manifest_id.as_str())
    {
        bail!("MCTS Factor Bank registry binding drifted");
    }
    if gate_revision.parent_revision_id.as_deref() != Some(gate.factor_bank_revision_id.as_str()) {
        bail!("MCTS baseline gate parent revision drifted");
    }
    let factor_bank: CexFactorBankRevisionV2 =
        serde_json::from_value(factor_bank_revision.payload.clone())
            .context("MCTS Factor Bank payload is invalid")?;
    factor_bank.validate().map_err(anyhow::Error::msg)?;
    if factor_bank.research_dataset != *research_dataset
        || factor_bank.walk_forward_partition != *walk_forward_partition
    {
        bail!("MCTS Factor Bank research identity drifted");
    }
    let control_revision = store
        .get_registry_revision(&gate.mission_id)
        .with_context(|| {
            format!(
                "MCTS source research mission registry revision {} is missing",
                gate.mission_id
            )
        })?;
    if control_revision.registry_kind != "cex_research_mission"
        || control_revision.revision_id != gate.mission_id
        || control_revision.parent_revision_id.as_deref()
            != Some(mission.dataset_manifest_id.as_str())
    {
        bail!("MCTS source research mission registry binding drifted");
    }
    let control_mission: CexResearchMissionArtifactV1 =
        serde_json::from_value(control_revision.payload.clone())
            .context("MCTS source research mission payload is invalid")?;
    control_mission.validate().map_err(anyhow::Error::msg)?;
    if control_mission.semantic_id().map_err(anyhow::Error::msg)? != gate.mission_id {
        bail!("MCTS source research mission identity drifted");
    }
    let policy_revision = store
        .get_registry_revision(&gate.policy_hash)
        .with_context(|| {
            format!(
                "MCTS baseline policy registry revision {} is missing",
                gate.policy_hash
            )
        })?;
    if policy_revision.registry_kind != CEX_BASELINE_POLICY_REGISTRY_KIND
        || policy_revision.revision_id != gate.policy_hash
        || policy_revision.parent_revision_id.as_deref() != Some(gate.mission_id.as_str())
    {
        bail!("MCTS baseline policy registry binding drifted");
    }
    let baseline_policy: CexBaselinePolicyV1 =
        serde_json::from_value(policy_revision.payload.clone())
            .context("MCTS baseline policy payload is invalid")?;
    if baseline_policy.content_hash().map_err(anyhow::Error::msg)? != gate.policy_hash {
        bail!("MCTS baseline policy identity drifted");
    }
    baseline_policy.validate_binding(&control_mission.spec.policies.baseline)?;

    let ridge = match gate.ridge_artifact_id.as_deref() {
        Some(ridge_id) => Some(read_mcts_baseline_artifact(
            store,
            ridge_id,
            CEX_BASELINE_RIDGE_REGISTRY_KIND,
            &factor_bank,
            &control_mission,
            &baseline_policy,
            research_context.context("MCTS baseline artifact verification context is missing")?,
            research_dataset,
            walk_forward_partition,
            evaluation_protocol_hash,
            CexBaselineModelKindV1::Ridge,
        )?),
        None => None,
    };
    let cart = match gate.cart_artifact_id.as_deref() {
        Some(cart_id) => Some(read_mcts_baseline_artifact(
            store,
            cart_id,
            CEX_BASELINE_CART_REGISTRY_KIND,
            &factor_bank,
            &control_mission,
            &baseline_policy,
            research_context.context("MCTS baseline artifact verification context is missing")?,
            research_dataset,
            walk_forward_partition,
            evaluation_protocol_hash,
            CexBaselineModelKindV1::ShallowCart,
        )?),
        None => None,
    };
    gate.validate_binding(
        &control_mission,
        &baseline_policy,
        &factor_bank,
        ridge.as_ref(),
        cart.as_ref(),
    )
    .map_err(anyhow::Error::msg)?;
    if !gate.passed {
        bail!("MCTS baseline gate did not pass");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn read_mcts_baseline_artifact(
    store: &AlphaStore,
    artifact_id: &str,
    registry_kind: &str,
    factor_bank: &CexFactorBankRevisionV2,
    control_mission: &CexResearchMissionArtifactV1,
    baseline_policy: &CexBaselinePolicyV1,
    research_context: &EngineContext<'_>,
    research_dataset: &CexResearchContentRefV1,
    walk_forward_partition: &CexResearchContentRefV1,
    evaluation_protocol_hash: &str,
    model_kind: CexBaselineModelKindV1,
) -> anyhow::Result<CexBaselineArtifactV1> {
    let revision = store.get_registry_revision(artifact_id).with_context(|| {
        format!("MCTS baseline artifact registry revision {artifact_id} is missing")
    })?;
    if revision.registry_kind != registry_kind
        || revision.revision_id != artifact_id
        || revision.parent_revision_id.as_deref() != Some(factor_bank.revision_id.as_str())
    {
        bail!("MCTS baseline artifact registry binding drifted");
    }
    let artifact: CexBaselineArtifactV1 = serde_json::from_value(revision.payload.clone())
        .context("MCTS baseline artifact payload is invalid")?;
    artifact.validate().map_err(anyhow::Error::msg)?;
    artifact
        .validate_binding(control_mission, baseline_policy, factor_bank)
        .map_err(anyhow::Error::msg)?;
    if artifact.model_kind != model_kind
        || artifact.mission_id != control_mission.semantic_id().map_err(anyhow::Error::msg)?
        || artifact.factor_bank_revision_id != factor_bank.revision_id
        || artifact.research_dataset != *research_dataset
        || artifact.walk_forward_partition != *walk_forward_partition
        || artifact.evaluation_policy.content_sha256 != evaluation_protocol_hash
        || artifact.evaluation.protocol_binding()?.1 != evaluation_protocol_hash
    {
        bail!("MCTS baseline artifact identity drifted");
    }
    verify_cex_baseline_artifact(research_context, factor_bank, &artifact)
        .map_err(anyhow::Error::msg)?;
    Ok(artifact)
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
    mission: &ResearchMission,
    governed_gp: Option<(&CexGpPolicyV1, &str)>,
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
    if governed_gp.is_none() {
        validate_live_feature_fields(&fields)?;
    }
    let primary = fields[0].clone();
    let engine: Box<dyn ProposalEngine> = match args.engine {
        EngineChoice::Gp => match governed_gp {
            Some((policy, candidate_namespace)) => {
                if fields != policy.admitted_fields
                    || args.seed != policy.seed
                    || mission.search_budget != policy.budget
                {
                    bail!("mission GP execution drifted from its frozen policy");
                }
                Box::new(
                    GeneticProgrammingEngine::new_governed(
                        policy.clone(),
                        candidate_namespace.to_string(),
                    )
                    .map_err(anyhow::Error::msg)?,
                )
            }
            None => Box::new(
                GeneticProgrammingEngine::new(args.seed, fields.clone(), 32, 5)
                    .map_err(anyhow::Error::msg)?,
            ),
        },
        EngineChoice::Mcts => bail!(CEX_FACTOR_BANK_MCTS_OPERATOR_ERROR),
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
    validate_mission_args(args, false)
}

fn validate_mission_args(args: &RunMissionArgs, governed_research: bool) -> anyhow::Result<()> {
    validate_live_formula_engine(args.engine)?;
    if args.feature_fields.is_empty()
        || args
            .feature_fields
            .iter()
            .any(|field| field.trim().is_empty())
    {
        bail!("mission feature fields are required");
    }
    if governed_research {
        Ok(())
    } else {
        validate_live_feature_fields(&args.feature_fields)
    }
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
    use alpha_domain::{MissionCompletionPolicy, SearchBudget, ValidatorMode};
    use chrono::Utc;
    use hft_research_manifest::ManifestId;
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
    fn live_feature_fields_reject_campaign_only_trade_flow() {
        let error = validate_live_feature_fields(&[
            alpha_domain::CEX_RESEARCH_AGGREGATE_TRADE_FLOW_IMBALANCE_FIELD.to_string(),
        ])
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("aggregate_trade_flow_imbalance is not live executable"));
    }

    #[test]
    fn mcts_shared_fan_in_requires_an_immutable_baseline_gate() {
        let store = AlphaStore::open_in_memory().unwrap();
        let mission = ResearchMission {
            mission_id: "consumer-mission".to_string(),
            objective: "test objective".to_string(),
            hypothesis_scope: "test hypothesis".to_string(),
            mutable_scope: vec!["factor_ast".to_string()],
            dataset_manifest_id: ManifestId::new("dataset-1").unwrap(),
            baseline_artifact_id: None,
            validation_mode: ValidatorMode::MissionValidator,
            validator_spec: serde_json::json!({}),
            search_budget: SearchBudget {
                max_candidates: 1,
                max_expansions: 1,
                max_tokens: 0,
                max_seconds: 1,
            },
            completion_policy: MissionCompletionPolicy::default(),
            prompt_snapshot_id: None,
            search_policy_snapshot_id: "policy-1".to_string(),
            status: MissionStatus::Pending,
            terminal_reason: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let reference = |id: &str| CexResearchContentRefV1 {
            id: id.to_string(),
            content_sha256: "a".repeat(64),
        };

        let error = validate_mcts_baseline_gate(
            &store,
            &mission,
            None,
            &reference("cex-research-dataset-1"),
            &reference("cex-walk-forward-partition-1"),
            &"b".repeat(64),
        )
        .unwrap_err();

        assert!(error.to_string().contains("baseline gate identity"));
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
