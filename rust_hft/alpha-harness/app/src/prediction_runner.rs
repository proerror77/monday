use crate::{
    cli::{print_json, PredictionExecuteArgs},
    data_mission,
    mission_runner::{
        configured_sibling_binary, create_bundle, fetch_to_file, normalized_sha256, publish_result,
        sha256_file,
    },
};
use anyhow::{bail, Context};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::Read,
    path::{Component, Path},
    process::{Command, Stdio},
    time::Duration,
};
use zip::ZipArchive;

const MAX_MISSION_BYTES: u64 = 1024 * 1024;
const MAX_SNAPSHOT_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_RESUME_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_SNAPSHOT_EXTRACTED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_SNAPSHOT_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PredictionTaskKind {
    SettlementProbability,
    UpExecution,
    DownExecution,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PredictionTokenSide {
    Up,
    Down,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PredictionTaskIdentity {
    kind: PredictionTaskKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    side: Option<PredictionTokenSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prediction_horizon_secs: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthenticatedPartitionViewIdentity {
    common_time_boundary_ms: i64,
    train_market_ids: Vec<String>,
    crossing_excluded_market_ids: Vec<String>,
    held_out_market_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PredictionMissionV4Identity {
    schema_version: String,
    mission_id: String,
    task: PredictionTaskIdentity,
    run_mode: String,
    cohort_manifest_id: String,
    partition_digest: String,
    causal_projection_policy_id: String,
    snapshot_contract_id: String,
    snapshot_hash: String,
    search_policy_snapshot_id: String,
}

#[derive(Debug)]
enum PredictionMissionIdentity {
    PipelineSmoke(PredictionMissionV4Identity),
    ResearchTrial(PredictionMissionV4Identity),
}

impl PredictionMissionIdentity {
    fn mission_id(&self) -> &str {
        match self {
            Self::PipelineSmoke(mission) | Self::ResearchTrial(mission) => &mission.mission_id,
        }
    }

    fn snapshot_contract_id(&self) -> &str {
        match self {
            Self::PipelineSmoke(mission) | Self::ResearchTrial(mission) => {
                &mission.snapshot_contract_id
            }
        }
    }

    fn policy_identity(&self) -> &str {
        match self {
            Self::PipelineSmoke(mission) | Self::ResearchTrial(mission) => {
                &mission.search_policy_snapshot_id
            }
        }
    }

    fn is_pipeline_smoke(&self) -> bool {
        matches!(self, Self::PipelineSmoke(_))
    }

    fn run_mode(&self) -> &'static str {
        if self.is_pipeline_smoke() {
            "pipeline_smoke"
        } else {
            "research_trial"
        }
    }

    fn task(&self) -> &PredictionTaskIdentity {
        match self {
            Self::PipelineSmoke(mission) | Self::ResearchTrial(mission) => &mission.task,
        }
    }

    fn matches_admitted_identity(&self, args: &PredictionExecuteArgs) -> bool {
        match self {
            Self::PipelineSmoke(mission) | Self::ResearchTrial(mission) => {
                mission.cohort_manifest_id == args.cohort_manifest_id
                    && mission.partition_digest == args.partition_digest
                    && mission.causal_projection_policy_id == args.policy_identity
                    && mission.snapshot_hash == args.snapshot_digest
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct PredictionSnapshotIdentity {
    schema_version: String,
    snapshot_hash: String,
    snapshot_contract_hash: String,
    source_kind: String,
}

#[derive(Debug, Serialize)]
struct PredictionExecutionEvidence<'a> {
    lane: &'static str,
    mission_id: &'a str,
    data_snapshot_id: &'a str,
    evaluator_version: &'a str,
    mission_sha256: &'a str,
    snapshot_archive_sha256: &'a str,
    snapshot_archive_source: &'static str,
    partition_digest: &'a str,
    policy_identity: &'a str,
    task_capability: &'a str,
    image_identity: &'a str,
    run_mode: &'static str,
    resume_bundle_sha256: Option<&'a str>,
    runner_exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
struct PredictionExecutionReport<'a> {
    #[serde(flatten)]
    evidence: PredictionExecutionEvidence<'a>,
    bundle_bytes: u64,
    bundle_sha256: String,
    readback_bundle_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline_smoke: Option<PipelineSmokeCompletion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    research_trial: Option<ResearchTrialCompletion>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PipelineSmokeCompletion {
    schema_version: String,
    status: String,
    mission_id: String,
    task: String,
    snapshot_contract_id: String,
    snapshot_digest: String,
    search_policy_snapshot_id: String,
    evaluator_report_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResearchTrialCompletion {
    schema_version: String,
    status: String,
    mission_id: String,
    task: PredictionTaskIdentity,
    snapshot_contract_id: String,
    snapshot_digest: String,
    search_policy_snapshot_id: String,
    receipt_path: String,
    receipt_artifact_sha256: String,
    receipt_sha256: String,
}

pub fn execute(args: PredictionExecuteArgs) -> anyhow::Result<()> {
    let runner = configured_sibling_binary(
        "MONDAY_PREDICTION_RESEARCH_BIN",
        "monday-prediction-research",
    )?;
    execute_with_runner(args, &runner)
}

fn execute_with_runner(args: PredictionExecuteArgs, runner: &Path) -> anyhow::Result<()> {
    validate_execute_args(&args)?;
    let input_dir = args.work_dir.join("input");
    let artifact_dir = args.work_dir.join("artifacts");
    let results_dir = args.work_dir.join("results");
    data_mission::ensure_real_directory(&args.work_dir, "prediction work")?;
    data_mission::ensure_real_directory(&input_dir, "prediction input")?;
    data_mission::ensure_real_directory(&artifact_dir, "prediction artifact")?;
    ensure_empty_results_directory(&results_dir)?;
    let stdout_path = artifact_dir.join("runner.stdout");
    let stderr_path = artifact_dir.join("runner.stderr");
    data_mission::ensure_output_path_is_not_symlink(&stdout_path, "prediction runner stdout")?;
    data_mission::ensure_output_path_is_not_symlink(&stderr_path, "prediction runner stderr")?;

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;
    let mission_path = input_dir.join("mission.json");
    let snapshot_archive = input_dir.join("snapshot.zip");
    let (_, mission_sha256) =
        fetch_to_file(&client, &args.mission_url, &mission_path, MAX_MISSION_BYTES)?;
    if mission_sha256 != normalized_sha256("mission", &args.mission_sha256)? {
        bail!("prediction mission SHA256 mismatch");
    }
    let mission = parse_prediction_mission_identity(&mission_path)?;
    validate_mission_identity(&mission)?;

    let (snapshot_sha256, snapshot_archive_source) =
        stage_snapshot_archive(&client, &args, &snapshot_archive)?;
    // The declared archive SHA-256 is the trust anchor. A prior work-dir
    // extraction is writable state, so a retry always extracts into a fresh
    // private directory instead of attempting to reuse it.
    let snapshot_dir = tempfile::Builder::new()
        .prefix("prediction-snapshot-")
        .tempdir_in(&input_dir)
        .with_context(|| {
            format!(
                "create isolated prediction snapshot directory in {}",
                input_dir.display()
            )
        })?;
    extract_archive(&snapshot_archive, snapshot_dir.path(), None)?;
    make_files_read_only(snapshot_dir.path())?;
    verify_admitted_snapshot_identity(&args, &mission, snapshot_dir.path())?;

    let resume_bundle_sha256 = if let Some((resume_url, resume_sha256)) = resume_source(&args)? {
        let resume_archive = input_dir.join("resume.zip");
        let (_, actual_sha256) = fetch_to_file(
            &client,
            resume_url,
            &resume_archive,
            MAX_RESUME_ARCHIVE_BYTES,
        )?;
        if actual_sha256 != normalized_sha256("resume bundle", resume_sha256)? {
            bail!("prediction resume bundle SHA256 mismatch");
        }
        extract_archive(&resume_archive, &args.work_dir, Some(Path::new("results")))?;
        Some(actual_sha256)
    } else {
        None
    };

    let stdout = data_mission::temporary_output_file(&stdout_path, ".monday-artifact-log-")?;
    let stderr = data_mission::temporary_output_file(&stderr_path, ".monday-artifact-log-")?;
    let mut command = Command::new(runner);
    command.arg(if mission.is_pipeline_smoke() {
        "--pipeline-smoke"
    } else {
        "--research-trial"
    });
    let status = command
        .arg(&mission_path)
        .arg(snapshot_dir.path())
        .arg(&results_dir)
        .args([
            "--admitted-cohort-manifest-id",
            args.cohort_manifest_id.as_str(),
            "--admitted-partition-digest",
            args.partition_digest.as_str(),
            "--admitted-policy-identity",
            args.policy_identity.as_str(),
            "--admitted-snapshot-contract-id",
            args.snapshot_contract_id.as_str(),
            "--admitted-snapshot-digest",
            args.snapshot_digest.as_str(),
            "--admitted-partition-view-json",
            args.partition_view_json.as_str(),
            "--immutable-image-identity",
            args.image_identity.as_str(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen()?))
        .stderr(Stdio::from(stderr.reopen()?))
        .status()
        .with_context(|| format!("start prediction research runner {}", runner.display()))?;
    stdout.as_file().sync_all()?;
    stderr.as_file().sync_all()?;
    data_mission::persist_output_file(stdout, &stdout_path, "prediction runner stdout")?;
    data_mission::persist_output_file(stderr, &stderr_path, "prediction runner stderr")?;
    let pipeline_smoke = if mission.is_pipeline_smoke() && status.success() {
        Some(read_pipeline_smoke_completion(
            &stdout_path,
            &mission,
            &args,
        )?)
    } else {
        None
    };
    if let Some(completion) = pipeline_smoke.as_ref() {
        verify_pipeline_smoke_report_digest(&results_dir, completion)?;
    }
    let research_trial = if !mission.is_pipeline_smoke() && status.success() {
        Some(read_research_trial_completion(
            &stdout_path,
            &mission,
            &args,
            &results_dir,
        )?)
    } else {
        None
    };
    let evidence = PredictionExecutionEvidence {
        lane: "prediction_market",
        mission_id: mission.mission_id(),
        data_snapshot_id: mission.snapshot_contract_id(),
        evaluator_version: mission.policy_identity(),
        mission_sha256: &mission_sha256,
        snapshot_archive_sha256: &snapshot_sha256,
        snapshot_archive_source,
        partition_digest: &args.partition_digest,
        policy_identity: &args.policy_identity,
        task_capability: &args.task_capability,
        image_identity: &args.image_identity,
        run_mode: mission.run_mode(),
        resume_bundle_sha256: resume_bundle_sha256.as_deref(),
        runner_exit_code: status.code(),
    };
    data_mission::write_json_atomic(&artifact_dir.join("execution-evidence.json"), &evidence)?;

    let bundle = args.work_dir.join("results.zip");
    create_bundle(&args.work_dir, &bundle, [&results_dir, &artifact_dir])?;
    let bundle_bytes = bundle.metadata()?.len();
    let bundle_sha256 = sha256_file(&bundle)?;
    publish_result(&client, &args.result_put_url, &bundle)?;
    let readback_bundle = input_dir.join("published-result-readback.zip");
    let (_, readback_sha256) = fetch_to_file(
        &client,
        &args.result_readback_url,
        &readback_bundle,
        MAX_RESUME_ARCHIVE_BYTES,
    )?;
    if readback_sha256 != bundle_sha256 {
        bail!("published prediction result readback SHA256 mismatch");
    }
    let report = PredictionExecutionReport {
        evidence,
        bundle_bytes,
        bundle_sha256,
        readback_bundle_sha256: readback_sha256,
        pipeline_smoke,
        research_trial,
    };
    // The readback hash proves the published bundle is the exact bundle whose smoke
    // report was verified before publication. Keep local status evidence outside that
    // hash cycle after readback is verified.
    data_mission::write_json_atomic(&artifact_dir.join("execution-evidence.json"), &report)?;
    print_json(&report)?;
    if !status.success() {
        bail!(
            "prediction research runner exited unsuccessfully with {:?}; immutable evidence was published",
            status.code()
        );
    }
    Ok(())
}

fn read_pipeline_smoke_completion(
    stdout_path: &Path,
    mission: &PredictionMissionIdentity,
    args: &PredictionExecuteArgs,
) -> anyhow::Result<PipelineSmokeCompletion> {
    let bytes = std::fs::read(stdout_path)
        .with_context(|| format!("read pipeline smoke completion {}", stdout_path.display()))?;
    if bytes.len() as u64 > MAX_MISSION_BYTES {
        bail!("pipeline smoke completion exceeds {MAX_MISSION_BYTES} bytes");
    }
    let completion: PipelineSmokeCompletion = serde_json::from_slice(&bytes)
        .context("pipeline smoke completion must be a typed JSON object")?;
    if completion.schema_version != "monday.prediction.pipeline_smoke.result.v1"
        || completion.status != "completed"
        || completion.mission_id != mission.mission_id()
        || completion.task != "settlement_probability"
        || completion.snapshot_contract_id != args.snapshot_contract_id
        || completion.snapshot_digest != args.snapshot_digest
        || completion.search_policy_snapshot_id != args.policy_identity
    {
        bail!("pipeline smoke completion does not bind the admitted identity");
    }
    normalized_sha256(
        "pipeline smoke evaluator report",
        &completion.evaluator_report_sha256,
    )?;
    Ok(completion)
}

fn verify_pipeline_smoke_report_digest(
    results_dir: &Path,
    completion: &PipelineSmokeCompletion,
) -> anyhow::Result<()> {
    let report_dir = results_dir.join("reports");
    let reports = std::fs::read_dir(&report_dir)
        .with_context(|| {
            format!(
                "read pipeline smoke report directory {}",
                report_dir.display()
            )
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pipeline-smoke-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    let [report] = reports.as_slice() else {
        bail!(
            "expected exactly one pipeline smoke evaluator report, found {}",
            reports.len()
        );
    };
    if sha256_file(report)?
        != normalized_sha256(
            "pipeline smoke evaluator report",
            &completion.evaluator_report_sha256,
        )?
    {
        bail!("pipeline smoke evaluator report SHA256 does not match its completion");
    }
    Ok(())
}

fn read_research_trial_completion(
    stdout_path: &Path,
    mission: &PredictionMissionIdentity,
    args: &PredictionExecuteArgs,
    results_dir: &Path,
) -> anyhow::Result<ResearchTrialCompletion> {
    let bytes = std::fs::read(stdout_path)
        .with_context(|| format!("read research trial completion {}", stdout_path.display()))?;
    if bytes.len() as u64 > MAX_MISSION_BYTES {
        bail!("research trial completion exceeds {MAX_MISSION_BYTES} bytes");
    }
    let completion: ResearchTrialCompletion = serde_json::from_slice(&bytes)
        .context("research trial completion must be a typed JSON object")?;
    if completion.schema_version != "monday.prediction.research_trial.result.v1"
        || completion.status != "completed"
        || completion.mission_id != mission.mission_id()
        || &completion.task != mission.task()
        || completion.snapshot_contract_id != args.snapshot_contract_id
        || completion.snapshot_digest != args.snapshot_digest
        || completion.search_policy_snapshot_id != args.policy_identity
    {
        bail!("research trial completion does not bind the admitted identity");
    }
    immutable_sha256_identity(
        "research trial receipt artifact",
        &completion.receipt_artifact_sha256,
    )?;
    immutable_sha256_identity("research trial receipt", &completion.receipt_sha256)?;
    let relative = Path::new(&completion.receipt_path);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("research trial receipt path is invalid");
    }
    let root = std::fs::canonicalize(results_dir).context("canonicalize research results")?;
    let receipt_path = std::fs::canonicalize(results_dir.join(relative))
        .context("read research trial receipt path")?;
    if !receipt_path.starts_with(&root) || !receipt_path.is_file() {
        bail!("research trial receipt path escapes its result bundle");
    }
    if sha256_file(&receipt_path)?
        != completion
            .receipt_artifact_sha256
            .strip_prefix("sha256:")
            .unwrap_or_default()
    {
        bail!("research trial receipt artifact SHA256 does not match its completion");
    }
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(&receipt_path)?)
        .context("research trial receipt must be valid JSON")?;
    let receipt_sha256 = research_trial_receipt_sha256(&receipt)?;
    if receipt_sha256 != completion.receipt_sha256
        || receipt["schema_version"] != "prediction_authenticated_result_receipt.v1"
        || receipt["sha256"] != completion.receipt_sha256
        || receipt["mission"]["mission_id"] != mission.mission_id()
        || receipt["mission"]["task"] != serde_json::to_value(mission.task())?
        || receipt["mission"]["cohort_manifest_id"] != args.cohort_manifest_id
        || receipt["mission"]["partition_digest"] != args.partition_digest
        || receipt["mission"]["causal_projection_policy_id"] != args.policy_identity
        || receipt["mission"]["snapshot_contract_id"] != args.snapshot_contract_id
        || receipt["mission"]["snapshot_hash"] != args.snapshot_digest
        || receipt["mission"]["search_policy_snapshot_id"] != args.policy_identity
        || receipt["immutable_image_identity"] != args.image_identity
    {
        bail!("research trial receipt does not bind its completion");
    }
    Ok(completion)
}

fn research_trial_receipt_sha256(receipt: &serde_json::Value) -> anyhow::Result<String> {
    let mut payload = receipt.clone();
    let serde_json::Value::String(sha256) = &mut payload["sha256"] else {
        bail!("research trial receipt sha256 is missing");
    };
    sha256.clear();
    let mut bytes = serde_json::to_vec(&payload)?;
    bytes.push(b'\n');
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn make_files_read_only(root: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_symlink() {
            bail!("prediction snapshot extraction contains a symbolic link");
        }
        if metadata.is_dir() {
            make_files_read_only(&entry.path())?;
        } else if metadata.is_file() {
            let mut permissions = entry.metadata()?.permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(entry.path(), permissions)?;
        } else {
            bail!("prediction snapshot extraction contains a non-regular file");
        }
    }
    Ok(())
}

fn validate_execute_args(args: &PredictionExecuteArgs) -> anyhow::Result<()> {
    if args.work_dir.as_os_str().is_empty()
        || [
            args.mission_url.as_str(),
            args.mission_sha256.as_str(),
            args.snapshot_url.as_str(),
            args.snapshot_sha256.as_str(),
            args.snapshot_contract_id.as_str(),
            args.snapshot_digest.as_str(),
            args.cohort_manifest_id.as_str(),
            args.partition_digest.as_str(),
            args.policy_identity.as_str(),
            args.task_capability.as_str(),
            args.image_identity.as_str(),
            args.partition_view_json.as_str(),
            args.result_put_url.as_str(),
            args.result_readback_url.as_str(),
        ]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        bail!("prediction execution paths, URLs, and hashes are required");
    }
    immutable_sha256_identity("prediction snapshot contract", &args.snapshot_contract_id)?;
    immutable_sha256_identity("prediction cohort manifest", &args.cohort_manifest_id)?;
    immutable_sha256_identity("prediction partition", &args.partition_digest)?;
    immutable_sha256_identity("prediction policy", &args.policy_identity)?;
    immutable_sha256_identity("prediction image", &args.image_identity)?;
    validate_snapshot_digest(&args.snapshot_digest)?;
    if args.task_capability != "btc_5m_backtest" {
        bail!("prediction task capability is not admitted for the current snapshot contract");
    }
    validate_partition_view_json(&args.partition_view_json)?;
    resume_source(args)?;
    Ok(())
}

fn validate_partition_view_json(value: &str) -> anyhow::Result<()> {
    let view: AuthenticatedPartitionViewIdentity =
        serde_json::from_str(value).context("admitted partition view is invalid")?;
    let ids = view
        .train_market_ids
        .iter()
        .chain(&view.crossing_excluded_market_ids)
        .chain(&view.held_out_market_ids)
        .collect::<Vec<_>>();
    let unique = ids.iter().map(|id| id.as_str()).collect::<BTreeSet<_>>();
    if view.common_time_boundary_ms <= 0
        || ids.len() != unique.len()
        || unique.iter().any(|id| id.trim().is_empty())
    {
        bail!("admitted partition view is invalid");
    }
    Ok(())
}

fn stage_snapshot_archive(
    client: &Client,
    args: &PredictionExecuteArgs,
    destination: &Path,
) -> anyhow::Result<(String, &'static str)> {
    let expected_sha256 = normalized_sha256("snapshot archive", &args.snapshot_sha256)?;
    if let Some(cache_dir) = &args.snapshot_cache_dir {
        let cached_archive = cache_dir.join(format!("{expected_sha256}.zip"));
        if cached_archive.try_exists().with_context(|| {
            format!(
                "inspect prediction snapshot cache entry {}",
                cached_archive.display()
            )
        })? {
            let cache_source = cached_archive.to_string_lossy();
            let (_, actual_sha256) = fetch_to_file(
                client,
                cache_source.as_ref(),
                destination,
                MAX_SNAPSHOT_ARCHIVE_BYTES,
            )?;
            if actual_sha256 != expected_sha256 {
                bail!("prediction snapshot archive SHA256 mismatch");
            }
            return Ok((actual_sha256, "verified_cache"));
        }
    }

    let (_, actual_sha256) = fetch_to_file(
        client,
        &args.snapshot_url,
        destination,
        MAX_SNAPSHOT_ARCHIVE_BYTES,
    )?;
    if actual_sha256 != expected_sha256 {
        bail!("prediction snapshot archive SHA256 mismatch");
    }
    Ok((actual_sha256, "trusted_fetch"))
}

fn resume_source(args: &PredictionExecuteArgs) -> anyhow::Result<Option<(&str, &str)>> {
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
        (Some(url), Some(sha256)) => Ok(Some((url, sha256))),
        _ => bail!("prediction resume URL and SHA256 must be supplied together"),
    }
}

fn parse_prediction_mission_identity(path: &Path) -> anyhow::Result<PredictionMissionIdentity> {
    let bytes = std::fs::read(path)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("prediction mission identity is invalid JSON")?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if schema_version == "prediction_research_mission.v4" {
        let mission = serde_json::from_value::<PredictionMissionV4Identity>(value)
            .context("prediction Mission v4 identity is invalid")?;
        let identity = match mission.run_mode.as_str() {
            "pipeline_smoke" => PredictionMissionIdentity::PipelineSmoke(mission),
            "research_trial" => PredictionMissionIdentity::ResearchTrial(mission),
            _ => bail!("prediction Mission v4 run mode is unsupported"),
        };
        validate_mission_identity(&identity)?;
        return Ok(identity);
    }
    bail!("prediction execution requires Mission v4")
}

fn validate_mission_identity(mission: &PredictionMissionIdentity) -> anyhow::Result<()> {
    match mission {
        PredictionMissionIdentity::PipelineSmoke(mission)
            if mission.schema_version == "prediction_research_mission.v4"
                && mission.run_mode == "pipeline_smoke"
                && matches!(
                    (
                        &mission.task.kind,
                        &mission.task.side,
                        mission.task.prediction_horizon_secs
                    ),
                    (PredictionTaskKind::SettlementProbability, None, None)
                )
                && !mission.mission_id.trim().is_empty()
                && !mission.cohort_manifest_id.trim().is_empty()
                && !mission.partition_digest.trim().is_empty()
                && !mission.causal_projection_policy_id.trim().is_empty()
                && !mission.snapshot_contract_id.trim().is_empty()
                && !mission.snapshot_hash.trim().is_empty()
                && !mission.search_policy_snapshot_id.trim().is_empty() =>
        {
            Ok(())
        }
        PredictionMissionIdentity::ResearchTrial(mission)
            if mission.schema_version == "prediction_research_mission.v4"
                && mission.run_mode == "research_trial"
                && valid_research_task(&mission.task)
                && !mission.mission_id.trim().is_empty()
                && !mission.cohort_manifest_id.trim().is_empty()
                && !mission.partition_digest.trim().is_empty()
                && !mission.causal_projection_policy_id.trim().is_empty()
                && !mission.snapshot_contract_id.trim().is_empty()
                && !mission.snapshot_hash.trim().is_empty()
                && !mission.search_policy_snapshot_id.trim().is_empty() =>
        {
            Ok(())
        }
        _ => bail!("prediction mission identity or lane is invalid"),
    }
}

fn valid_research_task(task: &PredictionTaskIdentity) -> bool {
    matches!(
        (&task.kind, &task.side, task.prediction_horizon_secs),
        (PredictionTaskKind::SettlementProbability, None, None)
            | (
                PredictionTaskKind::UpExecution,
                Some(PredictionTokenSide::Up),
                Some(5 | 10 | 15 | 30)
            )
            | (
                PredictionTaskKind::DownExecution,
                Some(PredictionTokenSide::Down),
                Some(5 | 10 | 15 | 30)
            )
    )
}

fn verify_admitted_snapshot_identity(
    args: &PredictionExecuteArgs,
    mission: &PredictionMissionIdentity,
    snapshot_dir: &Path,
) -> anyhow::Result<()> {
    let snapshot: PredictionSnapshotIdentity = serde_json::from_slice(
        &std::fs::read(snapshot_dir.join("manifest.json"))
            .context("read extracted prediction snapshot manifest")?,
    )
    .context("prediction snapshot manifest identity is invalid JSON")?;
    if snapshot.schema_version != "research_snapshot_v2"
        || mission.snapshot_contract_id() != args.snapshot_contract_id
        || snapshot.snapshot_contract_hash != args.snapshot_contract_id
        || snapshot.snapshot_hash != args.snapshot_digest
        || mission.policy_identity() != args.policy_identity
        || !mission.matches_admitted_identity(args)
        || snapshot.source_kind != "polymarket_chainlink_baseline"
    {
        bail!("prediction mission, admitted snapshot contract, and snapshot manifest do not match");
    }
    Ok(())
}

fn immutable_sha256_identity(label: &str, value: &str) -> anyhow::Result<()> {
    let digest = value
        .strip_prefix("sha256:")
        .with_context(|| format!("{label} must use sha256:<64 lowercase hex>"))?;
    if value != format!("sha256:{digest}") || digest != normalized_sha256(label, digest)? {
        bail!("{label} must use sha256:<64 lowercase hex>");
    }
    Ok(())
}

fn validate_snapshot_digest(value: &str) -> anyhow::Result<()> {
    if value.len() != 16
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("prediction snapshot digest must be exactly 16 lowercase ASCII hex characters");
    }
    Ok(())
}

fn ensure_empty_results_directory(results_dir: &Path) -> anyhow::Result<()> {
    data_mission::ensure_real_directory(results_dir, "prediction results")?;
    if std::fs::read_dir(results_dir)?
        .next()
        .transpose()?
        .is_some()
    {
        bail!(
            "prediction results directory is not empty; start a new work directory and restore from a pinned resume bundle"
        );
    }
    Ok(())
}

fn extract_archive(
    archive_path: &Path,
    destination: &Path,
    required_prefix: Option<&Path>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    let mut archive = ZipArchive::new(File::open(archive_path)?)
        .context("prediction snapshot is not a valid ZIP archive")?;
    if archive.len() > MAX_SNAPSHOT_ENTRIES {
        bail!("prediction snapshot archive has too many entries");
    }
    let mut extracted_bytes = 0_u64;
    let mut extracted_files = 0_usize;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("prediction snapshot entry escapes destination"))?
            .to_path_buf();
        if relative.as_os_str().is_empty() {
            bail!("prediction snapshot entry has an empty path");
        }
        let mode_type = entry.unix_mode().unwrap_or_default() & 0o170000;
        if mode_type == 0o120000 {
            bail!("prediction snapshot archive cannot contain symbolic links");
        }
        let is_directory = entry.is_dir();
        let is_file = entry.is_file();
        if is_directory {
            if mode_type != 0 && mode_type != 0o040000 {
                bail!("prediction snapshot directory has an unsupported file type");
            }
        } else if !is_file || (mode_type != 0 && mode_type != 0o100000) {
            bail!("prediction snapshot archive contains a non-regular file");
        }
        if required_prefix.is_some_and(|prefix| !relative.starts_with(prefix)) {
            continue;
        }
        if is_directory {
            std::fs::create_dir_all(destination.join(relative))?;
            continue;
        }
        let entry_size = entry.size();
        extracted_bytes = extracted_bytes
            .checked_add(entry_size)
            .filter(|bytes| *bytes <= MAX_SNAPSHOT_EXTRACTED_BYTES)
            .ok_or_else(|| anyhow::anyhow!("prediction snapshot expands beyond the size limit"))?;
        let output_path = destination.join(relative);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .with_context(|| format!("create extracted snapshot file {}", output_path.display()))?;
        let bytes = std::io::copy(&mut entry.by_ref().take(entry_size + 1), &mut output)?;
        if bytes != entry_size {
            bail!("prediction snapshot entry size does not match ZIP metadata");
        }
        output.sync_all()?;
        extracted_files += 1;
    }
    if required_prefix.is_some() && extracted_files == 0 {
        bail!("prediction resume bundle contains no results state");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn execute_rejects_snapshot_hash_mismatch_before_starting_runner() {
        let fixture = execute_fixture("hash-mismatch");
        let mut args = fixture.args;
        args.snapshot_sha256 = "0".repeat(64);

        let error = execute_with_runner(args, Path::new("/runner-must-not-start"))
            .expect_err("mismatched archive must fail");

        assert!(error
            .to_string()
            .contains("prediction snapshot archive SHA256 mismatch"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_admitted_contract_mismatch_before_starting_runner() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("admitted-contract-mismatch");
        let marker = fixture.root.join("runner-started");
        let runner = fixture.root.join("must-not-start");
        std::fs::write(&runner, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut args = fixture.args;
        args.snapshot_contract_id = format!("sha256:{}", "2".repeat(64));

        let error = execute_with_runner(args, &runner)
            .expect_err("mismatched admitted contract must prevent runner execution");

        assert!(error.to_string().contains(
            "prediction mission, admitted snapshot contract, and snapshot manifest do not match"
        ));
        assert!(!marker.exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn execute_rejects_an_unadmitted_task_capability_before_fetching_inputs() {
        let fixture = execute_fixture("wrong-task-capability");
        let mut args = fixture.args;
        args.task_capability = "unsupported_task".to_owned();

        let error = execute_with_runner(args, Path::new("/runner-must-not-start"))
            .expect_err("an unadmitted task capability must fail closed");

        assert!(error
            .to_string()
            .contains("task capability is not admitted"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_reuses_verified_snapshot_cache_across_isolated_attempts() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("verified-cache");
        let cache_dir = fixture.root.join("snapshot-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::copy(
            fixture.root.join("input-snapshot.zip"),
            cache_dir.join(format!("{}.zip", fixture.args.snapshot_sha256)),
        )
        .unwrap();
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            successful_research_runner(
                &fixture,
                "mkdir -p \"$4\"\nprintf '{\"status\":\"completed\"}\\n' > \"$4/summary.json\"",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let missing_remote = fixture.root.join("remote-must-not-be-read");
        let mut first = fixture.args.clone();
        first.snapshot_cache_dir = Some(cache_dir.clone());
        first.snapshot_url = missing_remote.to_string_lossy().into_owned();

        execute_with_runner(first, &runner).unwrap();

        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                fixture
                    .args
                    .work_dir
                    .join("artifacts/execution-evidence.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(evidence["snapshot_archive_source"], "verified_cache");
        let second_result = fixture.root.join("second-results.zip");
        let mut second = fixture.args.clone();
        second.work_dir = fixture.root.join("second-work");
        second.result_put_url = second_result.to_string_lossy().into_owned();
        second.result_readback_url = second_result.to_string_lossy().into_owned();
        second.snapshot_cache_dir = Some(cache_dir);
        second.snapshot_url = missing_remote.to_string_lossy().into_owned();

        execute_with_runner(second, &runner).unwrap();

        assert!(fixture.result_path.is_file());
        assert!(second_result.is_file());
        assert_ne!(fixture.args.work_dir, fixture.root.join("second-work"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_corrupt_snapshot_cache_before_starting_runner() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("corrupt-cache");
        let cache_dir = fixture.root.join("snapshot-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::write(
            cache_dir.join(format!("{}.zip", fixture.args.snapshot_sha256)),
            "corrupt\n",
        )
        .unwrap();
        let marker = fixture.root.join("runner-started");
        let runner = fixture.root.join("must-not-start");
        std::fs::write(&runner, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut args = fixture.args;
        args.snapshot_cache_dir = Some(cache_dir);

        let error = execute_with_runner(args, &runner).expect_err("corrupt cache must fail");

        assert!(error
            .to_string()
            .contains("prediction snapshot archive SHA256 mismatch"));
        assert!(!marker.exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[test]
    fn safe_extraction_rejects_path_traversal() {
        let root = temporary_root("zip-traversal");
        let archive_path = root.join("snapshot.zip");
        let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .start_file(
                "../escaped.json",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        archive.write_all(b"{}\n").unwrap();
        archive.finish().unwrap();

        let error = extract_archive(&archive_path, &root.join("snapshot"), None)
            .expect_err("traversal must fail");

        assert!(error.to_string().contains("escapes destination"));
        assert!(!root.join("escaped.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn safe_extraction_rejects_symbolic_links() {
        let root = temporary_root("zip-symlink");
        let archive_path = root.join("snapshot.zip");
        let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .add_symlink("manifest.json", "../outside", SimpleFileOptions::default())
            .unwrap();
        archive.finish().unwrap();

        let error = extract_archive(&archive_path, &root.join("snapshot"), None)
            .expect_err("symbolic link must fail");

        assert!(error.to_string().contains("symbolic links"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_publishes_one_bundle_through_a_precompiled_runner() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("precompiled-runner");
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            successful_research_runner(
                &fixture,
                "mkdir -p \"$4\"\nprintf '{\"status\":\"completed\"}\\n' > \"$4/summary.json\"",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        execute_with_runner(fixture.args, &runner).unwrap();

        assert!(fixture.result_path.is_file());
        let archive = ZipArchive::new(File::open(&fixture.result_path).unwrap()).unwrap();
        let names = archive.file_names().collect::<Vec<_>>();
        assert!(names.contains(&"results/summary.json"));
        assert!(names.contains(&"artifacts/execution-evidence.json"));
        let evidence: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.root.join("work/artifacts/execution-evidence.json")).unwrap(),
        )
        .unwrap();
        let published_digest = sha256_file(&fixture.result_path).unwrap();
        assert_eq!(evidence["bundle_sha256"], published_digest);
        assert_eq!(
            evidence["readback_bundle_sha256"],
            evidence["bundle_sha256"]
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_routes_a_v4_pipeline_smoke_mission_without_the_v2_runner_shape() {
        use std::os::unix::fs::PermissionsExt;

        let mut fixture = execute_fixture("pipeline-smoke");
        let mission = serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "btc-5m-smoke",
            "product": {"symbol": "BTC", "event_horizon_secs": 300},
            "task": {"kind": "settlement_probability"},
            "run_mode": "pipeline_smoke",
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": fixture.args.cohort_manifest_id.clone(),
            "partition_digest": fixture.args.partition_digest.clone(),
            "causal_projection_policy_id": fixture.args.policy_identity.clone(),
            "snapshot_contract_id": fixture.args.snapshot_contract_id.clone(),
            "snapshot_hash": fixture.args.snapshot_digest.clone(),
            "search_policy_snapshot_id": fixture.args.policy_identity.clone(),
            "search_budget": {"max_candidates": 0, "max_llm_calls": 0, "max_seconds": 1}
        });
        let mission_bytes = serde_json::to_vec(&mission).unwrap();
        let mission_path = fixture.root.join("pipeline-smoke-mission.json");
        std::fs::write(&mission_path, &mission_bytes).unwrap();
        fixture.args.mission_url = mission_path.to_string_lossy().into_owned();
        fixture.args.mission_sha256 = format!("{:x}", Sha256::digest(&mission_bytes));
        let evaluator_report = serde_json::json!({
            "schema_version": "monday.prediction.pipeline_smoke.v1",
            "status": "completed",
            "scope": "pipeline_compatibility_only",
        });
        let evaluator_report_bytes = serde_json::to_vec(&evaluator_report).unwrap();
        let evaluator_report_sha256 = format!("{:x}", Sha256::digest(&evaluator_report_bytes));
        let evaluator_report_path = fixture.root.join("pipeline-smoke-report.json");
        std::fs::write(&evaluator_report_path, evaluator_report_bytes).unwrap();
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            format!(
                "#!/bin/sh\ntest \"$1\" = '--pipeline-smoke' || exit 2\ntest \"$5\" = '--admitted-cohort-manifest-id' || exit 2\ntest \"$6\" = '{}' || exit 2\ntest \"$7\" = '--admitted-partition-digest' || exit 2\ntest \"$8\" = '{}' || exit 2\ntest \"$9\" = '--admitted-policy-identity' || exit 2\ntest \"${{10}}\" = '{}' || exit 2\ntest \"${{11}}\" = '--admitted-snapshot-contract-id' || exit 2\ntest \"${{12}}\" = '{}' || exit 2\ntest \"${{13}}\" = '--admitted-snapshot-digest' || exit 2\ntest \"${{14}}\" = '{}' || exit 2\ntest \"${{15}}\" = '--admitted-partition-view-json' || exit 2\ntest \"${{16}}\" = '{}' || exit 2\ntest \"${{17}}\" = '--immutable-image-identity' || exit 2\ntest \"${{18}}\" = '{}' || exit 2\nmkdir -p \"$4/reports\"\ncp '{}' \"$4/reports/pipeline-smoke-{}.json\"\nprintf '%s\\n' '{}'\n",
                fixture.args.cohort_manifest_id.as_str(),
                fixture.args.partition_digest.as_str(),
                fixture.args.policy_identity.as_str(),
                fixture.args.snapshot_contract_id.as_str(),
                fixture.args.snapshot_digest.as_str(),
                fixture.args.partition_view_json.as_str(),
                fixture.args.image_identity.as_str(),
                evaluator_report_path.display(),
                evaluator_report_sha256,
                serde_json::json!({
                    "schema_version": "monday.prediction.pipeline_smoke.result.v1",
                    "status": "completed",
                    "mission_id": "btc-5m-smoke",
                    "task": "settlement_probability",
                    "snapshot_contract_id": fixture.args.snapshot_contract_id,
                    "snapshot_digest": fixture.args.snapshot_digest,
                    "search_policy_snapshot_id": fixture.args.policy_identity,
                    "evaluator_report_sha256": evaluator_report_sha256,
                })
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        execute_with_runner(fixture.args, &runner).unwrap();

        assert!(fixture.result_path.is_file());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_forged_pipeline_partition_before_starting_the_runner() {
        let mut fixture = execute_fixture("pipeline-smoke-forged-partition");
        let mission = serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "btc-5m-smoke",
            "product": {"symbol": "BTC", "event_horizon_secs": 300},
            "task": {"kind": "settlement_probability"},
            "run_mode": "pipeline_smoke",
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": fixture.args.cohort_manifest_id.clone(),
            "partition_digest": format!("sha256:{}", "f".repeat(64)),
            "causal_projection_policy_id": fixture.args.policy_identity.clone(),
            "snapshot_contract_id": fixture.args.snapshot_contract_id.clone(),
            "snapshot_hash": fixture.args.snapshot_digest.clone(),
            "search_policy_snapshot_id": fixture.args.policy_identity.clone(),
            "search_budget": {"max_candidates": 0, "max_llm_calls": 0, "max_seconds": 1}
        });
        let mission_bytes = serde_json::to_vec(&mission).unwrap();
        let mission_path = fixture.root.join("pipeline-smoke-forged-mission.json");
        std::fs::write(&mission_path, &mission_bytes).unwrap();
        fixture.args.mission_url = mission_path.to_string_lossy().into_owned();
        fixture.args.mission_sha256 = format!("{:x}", Sha256::digest(&mission_bytes));

        let runner = fixture.root.join("must-not-run");
        let error = execute_with_runner(fixture.args, &runner)
            .expect_err("a forged partition digest must fail closed");
        assert!(error.to_string().contains(
            "prediction mission, admitted snapshot contract, and snapshot manifest do not match"
        ));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_pipeline_smoke_completion_with_wrong_report_digest() {
        use std::os::unix::fs::PermissionsExt;

        let mut fixture = execute_fixture("pipeline-smoke-report-digest");
        let mission = serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "btc-5m-smoke",
            "product": {"symbol": "BTC", "event_horizon_secs": 300},
            "task": {"kind": "settlement_probability"},
            "run_mode": "pipeline_smoke",
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": fixture.args.cohort_manifest_id.clone(),
            "partition_digest": fixture.args.partition_digest.clone(),
            "causal_projection_policy_id": fixture.args.policy_identity.clone(),
            "snapshot_contract_id": fixture.args.snapshot_contract_id.clone(),
            "snapshot_hash": fixture.args.snapshot_digest.clone(),
            "search_policy_snapshot_id": fixture.args.policy_identity.clone(),
            "search_budget": {"max_candidates": 0, "max_llm_calls": 0, "max_seconds": 1}
        });
        let mission_bytes = serde_json::to_vec(&mission).unwrap();
        let mission_path = fixture.root.join("pipeline-smoke-mission.json");
        std::fs::write(&mission_path, &mission_bytes).unwrap();
        fixture.args.mission_url = mission_path.to_string_lossy().into_owned();
        fixture.args.mission_sha256 = format!("{:x}", Sha256::digest(&mission_bytes));
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            format!(
                "#!/bin/sh\nmkdir -p \"$4/reports\"\nprintf '{{}}' > \"$4/reports/pipeline-smoke-report.json\"\nprintf '%s\\n' '{}'\n",
                serde_json::json!({
                    "schema_version": "monday.prediction.pipeline_smoke.result.v1",
                    "status": "completed",
                    "mission_id": "btc-5m-smoke",
                    "task": "settlement_probability",
                    "snapshot_contract_id": fixture.args.snapshot_contract_id,
                    "snapshot_digest": fixture.args.snapshot_digest,
                    "search_policy_snapshot_id": fixture.args.policy_identity,
                    "evaluator_report_sha256": "4".repeat(64),
                })
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = execute_with_runner(fixture.args, &runner)
            .expect_err("a mismatched evaluator report digest must fail closed");

        assert!(error
            .to_string()
            .contains("evaluator report SHA256 does not match"));
        assert!(
            !fixture.result_path.exists(),
            "a rejected smoke report must not occupy the immutable result object"
        );
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_a_published_result_readback_with_different_bytes() {
        use std::os::unix::fs::PermissionsExt;

        let mut fixture = execute_fixture("result-readback-mismatch");
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            successful_research_runner(
                &fixture,
                "mkdir -p \"$4\"\nprintf '{\"status\":\"completed\"}\\n' > \"$4/summary.json\"",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let readback = fixture.root.join("tampered-readback.zip");
        std::fs::write(&readback, "different immutable object").unwrap();
        fixture.args.result_readback_url = readback.to_string_lossy().into_owned();

        let error = execute_with_runner(fixture.args, &runner)
            .expect_err("a different published readback must fail closed");

        assert!(error
            .to_string()
            .contains("published prediction result readback SHA256 mismatch"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_runner_still_publishes_immutable_resume_evidence() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("failed-runner-evidence");
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            "#!/bin/sh\nmkdir -p \"$4\"\nprintf '{\"status\":\"paused\"}\\n' > \"$4/checkpoint.json\"\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = execute_with_runner(fixture.args, &runner)
            .expect_err("paused runner must keep a failing transport status");

        assert!(error
            .to_string()
            .contains("immutable evidence was published"));
        assert!(fixture.result_path.is_file());
        let archive = ZipArchive::new(File::open(&fixture.result_path).unwrap()).unwrap();
        assert!(archive
            .file_names()
            .any(|name| name == "results/checkpoint.json"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn same_work_dir_retry_uses_a_fresh_verified_snapshot_extraction() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = execute_fixture("same-work-dir-retry");
        let paused_runner = fixture.root.join("paused-runner");
        std::fs::write(&paused_runner, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&paused_runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        execute_with_runner(fixture.args.clone(), &paused_runner)
            .expect_err("first attempt must publish its pause checkpoint");
        std::fs::create_dir_all(fixture.args.work_dir.join("input/snapshot")).unwrap();
        std::fs::write(
            fixture.args.work_dir.join("input/snapshot/manifest.json"),
            "tampered\n",
        )
        .unwrap();
        let resumed_runner = fixture.root.join("resumed-runner");
        std::fs::write(
            &resumed_runner,
            successful_research_runner(
                &fixture,
                "test -f \"$3/manifest.json\" || exit 4\nprintf '{\"status\":\"completed\"}\\n' > \"$4/summary.json\"",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&resumed_runner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut retry_args = fixture.args.clone();
        let resumed_result = fixture.root.join("resumed-results.zip");
        retry_args.result_put_url = resumed_result.to_string_lossy().into_owned();
        retry_args.result_readback_url = resumed_result.to_string_lossy().into_owned();

        execute_with_runner(retry_args, &resumed_runner)
            .expect("retry with no local results must ignore stale extraction");

        assert!(fixture.root.join("resumed-results.zip").is_file());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_a_symlinked_artifact_leaf_before_starting_runner() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let fixture = execute_fixture("symlinked-artifact-leaf");
        let artifact_dir = fixture.args.work_dir.join("artifacts");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let protected_target = fixture.root.join("protected-runner-stdout");
        std::fs::write(&protected_target, "preserve\n").unwrap();
        symlink(&protected_target, artifact_dir.join("runner.stdout")).unwrap();
        let runner = fixture.root.join("must-not-start");
        std::fs::write(
            &runner,
            "#!/bin/sh\nprintf started > \"$4/runner-started\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        let error = execute_with_runner(fixture.args.clone(), &runner)
            .expect_err("a symlinked artifact path must be rejected before the runner starts");

        assert!(error.to_string().contains("symbolic link"));
        assert_eq!(
            std::fs::read_to_string(&protected_target).unwrap(),
            "preserve\n"
        );
        assert!(!fixture
            .args
            .work_dir
            .join("results/runner-started")
            .exists());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_untrusted_local_results_state() {
        let fixture = execute_fixture("untrusted-local-results");
        let results_dir = fixture.args.work_dir.join("results");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(results_dir.join("checkpoint.json"), "tampered\n").unwrap();

        let error = execute_with_runner(fixture.args, Path::new("/runner-must-not-start"))
            .expect_err("local writable results must not become resume input");

        assert!(error
            .to_string()
            .contains("prediction results directory is not empty"));
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn execute_rejects_a_work_directory_with_a_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let fixture = execute_fixture("symlinked-artifacts");
        let protected_directory = fixture.root.join("protected-artifacts");
        let protected_work_directory = protected_directory.join("work");
        std::fs::create_dir_all(&protected_work_directory).unwrap();
        let linked_parent = fixture.root.join("linked-parent");
        symlink(&protected_directory, &linked_parent).unwrap();
        let mut args = fixture.args;
        args.work_dir = linked_parent.join("work");

        let error = execute_with_runner(args, Path::new("/runner-must-not-start"))
            .expect_err("a symlinked work directory ancestor must be rejected");

        assert!(error.to_string().contains("symbolic link"));
        assert!(std::fs::read_dir(protected_work_directory)
            .unwrap()
            .next()
            .is_none());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn next_attempt_restores_results_from_a_pinned_resume_bundle() {
        use std::os::unix::fs::PermissionsExt;

        let mut fixture = execute_fixture("resume-results");
        let resume_path = fixture.root.join("resume.zip");
        let mut archive = ZipWriter::new(File::create(&resume_path).unwrap());
        archive
            .start_file("results/checkpoint.json", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"{\"status\":\"paused\"}\n").unwrap();
        archive.finish().unwrap();
        fixture.args.resume_url = Some(resume_path.to_string_lossy().into_owned());
        fixture.args.resume_sha256 = Some(sha256_file(&resume_path).unwrap());
        let runner = fixture.root.join("monday-prediction-research");
        std::fs::write(
            &runner,
            successful_research_runner(
                &fixture,
                "test -f \"$4/checkpoint.json\" || exit 2\nprintf '{\"status\":\"completed\"}\\n' > \"$4/summary.json\"",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&runner, std::fs::Permissions::from_mode(0o700)).unwrap();

        execute_with_runner(fixture.args, &runner).unwrap();

        assert!(fixture.result_path.is_file());
        std::fs::remove_dir_all(fixture.root).unwrap();
    }

    struct ExecuteFixture {
        root: PathBuf,
        result_path: PathBuf,
        receipt_source: PathBuf,
        research_completion: String,
        args: PredictionExecuteArgs,
    }

    fn execute_fixture(name: &str) -> ExecuteFixture {
        let root = temporary_root(name);
        let mission_path = root.join("mission.json");
        let snapshot_contract_id = format!("sha256:{}", "1".repeat(64));
        let snapshot_digest = "0123456789abcdef";
        let cohort_manifest_id = format!("sha256:{}", "5".repeat(64));
        let partition_digest = format!("sha256:{}", "2".repeat(64));
        let policy_identity = format!("sha256:{}", "3".repeat(64));
        let mission = serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "prediction-test",
            "product": {"symbol": "BTC", "event_horizon_secs": 300},
            "task": {"kind": "settlement_probability"},
            "run_mode": "research_trial",
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": cohort_manifest_id,
            "partition_digest": partition_digest,
            "causal_projection_policy_id": policy_identity,
            "snapshot_contract_id": snapshot_contract_id,
            "snapshot_hash": snapshot_digest,
            "search_policy_snapshot_id": policy_identity,
            "search_budget": {"max_candidates": 1, "max_llm_calls": 0, "max_seconds": 1}
        });
        let mission_bytes = serde_json::to_vec(&mission).unwrap();
        std::fs::write(&mission_path, &mission_bytes).unwrap();
        let snapshot_path = root.join("input-snapshot.zip");
        let mut archive = ZipWriter::new(File::create(&snapshot_path).unwrap());
        archive
            .start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        archive
            .write_all(
                serde_json::to_string(&serde_json::json!({
                    "schema_version": "research_snapshot_v2",
                    "snapshot_hash": snapshot_digest,
                    "snapshot_contract_hash": snapshot_contract_id,
                    "source_kind": "polymarket_chainlink_baseline",
                }))
                .unwrap()
                .as_bytes(),
            )
            .unwrap();
        archive.finish().unwrap();
        let mut receipt = serde_json::json!({
            "schema_version": "prediction_authenticated_result_receipt.v1",
            "mission": mission,
            "immutable_image_identity": format!("sha256:{}", "4".repeat(64)),
            "sha256": "",
        });
        let receipt_sha256 = research_trial_receipt_sha256(&receipt).unwrap();
        receipt["sha256"] = serde_json::Value::String(receipt_sha256.clone());
        let receipt_bytes = serde_json::to_vec(&receipt).unwrap();
        let receipt_source = root.join("fixture-receipt.json");
        std::fs::write(&receipt_source, &receipt_bytes).unwrap();
        let receipt_artifact_sha256 = format!("sha256:{:x}", Sha256::digest(&receipt_bytes));
        let research_completion = serde_json::json!({
            "schema_version": "monday.prediction.research_trial.result.v1",
            "status": "completed",
            "mission_id": "prediction-test",
            "task": {"kind": "settlement_probability"},
            "snapshot_contract_id": snapshot_contract_id,
            "snapshot_digest": snapshot_digest,
            "search_policy_snapshot_id": policy_identity,
            "receipt_path": "mcts-v4/receipts/receipt.json",
            "receipt_artifact_sha256": receipt_artifact_sha256,
            "receipt_sha256": receipt_sha256,
        })
        .to_string();
        let result_path = root.join("published.zip");
        let args = PredictionExecuteArgs {
            work_dir: root.join("work"),
            mission_url: mission_path.to_string_lossy().into_owned(),
            mission_sha256: format!("{:x}", Sha256::digest(&mission_bytes)),
            snapshot_url: snapshot_path.to_string_lossy().into_owned(),
            snapshot_sha256: sha256_file(&snapshot_path).unwrap(),
            snapshot_contract_id,
            snapshot_digest: snapshot_digest.to_owned(),
            cohort_manifest_id,
            partition_digest,
            policy_identity,
            task_capability: "btc_5m_backtest".to_owned(),
            image_identity: format!("sha256:{}", "4".repeat(64)),
            partition_view_json: serde_json::json!({
                "common_time_boundary_ms": 1,
                "train_market_ids": ["train"],
                "crossing_excluded_market_ids": [],
                "held_out_market_ids": ["held-out"]
            })
            .to_string(),
            snapshot_cache_dir: None,
            resume_url: None,
            resume_sha256: None,
            result_put_url: result_path.to_string_lossy().into_owned(),
            result_readback_url: result_path.to_string_lossy().into_owned(),
        };
        ExecuteFixture {
            root,
            result_path,
            receipt_source,
            research_completion,
            args,
        }
    }

    fn successful_research_runner(fixture: &ExecuteFixture, setup: &str) -> String {
        format!(
            "#!/bin/sh\ntest \"$1\" = '--research-trial' || exit 2\n{setup}\nmkdir -p \"$4/mcts-v4/receipts\"\ncp '{}' \"$4/mcts-v4/receipts/receipt.json\"\nprintf '%s\\n' '{}'\n",
            fixture.receipt_source.display(),
            fixture.research_completion,
        )
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "alpha-prediction-runner-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
