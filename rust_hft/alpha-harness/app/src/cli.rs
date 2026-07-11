use crate::{data_mission, governance, loop_control, mission};
use alpha_store::AlphaStore;
use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use hft_collector::{source_catalog, DataAcquisitionMission, QualityRequirements};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "alpha-harness",
    version,
    about = "Agentic alpha research control plane"
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
}

#[derive(Debug, Subcommand)]
enum CandidateCommand {
    List(MissionStatusArgs),
}

#[derive(Debug, Subcommand)]
enum DeploymentCommand {
    Sign(SignDeploymentArgs),
    ScopeHash(EnvelopeArgs),
}

#[derive(Debug, Subcommand)]
enum FeedbackCommand {
    Ingest(JsonRecordArgs),
    IngestLog(JsonLogArgs),
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
    #[arg(long, default_value_t = 100)]
    pub initial_train_rows: usize,
    #[arg(long, default_value_t = 20)]
    pub validation_rows: usize,
    #[arg(long, default_value_t = 3)]
    pub fold_count: usize,
    #[arg(long, default_value_t = 1)]
    pub purge_rows: usize,
    #[arg(long, default_value_t = 1)]
    pub embargo_rows: usize,
    #[arg(long, default_value_t = 20)]
    pub sealed_holdout_rows: usize,
    #[arg(long, default_value_t = 1.0)]
    pub fee_bps: f64,
    #[arg(long, default_value_t = 0.0)]
    pub funding_bps: f64,
    #[arg(long, default_value_t = 0.5)]
    pub latency_bps: f64,
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
pub struct JsonLogArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub log: PathBuf,
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

#[derive(Debug, Clone, Args)]
pub struct EvaluateArgs {
    #[arg(long)]
    pub db: PathBuf,
    #[arg(long)]
    pub mission_id: String,
    #[arg(long)]
    pub candidate_id: String,
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
        },
        Command::Candidate { command } => match command {
            CandidateCommand::List(args) => governance::candidate_list(args),
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
            "--target-stage",
            "shadow-healthy",
            "--max-research-missions",
            "2",
        ])
        .is_ok());
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
            "--dataset-manifest",
            "dataset.json",
            "--loop-run-id",
            "loop-1",
            "--target-stage",
            "live-small-eligible",
        ])
        .is_ok());
    }
}
