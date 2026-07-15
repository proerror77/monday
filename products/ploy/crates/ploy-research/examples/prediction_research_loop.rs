//! Governed Rust LoopRun for one BTC or SOL five-minute prediction mission.
//!
//! Binance is predictor context, Chainlink is settlement truth, and the
//! Polymarket CLOB is the execution/capacity surface. This binary has no live
//! order path.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ploy_research::prediction_llm::{
    OpenAiCompatibleProposalClient, PLOY_RESEARCH_LLM_API_KEY_ENV, PLOY_RESEARCH_LLM_BASE_URL_ENV,
    PLOY_RESEARCH_LLM_MODEL_ENV, PLOY_RESEARCH_LLM_PROVIDER_ENV,
};
use ploy_research::prediction_loop::{
    current_prediction_policy_snapshot_id, research_brief_snapshot_id, run_or_resume,
    LoopRunStatus, PredictionEvaluationOutput, PredictionEvaluationRequest, PredictionEvaluator,
    PredictionResearchMission,
};
use ploy_research::{
    load_research_snapshot, normalized_underlying_symbol, PredictionResearchFeedback,
};
use rustix::fd::OwnedFd;
use rustix::fs::{Dir, Mode, OFlags};
use sha2::{Digest, Sha256};

const MAX_EVALUATOR_LOG_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVALUATOR_FEEDBACK_BYTES: usize = 16 * 1024 * 1024;

fn usage() -> ! {
    eprintln!(
        "usage:\n  prediction_research_loop --print-policy-snapshot-id\n  prediction_research_loop --print-brief-snapshot-id <mission.json>\n  prediction_research_loop <mission.json> <snapshot-dir> <output-dir>"
    );
    std::process::exit(2);
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

struct RustProcessEvaluator {
    workspace_dir: PathBuf,
}

impl RustProcessEvaluator {
    fn new() -> Self {
        Self {
            workspace_dir: Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .expect("ploy-research is nested under the PLOY workspace")
                .to_path_buf(),
        }
    }

    fn command(&self, request: &PredictionEvaluationRequest) -> Result<Command, String> {
        let snapshot = load_research_snapshot(&request.snapshot_dir)
            .map_err(|error| format!("load evaluator snapshot: {error:#}"))?;
        let underlying = request.mission.symbols[0].as_str();
        let symbol = snapshot
            .manifest
            .symbols
            .iter()
            .find(|symbol| normalized_underlying_symbol(symbol) == underlying)
            .ok_or_else(|| format!("snapshot has no {underlying} evaluator symbol"))?;

        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
        let mut command = Command::new(cargo);
        command
            // The deterministic Rust evaluator never calls an LLM. Keep proposal
            // transport configuration, especially the API key, out of its
            // process environment even when the parent LoopRun needs it.
            .env_remove(PLOY_RESEARCH_LLM_API_KEY_ENV)
            .env_remove(PLOY_RESEARCH_LLM_BASE_URL_ENV)
            .env_remove(PLOY_RESEARCH_LLM_MODEL_ENV)
            .env_remove(PLOY_RESEARCH_LLM_PROVIDER_ENV)
            .current_dir(&self.workspace_dir)
            .arg("run")
            .arg("--quiet")
            .arg("-p")
            .arg("ploy-research")
            .arg("--example")
            .arg("factor_walk_forward_v2")
            .arg("--features")
            .arg("db")
            .arg("--")
            .arg("--snapshot-dir")
            .arg(&request.snapshot_dir)
            .arg("--start-ts")
            .arg(snapshot.manifest.start.to_rfc3339())
            .arg("--end-ts")
            .arg(snapshot.manifest.end.to_rfc3339())
            .arg("--symbols")
            .arg(symbol)
            .arg("--event-window-secs")
            .arg("300")
            .arg("--lob-sample-secs")
            .arg(snapshot.manifest.lob_sample_secs.to_string())
            .arg("--pm-book-sample-secs")
            .arg(
                snapshot
                    .manifest
                    .pm_book_sample_secs
                    .unwrap_or(snapshot.manifest.lob_sample_secs)
                    .to_string(),
            )
            .arg("--observation-sample-secs")
            .arg(snapshot.manifest.observation_sample_secs.to_string())
            .arg("--max-quote-age-secs")
            .arg(snapshot.manifest.max_quote_age_secs.to_string())
            .arg("--stake-usd")
            .arg(snapshot.manifest.stake_usd.to_string())
            .arg("--report-suite")
            .arg("core");

        if let Some(prior) = request.prior.as_ref() {
            let prior_path = request.prior_artifact_path.as_ref().ok_or_else(|| {
                "candidate evaluation is missing its content-addressed prior path".to_string()
            })?;
            if prior.probability_blends.is_empty() {
                return Err("candidate evaluation prior has no probability blends".to_string());
            }
            command
                .arg("--alpha-search-llm-prior-json")
                .arg(prior_path)
                .arg("--alpha-search-output-dir")
                .arg(request.artifact_dir.join("alpha-search"));
        }
        Ok(command)
    }
}

impl PredictionEvaluator for RustProcessEvaluator {
    fn evaluate(
        &mut self,
        request: &PredictionEvaluationRequest,
        timeout: Duration,
    ) -> PredictionEvaluationOutput {
        let artifact_fd = match open_artifact_staging_dir(&request.artifact_dir) {
            Ok(fd) => fd,
            Err(reason) => return failed_output(reason),
        };
        let stdout_path = request.artifact_dir.join("evaluator.stdout.raw");
        let stderr_path = request.artifact_dir.join("evaluator.stderr.raw");
        let stdout_file = match create_output(&artifact_fd, "evaluator.stdout.raw", &stdout_path) {
            Ok(file) => file,
            Err(reason) => return failed_output(reason),
        };
        let stderr_file = match create_output(&artifact_fd, "evaluator.stderr.raw", &stderr_path) {
            Ok(file) => file,
            Err(reason) => return failed_output(reason),
        };
        let mut stdout_reader = match stdout_file.try_clone() {
            Ok(file) => file,
            Err(error) => return failed_output(format!("retain evaluator stdout FD: {error}")),
        };
        let mut stderr_reader = match stderr_file.try_clone() {
            Ok(file) => file,
            Err(error) => return failed_output(format!("retain evaluator stderr FD: {error}")),
        };
        let mut command = match self.command(request) {
            Ok(command) => command,
            Err(reason) => return failed_output(reason),
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::from(stderr_file));
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return failed_output(format!("spawn Rust evaluator: {error}")),
        };
        let started = Instant::now();
        let (status, timed_out) = loop {
            match child.try_wait() {
                Ok(Some(status)) => break (Some(status), false),
                Ok(None) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let status = child.wait().ok();
                    break (status, true);
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return failed_output(format!("poll Rust evaluator: {error}"));
                }
            }
        };
        let stdout = match read_bounded(&mut stdout_reader, &stdout_path) {
            Ok(stdout) => stdout,
            Err(reason) => return failed_output(reason),
        };
        let stderr = match read_bounded(&mut stderr_reader, &stderr_path) {
            Ok(stderr) => stderr,
            Err(reason) => {
                return PredictionEvaluationOutput {
                    success: false,
                    feedback: None,
                    stdout,
                    stderr: String::new(),
                    failure_reason: Some(reason),
                };
            }
        };
        if timed_out {
            return PredictionEvaluationOutput {
                success: false,
                feedback: None,
                stdout,
                stderr,
                failure_reason: Some(format!(
                    "Rust evaluator exceeded {} ms",
                    timeout.as_millis()
                )),
            };
        }
        if !status.is_some_and(|status| status.success()) {
            return PredictionEvaluationOutput {
                success: false,
                feedback: None,
                stdout,
                stderr,
                failure_reason: Some(format!("Rust evaluator exited with {status:?}")),
            };
        }

        let feedback = if request.prior.is_some() {
            match read_unique_feedback(&artifact_fd, &request.artifact_dir) {
                Ok(feedback) => Some(feedback),
                Err(reason) => {
                    return PredictionEvaluationOutput {
                        success: false,
                        feedback: None,
                        stdout,
                        stderr,
                        failure_reason: Some(reason),
                    };
                }
            }
        } else {
            None
        };
        PredictionEvaluationOutput {
            success: true,
            feedback,
            stdout,
            stderr,
            failure_reason: None,
        }
    }
}

fn open_artifact_staging_dir(path: &Path) -> Result<OwnedFd, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("inspect evaluator staging {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "evaluator artifact staging must be a pre-created real directory: {}",
            path.display()
        ));
    }
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("canonicalize evaluator staging {}: {error}", path.display()))?;
    if canonical != path {
        return Err(format!(
            "evaluator artifact staging must already be canonical: {}",
            path.display()
        ));
    }
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "open evaluator artifact staging without following symlinks {}: {error}",
            path.display()
        )
    })
}

fn create_output(parent: &OwnedFd, name: &str, path: &Path) -> Result<File, String> {
    let fd = rustix::fs::openat(
        parent,
        name,
        OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| format!("create evaluator output {}: {error}", path.display()))?;
    Ok(File::from(fd))
}

fn read_bounded(file: &mut File, path: &Path) -> Result<String, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("rewind evaluator log {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_EVALUATOR_LOG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read evaluator log {}: {error}", path.display()))?;
    if bytes.len() > MAX_EVALUATOR_LOG_BYTES {
        return Err(format!(
            "evaluator log {} exceeded {} bytes",
            path.display(),
            MAX_EVALUATOR_LOG_BYTES
        ));
    }
    String::from_utf8(bytes)
        .map_err(|error| format!("evaluator log {} is not UTF-8: {error}", path.display()))
}

fn open_staging_subdirectory(
    parent: &OwnedFd,
    name: &str,
    display_path: &Path,
) -> Result<OwnedFd, String> {
    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "open evaluator directory without following symlinks {}: {error}",
            display_path.display()
        )
    })
}

fn read_regular_file_at_bounded(
    parent: &OwnedFd,
    name: &str,
    path: &Path,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    let fd = rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        format!(
            "open evaluator feedback without following symlinks {}: {error}",
            path.display()
        )
    })?;
    let mut file = File::from(fd);
    if !file
        .metadata()
        .map_err(|error| format!("inspect evaluator feedback {}: {error}", path.display()))?
        .is_file()
    {
        return Err(format!(
            "evaluator feedback must be a regular file: {}",
            path.display()
        ));
    }
    let mut body = Vec::new();
    file.by_ref()
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut body)
        .map_err(|error| format!("read evaluator feedback {}: {error}", path.display()))?;
    if body.len() > max_bytes {
        return Err(format!(
            "evaluator feedback {} exceeded {} bytes",
            path.display(),
            max_bytes
        ));
    }
    Ok(body)
}

fn read_unique_feedback(
    artifact_dir: &OwnedFd,
    artifact_path: &Path,
) -> Result<PredictionResearchFeedback, String> {
    let alpha_search_path = artifact_path.join("alpha-search");
    let alpha_search = open_staging_subdirectory(artifact_dir, "alpha-search", &alpha_search_path)?;
    let root = alpha_search_path.join("full_depth_settlement_executable_pnl");
    let feedback_dir =
        open_staging_subdirectory(&alpha_search, "full_depth_settlement_executable_pnl", &root)?;
    let mut directory = Dir::read_from(&feedback_dir)
        .map_err(|error| format!("read feedback directory {}: {error}", root.display()))?;
    let mut matches = Vec::new();
    for entry in &mut directory {
        let entry = entry
            .map_err(|error| format!("read feedback entry under {}: {error}", root.display()))?;
        let name = entry.file_name().to_str().map_err(|_| {
            format!(
                "evaluator feedback directory contains a non-UTF-8 filename: {}",
                root.display()
            )
        })?;
        if name.starts_with("prediction-research-feedback-") && name.ends_with(".json") {
            matches.push(name.to_string());
        }
    }
    let [file_name] = matches.as_slice() else {
        return Err(format!(
            "expected exactly one content-addressed prediction feedback under {}, found {}",
            root.display(),
            matches.len()
        ));
    };
    let path = root.join(file_name);
    let body = read_regular_file_at_bounded(
        &feedback_dir,
        file_name,
        &path,
        MAX_EVALUATOR_FEEDBACK_BYTES,
    )?;
    let digest = format!("{:x}", Sha256::digest(&body));
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    if !stem.ends_with(&digest) {
        return Err(format!(
            "Rust evaluator feedback is not content-addressed: {}",
            path.display()
        ));
    }
    serde_json::from_slice(&body)
        .map_err(|error| format!("parse feedback {}: {error}", path.display()))
}

fn failed_output(reason: String) -> PredictionEvaluationOutput {
    PredictionEvaluationOutput {
        success: false,
        feedback: None,
        stdout: String::new(),
        stderr: String::new(),
        failure_reason: Some(reason),
    }
}

#[cfg(test)]
mod evaluator_fs_tests {
    use super::*;
    use std::io::Write as _;

    fn fixture_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ploy-prediction-evaluator-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[cfg(unix)]
    #[test]
    fn retained_log_fd_does_not_follow_replaced_path_entry() {
        let root = fixture_root("log-fd");
        fs::create_dir_all(&root).expect("create evaluator artifact fixture");
        let root_fd = open_artifact_staging_dir(
            &fs::canonicalize(&root).expect("canonical evaluator artifact fixture"),
        )
        .expect("open evaluator artifact fixture");
        let path = root.join("evaluator.stdout.raw");
        let mut writer =
            create_output(&root_fd, "evaluator.stdout.raw", &path).expect("create evaluator log");
        let mut reader = writer.try_clone().expect("retain evaluator log FD");
        writer.write_all(b"descriptor-owned").expect("write log");
        writer.flush().expect("flush log");

        fs::remove_file(&path).expect("unlink original log path");
        fs::write(&path, b"replacement-path").expect("write hostile replacement log");

        assert_eq!(
            read_bounded(&mut reader, &path).expect("read held evaluator log FD"),
            "descriptor-owned"
        );
        drop(writer);
        drop(reader);
        drop(root_fd);
        fs::remove_dir_all(root).expect("remove log fixture");
    }

    #[cfg(unix)]
    #[test]
    fn feedback_reader_rejects_symlinked_content_addressed_entry() {
        use std::os::unix::fs::symlink;

        let base = fixture_root("feedback-symlink");
        let artifact_dir = base.join("artifacts");
        let feedback_dir = artifact_dir
            .join("alpha-search")
            .join("full_depth_settlement_executable_pnl");
        fs::create_dir_all(&feedback_dir).expect("create feedback fixture");
        let outside = base.join("outside.json");
        let body = br#"{"untrusted":true}"#;
        fs::write(&outside, body).expect("write outside feedback");
        let digest = format!("{:x}", Sha256::digest(body));
        let name = format!("prediction-research-feedback-{digest}.json");
        symlink(&outside, feedback_dir.join(name)).expect("create hostile feedback symlink");
        let canonical_artifact_dir =
            fs::canonicalize(&artifact_dir).expect("canonical artifact fixture");
        let artifact_fd =
            open_artifact_staging_dir(&canonical_artifact_dir).expect("open artifact fixture");

        let error = read_unique_feedback(&artifact_fd, &canonical_artifact_dir)
            .expect_err("feedback symlink must fail closed");
        assert!(error.contains("without following symlinks"));

        drop(artifact_fd);
        fs::remove_dir_all(base).expect("remove feedback fixture");
    }
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.as_slice() == ["--print-policy-snapshot-id"] {
        println!("{}", current_prediction_policy_snapshot_id());
        return;
    }
    if args.first().map(String::as_str) == Some("--print-brief-snapshot-id") {
        let [_, mission_path] = args.as_slice() else {
            usage();
        };
        let mission: PredictionResearchMission =
            read_json(Path::new(mission_path)).unwrap_or_else(|reason| {
                eprintln!("ERROR: {reason}");
                std::process::exit(2);
            });
        println!("{}", research_brief_snapshot_id(&mission));
        return;
    }
    let [mission_path, snapshot_dir, output_dir] = args.as_slice() else {
        usage();
    };
    let mission: PredictionResearchMission =
        read_json(Path::new(mission_path)).unwrap_or_else(|reason| {
            eprintln!("ERROR: {reason}");
            std::process::exit(2);
        });
    let timeout = Duration::from_secs(mission.search_budget.max_seconds.max(1));
    let mut client = OpenAiCompatibleProposalClient::from_env(timeout).unwrap_or_else(|error| {
        eprintln!("ERROR: configure the local Grok/OpenAI-compatible proposal client: {error:#}");
        std::process::exit(2);
    });
    let mut evaluator = RustProcessEvaluator::new();
    let summary = run_or_resume(
        mission,
        Path::new(snapshot_dir),
        Path::new(output_dir),
        &mut client,
        &mut evaluator,
    )
    .unwrap_or_else(|reason| {
        eprintln!("ERROR: {reason}");
        std::process::exit(2);
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).expect("serialize loop summary")
    );
    if matches!(
        summary.status,
        LoopRunStatus::Paused | LoopRunStatus::Failed
    ) {
        std::process::exit(1);
    }
}
