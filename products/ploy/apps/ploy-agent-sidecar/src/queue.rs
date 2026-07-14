use crate::{Result, SidecarError};
use chrono::Utc;
use ploy_operator_contracts::AgentRunCreateRequest;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedAgentRunRequest {
    pub run_id: String,
    pub created_at: String,
    pub request: AgentRunCreateRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_retry_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_retried_at: Option<String>,
}

impl QueuedAgentRunRequest {
    pub fn attempt(&self) -> u32 {
        self.attempt.unwrap_or_default()
    }

    fn key(&self) -> String {
        format!("{}:{}", self.run_id, self.attempt())
    }
}

#[derive(Debug, Clone)]
pub struct QueueStore {
    pub requests_path: PathBuf,
    pub in_progress_path: PathBuf,
    pub runs_path: PathBuf,
    pub harness_context_path: PathBuf,
    pub harness_events_path: PathBuf,
}

impl QueueStore {
    pub fn from_env(runtime_root: &Path) -> Result<Self> {
        let runs_path =
            env_path("PLOY_AGENT_RUNS_FILE").unwrap_or_else(|| default_runs_path(runtime_root));
        let run_dir = runs_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let requests_path = derived_path(
            "PLOY_AGENT_RUN_REQUESTS_FILE",
            run_dir.join("agent-run-requests.jsonl"),
        )?;
        let request_dir = requests_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Ok(Self {
            in_progress_path: env_path("PLOY_AGENT_RUN_IN_PROGRESS_FILE")
                .unwrap_or_else(|| request_dir.join("agent-run-requests.in-progress.jsonl")),
            harness_context_path: derived_path(
                "PLOY_HARNESS_CONTEXT_FILE",
                run_dir.join("harness-context.md"),
            )?,
            harness_events_path: derived_path(
                "PLOY_HARNESS_EVENTS_FILE",
                run_dir.join("harness-events.jsonl"),
            )?,
            requests_path,
            runs_path,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_dir(dir: &Path) -> Self {
        Self {
            requests_path: dir.join("agent-run-requests.jsonl"),
            in_progress_path: dir.join("agent-run-requests.in-progress.jsonl"),
            runs_path: dir.join("agent-runs.jsonl"),
            harness_context_path: dir.join("harness-context.md"),
            harness_events_path: dir.join("harness-events.jsonl"),
        }
    }

    pub fn claim(&self) -> Result<Option<ClaimedBatch>> {
        let _queue_lock = lock_jsonl(&self.requests_path)?;
        if self.in_progress_path.exists() {
            return self.read_claimed_batch().map(Some);
        }
        match fs::rename(&self.requests_path, &self.in_progress_path) {
            Ok(()) => self.read_claimed_batch().map(Some),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn read_claimed_batch(&self) -> Result<ClaimedBatch> {
        let body = fs::read(&self.in_progress_path)?;
        let terminal = self.terminal_attempts()?;
        let requests = parse_jsonl_with_tail_repair::<QueuedAgentRunRequest>(
            &self.in_progress_path,
            &body,
            "agent request",
        )?
        .into_iter()
        .filter(|request| !terminal.contains(&request.key()))
        .collect();
        Ok(ClaimedBatch {
            store: self.clone(),
            requests,
        })
    }

    fn terminal_attempts(&self) -> Result<BTreeSet<String>> {
        let _runs_lock = lock_jsonl(&self.runs_path)?;
        let mut terminal = BTreeSet::new();
        let body = match fs::read(&self.runs_path) {
            Ok(body) => body,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(terminal),
            Err(err) => return Err(err.into()),
        };
        for record in parse_jsonl_with_tail_repair::<Value>(&self.runs_path, &body, "run history")?
        {
            let status = record.get("status").and_then(Value::as_str).unwrap_or("");
            if matches!(status, "requested" | "started") {
                continue;
            }
            let Some(run_id) = record.get("run_id").and_then(Value::as_str) else {
                continue;
            };
            let attempt = record
                .pointer("/runtime_context/request/queue_attempt")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            terminal.insert(format!("{run_id}:{attempt}"));
        }
        Ok(terminal)
    }

    pub fn requeue(
        &self,
        queued: &QueuedAgentRunRequest,
        reason: &str,
        max_retries: u32,
    ) -> Result<Option<QueuedAgentRunRequest>> {
        let next_attempt = queued.attempt().saturating_add(1);
        if next_attempt > max_retries {
            return Ok(None);
        }
        let retry = QueuedAgentRunRequest {
            attempt: Some(next_attempt),
            last_retry_reason: Some(reason.to_string()),
            last_retried_at: Some(Utc::now().to_rfc3339()),
            ..queued.clone()
        };
        let _queue_lock = lock_jsonl(&self.requests_path)?;
        let body = match fs::read(&self.requests_path) {
            Ok(body) => body,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(err.into()),
        };
        let exists = parse_jsonl_with_tail_repair::<QueuedAgentRunRequest>(
            &self.requests_path,
            &body,
            "queued retry",
        )?
        .into_iter()
        .any(|candidate| candidate.key() == retry.key());
        if !exists {
            append_jsonl_sync_unlocked(&self.requests_path, &retry)?;
        }
        Ok(Some(retry))
    }

    pub fn acquire_worker_lease(&self) -> Result<File> {
        let path = worker_lock_path(&self.requests_path);
        let file = open_lock_file(&path)?;
        file.try_lock().map_err(|err| {
            SidecarError::Message(format!(
                "another ploy-agent-sidecar already owns {}: {err}",
                path.display()
            ))
        })?;
        Ok(file)
    }
}

pub struct ClaimedBatch {
    store: QueueStore,
    pub requests: Vec<QueuedAgentRunRequest>,
}

impl ClaimedBatch {
    pub fn complete(&mut self, completed: &QueuedAgentRunRequest) -> Result<()> {
        self.requests.retain(|candidate| {
            candidate.run_id != completed.run_id || candidate.attempt() != completed.attempt()
        });
        if self.requests.is_empty() {
            return remove_if_exists(&self.store.in_progress_path);
        }
        write_jsonl_atomically(&self.store.in_progress_path, &self.requests)
    }

    pub fn acknowledge(self) -> Result<()> {
        remove_if_exists(&self.store.in_progress_path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryFailpoint {
    AfterRetry,
    AfterTerminal,
}

pub fn finalize_needs_retry<R, C>(
    store: &QueueStore,
    queued: &QueuedAgentRunRequest,
    reason: &str,
    max_retries: u32,
    record_terminal: R,
    checkpoint: C,
    failpoint: Option<RetryFailpoint>,
) -> Result<Option<QueuedAgentRunRequest>>
where
    R: FnOnce() -> Result<()>,
    C: FnOnce() -> Result<()>,
{
    let retry = store.requeue(queued, reason, max_retries)?;
    if failpoint == Some(RetryFailpoint::AfterRetry) {
        return Err(SidecarError::Message("failpoint: after_retry".to_string()));
    }
    record_terminal()?;
    if failpoint == Some(RetryFailpoint::AfterTerminal) {
        return Err(SidecarError::Message(
            "failpoint: after_terminal".to_string(),
        ));
    }
    checkpoint()?;
    Ok(retry)
}

pub fn append_jsonl_sync<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let _lock = lock_jsonl(path)?;
    append_jsonl_sync_unlocked(path, value)
}

fn append_jsonl_sync_unlocked<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    ensure_parent(path)?;
    let mut body = serde_json::to_vec(value)?;
    body.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&body)?;
    file.sync_data()?;
    Ok(())
}

/// Parse a JSONL file while recovering only the one crash signature we can
/// identify without guessing: a malformed final fragment that has no trailing
/// newline. Complete malformed lines remain fatal. The removed bytes are kept
/// in a private quarantine file beside the source before the valid prefix is
/// installed atomically.
fn parse_jsonl_with_tail_repair<T: DeserializeOwned>(
    path: &Path,
    body: &[u8],
    record_kind: &str,
) -> Result<Vec<T>> {
    let mut records = Vec::new();
    let mut offset = 0_usize;
    for (index, segment) in body.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let next_offset = offset + segment.len();
        let has_newline = segment.ends_with(b"\n");
        let line = segment
            .strip_suffix(b"\n")
            .unwrap_or(segment)
            .strip_suffix(b"\r")
            .unwrap_or_else(|| segment.strip_suffix(b"\n").unwrap_or(segment));
        let decoded = std::str::from_utf8(line);
        if decoded.as_ref().is_ok_and(|line| line.trim().is_empty()) {
            offset = next_offset;
            continue;
        }
        let parsed = decoded
            .map_err(|err| format!("invalid UTF-8: {err}"))
            .and_then(|line| serde_json::from_str::<T>(line).map_err(|err| err.to_string()));
        match parsed {
            Ok(record) => {
                records.push(record);
                if !has_newline && next_offset == body.len() {
                    let mut normalized = body.to_vec();
                    normalized.push(b'\n');
                    write_bytes_atomically(path, &normalized)?;
                    eprintln!(
                        "normalized complete {record_kind} tail without newline in {}",
                        path.display()
                    );
                }
            }
            Err(err) if !has_newline && next_offset == body.len() => {
                let quarantine = quarantine_truncated_tail(path, segment)?;
                write_bytes_atomically(path, &body[..offset])?;
                eprintln!(
                    "quarantined truncated {record_kind} tail from {} to {}: {err}",
                    path.display(),
                    quarantine.display()
                );
                break;
            }
            Err(err) => {
                return Err(SidecarError::Message(format!(
                    "malformed {record_kind} on line {} of {}: {err}",
                    index + 1,
                    path.display()
                )));
            }
        }
        offset = next_offset;
    }
    Ok(records)
}

fn quarantine_truncated_tail(path: &Path, tail: &[u8]) -> Result<PathBuf> {
    ensure_parent(path)?;
    let suffix = format!(
        ".corrupt-tail-{}",
        Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| Utc::now().timestamp_micros() * 1_000)
    );
    let quarantine = suffixed_path(path, &suffix);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&quarantine)?;
    file.write_all(tail)?;
    file.sync_all()?;
    Ok(quarantine)
}

fn write_bytes_atomically(path: &Path, body: &[u8]) -> Result<()> {
    ensure_parent(path)?;
    let temporary = suffixed_path(path, ".repair.tmp");
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(body)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn lock_jsonl(path: &Path) -> Result<File> {
    let file = open_lock_file(&jsonl_lock_path(path))?;
    file.lock()?;
    Ok(file)
}

fn open_lock_file(path: &Path) -> Result<File> {
    ensure_parent(path)?;
    Ok(OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?)
}

fn jsonl_lock_path(path: &Path) -> PathBuf {
    suffixed_path(path, ".lock")
}

fn worker_lock_path(path: &Path) -> PathBuf {
    suffixed_path(path, ".worker.lock")
}

fn suffixed_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub fn append_text_sync(path: &Path, text: &str) -> Result<()> {
    ensure_parent(path)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_data()?;
    Ok(())
}

pub fn write_text_sync(path: &Path, text: &str) -> Result<()> {
    ensure_parent(path)?;
    let mut file = File::create(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn write_jsonl_atomically<T: Serialize>(path: &Path, values: &[T]) -> Result<()> {
    ensure_parent(path)?;
    let temporary = path.with_extension("jsonl.tmp");
    let mut file = File::create(&temporary)?;
    for value in values {
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn default_runs_path(runtime_root: &Path) -> PathBuf {
    runtime_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("run"))
        .join("sidecar/agent-runs.jsonl")
}

fn derived_path(key: &str, expected: PathBuf) -> Result<PathBuf> {
    let Some(configured) = env_path(key) else {
        return Ok(expected);
    };
    if configured != expected {
        return Err(SidecarError::Message(format!(
            "{key} must match the path derived from PLOY_AGENT_RUNS_FILE ({}) so new-ployd and the sidecar cannot diverge",
            expected.display()
        )));
    }
    Ok(configured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use uuid::Uuid;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ploy-sidecar-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create temp directory");
        dir
    }

    #[test]
    fn default_run_path_tracks_daemon_runtime_root_derivation() {
        assert_eq!(
            default_runs_path(Path::new("run/platform")),
            PathBuf::from("run/sidecar/agent-runs.jsonl")
        );
        assert_eq!(
            default_runs_path(Path::new("/opt/ploy/run/platform")),
            PathBuf::from("/opt/ploy/run/sidecar/agent-runs.jsonl")
        );
    }

    #[test]
    fn worker_lease_rejects_a_second_consumer() {
        let dir = temp_dir("worker-lease");
        let store = QueueStore::for_dir(&dir);
        let lease = store.acquire_worker_lease().expect("first lease");
        assert!(store.acquire_worker_lease().is_err());
        drop(lease);
        fs::remove_dir_all(dir).expect("remove temp");
    }

    #[test]
    fn claim_waits_for_an_open_producer_before_renaming() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let dir = temp_dir("producer-race");
        let store = QueueStore::for_dir(&dir);
        let producer_lock = lock_jsonl(&store.requests_path).expect("producer lock");
        let worker_store = store.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _ = sender.send(worker_store.claim());
        });
        assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
        append_jsonl_sync_unlocked(&store.requests_path, &queued("run-race"))
            .expect("producer append");
        drop(producer_lock);
        let batch = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("claim result")
            .expect("claim")
            .expect("batch");
        assert_eq!(batch.requests.len(), 1);
        assert_eq!(batch.requests[0].run_id, "run-race");
        batch.acknowledge().expect("acknowledge");
        worker.join().expect("worker");
        fs::remove_dir_all(dir).expect("remove temp");
    }

    fn queued(run_id: &str) -> QueuedAgentRunRequest {
        QueuedAgentRunRequest {
            run_id: run_id.to_string(),
            created_at: "2026-07-08T00:00:00Z".to_string(),
            request: AgentRunCreateRequest {
                objective: "test".to_string(),
                strategy_profile: "test".to_string(),
                autonomy_mode: "research_until_blocked".to_string(),
                target_evidence: "diagnostic".to_string(),
                symbols: vec!["TEST".to_string()],
                max_turns: 3,
                budget_usd: 1.0,
                run_packet: "packet".to_string(),
                run_contract: "completion_signal = \"required\"".to_string(),
            },
            attempt: None,
            last_retry_reason: None,
            last_retried_at: None,
        }
    }

    #[test]
    fn claim_recovers_in_progress_batch_and_skips_terminal_attempts() {
        let dir = temp_dir("claim");
        let store = QueueStore::for_dir(&dir);
        append_jsonl_sync(&store.requests_path, &queued("run-one")).expect("append one");
        append_jsonl_sync(&store.requests_path, &queued("run-two")).expect("append two");

        let first = store.claim().expect("claim").expect("batch");
        assert_eq!(first.requests.len(), 2);
        append_jsonl_sync(
            &store.runs_path,
            &serde_json::json!({
                "run_id": "run-one",
                "status": "needs_retry",
                "runtime_context": { "request": { "queue_attempt": 0 } }
            }),
        )
        .expect("terminal record");
        drop(first);

        let mut recovered = store.claim().expect("recover").expect("batch");
        assert_eq!(recovered.requests.len(), 1);
        assert_eq!(recovered.requests[0].run_id, "run-two");
        let second = recovered.requests[0].clone();
        recovered.complete(&second).expect("complete");
        assert!(store.claim().expect("empty claim").is_none());
        fs::remove_dir_all(dir).expect("remove temp");
    }

    #[test]
    fn malformed_claim_fails_closed_without_deleting_the_queue() {
        let dir = temp_dir("malformed");
        let store = QueueStore::for_dir(&dir);
        fs::write(&store.requests_path, b"{broken json\n").expect("write malformed queue");
        assert!(store.claim().is_err());
        assert!(store.in_progress_path.exists());
        assert!(!store.requests_path.exists());
        assert_eq!(
            fs::read_to_string(&store.in_progress_path).expect("preserved claim"),
            "{broken json\n"
        );
        fs::remove_dir_all(dir).expect("remove temp");
    }

    #[test]
    fn claim_quarantines_only_a_truncated_final_fragment() {
        let dir = temp_dir("truncated-tail");
        let store = QueueStore::for_dir(&dir);
        append_jsonl_sync(&store.requests_path, &queued("run-valid")).expect("valid request");
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&store.requests_path)
                .expect("open queue");
            file.write_all(b"{\"run_id\":\"truncated")
                .expect("write truncated tail");
            file.sync_all().expect("sync truncated tail");
        }

        let batch = store.claim().expect("repair claim").expect("batch");
        assert_eq!(batch.requests.len(), 1);
        assert_eq!(batch.requests[0].run_id, "run-valid");
        let repaired = fs::read_to_string(&store.in_progress_path).expect("repaired claim");
        assert_eq!(repaired.lines().count(), 1);
        assert!(repaired.ends_with('\n'));
        assert!(fs::read_dir(&dir)
            .expect("read queue directory")
            .filter_map(std::result::Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .contains(".corrupt-tail-")));
        batch.acknowledge().expect("acknowledge");
        fs::remove_dir_all(dir).expect("remove temp");
    }

    #[test]
    fn claim_quarantines_a_truncated_multibyte_utf8_tail() {
        let dir = temp_dir("truncated-utf8-tail");
        let store = QueueStore::for_dir(&dir);
        append_jsonl_sync(&store.requests_path, &queued("run-valid")).expect("valid request");
        {
            let mut file = OpenOptions::new()
                .append(true)
                .open(&store.requests_path)
                .expect("open queue");
            file.write_all(b"{\"run_id\":\"").expect("partial JSON");
            file.write_all(&[0xe4, 0xb8]).expect("partial UTF-8");
            file.sync_all().expect("sync truncated tail");
        }

        let batch = store.claim().expect("repair claim").expect("batch");
        assert_eq!(batch.requests.len(), 1);
        assert_eq!(batch.requests[0].run_id, "run-valid");
        assert!(fs::read(&store.in_progress_path)
            .expect("repaired claim")
            .ends_with(b"\n"));
        batch.acknowledge().expect("acknowledge");
        fs::remove_dir_all(dir).expect("remove temp");
    }

    #[test]
    fn complete_tail_without_newline_is_normalized_before_future_appends() {
        let dir = temp_dir("complete-tail");
        let store = QueueStore::for_dir(&dir);
        let encoded = serde_json::to_vec(&queued("run-no-newline")).expect("encode request");
        fs::write(&store.requests_path, encoded).expect("write complete tail");

        let batch = store.claim().expect("normalize claim").expect("batch");
        assert_eq!(batch.requests.len(), 1);
        assert!(fs::read(&store.in_progress_path)
            .expect("normalized queue")
            .ends_with(b"\n"));
        drop(batch);

        append_jsonl_sync(&store.in_progress_path, &queued("run-next")).expect("future append");
        let recovered = store.claim().expect("recover claim").expect("batch");
        assert_eq!(recovered.requests.len(), 2);
        assert_eq!(recovered.requests[1].run_id, "run-next");
        recovered.acknowledge().expect("acknowledge");
        fs::remove_dir_all(dir).expect("remove temp");
    }

    #[test]
    fn retry_is_deduplicated_and_crash_boundaries_keep_work_recoverable() {
        let dir = temp_dir("retry");
        let store = QueueStore::for_dir(&dir);
        let request = queued("run-retry");
        append_jsonl_sync(&store.in_progress_path, &request).expect("in progress");

        let terminal_writes = Cell::new(0);
        let checkpoints = Cell::new(0);
        let failed = finalize_needs_retry(
            &store,
            &request,
            "missing completion",
            1,
            || {
                terminal_writes.set(terminal_writes.get() + 1);
                Ok(())
            },
            || {
                checkpoints.set(checkpoints.get() + 1);
                Ok(())
            },
            Some(RetryFailpoint::AfterRetry),
        );
        assert!(failed.is_err());
        assert_eq!(terminal_writes.get(), 0);
        assert_eq!(checkpoints.get(), 0);
        assert_eq!(store.claim().expect("recover").unwrap().requests.len(), 1);

        let retry = store
            .requeue(&request, "missing completion", 1)
            .expect("dedupe")
            .expect("retry");
        assert_eq!(retry.attempt(), 1);
        let body = fs::read_to_string(&store.requests_path).expect("queue");
        assert_eq!(body.lines().count(), 1);
        assert!(store
            .requeue(&retry, "still missing", 1)
            .expect("limit")
            .is_none());

        let failed = finalize_needs_retry(
            &store,
            &request,
            "missing completion",
            1,
            || {
                terminal_writes.set(terminal_writes.get() + 1);
                append_jsonl_sync(
                    &store.runs_path,
                    &serde_json::json!({
                        "run_id": request.run_id,
                        "status": "needs_retry",
                        "runtime_context": { "request": { "queue_attempt": 0 } }
                    }),
                )
            },
            || {
                checkpoints.set(checkpoints.get() + 1);
                Ok(())
            },
            Some(RetryFailpoint::AfterTerminal),
        );
        assert!(failed.is_err());
        assert_eq!(terminal_writes.get(), 1);
        assert_eq!(checkpoints.get(), 0);
        let recovered = store.claim().expect("recover after terminal").unwrap();
        assert!(
            recovered.requests.is_empty(),
            "terminal history prevents original replay before checkpoint"
        );
        recovered.acknowledge().expect("ack terminal claim");
        assert_eq!(
            fs::read_to_string(&store.requests_path)
                .expect("retry queue")
                .lines()
                .count(),
            1,
            "crash after terminal preserves exactly one retry"
        );

        fs::remove_dir_all(dir).expect("remove temp");
    }
}
