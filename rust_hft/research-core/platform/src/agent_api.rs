//! A bounded, scoped interface for an existing Coding Agent. The Agent gets
//! neither PG credentials nor a Kubernetes client. No generic shell/admin RPC.
use crate::{postgres::Ledger, research::ResearchTool, sha256};
use anyhow::{ensure, Result};
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, sync::Arc};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentApiConfig {
    pub bind: String,
    pub tenant: String,
    pub token_file: String,
}
impl AgentApiConfig {
    pub fn validate(&self) -> Result<SocketAddr> {
        let bind: SocketAddr = self.bind.parse()?;
        ensure!(
            bind.ip().is_loopback()
                && bind.port() > 0
                && !self.tenant.is_empty()
                && self.tenant.len() <= 128,
            "research tool API requires explicit loopback principal"
        );
        Ok(bind)
    }
}
#[derive(Clone)]
struct Api {
    ledger: Ledger,
    tenant: String,
    token_sha256: String,
}
fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .is_some_and(|token| sha256(token.as_bytes()) == expected)
}
/// `tenant` comes only from the authenticated server principal, never the tool
/// payload. Definitions and approved task requests are written by native control.
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
            // Status deliberately contains scientific identities and execution
            // state, not provider handles, secrets or raw data/holdout labels.
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
    if !authorized(&headers, &api.token_sha256) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    execute(&api.ledger, &api.tenant, tool)
        .await
        .map(Json)
        .map_err(|_| StatusCode::BAD_REQUEST)
}
pub async fn start(
    config: AgentApiConfig,
    token: String,
    ledger: Ledger,
) -> Result<tokio::task::JoinHandle<Result<()>>> {
    let bind = config.validate()?;
    ensure!(
        token.len() >= 32 && token.len() <= 4096,
        "tool capability token requires 32..4096 bytes"
    );
    let api = Arc::new(Api {
        ledger,
        tenant: config.tenant,
        token_sha256: sha256(token.as_bytes()),
    });
    let router = Router::new()
        .route("/research", post(handle))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(api);
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
    fn tools_require_capability_and_cannot_select_server_principal_or_admin_method() {
        let token = "x".repeat(32);
        let hash = sha256(token.as_bytes());
        let mut headers = HeaderMap::new();
        assert!(!authorized(&headers, &hash));
        headers.insert("Authorization", format!("Bearer {token}").parse().unwrap());
        assert!(authorized(&headers, &hash));
        headers.insert("Authorization", "Bearer wrong".parse().unwrap());
        assert!(!authorized(&headers, &hash));
        assert!(serde_json::from_value::<ResearchTool>(
            serde_json::json!({"method":"research.kubectl","command":"apply"})
        )
        .is_err());
        assert!(serde_json::from_value::<ResearchTool>(serde_json::json!({"method":"research.status","run_sha256":"a".repeat(64),"tenant":"another"})).is_err());
        let config = AgentApiConfig {
            bind: "0.0.0.0:1234".into(),
            tenant: "fixture".into(),
            token_file: "/fixture".into(),
        };
        assert!(config.validate().is_err());
    }
}
