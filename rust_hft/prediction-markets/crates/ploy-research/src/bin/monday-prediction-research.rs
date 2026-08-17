//! Governed Prediction Market research entrypoint.
//!
//! This binary accepts only Mission v4. It can prove pipeline compatibility or
//! run the existing authenticated, event-disjoint ResearchTrial; it has no live
//! order path and no external proposal-provider dependency.

use std::fs;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ploy_research::prediction_loop::current_prediction_policy_snapshot_id;
use ploy_research::prediction_mcts_authenticated::{
    run_or_resume_authenticated_prediction_mcts_trial, BuiltInAuthenticatedPredictionMctsEvaluator,
};
use ploy_research::prediction_mission_v3::{
    admit_prediction_mission_v3, authenticate_prediction_mission_v3_inputs,
    parse_prediction_mission_json, validate_prediction_mission_v3, PredictionMissionTask,
    PredictionResearchMissionV3, PredictionRunMode, PredictionTaskKind,
};
use ploy_research::{
    admit_extracted_authenticated_research_snapshot, AuthenticatedPartitionView,
    AuthenticatedResearchSnapshot,
};
use sha2::{Digest, Sha256};

const MAX_PARTITION_VIEW_BYTES: usize = 16 * 1024;

fn usage() -> ! {
    eprintln!(
        "usage:\n  monday-prediction-research --print-policy-snapshot-id\n  monday-prediction-research --pipeline-smoke <mission.json> <snapshot-dir> <output-dir> <admitted identity flags>\n  monday-prediction-research --research-trial <mission.json> <snapshot-dir> <output-dir> <admitted identity flags>"
    );
    std::process::exit(2);
}

#[derive(serde::Serialize)]
struct PipelineSmokeSummary {
    schema_version: &'static str,
    status: &'static str,
    mission_id: String,
    task: String,
    snapshot_contract_id: String,
    snapshot_digest: String,
    search_policy_snapshot_id: String,
    evaluator_report_sha256: String,
}

#[derive(serde::Serialize)]
struct ResearchTrialSummary {
    schema_version: &'static str,
    status: &'static str,
    mission_id: String,
    task: PredictionMissionTask,
    snapshot_contract_id: String,
    snapshot_digest: String,
    search_policy_snapshot_id: String,
    receipt_path: String,
    receipt_artifact_sha256: String,
    receipt_sha256: String,
}

struct AuthenticatedRunAdmission {
    cohort_manifest_id: String,
    partition_digest: String,
    policy_identity: String,
    snapshot_contract_id: String,
    snapshot_hash: String,
    partition_view_json: String,
    immutable_image_identity: String,
}

fn parse_authenticated_run_admission(args: &[String]) -> Result<AuthenticatedRunAdmission, String> {
    let [cohort_flag, cohort_manifest_id, partition_flag, partition_digest, policy_flag, policy_identity, contract_flag, snapshot_contract_id, hash_flag, snapshot_hash, view_flag, partition_view_json, image_flag, immutable_image_identity] =
        args
    else {
        return Err("prediction research requires the complete admitted snapshot identity".into());
    };
    if cohort_flag != "--admitted-cohort-manifest-id"
        || partition_flag != "--admitted-partition-digest"
        || policy_flag != "--admitted-policy-identity"
        || contract_flag != "--admitted-snapshot-contract-id"
        || hash_flag != "--admitted-snapshot-digest"
        || view_flag != "--admitted-partition-view-json"
        || image_flag != "--immutable-image-identity"
        || partition_view_json.len() > MAX_PARTITION_VIEW_BYTES
    {
        return Err("prediction research admitted identity arguments are invalid".into());
    }
    Ok(AuthenticatedRunAdmission {
        cohort_manifest_id: cohort_manifest_id.clone(),
        partition_digest: partition_digest.clone(),
        policy_identity: policy_identity.clone(),
        snapshot_contract_id: snapshot_contract_id.clone(),
        snapshot_hash: snapshot_hash.clone(),
        partition_view_json: partition_view_json.clone(),
        immutable_image_identity: immutable_image_identity.clone(),
    })
}

fn read_v4_mission(path: &Path) -> Result<PredictionResearchMissionV3, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("read Mission v4 {}: {error}", path.display()))?;
    let mission = parse_prediction_mission_json(&bytes)?;
    validate_prediction_mission_v3(&mission)?;
    Ok(mission)
}

fn validate_admitted_mission_identity(
    mission: &PredictionResearchMissionV3,
    admitted: &AuthenticatedRunAdmission,
) -> Result<(), String> {
    if mission.cohort_manifest_id != admitted.cohort_manifest_id
        || mission.partition_digest != admitted.partition_digest
        || mission.causal_projection_policy_id != admitted.policy_identity
        || mission.snapshot_contract_id != admitted.snapshot_contract_id
        || mission.snapshot_hash != admitted.snapshot_hash
        || mission.search_policy_snapshot_id != admitted.policy_identity
    {
        return Err("Mission v4 does not match its admitted sealed identity".into());
    }
    Ok(())
}

fn admit_snapshot(
    snapshot_dir: &Path,
    admitted: &AuthenticatedRunAdmission,
) -> Result<AuthenticatedResearchSnapshot, String> {
    let partition_view: AuthenticatedPartitionView =
        serde_json::from_str(&admitted.partition_view_json)
            .map_err(|error| format!("parse admitted partition view: {error}"))?;
    admit_extracted_authenticated_research_snapshot(
        snapshot_dir,
        &admitted.cohort_manifest_id,
        &admitted.partition_digest,
        &admitted.policy_identity,
        partition_view,
        &admitted.snapshot_contract_id,
        &admitted.snapshot_hash,
    )
    .map_err(|rejection| format!("admit extracted authenticated snapshot: {rejection:?}"))
}

fn pipeline_smoke_task(mission: &PredictionResearchMissionV3) -> Result<String, String> {
    match (mission.task.kind, mission.task.prediction_horizon_secs) {
        (PredictionTaskKind::SettlementProbability, None) => Ok("settlement_probability".into()),
        _ => Err("pipeline smoke Mission v4 task is not admitted".into()),
    }
}

struct PipelineSmokeEvaluator {
    executable: PathBuf,
}

impl PipelineSmokeEvaluator {
    fn new() -> Result<Self, String> {
        let executable = std::env::var_os("MONDAY_PREDICTION_EVALUATOR_BIN")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(Ok)
            .unwrap_or_else(|| {
                let current = std::env::current_exe()
                    .map_err(|error| format!("resolve prediction research executable: {error}"))?;
                let parent = current.parent().ok_or_else(|| {
                    "prediction research executable has no parent directory".to_string()
                })?;
                Ok::<_, String>(parent.join("monday-prediction-evaluator"))
            })?;
        if !executable.is_file() {
            return Err(format!(
                "configured prediction evaluator does not exist: {}",
                executable.display()
            ));
        }
        Ok(Self { executable })
    }

    fn command(
        &self,
        mission: &PredictionResearchMissionV3,
        task: &str,
        snapshot_dir: &Path,
        snapshot_hash: &str,
        report_output_dir: &Path,
    ) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .arg("--pipeline-smoke-task")
            .arg(task)
            .arg("--snapshot-dir")
            .arg(snapshot_dir)
            .arg("--mission-id")
            .arg(&mission.mission_id)
            .arg("--snapshot-hash")
            .arg(snapshot_hash)
            .arg("--snapshot-contract-id")
            .arg(&mission.snapshot_contract_id)
            .arg("--expected-search-policy-snapshot-id")
            .arg(&mission.search_policy_snapshot_id)
            .arg("--report-output-dir")
            .arg(report_output_dir);
        command
    }
}

fn run_pipeline_smoke_evaluator(mut command: Command, timeout: Duration) -> Result<(), String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_evaluator_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn pipeline smoke evaluator: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!("pipeline smoke evaluator exited with {status}"))
            }
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = terminate_evaluator_group(&mut child);
                return Err(format!(
                    "pipeline smoke evaluator exceeded {} ms",
                    timeout.as_millis()
                ));
            }
            Err(error) => {
                let _ = terminate_evaluator_group(&mut child);
                return Err(format!("poll pipeline smoke evaluator: {error}"));
            }
        }
    }
}

fn read_pipeline_smoke_report(
    report_dir: &Path,
    mission: &PredictionResearchMissionV3,
    task: &str,
    snapshot_hash: &str,
) -> Result<String, String> {
    let matches = fs::read_dir(report_dir)
        .map_err(|error| format!("read pipeline smoke report directory: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pipeline-smoke-") && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    let [path] = matches.as_slice() else {
        return Err(format!(
            "expected exactly one pipeline smoke report, found {}",
            matches.len()
        ));
    };
    let bytes = fs::read(path)
        .map_err(|error| format!("read pipeline smoke report {}: {error}", path.display()))?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if !path
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(&digest))
    {
        return Err("pipeline smoke report is not content-addressed".into());
    }
    let report: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse pipeline smoke report: {error}"))?;
    if report["schema_version"] != "monday.prediction.pipeline_smoke.v1"
        || report["status"] != "completed"
        || report["scope"] != "pipeline_compatibility_only"
        || report["mission_id"] != mission.mission_id
        || report["task"] != task
        || report["snapshot_hash"] != snapshot_hash
        || report["snapshot_contract_id"] != mission.snapshot_contract_id
        || report["search_policy_snapshot_id"] != mission.search_policy_snapshot_id
        || report["claims_excluded"]
            != serde_json::json!([
                "alpha",
                "profitability",
                "paper",
                "shadow",
                "live",
                "promotion"
            ])
    {
        return Err(
            "pipeline smoke report does not bind its mechanical compatibility identity".into(),
        );
    }
    Ok(digest)
}

fn run_pipeline_smoke(
    mission_path: &Path,
    snapshot_dir: &Path,
    output_dir: &Path,
    admitted: &AuthenticatedRunAdmission,
) -> Result<PipelineSmokeSummary, String> {
    let mission = read_v4_mission(mission_path)?;
    if mission.run_mode != PredictionRunMode::PipelineSmoke {
        return Err("pipeline smoke requires run_mode pipeline_smoke".into());
    }
    if mission.search_policy_snapshot_id != current_prediction_policy_snapshot_id() {
        return Err("pipeline smoke Mission v4 has a stale evaluator policy identity".into());
    }
    validate_admitted_mission_identity(&mission, admitted)?;
    let task = pipeline_smoke_task(&mission)?;
    let snapshot = admit_snapshot(snapshot_dir, admitted)?;
    let snapshot_hash = snapshot.snapshot_hash();
    fs::create_dir_all(output_dir)
        .map_err(|error| format!("create pipeline smoke output directory: {error}"))?;
    let report_dir = output_dir.join("reports");
    let evaluator = PipelineSmokeEvaluator::new()?;
    run_pipeline_smoke_evaluator(
        evaluator.command(&mission, &task, snapshot_dir, snapshot_hash, &report_dir),
        Duration::from_secs(mission.search_budget.max_seconds.max(1)),
    )?;
    let evaluator_report_sha256 =
        read_pipeline_smoke_report(&report_dir, &mission, &task, snapshot_hash)?;
    Ok(PipelineSmokeSummary {
        schema_version: "monday.prediction.pipeline_smoke.result.v1",
        status: "completed",
        mission_id: mission.mission_id,
        task,
        snapshot_contract_id: mission.snapshot_contract_id,
        snapshot_digest: snapshot_hash.to_owned(),
        search_policy_snapshot_id: mission.search_policy_snapshot_id,
        evaluator_report_sha256,
    })
}

fn run_research_trial(
    mission_path: &Path,
    snapshot_dir: &Path,
    output_dir: &Path,
    admitted: &AuthenticatedRunAdmission,
) -> Result<ResearchTrialSummary, String> {
    let mission = read_v4_mission(mission_path)?;
    if mission.run_mode != PredictionRunMode::ResearchTrial {
        return Err("research trial requires run_mode research_trial".into());
    }
    validate_admitted_mission_identity(&mission, admitted)?;
    let snapshot = admit_snapshot(snapshot_dir, admitted)?;
    let inputs = authenticate_prediction_mission_v3_inputs(&snapshot, &mission)?;
    let mission_admission = admit_prediction_mission_v3(&mission, &inputs, None)?;
    let run = run_or_resume_authenticated_prediction_mcts_trial(
        &mission,
        &mission_admission,
        &snapshot,
        &admitted.immutable_image_identity,
        output_dir,
        &mut BuiltInAuthenticatedPredictionMctsEvaluator,
    )?;
    Ok(ResearchTrialSummary {
        schema_version: "monday.prediction.research_trial.result.v1",
        status: "completed",
        mission_id: mission.mission_id,
        task: mission.task,
        snapshot_contract_id: mission.snapshot_contract_id,
        snapshot_digest: mission.snapshot_hash,
        search_policy_snapshot_id: mission.search_policy_snapshot_id,
        receipt_path: run.receipt.path().to_string(),
        receipt_artifact_sha256: run.receipt.artifact_sha256().to_string(),
        receipt_sha256: run.receipt.receipt_sha256().to_string(),
    })
}

#[cfg(unix)]
fn configure_evaluator_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_evaluator_process_group(_command: &mut Command) {}

fn terminate_evaluator_group(child: &mut Child) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        let process_group = -(child.id() as libc::pid_t);
        unsafe {
            libc::kill(process_group, libc::SIGTERM);
        }
        for _ in 0..20 {
            if let Ok(Some(status)) = child.try_wait() {
                unsafe {
                    libc::kill(process_group, libc::SIGKILL);
                }
                return Some(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
        unsafe {
            libc::kill(process_group, libc::SIGKILL);
        }
        child.wait().ok()
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        child.wait().ok()
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--print-policy-snapshot-id"] {
        println!("{}", current_prediction_policy_snapshot_id());
        return;
    }
    let [mode, mission_path, snapshot_dir, output_dir, admission @ ..] = args.as_slice() else {
        usage();
    };
    let admission = parse_authenticated_run_admission(admission).unwrap_or_else(|reason| {
        eprintln!("ERROR: {reason}");
        std::process::exit(2);
    });
    let result = match mode.as_str() {
        "--pipeline-smoke" => run_pipeline_smoke(
            Path::new(mission_path),
            Path::new(snapshot_dir),
            Path::new(output_dir),
            &admission,
        )
        .and_then(|summary| serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())),
        "--research-trial" => run_research_trial(
            Path::new(mission_path),
            Path::new(snapshot_dir),
            Path::new(output_dir),
            &admission,
        )
        .and_then(|summary| serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())),
        _ => usage(),
    };
    match result {
        Ok(summary) => println!("{summary}"),
        Err(reason) => {
            eprintln!("ERROR: {reason}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mission(run_mode: &str, task: serde_json::Value) -> PredictionResearchMissionV3 {
        serde_json::from_value(serde_json::json!({
            "schema_version": "prediction_research_mission.v4",
            "mission_id": "btc-5m-test",
            "product": {"symbol": "BTC", "event_horizon_secs": 300},
            "task": task,
            "run_mode": run_mode,
            "authority_profile": "polymarket_chainlink_baseline",
            "required_capabilities": ["polymarket_chainlink"],
            "cohort_manifest_id": format!("sha256:{}", "1".repeat(64)),
            "partition_digest": format!("sha256:{}", "2".repeat(64)),
            "causal_projection_policy_id": format!("sha256:{}", "3".repeat(64)),
            "snapshot_contract_id": format!("sha256:{}", "4".repeat(64)),
            "snapshot_hash": "5".repeat(16),
            "search_policy_snapshot_id": format!("sha256:{}", "3".repeat(64)),
            "search_budget": {"max_candidates": 0, "max_seconds": 1}
        }))
        .unwrap()
    }

    #[test]
    fn pipeline_smoke_rejects_execution_tasks() {
        assert_eq!(
            pipeline_smoke_task(&mission(
                "pipeline_smoke",
                serde_json::json!({"kind": "settlement_probability"})
            ))
            .unwrap(),
            "settlement_probability"
        );
        assert!(pipeline_smoke_task(&mission(
            "pipeline_smoke",
            serde_json::json!({"kind": "up_execution", "side": "up", "prediction_horizon_secs": 10})
        ))
        .is_err());
    }

    #[test]
    fn authenticated_modes_require_the_complete_identity() {
        let args = [
            "--admitted-cohort-manifest-id",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--admitted-partition-digest",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "--admitted-policy-identity",
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "--admitted-snapshot-contract-id",
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "--admitted-snapshot-digest",
            "0123456789abcdef",
            "--admitted-partition-view-json",
            r#"{"common_time_boundary_ms":1,"train_market_ids":["train"],"crossing_excluded_market_ids":[],"held_out_market_ids":["held"]}"#,
            "--immutable-image-identity",
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        assert_eq!(
            parse_authenticated_run_admission(&args)
                .expect("complete identity")
                .partition_digest,
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert!(parse_authenticated_run_admission(&args[..12]).is_err());
    }
}
