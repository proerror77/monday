use crate::{
    cli::{
        print_json, DatasetArgs, EngineChoice, ExecuteMissionArgs, RunMissionArgs, ValidationArgs,
    },
    data_mission, mission, prediction_dispatch,
};
use alpha_domain::{
    canonical_json_hash, CandidateArtifact, CandidateEvaluation, CexBaselinePolicyV1,
    CexEqualAbsoluteWeightPolicyV1, CexEventReplayPolicyV1, CexFactorBankRevisionV2,
    CexFactorEvaluationEvidenceV2, CexFactorRejectionCodeV1, CexFactorScreeningAttemptV2,
    CexFactorScreeningVerdictV1, CexGpPolicyV1, CexResearchContentRefV1, CexResearchHoldoutStateV1,
    CexResearchMissionArtifactV1, EvaluationCostsV1, FormulaEvaluatorConfig, IterationVerdict,
    MissionCompletionPolicy, MissionStatus, ResearchMission, ValidatorMode,
    MAX_CEX_FACTOR_BANK_MCTS_CHECKPOINT_BYTES,
};
use alpha_engine::{
    baselines::evaluate_cex_baselines,
    engines::{
        CexCombinationResearchArtifactV1, CexFactorBankMcts, CexFactorBankMctsCheckpointV1,
        CexFactorBankMctsStopReasonV1,
    },
    evaluation::{prepare_dataset, EngineContext},
};
use alpha_store::{AlphaStore, MissionLineage, RegistryRevision, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;
use hft_backtest::{
    config::{
        verify_canonical_replay_artifact, CanonicalReplayEvidence, CanonicalSourceSegmentEvidence,
    },
    engine::{
        replay_target_positions, TargetPositionDecision, TargetPositionReplayConfig,
        TargetPositionReplayMetrics, TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION,
    },
};
use hft_research_manifest::{
    CexReplayDatasetManifestV2, CexReplaySnapshotV1, CexReplaySnapshotV2, ManifestId,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3,
};
use reqwest::{
    blocking::Client,
    header::{HeaderValue, CONTENT_TYPE},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

const MATERIALIZATION_KIND: &str = "lob_point_in_time_materialization";
const CEX_BASELINE_POLICY_REGISTRY_KIND: &str = "cex_baseline_policy";
const CEX_BASELINE_RIDGE_REGISTRY_KIND: &str = "cex_baseline_ridge";
const CEX_BASELINE_CART_REGISTRY_KIND: &str = "cex_baseline_cart";
const CEX_BASELINE_GATE_REGISTRY_KIND: &str = "cex_baseline_gate";
const MAX_MISSION_BYTES: u64 = 4 * 1024 * 1024;
// ponytail: one Mission is capped at 1 GiB; raise this only when staged partitions exceed it.
const MAX_FEATURE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MATERIALIZATION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REPLAY_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_REPLAY_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MCTS_CHECKPOINT_ARTIFACT_SCHEMA_VERSION: &str =
    "cex-factor-bank-subset-mcts-checkpoint-artifact-v1";
const CEX_EVENT_REPLAY_RECEIPT_SCHEMA_VERSION: &str = "cex-event-replay-receipt-v1";
// ponytail: fixed batching bounds checkpoint I/O; make it configurable only if recovery data requires it.
const MCTS_CHECKPOINT_INTERVAL: u64 = 256;

#[derive(Debug, Deserialize)]
struct Materialization {
    dataset_kind: String,
    schema_version: String,
    mission_id: String,
    symbol: String,
    market: String,
    source_revision: String,
    source_segments: Vec<SourceSegment>,
    artifact_sha256: String,
    bucket_ms: u64,
    label_horizon_buckets: usize,
    top_depth: usize,
    first_event_time: chrono::DateTime<Utc>,
    last_event_time: chrono::DateTime<Utc>,
    snapshot: CexReplaySnapshotV2,
    snapshot_sha256: String,
}

#[derive(Debug, Deserialize)]
struct SourceSegment {
    path: PathBuf,
    sha256: String,
    collector_manifest_sha256: String,
    success_marker_sha256: String,
    start_received_at_ns: u64,
    end_received_at_ns: u64,
    events: u64,
}

#[derive(Debug, Serialize)]
struct ExecutionReport<'a> {
    mission_id: &'a str,
    engine: &'static str,
    bundle_bytes: u64,
    bundle_sha256: String,
    readback_bundle_sha256: String,
    replay_receipt_id: Option<String>,
    replay_gate_passed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CexReplayCapabilitiesV1 {
    clock_semantics: String,
    modalities: Vec<String>,
    min_bid_depth_levels: usize,
    max_bid_depth_levels: usize,
    min_ask_depth_levels: usize,
    max_ask_depth_levels: usize,
    trade_tape_available: bool,
    queue_position: bool,
    partial_fills: bool,
    market_impact: bool,
    true_capacity: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CexReplayGateV1 {
    passed: bool,
    failures: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CexEventReplayReceiptV1 {
    schema_version: String,
    receipt_id: String,
    mission_id: String,
    strategy: CexResearchContentRefV1,
    dataset: CexResearchContentRefV1,
    materialization: CexResearchContentRefV1,
    tape_artifact: CexResearchContentRefV1,
    tape_manifest: CexResearchContentRefV1,
    source: CexResearchContentRefV1,
    replay_policy: CexResearchContentRefV1,
    replay_config_sha256: String,
    replay_config: TargetPositionReplayConfig,
    decision_sha256: String,
    decision_scope: String,
    implementation_source_revision: String,
    image_identity: String,
    replay_engine_version: String,
    metrics: TargetPositionReplayMetrics,
    capabilities: CexReplayCapabilitiesV1,
    gate: CexReplayGateV1,
    holdout_id: String,
    holdout_state: CexResearchHoldoutStateV1,
    deployment_authority: bool,
    order_submission_authority: bool,
}

impl CexEventReplayReceiptV1 {
    fn finalize(mut self) -> anyhow::Result<Self> {
        self.receipt_id = self.expected_receipt_id()?;
        self.validate()?;
        Ok(self)
    }

    fn validate(&self) -> anyhow::Result<()> {
        for reference in [
            &self.strategy,
            &self.dataset,
            &self.materialization,
            &self.tape_artifact,
            &self.tape_manifest,
            &self.source,
            &self.replay_policy,
        ] {
            reference.validate()?;
        }
        if self.schema_version != CEX_EVENT_REPLAY_RECEIPT_SCHEMA_VERSION
            || self.receipt_id != self.expected_receipt_id()?
            || self.mission_id.trim().is_empty()
            || self.replay_config_sha256 != canonical_json_hash(&self.replay_config)?
            || self.decision_scope != "pre_holdout_research_rows"
            || !valid_git_revision(&self.implementation_source_revision)
            || immutable_sha256_identity("CEX replay image", &self.image_identity).is_err()
            || self.replay_engine_version != TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION
            || self.metrics.decision_count == 0
            || self.capabilities.clock_semantics != "recorded_userspace_receive_time_us"
            || self.capabilities.modalities != ["lob"]
            || self.capabilities.min_bid_depth_levels != self.metrics.min_bid_depth_levels
            || self.capabilities.max_bid_depth_levels != self.metrics.max_bid_depth_levels
            || self.capabilities.min_ask_depth_levels != self.metrics.min_ask_depth_levels
            || self.capabilities.max_ask_depth_levels != self.metrics.max_ask_depth_levels
            || self.capabilities.trade_tape_available
            || self.metrics.trade_events != 0
            || self.replay_config.trade_tape_declared
            || self.capabilities.queue_position
            || self.capabilities.partial_fills
            || self.capabilities.market_impact
            || self.capabilities.true_capacity
            || self.gate.passed != self.gate.failures.is_empty()
            || self.holdout_id.trim().is_empty()
            || self.holdout_state != CexResearchHoldoutStateV1::Unopened
            || self.deployment_authority
            || self.order_submission_authority
        {
            bail!("CEX event replay receipt is invalid");
        }
        normalized_sha256("CEX replay decisions", &self.decision_sha256)?;
        Ok(())
    }

    fn expected_receipt_id(&self) -> anyhow::Result<String> {
        let mut semantic = self.clone();
        semantic.receipt_id.clear();
        Ok(format!(
            "cex-event-replay-receipt-{}",
            canonical_json_hash(&semantic)?
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MctsCheckpointArtifactV1 {
    schema_version: String,
    checkpoint_sha256: String,
    checkpoint: CexFactorBankMctsCheckpointV1,
}

impl MctsCheckpointArtifactV1 {
    fn new(checkpoint: CexFactorBankMctsCheckpointV1) -> anyhow::Result<Self> {
        let checkpoint_sha256 = checkpoint.content_hash().map_err(anyhow::Error::msg)?;
        Ok(Self {
            schema_version: MCTS_CHECKPOINT_ARTIFACT_SCHEMA_VERSION.to_string(),
            checkpoint_sha256,
            checkpoint,
        })
    }

    fn verified_checkpoint(
        self,
        expected_sha256: &str,
    ) -> anyhow::Result<CexFactorBankMctsCheckpointV1> {
        if self.schema_version != MCTS_CHECKPOINT_ARTIFACT_SCHEMA_VERSION
            || self.checkpoint_sha256
                != self.checkpoint.content_hash().map_err(anyhow::Error::msg)?
            || self.checkpoint_sha256
                != normalized_sha256("MCTS resume checkpoint", expected_sha256)?
        {
            bail!("MCTS resume checkpoint content SHA256 mismatch");
        }
        Ok(self.checkpoint)
    }
}

#[derive(Debug, Serialize)]
struct ExecutionModelEvidence {
    schema_version: &'static str,
    fee_bps: f64,
    rebate_bps: f64,
    funding_bps: f64,
    latency_bps: f64,
    additional_slippage_bps: f64,
    cross_spread: bool,
    turnover_definition: &'static str,
    queue_position_modeled: bool,
    partial_fills_modeled: bool,
    market_impact_modeled: bool,
    capacity_modeled: bool,
    capacity_gate_enabled: bool,
    capacity_gate_model: &'static str,
    position_notional_usd: f64,
    capacity_depth_levels: usize,
    max_book_depth_fraction: f64,
}

impl From<&EvaluationCostsV1> for ExecutionModelEvidence {
    fn from(costs: &EvaluationCostsV1) -> Self {
        let capacity_gate_enabled = costs.capacity_enabled();
        Self {
            schema_version: "execution_cost_model_v2",
            fee_bps: costs.fee_bps,
            rebate_bps: costs.rebate_bps,
            funding_bps: costs.funding_bps,
            latency_bps: costs.latency_bps,
            additional_slippage_bps: costs.slippage_bps,
            cross_spread: costs.cross_spread,
            turnover_definition: "absolute_position_change; a full side flip has turnover 2",
            queue_position_modeled: false,
            partial_fills_modeled: false,
            market_impact_modeled: false,
            capacity_modeled: false,
            capacity_gate_enabled,
            capacity_gate_model: if capacity_gate_enabled {
                "same_side_top_n_depth_fraction"
            } else {
                "disabled"
            },
            position_notional_usd: costs.position_notional_usd,
            capacity_depth_levels: costs.capacity_depth_levels,
            max_book_depth_fraction: costs.max_book_depth_fraction,
        }
    }
}

pub fn execute(args: ExecuteMissionArgs) -> anyhow::Result<()> {
    validate_args(&args)?;
    let input_dir = args.work_dir.join("input");
    let artifact_dir = args.work_dir.join("artifacts");
    let results_dir = args.work_dir.join("results");
    let bundle = args.work_dir.join("results.zip");
    for path in [&input_dir, &artifact_dir, &results_dir, &bundle] {
        if path.try_exists()? {
            bail!(
                "Mission execution requires a fresh work directory; existing path: {}",
                path.display()
            );
        }
    }
    std::fs::create_dir_all(&input_dir)?;
    std::fs::create_dir_all(&artifact_dir)?;
    std::fs::create_dir_all(&results_dir)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mission_path = input_dir.join("mission.json");
    let feature_path = input_dir.join("features.jsonl");
    let materialization_path = input_dir.join("materialization.json");
    let replay_artifact_sha256 =
        normalized_sha256("CEX replay artifact", &args.replay_artifact_sha256)?;
    let replay_artifact_path = input_dir.join(format!("{replay_artifact_sha256}.parquet"));
    let replay_manifest_path = input_dir.join("replay-manifest.json");
    let resume_checkpoint = if let Some((resume_url, resume_sha256)) = resume_source(&args)? {
        let checkpoint_path = input_dir.join("factor-subset-mcts-resume.json");
        fetch_to_file(
            &client,
            resume_url,
            &checkpoint_path,
            MAX_CEX_FACTOR_BANK_MCTS_CHECKPOINT_BYTES,
        )?;
        let artifact: MctsCheckpointArtifactV1 =
            serde_json::from_slice(&std::fs::read(checkpoint_path)?)
                .context("MCTS resume checkpoint is invalid JSON")?;
        Some(serde_json::to_value(
            artifact.verified_checkpoint(resume_sha256)?,
        )?)
    } else {
        None
    };
    let (_, mission_sha256) =
        fetch_to_file(&client, &args.mission_url, &mission_path, MAX_MISSION_BYTES)?;
    if mission_sha256 != normalized_sha256("Mission", &args.mission_sha256)? {
        bail!("Mission artifact SHA256 mismatch");
    }
    let control_mission: CexResearchMissionArtifactV1 =
        serde_json::from_slice(&std::fs::read(&mission_path)?)
            .context("CEX Research Mission artifact is invalid JSON or schema")?;
    control_mission.validate()?;
    let baseline_target = control_mission
        .spec
        .hypotheses
        .first()
        .context("CEX Research Mission has no baseline target")?
        .target
        .clone();
    if control_mission
        .spec
        .hypotheses
        .iter()
        .any(|hypothesis| hypothesis.target != baseline_target)
    {
        bail!("CEX Research Mission hypotheses do not share one frozen baseline target");
    }
    mission::validate_live_feature_fields(&control_mission.spec.feature_fields)?;
    let gp_policy = CexGpPolicyV1::controlled_v1(
        control_mission.spec.policies.gp.id.clone(),
        control_mission.spec.feature_fields.clone(),
        control_mission.spec.search.seed,
        &control_mission.spec.search.budget,
    )?;
    gp_policy.validate_binding(&control_mission.spec.policies.gp)?;
    data_mission::write_json_atomic(&results_dir.join("gp-policy.json"), &gp_policy)?;
    let baseline_policy =
        CexBaselinePolicyV1::controlled_v1(control_mission.spec.policies.baseline.id.clone())?;
    baseline_policy.validate_binding(&control_mission.spec.policies.baseline)?;
    data_mission::write_json_atomic(&results_dir.join("baseline-policy.json"), &baseline_policy)?;
    let weight_policy = CexEqualAbsoluteWeightPolicyV1::controlled_v1(
        control_mission.spec.policies.weight.id.clone(),
    )?;
    weight_policy.validate_binding(&control_mission.spec.policies.weight)?;
    data_mission::write_json_atomic(&results_dir.join("weight-policy.json"), &weight_policy)?;
    let mission_id = control_mission.semantic_id()?;
    let validation = ValidationArgs::from_protocol(&control_mission.spec.evaluation_protocol);
    let engine = EngineChoice::Gp;

    let (_, feature_sha256) =
        fetch_to_file(&client, &args.feature_url, &feature_path, MAX_FEATURE_BYTES)?;
    let (_, materialization_sha256) = fetch_to_file(
        &client,
        &args.materialization_url,
        &materialization_path,
        MAX_MATERIALIZATION_BYTES,
    )?;
    let materialization = decode_materialization(&std::fs::read(&materialization_path)?)?;
    validate_materialization(&materialization, &feature_sha256, &validation)?;
    validate_mission_materialization_binding(
        &control_mission,
        &materialization,
        &materialization_sha256,
        &feature_sha256,
    )?;
    let replay_policy = CexEventReplayPolicyV1::controlled_v1(
        control_mission.spec.policies.replay.id.clone(),
        materialization.top_depth,
        control_mission
            .spec
            .instrument
            .horizon
            .observation_frequency_millis,
    )?;
    replay_policy.validate_binding(&control_mission.spec.policies.replay)?;
    data_mission::write_json_atomic(&results_dir.join("replay-policy.json"), &replay_policy)?;
    let (_, fetched_replay_artifact_sha256) = fetch_to_file(
        &client,
        &args.replay_artifact_url,
        &replay_artifact_path,
        MAX_REPLAY_ARTIFACT_BYTES,
    )?;
    if fetched_replay_artifact_sha256 != replay_artifact_sha256 {
        bail!("CEX replay artifact SHA256 mismatch");
    }
    let (_, fetched_replay_manifest_sha256) = fetch_to_file(
        &client,
        &args.replay_manifest_url,
        &replay_manifest_path,
        MAX_REPLAY_MANIFEST_BYTES,
    )?;
    if fetched_replay_manifest_sha256
        != normalized_sha256("CEX replay manifest", &args.replay_manifest_sha256)?
    {
        bail!("CEX replay manifest SHA256 mismatch");
    }
    let evaluation_protocol = control_mission.spec.evaluation_protocol.clone();

    let db = results_dir.join("alpha.duckdb");
    let feature_manifest_path = results_dir.join("feature-manifest.json");
    let dataset_manifest_path = results_dir.join("cex-replay-dataset-manifest.json");
    let mut store = AlphaStore::open(&db)?;
    let feature_manifest = data_mission::import_and_register_features(
        &mut store,
        &control_mission.spec.data_mission_id,
        &feature_path,
        &artifact_dir,
    )?;
    let source_key = format!("binance-{}-lob", materialization.market);
    if feature_manifest.symbol != materialization.symbol
        || feature_manifest.source_revisions.get(&source_key)
            != Some(&materialization.source_revision)
        || feature_manifest.artifact_sha256 != feature_sha256
        || feature_manifest.label_spec.horizon_buckets != materialization.label_horizon_buckets
        || feature_manifest.label_spec.observation_frequency_millis != materialization.bucket_ms
    {
        bail!("registered feature lineage or label facts do not match the materialization");
    }
    let dataset_manifest = data_mission::admit_cex_replay_dataset(
        &mut store,
        &feature_manifest,
        &materialization.snapshot,
    )?;
    validate_mission_dataset_binding(&control_mission, &feature_manifest, &dataset_manifest)?;
    data_mission::write_json_atomic(&feature_manifest_path, &feature_manifest)?;
    data_mission::write_json_atomic(&dataset_manifest_path, &dataset_manifest)?;
    data_mission::write_json_atomic(
        &results_dir.join("data-import.json"),
        &serde_json::json!({
            "manifest": &dataset_manifest,
            "manifest_path": &dataset_manifest_path,
            "feature_manifest": &feature_manifest,
            "feature_manifest_path": &feature_manifest_path,
        }),
    )?;
    std::fs::copy(
        &materialization_path,
        results_dir.join("materialization.json"),
    )?;
    data_mission::write_json_atomic(
        &results_dir.join("execution-model.json"),
        &ExecutionModelEvidence::from(&evaluation_protocol.costs),
    )?;

    let now = Utc::now();
    let semantic_mission = serde_json::json!({
        "schema_version": &control_mission.schema_version,
        "spec": &control_mission.spec,
    });
    store.put_registry_revision(&RegistryRevision {
        revision_id: mission_id.clone(),
        registry_kind: "cex_research_mission".to_string(),
        asset_id: control_mission.spec.instrument.symbol.clone(),
        parent_revision_id: Some(dataset_manifest.manifest_id.clone()),
        payload: semantic_mission,
        created_at: now,
    })?;
    let baseline_policy_revision_id = baseline_policy.content_hash()?;
    store.put_registry_revision(&RegistryRevision {
        revision_id: baseline_policy_revision_id,
        registry_kind: CEX_BASELINE_POLICY_REGISTRY_KIND.to_string(),
        asset_id: control_mission.spec.instrument.symbol.clone(),
        parent_revision_id: Some(mission_id.clone()),
        payload: serde_json::to_value(&baseline_policy)?,
        created_at: now,
    })?;
    data_mission::write_json_atomic(
        &results_dir.join("control-plane-mission.json"),
        &control_mission,
    )?;
    data_mission::write_json_atomic(
        &results_dir.join("mission-admission.json"),
        &serde_json::json!({
            "schema_version": &control_mission.schema_version,
            "mission_id": &mission_id,
            "mission_artifact_sha256": &mission_sha256,
            "dataset_manifest_id": &dataset_manifest.manifest_id,
        }),
    )?;
    let research_mission = ResearchMission {
        mission_id: mission_id.clone(),
        objective: control_mission.spec.objective.clone(),
        hypothesis_scope: control_mission
            .spec
            .hypotheses
            .iter()
            .map(|hypothesis| hypothesis.hypothesis_id.as_str())
            .collect::<Vec<_>>()
            .join(","),
        mutable_scope: vec!["factor_ast".to_string()],
        dataset_manifest_id: ManifestId::new(dataset_manifest.manifest_id.clone())?,
        baseline_artifact_id: None,
        validation_mode: ValidatorMode::MissionValidator,
        validator_spec: serde_json::json!({
            "multiple_testing_trials": control_mission
                .spec
                .search
                .planned_gp_and_subset_trials()?
        }),
        search_budget: control_mission.spec.search.budget.clone(),
        completion_policy: MissionCompletionPolicy {
            min_kept_candidates: control_mission.spec.search.budget.max_candidates,
        },
        prompt_snapshot_id: None,
        search_policy_snapshot_id: control_mission.spec.policies.gp.id.clone(),
        status: MissionStatus::Pending,
        terminal_reason: None,
        created_at: now,
        updated_at: now,
    };
    if canonical_json_hash(&FormulaEvaluatorConfig::for_mission(&research_mission)?)?
        != control_mission.spec.policies.screening.content_sha256
    {
        bail!("Mission screening policy does not match the evaluator configuration");
    }
    data_mission::write_json_atomic(&results_dir.join("mission.json"), &research_mission)?;
    store.create_mission(&research_mission)?;
    data_mission::write_json_atomic(&results_dir.join("mission-create.json"), &research_mission)?;
    drop(store);

    let dataset = DatasetArgs {
        dataset_manifest: dataset_manifest_path.clone(),
        validation,
    };
    let run_args = RunMissionArgs {
        db: db.clone(),
        mission_id: mission_id.clone(),
        engine,
        seed: control_mission.spec.search.seed,
        feature_fields: control_mission.spec.feature_fields.clone(),
        offline_trace: None,
        max_new_iterations: Some(control_mission.spec.search.max_new_iterations),
        dataset: dataset.clone(),
    };
    let mut run_report = mission::execute_governed_gp_mission(
        &run_args,
        false,
        &gp_policy,
        &control_mission.spec.search_lineage_id,
    )?;
    while run_report.status == MissionStatus::Paused {
        let previous_iterations = run_report.total_iterations;
        run_report = mission::execute_governed_gp_mission(
            &run_args,
            true,
            &gp_policy,
            &control_mission.spec.search_lineage_id,
        )?;
        if run_report.total_iterations <= previous_iterations
            && run_report.status == MissionStatus::Paused
        {
            bail!("resumed mission made no progress");
        }
    }
    data_mission::write_json_atomic(&results_dir.join("mission-run.json"), &run_report)?;

    let mut store = AlphaStore::open(&db)?;
    let lineage = store.mission_lineage(&mission_id)?;
    let factor_bank = build_factor_bank(&control_mission, &gp_policy, &run_report, &lineage)?;
    let factor_bank_payload = serde_json::to_value(&factor_bank)?;
    store.put_registry_revision(&RegistryRevision {
        revision_id: factor_bank.revision_id.clone(),
        registry_kind: "cex_factor_bank".to_string(),
        asset_id: control_mission.spec.instrument.symbol.clone(),
        parent_revision_id: Some(run_report.dataset_manifest_id.clone()),
        payload: factor_bank_payload,
        created_at: Utc::now(),
    })?;
    data_mission::write_json_atomic(&results_dir.join("factor-bank.json"), &factor_bank)?;
    let baseline_dataset_manifest =
        data_mission::read_registered_research_dataset(&store, &dataset_manifest_path)?;
    let baseline_rows = baseline_dataset_manifest.load_rows(&evaluation_protocol.costs)?;
    let baseline_dataset = prepare_dataset(baseline_rows, &evaluation_protocol)?;
    let baseline_context = baseline_dataset.engine_context();
    let baseline_run = evaluate_cex_baselines(
        &baseline_context,
        &factor_bank,
        &baseline_policy,
        &mission_id,
        baseline_target,
        &control_mission.spec.policies.evaluation,
    )
    .map_err(anyhow::Error::msg)?;
    persist_baseline_evidence(
        &mut store,
        &results_dir,
        &control_mission,
        &baseline_policy,
        &factor_bank,
        &baseline_run,
    )?;
    let strategy = if baseline_run.gate.passed {
        run_factor_bank_subset_search(
            &results_dir,
            &control_mission,
            &factor_bank,
            &baseline_run,
            &baseline_context,
            resume_checkpoint,
        )?
    } else if resume_checkpoint.is_some() {
        bail!("MCTS resume checkpoint requires a passing baseline gate");
    } else {
        None
    };
    let replay_report = strategy
        .as_ref()
        .map(|strategy| {
            run_cex_event_replay(
                &results_dir,
                &control_mission,
                &materialization,
                &materialization_sha256,
                &factor_bank,
                &baseline_context,
                strategy,
                &replay_policy,
                &replay_artifact_path,
                &replay_artifact_sha256,
                &replay_manifest_path,
                &args.replay_manifest_sha256,
                &args.implementation_source_revision,
                &args.image_identity,
            )
        })
        .transpose()?;
    let checkpoint = match store.get_checkpoint(&mission_id) {
        Ok(checkpoint) => Some(checkpoint),
        Err(StoreError::NotFound) => None,
        Err(error) => return Err(error.into()),
    };
    data_mission::write_json_atomic(
        &results_dir.join("mission-status.json"),
        &serde_json::json!({
            "mission": lineage.mission,
            "iteration_count": lineage.iterations.len(),
            "candidate_count": lineage.candidates.len(),
            "evaluation_count": lineage.evaluations.len(),
            "checkpoint": checkpoint,
        }),
    )?;
    data_mission::write_json_atomic(
        &results_dir.join("candidates.json"),
        &serde_json::json!({
            "mission_id": &mission_id,
            "candidates": lineage.candidates,
            "evaluations": lineage.evaluations,
        }),
    )?;
    drop(store);
    create_bundle(&args.work_dir, &bundle, [&results_dir, &artifact_dir])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    let readback_bundle = input_dir.join("published-result-readback.zip");
    let (_, readback_sha256) = fetch_to_file(
        &client,
        &args.result_readback_url,
        &readback_bundle,
        bundle_bytes,
    )?;
    if readback_sha256 != bundle_sha256 {
        bail!("published CEX result readback SHA256 mismatch");
    }
    print_json(&ExecutionReport {
        mission_id: &mission_id,
        engine: "gp_then_factor_bank_subset_mcts",
        bundle_bytes,
        bundle_sha256,
        readback_bundle_sha256: readback_sha256,
        replay_receipt_id: replay_report
            .as_ref()
            .map(|receipt| receipt.receipt_id.clone()),
        replay_gate_passed: replay_report.as_ref().map(|receipt| receipt.gate.passed),
    })
}

fn decode_materialization(bytes: &[u8]) -> anyhow::Result<Materialization> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("materialization manifest is invalid JSON")?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
    {
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3) => {
            serde_json::from_value(value).context("V3 materialization manifest is invalid")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2) => {
            let snapshot: CexReplaySnapshotV1 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V2 materialization has no snapshot")?,
            )
            .context("historical V2 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V2 materialization is read-only and cannot execute")
        }
        _ => bail!("materialization kind or schema is unsupported"),
    }
}

fn persist_baseline_evidence(
    store: &mut AlphaStore,
    results_dir: &Path,
    control_mission: &CexResearchMissionArtifactV1,
    baseline_policy: &CexBaselinePolicyV1,
    factor_bank: &CexFactorBankRevisionV2,
    run: &alpha_engine::baselines::CexBaselineRun,
) -> anyhow::Result<()> {
    let alpha_engine::baselines::CexBaselineRun { ridge, cart, gate } = run;
    let asset_id = control_mission.spec.instrument.symbol.clone();
    if factor_bank.entries.is_empty() {
        if ridge.is_some() || cart.is_some() {
            bail!("empty Factor Bank baseline evaluation returned model artifacts");
        }
        gate.validate_binding(control_mission, baseline_policy, factor_bank, None, None)?;
        store.put_registry_revision(&RegistryRevision {
            revision_id: gate.gate_id.clone(),
            registry_kind: CEX_BASELINE_GATE_REGISTRY_KIND.to_string(),
            asset_id,
            parent_revision_id: Some(factor_bank.revision_id.clone()),
            payload: serde_json::to_value(gate)?,
            created_at: Utc::now(),
        })?;
        data_mission::write_json_atomic(&results_dir.join("baseline-gate.json"), &gate)?;
        return Ok(());
    }

    let ridge = ridge
        .as_ref()
        .context("non-empty Factor Bank baseline is missing Ridge artifact")?;
    let cart = cart
        .as_ref()
        .context("non-empty Factor Bank baseline is missing CART artifact")?;
    ridge.validate_binding(control_mission, baseline_policy, factor_bank)?;
    cart.validate_binding(control_mission, baseline_policy, factor_bank)?;
    gate.validate_binding(
        control_mission,
        baseline_policy,
        factor_bank,
        Some(ridge),
        Some(cart),
    )?;
    for (artifact_id, registry_kind, payload) in [
        (
            ridge.artifact_id.clone(),
            CEX_BASELINE_RIDGE_REGISTRY_KIND,
            serde_json::to_value(ridge)?,
        ),
        (
            cart.artifact_id.clone(),
            CEX_BASELINE_CART_REGISTRY_KIND,
            serde_json::to_value(cart)?,
        ),
        (
            gate.gate_id.clone(),
            CEX_BASELINE_GATE_REGISTRY_KIND,
            serde_json::to_value(gate)?,
        ),
    ] {
        store.put_registry_revision(&RegistryRevision {
            revision_id: artifact_id,
            registry_kind: registry_kind.to_string(),
            asset_id: asset_id.clone(),
            parent_revision_id: Some(factor_bank.revision_id.clone()),
            payload,
            created_at: Utc::now(),
        })?;
    }
    data_mission::write_json_atomic(&results_dir.join("ridge-baseline.json"), ridge)?;
    data_mission::write_json_atomic(&results_dir.join("cart-baseline.json"), cart)?;
    data_mission::write_json_atomic(&results_dir.join("baseline-gate.json"), gate)?;
    Ok(())
}

fn run_factor_bank_subset_search(
    results_dir: &Path,
    control_mission: &CexResearchMissionArtifactV1,
    factor_bank: &CexFactorBankRevisionV2,
    baselines: &alpha_engine::baselines::CexBaselineRun,
    context: &EngineContext<'_>,
    resume_checkpoint: Option<serde_json::Value>,
) -> anyhow::Result<Option<CexCombinationResearchArtifactV1>> {
    let ridge = baselines
        .ridge
        .as_ref()
        .context("passing baseline gate is missing Ridge evidence")?;
    let cart = baselines
        .cart
        .as_ref()
        .context("passing baseline gate is missing CART evidence")?;
    let mut search = match resume_checkpoint {
        Some(checkpoint) => CexFactorBankMcts::restore_json(
            control_mission,
            factor_bank,
            ridge,
            cart,
            &baselines.gate,
            context,
            checkpoint,
        ),
        None => CexFactorBankMcts::new(
            control_mission,
            factor_bank,
            ridge,
            cart,
            &baselines.gate,
            context,
        ),
    }
    .map_err(anyhow::Error::msg)?;
    loop {
        let stop = search
            .run(context, Some(MCTS_CHECKPOINT_INTERVAL))
            .map_err(anyhow::Error::msg)?;
        let checkpoint =
            MctsCheckpointArtifactV1::new(search.checkpoint().map_err(anyhow::Error::msg)?)?;
        data_mission::write_json_atomic_bounded(
            &results_dir.join("factor-subset-mcts-checkpoint.json"),
            &checkpoint,
            MAX_CEX_FACTOR_BANK_MCTS_CHECKPOINT_BYTES,
        )?;
        if stop == CexFactorBankMctsStopReasonV1::Paused {
            continue;
        }
        data_mission::write_json_atomic(
            &results_dir.join("factor-subset-mcts-trace.json"),
            &search.trace().map_err(anyhow::Error::msg)?,
        )?;
        let result = search.result().map_err(anyhow::Error::msg)?;
        data_mission::write_json_atomic(
            &results_dir.join("factor-subset-mcts-result.json"),
            &result,
        )?;
        if let Some(strategy) = search
            .combination_artifact(control_mission, ridge, cart, &baselines.gate)
            .map_err(anyhow::Error::msg)?
        {
            data_mission::write_json_atomic(
                &results_dir.join("combination-walk-forward.json"),
                &strategy,
            )?;
            return Ok(Some(strategy));
        }
        return Ok(None);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_cex_event_replay(
    results_dir: &Path,
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    materialization_sha256: &str,
    factor_bank: &CexFactorBankRevisionV2,
    context: &EngineContext<'_>,
    strategy: &CexCombinationResearchArtifactV1,
    policy: &CexEventReplayPolicyV1,
    replay_artifact_path: &Path,
    replay_artifact_sha256: &str,
    replay_manifest_path: &Path,
    replay_manifest_sha256: &str,
    implementation_source_revision: &str,
    image_identity: &str,
) -> anyhow::Result<CexEventReplayReceiptV1> {
    policy.validate_binding(&mission.spec.policies.replay)?;
    if mission.spec.inputs.materialization.content_sha256 != materialization_sha256 {
        bail!("CEX replay materialization identity drifted");
    }
    let positions = strategy
        .target_positions(factor_bank, context.rows())
        .map_err(anyhow::Error::msg)?;
    let decisions = context
        .rows()
        .iter()
        .zip(positions)
        .map(|(row, target_position)| TargetPositionDecision {
            timestamp_us: row.available_time.timestamp_micros(),
            target_position,
        })
        .collect::<Vec<_>>();
    let first_decision_time = decisions
        .first()
        .context("CEX event replay has no pre-holdout decisions")?
        .timestamp_us;
    let last_decision_time = decisions
        .last()
        .context("CEX event replay has no pre-holdout decisions")?
        .timestamp_us;
    let replay_manifest_sha256 = normalized_sha256("CEX replay manifest", replay_manifest_sha256)?;
    let verified = verify_canonical_replay_artifact(
        replay_artifact_path,
        replay_manifest_path,
        Some(replay_artifact_sha256),
        &replay_manifest_sha256,
        None,
        Some(last_decision_time),
    )?;
    validate_replay_materialization_binding(
        &verified.evidence,
        mission,
        materialization,
        first_decision_time,
        last_decision_time,
    )?;
    let trade_tape_declared = verified
        .evidence
        .modalities
        .iter()
        .any(|modality| matches!(modality.as_str(), "trade" | "aggregate_trade"));
    let max_depth_levels = materialization
        .top_depth
        .max(strategy.risk.capacity_depth_levels)
        .max(policy.required_depth_levels);
    let replay_config = TargetPositionReplayConfig {
        max_depth_levels,
        max_decision_delay_us: policy
            .max_decision_delay_millis
            .checked_mul(1_000)
            .context("CEX replay decision delay overflow")?,
        position_notional_usd: strategy.risk.position_notional_usd,
        fee_bps: strategy.execution.costs.fee_bps,
        rebate_bps: strategy.execution.costs.rebate_bps,
        funding_bps: strategy.execution.costs.funding_bps,
        latency_bps: strategy.execution.costs.latency_bps,
        additional_slippage_bps: strategy.execution.costs.slippage_bps,
        cross_spread: strategy.execution.costs.cross_spread,
        capacity_depth_levels: strategy.risk.capacity_depth_levels,
        trade_tape_declared,
    };
    let metrics = replay_target_positions(&verified.bytes, &decisions, &replay_config)?;
    let capabilities = CexReplayCapabilitiesV1 {
        clock_semantics: policy.clock_semantics.clone(),
        modalities: verified.evidence.modalities.clone(),
        min_bid_depth_levels: metrics.min_bid_depth_levels,
        max_bid_depth_levels: metrics.max_bid_depth_levels,
        min_ask_depth_levels: metrics.min_ask_depth_levels,
        max_ask_depth_levels: metrics.max_ask_depth_levels,
        trade_tape_available: metrics.trade_events > 0,
        queue_position: false,
        partial_fills: false,
        market_impact: false,
        true_capacity: false,
    };
    let mut failures = Vec::new();
    if metrics.snapshot_events + metrics.l2_update_events < policy.min_book_events {
        failures.push("book event count is below the frozen replay minimum".to_string());
    }
    if metrics.l2_update_events < policy.min_l2_updates {
        failures.push("L2 update count is below the frozen replay minimum".to_string());
    }
    if metrics.decision_count < policy.min_decisions {
        failures.push("decision count is below the frozen replay minimum".to_string());
    }
    if metrics.min_bid_depth_levels < policy.required_depth_levels
        || metrics.min_ask_depth_levels < policy.required_depth_levels
    {
        failures.push("observed L2 depth is below the frozen replay minimum".to_string());
    }
    if policy.require_trade_tape && !capabilities.trade_tape_available {
        failures.push("the frozen replay policy requires an unavailable trade tape".to_string());
    }
    if metrics.max_abs_position > strategy.risk.max_abs_position {
        failures.push("replay position exceeds the frozen Risk stage".to_string());
    }
    if metrics.max_drawdown > strategy.risk.max_drawdown {
        failures.push("replay drawdown exceeds the frozen Risk stage".to_string());
    }
    if strategy.risk.position_notional_usd > 0.0
        && metrics
            .max_same_side_depth_fraction
            .is_none_or(|fraction| fraction > strategy.risk.max_book_depth_fraction)
    {
        failures.push("same-side depth gate exceeds the frozen Risk stage".to_string());
    }
    let gate = CexReplayGateV1 {
        passed: failures.is_empty(),
        failures,
    };
    let receipt = CexEventReplayReceiptV1 {
        schema_version: CEX_EVENT_REPLAY_RECEIPT_SCHEMA_VERSION.to_string(),
        receipt_id: String::new(),
        mission_id: mission.semantic_id()?,
        strategy: CexResearchContentRefV1 {
            id: strategy.artifact_id.clone(),
            content_sha256: canonical_json_hash(strategy)?,
        },
        dataset: mission.spec.inputs.dataset.clone(),
        materialization: mission.spec.inputs.materialization.clone(),
        tape_artifact: CexResearchContentRefV1 {
            id: format!("cex-event-replay-tape-{replay_artifact_sha256}"),
            content_sha256: replay_artifact_sha256.to_string(),
        },
        tape_manifest: CexResearchContentRefV1 {
            id: format!("cex-event-replay-manifest-{replay_manifest_sha256}"),
            content_sha256: replay_manifest_sha256,
        },
        source: mission.spec.inputs.source.clone(),
        replay_policy: mission.spec.policies.replay.clone(),
        replay_config_sha256: canonical_json_hash(&replay_config)?,
        replay_config,
        decision_sha256: canonical_json_hash(&decisions)?,
        decision_scope: "pre_holdout_research_rows".to_string(),
        implementation_source_revision: implementation_source_revision.to_string(),
        image_identity: image_identity.to_string(),
        replay_engine_version: TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION.to_string(),
        metrics,
        capabilities,
        gate,
        holdout_id: mission.spec.holdout.holdout_id.clone(),
        holdout_state: mission.spec.holdout.state,
        deployment_authority: false,
        order_submission_authority: false,
    }
    .finalize()?;
    data_mission::write_json_atomic(&results_dir.join("cex-event-replay-receipt.json"), &receipt)?;
    Ok(receipt)
}

fn validate_replay_materialization_binding(
    replay: &CanonicalReplayEvidence,
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    first_decision_time_us: i64,
    last_decision_time_us: i64,
) -> anyhow::Result<()> {
    let expected_dataset = format!("binance_{}_lob", materialization.market);
    if replay.mission_id != materialization.mission_id
        || replay.market != materialization.market
        || replay.symbol != materialization.symbol
        || replay.dataset != expected_dataset
        || replay.modalities != vec!["lob".to_string()]
        || replay.source_revision != materialization.source_revision
        || replay.source_revision != mission.spec.inputs.source.content_sha256
        || replay.first_event_time_us > first_decision_time_us
        || replay.last_event_time_us < last_decision_time_us
        || replay.source_segments.len() != materialization.source_segments.len()
        || replay
            .source_segments
            .iter()
            .zip(&materialization.source_segments)
            .any(|(replay, materialized)| !same_replay_segment(replay, materialized))
    {
        bail!("CEX event replay identity does not match its materialization");
    }
    Ok(())
}

fn same_replay_segment(
    replay: &CanonicalSourceSegmentEvidence,
    materialized: &SourceSegment,
) -> bool {
    materialized.path.file_name().and_then(|name| name.to_str()) == Some(replay.file.as_str())
        && replay.sha256 == materialized.sha256
        && replay.collector_manifest_sha256 == materialized.collector_manifest_sha256
        && replay.success_marker_sha256 == materialized.success_marker_sha256
        && replay.start_received_at_ns == materialized.start_received_at_ns
        && replay.end_received_at_ns == materialized.end_received_at_ns
        && replay.events == materialized.events
}

fn build_factor_bank(
    control_mission: &CexResearchMissionArtifactV1,
    gp_policy: &CexGpPolicyV1,
    run_report: &mission::MissionRunReport,
    lineage: &MissionLineage,
) -> anyhow::Result<CexFactorBankRevisionV2> {
    let mut attempts = Vec::with_capacity(lineage.iterations.len());
    for iteration in &lineage.iterations {
        let candidate_id = iteration
            .candidate_artifact_id
            .as_deref()
            .with_context(|| {
                format!(
                    "iteration {} has no auditable candidate artifact",
                    iteration.iteration_id
                )
            })?;
        let candidate = lineage
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == candidate_id)
            .with_context(|| format!("candidate {candidate_id} is missing from lineage"))?;
        if candidate.mission_id != run_report.mission_id
            || candidate.iteration_id != iteration.iteration_id
        {
            bail!("candidate {candidate_id} does not match its mission iteration");
        }
        let canonical_ast = match &candidate.artifact {
            CandidateArtifact::Formula(ast) => ast.clone(),
            _ => bail!("governed GP lineage contains a non-formula candidate"),
        };
        let ast_sha256 = canonical_json_hash(&canonical_ast)?;
        let evaluation = iteration
            .evaluation_artifact_id
            .as_deref()
            .map(|evaluation_id| {
                let stored = lineage
                    .evaluations
                    .iter()
                    .find(|evaluation| evaluation.record.evaluation_id == evaluation_id)
                    .with_context(|| {
                        format!("evaluation {evaluation_id} is missing from lineage")
                    })?;
                if stored.record.mission_id != run_report.mission_id
                    || stored.record.candidate_id != candidate_id
                    || stored.record.dataset_manifest_id != run_report.dataset_manifest_id
                    || stored.record.evaluation_protocol_hash
                        != control_mission.spec.policies.evaluation.content_sha256
                {
                    bail!("evaluation {evaluation_id} does not match its screening lineage");
                }
                let evidence: CandidateEvaluation =
                    serde_json::from_value(stored.record.payload.clone())?;
                evidence.validate()?;
                Ok::<_, anyhow::Error>(CexFactorEvaluationEvidenceV2 {
                    candidate_id: candidate_id.to_string(),
                    candidate_ast_sha256: ast_sha256.clone(),
                    research_dataset: run_report.research_dataset.clone(),
                    walk_forward_partition: run_report.walk_forward_partition.clone(),
                    evidence,
                })
            })
            .transpose()?;
        let verdict = if iteration.verdict == IterationVerdict::Keep {
            CexFactorScreeningVerdictV1::Accepted
        } else {
            CexFactorScreeningVerdictV1::Rejected
        };
        let rejection_codes = match iteration.verdict {
            IterationVerdict::Keep => vec![],
            IterationVerdict::Discard => evaluation
                .as_ref()
                .context("discarded candidate is missing screening evidence")?
                .rejection_codes()?,
            IterationVerdict::Crash => vec![match iteration.failure_class.as_deref() {
                Some("duplicate_candidate") => CexFactorRejectionCodeV1::DuplicateCandidate,
                Some("evaluation_error") => CexFactorRejectionCodeV1::EvaluationFailed,
                _ => CexFactorRejectionCodeV1::EngineFailure,
            }],
        };
        let rejection_details = evaluation
            .as_ref()
            .map(|evaluation| evaluation.evidence.failure_reasons.clone())
            .filter(|reasons| !reasons.is_empty())
            .or_else(|| {
                iteration
                    .failure_explanation
                    .clone()
                    .map(|reason| vec![reason])
            })
            .unwrap_or_default();
        let post_warmup_coverage_rows = evaluation
            .as_ref()
            .map(|evaluation| evaluation.evidence.metrics.row_count)
            .unwrap_or(0);
        attempts.push(CexFactorScreeningAttemptV2 {
            candidate_id: candidate_id.to_string(),
            canonical_ast,
            ast_sha256,
            post_warmup_coverage_rows,
            verdict,
            rejection_codes,
            rejection_details,
            evaluation,
        });
    }
    Ok(CexFactorBankRevisionV2::new(
        control_mission.spec.search_lineage_id.clone(),
        gp_policy.clone(),
        control_mission.spec.policies.screening.clone(),
        control_mission.spec.policies.evaluation.clone(),
        run_report.research_dataset.clone(),
        run_report.walk_forward_partition.clone(),
        attempts,
    )?)
}

fn validate_args(args: &ExecuteMissionArgs) -> anyhow::Result<()> {
    if args.work_dir.as_os_str().is_empty()
        || [
            args.mission_url.as_str(),
            args.feature_url.as_str(),
            args.materialization_url.as_str(),
            args.replay_artifact_url.as_str(),
            args.replay_manifest_url.as_str(),
            args.result_put_url.as_str(),
            args.result_readback_url.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        bail!("Mission execution paths and URLs are required");
    }
    normalized_sha256("Mission", &args.mission_sha256)?;
    normalized_sha256("CEX replay artifact", &args.replay_artifact_sha256)?;
    normalized_sha256("CEX replay manifest", &args.replay_manifest_sha256)?;
    if !valid_git_revision(&args.implementation_source_revision) {
        bail!("CEX replay implementation source revision is invalid");
    }
    immutable_sha256_identity("CEX replay image", &args.image_identity)?;
    resume_source(args)?;
    validate_result_readback_binding(&args.result_put_url, &args.result_readback_url)?;
    Ok(())
}

fn resume_source(args: &ExecuteMissionArgs) -> anyhow::Result<Option<(&str, &str)>> {
    let url = args
        .resume_url
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let sha256 = args
        .resume_sha256
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    match (url, sha256) {
        (None, None) => Ok(None),
        (Some(url), Some(sha256)) => {
            normalized_sha256("MCTS resume checkpoint", sha256)?;
            Ok(Some((url, sha256)))
        }
        _ => bail!("MCTS resume checkpoint URL and SHA256 must be supplied together"),
    }
}

fn validate_result_readback_binding(
    result_put_url: &str,
    result_readback_url: &str,
) -> anyhow::Result<()> {
    let put_is_remote =
        result_put_url.starts_with("http://") || result_put_url.starts_with("https://");
    let readback_is_remote =
        result_readback_url.starts_with("http://") || result_readback_url.starts_with("https://");
    let same_object = match (put_is_remote, readback_is_remote) {
        (true, true) => {
            prediction_dispatch::canonical_https_object("CEX result", result_put_url)?
                == prediction_dispatch::canonical_https_object(
                    "CEX result readback",
                    result_readback_url,
                )?
        }
        (false, false) => {
            Path::new(
                result_put_url
                    .strip_prefix("file://")
                    .unwrap_or(result_put_url),
            ) == Path::new(
                result_readback_url
                    .strip_prefix("file://")
                    .unwrap_or(result_readback_url),
            )
        }
        _ => false,
    };
    if !same_object {
        bail!("CEX result readback URL must identify the same immutable result object");
    }
    Ok(())
}

fn validate_mission_materialization_binding(
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    materialization_sha256: &str,
    feature_sha256: &str,
) -> anyhow::Result<()> {
    let spec = &mission.spec;
    let snapshot_sha256 = materialization.snapshot.sha256();
    let partition_sha256 = canonical_json_hash(&materialization.snapshot.source_segments)?;
    if spec.instrument.venue.as_str() != materialization.snapshot.venue
        || spec.instrument.market.as_str() != materialization.market
        || spec.instrument.market.as_str() != materialization.snapshot.instrument_type
        || spec.instrument.symbol != materialization.symbol
        || spec.instrument.symbol != materialization.snapshot.symbol
        || spec.instrument.horizon.horizon_buckets != materialization.label_horizon_buckets
        || spec.instrument.horizon.observation_frequency_millis != materialization.bucket_ms
    {
        bail!("CEX Research Mission instrument or horizon does not match materialization");
    }
    if spec.data_mission_id != materialization.mission_id
        || spec.inputs.materialization.id != materialization.mission_id
        || spec.inputs.materialization.content_sha256 != materialization_sha256
        || spec.inputs.snapshot.id != format!("cex-replay-snapshot-{snapshot_sha256}")
        || spec.inputs.snapshot.content_sha256 != snapshot_sha256
        || spec.inputs.partition.id != format!("cex-replay-partition-{partition_sha256}")
        || spec.inputs.partition.content_sha256 != partition_sha256
        || spec.inputs.source.id != materialization.source_revision
        || spec.inputs.source.content_sha256 != materialization.source_revision
        || spec.inputs.feature.id != format!("dataset-{feature_sha256}")
        || spec.inputs.feature.content_sha256 != feature_sha256
    {
        bail!("CEX Research Mission input identities do not match materialization");
    }
    Ok(())
}

fn validate_mission_dataset_binding(
    mission: &CexResearchMissionArtifactV1,
    features: &hft_collector::FeatureDatasetManifest,
    dataset: &CexReplayDatasetManifestV2,
) -> anyhow::Result<()> {
    if mission.spec.inputs.feature.id != features.manifest_id
        || mission.spec.inputs.feature.content_sha256 != features.artifact_sha256
        || mission.spec.inputs.dataset.id != dataset.manifest_id
        || mission.spec.inputs.dataset.content_sha256 != canonical_json_hash(dataset)?
    {
        bail!("CEX Research Mission dataset identities do not match admitted manifests");
    }
    Ok(())
}

fn validate_materialization(
    materialization: &Materialization,
    feature_sha256: &str,
    validation: &ValidationArgs,
) -> anyhow::Result<()> {
    materialization
        .snapshot
        .validate()
        .map_err(anyhow::Error::new)?;
    let snapshot_sha256 =
        normalized_sha256("CEX replay snapshot", &materialization.snapshot_sha256)?;
    if snapshot_sha256 != materialization.snapshot.sha256() {
        bail!("CEX replay snapshot SHA256 mismatch");
    }
    if materialization.dataset_kind != MATERIALIZATION_KIND
        || materialization.schema_version != BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3
    {
        bail!("materialization kind or schema is unsupported");
    }
    if materialization.mission_id.trim().is_empty()
        || materialization.symbol.trim().is_empty()
        || !matches!(materialization.market.as_str(), "spot" | "usdm")
        || materialization.source_revision.trim().is_empty()
        || materialization.source_segments.is_empty()
        || materialization.bucket_ms == 0
        || materialization.label_horizon_buckets == 0
        || materialization.top_depth == 0
    {
        bail!("materialization lineage is incomplete");
    }
    if materialization.snapshot.symbol != materialization.symbol
        || materialization.snapshot.instrument_type != materialization.market
        || materialization.snapshot.bucket_ms != materialization.bucket_ms
        || materialization.snapshot.label_horizon_buckets != materialization.label_horizon_buckets
        || materialization.snapshot.top_depth != materialization.top_depth
        || materialization.snapshot.first_event_time != materialization.first_event_time
        || materialization.snapshot.last_event_time != materialization.last_event_time
        || materialization.snapshot.feature_artifact_sha256 != materialization.artifact_sha256
        || materialization.snapshot.source_segments.len() != materialization.source_segments.len()
        || materialization
            .snapshot
            .source_segments
            .iter()
            .zip(&materialization.source_segments)
            .any(|(snapshot, report)| {
                snapshot.content_sha256 != report.sha256
                    || snapshot.manifest_sha256 != report.collector_manifest_sha256
                    || snapshot.start_received_at_ns != report.start_received_at_ns
                    || snapshot.end_received_at_ns != report.end_received_at_ns
                    || snapshot.events != report.events
            })
    {
        bail!("CEX replay snapshot does not match the materialization");
    }
    if materialization.bucket_ms != validation.observation_frequency_millis
        || materialization.label_horizon_buckets != validation.label_horizon_buckets
    {
        bail!("evaluation label horizon or frequency does not match the materialization");
    }
    let (fee_bps, funding_bps, latency_bps) =
        data_mission::cex_snapshot_costs(&materialization.snapshot, validation.cross_spread)?;
    if validation.rebate_bps != 0.0
        || validation.fee_bps.to_bits() != fee_bps.to_bits()
        || validation.funding_bps.to_bits() != funding_bps.to_bits()
        || validation.latency_bps.to_bits() != latency_bps.to_bits()
    {
        bail!("evaluation costs do not match the verified CEX replay snapshot");
    }
    let artifact_sha256 = normalized_sha256("feature artifact", &materialization.artifact_sha256)?;
    for segment in &materialization.source_segments {
        let source_sha256 = normalized_sha256("source segment", &segment.sha256)?;
        normalized_sha256(
            "source collector manifest",
            &segment.collector_manifest_sha256,
        )?;
        let success_marker_sha256 =
            normalized_sha256("source success marker", &segment.success_marker_sha256)?;
        if success_marker_sha256 != hex::encode(Sha256::digest(format!("{source_sha256}\n"))) {
            bail!("source success marker does not bind its segment");
        }
    }
    if artifact_sha256 != feature_sha256 {
        bail!("PIT feature artifact does not match materialization");
    }
    Ok(())
}

pub(crate) fn normalized_sha256(label: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim().to_ascii_lowercase();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} SHA256 is invalid");
    }
    Ok(value)
}

fn immutable_sha256_identity(label: &str, value: &str) -> anyhow::Result<String> {
    let digest = value
        .strip_prefix("sha256:")
        .with_context(|| format!("{label} must use a sha256: identity"))?;
    let normalized = format!("sha256:{}", normalized_sha256(label, digest)?);
    if value != normalized {
        bail!("{label} must be a lowercase immutable SHA256 identity");
    }
    Ok(normalized)
}

fn valid_git_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn fetch_to_file(
    client: &Client,
    source: &str,
    destination: &Path,
    max_bytes: u64,
) -> anyhow::Result<(u64, String)> {
    let mut reader: Box<dyn Read> =
        if source.starts_with("http://") || source.starts_with("https://") {
            let response = client.get(source).send()?.error_for_status()?;
            if response
                .content_length()
                .is_some_and(|length| length > max_bytes)
            {
                bail!("source exceeds the allowed size");
            }
            Box::new(response)
        } else {
            let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
            let file = File::open(path)
                .with_context(|| format!("failed to open local source {}", path.display()))?;
            if file.metadata()?.len() > max_bytes {
                bail!("source exceeds the allowed size");
            }
            Box::new(file)
        };
    let mut temporary = data_mission::temporary_output_file(destination, ".monday-fetch-")?;
    let bytes = std::io::copy(
        &mut reader.by_ref().take(max_bytes + 1),
        temporary.as_file_mut(),
    )?;
    temporary.as_file().sync_all()?;
    if bytes > max_bytes {
        bail!("source exceeds the allowed size");
    }
    data_mission::persist_output_file(temporary, destination, "fetched source")?;
    Ok((bytes, sha256_file(destination)?))
}

pub(crate) fn create_bundle<'a>(
    work_dir: &Path,
    bundle: &Path,
    roots: impl IntoIterator<Item = &'a PathBuf>,
) -> anyhow::Result<()> {
    let mut files = Vec::new();
    for root in roots {
        collect_files(root, &mut files)?;
    }
    files.sort();
    let temporary = data_mission::temporary_output_file(bundle, ".monday-bundle-")?;
    let mut archive = ZipWriter::new(temporary.reopen()?);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for path in files {
        let name = path
            .strip_prefix(work_dir)
            .with_context(|| format!("bundle path escapes work directory: {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        archive.start_file(name, options)?;
        std::io::copy(&mut File::open(path)?, &mut archive)?;
    }
    let file = archive.finish()?;
    file.sync_all()?;
    drop(file);
    data_mission::persist_output_file(temporary, bundle, "bundle")?;
    Ok(())
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.path().symlink_metadata()?;
        if metadata.file_type().is_symlink() {
            bail!("bundle input cannot contain symbolic links");
        }
        if metadata.is_dir() {
            collect_files(&entry.path(), files)?;
        } else if metadata.is_file() {
            files.push(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn publish_result(
    client: &Client,
    destination: &str,
    bundle: &Path,
) -> anyhow::Result<()> {
    if destination.starts_with("http://") || destination.starts_with("https://") {
        client
            .put(destination)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/zip"))
            .header("x-oss-forbid-overwrite", "true")
            .body(File::open(bundle)?)
            .send()?
            .error_for_status()?;
        return Ok(());
    }
    let path = Path::new(destination.strip_prefix("file://").unwrap_or(destination));
    let mut output = data_mission::temporary_output_file(path, ".monday-result-")?;
    std::io::copy(&mut File::open(bundle)?, output.as_file_mut())?;
    output.as_file().sync_all()?;
    match output.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("result destination already exists: {}", path.display())
        }
        Err(error) => Err(error.error)
            .with_context(|| format!("atomically publish result to {}", path.display())),
    }
}

pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(hex::encode(digest.finalize()))
}

pub(crate) fn configured_sibling_binary(environment: &str, name: &str) -> anyhow::Result<PathBuf> {
    let path = std::env::var_os(environment)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(Ok)
        .unwrap_or_else(|| {
            let current = std::env::current_exe().context("resolve alpha-harness executable")?;
            let parent = current
                .parent()
                .context("alpha-harness executable has no parent directory")?;
            Ok::<_, anyhow::Error>(parent.join(name))
        })?;
    if !path.is_file() {
        bail!(
            "configured sibling binary does not exist: {}",
            path.display()
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::ValidationArgs;
    use alpha_domain::{
        CexBaselineArtifactV1, CexBaselineGateV1, CexEventReplayPolicyV1, CexResearchContentRefV1,
        CexResearchEvidenceKindV1, CexResearchEvidenceRefV1, CexResearchFalsificationTestV1,
        CexResearchHoldoutStateV1, CexResearchHoldoutV1, CexResearchHypothesisTargetV1,
        CexResearchHypothesisV1, CexResearchInputBindingsV1, CexResearchInstrumentV1,
        CexResearchMarketV1, CexResearchMissionSpecV1, CexResearchOperationalMetadataV1,
        CexResearchPolicyBindingsV1, CexResearchSearchPlanV1, CexResearchVenueV1,
        EvaluationLabelSpecV1, SearchBudget, CEX_RESEARCH_MISSION_SCHEMA_V1,
    };
    use alpha_engine::engines::{CexCombinationResearchArtifactV1, CexFactorBankMctsResultV1};

    fn cex_triplet(byte: char) -> hft_research_manifest::CexArtifactTripletV2 {
        hft_research_manifest::CexArtifactTripletV2 {
            data_sha256: byte.to_string().repeat(64),
            manifest_sha256: byte.to_string().repeat(64),
            success_sha256: byte.to_string().repeat(64),
        }
    }
    use chrono::{Duration as ChronoDuration, Utc};
    use hft_collector::{DataModality, PointInTimeFeatureRow};
    use parquet::{
        data_type::{ByteArray, ByteArrayType, Int64Type},
        file::{
            properties::WriterProperties,
            writer::{SerializedFileWriter, SerializedRowGroupWriter},
        },
        schema::parser::parse_message_type,
    };
    use std::{
        collections::{BTreeMap, BTreeSet},
        fs::File,
        io::{BufRead, Write},
        sync::atomic::{AtomicU64, Ordering},
        sync::Arc,
    };

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn execute_rejects_features_that_have_no_live_formula_semantics() {
        let mut fixture = fixture("unsupported-live-fields");
        fixture.mission.spec.feature_fields = vec!["book_imbalance_top5".to_string()];
        resign_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "feature field book_imbalance_top5 is not live executable: unsupported live field: book_imbalance_top5"
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_unknown_mission_schema_before_materialization() {
        let mut fixture = fixture("unknown-mission-schema");
        fixture.mission.schema_version = "cex-research-mission-v999".to_string();
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("schema version is unsupported"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_tampered_mission_artifact_before_admission() {
        let fixture = fixture("tampered-mission-artifact");
        let mut bytes = std::fs::read(&fixture.mission_path).unwrap();
        bytes.push(b'\n');
        std::fs::write(&fixture.mission_path, bytes).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("Mission artifact SHA256 mismatch"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_tampered_replay_artifact_hash() {
        let fixture = fixture("tampered-replay-artifact");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&fixture.replay_artifact_path)
            .unwrap()
            .write_all(b"tampered")
            .unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("CEX replay artifact SHA256 mismatch"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_replay_source_identity_drift() {
        let mut fixture = fixture("replay-source-identity-drift");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&fixture.replay_manifest_path).unwrap()).unwrap();
        manifest["source_segments"][0]["collector_manifest_sha256"] =
            serde_json::json!("9".repeat(64));
        std::fs::write(
            &fixture.replay_manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fixture.args.replay_manifest_sha256 = sha256_file(&fixture.replay_manifest_path).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("does not match its materialization"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_cross_instrument_mission_before_admission() {
        let mut fixture = fixture("cross-instrument-mission");
        fixture.mission.spec.instrument.symbol = "ETHUSDT".to_string();
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("instrument or horizon does not match materialization"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_prediction_market_fields_before_admission() {
        let mut fixture = fixture("cross-lane-mission");
        let mut value = serde_json::to_value(&fixture.mission).unwrap();
        value["task_capability"] = serde_json::json!("btc_5m_backtest");
        value["cohort_manifest_id"] = serde_json::json!("prediction-cohort-1");
        value["settlement"] = serde_json::json!({"token_id": "yes-token"});
        std::fs::write(
            &fixture.mission_path,
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        fixture.args.mission_sha256 = sha256_file(&fixture.mission_path).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(format!("{error:#}").contains("unknown field"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_agent_action_fields_before_admission() {
        let mut fixture = fixture("agent-action-mission");
        let mut value = serde_json::to_value(&fixture.mission).unwrap();
        value["spec"]["actions"] = serde_json::json!([
            "evaluate",
            "open_holdout",
            "create_deployment_envelope",
            "submit_order",
            "start_live_small"
        ]);
        std::fs::write(
            &fixture.mission_path,
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        fixture.args.mission_sha256 = sha256_file(&fixture.mission_path).unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(format!("{error:#}").contains("unknown field"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_exposed_holdout_from_resumed_search() {
        let mut fixture = fixture("resumed-search-holdout-evidence");
        fixture.mission.spec.evidence[0].kind = CexResearchEvidenceKindV1::ExposedHoldout;
        fixture.mission.spec.evidence[0].source_search_lineage_id =
            fixture.mission.spec.search_lineage_id.clone();
        fixture.mission.spec.evidence[0].holdout_id = Some("earlier-holdout-1".to_string());
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("exposed holdout evidence cannot feed the same search"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_feature_hash_mismatch() {
        let mut fixture = fixture("hash-mismatch");
        fixture.materialization["artifact_sha256"] = serde_json::json!("0".repeat(64));
        fixture.materialization["snapshot"]["feature_artifact_sha256"] =
            serde_json::json!("0".repeat(64));
        let snapshot: CexReplaySnapshotV2 =
            serde_json::from_value(fixture.materialization["snapshot"].clone()).unwrap();
        fixture.materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        resign_materialization_outer(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("PIT feature artifact does not match materialization"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_snapshot_missing_aggregate_trade_modality() {
        let mut fixture = fixture("missing-aggregate-trade-modality");
        fixture.materialization["snapshot"]["required_modalities"] = serde_json::json!(["lob"]);
        fixture.materialization["snapshot_sha256"] = serde_json::json!("0".repeat(64));
        resign_materialization_outer(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("required modalities"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_binds_snapshot_to_mission_dataset_identity() {
        let fixture = fixture("snapshot-dataset-identity");
        let expected = format!(
            "dataset-cex-replay-{}",
            fixture.materialization["snapshot_sha256"].as_str().unwrap()
        );

        execute(fixture.args.clone()).unwrap();

        let mission: ResearchMission = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/mission.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(mission.dataset_manifest_id.as_str(), expected);
        assert_eq!(mission.mission_id, fixture.mission.semantic_id().unwrap());
        let admission: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/mission-admission.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(admission["mission_id"], mission.mission_id);
        assert_eq!(
            admission["mission_artifact_sha256"],
            fixture.args.mission_sha256
        );
        assert_eq!(admission["dataset_manifest_id"], expected);
        let store = AlphaStore::open(fixture.args.work_dir.join("results/alpha.duckdb")).unwrap();
        let revision = store.get_registry_revision(&mission.mission_id).unwrap();
        assert_eq!(revision.revision_id, mission.mission_id);
        assert_eq!(revision.registry_kind, "cex_research_mission");
        assert_eq!(revision.asset_id, "BTCUSDT");
        assert_eq!(
            revision.parent_revision_id.as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            revision.payload,
            serde_json::json!({
                "schema_version": &fixture.mission.schema_version,
                "spec": &fixture.mission.spec,
            })
        );
        drop(store);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_feature_available_after_decision_clock() {
        let mut fixture = fixture("feature-after-decision-clock");
        rewrite_features(&mut fixture, |row| {
            row.feature_available_time += ChronoDuration::milliseconds(1);
        });

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature availability does not match the CEX replay decision clock"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_feature_source_revision_not_bound_to_snapshot_segments() {
        let mut fixture = fixture("forged-feature-source-revision");
        let forged_revision = "9".repeat(64);
        rewrite_features(&mut fixture, |row| {
            row.source_revisions
                .insert("binance-usdm-lob".to_string(), forged_revision.clone());
        });
        fixture.materialization["source_revision"] = serde_json::json!(forged_revision);
        resign_materialization(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature source revision does not match the CEX replay snapshot"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_snapshot_range_that_disagrees_with_feature_rows() {
        let mut fixture = fixture("snapshot-feature-range-mismatch");
        let first_event_time = serde_json::from_value::<chrono::DateTime<Utc>>(
            fixture.materialization["first_event_time"].clone(),
        )
        .unwrap()
            + ChronoDuration::seconds(1);
        fixture.materialization["first_event_time"] = serde_json::json!(first_event_time);
        fixture.materialization["snapshot"]["first_event_time"] =
            serde_json::json!(first_event_time);
        resign_materialization(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature time bounds do not match the CEX replay snapshot"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_label_availability_past_snapshot_segment_end() {
        let mut fixture = fixture("label-past-snapshot-segment-end");
        let last_event_time = serde_json::from_value::<chrono::DateTime<Utc>>(
            fixture.materialization["last_event_time"].clone(),
        )
        .unwrap();
        let source_end_ns = u64::try_from(
            (last_event_time + ChronoDuration::seconds(5))
                .timestamp_nanos_opt()
                .unwrap(),
        )
        .unwrap();
        fixture.materialization["source_segments"][0]["end_received_at_ns"] =
            serde_json::json!(source_end_ns);
        fixture.materialization["snapshot"]["source_segments"][0]["end_received_at_ns"] =
            serde_json::json!(source_end_ns);
        rewrite_features(&mut fixture, |row| {
            if row.event_time == last_event_time {
                row.label_available_time += ChronoDuration::nanoseconds(1);
            }
        });

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("feature label availability is outside the CEX replay snapshot"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_publishes_once_and_refuses_overwrite() {
        let fixture = fixture("immutable-result");
        execute(fixture.args.clone()).unwrap();
        assert!(fixture.result_path.is_file());

        let mut second_args = fixture.args;
        second_args.work_dir = fixture.root.join("work-2");
        let error = execute(second_args).unwrap_err();

        assert!(error
            .to_string()
            .contains("result destination already exists"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_records_explicit_taker_costs_in_evidence() {
        let mut fixture = fixture("explicit-taker-costs");
        fixture.mission.spec.evaluation_protocol.costs.fee_bps = 5.0;
        fixture.mission.spec.evaluation_protocol.costs.rebate_bps = 0.0;
        fixture.mission.spec.evaluation_protocol.costs.slippage_bps = 0.75;
        fixture.mission.spec.evaluation_protocol.costs.cross_spread = true;
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .position_notional_usd = 10_000.0;
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .capacity_depth_levels = 5;
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .max_book_depth_fraction = 0.1;
        resign_mission(&mut fixture);
        execute(fixture.args.clone()).unwrap();

        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/candidates.json")).unwrap(),
        )
        .unwrap();
        let costs =
            &evidence["evaluations"][0]["record"]["payload"]["evaluation_protocol"]["costs"];
        assert_eq!(costs["fee_bps"], 5.0);
        assert_eq!(costs["rebate_bps"], 0.0);
        assert_eq!(costs["latency_bps"], 0.5);
        assert_eq!(costs["slippage_bps"], 0.75);
        assert_eq!(costs["cross_spread"], true);
        assert_eq!(costs["position_notional_usd"], 10_000.0);
        assert_eq!(costs["capacity_depth_levels"], 5);
        assert_eq!(costs["max_book_depth_fraction"], 0.1);
        assert!(
            evidence["evaluations"][0]["record"]["payload"]["metrics"]["folds"]
                .as_array()
                .unwrap()
                .iter()
                .all(|fold| fold["max_book_depth_fraction"].as_f64().unwrap() <= 0.1)
        );
        assert!(
            evidence["evaluations"][0]["record"]["payload"]["metrics"]["total_turnover"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        let execution_model: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/execution-model.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(execution_model["schema_version"], "execution_cost_model_v2");
        assert_eq!(execution_model["rebate_bps"], 0.0);
        assert_eq!(execution_model["additional_slippage_bps"], 0.75);
        assert_eq!(execution_model["queue_position_modeled"], false);
        assert_eq!(execution_model["partial_fills_modeled"], false);
        assert_eq!(execution_model["market_impact_modeled"], false);
        assert_eq!(execution_model["capacity_modeled"], false);
        assert_eq!(execution_model["capacity_gate_enabled"], true);
        assert_eq!(
            execution_model["capacity_gate_model"],
            "same_side_top_n_depth_fraction"
        );
        assert!(!fixture
            .args
            .work_dir
            .join("results/sealed-evaluations.jsonl")
            .exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_optimistic_costs_below_snapshot_evidence() {
        let mut fixture = fixture("optimistic-snapshot-costs");
        fixture.mission.spec.evaluation_protocol.costs.fee_bps = 0.0;
        resign_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("evaluation costs do not match the verified CEX replay snapshot"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_stops_before_sealed_holdout() {
        let fixture = fixture("sealed-holdout-remains-closed");

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        assert!(results.join("mission-admission.json").exists());
        assert!(!results.join("sealed-evaluations.jsonl").exists());
        assert!(!results.join("mcts-research-receipt.json").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_screens_gp_candidates_into_an_immutable_factor_bank() {
        let mut fixture = fixture("gp-factor-bank");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        execute(fixture.args.clone()).unwrap();
        let results = fixture.args.work_dir.join("results");
        let run: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results.join("mission-run.json")).unwrap())
                .unwrap();
        assert_eq!(run["engine"], "Gp");

        let factor_bank: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results.join("factor-bank.json")).unwrap())
                .unwrap();
        assert_eq!(factor_bank["schema_version"], "cex-factor-bank-v2");
        assert_eq!(factor_bank["research_dataset"], run["research_dataset"]);
        let entries = factor_bank["entries"].as_array().unwrap();
        assert!(!entries.is_empty());
        assert!(entries
            .iter()
            .all(|entry| entry["orientation"] == "positive"));
        assert!(results.join("gp-policy.json").exists());
        let typed: CexFactorBankRevisionV2 = serde_json::from_value(factor_bank.clone()).unwrap();
        typed.validate().unwrap();
        let mut forged = typed.clone();
        forged.screening_policy.content_sha256 = "0".repeat(64);
        forged.revision_id.clear();
        forged.revision_id = format!("cex-factor-bank-{}", canonical_json_hash(&forged).unwrap());
        assert!(forged.validate().is_err());
        let revision_id = factor_bank["revision_id"].as_str().unwrap();
        let mut store = AlphaStore::open(results.join("alpha.duckdb")).unwrap();
        let revision = store.get_registry_revision(revision_id).unwrap();
        assert_eq!(revision.registry_kind, "cex_factor_bank");
        assert_eq!(
            revision.parent_revision_id.as_deref(),
            run["dataset_manifest_id"].as_str()
        );
        assert_eq!(revision.payload, factor_bank);
        let mut conflict = revision.clone();
        conflict.payload["schema_version"] = serde_json::json!("tampered");
        assert!(matches!(
            store.put_registry_revision(&conflict),
            Err(StoreError::DuplicateRecord)
        ));
        assert!(store
            .mission_lineage(fixture.mission.semantic_id().unwrap().as_str())
            .unwrap()
            .iterations
            .iter()
            .all(|iteration| iteration.engine == alpha_domain::EngineKind::GeneticProgramming));
        assert!(!results.join("mcts-research-receipt.json").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_emits_passing_ridge_and_cart_baseline_evidence() {
        let mut fixture = fixture("ridge-cart-baselines");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let ridge: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results.join("ridge-baseline.json")).unwrap())
                .unwrap();
        let cart: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results.join("cart-baseline.json")).unwrap())
                .unwrap();
        let gate: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json")).unwrap())
                .unwrap();
        let policy: CexBaselinePolicyV1 =
            serde_json::from_slice(&std::fs::read(results.join("baseline-policy.json")).unwrap())
                .unwrap();
        let policy_hash = policy.content_hash().unwrap();

        assert_eq!(ridge["model_kind"], "ridge");
        assert_eq!(cart["model_kind"], "shallow_cart");
        assert_eq!(
            ridge["factor_bank_revision_id"],
            cart["factor_bank_revision_id"]
        );
        assert_eq!(ridge["folds"].as_array().unwrap().len(), 2);
        assert_eq!(cart["folds"].as_array().unwrap().len(), 2);
        assert_eq!(gate["passed"], true);
        assert_eq!(gate["ridge_artifact_id"], ridge["artifact_id"]);
        assert_eq!(gate["cart_artifact_id"], cart["artifact_id"]);
        assert_eq!(gate["policy_hash"], policy_hash);
        let subset_result: serde_json::Value = serde_json::from_slice(
            &std::fs::read(results.join("factor-subset-mcts-result.json")).unwrap(),
        )
        .unwrap();
        let subset_trace: serde_json::Value = serde_json::from_slice(
            &std::fs::read(results.join("factor-subset-mcts-trace.json")).unwrap(),
        )
        .unwrap();
        let checkpoint_artifact: MctsCheckpointArtifactV1 = serde_json::from_slice(
            &std::fs::read(results.join("factor-subset-mcts-checkpoint.json")).unwrap(),
        )
        .unwrap();
        let store = AlphaStore::open(results.join("alpha.duckdb")).unwrap();
        let manifest = data_mission::read_registered_research_dataset(
            &store,
            &results.join("cex-replay-dataset-manifest.json"),
        )
        .unwrap();
        let rows = manifest
            .load_rows(&fixture.mission.spec.evaluation_protocol.costs)
            .unwrap();
        let dataset = prepare_dataset(rows, &fixture.mission.spec.evaluation_protocol).unwrap();
        let context = dataset.engine_context();
        let selected_factors = subset_result["selected"]["subset"]["factors"]
            .as_array()
            .unwrap();
        assert_eq!(
            subset_result["schema_version"],
            "cex-factor-bank-subset-mcts-result-v1"
        );
        assert_eq!(
            subset_result["factor_bank_revision_id"],
            ridge["factor_bank_revision_id"]
        );
        assert_eq!(subset_result["baseline_gate_id"], gate["gate_id"]);
        assert_eq!(
            subset_result["checkpoint_sha256"],
            checkpoint_artifact.checkpoint_sha256
        );
        assert!(!selected_factors.is_empty());
        assert_eq!(
            subset_result["selected"]["normalized_equal_abs_weight"]
                .as_f64()
                .unwrap()
                * selected_factors.len() as f64,
            1.0
        );
        assert!(!subset_trace.as_array().unwrap().is_empty());
        assert!(subset_trace.as_array().unwrap().iter().all(|step| {
            matches!(
                step["action"]["action"].as_str(),
                Some("add" | "remove" | "swap")
            )
        }));
        assert!(results.join("factor-subset-mcts-checkpoint.json").exists());
        assert!(!results.join("sealed-evaluations.jsonl").exists());
        let factor_bank_typed: CexFactorBankRevisionV2 =
            serde_json::from_slice(&std::fs::read(results.join("factor-bank.json")).unwrap())
                .unwrap();
        let ridge_typed: CexBaselineArtifactV1 = serde_json::from_value(ridge.clone()).unwrap();
        let cart_typed: CexBaselineArtifactV1 = serde_json::from_value(cart.clone()).unwrap();
        let gate_typed: CexBaselineGateV1 = serde_json::from_value(gate.clone()).unwrap();
        let subset_result_typed: CexFactorBankMctsResultV1 =
            serde_json::from_value(subset_result.clone()).unwrap();
        let strategy: CexCombinationResearchArtifactV1 = serde_json::from_slice(
            &std::fs::read(results.join("combination-walk-forward.json")).unwrap(),
        )
        .unwrap();
        strategy
            .validate_binding(
                &fixture.mission,
                &factor_bank_typed,
                &ridge_typed,
                &cart_typed,
                &gate_typed,
                &context,
                &checkpoint_artifact.checkpoint,
                &subset_result_typed,
            )
            .unwrap();
        assert_eq!(
            strategy.subset_checkpoint.content_sha256,
            checkpoint_artifact.checkpoint_sha256
        );
        let mut forged_result = subset_result_typed.clone();
        forged_result.checkpoint_sha256 = "0".repeat(64);
        assert!(strategy
            .validate_binding(
                &fixture.mission,
                &factor_bank_typed,
                &ridge_typed,
                &cart_typed,
                &gate_typed,
                &context,
                &checkpoint_artifact.checkpoint,
                &forged_result,
            )
            .is_err());
        let strategy_json = serde_json::to_value(&strategy).unwrap();
        assert_eq!(
            strategy_json["schema_version"],
            "cex-combination-research-artifact-v1"
        );
        assert_eq!(strategy_json["deployment_authority"], false);
        assert_eq!(strategy_json["order_submission_authority"], false);
        assert_eq!(strategy_json["signal"]["normalization"], "none_required");
        assert_eq!(
            strategy_json["signal"]["combination_rule"],
            "oriented_equal_absolute_sum_to_one"
        );
        assert_eq!(
            strategy_json["sizing"]["rule"],
            "zero_within_machine_epsilon_else_sign"
        );
        assert_eq!(strategy_json["risk"]["immutable"], true);
        assert_eq!(strategy_json["execution"]["venue"], "binance");
        assert_eq!(strategy_json["execution"]["symbol"], "BTCUSDT");
        assert_eq!(
            strategy_json["execution"]["event_modality"],
            "bucketed_point_in_time_l2_features"
        );
        assert_eq!(
            strategy_json["walk_forward_evidence"]["holdout_state"],
            "unopened"
        );
        assert_eq!(
            strategy_json["walk_forward_evidence"]["ridge"]["kind"],
            "ridge_baseline"
        );
        assert_eq!(
            strategy_json["walk_forward_evidence"]["cart"]["kind"],
            "cart_baseline"
        );
        let replay_receipt: CexEventReplayReceiptV1 = serde_json::from_slice(
            &std::fs::read(results.join("cex-event-replay-receipt.json")).unwrap(),
        )
        .unwrap();
        replay_receipt.validate().unwrap();
        assert!(replay_receipt.gate.passed);
        assert_eq!(replay_receipt.strategy.id, strategy.artifact_id);
        assert_eq!(
            replay_receipt.source.content_sha256,
            fixture.mission.spec.inputs.source.content_sha256
        );
        assert_eq!(replay_receipt.capabilities.modalities, vec!["lob"]);
        assert!(!replay_receipt.capabilities.trade_tape_available);
        assert!(!replay_receipt.capabilities.queue_position);
        assert!(!replay_receipt.capabilities.partial_fills);
        assert!(!replay_receipt.capabilities.market_impact);
        assert!(!replay_receipt.capabilities.true_capacity);
        assert_eq!(
            replay_receipt.holdout_state,
            CexResearchHoldoutStateV1::Unopened
        );
        assert!(!replay_receipt.deployment_authority);
        assert!(!replay_receipt.order_submission_authority);
        let selected_metrics =
            &strategy_json["walk_forward_evidence"]["selected"]["evaluation"]["metrics"];
        assert!(selected_metrics["predictive"].is_object());
        for metric in [
            "total_turnover",
            "mean_net_return",
            "cumulative_net_return",
            "max_drawdown",
            "net_sharpe",
        ] {
            assert!(selected_metrics[metric].is_number());
        }

        let mut bad_link = strategy.clone();
        bad_link.sizing.parent = bad_link.subset_result.clone();
        assert!(bad_link.validate().is_err());
        let mut bad_weight = strategy.clone();
        bad_weight.signal.factors[0].normalized_absolute_weight = 0.5;
        assert!(bad_weight.validate().is_err());
        let mut bad_factor = strategy.clone();
        bad_factor.signal.factors[0].factor.factor_id = "unknown-factor".to_string();
        assert!(bad_factor.validate().is_err());
        let mut bad_orientation = strategy.clone();
        bad_orientation.signal.factors[0].orientation =
            match bad_orientation.signal.factors[0].orientation {
                alpha_domain::CexFactorOrientationV1::Positive => {
                    alpha_domain::CexFactorOrientationV1::Negative
                }
                alpha_domain::CexFactorOrientationV1::Negative => {
                    alpha_domain::CexFactorOrientationV1::Positive
                }
            };
        assert!(bad_orientation.validate().is_err());
        let mut bad_policy = strategy.clone();
        bad_policy.signal.weight_policy.id.push_str("-drifted");
        assert!(bad_policy.validate().is_err());
        let mut bad_metric_identity = strategy.clone();
        bad_metric_identity
            .walk_forward_evidence
            .selected
            .evaluation_sha256 = "0".repeat(64);
        assert!(bad_metric_identity.validate().is_err());
        let policy_revision = store.get_registry_revision(&policy_hash).unwrap();
        assert_eq!(
            policy_revision.registry_kind,
            CEX_BASELINE_POLICY_REGISTRY_KIND
        );
        assert_eq!(policy_revision.revision_id, policy_hash);
        assert_eq!(
            policy_revision.asset_id,
            fixture.mission.spec.instrument.symbol
        );
        assert_eq!(
            policy_revision.parent_revision_id.as_deref(),
            Some(fixture.mission.semantic_id().unwrap().as_str())
        );
        assert_eq!(
            policy_revision.payload,
            serde_json::to_value(&policy).unwrap()
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn factor_bank_mcts_resume_is_exact_and_rejects_bound_identity_drift() {
        let mut fixture = fixture("factor-bank-mcts-resume");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let factor_bank: CexFactorBankRevisionV2 =
            serde_json::from_slice(&std::fs::read(results.join("factor-bank.json")).unwrap())
                .unwrap();
        let ridge: CexBaselineArtifactV1 =
            serde_json::from_slice(&std::fs::read(results.join("ridge-baseline.json")).unwrap())
                .unwrap();
        let cart: CexBaselineArtifactV1 =
            serde_json::from_slice(&std::fs::read(results.join("cart-baseline.json")).unwrap())
                .unwrap();
        let gate: CexBaselineGateV1 =
            serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json")).unwrap())
                .unwrap();
        let store = AlphaStore::open(results.join("alpha.duckdb")).unwrap();
        let manifest = data_mission::read_registered_research_dataset(
            &store,
            &results.join("cex-replay-dataset-manifest.json"),
        )
        .unwrap();
        let rows = manifest
            .load_rows(&fixture.mission.spec.evaluation_protocol.costs)
            .unwrap();
        let dataset =
            prepare_dataset(rows.clone(), &fixture.mission.spec.evaluation_protocol).unwrap();
        let context = dataset.engine_context();

        let mut failed_ridge = ridge.clone();
        failed_ridge.evaluation.passed = false;
        assert!(CexFactorBankMcts::new(
            &fixture.mission,
            &factor_bank,
            &failed_ridge,
            &cart,
            &gate,
            &context,
        )
        .is_err());

        let mut other_lineage_bank = factor_bank.clone();
        other_lineage_bank.search_lineage_id = "other-search-lineage".to_string();
        other_lineage_bank.revision_id.clear();
        other_lineage_bank.revision_id = format!(
            "cex-factor-bank-{}",
            canonical_json_hash(&other_lineage_bank).unwrap()
        );
        other_lineage_bank.validate().unwrap();
        assert!(CexFactorBankMcts::new(
            &fixture.mission,
            &other_lineage_bank,
            &ridge,
            &cart,
            &gate,
            &context,
        )
        .err()
        .expect("a Factor Bank from another search lineage must fail closed")
        .contains("producer bindings drifted"));

        let mut context_bound = CexFactorBankMcts::new(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
        )
        .unwrap();
        let mut drifted_rows = rows;
        drifted_rows[0].label += 0.000_001;
        let drifted_dataset =
            prepare_dataset(drifted_rows, &fixture.mission.spec.evaluation_protocol).unwrap();
        assert!(context_bound
            .run(&drifted_dataset.engine_context(), Some(1))
            .expect_err("a different research dataset must fail before a search transition")
            .contains("research dataset identity drifted"));

        let mut full = CexFactorBankMcts::new(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
        )
        .unwrap();
        assert_ne!(
            full.run(&context, None).unwrap(),
            CexFactorBankMctsStopReasonV1::Paused
        );
        let full_checkpoint = full.checkpoint().unwrap();
        let full_trace = full.trace().unwrap();
        let full_result = full.result().unwrap();

        let mut chunked = CexFactorBankMcts::new(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
        )
        .unwrap();
        let mut missing_untried_action =
            serde_json::to_value(chunked.checkpoint().unwrap()).unwrap();
        missing_untried_action["nodes"][0]["remaining_actions"] = serde_json::json!([]);
        missing_untried_action["nodes"][0]["subtree_has_expansion"] = serde_json::json!(false);
        assert!(CexFactorBankMcts::restore_json(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
            missing_untried_action,
        )
        .err()
        .expect("a checkpoint cannot delete untried actions")
        .contains("untried actions drifted"));
        assert_eq!(
            chunked.run(&context, Some(1)).unwrap(),
            CexFactorBankMctsStopReasonV1::Paused
        );
        let paused = serde_json::to_value(chunked.checkpoint().unwrap()).unwrap();
        let mut expansion_index_drift = paused.clone();
        expansion_index_drift["nodes"][0]["subtree_has_expansion"] = serde_json::json!(!paused
            ["nodes"][0]["subtree_has_expansion"]
            .as_bool()
            .unwrap());
        assert!(CexFactorBankMcts::restore_json(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
            expansion_index_drift,
        )
        .err()
        .expect("the persisted expansion index must be verified")
        .contains("expansion index drifted"));
        let mut rng_drift = paused.clone();
        rng_drift["rng"] = serde_json::json!(rng_drift["rng"].as_u64().unwrap() ^ 1);
        let mut stats_drift = paused.clone();
        let reward = stats_drift["nodes"][0]["total_reward"].as_f64().unwrap() + 1.0;
        stats_drift["nodes"][0]["total_reward"] = serde_json::json!(reward);
        stats_drift["nodes"][0]["best_reward"] = serde_json::json!(reward);
        for drifted in [rng_drift, stats_drift] {
            assert!(CexFactorBankMcts::restore_json(
                &fixture.mission,
                &factor_bank,
                &ridge,
                &cart,
                &gate,
                &context,
                drifted,
            )
            .err()
            .expect("derived checkpoint state must replay exactly")
            .contains("restored replay drifted"));
        }
        for (usage, limit) in [
            ("candidates_evaluated", "max_candidates"),
            ("expansions_used", "max_expansions"),
        ] {
            let mut over_budget = paused.clone();
            over_budget[usage] =
                serde_json::json!(over_budget["bindings"]["budget"][limit].as_u64().unwrap() + 1);
            assert!(CexFactorBankMcts::restore_json(
                &fixture.mission,
                &factor_bank,
                &ridge,
                &cart,
                &gate,
                &context,
                over_budget,
            )
            .err()
            .expect("checkpoint usage cannot exceed its frozen budget")
            .contains("exceeds its frozen budget"));
        }
        let mut resumed = CexFactorBankMcts::restore_json(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
            paused.clone(),
        )
        .unwrap();
        assert_ne!(
            resumed.run(&context, None).unwrap(),
            CexFactorBankMctsStopReasonV1::Paused
        );
        assert_eq!(resumed.checkpoint().unwrap(), full_checkpoint);
        assert_eq!(resumed.trace().unwrap(), full_trace);
        assert_eq!(resumed.result().unwrap(), full_result);

        for pointer in [
            "/bindings/mission_id",
            "/bindings/factor_bank_revision_id",
            "/bindings/ridge_artifact_id",
            "/bindings/cart_artifact_id",
            "/bindings/baseline_gate_id",
            "/bindings/implementation_version",
        ] {
            let mut drifted = paused.clone();
            *drifted.pointer_mut(pointer).unwrap() = serde_json::json!("drifted");
            assert!(CexFactorBankMcts::restore_json(
                &fixture.mission,
                &factor_bank,
                &ridge,
                &cart,
                &gate,
                &context,
                drifted,
            )
            .err()
            .expect("identity drift must be rejected")
            .contains("bindings drifted"));
        }
        for pointer in [
            "/bindings/subset_policy/content_sha256",
            "/bindings/weight_policy/content_sha256",
            "/bindings/screening_policy/content_sha256",
            "/bindings/scoring_policy/content_sha256",
        ] {
            let mut drifted = paused.clone();
            *drifted.pointer_mut(pointer).unwrap() = serde_json::json!("0".repeat(64));
            assert!(CexFactorBankMcts::restore_json(
                &fixture.mission,
                &factor_bank,
                &ridge,
                &cart,
                &gate,
                &context,
                drifted,
            )
            .err()
            .expect("policy drift must be rejected")
            .contains("bindings drifted"));
        }
        for (pointer, value) in [
            ("/trace/0/action/factor_bank_revision_id", "other-bank"),
            ("/trace/0/action/factor/factor_id", "unknown-factor"),
            (
                "/trace/0/action/factor/content_sha256",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
        ] {
            let mut invalid = paused.clone();
            *invalid.pointer_mut(pointer).unwrap() = serde_json::json!(value);
            assert!(CexFactorBankMcts::restore_json(
                &fixture.mission,
                &factor_bank,
                &ridge,
                &cart,
                &gate,
                &context,
                invalid,
            )
            .is_err());
        }
        let legacy_error = CexFactorBankMcts::restore_json(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
            serde_json::json!({ "kind": "Mcts", "version": 4, "state": {} }),
        )
        .err()
        .expect("legacy Formula-MCTS checkpoint must be rejected");
        assert!(legacy_error.contains("legacy Formula-MCTS checkpoint version 4"));
        assert!(legacy_error.contains("cex-factor-bank-subset-mcts-v1"));

        let resume_path = fixture.root.join("paused-factor-subset-mcts.json");
        let resume_artifact =
            MctsCheckpointArtifactV1::new(serde_json::from_value(paused.clone()).unwrap()).unwrap();
        let resume_content_sha256 = resume_artifact.checkpoint_sha256.clone();
        std::fs::write(
            &resume_path,
            serde_json::to_vec_pretty(&resume_artifact).unwrap(),
        )
        .unwrap();
        assert_ne!(sha256_file(&resume_path).unwrap(), resume_content_sha256);
        fixture.args.work_dir = fixture.root.join("work-resumed");
        fixture.args.result_put_url = fixture
            .root
            .join("result-resumed.zip")
            .to_string_lossy()
            .into();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();
        fixture.args.resume_url = Some(resume_path.to_string_lossy().into_owned());
        fixture.args.resume_sha256 = Some(resume_content_sha256);
        execute(fixture.args.clone()).unwrap();
        let resumed_result: CexFactorBankMctsResultV1 = serde_json::from_slice(
            &std::fs::read(
                fixture
                    .args
                    .work_dir
                    .join("results/factor-subset-mcts-result.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(resumed_result, full_result);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn factor_bank_mcts_reaches_full_depth_under_a_full_factor_bank_budget() {
        let mut fixture = fixture("factor-bank-mcts-full-depth-budget");
        fixture
            .mission
            .spec
            .feature_fields
            .push("mid_price".to_string());
        fixture.mission.spec.search.budget.max_candidates = 3;
        fixture.mission.spec.search.budget.max_expansions = 3;
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.features.insert("spread_bps".to_string(), direction);
            row.features
                .insert("mid_price".to_string(), direction * 0.25);
            row.label = direction * 0.001;
        });

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let factor_bank: CexFactorBankRevisionV2 =
            serde_json::from_slice(&std::fs::read(results.join("factor-bank.json")).unwrap())
                .unwrap();
        assert_eq!(factor_bank.entries.len(), 3);
        let trace: Vec<serde_json::Value> = serde_json::from_slice(
            &std::fs::read(results.join("factor-subset-mcts-trace.json")).unwrap(),
        )
        .unwrap();
        assert!(trace.iter().any(|step| {
            step["resulting_state"]["factors"]
                .as_array()
                .is_some_and(|factors| factors.len() == 3)
        }));

        let ridge: CexBaselineArtifactV1 =
            serde_json::from_slice(&std::fs::read(results.join("ridge-baseline.json")).unwrap())
                .unwrap();
        let cart: CexBaselineArtifactV1 =
            serde_json::from_slice(&std::fs::read(results.join("cart-baseline.json")).unwrap())
                .unwrap();
        let gate: CexBaselineGateV1 =
            serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json")).unwrap())
                .unwrap();
        let store = AlphaStore::open(results.join("alpha.duckdb")).unwrap();
        let manifest = data_mission::read_registered_research_dataset(
            &store,
            &results.join("cex-replay-dataset-manifest.json"),
        )
        .unwrap();
        let rows = manifest
            .load_rows(&fixture.mission.spec.evaluation_protocol.costs)
            .unwrap();
        let dataset = prepare_dataset(rows, &fixture.mission.spec.evaluation_protocol).unwrap();
        let context = dataset.engine_context();
        let checkpoint_artifact: MctsCheckpointArtifactV1 = serde_json::from_slice(
            &std::fs::read(results.join("factor-subset-mcts-checkpoint.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            checkpoint_artifact.checkpoint_sha256,
            checkpoint_artifact.checkpoint.content_hash().unwrap()
        );
        let mut checkpoint = serde_json::to_value(checkpoint_artifact.checkpoint).unwrap();
        let mut failed_combination: CandidateEvaluation =
            serde_json::from_value(checkpoint["nodes"][2]["evaluation"].clone()).unwrap();
        failed_combination.passed = false;
        failed_combination.failure_reasons = vec!["drawdown gate failed".to_string()];
        failed_combination.metrics.folds[0].max_drawdown = 1.0;
        failed_combination.metrics.max_drawdown = failed_combination
            .metrics
            .folds
            .iter()
            .map(|fold| fold.max_drawdown)
            .fold(0.0_f64, f64::max);
        failed_combination.validate().unwrap();
        checkpoint["nodes"][2]["evaluation"] = serde_json::to_value(&failed_combination).unwrap();
        let evaluation_sha256 = canonical_json_hash(&failed_combination).unwrap();
        let combination_step = checkpoint["trace"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|step| step["outcome"]["node_id"] == 2)
            .unwrap();
        combination_step["outcome"]["evaluation_sha256"] = serde_json::json!(evaluation_sha256);
        checkpoint["selected_node_id"] = serde_json::json!(2);
        assert!(CexFactorBankMcts::restore_json(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
            checkpoint.clone(),
        )
        .err()
        .expect("a failed subset cannot remain selected")
        .contains("selected subset drifted"));
        checkpoint["selected_node_id"] = serde_json::json!(1);
        assert!(CexFactorBankMcts::restore_json(
            &fixture.mission,
            &factor_bank,
            &ridge,
            &cart,
            &gate,
            &context,
            checkpoint,
        )
        .err()
        .expect("self-consistent fabricated evaluations must be rejected")
        .contains("restored replay drifted"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn mission_execute_then_mcts_refuses_after_replaying_passing_baselines() {
        let mut fixture = fixture("mcts-baseline-gate");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let db = results.join("alpha.duckdb");
        let producer_id = fixture.mission.semantic_id().unwrap();
        let mut store = AlphaStore::open(&db).unwrap();
        let producer = store.get_mission(&producer_id).unwrap();
        let gate: serde_json::Value =
            serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json")).unwrap())
                .unwrap();
        assert_eq!(gate["passed"], true);
        let gate_id = gate["gate_id"].as_str().unwrap().to_string();
        let gate_revision = store.get_registry_revision(&gate_id).unwrap();
        assert_eq!(gate_revision.registry_kind, "cex_baseline_gate");
        assert_eq!(
            gate_revision.parent_revision_id,
            gate["factor_bank_revision_id"].as_str().map(str::to_owned)
        );

        let mut consumer = producer;
        consumer.mission_id = format!("{producer_id}-mcts-consumer");
        consumer.baseline_artifact_id = Some(gate_id);
        consumer.status = MissionStatus::Pending;
        consumer.terminal_reason = None;
        consumer.created_at = Utc::now();
        consumer.updated_at = consumer.created_at;
        let consumer_id = consumer.mission_id.clone();
        store.create_mission(&consumer).unwrap();
        drop(store);

        let args = RunMissionArgs {
            db,
            mission_id: consumer_id,
            engine: EngineChoice::Mcts,
            seed: fixture.mission.spec.search.seed,
            feature_fields: fixture.mission.spec.feature_fields.clone(),
            offline_trace: None,
            max_new_iterations: Some(1),
            dataset: DatasetArgs {
                dataset_manifest: results.join("cex-replay-dataset-manifest.json"),
                validation: ValidationArgs::from_protocol(
                    &fixture.mission.spec.evaluation_protocol,
                ),
            },
        };
        let error = mission::execute_mission(&args, false).unwrap_err();

        assert!(error
            .to_string()
            .contains("available only through content-bound mission execute"));
        let store = AlphaStore::open(&args.db).unwrap();
        assert_eq!(
            store.get_mission(&args.mission_id).unwrap().status,
            MissionStatus::Pending
        );
        assert!(store
            .mission_lineage(&args.mission_id)
            .unwrap()
            .iterations
            .is_empty());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn mcts_rejects_missing_or_tampered_baseline_policy_revision_before_transition() {
        let mut fixture = fixture("mcts-baseline-policy-revision");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let source_db = results.join("alpha.duckdb");
        let producer_id = fixture.mission.semantic_id().unwrap();
        let source = AlphaStore::open(&source_db).unwrap();
        let producer = source.get_mission(&producer_id).unwrap();
        let dataset_manifest: CexReplayDatasetManifestV2 = serde_json::from_slice(
            &std::fs::read(results.join("cex-replay-dataset-manifest.json")).unwrap(),
        )
        .unwrap();
        let gate: alpha_domain::CexBaselineGateV1 =
            serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json")).unwrap())
                .unwrap();
        let policy_revision = source.get_registry_revision(&gate.policy_hash).unwrap();
        let copied_revisions = [
            dataset_manifest.feature_manifest_id.as_str(),
            dataset_manifest.manifest_id.as_str(),
            gate.mission_id.as_str(),
            gate.factor_bank_revision_id.as_str(),
            gate.ridge_artifact_id.as_deref().unwrap(),
            gate.cart_artifact_id.as_deref().unwrap(),
            gate.gate_id.as_str(),
        ]
        .map(|revision_id| source.get_registry_revision(revision_id).unwrap());
        drop(source);

        let mut wrong_kind = policy_revision.clone();
        wrong_kind.registry_kind = "cex_baseline_gate".to_string();
        let mut wrong_parent = policy_revision.clone();
        wrong_parent.parent_revision_id = Some("wrong-mission".to_string());
        let mut tampered_payload = policy_revision.clone();
        tampered_payload.payload["ridge_l2"] = serde_json::json!(2.0e-6);

        for (case, policy_revision) in [
            ("missing", None),
            ("wrong-kind", Some(wrong_kind)),
            ("wrong-parent", Some(wrong_parent)),
            ("tampered-payload", Some(tampered_payload)),
        ] {
            let db = results.join(format!("policy-{case}.duckdb"));
            let mut store = AlphaStore::open(&db).unwrap();
            for revision in &copied_revisions {
                store.put_registry_revision(revision).unwrap();
            }
            if let Some(revision) = policy_revision {
                store.put_registry_revision(&revision).unwrap();
            }
            let mut consumer = producer.clone();
            consumer.mission_id = format!("{producer_id}-mcts-{case}");
            consumer.baseline_artifact_id = Some(gate.gate_id.clone());
            consumer.status = MissionStatus::Pending;
            consumer.terminal_reason = None;
            consumer.created_at = Utc::now();
            consumer.updated_at = consumer.created_at;
            store.create_mission(&consumer).unwrap();
            drop(store);

            let args = RunMissionArgs {
                db,
                mission_id: consumer.mission_id,
                engine: EngineChoice::Mcts,
                seed: fixture.mission.spec.search.seed,
                feature_fields: fixture.mission.spec.feature_fields.clone(),
                offline_trace: None,
                max_new_iterations: Some(1),
                dataset: DatasetArgs {
                    dataset_manifest: results.join("cex-replay-dataset-manifest.json"),
                    validation: ValidationArgs::from_protocol(
                        &fixture.mission.spec.evaluation_protocol,
                    ),
                },
            };
            let error = mission::execute_mission(&args, false).unwrap_err();

            assert!(
                error.to_string().contains("baseline policy"),
                "{case}: {error:#}"
            );
            let store = AlphaStore::open(&args.db).unwrap();
            assert_eq!(
                store.get_mission(&args.mission_id).unwrap().status,
                MissionStatus::Pending,
                "{case}"
            );
            assert!(
                store
                    .mission_lineage(&args.mission_id)
                    .unwrap()
                    .iterations
                    .is_empty(),
                "{case}"
            );
        }
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn mcts_rejects_a_domain_valid_failed_gate_before_transition() {
        let fixture = fixture("mcts-failed-empty-gate");
        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let db = results.join("alpha.duckdb");
        let producer_id = fixture.mission.semantic_id().unwrap();
        let mut store = AlphaStore::open(&db).unwrap();
        let producer = store.get_mission(&producer_id).unwrap();
        let factor_bank: CexFactorBankRevisionV2 =
            serde_json::from_slice(&std::fs::read(results.join("factor-bank.json")).unwrap())
                .unwrap();
        assert!(factor_bank.entries.is_empty());
        let baseline_policy =
            CexBaselinePolicyV1::controlled_v1(fixture.mission.spec.policies.baseline.id.clone())
                .unwrap();
        let gate: alpha_domain::CexBaselineGateV1 =
            serde_json::from_slice(&std::fs::read(results.join("baseline-gate.json")).unwrap())
                .unwrap();
        assert!(!gate.passed);
        assert_eq!(
            gate.failure_codes,
            vec![alpha_domain::CexBaselineFailureCodeV1::EmptyFactorBank]
        );
        gate.validate().unwrap();
        assert_eq!(
            gate,
            alpha_domain::CexBaselineGateV1::empty_factor_bank(
                &producer_id,
                &baseline_policy,
                &factor_bank
            )
            .unwrap()
        );
        let gate_revision = store.get_registry_revision(&gate.gate_id).unwrap();
        assert_eq!(gate_revision.registry_kind, "cex_baseline_gate");
        assert_eq!(gate_revision.payload, serde_json::to_value(&gate).unwrap());

        let mut consumer = producer;
        consumer.mission_id = format!("{producer_id}-mcts-failed");
        consumer.baseline_artifact_id = Some(gate.gate_id.clone());
        consumer.status = MissionStatus::Pending;
        consumer.terminal_reason = None;
        consumer.created_at = Utc::now();
        consumer.updated_at = consumer.created_at;
        let consumer_id = consumer.mission_id.clone();
        store.create_mission(&consumer).unwrap();
        drop(store);

        let args = RunMissionArgs {
            db,
            mission_id: consumer_id,
            engine: EngineChoice::Mcts,
            seed: fixture.mission.spec.search.seed,
            feature_fields: fixture.mission.spec.feature_fields.clone(),
            offline_trace: None,
            max_new_iterations: Some(1),
            dataset: DatasetArgs {
                dataset_manifest: results.join("cex-replay-dataset-manifest.json"),
                validation: ValidationArgs::from_protocol(
                    &fixture.mission.spec.evaluation_protocol,
                ),
            },
        };
        let error = mission::execute_mission(&args, false).unwrap_err();

        assert_eq!(error.to_string(), "MCTS baseline gate did not pass");
        let store = AlphaStore::open(&args.db).unwrap();
        assert_eq!(
            store.get_mission(&args.mission_id).unwrap().status,
            MissionStatus::Pending
        );
        assert!(store
            .mission_lineage(&args.mission_id)
            .unwrap()
            .iterations
            .is_empty());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn mcts_rejects_a_tampered_published_gate_before_transition() {
        let mut fixture = fixture("mcts-tampered-gate");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let db = results.join("alpha.duckdb");
        let producer_id = fixture.mission.semantic_id().unwrap();
        let mut store = AlphaStore::open(&db).unwrap();
        let producer = store.get_mission(&producer_id).unwrap();
        let gate_revision = store
            .get_registry_revision(
                serde_json::from_slice::<serde_json::Value>(
                    &std::fs::read(results.join("baseline-gate.json")).unwrap(),
                )
                .unwrap()["gate_id"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        let mut tampered_gate: alpha_domain::CexBaselineGateV1 =
            serde_json::from_value(gate_revision.payload.clone()).unwrap();
        tampered_gate.policy_hash = "0".repeat(64);
        tampered_gate.gate_id.clear();
        tampered_gate.gate_id = format!(
            "cex-baseline-gate-{}",
            canonical_json_hash(&tampered_gate).unwrap()
        );
        tampered_gate.validate().unwrap();
        let tampered_gate_id = tampered_gate.gate_id.clone();
        store
            .put_registry_revision(&alpha_store::RegistryRevision {
                revision_id: tampered_gate_id.clone(),
                registry_kind: gate_revision.registry_kind,
                asset_id: gate_revision.asset_id,
                parent_revision_id: gate_revision.parent_revision_id,
                payload: serde_json::to_value(&tampered_gate).unwrap(),
                created_at: Utc::now(),
            })
            .unwrap();

        let mut consumer = producer;
        consumer.mission_id = format!("{producer_id}-mcts-tampered");
        consumer.baseline_artifact_id = Some(tampered_gate_id);
        consumer.status = MissionStatus::Pending;
        consumer.terminal_reason = None;
        consumer.created_at = Utc::now();
        consumer.updated_at = consumer.created_at;
        let consumer_id = consumer.mission_id.clone();
        store.create_mission(&consumer).unwrap();
        drop(store);

        let args = RunMissionArgs {
            db,
            mission_id: consumer_id,
            engine: EngineChoice::Mcts,
            seed: fixture.mission.spec.search.seed,
            feature_fields: fixture.mission.spec.feature_fields.clone(),
            offline_trace: None,
            max_new_iterations: Some(1),
            dataset: DatasetArgs {
                dataset_manifest: results.join("cex-replay-dataset-manifest.json"),
                validation: ValidationArgs::from_protocol(
                    &fixture.mission.spec.evaluation_protocol,
                ),
            },
        };
        let error = mission::execute_mission(&args, false).unwrap_err();

        assert!(error
            .to_string()
            .contains("baseline policy registry revision"));
        let store = AlphaStore::open(&args.db).unwrap();
        assert_eq!(
            store.get_mission(&args.mission_id).unwrap().status,
            MissionStatus::Pending
        );
        assert!(store
            .mission_lineage(&args.mission_id)
            .unwrap()
            .iterations
            .is_empty());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_mixed_hypothesis_targets_before_side_effects() {
        let mut fixture = fixture("mixed-hypothesis-targets");
        let mut second = fixture.mission.spec.hypotheses[0].clone();
        second.hypothesis_id = "hypothesis-2".to_string();
        second.target.name = "forward_mid_return_other".to_string();
        fixture.mission.spec.hypotheses.push(second);
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("CEX Research Mission hypotheses do not share one frozen baseline target"));
        assert!(!fixture.args.work_dir.join("input/features.jsonl").exists());
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        assert!(!fixture
            .args
            .work_dir
            .join("results/gp-policy.json")
            .exists());
        assert!(!fixture
            .args
            .work_dir
            .join("results/baseline-policy.json")
            .exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_gp_policy_drift_before_screening() {
        let mut fixture = fixture("gp-policy-drift");
        fixture.mission.spec.policies.gp.content_sha256 = "f".repeat(64);
        write_mission(&mut fixture);
        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("GP policy identity or content hash"));
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_records_duplicate_gp_attempts_with_a_typed_rejection() {
        let mut fixture = fixture("gp-duplicate-audit");
        fixture.mission.spec.search.seed = 1;
        fixture.mission.spec.search.budget.max_candidates = 6;
        fixture.mission.spec.search.budget.max_expansions = 6;
        rewrite_features(&mut fixture, |row| row.label = 0.0);
        rebind_mission_inputs(&mut fixture);

        execute(fixture.args.clone()).unwrap();

        let factor_bank = read_factor_bank(&fixture.args);
        let attempts = factor_bank["attempts"].as_array().unwrap();
        let duplicate = attempts
            .iter()
            .find(|attempt| {
                attempt["rejection_details"]
                    .as_array()
                    .is_some_and(|details| {
                        details.iter().any(|detail| {
                            detail
                                .as_str()
                                .is_some_and(|detail| detail.contains("duplicated an existing"))
                        })
                    })
            })
            .expect("fixture must exercise a duplicate GP proposal");
        assert_eq!(
            duplicate["rejection_codes"],
            serde_json::json!(["duplicate_candidate"])
        );
        let screened = attempts
            .iter()
            .find(|attempt| attempt["verdict"] == "rejected" && attempt["evaluation"].is_object())
            .expect("fixture must exercise an evaluated rejection");
        let rejection_codes = screened["rejection_codes"].as_array().unwrap();
        assert!(rejection_codes.contains(&serde_json::json!("predictive_gate_failed")));
        assert!(!rejection_codes.contains(&serde_json::json!("screening_failed")));
        let entries = factor_bank["entries"].as_array().unwrap();
        assert!(attempts
            .iter()
            .filter(|attempt| attempt["verdict"] == "rejected")
            .all(|attempt| entries
                .iter()
                .all(|entry| entry["candidate_id"] != attempt["candidate_id"])));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_is_deterministic_and_ignores_holdout_only_values() {
        let mut fixture = fixture("gp-holdout-isolation");
        fixture.mission.spec.search.budget.max_candidates = 4;
        fixture.mission.spec.search.budget.max_expansions = 4;
        fixture.mission.spec.search.max_new_iterations = 1;
        resign_mission(&mut fixture);
        execute(fixture.args.clone()).unwrap();
        let first = read_factor_bank(&fixture.args);

        fixture.mission.spec.search.max_new_iterations = 4;
        resign_mission(&mut fixture);
        fixture.args.work_dir = fixture.root.join("work-2");
        fixture.args.result_put_url = fixture.root.join("result-2.zip").to_string_lossy().into();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();
        execute(fixture.args.clone()).unwrap();
        assert_eq!(read_factor_bank(&fixture.args), first);

        let last: chrono::DateTime<Utc> =
            serde_json::from_value(fixture.materialization["last_event_time"].clone()).unwrap();
        let holdout_start = last - ChronoDuration::seconds(29);
        rewrite_features(&mut fixture, |row| {
            if row.event_time >= holdout_start {
                row.label = -row.label;
            }
        });
        rebind_mission_inputs(&mut fixture);
        fixture.args.work_dir = fixture.root.join("work-3");
        fixture.args.result_put_url = fixture.root.join("result-3.zip").to_string_lossy().into();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();
        execute(fixture.args.clone()).unwrap();
        assert_eq!(read_factor_bank(&fixture.args), first);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_preserves_prior_holdout_evidence_in_reused_work_dir() {
        let fixture = fixture("preserve-prior-holdout");
        let results = fixture.args.work_dir.join("results");
        let sealed = results.join("sealed-evaluations.jsonl");
        let receipt = results.join("mcts-research-receipt.json");
        std::fs::create_dir_all(&results).unwrap();
        std::fs::write(&sealed, b"sealed-evidence\n").unwrap();
        std::fs::write(&receipt, b"sealed-receipt\n").unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("fresh work directory"));
        assert_eq!(std::fs::read(&sealed).unwrap(), b"sealed-evidence\n");
        assert_eq!(std::fs::read(&receipt).unwrap(), b"sealed-receipt\n");
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn baseline_execution_model_discloses_zero_slippage_and_unmodeled_microstructure() {
        let fixture = fixture("baseline-execution-model");
        let protocol = &fixture.mission.spec.evaluation_protocol;
        let evidence = serde_json::to_value(ExecutionModelEvidence::from(&protocol.costs)).unwrap();

        assert_eq!(evidence["fee_bps"], 2.0);
        assert_eq!(evidence["rebate_bps"], 0.0);
        assert_eq!(evidence["latency_bps"], 0.5);
        assert_eq!(evidence["additional_slippage_bps"], 0.0);
        assert_eq!(evidence["cross_spread"], false);
        assert_eq!(evidence["queue_position_modeled"], false);
        assert_eq!(evidence["partial_fills_modeled"], false);
        assert_eq!(evidence["market_impact_modeled"], false);
        assert_eq!(evidence["capacity_modeled"], false);
        assert_eq!(evidence["capacity_gate_enabled"], false);
        assert_eq!(evidence["capacity_gate_model"], "disabled");
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn incomplete_capacity_inputs_fail_before_execution_evidence_is_written() {
        let mut fixture = fixture("incomplete-capacity-evidence");
        fixture
            .mission
            .spec
            .evaluation_protocol
            .costs
            .position_notional_usd = 10_000.0;
        write_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error.to_string().contains("evaluation protocol is invalid"));
        assert!(!fixture
            .args
            .work_dir
            .join("results/execution-model.json")
            .exists());
        assert!(!fixture.args.work_dir.join("results/alpha.duckdb").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn create_bundle_rejects_a_stale_symlink() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "alpha-bundle-symlink-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let results_dir = root.join("results");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(results_dir.join("summary.json"), "{}\n").unwrap();
        let protected_target = root.join("protected-target");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        let bundle = root.join("checkpoint.zip");
        symlink(&protected_target, &bundle).unwrap();

        let error = create_bundle(&root, &bundle, [&results_dir])
            .expect_err("a symlinked bundle path must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(protected_target).unwrap(),
            "preserve\n"
        );
        let metadata = std::fs::symlink_metadata(&bundle).unwrap();
        assert!(metadata.file_type().is_symlink());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publish_result_does_not_leave_a_destination_when_the_bundle_is_missing() {
        let root = tempfile::tempdir().expect("create result publication test root");
        let destination = root.path().join("result.zip");
        let client = Client::builder().build().unwrap();

        publish_result(
            &client,
            &destination.to_string_lossy(),
            &root.path().join("missing-bundle.zip"),
        )
        .expect_err("a missing bundle must fail before publication");

        assert!(!destination.exists());
    }

    #[test]
    fn execute_rejects_a_result_readback_for_another_object() {
        let mut fixture = fixture("result-readback-object-mismatch");
        let readback = fixture.root.join("another-result.zip");
        fixture.args.result_readback_url = readback.to_string_lossy().into_owned();

        let error =
            execute(fixture.args.clone()).expect_err("readback of another object must fail closed");

        assert!(error
            .to_string()
            .contains("CEX result readback URL must identify the same immutable result object"));
        assert!(!fixture.result_path.exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn cex_result_readback_accepts_distinct_signatures_for_the_same_object() {
        validate_result_readback_binding(
            "https://oss-internal/results/attempt-1/results.zip?upload=x",
            "https://oss-internal/results/attempt-1/results.zip?read=x",
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn publish_result_rejects_a_symlinked_parent_directory() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "alpha-result-parent-symlink-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let bundle = root.join("bundle.zip");
        std::fs::write(&bundle, "bundle").unwrap();
        let protected_directory = root.join("protected-directory");
        std::fs::create_dir(&protected_directory).unwrap();
        let linked_parent = root.join("linked-parent");
        symlink(&protected_directory, &linked_parent).unwrap();
        let client = Client::builder().build().unwrap();

        let error = publish_result(
            &client,
            &linked_parent.join("result.zip").to_string_lossy(),
            &bundle,
        )
        .expect_err("a symlinked result parent must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert!(std::fs::read_dir(protected_directory)
            .unwrap()
            .next()
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct Fixture {
        root: PathBuf,
        feature_path: PathBuf,
        mission_path: PathBuf,
        materialization_path: PathBuf,
        replay_artifact_path: PathBuf,
        replay_manifest_path: PathBuf,
        result_path: PathBuf,
        mission: CexResearchMissionArtifactV1,
        materialization: serde_json::Value,
        args: ExecuteMissionArgs,
    }

    fn write_replay_fixture(
        root: &Path,
        source_content_sha256: &str,
        source_revision: &str,
        source_start_ns: u64,
        source_end_ns: u64,
        first_feature_time: chrono::DateTime<Utc>,
    ) -> (PathBuf, String, PathBuf, String) {
        const MESSAGE: &str = "
message binance_replay {
  REQUIRED INT64 timestamp_us;
  REQUIRED INT64 sequence;
  REQUIRED BINARY event (UTF8);
  REQUIRED BINARY payload_json (UTF8);
}
";
        let mut timestamps = Vec::with_capacity(160);
        let mut sequences = Vec::with_capacity(160);
        let mut events = Vec::with_capacity(160);
        let mut payloads = Vec::with_capacity(160);
        let levels = serde_json::json!({
            "bids": [
                ["59999", "10"], ["59998", "10"], ["59997", "10"],
                ["59996", "10"], ["59995", "10"]
            ],
            "asks": [
                ["60001", "10"], ["60002", "10"], ["60003", "10"],
                ["60004", "10"], ["60005", "10"]
            ]
        });
        for index in 0..160_i64 {
            timestamps.push(if index == 0 {
                i64::try_from(
                    source_start_ns / 1_000 + u64::from(!source_start_ns.is_multiple_of(1_000)),
                )
                .unwrap()
            } else {
                (first_feature_time + ChronoDuration::seconds(index - 1)).timestamp_micros()
            });
            sequences.push(index + 1);
            events.push(if index == 0 {
                "snapshot".to_string()
            } else {
                "l2_update".to_string()
            });
            payloads.push(serde_json::to_string(&levels).unwrap());
        }

        let temporary_artifact = root.join("replay.parquet");
        let schema = Arc::new(parse_message_type(MESSAGE).unwrap());
        let properties = Arc::new(WriterProperties::builder().build());
        let mut writer = SerializedFileWriter::new(
            File::create(&temporary_artifact).unwrap(),
            schema,
            properties,
        )
        .unwrap();
        let mut group = writer.next_row_group().unwrap();
        write_replay_i64_column(&mut group, &timestamps);
        write_replay_i64_column(&mut group, &sequences);
        write_replay_utf8_column(&mut group, &events);
        write_replay_utf8_column(&mut group, &payloads);
        group.close().unwrap();
        writer.close().unwrap();
        let replay_artifact_sha256 = sha256_file(&temporary_artifact).unwrap();
        let replay_artifact_path = root.join(format!("{replay_artifact_sha256}.parquet"));
        std::fs::rename(&temporary_artifact, &replay_artifact_path).unwrap();
        let replay_manifest = serde_json::json!({
            "dataset_kind": "backtest_canonical_replay_parquet",
            "schema_version": "binance-replay-parquet-v1",
            "format": "parquet",
            "parquet_schema": "timestamp_us:int64,sequence:int64,event:utf8,payload_json:utf8",
            "mission_id": "data-1",
            "market": "usdm",
            "symbol": "BTCUSDT",
            "dataset": "binance_usdm_lob",
            "modalities": ["lob"],
            "source_revision": source_revision,
            "source_segments": [{
                "file": "segment.jsonl.zst",
                "sha256": source_content_sha256,
                "collector_manifest_sha256": "2".repeat(64),
                "success_marker_sha256": hex::encode(Sha256::digest(format!("{source_content_sha256}\n"))),
                "start_received_at_ns": source_start_ns,
                "end_received_at_ns": source_end_ns,
                "events": 160
            }],
            "rows": 160,
            "first_event_time_us": timestamps[0],
            "last_event_time_us": timestamps[159],
            "sequence_start": 1,
            "sequence_end": 160,
            "artifact_path": replay_artifact_path.file_name().unwrap().to_str().unwrap(),
            "artifact_sha256": &replay_artifact_sha256,
            "point_in_time": true
        });
        let replay_manifest_path = root.join("replay-manifest.json");
        let replay_manifest_bytes = serde_json::to_vec_pretty(&replay_manifest).unwrap();
        std::fs::write(&replay_manifest_path, &replay_manifest_bytes).unwrap();
        let replay_manifest_sha256 = hex::encode(Sha256::digest(&replay_manifest_bytes));
        (
            replay_artifact_path,
            replay_artifact_sha256,
            replay_manifest_path,
            replay_manifest_sha256,
        )
    }

    fn write_replay_i64_column(group: &mut SerializedRowGroupWriter<'_, File>, values: &[i64]) {
        let mut column = group.next_column().unwrap().unwrap();
        column
            .typed::<Int64Type>()
            .write_batch(values, None, None)
            .unwrap();
        column.close().unwrap();
    }

    fn write_replay_utf8_column(group: &mut SerializedRowGroupWriter<'_, File>, values: &[String]) {
        let values = values
            .iter()
            .map(|value| ByteArray::from(value.as_str()))
            .collect::<Vec<_>>();
        let mut column = group.next_column().unwrap().unwrap();
        column
            .typed::<ByteArrayType>()
            .write_batch(&values, None, None)
            .unwrap();
        column.close().unwrap();
    }

    fn fixture(name: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!(
            "alpha-mission-runner-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let feature_path = root.join("features.jsonl");
        let ingestion_time = Utc::now() - ChronoDuration::seconds(1);
        let source_content_sha256 = "1".repeat(64);
        let source_revision =
            hft_collector::lob_archiver::source_revision([source_content_sha256.as_str()]);
        let rows = (0..160)
            .map(|index| {
                let event_time = ingestion_time - ChronoDuration::seconds(300 - index);
                PointInTimeFeatureRow {
                    event_time,
                    feature_available_time: event_time,
                    label_available_time: event_time + ChronoDuration::seconds(5),
                    ingestion_time,
                    symbol: "BTCUSDT".to_string(),
                    source_revisions: BTreeMap::from([(
                        "binance-usdm-lob".to_string(),
                        source_revision.clone(),
                    )]),
                    modalities: BTreeSet::from([
                        DataModality::Lob,
                        DataModality::TradeTick,
                        DataModality::Funding,
                        DataModality::OpenInterest,
                    ]),
                    features: BTreeMap::from([
                        ("ask_depth_top5".to_string(), 10.0),
                        ("bid_depth_top5".to_string(), 10.0),
                        ("book_imbalance".to_string(), index as f64 / 100.0),
                        ("mid_price".to_string(), 60_000.0),
                        ("funding_rate".to_string(), 0.0001),
                        ("open_interest".to_string(), 12_345.5),
                        ("spread_bps".to_string(), (index as f64 / 10.0).sin().abs()),
                    ]),
                    label: if index % 2 == 0 { 0.001 } else { -0.0005 },
                }
            })
            .collect::<Vec<_>>();
        let mut feature_bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut feature_bytes, &row).unwrap();
            feature_bytes.push(b'\n');
        }
        std::fs::write(&feature_path, &feature_bytes).unwrap();
        let feature_sha256 = hex::encode(Sha256::digest(&feature_bytes));
        let first_event_time = ingestion_time - ChronoDuration::seconds(300);
        let last_event_time = ingestion_time - ChronoDuration::seconds(141);
        let source_start_ns = u64::try_from(
            (first_event_time - ChronoDuration::seconds(1))
                .timestamp_nanos_opt()
                .unwrap(),
        )
        .unwrap();
        let source_end_ns = u64::try_from(ingestion_time.timestamp_nanos_opt().unwrap()).unwrap();
        let snapshot = CexReplaySnapshotV2 {
            schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V2.to_string(),
            venue: "binance".to_string(),
            instrument_type: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
                hft_research_manifest::CEX_MODALITY_FUNDING.to_string(),
                hft_research_manifest::CEX_MODALITY_OPEN_INTEREST.to_string(),
            ]),
            source_segments: vec![hft_research_manifest::CexReplaySegmentIdentity {
                content_sha256: source_content_sha256.clone(),
                manifest_sha256: "2".repeat(64),
                start_received_at_ns: source_start_ns,
                end_received_at_ns: source_end_ns,
                events: 160,
            }],
            first_event_time,
            last_event_time,
            feature_artifact_sha256: feature_sha256.clone(),
            feature_availability_policy: hft_research_manifest::CEX_FEATURE_AVAILABILITY_POLICY
                .to_string(),
            bucket_ms: 1_000,
            label_horizon_buckets: 5,
            top_depth: 5,
            instrument_rules: hft_research_manifest::CexInstrumentRulesV2 {
                tick_size: "0.1".to_string(),
                step_size: "0.001".to_string(),
                min_notional: "5".to_string(),
                available_at: first_event_time - ChronoDuration::seconds(1),
                valid_through: last_event_time + ChronoDuration::seconds(5),
                evidence: vec![cex_triplet('4')],
            },
            fee_schedule: hft_research_manifest::CexFeeScheduleV2 {
                runtime_account_id: "binance-main".to_string(),
                account_fingerprint: "9".repeat(64),
                maker_buy_fee_bps: "2".to_string(),
                maker_sell_fee_bps: "2".to_string(),
                taker_buy_fee_bps: "5".to_string(),
                taker_sell_fee_bps: "5".to_string(),
                available_at: first_event_time - ChronoDuration::seconds(1),
                valid_through: last_event_time + ChronoDuration::seconds(5),
                evidence: vec![cex_triplet('5')],
            },
            derivatives_reference: Some(hft_research_manifest::CexDerivativesReferenceV2 {
                funding: hft_research_manifest::CexPitSeriesEvidenceV2 {
                    evidence: vec![cex_triplet('6'), cex_triplet('a'), cex_triplet('b')],
                    first_available_at: first_event_time - ChronoDuration::seconds(1),
                    last_available_at: last_event_time + ChronoDuration::seconds(5),
                    observations: 3,
                    max_gap_ns: 90_000_000_000,
                },
                open_interest: hft_research_manifest::CexPitSeriesEvidenceV2 {
                    evidence: vec![cex_triplet('7'), cex_triplet('c'), cex_triplet('d')],
                    first_available_at: first_event_time - ChronoDuration::seconds(1),
                    last_available_at: last_event_time + ChronoDuration::seconds(5),
                    observations: 3,
                    max_gap_ns: 90_000_000_000,
                },
                evaluation_funding_bps_per_bucket: "0".to_string(),
            }),
            latency_cost: hft_research_manifest::CexLatencyCostV2 {
                method: "verified_order_lifecycle_realized_slippage".to_string(),
                venue: "binance".to_string(),
                symbol: "BTCUSDT".to_string(),
                runtime_account_id: "binance-main".to_string(),
                account_fingerprint: "9".repeat(64),
                evidence: cex_triplet('8'),
                first_observed_at: first_event_time - ChronoDuration::seconds(2),
                last_observed_at: first_event_time - ChronoDuration::seconds(1),
                available_at: first_event_time - ChronoDuration::seconds(1),
                observations: 160,
                p50_ns: 1_000_000,
                p95_ns: 2_000_000,
                p99_ns: 3_000_000,
                p50_cost_bps: "0.1".to_string(),
                p95_cost_bps: "0.5".to_string(),
                p99_cost_bps: "0.6".to_string(),
            },
        };
        let snapshot_sha256 = snapshot.sha256();
        let materialization = serde_json::json!({
            "dataset_kind": MATERIALIZATION_KIND,
            "schema_version": BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3,
            "mission_id": "data-1",
            "symbol": "BTCUSDT",
            "market": "usdm",
            "bucket_ms": 1000,
            "label_horizon_buckets": 5,
            "top_depth": 5,
            "source_revision": source_revision,
            "source_segments": [{
                "path": "segment.jsonl.zst",
                "sha256": source_content_sha256,
                "collector_manifest_path": "segment.jsonl.zst.manifest.json",
                "collector_manifest_sha256": "2".repeat(64),
                "success_marker_path": "segment.jsonl.zst._SUCCESS",
                "success_marker_sha256": hex::encode(Sha256::digest(format!("{source_content_sha256}\n"))),
                "start_received_at_ns": source_start_ns,
                "end_received_at_ns": source_end_ns,
                "events": 160
            }],
            "rows": 160,
            "first_event_time": first_event_time,
            "last_event_time": last_event_time,
            "artifact_path": "features.jsonl",
            "artifact_sha256": feature_sha256,
            "snapshot": snapshot,
            "snapshot_sha256": snapshot_sha256,
            "created_at": ingestion_time
        });
        let materialization_path = root.join("materialization.json");
        std::fs::write(
            &materialization_path,
            serde_json::to_vec_pretty(&materialization).unwrap(),
        )
        .unwrap();
        let (
            replay_artifact_path,
            replay_artifact_sha256,
            replay_manifest_path,
            replay_manifest_sha256,
        ) = write_replay_fixture(
            &root,
            &source_content_sha256,
            &source_revision,
            source_start_ns,
            source_end_ns,
            first_event_time,
        );
        let validation = ValidationArgs {
            initial_train_rows: 40,
            validation_rows: 30,
            fold_count: 2,
            purge_rows: 5,
            embargo_rows: 1,
            sealed_holdout_rows: 30,
            fee_bps: 2.0,
            rebate_bps: 0.0,
            funding_bps: 0.0,
            latency_bps: 0.5,
            slippage_bps: 0.0,
            cross_spread: false,
            position_notional_usd: 0.0,
            capacity_depth_levels: 0,
            max_book_depth_fraction: 0.0,
            label_horizon_buckets: 5,
            observation_frequency_millis: 1_000,
        };
        let evaluation_protocol = validation
            .evaluation_protocol(&EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            })
            .unwrap();
        let search = CexResearchSearchPlanV1 {
            seed: 7,
            budget: SearchBudget {
                max_candidates: 1,
                max_expansions: 1,
                max_tokens: 0,
                max_seconds: 0,
            },
            max_new_iterations: 1,
        };
        let feature_fields = vec!["book_imbalance".to_string(), "spread_bps".to_string()];
        let gp_policy = CexGpPolicyV1::controlled_v1(
            "gp-policy-1",
            feature_fields.clone(),
            search.seed,
            &search.budget,
        )
        .unwrap();
        let baseline_policy = CexBaselinePolicyV1::controlled_v1("baseline-policy-1").unwrap();
        let weight_policy =
            CexEqualAbsoluteWeightPolicyV1::controlled_v1("weight-policy-1").unwrap();
        let replay_policy = CexEventReplayPolicyV1::controlled_v1(
            "replay-policy-1",
            5,
            evaluation_protocol.labels.observation_frequency_millis,
        )
        .unwrap();
        let reference = |id: &str, byte: char| CexResearchContentRefV1 {
            id: id.to_string(),
            content_sha256: byte.to_string().repeat(64),
        };
        let materialization_sha256 = sha256_file(&materialization_path).unwrap();
        let partition_sha256 = canonical_json_hash(&snapshot.source_segments).unwrap();
        let dataset =
            CexReplayDatasetManifestV2::new(format!("dataset-{feature_sha256}"), snapshot.clone())
                .unwrap();
        let mut mission = CexResearchMissionArtifactV1 {
            schema_version: CEX_RESEARCH_MISSION_SCHEMA_V1.to_string(),
            spec: CexResearchMissionSpecV1 {
                objective: "test objective".to_string(),
                search_lineage_id: "search-lineage-1".to_string(),
                data_mission_id: "data-1".to_string(),
                instrument: CexResearchInstrumentV1 {
                    venue: CexResearchVenueV1::Binance,
                    market: CexResearchMarketV1::Usdm,
                    symbol: "BTCUSDT".to_string(),
                    horizon: evaluation_protocol.labels.clone(),
                },
                hypotheses: vec![CexResearchHypothesisV1 {
                    hypothesis_id: "hypothesis-1".to_string(),
                    statement: "LOB imbalance predicts the next five buckets".to_string(),
                    target: CexResearchHypothesisTargetV1 {
                        name: "forward_mid_return".to_string(),
                        horizon: evaluation_protocol.labels.clone(),
                    },
                    required_feature_families: vec!["book_imbalance".to_string()],
                    required_template_families: vec!["signed_rolling_imbalance".to_string()],
                    falsification_tests: vec![CexResearchFalsificationTestV1 {
                        test_id: "rank-ic-positive".to_string(),
                        reject_when: "purged validation rank IC is non-positive".to_string(),
                    }],
                    source_evidence_ids: vec!["evidence-training-1".to_string()],
                }],
                inputs: CexResearchInputBindingsV1 {
                    dataset: CexResearchContentRefV1 {
                        id: dataset.manifest_id.clone(),
                        content_sha256: canonical_json_hash(&dataset).unwrap(),
                    },
                    snapshot: CexResearchContentRefV1 {
                        id: format!("cex-replay-snapshot-{snapshot_sha256}"),
                        content_sha256: snapshot_sha256.clone(),
                    },
                    partition: CexResearchContentRefV1 {
                        id: format!("cex-replay-partition-{partition_sha256}"),
                        content_sha256: partition_sha256,
                    },
                    source: CexResearchContentRefV1 {
                        id: source_revision.clone(),
                        content_sha256: source_revision.clone(),
                    },
                    feature: CexResearchContentRefV1 {
                        id: format!("dataset-{feature_sha256}"),
                        content_sha256: feature_sha256.clone(),
                    },
                    materialization: CexResearchContentRefV1 {
                        id: "data-1".to_string(),
                        content_sha256: materialization_sha256,
                    },
                },
                policies: CexResearchPolicyBindingsV1 {
                    gp: CexResearchContentRefV1 {
                        id: gp_policy.policy_id.clone(),
                        content_sha256: gp_policy.content_hash().unwrap(),
                    },
                    screening: reference("screening-policy-1", '2'),
                    baseline: CexResearchContentRefV1 {
                        id: baseline_policy.policy_id.clone(),
                        content_sha256: baseline_policy.content_hash().unwrap(),
                    },
                    subset_search: CexResearchContentRefV1 {
                        id: "subset-search-policy-1".to_string(),
                        content_sha256: canonical_json_hash(&search).unwrap(),
                    },
                    weight: CexResearchContentRefV1 {
                        id: weight_policy.policy_id.clone(),
                        content_sha256: weight_policy.content_hash().unwrap(),
                    },
                    evaluation: CexResearchContentRefV1 {
                        id: "evaluation-policy-1".to_string(),
                        content_sha256: evaluation_protocol.content_hash().unwrap(),
                    },
                    replay: CexResearchContentRefV1 {
                        id: replay_policy.policy_id.clone(),
                        content_sha256: replay_policy.content_hash().unwrap(),
                    },
                    holdout: reference("holdout-policy-1", '6'),
                },
                evidence: vec![CexResearchEvidenceRefV1 {
                    evidence_id: "evidence-training-1".to_string(),
                    kind: CexResearchEvidenceKindV1::TrainingValidation,
                    source_mission_id: "earlier-mission-1".to_string(),
                    source_search_lineage_id: "earlier-search-lineage-1".to_string(),
                    artifact_sha256: "7".repeat(64),
                    signature: None,
                    holdout_id: None,
                }],
                feature_fields,
                search,
                evaluation_protocol,
                holdout: CexResearchHoldoutV1 {
                    holdout_id: "holdout-fresh-1".to_string(),
                    state: CexResearchHoldoutStateV1::Unopened,
                },
            },
            operational: CexResearchOperationalMetadataV1 {
                submitted_at: Some(Utc::now()),
            },
        };
        mission.spec.policies.screening.content_sha256 = canonical_json_hash(
            &FormulaEvaluatorConfig::for_trials(
                mission.spec.search.planned_gp_and_subset_trials().unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        mission.validate().unwrap();
        let mission_path = root.join("mission.json");
        std::fs::write(&mission_path, serde_json::to_vec_pretty(&mission).unwrap()).unwrap();
        let result_path = root.join("result.zip");
        let args = ExecuteMissionArgs {
            work_dir: root.join("work-1"),
            mission_url: mission_path.to_string_lossy().into_owned(),
            mission_sha256: sha256_file(&mission_path).unwrap(),
            feature_url: feature_path.to_string_lossy().into_owned(),
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            replay_artifact_url: replay_artifact_path.to_string_lossy().into_owned(),
            replay_artifact_sha256,
            replay_manifest_url: replay_manifest_path.to_string_lossy().into_owned(),
            replay_manifest_sha256,
            implementation_source_revision: "a".repeat(40),
            image_identity: format!("sha256:{}", "b".repeat(64)),
            resume_url: None,
            resume_sha256: None,
            result_put_url: result_path.to_string_lossy().into_owned(),
            result_readback_url: result_path.to_string_lossy().into_owned(),
        };
        Fixture {
            root,
            feature_path,
            mission_path,
            materialization_path,
            replay_artifact_path,
            replay_manifest_path,
            result_path,
            mission,
            materialization,
            args,
        }
    }

    #[test]
    fn historical_v2_materialization_decodes_read_only() {
        let fixture = fixture("historical-v2-read-only");
        let mut value = fixture.materialization.clone();
        value["schema_version"] = serde_json::json!(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2);
        let snapshot = value["snapshot"].as_object_mut().unwrap();
        snapshot.insert(
            "schema_version".to_string(),
            serde_json::json!(hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V1),
        );
        snapshot.insert(
            "required_modalities".to_string(),
            serde_json::json!([
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE,
                hft_research_manifest::CEX_MODALITY_LOB
            ]),
        );
        for field in [
            "instrument_rules",
            "fee_schedule",
            "derivatives_reference",
            "latency_cost",
        ] {
            snapshot.remove(field);
        }

        let error = decode_materialization(&serde_json::to_vec(&value).unwrap()).unwrap_err();

        assert!(error
            .to_string()
            .contains("historical V2 materialization is read-only"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    fn rewrite_features(fixture: &mut Fixture, mutate: impl Fn(&mut PointInTimeFeatureRow)) {
        let bytes = std::fs::read(&fixture.feature_path).unwrap();
        let mut rows = std::io::BufReader::new(bytes.as_slice())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        rows.iter_mut().for_each(mutate);
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, &row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&fixture.feature_path, &bytes).unwrap();
        let sha256 = hex::encode(Sha256::digest(&bytes));
        fixture.materialization["artifact_sha256"] = serde_json::json!(&sha256);
        fixture.materialization["snapshot"]["feature_artifact_sha256"] = serde_json::json!(&sha256);
        resign_materialization(fixture);
    }

    fn read_factor_bank(args: &ExecuteMissionArgs) -> serde_json::Value {
        let file = File::open(args.work_dir.join("results/factor-bank.json")).unwrap();
        serde_json::from_reader(file).unwrap()
    }

    fn write_mission(fixture: &mut Fixture) {
        std::fs::write(
            &fixture.mission_path,
            serde_json::to_vec_pretty(&fixture.mission).unwrap(),
        )
        .unwrap();
        fixture.args.mission_sha256 = sha256_file(&fixture.mission_path).unwrap();
    }

    fn resign_mission(fixture: &mut Fixture) {
        let gp_policy = CexGpPolicyV1::controlled_v1(
            fixture.mission.spec.policies.gp.id.clone(),
            fixture.mission.spec.feature_fields.clone(),
            fixture.mission.spec.search.seed,
            &fixture.mission.spec.search.budget,
        );
        if let Ok(gp_policy) = gp_policy {
            fixture.mission.spec.policies.gp.content_sha256 = gp_policy.content_hash().unwrap();
        }
        if let Ok(baseline_policy) =
            CexBaselinePolicyV1::controlled_v1(fixture.mission.spec.policies.baseline.id.clone())
        {
            fixture.mission.spec.policies.baseline.content_sha256 =
                baseline_policy.content_hash().unwrap();
        }
        if let Ok(weight_policy) = CexEqualAbsoluteWeightPolicyV1::controlled_v1(
            fixture.mission.spec.policies.weight.id.clone(),
        ) {
            fixture.mission.spec.policies.weight.content_sha256 =
                weight_policy.content_hash().unwrap();
        }
        if let Ok(replay_policy) = CexEventReplayPolicyV1::controlled_v1(
            fixture.mission.spec.policies.replay.id.clone(),
            fixture.materialization["top_depth"]
                .as_u64()
                .unwrap_or_default() as usize,
            fixture
                .mission
                .spec
                .instrument
                .horizon
                .observation_frequency_millis,
        ) {
            fixture.mission.spec.policies.replay.content_sha256 =
                replay_policy.content_hash().unwrap();
        }
        fixture.mission.spec.policies.subset_search.content_sha256 =
            canonical_json_hash(&fixture.mission.spec.search).unwrap();
        fixture.mission.spec.policies.screening.content_sha256 = canonical_json_hash(
            &FormulaEvaluatorConfig::for_trials(
                fixture
                    .mission
                    .spec
                    .search
                    .planned_gp_and_subset_trials()
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        fixture.mission.spec.policies.evaluation.content_sha256 = fixture
            .mission
            .spec
            .evaluation_protocol
            .content_hash()
            .unwrap();
        write_mission(fixture);
    }

    fn rebind_mission_inputs(fixture: &mut Fixture) {
        let snapshot: CexReplaySnapshotV2 =
            serde_json::from_value(fixture.materialization["snapshot"].clone()).unwrap();
        let snapshot_sha256 = snapshot.sha256();
        let partition_sha256 = canonical_json_hash(&snapshot.source_segments).unwrap();
        let feature_sha256 = sha256_file(&fixture.feature_path).unwrap();
        let source_revision = fixture.materialization["source_revision"]
            .as_str()
            .unwrap()
            .to_string();
        let data_mission_id = fixture.materialization["mission_id"]
            .as_str()
            .unwrap()
            .to_string();
        let dataset =
            CexReplayDatasetManifestV2::new(format!("dataset-{feature_sha256}"), snapshot).unwrap();
        fixture.mission.spec.data_mission_id = data_mission_id.clone();
        fixture.mission.spec.inputs = CexResearchInputBindingsV1 {
            dataset: CexResearchContentRefV1 {
                id: dataset.manifest_id.clone(),
                content_sha256: canonical_json_hash(&dataset).unwrap(),
            },
            snapshot: CexResearchContentRefV1 {
                id: format!("cex-replay-snapshot-{snapshot_sha256}"),
                content_sha256: snapshot_sha256,
            },
            partition: CexResearchContentRefV1 {
                id: format!("cex-replay-partition-{partition_sha256}"),
                content_sha256: partition_sha256,
            },
            source: CexResearchContentRefV1 {
                id: source_revision.clone(),
                content_sha256: source_revision,
            },
            feature: CexResearchContentRefV1 {
                id: format!("dataset-{feature_sha256}"),
                content_sha256: feature_sha256,
            },
            materialization: CexResearchContentRefV1 {
                id: data_mission_id,
                content_sha256: sha256_file(&fixture.materialization_path).unwrap(),
            },
        };
        resign_mission(fixture);
    }

    fn resign_materialization_outer(fixture: &mut Fixture) {
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&fixture.materialization).unwrap(),
        )
        .unwrap();
        fixture.mission.spec.inputs.materialization.content_sha256 =
            sha256_file(&fixture.materialization_path).unwrap();
        write_mission(fixture);
    }

    fn resign_materialization(fixture: &mut Fixture) {
        let snapshot: CexReplaySnapshotV2 =
            serde_json::from_value(fixture.materialization["snapshot"].clone()).unwrap();
        fixture.materialization["snapshot_sha256"] = serde_json::json!(snapshot.sha256());
        std::fs::write(
            &fixture.materialization_path,
            serde_json::to_vec_pretty(&fixture.materialization).unwrap(),
        )
        .unwrap();
        rebind_mission_inputs(fixture);
    }
}
