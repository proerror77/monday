use crate::{
    data_mission, governance, loop_control, mission, mission_runner, prediction_runner,
    prediction_snapshot,
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
    Execute(PredictionExecuteArgs),
    Snapshot(PredictionSnapshotArgs),
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
    /// Prior immutable prediction result bundle for a paused LoopRun.
    #[arg(long, requires = "resume_sha256")]
    pub resume_url: Option<String>,
    #[arg(long, requires = "resume_url")]
    pub resume_sha256: Option<String>,
    #[arg(long)]
    pub result_put_url: String,
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
    #[arg(long, default_value_t = 1.0)]
    pub fee_bps: f64,
    #[arg(long, default_value_t = 0.0)]
    pub funding_bps: f64,
    #[arg(long, default_value_t = 0.5)]
    pub latency_bps: f64,
    #[arg(long)]
    pub label_horizon_buckets: usize,
    #[arg(long)]
    pub observation_frequency_millis: u64,
}

impl ValidationArgs {
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
                funding_bps: self.funding_bps,
                latency_bps: self.latency_bps,
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
    pub feature_url: String,
    #[arg(long)]
    pub materialization_url: String,
    #[arg(long)]
    pub materialization_sha256: String,
    #[arg(long)]
    pub result_put_url: String,
    #[arg(long)]
    pub data_mission_id: String,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long, value_enum)]
    pub engine: EngineChoice,
    #[arg(long, value_delimiter = ',')]
    pub feature_fields: Vec<String>,
    #[arg(long, default_value_t = 7)]
    pub seed: u64,
    #[arg(long, default_value_t = 8)]
    pub max_candidates: usize,
    #[arg(long, default_value_t = 40)]
    pub max_expansions: u64,
    #[arg(long, default_value_t = 300)]
    pub max_seconds: u64,
    #[arg(long, default_value_t = 8)]
    pub max_new_iterations: usize,
    #[arg(long, default_value = "Find a cost-aware, out-of-sample LOB factor")]
    pub objective: String,
    #[arg(long, default_value = "LOB imbalance and order-flow dynamics")]
    pub hypothesis_scope: String,
    #[command(flatten)]
    pub validation: ValidationArgs,
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
            MissionCommand::Execute(args) => mission_runner::execute(*args),
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
            PredictionCommand::Execute(args) => prediction_runner::execute(args),
            PredictionCommand::Snapshot(args) => prediction_snapshot::snapshot(args),
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
            funding_bps: 0.0,
            latency_bps: 0.5,
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
            "--resume-url",
            "previous-results.zip",
            "--resume-sha256",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "--result-put-url",
            "results.zip",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--materialization-sha256",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--result-put-url",
            "results.zip",
            "--data-mission-id",
            "data-1",
            "--mission-id",
            "mission-1",
            "--engine",
            "mcts",
            "--feature-fields",
            "book_imbalance_top5,ofi_top5",
            "--label-horizon-buckets",
            "5",
            "--observation-frequency-millis",
            "1000",
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
