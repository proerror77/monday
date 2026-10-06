//! A bounded, broker-scoped interface for an existing Coding Agent. The Agent
//! receives neither PG credentials nor an issuance, shell or administration RPC.
use crate::{postgres::Ledger, research::ResearchTool, sha256, valid_digest};
use anyhow::{ensure, Context, Result};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentApiConfig {
    pub bind: String,
    /// Broker-owned, private, atomically replaced projection. Empty means denied.
    pub capabilities_file: PathBuf,
}
impl AgentApiConfig {
    pub fn validate(&self) -> Result<SocketAddr> {
        let bind: SocketAddr = self.bind.parse()?;
        ensure!(
            bind.ip().is_loopback() && bind.port() > 0 && self.capabilities_file.is_absolute(),
            "research tool API requires loopback and a private broker projection"
        );
        Ok(bind)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolCapability {
    pub token_sha256: String,
    pub tenant: String,
    pub not_before_ms: u64,
    pub expires_ms: u64,
    pub permissions: Vec<ToolPermission>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum ToolPermission {
    #[serde(rename = "research.submit")]
    Submit {
        request_sha256: String,
        idempotency_key: String,
    },
    #[serde(rename = "research.status")]
    Status { run_sha256: String },
    #[serde(rename = "research.artifacts")]
    Artifacts { run_sha256: String },
}
impl ToolCapability {
    fn validate(&self) -> Result<()> {
        ensure!(
            valid_digest(&self.token_sha256)
                && !self.tenant.is_empty()
                && self.tenant.len() <= 128
                && self.expires_ms > self.not_before_ms
                && self.expires_ms - self.not_before_ms <= 60 * 60 * 1000
                && !self.permissions.is_empty()
                && self.permissions.len() <= 256,
            "invalid/unbounded tool capability"
        );
        for permission in &self.permissions {
            let tool = match permission {
                ToolPermission::Submit {
                    request_sha256,
                    idempotency_key,
                } => ResearchTool::Submit {
                    request_sha256: request_sha256.clone(),
                    idempotency_key: idempotency_key.clone(),
                },
                ToolPermission::Status { run_sha256 } => ResearchTool::Status {
                    run_sha256: run_sha256.clone(),
                },
                ToolPermission::Artifacts { run_sha256 } => ResearchTool::Artifacts {
                    run_sha256: run_sha256.clone(),
                },
            };
            tool.validate()?;
        }
        Ok(())
    }
    fn admits(&self, tool: &ResearchTool, now: u64) -> bool {
        now >= self.not_before_ms
            && now < self.expires_ms
            && self
                .permissions
                .iter()
                .any(|permission| match (permission, tool) {
                    (
                        ToolPermission::Submit {
                            request_sha256: allowed,
                            idempotency_key: key,
                        },
                        ResearchTool::Submit {
                            request_sha256,
                            idempotency_key,
                        },
                    ) => allowed == request_sha256 && key == idempotency_key,
                    (
                        ToolPermission::Status {
                            run_sha256: allowed,
                        },
                        ResearchTool::Status { run_sha256 },
                    )
                    | (
                        ToolPermission::Artifacts {
                            run_sha256: allowed,
                        },
                        ResearchTool::Artifacts { run_sha256 },
                    ) => allowed == run_sha256,
                    _ => false,
                })
    }
}

fn read_capabilities(path: &std::path::Path) -> Result<Vec<ToolCapability>> {
    let parent = path.parent().context("broker parent required")?;
    ensure!(
        parent.canonicalize()? == parent,
        "broker parent must be canonical"
    );
    let file = std::fs::File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )?);
    let meta = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0
                && parent.metadata()?.permissions().mode() & 0o077 == 0,
            "broker projection and parent must be private"
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
    let capabilities: Vec<ToolCapability> = serde_json::from_slice(&bytes)?;
    ensure!(capabilities.len() <= 1024, "too many tool capabilities");
    let mut seen = std::collections::BTreeSet::new();
    for capability in &capabilities {
        capability.validate()?;
        ensure!(
            seen.insert(&capability.token_sha256),
            "duplicate tool capability"
        );
    }
    Ok(capabilities)
}
fn authorize(
    config: &AgentApiConfig,
    headers: &HeaderMap,
    tool: &ResearchTool,
    now: u64,
) -> Result<ToolCapability> {
    tool.validate()?;
    let token = headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .context("tool capability required")?;
    ensure!(
        (32..=4096).contains(&token.len()),
        "invalid capability token"
    );
    let expected = sha256(token.as_bytes());
    read_capabilities(&config.capabilities_file)?
        .into_iter()
        .find(|cap| cap.token_sha256 == expected && cap.admits(tool, now))
        .context("tool capability expired, revoked or out of scope")
}
#[derive(Clone)]
struct Api {
    ledger: Ledger,
    config: AgentApiConfig,
}
/// Definitions and approved task requests remain native-controlled. A broker
/// capability narrows access; it never substitutes for PG's native admission.
pub async fn execute(
    ledger: &Ledger,
    tenant: &str,
    tool: ResearchTool,
) -> Result<serde_json::Value> {
    tool.validate()?;
    match tool {
        ResearchTool::Submit {
            request_sha256,
            idempotency_key,
        } => {
            let request = ledger.approved_request(tenant, &request_sha256).await?;
            let run = request.run_manifest_sha256.clone();
            let task = ledger.submit(tenant, &idempotency_key, request).await?;
            Ok(serde_json::json!({"run_sha256":run,"task_id":task}))
        }
        ResearchTool::Status { run_sha256 } => {
            let run = ledger.run_for_tenant(tenant, &run_sha256).await?;
            let task = ledger.task_for_run(tenant, &run_sha256).await?;
            Ok(
                serde_json::json!({"run":run,"state":task.state,"attempt":task.attempt,"deadline_ms":task.deadline_ms,"result_verified":task.state==crate::orchestrator::State::Succeeded}),
            )
        }
        ResearchTool::Artifacts { run_sha256 } => {
            ledger.run_for_tenant(tenant, &run_sha256).await?;
            let result = ledger.result_for_run(tenant, &run_sha256).await?;
            Ok(serde_json::json!({"run_sha256":run_sha256,"artifacts":result.map(|r|r.artifacts)}))
        }
    }
}
async fn handle(
    State(api): State<Arc<Api>>,
    headers: HeaderMap,
    Json(tool): Json<ResearchTool>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let capability =
        authorize(&api.config, &headers, &tool, now).map_err(|_| StatusCode::UNAUTHORIZED)?;
    execute(&api.ledger, &capability.tenant, tool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}
pub async fn start(
    config: AgentApiConfig,
    ledger: Ledger,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let bind = config.validate()?;
    read_capabilities(&config.capabilities_file)?;
    let router = Router::new()
        .route("/research", post(handle))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(Arc::new(Api { ledger, config }));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    Ok(tokio::spawn(async move {
        axum::serve(listener, router).await?;
        Ok(())
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn broker_reload_narrows_tenant_method_run_request_and_revokes_without_restart() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir()?;
        let parent = temporary.path().canonicalize()?;
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        let path = parent.join("capabilities.json");
        let capability = ToolCapability {
            token_sha256: sha256(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"),
            tenant: "tenant-a".into(),
            not_before_ms: 100,
            expires_ms: 200,
            permissions: vec![
                ToolPermission::Status {
                    run_sha256: "a".repeat(64),
                },
                ToolPermission::Submit {
                    request_sha256: "b".repeat(64),
                    idempotency_key: "fixed".into(),
                },
            ],
        };
        std::fs::write(&path, serde_json::to_vec(&vec![capability.clone()])?)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let config = AgentApiConfig {
            bind: "127.0.0.1:8081".into(),
            capabilities_file: path.clone(),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", "x".repeat(32)).parse()?,
        );
        let tool = ResearchTool::Status {
            run_sha256: "a".repeat(64),
        };
        assert_eq!(authorize(&config, &headers, &tool, 100)?.tenant, "tenant-a");
        assert!(authorize(&config, &headers, &tool, 99).is_err());
        assert!(authorize(&config, &headers, &tool, 200).is_err());
        assert!(authorize(
            &config,
            &headers,
            &ResearchTool::Status {
                run_sha256: "b".repeat(64)
            },
            100
        )
        .is_err());
        assert!(authorize(
            &config,
            &headers,
            &ResearchTool::Artifacts {
                run_sha256: "a".repeat(64)
            },
            100
        )
        .is_err());
        assert!(authorize(
            &config,
            &headers,
            &ResearchTool::Submit {
                request_sha256: "b".repeat(64),
                idempotency_key: "other".into()
            },
            100
        )
        .is_err());
        let next = parent.join("next.json");
        std::fs::write(&next, b"[]")?;
        std::fs::set_permissions(&next, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&next, &path)?;
        assert!(authorize(&config, &headers, &tool, 100).is_err());
        std::fs::remove_file(&path)?;
        ensure!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()?
                .success(),
            "FIFO fixture creation failed"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        assert!(authorize(&config, &headers, &tool, 100).is_err());
        std::fs::remove_file(&path)?;
        std::os::unix::fs::symlink(&next, &path)?;
        assert!(authorize(&config, &headers, &tool, 100).is_err());
        assert!(serde_json::from_value::<ResearchTool>(serde_json::json!({"method":"research.status","run_sha256":"a".repeat(64),"tenant":"another"})).is_err());
        assert!(serde_json::from_value::<ToolPermission>(
            serde_json::json!({"method":"research.cancel","task_id":"a".repeat(64)})
        )
        .is_err());
        Ok(())
    }
}
