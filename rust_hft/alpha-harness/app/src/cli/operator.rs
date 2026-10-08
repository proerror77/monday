use super::*;
#[cfg(feature = "scientific")]
use crate::mission_runner;
use crate::{
    data_mission, governance, loop_control, mission, mission_campaign, mission_dispatch,
    mission_fresh_inputs, mission_metrics,
};
use alpha_store::AlphaStore;
use anyhow::Context;
use clap::{Parser, Subcommand};
use hft_collector::{source_catalog, DataAcquisitionMission, QualityRequirements};

#[derive(Debug, Parser)]
#[command(
    name = "alpha-harness",
    version = BUILD_SOURCE_REVISION,
    about = "Governed CEX Campaign research control plane"
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
    /// Diagnostic research-only surfaces that cannot dispatch jobs or train models.
    Research {
        #[command(subcommand)]
        command: ResearchCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ResearchCommand {
    /// Read-only second-level orderflow import/audit. Never trains or dispatches.
    SecOrderflowAudit(SecOrderflowAuditArgs),
}

#[derive(Debug, Clone, Args)]
pub struct SecOrderflowAuditArgs {
    #[arg(long)]
    config: PathBuf,
    /// Explicit input root. Omitted or missing paths report research_eligible=false.
    /// This flag never defaults to a Mac collector path.
    #[arg(long)]
    input_root: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum MissionCommand {
    #[command(hide = true)]
    Create(CreateMissionArgs),
    #[command(hide = true)]
    #[cfg(feature = "scientific")]
    Execute(Box<ExecuteMissionArgs>),
    CampaignFreeze(CampaignFreezeArgs),
    /// Emit a development-only label report and bound plan before any freeze or fit.
    CampaignPrecheck(CampaignPrecheckArgs),
    /// Prepare a bounded research matrix once, without signing or dispatching jobs.
    CampaignPrepare(CampaignPrepareArgs),
    /// Run or resume a declared ACK workflow through terminal Campaign readback.
    CampaignWorkflow(CampaignWorkflowArgs),
    CampaignLearn(CampaignLearnArgs),
    CampaignStudyPropose(CampaignStudyProposeArgs),
    CampaignFinalize(CampaignFinalizeArgs),
    CampaignId(CampaignIdArgs),
    /// Summarize completed model evidence without training or submitting research.
    ModelMetrics(ModelMetricsArgs),
    PrepareFreshInputs(Box<PrepareFreshInputsArgs>),
    /// Assemble an immutable SOL sequence cohort from prepared PIT receipts.
    PrepareSequenceCohort(PrepareSequenceCohortArgs),
    Dispatch {
        #[command(subcommand)]
        command: MissionDispatchCommand,
    },
    #[command(hide = true)]
    Run(RunMissionArgs),
    #[command(hide = true)]
    Resume(RunMissionArgs),
    Status(MissionStatusArgs),
    #[command(hide = true)]
    Learn(LearnMissionArgs),
    #[command(hide = true)]
    RecoverLegacyCheckpoint(RecoverLegacyCheckpointArgs),
}

#[derive(Debug, Subcommand)]
enum LoopCommand {
    #[command(hide = true)]
    Run(Box<LoopRunArgs>),
    Status(LoopStatusArgs),
}

#[derive(Debug, Subcommand)]
enum DataCommand {
    Sources,
    Acquire(AcquireDataArgs),
    ImportFeatures(ImportFeatureDataArgs),
    FreezeInventory(FreezeInventoryArgs),
    VerifyMaterializationManifest(VerifyMaterializationManifestArgs),
}

#[derive(Debug, Subcommand)]
enum MissionDispatchCommand {
    /// Reserve the inspected canonical request and independently publish/read back native receipts.
    PreparePlatform(mission_dispatch::platform_admission::PlatformPrepareArgs),
    /// Export one source-reserved Campaign after actual software/data/configuration readback.
    ExportPlatform(mission_dispatch::platform_admission::PlatformExportArgs),
    /// Export authenticated Root/Study constraints for already transferred native operations.
    ExportPlatformRevocations(mission_dispatch::platform_admission::PlatformRevocationsArgs),
    /// Audit one transferred terminal Attempt using independent stopped/native readback.
    AuditPlatformTerminal(mission_dispatch::platform_terminal::PlatformTerminalArgs),
    CloseFamily(CampaignCloseFamilyArgs),
    Inspect(MissionDispatchInspectArgs),
    /// Read authenticated historical dispatch identity without submitting or settling.
    Status(MissionDispatchStatusArgs),
    Submit(MissionDispatchSubmitArgs),
    Settle(MissionDispatchSubmitArgs),
    ControllerHandoff(CampaignControllerHandoffArgs),
    PrepareController(CampaignControllerPrepareArgs),
    InitStageAuthority(InitStageAuthorityArgs),
    DescribeStudy(DescribeStudyArgs),
    SignRoot(mission_dispatch::study_authority::RootSignArgs),
    SignStudy(mission_dispatch::study_authority::StudySignArgs),
    RegisterStudy(mission_dispatch::study_authority::StudyRegisterArgs),
    InspectStudy(mission_dispatch::study_authority::StudyInspectArgs),
    StageController(CampaignStageControllerArgs),
}

#[derive(Debug, Subcommand)]
enum CandidateCommand {
    List(MissionStatusArgs),
    Show(CandidateShowArgs),
    #[cfg(feature = "onnx-compatibility")]
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

#[derive(Debug, Args)]
struct CreateMissionArgs {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    mission: PathBuf,
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
            #[cfg(feature = "scientific")]
            MissionCommand::Execute(args) => {
                tokio::task::spawn_blocking(move || mission_runner::execute(*args))
                    .await
                    .context("mission execution worker failed")?
            }
            MissionCommand::CampaignFreeze(args) => {
                require_cloud_data_host(std::env::consts::OS)?;
                mission_campaign::freeze(args)
            }
            MissionCommand::CampaignPrepare(args) => mission_campaign::preparation::prepare(args),
            MissionCommand::CampaignPrecheck(args) => crate::mission_calendar::precheck(args),
            MissionCommand::CampaignWorkflow(args) => mission_campaign::workflow::run(args),
            MissionCommand::CampaignLearn(args) => {
                tokio::task::spawn_blocking(move || mission_campaign::learn(args))
                    .await
                    .context("campaign learning worker failed")?
            }
            MissionCommand::CampaignStudyPropose(args) => {
                tokio::task::spawn_blocking(move || mission_campaign::propose_next_family(args))
                    .await
                    .context("next-family Campaign proposal worker failed")?
            }
            MissionCommand::CampaignFinalize(args) => mission_campaign::finalize(args),
            MissionCommand::CampaignId(args) => mission_campaign::print_expected_id(args),
            MissionCommand::ModelMetrics(args) => {
                require_cloud_data_host(std::env::consts::OS)?;
                tokio::task::spawn_blocking(move || mission_metrics::run(args))
                    .await
                    .context("model metrics reporting worker failed")?
            }
            MissionCommand::PrepareFreshInputs(args) => {
                require_cloud_data_host(std::env::consts::OS)?;
                tokio::task::spawn_blocking(move || mission_fresh_inputs::prepare(*args))
                    .await
                    .context("fresh Campaign input preparation worker failed")?
            }
            MissionCommand::PrepareSequenceCohort(args) => {
                require_cloud_data_host(std::env::consts::OS)?;
                tokio::task::spawn_blocking(move || {
                    let request: serde_json::Value =
                        mission_campaign::sequence::read_json(&args.request)?;
                    if matches!(
                        request["schema_version"].as_str(),
                        Some(
                            "monday.sol_market_encoder_cohort_request.v1"
                                | "monday.sol_market_encoder_cohort_request.v2"
                        )
                    ) {
                        mission_campaign::market_encoder::cohort::prepare(args)
                    } else {
                        mission_campaign::sequence::cohort::prepare(args)
                    }
                })
                .await
                .context("sequence cohort preparation worker failed")?
            }
            MissionCommand::Dispatch { command } => match command {
                MissionDispatchCommand::PreparePlatform(args) => {
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::platform_admission::prepare(args)
                    })
                    .await
                    .context("platform Campaign budget preparation task failed")?
                }
                MissionDispatchCommand::ExportPlatform(args) => {
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::platform_admission::export(args)
                    })
                    .await
                    .context("native platform signed export task failed")?
                }
                MissionDispatchCommand::ExportPlatformRevocations(args) => {
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::platform_admission::export_revocations(args)
                    })
                    .await
                    .context("native platform revocation export task failed")?
                }
                MissionDispatchCommand::AuditPlatformTerminal(args) => {
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::platform_terminal::audit(args)
                    })
                    .await
                    .context("native platform terminal observation failed")?
                }
                MissionDispatchCommand::DescribeStudy(args) => {
                    mission_dispatch::sequence_admission::describe(args)
                }
                MissionDispatchCommand::SignRoot(args) => {
                    mission_dispatch::study_authority::sign_root(args)
                }
                MissionDispatchCommand::SignStudy(args) => {
                    mission_dispatch::study_authority::sign_study(args)
                }
                MissionDispatchCommand::RegisterStudy(args) => {
                    require_cloud_data_host(std::env::consts::OS)?;
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::study_authority::register(args)
                    })
                    .await
                    .context("Campaign Study registration failed")?
                }
                MissionDispatchCommand::InspectStudy(args) => {
                    require_cloud_data_host(std::env::consts::OS)?;
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::study_authority::inspect(args)
                    })
                    .await
                    .context("Campaign Study readback failed")?
                }
                MissionDispatchCommand::InitStageAuthority(args) => {
                    require_cloud_data_host(std::env::consts::OS)?;
                    mission_dispatch::stage_controller::init_authority(args)
                }
                MissionDispatchCommand::StageController(args) => {
                    require_cloud_data_host(std::env::consts::OS)?;
                    tokio::task::spawn_blocking(move || {
                        mission_dispatch::stage_controller::run(args)
                    })
                    .await
                    .context("Campaign stage controller failed")?
                }
                MissionDispatchCommand::Submit(args) => {
                    require_cloud_data_host(std::env::consts::OS)?;
                    tokio::task::spawn_blocking(move || mission_dispatch::submit(args))
                        .await
                        .context("Campaign dispatch worker failed")?
                }
                MissionDispatchCommand::Inspect(args) => mission_dispatch::inspect(args),
                MissionDispatchCommand::Status(args) => mission_dispatch::status(args),
                MissionDispatchCommand::CloseFamily(args) => {
                    mission_dispatch::final_authority::close_family(args)
                }
                MissionDispatchCommand::ControllerHandoff(args) => {
                    mission_dispatch::controller::render(args)
                }
                MissionDispatchCommand::PrepareController(args) => {
                    mission_dispatch::controller::prepare(args)
                }
                MissionDispatchCommand::Settle(args) => {
                    require_cloud_data_host(std::env::consts::OS)?;
                    tokio::task::spawn_blocking(move || mission_dispatch::settle(args))
                        .await
                        .context("Campaign settlement worker failed")?
                }
            },
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
            DataCommand::FreezeInventory(args) => {
                tokio::task::spawn_blocking(move || data_mission::freeze_research_inventory(args))
                    .await
                    .context("inventory freezer worker failed")?
            }
            DataCommand::VerifyMaterializationManifest(args) => {
                tokio::task::spawn_blocking(move || {
                    data_mission::verify_materialization_manifest(args)
                })
                .await
                .context("materialization manifest verifier worker failed")?
            }
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
                hft_research_artifacts::write_json_atomic(&output, &manifest)?;
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
                hft_research_artifacts::write_json_atomic(&args.manifest_out, &manifest)?;
                print_json(&serde_json::json!({
                    "manifest": manifest,
                    "manifest_path": args.manifest_out,
                }))
            }
        },
        Command::Candidate { command } => match command {
            CandidateCommand::List(args) => governance::candidate_list(args),
            CandidateCommand::Show(args) => governance::candidate_show(args),
            #[cfg(feature = "onnx-compatibility")]
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
        Command::Research { command } => match command {
            ResearchCommand::SecOrderflowAudit(args) => {
                let report =
                    crate::sec_orderflow::audit(crate::sec_orderflow::SecOrderflowAuditRequest {
                        config: args.config,
                        input_root: args.input_root,
                    })?;
                print_json(&report)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_metrics_cli_accepts_multiple_backtests_and_explicit_selection() {
        let cli = Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "model-metrics",
            "--backtest",
            "ridge.json",
            "cart.json",
            "--selection",
            "selection.json",
            "--benchmark",
            "ridge",
            "--output",
            "metrics.json",
        ])
        .unwrap();
        match cli.command {
            Command::Mission {
                command: MissionCommand::ModelMetrics(args),
            } => {
                assert_eq!(args.backtest.len(), 2);
                assert_eq!(args.selection, [PathBuf::from("selection.json")]);
                assert_eq!(args.output, PathBuf::from("metrics.json"));
                assert!(matches!(args.benchmark, ModelMetricsBenchmark::Ridge));
            }
            _ => panic!("expected read-only model metrics command"),
        }
    }
    use clap::CommandFactory;

    #[test]
    fn help_defaults_to_the_campaign_surface() {
        let mut root = Cli::command();
        let root_help = root.render_help().to_string();
        assert!(root_help
            .lines()
            .any(|line| line.split_whitespace().next() == Some("loop")));

        let loop_help = root
            .find_subcommand_mut("loop")
            .unwrap()
            .render_help()
            .to_string();
        assert!(!loop_help
            .lines()
            .any(|line| line.split_whitespace().next() == Some("run")));
        assert!(loop_help
            .lines()
            .any(|line| line.split_whitespace().next() == Some("status")));

        let mission_help = root
            .find_subcommand_mut("mission")
            .unwrap()
            .render_help()
            .to_string();
        let listed = |name| {
            mission_help
                .lines()
                .any(|line| line.split_whitespace().next() == Some(name))
        };
        for command in [
            "create",
            "execute",
            "run",
            "resume",
            "learn",
            "recover-legacy-checkpoint",
            "campaign-execute",
        ] {
            assert!(!listed(command), "{command} must stay diagnostic-only");
        }
        for command in [
            "campaign-freeze",
            "campaign-finalize",
            "campaign-id",
            "prepare-fresh-inputs",
            "dispatch",
        ] {
            assert!(listed(command), "{command} must stay on the Campaign path");
        }
        #[cfg(feature = "scientific")]
        {
            let mut worker = WorkerCli::command();
            let worker_help = worker
                .find_subcommand_mut("mission")
                .unwrap()
                .render_help()
                .to_string();
            assert!(worker_help
                .lines()
                .any(|line| { line.split_whitespace().next() == Some("campaign-execute") }));
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
        let mission_id = format!("cex-mission-{}", "a".repeat(64));
        let holdout_id = "holdout-test".to_string();
        let mission_result_dir = root.path().join(format!("mission-id={mission_id}"));
        let result = mission_result_dir
            .join("attempt=test/results.zip")
            .display()
            .to_string();
        let (_, holdout_claim) = crate::mission_objects::cex_result_attempt_and_holdout_claim(
            &result,
            &mission_id,
            &holdout_id,
        )
        .unwrap();
        let cli = Cli::try_parse_from(vec![
            "alpha-harness".to_owned(),
            "mission".to_owned(),
            "execute".to_owned(),
            "--work-dir".to_owned(),
            work_dir,
            "--mission-id".to_owned(),
            mission_id,
            "--holdout-id".to_owned(),
            holdout_id,
            "--mission-url".to_owned(),
            missing_mission.clone(),
            "--mission-sha256".to_owned(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            "--feature-url".to_owned(),
            missing_features,
            "--materialization-url".to_owned(),
            missing_materialization,
            "--replay-artifact-url".to_owned(),
            "missing-replay.parquet".to_owned(),
            "--replay-artifact-sha256".to_owned(),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            "--replay-manifest-url".to_owned(),
            "missing-replay-manifest.json".to_owned(),
            "--replay-manifest-sha256".to_owned(),
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned(),
            "--result-put-url".to_owned(),
            result.clone(),
            "--result-readback-url".to_owned(),
            result,
            "--holdout-claim-put-url".to_owned(),
            holdout_claim.clone(),
            "--holdout-claim-readback-url".to_owned(),
            holdout_claim,
        ])
        .unwrap();

        let error = run(cli).await.unwrap_err();

        assert!(
            format!("{error:#}")
                .contains(&format!("failed to open local source {missing_mission}")),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn parses_mission_campaign_execute() {
        let args = "alpha-harness mission campaign-execute --work-dir work --campaign-id cex-campaign-1234567890abcdef1234567890abcdef --image-identity aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --request campaign.json --request-sha256 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_err());
        assert!(Cli::try_parse_from(format!("{args} --pre-holdout").split_whitespace()).is_err());
        #[cfg(feature = "scientific")]
        {
            assert!(WorkerCli::try_parse_from(args.split_whitespace()).is_err());
            assert!(
                WorkerCli::try_parse_from(format!("{args} --pre-holdout").split_whitespace())
                    .is_ok()
            );
            for suffix in [
                " --final-evaluation",
                " --final-trusted-keys keys.json",
                " --final-evaluation --final-trusted-keys keys.json --pre-holdout",
            ] {
                assert!(
                    WorkerCli::try_parse_from(format!("{args}{suffix}").split_whitespace())
                        .is_err()
                );
            }
            assert!(WorkerCli::try_parse_from(
                format!("{args} --final-evaluation --final-trusted-keys keys.json")
                    .split_whitespace()
            )
            .is_ok());
        }
    }

    #[test]
    fn parses_mission_campaign_freeze() {
        let args = "alpha-harness mission campaign-freeze --campaign-inputs campaign-inputs.json --input-root /mounted/run --source-revision aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --image registry/research-runner@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --campaign-root https://monday-lob-apne1-1045353359.oss-ap-northeast-1-internal.aliyuncs.com/research/campaigns --seed 7 --seed 11 --output freeze.json";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
        let final_args = args.replace(
            "--seed 7 --seed 11",
            "--final-evaluation-control final-control.json",
        );
        assert!(Cli::try_parse_from(final_args.split_whitespace()).is_ok());
        for suffix in [" --seed 7", " --research-plan plan.json"] {
            assert!(
                Cli::try_parse_from(format!("{final_args}{suffix}").split_whitespace()).is_err()
            );
        }
        assert!(
            Cli::try_parse_from(args.replace("--seed 7 --seed 11", "").split_whitespace()).is_err()
        );
    }

    #[test]
    fn parses_mission_campaign_learn() {
        let args = "alpha-harness mission campaign-learn --control control.json --submission submission.json --settlement settlement.json --namespace monday-research --request campaign.json --result campaign-result.json --result-sha256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --output next-plan.json";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());

        let obsolete = "alpha-harness mission campaign-learn --control control.json --submission submission.json --settlement settlement.json --namespace monday-research --request campaign.json --result campaign-result.json --result-sha256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --max-tokens 300 --output next-plan.json";
        assert!(Cli::try_parse_from(obsolete.split_whitespace()).is_err());
        for required in [
            "--control control.json ",
            "--submission submission.json ",
            "--settlement settlement.json ",
            "--namespace monday-research ",
        ] {
            assert!(Cli::try_parse_from(args.replace(required, "").split_whitespace()).is_err());
        }
    }

    #[test]
    fn parses_mission_campaign_finalize() {
        let args = "alpha-harness mission campaign-finalize --freeze freeze.json --signed-request signed-request.json --attempt-id attempt-001 --image registry/research-runner@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa --request-out request.json --submission-out submission.json";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[test]
    fn parses_mission_campaign_id() {
        let args = "alpha-harness mission campaign-id --request campaign.json";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[test]
    fn parses_bounded_fresh_input_preparation() {
        let args = [
            "alpha-harness",
            "mission",
            "prepare-fresh-inputs",
            "--raw-root",
            "/archive/raw",
            "--reference-root",
            "/archive/reference",
            "--market",
            "usdm",
            "--start-received-at-ns",
            "1700000000000000000",
            "--end-received-at-ns",
            "1700000060000000000",
            "--symbol",
            "BTCUSDT",
            "--image-ref",
            "registry/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--mission-id",
            "fresh-window",
            "--output-prefix",
            "campaigns/fresh-window",
            "--bucket-ms",
            "1000",
            "--label-horizon-buckets",
            "5",
            "--top-depth",
            "5",
            "--max-inputs",
            "64",
            "--max-input-bytes",
            "1000000",
            "--inventory-out",
            "/work/frozen.env",
            "--request-out",
            "/output/.fresh-inputs/campaigns/fresh-window/request.json",
            "--campaign-inputs-out",
            "/output/campaigns/fresh-window/receipts/campaign-inputs.json",
            "--output-root",
            "/output",
            "--materializer",
            "/usr/local/bin/cex-materialization-entrypoint.sh",
            "--materializer-work-dir",
            "/work/materializer",
        ];
        assert!(Cli::try_parse_from(args).is_ok());
    }

    #[test]
    fn parses_latest_bounded_fresh_input_preparation() {
        let args = "alpha-harness mission prepare-fresh-inputs \
            --raw-root /archive/raw \
            --reference-root /archive/reference \
            --market usdm \
            --duration-ns 3600000000000 \
            --cutoff-received-at-ns 1700000060000000000 \
            --max-candidates 8 \
            --symbol BTCUSDT \
            --image-ref registry/research@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
            --mission-id fresh-window \
            --output-prefix campaigns/fresh-window \
            --bucket-ms 1000 \
            --label-horizon-buckets 5 \
            --top-depth 5 \
            --max-inputs 64 \
            --max-input-bytes 1000000 \
            --inventory-out /work/frozen.env \
            --request-out /output/.fresh-inputs/campaigns/fresh-window/request.json \
            --campaign-inputs-out /output/campaigns/fresh-window/receipts/campaign-inputs.json \
            --output-root /output \
            --materializer /usr/local/bin/cex-materialization-entrypoint.sh \
            --materializer-work-dir /work/materializer";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[test]
    fn parses_mission_dispatch_submit() {
        let args = "alpha-harness mission dispatch submit --submission submission.json --context ack --namespace monday-research";
        assert!(Cli::try_parse_from(args.split_whitespace()).is_ok());
    }

    #[test]
    fn prediction_worker_commands_are_not_cex_execution_paths() {
        let args = "alpha-harness prediction dispatch render --submission submission.json --namespace monday-research";
        let error = Cli::try_parse_from(args.split_whitespace()).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        for command in ["execute", "snapshot", "dispatch"] {
            assert!(Cli::try_parse_from(["alpha-harness", "prediction", command]).is_err());
        }
    }

    #[test]
    fn parses_mission_and_data_control_plane_commands() {
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "dispatch",
            "close-family",
            "--ledger",
            "/ledger/alpha.duckdb",
            "--signed-grant",
            "/authority/final.json",
            "--trusted-keys",
            "/authority/keys.json",
            "--approval-id",
            "final-approval",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness", "data", "freeze-inventory",
            "--raw-root", "/archive/raw", "--reference-root", "/archive/reference",
            "--market", "usdm",
            "--start-received-at-ns", "1700000000000000000", "--end-received-at-ns", "1700000060000000000",
            "--symbol", "BTCUSDT", "--image-ref", "registry/runner@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--mission-id", "data-test", "--output-prefix", "runs/test", "--bucket-ms", "1000",
            "--label-horizon-buckets", "5", "--top-depth", "5", "--max-input-bytes", "1000000",
            "--output", "frozen.env",
        ]).is_ok());
        let handoff_args = [
            "alpha-harness",
            "mission",
            "dispatch",
            "controller-handoff",
            "--submission",
            "submission.json",
            "--control",
            "control.json",
            "--volume-root",
            "/campaign-root",
            "--work-dir",
            "/campaign-root/cycles/study",
            "--pvc",
            "study-ledger",
            "--service-account",
            "approved-operator",
            "--trusted-keys-configmap",
            "approved-public-keys",
            "--campaign-pod",
            "worker-pod",
            "--context",
            "monday-research-apne1",
            "--namespace",
            "monday-research",
            "--output",
            "private-handoff.json",
        ];
        assert!(Cli::try_parse_from(handoff_args)
            .unwrap_err()
            .to_string()
            .contains("--deadline-at"));
        let mut bounded_handoff = handoff_args.to_vec();
        bounded_handoff.extend(["--deadline-at", "2030-01-01T00:00:00Z"]);
        assert!(Cli::try_parse_from(bounded_handoff).is_ok());
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "dispatch",
            "prepare-controller",
            "--control",
            "/authority/control.json",
            "--context",
            "monday-research-apne1",
            "--namespace",
            "monday-research",
            "--kubeconfig-out",
            "/tmp/monday-campaign-kubeconfig.json",
        ])
        .is_ok());
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
        assert_eq!(
            Cli::try_parse_from([
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
            .unwrap_err()
            .kind(),
            clap::error::ErrorKind::InvalidSubcommand
        );
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
        assert_eq!(Cli::try_parse_from([
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
            "--partition-view-json",
            r#"{"common_time_boundary_ms":1,"train_market_ids":["train"],"crossing_excluded_market_ids":[],"held_out_market_ids":["held"]}"#,
            "--resume-url",
            "previous-results.zip",
            "--resume-sha256",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "--result-put-url",
            "results.zip",
            "--result-readback-url",
            "results.zip",
        ])
        .unwrap_err().kind(), clap::error::ErrorKind::InvalidSubcommand);
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--mission-id",
            "cex-mission-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--holdout-id",
            "holdout-test",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--replay-artifact-url",
            "replay.parquet",
            "--replay-artifact-sha256",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "--replay-manifest-url",
            "replay-manifest.json",
            "--replay-manifest-sha256",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "--resume-url",
            "checkpoint.json",
            "--resume-sha256",
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "--result-put-url",
            "results.zip",
            "--result-readback-url",
            "results.zip",
            "--holdout-claim-put-url",
            "sealed-holdout-claim.json",
            "--holdout-claim-readback-url",
            "sealed-holdout-claim.json",
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
    #[cfg(feature = "scientific")]
    fn mission_execute_accepts_only_content_bound_mission_transport() {
        let cli = Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--mission-id",
            "cex-mission-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--holdout-id",
            "holdout-test",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--replay-artifact-url",
            "replay.parquet",
            "--replay-artifact-sha256",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "--replay-manifest-url",
            "replay-manifest.json",
            "--replay-manifest-sha256",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "--result-put-url",
            "results.zip",
            "--result-readback-url",
            "results.zip",
            "--holdout-claim-put-url",
            "sealed-holdout-claim.json",
            "--holdout-claim-readback-url",
            "sealed-holdout-claim.json",
        ])
        .expect("content-bound Mission transport must be accepted");

        let Command::Mission {
            command: MissionCommand::Execute(args),
        } = cli.command
        else {
            panic!("expected mission execute command")
        };
        assert_eq!(args.mission_url, "mission.json");
        assert_eq!(
            args.mission_id,
            "cex-mission-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(args.mission_sha256, "b".repeat(64));
        assert!(args.resume_url.is_none());
        assert!(args.resume_sha256.is_none());

        assert!(Cli::try_parse_from([
            "alpha-harness",
            "mission",
            "execute",
            "--work-dir",
            "work",
            "--mission-id",
            "cex-mission-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--holdout-id",
            "holdout-test",
            "--mission-url",
            "mission.json",
            "--mission-sha256",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--feature-url",
            "features.jsonl",
            "--materialization-url",
            "materialization.json",
            "--replay-artifact-url",
            "replay.parquet",
            "--replay-artifact-sha256",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "--replay-manifest-url",
            "replay-manifest.json",
            "--replay-manifest-sha256",
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "--result-put-url",
            "results.zip",
            "--result-readback-url",
            "results.zip",
            "--holdout-claim-put-url",
            "sealed-holdout-claim.json",
            "--holdout-claim-readback-url",
            "sealed-holdout-claim.json",
            "--objective",
            "alternate authority",
        ])
        .is_err());
    }

    #[test]
    fn parses_candidate_show() {
        let cli = Cli::try_parse_from([
            "alpha-harness",
            "candidate",
            "show",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
            "--candidate-id",
            "candidate-1",
        ])
        .unwrap();
        let Command::Candidate {
            command: CandidateCommand::Show(args),
        } = cli.command
        else {
            panic!("expected candidate show command")
        };
        assert_eq!(args.mission_id, "mission-1");
        assert_eq!(args.candidate_id, "candidate-1");
        assert!(Cli::try_parse_from([
            "alpha-harness",
            "candidate",
            "show",
            "--db",
            "alpha.duckdb",
            "--mission-id",
            "mission-1",
        ])
        .is_err());
    }

    #[test]
    fn exposes_no_order_or_trade_command() {
        assert!(Cli::try_parse_from(["alpha-harness", "order"]).is_err());
        assert!(Cli::try_parse_from(["alpha-harness", "trade"]).is_err());
    }

    #[test]
    fn parses_diagnostic_sec_orderflow_audit_without_input_root() {
        let cli = Cli::try_parse_from([
            "alpha-harness",
            "research",
            "sec-orderflow-audit",
            "--config",
            "sec-orderflow.json",
        ])
        .unwrap();
        let Command::Research {
            command: ResearchCommand::SecOrderflowAudit(args),
        } = cli.command
        else {
            panic!("expected research sec-orderflow-audit");
        };
        assert_eq!(args.config, PathBuf::from("sec-orderflow.json"));
        assert!(args.input_root.is_none());
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
