#[cfg(feature = "scientific")]
use crate::mission_campaign;
use alpha_domain::{
    EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
};
#[cfg(feature = "scientific")]
use anyhow::Context;
use clap::{Args, ValueEnum};
#[cfg(feature = "scientific")]
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[cfg(feature = "operator")]
mod operator;
#[cfg(feature = "operator")]
pub use operator::{run, Cli};

#[cfg(test)]
pub(crate) const BUILD_SOURCE_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
#[cfg(not(test))]
pub(crate) const BUILD_SOURCE_REVISION: &str = match option_env!("MONDAY_SOURCE_REVISION") {
    Some(value) => value,
    None => "unbound-source-revision",
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ModelMetricsBenchmark {
    Ridge,
    Cart,
    BurnMlp,
    None,
}

#[derive(Debug, Clone, Args)]
pub struct ModelMetricsArgs {
    /// Completed supervised backtest JSON files; may span multiple Missions.
    #[arg(long, required = true, num_args = 1..)]
    pub backtest: Vec<PathBuf>,
    /// Optional original model selection JSON files; no selection is inferred.
    #[arg(long, num_args = 1..)]
    pub selection: Vec<PathBuf>,
    /// Benchmark is matched only inside an identical Mission evaluation cohort.
    #[arg(long, value_enum, default_value = "ridge")]
    pub benchmark: ModelMetricsBenchmark,
    /// New JSON report path. An existing identical report may be reused.
    #[arg(long)]
    pub output: PathBuf,
    /// Defaults to the JSON output path with a .csv extension.
    #[arg(long)]
    pub csv_output: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignCloseFamilyArgs {
    #[arg(long)]
    pub ledger: PathBuf,
    #[arg(long)]
    pub signed_grant: PathBuf,
    #[arg(long)]
    pub trusted_keys: PathBuf,
    #[arg(long)]
    pub approval_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignControllerHandoffArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub control: PathBuf,
    /// Existing block-volume root containing the ledger, inputs and cycle checkpoints.
    #[arg(long)]
    pub volume_root: PathBuf,
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub pvc: String,
    /// Existing operator service identity, including its separately configured OSS authority.
    #[arg(long)]
    pub service_account: String,
    /// Existing runtime-owned public-key ConfigMap; projection stays live across key rotation.
    #[arg(long)]
    pub trusted_keys_configmap: String,
    #[arg(long)]
    pub campaign_pod: String,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    /// Original absolute task deadline; re-rendering cannot extend this value.
    #[arg(long)]
    pub deadline_at: chrono::DateTime<chrono::Utc>,
    /// A new private JSON file; signed access URLs are never printed to stdout.
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignControllerPrepareArgs {
    #[arg(long)]
    pub control: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    #[arg(long)]
    pub kubeconfig_out: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct DescribeStudyArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub controller_image: String,
}

#[derive(Debug, Clone, Args)]
pub struct InitStageAuthorityArgs {
    #[arg(long)]
    pub private_key: PathBuf,
    #[arg(long)]
    pub public_out: PathBuf,
    #[arg(long)]
    pub work_pvc_name: String,
    #[arg(long)]
    pub work_pvc_uid: String,
}
#[derive(Debug, Clone, Args)]
pub struct CampaignStageControllerArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub control: PathBuf,
    /// ACK mount of the root of the task-owned work PVC (never given to the worker).
    #[arg(long)]
    pub pvc_root: PathBuf,
    #[arg(long)]
    pub private_key: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    /// Create only the attempt-owned durable directory before canonical submit.
    #[arg(long)]
    pub prepare_only: bool,
    /// Perform one bounded controller iteration; otherwise wait until terminal/deadline.
    #[arg(long)]
    pub once: bool,
}

#[derive(Debug, Clone, Args)]
pub struct MissionDispatchSubmitArgs {
    /// Operator control file; alternatively MONDAY_CAMPAIGN_CONTROL. Required for dispatch.
    #[arg(long)]
    pub control: Option<PathBuf>,
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    /// ACK generation directory containing its already downloaded round-readback cache.
    /// Required for pre-holdout settlement; not accepted by submission.
    #[arg(long)]
    pub readback_cache: Option<PathBuf>,
    /// Lightweight report derived in ACK from the authenticated settled cache.
    #[arg(long, requires = "readback_cache")]
    pub model_report: Option<PathBuf>,
    /// Mounted sequence cohort. Required for sequence settlement; ignored by other submissions.
    #[arg(long)]
    pub input_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct MissionDispatchStatusArgs {
    #[arg(long)]
    pub control: PathBuf,
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
}

#[derive(Debug, Clone, Args)]
pub struct MissionDispatchInspectArgs {
    #[arg(long)]
    pub control: Option<PathBuf>,
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub materialization: PathBuf,
    #[arg(long)]
    pub controller_image: String,
    #[arg(long, default_value_t = 0)]
    pub attempt_ordinal: u32,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignExecuteArgs {
    #[arg(long, requires = "final_trusted_keys", conflicts_with = "pre_holdout")]
    pub final_evaluation: bool,
    #[arg(long, requires = "final_evaluation")]
    pub final_trusted_keys: Option<PathBuf>,
    /// Stop before opening sealed holdout, including for the formula lane.
    /// Required unless `--final-evaluation` supplies an independent grant.
    #[arg(long, required_unless_present = "final_evaluation")]
    pub pre_holdout: bool,
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub campaign_id: String,
    #[arg(long)]
    pub image_identity: String,
    #[arg(long)]
    pub request: PathBuf,
    #[arg(long)]
    pub request_sha256: String,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignWorkflowArgs {
    /// Existing trusted ACK ledger, selected independently of the plan's artifacts.
    #[arg(long)]
    pub ledger: PathBuf,
    #[arg(long)]
    pub plan: PathBuf,
    #[arg(long)]
    pub work_dir: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignPrepareArgs {
    /// Existing trusted ACK ledger whose integrity key authenticates preparation.
    #[arg(long)]
    pub ledger: PathBuf,
    #[arg(long)]
    pub plan: PathBuf,
    #[arg(long)]
    pub output_root: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignPrecheckArgs {
    #[arg(long)]
    pub feature: PathBuf,
    #[arg(long)]
    pub materialization: PathBuf,
    #[arg(long)]
    pub research_plan: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
    #[arg(long)]
    pub research_plan_out: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignFreezeArgs {
    /// Public controller stage authority and task-owned work PVC; required for market studies.
    #[arg(long)]
    pub stage_authority: Option<PathBuf>,
    /// Trusted ledger selected independently of a caller-supplied cache artifact.
    #[arg(long)]
    pub preparation_ledger: Option<PathBuf>,
    /// Reuse a previously produced native freeze, verified against an explicit digest.
    #[arg(
        long,
        requires_all = ["reuse_sha256", "preparation_ledger"],
        conflicts_with = "final_evaluation_control"
    )]
    pub reuse: Option<PathBuf>,
    #[arg(long, requires = "reuse")]
    pub reuse_sha256: Option<String>,
    #[arg(long, conflicts_with_all = ["seeds", "research_plan"])]
    pub final_evaluation_control: Option<PathBuf>,
    #[arg(long)]
    pub campaign_inputs: PathBuf,
    #[arg(long)]
    pub input_root: PathBuf,
    #[arg(long)]
    pub source_revision: String,
    #[arg(long)]
    pub image: String,
    #[arg(long)]
    pub campaign_root: String,
    #[arg(long = "seed", required_unless_present = "final_evaluation_control")]
    pub seeds: Vec<u64>,
    #[arg(long)]
    pub research_plan: Option<PathBuf>,
    /// Optional authenticated next-family proposal. Freeze binds it into the signed request.
    #[arg(long)]
    pub study_proposal: Option<PathBuf>,
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignLearnArgs {
    /// Existing signed dispatch control and authenticated settlement ledger.
    #[arg(long)]
    pub control: PathBuf,
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub settlement: PathBuf,
    #[arg(long)]
    pub namespace: String,
    #[arg(long)]
    pub request: PathBuf,
    #[arg(long)]
    pub result: PathBuf,
    #[arg(long)]
    pub result_sha256: String,
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignStudyProposeArgs {
    /// Finalized parent submission whose ledger settlement must be read back.
    #[arg(long)]
    pub parent_submission: PathBuf,
    /// Independently read-back terminal Campaign result.
    #[arg(long)]
    pub parent_result: PathBuf,
    /// The durable report returned by `mission dispatch settle`.
    #[arg(long)]
    pub parent_settlement: PathBuf,
    /// Existing dispatch control containing the authenticated parent ledger.
    #[arg(long)]
    pub parent_control: PathBuf,
    #[arg(long)]
    pub study_id: String,
    /// Existing Study member to select. Missing or unlisted members yield needs_authority.
    #[arg(long)]
    pub target_family_id: Option<String>,
    /// Typed label-horizon JSON whose hash must match the selected Study member.
    #[arg(long)]
    pub target_horizon: Option<PathBuf>,
    /// Materialized campaign-inputs receipt for the selected target family.
    #[arg(long)]
    pub target_campaign_inputs: Option<PathBuf>,
    /// Local input root containing the receipt's feature/materialization objects.
    #[arg(long)]
    pub target_input_root: Option<PathBuf>,
    #[arg(long, default_value_t = 7)]
    pub target_seed: u64,
    #[arg(long)]
    pub target_start_received_at_ns: Option<u64>,
    #[arg(long)]
    pub target_end_received_at_ns: Option<u64>,
    #[arg(long)]
    pub target_mission_id: Option<String>,
    #[arg(long)]
    pub target_output_prefix: Option<String>,
    #[arg(long)]
    pub target_bucket_ms: Option<u64>,
    #[arg(long, default_value_t = 5)]
    pub target_top_depth: usize,
    #[arg(long, default_value = "monday-research")]
    pub parent_namespace: String,
    /// Create-once next-family proposal evidence.
    #[arg(long)]
    pub output: PathBuf,
    /// Create-once target research plan consumed by the canonical freeze seam.
    #[arg(long)]
    pub research_plan_output: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignFinalizeArgs {
    #[arg(long)]
    pub freeze: PathBuf,
    #[arg(long)]
    pub signed_request: PathBuf,
    #[arg(long)]
    pub attempt_id: String,
    #[arg(long)]
    pub image: String,
    #[arg(long)]
    pub request_out: PathBuf,
    #[arg(long)]
    pub submission_out: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignIdArgs {
    #[arg(long)]
    pub request: PathBuf,
}

/// Freeze a bounded archive window, run the existing CEX materializer, and
/// verify its immutable local receipt before the canonical Campaign freeze.
#[derive(Debug, Clone, Args)]
pub struct PrepareSequenceCohortArgs {
    #[arg(long)]
    pub request: PathBuf,
    #[arg(long)]
    pub input_root: PathBuf,
    /// Fresh directory containing only the worker's declared development view.
    #[arg(long)]
    pub output_root: PathBuf,
    /// Receipt outside the worker view, retained by the ACK controller.
    #[arg(long)]
    pub inputs_out: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct PrepareFreshInputsArgs {
    /// Read-only root of sealed raw collector triplets.
    #[arg(long)]
    pub raw_root: PathBuf,
    /// Read-only root of published Binance Spot or USD-M reference triplets.
    #[arg(long)]
    pub reference_root: PathBuf,
    /// Binance market whose raw and reference identities are frozen together.
    #[arg(long)]
    pub market: String,
    #[arg(long)]
    pub start_received_at_ns: Option<u64>,
    #[arg(long)]
    pub end_received_at_ns: Option<u64>,
    /// Latest-window duration in nanoseconds. Mutually exclusive with an explicit window.
    #[arg(long)]
    pub duration_ns: Option<u64>,
    /// Latest-window receive-time cutoff. When omitted, the first request resolves and persists now.
    #[arg(long)]
    pub cutoff_received_at_ns: Option<u64>,
    /// Maximum number of latest-window candidates to inspect.
    #[arg(long)]
    pub max_candidates: Option<usize>,
    #[arg(long)]
    pub symbol: String,
    /// Producer image digest recorded in the frozen inventory.
    #[arg(long)]
    pub image_ref: String,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub output_prefix: String,
    #[arg(long)]
    pub bucket_ms: u64,
    #[arg(long)]
    pub label_horizon_buckets: u64,
    #[arg(long)]
    pub top_depth: usize,
    #[arg(long, default_value_t = 100_000)]
    pub max_scan_entries: usize,
    /// Maximum number of frozen raw and reference inputs.
    #[arg(long)]
    pub max_inputs: usize,
    /// Maximum total source bytes verified by the collector freezer.
    #[arg(long)]
    pub max_input_bytes: u64,
    /// Directory that reuses partition listings across hourly preparations.
    #[arg(long)]
    pub discovery_index: Option<PathBuf>,
    /// Create-once frozen.env output retained as the preparation identity.
    #[arg(long)]
    pub inventory_out: PathBuf,
    /// Mandatory create-once preparation request binding the fresh window and limits.
    #[arg(long)]
    pub request_out: PathBuf,
    /// Create-once campaign-inputs.json output produced by the existing materializer.
    #[arg(long)]
    pub campaign_inputs_out: PathBuf,
    /// Parent of the materializer run-specific output prefix.
    #[arg(long)]
    pub output_root: PathBuf,
    /// Existing cex-materialization-entrypoint.sh.
    #[arg(long)]
    pub materializer: PathBuf,
    /// Private work directory for the existing materializer.
    #[arg(long)]
    pub materializer_work_dir: PathBuf,
    #[arg(long)]
    pub binary_dir: Option<PathBuf>,
    /// Export verified SOL 1s/top5/30s sequence inputs along with PIT artifacts.
    #[arg(long)]
    pub sequence_output: bool,
    /// Export separate label-free market features and 30-second targets.
    #[arg(long)]
    pub market_encoder_output: bool,
    /// Feature start after at most 60 seconds of admitted raw/PIT warmup.
    #[arg(long, requires = "market_encoder_output")]
    pub market_feature_start_received_at_ns: Option<u64>,
    /// Exclusive feature-partition end inside the explicit admitted label window.
    #[arg(long, requires = "market_encoder_output")]
    pub market_feature_end_received_at_ns: Option<u64>,
    /// Upper bound for the materializer process lifetime.
    #[arg(long, default_value_t = 7_200)]
    pub materializer_timeout_seconds: u64,
    /// Upper bound for captured materializer stdout and stderr combined.
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    pub max_materializer_output_bytes: u64,
    /// Optional create-once preparation report path.
    #[arg(long)]
    pub report_out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct CandidateShowArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub candidate_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct RevokeApprovalArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub approval_id: String,
    #[arg(long)]
    pub revoked_by: String,
    #[arg(long)]
    pub reason: String,
}

#[derive(Debug, Clone, Args)]
pub struct MissionStatusArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct DatasetArgs {
    #[arg(long)]
    pub dataset_manifest: PathBuf,
    #[command(flatten)]
    pub validation: ValidationArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ValidationArgs {
    #[arg(long, default_value_t = 200)]
    pub initial_train_rows: usize,
    #[arg(long, default_value_t = 64)]
    pub validation_rows: usize,
    #[arg(long, default_value_t = 3)]
    pub fold_count: usize,
    #[arg(long, default_value_t = 1)]
    pub purge_rows: usize,
    #[arg(long, default_value_t = 1)]
    pub embargo_rows: usize,
    #[arg(long, default_value_t = 64)]
    pub sealed_holdout_rows: usize,
    /// Reserve a separate selection window, withheld from search and learning.
    #[arg(long)]
    pub independent_selection_rows: Option<usize>,
    #[arg(long, hide = true)]
    pub calendar_binding_json: Option<String>,
    #[arg(long, default_value_t = 2.0)]
    pub fee_bps: f64,
    #[arg(long, default_value_t = 0.0)]
    pub rebate_bps: f64,
    #[arg(long, default_value_t = 0.0)]
    pub funding_bps: f64,
    #[arg(long, default_value_t = 0.5)]
    pub latency_bps: f64,
    /// Additional adverse execution slippage charged per unit of turnover.
    #[arg(long, default_value_t = 0.0)]
    pub slippage_bps: f64,
    /// Model taker execution by charging half of each row's observed spread_bps.
    #[arg(long, default_value_t = false)]
    pub cross_spread: bool,
    /// Gross USD notional represented by a unit position; zero disables capacity checks.
    #[arg(long, default_value_t = 0.0)]
    pub position_notional_usd: f64,
    /// Number N in the bid_depth_topN/ask_depth_topN capacity features.
    #[arg(long, default_value_t = 0)]
    pub capacity_depth_levels: usize,
    /// Maximum fraction of observed same-side top-N depth consumed by one position change.
    #[arg(long, default_value_t = 0.0)]
    pub max_book_depth_fraction: f64,
    #[arg(long)]
    pub label_horizon_buckets: usize,
    #[arg(long)]
    pub observation_frequency_millis: u64,
}

impl ValidationArgs {
    #[cfg(any(feature = "scientific", test))]
    pub fn from_protocol(protocol: &EvaluationProtocolV1) -> Self {
        Self {
            initial_train_rows: protocol.walk_forward.initial_train_rows,
            validation_rows: protocol.walk_forward.validation_rows,
            fold_count: protocol.walk_forward.fold_count,
            purge_rows: protocol.walk_forward.purge_rows,
            embargo_rows: protocol.walk_forward.embargo_rows,
            sealed_holdout_rows: protocol.walk_forward.sealed_holdout_rows,
            independent_selection_rows: protocol.selection.as_ref().map(|selection| selection.rows),
            calendar_binding_json: protocol
                .calendar
                .as_ref()
                .map(|binding| serde_json::to_string(binding).expect("calendar is serializable")),
            fee_bps: protocol.costs.fee_bps,
            rebate_bps: protocol.costs.rebate_bps,
            funding_bps: protocol.costs.funding_bps,
            latency_bps: protocol.costs.latency_bps,
            slippage_bps: protocol.costs.slippage_bps,
            cross_spread: protocol.costs.cross_spread,
            position_notional_usd: protocol.costs.position_notional_usd,
            capacity_depth_levels: protocol.costs.capacity_depth_levels,
            max_book_depth_fraction: protocol.costs.max_book_depth_fraction,
            label_horizon_buckets: protocol.labels.horizon_buckets,
            observation_frequency_millis: protocol.labels.observation_frequency_millis,
        }
    }

    pub fn evaluation_protocol(
        &self,
        labels: &EvaluationLabelSpecV1,
    ) -> Result<EvaluationProtocolV1, alpha_domain::DomainError> {
        if self.label_horizon_buckets != labels.horizon_buckets
            || self.observation_frequency_millis != labels.observation_frequency_millis
        {
            return Err(alpha_domain::DomainError::InvalidEvaluationProtocol);
        }
        let protocol = EvaluationProtocolV1::new(
            EvaluationWalkForwardV1 {
                initial_train_rows: self.initial_train_rows,
                validation_rows: self.validation_rows,
                fold_count: self.fold_count,
                purge_rows: self.purge_rows,
                embargo_rows: self.embargo_rows,
                sealed_holdout_rows: self.sealed_holdout_rows,
            },
            EvaluationCostsV1 {
                fee_bps: self.fee_bps,
                rebate_bps: self.rebate_bps,
                funding_bps: self.funding_bps,
                latency_bps: self.latency_bps,
                slippage_bps: self.slippage_bps,
                cross_spread: self.cross_spread,
                position_notional_usd: self.position_notional_usd,
                capacity_depth_levels: self.capacity_depth_levels,
                max_book_depth_fraction: self.max_book_depth_fraction,
            },
            labels.clone(),
        )?;
        let mut protocol = match self.independent_selection_rows {
            Some(rows) => protocol.with_independent_selection(rows),
            None => Ok(protocol),
        }?;
        if let Some(binding) = &self.calendar_binding_json {
            protocol.calendar = Some(
                serde_json::from_str(binding)
                    .map_err(|_| alpha_domain::DomainError::InvalidEvaluationProtocol)?,
            );
            protocol.version = alpha_domain::EVALUATION_PROTOCOL_VERSION_V3.into();
            protocol.validate()?;
        }
        Ok(protocol)
    }
}

#[derive(Debug, Clone, Args)]
pub struct ExecuteMissionArgs {
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    /// Exact sealed-holdout identity from the Mission artifact.
    #[arg(long)]
    pub holdout_id: String,
    #[arg(long)]
    pub mission_url: String,
    #[arg(long)]
    pub mission_sha256: String,
    #[arg(long)]
    pub feature_url: String,
    #[arg(long)]
    pub materialization_url: String,
    #[arg(long)]
    pub replay_artifact_url: String,
    #[arg(long)]
    pub replay_artifact_sha256: String,
    #[arg(long)]
    pub replay_manifest_url: String,
    #[arg(long)]
    pub replay_manifest_sha256: String,
    /// Prior immutable Factor-Bank subset checkpoint for a fresh-work-directory resume.
    #[arg(long, requires = "resume_sha256")]
    pub resume_url: Option<String>,
    /// Canonical SHA-256 from the checkpoint artifact's `checkpoint_sha256` field.
    #[arg(long, requires = "resume_url")]
    pub resume_sha256: Option<String>,
    #[arg(long)]
    pub result_put_url: String,
    /// Independently authorized read URL for the immutable published result bundle.
    #[arg(long)]
    pub result_readback_url: String,
    /// Create-once holdout-scoped claim written immediately before sealed holdout access.
    #[arg(long)]
    pub holdout_claim_put_url: String,
    /// Independently authorized read URL for the same immutable holdout claim object.
    #[arg(long)]
    pub holdout_claim_readback_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, serde::Serialize)]
pub enum EngineChoice {
    Gp,
    Mcts,
    Bayesian,
    OfflineRl,
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoopTargetChoice {
    Researching,
    WalkForwardKept,
    HoldoutPassed,
    PaperHealthy,
    ShadowHealthy,
    LiveSmallEligible,
}

#[derive(Debug, Clone, Args)]
pub struct RunMissionArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long, value_enum)]
    pub engine: EngineChoice,
    #[arg(long, default_value_t = 7)]
    pub seed: u64,
    #[arg(long, value_delimiter = ',', required = true)]
    pub feature_fields: Vec<String>,
    #[arg(long)]
    pub offline_trace: Option<PathBuf>,
    #[arg(long)]
    pub max_new_iterations: Option<usize>,
    #[command(flatten)]
    pub dataset: DatasetArgs,
}

#[derive(Debug, Clone, Args)]
pub struct LearnMissionArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long, default_value_t = 3)]
    pub repeated_failure_threshold: usize,
    #[arg(long, default_value_t = 500)]
    pub max_critic_tokens: u64,
    #[arg(long)]
    pub llm_critic: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RecoverLegacyCheckpointArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub replacement_mission_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct LoopRunArgs {
    #[command(flatten)]
    pub mission: RunMissionArgs,
    #[arg(long)]
    pub loop_run_id: String,
    #[arg(long, value_enum, default_value = "walk-forward-kept")]
    pub target_stage: LoopTargetChoice,
    #[arg(long, default_value_t = 3)]
    pub max_research_missions: usize,
    #[arg(long, default_value_t = 3)]
    pub repeated_failure_threshold: usize,
    #[arg(long, default_value_t = 500)]
    pub max_critic_tokens: u64,
    #[arg(long)]
    pub llm_critic: bool,
}

#[derive(Debug, Clone, Args)]
pub struct LoopStatusArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub loop_run_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct JsonRecordArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub record: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct FeedbackRecordArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub record: PathBuf,
    /// Runtime feedback key id to Ed25519 public key hex JSON map.
    #[arg(long)]
    pub trusted_keys: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct FeedbackLogArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub log: PathBuf,
    /// Runtime feedback key id to Ed25519 public key hex JSON map.
    #[arg(long)]
    pub trusted_keys: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct VerifyMaterializationManifestArgs {
    #[arg(long)]
    pub manifest: PathBuf,
    /// SHA-256 from the frozen inventory, checked against the parsed bytes.
    #[arg(long)]
    pub manifest_sha256: String,
    #[arg(long, value_enum)]
    pub kind: MaterializationManifestKindArg,
    #[arg(long)]
    pub market: String,
    #[arg(long, requires = "end_received_at_ns")]
    pub start_received_at_ns: Option<u64>,
    #[arg(long, requires = "start_received_at_ns")]
    pub end_received_at_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MaterializationManifestKindArg {
    Raw,
    Reference,
}

#[derive(Debug, Clone, Args)]
pub struct FreezeInventoryArgs {
    /// Read-only root of sealed raw collector triplets.
    #[arg(long)]
    pub raw_root: PathBuf,
    /// Read-only root of published Binance Spot or USD-M reference triplets.
    #[arg(long)]
    pub reference_root: PathBuf,
    /// Binance market whose raw and reference identities are frozen together.
    #[arg(long)]
    pub market: String,
    #[arg(long)]
    pub start_received_at_ns: u64,
    #[arg(long)]
    pub end_received_at_ns: u64,
    #[arg(long)]
    pub symbol: String,
    /// Paired research runner digest; source revision comes from this binary.
    #[arg(long)]
    pub image_ref: String,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub output_prefix: String,
    #[arg(long)]
    pub bucket_ms: u64,
    #[arg(long)]
    pub label_horizon_buckets: u64,
    #[arg(long)]
    pub top_depth: usize,
    #[arg(long, default_value_t = 100_000)]
    pub max_scan_entries: usize,
    #[arg(long, default_value_t = 8192)]
    pub max_inputs: usize,
    /// Explicit maximum total source bytes to verify; no source payload is copied.
    #[arg(long)]
    pub max_input_bytes: u64,
    /// Directory that reuses partition listings across hourly preparations.
    #[arg(long)]
    pub discovery_index: Option<PathBuf>,
    /// New private frozen.env file; an existing inventory is never overwritten.
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct EvaluateArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub candidate_id: String,
    #[arg(long)]
    pub model_root: Option<PathBuf>,
    #[command(flatten)]
    pub dataset: DatasetArgs,
}

#[derive(Debug, Clone, Args)]
pub struct RegisterOnnxArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub candidate_id: String,
    #[arg(long)]
    pub hypothesis: String,
    #[arg(long)]
    pub model: PathBuf,
    #[arg(long)]
    pub model_root: PathBuf,
    #[command(flatten)]
    pub dataset: DatasetArgs,
}

#[derive(Debug, Clone, Args)]
pub struct PromoteArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub candidate_id: String,
    #[arg(long)]
    pub promotion_id: Option<String>,
    #[arg(long)]
    pub bundle_out: Option<PathBuf>,
    #[arg(long)]
    pub model_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct SignDeploymentArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub envelope: PathBuf,
    #[arg(long)]
    pub signing_key: PathBuf,
    #[arg(long)]
    pub key_id: String,
    #[arg(long)]
    pub output: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct EnvelopeArgs {
    #[arg(long)]
    pub envelope: PathBuf,
}

pub fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}

pub(crate) fn require_cloud_data_host(os: &str) -> anyhow::Result<()> {
    if os != "linux" {
        anyhow::bail!("bulk CEX research, artifact verification and ledger execution belong in the ACK research Job; the workstation may sign control metadata and read lightweight reports");
    }
    Ok(())
}

#[cfg(feature = "scientific")]
#[derive(Debug, Parser)]
#[command(name = "monday-cex-worker", version = BUILD_SOURCE_REVISION, about = "Admitted CEX Campaign scientific worker")]
pub struct WorkerCli {
    #[command(subcommand)]
    command: WorkerCommand,
}
#[cfg(feature = "scientific")]
#[derive(Debug, Subcommand)]
enum WorkerCommand {
    Mission {
        #[command(subcommand)]
        command: WorkerMissionCommand,
    },
}
#[cfg(feature = "scientific")]
#[derive(Debug, Subcommand)]
enum WorkerMissionCommand {
    CampaignExecute(CampaignExecuteArgs),
}
#[cfg(feature = "scientific")]
pub async fn run_worker(cli: WorkerCli) -> anyhow::Result<()> {
    let WorkerCommand::Mission {
        command: WorkerMissionCommand::CampaignExecute(args),
    } = cli.command;
    require_cloud_data_host(std::env::consts::OS)?;
    tokio::task::spawn_blocking(move || mission_campaign::execute(args))
        .await
        .context("Campaign scientific worker failed")?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_research_commands_reject_workstation_hosts() {
        for host in ["macos", "windows"] {
            assert!(require_cloud_data_host(host)
                .unwrap_err()
                .to_string()
                .contains("ACK research Job"));
        }
        assert!(require_cloud_data_host("linux").is_ok());
    }

    #[test]
    fn validation_args_build_an_explicit_label_and_metric_protocol() {
        let args = ValidationArgs {
            initial_train_rows: 200,
            validation_rows: 64,
            fold_count: 3,
            purge_rows: 5,
            embargo_rows: 1,
            sealed_holdout_rows: 64,
            independent_selection_rows: None,
            calendar_binding_json: None,
            fee_bps: 1.0,
            rebate_bps: 0.25,
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

        let protocol = args
            .evaluation_protocol(&EvaluationLabelSpecV1 {
                horizon_buckets: 5,
                observation_frequency_millis: 1_000,
            })
            .unwrap();

        assert_eq!(protocol.labels.horizon_buckets, 5);
        assert_eq!(protocol.labels.observation_frequency_millis, 1_000);
        assert_eq!(protocol.costs.rebate_bps, 0.25);
        assert_eq!(
            protocol.metrics,
            alpha_domain::EvaluationMetricDefinitionsV1::default()
        );
        assert_eq!(
            args.evaluation_protocol(&EvaluationLabelSpecV1 {
                horizon_buckets: 4,
                observation_frequency_millis: 1_000,
            }),
            Err(alpha_domain::DomainError::InvalidEvaluationProtocol)
        );
    }
}
