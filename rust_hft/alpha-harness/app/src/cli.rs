use crate::{
    data_mission, governance, loop_control, mission, mission_runner, prediction_dispatch,
    prediction_runner, prediction_snapshot,
};
use alpha_domain::{
    EvaluationCostsV1, EvaluationLabelSpecV1, EvaluationProtocolV1, EvaluationWalkForwardV1,
};
use alpha_store::AlphaStore;
use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use hft_collector::{source_catalog, DataAcquisitionMission, QualityRequirements};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "alpha-harness",
    version,
    about = "Bounded Loop Engineer alpha research control plane"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Mission {
        #[command(subcommand)]
        command: MissionCommand,
    },
    Loop {
        #[command(subcommand)]
        command: LoopCommand,
    },
    Data {
        #[command(subcommand)]
        command: DataCommand,
    },
    Prediction {
        #[command(subcommand)]
        command: PredictionCommand,
    },
    Candidate {
        #[command(subcommand)]
        command: CandidateCommand,
    },
    Evaluate(EvaluateArgs),
    Promote(PromoteArgs),
    Deployment {
        #[command(subcommand)]
        command: DeploymentCommand,
    },
    Feedback {
        #[command(subcommand)]
        command: FeedbackCommand,
    },
    Policy {
        #[command(subcommand)]
        command: PolicyCommand,
    },
    Approval {
        #[command(subcommand)]
        command: ApprovalCommand,
    },
}

#[derive(Debug, Subcommand)]
enum MissionCommand {
    Create(CreateMissionArgs),
    Execute(Box<ExecuteMissionArgs>),
    Run(RunMissionArgs),
    Resume(RunMissionArgs),
    Status(MissionStatusArgs),
    Learn(LearnMissionArgs),
    RecoverLegacyCheckpoint(RecoverLegacyCheckpointArgs),
}

#[derive(Debug, Subcommand)]
enum LoopCommand {
    Run(Box<LoopRunArgs>),
    Status(LoopStatusArgs),
}

#[derive(Debug, Subcommand)]
enum DataCommand {
    Sources,
    Acquire(AcquireDataArgs),
    ImportFeatures(ImportFeatureDataArgs),
}

#[derive(Debug, Subcommand)]
enum PredictionCommand {
    Execute(Box<PredictionExecuteArgs>),
    Snapshot(PredictionSnapshotArgs),
    Dispatch {
        #[command(subcommand)]
        command: PredictionDispatchCommand,
    },
}

#[derive(Debug, Subcommand)]
enum PredictionDispatchCommand {
    Render(PredictionDispatchRenderArgs),
    Status(PredictionDispatchStatusArgs),
    Submit(PredictionDispatchSubmitArgs),
}

#[derive(Debug, Clone, Args)]
pub struct PredictionDispatchRenderArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub namespace: String,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionDispatchSubmitArgs {
    #[arg(long)]
    pub submission: PathBuf,
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionDispatchStatusArgs {
    #[arg(long)]
    pub context: String,
    #[arg(long)]
    pub namespace: String,
    #[arg(long)]
    pub job_name: String,
    #[arg(long)]
    pub evidence: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionExecuteArgs {
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub mission_url: String,
    #[arg(long)]
    pub mission_sha256: String,
    #[arg(long)]
    pub snapshot_url: String,
    #[arg(long)]
    pub snapshot_sha256: String,
    #[arg(long)]
    pub snapshot_contract_id: String,
    #[arg(long)]
    pub snapshot_digest: String,
    #[arg(long)]
    pub cohort_manifest_id: String,
    #[arg(long)]
    pub partition_digest: String,
    #[arg(long)]
    pub policy_identity: String,
    #[arg(long)]
    pub task_capability: String,
    #[arg(long)]
    pub image_identity: String,
    /// Read-only cache directory containing `<snapshot-sha256>.zip` archives.
    #[arg(long)]
    pub snapshot_cache_dir: Option<PathBuf>,
    /// Prior immutable prediction result bundle for a paused LoopRun.
    #[arg(long, requires = "resume_sha256")]
    pub resume_url: Option<String>,
    #[arg(long, requires = "resume_url")]
    pub resume_sha256: Option<String>,
    #[arg(long)]
    pub result_put_url: String,
    /// Independently authorized read URL for the immutable published result bundle.
    #[arg(long)]
    pub result_readback_url: String,
}

#[derive(Debug, Clone, Args)]
pub struct PredictionSnapshotArgs {
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub result_put_url: String,
    /// Arguments forwarded to the governed snapshot compiler. `--output-dir`
    /// is owned by alpha-harness and must not be supplied here.
    #[arg(last = true, required = true, allow_hyphen_values = true)]
    pub compiler_args: Vec<OsString>,
}

#[derive(Debug, Subcommand)]
enum CandidateCommand {
    List(MissionStatusArgs),
    RegisterOnnx(Box<RegisterOnnxArgs>),
}

#[derive(Debug, Subcommand)]
enum DeploymentCommand {
    Sign(SignDeploymentArgs),
    ScopeHash(EnvelopeArgs),
}

#[derive(Debug, Subcommand)]
enum FeedbackCommand {
    Ingest(FeedbackRecordArgs),
    IngestLog(FeedbackLogArgs),
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    Propose(JsonRecordArgs),
}

#[derive(Debug, Subcommand)]
enum ApprovalCommand {
    Record(JsonRecordArgs),
    Revoke(RevokeApprovalArgs),
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

#[derive(Debug, Args)]
struct CreateMissionArgs {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    mission: PathBuf,
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
    pub fn from_protocol(protocol: &EvaluationProtocolV1) -> Self {
        Self {
            initial_train_rows: protocol.walk_forward.initial_train_rows,
            validation_rows: protocol.walk_forward.validation_rows,
            fold_count: protocol.walk_forward.fold_count,
            purge_rows: protocol.walk_forward.purge_rows,
            embargo_rows: protocol.walk_forward.embargo_rows,
            sealed_holdout_rows: protocol.walk_forward.sealed_holdout_rows,
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
        EvaluationProtocolV1::new(
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
        )
    }
}

#[derive(Debug, Clone, Args)]
pub struct ExecuteMissionArgs {
    #[arg(long)]
    pub work_dir: PathBuf,
    #[arg(long)]
    pub mission_url: String,
    #[arg(long)]
    pub mission_sha256: String,
    #[arg(long)]
    pub feature_url: String,
    #[arg(long)]
    pub materialization_url: String,
    #[arg(long)]
    pub result_put_url: String,
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

#[derive(Debug, Args)]
struct AcquireDataArgs {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    mission_id: String,
    #[arg(long, default_value = "binance-public")]
    source_id: String,
    #[arg(long)]
    symbol: String,
    #[arg(long, default_value = "1m")]
    interval: String,
    #[arg(long, default_value_t = 500)]
    limit: usize,
    #[arg(long)]
    artifact_dir: PathBuf,
    #[arg(long)]
    manifest_out: Option<PathBuf>,
    #[arg(long, default_value_t = 0)]
    max_parse_failures: usize,
    #[arg(long, default_value_t = 0)]
    max_non_monotonic_events: usize,
    #[arg(long, default_value_t = 0)]
    max_non_finite_values: usize,
}

#[derive(Debug, Args)]
struct ImportFeatureDataArgs {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    mission_id: String,
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    artifact_dir: PathBuf,
    #[arg(long)]
    manifest_out: PathBuf,
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

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Mission { command } => match command {
            MissionCommand::Create(args) => {
                let bytes = std::fs::read(&args.mission).with_context(|| {
                    format!("failed to read mission {}", args.mission.display())
                })?;
                let mission: alpha_domain::ResearchMission = serde_json::from_slice(&bytes)?;
                let mut store = AlphaStore::open(args.db)?;
                store.create_mission(&mission)?;
                print_json(&mission)
            }
            MissionCommand::Execute(args) => {
                tokio::task::spawn_blocking(move || mission_runner::execute(*args))
                    .await
                    .context("mission execution worker failed")?
            }
            MissionCommand::Run(args) => mission::run_mission(args, false),
            MissionCommand::Resume(args) => mission::run_mission(args, true),
            MissionCommand::Status(args) => mission::mission_status(args),
            MissionCommand::Learn(args) => mission::learn_mission(args),
            MissionCommand::RecoverLegacyCheckpoint(args) => {
                loop_control::recover_legacy_checkpoint(args)
            }
        },
        Command::Loop { command } => match command {
            LoopCommand::Run(args) => loop_control::run_loop(*args),
            LoopCommand::Status(args) => loop_control::loop_status(args),
        },
        Command::Data { command } => match command {
            DataCommand::Sources => print_json(&source_catalog()),
            DataCommand::Acquire(args) => {
                let mut store = AlphaStore::open(&args.db)?;
                let data_mission = DataAcquisitionMission {
                    mission_id: args.mission_id,
                    source_id: args.source_id,
                    symbol: args.symbol,
                    interval: args.interval,
                    limit: args.limit,
                    artifact_dir: args.artifact_dir,
                    quality_requirements: QualityRequirements {
                        max_parse_failures: args.max_parse_failures,
                        max_non_monotonic_events: args.max_non_monotonic_events,
                        max_non_finite_values: args.max_non_finite_values,
                    },
                };
                let manifest =
                    data_mission::acquire_and_register(&mut store, &data_mission).await?;
                let output = args
                    .manifest_out
                    .unwrap_or_else(|| data_mission::default_manifest_path(&manifest));
                data_mission::write_json_atomic(&output, &manifest)?;
                print_json(&serde_json::json!({
                    "manifest": manifest,
                    "manifest_path": output,
                }))
            }
            DataCommand::ImportFeatures(args) => {
                let mut store = AlphaStore::open(&args.db)?;
                let manifest = data_mission::import_and_register_features(
                    &mut store,
                    &args.mission_id,
                    &args.input,
                    &args.artifact_dir,
                )?;
                data_mission::write_json_atomic(&args.manifest_out, &manifest)?;
                print_json(&serde_json::json!({
                    "manifest": manifest,
                    "manifest_path": args.manifest_out,
                }))
            }
        },
        Command::Prediction { command } => match command {
            PredictionCommand::Execute(args) => {
                tokio::task::spawn_blocking(move || prediction_runner::execute(*args))
                    .await
                    .context("prediction execution worker failed")?
            }
            PredictionCommand::Snapshot(args) => {
                tokio::task::spawn_blocking(move || prediction_snapshot::snapshot(args))
                    .await
                    .context("prediction snapshot worker failed")?
            }
            PredictionCommand::Dispatch { command } => match command {
                PredictionDispatchCommand::Render(args) => prediction_dispatch::render(args),
                PredictionDispatchCommand::Status(args) => prediction_dispatch::status(args),
                PredictionDispatchCommand::Submit(args) => prediction_dispatch::submit(args),
            },
        },
        Command::Candidate { command } => match command {
            CandidateCommand::List(args) => governance::candidate_list(args),
            CandidateCommand::RegisterOnnx(args) => governance::register_onnx_candidate(*args),
        },
        Command::Evaluate(args) => governance::evaluate(args),
        Command::Promote(args) => governance::promote(args),
        Command::Deployment { command } => match command {
            DeploymentCommand::Sign(args) => governance::sign_deployment(args),
            DeploymentCommand::ScopeHash(args) => governance::print_deployment_scope(args),
        },
        Command::Feedback { command } => match command {
            FeedbackCommand::Ingest(args) => governance::ingest_feedback(args),
            FeedbackCommand::IngestLog(args) => governance::ingest_feedback_log(args),
        },
        Command::Policy { command } => match command {
            PolicyCommand::Propose(args) => governance::propose_policy(args),
        },
        Command::Approval { command } => match command {
            ApprovalCommand::Record(args) => governance::record_approval(args),
            ApprovalCommand::Revoke(args) => governance::revoke_approval(args),
        },
    }
}

pub fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), value)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    static PREDICTION_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mission_execute_runs_blocking_pipeline_outside_async_runtime() {
        let root = tempfile::tempdir().unwrap();
        let work_dir = root.path().join("work").display().to_string();
        let missing_mission = root
            .path()
            .join("missing-mission.json")
            .display()
            .to_string();
        let missing_features = root
            .path()
            .join("missing-features.jsonl")
            .display()
            .to_string();
        let missing_materialization = root
            .path()
            .join("missing-materialization.json")
            .display()
            .to_string();
        let result = root.path().join("results.zip").display().to_string();
        let cli = Cli::try_parse_from(vec![
            "alpha-harness".to_owned(),
            "mission".to_owned(),
            "execute".to_owned(),
            "--work-dir".to_owned(),
            work_dir,
            "--mission-url".to_owned(),
            missing_mission.clone(),
            "--mission-sha256".to_owned(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            "--feature-url".to_owned(),
            missing_features,
            "--materialization-url".to_owned(),
            missing_materialization,
            "--result-put-url".to_owned(),
            result,
        ])
        .unwrap();

        let error = run(cli).await.unwrap_err();

        assert!(
            format!("{error:#}")
                .contains(&format!("failed to open local source {missing_mission}")),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prediction_execute_runs_blocking_pipeline_outside_async_runtime() {
        let _lock = PREDICTION_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let runner = root.path().join("unused-prediction-runner");
        std::fs::write(&runner, b"unused").unwrap();
        let _runner = EnvVarGuard::set("MONDAY_PREDICTION_RESEARCH_BIN", &runner);
        let missing_mission = root.path().join("missing-mission.json");
        let cli = Cli::try_parse_from([
            OsString::from("alpha-harness"),
            OsString::from("prediction"),
            OsString::from("execute"),
            OsString::from("--work-dir"),
            root.path().join("work").into_os_string(),
            OsString::from("--mission-url"),
            missing_mission.clone().into_os_string(),
            OsString::from("--mission-sha256"),
            OsString::from("a".repeat(64)),
            OsString::from("--snapshot-url"),
            root.path().join("missing-snapshot.zip").into_os_string(),
            OsString::from("--snapshot-sha256"),
            OsString::from("b".repeat(64)),
            OsString::from("--snapshot-contract-id"),
            OsString::from(format!("sha256:{}", "c".repeat(64))),
            OsString::from("--snapshot-digest"),
            OsString::from("0123456789abcdef"),
            OsString::from("--cohort-manifest-id"),
            OsString::from(format!("sha256:{}", "d".repeat(64))),
            OsString::from("--partition-digest"),
            OsString::from(format!("sha256:{}", "e".repeat(64))),
            OsString::from("--policy-identity"),
            OsString::from(format!("sha256:{}", "f".repeat(64))),
            OsString::from("--task-capability"),
            OsString::from("btc_5m_backtest"),
            OsString::from("--image-identity"),
            OsString::from(format!("sha256:{}", "a".repeat(64))),
            OsString::from("--result-put-url"),
            root.path().join("results.zip").into_os_string(),
            OsString::from("--result-readback-url"),
            root.path().join("results.zip").into_os_string(),
        ])
        .unwrap();

        let error = run(cli).await.unwrap_err();

        assert!(
            format!("{error:#}").contains(&format!(
                "failed to open local source {}",
                missing_mission.display()
            )),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn parses_prediction_dispatch_render() {
        let args = "alpha-harness prediction dispatch render --submission submission.json --namespace monday-research";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[test]
    fn parses_prediction_dispatch_status_with_explicit_cluster_identity() {
        let args = "alpha-harness prediction dispatch status --context ack --namespace monday-research --job-name prediction-job";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn prediction_snapshot_runs_blocking_pipeline_outside_async_runtime() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = PREDICTION_ENV_LOCK.lock().await;
        let root = tempfile::tempdir().unwrap();
        let compiler = root.path().join("snapshot-compiler.sh");
        std::fs::write(
            &compiler,
            format!(
                r#"#!/bin/sh
set -eu
test "$1" = "--output-dir"
mkdir -p "$2"
printf '%s\n' '{{"schema_version":"research_snapshot_v2","snapshot_hash":"0123456789abcdef","snapshot_contract_hash":"sha256:{}"}}' > "$2/manifest.json"
"#,
                "1".repeat(64)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&compiler, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _compiler = EnvVarGuard::set("MONDAY_PREDICTION_SNAPSHOT_BIN", &compiler);
        let published = root.path().join("published-snapshot.zip");
        std::fs::write(&published, b"occupied").unwrap();
        let cli = Cli::try_parse_from([
            OsString::from("alpha-harness"),
            OsString::from("prediction"),
            OsString::from("snapshot"),
            OsString::from("--work-dir"),
            root.path().join("work").into_os_string(),
            OsString::from("--result-put-url"),
            published.clone().into_os_string(),
            OsString::from("--"),
            OsString::from("--start-date"),
            OsString::from("2026-07-01"),
        ])
        .unwrap();

        let error = run(cli).await.unwrap_err();

        assert!(
            format!("{error:#}").contains(&format!(
                "result destination already exists: {}",
                published.display()
            )),
            "unexpected error: {error:#}"
        );
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

    #[test]
    fn parses_mission_and_data_control_plane_commands() {
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "status",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "prediction",
            "snapshot",
            "--work-dir",
            "work",
            "--result-put-url",
            "snapshot.zip",
            "--",
            "--start-date",
            "2026-07-01",
            "--end-date",
            "2026-07-02",
            "--optimizer-data-dir",
            "optimizer",
            "--data-audit-report",
            "audit.json",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "data",
            "acquire",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "data-1",
            "--symbol",
            "BTCUSDT",
            "--artifact-dir",
            "artifacts",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "prediction",
            "execute",
            "--work-dir",
            "work",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--snapshot-url",
            "snapshot.zip",
            "--snapshot-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--snapshot-contract-id",
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "--snapshot-digest",
            "0123456789abcdef",
            "--cohort-manifest-id",
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "--partition-digest",
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "--policy-identity",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--task-capability",
            "btc_5m_backtest",
            "--image-identity",
            "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            "--resume-url",
            "previous-results.zip",
            "--resume-sha256",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "--result-put-url",
            "results.zip",
            "--result-readback-url",
            "results.zip",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--result-put-url",
            "results.zip",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "recover-legacy-checkpoint",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
            "--replacement-mission-id",
            "mission-1-recovered",
        ])
        .is_ok());
    }

    #[test]
    fn mission_execute_accepts_only_content_bound_mission_transport() {
        let cli = Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--result-put-url",
            "results.zip",
        ])
        .expect("content-bound Mission transport must be accepted");

        let Command::Mission {
            command: MissionCommand::Execute(args),
        } = cli.command
        else {
            panic!("expected mission execute command")
        };
        assert_eq!(args.mission_url, "mission.json");
        assert_eq!(args.mission_sha256, "b".repeat(64));

        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--result-put-url",
            "results.zip",
            "--objective",
            "alternate authority",
        ])
        .is_err());
    }

    #[test]
    fn exposes_no_order_or_trade_command() {
        assert!(Cli::try_parse_from(["alpha-harness", "order"]).is_err());
        assert!(Cli::try_parse_from(["alpha-harness", "trade"]).is_err());
    }

    #[test]
    fn parses_bounded_loop_run_without_execution_authority() {
        let cli = Cli::try_parse_from([
            "alpha-harness",
            "loop",
            "run",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
            "--engine",
            "mcts",
            "--feature-fields",
            "book_imbalance",
            "--dataset-manifest",
            "dataset.json",
            "--loop-run-id",
            "loop-1",
            "--target-stage",
            "shadow-healthy",
            "--label-horizon-buckets",
            "1",
            "--observation-frequency-millis",
            "60000",
            "--max-research-missions",
            "2",
        ])
        .unwrap();
        let Command::Loop {
            command: LoopCommand::Run(args),
        } = cli.command
        else {
            panic!("expected loop run command")
        };
        assert_eq!(args.mission.feature_fields, ["book_imbalance"]);
        assert_eq!(args.mission.dataset.validation.fee_bps, 2.0);
        assert_eq!(args.mission.dataset.validation.latency_bps, 0.5);
        assert_eq!(args.mission.dataset.validation.slippage_bps, 0.0);
    }

    #[test]
    fn loop_run_requires_explicit_live_feature_fields() {
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "loop",
            "run",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
            "--engine",
            "mcts",
            "--dataset-manifest",
            "dataset.json",
            "--loop-run-id",
            "loop-1",
            "--label-horizon-buckets",
            "1",
            "--observation-frequency-millis",
            "60000",
        ])
        .is_err());
    }

    #[test]
    fn parses_staged_loop_targets() {
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "loop",
            "run",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
            "--engine",
            "mcts",
            "--feature-fields",
            "book_imbalance",
            "--dataset-manifest",
            "dataset.json",
            "--loop-run-id",
            "loop-1",
            "--target-stage",
            "live-small-eligible",
            "--label-horizon-buckets",
            "1",
            "--observation-frequency-millis",
            "60000",
        ])
        .is_ok());
    }

    #[test]
    fn feedback_ingestion_requires_runtime_trusted_keys() {
        let base = [
            "alpha-harness",
            "feedback",
            "ingest",
            "--db",
            "alpha.duckdb",
            "--record",
            "feedback.json",
        ];
        assert!(Cli::try_parse_from(base).is_err());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "feedback",
            "ingest",
            "--db",
            "alpha.duckdb",
            "--record",
            "feedback.json",
            "--trusted-keys",
            "runtime-feedback-trusted-keys.json",
        ])
        .is_ok());
    }
}
