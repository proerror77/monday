//! Pinned app-server child transport. Native state stays on a private persistent
//! volume; a PG thread ID alone never authorizes resume. No cloud provisioning.
use crate::{
    coding_agent::{ApprovalKind, Delivery, PendingApproval, RpcId},
    identity, sha256, valid_digest,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    time::timeout,
};

const FRAME_LIMIT: usize = 1024 * 1024;
const FILE_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    pub executable: PathBuf,
    pub executable_sha256: String,
    pub workspace: PathBuf,
    pub native_home: PathBuf,
    /// Host-only delivery records; never expose this directory to the agent.
    pub delivery_directory: PathBuf,
}

fn private_directory(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "session path must be absolute");
    std::fs::create_dir_all(path)?;
    ensure!(
        std::fs::symlink_metadata(path)?.is_dir() && path.canonicalize()? == path,
        "session directory must be canonical, without aliases"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn file_digest(path: &Path) -> Result<(String, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && meta.len() <= FILE_LIMIT,
        "unbounded or aliased session file"
    );
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        ensure!(total <= FILE_LIMIT, "session file grew beyond bound");
        hasher.update(&buffer[..n]);
    }
    ensure!(total == meta.len(), "session file changed during readback");
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    ensure!(
        std::fs::symlink_metadata(path)?.is_file(),
        "aliased delivery record"
    );
    File::open(path)?
        .take(FRAME_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= FRAME_LIMIT, "delivery record exceeds bound");
    Ok(serde_json::from_slice(&bytes)?)
}

fn durable_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("missing record parent")?;
    let temp = parent.join(format!(
        ".{}-{}.pending",
        path.file_name()
            .context("missing record name")?
            .to_string_lossy(),
        std::process::id()
    ));
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= FRAME_LIMIT, "delivery record exceeds bound");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() && temp.exists() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Result<(tempfile::TempDir, SessionConfig)> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let source = root.join("fixture.rs");
        std::fs::write(&source, include_str!("../tests/fixtures/app_server.rs"))?;
        let executable = root.join("fixture-codex");
        ensure!(
            std::process::Command::new("rustc")
                .args(["--edition=2021"])
                .arg(source)
                .arg("-o")
                .arg(&executable)
                .status()?
                .success(),
            "protocol fixture failed to compile"
        );
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let config = SessionConfig {
            executable_sha256: file_digest(&executable)?.0,
            executable,
            workspace,
            native_home: root.join("native"),
            delivery_directory: root.join("delivery"),
        };
        Ok((temporary, config))
    }
    #[tokio::test]
    async fn persistent_session_restarts_and_completion_is_delivered_once() -> Result<()> {
        let (_temp, config) = fixture()?;
        let mut server = AppServer::start(config.clone()).await?;
        server.open_thread(None).await?;
        assert!(AppServer::start(config.clone()).await.is_err());
        let intent = "a".repeat(64);
        let first = server
            .send_message(&intent, "Run finished; read its verified result")
            .await?;
        assert_eq!(first.delivery, Delivery::Accepted);
        let repeated = server
            .send_message(&intent, "Run finished; read its verified result")
            .await?;
        assert_eq!(first.turn_id, repeated.turn_id);
        assert!(server
            .send_message(&intent, "changed payload")
            .await
            .is_err());
        let event = server.next_event().await?;
        let id: RpcId = serde_json::from_value(event["id"].clone())?;
        assert!(server
            .approve(
                &"b".repeat(64),
                &id,
                ApprovalKind::Command,
                json!({"decision":"accept"})
            )
            .await
            .is_err());
        let generation = server.generation().to_string();
        server
            .approve(
                &generation,
                &id,
                ApprovalKind::Command,
                json!({"decision":"decline"}),
            )
            .await?;
        let native = server.checkpoint().await?;
        assert!(native
            .files
            .iter()
            .all(|f| !f.path.contains("auth") && !f.path.contains("config")));
        let mut resumed = AppServer::resume(config.clone(), &native).await?;
        assert_ne!(resumed.generation(), generation);
        assert_eq!(resumed.thread_id(), Some("thread-fixture"));
        assert!(resumed
            .approve(
                &generation,
                &id,
                ApprovalKind::Command,
                json!({"decision":"accept"})
            )
            .await
            .is_err());
        let accepted = resumed.reconcile_message(&intent).await?;
        assert_eq!(accepted.delivery, Delivery::Accepted);
        resumed.close().await?;
        std::fs::write(config.native_home.join(&native.files[0].path), "changed")?;
        assert!(AppServer::resume(config, &native).await.is_err());
        Ok(())
    }
    #[tokio::test]
    async fn disconnected_delivery_stays_unknown_until_native_readback() -> Result<()> {
        let (_temp, config) = fixture()?;
        std::fs::write(config.workspace.join("disconnect"), "")?;
        let mut server = AppServer::start(config.clone()).await?;
        server.open_thread(None).await?;
        let intent = "c".repeat(64);
        assert!(server.send_message(&intent, "completion").await.is_err());
        let record: MessageDelivery =
            read_json(&config.delivery_directory.join(format!("{intent}.json")))?;
        assert_eq!(record.delivery, Delivery::Unknown);
        let native = server.checkpoint().await?;
        std::fs::remove_file(config.workspace.join("disconnect"))?;
        let mut server = AppServer::resume(config, &native).await?;
        assert!(server.send_message(&intent, "completion").await.is_err());
        assert_eq!(
            server.reconcile_message(&intent).await?.delivery,
            Delivery::Accepted
        );
        assert_eq!(
            server.send_message(&intent, "completion").await?.delivery,
            Delivery::Accepted
        );
        server.close().await?;
        Ok(())
    }
    #[tokio::test]
    async fn oversized_frame_poisoned_transport_cannot_resend() -> Result<()> {
        let (_temp, config) = fixture()?;
        std::fs::write(config.workspace.join("oversize"), "")?;
        let mut server = AppServer::start(config).await?;
        server.open_thread(None).await?;
        assert!(server
            .send_message(&"d".repeat(64), "completion")
            .await
            .is_err());
        assert!(server.interrupt("turn-fixture").await.is_err());
        server.close().await?;
        Ok(())
    }
    #[tokio::test]
    async fn cancelled_event_wait_keeps_its_partial_frame() -> Result<()> {
        let (_temp, config) = fixture()?;
        std::fs::write(config.workspace.join("fragment"), "")?;
        let mut server = AppServer::start(config.clone()).await?;
        server.open_thread(None).await?;
        server.send_message(&"e".repeat(64), "completion").await?;
        server.next_event().await?; // queued approval before the response
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !config.workspace.join("fragment.sent").exists() {
            ensure!(
                tokio::time::Instant::now() < deadline,
                "fragment fixture did not send prefix"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(timeout(Duration::from_millis(10), server.next_event())
            .await
            .is_err());
        assert!(!server.frame.is_empty());
        std::fs::write(config.workspace.join("fragment.release"), "")?;
        let event = server.next_event().await?;
        assert_eq!(event["method"], "thread/status/changed");
        server.close().await?;
        Ok(())
    }
    #[tokio::test]
    async fn matching_client_id_cannot_acknowledge_a_different_native_payload() -> Result<()> {
        let (_temp, config) = fixture()?;
        let mut server = AppServer::start(config.clone()).await?;
        server.open_thread(None).await?;
        let intent = "f".repeat(64);
        server.send_message(&intent, "completion").await?;
        let native = server.checkpoint().await?;
        std::fs::write(
            config.native_home.join("received-text"),
            "different payload",
        )?;
        let mut server = AppServer::resume(config, &native).await?;
        assert!(server.verified_delivery(&intent).await.is_err());
        server.close().await?;
        Ok(())
    }
    #[test]
    fn native_snapshot_and_tools_cannot_import_secrets_or_admin_capabilities() -> Result<()> {
        for path in [
            "auth.json",
            "config.toml",
            "../state_5.sqlite",
            "sessions/../auth.json",
            "/sessions/thread.jsonl",
        ] {
            assert!(!native_path(path));
        }
        let event = json!({"id":1,"method":"item/tool/call","params":{"threadId":"thread","namespace":"research","tool":"status","arguments":{"run_sha256":"a".repeat(64)}}});
        research_call(&event, "thread")?;
        assert!(research_call(&event, "another").is_err());
        for tool in ["kubectl", "sign", "resume", "sql", "order"] {
            let mut denied = event.clone();
            denied["params"]["tool"] = json!(tool);
            assert!(research_call(&denied, "thread").is_err());
        }
        let mut denied = event.clone();
        denied["params"]["arguments"]["tenant"] = json!("another");
        assert!(research_call(&denied, "thread").is_err());
        for endpoint in [
            "http://example.com/research",
            "https://u:p@example.com/research",
            "https://example.com/research?token=x",
            "https://example.com/research#x",
        ] {
            assert!(ResearchClient::new(endpoint, "a".repeat(32)).is_err());
        }
        ResearchClient::new("https://example.com/research", "a".repeat(32))?;
        Ok(())
    }
    #[tokio::test]
    async fn research_client_reloads_private_host_token_and_fails_closed() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let app = axum::Router::new().route(
            "/research",
            axum::routing::post(|headers: axum::http::HeaderMap| async move {
                if headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    == Some(format!("Bearer {}", "x".repeat(32)).as_str())
                {
                    (axum::http::StatusCode::OK, "{\"verified\":true}")
                } else {
                    (axum::http::StatusCode::UNAUTHORIZED, "{}")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/research", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let temporary = tempfile::tempdir()?;
        let directory = temporary.path().canonicalize()?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let token = directory.join("host.token");
        std::fs::write(&token, "x".repeat(32))?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))?;
        let client = ResearchClient::from_file(&endpoint, token.clone(), &Default::default())?;
        let tool = crate::research::ResearchTool::Status {
            run_sha256: "a".repeat(64),
        };
        assert_eq!(client.execute(&tool).await?, json!({"verified":true}));
        std::fs::write(&token, "y".repeat(32))?;
        assert!(client.execute(&tool).await.is_err());
        std::fs::write(&token, "x".repeat(32))?;
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644))?;
        assert!(client.execute(&tool).await.is_err());
        std::fs::remove_file(&token)?;
        assert!(client.execute(&tool).await.is_err());
        server.abort();
        Ok(())
    }
    #[tokio::test]
    async fn research_client_authenticates_and_rejects_redirects() -> Result<()> {
        let app = axum::Router::new().route(
            "/research",
            axum::routing::post(|headers: axum::http::HeaderMap| async move {
                assert_eq!(
                    headers["authorization"],
                    format!("Bearer {}", "x".repeat(32))
                );
                axum::response::Redirect::temporary("http://127.0.0.1:1/secret")
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/research", listener.local_addr()?);
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = ResearchClient::new(&endpoint, "x".repeat(32))?;
        assert!(client
            .execute(&crate::research::ResearchTool::Status {
                run_sha256: "a".repeat(64)
            })
            .await
            .is_err());
        server.abort();
        Ok(())
    }
    #[tokio::test]
    #[ignore = "explicit installed app-server; protocol only, no turn or model call"]
    async fn installed_app_server_initializes_without_model_or_cloud() -> Result<()> {
        let executable = PathBuf::from(std::env::var("MONDAY_TEST_CODEX_EXE")?).canonicalize()?;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let server = AppServer::start(SessionConfig {
            executable_sha256: file_digest(&executable)?.0,
            executable,
            workspace,
            native_home: root.join("native"),
            delivery_directory: root.join("delivery"),
        })
        .await?;
        server.close().await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NativeFile {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// Authentication, config, private keys and tool tokens are deliberately absent.
/// Capture requires a stopped child under the same exclusive native-home lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeState {
    pub schema: u32,
    pub provider_binary_sha256: String,
    pub thread_id: String,
    pub files: Vec<NativeFile>,
}

fn native_path(path: &str) -> bool {
    let p = Path::new(path);
    !p.is_absolute()
        && p.components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
        && ((path.starts_with("sessions/") && path.ends_with(".jsonl"))
            || (!path.contains('/')
                && path.starts_with("state_")
                && (path.ends_with(".sqlite")
                    || path.ends_with(".sqlite-wal")
                    || path.ends_with(".sqlite-shm"))))
}

impl NativeState {
    fn validate(&self, expected_binary: &str, expected_thread: &str) -> Result<String> {
        ensure!(
            self.schema == 1
                && self.provider_binary_sha256 == expected_binary
                && valid_digest(expected_binary)
                && self.thread_id == expected_thread
                && !expected_thread.is_empty()
                && (1..=2048).contains(&self.files.len())
                && self.files.windows(2).all(|v| v[0].path < v[1].path),
            "native state identity/coverage mismatch"
        );
        let mut total = 0u64;
        let mut rollout = false;
        for entry in &self.files {
            ensure!(
                native_path(&entry.path)
                    && valid_digest(&entry.sha256)
                    && entry.bytes <= FILE_LIMIT,
                "unsafe native state entry"
            );
            total = total
                .checked_add(entry.bytes)
                .context("native state size overflow")?;
            ensure!(
                total <= 512 * 1024 * 1024,
                "native state total exceeds bound"
            );
            rollout |= entry.path.starts_with("sessions/") && entry.path.contains(expected_thread);
        }
        ensure!(rollout, "native thread rollout missing");
        identity(self)
    }
    /// An independent consumer reads each file once before starting a new child.
    pub fn verify(
        &self,
        home: &Path,
        expected_binary: &str,
        expected_thread: &str,
    ) -> Result<String> {
        let id = self.validate(expected_binary, expected_thread)?;
        let canonical_home = home.canonicalize()?;
        for entry in &self.files {
            let path = home.join(&entry.path);
            ensure!(
                path.canonicalize()?.starts_with(&canonical_home),
                "native state escaped its volume"
            );
            let (digest, bytes) = file_digest(&path)?;
            ensure!(
                digest == entry.sha256 && bytes == entry.bytes,
                "native state missing or changed"
            );
        }
        Ok(id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageDelivery {
    pub schema: u32,
    pub intent_sha256: String,
    pub thread_id: String,
    pub payload_sha256: String,
    pub process_generation: String,
    pub delivery: Delivery,
    pub turn_id: Option<String>,
    pub native_readback_sha256: Option<String>,
}

/// Constructed only from a current pinned child's native message readback.
pub struct VerifiedDelivery {
    record: MessageDelivery,
    provider_binary_sha256: String,
    message: String,
}
impl VerifiedDelivery {
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn record(&self) -> &MessageDelivery {
        &self.record
    }
    pub fn provider_binary_sha256(&self) -> &str {
        &self.provider_binary_sha256
    }
}

struct SessionLocks {
    _native: File,
    _delivery: File,
}

/// Owns one child and the OS native-home lock. Provider events are bounded and
/// returned to the host; prompts/approvals are never printed by the transport.
pub struct AppServer {
    child: Option<Child>,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    frame: Vec<u8>,
    lock: Option<SessionLocks>,
    config: SessionConfig,
    generation: String,
    next_id: i64,
    events: VecDeque<Value>,
    approvals: BTreeMap<String, PendingApproval>,
    tool_requests: BTreeMap<String, Value>,
    thread_id: Option<String>,
    verified_resume: Option<String>,
    verified_message: Option<String>,
    poisoned: bool,
}

impl Drop for AppServer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let lock = self.lock.take();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                    drop(lock);
                });
            } else {
                // Without a runtime, retain the lock rather than permit an
                // overlapping child before stop can be independently confirmed.
                if let Some(lock) = lock {
                    std::mem::forget(lock);
                }
            }
        }
    }
}

impl AppServer {
    pub async fn start(config: SessionConfig) -> Result<Self> {
        Self::launch(config, None).await
    }
    pub async fn resume(config: SessionConfig, native: &NativeState) -> Result<Self> {
        let mut client = Self::launch(config, Some(native)).await?;
        client.open_thread(Some(&native.thread_id)).await?;
        Ok(client)
    }
    async fn launch(config: SessionConfig, native: Option<&NativeState>) -> Result<Self> {
        ensure!(
            config.executable.is_absolute()
                && file_digest(&config.executable)?.0 == config.executable_sha256,
            "untrusted app-server executable"
        );
        let mut header = [0u8; 4];
        File::open(&config.executable)?.read_exact(&mut header)?;
        ensure!(
            matches!(
                header,
                [0x7f, b'E', b'L', b'F'] | [0xcf, 0xfa, 0xed, 0xfe] | [0xfe, 0xed, 0xfa, 0xcf]
            ),
            "app-server must be the pinned native binary, not a launcher script"
        );
        ensure!(
            config.workspace.is_absolute()
                && config.workspace.canonicalize()? == config.workspace
                && config.workspace.is_dir(),
            "invalid session workspace"
        );
        ensure!(
            !config.native_home.starts_with(&config.workspace)
                && !config.workspace.starts_with(&config.native_home)
                && !config.delivery_directory.starts_with(&config.workspace)
                && !config.workspace.starts_with(&config.delivery_directory),
            "state and host delivery records must be outside the workspace"
        );
        private_directory(&config.native_home)?;
        private_directory(&config.delivery_directory)?;
        ensure!(
            !config.native_home.join("config.toml").exists(),
            "session native home must not import user configuration"
        );
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(config.native_home.join(".monday-session.lock"))?;
        lock.try_lock()
            .context("native session already has a writer")?;
        let delivery_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(config.delivery_directory.join(".monday-delivery.lock"))?;
        delivery_lock
            .try_lock()
            .context("delivery ledger already has a writer")?;
        let lock = SessionLocks {
            _native: lock,
            _delivery: delivery_lock,
        };
        // Check stopped-state bytes before the new child opens SQLite/WAL.
        let verified_resume = native
            .map(|state| {
                state.verify(
                    &config.native_home,
                    &config.executable_sha256,
                    &state.thread_id,
                )?;
                Ok::<_, anyhow::Error>(state.thread_id.clone())
            })
            .transpose()?;
        let generation = identity(&(
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            &config.native_home,
        ))?;
        let mut command = tokio::process::Command::new(&config.executable);
        command
            .args([
                "app-server",
                "--listen",
                "stdio://",
                "-c",
                "features.shell_tool=false",
                "-c",
                "features.apps=false",
                "-c",
                "features.browser_use=false",
                "-c",
                "web_search=\"disabled\"",
            ])
            .env_clear()
            .env("HOME", &config.native_home)
            .env("CODEX_HOME", &config.native_home)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .current_dir(&config.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let input = child.stdin.take().context("app-server stdin missing")?;
        let output = BufReader::new(child.stdout.take().context("app-server stdout missing")?);
        let mut client = Self {
            child: Some(child),
            input,
            output,
            frame: Vec::new(),
            lock: Some(lock),
            config,
            generation,
            next_id: 1,
            events: VecDeque::new(),
            approvals: BTreeMap::new(),
            tool_requests: BTreeMap::new(),
            thread_id: None,
            verified_resume,
            verified_message: None,
            poisoned: false,
        };
        client.rpc("initialize", json!({"clientInfo":{"name":"monday_research","version":"0.1.0"},"capabilities":{"experimentalApi":true}})).await?;
        client.write(&json!({"method":"initialized"})).await?;
        Ok(client)
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    async fn write(&mut self, value: &Value) -> Result<()> {
        ensure!(
            !self.poisoned,
            "interrupted transport requires restart/reconciliation"
        );
        let mut bytes = serde_json::to_vec(value)?;
        ensure!(bytes.len() <= FRAME_LIMIT, "RPC exceeds bound");
        bytes.push(b'\n');
        self.input.write_all(&bytes).await?;
        self.input.flush().await?;
        Ok(())
    }
    async fn read(&mut self) -> Result<Value> {
        // Keep the prefix in the transport across select!/timer cancellation.
        loop {
            let chunk = self.output.fill_buf().await?;
            ensure!(!chunk.is_empty(), "app-server EOF");
            let end = chunk.iter().position(|b| *b == b'\n');
            let n = end.map_or(chunk.len(), |i| i + 1);
            ensure!(
                self.frame.len() + n <= FRAME_LIMIT,
                "app-server oversized frame"
            );
            self.frame.extend_from_slice(&chunk[..n]);
            self.output.consume(n);
            if end.is_some() {
                let bytes = std::mem::take(&mut self.frame);
                let value: Value = serde_json::from_slice(&bytes)?;
                ensure!(value.is_object(), "invalid RPC frame");
                return Ok(value);
            }
        }
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).context("RPC ID exhausted")?;
        let operation = async {
            self.write(&json!({"id":id,"method":method,"params":params}))
                .await?;
            loop {
                let value = self.read().await?;
                if value.get("id") == Some(&json!(id)) && value.get("method").is_none() {
                    if value.get("error").is_some() {
                        bail!("app-server rejected RPC");
                    }
                    return value.get("result").cloned().context("missing RPC result");
                }
                self.register_event(&value)?;
                ensure!(
                    self.events.len() < 256,
                    "app-server event backlog exceeds bound"
                );
                self.events.push_back(value);
            }
        };
        match timeout(Duration::from_secs(30), operation).await {
            Ok(result) => {
                if result.is_err() {
                    self.poisoned = true;
                }
                result
            }
            Err(_) => {
                self.poisoned = true;
                bail!("app-server RPC timed out; delivery requires reconciliation");
            }
        }
    }

    fn register_event(&mut self, event: &Value) -> Result<()> {
        let Some(id) = event.get("id") else {
            return Ok(());
        };
        let kind = match event.get("method").and_then(Value::as_str) {
            Some("item/commandExecution/requestApproval") => ApprovalKind::Command,
            Some("item/fileChange/requestApproval") => ApprovalKind::FileChange,
            Some("item/tool/requestUserInput") => ApprovalKind::UserInput,
            Some("item/tool/call") => {
                research_call(
                    event,
                    self.thread_id()
                        .context("tool call before thread admission")?,
                )?;
                ensure!(
                    self.tool_requests.len() < 64
                        && self
                            .tool_requests
                            .insert(serde_json::to_string(id)?, event.clone())
                            .is_none(),
                    "duplicate or excessive tool request"
                );
                return Ok(());
            }
            _ => bail!("unsupported server request; requires explicit host handling"),
        };
        ensure!(
            event.pointer("/params/threadId").and_then(Value::as_str) == self.thread_id(),
            "approval belongs to another native thread"
        );
        ensure!(self.approvals.len() < 64, "approval backlog exceeds bound");
        let pending = PendingApproval {
            process_generation: self.generation.clone(),
            original_rpc_id: serde_json::from_value(id.clone())?,
            kind,
        };
        ensure!(
            self.approvals
                .insert(serde_json::to_string(id)?, pending)
                .is_none(),
            "duplicate server request ID"
        );
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<Value> {
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        let event = self.read().await?;
        self.register_event(&event)?;
        Ok(event)
    }
    pub async fn approve(
        &mut self,
        generation: &str,
        id: &RpcId,
        kind: ApprovalKind,
        response: Value,
    ) -> Result<()> {
        let key = serde_json::to_string(id)?;
        let pending = self
            .approvals
            .get(&key)
            .context("unknown approval request")?;
        let reply = pending.reply(generation, id, kind, response)?;
        self.write(&reply).await?;
        self.approvals.remove(&key);
        Ok(())
    }

    pub async fn open_thread(&mut self, resume: Option<&str>) -> Result<String> {
        ensure!(self.thread_id.is_none(), "transport already owns a thread");
        ensure!(
            resume == self.verified_resume.as_deref(),
            "resume requires stopped native state readback"
        );
        let (method, mut params) = (
            "thread/start",
            json!({"cwd":self.config.workspace,"approvalPolicy":"untrusted","sandbox":"read-only","ephemeral":false,"experimentalRawEvents":false,"environments":[],"dynamicTools":research_tools()}),
        );
        let method = if let Some(thread) = resume {
            params
                .as_object_mut()
                .context("thread parameters")?
                .remove("dynamicTools");
            params
                .as_object_mut()
                .context("thread parameters")?
                .remove("ephemeral");
            params
                .as_object_mut()
                .context("thread parameters")?
                .remove("experimentalRawEvents");
            params
                .as_object_mut()
                .context("thread parameters")?
                .remove("environments");
            params["threadId"] = json!(thread);
            "thread/resume"
        } else {
            method
        };
        let response = self.rpc(method, params).await?;
        let thread = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .context("native thread ID missing")?
            .to_owned();
        if let Some(expected) = resume {
            ensure!(thread == expected, "provider resumed another thread");
        }
        ensure!(
            response.get("approvalPolicy").and_then(Value::as_str) == Some("untrusted")
                && response.pointer("/sandbox/type").and_then(Value::as_str) == Some("readOnly")
                && response
                    .pointer("/sandbox/networkAccess")
                    .and_then(Value::as_bool)
                    .is_none_or(|v| !v)
                && response.get("cwd").and_then(Value::as_str) == self.config.workspace.to_str(),
            "provider did not enforce Session policy"
        );
        ensure!(
            !thread.is_empty() && thread.len() <= 256,
            "invalid native thread ID"
        );
        self.thread_id = Some(thread.clone());
        Ok(thread)
    }

    fn delivery_path(&self, intent: &str) -> Result<PathBuf> {
        ensure!(valid_digest(intent), "invalid delivery identity");
        Ok(self
            .config
            .delivery_directory
            .join(format!("{intent}.json")))
    }
    /// Intent is stable across host/process restarts. Persist Unknown before
    /// crossing the provider seam; response loss never authorizes blind resend.
    pub async fn send_message(&mut self, intent: &str, text: &str) -> Result<MessageDelivery> {
        ensure!(
            !text.is_empty() && text.len() <= 64 * 1024,
            "message exceeds bound"
        );
        let thread = self.thread_id.clone().context("thread not opened")?;
        let path = self.delivery_path(intent)?;
        let mut record = MessageDelivery {
            schema: 1,
            intent_sha256: intent.into(),
            thread_id: thread.clone(),
            payload_sha256: sha256(text.as_bytes()),
            process_generation: self.generation.clone(),
            delivery: Delivery::Unknown,
            turn_id: None,
            native_readback_sha256: None,
        };
        if path.exists() {
            let prior: MessageDelivery = read_json(&path)?;
            ensure!(
                prior.schema == 1
                    && prior.intent_sha256 == intent
                    && prior.thread_id == thread
                    && prior.payload_sha256 == record.payload_sha256,
                "delivery identity reused for another message"
            );
            if prior.delivery == Delivery::Accepted {
                return Ok(prior);
            }
            ensure!(
                prior.delivery.may_resubmit(),
                "unknown delivery requires provider readback"
            );
        }
        durable_json(&path, &record)?;
        let result = self.rpc("turn/start", json!({"threadId":thread,"clientUserMessageId":intent,"input":[{"type":"text","text":text,"text_elements":[]}],"approvalPolicy":"untrusted","sandboxPolicy":{"type":"readOnly","networkAccess":false},"environments":[]})).await?;
        record.turn_id = Some(
            result
                .pointer("/turn/id")
                .and_then(Value::as_str)
                .context("turn ID missing")?
                .into(),
        );
        record.delivery = Delivery::Accepted;
        durable_json(&path, &record)?;
        Ok(record)
    }
    pub async fn reconcile_message(&mut self, intent: &str) -> Result<MessageDelivery> {
        let path = self.delivery_path(intent)?;
        let mut record: MessageDelivery = read_json(&path)?;
        record.native_readback_sha256 = None;
        self.verified_message = None;
        ensure!(
            record.schema == 1
                && record.intent_sha256 == intent
                && Some(record.thread_id.as_str()) == self.thread_id(),
            "foreign delivery readback"
        );
        let metadata = self
            .rpc(
                "thread/read",
                json!({"threadId":record.thread_id,"includeTurns":false}),
            )
            .await?;
        ensure!(
            metadata.pointer("/thread/id").and_then(Value::as_str) == Some(&record.thread_id),
            "foreign thread readback"
        );
        let mut cursor: Option<String> = None;
        for _ in 0..32 {
            let response = self.rpc("thread/items/list", json!({"threadId":record.thread_id,"turnId":record.turn_id,"cursor":cursor,"sortDirection":"desc","limit":16})).await?;
            let entries = response
                .get("data")
                .and_then(Value::as_array)
                .context("native item page missing")?;
            ensure!(entries.len() <= 16, "native item page exceeds bound");
            for entry in entries {
                let item = entry.get("item").context("native message missing")?;
                if item.get("type").and_then(Value::as_str) == Some("userMessage")
                    && item.get("clientId").and_then(Value::as_str) == Some(intent)
                {
                    let content = item
                        .get("content")
                        .and_then(Value::as_array)
                        .context("native user message missing content")?;
                    ensure!(
                        content.len() == 1
                            && content[0].get("type").and_then(Value::as_str) == Some("text")
                            && content[0]
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(
                                    |text| sha256(text.as_bytes()) == record.payload_sha256
                                ),
                        "native message identity reused for another payload"
                    );
                    let turn = entry
                        .get("turnId")
                        .and_then(Value::as_str)
                        .context("reconciled turn missing ID")?;
                    ensure!(
                        record.turn_id.as_deref().is_none_or(|id| id == turn),
                        "native message moved to another turn"
                    );
                    record.turn_id = Some(turn.into());
                    record.delivery = Delivery::Accepted;
                    record.native_readback_sha256 = Some(identity(entry)?);
                    self.verified_message = Some(
                        content[0]["text"]
                            .as_str()
                            .context("native text missing")?
                            .into(),
                    );
                    durable_json(&path, &record)?;
                    return Ok(record);
                }
            }
            let next = response
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if next.is_none() {
                break;
            }
            ensure!(
                next != cursor && next.as_ref().is_some_and(|s| s.len() <= 4096),
                "invalid native item cursor"
            );
            cursor = next;
        }
        // Absence from bounded pages never proves rejection or permits resend.
        durable_json(&path, &record)?;
        Ok(record)
    }
    pub async fn verified_delivery(&mut self, intent: &str) -> Result<VerifiedDelivery> {
        let mut record = self.reconcile_message(intent).await?;
        ensure!(
            record.delivery == Delivery::Accepted
                && record
                    .native_readback_sha256
                    .as_ref()
                    .is_some_and(|s| valid_digest(s)),
            "native delivery is not yet visible"
        );
        record.process_generation = self.generation.clone();
        Ok(VerifiedDelivery {
            record,
            provider_binary_sha256: self.config.executable_sha256.clone(),
            message: self
                .verified_message
                .take()
                .context("native message readback missing")?,
        })
    }
    /// Session interruption never changes a scientific Job or its budget.
    pub async fn interrupt(&mut self, turn: &str) -> Result<()> {
        let thread = self.thread_id.clone().context("thread not opened")?;
        ensure!(
            !turn.is_empty() && turn.len() <= 256,
            "invalid turn identity"
        );
        self.rpc("turn/interrupt", json!({"threadId":thread,"turnId":turn}))
            .await?;
        Ok(())
    }
    pub async fn answer_tool(&mut self, event: &Value, client: &ResearchClient) -> Result<()> {
        let id = event.get("id").context("tool request ID missing")?;
        let key = serde_json::to_string(id)?;
        ensure!(
            self.tool_requests.get(&key) == Some(event),
            "unknown or changed tool request"
        );
        let tool = research_call(event, self.thread_id().context("thread not opened")?)?;
        let (success, text) = match client.execute(&tool).await {
            Ok(result) => (true, serde_json::to_string(&result)?),
            Err(_) => (false, "research request unavailable or rejected; reconcile submit using its existing idempotency key".into()),
        };
        self.write(&json!({"id":id,"result":{"success":success,"contentItems":[{"type":"inputText","text":text}]}})).await?;
        self.tool_requests.remove(&key);
        Ok(())
    }
    async fn stop_child(&mut self) -> Result<()> {
        let child = self.child.as_mut().context("child already closed")?;
        child.start_kill()?;
        child.wait().await?;
        self.child.take();
        Ok(())
    }
    /// A persistent-volume checkpoint. Copy/archive these listed bytes only
    /// through the admitted artifact gateway; do not archive CODEX_HOME wholesale.
    pub async fn checkpoint(mut self) -> Result<NativeState> {
        let thread = self.thread_id.clone().context("thread not opened")?;
        self.stop_child().await?;
        fn collect(root: &Path, path: &Path, entries: &mut Vec<NativeFile>) -> Result<()> {
            ensure!(
                entries.len() < 2048,
                "native state file count exceeds bound"
            );
            let metadata = std::fs::symlink_metadata(path)?;
            ensure!(!metadata.is_symlink(), "native state contains a symlink");
            if metadata.is_dir() {
                for entry in std::fs::read_dir(path)? {
                    collect(root, &entry?.path(), entries)?;
                }
            } else {
                let relative = path
                    .strip_prefix(root)?
                    .to_str()
                    .context("invalid native path")?;
                if native_path(relative) {
                    let (sha256, bytes) = file_digest(path)?;
                    entries.push(NativeFile {
                        path: relative.into(),
                        sha256,
                        bytes,
                    });
                }
            }
            Ok(())
        }
        let mut files = Vec::new();
        let sessions = self.config.native_home.join("sessions");
        if sessions.exists() {
            collect(&self.config.native_home, &sessions, &mut files)?;
        }
        for entry in std::fs::read_dir(&self.config.native_home)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .context("native file name")?
                .to_str()
                .context("invalid native name")?;
            if native_path(name) {
                collect(&self.config.native_home, &path, &mut files)?;
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let native = NativeState {
            schema: 1,
            provider_binary_sha256: self.config.executable_sha256.clone(),
            thread_id: thread,
            files,
        };
        // File bytes were hashed during capture under this stopped-writer lock.
        // Validate the assembled manifest without rereading those same bytes.
        native.validate(&self.config.executable_sha256, &native.thread_id)?;
        Ok(native)
    }
    pub async fn close(mut self) -> Result<()> {
        self.stop_child().await?;
        self.lock.take();
        Ok(())
    }
}

/// Host-side capability client. The child receives neither this token nor PG,
/// Kubernetes or publisher credentials. Remote deployments require HTTPS.
pub struct ResearchClient {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    token: ClientToken,
}
enum ClientToken {
    Inline(String),
    File(PathBuf),
}
impl ClientToken {
    fn read(&self) -> Result<String> {
        let token = match self {
            Self::Inline(token) => token.clone(),
            Self::File(path) => String::from_utf8(crate::transport::read_private_file(path)?)?
                .trim()
                .to_owned(),
        };
        ensure!(
            (32..=4096).contains(&token.len()),
            "invalid capability token"
        );
        Ok(token)
    }
}
impl ResearchClient {
    pub fn new(endpoint: &str, token: String) -> Result<Self> {
        Self::connect(
            endpoint,
            ClientToken::Inline(token),
            &crate::transport::TlsConfig::default(),
        )
    }
    /// Reload the broker's host-only token on each call, including renewal.
    pub fn from_file(
        endpoint: &str,
        token_file: PathBuf,
        tls: &crate::transport::TlsConfig,
    ) -> Result<Self> {
        ensure!(
            token_file.is_absolute(),
            "absolute host token path required"
        );
        Self::connect(endpoint, ClientToken::File(token_file), tls)
    }
    fn connect(
        endpoint: &str,
        token: ClientToken,
        tls: &crate::transport::TlsConfig,
    ) -> Result<Self> {
        let endpoint = reqwest::Url::parse(endpoint)?;
        ensure!(
            (endpoint.scheme() == "https"
                || (endpoint.scheme() == "http"
                    && matches!(endpoint.host_str(), Some("127.0.0.1" | "::1"))))
                && endpoint.host_str().is_some()
                && endpoint.path() == "/research"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none()
                && endpoint.username().is_empty()
                && endpoint.password().is_none(),
            "invalid research capability endpoint"
        );
        token.read()?;
        Ok(Self {
            client: tls.client(Duration::from_secs(15), endpoint.scheme() == "https")?,
            endpoint,
            token,
        })
    }
    pub async fn execute(&self, tool: &crate::research::ResearchTool) -> Result<Value> {
        tool.validate()?;
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(self.token.read()?)
            .json(tool)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("research capability unavailable"))?;
        ensure!(
            response.status().is_success(),
            "research capability rejected request"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!("research capability interrupted"))?
        {
            ensure!(
                bytes.len() + chunk.len() <= FRAME_LIMIT,
                "research response exceeds bound"
            );
            bytes.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}

fn research_tools() -> Value {
    json!([{"type":"namespace","name":"research","description":"Governed research requests and readback only","tools":[
        {"name":"submit","description":"Submit an already approved immutable request","inputSchema":{"type":"object","properties":{"request_sha256":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["request_sha256","idempotency_key"],"additionalProperties":false}},
        {"name":"status","description":"Read a governed Run","inputSchema":{"type":"object","properties":{"run_sha256":{"type":"string"}},"required":["run_sha256"],"additionalProperties":false}},
        {"name":"artifacts","description":"Read verified result references","inputSchema":{"type":"object","properties":{"run_sha256":{"type":"string"}},"required":["run_sha256"],"additionalProperties":false}}
    ]}])
}

/// Only a validated research namespace call can reach the capability client.
pub fn research_call(event: &Value, thread: &str) -> Result<crate::research::ResearchTool> {
    ensure!(
        event.get("method").and_then(Value::as_str) == Some("item/tool/call")
            && event.get("id").is_some(),
        "not a dynamic tool request"
    );
    let params = event.get("params").context("tool parameters missing")?;
    ensure!(
        params.get("threadId").and_then(Value::as_str) == Some(thread)
            && params.get("namespace").and_then(Value::as_str) == Some("research"),
        "foreign tool namespace/thread"
    );
    let name = params
        .get("tool")
        .and_then(Value::as_str)
        .context("tool name missing")?;
    ensure!(
        matches!(name, "submit" | "status" | "artifacts"),
        "unadmitted research tool"
    );
    let mut arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .context("tool arguments missing")?
        .clone();
    ensure!(
        !arguments.contains_key("method"),
        "tool cannot override admitted method"
    );
    arguments.insert("method".into(), json!(format!("research.{name}")));
    let tool: crate::research::ResearchTool = serde_json::from_value(Value::Object(arguments))?;
    tool.validate()?;
    Ok(tool)
}
