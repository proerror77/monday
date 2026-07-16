//! Governed Rust LoopRun for one BTC or SOL five-minute prediction mission.
//!
//! Binance is predictor context, Chainlink is the contract reference-price
//! source, Polymarket resolution is the binary label, and the CLOB is the
//! execution/capacity surface. This binary has no live order path.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use ploy_research::prediction_llm::OpenAiCompatibleProposalClient;
use ploy_research::prediction_loop::{
    current_prediction_policy_snapshot_id, research_brief_snapshot_id, run_or_resume,
    LoopRunStatus, PredictionEvaluationOutput, PredictionEvaluationRequest, PredictionEvaluator,
    PredictionResearchMission, ProposalCallOutput, ProposalClient,
};
use ploy_research::{
    load_prediction_research_snapshot, normalized_underlying_symbol, PredictionResearchFeedback,
};
use sha2::{Digest, Sha256};

const MAX_EVALUATOR_LOG_BYTES: usize = 16 * 1024 * 1024;

fn usage() -> ! {
    eprintln!(
        "usage:\n  monday-prediction-research --print-policy-snapshot-id\n  monday-prediction-research --print-brief-snapshot-id <mission.json>\n  monday-prediction-research --verify-snapshot <snapshot-dir>\n  monday-prediction-research <mission.json> <snapshot-dir> <output-dir>"
    );
    std::process::exit(2);
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

struct RustProcessEvaluator {
    executable: PathBuf,
}

impl RustProcessEvaluator {
    fn new() -> Result<Self, String> {
        let executable = sibling_pinned_binary("monday-prediction-evaluator")?;
        Ok(Self { executable })
    }

    fn process(&self) -> Command {
        Command::new(&self.executable)
    }

    fn command(&self, request: &PredictionEvaluationRequest) -> Result<Command, String> {
        let snapshot = load_prediction_research_snapshot(&request.snapshot_dir)
            .map_err(|error| format!("load evaluator snapshot: {error:#}"))?;
        let underlying = request.mission.symbols[0].as_str();
        let symbol = snapshot
            .manifest
            .symbols
            .iter()
            .find(|symbol| normalized_underlying_symbol(symbol) == underlying)
            .ok_or_else(|| format!("snapshot has no {underlying} evaluator symbol"))?;

        let mut command = self.process();
        command
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
            .arg("core")
            .arg("--expected-search-policy-snapshot-id")
            .arg(&request.mission.search_policy_snapshot_id);

        if let Some(prior) = request.prior.as_ref() {
            if prior.value.probability_blends.is_empty() {
                return Err("candidate evaluation prior has no probability blends".to_string());
            }
            command
                .arg("--alpha-search-llm-prior-json")
                .arg(&prior.artifact_path)
                .arg("--alpha-search-output-dir")
                .arg(request.artifact_dir.join("alpha-search"));
        }
        Ok(command)
    }
}

fn sibling_pinned_binary(name: &str) -> Result<PathBuf, String> {
    let current = std::env::current_exe()
        .map_err(|error| format!("resolve prediction research executable: {error}"))?;
    let parent = current
        .parent()
        .ok_or_else(|| "prediction research executable has no parent directory".to_string())?;
    let executable = parent.join(name);
    if !executable.is_file() {
        return Err(format!(
            "release-pinned prediction evaluator does not exist: {}",
            executable.display()
        ));
    }
    verify_pinned_binary(&executable)?;
    Ok(executable)
}

fn verify_pinned_binary(path: &Path) -> Result<(), String> {
    let digest_path = path.with_extension("sha256");
    let expected = fs::read_to_string(&digest_path)
        .map_err(|error| {
            format!(
                "read pinned evaluator digest {}: {error}",
                digest_path.display()
            )
        })?
        .trim()
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "pinned evaluator digest is invalid: {}",
            digest_path.display()
        ));
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("read release-pinned evaluator {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes = file.read(&mut buffer).map_err(|error| {
            format!("hash release-pinned evaluator {}: {error}", path.display())
        })?;
        if bytes == 0 {
            break;
        }
        digest.update(&buffer[..bytes]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(format!(
            "release-pinned prediction evaluator digest mismatch: {}",
            path.display()
        ));
    }
    Ok(())
}

impl PredictionEvaluator for RustProcessEvaluator {
    fn evaluate(
        &mut self,
        request: &PredictionEvaluationRequest,
        timeout: Duration,
    ) -> PredictionEvaluationOutput {
        if let Err(error) = fs::create_dir_all(&request.artifact_dir) {
            return failed_output(format!(
                "create evaluator artifact directory {}: {error}",
                request.artifact_dir.display()
            ));
        }
        let mut command = match self.command(request) {
            Ok(command) => command,
            Err(reason) => return failed_output(reason),
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_evaluator_process_group(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return failed_output(format!("spawn Rust evaluator: {error}")),
        };
        let child_stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = terminate_evaluator_group(&mut child);
                return failed_output("Rust evaluator stdout pipe is unavailable".to_string());
            }
        };
        let child_stderr = match child.stderr.take() {
            Some(stderr) => stderr,
            None => {
                let _ = terminate_evaluator_group(&mut child);
                return failed_output("Rust evaluator stderr pipe is unavailable".to_string());
            }
        };
        let (capture_sender, capture_receiver) = mpsc::channel();
        spawn_bounded_capture(
            child_stdout,
            EvaluatorStream::Stdout,
            capture_sender.clone(),
        );
        spawn_bounded_capture(
            child_stderr,
            EvaluatorStream::Stderr,
            capture_sender.clone(),
        );
        drop(capture_sender);

        let started = Instant::now();
        let mut captures = Vec::with_capacity(2);
        let mut capture_failure = None;
        let mut process_failure = None;
        let (status, timed_out) = loop {
            while let Ok(capture) = capture_receiver.try_recv() {
                if capture_failure.is_none() {
                    capture_failure = capture.failure_reason();
                }
                captures.push(capture);
            }
            if capture_failure.is_some() {
                let status = terminate_evaluator_group(&mut child);
                break (status, false);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    #[cfg(unix)]
                    if !status.success() {
                        unsafe {
                            libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
                        }
                    }
                    break (Some(status), false);
                }
                Ok(None) if started.elapsed() < timeout => {
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let status = terminate_evaluator_group(&mut child);
                    break (status, true);
                }
                Err(error) => {
                    process_failure = Some(format!("poll Rust evaluator: {error}"));
                    let status = terminate_evaluator_group(&mut child);
                    break (status, false);
                }
            }
        };

        while captures.len() < 2 {
            match capture_receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(capture) => {
                    if capture_failure.is_none() {
                        capture_failure = capture.failure_reason();
                    }
                    captures.push(capture);
                }
                Err(error) => {
                    capture_failure.get_or_insert_with(|| {
                        format!("collect bounded Rust evaluator logs: {error}")
                    });
                    break;
                }
            }
        }
        let (stdout, stderr, persist_failure) =
            persist_bounded_captures(&request.artifact_dir, captures);
        let stdout = stdout.unwrap_or_default();
        let stderr = stderr.unwrap_or_default();
        if let Some(reason) = process_failure.or(capture_failure).or(persist_failure) {
            return PredictionEvaluationOutput::failure(reason, stdout, stderr);
        }
        if timed_out {
            return PredictionEvaluationOutput::failure(
                format!("Rust evaluator exceeded {} ms", timeout.as_millis()),
                stdout,
                stderr,
            );
        }
        if !status.is_some_and(|status| status.success()) {
            return PredictionEvaluationOutput::failure(
                format!("Rust evaluator exited with {status:?}"),
                stdout,
                stderr,
            );
        }

        let feedback = if request.prior.is_some() {
            match read_unique_feedback(&request.artifact_dir) {
                Ok(feedback) => Some(feedback),
                Err(reason) => {
                    return PredictionEvaluationOutput::failure(reason, stdout, stderr);
                }
            }
        } else {
            None
        };
        PredictionEvaluationOutput::success(feedback, stdout, stderr)
    }
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
        // The child was spawned as process-group leader. Always signal the
        // group so the evaluator cannot outlive the LoopRun lock.
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

struct LazyProposalClient {
    timeout: Duration,
    inner: Option<OpenAiCompatibleProposalClient>,
}

impl LazyProposalClient {
    fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            inner: None,
        }
    }
}

impl ProposalClient for LazyProposalClient {
    fn propose(&mut self, prompt: &str, timeout: Duration) -> Result<ProposalCallOutput, String> {
        if self.inner.is_none() {
            self.inner = Some(
                OpenAiCompatibleProposalClient::from_env(self.timeout).map_err(|error| {
                    format!("configure local Grok/OpenAI-compatible proposal client: {error:#}")
                })?,
            );
        }
        self.inner
            .as_mut()
            .expect("proposal client initialized")
            .propose(prompt, timeout)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvaluatorStream {
    Stdout,
    Stderr,
}

impl EvaluatorStream {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

struct EvaluatorCapture {
    stream: EvaluatorStream,
    bytes: Vec<u8>,
    overflow: bool,
    read_error: Option<String>,
}

impl EvaluatorCapture {
    fn failure_reason(&self) -> Option<String> {
        if self.overflow {
            Some(format!(
                "Rust evaluator {} exceeded {} bytes",
                self.stream.as_str(),
                MAX_EVALUATOR_LOG_BYTES
            ))
        } else {
            self.read_error
                .as_ref()
                .map(|error| format!("read Rust evaluator {}: {error}", self.stream.as_str()))
        }
    }
}

fn spawn_bounded_capture<R>(
    mut reader: R,
    stream: EvaluatorStream,
    sender: mpsc::Sender<EvaluatorCapture>,
) where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 16 * 1024];
        let mut overflow = false;
        let mut read_error = None;
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let remaining = MAX_EVALUATOR_LOG_BYTES.saturating_sub(bytes.len());
                    bytes.extend_from_slice(&buffer[..count.min(remaining)]);
                    if count > remaining {
                        overflow = true;
                        break;
                    }
                }
                Err(error) => {
                    read_error = Some(error.to_string());
                    break;
                }
            }
        }
        let _ = sender.send(EvaluatorCapture {
            stream,
            bytes,
            overflow,
            read_error,
        });
    });
}

fn persist_bounded_captures(
    artifact_dir: &Path,
    captures: Vec<EvaluatorCapture>,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut stdout = None;
    let mut stderr = None;
    let mut failure = None;
    for capture in captures {
        let path = artifact_dir.join(format!("evaluator.{}.raw", capture.stream.as_str()));
        if let Err(reason) = write_bounded_output(&path, &capture.bytes) {
            failure.get_or_insert(reason);
        }
        let text = match String::from_utf8(capture.bytes) {
            Ok(text) => text,
            Err(error) => {
                failure.get_or_insert_with(|| {
                    format!(
                        "Rust evaluator {} is not UTF-8: {error}",
                        capture.stream.as_str()
                    )
                });
                String::from_utf8_lossy(error.as_bytes()).into_owned()
            }
        };
        let slot = match capture.stream {
            EvaluatorStream::Stdout => &mut stdout,
            EvaluatorStream::Stderr => &mut stderr,
        };
        if slot.replace(text).is_some() {
            failure.get_or_insert_with(|| {
                format!(
                    "duplicate Rust evaluator {} capture",
                    capture.stream.as_str()
                )
            });
        }
    }
    if stdout.is_none() {
        failure.get_or_insert_with(|| "missing Rust evaluator stdout capture".to_string());
    }
    if stderr.is_none() {
        failure.get_or_insert_with(|| "missing Rust evaluator stderr capture".to_string());
    }
    (stdout, stderr, failure)
}

fn write_bounded_output(path: &Path, body: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create evaluator output {}: {error}", path.display()))?;
    file.write_all(body)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("write evaluator output {}: {error}", path.display()))
}

fn read_unique_feedback(artifact_dir: &Path) -> Result<PredictionResearchFeedback, String> {
    let root = artifact_dir
        .join("alpha-search")
        .join("full_depth_settlement_executable_pnl");
    let mut matches = Vec::new();
    for entry in fs::read_dir(&root)
        .map_err(|error| format!("read feedback directory {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| format!("read feedback directory entry: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("inspect feedback directory entry: {error}"))?;
        if !file_type.is_file() {
            return Err(format!(
                "feedback directory contains non-file entry {}",
                entry.path().display()
            ));
        }
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("prediction-research-feedback-") && name.ends_with(".json")
            })
        {
            matches.push(path);
        }
    }
    let [path] = matches.as_slice() else {
        return Err(format!(
            "expected exactly one content-addressed prediction feedback under {}, found {}",
            root.display(),
            matches.len()
        ));
    };
    let body =
        fs::read(path).map_err(|error| format!("read feedback {}: {error}", path.display()))?;
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
    PredictionEvaluationOutput::failure(reason, String::new(), String::new())
}

fn verify_snapshot(snapshot_dir: &Path) -> Result<(), String> {
    let snapshot = load_prediction_research_snapshot(snapshot_dir)
        .map_err(|error| format!("verify governed prediction snapshot: {error:#}"))?;
    println!(
        "{}",
        serde_json::json!({
            "schema_version": snapshot.manifest.schema_version,
            "snapshot_hash": snapshot.manifest.snapshot_hash,
            "snapshot_contract_hash": snapshot.manifest.snapshot_contract_hash,
            "immutable_input": snapshot.manifest.immutable_input,
            "require_official_settlement": snapshot.manifest.require_official_settlement,
        })
    );
    Ok(())
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
    if args.first().map(String::as_str) == Some("--verify-snapshot") {
        let [_, snapshot_dir] = args.as_slice() else {
            usage();
        };
        verify_snapshot(Path::new(snapshot_dir)).unwrap_or_else(|reason| {
            eprintln!("ERROR: {reason}");
            std::process::exit(2);
        });
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
    let mut client = LazyProposalClient::new(timeout);
    let mut evaluator = RustProcessEvaluator::new().unwrap_or_else(|reason| {
        eprintln!("ERROR: {reason}");
        std::process::exit(2);
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluator_capture_stops_at_the_governed_byte_limit() {
        let (sender, receiver) = mpsc::channel();
        spawn_bounded_capture(
            std::io::Cursor::new(vec![b'x'; MAX_EVALUATOR_LOG_BYTES + 1]),
            EvaluatorStream::Stdout,
            sender,
        );

        let capture = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("bounded capture");
        assert!(capture.overflow);
        assert_eq!(capture.bytes.len(), MAX_EVALUATOR_LOG_BYTES);
        assert!(capture
            .failure_reason()
            .expect("overflow reason")
            .contains("exceeded"));
    }

    #[test]
    fn evaluator_process_is_the_precompiled_monday_binary() {
        let evaluator = RustProcessEvaluator {
            executable: PathBuf::from("/usr/local/bin/monday-prediction-evaluator"),
        };

        let command = evaluator.process();

        assert_eq!(
            command.get_program(),
            Path::new("/usr/local/bin/monday-prediction-evaluator")
        );
        assert_ne!(command.get_program(), "cargo");
    }

    #[test]
    fn pinned_evaluator_rejects_a_digest_mismatch() {
        let root = tempfile::tempdir().expect("temporary evaluator root");
        let evaluator = root.path().join("monday-prediction-evaluator");
        fs::write(&evaluator, "release binary").expect("write evaluator");
        fs::write(evaluator.with_extension("sha256"), "0".repeat(64)).expect("write pinned digest");

        let error = verify_pinned_binary(&evaluator).expect_err("mismatch must fail");

        assert!(error.contains("digest mismatch"));
    }
}
