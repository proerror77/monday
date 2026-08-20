use crate::{
    cli::{
        print_json, DatasetArgs, EngineChoice, ExecuteMissionArgs, RunMissionArgs, ValidationArgs,
        BUILD_SOURCE_REVISION,
    },
    data_mission, mission, prediction_dispatch,
};
use alpha_domain::{
    canonical_json_hash, CandidateArtifact, CandidateEvaluation, CexBaselineArtifactV1,
    CexBaselineGateV1, CexBaselinePolicyV1, CexEqualAbsoluteWeightPolicyV1, CexEventReplayPolicyV1,
    CexFactorBankRevisionV2, CexFactorEvaluationEvidenceV2, CexFactorRejectionCodeV1,
    CexFactorScreeningAttemptV2, CexFactorScreeningVerdictV1, CexFinalPrecommitV1,
    CexFourStageStrategyCandidateV1, CexGpPolicyV1, CexResearchContentRefV1,
    CexResearchHoldoutStateV1, CexResearchMissionArtifactV1, CexSealedHoldoutClaimV1, EngineKind,
    EvaluationCostsV1, FormulaEvaluatorConfig, IterationVerdict, MissionCompletionPolicy,
    MissionStatus, PromotionRecord, ResearchIteration, ResearchMission, SearchBudgetUsage,
    StrategyBundle, ValidatorMode, CEX_FINAL_PRECOMMIT_SCHEMA_V1,
    MAX_CEX_FACTOR_BANK_MCTS_CHECKPOINT_BYTES, SEALED_HOLDOUT_EVALUATOR_VERSION,
};
use alpha_engine::{
    baselines::evaluate_cex_baselines,
    engines::{
        CexCombinationResearchArtifactV1, CexFactorBankMcts, CexFactorBankMctsCheckpointV1,
        CexFactorBankMctsResultV1, CexFactorBankMctsStopReasonV1,
    },
    evaluation::{prepare_dataset, EngineContext},
    formula_evaluator::FormulaEvaluator,
    CandidateEvaluator, EngineProposal,
};
use alpha_store::{AlphaStore, EvaluationRecord, MissionLineage, RegistryRevision, StoreError};
use anyhow::{bail, Context};
use chrono::Utc;
use hft_backtest::{
    config::{
        verify_and_replay_canonical_target_positions, CanonicalReplayEvidence,
        CanonicalSourceSegmentEvidence,
    },
    engine::{
        TargetPositionDecision, TargetPositionReplayConfig, TargetPositionReplayMetrics,
        TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION,
    },
};
use hft_research_manifest::{
    CexInstrumentRulesV2, CexReplayDatasetManifestV5, CexReplaySnapshotV1, CexReplaySnapshotV2,
    CexReplaySnapshotV3, CexReplaySnapshotV4, CexReplaySnapshotV5, ManifestId,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V2, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V4, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5,
    BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V6, BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7,
};
use reqwest::{
    blocking::Client,
    header::{HeaderValue, CONTENT_TYPE},
    redirect::Policy,
    StatusCode,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

const MATERIALIZATION_KIND: &str = "lob_point_in_time_materialization";
const CEX_BASELINE_POLICY_REGISTRY_KIND: &str = "cex_baseline_policy";
const CEX_BASELINE_RIDGE_REGISTRY_KIND: &str = "cex_baseline_ridge";
const CEX_BASELINE_CART_REGISTRY_KIND: &str = "cex_baseline_cart";
const CEX_BASELINE_GATE_REGISTRY_KIND: &str = "cex_baseline_gate";
const CEX_EVENT_REPLAY_RECEIPT_REGISTRY_KIND: &str = "cex_event_replay_receipt";
const MAX_MISSION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CEX_SEALED_HOLDOUT_CLAIM_BYTES: u64 = 64 * 1024;
// ponytail: one Mission is capped at 1 GiB; raise this only when staged partitions exceed it.
pub(crate) const MAX_FEATURE_BYTES: u64 = 1024 * 1024 * 1024;
pub(crate) const MAX_MATERIALIZATION_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REPLAY_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_REPLAY_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_RESULT_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
const MCTS_CHECKPOINT_ARTIFACT_SCHEMA_VERSION: &str =
    "cex-factor-bank-subset-mcts-checkpoint-artifact-v1";
const CEX_EVENT_REPLAY_RECEIPT_SCHEMA_VERSION: &str = "cex-event-replay-receipt-v1";
// ponytail: fixed batching bounds checkpoint I/O; make it configurable only if recovery data requires it.
const MCTS_CHECKPOINT_INTERVAL: u64 = 256;

fn bound_gp_policy(mission: &CexResearchMissionArtifactV1) -> anyhow::Result<CexGpPolicyV1> {
    let binding = &mission.spec.policies.gp;
    let legacy = CexGpPolicyV1::controlled_v1(
        binding.id.clone(),
        mission.spec.feature_fields.clone(),
        mission.spec.search.seed,
        &mission.spec.search.budget,
    )?;
    if legacy.validate_binding(binding).is_ok() {
        return Ok(legacy);
    }
    let dynamic = CexGpPolicyV1::controlled_dynamic_v2(
        binding.id.clone(),
        mission.spec.feature_fields.clone(),
        mission.spec.search.seed,
        &mission.spec.search.budget,
    )?;
    dynamic.validate_binding(binding)?;
    Ok(dynamic)
}

#[derive(Debug, Deserialize)]
pub(crate) struct Materialization {
    dataset_kind: String,
    schema_version: String,
    pub(crate) mission_id: String,
    pub(crate) symbol: String,
    pub(crate) market: String,
    pub(crate) source_revision: String,
    source_segments: Vec<SourceSegment>,
    pub(crate) artifact_sha256: String,
    pub(crate) bucket_ms: u64,
    pub(crate) label_horizon_buckets: usize,
    pub(crate) top_depth: usize,
    #[serde(default = "default_series_count")]
    pub(crate) series_count: usize,
    pub(crate) rows: usize,
    first_event_time: chrono::DateTime<Utc>,
    last_event_time: chrono::DateTime<Utc>,
    pub(crate) snapshot: CexReplaySnapshotV5,
    snapshot_sha256: String,
}

const fn default_series_count() -> usize {
    1
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

#[derive(Debug, Deserialize)]
struct MissionAdmissionEvidenceV1 {
    mission_id: String,
    mission_artifact_sha256: String,
    campaign_id: Option<String>,
    round_id: Option<String>,
    request_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ExecutionReport {
    pub(crate) mission_id: String,
    pub(crate) mission_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) campaign_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) round_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) request_sha256: Option<String>,
    engine: &'static str,
    pub(crate) bundle_bytes: u64,
    pub(crate) bundle_sha256: String,
    pub(crate) readback_bundle_sha256: String,
    pub(crate) replay_receipt_id: Option<String>,
    pub(crate) replay_gate_passed: Option<bool>,
    pub(crate) final_precommit_id: Option<String>,
    pub(crate) sealed_receipt_id: Option<String>,
    pub(crate) sealed_passed: Option<bool>,
    pub(crate) strategy_bundle_id: Option<String>,
    pub(crate) promotion_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) enum ExecutionBinding {
    Direct,
    Campaign {
        campaign_id: String,
        round_id: String,
        request_sha256: String,
    },
}

impl ExecutionBinding {
    fn is_search_only(&self) -> bool {
        matches!(self, Self::Campaign { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CexFinalizationReportV1 {
    pub(crate) schema_version: String,
    pub(crate) precommit_id: String,
    pub(crate) sealed_receipt_id: String,
    pub(crate) sealed_passed: bool,
    pub(crate) strategy_bundle_id: Option<String>,
    pub(crate) promotion_id: Option<String>,
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
    replay_engine_version: String,
    metrics: TargetPositionReplayMetrics,
    capabilities: CexReplayCapabilitiesV1,
    capabilities_sha256: String,
    gate: CexReplayGateV1,
    holdout_id: String,
    holdout_state: CexResearchHoldoutStateV1,
    deployment_authority: bool,
    order_submission_authority: bool,
}

impl CexEventReplayReceiptV1 {
    fn finalize(mut self) -> anyhow::Result<Self> {
        self.capabilities_sha256 = canonical_json_hash(&self.capabilities)?;
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
            || self.implementation_source_revision != BUILD_SOURCE_REVISION
            || !valid_git_revision(BUILD_SOURCE_REVISION)
            || self.replay_engine_version != TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION
            || self.metrics.decision_count == 0
            || self.capabilities_sha256 != canonical_json_hash(&self.capabilities)?
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
    latency_cost_basis: &'static str,
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
            schema_version: "execution_cost_model_v3",
            fee_bps: costs.fee_bps,
            rebate_bps: costs.rebate_bps,
            funding_bps: costs.funding_bps,
            latency_bps: costs.latency_bps,
            latency_cost_basis: "mission_declared_assumption",
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
    print_json(&execute_report(args, ExecutionBinding::Direct)?)
}

pub(crate) fn execute_report(
    args: ExecuteMissionArgs,
    binding: ExecutionBinding,
) -> anyhow::Result<ExecutionReport> {
    validate_args(&args, &binding)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(Policy::none())
        .build()?;
    if !binding.is_search_only() {
        ensure_holdout_claim_absent(&client, &args.holdout_claim_readback_url)?;
    }
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
    let gp_policy = bound_gp_policy(&control_mission)?;
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
    if mission_id != args.mission_id {
        bail!("CEX Research Mission semantic ID does not match the requested Mission ID");
    }
    if control_mission.spec.holdout.holdout_id != args.holdout_id {
        bail!("CEX Research Mission holdout ID does not match the requested holdout ID");
    }
    validate_holdout_claim_binding(&args, &control_mission.spec.holdout.holdout_id, &binding)?;
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
        || feature_manifest.series_count != materialization.series_count
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
    let (campaign_id, round_id) = match &binding {
        ExecutionBinding::Direct => (None, None),
        ExecutionBinding::Campaign {
            campaign_id,
            round_id,
            ..
        } => (Some(campaign_id.clone()), Some(round_id.clone())),
    };
    let request_sha256 = match &binding {
        ExecutionBinding::Direct => None,
        ExecutionBinding::Campaign { request_sha256, .. } => Some(request_sha256.clone()),
    };
    data_mission::write_json_atomic(
        &results_dir.join("mission-admission.json"),
        &serde_json::json!({
            "schema_version": &control_mission.schema_version,
            "mission_id": &mission_id,
            "mission_artifact_sha256": &mission_sha256,
            "dataset_manifest_id": &dataset_manifest.manifest_id,
            "campaign_id": campaign_id.clone(),
            "round_id": round_id.clone(),
            "request_sha256": request_sha256.clone(),
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
                .multiple_testing_trials
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
    let feature_decision_clocks = data_mission::feature_decision_clocks(&feature_manifest)?;
    if feature_decision_clocks.len() != baseline_rows.len() {
        bail!("CEX feature availability clock does not match the admitted dataset");
    }
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
    let subset_run = if factor_bank.entries.is_empty() {
        if resume_checkpoint.is_some() {
            bail!("MCTS resume checkpoint requires a non-empty Factor Bank");
        }
        None
    } else {
        run_factor_bank_subset_search(
            &results_dir,
            &control_mission,
            &factor_bank,
            &baseline_run,
            &baseline_context,
            resume_checkpoint,
        )?
    };
    let replay_report = subset_run
        .as_ref()
        .map(|(strategy, _, _)| {
            run_cex_event_replay(
                &results_dir,
                &mission_id,
                &control_mission,
                &materialization,
                &materialization_sha256,
                &factor_bank,
                &baseline_context,
                &feature_decision_clocks[..baseline_context.rows().len()],
                strategy,
                &replay_policy,
                &replay_artifact_path,
                &replay_artifact_sha256,
                &replay_manifest_path,
                &args.replay_manifest_sha256,
            )
        })
        .transpose()?;
    if let Some(receipt) = &replay_report {
        store.put_registry_revision(&RegistryRevision {
            revision_id: receipt.receipt_id.clone(),
            registry_kind: CEX_EVENT_REPLAY_RECEIPT_REGISTRY_KIND.to_string(),
            asset_id: mission_id.clone(),
            parent_revision_id: Some(receipt.strategy.id.clone()),
            payload: serde_json::to_value(receipt)?,
            created_at: Utc::now(),
        })?;
    }
    let finalization = match (&subset_run, &replay_report) {
        (Some((strategy, subset_checkpoint, subset_result)), Some(replay))
            if replay.gate.passed && !binding.is_search_only() =>
        {
            Some(finalize_cex_candidate(
                &mut store,
                &results_dir,
                &client,
                &args.holdout_claim_put_url,
                &args.holdout_claim_readback_url,
                &mission_id,
                &control_mission,
                &lineage,
                &factor_bank,
                &baseline_run,
                strategy,
                subset_checkpoint,
                subset_result,
                replay,
                &baseline_dataset,
                &gp_policy,
                &baseline_policy,
                &weight_policy,
                &replay_policy,
                &materialization.snapshot.instrument_rules,
            )?)
        }
        _ => None,
    };
    let lineage = store.mission_lineage(&mission_id)?;
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
    let bundle_bytes = checked_result_bundle_bytes(&bundle)?;
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
    Ok(ExecutionReport {
        mission_id,
        mission_sha256,
        campaign_id,
        round_id,
        request_sha256,
        engine: "gp_then_factor_bank_subset_mcts",
        bundle_bytes,
        bundle_sha256,
        readback_bundle_sha256: readback_sha256,
        replay_receipt_id: replay_report
            .as_ref()
            .map(|receipt| receipt.receipt_id.clone()),
        replay_gate_passed: replay_report.as_ref().map(|receipt| receipt.gate.passed),
        final_precommit_id: finalization
            .as_ref()
            .map(|report| report.precommit_id.clone()),
        sealed_receipt_id: finalization
            .as_ref()
            .map(|report| report.sealed_receipt_id.clone()),
        sealed_passed: finalization.as_ref().map(|report| report.sealed_passed),
        strategy_bundle_id: finalization
            .as_ref()
            .and_then(|report| report.strategy_bundle_id.clone()),
        promotion_id: finalization
            .as_ref()
            .and_then(|report| report.promotion_id.clone()),
    })
}

#[allow(clippy::too_many_arguments)]
fn finalize_cex_candidate(
    store: &mut AlphaStore,
    results_dir: &Path,
    client: &Client,
    holdout_claim_put_url: &str,
    holdout_claim_readback_url: &str,
    mission_id: &str,
    control_mission: &CexResearchMissionArtifactV1,
    lineage: &MissionLineage,
    factor_bank: &CexFactorBankRevisionV2,
    baselines: &alpha_engine::baselines::CexBaselineRun,
    strategy: &CexCombinationResearchArtifactV1,
    subset_checkpoint: &CexFactorBankMctsCheckpointV1,
    subset_result: &CexFactorBankMctsResultV1,
    replay: &CexEventReplayReceiptV1,
    dataset: &alpha_engine::evaluation::PreparedDataset,
    gp_policy: &CexGpPolicyV1,
    baseline_policy: &CexBaselinePolicyV1,
    weight_policy: &CexEqualAbsoluteWeightPolicyV1,
    replay_policy: &CexEventReplayPolicyV1,
    instrument_rules: &CexInstrumentRulesV2,
) -> anyhow::Result<CexFinalizationReportV1> {
    let ridge = baselines
        .ridge
        .as_ref()
        .context("final precommit is missing Ridge baseline evidence")?;
    let cart = baselines
        .cart
        .as_ref()
        .context("final precommit is missing CART baseline evidence")?;
    if !replay.gate.passed {
        bail!("final precommit requires a passing replay gate");
    }
    strategy
        .validate_binding(
            control_mission,
            factor_bank,
            ridge,
            cart,
            &baselines.gate,
            &dataset.engine_context(),
            subset_checkpoint,
            subset_result,
        )
        .map_err(anyhow::Error::msg)?;
    validate_replay_strategy_binding(replay, strategy, control_mission, mission_id)?;

    let precommit_id = format!("cex-final-precommit:{mission_id}");
    let executable_formula = strategy
        .executable_formula(factor_bank)
        .map_err(anyhow::Error::msg)?;
    let strategy_json = serde_json::to_string(strategy)?;
    let candidate = CandidateArtifact::CexFourStage(CexFourStageStrategyCandidateV1::new(
        precommit_id,
        mission_id.to_string(),
        strategy.artifact_id.clone(),
        strategy_json,
        executable_formula,
        control_mission.spec.instrument.venue.clone(),
        control_mission.spec.instrument.market.clone(),
        control_mission.spec.instrument.symbol.clone(),
        instrument_rules.clone(),
        control_mission
            .spec
            .policies
            .evaluation
            .content_sha256
            .clone(),
    )?);
    let candidate_hash = canonical_json_hash(&candidate)?;
    let candidate_id = format!("cex-final-candidate-{candidate_hash}");
    let proposal = EngineProposal {
        candidate_id: candidate_id.clone(),
        hypothesis: format!("frozen four-stage strategy {}", strategy.strategy_id),
        artifact: candidate.clone(),
        expansions: subset_result.expansions_used,
        tokens: 0,
        elapsed_ms: 0,
    };
    let evaluator = FormulaEvaluator::for_mission(&lineage.mission).map_err(anyhow::Error::msg)?;
    let walk_forward = evaluator
        .evaluate(&proposal, &dataset.engine_context())
        .map_err(anyhow::Error::msg)?;
    if walk_forward != strategy.walk_forward_evidence.selected.evaluation {
        bail!("final executable formula drifted from selected combination evidence");
    }
    let evaluation_hash = canonical_json_hash(&walk_forward)?;
    let evaluation_id = format!("cex-final-walk-forward-evaluation-{evaluation_hash}");
    let now = Utc::now();
    let evaluation_record = EvaluationRecord {
        evaluation_id: evaluation_id.clone(),
        mission_id: mission_id.to_string(),
        candidate_id: candidate_id.clone(),
        dataset_manifest_id: lineage.mission.dataset_manifest_id.as_str().to_string(),
        evaluation_protocol_hash: control_mission
            .spec
            .policies
            .evaluation
            .content_sha256
            .clone(),
        payload: serde_json::to_value(&walk_forward)?,
        created_at: now,
    };
    let previous_budget = lineage
        .iterations
        .last()
        .map(|iteration| iteration.budget_usage.clone())
        .unwrap_or_default();
    let parent_candidate_ids = strategy
        .signal
        .factors
        .iter()
        .map(|selected| {
            factor_bank
                .entries
                .iter()
                .find(|entry| entry.factor_id == selected.factor.factor_id)
                .map(|entry| entry.candidate_id.clone())
                .context("selected factor has no candidate lineage")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let iteration = ResearchIteration {
        iteration_id: format!("cex-final-iteration-{candidate_hash}"),
        mission_id: mission_id.to_string(),
        parent_candidate_ids,
        engine: EngineKind::Mcts,
        hypothesis: proposal.hypothesis.clone(),
        candidate_artifact_id: Some(candidate_id.clone()),
        evaluation_artifact_id: Some(evaluation_id.clone()),
        budget_usage: SearchBudgetUsage {
            candidates: previous_budget
                .candidates
                .saturating_add(subset_result.candidates_evaluated),
            expansions: previous_budget
                .expansions
                .saturating_add(subset_result.expansions_used),
            tokens: previous_budget.tokens,
            elapsed_ms: previous_budget.elapsed_ms,
        },
        verdict: IterationVerdict::Keep,
        failure_class: None,
        failure_explanation: None,
        created_at: now,
    };

    let semantic_mission = serde_json::json!({
        "schema_version": &control_mission.schema_version,
        "spec": &control_mission.spec,
    });
    let strategy_reference = content_reference(&strategy.artifact_id, strategy)?;
    let replay_reference = registry_content_reference(&replay.receipt_id, replay)?;
    let combination_hash = canonical_json_hash(&strategy.walk_forward_evidence)?;
    let precommit = CexFinalPrecommitV1 {
        schema_version: CEX_FINAL_PRECOMMIT_SCHEMA_V1.to_string(),
        precommit_id: String::new(),
        mission: CexResearchContentRefV1 {
            id: mission_id.to_string(),
            content_sha256: canonical_json_hash(&semantic_mission)?,
        },
        dataset_manifest_id: lineage.mission.dataset_manifest_id.clone(),
        snapshot: control_mission.spec.inputs.snapshot.clone(),
        dataset: control_mission.spec.inputs.dataset.clone(),
        partition: control_mission.spec.inputs.partition.clone(),
        source: control_mission.spec.inputs.source.clone(),
        factor_bank: strategy.signal.parent.clone(),
        ridge_baseline: strategy.walk_forward_evidence.ridge.source_artifact.clone(),
        cart_baseline: strategy.walk_forward_evidence.cart.source_artifact.clone(),
        baseline_gate: registry_content_reference(&baselines.gate.gate_id, &baselines.gate)?,
        mcts_checkpoint: strategy.subset_checkpoint.clone(),
        mcts_subset: strategy.subset_result.clone(),
        weight_policy: control_mission.spec.policies.weight.clone(),
        four_stage_strategy: strategy_reference,
        combination_evidence: CexResearchContentRefV1 {
            id: format!("cex-combination-walk-forward-evidence-{combination_hash}"),
            content_sha256: combination_hash,
        },
        fixed_weights_sha256: canonical_json_hash(&strategy.signal.factors)?,
        replay_receipt: replay_reference,
        replay_capabilities_sha256: replay.capabilities_sha256.clone(),
        evaluation_protocol: control_mission.spec.policies.evaluation.clone(),
        final_candidate: CexResearchContentRefV1 {
            id: candidate_id.clone(),
            content_sha256: candidate_hash.clone(),
        },
        final_walk_forward_evaluation: CexResearchContentRefV1 {
            id: evaluation_id,
            content_sha256: evaluation_hash,
        },
        holdout_id: control_mission.spec.holdout.holdout_id.clone(),
        holdout_state: control_mission.spec.holdout.state,
        implementation_source_revision: BUILD_SOURCE_REVISION.to_string(),
        configuration_sha256: canonical_json_hash(&serde_json::json!({
            "mission": semantic_mission,
            "gp_policy": gp_policy,
            "baseline_policy": baseline_policy,
            "weight_policy": weight_policy,
            "replay_policy": replay_policy,
        }))?,
        deployment_authority: false,
        order_submission_authority: false,
    }
    .finalize()?;
    store.put_cex_final_precommit(
        &iteration,
        &candidate_id,
        &candidate,
        &evaluation_record,
        &precommit,
    )?;
    data_mission::write_json_atomic(&results_dir.join("final-precommit.json"), &precommit)?;

    let claim = CexSealedHoldoutClaimV1::from_precommit(&precommit)?;
    let claim_path = results_dir.join("sealed-holdout-claim.json");
    data_mission::write_json_atomic(&claim_path, &claim)?;
    let claim_sha256 = sha256_file(&claim_path)?;
    // This create-once write is the at-most-once boundary. If it succeeds but
    // local claim or evaluation fails, the Mission is terminal and inconclusive.
    publish_immutable_file(
        client,
        holdout_claim_put_url,
        &claim_path,
        "application/json",
    )?;
    let claim_readback_path = results_dir.join("sealed-holdout-claim-readback.json");
    let (_, claim_readback_sha256) = fetch_to_file(
        client,
        holdout_claim_readback_url,
        &claim_readback_path,
        MAX_CEX_SEALED_HOLDOUT_CLAIM_BYTES,
    )?;
    if claim_readback_sha256 != claim_sha256 {
        bail!("published CEX sealed holdout claim readback SHA256 mismatch");
    }
    let sealed_revision = match store.claim_cex_sealed_holdout(&claim, Utc::now())? {
        Some(existing) => existing,
        None => {
            let sealed = evaluator
                .evaluate_sealed(&proposal, dataset)
                .map_err(anyhow::Error::msg)?;
            store.put_cex_sealed_evaluation(&claim, &sealed, Utc::now())?
        }
    };
    data_mission::write_json_atomic(
        &results_dir.join("sealed-holdout-receipt.json"),
        &sealed_revision,
    )?;
    let sealed_value = sealed_revision
        .payload
        .get("evaluation")
        .cloned()
        .context("sealed holdout receipt has no evaluation")?;
    let sealed: CandidateEvaluation = serde_json::from_value(sealed_value.clone())?;
    sealed.validate()?;

    let (strategy_bundle_id, promotion_id) = if sealed.passed {
        let evaluator_config_hash = canonical_json_hash(
            sealed_value
                .get("evaluator_config")
                .context("sealed evaluator config is missing")?,
        )?;
        let evaluation_metrics_hash = canonical_json_hash(
            sealed_value
                .get("metrics")
                .context("sealed evaluation metrics are missing")?,
        )?;
        let sealed_evaluation_hash = canonical_json_hash(&sealed_value)?;
        let bundle_id = format!("bundle:{candidate_id}");
        let promotion_id = format!("promotion:{candidate_id}");
        let existing = match store.get_promotion(&promotion_id) {
            Ok(existing) => Some(existing),
            Err(StoreError::NotFound) => None,
            Err(error) => return Err(error.into()),
        };
        let promoted_at = existing
            .as_ref()
            .map(|stored| stored.record.created_at)
            .unwrap_or_else(Utc::now);
        let bundle = StrategyBundle::new(
            bundle_id.clone(),
            candidate_id.clone(),
            candidate_hash.clone(),
            lineage.mission.dataset_manifest_id.clone(),
            sealed.evaluator_version.clone(),
            control_mission
                .spec
                .policies
                .evaluation
                .content_sha256
                .clone(),
            evaluator_config_hash.clone(),
            evaluation_metrics_hash.clone(),
            sealed_evaluation_hash.clone(),
            candidate.to_governed_strategy_bundle_artifact()?,
            promoted_at,
        )?;
        let promotion = PromotionRecord {
            promotion_id: promotion_id.clone(),
            mission_id: mission_id.to_string(),
            candidate_id: candidate_id.clone(),
            candidate_content_hash: candidate_hash,
            dataset_manifest_id: lineage.mission.dataset_manifest_id.clone(),
            evaluator_version: sealed.evaluator_version.clone(),
            evaluation_protocol_hash: control_mission
                .spec
                .policies
                .evaluation
                .content_sha256
                .clone(),
            evaluator_config_hash,
            evaluation_metrics_hash,
            sealed_evaluation_id: sealed_revision.revision_id.clone(),
            sealed_evaluation_hash,
            bundle_id: bundle.bundle_id.clone(),
            bundle_hash: bundle.bundle_hash.clone(),
            created_at: promoted_at,
        };
        if let Some(existing) = existing {
            let stored_bundle = store.get_strategy_bundle(&existing.record.bundle_id)?;
            if existing.record != promotion || stored_bundle != bundle {
                bail!("existing CEX promotion conflicts with the sealed receipt");
            }
        } else {
            store.promote_candidate(&bundle, &promotion)?;
        }
        data_mission::write_json_atomic(&results_dir.join("strategy-bundle.json"), &bundle)?;
        data_mission::write_json_atomic(&results_dir.join("promotion-record.json"), &promotion)?;
        (Some(bundle_id), Some(promotion_id))
    } else {
        if !matches!(
            store.get_promotion(&format!("promotion:{candidate_id}")),
            Err(StoreError::NotFound)
        ) || !matches!(
            store.get_strategy_bundle(&format!("bundle:{candidate_id}")),
            Err(StoreError::NotFound)
        ) {
            bail!("failed sealed holdout already has deployable lineage");
        }
        (None, None)
    };
    let report = CexFinalizationReportV1 {
        schema_version: "cex-finalization-report-v1".to_string(),
        precommit_id: precommit.precommit_id,
        sealed_receipt_id: sealed_revision.revision_id,
        sealed_passed: sealed.passed,
        strategy_bundle_id,
        promotion_id,
    };
    data_mission::write_json_atomic(&results_dir.join("finalization-report.json"), &report)?;
    Ok(report)
}

pub(crate) fn finalize_existing_search_round(
    round_execute_dir: &Path,
    finalization_dir: &Path,
    holdout_claim_put_url: &str,
    holdout_claim_readback_url: &str,
    control_mission: &CexResearchMissionArtifactV1,
) -> anyhow::Result<CexFinalizationReportV1> {
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(Policy::none())
        .build()?;
    ensure_holdout_claim_absent(&client, holdout_claim_readback_url)?;
    let round_results = round_execute_dir.join("results");
    std::fs::create_dir_all(finalization_dir)?;
    let mut store = AlphaStore::open(round_results.join("alpha.duckdb"))?;
    if store.has_cex_sealed_holdout_claim(&control_mission.spec.holdout.holdout_id)? {
        bail!("sealed holdout is already claimed; finalize requires a fresh unopened holdout");
    }
    let mission_id = control_mission.semantic_id()?;
    let lineage = store.mission_lineage(&mission_id)?;
    let factor_bank: CexFactorBankRevisionV2 =
        serde_json::from_slice(&std::fs::read(round_results.join("factor-bank.json"))?)?;
    let ridge: CexBaselineArtifactV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("ridge-baseline.json"))?)?;
    let cart: CexBaselineArtifactV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("cart-baseline.json"))?)?;
    let gate: CexBaselineGateV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("baseline-gate.json"))?)?;
    let subset_checkpoint: MctsCheckpointArtifactV1 = serde_json::from_slice(&std::fs::read(
        round_results.join("factor-subset-mcts-checkpoint.json"),
    )?)?;
    let subset_result: CexFactorBankMctsResultV1 = serde_json::from_slice(&std::fs::read(
        round_results.join("factor-subset-mcts-result.json"),
    )?)?;
    let strategy: CexCombinationResearchArtifactV1 = serde_json::from_slice(&std::fs::read(
        round_results.join("combination-walk-forward.json"),
    )?)?;
    let replay: CexEventReplayReceiptV1 = serde_json::from_slice(&std::fs::read(
        round_results.join("cex-event-replay-receipt.json"),
    )?)?;
    let gp_policy: CexGpPolicyV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("gp-policy.json"))?)?;
    let baseline_policy: CexBaselinePolicyV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("baseline-policy.json"))?)?;
    let weight_policy: CexEqualAbsoluteWeightPolicyV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("weight-policy.json"))?)?;
    let replay_policy: CexEventReplayPolicyV1 =
        serde_json::from_slice(&std::fs::read(round_results.join("replay-policy.json"))?)?;
    let materialization =
        decode_materialization(&std::fs::read(round_results.join("materialization.json"))?)?;
    let dataset_manifest = data_mission::read_registered_research_dataset(
        &store,
        &round_results.join("cex-replay-dataset-manifest.json"),
    )?;
    let rows = dataset_manifest.load_rows(&control_mission.spec.evaluation_protocol.costs)?;
    let dataset = prepare_dataset(rows, &control_mission.spec.evaluation_protocol)?;
    let baselines = alpha_engine::baselines::CexBaselineRun {
        ridge: Some(ridge),
        cart: Some(cart),
        gate,
    };
    finalize_cex_candidate(
        &mut store,
        finalization_dir,
        &client,
        holdout_claim_put_url,
        holdout_claim_readback_url,
        &mission_id,
        control_mission,
        &lineage,
        &factor_bank,
        &baselines,
        &strategy,
        &subset_checkpoint.checkpoint,
        &subset_result,
        &replay,
        &dataset,
        &gp_policy,
        &baseline_policy,
        &weight_policy,
        &replay_policy,
        &materialization.snapshot.instrument_rules,
    )
}

fn content_reference(
    id: &str,
    artifact: &impl Serialize,
) -> anyhow::Result<CexResearchContentRefV1> {
    Ok(CexResearchContentRefV1 {
        id: id.to_string(),
        content_sha256: canonical_json_hash(artifact)?,
    })
}

fn registry_content_reference(
    id: &str,
    artifact: &impl Serialize,
) -> anyhow::Result<CexResearchContentRefV1> {
    content_reference(id, &serde_json::to_value(artifact)?)
}

pub(crate) fn decode_materialization(bytes: &[u8]) -> anyhow::Result<Materialization> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).context("materialization manifest is invalid JSON")?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
    {
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7) => {
            serde_json::from_value(value).context("V7 materialization manifest is invalid")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V6) => {
            let snapshot: CexReplaySnapshotV4 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V6 materialization has no snapshot")?,
            )
            .context("historical V6 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V6 materialization is read-only and cannot execute")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5) => {
            let snapshot: CexReplaySnapshotV4 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V5 materialization has no snapshot")?,
            )
            .context("historical V5 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V5 materialization is read-only and cannot execute")
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
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3) => {
            let snapshot: CexReplaySnapshotV2 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V3 materialization has no snapshot")?,
            )
            .context("historical V3 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V3 materialization is read-only and cannot execute")
        }
        Some(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V4) => {
            let snapshot: CexReplaySnapshotV3 = serde_json::from_value(
                value
                    .get("snapshot")
                    .cloned()
                    .context("historical V4 materialization has no snapshot")?,
            )
            .context("historical V4 materialization snapshot is invalid")?;
            snapshot.validate().map_err(anyhow::Error::new)?;
            bail!("historical V4 materialization is read-only and cannot execute")
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
) -> anyhow::Result<
    Option<(
        CexCombinationResearchArtifactV1,
        CexFactorBankMctsCheckpointV1,
        CexFactorBankMctsResultV1,
    )>,
> {
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
            return Ok(Some((strategy, checkpoint.checkpoint, result)));
        }
        return Ok(None);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_cex_event_replay(
    results_dir: &Path,
    mission_id: &str,
    mission: &CexResearchMissionArtifactV1,
    materialization: &Materialization,
    materialization_sha256: &str,
    factor_bank: &CexFactorBankRevisionV2,
    context: &EngineContext<'_>,
    feature_decision_clocks: &[data_mission::FeatureDecisionClock],
    strategy: &CexCombinationResearchArtifactV1,
    policy: &CexEventReplayPolicyV1,
    replay_artifact_path: &Path,
    replay_artifact_sha256: &str,
    replay_manifest_path: &Path,
    replay_manifest_sha256: &str,
) -> anyhow::Result<CexEventReplayReceiptV1> {
    policy.validate_binding(&mission.spec.policies.replay)?;
    if mission.spec.inputs.materialization.content_sha256 != materialization_sha256 {
        bail!("CEX replay materialization identity drifted");
    }
    let positions = strategy
        .target_positions(factor_bank, context.rows())
        .map_err(anyhow::Error::msg)?;
    if feature_decision_clocks.len() != positions.len() {
        bail!("CEX replay feature clock does not match the selected strategy rows");
    }
    if feature_decision_clocks
        .iter()
        .zip(context.rows())
        .any(|(clock, row)| {
            clock.series_id != row.series_id
                || clock.series_close_time < clock.feature_available_time
        })
    {
        bail!("CEX replay feature clock drifted across PIT series boundaries");
    }
    let (decisions, non_forced_decision_count) =
        canonical_target_position_decisions(feature_decision_clocks, positions)?;
    let first_decision_time = decisions
        .first()
        .context("CEX event replay has no pre-holdout decisions")?
        .timestamp_us;
    let last_decision_time = decisions
        .last()
        .context("CEX event replay has no pre-holdout decisions")?
        .timestamp_us;
    let max_decision_delay_us = policy
        .max_decision_delay_millis
        .checked_mul(1_000)
        .context("CEX replay decision delay overflow")?;
    let replay_end_time = last_decision_time
        .checked_add(i64::try_from(max_decision_delay_us)?)
        .context("CEX replay end time overflow")?;
    let replay_manifest_sha256 = normalized_sha256("CEX replay manifest", replay_manifest_sha256)?;
    let max_depth_levels = materialization
        .top_depth
        .max(strategy.risk.capacity_depth_levels)
        .max(policy.required_depth_levels);
    let replay_config = TargetPositionReplayConfig {
        max_depth_levels,
        max_decision_delay_us,
        position_notional_usd: strategy.risk.position_notional_usd,
        fee_bps: strategy.execution.costs.fee_bps,
        rebate_bps: strategy.execution.costs.rebate_bps,
        funding_bps: strategy.execution.costs.funding_bps,
        latency_bps: strategy.execution.costs.latency_bps,
        additional_slippage_bps: strategy.execution.costs.slippage_bps,
        cross_spread: strategy.execution.costs.cross_spread,
        capacity_depth_levels: strategy.risk.capacity_depth_levels,
        trade_tape_declared: false,
    };
    let (replay_evidence, metrics) = verify_and_replay_canonical_target_positions(
        replay_artifact_path,
        replay_manifest_path,
        replay_artifact_sha256,
        &replay_manifest_sha256,
        None,
        Some(replay_end_time),
        &decisions,
        &replay_config,
    )?;
    validate_replay_materialization_binding(
        &replay_evidence,
        mission,
        materialization,
        first_decision_time,
        last_decision_time,
    )?;
    let capabilities = CexReplayCapabilitiesV1 {
        clock_semantics: policy.clock_semantics.clone(),
        modalities: replay_evidence.modalities.clone(),
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
    if metrics.snapshot_events < materialization.series_count {
        failures.push("snapshot count is below the PIT series boundary minimum".to_string());
    }
    if non_forced_decision_count < policy.min_decisions {
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
        mission_id: mission_id.to_string(),
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
        implementation_source_revision: BUILD_SOURCE_REVISION.to_string(),
        replay_engine_version: TARGET_POSITION_REPLAY_IMPLEMENTATION_VERSION.to_string(),
        metrics,
        capabilities,
        capabilities_sha256: String::new(),
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

fn canonical_target_position_decisions(
    feature_decision_clocks: &[data_mission::FeatureDecisionClock],
    positions: Vec<f64>,
) -> anyhow::Result<(Vec<TargetPositionDecision>, usize)> {
    let mut decisions = Vec::with_capacity(positions.len() + feature_decision_clocks.len());
    let mut ordinary_decisions = 0;
    for (index, (clock, target_position)) in
        feature_decision_clocks.iter().zip(positions).enumerate()
    {
        let feature_time_us = clock.feature_available_time.timestamp_micros();
        let close_time_us = clock.series_close_time.timestamp_micros();
        if close_time_us < feature_time_us {
            bail!("series close clock precedes feature availability");
        }
        if decisions
            .last()
            .is_some_and(|decision: &TargetPositionDecision| {
                decision.timestamp_us >= feature_time_us
            })
        {
            bail!("CEX replay decisions are not strictly ordered");
        }
        decisions.push(TargetPositionDecision {
            timestamp_us: feature_time_us,
            target_position,
        });
        ordinary_decisions += 1;
        let series_end = index + 1 == feature_decision_clocks.len()
            || feature_decision_clocks[index + 1].series_id != clock.series_id;
        if series_end {
            if decisions
                .last()
                .is_some_and(|decision: &TargetPositionDecision| {
                    decision.timestamp_us >= close_time_us
                })
            {
                bail!("CEX replay series close decisions are not strictly ordered");
            }
            decisions.push(TargetPositionDecision {
                timestamp_us: close_time_us,
                target_position: 0.0,
            });
        }
    }
    Ok((decisions, ordinary_decisions))
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
        let (verdict, rejection_codes, rejection_details) = match iteration.verdict {
            IterationVerdict::Keep | IterationVerdict::Discard => {
                let evidence = evaluation
                    .as_ref()
                    .context("evaluated candidate is missing screening evidence")?;
                let rejection_codes = evidence.screening_rejection_codes()?;
                if rejection_codes.is_empty() {
                    (
                        CexFactorScreeningVerdictV1::Accepted,
                        rejection_codes,
                        vec![],
                    )
                } else {
                    (
                        CexFactorScreeningVerdictV1::Rejected,
                        rejection_codes,
                        evidence.evidence.failure_reasons.clone(),
                    )
                }
            }
            IterationVerdict::Crash => (
                CexFactorScreeningVerdictV1::Rejected,
                vec![match iteration.failure_class.as_deref() {
                    Some("duplicate_candidate") => CexFactorRejectionCodeV1::DuplicateCandidate,
                    Some("evaluation_error") => CexFactorRejectionCodeV1::EvaluationFailed,
                    _ => CexFactorRejectionCodeV1::EngineFailure,
                }],
                iteration
                    .failure_explanation
                    .clone()
                    .map(|reason| vec![reason])
                    .unwrap_or_default(),
            ),
        };
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

fn validate_args(args: &ExecuteMissionArgs, binding: &ExecutionBinding) -> anyhow::Result<()> {
    if args.work_dir.as_os_str().is_empty()
        || [
            args.mission_url.as_str(),
            args.feature_url.as_str(),
            args.materialization_url.as_str(),
            args.replay_artifact_url.as_str(),
            args.replay_manifest_url.as_str(),
            args.result_put_url.as_str(),
            args.result_readback_url.as_str(),
            args.holdout_claim_put_url.as_str(),
            args.holdout_claim_readback_url.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        bail!("Mission execution paths and URLs are required");
    }
    normalized_sha256("Mission", &args.mission_sha256)?;
    validate_cex_mission_id(&args.mission_id)?;
    validate_cex_holdout_id(&args.holdout_id)?;
    normalized_sha256("CEX replay artifact", &args.replay_artifact_sha256)?;
    normalized_sha256("CEX replay manifest", &args.replay_manifest_sha256)?;
    if !valid_git_revision(BUILD_SOURCE_REVISION) {
        bail!("alpha-harness was built without an exact source revision");
    }
    resume_source(args)?;
    validate_result_readback_binding(&args.result_put_url, &args.result_readback_url)?;
    validate_holdout_claim_binding(args, &args.holdout_id, binding)?;
    Ok(())
}

fn validate_holdout_claim_binding(
    args: &ExecuteMissionArgs,
    holdout_id: &str,
    binding: &ExecutionBinding,
) -> anyhow::Result<()> {
    let result_is_remote =
        args.result_put_url.starts_with("http://") || args.result_put_url.starts_with("https://");
    let put_is_remote = args.holdout_claim_put_url.starts_with("http://")
        || args.holdout_claim_put_url.starts_with("https://");
    let readback_is_remote = args.holdout_claim_readback_url.starts_with("http://")
        || args.holdout_claim_readback_url.starts_with("https://");
    if result_is_remote != put_is_remote || put_is_remote != readback_is_remote {
        bail!("CEX result and holdout claim transports must match");
    }
    if put_is_remote {
        let claim_object = prediction_dispatch::canonical_https_object(
            "CEX holdout claim",
            &args.holdout_claim_put_url,
        )?;
        let claim_readback_object = prediction_dispatch::canonical_https_object(
            "CEX holdout claim readback",
            &args.holdout_claim_readback_url,
        )?;
        if claim_object != claim_readback_object {
            bail!("CEX holdout claim readback URL must identify the same immutable object");
        }
        let expected_claim_object =
            prediction_dispatch::cex_global_holdout_claim_object(holdout_id)?;
        if claim_object != expected_claim_object {
            bail!("CEX holdout claim object must use the global holdout claim path");
        }
    } else {
        let result_object = normalized_local_object(&args.result_put_url)?;
        let claim_object = normalized_local_object(&args.holdout_claim_put_url)?;
        let claim_readback_object = normalized_local_object(&args.holdout_claim_readback_url)?;
        if claim_object != claim_readback_object {
            bail!("CEX holdout claim readback path must identify the same immutable object");
        }
        let expected_claim_object = expected_local_holdout_claim_object(
            &result_object,
            &args.mission_id,
            holdout_id,
            binding,
        )?;
        if claim_object != expected_claim_object {
            bail!(
                "CEX holdout claim path must be the holdout-scoped sibling of the Mission results"
            );
        }
    }
    Ok(())
}

fn expected_local_holdout_claim_object(
    result_object: &str,
    mission_id: &str,
    holdout_id: &str,
    binding: &ExecutionBinding,
) -> anyhow::Result<String> {
    match binding {
        ExecutionBinding::Direct => Ok(prediction_dispatch::cex_result_attempt_and_holdout_claim(
            result_object,
            mission_id,
            holdout_id,
        )?
        .1),
        ExecutionBinding::Campaign {
            campaign_id,
            round_id,
            ..
        } => prediction_dispatch::cex_campaign_round_result_and_holdout_claim(
            result_object,
            campaign_id,
            round_id,
            holdout_id,
        ),
    }
}

fn normalized_local_object(value: &str) -> anyhow::Result<String> {
    let path = Path::new(value.strip_prefix("file://").unwrap_or(value));
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        bail!("local immutable object path must be non-empty and lexically normalized");
    }
    Ok(format!(
        "/{}",
        path.to_string_lossy().trim_start_matches('/')
    ))
}

pub(crate) fn ensure_holdout_claim_absent(client: &Client, source: &str) -> anyhow::Result<()> {
    let exists = if source.starts_with("http://") || source.starts_with("https://") {
        let response = client.get(source).send()?;
        match response.status() {
            StatusCode::NOT_FOUND => false,
            status if status.is_success() => true,
            _ => {
                response.error_for_status()?;
                unreachable!()
            }
        }
    } else {
        Path::new(source.strip_prefix("file://").unwrap_or(source)).try_exists()?
    };
    if exists {
        bail!("CEX sealed holdout claim already exists; Mission is terminal and inconclusive");
    }
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
    dataset: &CexReplayDatasetManifestV5,
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

pub(crate) fn validate_materialization(
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
        || materialization.schema_version != BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7
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
        || materialization.series_count == 0
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
    if validation.funding_bps.to_bits() != 0.0f64.to_bits() {
        bail!("current L2-only CEX replay requires zero funding cost");
    }
    let artifact_sha256 = normalized_sha256("feature artifact", &materialization.artifact_sha256)?;
    let source_revision = normalized_sha256("source revision", &materialization.source_revision)?;
    let mut source_sha256s = Vec::with_capacity(materialization.source_segments.len());
    for segment in &materialization.source_segments {
        let source_sha256 = normalized_sha256("source segment", &segment.sha256)?;
        source_sha256s.push(source_sha256.clone());
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
    if hft_collector::lob_archiver::source_revision(source_sha256s.iter().map(String::as_str))
        != source_revision
    {
        bail!("materialization source revision does not bind its source segments");
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

pub(crate) fn validate_cex_mission_id(value: &str) -> anyhow::Result<()> {
    let suffix = value
        .strip_prefix("cex-mission-")
        .context("CEX Mission ID must use the cex-mission-<sha256> form")?;
    if value != value.trim() || normalized_sha256("CEX Mission ID", suffix)? != suffix {
        bail!("CEX Mission ID must use the cex-mission-<sha256> form");
    }
    Ok(())
}

pub(crate) fn validate_cex_holdout_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value != value.trim()
        || value.len() > 256
        || value.chars().any(char::is_control)
    {
        bail!("CEX holdout ID is invalid");
    }
    Ok(())
}

pub(crate) fn valid_git_revision(value: &str) -> bool {
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

pub(crate) fn recover_execution_report_from_published_result(
    client: &Client,
    result_readback_url: &str,
    readback_destination: &Path,
    expected_mission_id: &str,
    expected_mission_sha256: &str,
    binding: &ExecutionBinding,
) -> anyhow::Result<Option<ExecutionReport>> {
    let Some((bundle_bytes, bundle_sha256)) = fetch_optional_to_file(
        client,
        result_readback_url,
        readback_destination,
        MAX_RESULT_BUNDLE_BYTES,
    )?
    else {
        return Ok(None);
    };
    let mut archive = ZipArchive::new(File::open(readback_destination)?)
        .context("open published result bundle")?;
    let admission: MissionAdmissionEvidenceV1 =
        read_bundle_json(&mut archive, "results/mission-admission.json", 64 * 1024)?
            .context("published result bundle is missing results/mission-admission.json")?;
    if admission.mission_id != expected_mission_id {
        bail!("published result bundle Mission ID does not match the Campaign Mission");
    }
    if admission.mission_artifact_sha256 != normalized_sha256("Mission", expected_mission_sha256)? {
        bail!("published result bundle Mission SHA256 does not match the Campaign Mission");
    }
    validate_recovered_binding(&admission, binding)?;
    let control_mission: CexResearchMissionArtifactV1 = read_bundle_json(
        &mut archive,
        "results/control-plane-mission.json",
        MAX_MISSION_BYTES,
    )?
    .context("published result bundle is missing results/control-plane-mission.json")?;
    control_mission.validate()?;
    if control_mission.semantic_id()? != expected_mission_id {
        bail!("published result bundle Mission artifact does not match the Campaign Mission");
    }

    let replay_receipt: Option<CexEventReplayReceiptV1> = read_bundle_json(
        &mut archive,
        "results/cex-event-replay-receipt.json",
        512 * 1024,
    )?;
    if let Some(receipt) = &replay_receipt {
        let strategy: CexCombinationResearchArtifactV1 = read_bundle_json(
            &mut archive,
            "results/combination-walk-forward.json",
            4 * 1024 * 1024,
        )?
        .context(
            "published result bundle replay receipt has no combination-walk-forward artifact",
        )?;
        validate_replay_strategy_binding(
            receipt,
            &strategy,
            &control_mission,
            expected_mission_id,
        )?;
    }

    let finalization: Option<CexFinalizationReportV1> =
        read_bundle_json(&mut archive, "results/finalization-report.json", 128 * 1024)?;
    let finalization = if let Some(report) = finalization {
        validate_recovered_finalization(
            &mut archive,
            &report,
            &control_mission,
            replay_receipt.as_ref(),
            expected_mission_id,
        )?;
        Some(report)
    } else {
        if replay_receipt
            .as_ref()
            .is_some_and(|receipt| receipt.gate.passed)
            && !binding.is_search_only()
        {
            bail!("published result bundle is missing finalization evidence for a passing replay");
        }
        None
    };

    Ok(Some(ExecutionReport {
        mission_id: expected_mission_id.to_string(),
        mission_sha256: normalized_sha256("Mission", expected_mission_sha256)?,
        campaign_id: admission.campaign_id,
        round_id: admission.round_id,
        request_sha256: admission.request_sha256,
        engine: "gp_then_factor_bank_subset_mcts",
        bundle_bytes,
        bundle_sha256: bundle_sha256.clone(),
        readback_bundle_sha256: bundle_sha256,
        replay_receipt_id: replay_receipt
            .as_ref()
            .map(|receipt| receipt.receipt_id.clone()),
        replay_gate_passed: replay_receipt.as_ref().map(|receipt| receipt.gate.passed),
        final_precommit_id: finalization
            .as_ref()
            .map(|report| report.precommit_id.clone()),
        sealed_receipt_id: finalization
            .as_ref()
            .map(|report| report.sealed_receipt_id.clone()),
        sealed_passed: finalization.as_ref().map(|report| report.sealed_passed),
        strategy_bundle_id: finalization
            .as_ref()
            .and_then(|report| report.strategy_bundle_id.clone()),
        promotion_id: finalization
            .as_ref()
            .and_then(|report| report.promotion_id.clone()),
    }))
}

fn fetch_optional_to_file(
    client: &Client,
    source: &str,
    destination: &Path,
    max_bytes: u64,
) -> anyhow::Result<Option<(u64, String)>> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let response = client.get(source).send()?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes)
        {
            bail!("source exceeds the allowed size");
        }
        let mut temporary = data_mission::temporary_output_file(destination, ".monday-fetch-")?;
        let mut reader = response;
        let bytes = std::io::copy(
            &mut reader.by_ref().take(max_bytes + 1),
            temporary.as_file_mut(),
        )?;
        temporary.as_file().sync_all()?;
        if bytes > max_bytes {
            bail!("source exceeds the allowed size");
        }
        data_mission::persist_output_file(temporary, destination, "fetched source")?;
        return Ok(Some((bytes, sha256_file(destination)?)));
    }

    let path = Path::new(source.strip_prefix("file://").unwrap_or(source));
    if !path.try_exists()? {
        return Ok(None);
    }
    Ok(Some(fetch_to_file(client, source, destination, max_bytes)?))
}

fn validate_recovered_binding(
    admission: &MissionAdmissionEvidenceV1,
    binding: &ExecutionBinding,
) -> anyhow::Result<()> {
    match binding {
        ExecutionBinding::Direct => {
            if admission.campaign_id.is_some()
                || admission.round_id.is_some()
                || admission.request_sha256.is_some()
            {
                bail!("published result bundle carries Campaign binding for a direct Mission");
            }
        }
        ExecutionBinding::Campaign {
            campaign_id,
            round_id,
            request_sha256: _,
        } => {
            if admission.campaign_id.as_deref() != Some(campaign_id)
                || admission.round_id.as_deref() != Some(round_id)
            {
                bail!("published result bundle does not match the Campaign binding");
            }
            normalized_sha256(
                "published result bundle Campaign request SHA256",
                admission.request_sha256.as_deref().context(
                    "published result bundle is missing Campaign request SHA256 evidence",
                )?,
            )?;
            // Presigned query rotation changes request bytes without changing the
            // canonical Campaign, round, or Mission identities checked above.
        }
    }
    Ok(())
}

fn validate_replay_strategy_binding(
    replay: &CexEventReplayReceiptV1,
    strategy: &CexCombinationResearchArtifactV1,
    mission: &CexResearchMissionArtifactV1,
    mission_id: &str,
) -> anyhow::Result<()> {
    replay.validate()?;
    strategy.validate().map_err(anyhow::Error::msg)?;
    if replay.mission_id != mission_id
        || strategy.mission_id != mission_id
        || replay.strategy != content_reference(&strategy.artifact_id, strategy)?
        || replay.dataset != mission.spec.inputs.dataset
        || replay.materialization != mission.spec.inputs.materialization
        || replay.source != mission.spec.inputs.source
        || replay.holdout_id != mission.spec.holdout.holdout_id
        || replay.holdout_state != mission.spec.holdout.state
    {
        bail!("CEX replay receipt does not bind the exact selected strategy and Mission inputs");
    }
    Ok(())
}

fn validate_recovered_finalization(
    archive: &mut ZipArchive<File>,
    report: &CexFinalizationReportV1,
    control_mission: &CexResearchMissionArtifactV1,
    replay_receipt: Option<&CexEventReplayReceiptV1>,
    expected_mission_id: &str,
) -> anyhow::Result<()> {
    if !replay_receipt.is_some_and(|receipt| receipt.gate.passed) {
        bail!("published result bundle finalization lacks a passing replay receipt");
    }
    let precommit: CexFinalPrecommitV1 =
        read_bundle_json(archive, "results/final-precommit.json", 512 * 1024)?
            .context("published result bundle is missing results/final-precommit.json")?;
    precommit.validate()?;
    let semantic_mission = serde_json::json!({
        "schema_version": &control_mission.schema_version,
        "spec": &control_mission.spec,
    });
    if precommit.mission.id != expected_mission_id
        || precommit.mission.content_sha256 != canonical_json_hash(&semantic_mission)?
        || precommit.holdout_id != control_mission.spec.holdout.holdout_id
        || precommit.holdout_state != control_mission.spec.holdout.state
    {
        bail!("published result bundle precommit does not match the exact Campaign Mission");
    }
    if precommit.precommit_id != report.precommit_id {
        bail!("published result bundle precommit evidence does not match its finalization report");
    }
    let replay_receipt = replay_receipt.expect("passing replay receipt already required");
    if replay_receipt.receipt_id != precommit.replay_receipt.id
        || canonical_json_hash(replay_receipt)? != precommit.replay_receipt.content_sha256
        || replay_receipt.mission_id != precommit.mission.id
        || replay_receipt.strategy != precommit.four_stage_strategy
        || replay_receipt.dataset != precommit.dataset
        || replay_receipt.source != precommit.source
        || replay_receipt.holdout_id != precommit.holdout_id
        || replay_receipt.holdout_state != precommit.holdout_state
        || replay_receipt.capabilities_sha256 != precommit.replay_capabilities_sha256
        || replay_receipt.deployment_authority
        || replay_receipt.order_submission_authority
    {
        bail!("published result bundle replay receipt does not prove the exact final precommit");
    }
    let claim = CexSealedHoldoutClaimV1::from_precommit(&precommit)?;
    let sealed_receipt: RegistryRevision =
        read_bundle_json(archive, "results/sealed-holdout-receipt.json", 512 * 1024)?
            .context("published result bundle is missing results/sealed-holdout-receipt.json")?;
    if sealed_receipt.revision_id != report.sealed_receipt_id {
        bail!("published result bundle sealed receipt does not match its finalization report");
    }
    let sealed: CandidateEvaluation = serde_json::from_value(
        sealed_receipt
            .payload
            .get("evaluation")
            .cloned()
            .context("published result bundle sealed receipt is missing evaluation")?,
    )
    .context("published result bundle sealed receipt carries invalid evaluation")?;
    sealed.validate()?;
    let (_, protocol_hash) = sealed.protocol_binding()?;
    if sealed_receipt.registry_kind != "sealed_evaluation"
        || sealed_receipt.asset_id != claim.candidate.id
        || sealed_receipt.parent_revision_id.as_deref() != Some(claim.claim_id.as_str())
        || sealed_receipt
            .payload
            .get("mission_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.mission_id.as_str())
        || sealed_receipt
            .payload
            .get("candidate_content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(claim.candidate.content_sha256.as_str())
        || sealed_receipt
            .payload
            .get("dataset_manifest_id")
            .and_then(serde_json::Value::as_str)
            != Some(precommit.dataset_manifest_id.as_str())
        || sealed_receipt
            .payload
            .get("evaluation_protocol_hash")
            .and_then(serde_json::Value::as_str)
            != Some(claim.evaluation_protocol.content_sha256.as_str())
        || sealed_receipt
            .payload
            .get("precommit_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.precommit.id.as_str())
        || sealed_receipt
            .payload
            .get("precommit_content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(claim.precommit.content_sha256.as_str())
        || sealed_receipt
            .payload
            .get("sealed_access_claim_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.claim_id.as_str())
        || sealed_receipt
            .payload
            .get("holdout_id")
            .and_then(serde_json::Value::as_str)
            != Some(claim.holdout_id.as_str())
        || sealed.evaluator_version != SEALED_HOLDOUT_EVALUATOR_VERSION
        || protocol_hash != claim.evaluation_protocol.content_sha256
        || sealed.passed != report.sealed_passed
    {
        bail!("published result bundle sealed evaluation does not match its finalization report");
    }
    if report.sealed_passed {
        let strategy_bundle: StrategyBundle =
            read_bundle_json(archive, "results/strategy-bundle.json", 4 * 1024 * 1024)?
                .context("published result bundle is missing results/strategy-bundle.json")?;
        strategy_bundle.validate()?;
        if strategy_bundle.candidate_id != precommit.final_candidate.id
            || strategy_bundle.candidate_content_hash != precommit.final_candidate.content_sha256
        {
            bail!("published result bundle strategy lineage does not match the final precommit");
        }
        let alpha_domain::StrategyBundleArtifact::CexFourStage {
            strategy: bundled_strategy,
        } = &strategy_bundle.artifact
        else {
            bail!(
                "published result bundle strategy lineage is not a final CEX four-stage strategy"
            );
        };
        bundled_strategy.validate_against_precommit(&precommit)?;
        if strategy_bundle.bundle_id
            != report
                .strategy_bundle_id
                .as_deref()
                .context("published result bundle finalization is missing strategy_bundle_id")?
        {
            bail!(
                "published result bundle strategy lineage does not match its finalization report"
            );
        }
        let promotion: PromotionRecord =
            read_bundle_json(archive, "results/promotion-record.json", 512 * 1024)?
                .context("published result bundle is missing results/promotion-record.json")?;
        promotion.validate(&strategy_bundle)?;
        if promotion.candidate_id != precommit.final_candidate.id
            || promotion.candidate_content_hash != precommit.final_candidate.content_sha256
        {
            bail!("published result bundle promotion lineage does not match the final precommit");
        }
        if promotion.promotion_id
            != report
                .promotion_id
                .as_deref()
                .context("published result bundle finalization is missing promotion_id")?
        {
            bail!(
                "published result bundle promotion lineage does not match its finalization report"
            );
        }
    } else if report.strategy_bundle_id.is_some() || report.promotion_id.is_some() {
        bail!("published result bundle failed sealed holdout cannot carry deployable lineage");
    }
    Ok(())
}

fn read_bundle_json<T: for<'de> Deserialize<'de>>(
    archive: &mut ZipArchive<File>,
    name: &str,
    max_bytes: u64,
) -> anyhow::Result<Option<T>> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open bundle entry {name}")),
    };
    if file.size() > max_bytes {
        bail!("bundle entry exceeds the allowed size: {name}");
    }
    let mut bytes = Vec::with_capacity(file.size() as usize);
    std::io::copy(&mut file.by_ref().take(max_bytes + 1), &mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        bail!("bundle entry exceeds the allowed size: {name}");
    }
    Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
        format!("parse bundle JSON entry {name}")
    })?))
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
    checked_result_bundle_bytes(bundle)?;
    publish_immutable_file(client, destination, bundle, "application/zip")
}

fn checked_result_bundle_bytes(bundle: &Path) -> anyhow::Result<u64> {
    let bundle_bytes = bundle.metadata()?.len();
    if bundle_bytes > MAX_RESULT_BUNDLE_BYTES {
        bail!(
            "result bundle exceeds the allowed size: {} bytes > {} bytes",
            bundle_bytes,
            MAX_RESULT_BUNDLE_BYTES
        );
    }
    Ok(bundle_bytes)
}

pub(crate) fn publish_immutable_file(
    client: &Client,
    destination: &str,
    source: &Path,
    content_type: &'static str,
) -> anyhow::Result<()> {
    if destination.starts_with("http://") || destination.starts_with("https://") {
        client
            .put(destination)
            .header(CONTENT_TYPE, HeaderValue::from_static(content_type))
            .header("x-oss-forbid-overwrite", "true")
            .body(File::open(source)?)
            .send()?
            .error_for_status()?;
        return Ok(());
    }
    let path = Path::new(destination.strip_prefix("file://").unwrap_or(destination));
    let mut output = data_mission::temporary_output_file(path, ".monday-result-")?;
    std::io::copy(&mut File::open(source)?, output.as_file_mut())?;
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
pub(crate) mod tests {
    use super::*;
    use crate::{
        cli::{FeedbackLogArgs, FeedbackRecordArgs, SignDeploymentArgs, ValidationArgs},
        governance,
    };
    use alpha_domain::{
        deployment_scope_hash, runtime_stage_is_healthy, sign_runtime_attribution_event,
        AllowedIntentType, ApprovalClass, AttributionKind, AttributionMode, AttributionOutcome,
        CexBaselineArtifactV1, CexBaselineGateV1, CexEventReplayPolicyV1, CexResearchContentRefV1,
        CexResearchEvidenceKindV1, CexResearchEvidenceRefV1, CexResearchEvidenceSignatureV1,
        CexResearchFalsificationTestV1, CexResearchHoldoutStateV1, CexResearchHoldoutV1,
        CexResearchHypothesisTargetV1, CexResearchHypothesisV1, CexResearchInputBindingsV1,
        CexResearchInstrumentV1, CexResearchMarketV1, CexResearchMissionSpecV1,
        CexResearchOperationalMetadataV1, CexResearchPolicyBindingsV1, CexResearchSearchPlanV1,
        CexResearchVenueV1, DeploymentEnvelope, EvaluationLabelSpecV1, RuntimeAttributionEvent,
        SearchBudget, SignedRuntimeAttributionEvent, CEX_RESEARCH_MISSION_SCHEMA_V1,
    };
    use alpha_engine::engines::{CexCombinationResearchArtifactV1, CexFactorBankMctsResultV1};
    use alpha_store::ApprovalRecord;
    use ed25519_dalek::SigningKey;

    fn cex_triplet(byte: char) -> hft_research_manifest::CexArtifactTripletV2 {
        hft_research_manifest::CexArtifactTripletV2 {
            data_sha256: byte.to_string().repeat(64),
            manifest_sha256: byte.to_string().repeat(64),
            success_sha256: byte.to_string().repeat(64),
        }
    }

    fn indexed_cex_triplet(index: usize) -> hft_research_manifest::CexArtifactTripletV2 {
        let data_sha256 = hex::encode(Sha256::digest(format!("runner-reference-data-{index}")));
        hft_research_manifest::CexArtifactTripletV2 {
            manifest_sha256: hex::encode(Sha256::digest(format!(
                "runner-reference-manifest-{index}"
            ))),
            success_sha256: data_sha256.clone(),
            data_sha256,
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

    fn ingest_governed_cex_feedback(
        db: &Path,
        directory: &Path,
        mission_id: &str,
        promotion: &PromotionRecord,
        bundle: &StrategyBundle,
    ) -> Vec<SignedRuntimeAttributionEvent> {
        let now = Utc::now();
        let deployment_key_path = directory.join("cex-deployment-signing-key.hex");
        std::fs::write(&deployment_key_path, hex::encode([9_u8; 32])).unwrap();
        let feedback_key = SigningKey::from_bytes(&[11_u8; 32]);
        let trusted_keys_path = directory.join("cex-runtime-feedback-keys.json");
        std::fs::write(
            &trusted_keys_path,
            serde_json::to_vec(&BTreeMap::from([(
                "cex-runtime-feedback".to_string(),
                hex::encode(feedback_key.verifying_key().to_bytes()),
            )]))
            .unwrap(),
        )
        .unwrap();

        let mut feedback = Vec::new();
        for (suffix, intent, approval_class, approval_name, mode) in [
            (
                "paper",
                AllowedIntentType::StartPaper,
                ApprovalClass::Paper,
                "paper",
                AttributionMode::Paper,
            ),
            (
                "shadow",
                AllowedIntentType::StartShadow,
                ApprovalClass::Shadow,
                "shadow",
                AttributionMode::Shadow,
            ),
        ] {
            let deployment_id = format!("cex-{suffix}-{}", bundle.candidate_id);
            let approval_id = format!("cex-{suffix}-approval");
            let envelope = DeploymentEnvelope {
                deployment_id: deployment_id.clone(),
                asset_revision_id: bundle.candidate_id.clone(),
                promotion_id: promotion.promotion_id.clone(),
                promotion_manifest_hash: canonical_json_hash(promotion).unwrap(),
                bundle_id: bundle.bundle_id.clone(),
                bundle_hash: bundle.bundle_hash.clone(),
                runtime_config_hash: "d".repeat(64),
                risk_policy_hash: "e".repeat(64),
                account_id: "binance-paper-shadow".to_string(),
                venue: "binance".to_string(),
                instruments: vec!["BTCUSDT".to_string()],
                allowed_intent_types: vec![AllowedIntentType::LoadFactor, intent],
                max_notional: 100.0,
                max_symbol_exposure: 50.0,
                max_order_size: 10.0,
                max_slippage_bps: 2.0,
                valid_from: now - ChronoDuration::minutes(1),
                expires_at: now + ChronoDuration::minutes(30),
                nonce: format!("cex-{suffix}-nonce"),
                approval_class,
                approval_signatures: vec![approval_id.clone()],
                payload_hash: String::new(),
            };
            AlphaStore::open(db)
                .unwrap()
                .record_approval(&ApprovalRecord {
                    approval_id,
                    approval_class: approval_name.to_string(),
                    subject_id: promotion.promotion_id.clone(),
                    payload: serde_json::json!({
                        "scope_hash": deployment_scope_hash(&envelope).unwrap(),
                    }),
                    signer_id: Some(format!("cex-{suffix}-risk-officer")),
                    valid_from: Some(now - ChronoDuration::minutes(1)),
                    expires_at: Some(now + ChronoDuration::minutes(30)),
                    revoked_at: None,
                    revoked_by: None,
                    revocation_reason: None,
                    created_at: now - ChronoDuration::minutes(1),
                })
                .unwrap();

            let envelope_path = directory.join(format!("cex-{suffix}-envelope.json"));
            let signed_path = directory.join(format!("cex-{suffix}-signed.json"));
            data_mission::write_json_atomic(&envelope_path, &envelope).unwrap();
            governance::sign_deployment(SignDeploymentArgs {
                db: db.to_path_buf(),
                envelope: envelope_path,
                signing_key: deployment_key_path.clone(),
                key_id: "cex-deployment-key".to_string(),
                output: signed_path,
            })
            .unwrap();

            let strategy_id = format!("{}:BTCUSDT", bundle.bundle_id);
            for event in [
                RuntimeAttributionEvent {
                    event_id: format!("{deployment_id}:activation"),
                    deployment_id: deployment_id.clone(),
                    asset_revision_id: bundle.candidate_id.clone(),
                    mission_id: None,
                    mode: mode.clone(),
                    outcome: AttributionOutcome::Activated,
                    kind: AttributionKind::Activation,
                    strategy_id: None,
                    order_id: None,
                    account_id: None,
                    venue: None,
                    symbol: None,
                    metrics: BTreeMap::from([(
                        "sealed_execution_cost_coverage_required".to_string(),
                        1.0,
                    )]),
                    reason: None,
                    observed_at: now,
                },
                RuntimeAttributionEvent {
                    event_id: format!("{deployment_id}:portfolio"),
                    deployment_id: deployment_id.clone(),
                    asset_revision_id: bundle.candidate_id.clone(),
                    mission_id: None,
                    mode: mode.clone(),
                    outcome: AttributionOutcome::Healthy,
                    kind: AttributionKind::PortfolioSnapshot,
                    strategy_id: Some(strategy_id.clone()),
                    order_id: None,
                    account_id: Some("binance-paper-shadow".to_string()),
                    venue: Some("binance".to_string()),
                    symbol: Some("BTCUSDT".to_string()),
                    metrics: BTreeMap::from([
                        ("gross_pnl_coverage_complete".to_string(), 1.0),
                        ("fee_coverage_complete".to_string(), 1.0),
                        ("execution_cost_coverage_complete".to_string(), 1.0),
                        ("mark_coverage_complete".to_string(), 1.0),
                    ]),
                    reason: None,
                    observed_at: now + ChronoDuration::seconds(1),
                },
                RuntimeAttributionEvent {
                    event_id: format!("{deployment_id}:fill"),
                    deployment_id,
                    asset_revision_id: bundle.candidate_id.clone(),
                    mission_id: None,
                    mode,
                    outcome: AttributionOutcome::Healthy,
                    kind: AttributionKind::Fill,
                    strategy_id: Some(strategy_id),
                    order_id: Some(format!("cex-{suffix}-order")),
                    account_id: Some("binance-paper-shadow".to_string()),
                    venue: Some("binance".to_string()),
                    symbol: Some("BTCUSDT".to_string()),
                    metrics: BTreeMap::from([("fill_quantity".to_string(), 1.0)]),
                    reason: None,
                    observed_at: now + ChronoDuration::seconds(2),
                },
            ] {
                feedback.push(
                    sign_runtime_attribution_event(event, "cex-runtime-feedback", &feedback_key)
                        .unwrap(),
                );
            }
        }

        let mut wrong_scope = feedback[2].clone();
        wrong_scope.event.event_id = "cex-cross-scope-feedback".to_string();
        wrong_scope.event.symbol = Some("ETHUSDT".to_string());
        wrong_scope = sign_runtime_attribution_event(
            wrong_scope.event,
            "cex-runtime-feedback",
            &feedback_key,
        )
        .unwrap();
        let wrong_scope_path = directory.join("cex-cross-scope-feedback.json");
        data_mission::write_json_atomic(&wrong_scope_path, &wrong_scope).unwrap();
        assert!(governance::ingest_feedback(FeedbackRecordArgs {
            db: db.to_path_buf(),
            record: wrong_scope_path,
            trusted_keys: trusted_keys_path.clone(),
        })
        .is_err());
        assert!(AlphaStore::open(db)
            .unwrap()
            .runtime_attributions_for_mission(mission_id)
            .unwrap()
            .is_empty());

        let feedback_path = directory.join("cex-runtime-feedback.jsonl");
        let mut bytes = Vec::new();
        for signed in &feedback {
            serde_json::to_writer(&mut bytes, signed).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(&feedback_path, bytes).unwrap();
        governance::ingest_feedback_log(FeedbackLogArgs {
            db: db.to_path_buf(),
            log: feedback_path,
            trusted_keys: trusted_keys_path,
        })
        .unwrap();
        feedback
    }

    #[test]
    fn execute_rejects_features_that_have_no_live_formula_semantics() {
        let mut fixture = fixture("unsupported-live-fields");
        fixture.mission.spec.feature_fields = vec!["ofi_top5".to_string()];
        fixture.mission.spec.hypotheses[0].required_feature_families = vec!["ofi_top5".to_string()];
        resign_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert_eq!(
            error.to_string(),
            "feature field ofi_top5 is not live executable: unsupported live field: ofi_top5"
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
        let snapshot: CexReplaySnapshotV5 =
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
    fn execute_rejects_non_zero_current_funding_cost() {
        let mut fixture = fixture("non-zero-current-funding");
        fixture.mission.spec.evaluation_protocol.costs.funding_bps = 0.1;
        resign_mission(&mut fixture);

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(error
            .to_string()
            .contains("current L2-only CEX replay requires zero funding cost"));
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

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(
            format!("{error:#}").contains("registered feature lineage or label facts do not match"),
            "{error:#}"
        );
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
        fixture.materialization["snapshot"]["series"][0]["first_event_time"] =
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
        assert_eq!(execution_model["schema_version"], "execution_cost_model_v3");
        assert_eq!(
            execution_model["latency_cost_basis"],
            "mission_declared_assumption"
        );
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
    fn execute_accepts_mission_supplied_fee_assumption_without_account_evidence() {
        let mut fixture = fixture("mission-fee-assumption");
        fixture.mission.spec.evaluation_protocol.costs.fee_bps = 0.0;
        resign_mission(&mut fixture);

        execute(fixture.args.clone()).unwrap();

        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.args.work_dir.join("results/candidates.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            evidence["evaluations"][0]["record"]["payload"]["evaluation_protocol"]["costs"]
                ["fee_bps"],
            0.0
        );
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
        assert_eq!(factor_bank["schema_version"], "cex-factor-bank-v3");
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
        let lineage = store
            .mission_lineage(fixture.mission.semantic_id().unwrap().as_str())
            .unwrap();
        for attempt in &typed.attempts {
            let iteration = lineage
                .iterations
                .iter()
                .find(|iteration| {
                    iteration.candidate_artifact_id.as_deref()
                        == Some(attempt.candidate_id.as_str())
                })
                .unwrap();
            assert_eq!(
                iteration.engine,
                alpha_domain::EngineKind::GeneticProgramming
            );
        }
        assert!(!results.join("mcts-research-receipt.json").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn discarded_full_evaluation_can_enter_the_atomic_factor_bank() {
        let mut fixture = fixture("gp-atomic-factor-bank");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        fixture.mission.spec.evaluation_protocol.costs.fee_bps = 1_000.0;
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        resign_mission(&mut fixture);

        execute(fixture.args.clone()).unwrap();

        let factor_bank: CexFactorBankRevisionV2 =
            serde_json::from_value(read_factor_bank(&fixture.args)).unwrap();
        let accepted = factor_bank
            .attempts
            .iter()
            .find(|attempt| {
                attempt.verdict == CexFactorScreeningVerdictV1::Accepted
                    && attempt
                        .evaluation
                        .as_ref()
                        .is_some_and(|evaluation| !evaluation.evidence.passed)
            })
            .expect("predictive factor should survive a full trading rejection");
        assert!(accepted.rejection_codes.is_empty());
        assert!(accepted.rejection_details.is_empty());
        assert!(!accepted
            .evaluation
            .as_ref()
            .unwrap()
            .evidence
            .failure_reasons
            .is_empty());
        assert!(factor_bank
            .entries
            .iter()
            .any(|entry| entry.candidate_id == accepted.candidate_id));

        let lineage = AlphaStore::open(fixture.args.work_dir.join("results/alpha.duckdb"))
            .unwrap()
            .mission_lineage(fixture.mission.semantic_id().unwrap().as_str())
            .unwrap();
        assert!(lineage.iterations.iter().any(|iteration| {
            iteration.candidate_artifact_id.as_deref() == Some(accepted.candidate_id.as_str())
                && iteration.verdict == IterationVerdict::Discard
        }));
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
        let mut store = AlphaStore::open(results.join("alpha.duckdb")).unwrap();
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
        assert_eq!(replay_receipt.metrics.max_decision_delay_us, 100);
        assert_eq!(
            replay_receipt.implementation_source_revision,
            BUILD_SOURCE_REVISION
        );
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

        let precommit: CexFinalPrecommitV1 =
            serde_json::from_slice(&std::fs::read(results.join("final-precommit.json")).unwrap())
                .unwrap();
        precommit.validate().unwrap();
        assert_eq!(precommit.mission.id, fixture.mission.semantic_id().unwrap());
        assert_eq!(precommit.factor_bank.id, factor_bank_typed.revision_id);
        assert_eq!(precommit.ridge_baseline.id, ridge_typed.artifact_id);
        assert_eq!(precommit.cart_baseline.id, cart_typed.artifact_id);
        assert_eq!(precommit.baseline_gate.id, gate_typed.gate_id);
        assert_eq!(precommit.mcts_checkpoint, strategy.subset_checkpoint);
        assert_eq!(precommit.mcts_subset, strategy.subset_result);
        assert_eq!(precommit.four_stage_strategy.id, strategy.artifact_id);
        assert_eq!(precommit.replay_receipt.id, replay_receipt.receipt_id);
        assert_eq!(
            precommit.replay_capabilities_sha256,
            canonical_json_hash(&replay_receipt.capabilities).unwrap()
        );
        assert_eq!(
            precommit.holdout_id,
            fixture.mission.spec.holdout.holdout_id
        );
        assert_eq!(
            precommit.implementation_source_revision,
            BUILD_SOURCE_REVISION
        );
        assert!(!precommit.deployment_authority);
        assert!(!precommit.order_submission_authority);
        let stored_precommit = store
            .get_registry_revision(&precommit.precommit_id)
            .unwrap();
        assert_eq!(stored_precommit.registry_kind, "cex_final_precommit");
        assert_eq!(
            stored_precommit.payload,
            serde_json::to_value(&precommit).unwrap()
        );
        let mut failed_replay_receipt = replay_receipt.clone();
        failed_replay_receipt.gate.passed = false;
        failed_replay_receipt
            .gate
            .failures
            .push("test replay failure".to_string());
        let failed_replay_receipt = failed_replay_receipt.finalize().unwrap();
        let failed_replay_payload = serde_json::to_value(&failed_replay_receipt).unwrap();
        let failed_replay = CexResearchContentRefV1 {
            id: failed_replay_receipt.receipt_id,
            content_sha256: canonical_json_hash(&failed_replay_payload).unwrap(),
        };
        store
            .put_registry_revision(&RegistryRevision {
                revision_id: failed_replay.id.clone(),
                registry_kind: CEX_EVENT_REPLAY_RECEIPT_REGISTRY_KIND.to_string(),
                asset_id: precommit.mission.id.clone(),
                parent_revision_id: Some(precommit.four_stage_strategy.id.clone()),
                payload: failed_replay_payload,
                created_at: Utc::now(),
            })
            .unwrap();
        let mut drifted_precommit = precommit.clone();
        drifted_precommit.replay_receipt = failed_replay;
        let final_lineage = store.mission_lineage(&precommit.mission.id).unwrap();
        let final_iteration = final_lineage
            .iterations
            .iter()
            .find(|iteration| {
                iteration.candidate_artifact_id.as_deref()
                    == Some(precommit.final_candidate.id.as_str())
            })
            .unwrap();
        let final_candidate = final_lineage
            .candidates
            .iter()
            .find(|candidate| candidate.candidate_id == precommit.final_candidate.id)
            .unwrap();
        let final_evaluation = final_lineage
            .evaluations
            .iter()
            .find(|evaluation| {
                evaluation.record.evaluation_id == precommit.final_walk_forward_evaluation.id
            })
            .unwrap();
        assert!(store
            .put_cex_final_precommit(
                final_iteration,
                &final_candidate.candidate_id,
                &final_candidate.artifact,
                &final_evaluation.record,
                &drifted_precommit,
            )
            .unwrap_err()
            .to_string()
            .contains("exact passing precommit strategy"));

        let claim: CexSealedHoldoutClaimV1 = serde_json::from_slice(
            &std::fs::read(results.join("sealed-holdout-claim.json")).unwrap(),
        )
        .unwrap();
        claim.validate().unwrap();
        let sealed_receipt: RegistryRevision = serde_json::from_slice(
            &std::fs::read(results.join("sealed-holdout-receipt.json")).unwrap(),
        )
        .unwrap();
        let sealed: CandidateEvaluation =
            serde_json::from_value(sealed_receipt.payload["evaluation"].clone()).unwrap();
        sealed.validate().unwrap();
        assert!(sealed.passed);
        assert_eq!(
            sealed_receipt.payload["precommit_id"],
            precommit.precommit_id
        );
        assert_eq!(
            sealed_receipt.payload["holdout_id"],
            fixture.mission.spec.holdout.holdout_id
        );
        assert_eq!(
            store
                .claim_cex_sealed_holdout(&claim, Utc::now())
                .unwrap()
                .unwrap(),
            sealed_receipt
        );
        let mut conflicting_claim = claim.clone();
        conflicting_claim.candidate.content_sha256 = "f".repeat(64);
        assert!(store
            .claim_cex_sealed_holdout(&conflicting_claim, Utc::now())
            .unwrap_err()
            .to_string()
            .contains("does not match the final precommit"));

        let bundle: StrategyBundle =
            serde_json::from_slice(&std::fs::read(results.join("strategy-bundle.json")).unwrap())
                .unwrap();
        bundle.validate().unwrap();
        let promotion: PromotionRecord =
            serde_json::from_slice(&std::fs::read(results.join("promotion-record.json")).unwrap())
                .unwrap();
        promotion.validate(&bundle).unwrap();
        assert_eq!(bundle.candidate_id, precommit.final_candidate.id);
        assert_eq!(
            bundle.candidate_content_hash,
            precommit.final_candidate.content_sha256
        );
        assert_eq!(promotion.sealed_evaluation_id, sealed_receipt.revision_id);
        let alpha_domain::StrategyBundleArtifact::CexFourStage {
            strategy: bundled_strategy,
        } = &bundle.artifact
        else {
            panic!("final CEX bundle must carry the four-stage strategy");
        };
        bundled_strategy.validate().unwrap();
        assert_eq!(bundled_strategy.precommit_id, precommit.precommit_id);
        assert_eq!(
            serde_json::from_str::<CexCombinationResearchArtifactV1>(
                &bundled_strategy.strategy_artifact_json
            )
            .unwrap(),
            strategy
        );
        let mut invalid_structure = bundled_strategy.clone();
        let mut invalid_strategy: serde_json::Value =
            serde_json::from_str(&invalid_structure.strategy_artifact_json).unwrap();
        invalid_strategy["signal"] = serde_json::Value::Null;
        invalid_structure.strategy_artifact_json =
            serde_json::to_string(&invalid_strategy).unwrap();
        invalid_structure.strategy_artifact_sha256 = hex::encode(Sha256::digest(
            invalid_structure.strategy_artifact_json.as_bytes(),
        ));
        assert!(invalid_structure.validate().is_err());
        let mut mismatched_formula = bundled_strategy.clone();
        mismatched_formula.executable_formula = hft_factor_dsl::FactorAst::Terminal(
            hft_factor_dsl::FactorTerminal::Field("mid_price".to_string()),
        );
        mismatched_formula.executable_formula_sha256 =
            canonical_json_hash(&mismatched_formula.executable_formula).unwrap();
        mismatched_formula.validate().unwrap();
        assert!(mismatched_formula
            .validate_against_factor_bank(&factor_bank_typed)
            .is_err());
        assert_eq!(
            store.get_strategy_bundle(&bundle.bundle_id).unwrap(),
            bundle
        );
        assert_eq!(
            store.get_promotion(&promotion.promotion_id).unwrap().record,
            promotion
        );
        assert!(store
            .runtime_attributions_for_mission(&precommit.mission.id)
            .unwrap()
            .is_empty());
        let locked_iteration = ResearchIteration {
            iteration_id: "post-precommit-resume".to_string(),
            mission_id: precommit.mission.id.clone(),
            parent_candidate_ids: vec![],
            engine: EngineKind::Mcts,
            hypothesis: "must remain terminal".to_string(),
            candidate_artifact_id: None,
            evaluation_artifact_id: None,
            budget_usage: SearchBudgetUsage::default(),
            verdict: IterationVerdict::Crash,
            failure_class: Some("resume".to_string()),
            failure_explanation: Some("must fail".to_string()),
            created_at: Utc::now(),
        };
        assert!(store
            .append_iteration(&locked_iteration, None, None)
            .unwrap_err()
            .to_string()
            .contains("search and resume terminal"));

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
        let source_mission_id = precommit.mission.id.clone();
        let source_iterations = store
            .mission_lineage(&source_mission_id)
            .unwrap()
            .iterations
            .len();
        drop(store);

        let db = results.join("alpha.duckdb");
        let signed_feedback =
            ingest_governed_cex_feedback(&db, &results, &source_mission_id, &promotion, &bundle);
        let store = AlphaStore::open(&db).unwrap();
        let admitted = store
            .runtime_attributions_for_mission(&source_mission_id)
            .unwrap();
        assert_eq!(admitted.len(), 6);
        assert!(runtime_stage_is_healthy(
            &admitted,
            &bundle.candidate_id,
            AttributionMode::Paper,
        ));
        assert!(runtime_stage_is_healthy(
            &admitted,
            &bundle.candidate_id,
            AttributionMode::Shadow,
        ));
        assert!(admitted.iter().all(|event| {
            event.mission_id.as_deref() == Some(source_mission_id.as_str())
                && event
                    .account_id
                    .as_deref()
                    .is_none_or(|value| value == "binance-paper-shadow")
                && event
                    .venue
                    .as_deref()
                    .is_none_or(|value| value == "binance")
                && event
                    .symbol
                    .as_deref()
                    .is_none_or(|value| value == "BTCUSDT")
        }));
        assert_eq!(
            store.get_mission(&source_mission_id).unwrap().status,
            MissionStatus::Completed
        );
        assert_eq!(
            store
                .mission_lineage(&source_mission_id)
                .unwrap()
                .iterations
                .len(),
            source_iterations
        );

        let mut later_mission = fixture.mission.clone();
        later_mission.spec.search_lineage_id = "later-signed-runtime-search".to_string();
        later_mission.spec.evidence.extend(
            signed_feedback
                .iter()
                .filter(|signed| signed.event.kind == AttributionKind::PortfolioSnapshot)
                .map(|signed| CexResearchEvidenceRefV1 {
                    evidence_id: signed.event.event_id.clone(),
                    kind: match signed.event.mode {
                        AttributionMode::Paper => CexResearchEvidenceKindV1::SignedPaper,
                        AttributionMode::Shadow => CexResearchEvidenceKindV1::SignedShadow,
                        AttributionMode::LiveSmall => unreachable!(),
                    },
                    source_mission_id: source_mission_id.clone(),
                    source_search_lineage_id: fixture.mission.spec.search_lineage_id.clone(),
                    artifact_sha256: signed.content_hash.clone(),
                    signature: Some(CexResearchEvidenceSignatureV1 {
                        key_id: signed.key_id.clone(),
                        signature_sha256: hex::encode(Sha256::digest(
                            hex::decode(&signed.signature_hex).unwrap(),
                        )),
                    }),
                    holdout_id: None,
                }),
        );
        later_mission.validate().unwrap();
        assert_ne!(later_mission.semantic_id().unwrap(), source_mission_id);
        drop(store);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn existing_claim_blocks_before_research_starts() {
        let mut fixture = fixture("duplicate-remote-holdout-claim");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        std::fs::write(&fixture.args.holdout_claim_put_url, b"already-claimed").unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(
            format!("{error:#}").contains("Mission is terminal and inconclusive"),
            "unexpected error: {error:#}"
        );
        assert!(!fixture.args.work_dir.exists());
        assert!(!fixture
            .args
            .work_dir
            .join("results/sealed-holdout-receipt.json")
            .exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn shared_holdout_claim_blocks_a_different_mission_before_research_starts() {
        let mut fixture = fixture("shared-holdout-across-missions");
        let first_mission_id = fixture.args.mission_id.clone();
        let shared_claim = fixture.args.holdout_claim_put_url.clone();
        fixture.mission.spec.search_lineage_id = "different-search-lineage".to_string();
        write_mission(&mut fixture);

        assert_ne!(fixture.args.mission_id, first_mission_id);
        assert_eq!(fixture.args.holdout_claim_put_url, shared_claim);
        std::fs::write(&shared_claim, b"already-claimed").unwrap();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(
            format!("{error:#}").contains("Mission is terminal and inconclusive"),
            "unexpected error: {error:#}"
        );
        assert!(!fixture.args.work_dir.exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn failed_sealed_holdout_keeps_precommit_but_creates_no_deployable_lineage() {
        let mut fixture = fixture("failed-final-holdout");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features_indexed(&mut fixture, |index, row| {
            let direction = if index % 2 == 0 { 1.0 } else { -1.0 };
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = if index < 130 {
                direction * 0.001
            } else {
                -direction * 0.001
            };
        });

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let strategy: CexCombinationResearchArtifactV1 = serde_json::from_slice(
            &std::fs::read(results.join("combination-walk-forward.json")).unwrap(),
        )
        .unwrap();
        assert!(strategy.walk_forward_evidence.selected.evaluation.passed);
        let precommit: CexFinalPrecommitV1 =
            serde_json::from_slice(&std::fs::read(results.join("final-precommit.json")).unwrap())
                .unwrap();
        precommit.validate().unwrap();
        let receipt: RegistryRevision = serde_json::from_slice(
            &std::fs::read(results.join("sealed-holdout-receipt.json")).unwrap(),
        )
        .unwrap();
        let sealed: CandidateEvaluation =
            serde_json::from_value(receipt.payload["evaluation"].clone()).unwrap();
        sealed.validate().unwrap();
        assert!(!sealed.passed);
        assert!(!sealed.failure_reasons.is_empty());
        assert!(!results.join("strategy-bundle.json").exists());
        assert!(!results.join("promotion-record.json").exists());
        let report: CexFinalizationReportV1 = serde_json::from_slice(
            &std::fs::read(results.join("finalization-report.json")).unwrap(),
        )
        .unwrap();
        assert!(!report.sealed_passed);
        assert_eq!(report.strategy_bundle_id, None);
        assert_eq!(report.promotion_id, None);
        let store = AlphaStore::open(results.join("alpha.duckdb")).unwrap();
        assert!(matches!(
            store.get_promotion(&format!("promotion:{}", precommit.final_candidate.id)),
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.get_strategy_bundle(&format!("bundle:{}", precommit.final_candidate.id)),
            Err(StoreError::NotFound)
        ));
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
        bind_result_attempt(&mut fixture, "resumed");
        fixture.args.resume_url = Some(resume_path.to_string_lossy().into_owned());
        fixture.args.resume_sha256 = Some(resume_content_sha256);
        let error = execute(fixture.args.clone()).unwrap_err();
        assert!(format!("{error:#}").contains("Mission is terminal and inconclusive"));
        assert!(!fixture.args.work_dir.exists());
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
        fixture.mission.spec.search.multiple_testing_trials = 6;
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
        let dataset_manifest: CexReplayDatasetManifestV5 = serde_json::from_slice(
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
        fixture.mission.spec.search.multiple_testing_trials = 12;
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
    fn subset_mcts_runs_with_failed_baselines_when_factor_bank_is_nonempty() {
        let mut fixture = fixture("failed-baseline-still-runs-subset");
        fixture.mission.spec.search.seed = 1;
        fixture.mission.spec.search.budget.max_candidates = 6;
        fixture.mission.spec.search.budget.max_expansions = 6;
        fixture.mission.spec.search.multiple_testing_trials = 12;
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        rebind_mission_inputs(&mut fixture);

        execute(fixture.args.clone()).unwrap();

        let results = fixture.args.work_dir.join("results");
        let original_factor_bank: CexFactorBankRevisionV2 =
            serde_json::from_slice(&std::fs::read(results.join("factor-bank.json")).unwrap())
                .unwrap();
        let accepted_attempt = original_factor_bank
            .attempts
            .iter()
            .find(|attempt| attempt.verdict == CexFactorScreeningVerdictV1::Accepted)
            .cloned()
            .expect("fixture must produce an accepted GP attempt");
        let rejected_attempt = original_factor_bank
            .attempts
            .iter()
            .find(|attempt| {
                attempt.verdict == CexFactorScreeningVerdictV1::Rejected
                    && attempt.evaluation.is_some()
            })
            .cloned()
            .expect("fixture must produce an evaluated rejected GP attempt");
        let mut synthetic_attempt = rejected_attempt;
        synthetic_attempt.verdict = CexFactorScreeningVerdictV1::Accepted;
        let mut borrowed_evaluation = accepted_attempt.evaluation.clone().unwrap();
        borrowed_evaluation.candidate_id = synthetic_attempt.candidate_id.clone();
        borrowed_evaluation.candidate_ast_sha256 = synthetic_attempt.ast_sha256.clone();
        synthetic_attempt.evaluation = Some(borrowed_evaluation);
        synthetic_attempt.rejection_codes.clear();
        synthetic_attempt.rejection_details.clear();
        synthetic_attempt.post_warmup_coverage_rows = accepted_attempt.post_warmup_coverage_rows;
        let factor_bank = CexFactorBankRevisionV2::new(
            original_factor_bank.search_lineage_id.clone(),
            original_factor_bank.gp_policy.clone(),
            original_factor_bank.screening_policy.clone(),
            original_factor_bank.evaluation_policy.clone(),
            original_factor_bank.research_dataset.clone(),
            original_factor_bank.walk_forward_partition.clone(),
            vec![synthetic_attempt],
        )
        .unwrap();
        assert!(!factor_bank.entries.is_empty());

        let baseline_policy: CexBaselinePolicyV1 =
            serde_json::from_slice(&std::fs::read(results.join("baseline-policy.json")).unwrap())
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
        let failed_baselines = evaluate_cex_baselines(
            &dataset.engine_context(),
            &factor_bank,
            &baseline_policy,
            &fixture.mission.semantic_id().unwrap(),
            fixture.mission.spec.hypotheses[0].target.clone(),
            &fixture.mission.spec.policies.evaluation,
        )
        .unwrap();
        assert!(!failed_baselines.gate.passed);
        assert!(failed_baselines.ridge.is_some());
        assert!(failed_baselines.cart.is_some());

        let subset_results = fixture.root.join("failed-baseline-subset-results");
        std::fs::create_dir_all(&subset_results).unwrap();
        let selection = run_factor_bank_subset_search(
            &subset_results,
            &fixture.mission,
            &factor_bank,
            &failed_baselines,
            &dataset.engine_context(),
            None,
        )
        .unwrap();

        assert!(selection.is_none());
        assert!(subset_results
            .join("factor-subset-mcts-checkpoint.json")
            .exists());
        assert!(subset_results
            .join("factor-subset-mcts-trace.json")
            .exists());
        assert!(subset_results
            .join("factor-subset-mcts-result.json")
            .exists());
        assert!(!subset_results
            .join("combination-walk-forward.json")
            .exists());
        assert!(!subset_results
            .join("cex-event-replay-receipt.json")
            .exists());
        assert!(!subset_results.join("final-precommit.json").exists());
        assert!(!subset_results.join("sealed-holdout-claim.json").exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_is_deterministic_and_ignores_holdout_only_values() {
        let mut fixture = fixture("gp-holdout-isolation");
        fixture.mission.spec.search.budget.max_candidates = 4;
        fixture.mission.spec.search.budget.max_expansions = 4;
        fixture.mission.spec.search.max_new_iterations = 1;
        fixture.mission.spec.search.multiple_testing_trials = 8;
        resign_mission(&mut fixture);
        execute(fixture.args.clone()).unwrap();
        let first = read_factor_bank(&fixture.args);

        fixture.mission.spec.search.max_new_iterations = 4;
        resign_mission(&mut fixture);
        fixture.args.work_dir = fixture.root.join("work-2");
        bind_result_attempt(&mut fixture, "2");
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
        bind_result_attempt(&mut fixture, "3");
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
    fn execute_rejects_a_noncanonical_result_attempt_before_side_effects() {
        let mut fixture = fixture("noncanonical-result-attempt");
        let result = fixture.root.join(format!(
            "mission-id={}/attempt-test.zip",
            fixture.args.mission_id
        ));
        fixture.args.result_put_url = result.to_string_lossy().into_owned();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();

        let error = execute(fixture.args.clone()).unwrap_err();

        assert!(format!("{error:#}").contains("attempt=<id>/results.zip"));
        assert!(!fixture.args.work_dir.exists());
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
    fn publish_result_rejects_a_bundle_larger_than_the_readback_limit() {
        let root = tempfile::tempdir().expect("create result publication test root");
        let bundle = root.path().join("oversized-result.zip");
        File::create(&bundle)
            .unwrap()
            .set_len(MAX_RESULT_BUNDLE_BYTES + 1)
            .unwrap();
        let destination = root.path().join("published-result.zip");
        let client = Client::builder().build().unwrap();

        let error = publish_result(&client, &destination.to_string_lossy(), &bundle).unwrap_err();

        assert!(error
            .to_string()
            .contains("result bundle exceeds the allowed size"));
        assert!(!destination.exists());
    }

    #[test]
    fn validate_holdout_claim_binding_uses_the_global_remote_claim_object() {
        let mut fixture = fixture("remote-global-holdout-claim");
        fixture.args.result_put_url = "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/anything/results.zip".to_string();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();
        fixture.args.holdout_claim_put_url =
            prediction_dispatch::cex_global_holdout_claim_object(&fixture.args.holdout_id).unwrap();
        fixture.args.holdout_claim_readback_url = fixture.args.holdout_claim_put_url.clone();

        validate_holdout_claim_binding(
            &fixture.args,
            &fixture.args.holdout_id,
            &ExecutionBinding::Direct,
        )
        .unwrap();

        fixture.args.holdout_claim_put_url =
            "https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/not-global/sealed-holdout-claim.json".to_string();
        fixture.args.holdout_claim_readback_url = fixture.args.holdout_claim_put_url.clone();
        let error = validate_holdout_claim_binding(
            &fixture.args,
            &fixture.args.holdout_id,
            &ExecutionBinding::Campaign {
                campaign_id: "cex-campaign-test".to_string(),
                round_id: "r1".to_string(),
                request_sha256: "a".repeat(64),
            },
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("CEX holdout claim object must use the global holdout claim path"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn recover_execution_report_from_an_existing_campaign_result_bundle() {
        let mut fixture = fixture("recover-campaign-result");
        let binding = campaign_binding_fixture(&mut fixture);
        let original = execute_report(fixture.args.clone(), binding.clone()).unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let recovered = recover_execution_report_from_published_result(
            &client,
            fixture.args.result_readback_url.as_str(),
            &fixture.root.join("recovered-results.zip"),
            &fixture.args.mission_id,
            &fixture.args.mission_sha256,
            &binding,
        )
        .unwrap()
        .expect("existing published result must recover");

        assert_eq!(recovered.mission_id, original.mission_id);
        assert_eq!(recovered.mission_sha256, original.mission_sha256);
        assert_eq!(recovered.campaign_id, original.campaign_id);
        assert_eq!(recovered.round_id, original.round_id);
        assert_eq!(recovered.request_sha256, original.request_sha256);
        assert_eq!(recovered.bundle_bytes, original.bundle_bytes);
        assert_eq!(recovered.bundle_sha256, original.bundle_sha256);
        assert_eq!(
            recovered.readback_bundle_sha256,
            original.readback_bundle_sha256
        );
        assert_eq!(recovered.replay_receipt_id, original.replay_receipt_id);
        assert_eq!(recovered.replay_gate_passed, original.replay_gate_passed);
        assert_eq!(recovered.final_precommit_id, original.final_precommit_id);
        assert_eq!(recovered.sealed_receipt_id, original.sealed_receipt_id);
        assert_eq!(recovered.sealed_passed, original.sealed_passed);
        assert_eq!(recovered.strategy_bundle_id, original.strategy_bundle_id);
        assert_eq!(recovered.promotion_id, original.promotion_id);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn recover_execution_report_accepts_a_renewed_campaign_request_sha() {
        let mut fixture = fixture("recover-campaign-binding-mismatch");
        let binding = campaign_binding_fixture(&mut fixture);
        let original = execute_report(fixture.args.clone(), binding).unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let renewed_binding = ExecutionBinding::Campaign {
            campaign_id: "cex-campaign-test".to_string(),
            round_id: "r1".to_string(),
            request_sha256: "b".repeat(64),
        };
        let recovered = recover_execution_report_from_published_result(
            &client,
            fixture.args.result_readback_url.as_str(),
            &fixture.root.join("recovered-mismatch.zip"),
            &fixture.args.mission_id,
            &fixture.args.mission_sha256,
            &renewed_binding,
        )
        .unwrap()
        .expect("renewed request SHA should still recover the same Campaign result");

        assert_eq!(recovered.mission_id, original.mission_id);
        assert_eq!(recovered.campaign_id, original.campaign_id);
        assert_eq!(recovered.round_id, original.round_id);
        assert_eq!(recovered.request_sha256, original.request_sha256);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn recover_execution_report_rejects_a_result_bundle_for_another_campaign_binding() {
        let mut fixture = fixture("recover-campaign-binding-mismatch");
        let binding = campaign_binding_fixture(&mut fixture);
        execute_report(fixture.args.clone(), binding).unwrap();
        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let wrong_binding = ExecutionBinding::Campaign {
            campaign_id: "cex-campaign-other".to_string(),
            round_id: "r1".to_string(),
            request_sha256: "b".repeat(64),
        };
        let error = recover_execution_report_from_published_result(
            &client,
            fixture.args.result_readback_url.as_str(),
            &fixture.root.join("recovered-mismatch.zip"),
            &fixture.args.mission_id,
            &fixture.args.mission_sha256,
            &wrong_binding,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("published result bundle does not match the Campaign binding"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn recover_execution_report_rejects_mixed_foreign_finalization_evidence() {
        let mut primary = finalizing_fixture("recover-campaign-foreign-finalization");
        let binding = campaign_binding_fixture(&mut primary);
        execute_report(primary.args.clone(), binding.clone()).unwrap();

        let mut foreign = finalizing_fixture("recover-campaign-foreign-source");
        let foreign_binding = campaign_binding_fixture(&mut foreign);
        execute_report(foreign.args.clone(), foreign_binding).unwrap();

        let foreign_finalize = foreign.root.join("foreign-finalize");
        finalize_existing_search_round(
            &foreign.args.work_dir,
            &foreign_finalize,
            &foreign.args.holdout_claim_put_url,
            &foreign.args.holdout_claim_readback_url,
            &foreign.mission,
        )
        .unwrap();
        rewrite_bundle_entry_bytes(
            &primary.result_path,
            "results/finalization-report.json",
            std::fs::read(foreign_finalize.join("finalization-report.json")).unwrap(),
        );
        rewrite_bundle_entry_bytes(
            &primary.result_path,
            "results/final-precommit.json",
            std::fs::read(foreign_finalize.join("final-precommit.json")).unwrap(),
        );
        rewrite_bundle_entry_bytes(
            &primary.result_path,
            "results/sealed-holdout-receipt.json",
            std::fs::read(foreign_finalize.join("sealed-holdout-receipt.json")).unwrap(),
        );

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let error = recover_execution_report_from_published_result(
            &client,
            primary.args.result_readback_url.as_str(),
            &primary.root.join("recovered-foreign-finalization.zip"),
            &primary.args.mission_id,
            &primary.args.mission_sha256,
            &binding,
        )
        .unwrap_err();

        assert!(error.to_string().contains(
            "published result bundle precommit does not match the exact Campaign Mission"
        ));
        std::fs::remove_dir_all(primary.root).unwrap();
        std::fs::remove_dir_all(foreign.root).unwrap();
    }

    #[test]
    fn recover_execution_report_rejects_a_foreign_strategy_for_the_replay_receipt() {
        let mut primary = finalizing_fixture("recover-campaign-foreign-strategy");
        let binding = campaign_binding_fixture(&mut primary);
        execute_report(primary.args.clone(), binding.clone()).unwrap();

        let mut foreign = finalizing_fixture("recover-campaign-foreign-strategy-source");
        let foreign_binding = campaign_binding_fixture(&mut foreign);
        execute_report(foreign.args.clone(), foreign_binding).unwrap();
        rewrite_bundle_entry_bytes(
            &primary.result_path,
            "results/combination-walk-forward.json",
            std::fs::read(
                foreign
                    .args
                    .work_dir
                    .join("results/combination-walk-forward.json"),
            )
            .unwrap(),
        );

        let client = Client::builder().redirect(Policy::none()).build().unwrap();
        let error = recover_execution_report_from_published_result(
            &client,
            primary.args.result_readback_url.as_str(),
            &primary.root.join("recovered-foreign-strategy.zip"),
            &primary.args.mission_id,
            &primary.args.mission_sha256,
            &binding,
        )
        .unwrap_err();

        assert!(error.to_string().contains("exact selected strategy"));
        std::fs::remove_dir_all(primary.root).unwrap();
        std::fs::remove_dir_all(foreign.root).unwrap();
    }

    fn campaign_binding_fixture(fixture: &mut Fixture) -> ExecutionBinding {
        let campaign_id = "cex-campaign-test";
        let round_id = "r1";
        let request_sha256 = "a".repeat(64);
        let result_path = fixture.root.join(format!(
            "campaign-root/campaign-id={campaign_id}/round={round_id}/results.zip"
        ));
        std::fs::create_dir_all(result_path.parent().unwrap()).unwrap();
        let holdout_claim = prediction_dispatch::cex_campaign_round_result_and_holdout_claim(
            &result_path.to_string_lossy(),
            campaign_id,
            round_id,
            &fixture.args.holdout_id,
        )
        .unwrap();
        let holdout_claim_path = PathBuf::from(holdout_claim);
        std::fs::create_dir_all(holdout_claim_path.parent().unwrap()).unwrap();
        fixture.result_path = result_path.clone();
        fixture.args.result_put_url = result_path.to_string_lossy().into_owned();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();
        fixture.args.holdout_claim_put_url = holdout_claim_path.to_string_lossy().into_owned();
        fixture.args.holdout_claim_readback_url = fixture.args.holdout_claim_put_url.clone();
        ExecutionBinding::Campaign {
            campaign_id: campaign_id.to_string(),
            round_id: round_id.to_string(),
            request_sha256,
        }
    }

    pub(crate) fn finalizing_fixture(name: &str) -> Fixture {
        let mut fixture = fixture(name);
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        fixture
    }

    fn rewrite_bundle_entry_bytes(bundle_path: &Path, entry_name: &str, replacement: Vec<u8>) {
        let mut entries = read_bundle_entries(bundle_path);
        if let Some(entry_index) = entries.iter().position(|(name, _)| name == entry_name) {
            entries[entry_index].1 = replacement;
        } else {
            entries.push((entry_name.to_string(), replacement));
        }
        rewrite_bundle_entries(bundle_path, entries);
    }

    fn read_bundle_entries(bundle_path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut archive = ZipArchive::new(File::open(bundle_path).unwrap()).unwrap();
        let mut entries = Vec::with_capacity(archive.len());
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            entries.push((entry.name().to_string(), bytes));
        }
        entries
    }

    fn rewrite_bundle_entries(bundle_path: &Path, entries: Vec<(String, Vec<u8>)>) {
        let rewritten = bundle_path.with_extension("rewritten.zip");
        let file = File::create(&rewritten).unwrap();
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, bytes) in entries {
            writer.start_file(name, options).unwrap();
            writer.write_all(&bytes).unwrap();
        }
        writer.finish().unwrap();
        std::fs::rename(rewritten, bundle_path).unwrap();
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

    pub(crate) struct Fixture {
        pub(crate) root: PathBuf,
        pub(crate) feature_path: PathBuf,
        pub(crate) mission_path: PathBuf,
        pub(crate) materialization_path: PathBuf,
        pub(crate) replay_artifact_path: PathBuf,
        pub(crate) replay_manifest_path: PathBuf,
        pub(crate) result_path: PathBuf,
        pub(crate) mission: CexResearchMissionArtifactV1,
        pub(crate) materialization: serde_json::Value,
        pub(crate) args: ExecuteMissionArgs,
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
                (first_feature_time + ChronoDuration::seconds(index - 1)).timestamp_micros() + 100
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
                    series_id: 1,
                    event_time,
                    feature_available_time: event_time,
                    label_available_time: event_time + ChronoDuration::seconds(5),
                    ingestion_time,
                    symbol: "BTCUSDT".to_string(),
                    source_revisions: BTreeMap::from([(
                        "binance-usdm-lob".to_string(),
                        source_revision.clone(),
                    )]),
                    modalities: BTreeSet::from([DataModality::Lob, DataModality::TradeTick]),
                    features: BTreeMap::from([
                        ("ask_depth_top5".to_string(), 10.0),
                        ("bid_depth_top5".to_string(), 10.0),
                        ("book_imbalance".to_string(), index as f64 / 100.0),
                        ("mid_price".to_string(), 60_000.0),
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
        let instrument_rules_evidence = (0..256).map(indexed_cex_triplet).collect::<Vec<_>>();
        let snapshot = CexReplaySnapshotV5 {
            schema_version: hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V5.to_string(),
            venue: "binance".to_string(),
            instrument_type: "usdm".to_string(),
            symbol: "BTCUSDT".to_string(),
            replay_clock: hft_research_manifest::CEX_REPLAY_CLOCK_RECEIVED_AT_NS.to_string(),
            required_modalities: BTreeSet::from([
                hft_research_manifest::CEX_MODALITY_LOB.to_string(),
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE.to_string(),
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
                evidence: instrument_rules_evidence.clone(),
            },
            series: vec![hft_research_manifest::CexReplaySeriesV1 {
                series_id: 1,
                first_event_time,
                last_event_time,
                instrument_rules_coverage: hft_research_manifest::CexPitSeriesEvidenceV2 {
                    evidence: instrument_rules_evidence,
                    first_available_at: first_event_time - ChronoDuration::seconds(1),
                    last_available_at: last_event_time + ChronoDuration::seconds(5),
                    observations: 256,
                    max_gap_ns: hft_research_manifest::CEX_DERIVATIVES_MAX_GAP_NS,
                },
            }],
        };
        let snapshot_sha256 = snapshot.sha256();
        let materialization = serde_json::json!({
            "dataset_kind": MATERIALIZATION_KIND,
            "schema_version": BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V7,
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
            "series_count": 1,
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
            multiple_testing_trials: 2,
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
            CexReplayDatasetManifestV5::new(format!("dataset-{feature_sha256}"), snapshot.clone())
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
        let mission_id = mission.semantic_id().unwrap();
        let mission_path = root.join("mission.json");
        std::fs::write(&mission_path, serde_json::to_vec_pretty(&mission).unwrap()).unwrap();
        let mission_result_dir = root.join(format!("mission-id={mission_id}"));
        let attempt_result_dir = mission_result_dir.join("attempt=test");
        std::fs::create_dir_all(&attempt_result_dir).unwrap();
        let result_path = attempt_result_dir.join("results.zip");
        let (_, holdout_claim_path) = prediction_dispatch::cex_result_attempt_and_holdout_claim(
            &result_path.to_string_lossy(),
            &mission_id,
            &mission.spec.holdout.holdout_id,
        )
        .unwrap();
        let holdout_claim_path = PathBuf::from(holdout_claim_path);
        std::fs::create_dir_all(holdout_claim_path.parent().unwrap()).unwrap();
        let args = ExecuteMissionArgs {
            work_dir: root.join("work-1"),
            mission_id,
            holdout_id: mission.spec.holdout.holdout_id.clone(),
            mission_url: mission_path.to_string_lossy().into_owned(),
            mission_sha256: sha256_file(&mission_path).unwrap(),
            feature_url: feature_path.to_string_lossy().into_owned(),
            materialization_url: materialization_path.to_string_lossy().into_owned(),
            replay_artifact_url: replay_artifact_path.to_string_lossy().into_owned(),
            replay_artifact_sha256,
            replay_manifest_url: replay_manifest_path.to_string_lossy().into_owned(),
            replay_manifest_sha256,
            resume_url: None,
            resume_sha256: None,
            result_put_url: result_path.to_string_lossy().into_owned(),
            result_readback_url: result_path.to_string_lossy().into_owned(),
            holdout_claim_put_url: holdout_claim_path.to_string_lossy().into_owned(),
            holdout_claim_readback_url: holdout_claim_path.to_string_lossy().into_owned(),
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

    fn insert_historical_fee_schedule(snapshot: &mut serde_json::Map<String, serde_json::Value>) {
        let first_event_time: chrono::DateTime<Utc> =
            serde_json::from_value(snapshot["first_event_time"].clone()).unwrap();
        let last_event_time: chrono::DateTime<Utc> =
            serde_json::from_value(snapshot["last_event_time"].clone()).unwrap();
        snapshot.insert(
            "fee_schedule".to_string(),
            serde_json::json!({
                "runtime_account_id": "binance-main",
                "account_fingerprint": "9".repeat(64),
                "maker_buy_fee_bps": "2",
                "maker_sell_fee_bps": "2",
                "taker_buy_fee_bps": "5",
                "taker_sell_fee_bps": "5",
                "available_at": first_event_time - ChronoDuration::seconds(1),
                "valid_through": last_event_time + ChronoDuration::seconds(5),
                "evidence": [cex_triplet('5')]
            }),
        );
    }

    fn insert_historical_derivatives_reference(
        snapshot: &mut serde_json::Map<String, serde_json::Value>,
    ) {
        let first_event_time: chrono::DateTime<Utc> =
            serde_json::from_value(snapshot["first_event_time"].clone()).unwrap();
        let last_event_time: chrono::DateTime<Utc> =
            serde_json::from_value(snapshot["last_event_time"].clone()).unwrap();
        snapshot.insert(
            "required_modalities".to_string(),
            serde_json::json!([
                hft_research_manifest::CEX_MODALITY_AGGREGATE_TRADE,
                hft_research_manifest::CEX_MODALITY_FUNDING,
                hft_research_manifest::CEX_MODALITY_LOB,
                hft_research_manifest::CEX_MODALITY_OPEN_INTEREST
            ]),
        );
        snapshot.insert(
            "derivatives_reference".to_string(),
            serde_json::json!({
                "funding": {
                    "evidence": [cex_triplet('6'), cex_triplet('a'), cex_triplet('b')],
                    "first_available_at": first_event_time - ChronoDuration::seconds(1),
                    "last_available_at": last_event_time + ChronoDuration::seconds(5),
                    "observations": 3,
                    "max_gap_ns": 90_000_000_000u64,
                },
                "open_interest": {
                    "evidence": [cex_triplet('7'), cex_triplet('c'), cex_triplet('d')],
                    "first_available_at": first_event_time - ChronoDuration::seconds(1),
                    "last_available_at": last_event_time + ChronoDuration::seconds(5),
                    "observations": 3,
                    "max_gap_ns": 90_000_000_000u64,
                },
                "evaluation_funding_bps_per_bucket": "0"
            }),
        );
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
        snapshot.remove("series");
        for field in ["instrument_rules", "fee_schedule", "derivatives_reference"] {
            snapshot.remove(field);
        }

        let error = decode_materialization(&serde_json::to_vec(&value).unwrap()).unwrap_err();

        assert!(error
            .to_string()
            .contains("historical V2 materialization is read-only"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn historical_v3_materialization_decodes_read_only() {
        let fixture = fixture("historical-v3-read-only");
        let mut value = fixture.materialization.clone();
        value["schema_version"] = serde_json::json!(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V3);
        let snapshot = value["snapshot"].as_object_mut().unwrap();
        snapshot.insert(
            "schema_version".to_string(),
            serde_json::json!(hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V2),
        );
        snapshot.remove("series");
        insert_historical_fee_schedule(snapshot);
        insert_historical_derivatives_reference(snapshot);
        let first_event_time: chrono::DateTime<Utc> =
            serde_json::from_value(snapshot["first_event_time"].clone()).unwrap();
        snapshot.insert(
            "latency_cost".to_string(),
            serde_json::json!({
                "method": "verified_order_lifecycle_realized_slippage",
                "venue": "binance",
                "symbol": "BTCUSDT",
                "runtime_account_id": "binance-main",
                "account_fingerprint": "9".repeat(64),
                "evidence": cex_triplet('8'),
                "first_observed_at": first_event_time - ChronoDuration::seconds(2),
                "last_observed_at": first_event_time - ChronoDuration::seconds(1),
                "available_at": first_event_time - ChronoDuration::seconds(1),
                "observations": 160,
                "p50_ns": 1_000_000,
                "p95_ns": 2_000_000,
                "p99_ns": 3_000_000,
                "p50_cost_bps": "0.1",
                "p95_cost_bps": "0.5",
                "p99_cost_bps": "0.6"
            }),
        );

        let error = decode_materialization(&serde_json::to_vec(&value).unwrap()).unwrap_err();

        assert!(error
            .to_string()
            .contains("historical V3 materialization is read-only"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn historical_v4_materialization_decodes_read_only() {
        let fixture = fixture("historical-v4-read-only");
        let mut value = fixture.materialization.clone();
        value["schema_version"] = serde_json::json!(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V4);
        let snapshot = value["snapshot"].as_object_mut().unwrap();
        snapshot.insert(
            "schema_version".to_string(),
            serde_json::json!(hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V3),
        );
        snapshot.remove("series");
        insert_historical_fee_schedule(snapshot);
        insert_historical_derivatives_reference(snapshot);

        let error = decode_materialization(&serde_json::to_vec(&value).unwrap()).unwrap_err();

        assert!(error
            .to_string()
            .contains("historical V4 materialization is read-only"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn historical_v5_materialization_decodes_read_only() {
        let fixture = fixture("historical-v5-read-only");
        let mut value = fixture.materialization.clone();
        value["schema_version"] = serde_json::json!(BINANCE_LOB_PIT_MATERIALIZATION_SCHEMA_V5);
        let snapshot = value["snapshot"].as_object_mut().unwrap();
        snapshot.insert(
            "schema_version".to_string(),
            serde_json::json!(hft_research_manifest::CEX_REPLAY_SNAPSHOT_SCHEMA_V4),
        );
        snapshot.remove("series");
        insert_historical_derivatives_reference(snapshot);

        let error = decode_materialization(&serde_json::to_vec(&value).unwrap()).unwrap_err();

        assert!(error
            .to_string()
            .contains("historical V5 materialization is read-only"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn canonical_target_position_decisions_insert_series_end_closes() {
        let start = chrono::DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let clocks = vec![
            data_mission::FeatureDecisionClock {
                series_id: 1,
                feature_available_time: start,
                series_close_time: start + ChronoDuration::seconds(5),
            },
            data_mission::FeatureDecisionClock {
                series_id: 1,
                feature_available_time: start + ChronoDuration::seconds(1),
                series_close_time: start + ChronoDuration::seconds(6),
            },
            data_mission::FeatureDecisionClock {
                series_id: 2,
                feature_available_time: start + ChronoDuration::seconds(10),
                series_close_time: start + ChronoDuration::seconds(15),
            },
        ];

        let (decisions, ordinary) =
            canonical_target_position_decisions(&clocks, vec![1.0, -1.0, 1.0]).unwrap();

        assert_eq!(ordinary, 3);
        assert_eq!(
            decisions
                .iter()
                .map(|decision| (decision.timestamp_us, decision.target_position))
                .collect::<Vec<_>>(),
            vec![
                (start.timestamp_micros(), 1.0),
                (
                    (start + ChronoDuration::seconds(1)).timestamp_micros(),
                    -1.0
                ),
                ((start + ChronoDuration::seconds(6)).timestamp_micros(), 0.0),
                (
                    (start + ChronoDuration::seconds(10)).timestamp_micros(),
                    1.0
                ),
                (
                    (start + ChronoDuration::seconds(15)).timestamp_micros(),
                    0.0
                ),
            ]
        );
    }

    #[test]
    fn campaign_override_mission_id_reaches_replay_receipt() {
        let mut fixture = fixture("campaign-override-replay-receipt");
        fixture.mission.spec.feature_fields = vec!["book_imbalance".to_string()];
        rewrite_features(&mut fixture, |row| {
            let direction = row.label.signum();
            row.features.insert("book_imbalance".to_string(), direction);
            row.label = direction * 0.001;
        });
        execute(fixture.args.clone()).unwrap();

        let results_dir = fixture.args.work_dir.join("results");
        let mut override_mission = fixture.mission.clone();
        let declared_total_trials = override_mission
            .spec
            .search
            .planned_gp_and_subset_trials()
            .unwrap()
            + 1;
        override_mission.spec.search.multiple_testing_trials = declared_total_trials;
        override_mission.spec.policies.subset_search.content_sha256 =
            canonical_json_hash(&override_mission.spec.search).unwrap();
        override_mission.spec.policies.screening.content_sha256 = canonical_json_hash(
            &FormulaEvaluatorConfig::for_trials(declared_total_trials).unwrap(),
        )
        .unwrap();
        override_mission.validate().unwrap();
        let override_mission_id = override_mission.semantic_id().unwrap();
        let store = AlphaStore::open(results_dir.join("alpha.duckdb")).unwrap();
        let feature_manifest: hft_collector::FeatureDatasetManifest = serde_json::from_slice(
            &std::fs::read(results_dir.join("feature-manifest.json")).unwrap(),
        )
        .unwrap();
        let feature_decision_clocks =
            data_mission::feature_decision_clocks(&feature_manifest).unwrap();
        let dataset_manifest = data_mission::read_registered_research_dataset(
            &store,
            &results_dir.join("cex-replay-dataset-manifest.json"),
        )
        .unwrap();
        let baseline_rows = dataset_manifest
            .load_rows(&fixture.mission.spec.evaluation_protocol.costs)
            .unwrap();
        let baseline_dataset =
            prepare_dataset(baseline_rows, &fixture.mission.spec.evaluation_protocol).unwrap();
        let baseline_context = baseline_dataset.engine_context();
        let factor_bank: CexFactorBankRevisionV2 =
            serde_json::from_slice(&std::fs::read(results_dir.join("factor-bank.json")).unwrap())
                .unwrap();
        let strategy: CexCombinationResearchArtifactV1 = serde_json::from_slice(
            &std::fs::read(results_dir.join("combination-walk-forward.json")).unwrap(),
        )
        .unwrap();
        let replay_policy = CexEventReplayPolicyV1::controlled_v1(
            fixture.mission.spec.policies.replay.id.clone(),
            fixture.materialization["top_depth"].as_u64().unwrap() as usize,
            fixture
                .mission
                .spec
                .instrument
                .horizon
                .observation_frequency_millis,
        )
        .unwrap();
        let materialization_sha256 = sha256_file(&fixture.materialization_path).unwrap();
        let materialization =
            decode_materialization(&std::fs::read(&fixture.materialization_path).unwrap()).unwrap();

        let replay_receipt = run_cex_event_replay(
            &results_dir,
            &override_mission_id,
            &override_mission,
            &materialization,
            &materialization_sha256,
            &factor_bank,
            &baseline_context,
            &feature_decision_clocks[..baseline_context.rows().len()],
            &strategy,
            &replay_policy,
            &fixture.replay_artifact_path,
            &fixture.args.replay_artifact_sha256,
            &fixture.replay_manifest_path,
            &fixture.args.replay_manifest_sha256,
        )
        .unwrap();

        assert_eq!(replay_receipt.mission_id, override_mission_id);

        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    fn rewrite_features(fixture: &mut Fixture, mutate: impl Fn(&mut PointInTimeFeatureRow)) {
        rewrite_features_indexed(fixture, |_, row| mutate(row));
    }

    #[test]
    fn bound_gp_policy_uses_the_signed_policy_version() {
        let mut fixture = fixture("dynamic-gp-policy-binding");
        let expected = CexGpPolicyV1::controlled_dynamic_v2(
            fixture.mission.spec.policies.gp.id.clone(),
            fixture.mission.spec.feature_fields.clone(),
            fixture.mission.spec.search.seed,
            &fixture.mission.spec.search.budget,
        )
        .unwrap();
        fixture.mission.spec.policies.gp.content_sha256 = expected.content_hash().unwrap();

        assert_eq!(bound_gp_policy(&fixture.mission).unwrap(), expected);
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    fn rewrite_features_indexed(
        fixture: &mut Fixture,
        mutate: impl Fn(usize, &mut PointInTimeFeatureRow),
    ) {
        let bytes = std::fs::read(&fixture.feature_path).unwrap();
        let mut rows = std::io::BufReader::new(bytes.as_slice())
            .lines()
            .map(|line| serde_json::from_str::<PointInTimeFeatureRow>(&line.unwrap()).unwrap())
            .collect::<Vec<_>>();
        rows.iter_mut()
            .enumerate()
            .for_each(|(index, row)| mutate(index, row));
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
        if let Ok(mission_id) = fixture.mission.semantic_id() {
            fixture.args.mission_id = mission_id;
            bind_result_attempt(fixture, "test");
        }
    }

    fn bind_result_attempt(fixture: &mut Fixture, attempt: &str) {
        let mission_dir = fixture
            .root
            .join(format!("mission-id={}", fixture.args.mission_id));
        let attempt_dir = mission_dir.join(format!("attempt={attempt}"));
        std::fs::create_dir_all(&attempt_dir).unwrap();
        fixture.result_path = attempt_dir.join("results.zip");
        fixture.args.result_put_url = fixture.result_path.to_string_lossy().into_owned();
        fixture.args.result_readback_url = fixture.args.result_put_url.clone();
        let (_, claim_path) = prediction_dispatch::cex_result_attempt_and_holdout_claim(
            &fixture.args.result_put_url,
            &fixture.args.mission_id,
            &fixture.args.holdout_id,
        )
        .unwrap();
        let claim_path = PathBuf::from(claim_path);
        std::fs::create_dir_all(claim_path.parent().unwrap()).unwrap();
        fixture.args.holdout_claim_put_url = claim_path.to_string_lossy().into_owned();
        fixture.args.holdout_claim_readback_url = fixture.args.holdout_claim_put_url.clone();
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
        let snapshot: CexReplaySnapshotV5 =
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
            CexReplayDatasetManifestV5::new(format!("dataset-{feature_sha256}"), snapshot).unwrap();
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
        let snapshot: CexReplaySnapshotV5 =
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
