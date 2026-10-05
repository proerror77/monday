//! Immutable persistent objects behind operator-issued, short-lived capabilities.
//! TLS termination belongs to the private ingress; the storage listener is loopback.
use crate::{postgres::Ledger, sha256, valid_digest};
use anyhow::{ensure, Context, Result};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::StreamExt;
use rustix::fs::{linkat, mkdirat, openat, unlinkat, AtFlags, Mode, OFlags};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::Read,
    net::SocketAddr,
    os::fd::{AsFd, OwnedFd},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

const MAX_OBJECT: u64 = 512 * 1024 * 1024;
static UPLOAD: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub bind: String,
    pub root: PathBuf,
    /// Private, atomically replaced broker projection. No issuance endpoint.
    pub capabilities_file: PathBuf,
    pub max_object_bytes: u64,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub token_sha256: String,
    pub expires_ms: u64,
    pub access: Access,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum Access {
    Reader {
        prefixes: Vec<String>,
    },
    AttemptWriter {
        tenant: String,
        task_id: String,
        attempt: u32,
        fence: i64,
    },
    /// Existing trusted publisher only; cannot write scientific outputs.
    Publisher {
        prefixes: Vec<String>,
    },
}

fn key_valid(key: &str) -> bool {
    key.starts_with("research/")
        && key.len() <= 2048
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        && key.split('/').count() <= 32
        && key
            .split('/')
            .all(|p| !p.is_empty() && p.len() <= 255 && !p.starts_with('.'))
}
fn prefix_valid(prefix: &str) -> bool {
    prefix.strip_suffix('/').is_some_and(key_valid)
}
fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn read_capabilities(path: &std::path::Path) -> Result<Vec<Capability>> {
    // The broker owns this file and its private parent, never the Agent.
    // O_NOFOLLOW excludes a substituted symlink even during atomic reload.
    let parent = path.parent().context("broker parent required")?;
    ensure!(
        parent.canonicalize()? == parent,
        "broker parent must be canonical"
    );
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = File::from(fd);
    let meta = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0
                && parent.metadata()?.permissions().mode() & 0o077 == 0,
            "capability file must be private"
        );
    }
    ensure!(
        meta.is_file() && meta.len() <= 1024 * 1024,
        "invalid broker projection"
    );
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "broker projection exceeds bound"
    );
    let caps: Vec<Capability> = serde_json::from_slice(&bytes)?;
    ensure!(caps.len() <= 1024, "invalid capability count");
    let mut seen = std::collections::BTreeSet::new();
    for cap in &caps {
        ensure!(
            valid_digest(&cap.token_sha256) && seen.insert(&cap.token_sha256),
            "invalid or duplicate capability"
        );
        match &cap.access {
            Access::Reader { prefixes } | Access::Publisher { prefixes } => {
                ensure!(
                    !prefixes.is_empty()
                        && prefixes.len() <= 256
                        && prefixes.iter().all(|p| prefix_valid(p)),
                    "invalid object scope"
                );
                if matches!(cap.access, Access::Publisher { .. }) {
                    ensure!(prefixes.iter().all(|p| {
                        let parts: Vec<_> = p.trim_end_matches('/').split('/').collect();
                        matches!(parts.as_slice(), ["research", "builds", id] if valid_digest(id))
                            || matches!(parts.as_slice(), ["research", "sources", commit] if commit.len() == 40 && commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                    }), "publisher cannot write outside an exact Build/source");
                }
            }
            Access::AttemptWriter {
                tenant,
                task_id,
                attempt,
                fence,
            } => ensure!(
                !tenant.is_empty()
                    && tenant.len() <= 128
                    && valid_digest(task_id)
                    && *attempt > 0
                    && *fence > 0,
                "invalid Attempt capability"
            ),
        }
    }
    Ok(caps)
}

pub struct Gateway {
    config: GatewayConfig,
    ledger: Ledger,
    root: File,
    _lock: File,
    uploads: tokio::sync::Mutex<()>,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        // Release the open-description lock before close. A concurrent unrelated
        // fork can briefly hold a duplicate until exec applies CLOEXEC.
        let _ = self._lock.unlock();
    }
}

impl Gateway {
    pub fn new(config: GatewayConfig, ledger: Ledger) -> Result<Self> {
        let bind: SocketAddr = config.bind.parse()?;
        ensure!(
            bind.ip().is_loopback() && bind.port() > 0,
            "gateway requires private loopback TLS ingress"
        );
        ensure!(
            config.root.is_absolute()
                && config.capabilities_file.is_absolute()
                && !config.capabilities_file.starts_with(&config.root)
                && config.max_object_bytes > 0
                && config.max_object_bytes <= MAX_OBJECT,
            "invalid gateway storage/configuration"
        );
        std::fs::create_dir_all(&config.root)?;
        ensure!(
            config.root.canonicalize()? == config.root,
            "storage root must be canonical"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config.root, std::fs::Permissions::from_mode(0o700))?;
        }
        let root = File::from(rustix::fs::open(
            &config.root,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )?);
        let lock_fd = openat(
            &root,
            ".gateway.lock",
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::CREATE | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )?;
        let lock = File::from(lock_fd);
        lock.try_lock()
            .context("persistent artifact store already has a writer")?;
        read_capabilities(&config.capabilities_file)?;
        Ok(Self {
            config,
            ledger,
            root,
            _lock: lock,
            uploads: tokio::sync::Mutex::new(()),
        })
    }
    fn capability(&self, headers: &HeaderMap) -> Result<Capability> {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .context("capability required")?;
        ensure!(
            (32..=4096).contains(&token.len()),
            "invalid capability token"
        );
        let token_sha = sha256(token.as_bytes());
        let cap = read_capabilities(&self.config.capabilities_file)?
            .into_iter()
            .find(|c| c.token_sha256 == token_sha)
            .context("unknown capability")?;
        let now = now_ms()?;
        ensure!(
            cap.expires_ms > now && cap.expires_ms - now <= 24 * 60 * 60 * 1000,
            "expired/unbounded capability"
        );
        Ok(cap)
    }
    async fn admits(&self, cap: &Capability, key: &str, write: bool) -> Result<()> {
        ensure!(key_valid(key), "unsafe object key");
        match &cap.access {
            Access::Reader { prefixes } => ensure!(
                !write && prefixes.iter().any(|p| key.starts_with(p)),
                "readonly/foreign object"
            ),
            Access::Publisher { prefixes } => ensure!(
                prefixes.iter().any(|p| key.starts_with(p)),
                "foreign publication"
            ),
            Access::AttemptWriter {
                tenant,
                task_id,
                attempt,
                fence,
            } => {
                let prefix = self
                    .ledger
                    .artifact_writer(tenant, task_id, *attempt, *fence)
                    .await?;
                ensure!(key.starts_with(&prefix), "foreign Attempt object");
            }
        }
        Ok(())
    }
    fn parent(&self, key: &str, create: bool) -> Result<(OwnedFd, String)> {
        ensure!(key_valid(key), "unsafe object key");
        let mut parts: Vec<_> = key.split('/').collect();
        let name = parts.pop().context("object name missing")?.to_owned();
        let mut fd: OwnedFd = self.root.try_clone()?.into();
        for part in parts {
            if create {
                match mkdirat(&fd, part, Mode::from_raw_mode(0o700)) {
                    Ok(()) => {}
                    Err(rustix::io::Errno::EXIST) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            // Every component opens relative to the already held directory.
            fd = openat(
                &fd,
                part,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
                Mode::empty(),
            )?;
        }
        Ok((fd, name))
    }
    fn read_object(&self, key: &str) -> Result<File> {
        let (parent, name) = self.parent(key, false)?;
        let file = File::from(openat(
            parent.as_fd(),
            name.as_str(),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )?);
        let meta = file.metadata()?;
        ensure!(
            meta.is_file() && meta.len() > 0 && meta.len() <= self.config.max_object_bytes,
            "invalid object file"
        );
        Ok(file)
    }
    fn begin_upload(&self, key: &str) -> Result<Upload> {
        let (parent, name) = self.parent(key, true)?;
        let temporary = format!(
            ".gateway-{}-{}-{}.partial",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            UPLOAD.fetch_add(1, Ordering::Relaxed)
        );
        let fd = openat(
            &parent,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CLOEXEC | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
            Mode::from_raw_mode(0o600),
        )?;
        Ok(Upload {
            parent,
            name,
            temporary,
            file: tokio::fs::File::from_std(File::from(fd)),
        })
    }
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/research/*key", get(read).put(write))
            .with_state(self)
    }
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let listener = tokio::net::TcpListener::bind(&self.config.bind).await?;
        axum::serve(listener, self.router())
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    }
}

struct Upload {
    parent: OwnedFd,
    name: String,
    temporary: String,
    file: tokio::fs::File,
}
impl Upload {
    fn publish(&mut self) -> Result<()> {
        // Link publishes the complete file once, atomically. Existing bytes win.
        linkat(
            &self.parent,
            self.temporary.as_str(),
            &self.parent,
            self.name.as_str(),
            AtFlags::empty(),
        )?;
        rustix::fs::fsync(&self.parent)?;
        Ok(())
    }
}
impl Drop for Upload {
    fn drop(&mut self) {
        let _ = unlinkat(&self.parent, self.temporary.as_str(), AtFlags::empty());
    }
}

async fn read(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let key = format!("research/{key}");
    let cap = gateway
        .capability(&headers)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    gateway
        .admits(&cap, &key, false)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let file = match gateway.read_object(&key) {
        Ok(file) => file,
        Err(error)
            if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::NOENT) =>
        {
            return Err(StatusCode::NOT_FOUND)
        }
        Err(_) => return Err(StatusCode::FORBIDDEN),
    };
    let size = file
        .metadata()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();
    let body = Body::from_stream(ReaderStream::new(tokio::fs::File::from_std(file)));
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        body,
    )
        .into_response())
}
async fn write(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, StatusCode> {
    ensure_new_object(&headers)?;
    let key = format!("research/{key}");
    let cap = gateway
        .capability(&headers)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    gateway
        .admits(&cap, &key, true)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    // One active upload bounds disk/memory pressure. Busy callers retain their
    // object identity and can retry; no hidden queue accumulates request bodies.
    let _guard = gateway
        .uploads
        .try_lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut upload = gateway
        .begin_upload(&key)
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let upload_body = async {
        let mut stream = body.into_data_stream();
        let mut size = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| StatusCode::BAD_REQUEST)?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(StatusCode::PAYLOAD_TOO_LARGE)?;
            if size > gateway.config.max_object_bytes {
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            upload
                .file
                .write_all(&chunk)
                .await
                .map_err(|_| StatusCode::INSUFFICIENT_STORAGE)?;
        }
        if size == 0 {
            return Err(StatusCode::BAD_REQUEST);
        }
        upload
            .file
            .flush()
            .await
            .map_err(|_| StatusCode::INSUFFICIENT_STORAGE)?;
        upload
            .file
            .sync_all()
            .await
            .map_err(|_| StatusCode::INSUFFICIENT_STORAGE)?;
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(30), upload_body)
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)??;
    // Re-read broker revocation and live PG identity after receiving bytes.
    let current = gateway
        .capability(&headers)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if current != cap {
        return Err(StatusCode::UNAUTHORIZED);
    }
    gateway
        .admits(&current, &key, true)
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    let _permit = if let Access::AttemptWriter {
        tenant,
        task_id,
        attempt,
        fence,
    } = &current.access
    {
        let permit = gateway
            .ledger
            .artifact_write_permit(tenant, task_id, *attempt, *fence)
            .await
            .map_err(|_| StatusCode::FORBIDDEN)?;
        if !key.starts_with(&permit.prefix) {
            return Err(StatusCode::FORBIDDEN);
        }
        Some(permit)
    } else {
        None
    };
    upload.publish().map_err(|error| {
        if error.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::EXIST) {
            StatusCode::PRECONDITION_FAILED
        } else {
            StatusCode::INSUFFICIENT_STORAGE
        }
    })?;
    Ok(StatusCode::CREATED)
}
fn ensure_new_object(headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        != Some("*")
    {
        return Err(StatusCode::PRECONDITION_REQUIRED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write_caps(path: &std::path::Path, caps: &[Capability]) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, serde_json::to_vec(caps)?)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
    fn fixture(max: u64) -> Result<(tempfile::TempDir, Arc<Gateway>, String, String)> {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        let token = "publisher-fixture".repeat(4);
        let prefix = format!("research/builds/{}/", "a".repeat(64));
        let cap = Capability {
            token_sha256: sha256(token.as_bytes()),
            expires_ms: now_ms()? + 60_000,
            access: Access::Publisher {
                prefixes: vec![prefix.clone()],
            },
        };
        let path = root.join("capabilities.json");
        write_caps(&path, &[cap])?;
        let gateway = Gateway::new(
            GatewayConfig {
                bind: "127.0.0.1:12345".into(),
                root: root.join("store"),
                capabilities_file: path,
                max_object_bytes: max,
            },
            Ledger::gateway_fixture(),
        )?;
        Ok((temp, Arc::new(gateway), token, prefix))
    }
    #[tokio::test]
    async fn broker_projection_rejects_public_parent_and_fifo_without_waiting() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, gateway, _token, _prefix) = fixture(128)?;
        let path = &gateway.config.capabilities_file;
        let parent = path.parent().unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755))?;
        assert!(read_capabilities(path).is_err());
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(path, b"[]")?;
        assert!(read_capabilities(path)?.is_empty());
        std::fs::remove_file(path)?;
        ensure!(
            std::process::Command::new("mkfifo")
                .arg(path)
                .status()?
                .success(),
            "FIFO fixture creation failed"
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        assert!(read_capabilities(path).is_err());
        Ok(())
    }
    struct Server {
        stop: tokio::sync::oneshot::Sender<()>,
        handle: tokio::task::JoinHandle<()>,
    }
    impl Server {
        async fn close(self) {
            let _ = self.stop.send(());
            self.handle.await.unwrap();
        }
    }
    async fn server(gateway: Arc<Gateway>) -> Result<(String, Server)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/", listener.local_addr()?);
        let (stop, receiver) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            axum::serve(listener, gateway.router())
                .with_graceful_shutdown(async {
                    let _ = receiver.await;
                })
                .await
                .unwrap();
        });
        Ok((base, Server { stop, handle }))
    }
    #[tokio::test]
    async fn immutable_put_readback_and_restart_preserve_exact_bytes() -> Result<()> {
        let (_temp, gateway, token, prefix) = fixture(128)?;
        let config = gateway.config.clone();
        let (base, handle) = server(gateway.clone()).await?;
        let client = reqwest::Client::new();
        let url = format!("{base}{prefix}model.bin");
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&token)
                .body("model bytes")
                .send()
                .await?
                .status(),
            StatusCode::PRECONDITION_REQUIRED
        );
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&token)
                .header("If-None-Match", "*")
                .body("model bytes")
                .send()
                .await?
                .status(),
            StatusCode::CREATED
        );
        assert_eq!(
            client
                .put(&url)
                .bearer_auth(&token)
                .header("If-None-Match", "*")
                .body("changed bytes")
                .send()
                .await?
                .status(),
            StatusCode::PRECONDITION_FAILED
        );
        assert_eq!(
            client
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await?
                .bytes()
                .await?,
            "model bytes"
        );
        assert!(Gateway::new(config.clone(), Ledger::gateway_fixture()).is_err());
        handle.close().await;
        drop(gateway);
        let restored = Gateway::new(config, Ledger::gateway_fixture())?;
        let mut bytes = Vec::new();
        restored
            .read_object(&format!("{prefix}model.bin"))?
            .read_to_end(&mut bytes)?;
        assert_eq!(bytes, b"model bytes");
        Ok(())
    }
    #[tokio::test]
    async fn an_unrelated_child_cannot_inherit_the_storage_lock() -> Result<()> {
        let (_temp, gateway, _token, _prefix) = fixture(128)?;
        let config = gateway.config.clone();
        let mut child = std::process::Command::new("/bin/sleep").arg("2").spawn()?;
        drop(gateway);
        let restart = Gateway::new(config, Ledger::gateway_fixture());
        let _ = child.kill();
        child.wait()?;
        restart?;
        Ok(())
    }
    #[tokio::test]
    async fn scoped_tokens_and_size_limit_reject_without_visible_partial_objects() -> Result<()> {
        let (_temp, gateway, token, prefix) = fixture(8)?;
        let (base, handle) = server(gateway.clone()).await?;
        let client = reqwest::Client::new();
        for (key, body, expected) in [
            (
                format!("{prefix}large.bin"),
                "exceeds limit",
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            (
                format!("research/builds/{}/foreign.bin", "b".repeat(64)),
                "ok",
                StatusCode::FORBIDDEN,
            ),
        ] {
            assert_eq!(
                client
                    .put(format!("{base}{key}"))
                    .bearer_auth(&token)
                    .header("If-None-Match", "*")
                    .body(body)
                    .send()
                    .await?
                    .status(),
                expected
            );
        }
        assert!(gateway.read_object(&format!("{prefix}large.bin")).is_err());
        assert_eq!(
            client
                .get(format!("{base}{prefix}missing.bin"))
                .bearer_auth("wrong".repeat(32))
                .send()
                .await?
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let reader = Capability {
            token_sha256: sha256(token.as_bytes()),
            expires_ms: now_ms()? + 60_000,
            access: Access::Reader {
                prefixes: vec![prefix.clone()],
            },
        };
        write_caps(&gateway.config.capabilities_file, &[reader])?;
        assert_eq!(
            client
                .put(format!("{base}{prefix}model.bin"))
                .bearer_auth(&token)
                .header("If-None-Match", "*")
                .body("ok")
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        handle.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn revoked_capability_during_upload_cannot_publish() -> Result<()> {
        let (_temp, gateway, token, prefix) = fixture(128)?;
        let (base, handle) = server(gateway.clone()).await?;
        let caps = gateway.config.capabilities_file.clone();
        let token_for_body = token.clone();
        let body = futures_util::stream::unfold(0, move |state| {
            let caps = caps.clone();
            let token = token_for_body.clone();
            async move {
                match state {
                    0 => Some((Ok::<_, std::io::Error>("partial"), 1)),
                    1 => {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let expired = Capability {
                            token_sha256: sha256(token.as_bytes()),
                            expires_ms: 0,
                            access: Access::Reader {
                                prefixes: vec!["research/builds/".into()],
                            },
                        };
                        write_caps(&caps, &[expired]).unwrap();
                        Some((Ok("remaining"), 2))
                    }
                    _ => None,
                }
            }
        });
        let response = reqwest::Client::new()
            .put(format!("{base}{prefix}model.bin"))
            .bearer_auth(token)
            .header("If-None-Match", "*")
            .body(reqwest::Body::wrap_stream(body))
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(gateway.read_object(&format!("{prefix}model.bin")).is_err());
        let (parent, _) = gateway.parent(&format!("{prefix}model.bin"), false)?;
        let _ = parent;
        for entry in std::fs::read_dir(gateway.config.root.join(prefix.trim_end_matches('/')))? {
            assert!(!entry?.file_name().to_string_lossy().ends_with(".partial"));
        }
        handle.close().await;
        Ok(())
    }
    #[tokio::test]
    async fn filesystem_aliases_and_internal_temporary_names_are_inaccessible() -> Result<()> {
        let (temp, gateway, _token, prefix) = fixture(128)?;
        let external = temp.path().join("outside");
        std::fs::create_dir(&external)?;
        std::fs::write(external.join("secret"), "secret")?;
        std::os::unix::fs::symlink(&external, gateway.config.root.join("research"))?;
        assert!(gateway.begin_upload(&format!("{prefix}model.bin")).is_err());
        assert!(gateway.read_object("research/secret").is_err());
        for key in [
            "research/../secret",
            "research//secret",
            "research/.gateway-1.partial",
            "research/a\\secret",
        ] {
            assert!(gateway.read_object(key).is_err());
            assert!(gateway.begin_upload(key).is_err());
        }
        assert_eq!(std::fs::read(external.join("secret"))?, b"secret");
        Ok(())
    }
}
